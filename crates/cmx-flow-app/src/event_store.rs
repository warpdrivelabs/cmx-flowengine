//! 事件订阅域存储层（20260902 重构方案 §三）：流程分组 + 事件订阅者（rules JSONB 内嵌）
//! + 持久化投递队列。
//!
//! 惯例对齐 [`crate::biz_link`]：薄持久化直走 cmx-database-pg、落当前租户运行态库
//! （`cmx_flow_*` 表 = 业务库）、DDL 幂等自举（engine build 调一次 + 生产走 docs/sql 迁移）。
//! 三表主键 = BIGINT 应用层 Pk52 雪花（`cmx_utils::next_pk_id`，52 位 JS 安全）；
//! 投递表保序键独立为 `seq BIGSERIAL`（DB 提交序）——身份与排序分离。
//!
//! 多副本：路由数据（订阅者 + 定义分组映射）走进程内缓存 + **TTL 5s 版本对账**——指纹 =
//! 三表 `(COUNT, MAX(updated_at))` 并集（订阅者仅 active 行），均为 DB 时钟赋值；写操作
//! 失效本副本，跨副本 ≤5s 收敛。抢占/续租/结果落库全带租约列——见 [`crate::event_outbox`]。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cmx_core::model::cell::{DataValue, SqlTypeMarker};
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql_with_params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 路由缓存 TTL（版本对账周期；跨副本收敛上界 ≈ 此值）。
pub const CACHE_TTL: Duration = Duration::from_secs(5);

// ============================================================
// DDL（幂等自举；与 docs/sql/v2/biz init_ddl + 20260902_004 迁移同源维护）
// ============================================================

/// 建表 DDL（幂等）。engine build 时调一次自举；生产走 docs/sql 迁移。
pub const DDL_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_def_group (
        id         BIGINT       NOT NULL,
        name       VARCHAR(64)  NOT NULL,
        sort_no    INT          NOT NULL DEFAULT 0,
        enabled    BOOLEAN      NOT NULL DEFAULT TRUE,
        remark     VARCHAR(512),
        created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
        PRIMARY KEY (id)
    )"#,
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_def_group_name ON cmx_flow_def_group (name)",
    // 幂等补列：既有库升级时补定义分组列（五处同改纪律：迁移/init_ddl/本文件/def store）。
    "ALTER TABLE cmx_flow_definition ADD COLUMN IF NOT EXISTS group_id BIGINT",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_definition_group ON cmx_flow_definition (group_id)",
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_event_subscriber (
        id             BIGINT       NOT NULL,
        name           VARCHAR(128) NOT NULL,
        description    VARCHAR(512),
        channel        VARCHAR(16)  NOT NULL DEFAULT 'webhook',
        channel_config JSONB        NOT NULL DEFAULT '{}',
        rules          JSONB        NOT NULL DEFAULT '[]',
        retry_max      INT          NOT NULL DEFAULT 10,
        active         BOOLEAN      NOT NULL DEFAULT TRUE,
        tenant_id      VARCHAR(64)  NOT NULL DEFAULT 'default',
        created_by     VARCHAR(64),
        created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
        PRIMARY KEY (id)
    )"#,
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_event_sub_name ON cmx_flow_event_subscriber (tenant_id, name)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_event_sub_upd ON cmx_flow_event_subscriber (updated_at)",
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_event_delivery (
        id                    BIGINT        NOT NULL,
        seq                   BIGSERIAL     NOT NULL,
        subscriber_id         BIGINT        NOT NULL,
        subscriber_name       VARCHAR(128)  NOT NULL,
        channel               VARCHAR(16)   NOT NULL,
        event_id              VARCHAR(64)   NOT NULL,
        delivery_id           VARCHAR(160)  NOT NULL,
        source                VARCHAR(8)    NOT NULL DEFAULT 'emit',
        event_type            VARCHAR(32)   NOT NULL,
        definition_key        VARCHAR(128),
        business_key          VARCHAR(128),
        instance_id           VARCHAR(64)   NOT NULL,
        payload               JSONB         NOT NULL,
        state                 VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
        attempts              INT           NOT NULL DEFAULT 0,
        next_attempt_at       TIMESTAMPTZ,
        locked_by             VARCHAR(64),
        lock_expires_at       TIMESTAMPTZ,
        last_error            TEXT,
        last_http_status      INT,
        last_response_snippet VARCHAR(512),
        matched_rule          VARCHAR(64),
        created_at            TIMESTAMPTZ   NOT NULL DEFAULT now(),
        delivered_at          TIMESTAMPTZ,
        PRIMARY KEY (id)
    )"#,
    // 幂等补列：既有库升级到投递表加 matched_rule 时补列。
    "ALTER TABLE cmx_flow_event_delivery ADD COLUMN IF NOT EXISTS matched_rule VARCHAR(64)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_event_dlv_seq ON cmx_flow_event_delivery (seq)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_event_dlv_sub_event ON cmx_flow_event_delivery (subscriber_id, event_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_event_dlv_due ON cmx_flow_event_delivery (state, next_attempt_at)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_event_dlv_sub ON cmx_flow_event_delivery (subscriber_id, seq)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_event_dlv_did ON cmx_flow_event_delivery (delivery_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_event_dlv_created ON cmx_flow_event_delivery (created_at)",
];

/// 自举建表（engine build 后调一次）。失败返回错误（调用方告警不阻断启动）。
pub async fn ensure_schema(db_id: &str) -> Result<(), String> {
    for stmt in DDL_STATEMENTS {
        execute_sql(db_id, None, stmt).await.map_err(|e| format!("事件订阅建表失败: {e}"))?;
    }
    Ok(())
}

// ============================================================
// 流程分组
// ============================================================

/// 一条流程分组（def_count 仅列表投影填充）。
#[derive(Debug, Clone)]
pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub sort_no: i32,
    pub enabled: bool,
    pub remark: Option<String>,
    pub def_count: i64,
}

/// 分组保存入参。
#[derive(Debug, Clone)]
pub struct GroupUpsert {
    pub id: Option<i64>,
    pub name: String,
    pub sort_no: i32,
    pub enabled: bool,
    pub remark: Option<String>,
}

fn group_json(g: &GroupRow) -> Value {
    json!({
        "id": g.id,
        "name": g.name,
        "sortNo": g.sort_no,
        "enabled": g.enabled,
        "remark": g.remark,
        "defCount": g.def_count,
    })
}

/// 分组列表（按 sort_no, id 排序；附每组定义数）。分组量级极小，不分页。
pub async fn list_groups(db_id: &str) -> Result<Vec<Value>, String> {
    let sql = "SELECT g.id, g.name, g.sort_no, g.enabled, g.remark, \
         (SELECT COUNT(*) FROM cmx_flow_definition d WHERE d.group_id = g.id) AS def_count \
         FROM cmx_flow_def_group g ORDER BY g.sort_no, g.id";
    let ds = query_sql_with_params(db_id, None, sql, SqlParams::DataValues(vec![]), "evt_group_list")
        .await
        .map_err(|e| format!("查分组失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .map(|row| {
            group_json(&GroupRow {
                id: get_i64(row, schema, "id").unwrap_or(0),
                name: get_str(row, schema, "name"),
                sort_no: get_i64(row, schema, "sort_no").unwrap_or(0) as i32,
                enabled: get_bool(row, schema, "enabled"),
                remark: get_opt(row, schema, "remark"),
                def_count: get_i64(row, schema, "def_count").unwrap_or(0),
            })
        })
        .collect())
}

