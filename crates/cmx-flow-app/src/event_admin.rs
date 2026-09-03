//! 事件订阅管理端点（20260902 重构方案 §五：19 端点）。
//!
//! 四组：订阅者（9，含 rules/preview 与 rebuild）、投递（5，含 stats）、流程分组（3）、
//! 定义分组查询（2）。契约纪律：POST + JSON body、无 Path Variable、禁 PUT/PATCH/DELETE；
//! GET 仅详情/目录（简单只读）。**鉴权 fail-close**：auth 中间件未挂或 off 形态下写端点
//! 一律拒绝；写操作挂占位角色 `flow-event-admin`。openapi.rs ENDPOINTS 同步收录。
//!
//! secret 防线（mdm mask/keep 同款）：API 永不回显明文（掩码）、编辑回传掩码值或留空 =
//! 沿用旧值；channel_config 为**开放对象**——按通道校验必填键、不拒额外键。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Query;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::resp::{ApiResp, FlowError, Result};

use cmx_flow_adapters::{DeliveryOutcome, DeliveryTask, global_registry};

use crate::event_outbox::{dispatch_via_channel, match_rules};
use crate::event_store::{self, DlvFilter, GroupUpsert, SubFilter, SubRule, SubscriberUpsert};

/// 占位角色常量：写操作的目标角色（角色体系对齐时映射；平台 admin 过渡豁免见闸注释）。
pub const EVENT_ADMIN_ROLE: &str = "flow-event-admin";

/// 测试投递短超时（不等基座 30s 键级超时）。
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 单订阅者规则数上限（防病态长数组；分组订阅场景 20 条绰绰有余）。
const MAX_RULES: usize = 20;

/// 规则名/单条 key 模式长度上限（matched_rule 列宽同 64——超长在入口拦，防炸批）。
const MAX_RULE_NAME: usize = 64;
const MAX_PATTERN_LEN: usize = 128;

// ———————— 鉴权门 ————————

/// 写端点角色闸（与既有 webhook 管理同口径：service 身份 / 占位角色 / 平台 admin 过渡豁免）。
///
/// 纯角色判定、不看 auth mode（off 模式 ctx 无角色，无角色身份自然被拒，等效 fail-close）。
/// 拒绝一律 403（按状态码统计的监控可见）。
fn ensure_auth_gate() -> Result<()> {
    let roles = crate::tenant::current_roles();
    let allowed = [
        "service",
        crate::handlers::FLOW_ADMIN_ROLE,
        EVENT_ADMIN_ROLE,
        "admin", // 过渡豁免（门户反代剥 key 只透传 JWT，浏览器管理页用户暂无占位角色）
    ];
    if !roles.iter().any(|r| allowed.contains(&r.as_str())) {
        let who = crate::tenant::current_display_user().unwrap_or_else(|| "anonymous".into());
        return Err(FlowError::forbidden(format!(
            "EVENT_ADMIN_REQUIRED: 事件订阅管理写端点需 {EVENT_ADMIN_ROLE}/{} 角色或系统 key（service 身份）；当前身份 {who} 未持有",
            crate::handlers::FLOW_ADMIN_ROLE
        )));
    }
    tracing::info!(
        operator = crate::tenant::current_display_user().unwrap_or_else(|| "service".into()),
        roles = ?roles,
        "事件订阅管理写操作审计"
    );
    Ok(())
}

// ———————— secret 掩码（mdm mask/keep 同款） ———————

const MASK: &str = "******";

/// 掩码：短密钥整体打码，长密钥露前 4 后 4（按字符取，防多字节 UTF-8 字节切片 panic）。
fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        MASK.to_string()
    } else {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}{MASK}{tail}")
    }
}

/// channel_config 出参脱敏（secret 列掩码，其余键原样）。
fn masked_config(cfg: &Value) -> Value {
    let mut v = cfg.clone();
    if let Some(secret) = v.get("secret").and_then(Value::as_str) {
        let m = mask_secret(secret);
        v["secret"] = Value::String(m);
    }
    v
}

/// 解析入参 secret：留空 / 掩码回传 = None（沿用旧值）；否则 Some(新明文)。
fn resolve_incoming_secret(incoming: Option<&str>) -> Option<String> {
    incoming
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains(MASK))
        .map(String::from)
}

// ———————— DTO ———————

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubQueryReq {
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_page_size")]
    page_size: i64,
}

fn default_page_size() -> i64 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSubReq {
    /// None = 新建（服务端 Pk52 铸号）。
    #[serde(default)]
    id: Option<i64>,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_channel")]
    channel: String,
    /// 通道配置开放对象（webhook：service_key / callback_path / secret）。
    #[serde(default)]
    channel_config: Value,
    #[serde(default)]
    rules: Vec<SubRule>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    retry_max: Option<i64>,
}

fn default_channel() -> String {
    "webhook".into()
}

#[derive(Debug, Deserialize)]
pub struct IdReq {
    id: i64,
}

