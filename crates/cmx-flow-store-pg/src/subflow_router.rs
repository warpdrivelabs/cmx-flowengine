/*
 * @Describe: PgSubflowRouter —— 子流程组织路由的 PG 实现（M5.2）。
 *
 * 实现 cmx-flow-model::SubflowRouter：给定「逻辑子流程 key + 组织 id」解析出具体子流程定义 key。
 * 数据源 cmx_flow_subflow_binding（called_key + org_id → target_definition_key），三层解析：
 *   1. 精确：本组织 org_id 的启用绑定；
 *   2. 继承：沿 cmx_org.path 向上找最近祖先的绑定（path 最长 = 最近，优先）；
 *   3. 兜底：org_id IS NULL 的默认绑定。
 * 全无 → RouteError::NoBinding。
 *
 * 只读查询走 cmx-database-pg 的 query_sql。引擎经 SubflowRouter trait 依赖它，不直连表——中立。
 */

use async_trait::async_trait;
use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};
use cmx_flow_model::{RouteError, RouteResult, SubflowRouter};

/// 子流程组织路由器。持目标 db_id（绑定表 + cmx_org 所在库），所有查询走该库。
#[derive(Clone)]
pub struct PgSubflowRouter {
    db_id: String,
}

impl PgSubflowRouter {
    /// 用指定 db_id 构建（须已在 cmx-database-pg 注册数据源）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 执行一条只取首行 target_definition_key 的查询；无行 → None。
    async fn query_one_target(&self, sql: &str, tag: &str) -> RouteResult<Option<String>> {
        let ds = query_sql(&self.db_id, None, sql, tag)
            .await
            .map_err(|e| RouteError::Backend(format!("查询子流程绑定失败: {e}")))?;
        let schema = ds.schema.as_ref();
        for row in ds.iter() {
            match row.get_by_name(schema, "target_definition_key") {
                Some(DataValue::String(s)) => return Ok(Some(s.clone())),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => {
                    return Ok(Some(s.to_string()));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

/// 单引号转义（值来自 BPMN 定义 / 实例组织，无强注入面，仍防御）。
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

#[async_trait]
impl SubflowRouter for PgSubflowRouter {
    async fn resolve(&self, called_key: &str, org_id: Option<&str>) -> RouteResult<String> {
        let k = esc(called_key);

        // 有组织：先精确，再沿 path 向上继承。
        if let Some(org) = org_id {
            let o = esc(org);
            // 1) 精确：本组织的启用绑定。
            let exact = format!(
                "SELECT target_definition_key FROM cmx_flow_subflow_binding \
                 WHERE called_key = '{k}' AND org_id = '{o}' AND enabled = TRUE LIMIT 1"
            );
            if let Some(t) = self.query_one_target(&exact, "subflow_exact").await? {
                return Ok(t);
            }
            // 2) 继承：本组织的所有祖先（含自身）里，谁绑了本 key，取 path 最长（最近）。
            //    cmx_org 的 path 为物化路径，祖先的 path 是本组织 path 的前缀。
            let inherited = format!(
                "SELECT b.target_definition_key \
                 FROM cmx_flow_subflow_binding b \
                 JOIN cmx_org anc ON anc.id = b.org_id \
                 JOIN cmx_org self_org ON self_org.id = '{o}' \
                 WHERE b.called_key = '{k}' AND b.enabled = TRUE \
                   AND self_org.path IS NOT NULL AND anc.path IS NOT NULL \
                   AND self_org.path LIKE anc.path || '%' \
                 ORDER BY length(anc.path) DESC LIMIT 1"
            );
            if let Some(t) = self.query_one_target(&inherited, "subflow_inherit").await? {
                return Ok(t);
            }
        }

        // 3) 兜底：org_id IS NULL 的默认绑定。
        let default = format!(
            "SELECT target_definition_key FROM cmx_flow_subflow_binding \
             WHERE called_key = '{k}' AND org_id IS NULL AND enabled = TRUE LIMIT 1"
        );
        if let Some(t) = self.query_one_target(&default, "subflow_default").await? {
            return Ok(t);
        }

        Err(RouteError::NoBinding {
            called_key: called_key.to_string(),
            org: org_id.map(|s| s.to_string()),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 绑定管理（设计态 CRUD）
//
// PgSubflowRouter 只读解析（运行期）；设计器要能配置绑定，故补一个管理面。同库同表
// （cmx_flow_subflow_binding + cmx_org，IAM_DB_ID）。列绑定时 LEFT JOIN cmx_org 带出
// 组织名，前端不必再查一次。
// ─────────────────────────────────────────────────────────────────────────────

/// 一条子流程组织绑定（设计态视图，含组织名便于展示）。
#[derive(Debug, Clone)]
pub struct SubflowBinding {
    /// 绑定行 id。
    pub id: String,
    /// 逻辑子流程 key（= callActivity 的 cmx:calledKey）。
    pub called_key: String,
    /// 组织 id（None = 默认兜底绑定）。
    pub org_id: Option<String>,
    /// 组织名（JOIN cmx_org 得，兜底绑定为 None）。
    pub org_name: Option<String>,
    /// 目标子流程定义 key。
    pub target_definition_key: String,
    /// 是否启用。
    pub enabled: bool,
    /// 备注。
    pub remark: Option<String>,
}

/// 一个组织节点（设计器组织选择器用）。
#[derive(Debug, Clone)]
pub struct OrgNode {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub path: Option<String>,
}

/// 子流程绑定管理器（设计态 CRUD + 组织树读取）。持绑定表/cmx_org 所在库 db_id。
#[derive(Clone)]
pub struct PgSubflowBindingStore {
    db_id: String,
}

impl PgSubflowBindingStore {
    /// 用指定 db_id 构建（须已注册；同 PgSubflowRouter 的库）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。生产库（primary/IAM）不由引擎 ensure_schema 覆盖，故管理面自带 DDL 兜底。
    pub async fn ensure_schema(&self) -> Result<(), String> {
        let ddl = "CREATE TABLE IF NOT EXISTS cmx_flow_subflow_binding (\
            id VARCHAR(64) PRIMARY KEY, called_key VARCHAR(128) NOT NULL, org_id VARCHAR(64), \
            target_definition_key VARCHAR(128) NOT NULL, enabled BOOLEAN NOT NULL DEFAULT TRUE, \
            remark VARCHAR(500), created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now())";
        execute_sql(&self.db_id, None, ddl)
            .await
            .map_err(|e| format!("建绑定表失败: {e}"))?;
        execute_sql(
            &self.db_id,
            None,
            "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_key ON cmx_flow_subflow_binding (called_key)",
        )
        .await
        .map_err(|e| format!("建绑定索引失败: {e}"))?;
        Ok(())
    }

    /// 列某逻辑 key 的全部绑定（带组织名，兜底绑定排最后）。
    pub async fn list_by_key(&self, called_key: &str) -> Result<Vec<SubflowBinding>, String> {
        let sql = format!(
            "SELECT b.id, b.called_key, b.org_id, o.name AS org_name, \
                    b.target_definition_key, b.enabled, b.remark \
             FROM cmx_flow_subflow_binding b \
             LEFT JOIN cmx_org o ON o.id = b.org_id \
             WHERE b.called_key = '{}' \
             ORDER BY (b.org_id IS NULL), o.path NULLS FIRST",
            esc(called_key)
        );
        let ds = query_sql(&self.db_id, None, &sql, "subflow_binding_list")
            .await
            .map_err(|e| format!("查询绑定失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let gs = |row: &cmx_core::model::data::dataset::Row, c: &str| -> Option<String> {
            match row.get_by_name(schema, c) {
                Some(DataValue::String(s)) => Some(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
                _ => None,
            }
        };
        let gb = |row: &cmx_core::model::data::dataset::Row, c: &str| -> bool {
            matches!(row.get_by_name(schema, c), Some(DataValue::Bool(true)))
        };
        Ok(ds
            .iter()
            .map(|row| SubflowBinding {
                id: gs(row, "id").unwrap_or_default(),
                called_key: gs(row, "called_key").unwrap_or_default(),
                org_id: gs(row, "org_id"),
                org_name: gs(row, "org_name"),
                target_definition_key: gs(row, "target_definition_key").unwrap_or_default(),
                enabled: gb(row, "enabled"),
                remark: gs(row, "remark"),
            })
            .collect())
    }

    /// upsert 一条绑定：同 (called_key, org_id) 视为同一绑定（改目标/启用/备注）。
    /// org_id 为 None 表示默认兜底绑定。返回绑定 id。
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert(
        &self,
        id: &str,
        called_key: &str,
        org_id: Option<&str>,
        target_definition_key: &str,
        enabled: bool,
        remark: Option<&str>,
    ) -> Result<(), String> {
        // 先删同 (called_key, org_id) 的旧绑定（org_id NULL 要特判），再插——避免同组织多条。
        let del = match org_id {
            Some(o) => format!(
                "DELETE FROM cmx_flow_subflow_binding WHERE called_key = '{}' AND org_id = '{}'",
                esc(called_key),
                esc(o)
            ),
            None => format!(
                "DELETE FROM cmx_flow_subflow_binding WHERE called_key = '{}' AND org_id IS NULL",
                esc(called_key)
            ),
        };
        execute_sql(&self.db_id, None, &del)
            .await
            .map_err(|e| format!("清理旧绑定失败: {e}"))?;

        let sql = "INSERT INTO cmx_flow_subflow_binding \
            (id, called_key, org_id, target_definition_key, enabled, remark, created_at, updated_at) \
            VALUES ($1, $2, $3, $4, $5, $6, now(), now())";
        let params = SqlParams::DataValues(vec![
            DataValue::String(id.to_string()),
            DataValue::String(called_key.to_string()),
            match org_id {
                Some(o) => DataValue::String(o.to_string()),
                None => DataValue::Null,
            },
            DataValue::String(target_definition_key.to_string()),
            DataValue::Bool(enabled),
            match remark {
                Some(r) => DataValue::String(r.to_string()),
                None => DataValue::Null,
            },
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| format!("写入绑定失败: {e}"))?;
        Ok(())
    }

    /// 删除一条绑定（按 id）。
    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let sql = "DELETE FROM cmx_flow_subflow_binding WHERE id = $1";
        let params = SqlParams::DataValues(vec![DataValue::String(id.to_string())]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| format!("删除绑定失败: {e}"))?;
        Ok(())
    }

    /// 读组织树（全部启用组织，按 path 排序，前端自行建树）。
    pub async fn list_orgs(&self) -> Result<Vec<OrgNode>, String> {
        let sql = "SELECT id, name, parent_id, path FROM cmx_org \
             WHERE archived = 0 ORDER BY path";
        let ds = query_sql(&self.db_id, None, sql, "subflow_org_list")
            .await
            .map_err(|e| format!("查询组织失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let gs = |row: &cmx_core::model::data::dataset::Row, c: &str| -> Option<String> {
            match row.get_by_name(schema, c) {
                Some(DataValue::String(s)) => Some(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
                _ => None,
            }
        };
        Ok(ds
            .iter()
            .map(|row| OrgNode {
                id: gs(row, "id").unwrap_or_default(),
                name: gs(row, "name").unwrap_or_default(),
                parent_id: gs(row, "parent_id"),
                path: gs(row, "path"),
            })
            .collect())
    }
}
