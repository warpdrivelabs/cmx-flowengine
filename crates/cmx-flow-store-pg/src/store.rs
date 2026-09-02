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
    ActivityRecord, AsyncJob, DeadLetterJob, DueJob, InstanceSnapshot, InstanceSummary,
    RuntimeStore, StoreError, StoreResult, VarChangeRecord,
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
                .map_err(|e| StoreError::Backend(format!("建表失败:{stmt}, {e}")))?;
        }
        Ok(())
    }

    /// save_snapshot 的事务骨架（技术债 007 乐观锁版）。
    ///
    /// begin → **先执行实例 CAS UPDATE 并检查影响行数**（0 行 = 该实例已被并发推进段保存，
    /// 整体回滚返回 [`StoreError::Conflict`]，子表重写不发生）→ 命中则逐条执行子表重写 +
    /// hi 归档 → commit。保证「版本比对 → 全量覆盖」的原子性：冲突者连子表也碰不到。
    async fn exec_save_ops(
        &self,
        instance_id: &str,
        expected_version: i64,
        cas_ops: Vec<(String, SqlParams)>,
        ops: Vec<(String, SqlParams)>,
    ) -> StoreResult<()> {
        let manager = get_default_pg_db_manager();
        let txn_ctx = manager.get_transaction_context();
        let txn_id = txn_ctx
            .begin(&self.db_id)
            .await
            .map_err(|e| StoreError::Backend(format!("开启事务失败: {e}")))?;

        for (sql, params) in cas_ops {
            // 错误分支须真正 await 回滚（map_err 同步闭包里丢弃 future 会让事务悬挂——审查 C-02）。
            let affected = match execute_sql_with_params(&self.db_id, Some(&txn_id), &sql, params)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    let _ = txn_ctx.rollback(&txn_id).await;
                    return Err(StoreError::Backend(format!("事务内执行失败: {e}")));
                }
            };
            if affected == 0 {
                let _ = txn_ctx.rollback(&txn_id).await;
                return Err(StoreError::Conflict(format!(
                    "实例 {instance_id} 已被并发修改（保存期望 version={expected_version}）；\
                     该实例已被另一操作推进，请刷新后重试"
                )));
            }
        }

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

    /// 组装「重写某实例全部子实体 + 更新实例」的事务操作序列（技术债 007 改造后）。
    ///
    /// 返回 (cas_ops, rest_ops)：`cas_ops` 是实例乐观锁 UPDATE（必须最先执行并检查影响行数），
    /// `rest_ops` 是子表重写 + hi 归档（仅 CAS 命中后执行）。
    ///
    /// 旁路剥离（007 先行子项）：
    /// - **cc 已读**：本表不再 DELETE——cc 只增不减，改 `ON CONFLICT DO UPDATE` 且更新列
    ///   刻意不含 read_at，并发 `mark_cc_read`（旁路小写）不被旧快照重存抹回未读；
    /// - **转签台账**：本表不再 DELETE——append-only 审计实体，`ON CONFLICT DO NOTHING`
    ///   幂等重放，并发推进段新记的台账不被旧快照全量覆盖丢失。
    fn build_save_ops(
        snapshot: &InstanceSnapshot,
        is_create: bool,
    ) -> (Vec<(String, SqlParams)>, Vec<(String, SqlParams)>) {
        let mut cas_ops: Vec<(String, SqlParams)> = Vec::new();
        let mut ops: Vec<(String, SqlParams)> = Vec::new();
        let iid = snapshot.instance.id.clone();

        if is_create {
            ops.push(mapping::insert_instance(&snapshot.instance));
        } else {
            cas_ops.push(mapping::update_instance_cas(
                &snapshot.instance,
                snapshot.version,
            ));
            // 重写子实体：先删旧（cc/task_delegation 已剥离，见函数头注释）。
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
            let archived_version = if is_create {
                0
            } else {
                snapshot.version + 1 // CAS 命中后 version 已 +1，归档行与运行态行一致
            };
            ops.push(mapping::upsert_hi_instance(
                &snapshot.instance,
                archived_at,
                archived_version,
            ));
            for task in snapshot.tasks.iter().filter(|t| t.completed) {
                ops.push(mapping::upsert_hi_task(task, archived_at));
            }
        }
        (cas_ops, ops)
    }
}

