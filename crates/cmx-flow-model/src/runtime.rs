/*
 * @Describe: 运行态 DTO —— 流程实例、令牌、用户任务。
 *
 * 这些是**引擎与持久化之间流转的数据**，与 IR（静态定义）分离。核心抽象是「令牌」
 * (Token)：BPMN 语义里流经流程图的执行指针。M1 单令牌顺序推进即可覆盖顺序审批流；
 * 并行网关会产生多令牌，那是 M2 的事，但数据结构现在就按「多令牌」建模，避免返工。
 */

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ir::CandidateKind;
use crate::variables::Variables;

/// 流程实例的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstanceState {
    /// 活动中：至少有一个活动令牌，或停在等待态待外部触发。
    Active,
    /// 已挂起（A7）：管理员暂停实例——令牌位置/任务保留，但拒绝一切办理动作（complete/reject/
    /// claim 等），直到 resume。审批场景：暂停一个正在跑的流程（如等待外部裁决、争议冻结）。
    Suspended,
    /// 已完成：所有令牌抵达 endEvent 并被消费。
    Completed,
    /// 已终止：被外部取消（M1 预留，暂无触发路径）。
    Terminated,
}

impl InstanceState {
    /// 是否为终态（已完成或已终止）。终态实例应归档到历史表。
    pub fn is_terminal(self) -> bool {
        matches!(self, InstanceState::Completed | InstanceState::Terminated)
    }
}

/// 令牌的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TokenState {
    /// 活动：可被推进（引擎下一轮会拾取）。
    Active,
    /// 等待：停在等待态节点（如 userTask），等外部 complete。
    Waiting,
    /// 合流等待：停在并行网关 join，等待兄弟令牌到齐（结构性阻塞，非外部等待态）。
    /// 与 Waiting 的区别：无需外部触发，在同一次推进段内当最后一个兄弟到达时即被消解。
    Joining,
    /// 子流程等待（M5）：停在 callActivity，等被调子实例完成。与 Waiting/Joining 并列的
    /// 「挂起等待」态；唤醒信号来自子实例完成（complete_subflow），按 parent_token_id 精确唤醒。
    WaitingSubflow,
    /// 消息等待（A4）：停在消息中间捕获事件，等外部经 `correlate_message` 按消息名+相关键唤醒。
    /// 与 WaitingSubflow 并列的挂起等待态；唤醒信号来自外部系统回调，非内部推进。
    WaitingMessage,
    /// 定时等待（A1）：停在中间定时捕获事件（`intermediateCatchEvent` + timer），等 TimerJob 到期。
    /// 与 WaitingMessage 并列的挂起等待态；唤醒信号来自 `trigger_due_timers`（时间驱动），
    /// 到期后令牌转 Active 沿唯一出边前进。审批场景「等 N 天后自动推进/发提醒」的建模基石。
    WaitingTimer,
    /// 异步服务任务等待（P1）：停在 `serviceTask`（`flowable:async="true"`），等后台 worker 执行完毕。
    ///
    /// `run_to_wait` 为异步 serviceTask 建一个 `AsyncJob` 并把令牌停为 WaitingAsync；
    /// worker 通过 `acquire_async_jobs`（SKIP LOCKED）抢占并执行 delegate，完成后调
    /// `complete_async_job` 把令牌转 Active 并继续 run_to_wait。慢外部调用不再阻塞推进线程。
    WaitingAsync,
    /// 事件网关等待（A10）：停在 `eventBasedGateway`，等所有出边后继事件中第一个触发的到来。
    ///
    /// 引擎为所有后继节点注册竞争事件（TimerJob / MessageSubscription）；第一个触发时令牌从
    /// 网关节点前进到胜出后继，其余竞争事件全部撤销。此状态在 cancel/terminate 时与
    /// WaitingMessage/WaitingTimer 同等处理（清理所有关联竞争事件）。
    WaitingEventGateway,
    /// 异常挂起（H2 Incident）：serviceTask/delegate 执行失败且重试耗尽——令牌停在故障节点，
    /// 实例**不丢失、不终止**，等待人工介入 `retry_incident` 重试或改数据后恢复。失败原因与
    /// 已重试次数记在实例变量 `__incident`。这是生产可用性的关键：失败可见、可恢复，而非静默丢。
    Incident,
    /// 已结束：抵达 endEvent，等待实例收尾。
    Ended,
}

