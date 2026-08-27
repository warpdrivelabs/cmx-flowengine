//! 流程引擎监控统计（大盘数据源）。
//!
//! `GET /api/flow/v1/stats` —— 一次聚合引擎全状态 → JSON，供根路径监控大盘（dashboard.rs）轮询。
//! 全部只读 SQL（走 cmx-database-pg query_sql），按当前租户库聚合（多租户下各租户各自一份）。
//!
//! 维度：概览计数（实例/任务/定义/用户）、实例按状态、任务待办/已办、活跃定义分布、待办 top 办理人、
//! 定时器作业、协作（抄送/意见/转签/子流程/会签）、近 30 天创建时间线、引擎运行时（租户/已装载定义）。

use axum::Json;
use axum::extract::Query;
use serde::Deserialize;
use serde_json::{Value, json};

use cmx_core::model::cell::DataValue;
use cmx_core::model::data::dataset::Row;
use cmx_database_pg::SqlParams;

use crate::engine::current_flow_db_id;
use crate::resp::{ApiResp, FlowError, Result};
use crate::tenant::{current_tenant, in_scope};

/// 聚合引擎全状态。任一子查询失败不致命（表可能未建）——降级为 0/空，保证大盘总能出。
pub async fn flow_stats() -> Result<Json<ApiResp<Value>>> {
    let db = current_flow_db_id();

    // —— 概览标量 ——
    let total_instances = scalar(&db, "SELECT count(*) FROM cmx_flow_instance").await;
    let total_tasks = scalar(&db, "SELECT count(*) FROM cmx_flow_task").await;
    let open_tasks =
        scalar(&db, "SELECT count(*) FROM cmx_flow_task WHERE completed = FALSE").await;
    let done_tasks =
        scalar(&db, "SELECT count(*) FROM cmx_flow_task WHERE completed = TRUE").await;
    let total_defs = scalar(&db, "SELECT count(*) FROM cmx_flow_definition").await;
    let distinct_users = scalar(
        &db,
        "SELECT count(DISTINCT assignee) FROM cmx_flow_task WHERE assignee IS NOT NULL AND assignee <> ''",
    )
    .await;
    let pending_timers = scalar(
        &db,
        "SELECT count(*) FROM cmx_flow_job",
    )
    .await;
    let cc_count = scalar(&db, "SELECT count(*) FROM cmx_flow_cc").await;
    let comment_count = scalar(&db, "SELECT count(*) FROM cmx_flow_task_comment").await;
    let delegation_count = scalar(&db, "SELECT count(*) FROM cmx_flow_task_delegation").await;
    let subflow_count = scalar(
        &db,
        "SELECT count(*) FROM cmx_flow_instance WHERE parent_instance_id IS NOT NULL",
    )
    .await;
    let mi_count = scalar(&db, "SELECT count(*) FROM cmx_flow_mi_scope").await;

    // —— 实例按状态 ——
    let by_state = group_pairs(
        &db,
        "SELECT state, count(*) c FROM cmx_flow_instance GROUP BY state ORDER BY c DESC",
        "state",
        "c",
    )
    .await;
    let active_instances = by_state
        .iter()
        .find(|(k, _)| k == "ACTIVE")
        .map(|(_, v)| *v)
        .unwrap_or(0);

    // —— 活跃定义分布（实例数 top 10）——
    let by_definition = group_pairs(
        &db,
        "SELECT definition_key, count(*) c FROM cmx_flow_instance GROUP BY definition_key ORDER BY c DESC LIMIT 10",
        "definition_key",
        "c",
    )
    .await;

    // —— 待办 top 办理人 ——
    let top_assignees = group_pairs(
        &db,
        "SELECT assignee, count(*) c FROM cmx_flow_task WHERE completed = FALSE AND assignee IS NOT NULL AND assignee <> '' GROUP BY assignee ORDER BY c DESC LIMIT 8",
        "assignee",
        "c",
    )
    .await;

    // —— 近 30 天实例创建时间线 ——
    let timeline = group_pairs(
        &db,
        "SELECT to_char(created_at::date, 'MM-DD') d, count(*) c FROM cmx_flow_instance \
         WHERE created_at > now() - interval '30 days' GROUP BY 1, created_at::date ORDER BY created_at::date",
        "d",
        "c",
    )
    .await;

    // —— 组织分布（实例按 org_id，空归为「未分配」）——
    let by_org = group_pairs(
        &db,
        "SELECT COALESCE(NULLIF(org_id, ''), '未分配') o, count(*) c \
         FROM cmx_flow_instance GROUP BY 1 ORDER BY c DESC LIMIT 10",
        "o",
        "c",
    )
    .await;

    // —— 节点瓶颈（待办任务按节点聚集，越多越堵）——
    let node_bottleneck = group_pairs(
        &db,
        "SELECT node_bpmn_id n, count(*) c FROM cmx_flow_task \
         WHERE completed = FALSE AND node_bpmn_id IS NOT NULL GROUP BY node_bpmn_id ORDER BY c DESC LIMIT 8",
        "n",
        "c",
    )
    .await;

    // —— 归档与性能 ——
    let hi_instances = scalar(&db, "SELECT count(*) FROM cmx_flow_hi_instance").await;
    let hi_tasks = scalar(&db, "SELECT count(*) FROM cmx_flow_hi_task").await;
    // 平均实例耗时（归档 duration_ms → 秒）
    let avg_instance_secs = scalar(
        &db,
        "SELECT COALESCE(round(avg(duration_ms) / 1000.0), 0)::bigint FROM cmx_flow_hi_instance WHERE duration_ms IS NOT NULL",
    )
    .await;
    // 平均任务处理时长（归档 duration_ms → 秒）
    let avg_task_secs = scalar(
        &db,
        "SELECT COALESCE(round(avg(duration_ms) / 1000.0), 0)::bigint FROM cmx_flow_hi_task WHERE duration_ms IS NOT NULL",
    )
    .await;
    // 每定义平均实例耗时（秒，top 慢的）——性能榜
    let perf_by_def = group_pairs(
        &db,
        "SELECT definition_key k, COALESCE(round(avg(duration_ms) / 1000.0), 0)::bigint s \
         FROM cmx_flow_hi_instance WHERE duration_ms IS NOT NULL GROUP BY definition_key ORDER BY s DESC LIMIT 8",
        "k",
        "s",
    )
    .await;

    // —— 引擎运行时 ——
    let tenant = if in_scope() { current_tenant() } else { "default".to_string() };

    let pairs_json = |v: &[(String, i64)]| -> Vec<Value> {
        v.iter().map(|(k, c)| json!({ "label": k, "value": c })).collect()
    };

    let data = json!({
        "overview": {
            "totalInstances": total_instances,
            "activeInstances": active_instances,
            "totalTasks": total_tasks,
            "openTasks": open_tasks,
            "doneTasks": done_tasks,
            "totalDefinitions": total_defs,
            "distinctUsers": distinct_users,
            "pendingTimers": pending_timers,
        },
        "instancesByState": pairs_json(&by_state),
        "byDefinition": pairs_json(&by_definition),
        "topAssignees": pairs_json(&top_assignees),
        "timeline": pairs_json(&timeline),
        "byOrg": pairs_json(&by_org),
        "nodeBottleneck": pairs_json(&node_bottleneck),
        "performance": pairs_json(&perf_by_def),
        "archive": {
            "hiInstances": hi_instances,
            "hiTasks": hi_tasks,
            "avgInstanceSecs": avg_instance_secs,
            "avgTaskSecs": avg_task_secs,
        },
        "collaboration": {
            "cc": cc_count,
            "comments": comment_count,
            "delegations": delegation_count,
            "subprocesses": subflow_count,
            "multiInstance": mi_count,
        },
        "runtime": {
            "tenant": tenant,
            "dbId": db,
            "engine": "cmx-flow",
        },
    });

    Ok(Json(ApiResp::ok(data)))
}