/// 新建/更新分组（新建 Pk52 铸号；uk 冲突由 DB 兜底，调用方预检给友好错误）。
/// 任何变更都带 updated_at = now()（缓存指纹对账依赖）。
pub async fn upsert_group(db_id: &str, g: &GroupUpsert) -> Result<i64, String> {
    let id = g.id.unwrap_or_else(cmx_utils::next_pk_id);
    let sql = "INSERT INTO cmx_flow_def_group (id, name, sort_no, enabled, remark) \
        VALUES ($1, $2, $3, $4, $5) \
        ON CONFLICT (id) DO UPDATE SET \
          name = EXCLUDED.name, sort_no = EXCLUDED.sort_no, \
          enabled = EXCLUDED.enabled, remark = EXCLUDED.remark, updated_at = now()";
    execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![
            DataValue::Int(id),
            DataValue::String(g.name.clone()),
            DataValue::Int(g.sort_no as i64),
            DataValue::Bool(g.enabled),
            opt_str(g.remark.clone()),
        ]),
    )
    .await
    .map_err(|e| format!("保存分组失败: {e}"))?;
    invalidate_route_cache();
    Ok(id)
}

/// 删除分组——**单条 SQL 守卫**：组内仍有定义 → 拒绝（0 行命中分类报错）。
/// check-then-act 的 TOCTOU 窗口压缩进一条语句（与订阅者删除守卫同纪律）。
pub async fn delete_group(db_id: &str, id: i64) -> Result<(), String> {
    let sql = "DELETE FROM cmx_flow_def_group g \
        WHERE g.id = $1 AND NOT EXISTS ( \
          SELECT 1 FROM cmx_flow_definition d WHERE d.group_id = g.id)";
    let n = execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![DataValue::Int(id)]),
    )
    .await
    .map_err(|e| format!("删除分组失败: {e}"))?;
    if n == 0 {
        // 分类：不存在 vs 组内有定义。
        let cnt = query_one_i64_p(
            db_id,
            "SELECT COUNT(*) AS n FROM cmx_flow_definition WHERE group_id = $1",
            SqlParams::DataValues(vec![DataValue::Int(id)]),
            "evt_group_defcount",
        )
        .await
        .unwrap_or(-1);
        return Err(if cnt > 0 {
            format!("分组下仍有 {cnt} 个流程定义，请先移出或改挂其它分组")
        } else {
            format!("分组不存在: {id}")
        });
    }
    invalidate_route_cache();
    Ok(())
}

/// 取全部分组（预览/前端下拉用；分组量级极小）。
pub async fn get_groups(db_id: &str) -> Result<Vec<(i64, String)>, String> {
    let ds = query_sql_with_params(
        db_id,
        None,
        "SELECT id, name FROM cmx_flow_def_group",
        SqlParams::DataValues(vec![]),
        "evt_group_all",
    )
    .await
    .map_err(|e| format!("查分组失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .map(|row| (get_i64(row, schema, "id").unwrap_or(0), get_str(row, schema, "name")))
        .collect())
}

// ============================================================
// 订阅规则（rules JSONB 元素；存储与 API 共用同一形状）
// ============================================================

/// 一条订阅规则：规则内三维 AND（eventTypes × groupIds × keyPatterns，空 = 不限）、
/// 跨规则 OR、数组序 = 命中序；全空且 enabled = 匹配全部（网关形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubRule {
    /// 规则名（≤64 字符；同订阅者内唯一；命中后落投递行 matched_rule 快照）。
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 事件类型集合；[] = 全部 6 种。
    #[serde(default)]
    pub event_types: Vec<String>,
    /// 流程分组 id 集合（JSON number）；[] = 不限分组。
    #[serde(default)]
    pub group_ids: Vec<i64>,
    /// definitionKey glob 模式集合（仅 `*` 通配）；[] = 不限。
    #[serde(default)]
    pub key_patterns: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl SubRule {
    /// 从 JSONB 值宽容解析（缺 enabled 默认 true；缺数组默认空）。
    pub fn from_value(v: &Value) -> Option<SubRule> {
        serde_json::from_value(v.clone()).ok()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

// ============================================================
// 订阅者
// ============================================================

/// 一条订阅者（channel_config 为原始 JSON；secret 掩码在 handler 层做）。
#[derive(Debug, Clone)]
pub struct SubscriberRow {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub channel: String,
    pub channel_config: Value,
    pub rules: Vec<SubRule>,
    pub retry_max: i32,
    pub active: bool,
    pub tenant_id: String,
    pub created_by: Option<String>,
    /// 未投存量（PENDING+IN_FLIGHT；仅管理投影查询填充，匹配/缓存路径不查、恒 0）。
    pub pending_count: i64,
    /// 最近成功投递时刻（RFC3339；仅管理投影查询填充）。
    pub last_delivered_at: Option<String>,
    /// 24h 死信数（仅管理投影查询填充）。
    pub dead_count_24h: i64,
}

/// 订阅者保存入参（secret 已由 handler 解析为**终值**：留空/掩码 = 沿用旧值在 handler 处理）。
#[derive(Debug, Clone)]
pub struct SubscriberUpsert {
    pub id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    pub channel: String,
    pub channel_config: Value,
    pub rules: Vec<SubRule>,
    pub retry_max: i32,
    pub active: bool,
    pub created_by: Option<String>,
}

const SUB_COLS: &str = "id, name, description, channel, channel_config, rules, retry_max, active, tenant_id, created_by";

fn sub_row(row: &cmx_core::model::data::dataset::Row, schema: &cmx_core::model::data::dataset::Schema) -> Option<SubscriberRow> {
    let channel_config = get_opt(row, schema, "channel_config")?;
    let rules_raw = get_opt(row, schema, "rules")?;
    let rules: Vec<SubRule> = serde_json::from_str::<Value>(&rules_raw)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|arr| arr.iter().filter_map(SubRule::from_value).collect())
        .unwrap_or_default();
    Some(SubscriberRow {
        id: get_i64(row, schema, "id")?,
        name: get_str(row, schema, "name"),
        description: get_opt(row, schema, "description"),
        channel: get_str(row, schema, "channel"),
        channel_config: serde_json::from_str(&channel_config).unwrap_or(Value::Null),
        rules,
        retry_max: get_i64(row, schema, "retry_max").unwrap_or(10) as i32,
        active: get_bool(row, schema, "active"),
        tenant_id: get_str(row, schema, "tenant_id"),
        created_by: get_opt(row, schema, "created_by"),
        // 投影 SQL 未带该列（缓存/匹配路径）时取 0/None。
        pending_count: get_i64(row, schema, "pending_count").unwrap_or(0),
        last_delivered_at: get_opt(row, schema, "last_delivered_at"),
        dead_count_24h: get_i64(row, schema, "dead_count_24h").unwrap_or(0),
    })
}

/// 订阅者列表过滤。
#[derive(Clone)]
pub struct SubFilter {
    pub keyword: Option<String>,
    pub channel: Option<String>,
    pub active: Option<bool>,
    pub page: i64,
    pub page_size: i64,
}

impl Default for SubFilter {
    fn default() -> Self {
        Self { keyword: None, channel: None, active: None, page: 1, page_size: 20 }
    }
}

impl SubFilter {
    pub fn norm(&self) -> (i64, i64) {
        (self.page.max(1), self.page_size.clamp(1, 200))
    }
}

/// 健康度投影子查询（列表与详情共用）：待投存量 / 最近成功投递 / 24h 死信数。
const SUB_HEALTH: &str = "\
 (SELECT COUNT(*) FROM cmx_flow_event_delivery d \
   WHERE d.subscriber_id = s.id AND d.state IN ('PENDING','IN_FLIGHT')) AS pending_count, \
 (SELECT MAX(d.delivered_at) FROM cmx_flow_event_delivery d WHERE d.subscriber_id = s.id) AS last_delivered_at, \
 (SELECT COUNT(*) FROM cmx_flow_event_delivery d \
   WHERE d.subscriber_id = s.id AND d.state = 'DEAD' \
     AND d.created_at >= now() - interval '24 hours') AS dead_count_24h";

/// 分页列订阅者（keyword 模糊匹配名称；ORDER BY created_at DESC）。
pub async fn list_subscribers(
    db_id: &str,
    tenant: &str,
    f: &SubFilter,
) -> Result<Vec<Value>, String> {
    let (page, size) = f.norm();
    let mut params: Vec<DataValue> = vec![DataValue::String(tenant.to_string())];
    let mut pn = 1;
    let mut cond = format!("tenant_id = ${pn}");
    if let Some(kw) = f.keyword.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        let kw = kw
            .trim()
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        cond.push_str(&format!(" AND lower(name) LIKE ${pn} ESCAPE '\\'"));
        params.push(DataValue::String(format!("%{kw}%")));
    }
    if let Some(ch) = f.channel.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND channel = ${pn}"));
        params.push(DataValue::String(ch.trim().to_string()));
    }
    if let Some(a) = f.active {
        cond.push_str(&format!(" AND active = {}", if a { "TRUE" } else { "FALSE" }));
    }
    let offset = (page - 1) * size;
    let mut list_params = params;
    list_params.push(DataValue::Int(size));
    list_params.push(DataValue::Int(offset));
    let sql = format!(
        "SELECT {SUB_COLS}, {SUB_HEALTH} \
         FROM cmx_flow_event_subscriber s WHERE {cond} \
         ORDER BY created_at DESC LIMIT ${} OFFSET ${}",
        pn + 1,
        pn + 2
    );
    let ds = query_sql_with_params(db_id, None, &sql, SqlParams::DataValues(list_params), "evt_sub_list")
        .await
        .map_err(|e| format!("查订阅者失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let rows: Vec<Value> = ds
        .iter()
        .filter_map(|row| sub_row(row, schema))
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "description": s.description,
                "channel": s.channel,
                "rules": s.rules,
                "ruleCount": s.rules.len(),
                "retryMax": s.retry_max,
                "active": s.active,
                "tenantId": s.tenant_id,
                "createdBy": s.created_by,
                "pendingCount": s.pending_count,
                "lastDeliveredAt": s.last_delivered_at,
                "deadCount24h": s.dead_count_24h,
            })
        })
        .collect();
    Ok(rows)
}

