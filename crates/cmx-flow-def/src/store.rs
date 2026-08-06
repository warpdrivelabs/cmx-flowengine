/*
 * @Describe: PgDefinitionStore —— 流程定义持久化的 PG 实现（接 cmx-database-pg）。
 *
 * 实现 cmx-flow-def::DefinitionStore。两张表：
 *   cmx_flow_definition          —— 定义主记录（当前指针：draft_xml + active_version + state）
 *   cmx_flow_definition_version  —— 版本历史（不可变，每次 publish 追加一行 bpmn_xml 快照）
 * 约束遵循：cmx_ 前缀；无外键（def_key 关联 + 索引替代）；DDL 幂等。
 *
 * 写路径一律参数化（bpmn_xml 可能含单引号，绝不拼字面量）；读路径 id/key 用字面量拼接时
 * 走 esc 转义防御。时间戳可空列绑 NULL 用 DataValue::NullTyped(Timestamp)（tokio-postgres 严格类型）。
 */

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cmx_core::model::cell::{DataValue, SqlTypeMarker};
use cmx_core::model::data::dataset::{Row, Schema};
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};

use crate::{
    DefError, DefResult, DefinitionRecord, DefinitionState, DefinitionStore, PublishedEntry,
    VersionMeta, VersionRecord,
};

/// 建表 DDL（幂等）。测试/自举时调用；生产走 docs/sql 迁移。
pub const DDL_STATEMENTS: &[&str] = &[
    // —— 定义主表（当前指针） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_definition (
        key            VARCHAR(128) PRIMARY KEY,
        name           VARCHAR(255) NOT NULL,
        domain         VARCHAR(64),
        application    VARCHAR(64),
        module         VARCHAR(64),
        category       VARCHAR(64),
        state          VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
        active_version INTEGER,
        draft_xml      TEXT,
        updated_at     TIMESTAMPTZ  NOT NULL,
        updated_by     VARCHAR(64)
    )"#,
    // 已有表补 DAM 列（幂等；CREATE TABLE IF NOT EXISTS 不改旧表结构）。
    "ALTER TABLE cmx_flow_definition ADD COLUMN IF NOT EXISTS domain VARCHAR(64)",
    "ALTER TABLE cmx_flow_definition ADD COLUMN IF NOT EXISTS application VARCHAR(64)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_definition_module ON cmx_flow_definition (module)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_definition_dam ON cmx_flow_definition (domain, application, module)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_definition_state ON cmx_flow_definition (state)",
    // —— 版本表（不可变历史） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_definition_version (
        id            VARCHAR(64)  PRIMARY KEY,
        def_key       VARCHAR(128) NOT NULL,
        version       INTEGER      NOT NULL,
        bpmn_xml      TEXT         NOT NULL,
        note          VARCHAR(512),
        published_at  TIMESTAMPTZ  NOT NULL,
        published_by  VARCHAR(64)
    )"#,
    // 已有表补 note 列（幂等）。
    "ALTER TABLE cmx_flow_definition_version ADD COLUMN IF NOT EXISTS note VARCHAR(512)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_def_version ON cmx_flow_definition_version (def_key, version)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_def_version_key ON cmx_flow_definition_version (def_key)",
];

/// PG 版定义存储。持目标 db_id，所有读写走该库。
#[derive(Clone)]
pub struct PgDefinitionStore {
    db_id: String,
}

impl PgDefinitionStore {
    /// 用指定 db_id 构建（须已在 cmx-database-pg 注册数据源）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }
}

#[async_trait]
impl DefinitionStore for PgDefinitionStore {
    async fn ensure_schema(&self) -> DefResult<()> {
        for stmt in DDL_STATEMENTS {
            execute_sql(&self.db_id, None, stmt)
                .await
                .map_err(|e| DefError::Backend(format!("建表失败: {e}")))?;
        }
        Ok(())
    }

