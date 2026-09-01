//! 出站 webhook 管理端点（001 方案 §五：11 端点随 M1，rebuild 随 M3）。
//!
//! 契约纪律：POST + JSON body、无 Path Variable、无 PUT/PATCH/DELETE；GET 仅详情/通道
//! 目录（简单只读）。**鉴权 fail-close**（对齐 complete_task T0b）：auth 中间件未挂或
//! off 形态下**写端点一律拒绝**，不静默放行；写操作挂占位角色常量 `flow-webhook-admin`
//! （M3 对齐平台角色体系时再映射）。openapi.rs ENDPOINTS 同步收录。
//!
//! secret 防线（照搬 mdm mask/keep 模式）：API 永不回显明文（掩码）、编辑回传掩码值
//! 或留空 = 沿用旧值；channel_config 为**开放对象**——按通道校验必填键、不拒额外键。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Query;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::resp::{ApiResp, FlowError, Result};

use cmx_flow_adapters::{DeliveryOutcome, DeliveryTask, global_registry};

use crate::webhook_outbox::dispatch_via_channel;
use crate::webhook_store::{self, DlvFilter, SubFilter, SubUpsert};

/// 占位角色常量：写操作的目标角色（M1 仅 fail-close + 常量登记；M3 对齐平台角色体系时映射）。
pub const WEBHOOK_ADMIN_ROLE: &str = "flow-webhook-admin";

/// 测试投递短超时（不等基座 30s 键级超时）。
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// ———————— 鉴权门 ————————

/// 写端点 fail-close：auth 中间件未挂/off 形态即拒绝（001 方案 §五；安全不排 M3）。
fn ensure_auth_gate() -> Result<()> {
    if !crate::auth::auth_middleware_active() {
        return Err(FlowError::business_error(
            "webhook 管理写端点要求 auth 中间件生效（fail-close）",
        ));
    }
    Ok(())
}

// ———————— secret 掩码（mdm mask/keep 同款） ————————

const MASK: &str = "******";

/// 掩码：短密钥整体打码，长密钥露前 4 后 4。
fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    if s.len() <= 8 {
        MASK.to_string()
    } else {
        format!("{}{MASK}{}", &s[..4], &s[s.len() - 4..])
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

// ———————— DTO ————————

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
    #[serde(default = "default_channel")]
    channel: String,
    /// 通道配置开放对象（webhook：service_key / callback_path / secret；键值域见 channels 端点）。
    #[serde(default)]
    channel_config: Value,
    #[serde(default)]
    definition_keys: Vec<String>,
    #[serde(default)]
    event_types: Vec<String>,
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
    subscription_id: Option<i64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    page: i64,
    #[serde(default = "default_page_size")]
    page_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryDlvReq {
    #[serde(default)]
    ids: Vec<i64>,
    #[serde(default)]
    subscription_id: Option<i64>,
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

// ———————— 公共小件 ————————

fn db() -> String {
    crate::engine::current_flow_db_id()
}

fn msg_err(e: String) -> FlowError {
    FlowError::business_error(e)
}

/// 合法事件类型（6 种生命周期事件；管理页推荐集在页面侧定义）。
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

// ———————— 订阅端点 ————————

/// POST /webhook-subscriptions/query：分页列表（secret 掩码）。
pub async fn query_subscriptions(
    Json(req): Json<SubQueryReq>,
) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?; // 确保运行时/建表就绪（fresh boot 首访兜底）
    let filter = SubFilter {
        keyword: req.keyword,
        channel: req.channel,
        active: req.active,
        page: req.page,
        page_size: req.page_size,
    };
    let (rows, total) = webhook_store::list_subscriptions(&db(), &crate::tenant::current_tenant(), &filter)
        .await
        .map_err(msg_err)?;
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            if let Some(cfg) = r.get_mut("channelConfig") {
                *cfg = masked_config(cfg);
            }
            r
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "rows": rows, "total": total }))))
}

/// GET /webhook-subscriptions/detail?id=：详情（secret 掩码）。
pub async fn get_subscription_detail(
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let id: i64 = params
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| msg_err("缺参数 id".into()))?;
    let sub = webhook_store::get_subscription(&db(), &crate::tenant::current_tenant(), id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err(format!("订阅不存在: {id}")))?;
    Ok(Json(ApiResp::ok(sub_detail_json(&sub))))
}

