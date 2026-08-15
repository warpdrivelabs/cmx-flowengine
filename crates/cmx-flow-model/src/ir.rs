/*
 * @Describe: 编译后的流程定义 IR（Intermediate Representation）。
 *
 * 设计要点（Rust 化，不照抄 Java 引擎的继承树）：
 * 1. **arena + 索引**：所有节点存在 `Vec<FlowNode>`，节点间引用用 `NodeId`（下标的
 *    newtype），不用 `Rc<RefCell>` 指针，杜绝共享可变图。
 * 2. **enum dispatch 取代继承**：BPMN 元素类型是 `NodeKind` 枚举，不是 trait 对象层级。
 * 3. **bpmn_id 是稳定锚点**：令牌持久化时记 `bpmn_id`（字符串），不记 arena 下标——
 *    下标随编译顺序变化，跨进程/跨版本不稳定；bpmn_id 来自 XML，稳定可迁移。
 *
 * M1 支持子集：startEvent / endEvent / userTask / serviceTask / exclusiveGateway
 * + 带条件的 sequenceFlow。并行网关、边界事件、多实例留给 M2/M3。
 */

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{Error, Result};

/// 节点在 arena 中的稳定句柄（`ProcessDefinition::nodes` 的下标）。
///
/// 仅在**单个已编译的 ProcessDefinition 内**有效，不可跨定义、不可持久化。
/// 持久化请用节点的 `bpmn_id`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

/// BPMN 节点类型（enum dispatch，取代 Java 侧的 `ActivityBehavior` 继承体系）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// 开始事件：流程实例的唯一入口（M1 只支持无类型 none-start）。
    StartEvent,
    /// 结束事件：消费令牌；当实例再无活动令牌时实例结束。
    EndEvent,
    /// 用户任务：**等待态**节点。令牌到此即挂起并落库，等待外部 complete。
    UserTask(UserTask),
    /// 服务任务：由注册的 delegate 同步执行（M1 不落等待态），执行完继续前进。
    ServiceTask(ServiceTask),
    /// 排他网关：按出边条件择一前进（对齐 BPMN：命中第一个 true，否则走 default）。
    ExclusiveGateway,
    /// 并行网关（AND，BPMN parallelGateway）：
    /// - fork（出边 > 1）：为每条出边各产生一个子令牌，齐头并进。
    /// - join（入边 > 1）：等待所有入边的令牌到齐，合并成一个令牌再放行。
    ///
    /// 同一个网关既可 fork 又可 join（既有多入边又有多出边），引擎按到达令牌数判定。
    ParallelGateway,
    /// 包容网关（A1，BPMN inclusiveGateway，OR）：
    /// - fork：对每条出边求条件，**满足的都分裂令牌**（≥1；全不满足走 default）。介于排他
    ///   （择一）与并行（全分裂）之间——按条件择「若干」。审批高频（金额+类型同时命中多路）。
    /// - join：等所有**实际会到达**的分支令牌到齐再放行。比并行 join 难（到达数依 fork 时条件而定），
    ///   本引擎采用务实规则：本网关已到达(Joining)令牌 ≥1 且实例内**再无其它可能到达本网关的
    ///   非结束令牌**时放行（对无环审批流正确）。
    InclusiveGateway,
    /// 边界定时器事件（M2.5，BPMN boundaryEvent + timerEventDefinition）：
    /// 附着在某个 userTask 上，超时触发。它**不是**正常推进能到达的节点（无入边），
    /// 只有定时器到期时引擎把宿主令牌（中断型）或新令牌（非中断型）置于此节点，再沿其
    /// 唯一出边推进到「升级 / 催办」分支。
    BoundaryTimerEvent(BoundaryTimer),
    /// 调用活动（M5，BPMN callActivity）：调用一份**独立部署的子流程**并同步等待。
    ///
    /// 令牌到达时：解析子流程定义 → 启动子实例（父子关系）→ 主令牌转 WaitingSubflow 挂起。
    /// 子实例跑完（到 endEvent）→ 引擎回调 complete_subflow：回写变量 + 唤醒父令牌沿出边前进。
    /// M5.1 先支持写死的 calledElement（具体子流程 key）；M5.2 叠组织路由（calledKey 逻辑名）。
    CallActivity(CallActivity),
    /// 消息中间捕获事件（A4，BPMN intermediateCatchEvent + messageEventDefinition）：
    ///
    /// 令牌到达时**挂起等待外部消息**（WaitingMessage），由外部系统经 `correlate_message` 按
    /// 消息名 + 相关键唤醒。打通「等第三方审批结果/外部回调再继续」这类集成场景——审批流常见的
    /// 「发起后等对方系统裁决」。相关键取实例变量（`correlation_var`）的值。
    MessageCatchEvent(MessageCatch),
    /// 业务规则任务（A3，BPMN businessRuleTask + decisionRef）：调一张决策表求值，把输出写回
    /// 实例变量。审批矩阵剥离到表——「什么金额/部门 → 几级审批」不硬编码进流程图。
    /// 执行是同步的（决策纯函数、无 IO），不落等待态；求值完沿唯一出边继续（后续网关可用输出变量）。
    BusinessRuleTask(BusinessRule),
    /// 嵌入式子流程（A5，BPMN subProcess）——**透传节点**（编译期扁平化）。
    ///
    /// 引擎的节点 arena 是平铺的，故嵌入子流程在编译期被「摊平」进父图：subProcess 本身是一个
    /// 透传节点（令牌到达即沿唯一出边进入其内部 startEvent 的后继），其内部节点提升进父 arena，
    /// 内部 endEvent 被合成边接到 subProcess 的出口。这样无需引入嵌套令牌作用域即可支持基本嵌入
    /// 子流程。**已知限制**：附着在 subProcess 上的块级边界事件（整块超时/取消）需真正的嵌套
    /// 作用域，本轮不支持（编译期显式报错），列为后续增强。
    SubProcess,
    /// 终止结束事件（A2，BPMN endEvent + `<terminateEventDefinition>`）：一票否决。
    ///
    /// 与普通 EndEvent 只消费**本令牌**不同：令牌到达终止事件时，**杀掉本实例所有其它未结束
    /// 令牌 + 丢弃全部未办结任务**，实例立即 Completed。审批场景的「一票否决/否决即终止全流程」
    /// 标准建模——尤其配合并行会签/多分支：任一分支到达终止事件，整个实例收口。
    TerminateEndEvent,
}

