//! 出站 webhook 的 outbox 半边：emit 侧事件落库（§4.1）+ 租约式投递 poller（§4.2）。
//!
//! **双轨 MODE**（决议 2）：`FLOW_WEBHOOK_MODE = outbox（默认）| legacy`——legacy 走
//! 内存链路（adapters `WebhookSender`，保留至 M3），outbox 走「事件落投递行 → poller
//! 租约式投递」。MODE 只分流 webhook 半边；SSE 半边不受影响，两种模式照常广播。
//!
//! **投递语义 = at-least-once**：常态（逐行续租正常）下各 worker 行集互不相交；租约真
//! 过期（进程长停顿/网络分区）的异常窗口同一行可能双投——接收方须按 delivery_id 或
//! 业务键幂等（对接文档要求；mdm 靠五规则状态机天然满足）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Semaphore;

use cmx_flow_adapters::{DeliveryOutcome, DeliveryTask, global_registry};

use crate::engine::FlowRuntime;
use crate::webhook_store::SubRow;
use crate::webhook_store::{
    active_subscriptions_cached, claim_due_deliveries, finish_dead, finish_done,
    finish_retry_or_dead, get_subscriptions_by_ids, insert_deliveries, renew_leases, DeliveryInsert,
};

// ============================================================
// emit 侧：事件 → 投递行（事务提交后、单批写入）
// ============================================================

/// 非原子窗口计数（业务成功、投递行写入失败的理论窗口；联动 013 观测）。
pub static OUTBOX_INSERT_ERRORS: AtomicU64 = AtomicU64::new(0);
/// 幽灵绑定全局计数（实例的 subscriber_id 指向已删除订阅；事件丢弃 + warn 日志，方案 §3.6）。
pub static GHOST_BINDINGS: AtomicU64 = AtomicU64::new(0);
/// emit 侧订阅点查失败计数（fail-close：S 支路清空只投显式旁听，不降级广播）。
pub static EMIT_LOOKUP_ERRORS: AtomicU64 = AtomicU64::new(0);

/// per-subscription 丢弃计数（S 停用 / event_types 白名单外 → 事件丢弃不回退默认层）。
/// 照 OUTBOX_INSERT_ERRORS 现成模式的进程内计数：重启清零可接受（发现异常按日志追溯）；
/// 订阅量小 + emit 低频，Mutex<HashMap> 足够（不为此引入 dashmap 依赖）。
fn dropped_counters() -> &'static std::sync::Mutex<HashMap<i64, u64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, u64>>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 累加一条丢弃计数。
fn count_dropped(subscription_id: i64) {
    if let Ok(mut m) = dropped_counters().lock() {
        *m.entry(subscription_id).or_insert(0) += 1;
    }
}

/// 读某订阅的累计丢弃数（管理页订阅卡片 / mon 端点投影）。
pub fn dropped_count(subscription_id: i64) -> u64 {
    dropped_counters().lock().ok().and_then(|m| m.get(&subscription_id).copied()).unwrap_or(0)
}

/// webhook 路由域运维计数快照（GET /webhook-subscriptions/mon 投影）。
pub fn mon_snapshot() -> serde_json::Value {
    let per: serde_json::Map<String, serde_json::Value> = dropped_counters()
        .lock()
        .ok()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "ghostBindings": GHOST_BINDINGS.load(Ordering::Relaxed),
        "emitLookupErrors": EMIT_LOOKUP_ERRORS.load(Ordering::Relaxed),
        "outboxInsertErrors": OUTBOX_INSERT_ERRORS.load(Ordering::Relaxed),
        "droppedPerSubscription": per,
    })
}

