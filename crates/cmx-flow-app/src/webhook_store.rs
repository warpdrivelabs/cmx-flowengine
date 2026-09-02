//! 出站 webhook 订阅 + 持久化投递队列的存储层（001 方案 v2.3 §三/§4）。
//!
//! 惯例对齐 [`crate::biz_link`]：薄持久化直走 cmx-database-pg、落当前租户运行态库
//! （`cmx_flow_*` 表 = 业务库）、DDL 幂等自举（engine build 调一次 + 生产走 docs/sql 迁移）。
//! 两表主键 = BIGINT 应用层 Pk52 雪花（`cmx_utils::next_pk_id`，52 位 JS 安全）；
//! 投递表保序键独立为 `seq BIGSERIAL`（DB 提交序）——身份与排序分离（决议 20/21）。
//!
//! 多副本（方案 §7）：订阅列表走进程内缓存 + **TTL 5s 版本对账**（轮询
//! COUNT + MAX(updated_at)，均为 DB 时钟赋值，不受应用时钟漂移影响）；写操作失效本副本，
//! 跨副本 ≤5s 收敛。抢占/续租/结果落库全带租约列——见 [`crate::webhook_outbox`]。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cmx_core::model::cell::{DataValue, SqlTypeMarker};
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql, query_sql_with_params};
use serde_json::{Value, json};

use cmx_flow_adapters::WebhookTarget;

/// 订阅缓存 TTL（版本对账周期；跨副本收敛上界 ≈ 此值）。
pub const CACHE_TTL: Duration = Duration::from_secs(5);

// ============================================================
// DDL（幂等自举；与 docs/sql/v2/biz init_ddl + 20260901_001 迁移同源维护）
// ============================================================

/// 建表 DDL（幂等）。engine build 时调一次自举；生产走 docs/sql 迁移。
pub const DDL_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_webhook_subscription (
        id              BIGINT        NOT NULL,
        name            VARCHAR(128)  NOT NULL,
        channel         VARCHAR(16)   NOT NULL DEFAULT 'webhook',
        channel_config  JSONB         NOT NULL DEFAULT '{}',
        definition_keys JSONB         NOT NULL DEFAULT '[]',
        event_types     JSONB         NOT NULL DEFAULT '[]',
        active          BOOLEAN       NOT NULL DEFAULT TRUE,
        retry_max       INT           NOT NULL DEFAULT 10,
        source          VARCHAR(8)    NOT NULL DEFAULT 'manual',
        tenant_id       VARCHAR(64)   NOT NULL DEFAULT 'default',
        created_by      VARCHAR(64),
        created_at      TIMESTAMPTZ   NOT NULL DEFAULT now(),
        updated_at      TIMESTAMPTZ   NOT NULL DEFAULT now(),
        PRIMARY KEY (id)
    )"#,
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_webhook_sub_name ON cmx_flow_webhook_subscription (tenant_id, name)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_webhook_sub_upd ON cmx_flow_webhook_subscription (updated_at)",
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_webhook_delivery (
        id                    BIGINT        NOT NULL,
        seq                   BIGSERIAL     NOT NULL,
        subscription_id       BIGINT        NOT NULL,
        subscription_name     VARCHAR(128)  NOT NULL,
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
        route_source          VARCHAR(8)    NOT NULL DEFAULT 'matched',
        created_at            TIMESTAMPTZ   NOT NULL DEFAULT now(),
        delivered_at          TIMESTAMPTZ,
        PRIMARY KEY (id)
    )"#,
    // 幂等补列（既有库升级到 v2.4 三级路由时补 route_source；五处同改纪律 §3.3）。
    "ALTER TABLE cmx_flow_webhook_delivery ADD COLUMN IF NOT EXISTS route_source VARCHAR(8) NOT NULL DEFAULT 'matched'",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_webhook_dlv_seq ON cmx_flow_webhook_delivery (seq)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_webhook_dlv_sub_event ON cmx_flow_webhook_delivery (subscription_id, event_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_webhook_dlv_due ON cmx_flow_webhook_delivery (state, next_attempt_at)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_webhook_dlv_sub ON cmx_flow_webhook_delivery (subscription_id, seq)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_webhook_dlv_did ON cmx_flow_webhook_delivery (delivery_id)",
];

/// 自举建表（engine build 后调一次）。失败返回错误（调用方告警不阻断启动）。
pub async fn ensure_schema(db_id: &str) -> Result<(), String> {
    for stmt in DDL_STATEMENTS {
        execute_sql(db_id, None, stmt).await.map_err(|e| format!("webhook 建表失败: {e}"))?;
    }
    Ok(())
}

// ============================================================
// 订阅行
// ============================================================

/// 一条订阅（channel_config 为原始 JSON；secret 掩码在 handler 层做）。
#[derive(Debug, Clone)]
pub struct SubRow {
    pub id: i64,
    pub name: String,
    pub channel: String,
    pub channel_config: Value,
    pub definition_keys: Vec<String>,
    pub event_types: Vec<String>,
    pub active: bool,
    pub retry_max: i32,
    pub source: String,
    pub tenant_id: String,
    pub created_by: Option<String>,
    /// 非终态绑定实例数（仅 list/detail 投影查询填充；匹配/缓存路径不查、恒 0）。
    pub binding_count: i64,
}