#[async_trait]
impl RuntimeStore for PgRuntimeStore {
    async fn create_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        // 新实例首存也走事务：实例 + 令牌 + 任务一起原子落地。新行无并发覆盖面，无 CAS。
        let (_, ops) = Self::build_save_ops(snapshot, true);
        self.exec_save_ops(&snapshot.instance.id, 0, Vec::new(), ops)
            .await
    }

    async fn load_snapshot(&self, instance_id: &str) -> StoreResult<InstanceSnapshot> {
        // 实例（含乐观锁 version，save 时 CAS 比对；system_id 为 005 归属列）。
        let inst_sql = format!(
            "SELECT id, definition_key, business_key, state, variables, created_at, updated_at, ended_at, \
                    org_id, dimensions, parent_instance_id, parent_token_id, parent_node_bpmn_id, subscriber_id, version, system_id \
             FROM cmx_flow_instance WHERE id = '{}'",
            escape(instance_id)
        );
        let inst_ds = query_sql(&self.db_id, None, &inst_sql, "flow_instance")
            .await
            .map_err(|e| StoreError::Backend(format!("查询实例失败: {e}")))?;
        let instance = mapping::row_to_instance(&inst_ds)?
            .ok_or_else(|| StoreError::InstanceNotFound(instance_id.to_string()))?;
        // version 与快照同行读出：CAS 期望值（列 NOT NULL DEFAULT 0，缺失回退 0）。
        let version = inst_ds
            .iter()
            .next()
            .map(|row| mapping::get_i64(row, inst_ds.schema.as_ref(), "version"))
            .unwrap_or(0);

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

        // 定时器作业（含 A1/A5 的 kind + 周期列）。
        let job_sql = format!(
            "SELECT id, instance_id, token_id, boundary_bpmn_id, cancel_activity, due_at, created_at, \
             kind, cycle_interval_seconds, cycle_remaining \
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

        // 异步作业（P1）：侧表，随实例一起载回快照。锁列（locked_by/lock_expires_at）
        // 只由 acquire/fail 改，save_snapshot 不重写本表、flush 用 ON CONFLICT DO NOTHING，
        // 故载回后即便再存也不会抹掉 worker 的锁。
        let async_sql = format!(
            "SELECT id, instance_id, token_id, node_bpmn_id, delegate_key, topic, max_retries, \
                    retries, retry_backoff_seconds, locked_by, lock_expires_at, created_at \
             FROM cmx_flow_async_job WHERE instance_id = '{}'",
            escape(instance_id)
        );
        let async_ds = query_sql(&self.db_id, None, &async_sql, "flow_async_job")
            .await
            .map_err(|e| StoreError::Backend(format!("查询异步作业失败: {e}")))?;
        let async_jobs = mapping::rows_to_async_jobs(&async_ds)?;

        Ok(InstanceSnapshot {
            instance,
            tokens,
            tasks,
            mi_scopes,
            jobs,
            async_jobs,
            candidates,
            cc_records,
            delegations,
            // 乐观锁期望版本（007）：save_snapshot 以此 CAS 提交。
            version,
            pending_subs: Vec::new(),
            pending_activities: Vec::new(),
            pending_var_changes: Vec::new(),
        })
    }

    async fn save_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        let (cas_ops, ops) = Self::build_save_ops(snapshot, false);
        self.exec_save_ops(
            &snapshot.instance.id,
            snapshot.version,
            cas_ops,
            ops,
        )
        .await
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

    async fn acquire_due_jobs(
        &self,
        worker_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        lease_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>> {
        // 技术债 008：SKIP LOCKED 集群安全抢占（对齐 acquire_async_jobs）。内层 SELECT
        // 只锁本 worker 能拿到的行，外层 UPDATE 打租约并 RETURNING——多副本下同一
        // 定时器作业不会被两个副本同时取到 fire（单副本行为不变：无竞争，照常领取）。
        let lease_expires = now + chrono::Duration::seconds(lease_secs);
        let sql = format!(
            "UPDATE cmx_flow_job SET claimed_by = $1, lease_expires_at = $2 \
             WHERE id IN ( \
                SELECT j.id FROM cmx_flow_job j \
                JOIN cmx_flow_instance i ON i.id = j.instance_id \
                WHERE j.due_at <= $3 AND i.state = 'ACTIVE' \
                  AND (j.claimed_by IS NULL OR j.lease_expires_at <= $3) \
                ORDER BY j.due_at ASC \
                LIMIT {} \
                FOR UPDATE OF j SKIP LOCKED \
             ) \
             RETURNING id, instance_id, due_at",
            limit.min(1000)
        );
        let params = SqlParams::DataValues(vec![
            DataValue::String(worker_id.to_string()),
            DataValue::DateTime(lease_expires),
            DataValue::DateTime(now),
        ]);
        let ds = query_sql_with_params(&self.db_id, None, &sql, params, "flow_acquire_due")
            .await
            .map_err(|e| StoreError::Backend(format!("抢占到期作业失败: {e}")))?;
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
                    org_id, dimensions, parent_instance_id, parent_token_id, parent_node_bpmn_id, subscriber_id, system_id \
             FROM cmx_flow_instance WHERE parent_instance_id = '{}'",
            escape(parent_instance_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_child_instances")
            .await
            .map_err(|e| StoreError::Backend(format!("查询子实例失败: {e}")))?;
        mapping::rows_to_instances(&ds)
    }

    // ============================ 消息订阅（P3 + A2） ============================

    async fn upsert_message_subscription(
        &self,
        sub: &cmx_flow_model::MessageSubscription,
    ) -> StoreResult<()> {
        let (sql, params) = mapping::upsert_message_subscription(sub);
        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("写入消息订阅失败: {e}")))?;
        Ok(())
    }

    async fn find_catch_subscription(
        &self,
        message_name: &str,
        correlation_key: Option<&str>,
        tenant_id: &str,
    ) -> StoreResult<Option<cmx_flow_model::MessageSubscription>> {
        // 先按 message_name + tenant_id + kind=CATCH 找候选，再在 Rust 侧做相关键校验。
        // 生产上相关键可以下推进 SQL — 但这里保持与 InMemory 一致的「候选 + 精确匹配」语义。
        let sql = format!(
            "SELECT id, kind, message_name, instance_id, token_id, node_bpmn_id, \
                    correlation_var, definition_key, tenant_id, created_at \
             FROM cmx_flow_message_subscription \
             WHERE message_name = '{}' AND tenant_id = '{}' AND kind = 'CATCH' \
             ORDER BY created_at ASC LIMIT 100",
            escape(message_name),
            escape(tenant_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_catch_sub")
            .await
            .map_err(|e| StoreError::Backend(format!("查询消息订阅失败: {e}")))?;
        let subs = mapping::rows_to_message_subscriptions(&ds)?;
        // 相关键过滤：无相关键 → 首个匹配；有相关键 → 要求 correlation_var 有值（精确匹配由引擎做）。
        let result = subs
            .into_iter()
            .find(|s| match (correlation_key, &s.correlation_var) {
                (Some(_key), Some(_var)) => true, // 引擎层做变量值比对
                (None, _) => true,
                _ => false,
            });
        Ok(result)
    }

    async fn find_start_subscription(
        &self,
        message_name: &str,
        tenant_id: &str,
    ) -> StoreResult<Option<cmx_flow_model::MessageSubscription>> {
        let sql = format!(
            "SELECT id, kind, message_name, instance_id, token_id, node_bpmn_id, \
                    correlation_var, definition_key, tenant_id, created_at \
             FROM cmx_flow_message_subscription \
             WHERE message_name = '{}' AND tenant_id = '{}' AND kind = 'START' \
             ORDER BY created_at DESC LIMIT 1",
            escape(message_name),
            escape(tenant_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_start_sub")
            .await
            .map_err(|e| StoreError::Backend(format!("查询消息启动订阅失败: {e}")))?;
        let mut subs = mapping::rows_to_message_subscriptions(&ds)?;
        Ok(subs.pop())
    }

    async fn delete_message_subscription(&self, sub_id: &str) -> StoreResult<()> {
        let sql = format!(
            "DELETE FROM cmx_flow_message_subscription WHERE id = '{}'",
            escape(sub_id)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| StoreError::Backend(format!("删除消息订阅失败: {e}")))?;
        Ok(())
    }

    async fn delete_subscriptions_by_instance(&self, instance_id: &str) -> StoreResult<()> {
        let sql = format!(
            "DELETE FROM cmx_flow_message_subscription WHERE instance_id = '{}'",
            escape(instance_id)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| StoreError::Backend(format!("批量删除实例消息订阅失败: {e}")))?;
        Ok(())
    }

    async fn delete_start_subscriptions_by_def(&self, definition_key: &str) -> StoreResult<()> {
        let sql = format!(
            "DELETE FROM cmx_flow_message_subscription WHERE definition_key = '{}' AND kind = 'START'",
            escape(definition_key)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| StoreError::Backend(format!("删除 Start 消息订阅失败: {e}")))?;
        Ok(())
    }

    // ============================ 异步 Job（P1）============================

    async fn upsert_async_job(&self, job: &AsyncJob) -> StoreResult<()> {
        let (sql, params) = mapping::upsert_async_job(job);
        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("写入异步作业失败: {e}")))?;
        Ok(())
    }

    async fn acquire_async_jobs(
        &self,
        worker_id: &str,
        topic_filter: Option<&str>,
        lock_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<AsyncJob>> {
        // SKIP LOCKED 集群安全抢占：内层 SELECT ... FOR UPDATE SKIP LOCKED 只锁本 worker
        // 能拿到的行（跳过其它事务已持有的），外层 UPDATE 打上 locked_by/lock_expires_at 并
        // RETURNING 回被锁定的作业。两个 worker 并发调用不会拿到同一批行——这是不重复执行的根基。
        let now = chrono::Utc::now();
        let lock_expires = now + chrono::Duration::seconds(lock_secs);
        // A7 topic 隔离：None → topic IS NULL（进程内）；Some(t) → topic = $4（外部 worker）。
        let mut params: Vec<DataValue> = vec![
            DataValue::String(worker_id.to_string()),
            DataValue::DateTime(lock_expires),
            DataValue::DateTime(now),
        ];
        let topic_clause = match topic_filter {
            None => "topic IS NULL".to_string(),
            Some(t) => {
                params.push(DataValue::String(t.to_string()));
                "topic = $4".to_string()
            }
        };
        let sql = format!(
            "UPDATE cmx_flow_async_job SET locked_by = $1, lock_expires_at = $2 \
             WHERE id IN ( \
                SELECT id FROM cmx_flow_async_job \
                WHERE (locked_by IS NULL OR lock_expires_at <= $3) AND {topic_clause} \
                ORDER BY created_at ASC \
                LIMIT {} \
                FOR UPDATE SKIP LOCKED \
             ) \
             RETURNING id, instance_id, token_id, node_bpmn_id, delegate_key, topic, max_retries, \
                       retries, retry_backoff_seconds, locked_by, lock_expires_at, created_at",
            limit.min(1000)
        );
        let ds = query_sql_with_params(
            &self.db_id,
            None,
            &sql,
            SqlParams::DataValues(params),
            "flow_acquire_async",
        )
        .await
        .map_err(|e| StoreError::Backend(format!("抢占异步作业失败: {e}")))?;
        mapping::rows_to_async_jobs(&ds)
    }

    async fn complete_async_job(
        &self,
        job_id: &str,
        _result_variables: Option<serde_json::Value>,
    ) -> StoreResult<Option<AsyncJob>> {
        // 删除作业并 RETURNING 全列——引擎据此把令牌转 Active 继续推进；完整作业数据
        // 供实例 save CAS 冲突时的补偿重插（作业行复活 → 租约过期后可重抢，审查 C-01）。
        // 结果变量由引擎在 complete_async_job 里 merge 进实例变量，不落本表（本表无变量列）。
        let sql = "DELETE FROM cmx_flow_async_job WHERE id = $1 RETURNING id, instance_id,                    token_id, node_bpmn_id, delegate_key, topic, max_retries, retries,                    retry_backoff_seconds, locked_by, lock_expires_at, created_at"
            .to_string();
        let params = SqlParams::DataValues(vec![DataValue::String(job_id.to_string())]);
        let ds = query_sql_with_params(&self.db_id, None, &sql, params, "flow_complete_async")
            .await
            .map_err(|e| StoreError::Backend(format!("完成异步作业失败: {e}")))?;
        Ok(mapping::rows_to_async_jobs(&ds)?.into_iter().next())
    }

    async fn fail_async_job(&self, job_id: &str, _error: &str) -> StoreResult<bool> {
        // 失败：retries-1 并释放锁（其它 worker 可重抢）；RETURNING 新 retries 判是否耗尽。
        let sql = "UPDATE cmx_flow_async_job \
                   SET retries = retries - 1, locked_by = NULL, lock_expires_at = NULL \
                   WHERE id = $1 RETURNING retries"
            .to_string();
        let params = SqlParams::DataValues(vec![DataValue::String(job_id.to_string())]);
        let ds = query_sql_with_params(&self.db_id, None, &sql, params, "flow_fail_async")
            .await
            .map_err(|e| StoreError::Backend(format!("标记异步作业失败: {e}")))?;
        match mapping::first_retries(&ds) {
            // 不存在（已被完成/删除）：视为无重试余地。
            None => Ok(false),
            Some(r) if r <= 0 => {
                // 重试耗尽 → 删除（P1：删除即死信；后续 P2 可改为转独立死信表）。
                let del = "DELETE FROM cmx_flow_async_job WHERE id = $1".to_string();
                let dp = SqlParams::DataValues(vec![DataValue::String(job_id.to_string())]);
                execute_sql_with_params(&self.db_id, None, &del, dp)
                    .await
                    .map_err(|e| StoreError::Backend(format!("删除耗尽异步作业失败: {e}")))?;
                Ok(false)
            }
            Some(_) => Ok(true),
        }
    }

    async fn delete_async_jobs_by_instance(&self, instance_id: &str) -> StoreResult<()> {
        let sql = format!(
            "DELETE FROM cmx_flow_async_job WHERE instance_id = '{}'",
            escape(instance_id)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| StoreError::Backend(format!("批量删除实例异步作业失败: {e}")))?;
        Ok(())
    }

    async fn get_async_job(&self, job_id: &str) -> StoreResult<Option<AsyncJob>> {
        let sql = format!(
            "SELECT id, instance_id, token_id, node_bpmn_id, delegate_key, topic, max_retries, \
                    retries, retry_backoff_seconds, locked_by, lock_expires_at, created_at \
             FROM cmx_flow_async_job WHERE id = '{}'",
            escape(job_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_get_async")
            .await
            .map_err(|e| StoreError::Backend(format!("查询异步作业失败: {e}")))?;
        Ok(mapping::rows_to_async_jobs(&ds)?.into_iter().next())
    }

    // ============================ 死信队列（P2）============================

    async fn upsert_dead_letter_job(&self, job: &DeadLetterJob) -> StoreResult<()> {
        let (sql, params) = mapping::upsert_dead_letter_job(job);
        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("写入死信作业失败: {e}")))?;
        Ok(())
    }

    async fn list_dead_letter_jobs(&self, limit: usize) -> StoreResult<Vec<DeadLetterJob>> {
        let sql = format!(
            "SELECT id, instance_id, token_id, node_bpmn_id, delegate_key, max_retries, \
                    error, original_created_at, dead_lettered_at, tenant_id \
             FROM cmx_flow_deadletter_job \
             ORDER BY dead_lettered_at DESC LIMIT {}",
            limit.min(1000)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_deadletter_list")
            .await
            .map_err(|e| StoreError::Backend(format!("查询死信作业失败: {e}")))?;
        mapping::rows_to_dead_letter_jobs(&ds)
    }

    async fn get_dead_letter_job(&self, job_id: &str) -> StoreResult<Option<DeadLetterJob>> {
        let sql = format!(
            "SELECT id, instance_id, token_id, node_bpmn_id, delegate_key, max_retries, \
                    error, original_created_at, dead_lettered_at, tenant_id \
             FROM cmx_flow_deadletter_job WHERE id = '{}'",
            escape(job_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_deadletter_get")
            .await
            .map_err(|e| StoreError::Backend(format!("查询死信作业失败: {e}")))?;
        Ok(mapping::rows_to_dead_letter_jobs(&ds)?.into_iter().next())
    }

    async fn delete_dead_letter_job(&self, job_id: &str) -> StoreResult<()> {
        let sql = format!(
            "DELETE FROM cmx_flow_deadletter_job WHERE id = '{}'",
            escape(job_id)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| StoreError::Backend(format!("删除死信作业失败: {e}")))?;
        Ok(())
    }

    // ============================ 活动历史（A6）============================

    async fn upsert_hi_activity(&self, activity: &ActivityRecord) -> StoreResult<()> {
        let (sql, params) = mapping::upsert_hi_activity(activity);
        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("写入活动历史失败: {e}")))?;
        Ok(())
    }

    async fn list_activities_by_instance(
        &self,
        instance_id: &str,
    ) -> StoreResult<Vec<ActivityRecord>> {
        let sql = format!(
            "SELECT id, instance_id, token_id, activity_bpmn_id, activity_name, activity_type, \
                    entered_at, exited_at, duration_ms, assignee, tenant_id \
             FROM cmx_flow_hi_activity WHERE instance_id = '{}' \
             ORDER BY entered_at ASC, exited_at ASC",
            escape(instance_id)
        );
        let ds = query_sql(&self.db_id, None, &sql, "flow_hi_activity")
            .await
            .map_err(|e| StoreError::Backend(format!("查询活动历史失败: {e}")))?;
        mapping::rows_to_activities(&ds)
    }

    // ==================== 故障清单（技术债 011）====================

    async fn upsert_incident(
        &self,
        inc: &cmx_flow_model::IncidentRecord,
    ) -> StoreResult<()> {
        let sql = "INSERT INTO cmx_flow_incident \
            (id, instance_id, token_id, node_bpmn_id, definition_key, business_key, reason, retries, state, created_at, updated_at) \
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'OPEN', $9, $10) \
            ON CONFLICT (instance_id, node_bpmn_id) DO UPDATE SET \
              token_id = EXCLUDED.token_id, reason = EXCLUDED.reason, retries = EXCLUDED.retries, \
              state = 'OPEN', updated_at = EXCLUDED.updated_at";
        let params = SqlParams::DataValues(vec![
            // O-08：主键用 uuid——原 `inc-{instance_id}-{node}` 拼串在 node>19 字符时超出
            // VARCHAR(64) 被 PG 拒绝（upsert_incident_aux 仅 warn，台账静默丢失）；幂等由
            // uk(instance_id, node_bpmn_id) 的 ON CONFLICT 承担，主键无语义职责。
            DataValue::String(uuid::Uuid::new_v4().to_string()),
            DataValue::String(inc.instance_id.clone()),
            mapping::opt_text(&inc.token_id),
            DataValue::String(inc.node_bpmn_id.clone()),
            DataValue::String(inc.definition_key.clone()),
            mapping::opt_text(&inc.business_key),
            DataValue::String(inc.reason.clone()),
            DataValue::Int(inc.retries),
            DataValue::DateTime(inc.created_at),
            DataValue::DateTime(inc.updated_at),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("登记 incident 失败: {e}")))?;
        Ok(())
    }

    async fn resolve_incident_by_node(
        &self,
        instance_id: &str,
        node_bpmn_id: &str,
    ) -> StoreResult<()> {
        let sql = "UPDATE cmx_flow_incident SET state = 'RESOLVED', updated_at = $3                    WHERE instance_id = $1 AND node_bpmn_id = $2 AND state = 'OPEN'";
        let params = SqlParams::DataValues(vec![
            DataValue::String(instance_id.to_string()),
            DataValue::String(node_bpmn_id.to_string()),
            DataValue::DateTime(chrono::Utc::now()),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("关闭节点 incident 失败: {e}")))?;
        Ok(())
    }

    async fn resolve_incidents_by_instance(&self, instance_id: &str) -> StoreResult<()> {
        let sql = "UPDATE cmx_flow_incident SET state = 'RESOLVED', updated_at = $2 \
                   WHERE instance_id = $1 AND state = 'OPEN'";
        let params = SqlParams::DataValues(vec![
            DataValue::String(instance_id.to_string()),
            DataValue::DateTime(chrono::Utc::now()),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| StoreError::Backend(format!("关闭 incident 失败: {e}")))?;
        Ok(())
    }

    // ==================== 引擎派生变量历史（决策/子流程回填）====================

    /// 把引擎派生的变量变更落到与 app 层「调用方送入」历史**同一张表** `cmx_flow_var_history`——
    /// 读端点 `GET /instances/{id}/variables/history` 遂一次返回全部来源。`changed_by='system'`
    /// 标记引擎派生（区别于人工办理），`source` 为 `decision`/`subflow`。表由 `PgVarHistoryStore::
    /// ensure_schema` 在启动时建好。复用 `PgVarHistoryStore::record`（同表同 INSERT），只把中立模型的
    /// `VarChangeRecord` 映射成 store 层 `VarChange`（`changed_by=system`），避免重复 INSERT 语句。
    async fn record_var_changes(&self, changes: &[VarChangeRecord]) -> StoreResult<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mapped: Vec<crate::var_history::VarChange> = changes
            .iter()
            .map(|c| crate::var_history::VarChange {
                instance_id: c.instance_id.clone(),
                var_name: c.var_name.clone(),
                old_value: c.old_value.clone(),
                new_value: c.new_value.clone(),
                source: c.source.clone(),
                node_bpmn_id: c.node_bpmn_id.clone(),
                changed_by: Some("system".to_string()),
            })
            .collect();
        crate::var_history::PgVarHistoryStore::new(self.db_id.clone())
            .record(&mapped)
            .await
            .map_err(StoreError::Backend)
    }
}

/// 单引号转义：load 路径的 id 用字面量拼接（id 是引擎生成的 UUID，无注入面），
/// 仍做转义以防御。写路径一律走参数化，不用此函数。
fn escape(s: &str) -> String {
    s.replace('\'', "''")
}