/// 一个令牌：指向「当前所在节点」的执行指针。
///
/// 位置用 `node_bpmn_id`（稳定锚点）记录，不用 arena 下标——跨进程恢复必须稳定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Token {
    /// 令牌唯一 id。
    pub id: String,
    /// 所属流程实例 id。
    pub instance_id: String,
    /// 当前所在节点的 bpmn_id。
    pub node_bpmn_id: String,
    /// 令牌状态。
    pub state: TokenState,
    /// 父令牌 id（并行网关分裂时用；M1 恒为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 一个流程实例。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInstance {
    /// 实例唯一 id。
    pub id: String,
    /// 流程定义 key。
    pub definition_key: String,
    /// 业务键（对接业务单据，可空；对齐 Flowable businessKey）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_key: Option<String>,
    /// 实例状态。
    pub state: InstanceState,
    /// 实例级变量。
    pub variables: Variables,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近更新时间。
    pub updated_at: DateTime<Utc>,
    /// 完成时间（完成/终止时置位）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// 所属组织（M5.2 子流程组织路由的依据；M5.1 恒为 None，向后兼容）。
    /// RD0 起是 `dimensions["org"]` 的快捷别名/兼容投影（见 [`Self::dimensions`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    /// 维度上下文（RD0/RD3）：实例在各路由维度上的取值，`dim_key → dim_value`。
    /// 子流程按挂载点的 `dim_key` 从此取维度值路由。`"org"` 维度缺省回退 `org_id`（向后兼容）。
    /// 一个实例可同时携带多个维度取值（如 `{"org":"df_bj","legal_entity":"LE_CN"}`），
    /// 支持「同实例内挂载 A 按组织、挂载 B 按法人」各走不同维度。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub dimensions: std::collections::BTreeMap<String, String>,
    /// 父实例 id（M5：子实例指向调用它的主实例；主实例/顶层实例为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_instance_id: Option<String>,
    /// 父实例中挂起等待的令牌 id（M5：子实例完成时据此精确唤醒主令牌）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_token_id: Option<String>,
    /// 父实例中发起本子实例的 callActivity 节点 bpmn id（M5.3：多挂载去重键）。
    /// 同一令牌可串行经过多个 callActivity，仅凭 parent_token_id 无法区分是哪一挂载点启动的，
    /// 故补记节点。去重键 = (parent_token_id, parent_node_bpmn_id)。M5.1/5.2 单挂载恒可为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node_bpmn_id: Option<String>,
}

/// 用户任务（等待态节点的外化产物）。
///
/// 令牌停在 userTask 时创建一条 Task；外部 complete 该 Task → 令牌恢复推进。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// 任务唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 产生该任务的令牌 id。
    pub token_id: String,
    /// 对应 userTask 节点的 bpmn_id。
    pub node_bpmn_id: String,
    /// 任务名（取自节点 name）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 办理人。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// 候选组。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_groups: Option<String>,
    /// 多实例子任务携带的「当前元素」值（会签每人各自的数据；单实例任务为 None）。M3 新增。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_value: Option<serde_json::Value>,
    /// 任务所有者（M4.3）。委派时 owner ≠ assignee（owner 是原主，assignee 是代理人）。
    /// None = owner 即 assignee（常规任务，与 M1~M4.2 行为一致，零回归）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_user_id: Option<String>,
    /// 父任务 id（M4.3 加签）。加签产生的临时任务挂在原任务下；办结临时任务后回到父任务。
    /// None = 主任务（非加签临时任务）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// 转签状态（M4.3）：None=常规 / "DELEGATED"=委派中 / "ADDSIGN"=加签临时任务 /
    /// "SUSPENDED"=因子任务加签被挂起的父任务（待子任务回归）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_state: Option<String>,
    /// 任务状态：true = 已办结。
    pub completed: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 办结时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// 任务候选人（M4.1）—— 候选人表达式解析出「多人候选」时的一条候选记录。