/// 列表总数（与 [`list_subscribers`] 同条件 COUNT）。
pub async fn count_subscribers(db_id: &str, tenant: &str, f: &SubFilter) -> Result<i64, String> {
    let mut params: Vec<DataValue> = vec![DataValue::String(tenant.to_string())];
    let mut pn = 1;
    let mut cond = format!("tenant_id = ${pn}");
    if let Some(kw) = f.keyword.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        let kw = kw.trim().to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        cond.push_str(&format!(" AND lower(name) LIKE ${pn} ESCAPE '\\'"));
        params.push(DataValue::String(format!("%{kw}%")));
    }
    if let Some(ch) = f.channel.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND channel = ${pn}"));
        params.push(DataValue::String(ch.trim().to_string()));
    }
    if let Some(a) = f.active {
        cond.push_str(&format!(" AND active = {}", if a { "TRUE" } else { "FALSE" }));
    }
    query_one_i64_p(
        db_id,
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_event_subscriber WHERE {cond}"),
        SqlParams::DataValues(params),
        "evt_sub_count",
    )
    .await
}

/// 取一条订阅者（含健康度投影）。
pub async fn get_subscriber(db_id: &str, tenant: &str, id: i64) -> Result<Option<SubscriberRow>, String> {
    let sql = format!(
        "SELECT {SUB_COLS}, {SUB_HEALTH} \
         FROM cmx_flow_event_subscriber s WHERE id = $1 AND tenant_id = $2"
    );
    Ok(query_sub_rows_p(
        db_id,
        &sql,
        SqlParams::DataValues(vec![DataValue::Int(id), DataValue::String(tenant.to_string())]),
        "evt_sub_get",
    )
    .await?
    .into_iter()
    .next())
}

/// 按名点查（纯键列；缓存/匹配路径不带健康度子查询）。
pub async fn get_subscriber_by_name(
    db_id: &str,
    tenant: &str,
    name: &str,
) -> Result<Option<SubscriberRow>, String> {
    let sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_event_subscriber \
         WHERE tenant_id = $1 AND name = $2"
    );
    Ok(query_sub_rows_p(
        db_id,
        &sql,
        SqlParams::DataValues(vec![
            DataValue::String(tenant.to_string()),
            DataValue::String(name.to_string()),
        ]),
        "evt_sub_by_name",
    )
    .await?
    .into_iter()
    .next())
}

/// 按一批 id 取订阅者（poller 投递前取通道配置用；不筛 active——停用订阅者的存量行仍投）。
pub async fn get_subscribers_by_ids(
    db_id: &str,
    tenant: &str,
    ids: &[i64],
) -> Result<HashMap<i64, SubscriberRow>, String> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    // id 为 i64 数值（无注入面）；tenant 走参数。
    let sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_event_subscriber \
         WHERE id IN ({id_list}) AND tenant_id = $1"
    );
    Ok(query_sub_rows_p(
        db_id,
        &sql,
        SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]),
        "evt_sub_by_ids",
    )
    .await?
    .into_iter()
    .map(|s| (s.id, s))
    .collect())
}