#[derive(Debug, Deserialize)]
pub struct SetActiveReq {
    id: i64,
    active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DlvQueryReq {
    #[serde(default)]
    subscriber_id: Option<i64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    matched_rule: Option<String>,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_page_size")]
    page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsReq {
    /// 统计窗口（小时；默认 24，1..=720）。
    #[serde(default = "default_stats_hours")]
    hours: i64,
    #[serde(default)]
    subscriber_id: Option<i64>,
}

fn default_stats_hours() -> i64 {
    24
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryDlvReq {
    #[serde(default)]
    ids: Vec<i64>,
    #[serde(default)]
    subscriber_id: Option<i64>,
    /// DEAD（默认）/ IN_FLIGHT（仅租约过期的卡死行可重置）。
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkipDlvReq {
    #[serde(default)]
    ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeDlvReq {
    #[serde(default = "default_before_days")]
    before_days: i64,
    #[serde(default)]
    state: Option<String>,
}

fn default_before_days() -> i64 {
    7
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSaveReq {
    #[serde(default)]
    id: Option<i64>,
    name: String,
    #[serde(default)]
    sort_no: Option<i32>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    remark: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefQueryReq {
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// 按分组过滤；与 ungrouped 互斥（同传时 groupId 优先）。
    #[serde(default)]
    group_id: Option<i64>,
    /// true = 只看未分组定义。
    #[serde(default)]
    ungrouped: Option<bool>,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_page_size")]
    page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetGroupReq {
    /// 批量设置的目标定义 key（非空）。
    keys: Vec<String>,
    /// 目标分组 id；null = 移出分组。
    group_id: Option<i64>,
}

// ———————— 公共小件 ———————

fn db() -> String {
    crate::engine::current_flow_db_id()
}

fn msg_err(e: String) -> FlowError {
    FlowError::business_error(e)
}

/// fail-fast 拒绝（→ HTTP 400）：守卫/校验统一走此形态（「以状态码判断」的接入方拿得到 400）。
fn bad_err(e: String) -> FlowError {
    FlowError::bad_request(e)
}

/// 合法事件类型（6 种生命周期事件）。
const EVENT_TYPES: &[&str] = &[
    "instance.started",
    "instance.completed",
    "instance.terminated",
    "task.created",
    "task.completed",
    "task.reassigned",
];

fn validate_event_types(types: &[String]) -> Result<()> {
    for t in types {
        if !EVENT_TYPES.contains(&t.as_str()) {
            return Err(msg_err(format!(
                "未知事件类型: {t}（合法值域: {}）",
                EVENT_TYPES.join(", ")
            )));
        }
    }
    Ok(())
}

/// 规则集校验（save 与 preview 共用）：名称非空 ≤64 且组内唯一、事件类型合法、
/// glob 语法仅 `*`、单条模式 ≤128、groupIds 存在性（`strict_groups=false` 时降级为
/// 标注——便于先建规则后建分组的编排顺序）、数量 ≤20。返回归一化后的规则集。
fn validate_rules(
    rules: &[SubRule],
    known_groups: &HashMap<i64, String>,
    strict_groups: bool,
) -> Result<Vec<SubRule>> {
    if rules.len() > MAX_RULES {
        return Err(msg_err(format!("规则数超上限（≤{MAX_RULES} 条）")));
    }
    let mut names = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        let name = r.name.trim();
        if name.is_empty() || name.len() > MAX_RULE_NAME {
            return Err(msg_err(format!("规则名必填且 ≤{MAX_RULE_NAME} 字符")));
        }
        if !names.insert(name.to_string()) {
            return Err(msg_err(format!("规则名同订阅者内须唯一: {name}")));
        }
        validate_event_types(&r.event_types)?;
        for gid in &r.group_ids {
            if strict_groups && !known_groups.contains_key(gid) {
                return Err(msg_err(format!("规则 {name} 引用的分组不存在: {gid}")));
            }
        }
        let mut patterns = Vec::with_capacity(r.key_patterns.len());
        for p in &r.key_patterns {
            let p = p.trim();
            if p.is_empty() {
                return Err(msg_err(format!("规则 {name} 存在空的 key 模式")));
            }
            if p.len() > MAX_PATTERN_LEN {
                return Err(msg_err(format!("规则 {name} 的 key 模式单条 ≤{MAX_PATTERN_LEN} 字符")));
            }
            if p.contains(['?', '[', ']']) {
                return Err(msg_err(format!(
                    "规则 {name} 的 key 模式仅支持 * 通配（不支持 ? / []）: {p}"
                )));
            }
            patterns.push(p.to_string());
        }
        out.push(SubRule {
            name: name.to_string(),
            enabled: r.enabled,
            event_types: r.event_types.clone(),
            group_ids: r.group_ids.clone(),
            key_patterns: patterns,
        });
    }
    Ok(out)
}

// ———————— 订阅者端点 ———————

/// POST /event-subscribers/query：分页列表（secret 掩码 + 健康度投影）。
pub async fn query_subscribers(
    Json(req): Json<SubQueryReq>,
) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?; // 确保运行时/建表就绪（fresh boot 首访兜底）
    let tenant = crate::tenant::current_tenant();
    let filter = SubFilter {
        keyword: req.keyword,
        channel: req.channel,
        active: req.active,
        page: req.page,
        page_size: req.page_size,
    };
    let total = event_store::count_subscribers(&db(), &tenant, &filter)
        .await
        .map_err(msg_err)?;
    let rows = event_store::list_subscribers(&db(), &tenant, &filter)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "rows": rows, "total": total }))))
}

/// GET /event-subscribers/detail?id=：详情（secret 掩码）。
pub async fn get_subscriber_detail(
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let id: i64 = params
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| msg_err("缺参数 id".into()))?;
    let sub = event_store::get_subscriber(&db(), &crate::tenant::current_tenant(), id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err(format!("订阅者不存在: {id}")))?;
    Ok(Json(ApiResp::ok(sub_detail_json(&sub))))
}

fn sub_detail_json(sub: &event_store::SubscriberRow) -> Value {
    json!({
        "id": sub.id,
        "name": sub.name,
        "description": sub.description,
        "channel": sub.channel,
        "channelConfig": masked_config(&sub.channel_config),
        "rules": sub.rules,
        "retryMax": sub.retry_max,
        "active": sub.active,
        "tenantId": sub.tenant_id,
        "createdBy": sub.created_by,
        "pendingCount": sub.pending_count,
        "lastDeliveredAt": sub.last_delivered_at,
        "deadCount24h": sub.dead_count_24h,
    })
}