///
/// 语义对齐 Flowable 的 candidate/claim：任务解析出多人时不直派，落若干 TaskCandidate；
/// 任一候选人 `claim`（认领）后，任务 assignee 被写为该人，其余候选记录随之作废/保留供审计。
/// 单人直派任务不产生候选记录（assignee 直接置位，走 M1 老路）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCandidate {
    /// 候选记录唯一 id。
    pub id: String,
    /// 所属任务 id。
    pub task_id: String,
    /// 所属实例 id（冗余，便于「我的待办」跨实例查询）。
    pub instance_id: String,
    /// 候选来源类型（记录解析自哪种引用，便于审计与展示）。
    pub candidate_type: CandidateKind,
    /// 候选引用原值（role code / position code / org id / user id）。
    pub candidate_ref: String,
    /// 解析出的具体用户 id（供「我的待办」按用户查）。
    pub resolved_user_id: String,
}

/// 抄送记录（M4.2）—— 把流程进展只读知会给一个用户。
///
/// 抄送是流程的旁路投影：**不阻塞流程、不产生待办、不影响令牌推进**。每个被抄送人一条。
/// 带已读追踪（read_at）。随 `InstanceSnapshot` 持久化，实例终态时随聚合归档。触发方式：
/// - 节点配置抄送：userTask 上配 `cmx:cc` 表达式，任务办结时对该节点抄送。
/// - 手动抄送：办理人办结时附带抄送名单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcRecord {
    /// 抄送记录唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 抄送发生的节点 bpmn_id（哪个环节触发的知会）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_bpmn_id: Option<String>,
    /// 被抄送人 user id。
    pub to_user_id: String,
    /// 抄送发起人 user id（手动抄送时记录办理人；节点自动抄送可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<String>,
    /// 抄送说明（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 已读时刻（None = 未读）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    /// 抄送时刻。
    pub created_at: DateTime<Utc>,
}

/// 抄送摘要（M4.2）—— 「抄送我的」列表的轻量视图，跨实例查询用。
///
/// 由 `RuntimeStore::find_cc_for_user` 返回：只带展示所需字段 + 实例业务键，不载完整聚合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CcSummary {
    /// 抄送记录 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 实例业务键（列表展示用，可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_key: Option<String>,
    /// 流程定义 key。
    pub definition_key: String,
    /// 抄送发生的节点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_bpmn_id: Option<String>,
    /// 抄送说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 是否已读。
    pub read: bool,
    /// 抄送时刻。
    pub created_at: DateTime<Utc>,
}

/// 转签台账（M4.3）—— 转办 / 加签 / 委派的一条流转记录，可追溯完整流转链。
///
/// 三种动作统一记账：谁在什么任务上把它转给谁、为什么。加签会关联临时任务 id。
/// 这是审计与展示核心（前端画「张三→李四→王五」的流转轨迹）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDelegation {
    /// 台账记录唯一 id。
    pub id: String,
    /// 被操作的任务 id。
    pub task_id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 动作类型：TRANSFER（转办）/ ADDSIGN_BEFORE（向前加签）/ ADDSIGN_AFTER（向后加签）/
    /// DELEGATE（委派）/ RESOLVE（委派办结回归 owner，可选记录）。
    pub kind: String,
    /// 操作发起人（原办理人）user id。
    pub from_user_id: String,
    /// 目标人 user id。
    pub to_user_id: String,
    /// 加签产生的临时任务 id（转办/委派为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_task_id: Option<String>,
    /// 转签意见（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 操作时刻。
    pub created_at: DateTime<Utc>,
}
///
/// 会签/或签把「一个逻辑节点」展开成「多个并发/顺序子任务」，需要一个域来记账：
/// 总数、已完成数、顺序游标、完成条件。它随 `InstanceSnapshot` 一起持久化，故重启后
/// 仍能正确求值 completionCondition（延续 M2/b「重启从库恢复」的保证）。
///
/// 归属判定：子任务/子令牌通过 `node_bpmn_id` 关联到本域（同一实例内一个 MI 节点至多一个
/// 未完成域）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MiScope {
    /// 域唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 对应 multiInstance 节点的 bpmn_id。
    pub node_bpmn_id: String,
    /// true = 顺序（或签）；false = 并行（会签）。
    pub sequential: bool,
    /// 展开的子实例总数（nrOfInstances）。
    pub total: usize,
    /// 已办结的子实例数（nrOfCompletedInstances）。
    pub completed: usize,
    /// 顺序模式的下一个待展开元素下标；并行模式恒等于 total。
    pub next_index: usize,
    /// 展开用的元素快照（从 collection 变量取值时定格，避免中途变量被改动引发不一致）。
    pub collection: Vec<serde_json::Value>,
    /// 子任务携带当前元素的变量名（elementVariable）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_var: Option<String>,
    /// 完成条件表达式（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_condition: Option<String>,
    /// 本域是否已收口（completionCondition 命中或自然全部完成）。收口后不再展开/等待。
    #[serde(default)]
    pub finished: bool,
}