fn sub_detail_json(
    sub: &webhook_store::SubRow,
) -> Value {
    json!({
        "id": sub.id,
        "name": sub.name,
        "channel": sub.channel,
        "channelConfig": masked_config(&sub.channel_config),
        "definitionKeys": sub.definition_keys,
        "eventTypes": sub.event_types,
        "active": sub.active,
        "retryMax": sub.retry_max,
        "source": sub.source,
        "tenantId": sub.tenant_id,
        "createdBy": sub.created_by,
    })
}

/// POST /webhook-subscriptions/save：新建/更新（channel 校验注册表；secret 掩码回传 = 沿用旧值）。
pub async fn save_subscription(Json(req): Json<SaveSubReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err(msg_err("订阅名必填且 ≤128 字符".into()));
    }
    // 通道必须在注册表（未启用 feature 的通道拒绝建订阅——防建出永远重试的死订阅）。
    let channel = global_registry()
        .get(&req.channel)
        .ok_or_else(|| msg_err(format!("通道 {} 未注册（未启用）", req.channel)))?;
    validate_event_types(&req.event_types)?;
    let retry_max = req.retry_max.unwrap_or(10).clamp(1, 50) as i32;

    // secret 沿用语义：编辑时留空/掩码回传 = 保留旧值（先取旧行）。
    let old = match req.id {
        Some(id) => {
            webhook_store::get_subscription(&db(), &crate::tenant::current_tenant(), id)
                .await
                .map_err(msg_err)?
                .ok_or_else(|| msg_err(format!("订阅不存在: {id}")))?
        }
        None => webhook_store::SubRow {
            id: 0,
            name: String::new(),
            channel: String::new(),
            channel_config: json!({}),
            definition_keys: vec![],
            event_types: vec![],
            active: true,
            retry_max: 10,
            source: "manual".into(),
            tenant_id: String::new(),
            created_by: None,
        },
    };
    let mut config = req.channel_config.clone();
    if config.get("secret").is_none() || resolve_incoming_secret(config.get("secret").and_then(Value::as_str)).is_none() {
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

    let id = webhook_store::upsert_subscription(
        &db(),
        &crate::tenant::current_tenant(),
        &SubUpsert {
            id: req.id,
            name: name.to_string(),
            channel: req.channel.clone(),
            channel_config: config,
            definition_keys: req.definition_keys,
            event_types: req.event_types,
            active: req.active.unwrap_or(true),
            retry_max,
            created_by: crate::tenant::current_user(),
        },
    )
    .await
    .map_err(msg_err)?;
    let sub = webhook_store::get_subscription(&db(), &crate::tenant::current_tenant(), id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err("保存后回读失败".into()))?;
    Ok(Json(ApiResp::ok(json!({ "id": id, "subscription": sub_detail_json(&sub) }))))
}

/// POST /webhook-subscriptions/delete：物理删，仅停用态可删（流水凭 name 快照保审计）。
pub async fn delete_subscription(Json(req): Json<IdReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    webhook_store::delete_subscription(&db(), &crate::tenant::current_tenant(), req.id)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "deleted": req.id }))))
}

/// POST /webhook-subscriptions/set-active：启停（停用即不再生成新投递行）。
pub async fn set_subscription_active(Json(req): Json<SetActiveReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    webhook_store::set_subscription_active(&db(), &crate::tenant::current_tenant(), req.id, req.active)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "id": req.id, "active": req.active }))))
}

/// GET /webhook-subscriptions/channels：注册表当前可用通道 + channel_config schema
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

// ———————— 测试投递 ————————

/// 同订阅 1 分钟窗口至多 3 次（副本本地计数；多副本上限 ≈ N×3，方案 §5 已声明可接受）。
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
        return Err(msg_err("测试投递限流：同订阅 1 分钟内至多 3 次".into()));
    }
    stamps.push(now);
    Ok(())
}