/// POST /event-subscribers/save：新建/更新（rules 整体校验；secret 掩码回传 = 沿用旧值）。
///
/// 单行 upsert：rules 内嵌 JSONB，整集覆盖——并发保存 last-write-wins（无部分更新语义，
/// 前端编辑弹框整集提交）。名称唯一性 DB uk 兜底（预检给友好错误）。
pub async fn save_subscriber(Json(req): Json<SaveSubReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err(msg_err("订阅者名必填且 ≤128 字符".into()));
    }
    // 通道必须在注册表（未启用 feature 的通道拒绝——防建出永远重试的死订阅）。
    let channel = global_registry()
        .get(&req.channel)
        .ok_or_else(|| msg_err(format!("通道 {} 未注册（未启用）", req.channel)))?;
    let retry_max = req.retry_max.unwrap_or(10).clamp(1, 50) as i32;

    // 分组存在性 + 规则校验（groups 一次取回）。
    let groups = event_store::get_groups(&db()).await.map_err(msg_err)?;
    let known: HashMap<i64, String> = groups.into_iter().collect();
    let rules = validate_rules(&req.rules, &known, true)?;

    // 名称唯一预检 + secret 沿用取旧行——合并为一次查询。
    let tenant = crate::tenant::current_tenant();
    let old = match req.id {
        Some(id) => {
            event_store::get_subscriber(&db(), &tenant, id)
                .await
                .map_err(msg_err)?
                .ok_or_else(|| msg_err(format!("订阅者不存在: {id}")))?
        }
        None => {
            // 新建时撞名预检（uk 兜底并发窗口）。
            if let Some(other) = event_store::get_subscriber_by_name(&db(), &tenant, name)
                .await
                .map_err(msg_err)?
            {
                return Err(bad_err(format!("SUBSCRIBER_NAME_EXISTS: 订阅者名已存在: {}", other.name)));
            }
            event_store::SubscriberRow {
                id: 0,
                name: String::new(),
                description: None,
                channel: String::new(),
                channel_config: json!({}),
                rules: vec![],
                retry_max: 10,
                active: true,
                tenant_id: String::new(),
                created_by: None,
                pending_count: 0,
                last_delivered_at: None,
                dead_count_24h: 0,
            }
        }
    };
    let mut config = req.channel_config.clone();
    if config.get("secret").is_none()
        || resolve_incoming_secret(config.get("secret").and_then(Value::as_str)).is_none()
    {
        // 入参缺 secret 或掩码：新建 = 置空（validate 会拒），编辑 = 沿用旧值。
        if let Some(obj) = config.as_object_mut() {
            let kept = old.channel_config.get("secret").and_then(Value::as_str).map(String::from);
            match (req.id, kept) {
                (Some(_), Some(old_secret)) => {
                    obj.insert("secret".into(), Value::String(old_secret));
                }
                _ => {
                    obj.insert("secret".into(), Value::String(String::new()));
                }
            }
        }
    }
    // 通道配置结构校验（必填键；不拒额外键）。
    channel
        .validate_config(&config)
        .await
        .map_err(msg_err)?;

    let id = event_store::upsert_subscriber(
        &db(),
        &tenant,
        &SubscriberUpsert {
            id: req.id,
            name: name.to_string(),
            description: req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
            channel: req.channel.clone(),
            channel_config: config,
            rules,
            retry_max,
            active: req.active.unwrap_or(true),
            created_by: crate::tenant::current_user(),
        },
    )
    .await
    .map_err(msg_err)?;
    let sub = event_store::get_subscriber(&db(), &tenant, id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err("保存后回读失败".into()))?;
    Ok(Json(ApiResp::ok(json!({ "id": id, "subscriber": sub_detail_json(&sub) }))))
}

/// POST /event-subscribers/delete：物理删，仅停用态且无待投/投递中/死信行可删
/// （流水凭 subscriber_name 快照保审计）。
pub async fn delete_subscriber(Json(req): Json<IdReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    event_store::delete_subscriber(&db(), &crate::tenant::current_tenant(), req.id)
        .await
        // 守卫拒绝（不存在 / 启用中 / 有未终态或死信行）= 客户端可纠正 → 400 fail-fast。
        .map_err(bad_err)?;
    Ok(Json(ApiResp::ok(json!({ "deleted": req.id }))))
}

/// POST /event-subscribers/set-active：启停（停用即不再生成新投递行）。
/// 响应携带 `pendingCount`（待投存量）——页面停用确认框据此提示「存量行仍会投完」。
pub async fn set_subscriber_active(Json(req): Json<SetActiveReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    event_store::set_subscriber_active(&db(), &crate::tenant::current_tenant(), req.id, req.active)
        .await
        .map_err(bad_err)?;
    let sub = event_store::get_subscriber(&db(), &crate::tenant::current_tenant(), req.id)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({
        "id": req.id,
        "active": req.active,
        "pendingCount": sub.map(|s| s.pending_count).unwrap_or(0),
    }))))
}

/// GET /event-subscribers/channels：注册表当前可用通道 + channel_config schema
/// （页面下拉数据源；未启用 feature 的通道天然不出现）。
pub async fn list_channels() -> Result<Json<ApiResp<Value>>> {
    let reg = global_registry();
    let channels: Vec<Value> = reg
        .types()
        .into_iter()
        .filter_map(|t| reg.get(t).map(|ch| {
            json!({
                "type": ch.channel_type(),
                "name": ch.display_name(),
                "configSchema": ch.config_schema(),
            })
        }))
        .collect();
    Ok(Json(ApiResp::ok(json!({ "channels": channels }))))
}