impl MiScope {
    /// 当前活动（未办结）子实例数（nrOfActiveInstances）。
    ///
    /// 并行：total - completed；顺序：收口前恒为 1（同时只有一个在办），收口后为 0。
    pub fn active_count(&self) -> usize {
        if self.finished {
            0
        } else if self.sequential {
            // 顺序模式：还有未展开或在办的元素即为 1。
            usize::from(self.completed < self.total)
        } else {
            self.total.saturating_sub(self.completed)
        }
    }
}

/// 实例聚合快照 —— 持久化与恢复的**原子单元**。
///
/// 设计核心：把「实例 + 其全部令牌 + 其未办结任务」当成一个聚合根一起 load/save。
/// 每个「运行段」= 载入快照 → 内存推进到等待态/结束 → 一次 `save_snapshot` 落库。
/// 这让 BPMN 的「等待态即提交点」**恰好**对应一次 DB 事务，原子且无中间态泄漏。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    /// 实例本体。
    pub instance: ProcessInstance,
    /// 该实例的全部令牌。
    pub tokens: Vec<Token>,
    /// 该实例当前**未办结**的任务（已办结的进历史，M1 简化为仍保留在表中）。
    pub tasks: Vec<Task>,
    /// 该实例当前的多实例执行域（会签/或签账本）。M3 新增，默认空——既有 M1/M2 快照
    /// 反序列化时该字段缺省，向后兼容。
    #[serde(default)]
    pub mi_scopes: Vec<MiScope>,
    /// 该实例当前挂起的定时器作业（边界定时器到期表）。M2.5 新增，默认空，向后兼容。
    #[serde(default)]
    pub jobs: Vec<TimerJob>,
    /// 该实例当前挂起的异步服务任务作业（P1）。默认空，向后兼容。
    #[serde(default)]
    pub async_jobs: Vec<AsyncJob>,
    /// 该实例当前的任务候选人（多人候选待认领）。M4.1 新增，默认空，向后兼容。
    #[serde(default)]
    pub candidates: Vec<TaskCandidate>,
    /// 该实例的抄送记录（只读知会 + 已读追踪）。M4.2 新增，默认空，向后兼容。
    #[serde(default)]
    pub cc_records: Vec<CcRecord>,
    /// 该实例的转签台账（转办/加签/委派流转链）。M4.3 新增，默认空，向后兼容。
    #[serde(default)]
    pub delegations: Vec<TaskDelegation>,
    /// 本次推进段待写入的消息订阅（P3 + A2）。**不持久化**（serde skip）：
    /// `run_to_wait` 把新增的 Catch 订阅追加到此，调用方在 `save_snapshot` 后批量 `upsert_message_subscription`。
    /// 重启后由订阅表重建，此字段仅为推进段内暂存，不参与快照序列化。
    #[serde(skip)]
    pub pending_subs: Vec<MessageSubscription>,
    /// 本次推进段闭合的活动历史记录（A6）。**不持久化**（serde skip）：令牌离开节点时
    /// `transition_token` 把闭合的 ActivityRecord 追加到此，调用方在 `save_snapshot` 后批量
    /// `upsert_hi_activity`。与 pending_subs 同构——推进段内暂存，落库后即弃，不参与快照序列化。
    #[serde(skip)]
    pub pending_activities: Vec<ActivityRecord>,
}