/// 消息捕获事件的静态配置（A4，来自 BPMN intermediateCatchEvent + messageEventDefinition）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCatch {
    /// 消息名（对齐 BPMN `<message name=...>` 或 messageRef）。外部 correlate 时按此名匹配。
    pub message_name: String,
    /// 相关键变量名：实例变量里承载相关键的键名（如 "orderId"）。外部 correlate 传相关键值，
    /// 引擎找到「该变量值 == 相关键」且停在本消息事件的实例令牌唤醒。空 = 仅按消息名 + 实例 id 唤醒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_var: Option<String>,
}

/// 业务规则任务的静态配置（A3，来自 BPMN businessRuleTask + decisionRef）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessRule {
    /// 决策表 key：引擎按此键在决策表注册表查表，对当前变量求值，输出 merge 进实例变量。
    pub decision_key: String,
}

/// 调用活动的静态配置（来自 BPMN callActivity）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallActivity {
    /// 被调子流程的定义 key。M5.1：直接写死（对齐 BPMN `calledElement`）。
    /// M5.2 将新增 `called_key`（逻辑名）+ 组织路由，二选一。
    pub called_element: String,
    /// 逻辑 key（M5.2 组织路由用；M5.1 恒为 None，走 called_element 写死）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub called_key: Option<String>,
    /// 输入变量映射（主 → 子，启动子实例时拷贝）。空 = 全量传递主实例变量。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_vars: Vec<VarMapping>,
    /// 输出变量映射（子 → 主，子实例完成回归时拷贝）。空 = 全量回写子实例变量。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_vars: Vec<VarMapping>,
}