// ———————— 规则预演（编辑弹框「预览命中定义」） ———————

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReq {
    /// 待预演的规则集（未保存草稿；同 save 校验口径，但 groupIds 不做存在性硬拒——
    /// 死组标注返回，便于先建规则后建分组的编排顺序）。
    #[serde(default)]
    rules: Vec<SubRule>,
    /// 样例事件（定义 key + 事件类型）→ 命中规则判定（不传则跳过）。
    #[serde(default)]
    sample: Option<PreviewSample>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSample {
    definition_key: String,
    #[serde(default)]
    event_type: Option<String>,
}

/// 每规则样例定义数上限（预演是可视化辅助，不是全量报表）。
const PREVIEW_SAMPLE_CAP: usize = 50;

/// POST /event-subscribers/rules/preview：规则集 → 命中定义清单（后端权威、按定义表
/// 实数据演算；死组标注）+ 样例事件命中判定。与 emit/rebuild 共用同一 matcher。
pub async fn preview_rules(Json(req): Json<PreviewReq>) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let db_id = db();
    let groups = event_store::get_groups(&db_id).await.map_err(msg_err)?;
    let known: HashMap<i64, String> = groups.iter().cloned().collect();
    // 规则结构校验（groupIds 存在性降级为标注——死组返回 deadGroupIds 便于先建规则后建分组）。
    let rules = validate_rules(&req.rules, &known, false)?;
    // 定义表实数据（key + group_id）——预演的权威数据源。
    let ds = cmx_database_pg::query_sql_with_params(
        &db_id,
        None,
        "SELECT key, group_id FROM cmx_flow_definition",
        cmx_database_pg::SqlParams::DataValues(vec![]),
        "evt_preview_defs",
    )
    .await
    .map_err(|e| msg_err(format!("查定义失败: {e}")))?;
    let schema = ds.schema.as_ref();
    let mut defs: Vec<(String, Option<i64>)> = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        let key = match row.get_by_name(schema, "key") {
            Some(cmx_core::model::cell::DataValue::String(s)) => s.clone(),
            Some(cmx_core::model::cell::DataValue::ShortStr(s))
            | Some(cmx_core::model::cell::DataValue::LongStr(s)) => s.to_string(),
            _ => String::new(),
        };
        let gid = row.get_by_name(schema, "group_id").and_then(|v| match v {
            cmx_core::model::cell::DataValue::Int(i) => Some(*i),
            _ => None,
        });
        defs.push((key, gid));
    }
    // 逐规则演算（与 emit 同一 rule 判定路径——单规则展开计数）。
    let rule_results: Vec<Value> = rules
        .iter()
        .map(|r| {
            let dead_groups: Vec<i64> = r.group_ids.iter().filter(|g| !known.contains_key(g)).copied().collect();
        let mut sample_keys = Vec::new();
        let mut matched = 0i64;
        for (key, gid) in &defs {
            // 事件维演算：规则事件集任一命中即算（事件集为空 = 不限，任取代表事件——
            // 三维 AND 中事件维恒真，匹配结果只取决于分组/key 维）。固定演算单一事件
            // 会让任务级规则（task.created 等）恒 0 命中，误导预览。
            let hit = if r.event_types.is_empty() {
                crate::event_outbox::rule_matches(r, "instance.started", Some(key), *gid)
            } else {
                r.event_types
                    .iter()
                    .any(|et| crate::event_outbox::rule_matches(r, et, Some(key), *gid))
            };
            if hit {
                matched += 1;
                if sample_keys.len() < PREVIEW_SAMPLE_CAP {
                    sample_keys.push(key.clone());
                }
            }
        }
            json!({
                "name": r.name,
                "enabled": r.enabled,
                "matchedCount": matched,
                "sampleKeys": sample_keys,
                "deadGroupIds": dead_groups,
            })
        })
        .collect();
    // 样例事件命中（事件类型缺省 = 全部——以 instance.started 演算事件维不限的场景无意义，
    // 故样例必填事件类型才有判定意义；缺省时跳过）。
    let sample_match = req.sample.as_ref().map(|s| {
        let gid = defs
            .iter()
            .find(|(k, _)| k == &s.definition_key)
            .and_then(|(_, g)| *g);
        let event_type = s.event_type.clone().unwrap_or_else(|| "instance.started".into());
        let matched = match_rules(&rules, &event_type, Some(&s.definition_key), gid);
        json!({
            "definitionKey": s.definition_key,
            "eventType": event_type,
            "matched": matched.is_some(),
            "ruleName": matched,
        })
    });
    Ok(Json(ApiResp::ok(json!({
        "groups": groups.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "rules": rule_results,
        "sample": sample_match,
    }))))
}

// ———————— 测试投递 ———————

/// 同订阅者 1 分钟窗口至多 3 次（副本本地计数；多副本上限 ≈ N×3，可接受）。
fn test_rate_limit(tenant: &str, sub_id: i64) -> Result<()> {
    type RateMap = HashMap<(String, i64), Vec<Instant>>;
    static LIMITS: std::sync::Mutex<Option<RateMap>> = std::sync::Mutex::new(None);
    let mut guard = LIMITS.lock().expect("test 限流锁中毒");
    let map = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    let window = Duration::from_secs(60);
    let key = (tenant.to_string(), sub_id);
    let stamps = map.entry(key).or_default();
    stamps.retain(|t| now.duration_since(*t) < window);
    if stamps.len() >= 3 {
        return Err(msg_err("测试投递限流：同订阅者 1 分钟内至多 3 次".into()));
    }
    stamps.push(now);
    Ok(())
}

