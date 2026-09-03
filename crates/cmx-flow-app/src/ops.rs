//! 运维面端点（技术债 012/013，治理方案批次 6）。
//!
//! - `POST /jobs/query`：定时器作业清单（cmx_flow_job 跨实例视图；012——此前无任何 job 管理端点）；
//! - `POST /incidents/query`：跨实例故障清单（011——incident 不再埋在 variables 里翻 100 条列表）；
//! - `GET  /metrics`：Prometheus text 格式业务指标（013——webhook/incident/死信/业务失败计数，
//!   投递流水表天然是成功率指标源，不建重复计数器）。
//!
//! 端点全部合规（POST + JSON body 分页，无 Path Variable / PUT-PATCH-DELETE）；查询全参数化。

use axum::Json;
use crate::resp::{ApiResp, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::engine::{current_flow_db_id, flow};
use crate::resp::FlowError;

fn msg_err(msg: String) -> FlowError {
    // 013：业务失败统一计数（Prometheus /metrics 暴露）。
    crate::ops::BUSINESS_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FlowError::business_error(msg)
}

/// 013：业务失败计数（HTTP 200 + code=1 信封语义的业务错误在 /metrics 以
/// `cmx_flow_business_errors_total` 暴露——补请求遥测只有 HTTP 状态码的盲区）。
pub static BUSINESS_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 实例分页查询参数（技术债 016：GET /instances 全表 100 条无过滤——新增合规端点
/// POST + body 分页；存量端点不动）。复用 biz_link 的 paged 查询（含 005 system 过滤）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstancesQuery {
    /// 按流程定义 key 过滤。
    #[serde(default)]
    definition_key: Option<String>,
    /// 按实例状态过滤（ACTIVE/COMPLETED/TERMINATED）。
    #[serde(default)]
    state: Option<String>,
    /// 按发起人过滤（匹配实例变量 initiator）。
    #[serde(default)]
    initiator: Option<String>,
    /// 关键字（单号/流程名模糊）。
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
}

