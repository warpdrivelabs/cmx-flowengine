//! 设计器协同 M1（感知 + 防冲突）+ M2（对象级属性合并）。
//!
//! 与 [`crate::events`]（生命周期 `FlowEvent` 闭合 enum）解耦：本模块自持一条独立的进程内
//! `tokio::broadcast` 通道 + 自由 JSON payload，只服务**设计态协同**（谁在线 / 谁选中哪个节点 /
//! 草稿被谁保存了 / 谁改了哪个节点的属性）。SSE 按 `(tenant, defKey)` 过滤——补上 events.rs 只按
//! tenant 过滤的缺口，使「只有同一草稿的编辑者互相收到对方的在场/选中/编辑」。
//!
//! - **M1 感知**：presence 为**进程内内存态**（`Mutex<HashMap>`）+ **sweep-on-build**（构 roster 时
//!   剔除超 TTL 的会话，免后台任务）。远端选中高亮。
//! - **M1 防冲突**：草稿保存乐观锁（`updated_at` 当 etag，冲突走 `draft.saved` 通知 + 前端确认框）。
//! - **M2 对象级合并**：一端改节点属性即经 `/design/op` 广播 `op` 事件，服务端盖 per-(tenant,defKey)
//!   单调 seq；另一端就地 `updateProperties` 合并（对象级 LWW：同元素同属性只应用更大 seq）。结构级
//!   增删/移动留 M3。
//!
//! 多副本部署不共享（对齐 flow SSE 现状 + 报表单实例假设，已知限制）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::extract::Query;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::resp::{ApiResp, Result};
use crate::tenant::{current_tenant, current_user};

/// 事件总线容量（满则最老被丢，SSE 侧忽略 Lagged 继续）。
const BUS_CAPACITY: usize = 512;
/// presence 会话存活 TTL（秒）；心跳周期约 10s，故 25s 容两次丢失。
const PRESENCE_TTL_SECS: i64 = 25;

/// 协同事件（自由 payload；SSE 按 tenant+defKey 过滤）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabEvent {
    /// 事件名：`presence`（roster 快照）| `draft.saved`（草稿被保存）。
    pub kind: String,
    pub tenant: String,
    pub def_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// presence: `{roster:[Presence...]}`；draft.saved: `{updatedAt}`。
    pub payload: Value,
    /// 事件时间 RFC3339。
    pub at: String,
}

static BUS: OnceLock<broadcast::Sender<CollabEvent>> = OnceLock::new();
fn bus() -> &'static broadcast::Sender<CollabEvent> {
    BUS.get_or_init(|| broadcast::channel(BUS_CAPACITY).0)
}

/// 广播一条协同事件（无订阅者则静默丢）。供本模块及 handler（draft.saved）调用。
pub fn publish(ev: CollabEvent) {
    let _ = bus().send(ev);
}

/// 便捷构造 + 广播 draft.saved（handler 保存成功后调）。
pub fn publish_draft_saved(def_key: &str, user: Option<String>, updated_at: &str) {
    publish(CollabEvent {
        kind: "draft.saved".into(),
        tenant: current_tenant(),
        def_key: def_key.to_string(),
        user,
        payload: json!({ "updatedAt": updated_at }),
        at: now_rfc3339(),
    });
}

// ───────────────────────── op-log 序列（M2 对象级合并） ─────────────────────────
//
// M2 = 对象级实时合并：一端改节点属性（bpmn-js updateProperties）即广播一条 `op`，另一端在
// 画布上就地 applyProperties（echo-guard 防回声）。为让多端「同键后写覆盖先写」有稳定判据，
// 每条 op 由服务端盖一个 **per-(tenant,defKey) 单调递增 seq**——接收端只应用 seq 更大的同元素同属性
// 变更（对象级 LWW，对齐报表 B 档 op_log 的 last-writer 语义）。进程内计数器，与 presence 同为
// 单实例假设（多副本不共享，M2 已知限制）。结构级增删/移动留 M3，本轮只做属性合并。

/// (tenant, defKey) → 下一个 op seq。
type SeqMap = HashMap<(String, String), u64>;
static OP_SEQ: OnceLock<Mutex<SeqMap>> = OnceLock::new();
fn op_seq() -> &'static Mutex<SeqMap> {
    OP_SEQ.get_or_init(|| Mutex::new(HashMap::new()))
}
fn next_seq(tenant: &str, def_key: &str) -> u64 {
    let mut m = op_seq().lock().unwrap();
    let e = m.entry((tenant.to_string(), def_key.to_string())).or_insert(0);
    *e += 1;
    *e
}