/// POST /event-subscribers/test：真实投递一条伪事件（**不校验规则**——文案向用户澄清：
/// 测试验证的是回调连通与签名配置，不是规则命中），结果落审计行（source='test'，直达
/// 终态不参与保序）并同步返回。
///
/// 审计行 state 恒 DONE（失败也 DONE + last_error/http_status 留痕）：DEAD 是死信工作态，
/// 测试失败行进 DEAD 会污染死信队列。
pub async fn test_subscriber(Json(req): Json<IdReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let tenant = crate::tenant::current_tenant();
    test_rate_limit(&tenant, req.id)?;
    let sub = event_store::get_subscriber(&db(), &tenant, req.id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err(format!("订阅者不存在: {}", req.id)))?;
    let delivery_id = format!("test-{}", uuid::Uuid::new_v4());
    let payload = json!({
        "event": "webhook.test",
        "instanceId": "__event_test__",
        "subscriberId": sub.id,
        "subscriberName": sub.name,
        "message": "CMX flow 事件订阅测试投递（管理端点；不校验规则，仅验证回调连通与签名）",
        "occurredAt": chrono::Utc::now().to_rfc3339(),
    });
    let task = DeliveryTask {
        subscription_name: sub.name.clone(),
        event_type: "webhook.test".into(),
        definition_key: Some("__event_test__".into()),
        business_key: None,
        instance_id: "__event_test__".into(),
        delivery_id: delivery_id.clone(),
        payload: payload.clone(),
    };
    let outcome =
        dispatch_via_channel(&sub.channel, &sub.channel_config, task, Some(TEST_TIMEOUT)).await;
    let (success, http_status, error, snippet) = match &outcome {
        DeliveryOutcome::Success => (true, None, None, None),
        DeliveryOutcome::Retryable { http_status, error, snippet }
        | DeliveryOutcome::Fatal { http_status, error, snippet } => {
            (false, *http_status, Some(error.clone()), snippet.clone())
        }
    };
    // 审计行（直达终态 DONE；event_id 每次唯一——否则被 uk 吞掉留不下痕迹）。
    let audit = event_store::DeliveryInsert {
        subscriber_id: sub.id,
        subscriber_name: sub.name.clone(),
        channel: sub.channel.clone(),
        event_id: delivery_id.clone(),
        delivery_id: delivery_id.clone(),
        source: "test",
        event_type: "webhook.test".into(),
        definition_key: Some("__event_test__".into()),
        business_key: None,
        instance_id: "__event_test__".into(),
        payload,
        initial_state: "DONE",
        last_error: error.clone(),
        last_http_status: http_status.map(i64::from),
        last_response_snippet: snippet.clone(),
        delivered: success,
        matched_rule: None,
    };
    if let Err(e) = event_store::insert_deliveries(&db(), &[audit]).await {
        tracing::error!(error = %e, "测试投递审计行写入失败（不影响同步返回）");
    }
    Ok(Json(ApiResp::ok(json!({
        "success": success,
        "state": "DONE",
        "deliveryId": delivery_id,
        "httpStatus": http_status,
        "error": error,
        "responseSnippet": snippet,
    }))))
}

// ———————— 补发 rebuild ———————

/// 补发请求：按订阅者把时间窗内终态实例的「完成/终止」事件重放进投递管线。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildReq {
    /// 订阅者 name。
    name: String,
    /// 起始时刻（RFC3339，按 hi_instance.ended_at 过滤；可空 = 不限）。
    #[serde(default)]
    since: Option<String>,
    /// 截止时刻（可空 = 至今）。
    #[serde(default)]
    until: Option<String>,
}

