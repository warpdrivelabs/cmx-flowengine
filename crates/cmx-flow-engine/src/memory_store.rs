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
    CcSummary, DueJob, InstanceSnapshot, InstanceSummary, RuntimeStore, StoreError, StoreResult,
};
use tokio::sync::Mutex;

/// 进程内运行态存储。
#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<HashMap<String, InstanceSnapshot>>>,
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
        guard.insert(snapshot.instance.id.clone(), snapshot.clone());
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
        let mut guard = self.inner.lock().await;
        if !guard.contains_key(&snapshot.instance.id) {
            return Err(StoreError::InstanceNotFound(snapshot.instance.id.clone()));
        }
        guard.insert(snapshot.instance.id.clone(), snapshot.clone());
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

    async fn find_due_jobs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>> {
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
}
