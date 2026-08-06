/*
 * @Describe: RuntimeStore 的 PostgreSQL 实现（接 cmx-database-pg）。
 *
 * 聚合读写 + 事务边界即等待态提交点：
 * - create_snapshot：非事务批量插入（新实例，无并发覆盖风险）。
 * - save_snapshot：单事务内「更新实例 + 删旧令牌任务 + 插新令牌任务」。M1 用「全删重插」
 *   重写子实体——实例的令牌/任务数量极小（顺序审批个位数），DELETE+INSERT 最简单且原子，
 *   避免 diff 逻辑。日后量大可换成 upsert + 差量删。
 * - load_snapshot：三条 SELECT 拼回聚合。
 *
 * tokio-postgres 严格类型（见 cmx-database-pg 记忆）：
 * - 可空 TIMESTAMPTZ 列绑 NULL 必须用 DataValue::NullTyped(Timestamp)，裸 Null 会 WrongType。
 * - DataValue::Int 自适应 INT/BIGINT，DateTime 自适应 TIMESTAMP/TIMESTAMPTZ。
 * - variables 用 jsonb 列，写入用 DataValue::Json(String)，读回也是 Json(String)。
 */

use async_trait::async_trait;
use cmx_core::model::cell::DataValue;
use cmx_database_pg::{
    SqlParams, execute_sql, execute_sql_with_params, get_default_pg_db_manager, query_sql,
    query_sql_with_params,
};
use cmx_flow_model::{
    DueJob, InstanceSnapshot, InstanceSummary, RuntimeStore, StoreError, StoreResult,
};

use crate::mapping;

/// PG 版运行态存储。
///
/// 持有目标 db_id；所有读写走 cmx-database-pg 的全局 manager 与自由函数门面。
#[derive(Clone)]
pub struct PgRuntimeStore {
    db_id: String,
}

impl PgRuntimeStore {
    /// 用指定 db_id 构建。db_id 须已通过 cmx-database-pg 的 manager 注册数据源。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。测试/自举时调用；生产走 docs/sql 迁移。
    pub async fn ensure_schema(&self) -> StoreResult<()> {
        for stmt in crate::ddl::DDL_STATEMENTS {
            execute_sql(&self.db_id, None, stmt)
                .await
                .map_err(|e| StoreError::Backend(format!("建表失败: {e}")))?;
        }
        Ok(())
    }

    /// 批量执行一组 (sql, params)，全部在同一事务内。
    ///
    /// 这是 save_snapshot 的事务骨架：begin → 逐条执行 → commit；任一失败即 rollback。
    async fn exec_in_txn(&self, ops: Vec<(String, SqlParams)>) -> StoreResult<()> {
        let manager = get_default_pg_db_manager();
        // begin / commit / rollback 走 TransactionContext（manager 的事务门面）。
        let txn_ctx = manager.get_transaction_context();
        let txn_id = txn_ctx
            .begin(&self.db_id)
            .await
            .map_err(|e| StoreError::Backend(format!("开启事务失败: {e}")))?;

        // 逐条执行；出错则回滚并返回。
        for (sql, params) in ops {
            if let Err(e) = execute_sql_with_params(&self.db_id, Some(&txn_id), &sql, params).await
            {
                let _ = txn_ctx.rollback(&txn_id).await;
                return Err(StoreError::Backend(format!("事务内执行失败: {e}")));
            }
        }

        txn_ctx
            .commit(&txn_id)
            .await
            .map_err(|e| StoreError::Backend(format!("提交事务失败: {e}")))?;
        Ok(())
    }