/// 把一批生命周期事件写入投递行（候选集并集去重后，每事件 × 每命中订阅一行）。
///
/// **v2.4 三级路由匹配（方案 §3.2，仅 2 条规则）**：
/// - 规则 2（实例未绑定，`bound_subscriber = None`）：活跃订阅中「definition_keys 空或命中
///   def_key × event_types 空或含 event_type」全量匹配——与 v2.3 行为逐字节一致（存量零破坏）。
/// - 规则 1（实例已绑定 S）：候选 = {S（active 且 event_types 空集豁免）} ∪ {显式订阅 R 旁听
///   （definition_keys 非空且命中 + event_types 豁免）}。绑定投递是**全量定向委托**（不受 S
///   自身 definition_keys 约束，event_types 白名单仍有效）；绑定只屏蔽**通配订阅**。
/// - **S 主键点查不走 5s TTL 缓存**（R9）：消除「绑定后首个事件因缓存时滞被静默丢弃」的欠投面。
/// - **fail-close**（R3）：S 点查失败/未知时不降级规则 2 广播——只清空定向支路，旁听支路照常。
/// - 白名单外/停用/幽灵 → 丢弃 + 计数（§3.6），不回退默认层（防泄露）。
///
/// 实例订阅绑定的路由状态（三态，X2-1 fail-close 修复 W-01）。
///
/// handler 层借快照读取绑定时，**读失败 ≠ 未绑定**：折叠为同一个 `None` 会让绑定实例
/// 的事件在异常窗口被广播给全部通配订阅（v2.4 要消除的泄露面）。`Unknown` 走 fail-close
/// ——只求旁听支路（显式声明定义集的订阅），绝不降级规则 2（方案 §3.2 R3）。
/// 被排除的更轻选项（RB-09 记录）：Unknown 时完全不投（只 SSE+计数）——不取，因旁听订阅
/// 是接收方显式声明，异常窗口静默全丢比少投更伤信任。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriberRoute {
    /// 快照读取成功且实例已绑定订阅（定向必达 + 旁听）。
    Bound(i64),
    /// 快照读取成功，实例未绑定（规则 2 全量匹配——与 v2.3 行为一致）。
    Unbound,
    /// 快照读取失败，绑定未知：fail-close 只投显式旁听 + 计数。
    Unknown,
}