/// POST /event-subscribers/rebuild —— 缺行补发。
///
/// 语义：对指定订阅者，扫时间窗内终态实例（hi 归档表），**按该订阅者的 rules 走与
/// emit 同一 matcher**，把 `instance.completed` / `instance.terminated` 事件以 PENDING
/// 投递行重放进既有管线（确定性 event_id `rb-` 前缀——重复 rebuild 幂等，uk 吞已存在行）。
/// 任务级事件（task.*）历史数据不全，不在补发范围。三段计数：scanned/matched/inserted。
pub async fn rebuild_subscriber(
    Json(req): Json<RebuildReq>,
) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let db_id = crate::engine::current_flow_db_id();
    let tenant = crate::tenant::current_tenant();
    let sub = event_store::get_subscriber_by_name(&db_id, &tenant, &req.name)
        .await
        .map_err(bad_err)?
        .ok_or_else(|| bad_err(format!("SUBSCRIBER_NOT_FOUND: 订阅者不存在: {}", req.name)))?;
    let since = parse_ts(req.since.as_deref());
    let until = parse_ts(req.until.as_deref());
    let mut conds: Vec<String> = vec![
        "state IN ('COMPLETED','TERMINATED')".to_string(),
        "ended_at IS NOT NULL".to_string(),
    ];
    let mut params: Vec<cmx_core::model::cell::DataValue> = Vec::new();
    if let Some(s) = since {
        params.push(cmx_core::model::cell::DataValue::DateTime(s));
        conds.push(format!("ended_at >= ${}", params.len()));
    }
    if let Some(u) = until {
        params.push(cmx_core::model::cell::DataValue::DateTime(u));
        conds.push(format!("ended_at <= ${}", params.len()));
    }
    let cond = conds.join(" AND ");
    let ds = cmx_database_pg::query_sql_with_params(
        &db_id,
        None,
        &format!(
            "SELECT id, definition_key, business_key, state, ended_at \
             FROM cmx_flow_hi_instance WHERE {cond} ORDER BY ended_at DESC LIMIT 2000"
        ),
        cmx_database_pg::SqlParams::DataValues(params),
        "flow_rebuild_scan",
    )
    .await
    .map_err(|e| msg_err(format!("扫描终态实例失败: {e}")))?;
    let schema = ds.schema.as_ref();
    let now = chrono::Utc::now();
    // 定义→分组映射（与 emit 同源：路由缓存快照）。
    let snap = event_store::route_snapshot_cached(&db_id, &tenant).await;
    let mut scanned = 0i64;
    let mut matched = 0i64;
    let mut rows: Vec<event_store::DeliveryInsert> = Vec::new();
    for row in ds.iter() {
        scanned += 1;
        let get_s = |col: &str| -> Option<String> {
            match row.get_by_name(schema, col) {
                Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
                _ => None,
            }
        };
        let ended_at = match row.get_by_name(schema, "ended_at") {
            Some(cmx_core::model::cell::DataValue::DateTime(dt)) => dt.to_rfc3339(),
            _ => now.to_rfc3339(),
        };
        let state = get_s("state").unwrap_or_default();
        let event_type = if state == "COMPLETED" { "instance.completed" } else { "instance.terminated" };
        let def_key = get_s("definition_key");
        let def_group = def_key.as_deref().and_then(|k| snap.def_group.get(k).copied());
        // 订阅过滤：与 emit 同一 matcher（禁第二份过滤逻辑）。
        if match_rules(&sub.rules, event_type, def_key.as_deref(), def_group).is_none() {
            continue;
        }
        matched += 1;
        let event = cmx_flow_adapters::FlowEvent {
            event: event_type.to_string(),
            instance_id: get_s("id").unwrap_or_default(),
            state: Some(state),
            definition_key: def_key.clone(),
            business_key: get_s("business_key"),
            task_id: None,
            node_bpmn_id: None,
            assignee: None,
            tenant: Some(tenant.clone()),
            system_id: None,
            occurred_at: ended_at,
        };
        let iid = get_s("id").unwrap_or_default();
        rows.push(event_store::DeliveryInsert {
            subscriber_id: sub.id,
            subscriber_name: sub.name.clone(),
            channel: sub.channel.clone(),
            // 确定性 event_id（重复 rebuild 幂等）：订阅者 id + instance + 事件缩码，
            // 64 字符限内（rb-19雪花-32iid-3缩码 = 59）。
            event_id: format!(
                "rb-{}-{}-{}",
                sub.id,
                iid.replace('-', ""),
                if event_type == "instance.completed" { "cmp" } else { "trm" }
            ),
            delivery_id: event.delivery_id(),
            source: "rebuild",
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
            matched_rule: None,
        });
    }
    let inserted = rows.len();
    if inserted > 0 {
        event_store::insert_deliveries(&db_id, &rows)
            .await
            .map_err(|e| msg_err(format!("补发行写入失败: {e}")))?;
    }
    tracing::info!(
        operator = crate::tenant::current_display_user().unwrap_or_else(|| "service".into()),
        subscriber = %sub.name, scanned, matched, inserted,
        "事件补发：终态事件重放进投递管线"
    );
    Ok(Json(ApiResp::ok(json!({
        "subscriber": sub.name,
        "scanned": scanned,
        "matched": matched,
        "inserted": inserted,
        "note": "补发行已按 PENDING 进入投递管线（同订阅者保序；重复补发幂等——接收方按 businessKey/occurredAt 幂等）",
    }))))
}

/// RFC3339 宽松解析（None/空/非法 → None；非法值以 warn 提示而非报错——补发是运维动作）。
fn parse_ts(v: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(dt) => Some(dt.with_timezone(&chrono::Utc)),
            Err(_) => {
                tracing::warn!(value = %s, "rebuild 时间参数非法（应为 RFC3339），已忽略");
                None
            }
        },
    }
}

// ———————— 投递流水端点 ———————

/// POST /event-deliveries/query：投递流水分页。
pub async fn query_deliveries(Json(req): Json<DlvQueryReq>) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let filter = DlvFilter {
        subscriber_id: req.subscriber_id,
        state: req.state,
        channel: req.channel,
        definition_key: req.definition_key,
        matched_rule: req.matched_rule,
        page: req.page,
        page_size: req.page_size,
    };
    let (rows, total) = event_store::query_deliveries(&db(), &crate::tenant::current_tenant(), &filter)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "rows": rows, "total": total }))))
}

/// POST /event-deliveries/stats：投递统计 KPI（时间窗 + emit 口径成功率）+ emit 侧
/// 可观测计数（零命中/缺定义键/写失败）+ 旁路写失败计数（原 mon 端点口径并入此处）。
pub async fn delivery_stats(Json(req): Json<StatsReq>) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let mut stats = event_store::delivery_stats(
        &db(),
        &crate::tenant::current_tenant(),
        req.hours,
        req.subscriber_id,
    )
    .await
    .map_err(msg_err)?;
    if let Some(obj) = stats.as_object_mut() {
        obj.insert("emitCounters".into(), crate::event_outbox::emit_counters());
        obj.insert(
            "sideEffectErrors".into(),
            json!(crate::handlers::SIDE_EFFECT_ERRORS.load(std::sync::atomic::Ordering::Relaxed)),
        );
    }
    Ok(Json(ApiResp::ok(stats)))
}

/// POST /event-deliveries/retry：死信重发（DEAD / 租约过期的 IN_FLIGHT → PENDING，
/// attempts 归零 = 人工显式重发重置完整重试预算）。
pub async fn retry_deliveries(Json(req): Json<RetryDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    if req.ids.is_empty() && req.subscriber_id.is_none() && req.state.is_none() {
        return Err(msg_err("须提供 ids 或 subscriberId/state 过滤（防全表误重发）".into()));
    }
    let n = event_store::retry_deliveries(
        &db(),
        &crate::tenant::current_tenant(),
        &req.ids,
        req.subscriber_id,
        req.state.as_deref(),
    )
    .await
    .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "reset": n }))))
}

/// POST /event-deliveries/skip：处置（DEAD/PENDING → SKIPPED，人工确认放弃的显式留痕）。
pub async fn skip_deliveries(Json(req): Json<SkipDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    if req.ids.is_empty() {
        return Err(msg_err("须提供 ids".into()));
    }
    let n = event_store::skip_deliveries(&db(), &crate::tenant::current_tenant(), &req.ids)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "skipped": n }))))
}