/// 新建/更新订阅者——**单行 upsert**（rules 内嵌，天然原子；并发保存 last-write-wins，
/// 无跨表一致性窗口）。新建 Pk52 铸号；updated_at 由 DB 时钟 now() 赋值。
pub async fn upsert_subscriber(db_id: &str, tenant: &str, s: &SubscriberUpsert) -> Result<i64, String> {
    let id = s.id.unwrap_or_else(cmx_utils::next_pk_id);
    let cfg = serde_json::to_string(&s.channel_config).unwrap_or_else(|_| "{}".into());
    let rules = serde_json::to_string(&s.rules).unwrap_or_else(|_| "[]".into());
    let sql = "INSERT INTO cmx_flow_event_subscriber \
        (id, name, description, channel, channel_config, rules, retry_max, active, tenant_id, created_by) \
        VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb, $7, $8, $9, $10) \
        ON CONFLICT (id) DO UPDATE SET \
          name = EXCLUDED.name, description = EXCLUDED.description, channel = EXCLUDED.channel, \
          channel_config = EXCLUDED.channel_config, rules = EXCLUDED.rules, \
          retry_max = EXCLUDED.retry_max, active = EXCLUDED.active, updated_at = now()";
    let params = SqlParams::DataValues(vec![
        DataValue::Int(id),
        DataValue::String(s.name.clone()),
        opt_str(s.description.clone()),
        DataValue::String(s.channel.clone()),
        DataValue::Json(cfg),
        DataValue::Json(rules),
        DataValue::Int(s.retry_max as i64),
        DataValue::Bool(s.active),
        DataValue::String(tenant.to_string()),
        opt_str(s.created_by.clone()),
    ]);
    execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("保存订阅者失败: {e}"))?;
    invalidate_route_cache();
    Ok(id)
}

/// 物理删一条订阅者——**仅停用态且无未终态/死信投递行可删**（单条 SQL 守卫：TOCTOU
/// 窗口压缩进一条语句；流水凭 subscriber_name 快照保审计；PENDING/IN_FLIGHT/DEAD 行
/// 的存在意味着仍有待投/待处置流量，删订阅者会产生无主死信）。
pub async fn delete_subscriber(db_id: &str, tenant: &str, id: i64) -> Result<(), String> {
    let sql = "DELETE FROM cmx_flow_event_subscriber s \
               WHERE s.id = $1 AND s.tenant_id = $2 AND s.active = FALSE \
                 AND NOT EXISTS (SELECT 1 FROM cmx_flow_event_delivery d \
                                  WHERE d.subscriber_id = s.id \
                                    AND d.state IN ('PENDING','IN_FLIGHT','DEAD'))";
    let n = execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![DataValue::Int(id), DataValue::String(tenant.to_string())]),
    )
    .await
    .map_err(|e| format!("删除订阅者失败: {e}"))?;
    if n == 0 {
        // 分类拒绝原因（不存在 / 仍启用 / 有未终态或死信行）。
        let sub = get_subscriber(db_id, tenant, id).await?;
        return Err(match sub {
            None => format!("订阅者不存在: {id}"),
            Some(s) if s.active => "订阅者仍处于启用状态，请先停用再删除".into(),
            Some(rest) => format!(
                "存在待投/投递中/死信投递行 {} 个，无法删除（请先重发或处置死信、等待投递完成）",
                rest.pending_count
            ),
        });
    }
    invalidate_route_cache();
    Ok(())
}

/// 启停订阅者（停用即不再生成新投递行，存量行保留可查可清）。
/// **硬约束**：UPDATE 必带 updated_at = now()——缓存指纹对账依赖（只改 active 不动指纹
/// 会让跨副本路由 5s 内不收敛且永不对账出变更）。
pub async fn set_subscriber_active(
    db_id: &str,
    tenant: &str,
    id: i64,
    active: bool,
) -> Result<(), String> {
    let sql = "UPDATE cmx_flow_event_subscriber SET active = $1, updated_at = now() \
               WHERE id = $2 AND tenant_id = $3";
    let n = execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![
            DataValue::Bool(active),
            DataValue::Int(id),
            DataValue::String(tenant.to_string()),
        ]),
    )
    .await
    .map_err(|e| format!("启停订阅者失败: {e}"))?;
    if n == 0 {
        return Err(format!("订阅者不存在: {id}"));
    }
    invalidate_route_cache();
    Ok(())
}

// ============================================================
// 进程内路由缓存（TTL 5s 版本对账；指纹 = 三表 (COUNT, MAX(updated_at)) 并集）
// ============================================================

/// 路由快照：匹配所需的全部数据（订阅者 + 定义→分组映射 + 分组名）。
pub struct RouteSnapshot {
    /// 活跃订阅者（rules 已解析）。
    pub subscribers: Arc<Vec<SubscriberRow>>,
    /// definition_key → group_id（未分组/未知定义不在 map 中）。
    pub def_group: Arc<HashMap<String, i64>>,
    /// group_id → 分组名（payload groupName 注入用）。
    pub group_name: Arc<HashMap<i64, String>>,
}

/// 指纹：((sub_n, sub_mx), (grp_n, grp_mx), (def_n, def_mx))——DB 时钟赋值，
/// 应用时钟漂移免疫；任一表变更 → 至少一个分量变化。
type Fingerprint = ((i64, Option<String>), (i64, Option<String>), (i64, Option<String>));

struct CacheEntry {
    snap: Arc<RouteSnapshot>,
    version: Fingerprint,
    checked: Instant,
}

fn route_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static C: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 写操作后失效本副本缓存（跨副本靠 TTL 对账 ≤5s 收敛）。
pub fn invalidate_route_cache() {
    route_cache().lock().expect("路由缓存锁中毒").clear();
}

/// 取当前路由快照（匹配用；缓存命中即返，过期先对账版本再决定是否重载）。
pub async fn route_snapshot_cached(db_id: &str, tenant: &str) -> Arc<RouteSnapshot> {
    // 1) 快路径：缓存新鲜直接返回（不持锁做 IO）。
    if let Some(entry) = route_cache().lock().expect("路由缓存锁中毒").get(tenant)
        && entry.checked.elapsed() < CACHE_TTL
    {
        return entry.snap.clone();
    }
    // 2) 版本对账：三表 COUNT + MAX(updated_at)（一条 UNION 聚合，表都极小）。
    let version = route_fingerprint(db_id, tenant).await;
    {
        let mut map = route_cache().lock().expect("路由缓存锁中毒");
        if let Some(entry) = map.get(tenant) {
            if entry.checked.elapsed() < CACHE_TTL {
                // 别的请求刚刷新过。
                return entry.snap.clone();
            }
            if let Some(v) = &version
                && *v == entry.version
            {
                // 版本未变：只续 TTL，不重载。
                let snap = entry.snap.clone();
                if let Some(e) = map.get_mut(tenant) {
                    e.checked = Instant::now();
                }
                return snap;
            }
        }
    }
    // 3) 版本变了（或无缓存）：全量重载。
    let snap = Arc::new(load_route_snapshot(db_id, tenant).await.unwrap_or(RouteSnapshot {
        subscribers: Arc::new(Vec::new()),
        def_group: Arc::new(HashMap::new()),
        group_name: Arc::new(HashMap::new()),
    }));
    if let Some(v) = version {
        route_cache().lock().expect("路由缓存锁中毒").insert(
            tenant.to_string(),
            CacheEntry { snap: snap.clone(), version: v, checked: Instant::now() },
        );
    }
    snap
}