    async fn upsert_draft(&self, rec: &DefinitionRecord) -> DefResult<()> {
        // upsert：存在则更新草稿列，不存在则插入 DRAFT 行。参数化（draft_xml 含任意字符）。
        let sql = "INSERT INTO cmx_flow_definition \
            (key, name, domain, application, module, category, state, active_version, draft_xml, updated_at, updated_by) \
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
            ON CONFLICT (key) DO UPDATE SET \
              name = EXCLUDED.name, domain = EXCLUDED.domain, application = EXCLUDED.application, \
              module = EXCLUDED.module, category = EXCLUDED.category, \
              draft_xml = EXCLUDED.draft_xml, updated_at = EXCLUDED.updated_at, \
              updated_by = EXCLUDED.updated_by";
        let params = SqlParams::DataValues(vec![
            DataValue::String(rec.key.clone()),
            DataValue::String(rec.name.clone()),
            opt_text(&rec.domain),
            opt_text(&rec.application),
            opt_text(&rec.module),
            opt_text(&rec.category),
            DataValue::String(rec.state.as_str().to_string()),
            opt_int(rec.active_version),
            opt_text(&rec.draft_xml),
            DataValue::DateTime(rec.updated_at),
            opt_text(&rec.updated_by),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| DefError::Backend(format!("保存定义草稿失败: {e}")))?;
        Ok(())
    }

    async fn get(&self, key: &str) -> DefResult<Option<DefinitionRecord>> {
        let sql = format!(
            "SELECT key, name, domain, application, module, category, state, active_version, draft_xml, updated_at, updated_by \
             FROM cmx_flow_definition WHERE key = '{}'",
            esc(key)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_def_get")
            .await
            .map_err(|e| DefError::Backend(format!("查询定义失败: {e}")))?;
        let schema = ds.schema.as_ref();
        match ds.iter().next() {
            Some(row) => Ok(Some(row_to_record(row, schema, true)?)),
            None => Ok(None),
        }
    }

    async fn list(&self) -> DefResult<Vec<DefinitionRecord>> {
        // 列表不带 draft_xml，省流量。
        let sql = "SELECT key, name, domain, application, module, category, state, active_version, updated_at, updated_by \
             FROM cmx_flow_definition ORDER BY updated_at DESC";
        let ds = query_sql(&self.db_id, None, sql, "flow_def_list")
            .await
            .map_err(|e| DefError::Backend(format!("查询定义列表失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            out.push(row_to_record(row, schema, false)?);
        }
        Ok(out)
    }

    async fn insert_version(&self, ver: &VersionRecord) -> DefResult<()> {
        let sql = "INSERT INTO cmx_flow_definition_version \
            (id, def_key, version, bpmn_xml, note, published_at, published_by) \
            VALUES ($1, $2, $3, $4, $5, $6, $7)";
        let params = SqlParams::DataValues(vec![
            DataValue::String(ver.id.clone()),
            DataValue::String(ver.def_key.clone()),
            DataValue::Int(ver.version as i64),
            DataValue::String(ver.bpmn_xml.clone()),
            opt_text(&ver.note),
            DataValue::DateTime(ver.published_at),
            opt_text(&ver.published_by),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| DefError::Backend(format!("写入定义版本失败: {e}")))?;
        Ok(())
    }

    async fn max_version(&self, def_key: &str) -> DefResult<i32> {
        let sql = format!(
            "SELECT COALESCE(MAX(version), 0) AS v FROM cmx_flow_definition_version WHERE def_key = '{}'",
            esc(def_key)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_def_maxver")
            .await
            .map_err(|e| DefError::Backend(format!("查询最大版本失败: {e}")))?;
        let schema = ds.schema.as_ref();
        match ds.iter().next() {
            Some(row) => Ok(get_i64(row, schema, "v").max(0) as i32),
            None => Ok(0),
        }
    }

    async fn get_version(&self, def_key: &str, version: i32) -> DefResult<Option<VersionRecord>> {
        let sql = format!(
            "SELECT id, def_key, version, bpmn_xml, note, published_at, published_by \
             FROM cmx_flow_definition_version WHERE def_key = '{}' AND version = {}",
            esc(def_key),
            version
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_def_getver")
            .await
            .map_err(|e| DefError::Backend(format!("查询定义版本失败: {e}")))?;
        let schema = ds.schema.as_ref();
        match ds.iter().next() {
            Some(row) => Ok(Some(row_to_version(row, schema)?)),
            None => Ok(None),
        }
    }

    async fn list_versions(&self, def_key: &str) -> DefResult<Vec<VersionMeta>> {
        let sql = format!(
            "SELECT def_key, version, note, published_at, published_by \
             FROM cmx_flow_definition_version WHERE def_key = '{}' ORDER BY version DESC",
            esc(def_key)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_def_listver")
            .await
            .map_err(|e| DefError::Backend(format!("查询版本列表失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            out.push(row_to_version_meta(row, schema)?);
        }
        Ok(out)
    }

    async fn list_all_versions(&self) -> DefResult<Vec<VersionMeta>> {
        let sql = "SELECT def_key, version, note, published_at, published_by \
             FROM cmx_flow_definition_version ORDER BY def_key, version DESC";
        let ds = query_sql(&self.db_id, None, sql, "flow_def_listallver")
            .await
            .map_err(|e| DefError::Backend(format!("查询全部版本失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            out.push(row_to_version_meta(row, schema)?);
        }
        Ok(out)
    }

    async fn delete_version(&self, def_key: &str, version: i32) -> DefResult<()> {
        let sql = "DELETE FROM cmx_flow_definition_version WHERE def_key = $1 AND version = $2";
        let params = SqlParams::DataValues(vec![
            DataValue::String(def_key.to_string()),
            DataValue::Int(version as i64),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| DefError::Backend(format!("删除定义版本失败: {e}")))?;
        Ok(())
    }

    async fn mark_published(&self, key: &str, version: i32) -> DefResult<()> {
        let sql = "UPDATE cmx_flow_definition SET state = 'PUBLISHED', active_version = $1 WHERE key = $2";
        let params = SqlParams::DataValues(vec![
            DataValue::Int(version as i64),
            DataValue::String(key.to_string()),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| DefError::Backend(format!("标记发布失败: {e}")))?;
        Ok(())
    }

    async fn load_published(&self) -> DefResult<Vec<PublishedEntry>> {
        // 取每个已发布定义的 active_version 对应的 XML。JOIN 版本表按 (key, active_version)。
        let sql = "SELECT d.key AS key, d.active_version AS version, v.bpmn_xml AS bpmn_xml \
             FROM cmx_flow_definition d \
             JOIN cmx_flow_definition_version v \
               ON v.def_key = d.key AND v.version = d.active_version \
             WHERE d.state = 'PUBLISHED' AND d.active_version IS NOT NULL";
        let ds = query_sql(&self.db_id, None, sql, "flow_def_published")
            .await
            .map_err(|e| DefError::Backend(format!("查询已发布定义失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            out.push(PublishedEntry {
                key: get_string(row, schema, "key")?,
                version: get_i64(row, schema, "version").max(0) as i32,
                bpmn_xml: get_string(row, schema, "bpmn_xml")?,
            });
        }
        Ok(out)
    }
}

// ————————————————————— 行映射 —————————————————————

fn row_to_record(row: &Row, schema: &Schema, with_xml: bool) -> DefResult<DefinitionRecord> {
    Ok(DefinitionRecord {
        key: get_string(row, schema, "key")?,
        name: get_string(row, schema, "name")?,
        domain: get_opt_string(row, schema, "domain"),
        application: get_opt_string(row, schema, "application"),
        module: get_opt_string(row, schema, "module"),
        category: get_opt_string(row, schema, "category"),
        state: DefinitionState::parse_str(&get_string(row, schema, "state")?),
        active_version: get_opt_i32(row, schema, "active_version"),
        draft_xml: if with_xml {
            get_opt_string(row, schema, "draft_xml")
        } else {
            None
        },
        updated_at: get_ts(row, schema, "updated_at")?,
        updated_by: get_opt_string(row, schema, "updated_by"),
    })
}

fn row_to_version(row: &Row, schema: &Schema) -> DefResult<VersionRecord> {
    Ok(VersionRecord {
        id: get_string(row, schema, "id")?,
        def_key: get_string(row, schema, "def_key")?,
        version: get_i64(row, schema, "version").max(0) as i32,
        bpmn_xml: get_string(row, schema, "bpmn_xml")?,
        note: get_opt_string(row, schema, "note"),
        published_at: get_ts(row, schema, "published_at")?,
        published_by: get_opt_string(row, schema, "published_by"),
    })
}

fn row_to_version_meta(row: &Row, schema: &Schema) -> DefResult<VersionMeta> {
    Ok(VersionMeta {
        def_key: get_string(row, schema, "def_key")?,
        version: get_i64(row, schema, "version").max(0) as i32,
        note: get_opt_string(row, schema, "note"),
        published_at: get_ts(row, schema, "published_at")?,
        published_by: get_opt_string(row, schema, "published_by"),
    })
}

// ————————————————————— 取值/写值 helper —————————————————————

fn opt_text(v: &Option<String>) -> DataValue {
    match v {
        Some(s) => DataValue::String(s.clone()),
        None => DataValue::Null,
    }
}

fn opt_int(v: Option<i32>) -> DataValue {
    match v {
        Some(n) => DataValue::Int(n as i64),
        None => DataValue::NullTyped(SqlTypeMarker::Int),
    }
}

fn get_string(row: &Row, schema: &Schema, col: &str) -> DefResult<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Ok(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Ok(s.to_string()),
        other => Err(DefError::Backend(format!(
            "列 {col} 期望文本，实际 {other:?}"
        ))),
    }
}

fn get_opt_string(row: &Row, schema: &Schema, col: &str) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn get_ts(row: &Row, schema: &Schema, col: &str) -> DefResult<DateTime<Utc>> {
    match row.get_by_name(schema, col) {
        Some(DataValue::DateTime(dt)) => Ok(*dt),
        other => Err(DefError::Backend(format!(
            "列 {col} 期望时间戳，实际 {other:?}"
        ))),
    }
}

fn get_i64(row: &Row, schema: &Schema, col: &str) -> i64 {
    match row.get_by_name(schema, col) {
        Some(DataValue::Int(v)) => *v,
        _ => 0,
    }
}

fn get_opt_i32(row: &Row, schema: &Schema, col: &str) -> Option<i32> {
    match row.get_by_name(schema, col) {
        Some(DataValue::Int(v)) => Some(*v as i32),
        _ => None,
    }
}

/// 单引号转义（key/def_key 来自流程定义标识，无强注入面，仍防御）。
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}