/// POST /event-deliveries/purge：清理 DONE/SKIPPED 行（beforeDays 默认 7）。
pub async fn purge_deliveries(Json(req): Json<PurgeDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let n = event_store::purge_deliveries(&db(), req.before_days, req.state.as_deref())
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "purged": n }))))
}

// ———————— 流程分组端点 ———————

/// GET /definition-groups：分组列表（含每组定义数、启停标注）。分组量级极小不分页。
pub async fn list_groups() -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let rows = event_store::list_groups(&db()).await.map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "rows": rows }))))
}

/// POST /definition-groups/save：新建/更新（name ≤64；排序即改 sortNo；启停只影响
/// 定义页展示位，不参与运行时匹配）。
pub async fn save_group(Json(req): Json<GroupSaveReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(msg_err("分组名必填且 ≤64 字符".into()));
    }
    // 重名预检（DB uk 兜底并发窗口）。
    let groups = event_store::get_groups(&db()).await.map_err(msg_err)?;
    if let Some((_, existing)) = groups.iter().find(|(gid, gname)| gname == name && Some(*gid) != req.id) {
        let _ = existing;
        return Err(bad_err(format!("GROUP_NAME_EXISTS: 分组名已存在: {name}")));
    }
    let id = event_store::upsert_group(
        &db(),
        &GroupUpsert {
            id: req.id,
            name: name.to_string(),
            sort_no: req.sort_no.unwrap_or(0),
            enabled: req.enabled,
            remark: req.remark.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
        },
    )
    .await
    .map_err(bad_err)?;
    Ok(Json(ApiResp::ok(json!({ "id": id }))))
}

/// POST /definition-groups/delete：删除（组内有定义 → 400；单条 SQL 守卫防 TOCTOU）。
pub async fn delete_group(Json(req): Json<IdReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    event_store::delete_group(&db(), req.id).await.map_err(bad_err)?;
    Ok(Json(ApiResp::ok(json!({ "deleted": req.id }))))
}

// ———————— 定义分组查询端点（定义管理页数据源） ———————

/// POST /definitions/query：定义分页（keyword=key/name 模糊、state、groupId/ungrouped；
/// 附分组名）。定义量级小但此页是主入口，分页口径与其它列表页一致。
pub async fn query_definitions(Json(req): Json<DefQueryReq>) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let (page, size) = (req.page.max(1), req.page_size.clamp(1, 200));
    let mut params: Vec<cmx_core::model::cell::DataValue> = Vec::new();
    let mut pn = 0;
    let mut cond = String::new();
    if let Some(kw) = req.keyword.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        pn += 1;
        let kw = kw
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        cond.push_str(&format!(
            " AND (lower(d.key) LIKE ${pn} ESCAPE '\\' OR lower(d.name) LIKE ${pn} ESCAPE '\\')"
        ));
        params.push(cmx_core::model::cell::DataValue::String(format!("%{kw}%")));
    }
    if let Some(st) = req.state.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND d.state = ${pn}"));
        params.push(cmx_core::model::cell::DataValue::String(st.trim().to_uppercase()));
    }
    if let Some(gid) = req.group_id {
        pn += 1;
        cond.push_str(&format!(" AND d.group_id = ${pn}"));
        params.push(cmx_core::model::cell::DataValue::Int(gid));
    } else if req.ungrouped == Some(true) {
        cond.push_str(" AND d.group_id IS NULL");
    }
    let total = query_one_i64(
        &db(),
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_definition d WHERE 1=1{cond}"),
        &mut params.clone(),
    )
    .await?;
    let offset = (page - 1) * size;
    let mut list_params = params;
    let sql = format!(
        "SELECT d.key, d.name, d.state, d.active_version, d.group_id, g.name AS group_name, \
                d.domain, d.application, d.module, d.category, d.updated_at, d.updated_by \
         FROM cmx_flow_definition d LEFT JOIN cmx_flow_def_group g ON g.id = d.group_id \
         WHERE 1=1{cond} ORDER BY d.updated_at DESC LIMIT {size} OFFSET {offset}"
    );
    let ds = cmx_database_pg::query_sql_with_params(
        &db(),
        None,
        &sql,
        cmx_database_pg::SqlParams::DataValues(std::mem::take(&mut list_params)),
        "evt_def_query",
    )
    .await
    .map_err(|e| msg_err(format!("查定义失败: {e}")))?;
    let schema = ds.schema.as_ref();
    let get_s = |row: &cmx_core::model::data::dataset::Row, col: &str| -> String {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::String(s)) => s.clone(),
            Some(cmx_core::model::cell::DataValue::ShortStr(s))
            | Some(cmx_core::model::cell::DataValue::LongStr(s)) => s.to_string(),
            _ => String::new(),
        }
    };
    let get_o = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
            Some(cmx_core::model::cell::DataValue::ShortStr(s))
            | Some(cmx_core::model::cell::DataValue::LongStr(s)) => Some(s.to_string()),
            _ => None,
        }
    };
    let get_i = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<i64> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
            _ => None,
        }
    };
    let get_ts = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::DateTime(dt)) => Some(dt.to_rfc3339()),
            Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let rows: Vec<Value> = ds
        .iter()
        .map(|row| {
            json!({
                "key": get_s(row, "key"),
                "name": get_s(row, "name"),
                "state": get_s(row, "state"),
                "activeVersion": get_i(row, "active_version"),
                "groupId": get_i(row, "group_id"),
                "groupName": get_o(row, "group_name"),
                "domain": get_o(row, "domain"),
                "application": get_o(row, "application"),
                "module": get_o(row, "module"),
                "category": get_o(row, "category"),
                "updatedAt": get_ts(row, "updated_at"),
                "updatedBy": get_o(row, "updated_by"),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "rows": rows, "total": total }))))
}

