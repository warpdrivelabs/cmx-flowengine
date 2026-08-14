/*
 * @Describe: LocalAssigneeResolver —— 内建身份模块的候选人解析器（对标 PgIamAssigneeResolver）。
 *
 * 实现引擎 `AssigneeResolver`：把 role/position/org 引用及 P0 关系型引用（部门领导/发起人上级/
 * 本人）解析成真实用户 id，数据源是本模块自建的 `fid_*` 表（而非外接 IAM 的 cmx_* 表）。
 *
 * 与 Pg/IAM 版**同契约**：同一 trait、同样的 resolve/resolve_with 语义，故 app 层按
 * FLOW_IDENTITY_MODE 注入哪个实现，引擎完全无感。只读查询，走 cmx-database-pg query_sql。
 */

use async_trait::async_trait;
use cmx_core::model::cell::DataValue;
use cmx_database_pg::query_sql;
use cmx_flow_model::{
    AssigneeResolver, CandidateKind, CandidateRef, ResolveContext, ResolveError, ResolveResult,
};

/// 内建身份候选人解析器。持目标 db_id（存 fid_* 表的库），所有查询走该库。
#[derive(Clone)]
pub struct LocalAssigneeResolver {
    db_id: String,
}

impl LocalAssigneeResolver {
    /// 用指定 db_id 构建（须已注册数据源，且已 ensure_schema 建好 fid_* 表）。
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

    /// 取某组织的领导 user_id（fid_org.leader_user_id）。
    async fn org_leader(&self, org_id: &str) -> ResolveResult<Vec<String>> {
        let v = esc(org_id);
        let sql = format!(
            "SELECT o.leader_user_id AS user_id FROM fid_org o \
             WHERE o.id = '{v}' AND o.leader_user_id IS NOT NULL AND o.leader_user_id <> ''"
        );
        self.query_user_ids(&sql, "fid_resolve_org_leader").await
    }

    /// 取某用户所属组织的领导（用户的部门领导）。
    async fn leader_of_user(&self, user_id: &str) -> ResolveResult<Vec<String>> {
        let v = esc(user_id);
        let sql = format!(
            "SELECT o.leader_user_id AS user_id \
             FROM fid_user u JOIN fid_org o ON o.id = u.org_id \
             WHERE u.id = '{v}' AND o.leader_user_id IS NOT NULL AND o.leader_user_id <> ''"
        );
        self.query_user_ids(&sql, "fid_resolve_user_leader").await
    }
}

/// 单引号转义（引用值来自 BPMN 定义，无强注入面，仍防御）。
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

#[async_trait]
impl AssigneeResolver for LocalAssigneeResolver {
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>> {
        let v = esc(&candidate.value);
        match candidate.kind {
            CandidateKind::User => Ok(vec![candidate.value.clone()]),

            // 角色：fid_role.code → id → fid_user_role 反查（仅未归档）。
            CandidateKind::Role => {
                let sql = format!(
                    "SELECT ur.user_id AS user_id \
                     FROM fid_user_role ur \
                     JOIN fid_role r ON r.id = ur.role_id \
                     WHERE r.code = '{v}' AND r.archived = 0 AND ur.archived = 0"
                );
                self.query_user_ids(&sql, "fid_resolve_role").await
            }

            // 岗位：fid_position.code → id → fid_user_position 反查。
            CandidateKind::Position => {
                let sql = format!(
                    "SELECT up.user_id AS user_id \
                     FROM fid_user_position up \
                     JOIN fid_position p ON p.id = up.position_id \
                     WHERE p.code = '{v}' AND p.archived = 0 AND up.archived = 0"
                );
                self.query_user_ids(&sql, "fid_resolve_position").await
            }

            // 部门：取该部门及其子树（fid_org.path 前缀）下所有用户。
            CandidateKind::Org => {
                let sql = format!(
                    "SELECT u.id AS user_id \
                     FROM fid_user u \
                     WHERE u.archived = 0 AND u.org_id IN ( \
                        SELECT o.id FROM fid_org o \
                        JOIN fid_org root ON root.id = '{v}' \
                        WHERE o.path LIKE root.path || '%' OR o.id = '{v}' \
                     )"
                );
                self.query_user_ids(&sql, "fid_resolve_org").await
            }

            // 关系型：无上下文时 orgLeader 显式给 org 可解析，其余返空（走 resolve_with）。
            CandidateKind::OrgLeader if !candidate.value.is_empty() => {
                self.org_leader(&candidate.value).await
            }
            CandidateKind::OrgLeader
            | CandidateKind::Initiator
            | CandidateKind::InitiatorLeader => Ok(Vec::new()),
        }
    }

    async fn resolve_with(
        &self,
        candidate: &CandidateRef,
        ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        match candidate.kind {
            CandidateKind::OrgLeader => {
                let org = if !candidate.value.is_empty() {
                    Some(candidate.value.clone())
                } else {
                    ctx.org_id.clone()
                };
                match org {
                    Some(o) => self.org_leader(&o).await,
                    None => Ok(Vec::new()),
                }
            }
            CandidateKind::Initiator => Ok(ctx
                .initiator
                .clone()
                .filter(|s| !s.is_empty())
                .into_iter()
                .collect()),
            CandidateKind::InitiatorLeader => match ctx.initiator.as_deref() {
                Some(u) if !u.is_empty() => self.leader_of_user(u).await,
                _ => Ok(Vec::new()),
            },
            _ => self.resolve(candidate).await,
        }
    }
}