/// 订阅保存入参（secret 已由 handler 解析为**终值**：留空/掩码 = 沿用旧值在 handler 处理）。
#[derive(Debug, Clone)]
pub struct SubUpsert {
    pub id: Option<i64>,
    pub name: String,
    pub channel: String,
    pub channel_config: Value,
    pub definition_keys: Vec<String>,
    pub event_types: Vec<String>,
    pub active: bool,
    pub retry_max: i32,
    pub created_by: Option<String>,
}

const SUB_COLS: &str = "id, name, channel, channel_config, definition_keys, event_types, active, retry_max, source, tenant_id, created_by";

fn sub_row(row: &cmx_core::model::data::dataset::Row, schema: &cmx_core::model::data::dataset::Schema) -> Option<SubRow> {
    let channel_config = get_opt(row, schema, "channel_config")?;
    let definition_keys = get_opt(row, schema, "definition_keys")?;
    let event_types = get_opt(row, schema, "event_types")?;
    Some(SubRow {
        id: get_i64(row, schema, "id")?,
        name: get_str(row, schema, "name"),
        channel: get_str(row, schema, "channel"),
        channel_config: serde_json::from_str(&channel_config).unwrap_or(Value::Null),
        definition_keys: serde_json::from_str(&definition_keys).unwrap_or_default(),
        event_types: serde_json::from_str(&event_types).unwrap_or_default(),
        active: get_bool(row, schema, "active"),
        retry_max: get_i64(row, schema, "retry_max").unwrap_or(10) as i32,
        source: get_str(row, schema, "source"),
        tenant_id: get_str(row, schema, "tenant_id"),
        created_by: get_opt(row, schema, "created_by"),
        // 投影 SQL 未带该列（缓存/匹配路径）时取 0。
        binding_count: get_i64(row, schema, "binding_count").unwrap_or(0),
    })
}

fn sub_json(s: &SubRow) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "channel": s.channel,
        "channelConfig": s.channel_config,
        "definitionKeys": s.definition_keys,
        "eventTypes": s.event_types,
        "active": s.active,
        "retryMax": s.retry_max,
        "source": s.source,
        "tenantId": s.tenant_id,
        "createdBy": s.created_by,
        "bindingCount": s.binding_count,
    })
}

/// 订阅列表过滤。
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

/// 分页列订阅（keyword 模糊匹配名称；ORDER BY created_at DESC）。
pub async fn list_subscriptions(
    db_id: &str,
    tenant: &str,
    f: &SubFilter,
) -> Result<(Vec<Value>, i64), String> {
    let (page, size) = f.norm();
    let mut cond = format!("tenant_id = '{}'", esc(tenant));
    if let Some(kw) = f.keyword.as_deref().filter(|s| !s.trim().is_empty()) {
        // 017 小项：keyword 中的 LIKE 通配符转义为字面量（ESCAPE '\'）。
        let kw = kw.trim().to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        cond.push_str(&format!(" AND name LIKE '%{kw}%' ESCAPE '\\'"));
    }
    if let Some(ch) = f.channel.as_deref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND channel = '{}'", esc(ch.trim())));
    }
    if let Some(a) = f.active {
        cond.push_str(&format!(" AND active = {}", if a { "TRUE" } else { "FALSE" }));
    }
    let total = query_one_i64(
        db_id,
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_webhook_subscription WHERE {cond}"),
        "wh_sub_count",
    )
    .await?;
    let offset = (page - 1) * size;
    // 投影附非终态绑定实例数（v2.4 §3.6 可见性；订阅量小，相关子查询开销可忽略）。
    let sql = format!(
        "SELECT {SUB_COLS}, \
         (SELECT COUNT(*) FROM cmx_flow_instance i \
           WHERE i.subscriber_id = s.id AND i.state NOT IN ('COMPLETED','TERMINATED')) AS binding_count \
         FROM cmx_flow_webhook_subscription s WHERE {cond} \
         ORDER BY created_at DESC LIMIT {size} OFFSET {offset}"
    );
    let rows = query_sub_rows(db_id, &sql, "wh_sub_list").await?;
    Ok((rows.iter().map(sub_json).collect(), total))
}

/// 取一条订阅。
pub async fn get_subscription(db_id: &str, tenant: &str, id: i64) -> Result<Option<SubRow>, String> {
    let sql = format!(
        "SELECT {SUB_COLS}, \
         (SELECT COUNT(*) FROM cmx_flow_instance i \
           WHERE i.subscriber_id = s.id AND i.state NOT IN ('COMPLETED','TERMINATED')) AS binding_count \
         FROM cmx_flow_webhook_subscription s \
         WHERE id = {id} AND tenant_id = '{}'",
        esc(tenant)
    );
    Ok(query_sub_rows(db_id, &sql, "wh_sub_get").await?.into_iter().next())
}

/// 按一批 id 取订阅（poller 投递前取通道配置用；不筛 active——停用订阅的存量行仍投）。
pub async fn get_subscriptions_by_ids(
    db_id: &str,
    tenant: &str,
    ids: &[i64],
) -> Result<HashMap<i64, SubRow>, String> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_webhook_subscription \
         WHERE id IN ({id_list}) AND tenant_id = '{}'",
        esc(tenant)
    );
    Ok(query_sub_rows(db_id, &sql, "wh_sub_by_ids")
        .await?
        .into_iter()
        .map(|s| (s.id, s))
        .collect())
}