/// 写入失败只记 error 日志 + 计数——emit 是通知路径，不因投递行写不进阻断业务。
pub async fn emit_to_outbox(events: &[cmx_flow_adapters::FlowEvent], route: SubscriberRoute) {
    if events.is_empty() {
        return;
    }
    let tenant = crate::tenant::current_tenant();
    let db_id = crate::engine::current_flow_db_id();
    let subs = active_subscriptions_cached(&db_id, &tenant).await;
    // W-06：S 主键点查批外一次（轻量路由查询，无 binding_count 子查询）——同批 N 事件
    // 共用同一定向支路结果；点查失败按 fail-close（仅旁听）处理，逐事件不再重查。
    let bound_sub: Option<Result<Option<SubRow>, String>> = match route {
        SubscriberRoute::Bound(sid) => Some(
            crate::webhook_store::get_subscription_for_route(&db_id, &tenant, sid).await,
        ),
        _ => None,
    };
    let mut rows = Vec::new();
    for event in events {
        // 候选集（并集后再落行——同一订阅被多条路径命中只产生一行，R25）。
        let candidates: Vec<(SubRow, bool)> = match route {
            SubscriberRoute::Unbound => {
                if subs.is_empty() {
                    continue;
                }
                subs.iter()
                    .filter(|s| subscription_matches(s, event))
                    .map(|s| (s.clone(), false))
                    .collect()
            }
            SubscriberRoute::Unknown => {
                // fail-close（W-01/R3）：绑定未知不降级规则 2 广播——只投显式旁听支路。
                EMIT_LOOKUP_ERRORS.fetch_add(1, Ordering::Relaxed);
                if subs.is_empty() {
                    tracing::warn!(
                        instance = %event.instance_id,
                        event = %event.event,
                        "绑定未知且订阅缓存为空，事件零投递（fail-close）"
                    );
                    continue;
                }
                tracing::warn!(
                    instance = %event.instance_id,
                    event = %event.event,
                    "绑定未知（快照读取失败），fail-close：仅投显式旁听订阅"
                );
                let cands: Vec<(SubRow, bool)> = subs
                    .iter()
                    .filter(|r| listen_matches(r, event))
                    .map(|r| (r.clone(), false))
                    .collect();
                cands
            }
            SubscriberRoute::Bound(sid) => {
                let mut cands: Vec<(SubRow, bool)> = Vec::new();
                // —— 定向支路：S 主键点查（不过缓存；批外已查，此处消费结果）——
                match bound_sub.clone().expect("Bound 分支必有点查结果") {
                    Ok(Some(s)) if s.active => {
                        if s.event_types.is_empty() || s.event_types.contains(&event.event) {
                            cands.push((s, true));
                        } else {
                            // 接收方显式白名单排除该事件：丢弃不回退默认层（防泄露）+ 计数。
                            count_dropped(sid);
                            tracing::warn!(
                                subscription = sid,
                                event = %event.event,
                                "绑定订阅 event_types 白名单外，事件丢弃（不回退默认层）"
                            );
                        }
                    }
                    Ok(Some(_)) => {
                        // 订阅存在但已停用：绑定实例的事件丢弃 + 计数（页面卡片可见）。
                        count_dropped(sid);
                        tracing::warn!(subscription = sid, "绑定订阅已停用，事件丢弃");
                    }
                    Ok(None) => {
                        // 幽灵绑定（订阅已删，极窄窗口残余态）：全局计数 + warn；人工修复走 SQL。
                        GHOST_BINDINGS.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            instance = %event.instance_id,
                            subscription = sid,
                            "幽灵绑定（订阅已删除），事件丢弃（人工修复见零 SQL 运维例外清单）"
                        );
                    }
                    Err(e) => {
                        // fail-close：点查失败不清空旁听、但绝不降级规则 2 广播。
                        EMIT_LOOKUP_ERRORS.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(error = %e, "绑定订阅点查失败（fail-close：仅投显式旁听）");
                    }
                }
                // —— 旁听支路：缓存中显式声明定义集的订阅不受屏蔽（L2 不屏蔽 L3）——
                if !subs.is_empty() {
                    let sids: Vec<i64> = cands.iter().map(|(s, _)| s.id).collect();
                    cands.extend(
                        subs.iter()
                            .filter(|r| !sids.contains(&r.id))
                            .filter(|r| listen_matches(r, event))
                            .map(|r| (r.clone(), false)),
                    );
                }
                cands
            }
        };
        for (sub, bound) in candidates {
            rows.push(DeliveryInsert {
                subscription_id: sub.id,
                subscription_name: sub.name.clone(),
                channel: sub.channel.clone(),
                // 事件唯一键：emit 时生成 uuid；uk(sub, event_id) 幂等吸收重复写入。
                event_id: uuid::Uuid::new_v4().to_string(),
                delivery_id: event.delivery_id(),
                source: "emit",
                event_type: event.event.clone(),
                definition_key: event.definition_key.clone(),
                business_key: event.business_key.clone(),
                instance_id: event.instance_id.clone(),
                payload: json!(event),
                initial_state: "PENDING",
                last_error: None,
                last_http_status: None,
                last_response_snippet: None,
                delivered: false,
                // 路由成因：绑定定向投递 bound / 规则匹配（含旁听）matched（§3.3）。
                route_source: if bound { "bound" } else { "matched" },
            });
        }
    }
    if rows.is_empty() {
        return;
    }
    if let Err(e) = insert_deliveries(&db_id, &rows).await {
        OUTBOX_INSERT_ERRORS.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            tenant,
            rows = rows.len(),
            error = %e,
            "投递行写入失败（事件可能缺行，走对账/补发口径）"
        );
    }
}

/// 规则 2 全量匹配（通道无关；空集 = 全部；null definitionKey 仅被「订阅全部」命中——
/// 修复前置补齐后不应再出现 null 载荷）。
fn subscription_matches(sub: &SubRow, event: &cmx_flow_adapters::FlowEvent) -> bool {
    let def_hit = sub.definition_keys.is_empty()
        || event
            .definition_key
            .as_ref()
            .map(|dk| sub.definition_keys.iter().any(|k| k == dk))
            .unwrap_or(false);
    let evt_hit = sub.event_types.is_empty() || sub.event_types.contains(&event.event);
    def_hit && evt_hit
}