/// 三表指纹（订阅者仅 active 行——停用订阅者不参与匹配，其指纹变化不影响路由正确性）；
/// 表未建/查询失败 → None（下次再对账）。
async fn route_fingerprint(db_id: &str, tenant: &str) -> Option<Fingerprint> {
    let ds = query_sql_with_params(
        db_id,
        None,
        "SELECT 'sub' AS k, COUNT(*) AS n, MAX(updated_at)::text AS mx \
           FROM cmx_flow_event_subscriber WHERE tenant_id = $1 AND active = TRUE \
         UNION ALL \
         SELECT 'grp', COUNT(*), MAX(updated_at)::text FROM cmx_flow_def_group \
         UNION ALL \
         SELECT 'def', COUNT(*), MAX(updated_at)::text FROM cmx_flow_definition",
        SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]),
        "evt_route_fingerprint",
    )
    .await
    .ok()?;
    let schema = ds.schema.as_ref();
    let mut sub = (0i64, None::<String>);
    let mut grp = (0i64, None::<String>);
    let mut def = (0i64, None::<String>);
    for row in ds.iter() {
        match get_str(row, schema, "k").as_str() {
            "sub" => sub = (get_i64(row, schema, "n").unwrap_or(0), get_opt(row, schema, "mx")),
            "grp" => grp = (get_i64(row, schema, "n").unwrap_or(0), get_opt(row, schema, "mx")),
            "def" => def = (get_i64(row, schema, "n").unwrap_or(0), get_opt(row, schema, "mx")),
            _ => {}
        }
    }
    Some((sub, grp, def))
}

async fn load_route_snapshot(db_id: &str, tenant: &str) -> Result<RouteSnapshot, String> {
    let subs_sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_event_subscriber \
         WHERE tenant_id = $1 AND active = TRUE ORDER BY id"
    );
    let subscribers = query_sub_rows_p(
        db_id,
        &subs_sql,
        SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]),
        "evt_sub_active",
    )
    .await?;
    let ds = query_sql_with_params(
        db_id,
        None,
        "SELECT key, group_id FROM cmx_flow_definition WHERE group_id IS NOT NULL",
        SqlParams::DataValues(vec![]),
        "evt_def_groups",
    )
    .await
    .map_err(|e| format!("查定义分组失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut def_group = HashMap::with_capacity(ds.row_count());
    for row in ds.iter() {
        def_group.insert(
            get_str(row, schema, "key"),
            get_i64(row, schema, "group_id").unwrap_or(0),
        );
    }
    let groups = get_groups(db_id).await?;
    let group_name: HashMap<i64, String> = groups.into_iter().collect();
    Ok(RouteSnapshot {
        subscribers: Arc::new(subscribers),
        def_group: Arc::new(def_group),
        group_name: Arc::new(group_name),
    })
}

/// 参数版订阅者行查询（全部读侧 $n 绑定）。
async fn query_sub_rows_p(
    db_id: &str,
    sql: &str,
    params: SqlParams,
    tag: &str,
) -> Result<Vec<SubscriberRow>, String> {
    let ds = query_sql_with_params(db_id, None, sql, params, tag)
        .await
        .map_err(|e| format!("查订阅者失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds.iter().filter_map(|row| sub_row(row, schema)).collect())
}

// ============================================================
// 投递行
// ============================================================

/// 一条待写入的投递行（emit / test 审计 / rebuild 共用；state/租约列由 SQL 侧赋初值）。
#[derive(Debug, Clone)]
pub struct DeliveryInsert {
    pub subscriber_id: i64,
    pub subscriber_name: String,
    pub channel: String,
    pub event_id: String,
    pub delivery_id: String,
    /// emit | test | rebuild
    pub source: &'static str,
    pub event_type: String,
    pub definition_key: Option<String>,
    pub business_key: Option<String>,
    pub instance_id: String,
    pub payload: Value,
    /// 初始状态：emit/rebuild 行 PENDING（进队列）；test 行直达终态（不参与保序）。
    pub initial_state: &'static str,
    /// 终态直达时（test 行）的诊断三列。
    pub last_error: Option<String>,
    pub last_http_status: Option<i64>,
    pub last_response_snippet: Option<String>,
    /// DONE 时置 delivered_at。
    pub delivered: bool,
    /// 命中规则名快照（emit 时定版；test/rebuild 行 None）。
    pub matched_rule: Option<String>,
}

/// 单批 INSERT 的行数上限（19 列/行 × 上限，远低于 65535 参数上限）。
const INSERT_CHUNK: usize = 500;

/// 批量落投递行（单批写入；uk(subscriber_id, event_id) 吸收 rebuild/test 的确定性
/// event_id 重复写入——重复点击/重跑幂等）。返回实际插入行数。
pub async fn insert_deliveries(db_id: &str, rows: &[DeliveryInsert]) -> Result<u64, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    const COLS: &str = "id, subscriber_id, subscriber_name, channel, event_id, delivery_id, source, \
        event_type, definition_key, business_key, instance_id, payload, state, next_attempt_at, last_error, \
        last_http_status, last_response_snippet, matched_rule, delivered_at";
    let mut total = 0u64;
    for chunk in rows.chunks(INSERT_CHUNK) {
        let mut values = Vec::with_capacity(chunk.len());
        let mut params: Vec<DataValue> = Vec::with_capacity(chunk.len() * 18);
        for r in chunk {
            // 一行 19 列：占位 17 个 + 两个字面量列（next_attempt_at / delivered_at）——
            // emit/rebuild 行立即可投（now()）；test 审计行直达终态不进队列（NULL）。
            let start = params.len() + 1;
            let ph = |i: usize| format!("${}", start + i);
            let delivered = if r.delivered { "now()".to_string() } else { "NULL".to_string() };
            let next_attempt =
                if r.initial_state == "PENDING" { "now()".to_string() } else { "NULL".to_string() };
            values.push(format!(
                "({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}::jsonb, {}, {next_attempt}, {}, {}, {}, {}, {delivered})",
                ph(0), ph(1), ph(2), ph(3), ph(4), ph(5), ph(6), ph(7), ph(8), ph(9), ph(10),
                ph(11), ph(12), ph(13), ph(14), ph(15), ph(16),
            ));
            params.push(DataValue::Int(cmx_utils::next_pk_id()));
            params.push(DataValue::Int(r.subscriber_id));
            params.push(DataValue::String(r.subscriber_name.clone()));
            params.push(DataValue::String(r.channel.clone()));
            params.push(DataValue::String(r.event_id.clone()));
            params.push(DataValue::String(r.delivery_id.clone()));
            params.push(DataValue::String(r.source.to_string()));
            params.push(DataValue::String(r.event_type.clone()));
            params.push(opt_str(r.definition_key.clone()));
            params.push(opt_str(r.business_key.clone()));
            params.push(DataValue::String(r.instance_id.clone()));
            params.push(DataValue::Json(
                serde_json::to_string(&r.payload).unwrap_or_else(|_| "{}".into()),
            ));
            params.push(DataValue::String(r.initial_state.to_string()));
            params.push(opt_str(r.last_error.clone()));
            params.push(opt_int(r.last_http_status));
            params.push(opt_str(r.last_response_snippet.clone()));
            params.push(opt_str(r.matched_rule.clone()));
        }
        let sql = format!(
            "INSERT INTO cmx_flow_event_delivery ({COLS}) VALUES {} \
             ON CONFLICT (subscriber_id, event_id) DO NOTHING",
            values.join(", ")
        );
        let n = execute_sql_with_params(db_id, None, &sql, SqlParams::DataValues(params))
            .await
            .map_err(|e| format!("写入投递行失败: {e}"))?;
        total += n;
    }
    Ok(total)
}

/// 抢占到的投递行（claim RETURNING 投影）。
#[derive(Debug, Clone)]
pub struct ClaimedDelivery {
    pub id: i64,
    pub seq: i64,
    pub subscriber_id: i64,
    pub subscriber_name: String,
    pub channel: String,
    pub event_id: String,
    pub delivery_id: String,
    pub event_type: String,
    pub definition_key: Option<String>,
    pub business_key: Option<String>,
    pub instance_id: String,
    pub payload: Value,
    /// claim 时已 +1（retry_max 含首发的比较基准）。
    pub attempts: i64,
}

/// 抢占到期可投的行（租约模式 + SKIP LOCKED + 同订阅者保序守卫）。
///
/// - 可抢占态：`PENDING` 或**租约过期**的 `IN_FLIGHT`（worker 崩溃自愈）；
/// - 保序守卫：同订阅者存在更小 seq 的未终态行（PENDING/IN_FLIGHT，含退避等待）即阻塞——
///   终态 DONE/DEAD/SKIPPED 不阻塞；test 行不参与保序（source='emit' 守卫）；
/// - claim 即 `attempts+1` 并打租约（locked_by/lock_expires_at）。
pub async fn claim_due_deliveries(
    db_id: &str,
    worker: &str,
    lease_secs: i64,
    limit: usize,
) -> Result<Vec<ClaimedDelivery>, String> {
    // 到期/租约比较全用 **DB 时钟 now()**（与 next_attempt_at / lock_expires_at 的赋值时钟
    // 同源）——应用时钟与 DB 时钟的偏差会让「插完即抢」的行被判成未到期。
    let sql = format!(
        r#"WITH candidates AS (
        SELECT d.id FROM cmx_flow_event_delivery d
        WHERE (d.state = 'PENDING' OR (d.state = 'IN_FLIGHT' AND d.lock_expires_at <= now()))
          AND d.next_attempt_at <= now()
          AND NOT EXISTS (
            SELECT 1 FROM cmx_flow_event_delivery b
            WHERE b.subscriber_id = d.subscriber_id AND b.source = 'emit'
              AND b.seq < d.seq AND b.state IN ('PENDING','IN_FLIGHT'))
        ORDER BY d.seq
        FOR UPDATE SKIP LOCKED LIMIT $1)
        UPDATE cmx_flow_event_delivery w
        SET state = 'IN_FLIGHT', locked_by = $2,
            lock_expires_at = now() + interval '{lease_secs} seconds', attempts = attempts + 1
        WHERE id IN (SELECT id FROM candidates)
        RETURNING w.id, w.seq, w.subscriber_id, w.subscriber_name, w.channel, w.event_id,
                  w.delivery_id, w.event_type, w.definition_key, w.business_key, w.instance_id,
                  w.payload, w.attempts"#
    );
    let params = SqlParams::DataValues(vec![
        DataValue::Int(limit as i64),
        DataValue::String(worker.to_string()),
    ]);
    let ds = query_sql_with_params(db_id, None, &sql, params, "evt_dlv_claim")
        .await
        .map_err(|e| format!("抢占投递行失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        let payload = get_opt(row, schema, "payload").unwrap_or_else(|| "{}".into());
        out.push(ClaimedDelivery {
            id: get_i64(row, schema, "id").unwrap_or(0),
            seq: get_i64(row, schema, "seq").unwrap_or(0),
            subscriber_id: get_i64(row, schema, "subscriber_id").unwrap_or(0),
            subscriber_name: get_str(row, schema, "subscriber_name"),
            channel: get_str(row, schema, "channel"),
            event_id: get_str(row, schema, "event_id"),
            delivery_id: get_str(row, schema, "delivery_id"),
            event_type: get_str(row, schema, "event_type"),
            definition_key: get_opt(row, schema, "definition_key"),
            business_key: get_opt(row, schema, "business_key"),
            instance_id: get_str(row, schema, "instance_id"),
            payload: serde_json::from_str(&payload).unwrap_or_else(|_| json!({})),
            attempts: get_i64(row, schema, "attempts").unwrap_or(0),
        });
    }
    Ok(out)
}

/// 批内逐行续租：把本 worker 名下全部 IN_FLIGHT 行的租约延长一窗——每完成一行调一次，
/// 使批长度不进入租约算术（租约 120s 只须大于单行 30s 超时）。DB 时钟统一。
pub async fn renew_leases(db_id: &str, worker: &str, lease_secs: i64) -> Result<u64, String> {
    let sql = "UPDATE cmx_flow_event_delivery \
               SET lock_expires_at = now() + ($1::int * interval '1 second') \
               WHERE locked_by = $2 AND state = 'IN_FLIGHT'";
    let params = SqlParams::DataValues(vec![
        DataValue::Int(lease_secs),
        DataValue::String(worker.to_string()),
    ]);
    execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("续租失败: {e}"))
}

/// 成功 → DONE。
pub async fn finish_done(db_id: &str, id: i64, worker: &str) -> Result<bool, String> {
    let sql = "UPDATE cmx_flow_event_delivery SET state = 'DONE', delivered_at = now(), \
               locked_by = NULL, lock_expires_at = NULL, next_attempt_at = NULL, last_error = NULL \
               WHERE id = $1 AND locked_by = $2";
    let n = execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![DataValue::Int(id), DataValue::String(worker.to_string())]),
    )
    .await
    .map_err(|e| format!("落投递成功态失败: {e}"))?;
    Ok(n > 0)
}