/// 节点耗时/瓶颈/SLA 分析查询参数（A6）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTimingQuery {
    /// 可选：只统计某流程定义。空 = 全部。
    #[serde(default)]
    definition_key: Option<String>,
    /// SLA 阈值（秒）：耗时超过它的任务计入 slaBreached。默认 86400（1 天）。
    #[serde(default)]
    sla_secs: Option<i64>,
}

/// 节点耗时/瓶颈/SLA 分析（A6）—— 基于归档的 hi_task（节点级 created/completed/duration）。
///
/// `GET /flow/analytics/node-timing?definitionKey=&slaSecs=`：按 node_bpmn_id 聚合
/// count/avg/max/min 耗时（毫秒）+ 超 SLA 任务数，avg 降序（瓶颈在前）。全部只读，表未建降级空。
pub async fn node_timing(Query(q): Query<NodeTimingQuery>) -> Result<Json<ApiResp<Value>>> {
    let db = current_flow_db_id();
    let sla_ms = q.sla_secs.unwrap_or(86_400).max(0) * 1000;

    // 归档任务的节点级耗时聚合。definitionKey 需 join 实例历史（hi_task 无 def 列）。
    // 简化：hi_task 直接聚合（跨定义）；给了 definitionKey 则 join hi_instance 过滤。
    let (sql, rows) = if let Some(key) = q.definition_key.clone().filter(|k| !k.is_empty()) {
        let sql = format!(
            "SELECT t.node_bpmn_id AS node, t.name AS name, count(*) AS cnt, \
                    round(avg(t.duration_ms)) AS avg_ms, max(t.duration_ms) AS max_ms, \
                    min(t.duration_ms) AS min_ms, \
                    count(*) FILTER (WHERE t.duration_ms > {sla_ms}) AS sla_breached \
             FROM cmx_flow_hi_task t \
             JOIN cmx_flow_hi_instance i ON i.id = t.instance_id \
             WHERE t.duration_ms IS NOT NULL AND i.definition_key = $1 \
             GROUP BY t.node_bpmn_id, t.name ORDER BY avg_ms DESC NULLS LAST LIMIT 50"
        );
        (sql.clone(), query_rows_params(&db, &sql, key).await)
    } else {
        let sql = format!(
            "SELECT node_bpmn_id AS node, name, count(*) AS cnt, \
                    round(avg(duration_ms)) AS avg_ms, max(duration_ms) AS max_ms, \
                    min(duration_ms) AS min_ms, \
                    count(*) FILTER (WHERE duration_ms > {sla_ms}) AS sla_breached \
             FROM cmx_flow_hi_task \
             WHERE duration_ms IS NOT NULL \
             GROUP BY node_bpmn_id, name ORDER BY avg_ms DESC NULLS LAST LIMIT 50"
        );
        (sql.clone(), query_rows(&db, &sql).await)
    };
    let _ = sql;

    // 概览：总归档任务、超 SLA 总数（rows 的键 = SQL 列别名 snake_case）。
    let total = rows
        .iter()
        .filter_map(|r| r.get("cnt").and_then(|v| v.as_i64()))
        .sum::<i64>();
    let breached = rows
        .iter()
        .filter_map(|r| r.get("sla_breached").and_then(|v| v.as_i64()))
        .sum::<i64>();

    Ok(Json(ApiResp::ok(json!({
        "slaSecs": sla_ms / 1000,
        "definitionKey": q.definition_key,
        "totalTasks": total,
        "slaBreachedTotal": breached,
        "nodes": rows,
    }))))
}

