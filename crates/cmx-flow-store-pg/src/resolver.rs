/*
 * @Describe: PgIamAssigneeResolver —— 候选人解析器的 PG/IAM 实现（M4.1）。
 *
 * 实现 cmx-flow-model::AssigneeResolver：把 role/position/org 引用解析成真实用户 id。
 * 复用现有 IAM 表（cmx_user_role / cmx_role）+ M4 新建表（cmx_user_position / cmx_org）：
 *   - User(id)       → 该 id 本身（校验存在性可选，这里直接返回）
 *   - Role(code)     → cmx_role 按 code 找 role_id → cmx_user_role 反查启用用户
 *   - Position(code) → cmx_position 按 code 找 id → cmx_user_position 反查用户
 *   - Org(id)        → cmx_user.org_id 命中该部门（含子树，用 cmx_org.path 前缀匹配）的用户
 *
 * 只读查询，走 cmx-database-pg 的 query_sql。引擎通过 AssigneeResolver trait 依赖它，不直接
 * 依赖 IAM crate——保持引擎中立。
 */

use async_trait::async_trait;
use cmx_core::model::cell::DataValue;
use cmx_database_pg::query_sql;
use cmx_flow_model::{AssigneeResolver, CandidateKind, CandidateRef, ResolveError, ResolveResult};

/// PG/IAM 候选人解析器。持目标 db_id，所有查询走该库。
#[derive(Clone)]
pub struct PgIamAssigneeResolver {
    db_id: String,
}

impl PgIamAssigneeResolver {
    /// 用指定 db_id 构建（须已在 cmx-database-pg 注册数据源）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 执行一条只返回单列 user_id 的查询，收集成 Vec<String>。
    async fn query_user_ids(&self, sql: &str, tag: &str) -> ResolveResult<Vec<String>> {
        let ds = query_sql(&self.db_id, None, sql, tag)
            .await
            .map_err(|e| ResolveError::Backend(format!("查询用户失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            if let Some(DataValue::String(s)) = row.get_by_name(schema, "user_id") {
                out.push(s.clone());
            } else if let Some(DataValue::ShortStr(s)) = row.get_by_name(schema, "user_id") {
                out.push(s.to_string());
            }
        }
        Ok(out)
    }
}

/// 单引号转义（引用值来自 BPMN 定义，无强注入面，仍防御）。
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

#[async_trait]
impl AssigneeResolver for PgIamAssigneeResolver {
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>> {
        let v = esc(&candidate.value);
        match candidate.kind {
            // 指定用户：就是该 id 本身。
            CandidateKind::User => Ok(vec![candidate.value.clone()]),

            // 角色：cmx_role.code → role_id → cmx_user_role 反查（仅未归档）。
            CandidateKind::Role => {
                let sql = format!(
                    "SELECT ur.user_id AS user_id \
                     FROM cmx_user_role ur \
                     JOIN cmx_role r ON r.id = ur.role_id \
                     WHERE r.code = '{v}' AND r.archived = 0 AND ur.archived = 0"
                );
                self.query_user_ids(&sql, "resolve_role").await
            }

            // 岗位：cmx_position.code → id → cmx_user_position 反查。
            CandidateKind::Position => {
                let sql = format!(
                    "SELECT up.user_id AS user_id \
                     FROM cmx_user_position up \
                     JOIN cmx_position p ON p.id = up.position_id \
                     WHERE p.code = '{v}' AND p.archived = 0 AND up.archived = 0"
                );
                self.query_user_ids(&sql, "resolve_position").await
            }

            // 部门：取该部门及其子树（cmx_org.path 前缀）下所有用户。
            CandidateKind::Org => {
                let sql = format!(
                    "SELECT u.id AS user_id \
                     FROM cmx_user u \
                     WHERE u.archived = 0 AND u.org_id IN ( \
                        SELECT o.id FROM cmx_org o \
                        JOIN cmx_org root ON root.id = '{v}' \
                        WHERE o.path LIKE root.path || '%' OR o.id = '{v}' \
                     )"
                );
                self.query_user_ids(&sql, "resolve_org").await
            }
        }
    }
}