impl InstanceSnapshot {
    /// 取指定 id 的活动/等待令牌（可变）。
    pub fn token_mut(&mut self, token_id: &str) -> Option<&mut Token> {
        self.tokens.iter_mut().find(|t| t.id == token_id)
    }

    /// 取绑定到指定令牌的未办结任务（可变）。
    pub fn open_task_of_token_mut(&mut self, token_id: &str) -> Option<&mut Task> {
        self.tasks
            .iter_mut()
            .find(|t| t.token_id == token_id && !t.completed)
    }

    /// 是否还有可推进（Active）的令牌。
    pub fn has_active_token(&self) -> bool {
        self.tokens.iter().any(|t| t.state == TokenState::Active)
    }

    /// 取某 MI 节点当前**未收口**的执行域（可变）。一个实例内一个 MI 节点至多一个活动域。
    pub fn open_mi_scope_mut(&mut self, node_bpmn_id: &str) -> Option<&mut MiScope> {
        self.mi_scopes
            .iter_mut()
            .find(|s| s.node_bpmn_id == node_bpmn_id && !s.finished)
    }
}

/// 实例列表摘要 —— 轻量视图，用于列表/看板，不含令牌与任务明细。
///
/// 由 `RuntimeStore::list_instances` 返回。相较 `InstanceSnapshot`，它只带实例级字段
/// + 未办结任务数，避免为列出 N 个实例而载入 N 份完整聚合（列表页的常见性能陷阱）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceSummary {
    /// 实例 id。
    pub id: String,
    /// 流程定义 key。
    pub definition_key: String,
    /// 业务键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_key: Option<String>,
    /// 实例状态。
    pub state: InstanceState,
    /// 实例级变量（列表页常需展示申请人/金额等，直接带上）。
    pub variables: Variables,
    /// 未办结任务数。
    pub open_task_count: usize,
    /// 创建时间（列表默认按此倒序）。
    pub created_at: DateTime<Utc>,
    /// 最近更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 定时器作业的类型 —— 触发时令牌怎么走（M2.5 边界 / A1 中间捕获）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TimerJobKind {
    /// 边界定时器（M2.5）：触发时令牌改走 `boundary_bpmn_id` 边界事件节点（中断型重定位宿主令牌，
    /// 非中断型发旁路令牌）。默认值，保证既有快照反序列化为此类型（向后兼容）。
    #[default]
    Boundary,
    /// 中间定时捕获（A1）：令牌停在 `intermediateCatchEvent` 节点等待（WaitingTimer），触发时
    /// 就地转 Active 沿该节点唯一出边前进（不重定位到别的节点）。`boundary_bpmn_id` 存本节点自身
    /// bpmn_id（仅供追溯）。
    IntermediateCatch,
}

impl TimerJobKind {
    /// 是否为边界类型（serde skip 用：默认值不写盘，保持既有快照字节一致）。
    pub fn is_boundary(&self) -> bool {
        matches!(self, TimerJobKind::Boundary)
    }
}

/// 定时器作业 —— 一个「到期待触发」的定时器实例（M2.5 边界 / A1 中间捕获）。
///
/// 令牌到达挂有边界定时器的 userTask（或中间定时捕获事件）时建一条 TimerJob，记 `due_at`。
/// 外部（引擎的 trigger_due_timers）在 `now >= due_at` 时触发它。随聚合快照持久化，重启后不丢。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerJob {
    /// 作业唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 挂载该定时器的令牌 id（停在宿主 userTask / 中间捕获节点的令牌）。令牌离开即撤销本作业。
    pub token_id: String,
    /// 边界类型：触发时令牌要去的边界事件节点 bpmn_id。中间捕获类型：本捕获节点自身 bpmn_id（追溯用）。
    pub boundary_bpmn_id: String,
    /// 是否中断型（边界类型冗余存，触发时免回查定义；中间捕获恒 false）。
    pub cancel_activity: bool,
    /// 到期时刻（宿主到达时刻 + 时长，或绝对时刻）。
    pub due_at: DateTime<Utc>,
    /// 作业创建时间。
    pub created_at: DateTime<Utc>,
    /// 作业类型（A1）。默认 Boundary，既有快照反序列化不变。
    #[serde(default, skip_serializing_if = "TimerJobKind::is_boundary")]
    pub kind: TimerJobKind,
    /// 周期定时器（A5 timeCycle）：每次间隔秒数。None = 非周期（触发一次即止）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_interval_seconds: Option<i64>,
    /// 周期定时器剩余重复次数。None = 无界（引擎按上限封顶）；Some(0) = 用尽不再重发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_remaining: Option<u32>,
}

