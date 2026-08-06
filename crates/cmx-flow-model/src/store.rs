/*
 * @Describe: RuntimeStore —— 驱动无关的持久化契约。
 *
 * 引擎只依赖这个 trait，不认识任何数据库。内存实现（cmx-flow-engine）用于测试与嵌入，
 * PG 实现（cmx-flow-store-pg）接入 cmx-database-pg。契约以 InstanceSnapshot 聚合为
 * 单位，把事务边界收在实现内部：一次 save_snapshot = 一个原子提交。
 */

use async_trait::async_trait;

use crate::runtime::{CcSummary, DueJob, InstanceSnapshot, InstanceSummary};

/// 持久化错误：跨实现的中立错误壳，实现方把自身错误转成字符串塞入。
///
/// 不在此 crate `#[from]` 具体驱动错误——那会把 DB 依赖泄漏进中立模型层。
#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    /// 目标实例不存在。
    #[error("流程实例不存在: {0}")]
    InstanceNotFound(String),

    /// 底层存储错误（DB/IO 等，由实现方桥接）。
    #[error("存储层错误: {0}")]
    Backend(String),

    /// 乐观并发冲突（M1 预留，暂不启用版本号）。
    #[error("并发冲突: {0}")]
    Conflict(String),
}

/// 持久化结果别名。
pub type StoreResult<T> = core::result::Result<T, StoreError>;

/// 运行态存储契约。
///
/// 方法刻意精简到 M1 所需：建实例、按 id 载入聚合、原子保存聚合。查询类 API
/// （按办理人列任务等）留给消费侧或后续里程碑，避免过早膨胀契约。
#[async_trait]
pub trait RuntimeStore: Send + Sync {
    /// 持久化一个**新建**实例聚合（实例首次落库）。
    ///
    /// 语义上等价于 save_snapshot 的 insert 路径，单列出来是为了让「创建」在实现层
    /// 可以走更直白的 INSERT，也让调用点语义清晰。
    async fn create_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()>;

    /// 按实例 id 载入完整聚合（实例 + 令牌 + 未办结任务）。
    async fn load_snapshot(&self, instance_id: &str) -> StoreResult<InstanceSnapshot>;

    /// 原子保存一个**已存在**实例的聚合快照（一次运行段的落库提交点）。
    ///
    /// 实现须在单事务内完成：更新实例、重写其令牌与任务，使 DB 状态与内存快照一致。
    async fn save_snapshot(&self, snapshot: &InstanceSnapshot) -> StoreResult<()>;

    /// 列出实例摘要，按创建时间**倒序**（最新在前），最多 `limit` 条。
    ///
    /// 只读实例级字段 + 未办结任务数，不载入完整聚合。供列表/看板页使用；重启后从这里
    /// 恢复实例列表，无需进程内台账。
    async fn list_instances(&self, limit: usize) -> StoreResult<Vec<InstanceSummary>>;

    /// 跨实例查出所有 `due_at <= now` 的到期定时器作业（M2.5），按到期时刻升序，最多 `limit` 条。
    ///
    /// 定时器推进器据此逐个 load→fire→save。只读轻量视图，不载入完整聚合。
    async fn find_due_jobs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>>;

    /// 跨实例查出抄送给指定用户的记录（M4.2），按抄送时刻倒序，最多 `limit` 条。
    ///
    /// `unread_only` = true 只返回未读。供「抄送我的」列表；只读轻量视图。
    async fn find_cc_for_user(
        &self,
        user_id: &str,
        unread_only: bool,
        limit: usize,
    ) -> StoreResult<Vec<CcSummary>>;

    /// 标记一条抄送记录为已读（M4.2）。幂等：已读再标无害。返回是否命中一条记录。
    async fn mark_cc_read(
        &self,
        cc_id: &str,
        read_at: chrono::DateTime<chrono::Utc>,
    ) -> StoreResult<bool>;

    /// 查某主实例的全部子实例（M5，按 parent_instance_id）。返回子实例头（含 parent_token_id）。
    ///
    /// 供 callActivity 判断某令牌是否已启动子实例、以及级联处理。只读，不载完整聚合。
    async fn find_child_instances(
        &self,
        parent_instance_id: &str,
    ) -> StoreResult<Vec<crate::runtime::ProcessInstance>>;
}