/// 一条变量映射：把 `source` 变量的值拷到 `target` 变量。source==target 即同名传递。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarMapping {
    /// 源变量名。
    pub source: String,
    /// 目标变量名。
    pub target: String,
}

/// 边界定时器的静态配置（来自 BPMN boundaryEvent）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryTimer {
    /// 宿主节点 bpmn_id（attachedToRef 指向的 userTask）。
    pub attached_to_bpmn_id: String,
    /// 触发时长（相对宿主任务到达时刻）。
    pub duration: TimerDuration,
    /// true = 中断型（cancelActivity）：超时中断宿主任务，令牌改走定时器出边。
    /// false = 非中断型：超时发一个旁路令牌（如催办），宿主任务不中断继续等。
    pub cancel_activity: bool,
}

/// 归一化后的定时器时长（ISO 8601 duration 解析结果，统一到秒）。
///
/// 只承载「相对时长」（timeDuration），不含绝对时刻（timeDate）与循环（timeCycle）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerDuration {
    /// 总秒数（`PT1H30M` → 5400）。
    pub seconds: i64,
}

impl NodeKind {
    /// 该节点类型是否为「等待态」——令牌到达后必须挂起、落库、等外部触发。
    ///
    /// 这是事务边界的判定核心：推进循环遇到等待态即停止并提交。
    /// 注意：并行网关的 join 是「结构性阻塞」（等兄弟令牌），不是等待态——它不落任务、
    /// 不需外部触发，在一次推进段内即可解除（当最后一个兄弟到达时）。
    pub fn is_wait_state(&self) -> bool {
        matches!(self, NodeKind::UserTask(_) | NodeKind::CallActivity(_))
    }
}

/// 用户任务的静态配置（来自 BPMN，实例无关）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserTask {
    /// 任务办理人（M1 为静态字符串；M4 起可为候选人表达式，见 candidates）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// 候选组（逗号分隔原样保留，M1 不做解析）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_groups: Option<String>,
    /// 多实例配置（None = 普通单实例任务；Some = 会签/或签）。M3 新增。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_instance: Option<MultiInstance>,
    /// 候选人来源（M4：解析后的结构化候选引用）。空 = 沿用 assignee 静态字符串（M1 老路）。
    ///
    /// 令牌到达时由引擎的 `AssigneeResolver` 把这些引用解析成真实用户集合：单人则直派
    /// （写 task.assignee），多人则落候选池待认领。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<CandidateRef>,
    /// 抄送来源（M4.2：节点级抄送表达式）。空 = 该节点不自动抄送。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<CandidateRef>,
    /// 绑定表单逻辑名（F2）。空 = 该环节无表单（纯审批按钮）。运行期由待办/表单宿主解析成具体页。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_key: Option<String>,
    /// 表单模式（F2）：edit | readonly | approve。空 = 默认 approve（审阅 + 意见）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_mode: Option<String>,
    /// 本环节可写字段白名单（F2 仅透传；F4 据此限制表单可编辑区）。空 = 不限制。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub form_fields: Vec<String>,
}

/// 一条候选人引用（候选人表达式的解析产物）。
///
/// BPMN 里写成 `role(finance), position(cfo), user(u_1001), org(d_fin)` 这样的表达式，
/// 编译期拆成若干 CandidateRef；运行期由 AssigneeResolver 逐条解析成用户 id 集合再并集。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRef {
    /// 引用类型：按人 / 按角色 / 按岗位 / 按部门。
    pub kind: CandidateKind,
    /// 引用值：user_id / role code / position code / org id。
    pub value: String,
}