/// 取单值 count 标量。查询失败/空 → 0（表未建等，降级不报错）。
async fn scalar(db: &str, sql: &str) -> i64 {
    match cmx_database_pg::query_sql(db, None, sql, "flow_stats").await {
        Ok(ds) => {
            let schema = ds.schema.as_ref();
            ds.iter()
                .next()
                .and_then(|row| first_int(schema, row))
                .unwrap_or(0)
        }
        Err(_) => 0,
    }
}

/// 取 (label, count) 分组对列表。失败 → 空。
async fn group_pairs(db: &str, sql: &str, key_col: &str, val_col: &str) -> Vec<(String, i64)> {
    match cmx_database_pg::query_sql(db, None, sql, "flow_stats_group").await {
        Ok(ds) => {
            let schema = ds.schema.as_ref();
            ds.iter()
                .map(|row| {
                    let label = match row.get_by_name(schema, key_col) {
                        Some(DataValue::String(s)) => s.clone(),
                        Some(DataValue::Int(i)) => i.to_string(),
                        _ => String::new(),
                    };
                    let val = int_of(row.get_by_name(schema, val_col));
                    (label, val)
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

/// 从行取第一个数值列（count 标量场景，列名可能是 count/c/?column?）。
fn first_int(schema: &cmx_core::model::data::dataset::Schema, row: &Row) -> Option<i64> {
    for field in &schema.fields {
        if let Some(v) = row.get_by_name(schema, &field.name) {
            if let Some(n) = as_int(v) {
                return Some(n);
            }
        }
    }
    None
}

fn int_of(v: Option<&DataValue>) -> i64 {
    v.and_then(as_int).unwrap_or(0)
}

fn as_int(v: &DataValue) -> Option<i64> {
    match v {
        DataValue::Int(i) => Some(*i),
        DataValue::Float(f) => Some(*f as i64),
        DataValue::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

/// 便于未来扩展错误路径（当前全降级，不产生错误）。
#[allow(dead_code)]
fn _stats_err(msg: impl Into<String>) -> FlowError {
    FlowError::business_error(msg)
}

// ═══════════════════════════════════════════════════════════
// 明细下钻：大盘每个数据点可点击 → 列该维度的明细行；每行含完整字段供展开。
// GET /api/flow/v1/stats/detail?dim=<维度>&key=<可选筛选值>&limit=<可选>
// ═══════════════════════════════════════════════════════════

/// 明细下钻查询参数。
#[derive(Debug, Deserialize)]
pub struct DetailQuery {
    /// 维度：instanceState | definition | assignee | timelineDay | cc | comment | delegation | subprocess | multiInstance | timer。
    pub dim: String,
    /// 该维度的筛选键（如 state=ACTIVE、definition_key=credit_approval、assignee=mgr、day=08-05）。可空=全量。
    #[serde(default)]
    pub key: Option<String>,
    /// 返回上限（默认 200）。
    #[serde(default)]
    pub limit: Option<i64>,
}

/// 明细下钻：按维度返回明细行列表。每行是一个对象（全字段），前端直接展开显示。
///
/// 参数化（`$1` 绑定 key）防注入；维度白名单固定 SQL，key 只作数据值绑定。
pub async fn stats_detail(Query(q): Query<DetailQuery>) -> Result<Json<ApiResp<Value>>> {
    let db = current_flow_db_id();
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let key = q.key.clone().unwrap_or_default();
    let has_key = !key.is_empty();

    // (SQL 模板, 是否用 key 过滤, 标题, 该维度每行的主要展示列顺序)
    let (sql, use_key, title): (String, bool, &str) = match q.dim.as_str() {
        // —— 实例：按状态 / 按定义 / 全量 ——
        "instanceState" => (
            format!(
                "SELECT id, definition_key, business_key, state, org_id, created_at, ended_at \
                 FROM cmx_flow_instance {} ORDER BY created_at DESC LIMIT {limit}",
                if has_key { "WHERE state = $1" } else { "" }
            ),
            has_key,
            "实例明细",
        ),
        "definition" => (
            format!(
                "SELECT id, definition_key, business_key, state, org_id, created_at, ended_at \
                 FROM cmx_flow_instance {} ORDER BY created_at DESC LIMIT {limit}",
                if has_key { "WHERE definition_key = $1" } else { "" }
            ),
            has_key,
            "实例明细",
        ),
        "instances" => (
            format!(
                "SELECT id, definition_key, business_key, state, org_id, created_at, ended_at \
                 FROM cmx_flow_instance ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "实例明细",
        ),
        // —— 任务：按办理人 / 全量待办 / 全量 ——
        "assignee" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, completed, created_at, completed_at \
                 FROM cmx_flow_task {} ORDER BY created_at DESC LIMIT {limit}",
                if has_key { "WHERE assignee = $1 AND completed = FALSE" } else { "WHERE completed = FALSE" }
            ),
            has_key,
            "任务明细",
        ),
        "openTasks" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, completed, created_at, completed_at \
                 FROM cmx_flow_task WHERE completed = FALSE ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "待办任务明细",
        ),
        "doneTasks" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, completed, created_at, completed_at \
                 FROM cmx_flow_task WHERE completed = TRUE ORDER BY completed_at DESC LIMIT {limit}"
            ),
            false,
            "已办任务明细",
        ),
        "tasks" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, completed, created_at, completed_at \
                 FROM cmx_flow_task ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "任务明细",
        ),
        // —— 时间线：某天创建的实例 ——
        "timelineDay" => (
            format!(
                "SELECT id, definition_key, business_key, state, created_at \
                 FROM cmx_flow_instance WHERE to_char(created_at::date, 'MM-DD') = $1 \
                 ORDER BY created_at DESC LIMIT {limit}"
            ),
            true,
            "当日创建实例",
        ),
        // —— 定义：定义表本身 ——
        "definitions" => (
            format!(
                "SELECT key, name, domain, application, module, active_version \
                 FROM cmx_flow_definition ORDER BY key LIMIT {limit}"
            ),
            false,
            "流程定义明细",
        ),
        // —— 用户：参与用户（去重办理人）——
        "users" => (
            format!(
                "SELECT assignee AS user_id, count(*) AS task_count, \
                 count(*) FILTER (WHERE completed = FALSE) AS open_count \
                 FROM cmx_flow_task WHERE assignee IS NOT NULL AND assignee <> '' \
                 GROUP BY assignee ORDER BY open_count DESC, task_count DESC LIMIT {limit}"
            ),
            false,
            "参与用户明细",
        ),
        // —— 协作特性 ——
        "cc" => (
            format!(
                "SELECT id, instance_id, from_user_id, to_user_id, node_bpmn_id, reason, read_at, created_at \
                 FROM cmx_flow_cc ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "抄送明细",
        ),
        "comment" => (
            format!(
                "SELECT id, instance_id, task_id, user_id, node_bpmn_id, decision, comment, created_at \
                 FROM cmx_flow_task_comment ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "审批意见明细",
        ),
        "delegation" => (
            format!(
                "SELECT id, instance_id, task_id, kind, from_user_id, to_user_id, reason, created_at \
                 FROM cmx_flow_task_delegation ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "转办委派明细",
        ),
        "subprocess" => (
            format!(
                "SELECT id, definition_key, business_key, state, parent_instance_id, created_at \
                 FROM cmx_flow_instance WHERE parent_instance_id IS NOT NULL ORDER BY created_at DESC LIMIT {limit}"
            ),
            false,
            "子流程实例明细",
        ),
        "multiInstance" => (
            format!(
                "SELECT id, instance_id, node_bpmn_id, total, completed, sequential, finished, completion_condition \
                 FROM cmx_flow_mi_scope ORDER BY id DESC LIMIT {limit}"
            ),
            false,
            "会签/或签明细",
        ),
        // —— 组织：某 org 的实例 ——
        "org" => (
            format!(
                "SELECT id, definition_key, business_key, state, org_id, created_at, ended_at \
                 FROM cmx_flow_instance WHERE COALESCE(NULLIF(org_id, ''), '未分配') = $1 \
                 ORDER BY created_at DESC LIMIT {limit}"
            ),
            true,
            "组织实例明细",
        ),
        // —— 节点瓶颈：某节点的待办任务 ——
        "node" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, completed, created_at \
                 FROM cmx_flow_task WHERE completed = FALSE AND node_bpmn_id = $1 \
                 ORDER BY created_at DESC LIMIT {limit}"
            ),
            true,
            "节点待办明细",
        ),
        // —— 归档：历史实例（含耗时）——
        "hiInstance" => (
            format!(
                "SELECT id, definition_key, business_key, state, duration_ms, created_at, ended_at, archived_at \
                 FROM cmx_flow_hi_instance {} ORDER BY archived_at DESC LIMIT {limit}",
                if has_key { "WHERE definition_key = $1" } else { "" }
            ),
            has_key,
            "归档实例明细",
        ),
        // —— 归档：历史任务（含处理时长）——
        "hiTask" => (
            format!(
                "SELECT id, name, assignee, instance_id, node_bpmn_id, duration_ms, created_at, completed_at, archived_at \
                 FROM cmx_flow_hi_task ORDER BY archived_at DESC LIMIT {limit}"
            ),
            false,
            "归档任务明细",
        ),
        // —— 性能：某定义的归档实例（按耗时倒序，看最慢的）——
        "performance" => (
            format!(
                "SELECT id, definition_key, business_key, state, duration_ms, created_at, ended_at \
                 FROM cmx_flow_hi_instance WHERE definition_key = $1 AND duration_ms IS NOT NULL \
                 ORDER BY duration_ms DESC LIMIT {limit}"
            ),
            true,
            "定义性能明细（最慢在前）",
        ),
        "timer" => (
            format!(
                "SELECT id, instance_id, token_id, boundary_bpmn_id, cancel_activity, due_at, created_at \
                 FROM cmx_flow_job ORDER BY due_at LIMIT {limit}"
            ),
            false,
            "待触发定时器明细",
        ),
        other => {
            return Err(FlowError::business_error(format!("未知下钻维度: {other}")));
        }
    };

    let rows = if use_key {
        query_rows_params(&db, &sql, key.clone()).await
    } else {
        query_rows(&db, &sql).await
    };

    Ok(Json(ApiResp::ok(json!({
        "dim": q.dim,
        "key": q.key,
        "title": title,
        "count": rows.len(),
        "rows": rows,
    }))))
}

/// 执行查询 → 每行转成 JSON 对象（列名→值，全字段供展开）。失败 → 空。
async fn query_rows(db: &str, sql: &str) -> Vec<Value> {
    match cmx_database_pg::query_sql(db, None, sql, "flow_stats_detail").await {
        Ok(ds) => rows_to_json(&ds),
        Err(_) => Vec::new(),
    }
}

/// 参数化版本（$1 绑定 key，防注入）。
async fn query_rows_params(db: &str, sql: &str, key: String) -> Vec<Value> {
    let params = SqlParams::DataValues(vec![DataValue::String(key)]);
    match cmx_database_pg::query_sql_with_params(db, None, sql, params, "flow_stats_detail").await {
        Ok(ds) => rows_to_json(&ds),
        Err(_) => Vec::new(),
    }
}

/// ZmcDataSet → Vec<JSON 对象>（列名→值，全列）。供前端逐行展开显示所有字段。
fn rows_to_json(ds: &cmx_core::model::data::dataset::DataSet) -> Vec<Value> {
    let schema = ds.schema.as_ref();
    let fields: Vec<String> = schema.fields.iter().map(|f| f.name.clone()).collect();
    ds.iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for name in &fields {
                obj.insert(name.clone(), dv_to_json(row.get_by_name(schema, name)));
            }
            Value::Object(obj)
        })
        .collect()
}

/// DataValue → serde_json::Value（展开显示用；时间/decimal 转字符串）。
fn dv_to_json(v: Option<&DataValue>) -> Value {
    match v {
        None | Some(DataValue::Null) | Some(DataValue::NullTyped(_)) => Value::Null,
        Some(DataValue::String(s)) => Value::String(s.clone()),
        Some(DataValue::Int(i)) => json!(i),
        Some(DataValue::Float(f)) => json!(f),
        Some(DataValue::Bool(b)) => json!(b),
        Some(DataValue::Decimal(d)) => Value::String(d.to_string()),
        Some(DataValue::DateTime(dt)) => Value::String(dt.to_rfc3339()),
        Some(DataValue::Date(d)) => Value::String(d.to_string()),
        Some(DataValue::Json(s)) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        Some(other) => Value::String(format!("{other:?}")),
    }
}