// ───────────────────────── presence 内存注册表 ─────────────────────────

/// 一个编辑会话的在场信息。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Presence {
    session_id: String,
    user: String,
    color: String,
    /// 当前选中的元素 bpmn id（远端高亮用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<String>,
    /// 最近心跳（epoch 秒）；序列化省略（仅服务端 sweep 用）。
    #[serde(skip)]
    last_seen: i64,
}

/// (tenant, defKey) → sessionId → Presence。
type Registry = HashMap<(String, String), HashMap<String, Presence>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 由 user 派生稳定色（CVD-safe 调色板，hash 取模；同人同色跨会话一致）。
fn color_for(user: &str) -> String {
    const PALETTE: &[&str] = &[
        "#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#4a3aa7", "#008300", "#e34948",
    ];
    let h = user.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[(h as usize) % PALETTE.len()].to_string()
}

/// 构 roster（含 sweep-on-build：剔除超 TTL 的会话；空则移除该草稿桶）。返回按 user 排序的快照。
fn build_roster(tenant: &str, def_key: &str) -> Vec<Presence> {
    let mut reg = registry().lock().unwrap();
    let key = (tenant.to_string(), def_key.to_string());
    let cutoff = now_secs() - PRESENCE_TTL_SECS;
    let mut out = Vec::new();
    if let Some(bucket) = reg.get_mut(&key) {
        bucket.retain(|_, p| p.last_seen >= cutoff);
        out = bucket.values().cloned().collect();
        if bucket.is_empty() {
            reg.remove(&key);
        }
    }
    out.sort_by(|a, b| a.user.cmp(&b.user).then(a.session_id.cmp(&b.session_id)));
    out
}

/// 构 roster 后广播 presence 事件（join/select/leave/心跳掉人时调）。
fn broadcast_roster(tenant: &str, def_key: &str) {
    let roster = build_roster(tenant, def_key);
    publish(CollabEvent {
        kind: "presence".into(),
        tenant: tenant.to_string(),
        def_key: def_key.to_string(),
        user: None,
        payload: json!({ "roster": roster }),
        at: now_rfc3339(),
    });
}

// ───────────────────────── SSE 订阅端点 ─────────────────────────

#[derive(Debug, Deserialize)]
pub struct CollabQuery {
    /// 只推该草稿的协同事件（必填；缺省空串=不匹配任何真实草稿）。
    #[serde(default, rename = "defKey")]
    def_key: String,
}

/// `GET /flow/v1/design/collab?defKey=K` —— 设计态协同 SSE，按 (tenant, defKey) 过滤。
///
/// tenant 取自认证 scope（off 模式 EventSource 无 header → default），非 query，防越权。
pub async fn collab_sse(
    Query(q): Query<CollabQuery>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let tenant = current_tenant();
    let def_key = q.def_key;

    let stream = BroadcastStream::new(bus().subscribe())
        .filter_map(|res| res.ok())
        .filter(move |ev: &CollabEvent| ev.tenant == tenant && ev.def_key == def_key)
        .map(|ev: CollabEvent| {
            let name = ev.kind.clone();
            Ok(Event::default()
                .event(name)
                .json_data(&ev)
                .unwrap_or_else(|_| Event::default().comment("serialize error")))
        });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// ───────────────────────── presence POST 端点 ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceReq {
    def_key: String,
    session_id: String,
    #[serde(default)]
    selection: Option<String>,
    /// 可选显式 user（current_user() 缺省时兜底；off 模式一般由 X-User 定）。
    #[serde(default)]
    user: Option<String>,
}