/// 规则 1 旁听匹配（仅**显式**声明定义集的订阅可旁听绑定实例；event_types 空集豁免对称）。
fn listen_matches(sub: &SubRow, event: &cmx_flow_adapters::FlowEvent) -> bool {
    if sub.definition_keys.is_empty() {
        return false; // 通配订阅被绑定屏蔽（v2.4 消除广播的初心）。
    }
    let def_hit = event
        .definition_key
        .as_ref()
        .map(|dk| sub.definition_keys.iter().any(|k| k == dk))
        .unwrap_or(false);
    let evt_hit = sub.event_types.is_empty() || sub.event_types.contains(&event.event);
    def_hit && evt_hit
}

// ============================================================
// worker 侧：投递执行（租约 + 逐行续租 + 持有者守卫 + 同订阅保序）
// ============================================================

/// 租约时长：只须大于单行最长投递（基座 30s 键级超时）+ 续租余量；批长度经逐行续租
/// 不进入租约算术（决议 15）。
pub const LEASE_SECS: i64 = 120;
/// 单轮抢占上限（批内按订阅分组、组内按 seq 串行投递）。
pub const BATCH: usize = 50;
/// 跨订阅投递并发上限（semaphore）。
pub const CONCURRENCY: usize = 8;

/// 退避曲线：1s 起指数、封顶 5min（第 n 次尝试失败后等待 2^(n-1) 秒，决议 19；
/// retry_max = 最大尝试次数含首发，默认 10 → 首发到 DEAD 约 9~14 分钟）。
pub fn backoff_after(attempts: i64) -> chrono::Duration {
    let exp = (attempts.max(1) - 1).min(9) as u32;
    let secs = 1u64.checked_shl(exp).unwrap_or(300).min(300);
    chrono::Duration::seconds(secs as i64)
}

/// 经通道注册表投递一条任务（poller 与 test 端点共用；timeout 仅 test 传短超时 10s）。
pub(crate) async fn dispatch_via_channel(
    channel_type: &str,
    config: &serde_json::Value,
    task: DeliveryTask,
    timeout: Option<Duration>,
) -> DeliveryOutcome {
    let Some(channel) = global_registry().get(channel_type) else {
        return DeliveryOutcome::retry(format!("通道 {channel_type} 未注册（feature 未启用或已下线）"));
    };
    channel.deliver(config, &task, timeout).await
}

/// 对一个租户运行态跑一轮投递（engine.rs 的 poller tick 每 2s 调一次）。
///
/// 循环抢占直到空批/不足批；每行投递 → **持有者守卫**落结果 → **续租**本 worker 剩余
/// 在途行。守卫 0 行命中 = 租约丢失（进程长停顿/分区），立即中止本租户本轮——剩余行
/// 留待租约过期后重抢（防双投扩大）。
pub async fn poll_once(rt: &FlowRuntime, worker: &str) {
    let db_id = crate::tenancy::TenancyConfig::global().flow_db_id(&rt.tenant);
    loop {
        let claimed = match claim_due_deliveries(&db_id, worker, LEASE_SECS, BATCH).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(tenant = %rt.tenant, error = %e, "webhook 投递行抢占失败");
                return;
            }
        };
        if claimed.is_empty() {
            return;
        }
        let full = claimed.len() >= BATCH;
        // 投递前取本批涉及的订阅配置（不筛 active——停用订阅的存量行仍投）。
        let mut sub_ids: Vec<i64> = claimed.iter().map(|c| c.subscription_id).collect();
        sub_ids.sort_unstable();
        sub_ids.dedup();
        let subs = get_subscriptions_by_ids(&db_id, &rt.tenant, &sub_ids)
            .await
            .unwrap_or_default();
        if !deliver_batch(&db_id, worker, claimed, &subs).await {
            tracing::warn!(tenant = %rt.tenant, "投递租约丢失（持有者守卫命中），中止本轮，剩余行待租约过期重抢");
            return;
        }
        if !full {
            return;
        }
    }
}

