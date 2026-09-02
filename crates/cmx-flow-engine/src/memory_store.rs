/*
 * @Describe: 进程内 RuntimeStore 实现（测试与嵌入式用）。
 *
 * 用一把 Mutex 保护 instance_id → InstanceSnapshot 的 map，深拷贝进出。语义与 PG 实现
 * 对齐：create/load/save 三方法，save 覆盖整份聚合。零外部依赖，M1 的 e2e 测试主力。
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_model::{
    ActivityRecord, AsyncJob, CcSummary, DeadLetterJob, DueJob, InstanceSnapshot, InstanceSummary,
    MessageSubscription, MessageSubscriptionKind, RuntimeStore, StoreError, StoreResult,
};
use tokio::sync::Mutex;

/// 落库前剥离**推进段瞬态**字段（`pending_subs`/`pending_activities`/`pending_var_changes`）。
///
/// 这些字段契约上「不持久化、落库后即弃」（serde skip）。PG 实现天然如此：`load_snapshot` 从列
/// 重建时它们恒为空。内存实现深拷贝整份聚合，若不剥离，则后续 `load_snapshot` 会把上段已 flush
/// 的暂存项一并带回、被下段再次 flush（A6 活动历史靠 `ON CONFLICT(id) DO NOTHING` 幂等掩盖了此
/// 重复；无 id 的派生变量历史会因此重复记录）。在此剥离使内存/PG 两实现对齐同一契约。
fn stripped(snapshot: &InstanceSnapshot) -> InstanceSnapshot {
    let mut s = snapshot.clone();
    s.pending_subs.clear();
    s.pending_activities.clear();
    s.pending_var_changes.clear();
    s
}

/// 进程内运行态存储。
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<HashMap<String, InstanceSnapshot>>>,
    /// 消息订阅（P3 + A2）：id → MessageSubscription。
    subs: Arc<Mutex<HashMap<String, MessageSubscription>>>,
    /// 异步服务任务作业（P1）：job_id → AsyncJob。
    async_jobs: Arc<Mutex<HashMap<String, AsyncJob>>>,
    /// 死信作业（P2）：job_id → DeadLetterJob。
    dead_letter: Arc<Mutex<HashMap<String, DeadLetterJob>>>,
    /// 活动历史（A6）：activity_id → ActivityRecord。
    activities: Arc<Mutex<HashMap<String, ActivityRecord>>>,
}

impl InMemoryStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前保存的实例数（测试辅助）。
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// 是否无任何实例（测试辅助；与 len 配套，满足 clippy 约定）。
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}

#[async_trait]
impl RuntimeStore for InMemoryStore {
    async fn create_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        let mut guard = self.inner.lock().await;
        guard.insert(snapshot.instance.id.clone(), stripped(snapshot));
        Ok(())
    }

    async fn load_snapshot(&self, instance_id: &str) -> StoreResult<InstanceSnapshot> {
        let guard = self.inner.lock().await;
        guard
            .get(instance_id)
            .cloned()
            .ok_or_else(|| StoreError::InstanceNotFound(instance_id.to_string()))
    }

    async fn save_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()> {
        // X3-7（C-03）：与 PG 实现的 CAS 语义对齐（文件头「语义与 PG 对齐」此前对 CAS 不
        // 成立）——以 load 所得 version 比对，冲突返回 Conflict；成功对**存储副本** version+1
        //（不回写调用者，与 PG 的 `SET version=version+1` 逐字节对齐）。常规单测由此可覆盖
        // 冲突分支（PG 门控测试之外的第二通道）。
        let mut guard = self.inner.lock().await;
        let expected = snapshot.version;
        match guard.get_mut(&snapshot.instance.id) {
            None => return Err(StoreError::InstanceNotFound(snapshot.instance.id.clone())),
            Some(stored) => {
                if stored.version != expected {
                    return Err(StoreError::Conflict(format!(
                        "实例 {} 已被并发修改（内存存储期望 version={expected}，实际 {}）",
                        snapshot.instance.id, stored.version
                    )));
                }
                let mut next = stripped(snapshot);
                next.version = expected + 1;
                *stored = next;
            }
        }
        Ok(())
    }

    async fn list_instances(&self, limit: usize) -> StoreResult<Vec<InstanceSummary>> {
        let guard = self.inner.lock().await;
        let mut summaries: Vec<InstanceSummary> = guard
            .values()
            .map(|snap| InstanceSummary {
                id: snap.instance.id.clone(),
                definition_key: snap.instance.definition_key.clone(),
                business_key: snap.instance.business_key.clone(),
                state: snap.instance.state,
                variables: snap.instance.variables.clone(),
                open_task_count: snap.tasks.iter().filter(|t| !t.completed).count(),
                created_at: snap.instance.created_at,
                updated_at: snap.instance.updated_at,
            })
            .collect();
        // 按创建时间倒序（最新在前）。
        summaries.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        summaries.truncate(limit);
        Ok(summaries)
    }

    async fn acquire_due_jobs(
        &self,
        _worker_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        _lease_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>> {
        // 内存实现（测试/嵌入）：Mutex 内 load-modify-save 原子，无跨进程竞争，
        // 租约打标省略——抢占语义由互斥锁天然保证。
        let guard = self.inner.lock().await;
        let mut due: Vec<DueJob> = guard
            .values()
            .filter(|snap| !snap.instance.state.is_terminal())
            .flat_map(|snap| {
                snap.jobs
                    .iter()
                    .filter(|j| j.due_at <= now)
                    .map(|j| DueJob {
                        instance_id: j.instance_id.clone(),
                        job_id: j.id.clone(),
                        due_at: j.due_at,
                    })
            })
            .collect();
        // 按到期时刻升序（先到期先处理）。
        due.sort_by_key(|d| d.due_at);
        due.truncate(limit);
        Ok(due)
    }

    async fn find_cc_for_user(
        &self,
        user_id: &str,
        unread_only: bool,
        limit: usize,
    ) -> StoreResult<Vec<CcSummary>> {
        let guard = self.inner.lock().await;
        let mut out: Vec<CcSummary> = guard
            .values()
            .flat_map(|snap| {
                snap.cc_records
                    .iter()
                    .filter(|c| c.to_user_id == user_id)
                    .filter(|c| !unread_only || c.read_at.is_none())
                    .map(|c| CcSummary {
                        id: c.id.clone(),
                        instance_id: c.instance_id.clone(),
                        business_key: snap.instance.business_key.clone(),
                        definition_key: snap.instance.definition_key.clone(),
                        node_bpmn_id: c.node_bpmn_id.clone(),
                        reason: c.reason.clone(),
                        read: c.read_at.is_some(),
                        created_at: c.created_at,
                    })
            })
            .collect();
        // 按抄送时刻倒序（最新在前）。
        out.sort_by_key(|c| std::cmp::Reverse(c.created_at));
        out.truncate(limit);
        Ok(out)
    }

    async fn mark_cc_read(
        &self,
        cc_id: &str,
        read_at: chrono::DateTime<chrono::Utc>,
    ) -> StoreResult<bool> {
        let mut guard = self.inner.lock().await;
        for snap in guard.values_mut() {
            if let Some(c) = snap.cc_records.iter_mut().find(|c| c.id == cc_id) {
                if c.read_at.is_none() {
                    c.read_at = Some(read_at);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn find_child_instances(
        &self,
        parent_instance_id: &str,
    ) -> StoreResult<Vec<cmx_flow_model::ProcessInstance>> {
        let guard = self.inner.lock().await;
        let out = guard
            .values()
            .filter(|snap| snap.instance.parent_instance_id.as_deref() == Some(parent_instance_id))
            .map(|snap| snap.instance.clone())
            .collect();
        Ok(out)
    }

    // ============================ 消息订阅（P3 + A2） ============================

    async fn upsert_message_subscription(&self, sub: &MessageSubscription) -> StoreResult<()> {
        self.subs.lock().await.insert(sub.id.clone(), sub.clone());
        Ok(())
    }

    async fn find_catch_subscription(
        &self,
        message_name: &str,
        correlation_key: Option<&str>,
        tenant_id: &str,
    ) -> StoreResult<Option<MessageSubscription>> {
        let guard = self.subs.lock().await;
        let result = guard
            .values()
            .find(|s| {
                s.kind == MessageSubscriptionKind::Catch
                    && s.message_name == message_name
                    && s.tenant_id == tenant_id
                    && match (correlation_key, &s.correlation_var) {
                        // 若节点声明了相关键变量且调用方提供了 key，从实例变量中验证——
                        // InMemory 无法直接查实例变量，这里只匹配有无声明，PG 实现做精确匹配。
                        (Some(_key), Some(_var)) => true,
                        (None, None) | (None, Some(_)) => true,
                        (Some(_), None) => true,
                    }
            })
            .cloned();
        Ok(result)
    }

    async fn find_start_subscription(
        &self,
        message_name: &str,
        tenant_id: &str,
    ) -> StoreResult<Option<MessageSubscription>> {
        let guard = self.subs.lock().await;
        // 最新创建的胜出（最后部署）。
        let result = guard
            .values()
            .filter(|s| {
                s.kind == MessageSubscriptionKind::Start
                    && s.message_name == message_name
                    && s.tenant_id == tenant_id
            })
            .max_by_key(|s| s.created_at)
            .cloned();
        Ok(result)
    }

    async fn delete_message_subscription(&self, sub_id: &str) -> StoreResult<()> {
        self.subs.lock().await.remove(sub_id);
        Ok(())
    }

    async fn delete_subscriptions_by_instance(&self, instance_id: &str) -> StoreResult<()> {
        self.subs
            .lock()
            .await
            .retain(|_, s| s.instance_id.as_deref() != Some(instance_id));
        Ok(())
    }

    async fn delete_start_subscriptions_by_def(&self, definition_key: &str) -> StoreResult<()> {
        self.subs.lock().await.retain(|_, s| {
            !(s.kind == MessageSubscriptionKind::Start
                && s.definition_key.as_deref() == Some(definition_key))
        });
        Ok(())
    }

    async fn upsert_async_job(&self, job: &AsyncJob) -> StoreResult<()> {
        self.async_jobs.lock().await.insert(job.id.clone(), job.clone());
        Ok(())
    }

    async fn acquire_async_jobs(
        &self,
        worker_id: &str,
        topic_filter: Option<&str>,
        lock_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<AsyncJob>> {
        let now = chrono::Utc::now();
        let lock_expires = now + chrono::Duration::seconds(lock_secs);
        let mut guard = self.async_jobs.lock().await;
        let mut result = Vec::new();
        for job in guard.values_mut() {
            if result.len() >= limit {
                break;
            }
            // A7 topic 隔离：None 只取无 topic 作业（进程内），Some(t) 只取该 topic 作业（外部）。
            let topic_ok = match topic_filter {
                None => job.topic.is_none(),
                Some(t) => job.topic.as_deref() == Some(t),
            };
            if !topic_ok {
                continue;
            }
            let unlocked = job.locked_by.is_none()
                || job.lock_expires_at.map(|e| e <= now).unwrap_or(true);
            if unlocked {
                job.locked_by = Some(worker_id.to_string());
                job.lock_expires_at = Some(lock_expires);
                result.push(job.clone());
            }
        }
        Ok(result)
    }

    async fn complete_async_job(
        &self,
        job_id: &str,
        _result_variables: Option<serde_json::Value>,
    ) -> StoreResult<Option<AsyncJob>> {
        let mut guard = self.async_jobs.lock().await;
        Ok(guard.remove(job_id))
    }

    async fn fail_async_job(
        &self,
        job_id: &str,
        _error: &str,
    ) -> StoreResult<bool> {
        let mut guard = self.async_jobs.lock().await;
        let retries = match guard.get_mut(job_id) {
            Some(job) => {
                job.retries -= 1;
                if job.retries > 0 {
                    job.locked_by = None;
                    job.lock_expires_at = None;
                }
                job.retries
            }
            None => return Ok(false),
        };
        if retries <= 0 {
            guard.remove(job_id);
            return Ok(false);
        }
        Ok(true)
    }

    async fn delete_async_jobs_by_instance(&self, instance_id: &str) -> StoreResult<()> {
        self.async_jobs
            .lock()
            .await
            .retain(|_, j| j.instance_id != instance_id);
        Ok(())
    }

    async fn get_async_job(&self, job_id: &str) -> StoreResult<Option<AsyncJob>> {
        Ok(self.async_jobs.lock().await.get(job_id).cloned())
    }

    // ============================ 死信队列（P2）============================

    async fn upsert_dead_letter_job(&self, job: &DeadLetterJob) -> StoreResult<()> {
        self.dead_letter
            .lock()
            .await
            .insert(job.id.clone(), job.clone());
        Ok(())
    }

    async fn list_dead_letter_jobs(&self, limit: usize) -> StoreResult<Vec<DeadLetterJob>> {
        let guard = self.dead_letter.lock().await;
        let mut out: Vec<DeadLetterJob> = guard.values().cloned().collect();
        // 按死信时刻倒序（最新在前）。
        out.sort_by_key(|j| std::cmp::Reverse(j.dead_lettered_at));
        out.truncate(limit);
        Ok(out)
    }

    async fn get_dead_letter_job(&self, job_id: &str) -> StoreResult<Option<DeadLetterJob>> {
        Ok(self.dead_letter.lock().await.get(job_id).cloned())
    }

    async fn delete_dead_letter_job(&self, job_id: &str) -> StoreResult<()> {
        self.dead_letter.lock().await.remove(job_id);
        Ok(())
    }

    // ============================ 活动历史（A6）============================

    async fn upsert_hi_activity(&self, activity: &ActivityRecord) -> StoreResult<()> {
        self.activities
            .lock()
            .await
            .insert(activity.id.clone(), activity.clone());
        Ok(())
    }

    async fn list_activities_by_instance(
        &self,
        instance_id: &str,
    ) -> StoreResult<Vec<ActivityRecord>> {
        let guard = self.activities.lock().await;
        let mut out: Vec<ActivityRecord> = guard
            .values()
            .filter(|a| a.instance_id == instance_id)
            .cloned()
            .collect();
        // 按进入时刻升序；同刻（零时长穿透链）用离开时刻兜底，保证轨迹顺序稳定。
        out.sort_by_key(|a| (a.entered_at, a.exited_at));
        Ok(out)
    }
}