/// 失败诊断三列（last_error / last_http_status / last_response_snippet）。
pub struct Diagnostics {
    pub error: String,
    pub http_status: Option<i64>,
    pub snippet: Option<String>,
}

/// 可重试失败 → 退避后回 PENDING；尝试耗尽（attempts ≥ retry_max，含首发口径）→ DEAD。
pub async fn finish_retry_or_dead(
    db_id: &str,
    id: i64,
    worker: &str,
    attempts: i64,
    retry_max: i32,
    backoff: chrono::Duration,
    d: &Diagnostics,
) -> Result<bool, String> {
    let (error, http_status, snippet) = (&d.error, d.http_status, d.snippet.as_deref());
    let (state, next_clause) = if attempts >= retry_max as i64 {
        ("DEAD", "next_attempt_at = NULL".to_string())
    } else {
        (
            "PENDING",
            format!("next_attempt_at = now() + interval '{} seconds'", backoff.num_seconds().max(1)),
        )
    };
    let sql = format!(
        "UPDATE cmx_flow_event_delivery SET state = '{state}', {next_clause}, \
         lock_expires_at = NULL, locked_by = NULL, last_error = $2, last_http_status = $3, \
         last_response_snippet = $4 \
         WHERE id = $1 AND locked_by = $5"
    );
    let n = execute_sql_with_params(
        db_id,
        None,
        &sql,
        SqlParams::DataValues(vec![
            DataValue::Int(id),
            DataValue::String(error.to_string()),
            opt_int(http_status),
            opt_str(snippet.map(str::to_string)),
            DataValue::String(worker.to_string()),
        ]),
    )
    .await
    .map_err(|e| format!("落投递失败态: {e}"))?;
    Ok(n > 0)
}