/// 异步服务任务作业（P1）：serviceTask `flowable:async="true"` 触发，令牌停为 WaitingAsync。
///
/// 集群安全：worker 通过 `acquire_async_jobs`（SKIP LOCKED）抢占执行；
/// 完成后调 `complete_async_job` 把令牌转 Active 继续推进。
///
/// A7 外部 Worker：`topic` 非空的作业由**外部进程**（多语言 worker）按 topic 拉取执行，
/// `delegate_key` 为空；`topic` 为空的作业由**进程内** poller 执行注册的 delegate。两者靠
/// `acquire_async_jobs` 的 topic 过滤天然隔离（poller 只取 topic IS NULL，外部 worker 按 topic）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsyncJob {
    /// 作业唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 宿主令牌 id（WaitingAsync 令牌）。
    pub token_id: String,
    /// 服务任务节点的 bpmn_id（delegate key 来源）。
    pub node_bpmn_id: String,
    /// delegate class/key（对应 flowable:class 或 flowable:expression）。外部 Worker 作业为空串。
    pub delegate_key: String,
    /// 外部 Worker 主题（A7，`flowable:type="external-worker"` + `flowable:topic`）。
    /// `Some` = 外部 worker 按此 topic 拉取执行（进程内 poller 不碰）；`None` = 进程内 delegate 执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// 重试次数上限（默认 3）。
    pub max_retries: i32,
    /// 已重试次数。
    pub retries: i32,
    /// 重试退避（秒），None = 不退避。
    pub retry_backoff_seconds: Option<i64>,
    /// 锁定者（worker id）；None = 可抢占。SKIP LOCKED 集群安全依赖此列。
    pub locked_by: Option<String>,
    /// 锁定到期时间；超时后其它 worker 可抢占。
    pub lock_expires_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 活动历史记录（A6）：一次「令牌在某节点的停留」的完整时段——节点级审计/SLA 看板的原子行。
///
/// 与 `Task`/`hi_task` 的关系类比：**已闭合**的活动（令牌已离开该节点）落此表，正在进行的活动
/// 由活动令牌派生（token.node_bpmn_id + updated_at 即「当前在哪、何时进入」），不落表——与
/// 「未办结任务留 cmx_flow_task、办结才进 hi_task」同构。故一条节点访问的时序：令牌进入节点时
/// 记 enter（= token.updated_at），令牌离开时闭合成一条 ActivityRecord 写入。等待态节点
/// （userTask/WaitingAsync/WaitingTimer）的停留跨多个 run_to_wait 段，enter/exit 之差即真实
/// 等待时长——这是 SLA 分析的核心，快照差分法（只在终态归档）无法捕获。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityRecord {
    /// 记录唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 产生该活动的令牌 id（并行多令牌时区分各分支）。
    pub token_id: String,
    /// 节点 bpmn_id。
    pub activity_bpmn_id: String,
    /// 节点名（取自定义，冗余存便于报表免回查定义）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_name: Option<String>,
    /// 节点类型文本（startEvent/userTask/serviceTask/… 供按类型聚合）。
    pub activity_type: String,
    /// 进入时刻。
    pub entered_at: DateTime<Utc>,
    /// 离开时刻（闭合时置位）。
    pub exited_at: DateTime<Utc>,
    /// 停留时长毫秒（exited - entered，冗余存便于 SQL 聚合 SLA）。
    pub duration_ms: i64,
    /// 办理人（userTask 活动记 assignee，供按人聚合工作量；其它节点为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// 租户 id（多租户隔离；单租户 "default"）。
    pub tenant_id: String,
}