/// 候选人引用的类型（正交于 IAM：user/role 复用现有表，position/org 为 M4 新建）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CandidateKind {
    /// 指定用户（user(id)）——直接就是一个用户 id。
    User,
    /// 按角色（role(code)）——复用 cmx_role / cmx_user_role 反查用户。
    Role,
    /// 按岗位（position(code)）——M4 新建 cmx_position / cmx_user_position 反查。
    Position,
    /// 按部门（org(id)）——M4 新建 cmx_org，取该部门（及子树）下的用户。
    Org,
    /// 部门领导（orgLeader(orgId?)）——P0 关系型。取该组织的 leader；orgId 省略时用实例所属组织
    /// （ResolveContext.org_id）。对应外接 IAM 的 cmx_org.leader_user_id。
    OrgLeader,
    /// 发起人本人（initiator）——P0 关系型。解析成 ResolveContext.initiator 单元素。value 忽略。
    Initiator,
    /// 发起人上级（initiatorLeader）——P0 关系型。取发起人所属组织的 leader（发起人的部门领导）。
    /// value 忽略；锚点是发起人而非实例组织。
    InitiatorLeader,
}

/// 多实例（multiInstance）静态配置——会签 / 或签的定义侧描述。
///
/// 对齐 BPMN `multiInstanceLoopCharacteristics`。M3 先支持 **collection 驱动**
/// （按一个数组变量的元素个数展开），不做 loopCardinality 纯数字循环——审批场景
/// 天然「按人展开」，collection 更贴切。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiInstance {
    /// true = 顺序（或签，逐个办理）；false = 并行（会签，齐头并进）。
    #[serde(default)]
    pub sequential: bool,
    /// 集合来源变量名：其值应为 JSON 数组，数组长度 = 展开的实例数。
    /// 对齐 Flowable `flowable:collection` / `<loopDataInputRef>`。
    pub collection_var: String,
    /// 每个子实例把「当前元素」写入这个变量名（对齐 Flowable `elementVariable`）。
    /// 用于让每个办理人任务携带各自的数据（如各自的审批人对象）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_var: Option<String>,
    /// 完成条件表达式（可空）。每次有子实例办结后求值；命中即**提前收口**剩余子实例。
    ///
    /// 求值时注入内置计数：`nrOfInstances` / `nrOfCompletedInstances` /
    /// `nrOfActiveInstances`。例：`${nrOfCompletedInstances/nrOfInstances >= 0.5}`（过半）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_condition: Option<String>,
}

/// 服务任务的静态配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTask {
    /// delegate 键：引擎按此键在 delegate 注册表查执行体。
    ///
    /// 对齐 Flowable 的 `flowable:delegateExpression` / `class` 思路，但 M1 收敛成
    /// 一个「注册表键」，不引入 EL 反射——Rust 侧用显式注册取代 Java 的类加载。
    pub delegate: String,
}

/// 一条节点（arena 元素）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowNode {
    /// 该节点在 arena 中的 id（冗余存一份，便于从 &FlowNode 反查）。
    pub id: NodeId,
    /// BPMN 原始 id（XML 的 `id` 属性），持久化 / 迁移的稳定锚点。
    pub bpmn_id: String,
    /// 展示名（XML 的 `name` 属性，可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 节点类型与其静态配置。
    pub kind: NodeKind,
    /// 入边数量（编译期从 sequenceFlow 统计）。并行网关 join 需据此判断令牌是否到齐。
    #[serde(default)]
    pub incoming_count: usize,
    /// 出边（有序）。排他网关按此顺序求值，命中即止；并行网关 fork 时全部分裂。
    pub outgoing: Vec<SequenceFlow>,
}

/// 一条顺序流（有向边 + 可选条件）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceFlow {
    /// 边的 BPMN id。
    pub bpmn_id: String,
    /// 目标节点。
    pub target: NodeId,
    /// 目标节点的 bpmn_id（冗余，日志/持久化友好）。
    pub target_bpmn_id: String,
    /// 条件表达式源文本（None = 无条件边，恒可走）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// 是否为 default flow（排他网关在所有条件均不满足时走它）。
    #[serde(default)]
    pub is_default: bool,
}