/// 新建/更新一条订阅。新建用 Pk52 铸号；updated_at 由 DB 时钟 now() 赋值（决议 19）。
pub async fn upsert_subscription(db_id: &str, tenant: &str, s: &SubUpsert) -> Result<i64, String> {
    let id = s.id.unwrap_or_else(cmx_utils::next_pk_id);
    let cfg = serde_json::to_string(&s.channel_config).unwrap_or_else(|_| "{}".into());
    let dks = serde_json::to_string(&s.definition_keys).unwrap_or_else(|_| "[]".into());
    let ets = serde_json::to_string(&s.event_types).unwrap_or_else(|_| "[]".into());
    let sql = "INSERT INTO cmx_flow_webhook_subscription \
        (id, name, channel, channel_config, definition_keys, event_types, active, retry_max, source, tenant_id, created_by) \
        VALUES ($1, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb, $7, $8, 'manual', $9, $10) \
        ON CONFLICT (id) DO UPDATE SET \
          name = EXCLUDED.name, channel = EXCLUDED.channel, channel_config = EXCLUDED.channel_config, \
          definition_keys = EXCLUDED.definition_keys, event_types = EXCLUDED.event_types, \
          active = EXCLUDED.active, retry_max = EXCLUDED.retry_max, updated_at = now()";
    let params = SqlParams::DataValues(vec![
        DataValue::Int(id),
        DataValue::String(s.name.clone()),
        DataValue::String(s.channel.clone()),
        DataValue::Json(cfg),
        DataValue::Json(dks),
        DataValue::Json(ets),
        DataValue::Bool(s.active),
        DataValue::Int(s.retry_max as i64),
        DataValue::String(tenant.to_string()),
        opt_str(s.created_by.clone()),
    ]);
    execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("保存订阅失败: {e}"))?;
    invalidate_cache(tenant);
    Ok(id)
}

/// 物理删一条订阅——**仅停用态且无非终态绑定实例可删**（v2.4 §3.5 单条 SQL 守卫：
/// check-then-act 的 TOCTOU 窗口压缩进一条语句；流水凭 subscription_name 快照保审计）。
/// 返回 Err(原因) 当订阅不存在 / 仍启用 / 存在进行中的绑定实例。
pub async fn delete_subscription(db_id: &str, tenant: &str, id: i64) -> Result<(), String> {
    let sub = get_subscription(db_id, tenant, id)
        .await?
        .ok_or_else(|| format!("订阅不存在: {id}"))?;
    if sub.active {
        return Err("订阅仍处于启用状态，请先停用再删除".into());
    }
    // 单条守卫 SQL（方案 §3.5）：0 行命中 = 有非终态绑定实例（或并发侧写），拒绝并给原因。
    let sql = "DELETE FROM cmx_flow_webhook_subscription s \
               WHERE s.id = $1 AND s.active = FALSE \
                 AND NOT EXISTS (SELECT 1 FROM cmx_flow_instance i \
                                  WHERE i.subscriber_id = s.id \
                                    AND i.state NOT IN ('COMPLETED','TERMINATED'))";
    let params = SqlParams::DataValues(vec![DataValue::Int(id)]);
    let n = execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("删除订阅失败: {e}"))?;
    if n == 0 {
        let bc = binding_count(db_id, id).await?;
        return Err(format!("存在进行中的绑定实例 {bc} 个，无法删除（须待其办结/终止，或走 SQL 运维例外清单人工处置）"));
    }
    invalidate_cache(tenant);
    Ok(())
}

/// 非终态绑定实例数（守卫文案 + 停用提示共用；绑定列部分索引命中）。
pub async fn binding_count(db_id: &str, subscriber_id: i64) -> Result<i64, String> {
    let sql = "SELECT COUNT(*) AS n FROM cmx_flow_instance i \
               WHERE i.subscriber_id = $1 AND i.state NOT IN ('COMPLETED','TERMINATED')";
    let params = SqlParams::DataValues(vec![DataValue::Int(subscriber_id)]);
    let ds = query_sql_with_params(db_id, None, sql, params, "wh_sub_binding_count")
        .await
        .map_err(|e| format!("查绑定实例数失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .next()
        .and_then(|row| get_i64(row, schema, "n"))
        .unwrap_or(0))
}

/// 按订阅名点查（发起绑定 name → id 解析；id 每环境不同不作引用形态，见方案 §3.4）。
pub async fn get_subscription_by_name(
    db_id: &str,
    tenant: &str,
    name: &str,
) -> Result<Option<SubRow>, String> {
    let sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_webhook_subscription \
         WHERE tenant_id = '{}' AND name = '{}'",
        esc(tenant),
        esc(name)
    );
    Ok(query_sub_rows(db_id, &sql, "wh_sub_by_name")
        .await?
        .into_iter()
        .next())
}