    /// 组装「重写某实例全部子实体 + 更新实例」的事务操作序列。
    fn build_save_ops(snapshot: &InstanceSnapshot, is_create: bool) -> Vec<(String, SqlParams)> {
        let mut ops: Vec<(String, SqlParams)> = Vec::new();
        let iid = snapshot.instance.id.clone();

        if is_create {
            ops.push(mapping::insert_instance(&snapshot.instance));
        } else {
            ops.push(mapping::update_instance(&snapshot.instance));
            // 重写子实体：先删旧。
            ops.push((
                "DELETE FROM cmx_flow_token WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_task WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_mi_scope WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_job WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_task_candidate WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_cc WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
            ops.push((
                "DELETE FROM cmx_flow_task_delegation WHERE instance_id = $1".to_string(),
                SqlParams::DataValues(vec![DataValue::String(iid.clone())]),
            ));
        }

        for token in &snapshot.tokens {
            ops.push(mapping::insert_token(token));
        }
        for task in &snapshot.tasks {
            ops.push(mapping::insert_task(task));
        }
        for scope in &snapshot.mi_scopes {
            ops.push(mapping::insert_mi_scope(scope));
        }
        for job in &snapshot.jobs {
            ops.push(mapping::insert_job(job));
        }
        for cand in &snapshot.candidates {
            ops.push(mapping::insert_candidate(cand));
        }
        for cc in &snapshot.cc_records {
            ops.push(mapping::insert_cc(cc));
        }
        for d in &snapshot.delegations {
            ops.push(mapping::insert_delegation(d));
        }

        // 实例进入终态时，同事务归档到历史表（RU/HI 分离）。幂等 upsert，重复保存无害。
        if snapshot.instance.state.is_terminal() {
            let archived_at = chrono::Utc::now();
            ops.push(mapping::upsert_hi_instance(&snapshot.instance, archived_at));
            for task in snapshot.tasks.iter().filter(|t| t.completed) {
                ops.push(mapping::upsert_hi_task(task, archived_at));
            }
        }
        ops
    }
}

#[async_trait]
impl RuntimeStore for PgRuntimeStore {
    async fn create_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        // 新实例首存也走事务：实例 + 令牌 + 任务一起原子落地。
        let ops = Self::build_save_ops(snapshot, true);
        self.exec_in_txn(ops).await
    }

    async fn load_snapshot(&self, instance_id: &str) -> StoreResult<InstanceSnapshot> {
        // 实例。
        let inst_sql = format!(
            "SELECT id, definition_key, business_key, state, variables, created_at, updated_at, ended_at, \
                    org_id, parent_instance_id, parent_token_id, parent_node_bpmn_id \
             FROM cmx_flow_instance WHERE id = '{}'",
            escape(instance_id)
        );
        let inst_ds = query_sql(&self.db_id, None, &inst_sql, "flow_instance")
            .await
            .map_err(|e| StoreError::Backend(format!("查询实例失败: {e}")))?;
        let instance = mapping::row_to_instance(&inst_ds)?
            .ok_or_else(|| StoreError::InstanceNotFound(instance_id.to_string()))?;

        // 令牌。
        let tok_sql = format!(
            "SELECT id, instance_id, node_bpmn_id, state, parent_id, created_at, updated_at \
             FROM cmx_flow_token WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let tok_ds = query_sql(&self.db_id, None, &tok_sql, "flow_token")
            .await
            .map_err(|e| StoreError::Backend(format!("查询令牌失败: {e}")))?;
        let tokens = mapping::rows_to_tokens(&tok_ds)?;

        // 任务。
        let task_sql = format!(
            "SELECT id, instance_id, token_id, node_bpmn_id, name, assignee, candidate_groups, \
             element_value, owner_user_id, parent_task_id, delegation_state, \
             completed, created_at, completed_at \
             FROM cmx_flow_task WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let task_ds = query_sql(&self.db_id, None, &task_sql, "flow_task")
            .await
            .map_err(|e| StoreError::Backend(format!("查询任务失败: {e}")))?;
        let tasks = mapping::rows_to_tasks(&task_ds)?;

        // 多实例执行域。
        let mi_sql = format!(
            "SELECT id, instance_id, node_bpmn_id, sequential, total, completed, next_index, \
             collection, element_var, completion_condition, finished \
             FROM cmx_flow_mi_scope WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let mi_ds = query_sql(&self.db_id, None, &mi_sql, "flow_mi_scope")
            .await
            .map_err(|e| StoreError::Backend(format!("查询多实例域失败: {e}")))?;
        let mi_scopes = mapping::rows_to_mi_scopes(&mi_ds)?;

        // 定时器作业。
        let job_sql = format!(
            "SELECT id, instance_id, token_id, boundary_bpmn_id, cancel_activity, due_at, created_at \
             FROM cmx_flow_job WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let job_ds = query_sql(&self.db_id, None, &job_sql, "flow_job")
            .await
            .map_err(|e| StoreError::Backend(format!("查询定时器作业失败: {e}")))?;
        let jobs = mapping::rows_to_jobs(&job_ds)?;

        // 任务候选人（M4.1）。
        let cand_sql = format!(
            "SELECT id, task_id, instance_id, candidate_type, candidate_ref, resolved_user_id \
             FROM cmx_flow_task_candidate WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let cand_ds = query_sql(&self.db_id, None, &cand_sql, "flow_task_candidate")
            .await
            .map_err(|e| StoreError::Backend(format!("查询任务候选人失败: {e}")))?;
        let candidates = mapping::rows_to_candidates(&cand_ds)?;

        // 抄送记录（M4.2）。
        let cc_sql = format!(
            "SELECT id, instance_id, node_bpmn_id, to_user_id, from_user_id, reason, read_at, created_at \
             FROM cmx_flow_cc WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let cc_ds = query_sql(&self.db_id, None, &cc_sql, "flow_cc")
            .await
            .map_err(|e| StoreError::Backend(format!("查询抄送记录失败: {e}")))?;
        let cc_records = mapping::rows_to_cc(&cc_ds)?;

        // 转签台账（M4.3）。
        let deleg_sql = format!(
            "SELECT id, task_id, instance_id, kind, from_user_id, to_user_id, temp_task_id, reason, created_at \
             FROM cmx_flow_task_delegation WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let deleg_ds = query_sql(&self.db_id, None, &deleg_sql, "flow_task_delegation")
            .await
            .map_err(|e| StoreError::Backend(format!("查询转签台账失败: {e}")))?;
        let delegations = mapping::rows_to_delegations(&deleg_ds)?;

        Ok(InstanceSnapshot {
            instance,
            tokens,
            tasks,
            mi_scopes,
            jobs,
            candidates,
            cc_records,
            delegations,
        })
    }

    async fn save_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        let ops = Self::build_save_ops(snapshot, false);
        self.exec_in_txn(ops).await
    }

    async fn list_instances(&self, limit: usize) -> StoreResult<Vec<InstanceSummary>> {
        // 用相关子查询算未办结任务数，避免 N+1 载入完整聚合。按创建时间倒序。
        let sql = format!(
            "SELECT i.id, i.definition_key, i.business_key, i.state, i.variables, \
                    i.created_at, i.updated_at, \
                    (SELECT count(*) FROM cmx_flow_task t \
                     WHERE t.instance_id = i.id AND t.completed = FALSE) AS open_task_count \
             FROM cmx_flow_instance i \
             ORDER BY i.created_at DESC \
             LIMIT {}",
            limit.min(1000) // 硬上限，防御超大 limit
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_instance_list")
            .await
            .map_err(|e| StoreError::Backend(format!("查询实例列表失败: {e}")))?;
        mapping::rows_to_summaries(&ds)
    }

    async fn find_due_jobs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>> {
        // 跨实例查到期作业，按到期时刻升序（先到期先处理）。参数化绑定 now。
        let sql = format!(
            "SELECT j.id, j.instance_id, j.due_at \
             FROM cmx_flow_job j \
             JOIN cmx_flow_instance i ON i.id = j.instance_id \
             WHERE j.due_at <= $1 AND i.state = 'ACTIVE' \
             ORDER BY j.due_at ASC \
             LIMIT {}",
            limit.min(1000)
        );
        let params = SqlParams::DataValues(vec![DataValue::DateTime(now)]);
        let ds = query_sql_with_params(&self.db_id, None, &sql, params, "flow_due_jobs")
            .await
            .map_err(|e| StoreError::Backend(format!("查询到期作业失败: {e}")))?;
        mapping::rows_to_due_jobs(&ds)
    }

    async fn find_cc_for_user(
        &self,
        user_id: &str,
        unread_only: bool,
        limit: usize,
    ) -> StoreResult<Vec<cmx_flow_model::CcSummary>> {
        // JOIN 实例取 def_key/biz_key。参数化绑 user_id。
        let unread_clause = if unread_only {
            " AND c.read_at IS NULL"
        } else {
            ""
        };
        let sql = format!(
            "SELECT c.id, c.instance_id, c.node_bpmn_id, c.reason, c.read_at, c.created_at, \
                    i.business_key, i.definition_key \
             FROM cmx_flow_cc c \
             JOIN cmx_flow_instance i ON i.id = c.instance_id \
             WHERE c.to_user_id = $1{unread_clause} \
             ORDER BY c.created_at DESC \
             LIMIT {}",
            limit.min(1000)
        );
        let params = SqlParams::DataValues(vec![DataValue::String(user_id.to_string())]);
        let ds = query_sql_with_params(&self.db_id, None, &sql, params, "flow_cc_for_user")
            .await
            .map_err(|e| StoreError::Backend(format!("查询抄送失败: {e}")))?;
        mapping::rows_to_cc_summaries(&ds)
    }

    async fn mark_cc_read(
        &self,
        cc_id: &str,
        read_at: chrono::DateTime<chrono::Utc>,
    ) -> StoreResult<bool> {
        // 仅当未读时置 read_at（幂等）。参数化绑定。
        let sql = "UPDATE cmx_flow_cc SET read_at = $1 WHERE id = $2 AND read_at IS NULL";
        let params = SqlParams::DataValues(vec![
            DataValue::DateTime(read_at),
            DataValue::String(cc_id.to_string()),
        ]);
        let affected = execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("标记抄送已读失败: {e}")))?;
        // affected=0 可能是已读或不存在——都视为「命中/无害」，返回是否存在该记录不强求。
        Ok(affected > 0)
    }

    async fn find_child_instances(
        &self,
        parent_instance_id: &str,
    ) -> StoreResult<Vec<cmx_flow_model::ProcessInstance>> {
        let sql = format!(
            "SELECT id, definition_key, business_key, state, variables, created_at, updated_at, ended_at, \
                    org_id, parent_instance_id, parent_token_id, parent_node_bpmn_id \
             FROM cmx_flow_instance WHERE parent_instance_id = '{}'",
            escape(parent_instance_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_child_instances")
            .await
            .map_err(|e| StoreError::Backend(format!("查询子实例失败: {e}")))?;
        mapping::rows_to_instances(&ds)
    }
}

/// 单引号转义：load 路径的 id 用字面量拼接（id 是引擎生成的 UUID，无注入面），
/// 仍做转义以防御。写路径一律走参数化，不用此函数。
fn escape(s: &str) -> String {
    s.replace('\'', "''")
}