/// 投递一批：按订阅分组（组内按 seq 串行——保序）、跨组 semaphore 并发。
/// 返回 false = 任一租约丢失（调用方应中止本轮）。
async fn deliver_batch(
    db_id: &str,
    worker: &str,
    claimed: Vec<crate::webhook_store::ClaimedDelivery>,
    subs: &HashMap<i64, SubRow>,
) -> bool {
    let mut groups: HashMap<i64, Vec<crate::webhook_store::ClaimedDelivery>> = HashMap::new();
    for row in claimed {
        groups.entry(row.subscription_id).or_default().push(row);
    }
    let sem = Arc::new(Semaphore::new(CONCURRENCY));
    let mut handles = Vec::with_capacity(groups.len());
    for (_, mut rows) in groups {
        rows.sort_by_key(|r| r.seq);
        let permit = sem.clone().acquire_owned().await;
        let db = db_id.to_string();
        let w = worker.to_string();
        let subs = subs.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            deliver_group(&db, &w, rows, &subs).await
        }));
    }
    for h in handles {
        match h.await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                tracing::error!(error = %e, "投递组任务 panic/取消");
                return false;
            }
        }
    }
    true
}

/// 一组（同订阅）投递：组内按 seq 串行，逐行「结果落库（守卫）→ 续租」。
async fn deliver_group(
    db_id: &str,
    worker: &str,
    rows: Vec<crate::webhook_store::ClaimedDelivery>,
    subs: &HashMap<i64, SubRow>,
) -> bool {
    for row in rows {
        let sub = subs.get(&row.subscription_id);
        let outcome = match sub {
            // 订阅已物理删（仅停用态可删，正常不应再有存量行）：直达死信，不空转重试。
            None => DeliveryOutcome::fatal("订阅已删除，通道配置不可得（可 retry 重发或 skip 处置）"),
            Some(s) => {
                let task = DeliveryTask {
                    subscription_name: row.subscription_name.clone(),
                    event_type: row.event_type.clone(),
                    definition_key: row.definition_key.clone(),
                    business_key: row.business_key.clone(),
                    instance_id: row.instance_id.clone(),
                    delivery_id: row.delivery_id.clone(),
                    payload: row.payload.clone(),
                };
                dispatch_via_channel(&row.channel, &s.channel_config, task, None).await
            }
        };
        // 结果落库（持有者守卫；0 行命中 = 租约已丢，返回 false 中止本组/批）。
        let (retry_max, attempts) = (
            sub.map(|s| s.retry_max).unwrap_or(10),
            row.attempts,
        );
        let diag = |error: &String, http_status: &Option<u16>, snippet: &Option<String>| {
            crate::webhook_store::Diagnostics {
                error: error.clone(),
                http_status: http_status.map(i64::from),
                snippet: snippet.clone(),
            }
        };
        let guarded = match &outcome {
            DeliveryOutcome::Success => finish_done(db_id, row.id, worker).await,
            DeliveryOutcome::Retryable { http_status, error, snippet } => {
                finish_retry_or_dead(
                    db_id,
                    row.id,
                    worker,
                    attempts,
                    retry_max,
                    backoff_after(attempts),
                    &diag(error, http_status, snippet),
                )
                .await
            }
            DeliveryOutcome::Fatal { http_status, error, snippet } => {
                finish_dead(db_id, row.id, worker, &diag(error, http_status, snippet)).await
            }
        };
        match guarded {
            Ok(true) => {}
            Ok(false) => return false, // 守卫命中：租约丢失。
            Err(e) => {
                tracing::error!(delivery = row.id, error = %e, "落投递结果失败（行留 IN_FLIGHT，租约过期后自愈重投）");
                return false;
            }
        }
        // 逐行续租（决议 15）：本 worker 名下全部在途行延长一窗——批长度不进入租约算术。
        if let Err(e) = renew_leases(db_id, worker, LEASE_SECS).await {
            tracing::warn!(error = %e, "投递租约续租失败（窗口内租约可能过期，行会被重抢）");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook_store::SubRow;

    fn sub(dks: Vec<&str>, ets: Vec<&str>) -> SubRow {
        SubRow {
            id: 1,
            name: "s".into(),
            channel: "webhook".into(),
            channel_config: json!({}),
            definition_keys: dks.into_iter().map(String::from).collect(),
            event_types: ets.into_iter().map(String::from).collect(),
            active: true,
            retry_max: 10,
            source: "manual".into(),
            tenant_id: "default".into(),
            created_by: None,
            binding_count: 0,
        }
    }

    fn evt(def_key: Option<&str>, event: &str) -> cmx_flow_adapters::FlowEvent {
        let mut ev = cmx_flow_adapters::FlowEvent::new(
            cmx_flow_adapters::FlowEventKind::InstanceStarted,
            "i-1",
            "t".into(),
        )
        .definition_key(def_key.map(String::from))
        .task(None, None)
        .tenant(None)
        .business_key(None)
        .assignee(None)
        .state(None);
        ev.event = event.to_string(); // 匹配语义测试需要任意事件名，直接覆盖。
        ev
    }

    /// 匹配语义（通道无关）：空集 = 全部；命中/未命中；null definitionKey 仅被「订阅全部」命中。
    #[test]
    fn subscription_match_semantics() {
        let all = sub(vec![], vec![]);
        let filtered = sub(vec!["mdm_cr"], vec!["instance.started"]);
        // 订阅全部：null / 具体值都命中。
        assert!(subscription_matches(&all, &evt(None, "instance.started")));
        assert!(subscription_matches(&all, &evt(Some("any"), "task.created")));
        // 过滤订阅：命中与未命中。
        assert!(subscription_matches(&filtered, &evt(Some("mdm_cr"), "instance.started")));
        assert!(!subscription_matches(&filtered, &evt(Some("other"), "instance.started")));
        assert!(!subscription_matches(&filtered, &evt(Some("mdm_cr"), "task.created")));
        // null definitionKey 的事件只被「订阅全部」命中。
        assert!(!subscription_matches(&filtered, &evt(None, "instance.started")));
    }

    /// v2.4 规则 1 旁听匹配（方案 §3.2）：仅显式声明定义集的订阅可旁听；
    /// definition_keys 空集（通配）被绑定屏蔽；event_types 空集豁免对称。
    #[test]
    fn listen_match_semantics() {
        let explicit = sub(vec!["mdm_cr"], vec![]); // 显式定义集 + 全部事件
        let explicit_evt = sub(vec!["mdm_cr"], vec!["task.created"]); // 显式定义集 + 白名单
        let wildcard = sub(vec![], vec![]); // 通配（被绑定屏蔽）
        // 显式订阅 + def_key 命中 + 事件集空集豁免 → 旁听成立。
        assert!(listen_matches(&explicit, &evt(Some("mdm_cr"), "instance.started")));
        // def_key 不命中 → 不旁听。
        assert!(!listen_matches(&explicit, &evt(Some("other"), "instance.started")));
        // 事件白名单外 → 不旁听（豁免只对空集）。
        assert!(!listen_matches(&explicit_evt, &evt(Some("mdm_cr"), "instance.started")));
        assert!(listen_matches(&explicit_evt, &evt(Some("mdm_cr"), "task.created")));
        // 通配订阅被绑定屏蔽（v2.4 消除广播初心）。
        assert!(!listen_matches(&wildcard, &evt(Some("mdm_cr"), "instance.started")));
        // null definitionKey 的事件不被显式订阅旁听（无法证明命中）。
        assert!(!listen_matches(&explicit, &evt(None, "instance.started")));
    }

    /// v2.4 丢弃计数（§3.6）：per-subscription 计数、mon 快照投影。
    #[test]
    fn dropped_counter_semantics() {
        let probe = 987654321i64; // 测试专用 id，不与其它测试共用
        assert_eq!(dropped_count(probe), 0);
        count_dropped(probe);
        count_dropped(probe);
        assert_eq!(dropped_count(probe), 2);
        let snap = mon_snapshot();
        assert_eq!(snap["droppedPerSubscription"]["987654321"], json!(2));
        assert!(snap["ghostBindings"].is_u64() && snap["outboxInsertErrors"].is_u64());
    }

    /// 退避曲线（决议 19）：1s 起指数、封顶 300s。
    #[test]
    fn backoff_curve() {
        assert_eq!(backoff_after(1), chrono::Duration::seconds(1));
        assert_eq!(backoff_after(2), chrono::Duration::seconds(2));
        assert_eq!(backoff_after(3), chrono::Duration::seconds(4));
        assert_eq!(backoff_after(10), chrono::Duration::seconds(300));
        assert_eq!(backoff_after(20), chrono::Duration::seconds(300));
    }

}