/// 定义订阅（L3）：把 definitionKeys **增量并入**指定订阅的 definition_keys。
///
/// 实现硬约束（方案 §四 R6）：单语句原子 UPDATE、**仅触碰 definition_keys 列**（禁复用
/// upsert_subscription——它全列覆盖，会把 secret/通道配置清空）；并发读改写丢失更新由
/// 单语句原子性消除；空集防线进 SQL 谓词——通配行（definition_keys='[]'，含 L1 env 导入）
/// 不可 subscribe（防静默窄化 L1 默认层），停用订阅不可变更。影响 0 行即拒绝（原因由调用方
/// 借 [`get_subscription_by_name`] 分类）。
pub async fn subscribe_definitions(
    db_id: &str,
    tenant: &str,
    name: &str,
    keys: &[String],
) -> Result<(), String> {
    let keys_json = serde_json::to_string(keys).map_err(|e| format!("序列化定义集失败: {e}"))?;
    // 并集去重（DISTINCT 排序输出）；谓词拦：停用 / env 行 / 通配行。
    let sql = "UPDATE cmx_flow_webhook_subscription s \
               SET definition_keys = ( \
                     SELECT COALESCE(jsonb_agg(DISTINCT k ORDER BY k), '[]'::jsonb) \
                     FROM jsonb_array_elements(s.definition_keys || $1::jsonb) t(k)), \
                   updated_at = now() \
               WHERE s.tenant_id = $2 AND s.name = $3 AND s.active = TRUE \
                 AND s.source <> 'env' AND s.definition_keys <> '[]'::jsonb";
    let params = SqlParams::DataValues(vec![
        DataValue::Json(keys_json),
        DataValue::String(tenant.to_string()),
        DataValue::String(name.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("定义订阅失败: {e}"))?;
    if n > 0 {
        invalidate_cache(tenant);
    }
    Ok(())
}

/// 定义退订（L3）：从 definition_keys **增量删除** definitionKeys。
///
/// 空化校验进 SQL 谓词（EXISTS 至少剩一个不删元素）：将导致 definition_keys 为空（= 通配，
/// 广播面全开）→ 影响 0 行拒绝（`DEFINITION_SET_EMPTY`，提示停用订阅或联系管理员）。
/// 删除未包含的 key 天然幂等（不影响剩余集）。成功后由调用方回读回显剩余集。
pub async fn unsubscribe_definitions(
    db_id: &str,
    tenant: &str,
    name: &str,
    keys: &[String],
) -> Result<(), String> {
    let keys_json = serde_json::to_string(keys).map_err(|e| format!("序列化定义集失败: {e}"))?;
    let sql = "UPDATE cmx_flow_webhook_subscription s \
               SET definition_keys = ( \
                     SELECT COALESCE(jsonb_agg(e ORDER BY ord), '[]'::jsonb) \
                     FROM jsonb_array_elements(s.definition_keys) WITH ORDINALITY AS t(e, ord) \
                     WHERE NOT (e #>> '{}' IN (SELECT g.j FROM jsonb_array_elements_text($1::jsonb) AS g(j)))), \
                   updated_at = now() \
               WHERE s.tenant_id = $2 AND s.name = $3 AND s.active = TRUE \
                 AND EXISTS ( \
                   SELECT 1 FROM jsonb_array_elements(s.definition_keys) e \
                   WHERE NOT (e #>> '{}' IN (SELECT g.j FROM jsonb_array_elements_text($1::jsonb) AS g(j))))";
    let params = SqlParams::DataValues(vec![
        DataValue::Json(keys_json),
        DataValue::String(tenant.to_string()),
        DataValue::String(name.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("定义退订失败: {e}"))?;
    if n > 0 {
        invalidate_cache(tenant);
    }
    Ok(())
}

/// 启停订阅（停用即不再生成新投递行，存量行保留可查可清）。
pub async fn set_subscription_active(
    db_id: &str,
    tenant: &str,
    id: i64,
    active: bool,
) -> Result<(), String> {
    let sql = "UPDATE cmx_flow_webhook_subscription SET active = $1, updated_at = now() \
               WHERE id = $2 AND tenant_id = $3";
    let params = SqlParams::DataValues(vec![
        DataValue::Bool(active),
        DataValue::Int(id),
        DataValue::String(tenant.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("启停订阅失败: {e}"))?;
    if n == 0 {
        return Err(format!("订阅不存在: {id}"));
    }
    invalidate_cache(tenant);
    Ok(())
}

// ———————— 进程内订阅缓存（TTL 5s 版本对账，方案 §7） ————————

struct CacheEntry {
    subs: Arc<Vec<SubRow>>,
    /// 版本指纹：活跃订阅 (COUNT, MAX(updated_at))——DB 时钟赋值，应用时钟漂移免疫。
    version: (i64, Option<String>),
    checked: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static C: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 写操作后失效本副本缓存（跨副本靠 TTL 对账 ≤5s 收敛）。
pub fn invalidate_cache(tenant: &str) {
    cache().lock().expect("订阅缓存锁中毒").remove(tenant);
}

/// 取当前活跃订阅集（匹配用；缓存命中即返，过期先对账版本再决定是否重载）。
pub async fn active_subscriptions_cached(db_id: &str, tenant: &str) -> Arc<Vec<SubRow>> {
    // 1) 快路径：缓存新鲜直接返回（不持锁做 IO）。
    if let Some(entry) = cache().lock().expect("订阅缓存锁中毒").get(tenant)
        && entry.checked.elapsed() < CACHE_TTL
    {
        return entry.subs.clone();
    }
    // 2) 版本对账：COUNT + MAX(updated_at)（都是廉价聚合，订阅量小）。
    let version = subscription_version(db_id, tenant).await;
    {
        let mut map = cache().lock().expect("订阅缓存锁中毒");
        if let Some(entry) = map.get(tenant) {
            if entry.checked.elapsed() < CACHE_TTL {
                // 别的请求刚刷新过。
                return entry.subs.clone();
            }
            if let Some(v) = &version
                && *v == entry.version
            {
                // 版本未变：只续 TTL，不重载。
                let subs = entry.subs.clone();
                if let Some(e) = map.get_mut(tenant) {
                    e.checked = Instant::now();
                }
                return subs;
            }
        }
    }
    // 3) 版本变了（或无缓存）：全量重载活跃订阅。
    let subs = Arc::new(load_active_subscriptions(db_id, tenant).await.unwrap_or_default());
    if let Some(v) = version {
        cache().lock().expect("订阅缓存锁中毒").insert(
            tenant.to_string(),
            CacheEntry { subs: subs.clone(), version: v, checked: Instant::now() },
        );
    }
    subs
}

/// 活跃订阅版本指纹 (COUNT, MAX(updated_at))；表还没建/查询失败 → None（下次再对账）。
async fn subscription_version(db_id: &str, tenant: &str) -> Option<(i64, Option<String>)> {
    let sql = format!(
        "SELECT COUNT(*) AS n, MAX(updated_at) AS mx FROM cmx_flow_webhook_subscription \
         WHERE tenant_id = '{}' AND active = TRUE",
        esc(tenant)
    );
    let ds = query_sql(db_id, None, &sql, "wh_sub_version").await.ok()?;
    let schema = ds.schema.as_ref();
    let row = ds.iter().next()?;
    Some((get_i64(row, schema, "n").unwrap_or(0), get_ts(row, schema, "mx")))
}

async fn load_active_subscriptions(db_id: &str, tenant: &str) -> Result<Vec<SubRow>, String> {
    let sql = format!(
        "SELECT {SUB_COLS} FROM cmx_flow_webhook_subscription \
         WHERE tenant_id = '{}' AND active = TRUE ORDER BY id",
        esc(tenant)
    );
    query_sub_rows(db_id, &sql, "wh_sub_active").await
}

async fn query_sub_rows(db_id: &str, sql: &str, tag: &str) -> Result<Vec<SubRow>, String> {
    let ds = query_sql(db_id, None, sql, tag).await.map_err(|e| format!("查订阅失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds.iter().filter_map(|row| sub_row(row, schema)).collect())
}

// ============================================================
// 首启 env 导入（方案 §7：空表才种、确定性 name、secret 沿用全局密钥、绝不复位用户改动）
// ============================================================

/// 首启导入：订阅表（本租户）为空且 `FLOW_WEBHOOK_TARGETS` 非空 → 按环境变量生成订阅行。
///
/// - name 确定性 `env-{service_key}`：并发首启两副本对同键生成同 name → 撞 uk →
///   `ON CONFLICT DO NOTHING`，幂等成立；
/// - secret 沿用全局 `FLOW_WEBHOOK_SIGNING_KEY`（导入即兼容存量接收端验签、不断签）；
/// - `source='env'` 标记（运维辨识共享密钥的存量导入行，建议尽快改独立 secret）。
///
/// 返回导入条数（幂等重跑返回 0）。
pub async fn import_env_subscriptions(
    db_id: &str,
    tenant: &str,
    targets: &[WebhookTarget],
    signing_key: Option<&str>,
) -> Result<usize, String> {
    if targets.is_empty() {
        return Ok(0);
    }
    let existing = query_one_i64(
        db_id,
        &format!(
            "SELECT COUNT(*) AS n FROM cmx_flow_webhook_subscription WHERE tenant_id = '{}'",
            esc(tenant)
        ),
        "wh_sub_import_count",
    )
    .await?;
    if existing > 0 {
        return Ok(0); // 只在空表时导入一次，绝不复位用户改动（014 教训）。
    }
    let secret = signing_key.unwrap_or("");
    let mut imported = 0usize;
    for t in targets {
        let cfg = json!({
            "service_key": t.key,
            "callback_path": t.path,
            "secret": secret,
        });
        let sql = "INSERT INTO cmx_flow_webhook_subscription \
            (id, name, channel, channel_config, definition_keys, event_types, active, retry_max, source, tenant_id) \
            VALUES ($1, $2, 'webhook', $3::jsonb, '[]'::jsonb, '[]'::jsonb, TRUE, 10, 'env', $4) \
            ON CONFLICT (tenant_id, name) DO NOTHING";
        let params = SqlParams::DataValues(vec![
            DataValue::Int(cmx_utils::next_pk_id()),
            DataValue::String(format!("env-{}", t.key)),
            DataValue::Json(serde_json::to_string(&cfg).unwrap_or_default()),
            DataValue::String(tenant.to_string()),
        ]);
        let n = execute_sql_with_params(db_id, None, sql, params)
            .await
            .map_err(|e| format!("首启导入订阅失败: {e}"))?;
        imported += n as usize;
    }
    if imported > 0 {
        invalidate_cache(tenant);
        tracing::info!(tenant, imported, "已按 FLOW_WEBHOOK_TARGETS 首启导入订阅（source=env，secret=全局密钥）");
    }
    Ok(imported)
}

// ============================================================
// 投递行
// ============================================================

/// 一条待写入的投递行（emit 侧 / test 审计行共用；state/租约列由 SQL 侧赋初值）。
#[derive(Debug, Clone)]
pub struct DeliveryInsert {
    pub subscription_id: i64,
    pub subscription_name: String,
    pub channel: String,
    pub event_id: String,
    pub delivery_id: String,
    /// emit | test
    pub source: &'static str,
    pub event_type: String,
    pub definition_key: Option<String>,
    pub business_key: Option<String>,
    pub instance_id: String,
    pub payload: Value,
    /// 初始状态：emit 行 PENDING（进队列）；test 行直达终态（不参与保序）。
    pub initial_state: &'static str,
    /// 终态直达时（test 行）的诊断三列。
    pub last_error: Option<String>,
    pub last_http_status: Option<i64>,
    pub last_response_snippet: Option<String>,
    /// DONE 时置 delivered_at。
    pub delivered: bool,
    /// 路由成因（v2.4）：bound = 绑定定向投递；matched = 规则匹配（含旁听）。
    /// test 审计行不经此列区分（复用 source='test'），落默认 matched。
    pub route_source: &'static str,
}

/// 单批 INSERT 的行数上限（14 列/行 × 上限，远低于 65535 参数上限）。
const INSERT_CHUNK: usize = 500;

/// 批量落投递行（emit 侧单批写入；uk(subscription_id, event_id) 幂等——重复 emit 被吞）。
/// 返回实际插入行数。
pub async fn insert_deliveries(db_id: &str, rows: &[DeliveryInsert]) -> Result<u64, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    const COLS: &str = "id, subscription_id, subscription_name, channel, event_id, delivery_id, source, \
        event_type, definition_key, business_key, instance_id, payload, state, next_attempt_at, last_error, \
        last_http_status, last_response_snippet, route_source, delivered_at";
    let mut total = 0u64;
    for chunk in rows.chunks(INSERT_CHUNK) {
        let mut values = Vec::with_capacity(chunk.len());
        let mut params: Vec<DataValue> = Vec::with_capacity(chunk.len() * 16);
        for r in chunk {
            // 一行 18 列：占位 17 个 + 两个字面量列（next_attempt_at / delivered_at）——
            // emit 行立即可投（now()）；test 审计行直达终态不进队列（NULL）。
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
            params.push(DataValue::Int(r.subscription_id));
            params.push(DataValue::String(r.subscription_name.clone()));
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
            params.push(DataValue::String(r.route_source.to_string()));
        }
        let sql = format!(
            "INSERT INTO cmx_flow_webhook_delivery ({COLS}) VALUES {} \
             ON CONFLICT (subscription_id, event_id) DO NOTHING",
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
    pub subscription_id: i64,
    pub subscription_name: String,
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

/// 抢占到期可投的行（租约模式 + SKIP LOCKED + 同订阅保序守卫，方案 §4.2）。
///
/// - 可抢占态：`PENDING` 或**租约过期**的 `IN_FLIGHT`（worker 崩溃自愈）；
/// - 保序守卫：同订阅存在更小 seq 的未终态行（PENDING/IN_FLIGHT，含退避等待）即阻塞——
///   终态 DONE/DEAD/SKIPPED 不阻塞（对齐 mdm 守卫真实语义，决议 16；状态集按本表
///   状态机取，**不可照抄 mdm 的小写状态名**）；
/// - claim 即 `attempts+1` 并打租约（locked_by/lock_expires_at）。
pub async fn claim_due_deliveries(
    db_id: &str,
    worker: &str,
    lease_secs: i64,
    limit: usize,
) -> Result<Vec<ClaimedDelivery>, String> {
    // 到期/租约比较全用 **DB 时钟 now()**（与 next_attempt_at / lock_expires_at 的赋值时钟
    // 同源）——应用时钟与 DB 时钟的偏差会让「插完即抢」的行被判成未到期（多副本下同理会
    // 擦枪走火），方案 §7「关键比较用 DB 时钟」原则的落点。
    let sql = format!(
        r#"WITH candidates AS (
        SELECT d.id FROM cmx_flow_webhook_delivery d
        WHERE (d.state = 'PENDING' OR (d.state = 'IN_FLIGHT' AND d.lock_expires_at <= now()))
          AND d.next_attempt_at <= now()
          AND NOT EXISTS (
            SELECT 1 FROM cmx_flow_webhook_delivery b
            WHERE b.subscription_id = d.subscription_id AND b.source = 'emit'
              AND b.seq < d.seq AND b.state IN ('PENDING','IN_FLIGHT'))
        ORDER BY d.seq
        FOR UPDATE SKIP LOCKED LIMIT $1)
        UPDATE cmx_flow_webhook_delivery w
        SET state = 'IN_FLIGHT', locked_by = $2,
            lock_expires_at = now() + interval '{lease_secs} seconds', attempts = attempts + 1
        WHERE id IN (SELECT id FROM candidates)
        RETURNING w.id, w.seq, w.subscription_id, w.subscription_name, w.channel, w.event_id,
                  w.delivery_id, w.event_type, w.definition_key, w.business_key, w.instance_id,
                  w.payload, w.attempts"#
    );
    let params = SqlParams::DataValues(vec![
        DataValue::Int(limit as i64),
        DataValue::String(worker.to_string()),
    ]);
    let ds = query_sql_with_params(db_id, None, &sql, params, "wh_dlv_claim")
        .await
        .map_err(|e| format!("抢占投递行失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        let payload = get_opt(row, schema, "payload").unwrap_or_else(|| "{}".into());
        out.push(ClaimedDelivery {
            id: get_i64(row, schema, "id").unwrap_or(0),
            seq: get_i64(row, schema, "seq").unwrap_or(0),
            subscription_id: get_i64(row, schema, "subscription_id").unwrap_or(0),
            subscription_name: get_str(row, schema, "subscription_name"),
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

/// 批内逐行续租（v2.1 决议 15）：把本 worker 名下全部 IN_FLIGHT 行的租约延长一窗——
/// 每完成一行调一次，使批长度不进入租约算术（租约 120s 只须大于单行 30s 超时）。
pub async fn renew_leases(db_id: &str, worker: &str, lease_secs: i64) -> Result<u64, String> {
    let sql = "UPDATE cmx_flow_webhook_delivery SET lock_expires_at = $1 \
               WHERE locked_by = $2 AND state = 'IN_FLIGHT'";
    let params = SqlParams::DataValues(vec![
        DataValue::DateTime(chrono::Utc::now() + chrono::Duration::seconds(lease_secs)),
        DataValue::String(worker.to_string()),
    ]);
    execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("续租失败: {e}"))
}

/// 成功 → DONE。
pub async fn finish_done(db_id: &str, id: i64, worker: &str) -> Result<bool, String> {
    let sql = "UPDATE cmx_flow_webhook_delivery SET state = 'DONE', delivered_at = now(), \
               lock_expires_at = NULL, next_attempt_at = NULL, last_error = NULL \
               WHERE id = $1 AND locked_by = $2";
    let params = SqlParams::DataValues(vec![
        DataValue::Int(id),
        DataValue::String(worker.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, sql, params)
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
    // 尝试耗尽 → DEAD；否则回 PENDING，退避到期时间用 DB 时钟（now() + interval）。
    let (state, next_clause) = if attempts >= retry_max as i64 {
        ("DEAD", "next_attempt_at = NULL".to_string())
    } else {
        (
            "PENDING",
            format!("next_attempt_at = now() + interval '{} seconds'", backoff.num_seconds().max(1)),
        )
    };
    let sql = format!(
        "UPDATE cmx_flow_webhook_delivery SET state = '{state}', {next_clause}, \
         lock_expires_at = NULL, locked_by = NULL, last_error = $2, last_http_status = $3, \
         last_response_snippet = $4 \
         WHERE id = $1 AND locked_by = $5"
    );
    let params = SqlParams::DataValues(vec![
        DataValue::Int(id),
        DataValue::String(error.to_string()),
        opt_int(http_status),
        opt_str(snippet.map(str::to_string)),
        DataValue::String(worker.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, &sql, params)
        .await
        .map_err(|e| format!("落投递失败态: {e}"))?;
    Ok(n > 0)
}

/// 不可重试失败（其余 4xx / 配置性错误 / 订阅已删）→ 直达 DEAD（不进退避，白耗窗）。
pub async fn finish_dead(
    db_id: &str,
    id: i64,
    worker: &str,
    d: &Diagnostics,
) -> Result<bool, String> {
    let (error, http_status, snippet) = (&d.error, d.http_status, d.snippet.as_deref());
    let sql = "UPDATE cmx_flow_webhook_delivery SET state = 'DEAD', next_attempt_at = NULL, \
               lock_expires_at = NULL, locked_by = NULL, last_error = $2, last_http_status = $3, \
               last_response_snippet = $4 \
               WHERE id = $1 AND locked_by = $5";
    let params = SqlParams::DataValues(vec![
        DataValue::Int(id),
        DataValue::String(error.to_string()),
        opt_int(http_status),
        opt_str(snippet.map(str::to_string)),
        DataValue::String(worker.to_string()),
    ]);
    let n = execute_sql_with_params(db_id, None, sql, params)
        .await
        .map_err(|e| format!("落投递死信态: {e}"))?;
    Ok(n > 0)
}

/// 投递流水分页过滤。
#[derive(Clone)]
pub struct DlvFilter {
    pub subscription_id: Option<i64>,
    pub state: Option<String>,
    pub channel: Option<String>,
    pub definition_key: Option<String>,
    pub page: i64,
    pub page_size: i64,
}

impl Default for DlvFilter {
    fn default() -> Self {
        Self {
            subscription_id: None,
            state: None,
            channel: None,
            definition_key: None,
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

const DLV_COLS: &str = "id, seq, subscription_id, subscription_name, channel, event_id, delivery_id, \
    source, event_type, definition_key, business_key, instance_id, payload, state, attempts, \
    next_attempt_at, locked_by, last_error, last_http_status, last_response_snippet, route_source, created_at, delivered_at";

fn dlv_json(row: &cmx_core::model::data::dataset::Row, schema: &cmx_core::model::data::dataset::Schema) -> Value {
    json!({
        "id": get_i64(row, schema, "id").unwrap_or(0),
        "seq": get_i64(row, schema, "seq").unwrap_or(0),
        "subscriptionId": get_i64(row, schema, "subscription_id").unwrap_or(0),
        "subscriptionName": get_str(row, schema, "subscription_name"),
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
        "routeSource": get_str(row, schema, "route_source"),
        "createdAt": get_ts(row, schema, "created_at"),
        "deliveredAt": get_ts(row, schema, "delivered_at"),
    })
}

/// 投递流水分页（按 seq DESC；租户经 subscription 关联过滤）。
pub async fn query_deliveries(db_id: &str, tenant: &str, f: &DlvFilter) -> Result<(Vec<Value>, i64), String> {
    let (page, size) = f.norm();
    let mut cond = format!("d.subscription_id IN (SELECT id FROM cmx_flow_webhook_subscription WHERE tenant_id = '{}')", esc(tenant));
    if let Some(sid) = f.subscription_id {
        cond.push_str(&format!(" AND d.subscription_id = {sid}"));
    }
    if let Some(st) = f.state.as_deref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND d.state = '{}'", esc(st.trim().to_uppercase().as_str())));
    }
    if let Some(ch) = f.channel.as_deref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND d.channel = '{}'", esc(ch.trim())));
    }
    if let Some(dk) = f.definition_key.as_deref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND d.definition_key = '{}'", esc(dk.trim())));
    }
    let total = query_one_i64(
        db_id,
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_webhook_delivery d WHERE {cond}"),
        "wh_dlv_count",
    )
    .await?;
    let offset = (page - 1) * size;
    let sql = format!(
        "SELECT {DLV_COLS} FROM cmx_flow_webhook_delivery d WHERE {cond} \
         ORDER BY d.seq DESC LIMIT {size} OFFSET {offset}"
    );
    let ds = query_sql(db_id, None, &sql, "wh_dlv_list")
        .await
        .map_err(|e| format!("查投递流水失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let rows: Vec<Value> = ds.iter().map(|row| dlv_json(row, schema)).collect();
    Ok((rows, total))
}

/// 死信重发：DEAD（及**租约已过期**的 IN_FLIGHT——012 缺口补齐）行批量重置 PENDING，
/// 清租约、attempts 归零（人工显式重发 = 重置完整重试预算）。返回重置行数。
pub async fn retry_deliveries(
    db_id: &str,
    tenant: &str,
    ids: &[i64],
    subscription_id: Option<i64>,
    state: Option<&str>,
) -> Result<u64, String> {
    let mut cond = format!(
        "subscription_id IN (SELECT id FROM cmx_flow_webhook_subscription WHERE tenant_id = '{}')",
        esc(tenant)
    );
    if !ids.is_empty() {
        let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        cond.push_str(&format!(" AND id IN ({id_list})"));
    }
    if let Some(sid) = subscription_id {
        cond.push_str(&format!(" AND subscription_id = {sid}"));
    }
    // 允许重置的来源态：DEAD；IN_FLIGHT 仅限租约过期（在投的行不可动——worker 正在投）。
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
        "UPDATE cmx_flow_webhook_delivery SET state = 'PENDING', attempts = 0, \
         next_attempt_at = now(), locked_by = NULL, lock_expires_at = NULL WHERE {cond}"
    );
    execute_sql(db_id, None, &sql)
        .await
        .map_err(|e| format!("重发投递行失败: {e}"))
}

/// 死信处置：DEAD → SKIPPED（人工确认放弃的显式标记；终态本不阻塞保序，此为处置留痕）。
pub async fn skip_deliveries(db_id: &str, tenant: &str, ids: &[i64]) -> Result<u64, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let id_list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE cmx_flow_webhook_delivery SET state = 'SKIPPED', next_attempt_at = NULL, \
         locked_by = NULL, lock_expires_at = NULL \
         WHERE id IN ({id_list}) AND state = 'DEAD' \
         AND subscription_id IN (SELECT id FROM cmx_flow_webhook_subscription WHERE tenant_id = '{}')",
        esc(tenant)
    );
    execute_sql(db_id, None, &sql)
        .await
        .map_err(|e| format!("处置死信失败: {e}"))
}

/// 手动清理 DONE / SKIPPED 行（保留 N 天；M3 起常态由自动清理承担，端点保留）。
pub async fn purge_deliveries(db_id: &str, tenant: &str, before_days: i64, state: Option<&str>) -> Result<u64, String> {
    let states = match state.map(str::trim).filter(|s| !s.is_empty()).map(str::to_uppercase) {
        Some(ref s) if s == "DONE" || s == "SKIPPED" => format!("'{s}'"),
        _ => "'DONE','SKIPPED'".to_string(),
    };
    let days = before_days.clamp(1, 365);
    let sql = format!(
        "DELETE FROM cmx_flow_webhook_delivery WHERE state IN ({states}) \
         AND created_at < now() - interval '{days} days' \
         AND subscription_id IN (SELECT id FROM cmx_flow_webhook_subscription WHERE tenant_id = '{}')",
        esc(tenant)
    );
    execute_sql(db_id, None, &sql)
        .await
        .map_err(|e| format!("清理投递行失败: {e}"))
}

// ============================================================
// 小工具（对齐 biz_link 的按名取值 helpers）
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

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

async fn query_one_i64(db_id: &str, sql: &str, tag: &str) -> Result<i64, String> {
    let ds = query_sql(db_id, None, sql, tag).await.map_err(|e| format!("查询失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .next()
        .and_then(|row| get_i64(row, schema, "n"))
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
