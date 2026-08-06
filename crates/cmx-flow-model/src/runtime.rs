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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
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
    /// 该实例当前的任务候选人（多人候选待认领）。M4.1 新增，默认空，向后兼容。
    #[serde(default)]
    pub candidates: Vec<TaskCandidate>,
    /// 该实例的抄送记录（只读知会 + 已读追踪）。M4.2 新增，默认空，向后兼容。
    #[serde(default)]
    pub cc_records: Vec<CcRecord>,
    /// 该实例的转签台账（转办/加签/委派流转链）。M4.3 新增，默认空，向后兼容。
    #[serde(default)]
    pub delegations: Vec<TaskDelegation>,
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

/// 定时器作业 —— 一个「到期待触发」的边界定时器实例（M2.5）。
///
/// 令牌到达挂有边界定时器的 userTask 时，为每个边界定时器建一条 TimerJob，记 `due_at`。
/// 外部（引擎的 trigger_due_timers）在 `now >= due_at` 时触发它：令牌改走定时器出边
/// （中断型）或发旁路令牌（非中断型）。随聚合快照持久化，重启后定时器不丢。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerJob {
    /// 作业唯一 id。
    pub id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 挂载该定时器的令牌 id（停在宿主 userTask 的令牌）。令牌离开宿主即撤销本作业。
    pub token_id: String,
    /// 触发时令牌要去的边界事件节点 bpmn_id。
    pub boundary_bpmn_id: String,
    /// 是否中断型（冗余存，触发时免回查定义）。
    pub cancel_activity: bool,
    /// 到期时刻（宿主到达时刻 + 时长）。
    pub due_at: DateTime<Utc>,
    /// 作业创建时间。
    pub created_at: DateTime<Utc>,
}

/// 跨实例的「到期作业」轻量视图 —— `RuntimeStore::find_due_jobs` 返回。
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