/// POST /webhook-subscriptions/test：真实投递一条伪事件（不走过滤规则——配了过滤的订阅
/// 也能收到），结果落审计行（source='test'，直达终态不参与保序）并同步返回。
///
/// 审计行 state 恒 DONE（失败也 DONE + last_error/http_status 留痕）：DEAD 是死信工作态
/// （会被 retry/skip/purge 当业务死信处理），测试失败行进 DEAD 会污染死信队列——方案 §五 定稿口径。
pub async fn test_subscription(Json(req): Json<IdReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let tenant = crate::tenant::current_tenant();
    test_rate_limit(&tenant, req.id)?;
    let sub = webhook_store::get_subscription(&db(), &tenant, req.id)
        .await
        .map_err(msg_err)?
        .ok_or_else(|| msg_err(format!("订阅不存在: {}", req.id)))?;
    let delivery_id = format!("test-{}", uuid::Uuid::new_v4());
    let payload = json!({
        "event": "webhook.test",
        "instanceId": "__webhook_test__",
        "subscriptionId": sub.id,
        "subscriptionName": sub.name,
        "message": "CMX flow webhook 订阅测试投递（管理端点，不参与业务事件路由）",
        "occurredAt": chrono::Utc::now().to_rfc3339(),
    });
    let task = DeliveryTask {
        subscription_name: sub.name.clone(),
        event_type: "webhook.test".into(),
        definition_key: Some("__webhook_test__".into()),
        business_key: None,
        instance_id: "__webhook_test__".into(),
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
    let audit = webhook_store::DeliveryInsert {
        subscription_id: sub.id,
        subscription_name: sub.name.clone(),
        channel: sub.channel.clone(),
        event_id: delivery_id.clone(),
        delivery_id: delivery_id.clone(),
        source: "test",
        event_type: "webhook.test".into(),
        definition_key: Some("__webhook_test__".into()),
        business_key: None,
        instance_id: "__webhook_test__".into(),
        payload,
        initial_state: "DONE",
        last_error: error.clone(),
        last_http_status: http_status.map(i64::from),
        last_response_snippet: snippet.clone(),
        delivered: success,
    };
    if let Err(e) = webhook_store::insert_deliveries(&db(), &[audit]).await {
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

// ———————— 投递流水端点 ————————

/// POST /webhook-deliveries/query：投递流水分页。
pub async fn query_deliveries(Json(req): Json<DlvQueryReq>) -> Result<Json<ApiResp<Value>>> {
    let _ = crate::engine::flow().await?;
    let filter = DlvFilter {
        subscription_id: req.subscription_id,
        state: req.state,
        channel: req.channel,
        definition_key: req.definition_key,
        page: req.page,
        page_size: req.page_size,
    };
    let (rows, total) = webhook_store::query_deliveries(&db(), &crate::tenant::current_tenant(), &filter)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "rows": rows, "total": total }))))
}

/// POST /webhook-deliveries/retry：死信重发（DEAD / 租约过期的 IN_FLIGHT → PENDING，
/// attempts 归零 = 人工显式重发重置完整重试预算）。
pub async fn retry_deliveries(Json(req): Json<RetryDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    if req.ids.is_empty() && req.subscription_id.is_none() && req.state.is_none() {
        return Err(msg_err("须提供 ids 或 subscriptionId/state 过滤（防全表误重发）".into()));
    }
    let n = webhook_store::retry_deliveries(
        &db(),
        &crate::tenant::current_tenant(),
        &req.ids,
        req.subscription_id,
        req.state.as_deref(),
    )
    .await
    .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "reset": n }))))
}

/// POST /webhook-deliveries/skip：死信处置 DEAD → SKIPPED（人工确认放弃的显式留痕）。
pub async fn skip_deliveries(Json(req): Json<SkipDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    if req.ids.is_empty() {
        return Err(msg_err("须提供 ids".into()));
    }
    let n = webhook_store::skip_deliveries(&db(), &crate::tenant::current_tenant(), &req.ids)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "skipped": n }))))
}

/// POST /webhook-deliveries/purge：清理 DONE/SKIPPED 行（beforeDays 默认 7）。
pub async fn purge_deliveries(Json(req): Json<PurgeDlvReq>) -> Result<Json<ApiResp<Value>>> {
    ensure_auth_gate()?;
    let _ = crate::engine::flow().await?;
    let n = webhook_store::purge_deliveries(
        &db(),
        &crate::tenant::current_tenant(),
        req.before_days,
        req.state.as_deref(),
    )
    .await
    .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "purged": n }))))
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
}