/// 不可重试失败（其余 4xx / 配置性错误 / 订阅者已删）→ 直达 DEAD（不进退避，白耗窗）。
pub async fn finish_dead(
    db_id: &str,
    id: i64,
    worker: &str,
    d: &Diagnostics,
) -> Result<bool, String> {
    let (error, http_status, snippet) = (&d.error, d.http_status, d.snippet.as_deref());
    let sql = "UPDATE cmx_flow_event_delivery SET state = 'DEAD', next_attempt_at = NULL, \
               lock_expires_at = NULL, locked_by = NULL, last_error = $2, last_http_status = $3, \
               last_response_snippet = $4 \
               WHERE id = $1 AND locked_by = $5";
    let n = execute_sql_with_params(
        db_id,
        None,
        sql,
        SqlParams::DataValues(vec![
            DataValue::Int(id),
            DataValue::String(error.to_string()),
            opt_int(http_status),
            opt_str(snippet.map(str::to_string)),
            DataValue::String(worker.to_string()),
        ]),
    )
    .await
    .map_err(|e| format!("落投递死信态: {e}"))?;
    Ok(n > 0)
}

/// 投递流水分页过滤。
#[derive(Clone)]
pub struct DlvFilter {
    pub subscriber_id: Option<i64>,
    pub state: Option<String>,
    pub channel: Option<String>,
    pub definition_key: Option<String>,
    pub matched_rule: Option<String>,
    pub page: i64,
    pub page_size: i64,
}

impl Default for DlvFilter {
    fn default() -> Self {
        Self {
            subscriber_id: None,
            state: None,
            channel: None,
            definition_key: None,
            matched_rule: None,
            page: 1,
            page_size: 20,
        }
    }
}

impl DlvFilter {
    pub fn norm(&self) -> (i64, i64) {
        (self.page.max(1), self.page_size.clamp(1, 200))
    }
}

const DLV_COLS: &str = "id, seq, subscriber_id, subscriber_name, channel, event_id, delivery_id, \
    source, event_type, definition_key, business_key, instance_id, payload, state, attempts, \
    next_attempt_at, locked_by, last_error, last_http_status, last_response_snippet, matched_rule, created_at, delivered_at";

fn dlv_json(row: &cmx_core::model::data::dataset::Row, schema: &cmx_core::model::data::dataset::Schema) -> Value {
    json!({
        "id": get_i64(row, schema, "id").unwrap_or(0),
        "seq": get_i64(row, schema, "seq").unwrap_or(0),
        "subscriberId": get_i64(row, schema, "subscriber_id").unwrap_or(0),
        "subscriberName": get_str(row, schema, "subscriber_name"),
        "channel": get_str(row, schema, "channel"),
        "eventId": get_str(row, schema, "event_id"),
        "deliveryId": get_str(row, schema, "delivery_id"),
        "source": get_str(row, schema, "source"),
        "eventType": get_str(row, schema, "event_type"),
        "definitionKey": get_opt(row, schema, "definition_key"),
        "businessKey": get_opt(row, schema, "business_key"),
        "instanceId": get_str(row, schema, "instance_id"),
        "payload": serde_json::from_str::<Value>(&get_opt(row, schema, "payload").unwrap_or_default()).unwrap_or(json!({})),
        "state": get_str(row, schema, "state"),
        "attempts": get_i64(row, schema, "attempts").unwrap_or(0),
        "nextAttemptAt": get_ts(row, schema, "next_attempt_at"),
        "lockedBy": get_opt(row, schema, "locked_by"),
        "lastError": get_opt(row, schema, "last_error"),
        "lastHttpStatus": get_i64(row, schema, "last_http_status"),
        "lastResponseSnippet": get_opt(row, schema, "last_response_snippet"),
        "matchedRule": get_opt(row, schema, "matched_rule"),
        "createdAt": get_ts(row, schema, "created_at"),
        "deliveredAt": get_ts(row, schema, "delivered_at"),
    })
}