/// 死信作业（P2）：异步 Job 重试耗尽后的托底记录 —— 失败不丢，运维台可见可重试可删除。
///
/// 与 `AsyncJob` 的关系：async job 在 worker 侧 `fail_async_job` 重试到 0 时，**从抢占池
/// 删除**并在此表落一条死信；同时其宿主令牌转 `Incident`（两视图一致：实例卡在故障节点、
/// 死信表存作业级明细）。运维经 `retry_dead_letter_job` 把它重新投回抢占池（重建 AsyncJob +
/// 令牌回 WaitingAsync），或 `delete_dead_letter_job` 判定放弃。保留原 async job 的全部身份
/// 字段以便原样重建，另记最终失败原因与死信时刻供审计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadLetterJob {
    /// 死信记录唯一 id（复用原 AsyncJob.id，便于溯源与幂等）。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 原宿主令牌 id（重试时令牌从 Incident 回 WaitingAsync）。
    pub token_id: String,
    /// 服务任务节点 bpmn_id（重建 AsyncJob 的节点锚点）。
    pub node_bpmn_id: String,
    /// delegate class/key（重建 AsyncJob 用）。
    pub delegate_key: String,
    /// 原重试次数上限（重投时恢复为此值）。
    pub max_retries: i32,
    /// 最终失败原因（末次 delegate 返回的错误）。
    pub error: String,
    /// 原作业创建时刻（溯源）。
    pub original_created_at: DateTime<Utc>,
    /// 转入死信的时刻。
    pub dead_lettered_at: DateTime<Utc>,
    /// 租户 id（多租户隔离；单租户 "default"）。
    pub tenant_id: String,
}

/// 消息订阅 —— 持久化「谁在等哪条消息」，重启后不丢（P3 + A2）。
///
/// `kind = Catch`：实例里的令牌停在 `MessageCatchEvent`，等 `correlate_message` 唤醒。
/// 引擎在令牌变 `WaitingMessage` 时写入，`correlate_message` / 取消 / 终止时删除。
///
/// `kind = Start`：已部署的流程定义声明了消息启动事件（A2），等 `start_by_message` 触发。
/// 引擎在 `deploy` 时写入（扫描 startEvent 里的 messageEventDefinition），撤部署时删除。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageSubscription {
    /// 订阅唯一 id。
    pub id: String,
    /// 订阅类型。
    pub kind: MessageSubscriptionKind,
    /// 消息名（BPMN message name）。
    pub message_name: String,
    /// Catch 类型：所属实例 id；Start 类型：为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Catch 类型：等待中的令牌 id（correlate 按此精确唤醒）；Start 类型：为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// Catch 类型：等待节点 bpmn_id；Start 类型：startEvent bpmn_id。
    pub node_bpmn_id: String,
    /// 相关键变量名（`MessageCatch.correlation_var` / 消息启动事件 cmx:correlationVar）。None = 无相关键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_var: Option<String>,
    /// Start 类型：流程定义 key（用于按消息名 + tenant 找对应定义）；Catch 类型：为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_key: Option<String>,
    /// 租户 id（多租户隔离；单租户模式为 "default"）。
    pub tenant_id: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 消息订阅的类型：等待令牌 vs 启动事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageSubscriptionKind {
    /// 中间消息捕获：实例令牌等待外部消息唤醒（P3）。
    Catch,
    /// 消息启动：已部署定义等待消息触发发起新实例（A2）。
    Start,
}
///
/// 定时器推进器先用它跨实例查出所有到期作业，再按 instance_id 分组逐个 load→fire→save，
/// 避免为扫描到期作业而载入全部实例聚合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DueJob {
    /// 所属实例 id（据此 load 聚合）。
    pub instance_id: String,
    /// 作业 id。
    pub job_id: String,
    /// 到期时刻。
    pub due_at: DateTime<Utc>,
}