/// POST /definitions/set-group：批量设置定义分组（keys 非空；groupId null = 移出分组）。
///
/// **硬约束**：UPDATE 必带 updated_at = now()——路由缓存指纹含 definition 表
/// MAX(updated_at)，换组后跨副本 ≤5s 感知（不 bump 则指纹永远不变、路由持续用旧组）。
pub async fn set_definition_group(Json(req): Json<SetGroupReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let keys: Vec<String> = req
        .keys
        .iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if keys.is_empty() {
        return Err(msg_err("keys 不能为空".into()));
    }
    if keys.len() > 500 {
        return Err(msg_err("单批 ≤500 个定义".into()));
    }
    if let Some(gid) = req.group_id {
        let groups = event_store::get_groups(&db()).await.map_err(msg_err)?;
        if !groups.iter().any(|(id, _)| *id == gid) {
            return Err(bad_err(format!("GROUP_NOT_FOUND: 分组不存在: {gid}")));
        }
    }
    let keys_json = serde_json::to_string(&keys).map_err(|e| msg_err(e.to_string()))?;
    let sql = "UPDATE cmx_flow_definition \
               SET group_id = $1::bigint, updated_at = now() \
               WHERE key = ANY(SELECT k FROM jsonb_array_elements_text($2::jsonb) AS t(k))";
    let n = cmx_database_pg::execute_sql_with_params(
        &db(),
        None,
        sql,
        cmx_database_pg::SqlParams::DataValues(vec![
            match req.group_id {
                Some(g) => cmx_core::model::cell::DataValue::Int(g),
                None => cmx_core::model::cell::DataValue::NullTyped(cmx_core::model::cell::SqlTypeMarker::Int),
            },
            cmx_core::model::cell::DataValue::Json(keys_json),
        ]),
    )
    .await
    .map_err(|e| msg_err(format!("设置分组失败: {e}")))?;
    event_store::invalidate_route_cache();
    Ok(Json(ApiResp::ok(json!({ "updated": n }))))
}

/// 计数小件（definitions/query 用）。
async fn query_one_i64(
    db_id: &str,
    sql: &str,
    params: &mut Vec<cmx_core::model::cell::DataValue>,
) -> Result<i64> {
    let ds = cmx_database_pg::query_sql_with_params(
        db_id,
        None,
        sql,
        cmx_database_pg::SqlParams::DataValues(std::mem::take(params)),
        "evt_def_count",
    )
    .await
    .map_err(|e| msg_err(format!("计数失败: {e}")))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .next()
        .and_then(|row| match row.get_by_name(schema, "n") {
            Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_mask_roundtrip() {
        // 长密钥露前 4 后 4；短密钥整体打码；空串原样。
        assert_eq!(mask_secret("0123456789abcdef"), "0123******cdef");
        assert_eq!(mask_secret("short"), MASK);
        assert_eq!(mask_secret(""), "");
        // 掩码回传 / 留空 = 沿用旧值（None）；明文 = 换新。
        assert_eq!(resolve_incoming_secret(Some("")), None);
        assert_eq!(resolve_incoming_secret(Some("0123******cdef")), None);
        assert_eq!(
            resolve_incoming_secret(Some("brand-new-secret")),
            Some("brand-new-secret".to_string())
        );
        assert_eq!(resolve_incoming_secret(None), None);
    }

    #[tokio::test]
    async fn masked_config_keeps_other_keys() {
        let cfg = json!({ "service_key": "mdm", "callback_path": "/api/x", "secret": "0123456789abcdef", "future": 1 });
        let out = masked_config(&cfg);
        assert_eq!(out["secret"], json!("0123******cdef"));
        assert_eq!(out["service_key"], json!("mdm"));
        assert_eq!(out["future"], json!(1));
    }

    #[tokio::test]
    async fn event_type_validation() {
        assert!(validate_event_types(&["instance.started".into()]).is_ok());
        assert!(validate_event_types(&[]).is_ok());
        assert!(validate_event_types(&["nope".into()]).is_err());
    }

    #[test]
    fn rules_validation() {
        let groups: HashMap<i64, String> = [(7i64, "审批".to_string())].into_iter().collect();
        let mk = |name: &str, pats: Vec<&str>| SubRule {
            name: name.into(),
            enabled: true,
            event_types: vec![],
            group_ids: vec![],
            key_patterns: pats.into_iter().map(String::from).collect(),
        };
        // 正常 + trim 归一。
        let ok = validate_rules(&[mk("  r1 ", vec![" mdm_* "])], &groups, true).unwrap();
        assert_eq!(ok[0].name, "r1");
        assert_eq!(ok[0].key_patterns, vec!["mdm_*"]);
        // 重名 / 长名 / 坏语法 / 未知分组 / 空模式。
        assert!(validate_rules(&[mk("a", vec![]), mk("a", vec![])], &groups, true).is_err());
        assert!(validate_rules(&[mk(&"x".repeat(65), vec![])], &groups, true).is_err());
        assert!(validate_rules(&[mk("a", vec!["mdm_?x"])], &groups, true).is_err());
        assert!(validate_rules(&[mk("a", vec!["mdm_[12]"])], &groups, true).is_err());
        assert!(validate_rules(&[mk("a", vec![])], &groups, true).is_ok());
        let bad_group = SubRule { group_ids: vec![999], ..mk("a", vec![]) };
        // 严格模式拒未知分组；宽容模式（preview）放行。
        assert!(validate_rules(&[bad_group.clone()], &groups, true).is_err());
        assert!(validate_rules(&[bad_group], &groups, false).is_ok());
        let empty_pat = mk("a", vec!["  "]);
        assert!(validate_rules(&[empty_pat], &groups, true).is_err());
    }
}
