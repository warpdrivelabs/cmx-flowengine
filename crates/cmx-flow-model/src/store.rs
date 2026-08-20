/*
 * @Describe: RuntimeStore —— 驱动无关的持久化契约。
 *
 * 引擎只依赖这个 trait，不认识任何数据库。内存实现（cmx-flow-engine）用于测试与嵌入，
 * PG 实现（cmx-flow-store-pg）接入 cmx-database-pg。契约以 InstanceSnapshot 聚合为
 * 单位，把事务边界收在实现内部：一次 save_snapshot = 一个原子提交。
 */

use async_trait::async_trait;

use crate::runtime::{CcSummary, DueJob, InstanceSnapshot, InstanceSummary, MessageSubscription};

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

    // ============================ 消息订阅（P3 + A2） ============================

    /// 写入一条消息订阅记录（幂等：同 id 再写直接覆盖）。
    ///
    /// - `Catch` 类型：`WaitingMessage` 令牌到达时调用。
    /// - `Start` 类型：流程定义发布/部署时调用（每个消息启动事件一条）。
    async fn upsert_message_subscription(
        &self,
        sub: &MessageSubscription,
    ) -> StoreResult<()>;

    /// 按消息名 + 租户 + 可选相关键查找匹配的 `Catch` 订阅（P3）。
    ///
    /// 替代 `find_message_instance` 的 500 实例全量扫描：直接索引查询，O(1) 而非 O(n)。
    /// 返回首个匹配记录（instance_id + token_id + correlation_var）；无命中返回 None。
    async fn find_catch_subscription(
        &self,
        message_name: &str,
        correlation_key: Option<&str>,
        tenant_id: &str,
    ) -> StoreResult<Option<MessageSubscription>>;

    /// 按消息名 + 租户查找 `Start` 类型订阅（A2 消息启动）。
    ///
    /// 返回第一个匹配的已部署定义 definition_key；多个时按创建时间取最新（最后部署的胜出）。
    async fn find_start_subscription(
        &self,
        message_name: &str,
        tenant_id: &str,
    ) -> StoreResult<Option<MessageSubscription>>;

    /// 删除指定 id 的消息订阅（令牌离开 / 实例终止 / 撤部署时清理）。幂等，不存在视为成功。
    async fn delete_message_subscription(&self, sub_id: &str) -> StoreResult<()>;

    /// 删除某实例的全部消息订阅（实例取消/终止时批量清理）。幂等。
    async fn delete_subscriptions_by_instance(&self, instance_id: &str) -> StoreResult<()>;

    /// 删除某流程定义的全部 `Start` 类型消息订阅（撤部署/重部署时清理旧记录）。幂等。
    async fn delete_start_subscriptions_by_def(&self, definition_key: &str) -> StoreResult<()>;

    // ============================ 异步 Job（P1）============================

    /// 写入或更新一个 AsyncJob（首次创建时 locked_by/lock_expires_at 为 None）。
    async fn upsert_async_job(&self, job: &crate::runtime::AsyncJob) -> StoreResult<()>;

    /// SKIP LOCKED 抢占：取最多 `limit` 个未锁定/锁已超期的 AsyncJob，
    /// 更新 locked_by + lock_expires_at，返回被锁定的作业列表。
    ///
    /// `topic_filter`（A7 隔离进程内 vs 外部 worker）：
    /// - `None` → 只取 `topic IS NULL` 的作业（进程内 poller，跑注册 delegate）；
    /// - `Some(t)` → 只取 `topic = t` 的作业（外部 worker 按主题拉取）。
    /// 这保证两类 worker 拿到互不相交的作业集，外部作业永不被进程内 poller 误领（无 delegate 会误杀）。
    async fn acquire_async_jobs(
        &self,
        worker_id: &str,
        topic_filter: Option<&str>,
        lock_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<crate::runtime::AsyncJob>>;

    /// 完成一个 AsyncJob：删除记录（或标记 done），并把对应令牌 instance 加载回调用者处理。
    /// 返回 instance_id + token_id，引擎据此继续推进。
    async fn complete_async_job(
        &self,
        job_id: &str,
        result_variables: Option<serde_json::Value>,
    ) -> StoreResult<Option<(String, String)>>;

    /// 失败一个 AsyncJob：重试次数 -1；若 retries <= 0 则转死信（或删除）。
    /// 返回是否仍有重试余地（true = 仍可重试，false = 已死信/删除）。
    async fn fail_async_job(
        &self,
        job_id: &str,
        error: &str,
    ) -> StoreResult<bool>;

    /// 删除某实例的全部 AsyncJob（实例取消/终止时清理）。幂等。
    async fn delete_async_jobs_by_instance(&self, instance_id: &str) -> StoreResult<()>;

    /// 按 id 读一个 AsyncJob（外部 worker 完成/失败前取令牌坐标；不存在返回 None）。
    async fn get_async_job(
        &self,
        job_id: &str,
    ) -> StoreResult<Option<crate::runtime::AsyncJob>>;

    // ============================ 死信队列（P2）============================

    /// 写入一条死信作业（幂等：同 id 覆盖）。async job 重试耗尽时调用。
    async fn upsert_dead_letter_job(
        &self,
        job: &crate::runtime::DeadLetterJob,
    ) -> StoreResult<()>;

    /// 列出死信作业（运维台展示），按死信时刻倒序，最多 `limit` 条。
    async fn list_dead_letter_jobs(
        &self,
        limit: usize,
    ) -> StoreResult<Vec<crate::runtime::DeadLetterJob>>;

    /// 按 id 读一条死信作业（重投前取原作业身份；不存在返回 None）。
    async fn get_dead_letter_job(
        &self,
        job_id: &str,
    ) -> StoreResult<Option<crate::runtime::DeadLetterJob>>;

    /// 删除一条死信作业（重投成功后清理 / 运维放弃时删除）。幂等。
    async fn delete_dead_letter_job(&self, job_id: &str) -> StoreResult<()>;

    // ============================ 活动历史（A6）============================

    /// 写入一条已闭合的活动历史记录（幂等：同 id 覆盖）。令牌离开节点时批量调用。
    async fn upsert_hi_activity(
        &self,
        activity: &crate::runtime::ActivityRecord,
    ) -> StoreResult<()>;

    /// 列出某实例的活动历史（节点级审计/SLA），按进入时刻升序。
    async fn list_activities_by_instance(
        &self,
        instance_id: &str,
    ) -> StoreResult<Vec<crate::runtime::ActivityRecord>>;
}