/// 一份已编译、不可变、可执行的流程定义。
///
/// 由 `cmx-flow-bpmn` 从 BPMN XML 编译产生；引擎只读地在其上跑令牌。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDefinition {
    /// 流程 key（BPMN `process` 的 id），用于按 key 部署/启动。
    pub key: String,
    /// 流程展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 节点 arena。
    pub nodes: Vec<FlowNode>,
    /// 唯一 startEvent 的 NodeId（M1 强约束单一开始事件）。
    pub start: NodeId,
    /// bpmn_id → NodeId 索引，供令牌恢复（持久化存 bpmn_id）与边解析。
    pub index: HashMap<String, NodeId>,
    /// 起点表单逻辑名（F4：process 级 cmx:startFormKey）。发起时渲染此表单填单。空 = 无起点表单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_form_key: Option<String>,
    /// 撤回策略（④：process 级 cmx:withdrawPolicy）。`"lenient"` = 只要未办结即可取回；
    /// 其它/空 = `strict`（下游完全未处理才可取回）。定义级可配。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdraw_policy: Option<String>,
    /// 变量声明（⑤：process 级 <cmx:varSchema> 里的设计态变量元信息）。None = 未声明（向后兼容）。
    /// 引擎运行不依赖它；供设计器下拉联动 + 可选的发起态默认值物化/软校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub var_schema: Option<crate::var_schema::VarSchema>,
    /// 变量校验策略（⑤：process 级 cmx:varValidation）。`"strict"` = 发起时违规即拒；
    /// `"off"` = 不校验；其它/空 = `lenient`（违规仅 warn，照常发起）。仅当 var_schema 存在时有意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub var_validation: Option<String>,
}

impl ProcessDefinition {
    /// 按 arena id 取节点。
    #[inline]
    pub fn node(&self, id: NodeId) -> &FlowNode {
        &self.nodes[id.0]
    }

    /// 按 bpmn_id 取节点（令牌恢复路径：持久化存的是 bpmn_id）。
    pub fn node_by_bpmn(&self, bpmn_id: &str) -> Option<&FlowNode> {
        self.index.get(bpmn_id).map(|id| self.node(*id))
    }

    /// 找出附着在指定宿主节点上的所有边界定时器事件（M2.5，图小线性扫）。
    pub fn boundary_timers_on(&self, host_bpmn_id: &str) -> Vec<&FlowNode> {
        self.nodes
            .iter()
            .filter(|n| match &n.kind {
                NodeKind::BoundaryTimerEvent(bt) => bt.attached_to_bpmn_id == host_bpmn_id,
                _ => false,
            })
            .collect()
    }

    /// 编译后自检：结构完整性校验。
    ///
    /// 在 IR 层挡住非法定义，让引擎的推进循环可以假设「结构永远合法」。
    pub fn validate(&self) -> Result<()> {
        if self.nodes.is_empty() {
            return Err(Error::InvalidDefinition("流程没有任何节点".into()));
        }
        // start 必须指向一个 StartEvent。
        match &self.node(self.start).kind {
            NodeKind::StartEvent => {}
            other => {
                return Err(Error::InvalidDefinition(format!(
                    "start 未指向 startEvent，实际为 {other:?}"
                )));
            }
        }
        // 每条出边的 target 必须落在 arena 内，且 bpmn_id 索引自洽。
        for node in &self.nodes {
            for flow in &node.outgoing {
                if flow.target.0 >= self.nodes.len() {
                    return Err(Error::InvalidDefinition(format!(
                        "节点 {} 的出边 {} 指向越界目标",
                        node.bpmn_id, flow.bpmn_id
                    )));
                }
            }
            // 排他网关：至多一条 default，且若有多出边应尽量带条件（仅告警，不拦）。
            if matches!(node.kind, NodeKind::ExclusiveGateway) {
                let defaults = node.outgoing.iter().filter(|f| f.is_default).count();
                if defaults > 1 {
                    return Err(Error::InvalidDefinition(format!(
                        "排他网关 {} 有多条 default 出边",
                        node.bpmn_id
                    )));
                }
            }
            // endEvent 不应有出边。
            if matches!(node.kind, NodeKind::EndEvent | NodeKind::TerminateEndEvent)
                && !node.outgoing.is_empty()
            {
                return Err(Error::InvalidDefinition(format!(
                    "结束事件 {} 不应有出边",
                    node.bpmn_id
                )));
            }
        }
        Ok(())
    }
}