/// 投递流水分页（按 seq DESC；租户经 subscriber 关联过滤）。
pub async fn query_deliveries(db_id: &str, tenant: &str, f: &DlvFilter) -> Result<(Vec<Value>, i64), String> {
    let (page, size) = f.norm();
    let mut params: Vec<DataValue> = vec![DataValue::String(tenant.to_string())];
    let mut pn = 1;
    let mut cond = format!(
        "d.subscriber_id IN (SELECT id FROM cmx_flow_event_subscriber WHERE tenant_id = ${pn})"
    );
    if let Some(sid) = f.subscriber_id {
        pn += 1;
        cond.push_str(&format!(" AND d.subscriber_id = ${pn}"));
        params.push(DataValue::Int(sid));
    }
    if let Some(st) = f.state.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND d.state = ${pn}"));
        params.push(DataValue::String(st.trim().to_uppercase()));
    }
    if let Some(ch) = f.channel.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND d.channel = ${pn}"));
        params.push(DataValue::String(ch.trim().to_string()));
    }
    if let Some(dk) = f.definition_key.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND d.definition_key = ${pn}"));
        params.push(DataValue::String(dk.trim().to_string()));
    }
    if let Some(mr) = f.matched_rule.as_deref().filter(|s| !s.trim().is_empty()) {
        pn += 1;
        cond.push_str(&format!(" AND d.matched_rule = ${pn}"));
        params.push(DataValue::String(mr.trim().to_string()));
    }
    let total = query_one_i64_p(
        db_id,
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_event_delivery d WHERE {cond}"),
        SqlParams::DataValues(params.clone()),
        "evt_dlv_count",
    )
    .await?;
    let offset = (page - 1) * size;
    let mut list_params = params;
    list_params.push(DataValue::Int(size));
    list_params.push(DataValue::Int(offset));
    let sql = format!(
        "SELECT {DLV_COLS} FROM cmx_flow_event_delivery d WHERE {cond} \
         ORDER BY d.seq DESC LIMIT ${} OFFSET ${}",
        pn + 1,
        pn + 2
    );
    let ds = query_sql_with_params(db_id, None, &sql, SqlParams::DataValues(list_params), "evt_dlv_list")
        .await
        .map_err(|e| format!("查投递流水失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let rows: Vec<Value> = ds.iter().map(|row| dlv_json(row, schema)).collect();
    Ok((rows, total))
}

/// 投递统计（KPI）：时间窗（`hours` 小时内，DB 时钟 now() 反推；source 维度双口径——
/// 成功率只算 emit 业务行，test/rebuild 行计入总量但不稀释成功率）。
/// 附 per-subscriber 死信/待投 TOP（页面死信下钻用，取前 20）。
pub async fn delivery_stats(
    db_id: &str,
    tenant: &str,
    hours: i64,
    subscriber_id: Option<i64>,
) -> Result<Value, String> {
    let hours = hours.clamp(1, 24 * 30);
    #[allow(clippy::useless_format)] // 与 push_str 拼接链同形态（X3-12 参数化纪律）
    let mut cond = format!(
        "created_at >= now() - ($1::int * interval '1 hour') \
         AND subscriber_id IN (SELECT id FROM cmx_flow_event_subscriber WHERE tenant_id = $2)"
    );
    let mut params = vec![
        DataValue::Int(hours),
        DataValue::String(tenant.to_string()),
    ];
    if let Some(sid) = subscriber_id {
        cond.push_str(" AND subscriber_id = $3");
        params.push(DataValue::Int(sid));
    }
    let ds = query_sql_with_params(
        db_id,
        None,
        &format!(
            "SELECT state, source, COUNT(*) AS n FROM cmx_flow_event_delivery WHERE {cond} \
             GROUP BY state, source"
        ),
        SqlParams::DataValues(params),
        "evt_dlv_stats",
    )
    .await
    .map_err(|e| format!("查投递统计失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut total: i64 = 0;
    let mut by_state: std::collections::BTreeMap<String, i64> = Default::default();
    // 成功率口径只算 emit 行。
    let mut emit_states: std::collections::BTreeMap<String, i64> = Default::default();
    for row in ds.iter() {
        let state = get_str(row, schema, "state");
        let source = get_str(row, schema, "source");
        let n = get_i64(row, schema, "n").unwrap_or(0);
        total += n;
        *by_state.entry(state.clone()).or_insert(0) += n;
        if source == "emit" {
            *emit_states.entry(state).or_insert(0) += n;
        }
    }
    let g = |m: &std::collections::BTreeMap<String, i64>, k: &str| m.get(k).copied().unwrap_or(0);
    let done = g(&emit_states, "DONE");
    let dead = g(&emit_states, "DEAD");
    let pending = g(&emit_states, "PENDING");
    let in_flight = g(&emit_states, "IN_FLIGHT");
    let denom = done + dead + pending + in_flight;
    let success_rate = if denom == 0 { None } else { Some((done as f64 / denom as f64 * 10000.0).round() / 100.0) };
    Ok(json!({
        "windowHours": hours,
        "total": total,
        "byState": by_state,
        "emit": {
            "done": done, "dead": dead, "pending": pending, "inFlight": in_flight,
            "skipped": g(&emit_states, "SKIPPED"),
            "successRate": success_rate,
        },
    }))
}

/// 死信重发：DEAD（及**租约已过期**的 IN_FLIGHT）行批量重置 PENDING，清租约、attempts
/// 归零（人工显式重发 = 重置完整重试预算）。返回重置行数。
pub async fn retry_deliveries(
    db_id: &str,
    tenant: &str,
    ids: &[i64],
    subscriber_id: Option<i64>,
    state: Option<&str>,
) -> Result<u64, String> {
    #[allow(clippy::useless_format)] // 与 push_str 拼接链同形态（X3-12 参数化纪律）
    let mut cond = format!(
        "subscriber_id IN (SELECT id FROM cmx_flow_event_subscriber WHERE tenant_id = $1)"
    );
    if !ids.is_empty() {
        let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        cond.push_str(&format!(" AND id IN ({id_list})"));
    }
    if let Some(sid) = subscriber_id {
        cond.push_str(&format!(" AND subscriber_id = {sid}"));
    }
    match state.map(str::trim).filter(|s| !s.is_empty()).map(str::to_uppercase) {
        Some(ref s) if s == "IN_FLIGHT" => {
            cond.push_str(" AND state = 'IN_FLIGHT' AND lock_expires_at <= now()");
        }
        Some(ref s) if s == "DEAD" => cond.push_str(" AND state = 'DEAD'"),
        _ => cond.push_str(
            " AND (state = 'DEAD' OR (state = 'IN_FLIGHT' AND lock_expires_at <= now()))",
        ),
    }
    let sql = format!(
        "UPDATE cmx_flow_event_delivery SET state = 'PENDING', attempts = 0, \
         next_attempt_at = now(), locked_by = NULL, lock_expires_at = NULL WHERE {cond}"
    );
    execute_sql_with_params(
        db_id,
        None,
        &sql,
        SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]),
    )
    .await
    .map_err(|e| format!("重发投递行失败: {e}"))
}

/// 死信处置：DEAD/PENDING → SKIPPED（人工确认放弃的显式留痕；PENDING 出口给停用订阅者
/// 的存量待投行一个弃投入口）。
pub async fn skip_deliveries(db_id: &str, tenant: &str, ids: &[i64]) -> Result<u64, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE cmx_flow_event_delivery SET state = 'SKIPPED', next_attempt_at = NULL, \
         locked_by = NULL, lock_expires_at = NULL \
         WHERE id IN ({id_list}) AND state IN ('DEAD','PENDING') \
         AND subscriber_id IN (SELECT id FROM cmx_flow_event_subscriber WHERE tenant_id = $1)"
    );
    execute_sql_with_params(
        db_id,
        None,
        &sql,
        SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]),
    )
    .await
    .map_err(|e| format!("处置死信失败: {e}"))
}

/// 手动清理 DONE / SKIPPED 行（保留 N 天；常态由自动清理承担，端点保留）。
/// 按行自身 state + created_at 全库清（订阅者物理删后其名下行不被 IN 子查询挡住）；
/// 库即租户边界，清理无租户语义。
pub async fn purge_deliveries(db_id: &str, before_days: i64, state: Option<&str>) -> Result<u64, String> {
    let states = match state.map(str::trim).filter(|s| !s.is_empty()).map(str::to_uppercase) {
        Some(ref s) if s == "DONE" || s == "SKIPPED" => format!("'{s}'"),
        _ => "'DONE','SKIPPED'".to_string(),
    };
    let days = before_days.clamp(1, 365);
    let sql = format!(
        "DELETE FROM cmx_flow_event_delivery WHERE state IN ({states}) \
         AND created_at < now() - interval '{days} days'"
    );
    execute_sql(db_id, None, &sql)
        .await
        .map_err(|e| format!("清理投递行失败: {e}"))
}

// ============================================================
// 小工具
// ============================================================

fn opt_str(v: Option<String>) -> DataValue {
    match v {
        Some(s) => DataValue::String(s),
        None => DataValue::Null,
    }
}

/// 可空整型（绑定到 INT 列的 NULL 须带类型标记）。
fn opt_int(v: Option<i64>) -> DataValue {
    match v {
        Some(i) => DataValue::Int(i),
        None => DataValue::NullTyped(SqlTypeMarker::Int),
    }
}

async fn query_one_i64_p(
    db_id: &str,
    sql: &str,
    params: SqlParams,
    tag: &str,
) -> Result<i64, String> {
    let ds = query_sql_with_params(db_id, None, sql, params, tag)
        .await
        .map_err(|e| format!("{tag} 失败: {e}"))?;
    Ok(ds
        .iter()
        .next()
        .and_then(|row| get_i64(row, ds.schema.as_ref(), "n"))
        .unwrap_or(0))
}

fn get_str(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> String {
    get_opt(row, schema, col).unwrap_or_default()
}

fn get_opt(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        // jsonb 列读回是 Json(String)——原样取字符串（调用方 from_str 解析）。
        Some(DataValue::Json(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_i64(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> Option<i64> {
    match row.get_by_name(schema, col) {
        Some(DataValue::Int(v)) => Some(*v),
        Some(DataValue::Float(v)) => Some(*v as i64),
        _ => None,
    }
}

fn get_bool(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> bool {
    matches!(row.get_by_name(schema, col), Some(DataValue::Bool(true)))
}

/// TIMESTAMPTZ 读回 → RFC3339 字符串。
fn get_ts(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::DateTime(dt)) => Some(dt.to_rfc3339()),
        Some(DataValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}