/// POST /instances/query —— 实例清单分页查询（016）。
pub async fn query_instances(Json(req): Json<InstancesQuery>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    // X2-3（S-04）：initiator 是可选请求参数——走 effective_query_user（服务端身份优先；
    // 非 admin 传他人 → 403，admin 显式传 = 真代查），不再「传谁查谁」。
    let initiator = crate::handlers::effective_query_user(req.initiator.as_deref())?;
    let f = crate::biz_link::TodoFilter {
        keyword: req.keyword,
        definition_key: req.definition_key,
        node_bpmn_id: None,
        state: req.state,
        initiator,
        system_id: crate::tenant::current_system(),
        page: req.page.unwrap_or(1),
        page_size: req.page_size.unwrap_or(20),
    };
    let page = crate::biz_link::list_instances_paged(&f)
        .await
        .map_err(msg_err)?;
    let defs = rt.definitions.read().await;
    let items: Vec<Value> = page
        .rows
        .iter()
        .map(|t| crate::handlers::raw_todo_json(t, &defs))
        .collect();
    let (pno, psize) = f.norm();
    Ok(Json(ApiResp::ok(json!({
        "instances": items, "total": page.total, "page": pno, "pageSize": psize,
    }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobsQuery {
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
}

/// POST /jobs/query —— 定时器作业清单（012：多副本部署前 jobs 可见性）。
pub async fn query_jobs(Json(req): Json<JobsQuery>) -> Result<Json<ApiResp<Value>>> {
    let page = req.page.unwrap_or(1).max(1);
    let size = req.page_size.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * size;
    let db = current_flow_db_id();
    let mut conds: Vec<String> = vec!["1=1".to_string()];
    let mut params: Vec<cmx_core::model::cell::DataValue> = Vec::new();
    if let Some(iid) = req.instance_id.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(cmx_core::model::cell::DataValue::String(iid.to_string()));
        conds.push(format!("j.instance_id = ${}", params.len()));
    }
    params.push(cmx_core::model::cell::DataValue::Int(size));
    let limit_n = params.len();
    params.push(cmx_core::model::cell::DataValue::Int(offset));
    let offset_n = params.len();
    let cond = conds.join(" AND ");
    let ds = cmx_database_pg::query_sql_with_params(
        &db,
        None,
        &format!(
            "SELECT j.id, j.instance_id, j.boundary_bpmn_id AS node_bpmn_id, j.kind, j.due_at, i.state, \
                    j.claimed_by, j.lease_expires_at, i.definition_key, i.business_key \
             FROM cmx_flow_job j JOIN cmx_flow_instance i ON i.id = j.instance_id \
             WHERE {cond} ORDER BY j.due_at ASC LIMIT ${limit_n} OFFSET ${offset_n}"
        ),
        cmx_database_pg::SqlParams::DataValues(params.clone()),
        "flow_jobs_query",
    )
    .await
    .map_err(|e| msg_err(format!("查询定时器作业失败: {e}")))?;
    // 计数（复用同条件）。
    let mut cparams = params.clone();
    cparams.truncate(limit_n - 1);
    let total_ds = cmx_database_pg::query_sql_with_params(
        &db,
        None,
        &format!(
            "SELECT COUNT(*) AS n FROM cmx_flow_job j JOIN cmx_flow_instance i ON i.id = j.instance_id WHERE {cond}"
        ),
        cmx_database_pg::SqlParams::DataValues(cparams),
        "flow_jobs_count",
    )
    .await
    .map_err(|e| msg_err(format!("统计定时器作业失败: {e}")))?;
    let total = total_ds
        .iter()
        .next()
        .and_then(|row| match row.get_by_name(total_ds.schema.as_ref(), "n") {
            Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(0);
    let schema = ds.schema.as_ref();
    let get = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let get_ts = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::DateTime(dt)) => Some(dt.to_rfc3339()),
            _ => None,
        }
    };
    let jobs: Vec<Value> = ds
        .iter()
        .map(|row| {
            json!({
                "jobId": get(row, "id"),
                "instanceId": get(row, "instance_id"),
                "definitionKey": get(row, "definition_key"),
                "businessKey": get(row, "business_key"),
                "nodeBpmnId": get(row, "node_bpmn_id"),
                "kind": get(row, "kind"),
                "dueAt": get_ts(row, "due_at"),
                "state": get(row, "state"),
                "claimedBy": get(row, "claimed_by"),
                "leaseExpiresAt": get_ts(row, "lease_expires_at"),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "jobs": jobs, "total": total, "page": page, "pageSize": size,
    }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentsQuery {
    /// OPEN / RESOLVED；空 = 全部。
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
}

/// POST /incidents/query —— 跨实例故障清单（011）。
pub async fn query_incidents(Json(req): Json<IncidentsQuery>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let _ = rt;
    let page = req.page.unwrap_or(1).max(1);
    let size = req.page_size.unwrap_or(50).clamp(1, 200);
    let db = current_flow_db_id();
    let mut conds: Vec<String> = vec!["1=1".to_string()];
    let mut params: Vec<cmx_core::model::cell::DataValue> = Vec::new();
    if let Some(st) = req.state.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(cmx_core::model::cell::DataValue::String(st.to_uppercase()));
        conds.push(format!("state = ${}", params.len()));
    }
    if let Some(dk) = req.definition_key.as_deref().filter(|s| !s.trim().is_empty()) {
        params.push(cmx_core::model::cell::DataValue::String(dk.to_string()));
        conds.push(format!("definition_key = ${}", params.len()));
    }
    let cond = conds.join(" AND ");
    let count_params = params.clone();
    let total_ds = cmx_database_pg::query_sql_with_params(
        &db,
        None,
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_incident WHERE {cond}"),
        cmx_database_pg::SqlParams::DataValues(count_params),
        "flow_incidents_count",
    )
    .await
    .map_err(|e| msg_err(format!("统计 incident 失败: {e}")))?;
    let total = total_ds
        .iter()
        .next()
        .and_then(|row| match row.get_by_name(total_ds.schema.as_ref(), "n") {
            Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(0);
    let offset = (page - 1) * size;
    params.push(cmx_core::model::cell::DataValue::Int(size));
    let limit_n = params.len();
    params.push(cmx_core::model::cell::DataValue::Int(offset));
    let offset_n = params.len();
    let ds = cmx_database_pg::query_sql_with_params(
        &db,
        None,
        &format!(
            "SELECT id, instance_id, node_bpmn_id, definition_key, business_key, reason, \
                    retries, state, created_at, updated_at \
             FROM cmx_flow_incident WHERE {cond} \
             ORDER BY updated_at DESC LIMIT ${limit_n} OFFSET ${offset_n}"
        ),
        cmx_database_pg::SqlParams::DataValues(params),
        "flow_incidents_query",
    )
    .await
    .map_err(|e| msg_err(format!("查询 incident 失败: {e}")))?;
    let schema = ds.schema.as_ref();
    let get = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let get_ts = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::DateTime(dt)) => Some(dt.to_rfc3339()),
            _ => None,
        }
    };
    let items: Vec<Value> = ds
        .iter()
        .map(|row| {
            json!({
                "instanceId": get(row, "instance_id"),
                "nodeBpmnId": get(row, "node_bpmn_id"),
                "definitionKey": get(row, "definition_key"),
                "businessKey": get(row, "business_key"),
                "reason": get(row, "reason"),
                "retries": get(row, "retries").and_then(|s| s.parse::<i64>().ok()),
                "state": get(row, "state"),
                "createdAt": get_ts(row, "created_at"),
                "updatedAt": get_ts(row, "updated_at"),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "incidents": items, "total": total, "page": page, "pageSize": size,
    }))))
}

/// GET /metrics —— Prometheus text 格式（013）。
///
/// 业务指标源：①投递流水表按状态聚合（成功率口径真源，不建重复计数器）；②incident OPEN 数；
/// ③死信数；④运行实例数；⑤emit 侧内存计数器（入队失败/零命中/缺定义键/旁路失败——
/// 随 /event-deliveries/stats 一并返回）。DB 聚合失败按 0 上报
/// （Prometheus 抓取不能 500；连续 0 值本身即异常信号）。
/// /metrics 端点包装（X3-13/O-12）：响应头带 Prometheus 文本格式版本参数
///（主流抓取器兼容裸 text/plain，此为规范对齐）。
pub async fn metrics_endpoint() -> axum::response::Response {
    let body = prometheus_metrics().await;
    axum::response::IntoResponse::into_response((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    ))
}

pub async fn prometheus_metrics() -> String {
    use std::sync::atomic::Ordering;
    let delivery_done = count_one("SELECT COUNT(*) AS n FROM cmx_flow_event_delivery WHERE state='DONE'", "m_done").await;
    let delivery_pending = count_one("SELECT COUNT(*) AS n FROM cmx_flow_event_delivery WHERE state IN ('PENDING','IN_FLIGHT')", "m_pend").await;
    let delivery_dead = count_one("SELECT COUNT(*) AS n FROM cmx_flow_event_delivery WHERE state='DEAD'", "m_dead").await;
    let delivery_skipped = count_one("SELECT COUNT(*) AS n FROM cmx_flow_event_delivery WHERE state='SKIPPED'", "m_skip").await;
    let incidents_open = count_one("SELECT COUNT(*) AS n FROM cmx_flow_incident WHERE state='OPEN'", "m_inc").await;
    let deadletters = count_one("SELECT COUNT(*) AS n FROM cmx_flow_deadletter_job", "m_dl").await;
    let running = count_one("SELECT COUNT(*) AS n FROM cmx_flow_instance WHERE state='ACTIVE'", "m_run").await;

    let mut out = String::new();
    let mut m = |name: &str, help: &str, val: i64| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {val}\n"));
    };
    m("cmx_flow_delivery_done", "事件投递成功行数（当前存量，随 retention 清理下降）", delivery_done);
    m("cmx_flow_delivery_pending", "事件待投/投递中行数", delivery_pending);
    m("cmx_flow_delivery_dead", "事件死信行数（重试耗尽）", delivery_dead);
    m("cmx_flow_delivery_skipped", "事件人工跳过行数", delivery_skipped);
    m("cmx_flow_incidents_open", "OPEN incident 数", incidents_open);
    m("cmx_flow_deadletter_jobs", "异步作业死信数", deadletters);
    m("cmx_flow_instances_running", "运行中实例数", running);
    let mut g = |name: &str, help: &str, val: u64| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {val}\n"));
    };
    // O-07：口径如实缩小——本计数仅覆盖 ops 查询端点（3 端点 6 类查询错误）；
    // 全服务信封级计数下沉随 013 另行（勿当全站业务失败率告警）。
    g("cmx_flow_business_errors_total", "ops 查询端点业务失败数（HTTP 200 + code=1 信封，非全站口径）", BUSINESS_ERRORS.load(Ordering::Relaxed));
    g("cmx_flow_outbox_insert_errors_total", "事件投递行入队失败次数", crate::event_outbox::OUTBOX_INSERT_ERRORS.load(Ordering::Relaxed));
    g("cmx_flow_emit_no_match_total", "事件零订阅命中次数（活跃订阅者存在但无规则命中）", crate::event_outbox::EMIT_NO_MATCH.load(Ordering::Relaxed));
    g("cmx_flow_emit_null_defkey_total", "事件缺 definitionKey 次数", crate::event_outbox::EMIT_NULL_DEFKEY.load(Ordering::Relaxed));
    g("cmx_flow_side_effect_errors_total", "旁路写失败次数（留痕/补偿）", crate::handlers::SIDE_EFFECT_ERRORS.load(Ordering::Relaxed));
    out
}

/// 单条 COUNT 聚合（失败回 0——Prometheus 抓取不能 500）。
async fn count_one(sql: &str, tag: &'static str) -> i64 {
    let ds = cmx_database_pg::query_sql(&current_flow_db_id(), None, sql, tag).await;
    match ds {
        Ok(ds) => ds
            .iter()
            .next()
            .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "n") {
                Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
                _ => None,
            })
            .unwrap_or(0),
        Err(_) => 0,
    }
}