/// 解析 actor：认证上下文优先，其次 body.user，最后 anon。
fn actor(req_user: &Option<String>) -> String {
    current_user()
        .or_else(|| req_user.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anon".into())
}

/// `POST /flow/v1/design/presence/join` —— 加入草稿编辑会话 + 广播 roster。
pub async fn presence_join(Json(req): Json<PresenceReq>) -> Result<Json<ApiResp<Value>>> {
    let tenant = current_tenant();
    let user = actor(&req.user);
    {
        let mut reg = registry().lock().unwrap();
        let bucket = reg.entry((tenant.clone(), req.def_key.clone())).or_default();
        bucket.insert(
            req.session_id.clone(),
            Presence {
                session_id: req.session_id.clone(),
                color: color_for(&user),
                user,
                selection: req.selection.clone(),
                last_seen: now_secs(),
            },
        );
    }
    broadcast_roster(&tenant, &req.def_key);
    Ok(Json(ApiResp::ok(json!({ "ok": true }))))
}

/// `POST /flow/v1/design/presence/heartbeat` —— 刷新存活 + 选中；roster 变化（选中变/sweep 掉人）则广播。
pub async fn presence_heartbeat(Json(req): Json<PresenceReq>) -> Result<Json<ApiResp<Value>>> {
    let tenant = current_tenant();
    let mut changed;
    {
        let mut reg = registry().lock().unwrap();
        let cutoff = now_secs() - PRESENCE_TTL_SECS;
        let key = (tenant.clone(), req.def_key.clone());
        let bucket = reg.entry(key).or_default();
        // sweep：本次心跳顺带剔掉其它掉线会话（有剔除即 roster 变化 → 广播传播掉人）。
        let before = bucket.len();
        bucket.retain(|sid, p| *sid == req.session_id || p.last_seen >= cutoff);
        changed = bucket.len() != before;
        match bucket.get_mut(&req.session_id) {
            Some(p) => {
                p.last_seen = now_secs();
                if p.selection != req.selection {
                    p.selection = req.selection.clone();
                    changed = true;
                }
            }
            None => {
                // 会话已过期/未 join（如 SSE 重连后）→ 自愈重加。
                let user = actor(&req.user);
                bucket.insert(
                    req.session_id.clone(),
                    Presence {
                        session_id: req.session_id.clone(),
                        color: color_for(&user),
                        user,
                        selection: req.selection.clone(),
                        last_seen: now_secs(),
                    },
                );
                changed = true;
            }
        }
    }
    if changed {
        broadcast_roster(&tenant, &req.def_key);
    }
    Ok(Json(ApiResp::ok(json!({ "ok": true }))))
}

/// `POST /flow/v1/design/presence/select` —— 更新选中并广播（远端画布高亮）。
pub async fn presence_select(Json(req): Json<PresenceReq>) -> Result<Json<ApiResp<Value>>> {
    let tenant = current_tenant();
    {
        let mut reg = registry().lock().unwrap();
        if let Some(p) = reg
            .get_mut(&(tenant.clone(), req.def_key.clone()))
            .and_then(|b| b.get_mut(&req.session_id))
        {
            p.selection = req.selection.clone();
            p.last_seen = now_secs();
        }
    }
    broadcast_roster(&tenant, &req.def_key);
    Ok(Json(ApiResp::ok(json!({ "ok": true }))))
}

/// `POST /flow/v1/design/presence/leave` —— 离开会话并广播。
pub async fn presence_leave(Json(req): Json<PresenceReq>) -> Result<Json<ApiResp<Value>>> {
    let tenant = current_tenant();
    {
        let mut reg = registry().lock().unwrap();
        if let Some(b) = reg.get_mut(&(tenant.clone(), req.def_key.clone())) {
            b.remove(&req.session_id);
            if b.is_empty() {
                reg.remove(&(tenant.clone(), req.def_key.clone()));
            }
        }
    }
    broadcast_roster(&tenant, &req.def_key);
    Ok(Json(ApiResp::ok(json!({ "ok": true }))))
}

// ───────────────────────── op 中继端点（M2 对象级合并） ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpReq {
    def_key: String,
    session_id: String,
    /// op 类型：目前只 `updateProperties`（对象级属性合并）。
    op: String,
    /// 目标元素 bpmn id。
    element_id: String,
    /// 属性增量 { propName: value|null }（null=删除该属性）。自由 JSON，前端 bpmn-js 直接 apply。
    #[serde(default)]
    props: Value,
    #[serde(default)]
    user: Option<String>,
}

/// `POST /flow/v1/design/op` —— 中继一条对象级编辑操作（M2）。
///
/// 服务端盖 per-(tenant,defKey) 单调 seq 后广播（kind=`op`）；同 defKey 的其它编辑者据 seq 做
/// 对象级 LWW 合并（只应用更新的 seq）。origin sessionId 随事件下发，发起端据此忽略自己的回声。
pub async fn presence_op(Json(req): Json<OpReq>) -> Result<Json<ApiResp<Value>>> {
    let tenant = current_tenant();
    let seq = next_seq(&tenant, &req.def_key);
    let user = actor(&req.user);
    publish(CollabEvent {
        kind: "op".into(),
        tenant: tenant.clone(),
        def_key: req.def_key.clone(),
        user: Some(user),
        payload: json!({
            "seq": seq,
            "origin": req.session_id,
            "op": req.op,
            "elementId": req.element_id,
            "props": req.props,
        }),
        at: now_rfc3339(),
    });
    Ok(Json(ApiResp::ok(json!({ "ok": true, "seq": seq }))))
}
