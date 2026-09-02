/*
 * @Describe: 令牌执行内核 —— M1 的心脏。
 *
 * 不变量（BPMN 语义 + 「等待态即提交点」）：
 * - 令牌是流经流程图的执行指针，`node_bpmn_id` 记录其当前占据的节点。
 * - `run_to_wait` 反复拾取 Active 令牌并「执行其所在节点」，直到没有 Active 令牌：
 *     StartEvent      → 直接沿唯一出边离开
 *     ServiceTask     → 调 delegate（同步执行副作用/写变量），再沿唯一出边离开
 *     ExclusiveGateway→ 按出边条件择一离开（命中第一个 true；否则走 default）
 *     UserTask        → **等待态**：到达即建任务、令牌转 Waiting、停止推进（提交点）
 *     EndEvent        → 消费令牌、转 Ended
 * - 一个「运行段」= 载入聚合 → run_to_wait → 一次 store 落库。等待态恰好对应事务边界。
 *
 * 令牌移动只依赖 SequenceFlow 上冗余存的 `target_bpmn_id`（稳定锚点），不碰 arena 下标，
 * 故恢复后的推进与首次推进走同一套代码。
 */

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use cmx_flow_model::{
    AssigneeResolver, CandidateKind, CcRecord, DecisionTable, InstanceSnapshot, InstanceState,
    MessageSubscription, MessageSubscriptionKind, MiScope, NodeKind, ProcessDefinition,
    ProcessInstance, RuntimeStore, SequenceFlow, Task, TaskCandidate, TaskDelegation, TimerJob,
    TimerJobKind, TimerSpec, Token, TokenState, UserTask, Variables,
    decision::evaluate as evaluate_decision, expr::eval_condition,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::clock::{Clock, SystemClock};
use crate::delegate::{DelegateContext, DelegateError, DelegateRegistry, JavaDelegate};
use crate::error::{Error, Result};

/// 推进步数安全上限：防止定义中存在「无等待态环路」时死循环。
const STEP_LIMIT: usize = 10_000;

/// 一次推进后的对外结果视图。
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// 实例 id。
    pub instance_id: String,
    /// 实例状态。
    pub state: InstanceState,
    /// 当前未办结任务视图。
    pub open_tasks: Vec<TaskView>,
}

/// 任务的对外只读视图。
#[derive(Debug, Clone)]
pub struct TaskView {
    /// 任务 id。
    pub id: String,
    /// 对应 userTask 节点 bpmn_id。
    pub node_bpmn_id: String,
    /// 任务名。
    pub name: Option<String>,
    /// 办理人。
    pub assignee: Option<String>,
}

/// 退回目标：某任务可退回的一个上游用户任务（reject_targets 返回，供前端渲染选择器）。
#[derive(Debug, Clone)]
pub struct RejectTarget {
    /// 目标节点 bpmn_id。
    pub bpmn_id: String,
    /// 目标节点名。
    pub name: Option<String>,
    /// 是否为默认退回目标（reject_task 省略 target 时会去的那个唯一直接前驱）。
    pub is_direct_predecessor: bool,
    /// 反向跳数（供前端「上 1 步 / 上 2 步」排序）。
    pub distance: usize,
}

/// 一个任务的退回目标全集（reject_targets 返回）。
#[derive(Debug, Clone)]
pub struct RejectTargetInfo {
    /// 任务 id。
    pub task_id: String,
    /// 任务当前所在节点 bpmn_id。
    pub current_node: String,
    /// 是否可退回（会签/或签域内任务、或无上游用户任务 → false）。
    pub rejectable: bool,
    /// 默认退回目标 bpmn_id（None = 无唯一直接前驱，须显式选一个）。
    pub default_target: Option<String>,
    /// 全部可退上游用户任务，按 distance 升序。
    pub targets: Vec<RejectTarget>,
}

/// 一次定时器触发的结果（trigger_due_timers 返回，供日志/demo 展示）。
#[derive(Debug, Clone)]
pub struct FiredTimer {
    /// 触发的作业 id。
    pub job_id: String,
    /// 所属实例 id。
    pub instance_id: String,
    /// 触发的边界事件节点 bpmn_id。
    pub boundary_bpmn_id: String,
    /// 是否中断型。
    pub cancel_activity: bool,
    /// 触发后实例状态。
    pub instance_state: InstanceState,
}

/// 流程引擎。泛型于 RuntimeStore：测试用 InMemoryStore，生产用 PG 实现。
/// 发起实例的完整上下文（v2.4：原 start_process_inner 9 个位置参数收拢，方案 §3.3）。
///
/// `subscriber_id` = L2 发起绑定的订阅 id（发起方向 handler 解析 name → id 后传入；与实例
/// 落库同一事务写入，方案 §3.4）；子流程经 launch_one_subflow 复制父实例值（§3.2 继承）。
#[derive(Debug, Clone, Default)]
pub struct StartContext {
    /// 流程定义 key。
    pub definition_key: String,
    /// 初始变量。
    pub variables: Variables,
    /// 业务键（可空）。
    pub business_key: Option<String>,
    /// 所属组织（可空；`dimensions["org"]` 的兼容投影）。
    pub org_id: Option<String>,
    /// 路由维度上下文（RD3）。
    pub dimensions: std::collections::BTreeMap<String, String>,
    /// 发起绑定的 webhook 订阅 id（L2；None = 未绑定走规则 2 全量匹配）。
    pub subscriber_id: Option<i64>,
    /// 发起方业务系统归属（技术债 005；来自结构化 key 声明的 TenantCtx.system，003）。
    /// None = legacy 调用，归属过滤放行。
    pub system_id: Option<String>,
    /// 父实例 id（子流程）。
    pub parent_instance_id: Option<String>,
    /// 父令牌 id（子流程）。
    pub parent_token_id: Option<String>,
    /// 父发起节点 bpmn id（子流程）。
    pub parent_node_bpmn_id: Option<String>,
}

pub struct Engine<S: RuntimeStore> {
    store: S,
    /// 已部署定义。用 `RwLock` 内部可变：`deploy` 取 `&self`，故 `Arc<Engine>` 之后仍可**热装载**
    /// 新发布的定义（H1：发布即生效，无需重启）。读多写极少，RwLock 合适。
    definitions: RwLock<HashMap<String, ProcessDefinition>>,
    delegates: DelegateRegistry,
    /// 决策表注册表（A3）：businessRuleTask 按 key 查表求值。RwLock 内部可变，Arc 后仍可注册。
    decisions: RwLock<HashMap<String, DecisionTable>>,
    /// 可注入时钟：生产 SystemClock，测试 TestClock（M2.5 定时器可测的关键）。
    clock: Arc<dyn Clock>,
    /// 候选人解析器（M4.1）：把角色/岗位/部门引用解析成真实用户。None = 未注入，
    /// 此时含候选引用的任务退回静态 assignee（宽容降级，不阻断）。
    resolver: Option<Arc<dyn AssigneeResolver>>,
    /// 子流程路由器（M5.2）：把 callActivity 的逻辑 key + 组织解析成具体子流程 key。
    /// None = 未注入，此时 callActivity 仅支持写死的 calledElement（M5.1 行为）。
    subflow_router: Option<Arc<dyn cmx_flow_model::SubflowRouter>>,
}

impl<S: RuntimeStore> Engine<S> {
    /// 用给定存储新建引擎（默认系统时钟，无候选人解析器）。
    pub fn new(store: S) -> Self {
        Self::with_clock(store, Arc::new(SystemClock))
    }

    /// 用给定存储 + 自定义时钟新建引擎。测试注入 TestClock 以确定性驱动定时器；
    /// 可控 demo 也可注入。
    pub fn with_clock(store: S, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            definitions: RwLock::new(HashMap::new()),
            delegates: DelegateRegistry::new(),
            decisions: RwLock::new(HashMap::new()),
            clock,
            resolver: None,
            subflow_router: None,
        }
    }

    /// 注入候选人解析器（M4.1）。生产接 cmx-iam 适配器，测试用假实现。
    pub fn set_resolver(&mut self, resolver: Arc<dyn AssigneeResolver>) {
        self.resolver = Some(resolver);
    }

    /// 注入子流程路由器（M5.2）。生产接 PgSubflowRouter（查绑定表 + 组织继承），测试用假实现。
    pub fn set_subflow_router(&mut self, router: Arc<dyn cmx_flow_model::SubflowRouter>) {
        self.subflow_router = Some(router);
    }

    /// 解析候选引用为用户 id 集合（并集去重）。无 resolver 或无引用 → 空 Vec（调用方降级）。
    ///
    /// `ctx` 携带 initiator/org，供关系型候选（部门领导/发起人上级/本人）解析；非关系型忽略它。
    async fn resolve_candidates(
        &self,
        candidates: &[cmx_flow_model::CandidateRef],
        ctx: &cmx_flow_model::ResolveContext,
    ) -> Result<Vec<String>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        match &self.resolver {
            Some(r) => r.resolve_all_with(candidates, ctx).await.map_err(Error::from),
            None => {
                tracing::warn!(
                    count = candidates.len(),
                    "userTask 含候选引用但未注入 AssigneeResolver，退回静态 assignee"
                );
                Ok(Vec::new())
            }
        }
    }

    /// 从实例快照构建解析上下文：initiator 取实例变量 `initiator`（约定），org 取实例 org_id。
    fn resolve_ctx_of(snapshot: &InstanceSnapshot) -> cmx_flow_model::ResolveContext {
        let initiator = snapshot
            .instance
            .variables
            .get("initiator")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        cmx_flow_model::ResolveContext::new(initiator, snapshot.instance.org_id.clone())
    }

    /// 展开一个多实例子实例（②）：按**当前元素**求值办理人后推入 token + task + 候选池。
    ///
    /// 支持三种写法（共用同一元素作用域求值）：
    ///   A 元素插值  `assignee="${product.ownerUser}"` → 直派；
    ///   B 候选表达式插值 `candidates="role(${product.ownerRole})"` → 解析(0 兜底/1 直派/≥2 候选池)；
    ///   C 集合即人  `assignee="${approver}"`（元素为用户 id 标量）→ 直派。
    ///
    /// 求值作用域 = 实例变量 + `elementVariable`(当前元素) + `loopCounter`(下标)。
    /// 借用纪律：先克隆出 scope/rctx/refs（owned）再 await 解析，await 期间不持 snapshot 读借用。
    async fn expand_mi_element(
        &self,
        snapshot: &mut InstanceSnapshot,
        node_bpmn: &str,
        node_name: &Option<String>,
        ut: &UserTask,
        element: Value,
        index: usize,
        now: DateTime<Utc>,
    ) -> Result<()> {
        // 1) 元素作用域：实例变量 + 元素变量 + loopCounter。
        let mut scope = snapshot.instance.variables.clone();
        if let Some(ev) = ut.multi_instance.as_ref().and_then(|mi| mi.element_var.as_ref()) {
            scope.set(ev.clone(), element.clone());
        }
        scope.set("loopCounter", json!(index));
        // 2) 解析上下文（读快照，得 owned）。
        let rctx = Self::resolve_ctx_of(snapshot);
        // 3) 插值静态 assignee（无 ${} 原样；求值空/无 assignee → None，视作「无此人」）。
        let static_assignee = match &ut.assignee {
            Some(a) => {
                let s = cmx_flow_model::interpolate(a, &scope).map_err(Error::from)?;
                if s.is_empty() { None } else { Some(s) }
            }
            None => None,
        };
        // 4) 逐条插值候选人 refs 的 value。
        let mut refs: Vec<cmx_flow_model::CandidateRef> = Vec::with_capacity(ut.candidates.len());
        for r in &ut.candidates {
            refs.push(cmx_flow_model::CandidateRef {
                kind: r.kind,
                value: cmx_flow_model::interpolate(&r.value, &scope).map_err(Error::from)?,
            });
        }
        // 5) 解析候选人（async；此刻不借 snapshot）。
        let resolved = self.resolve_candidates(&refs, &rctx).await?;
        // 6) 决定分派（0 兜底静态 assignee / 1 直派 / ≥2 候选池）。
        let task_id = Uuid::new_v4().to_string();
        let instance_id = snapshot.instance.id.clone();
        let (assignee, candidates) =
            decide_assignment(&task_id, &instance_id, &refs, &resolved, static_assignee);
        // 7) 改 snapshot：push token + task + 候选池。
        let token_id = Uuid::new_v4().to_string();
        snapshot.tokens.push(Token {
            id: token_id.clone(),
            instance_id: instance_id.clone(),
            node_bpmn_id: node_bpmn.to_string(),
            state: TokenState::Waiting,
            parent_id: None,
            created_at: now,
            updated_at: now,
        });
        snapshot.tasks.push(Task {
            id: task_id,
            instance_id,
            token_id,
            node_bpmn_id: node_bpmn.to_string(),
            name: node_name.clone(),
            assignee,
            candidate_groups: ut.candidate_groups.clone(),
            element_value: Some(element),
            owner_user_id: None,
            parent_task_id: None,
            delegation_state: None,
            completed: false,
            created_at: now,
            completed_at: None,
        });
        snapshot.candidates.extend(candidates);
        Ok(())
    }

    /// 部署一份流程定义。会做 M1 拓扑子集校验（引擎声明自己支持的形态）。
    ///
    /// **取 `&self`**（内部 RwLock）：故 `Arc<Engine>` 包好后仍可热装载新发布的定义（H1）。
    /// 同 key 覆盖（发布新版即替换运行时定义；已在跑的旧实例按各自快照+其定义 key 继续，
    /// 因定义按 key 存，覆盖后旧实例读到的是新版——审批场景可接受，实例迁移是后续 B1）。
    pub fn deploy(&self, def: ProcessDefinition) -> Result<()> {
        Self::check_topology(&def)?;
        tracing::info!(key = %def.key, nodes = def.nodes.len(), "部署流程定义");
        self.definitions
            .write()
            .expect("definitions 锁中毒")
            .insert(def.key.clone(), def);
        Ok(())
    }

    /// 部署并写入消息启动订阅（A2）。同步部署定义，再异步写入 `MessageStartEvent` 的 Start 订阅。
    ///
    /// 先删除旧订阅（幂等重部署），再逐个节点扫描写入。写入失败记 warn，不中断部署。
    pub async fn deploy_and_subscribe(
        &self,
        def: ProcessDefinition,
        tenant_id: &str,
    ) -> Result<()> {
        let key = def.key.clone();
        self.deploy(def.clone())?;
        // 删除旧的 Start 订阅（幂等重部署：先清后写）。
        if let Err(e) = self.store.delete_start_subscriptions_by_def(&key).await {
            tracing::warn!(key = %key, error = %e, "清理旧消息启动订阅失败，忽略继续");
        }
        // 扫描所有 MessageStartEvent 节点，建 Start 订阅。
        let now = self.clock.now();
        for node in &def.nodes {
            if let NodeKind::MessageStartEvent(ms) = &node.kind {
                let sub = MessageSubscription {
                    id: Uuid::new_v4().to_string(),
                    kind: MessageSubscriptionKind::Start,
                    message_name: ms.message_name.clone(),
                    instance_id: None,
                    token_id: None,
                    node_bpmn_id: node.bpmn_id.clone(),
                    correlation_var: ms.correlation_var.clone(),
                    definition_key: Some(key.clone()),
                    tenant_id: tenant_id.to_string(),
                    created_at: now,
                };
                if let Err(e) = self.store.upsert_message_subscription(&sub).await {
                    tracing::warn!(key = %key, node = %node.bpmn_id, error = %e, "写入消息启动订阅失败");
                }
            }
        }
        Ok(())
    }

    /// 按消息名触发一个新流程实例（A2 消息启动事件）。
    ///
    /// 在消息订阅表查找 `kind=Start + message_name + tenant_id` 的记录，取到 `definition_key`，
    /// 然后发起实例（等价于带 `MessageStartEvent` 节点的定义的 `start_process`）。
    /// `business_key` 和 `dimensions` 与普通发起相同。若无匹配订阅返回 `DefinitionNotFound` 错误。
    pub async fn start_by_message(
        &self,
        message_name: &str,
        tenant_id: &str,
        variables: Variables,
        business_key: Option<String>,
        dimensions: std::collections::BTreeMap<String, String>,
    ) -> Result<ExecutionResult> {
        let sub = self.store
            .find_start_subscription(message_name, tenant_id)
            .await?
            .ok_or_else(|| Error::DefinitionNotFound(format!("消息启动订阅不存在: {message_name}")))?;
        let def_key = sub.definition_key.ok_or_else(|| {
            Error::DefinitionNotFound(format!("消息启动订阅缺少 definition_key: {message_name}"))
        })?;
        let org_id = dimensions.get(cmx_flow_model::DIM_ORG).cloned();
        let snap = self
            .start_process_inner(StartContext {
                definition_key: def_key,
                variables,
                business_key,
                org_id,
                dimensions,
                subscriber_id: None,
                system_id: None,
                parent_instance_id: None,
                parent_token_id: None,
                parent_node_bpmn_id: None,
            })
            .await?;
        let snap = self.launch_subflows_for(&snap.instance.id).await?;
        self.flush_pending(&snap).await;
        Ok(Self::result_of(&snap))
    }

    /// 把 `snapshot.pending_subs` 逐条写入 store（非阻塞，失败只 warn）。
    ///
    /// 在每个可能产生 `WaitingMessage` 令牌的 save_snapshot 之后调用。
    /// `pending_subs` 字段不参与序列化，所以不会重复写入。
    async fn flush_pending_subs(&self, snap: &InstanceSnapshot) {
        for sub in &snap.pending_subs {
            if let Err(e) = self.store.upsert_message_subscription(sub).await {
                tracing::warn!(instance = %snap.instance.id, sub_id = %sub.id, error = %e, "写入消息订阅失败（非 fatal）");
            }
        }
    }

    /// 把 `snapshot.async_jobs` 逐条写入 store（非阻塞，失败只 warn）。
    ///
    /// 在每个可能产生 `WaitingAsync` 令牌的 save_snapshot 之后调用。
    async fn flush_pending_async_jobs(&self, snap: &InstanceSnapshot) {
        for job in &snap.async_jobs {
            if let Err(e) = self.store.upsert_async_job(job).await {
                tracing::warn!(instance = %snap.instance.id, job_id = %job.id, error = %e, "写入 AsyncJob 失败（非 fatal）");
            }
        }
    }

    /// 把 `snapshot.pending_activities`（本段闭合的活动历史 A6）逐条写入 store（非阻塞，失败只 warn）。
    ///
    /// 在每个 save_snapshot 之后调用，与 flush_pending_subs 同构。写库不阻塞主推进——活动历史是
    /// 审计/报表旁路，丢一条不影响流程正确性（但会在 SLA 看板留缺口，故记 warn）。
    async fn flush_pending_activities(&self, snap: &InstanceSnapshot) {
        for act in &snap.pending_activities {
            if let Err(e) = self.store.upsert_hi_activity(act).await {
                tracing::warn!(instance = %snap.instance.id, activity = %act.activity_bpmn_id, error = %e, "写入活动历史失败（非 fatal）");
            }
        }
    }

    /// 把 `snapshot.pending_var_changes`（本段引擎派生的变量变更：决策输出/子流程回填）批量写入
    /// store（非阻塞，失败只 warn）。与 flush_pending_activities 同构——变量历史是审计旁路，丢一条
    /// 不影响流程正确性。空则底层 record_var_changes 直接返回。
    async fn flush_pending_var_changes(&self, snap: &InstanceSnapshot) {
        if snap.pending_var_changes.is_empty() {
            return;
        }
        if let Err(e) = self.store.record_var_changes(&snap.pending_var_changes).await {
            tracing::warn!(instance = %snap.instance.id, error = %e, "写入引擎派生变量历史失败（非 fatal）");
        }
    }

    /// 一次性 flush 本推进段的三类旁路记录：消息订阅（P3/A2）+ 活动历史（A6）+ 派生变量历史。
    /// 三者恒作为一组、同序在每个 `save_snapshot` 之后写出——收进一个 helper 使调用点从 3 行降到 1 行，
    /// 并杜绝「漏 flush 其中一员」（`complete_subflow` 曾漏过）。异步 Job flush（P1）不在此组，
    /// 因它只在少数路径需要、且须在本组之后调（见调用点）。
    async fn flush_pending(&self, snap: &InstanceSnapshot) {
        self.flush_pending_subs(snap).await;
        self.flush_pending_activities(snap).await;
        self.flush_pending_var_changes(snap).await;
    }

    /// 克隆出一份已部署定义（读锁只在克隆期间持有，绝不跨 await）。未部署 → DefinitionNotFound。
    fn get_definition(&self, key: &str) -> Result<ProcessDefinition> {
        self.definitions
            .read()
            .expect("definitions 锁中毒")
            .get(key)
            .cloned()
            .ok_or_else(|| Error::DefinitionNotFound(key.to_string()))
    }

    /// 某 key 是否已部署（读锁瞬时持有）。
    fn has_definition(&self, key: &str) -> bool {
        self.definitions
            .read()
            .expect("definitions 锁中毒")
            .contains_key(key)
    }

    /// 注册一个 serviceTask delegate。
    pub fn register_delegate(
        &mut self,
        key: impl Into<String>,
        delegate: impl JavaDelegate + 'static,
    ) {
        self.delegates.register_delegate(key, delegate);
    }

    /// 注册一张决策表（A3）。**取 `&self`**（内部 RwLock），Arc 后仍可热注册；同 key 覆盖。
    pub fn register_decision(&self, table: DecisionTable) {
        self.decisions
            .write()
            .expect("decisions 锁中毒")
            .insert(table.key.clone(), table);
    }

    /// 克隆出一张已注册决策表（读锁瞬时持有，不跨 await）。未注册 → None。
    fn get_decision(&self, key: &str) -> Option<DecisionTable> {
        self.decisions
            .read()
            .expect("decisions 锁中毒")
            .get(key)
            .cloned()
    }

    /// 注销一张决策表（管理面删除时同步内存注册表；未注册则无操作）。
    pub fn unregister_decision(&self, key: &str) {
        self.decisions
            .write()
            .expect("decisions 锁中毒")
            .remove(key);
    }

    /// 借用底层存储（消费侧偶尔需要直接查询）。
    pub fn store(&self) -> &S {
        &self.store
    }

    /// M1 拓扑校验：确认定义只用了引擎支持的形态。
    ///
    /// 把「引擎支持哪个子集」这件事显式化，避免运行到一半才发现某节点出边数不合法。
    fn check_topology(def: &ProcessDefinition) -> Result<()> {
        for node in &def.nodes {
            let out = node.outgoing.len();
            let ok = match &node.kind {
                NodeKind::StartEvent => out == 1,
                NodeKind::EndEvent => out == 0,
                // 终止结束事件：同普通 endEvent，无出边。
                NodeKind::TerminateEndEvent => out == 0,
                NodeKind::UserTask(_) | NodeKind::ServiceTask(_) => out == 1,
                // 业务规则任务：求值决策表后沿唯一出边继续，需恰好一条出边。
                NodeKind::BusinessRuleTask(_) => out == 1,
                // 嵌入子流程透传节点（A5）：单出边（透传到内部 start / 内部 end 接子流程出口）。
                // 内部 endEvent 若无子流程出口则出边为 0（保持 EndEvent 语义）——放宽为 <= 1。
                NodeKind::SubProcess => out <= 1,
                // 排他网关：择一，至少一条出边。
                NodeKind::ExclusiveGateway => out >= 1,
                // 并行网关：作为 fork 需多出边，作为纯 join 出边可为 1；统一要求 >= 1。
                NodeKind::ParallelGateway => out >= 1,
                // 包容网关：同并行，>= 1 出边。
                NodeKind::InclusiveGateway => out >= 1,
                // 边界定时器事件：触发后沿唯一出边走升级/催办分支，需恰好一条出边。
                NodeKind::BoundaryTimerEvent(_) => out == 1,
                // 调用活动：子流程完成后沿唯一出边继续，需恰好一条出边。
                NodeKind::CallActivity(_) => out == 1,
                // 消息中间捕获事件：唤醒后沿唯一出边继续，需恰好一条出边。
                NodeKind::MessageCatchEvent(_) => out == 1,
                // 中间定时捕获事件（A1）：到期后沿唯一出边继续，需恰好一条出边。
                NodeKind::IntermediateTimerCatchEvent(_) => out == 1,
                // 消息启动事件（A2）：发起时作为入口，有唯一出边指向后继节点。
                NodeKind::MessageStartEvent(_) => out == 1,
                // 事件网关（A10）：至少两条出边（竞速的两个或以上的后继事件）。
                NodeKind::EventBasedGateway => out >= 2,
                // 边界错误事件（A8）：命中后沿唯一出边走错误处理分支，需恰好一条出边。
                NodeKind::BoundaryErrorEvent(_) => out == 1,
                // 事件子流程（A3）：透传标记节点，不在主流程路径上（无出边）。
                NodeKind::EventSubProcess => out == 0,
                // 错误启动事件（A3）：激活后沿唯一出边进入处理分支，需恰好一条出边。
                NodeKind::ErrorStartEvent(_) => out == 1,
            };
            if !ok {
                return Err(Error::UnsupportedTopology(format!(
                    "节点 {} ({:?}) 的出边数 {} 不符合 M1 约束",
                    node.bpmn_id, node.kind, out
                )));
            }
        }
        Ok(())
    }

    // ============================ 对外流程 API ============================

    /// 启动一个流程实例：建实例 + 起始令牌 → 推进到等待态/结束 → 落库。
    ///
    /// 返回结果含实例 id、终态、当前未办结任务。
    pub async fn start_process(
        &self,
        definition_key: &str,
        variables: Variables,
        business_key: Option<String>,
    ) -> Result<ExecutionResult> {
        self.start_process_org(definition_key, variables, business_key, None)
            .await
    }

    /// 启动一个流程实例并指定所属组织（M5.2）。顶层实例的组织决定其内 callActivity 子流程的
    /// 路由归属；子实例默认继承主实例组织。org=None 等价于 `start_process`。
    pub async fn start_process_org(
        &self,
        definition_key: &str,
        variables: Variables,
        business_key: Option<String>,
        org_id: Option<String>,
    ) -> Result<ExecutionResult> {
        self.start_process_dims(definition_key, variables, business_key, org_id, Default::default())
            .await
    }

    /// 启动一个流程实例并指定**维度上下文**（RD3）。顶层实例携带各路由维度的取值，其内不同挂载点的
    /// callActivity 按各自 `cmx:dimKey` 取对应维度值路由；子实例默认整体继承。`org_id` 作为 "org"
    /// 维度的兼容投影（若 dimensions 未含 "org" 则补入）。
    pub async fn start_process_dims(
        &self,
        definition_key: &str,
        variables: Variables,
        business_key: Option<String>,
        org_id: Option<String>,
        dimensions: std::collections::BTreeMap<String, String>,
    ) -> Result<ExecutionResult> {
        self.start_process_ctx(StartContext {
            definition_key: definition_key.to_string(),
            variables,
            business_key,
            org_id,
            dimensions,
            subscriber_id: None,
            system_id: None,
            parent_instance_id: None,
            parent_token_id: None,
            parent_node_bpmn_id: None,
        })
        .await
    }

    /// 启动一个流程实例（完整上下文形态，v2.4：支持发起绑定 subscriber_id；原 9 参
    /// start_process_inner 收拢为 StartContext——止住位置参数膨胀，方案 §3.3）。
    pub async fn start_process_ctx(&self, ctx: StartContext) -> Result<ExecutionResult> {
        let snap = self.start_process_inner(ctx).await?;
        // 若起始段就走到 callActivity，挂起了 WaitingSubflow 令牌，启动其子实例。
        let snap = self.launch_subflows_for(&snap.instance.id).await?;
        self.flush_pending(&snap).await;
        Ok(Self::result_of(&snap))
    }

    /// 启动实例（可指定组织与父实例链接）。顶层实例 parent 为 None；子流程由 callActivity
    /// 调用时传入 parent_instance/parent_token，建立父子关系。返回落库后的子实例快照。
    async fn start_process_inner(&self, ctx: StartContext) -> Result<InstanceSnapshot> {
        let StartContext {
            definition_key,
            variables,
            business_key,
            org_id,
            dimensions,
            subscriber_id,
            system_id,
            parent_instance_id,
            parent_token_id,
            parent_node_bpmn_id,
        } = ctx;
        let def = self.get_definition(&definition_key)?;

        // ⑤：按 var_schema 物化默认值 + 软校验（仅当声明了 schema）。运行核不看 schema，
        // 校验只在此发起边界做——strict 拒绝、lenient 仅 warn、off 跳过。
        let variables = match &def.var_schema {
            Some(schema) if !schema.is_empty() => {
                let filled = schema.materialize_defaults(&variables);
                let policy = def.var_validation.as_deref().unwrap_or("lenient");
                if policy != "off" {
                    let violations = schema.validate_values(&filled);
                    if !violations.is_empty() {
                        let msg = violations
                            .iter()
                            .map(|v| v.message.clone())
                            .collect::<Vec<_>>()
                            .join("; ");
                        if policy == "strict" {
                            return Err(Error::VarValidation(msg));
                        }
                        tracing::warn!(instance_def = %definition_key, violations = %msg, "变量软校验有违规（lenient，照常发起）");
                    }
                }
                filled
            }
            _ => variables,
        };

        let now = self.clock.now();
        let instance_id = Uuid::new_v4().to_string();
        let start_bpmn = def.node(def.start).bpmn_id.clone();

        // 维度上下文：以传入 dimensions 为基，org_id 存在但 dimensions 未含 "org" 时补进（兼容投影）。
        let mut dimensions = dimensions;
        if let Some(o) = &org_id {
            dimensions.entry(cmx_flow_model::DIM_ORG.to_string()).or_insert_with(|| o.clone());
        }

        let instance = ProcessInstance {
            id: instance_id.clone(),
            definition_key,
            business_key,
            state: InstanceState::Active,
            variables,
            created_at: now,
            updated_at: now,
            ended_at: None,
            org_id,
            dimensions,
            parent_instance_id,
            parent_token_id,
            parent_node_bpmn_id,
            // v2.4 L2 发起绑定：与实例落库同一事务写入；子流程继承见 launch_one_subflow。
            subscriber_id,
            // 005 系统归属：与实例落库同一事务写入（insert_instance 16 列）；子流程继承。
            system_id,
        };
        let token = Token {
            id: Uuid::new_v4().to_string(),
            instance_id: instance_id.clone(),
            node_bpmn_id: start_bpmn,
            state: TokenState::Active,
            parent_id: None,
            created_at: now,
            updated_at: now,
        };
        let mut snapshot = InstanceSnapshot {
            instance,
            tokens: vec![token],
            tasks: Vec::new(),
            mi_scopes: Vec::new(),
            jobs: Vec::new(),
            async_jobs: Vec::new(),
            candidates: Vec::new(),
            cc_records: Vec::new(),
            delegations: Vec::new(),
            // 007：新实例版本从 0 起（INSERT 直接落 0，save 时 CAS 期望 1）。
            version: 0,
            pending_subs: Vec::new(),
            pending_activities: Vec::new(),
            pending_var_changes: Vec::new(),
        };

        self.run_to_wait(&def, &mut snapshot, now).await?;
        self.store.create_snapshot(&snapshot).await?;
        // P3：flush 本次推进段产生的消息订阅（WaitingMessage 令牌的 Catch 订阅）。
        self.flush_pending(&snapshot).await;
        // P1：flush 本次推进段产生的异步 Job（WaitingAsync 令牌）。
        self.flush_pending_async_jobs(&snapshot).await;
        Ok(snapshot)
    }

    /// 为某实例中所有「已挂起但尚未启动子实例」的 callActivity 令牌启动子流程（M5）。
    ///
    /// 令牌进入 WaitingSubflow 后并不立即建子实例（避免与同步推进循环纠缠）；由本方法在
    /// 推进段结束后统一处理。子实例可能立即完成（子流程无等待态）→ 递归回写唤醒父令牌。
    /// 返回最终的父实例快照。用 Box::pin 显式装箱以支持递归 async。
    fn launch_subflows_for<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<InstanceSnapshot>> + Send + 'a>>
    {
        Box::pin(async move {
            loop {
                let snapshot = self.store.load_snapshot(instance_id).await?;
                // 找一个 WaitingSubflow 且还没有对应子实例的令牌。
                let pending = snapshot
                    .tokens
                    .iter()
                    .find(|t| t.state == TokenState::WaitingSubflow)
                    .map(|t| (t.id.clone(), t.node_bpmn_id.clone()));
                let Some((token_id, node_bpmn)) = pending else {
                    return Ok(snapshot);
                };
                // 已存在「同一令牌 + 同一 callActivity 节点」发起的子实例？（防重复启动）
                // 去重键必须含节点：同一令牌可串行经过多个 callActivity，仅凭 token 会把
                // 前一挂载点已完成的子实例误判成"本挂载点已启动"，导致后续挂载点漏起。
                let existing = self.store.find_child_instances(instance_id).await?;
                let already = existing.iter().any(|c| {
                    c.parent_token_id.as_deref() == Some(&token_id)
                        && c.parent_node_bpmn_id.as_deref() == Some(&node_bpmn)
                });
                if already {
                    // 本挂载点的子实例已在跑（等其完成回调）→ 找下一个尚未启动的挂载点令牌。
                    if let Some(next) = self.next_unlaunched_subflow(&snapshot, &existing) {
                        self.launch_one_subflow(&snapshot, &next.0, &next.1).await?;
                        continue;
                    }
                    return Ok(snapshot);
                }
                // 启动这一个子实例。
                self.launch_one_subflow(&snapshot, &token_id, &node_bpmn)
                    .await?;
                // 循环：可能子实例立即完成已唤醒父令牌，或还有别的待启动令牌。
            }
        })
    }

    /// 找一个「WaitingSubflow 但当前挂载点尚无子实例」的令牌。
    /// 去重键 = (令牌 id, callActivity 节点 bpmn id)：同一令牌串行经过多个 callActivity 时，
    /// 每个节点各算一个独立挂载点，前一节点已启动/已完成的子实例不阻塞后一节点。
    fn next_unlaunched_subflow(
        &self,
        snapshot: &InstanceSnapshot,
        existing_children: &[ProcessInstance],
    ) -> Option<(String, String)> {
        snapshot
            .tokens
            .iter()
            .filter(|t| t.state == TokenState::WaitingSubflow)
            .find(|t| {
                !existing_children.iter().any(|c| {
                    c.parent_token_id.as_deref() == Some(&t.id)
                        && c.parent_node_bpmn_id.as_deref() == Some(&t.node_bpmn_id)
                })
            })
            .map(|t| (t.id.clone(), t.node_bpmn_id.clone()))
    }

    /// 启动一个子实例：解析子流程定义 + 传入变量 → start_process_inner。若子实例立即完成，
    /// 递归调 complete_subflow 唤醒父令牌。
    ///
    /// 子流程解析失败（无组织绑定 / 目标未部署 / 未注入 router）**不再向上抛错**——否则父实例
    /// 已落库、令牌停在 WaitingSubflow，start 却返回 Err 且拿不到 id，导致「不可见、不可恢复的
    /// 僵尸实例」（Bug）。改为把该挂载点令牌停到 Incident 态并记 `__incident`（对齐 serviceTask
    /// 失败语义）：实例保持 Active、`hasIncident=true`、运维台可见；修好绑定后 `retry_incident`
    /// 重新激活令牌即可继续（retry 会把 Incident→Active，run_to_wait 复经 callActivity 重挂 →
    /// launch_subflows_for 再次解析）。
    async fn launch_one_subflow(
        &self,
        parent_snap: &InstanceSnapshot,
        parent_token_id: &str,
        node_bpmn: &str,
    ) -> Result<()> {
        let def = self.get_definition(&parent_snap.instance.definition_key)?;
        let node = def
            .node_by_bpmn(node_bpmn)
            .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
        let ca = match &node.kind {
            NodeKind::CallActivity(ca) => ca.clone(),
            other => {
                return Err(Error::IllegalTokenState(format!(
                    "节点 {node_bpmn} 非 callActivity，实际 {other:?}"
                )));
            }
        };
        // 子流程维度上下文默认继承主实例。
        // RD0：路由维度 = 挂载点声明的 dim_key（缺省 "org"，向后兼容 M5.2）。
        let dim_key = ca
            .dim_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(cmx_flow_model::DIM_ORG);
        // 维度取值：从实例的维度上下文取该维度；"org" 维度回退 org_id 标量列（RD0 兼容，RD3 前唯一来源）。
        let dim_value = parent_snap
            .instance
            .dimensions
            .get(dim_key)
            .cloned()
            .or_else(|| {
                if dim_key == cmx_flow_model::DIM_ORG {
                    parent_snap.instance.org_id.clone()
                } else {
                    None
                }
            });
        // 子实例组织默认继承主实例（沿用；RD3 后整个 dimensions 继承）。
        let org = parent_snap.instance.org_id.clone();

        // 解析子流程定义 key：
        // - called_key 非空（M5.2 逻辑名）→ 走 SubflowRouter 按维度路由；
        // - 否则用 called_element 写死值（M5.1 行为）。
        // 解析/校验失败 → 把本挂载点令牌转 Incident（见方法注释），不抛错、不留僵尸。
        let sub_key = match &ca.called_key {
            Some(key) if !key.is_empty() => match &self.subflow_router {
                Some(router) => match router.resolve(key, dim_key, dim_value.as_deref()).await {
                    Ok(k) => k,
                    Err(e) => {
                        return self
                            .mark_subflow_incident(&parent_snap.instance.id, node_bpmn, &e.to_string())
                            .await;
                    }
                },
                None => {
                    let reason =
                        format!("callActivity {node_bpmn} 用逻辑 key '{key}' 但未注入 SubflowRouter");
                    return self
                        .mark_subflow_incident(&parent_snap.instance.id, node_bpmn, &reason)
                        .await;
                }
            },
            _ => ca.called_element.clone(),
        };
        if !self.has_definition(&sub_key) {
            let reason = format!("子流程 {sub_key}（callActivity {node_bpmn} 调用）未部署");
            return self
                .mark_subflow_incident(&parent_snap.instance.id, node_bpmn, &reason)
                .await;
        }

        // 输入变量映射：主 → 子。空映射 = 全量传递。
        let child_vars = map_vars(&parent_snap.instance.variables, &ca.input_vars, true);

        let child_snap = self
            .start_process_inner(StartContext {
                definition_key: sub_key,
                variables: child_vars,
                business_key: parent_snap.instance.business_key.clone(),
                org_id: org,
                // 子实例整体继承父的维度上下文（RD3）——各挂载点仍按自己的 dim_key 取对应维度值。
                dimensions: parent_snap.instance.dimensions.clone(),
                // v2.4：子实例复制父实例的发起绑定（同一业务流程的组成部分，回调同一家，方案 §3.2）。
                subscriber_id: parent_snap.instance.subscriber_id,
                // 005：子实例继承父实例的系统归属（同一业务流程的组成部分）。
                system_id: parent_snap.instance.system_id.clone(),
                parent_instance_id: Some(parent_snap.instance.id.clone()),
                parent_token_id: Some(parent_token_id.to_string()),
                parent_node_bpmn_id: Some(node_bpmn.to_string()),
            })
            .await?;

        // 子实例若立即完成（无等待态）→ 唤醒父令牌。
        if child_snap.instance.state == InstanceState::Completed {
            self.complete_subflow(&child_snap.instance.id).await?;
        } else {
            // 子实例自身也可能起始就有 callActivity，递归启动其子流程。
            self.launch_subflows_for(&child_snap.instance.id).await?;
        }
        Ok(())
    }

    /// 子流程解析失败 → 把该 callActivity 挂载点的 WaitingSubflow 令牌转 Incident + 记原因。
    /// 实例保持 Active、可见（hasIncident）、可 retry_incident 恢复。对齐 serviceTask 失败语义。
    async fn mark_subflow_incident(
        &self,
        instance_id: &str,
        node_bpmn: &str,
        reason: &str,
    ) -> Result<()> {
        let mut snap = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        let retries = record_incident(&mut snap.instance.variables, node_bpmn, reason);
        // 把本挂载点（同一 callActivity 节点）仍在 WaitingSubflow 的令牌转 Incident。
        for tok in snap.tokens.iter_mut() {
            if tok.node_bpmn_id == node_bpmn && tok.state == TokenState::WaitingSubflow {
                tok.state = TokenState::Incident;
                tok.updated_at = now;
            }
        }
        snap.instance.updated_at = now;
        self.store.save_snapshot(&snap).await?;
        let token_id = snap
            .tokens
            .iter()
            .find(|t| t.node_bpmn_id == node_bpmn && t.state == TokenState::Incident)
            .map(|t| t.id.clone());
        self.upsert_incident_aux(&snap, node_bpmn, token_id.as_deref(), reason, retries)
            .await;
        tracing::warn!(
            instance = %instance_id, node = %node_bpmn, retries, reason,
            "子流程解析失败 → Incident（实例保留，修绑定后 retry_incident 恢复）"
        );
        Ok(())
    }

    /// 子实例完成回调（M5）：把子实例输出变量回写主实例 → 唤醒父令牌沿 callActivity 出边前进
    /// → 继续推进主实例。用 Box::pin 支持递归（父完成又可能唤醒祖父）。
    fn complete_subflow<'a>(
        &'a self,
        child_instance_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let child = self.store.load_snapshot(child_instance_id).await?;
            let (Some(parent_id), Some(parent_token_id)) = (
                child.instance.parent_instance_id.clone(),
                child.instance.parent_token_id.clone(),
            ) else {
                return Ok(()); // 顶层实例，无父可唤醒。
            };

            let mut parent = self.store.load_snapshot(&parent_id).await?;
            let now = self.clock.now();
            let def = self.get_definition(&parent.instance.definition_key)?;

            // 定位父令牌，必须是 WaitingSubflow。
            let Some(tidx) = parent.tokens.iter().position(|t| t.id == parent_token_id) else {
                return Ok(()); // 父令牌已不存在（父被取消等），静默收尾。
            };
            if parent.tokens[tidx].state != TokenState::WaitingSubflow {
                return Ok(()); // 已被唤醒（幂等）。
            }
            let node_bpmn = parent.tokens[tidx].node_bpmn_id.clone();
            let node = def
                .node_by_bpmn(&node_bpmn)
                .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
            let (outgoing, ca) = match &node.kind {
                NodeKind::CallActivity(ca) => (node.outgoing.clone(), ca.clone()),
                other => {
                    return Err(Error::IllegalTokenState(format!(
                        "父令牌节点 {node_bpmn} 非 callActivity，实际 {other:?}"
                    )));
                }
            };

            // 输出变量映射：子 → 父。空映射 = 全量回写。
            let back = map_vars(&child.instance.variables, &ca.output_vars, true);
            // 变量历史（引擎派生）：子流程回填逐 key 相对父旧值 diff → pending_var_changes。
            let derived = diff_derived_var_changes(
                &parent.instance.id,
                &back,
                &parent.instance.variables,
                "subflow",
                &node_bpmn,
            );
            parent.instance.variables.merge(back);
            parent.pending_var_changes.extend(derived);

            // 父令牌离开 callActivity 沿唯一出边，转 Active。
            let target = outgoing
                .first()
                .map(|f| f.target_bpmn_id.clone())
                .ok_or_else(|| {
                    Error::IllegalTokenState(format!("callActivity {node_bpmn} 无出边可离开"))
                })?;
            {
                let tok = &mut parent.tokens[tidx];
                tok.node_bpmn_id = target;
                tok.state = TokenState::Active;
                tok.updated_at = now;
            }

            self.run_to_wait(&def, &mut parent, now).await?;
            self.store.save_snapshot(&parent).await?;
            // flush 本段父推进产生的旁路记录（与其它 advance 路径同构）：子流程回填的变量历史
            //   （pending_var_changes，本轮新增）、父段闭合的活动历史、新增消息订阅。此前 complete_subflow
            //   遗漏 flush——回填变量历史会丢（现补上；活动/订阅同理，虽多在别处再生，一并对齐）。
            self.flush_pending(&parent).await;

            // 父推进后可能又停在新的 callActivity，或自身完成再唤醒祖父。
            if parent.instance.state == InstanceState::Completed {
                self.complete_subflow(&parent_id).await?;
            } else {
                self.launch_subflows_for(&parent_id).await?;
            }
            Ok(())
        })
    }

    /// 办结一个用户任务：合并变量 → 令牌离开 userTask → 继续推进 → 落库。
    ///
    /// 这是「等待态恢复」路径：与首次推进复用同一 run_to_wait，唯一区别是需要先把令牌
    /// 从 userTask 沿出边移走（否则会重复建任务）。多实例（会签/或签）任务走独立的收口/
    /// 展开逻辑（complete_mi_task），普通任务走 M1 单实例路径。
    pub async fn complete_task(
        &self,
        instance_id: &str,
        task_id: &str,
        variables: Variables,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;

        // 实例已挂起（A7）：拒绝办结，恢复后方可办理。
        if snapshot.instance.state == InstanceState::Suspended {
            return Err(Error::TaskNotActionable(format!(
                "实例 {instance_id} 已挂起，恢复(resume)后方可办理"
            )));
        }

        // 定位任务，必须存在且未办结。
        let task_idx = snapshot
            .tasks
            .iter()
            .position(|t| t.id == task_id && !t.completed)
            .ok_or_else(|| Error::TaskNotActionable(task_id.to_string()))?;
        // 被加签挂起的父任务不可直接办结——必须先办完临时任务（M4.3）。
        if snapshot.tasks[task_idx].delegation_state.as_deref() == Some("SUSPENDED") {
            return Err(Error::TaskNotActionable(format!(
                "任务 {task_id} 已被加签挂起，需先办结加签的临时任务"
            )));
        }
        let token_id = snapshot.tasks[task_idx].token_id.clone();
        let node_bpmn = snapshot.tasks[task_idx].node_bpmn_id.clone();
        // 办结人（抄送发起人来源）。
        let actor = snapshot.tasks[task_idx].assignee.clone();
        // 加签临时任务？（有父任务 → 办结后回父任务，不推进令牌）M4.3。
        let parent_task_id = snapshot.tasks[task_idx].parent_task_id.clone();
        // 该节点配置的抄送表达式（M4.2）——办结后对这些人知会。先取出，规避后续可变借用。
        let cc_refs = def
            .node_by_bpmn(&node_bpmn)
            .and_then(|n| match &n.kind {
                NodeKind::UserTask(ut) => Some(ut.cc.clone()),
                _ => None,
            })
            .unwrap_or_default();

        // 合并提交的变量到实例。
        snapshot.instance.variables.merge(variables);

        // 办结任务。
        let now = self.clock.now();
        {
            let task = &mut snapshot.tasks[task_idx];
            task.completed = true;
            task.completed_at = Some(now);
        }

        // 节点配置抄送：解析 cc 表达式为用户，落抄送记录（不阻塞流程）。
        if !cc_refs.is_empty() {
            let cc_ctx = Self::resolve_ctx_of(&snapshot);
            let cc_users = self.resolve_candidates(&cc_refs, &cc_ctx).await?;
            append_cc_records(
                &mut snapshot.cc_records,
                &snapshot.instance.id,
                Some(&node_bpmn),
                actor.as_deref(),
                None,
                &cc_users,
                now,
            );
        }

        // 该任务是否属于某个活动的多实例域？
        let is_mi = snapshot
            .mi_scopes
            .iter()
            .any(|s| s.node_bpmn_id == node_bpmn && !s.finished);

        if let Some(parent_id) = parent_task_id {
            // —— 加签临时任务办结（M4.3）：不推进令牌，回到父任务 —— //
            // 临时任务用完即弃（已 completed=true）。父任务恢复可办理（解除挂起）。
            // 若父任务本身也是临时任务（嵌套加签），它的 SUSPENDED 也一并解除。
            if let Some(parent) = snapshot.tasks.iter_mut().find(|t| t.id == parent_id) {
                // 父任务解除挂起：清 SUSPENDED，恢复常规待办。
                if parent.delegation_state.as_deref() == Some("SUSPENDED") {
                    parent.delegation_state = None;
                }
            }
            // 令牌不动（仍 Waiting 在本节点），流程不推进——父任务还没办。
        } else if is_mi {
            // —— 多实例（会签/或签）：计数、判完成条件、收口或（顺序）展开下一个 —— //
            // 展开须逐元素 async 解析办理人，故 complete_mi_task 只算出「待展开元素」，
            // 由此处（有 &self）调 expand_mi_element 完成 async 展开（②）。
            let pending = complete_mi_task(&def, &mut snapshot, &node_bpmn, &token_id, now)?;
            if let Some(p) = pending {
                self.expand_mi_element(
                    &mut snapshot,
                    &p.node_bpmn,
                    &p.node_name,
                    &p.ut,
                    p.element,
                    p.index,
                    now,
                )
                .await?;
            }
        } else {
            // —— 普通单实例：令牌离开 userTask 沿唯一出边，转 Active —— //
            let token_idx = snapshot
                .tokens
                .iter()
                .position(|t| t.id == token_id)
                .ok_or_else(|| {
                    Error::IllegalTokenState(format!("任务 {task_id} 的令牌 {token_id} 不存在"))
                })?;
            if snapshot.tokens[token_idx].state != TokenState::Waiting {
                return Err(Error::IllegalTokenState(format!(
                    "令牌 {token_id} 非 Waiting，无法从 userTask 恢复"
                )));
            }
            // 令牌离开宿主 userTask：连带撤销挂在它上面的定时器作业（未触发即作废）。
            snapshot.jobs.retain(|j| j.token_id != token_id);
            // 任务办结：其候选记录已无意义，清理（M4.1）。
            snapshot.candidates.retain(|c| c.task_id != task_id);
            let node = def
                .node_by_bpmn(&node_bpmn)
                .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
            let outgoing = node.outgoing.clone();
            let kind = node.kind.clone();
            let target = choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
            // A6：令牌离开 userTask（可能已停留数日）——闭合这段等待活动，enter=令牌到达该任务时刻。
            close_activity(&mut snapshot, token_idx, &def, now);
            let tok = &mut snapshot.tokens[token_idx];
            tok.node_bpmn_id = target;
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }

        self.run_to_wait(&def, &mut snapshot, now).await?;
        self.store.save_snapshot(&snapshot).await?;

        // P3：flush 本次推进段产生的消息订阅。
        self.flush_pending(&snapshot).await;
        // 办结可能把令牌推进到 callActivity（挂起 WaitingSubflow）→ 启动子实例。
        let snapshot = self.launch_subflows_for(instance_id).await?;
        // 若本实例是子流程且已完成 → 唤醒父实例（回写变量 + 推进父流程）。
        if snapshot.instance.state == InstanceState::Completed
            && snapshot.instance.parent_instance_id.is_some()
        {
            self.complete_subflow(instance_id).await?;
        }
        Ok(Self::result_of(&snapshot))
    }

    /// 认领一个候选任务（M4.1）——多人候选池里的某用户把任务据为己有。
    ///
    /// 语义对齐 Flowable claim：校验该用户确在候选池 → 写 task.assignee = 该用户 →
    /// 清空本任务的候选记录（认领后不再是"待认领"）→ 落库。不推进令牌（任务仍在等待，
    /// 只是有了明确办理人）。幂等性：若任务已被本人认领，重复认领无害；被他人认领则报错。
    pub async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        user_id: &str,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;

        let task_idx = snapshot
            .tasks
            .iter()
            .position(|t| t.id == task_id && !t.completed)
            .ok_or_else(|| Error::TaskNotActionable(task_id.to_string()))?;

        // 已有办理人：仅允许本人幂等认领，他人认领报错。
        if let Some(existing) = &snapshot.tasks[task_idx].assignee {
            if existing == user_id {
                return Ok(Self::result_of(&snapshot));
            }
            return Err(Error::ClaimFailed(format!(
                "任务 {task_id} 已被 {existing} 认领"
            )));
        }

        // 校验 user 在候选池内。
        let in_pool = snapshot
            .candidates
            .iter()
            .any(|c| c.task_id == task_id && c.resolved_user_id == user_id);
        if !in_pool {
            return Err(Error::ClaimFailed(format!(
                "用户 {user_id} 不在任务 {task_id} 的候选池中"
            )));
        }

        // 认领：置 assignee，清空本任务候选记录。
        let now = self.clock.now();
        snapshot.tasks[task_idx].assignee = Some(user_id.to_string());
        snapshot.candidates.retain(|c| c.task_id != task_id);
        snapshot.instance.updated_at = now;

        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 转办（M4.3）——把任务整个转给另一人，原办理人退出，彻底换人。
    ///
    /// assignee 与 owner 都改成 to_user（新人完全接手）。清候选池（已定人）。记台账。
    pub async fn transfer_task(
        &self,
        instance_id: &str,
        task_id: &str,
        from_user: &str,
        to_user: &str,
        reason: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        let idx = self.actionable_task_idx(&snapshot, task_id)?;
        {
            let t = &mut snapshot.tasks[idx];
            t.assignee = Some(to_user.to_string());
            t.owner_user_id = Some(to_user.to_string()); // 彻底换人：owner 也归新人
            t.delegation_state = None;
        }
        snapshot.candidates.retain(|c| c.task_id != task_id);
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            task_id,
            "TRANSFER",
            from_user,
            to_user,
            None,
            reason,
            now,
        );
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 委派（M4.3）——委托他人代办，所有权仍归 owner（区别于转办的彻底换人）。
    ///
    /// owner 首次委派时记为当前 assignee；assignee 改为代理人；delegation_state=DELEGATED。
    /// M4.3 简化：代理人办结后直接完成（不回 owner 二次确认）。记台账。
    pub async fn delegate_task(
        &self,
        instance_id: &str,
        task_id: &str,
        from_user: &str,
        to_user: &str,
        reason: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        let idx = self.actionable_task_idx(&snapshot, task_id)?;
        {
            let t = &mut snapshot.tasks[idx];
            // owner 保持原主：若尚未记 owner，则把当前 assignee（或发起人）固定为 owner。
            if t.owner_user_id.is_none() {
                t.owner_user_id = Some(t.assignee.clone().unwrap_or_else(|| from_user.to_string()));
            }
            t.assignee = Some(to_user.to_string());
            t.delegation_state = Some("DELEGATED".to_string());
        }
        snapshot.candidates.retain(|c| c.task_id != task_id);
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            task_id,
            "DELEGATE",
            from_user,
            to_user,
            None,
            reason,
            now,
        );
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 加签（M4.3）——在当前任务前/后插一个临时审批人，办完回原任务。可嵌套。
    ///
    /// `before=true`（向前加签）：原任务**挂起**（SUSPENDED，不可直接办结），建一个临时任务给
    /// to_user；临时任务办结后原任务恢复。`before=false`（向后加签）：语义上"我先办再交他审"，
    /// M4.3 统一实现为：同样挂起原任务 + 建临时任务，临时办结后原任务恢复——差异仅记在台账
    /// 类型（ADDSIGN_BEFORE / ADDSIGN_AFTER），运行行为一致（都是「多一道，办完回来」）。
    ///
    /// 临时任务与原任务**共享同一令牌**（令牌仍 Waiting 在本节点，流程不推进），故不产生新令牌，
    /// 也不影响下游。嵌套：临时任务可再被加签（它成为新的父，自己被挂起）。
    pub async fn add_sign(
        &self,
        instance_id: &str,
        task_id: &str,
        from_user: &str,
        to_user: &str,
        before: bool,
        reason: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        let idx = self.actionable_task_idx(&snapshot, task_id)?;

        // 复制原任务的定位信息给临时任务。
        let (token_id, node_bpmn, name) = {
            let t = &snapshot.tasks[idx];
            (t.token_id.clone(), t.node_bpmn_id.clone(), t.name.clone())
        };
        // 原任务挂起。
        snapshot.tasks[idx].delegation_state = Some("SUSPENDED".to_string());

        // 建临时任务（同令牌、同节点，办理人=被加签人，parent 指向原任务）。
        let temp_id = Uuid::new_v4().to_string();
        snapshot.tasks.push(Task {
            id: temp_id.clone(),
            instance_id: instance_id.to_string(),
            token_id,
            node_bpmn_id: node_bpmn,
            name: name.map(|n| format!("{n}（加签）")),
            assignee: Some(to_user.to_string()),
            candidate_groups: None,
            element_value: None,
            owner_user_id: Some(to_user.to_string()),
            parent_task_id: Some(task_id.to_string()),
            delegation_state: Some("ADDSIGN".to_string()),
            completed: false,
            created_at: now,
            completed_at: None,
        });

        let kind = if before {
            "ADDSIGN_BEFORE"
        } else {
            "ADDSIGN_AFTER"
        };
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            task_id,
            kind,
            from_user,
            to_user,
            Some(&temp_id),
            reason,
            now,
        );
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 定位一个「可操作」任务的下标：存在、未办结、未被挂起。转签家族共用。
    fn actionable_task_idx(&self, snapshot: &InstanceSnapshot, task_id: &str) -> Result<usize> {
        // 实例已挂起（A7）：拒绝一切任务动作，直到 resume。
        if snapshot.instance.state == InstanceState::Suspended {
            return Err(Error::TaskNotActionable(format!(
                "实例 {} 已挂起，恢复(resume)后方可办理",
                snapshot.instance.id
            )));
        }
        let idx = snapshot
            .tasks
            .iter()
            .position(|t| t.id == task_id && !t.completed)
            .ok_or_else(|| Error::TaskNotActionable(task_id.to_string()))?;
        if snapshot.tasks[idx].delegation_state.as_deref() == Some("SUSPENDED") {
            return Err(Error::TaskNotActionable(format!(
                "任务 {task_id} 已被加签挂起，不可再操作"
            )));
        }
        Ok(idx)
    }

    /// 手动抄送（M4.2）——办理人主动把某实例进展知会给一组用户（表达式，解析成用户集）。
    ///
    /// 不阻塞流程、不改令牌。落抄送记录（去重）后保存。`node_bpmn` 可传当前节点（可空）。
    pub async fn notify_cc(
        &self,
        instance_id: &str,
        cc_refs: &[cmx_flow_model::CandidateRef],
        from_user: Option<&str>,
        reason: Option<&str>,
    ) -> Result<usize> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let cc_ctx = Self::resolve_ctx_of(&snapshot);
        let cc_users = self.resolve_candidates(cc_refs, &cc_ctx).await?;
        let before = snapshot.cc_records.len();
        let now = self.clock.now();
        append_cc_records(
            &mut snapshot.cc_records,
            instance_id,
            None,
            from_user,
            reason,
            &cc_users,
            now,
        );
        let added = snapshot.cc_records.len() - before;
        if added > 0 {
            self.store.save_snapshot(&snapshot).await?;
        }
        Ok(added)
    }

    /// 标记一条抄送记录为已读（M4.2）。直通存储层，幂等。
    pub async fn mark_cc_read(&self, cc_id: &str) -> Result<bool> {
        let now = self.clock.now();
        self.store
            .mark_cc_read(cc_id, now)
            .await
            .map_err(Error::from)
    }

    /// 查询抄送给某用户的记录（M4.2）。unread_only=true 只返回未读。
    pub async fn cc_for_user(
        &self,
        user_id: &str,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<cmx_flow_model::CcSummary>> {
        self.store
            .find_cc_for_user(user_id, unread_only, limit)
            .await
            .map_err(Error::from)
    }

    /// 退回 / 驳回（P6）——把一个待办任务打回到之前的节点重新办理。
    ///
    /// 与 complete_task 的区别：complete 沿出边**向前**推进；reject 把令牌移到**目标节点**
    /// （回退），由 run_to_wait 在目标节点重建待办。目标选择：
    ///   - 显式 `target_bpmn_id`（驳回到指定节点，须是 userTask，A/B8「驳回到指定节点」）；
    ///   - 省略则回退到**直接前驱**（沿入边找唯一上游节点；多前驱时报错要求显式指定）。
    ///
    /// 语义：当前任务标记办结（记 REJECT 台账，comment 存驳回意见），令牌回目标节点重新展开。
    /// 合并提交的变量（可带驳回意见变量）。不做补偿。多实例任务不支持退回（须整域处理）——报错。
    pub async fn reject_task(
        &self,
        instance_id: &str,
        task_id: &str,
        from_user: &str,
        target_bpmn_id: Option<&str>,
        reason: Option<&str>,
        variables: Variables,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;

        let idx = self.actionable_task_idx(&snapshot, task_id)?;
        let node_bpmn = snapshot.tasks[idx].node_bpmn_id.clone();
        let token_id = snapshot.tasks[idx].token_id.clone();

        // 多实例任务不支持退回（一个子任务打回会破坏会签计数）——显式挡回。
        if snapshot
            .mi_scopes
            .iter()
            .any(|s| s.node_bpmn_id == node_bpmn && !s.finished)
        {
            return Err(Error::TaskNotActionable(format!(
                "任务 {task_id} 属于会签/或签域，不支持退回（请整域处理）"
            )));
        }

        // 目标节点：显式优先，否则找直接前驱。
        let target = match target_bpmn_id {
            Some(t) if !t.is_empty() => {
                // 校验目标存在且是 userTask（回退到非人工节点无意义）。
                let tn = def.node_by_bpmn(t).ok_or_else(|| {
                    Error::TaskNotActionable(format!("退回目标节点 {t} 不在定义中"))
                })?;
                if !matches!(tn.kind, NodeKind::UserTask(_)) {
                    return Err(Error::TaskNotActionable(format!(
                        "退回目标 {t} 不是用户任务，无法回退办理"
                    )));
                }
                t.to_string()
            }
            _ => predecessor_user_task(&def, &node_bpmn)?,
        };

        let now = self.clock.now();
        // 合并驳回意见等变量。
        snapshot.instance.variables.merge(variables);

        // 当前任务办结 + 台账（REJECT）。
        {
            let t = &mut snapshot.tasks[idx];
            t.completed = true;
            t.completed_at = Some(now);
        }
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            task_id,
            "REJECT",
            from_user,
            &target,
            None,
            reason,
            now,
        );
        // 清本任务候选 + 挂在令牌上的定时器。
        snapshot.candidates.retain(|c| c.task_id != task_id);
        snapshot.jobs.retain(|j| j.token_id != token_id);

        // 令牌回目标节点，转 Active，由 run_to_wait 重建目标待办。
        let token_idx = snapshot
            .tokens
            .iter()
            .position(|t| t.id == token_id)
            .ok_or_else(|| {
                Error::IllegalTokenState(format!("任务 {task_id} 的令牌 {token_id} 不存在"))
            })?;
        {
            let tok = &mut snapshot.tokens[token_idx];
            tok.node_bpmn_id = target.clone();
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }

        self.run_to_wait(&def, &mut snapshot, now).await?;
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        self.flush_pending(&snapshot).await;
        Ok(Self::result_of(&snapshot))
    }

    /// 列举一个任务的**全部合法退回目标**（只读，不改运行态）——供前端渲染「退回到…」选择器。
    ///
    /// 退回目标 = 从任务当前节点沿入边**反向可达**的上游 userTask 集合（穿透网关/事件/服务任务，
    /// 只收 userTask）。`is_direct_predecessor` 标出 reject_task 省略 target 时会去的那个（默认目标）。
    /// 会签/或签域内任务不支持退回（引擎 reject 亦挡回）→ `rejectable=false`、`targets` 空。
    pub async fn reject_targets(
        &self,
        instance_id: &str,
        task_id: &str,
    ) -> Result<RejectTargetInfo> {
        let snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;

        let task = snapshot
            .tasks
            .iter()
            .find(|t| t.id == task_id && !t.completed)
            .ok_or_else(|| {
                Error::TaskNotActionable(format!("任务 {task_id} 不存在或已办结"))
            })?;
        let node_bpmn = task.node_bpmn_id.clone();

        // 会签/或签域内任务：与 reject_task 一致挡回。
        let in_mi = snapshot
            .mi_scopes
            .iter()
            .any(|s| s.node_bpmn_id == node_bpmn && !s.finished);
        if in_mi {
            return Ok(RejectTargetInfo {
                task_id: task_id.to_string(),
                current_node: node_bpmn,
                rejectable: false,
                default_target: None,
                targets: Vec::new(),
            });
        }

        let mut targets = reachable_upstream_user_tasks(&def, &node_bpmn);
        // 默认目标 = reject_task 省略 target 时的直接前驱（唯一上游 userTask，回溯跳网关）。
        let default_target = predecessor_user_task(&def, &node_bpmn).ok();
        if let Some(def_t) = &default_target {
            for t in targets.iter_mut() {
                if &t.bpmn_id == def_t {
                    t.is_direct_predecessor = true;
                }
            }
        }
        Ok(RejectTargetInfo {
            task_id: task_id.to_string(),
            rejectable: !targets.is_empty(),
            current_node: node_bpmn,
            default_target,
            targets,
        })
    }

    /// 运维干预：直接改实例变量（H4）——merge 传入变量到实例并落库，不推进令牌。
    ///
    /// 用于运维台修数据（如修正一个导致 incident 的错误值），改完可再 `retry_incident`。
    /// 只改变量、不动令牌/任务，是安全的最小干预。返回更新后的实例视图。
    pub async fn set_variables(
        &self,
        instance_id: &str,
        variables: Variables,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        snapshot.instance.variables.merge(variables);
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 011：把 incident 同步登记到跨实例故障清单表（`cmx_flow_incident`）。
    /// 台账是运维辅助，失败仅 warn 不阻塞主流程；同 (instance, node) 幂等累加 retries。
    async fn upsert_incident_aux(
        &self,
        snapshot: &InstanceSnapshot,
        node_bpmn: &str,
        token_id: Option<&str>,
        reason: &str,
        retries: i64,
    ) {
        let now = self.clock.now();
        let rec = cmx_flow_model::IncidentRecord {
            instance_id: snapshot.instance.id.clone(),
            node_bpmn_id: node_bpmn.to_string(),
            token_id: token_id.map(|s| s.to_string()),
            definition_key: snapshot.instance.definition_key.clone(),
            business_key: snapshot.instance.business_key.clone(),
            reason: reason.to_string(),
            retries,
            state: "OPEN".to_string(),
            created_at: now,
            updated_at: now,
        };
        if let Err(e) = self.store.upsert_incident(&rec).await {
            tracing::warn!(
                instance = %snapshot.instance.id, node = %node_bpmn, error = %e,
                "incident 台账登记失败（不阻塞主流程）"
            );
        }
    }

    /// 重试 incident（H2）——把实例内所有 `Incident` 态令牌重新激活，再跑一遍推进。
    ///
    /// 用于 serviceTask 失败挂起后：运维改了数据/修了外部依赖，手动触发重试。重试仍失败则
    /// 令牌再次落 Incident（重试次数累加，见 `record_incident`）。无 incident 令牌 → 幂等无操作。
    /// 可选 `variables`：重试前 merge（如运维手改的修正值）。
    pub async fn retry_incident(
        &self,
        instance_id: &str,
        variables: Variables,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        let now = self.clock.now();

        snapshot.instance.variables.merge(variables);
        // 所有 Incident 令牌 → Active（重新可推进）。
        let mut reactivated = 0usize;
        for tok in &mut snapshot.tokens {
            if tok.state == TokenState::Incident {
                tok.state = TokenState::Active;
                tok.updated_at = now;
                reactivated += 1;
            }
        }
        if reactivated == 0 {
            // 无 incident：幂等返回当前视图，不写库。
            return Ok(Self::result_of(&snapshot));
        }
        tracing::info!(instance = %instance_id, reactivated, "重试 incident：重新激活令牌");

        self.run_to_wait(&def, &mut snapshot, now).await?;
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        self.flush_pending(&snapshot).await;
        // 子流程解析类 incident：令牌重激活后经 run_to_wait 会重新停到 WaitingSubflow，
        // 需再跑一遍子流程启动（绑定已修好则这次能解析成功）。对 serviceTask incident 无副作用。
        let snapshot = self.launch_subflows_for(instance_id).await?;
        // 011：重试成功 → 该实例全部 OPEN incident 置 RESOLVED（清单闭环）。
        if let Err(e) = self.store.resolve_incidents_by_instance(instance_id).await {
            tracing::warn!(instance = %instance_id, error = %e, "incident 清单关闭失败（不阻塞主流程）");
        }
        Ok(Self::result_of(&snapshot))
    }

    // ============================ 实例迁移（A9）============================

    /// 校验一个迁移计划（干运行，不改数据）。返回违规明细；空 = 可迁移。
    ///
    /// 校验项：①目标定义已部署 ②实例 Active ③每个未结束令牌所在节点有映射
    /// ④映射的目标节点在目标定义中存在。
    pub async fn validate_migration(
        &self,
        instance_id: &str,
        plan: &cmx_flow_model::MigrationPlan,
    ) -> Result<cmx_flow_model::MigrationValidation> {
        use cmx_flow_model::{MigrationViolation, MigrationViolationCode as C};
        let snapshot = self.store.load_snapshot(instance_id).await?;
        let mut violations = Vec::new();

        // ① 目标定义已部署？
        let target_def = self.get_definition(&plan.target_definition_key).ok();
        if target_def.is_none() {
            violations.push(MigrationViolation {
                code: C::TargetNotDeployed,
                node_bpmn_id: None,
                message: format!("目标定义未部署: {}", plan.target_definition_key),
            });
        }

        // ② 实例须 Active（终态/挂起不迁）。
        if snapshot.instance.state != InstanceState::Active {
            violations.push(MigrationViolation {
                code: C::InstanceNotActive,
                node_bpmn_id: None,
                message: format!("实例非 Active（{:?}），不可迁移", snapshot.instance.state),
            });
        }

        // ③④ 每个未结束令牌所在节点：有映射 + 映射目标在目标定义存在。
        for tok in &snapshot.tokens {
            if tok.state == TokenState::Ended {
                continue;
            }
            let src = &tok.node_bpmn_id;
            match plan.activity_mappings.get(src) {
                None => violations.push(MigrationViolation {
                    code: C::UnmappedActivity,
                    node_bpmn_id: Some(src.clone()),
                    message: format!("活动令牌所在节点缺映射: {src}"),
                }),
                Some(dst) => {
                    if let Some(def) = &target_def {
                        if def.node_by_bpmn(dst).is_none() {
                            violations.push(MigrationViolation {
                                code: C::TargetNodeMissing,
                                node_bpmn_id: Some(dst.clone()),
                                message: format!("映射目标节点在目标定义中不存在: {dst}"),
                            });
                        }
                    }
                }
            }
        }

        Ok(cmx_flow_model::MigrationValidation {
            ok: violations.is_empty(),
            violations,
        })
    }

    /// 执行实例迁移（A9）——校验通过后按映射重写令牌节点位置 + 改实例定义指向，落库。
    ///
    /// 只重写未结束令牌的 `node_bpmn_id`（稳定锚点）+ 实例 `definition_key`；绑定到任务的令牌，
    /// 其任务 `node_bpmn_id` 同步重写（否则任务视图指向旧节点）。迁移后实例在新定义上照常推进。
    /// 校验不通过 → 返回 `Error::TaskNotActionable`（含首条违规），不改任何数据。
    pub async fn migrate_instance(
        &self,
        instance_id: &str,
        plan: &cmx_flow_model::MigrationPlan,
    ) -> Result<ExecutionResult> {
        let validation = self.validate_migration(instance_id, plan).await?;
        if !validation.ok {
            let first = validation
                .violations
                .first()
                .map(|v| v.message.clone())
                .unwrap_or_else(|| "迁移校验失败".into());
            return Err(Error::TaskNotActionable(format!("迁移校验不通过: {first}")));
        }

        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        // 重写未结束令牌节点 + 其绑定任务节点。
        for tok in snapshot.tokens.iter_mut() {
            if tok.state == TokenState::Ended {
                continue;
            }
            if let Some(dst) = plan.activity_mappings.get(&tok.node_bpmn_id) {
                let old = tok.node_bpmn_id.clone();
                tok.node_bpmn_id = dst.clone();
                tok.updated_at = now;
                // 同步重写绑定到本令牌、且停在旧节点的未办结任务。
                for task in snapshot.tasks.iter_mut() {
                    if task.token_id == tok.id && task.node_bpmn_id == old && !task.completed {
                        task.node_bpmn_id = dst.clone();
                    }
                }
            }
        }
        // 实例定义指向目标。
        snapshot.instance.definition_key = plan.target_definition_key.clone();
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        tracing::info!(
            instance = %instance_id, target = %plan.target_definition_key,
            mapped = plan.activity_mappings.len(), "实例已迁移到新定义"
        );
        Ok(Self::result_of(&snapshot))
    }

    /// 挂起实例（A7）——暂停一个运行中实例：令牌/任务保留，拒绝一切办理动作，直到 resume。
    ///
    /// 幂等：非 Active 实例（已挂起/完成/终止）直接返回其视图。审批场景：争议冻结、等外部裁决。
    pub fn suspend_process<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExecutionResult>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut snapshot = self.store.load_snapshot(instance_id).await?;
            if snapshot.instance.state != InstanceState::Active {
                return Ok(Self::result_of(&snapshot));
            }
            // 级联挂起运行中的子实例（父冻结，子也该冻结，否则子仍可办理）。
            let children = self.store.find_child_instances(instance_id).await?;
            for child in &children {
                if child.state == InstanceState::Active {
                    self.suspend_process(&child.id).await?;
                }
            }
            snapshot.instance.state = InstanceState::Suspended;
            snapshot.instance.updated_at = self.clock.now();
            self.store.save_snapshot(&snapshot).await?;
            tracing::info!(instance = %instance_id, "实例已挂起（含级联子实例）");
            Ok(Self::result_of(&snapshot))
        })
    }

    /// 恢复实例（A7）——把挂起的实例转回 Active，可继续办理。
    ///
    /// 幂等：非 Suspended 实例直接返回其视图。恢复后推进任何 Active 令牌。
    /// 级联：一并恢复因父挂起而被级联挂起的子实例（与 suspend 对称）。
    pub fn resume_process<'a>(
        &'a self,
        instance_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExecutionResult>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut snapshot = self.store.load_snapshot(instance_id).await?;
            if snapshot.instance.state != InstanceState::Suspended {
                return Ok(Self::result_of(&snapshot));
            }
            let def = self.get_definition(&snapshot.instance.definition_key)?;
            let now = self.clock.now();
            // 级联恢复被级联挂起的子实例。
            let children = self.store.find_child_instances(instance_id).await?;
            for child in &children {
                if child.state == InstanceState::Suspended {
                    self.resume_process(&child.id).await?;
                }
            }
            snapshot.instance.state = InstanceState::Active;
            snapshot.instance.updated_at = now;
            self.run_to_wait(&def, &mut snapshot, now).await?;
            self.store.save_snapshot(&snapshot).await?;
            self.flush_pending(&snapshot).await;
            tracing::info!(instance = %instance_id, "实例已恢复（含级联子实例）");
            Ok(Self::result_of(&snapshot))
        })
    }

    /// 自由跳转（A7）——管理员把实例令牌强制移到任意用户任务节点重新办理。
    ///
    /// 运维纠偏（跳过卡住环节 / 回到某节点重办）。目标须 userTask。作废当前未办结任务、清候选/
    /// 定时器/会签域，只保留被跳转的主令牌落到目标由 run_to_wait 重建待办。
    /// 简化：跳转**第一个**非结束令牌（审批流通常单主令牌；多令牌并行分支跳转语义复杂，暂不支持）。
    pub async fn jump_to(
        &self,
        instance_id: &str,
        target_bpmn_id: &str,
        reason: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        if snapshot.instance.state != InstanceState::Active {
            return Err(Error::TaskNotActionable(format!(
                "实例 {instance_id} 非活动态，不能跳转"
            )));
        }
        let tn = def
            .node_by_bpmn(target_bpmn_id)
            .ok_or_else(|| Error::TaskNotActionable(format!("跳转目标 {target_bpmn_id} 不在定义中")))?;
        if !matches!(tn.kind, NodeKind::UserTask(_)) {
            return Err(Error::TaskNotActionable(format!(
                "跳转目标 {target_bpmn_id} 不是用户任务"
            )));
        }
        let now = self.clock.now();
        let tok_idx = snapshot
            .tokens
            .iter()
            .position(|t| t.state != TokenState::Ended)
            .ok_or_else(|| Error::IllegalTokenState("无可跳转的活动令牌".into()))?;
        let jumped_token = snapshot.tokens[tok_idx].id.clone();
        for t in snapshot.tasks.iter_mut() {
            if !t.completed {
                t.completed = true;
                t.completed_at = Some(now);
            }
        }
        snapshot.candidates.clear();
        snapshot.jobs.clear();
        for s in snapshot.mi_scopes.iter_mut() {
            s.finished = true;
        }
        for (i, tok) in snapshot.tokens.iter_mut().enumerate() {
            if i != tok_idx && tok.state != TokenState::Ended {
                tok.state = TokenState::Ended;
                tok.updated_at = now;
            }
        }
        {
            let tok = &mut snapshot.tokens[tok_idx];
            tok.node_bpmn_id = target_bpmn_id.to_string();
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            &jumped_token,
            "JUMP",
            "admin",
            target_bpmn_id,
            None,
            reason,
            now,
        );
        self.run_to_wait(&def, &mut snapshot, now).await?;
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        self.flush_pending(&snapshot).await;
        tracing::info!(instance = %instance_id, target = %target_bpmn_id, "自由跳转");
        Ok(Self::result_of(&snapshot))
    }

    /// 催办（A7）——对待办任务当前办理人发催办知会（落抄送 + 台账），不改流程。
    pub async fn urge_task(
        &self,
        instance_id: &str,
        task_id: &str,
        from_user: &str,
        message: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let now = self.clock.now();
        let idx = snapshot
            .tasks
            .iter()
            .position(|t| t.id == task_id && !t.completed)
            .ok_or_else(|| Error::TaskNotActionable(task_id.to_string()))?;
        let assignee = snapshot.tasks[idx].assignee.clone();
        let node_bpmn = snapshot.tasks[idx].node_bpmn_id.clone();
        if let Some(to) = &assignee {
            append_cc_records(
                &mut snapshot.cc_records,
                instance_id,
                Some(&node_bpmn),
                Some(from_user),
                message,
                std::slice::from_ref(to),
                now,
            );
        }
        push_delegation(
            &mut snapshot.delegations,
            instance_id,
            task_id,
            "URGE",
            from_user,
            assignee.as_deref().unwrap_or(""),
            None,
            message,
            now,
        );
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 相关消息（A4）——外部系统投递一条消息，唤醒停在消息中间捕获事件的令牌。
    ///
    /// 匹配规则：找一个 `WaitingMessage` 令牌，其所在节点是 `MessageCatchEvent` 且消息名 ==
    /// `message_name`，并且（若节点声明了 `correlation_var`）该实例变量的值 == `correlation_key`。
    /// 命中则 merge 传入变量、令牌转 Active、run_to_wait 继续推进。无命中 → 错误（可让调用方降级）。
    ///
    /// `instance_id` 显式给定时只在该实例内找（点对点回调）；为 None 时跨实例扫描（相关键路由）。
    pub async fn correlate_message(
        &self,
        instance_id: Option<&str>,
        message_name: &str,
        correlation_key: Option<&str>,
        variables: Variables,
    ) -> Result<ExecutionResult> {
        // 定位候选实例：显式 id → 单实例；否则**先查订阅表**（技术债 010 接线：
        // find_catch_subscription 索引点查 O(1)，基建已齐），未命中再退 500 实例扫描兜底——
        // 活跃实例超 500 后纯扫描会永久失配，订阅表命中即不受该上限约束。
        let target_iid = match instance_id {
            Some(id) => id.to_string(),
            None => {
                let key = correlation_key.ok_or_else(|| {
                    Error::TaskNotActionable("跨实例相关消息需提供 correlationKey".into())
                })?;
                // 订阅表 tenant_id 维度现状写死 'default'（005 记录的侧表口径），
                // correlate 无租户上下文，按默认租户查（与写入侧一致）。
                // find_catch_subscription 只按 message+tenant 粗筛（首个候选），相关键的
                // 精确匹配历来由引擎做——这里对候选实例做变量级精确校验，不过则退
                // 500 实例扫描兜底（与旧语义逐字节一致；粗筛只是加速路径）。
                let mut candidate = self
                    .store
                    .find_catch_subscription(message_name, Some(key), "default")
                    .await
                    .ok()
                    .flatten();
                if let Some(sub) = &candidate
                    && let (Some(iid), Some(var)) = (&sub.instance_id, &sub.correlation_var)
                {
                    let exact = self.store.load_snapshot(iid).await.ok().and_then(|s| {
                        let cur = s
                            .instance
                            .variables
                            .get(var)
                            .and_then(|v| v.as_str().map(|x| x.to_string()))
                            .or_else(|| s.instance.variables.get(var).map(|v| v.to_string()));
                        Some(cur.as_deref() == Some(key))
                    });
                    if exact != Some(true) {
                        candidate = None;
                    }
                }
                match candidate.and_then(|s| s.instance_id) {
                    Some(iid) => iid,
                    // 订阅未命中（含库错误/粗筛候选不精确）→ 退 500 实例扫描兜底。
                    None => self.find_message_instance(message_name, key).await?,
                }
            }
        };

        let mut snapshot = self.store.load_snapshot(&target_iid).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        let now = self.clock.now();

        // 在该实例里找匹配的 WaitingMessage 令牌，
        // 或 WaitingEventGateway 令牌（A10：消息竞争分支胜出）。
        let mut matched: Option<usize> = None;
        // 若令牌在 WaitingEventGateway，winning_catch_bpmn 记录胜出的捕获节点 bpmn_id
        // （run_to_wait 会从那里继续推进）。
        let mut winning_catch_bpmn: Option<String> = None;
        for (i, tok) in snapshot.tokens.iter().enumerate() {
            match tok.state {
                TokenState::WaitingMessage => {
                    let node = match def.node_by_bpmn(&tok.node_bpmn_id) {
                        Some(n) => n,
                        None => continue,
                    };
                    if let NodeKind::MessageCatchEvent(mc) = &node.kind {
                        if mc.message_name != message_name {
                            continue;
                        }
                        if let (Some(var), Some(key)) = (&mc.correlation_var, correlation_key) {
                            let cur = snapshot
                                .instance
                                .variables
                                .get(var)
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                                .or_else(|| snapshot.instance.variables.get(var).map(|v| v.to_string()));
                            if cur.as_deref() != Some(key) {
                                continue;
                            }
                        }
                        matched = Some(i);
                        break;
                    }
                }
                TokenState::WaitingEventGateway => {
                    // A10：检查是否有针对本消息的竞争分支（出边后继含 MessageCatchEvent(mc.message_name == message_name)）。
                    let gw_node = match def.node_by_bpmn(&tok.node_bpmn_id) {
                        Some(n) => n,
                        None => continue,
                    };
                    let gw_outgoing = gw_node.outgoing.clone();
                    for sf in &gw_outgoing {
                        let Some(target_node) = def.node_by_bpmn(&sf.target_bpmn_id) else {
                            continue;
                        };
                        if let NodeKind::MessageCatchEvent(mc) = &target_node.kind {
                            if mc.message_name == message_name {
                                matched = Some(i);
                                winning_catch_bpmn = Some(sf.target_bpmn_id.clone());
                                break;
                            }
                        }
                    }
                    if matched.is_some() {
                        break;
                    }
                }
                _ => continue,
            }
        }
        let idx = matched.ok_or_else(|| {
            Error::TaskNotActionable(format!(
                "实例 {target_iid} 无等待消息 '{message_name}' 的令牌（相关键 {correlation_key:?}）"
            ))
        })?;

        // 唤醒：merge 变量，令牌离开消息事件沿唯一出边，转 Active，run_to_wait。
        snapshot.instance.variables.merge(variables);
        if let Some(winning_bpmn) = winning_catch_bpmn {
            // A10 事件网关：消息竞争分支胜出，令牌从网关节点直接前进到胜出捕获节点的
            // 唯一出边目标（跳过捕获节点本身，避免 run_to_wait 把它再当 MessageCatchEvent 重挂）。
            // 取消同令牌的所有竞争 TimerJob（定时器竞争方）。
            let tok_id = snapshot.tokens[idx].id.clone();
            snapshot.jobs.retain(|j| j.token_id != tok_id);
            // 取出胜出捕获节点的唯一出边目标。
            let target = def
                .node_by_bpmn(&winning_bpmn)
                .and_then(|n| n.outgoing.first())
                .map(|f| f.target_bpmn_id.clone());
            if let Some(target) = target {
                let tok = &mut snapshot.tokens[idx];
                tok.node_bpmn_id = target;
                tok.state = TokenState::Active;
                tok.updated_at = now;
            }
        } else {
            // 普通消息捕获（A4）：令牌沿捕获节点的唯一出边前进。
            let node_bpmn = snapshot.tokens[idx].node_bpmn_id.clone();
            let outgoing = def
                .node_by_bpmn(&node_bpmn)
                .map(|n| n.outgoing.clone())
                .unwrap_or_default();
            let target = outgoing
                .first()
                .map(|f| f.target_bpmn_id.clone())
                .ok_or_else(|| Error::IllegalTokenState(format!("消息事件 {node_bpmn} 无出边")))?;
            let tok = &mut snapshot.tokens[idx];
            tok.node_bpmn_id = target;
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }
        self.run_to_wait(&def, &mut snapshot, now).await?;
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        self.flush_pending(&snapshot).await;
        // P3：唤醒成功，删除已消费的 Catch 订阅（按实例批量删，幂等）。
        let _ = self.store.delete_subscriptions_by_instance(&target_iid).await;
        tracing::info!(instance = %target_iid, message = %message_name, "相关消息已投递");
        Ok(Self::result_of(&snapshot))
    }

    /// 跨实例查找：哪个实例停在等待 `message_name` 的令牌，且相关键变量值 == `key`。
    ///
    /// P3 改进前的遗留实现（500 实例全量扫描），保留作为兜底路径（订阅表查不到时回退）。
    /// 审批量级可接受；大规模场景直接用订阅索引（`find_catch_subscription`）。
    async fn find_message_instance(&self, message_name: &str, key: &str) -> Result<String> {
        let summaries = self.store.list_instances(500).await?;
        for s in summaries {
            if s.state != InstanceState::Active {
                continue;
            }
            let snap = match self.store.load_snapshot(&s.id).await {
                Ok(sn) => sn,
                Err(_) => continue,
            };
            let def = match self.get_definition(&snap.instance.definition_key) {
                Ok(d) => d,
                Err(_) => continue,
            };
            for tok in &snap.tokens {
                if tok.state != TokenState::WaitingMessage {
                    continue;
                }
                if let Some(node) = def.node_by_bpmn(&tok.node_bpmn_id) {
                    if let NodeKind::MessageCatchEvent(mc) = &node.kind {
                        if mc.message_name != message_name {
                            continue;
                        }
                        let matches_key = match &mc.correlation_var {
                            Some(var) => {
                                let cur = snap
                                    .instance
                                    .variables
                                    .get(var)
                                    .and_then(|v| v.as_str().map(|x| x.to_string()))
                                    .or_else(|| {
                                        snap.instance.variables.get(var).map(|v| v.to_string())
                                    });
                                cur.as_deref() == Some(key)
                            }
                            None => true,
                        };
                        if matches_key {
                            return Ok(s.id.clone());
                        }
                    }
                }
            }
        }
        Err(Error::TaskNotActionable(format!(
            "无实例等待消息 '{message_name}'（相关键 {key}）"
        )))
    }

    /// 幂等：已处于终态的实例直接返回其视图。
    ///
    /// 级联：取消父实例时，其运行中（非终态）的子流程实例一并取消（递归到孙…）。否则子流程
    /// 会被孤立（父 Terminated 而子仍 Active，继续占用待办）。用 Box::pin 支持递归 async。
    pub fn cancel_process<'a>(
        &'a self,
        instance_id: &'a str,
        reason: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExecutionResult>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut snapshot = self.store.load_snapshot(instance_id).await?;
            if snapshot.instance.state.is_terminal() {
                return Ok(Self::result_of(&snapshot));
            }

            let now = self.clock.now();
            // 先级联取消运行中的子实例（父终止，子不该继续跑）。
            let children = self.store.find_child_instances(instance_id).await?;
            for child in &children {
                if !child.state.is_terminal() {
                    self.cancel_process(&child.id, Some("父实例取消，级联终止".into()))
                        .await?;
                }
            }

            // 全部未结束令牌置 Ended。
            for tok in snapshot.tokens.iter_mut() {
                if tok.state != TokenState::Ended {
                    tok.state = TokenState::Ended;
                    tok.updated_at = now;
                }
            }
            // 丢弃未办结任务（作废）；已办结任务保留供归档。
            snapshot.tasks.retain(|t| t.completed);
            // 收口所有未完成的多实例域。
            for s in snapshot.mi_scopes.iter_mut() {
                s.finished = true;
            }
            // 清空所有未触发的定时器作业（实例已终止，定时器无意义）。
            snapshot.jobs.clear();
            // 清空候选池（实例已终止，待认领无意义）。
            snapshot.candidates.clear();
            if let Some(r) = reason {
                snapshot.instance.variables.set("_cancelReason", json!(r));
            }
            snapshot.async_jobs.clear();
            let _ = self.store.delete_async_jobs_by_instance(&snapshot.instance.id).await;
            snapshot.instance.state = InstanceState::Terminated;
            snapshot.instance.ended_at = Some(now);
            snapshot.instance.updated_at = now;

            self.store.save_snapshot(&snapshot).await?;
            // P3：实例终止，清理全部消息订阅（Catch 类型；Start 类型随定义，不清理）。
            let _ = self.store.delete_subscriptions_by_instance(&snapshot.instance.id).await;
            Ok(Self::result_of(&snapshot))
        })
    }

    /// 撤回 / 取回（④）：发起人在下游未处理时把流程拉回发起处，可改后重交。实例保持 ACTIVE。
    ///
    /// 护栏见 [`withdraw_denial`]（实例 ACTIVE + by_user 是发起人 + strict/lenient 策略判据）。
    /// 落点 = start 后首个 userTask，**回派给发起人**（绕过 run_to_wait 候选解析，确保归发起人），
    /// 令牌 Waiting 在此；发起人改数据后 complete 即沿正常流程重新前进。记 WITHDRAW 台账。
    pub async fn withdraw_process(
        &self,
        instance_id: &str,
        by_user: &str,
        reason: Option<&str>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        if let Some(deny) = withdraw_denial(&snapshot, &def, by_user) {
            return Err(Error::TaskNotActionable(deny));
        }
        let landing = first_user_task_from_start(&def).ok_or_else(|| {
            Error::TaskNotActionable("流程无用户任务节点，无处可退回发起人".into())
        })?;
        let now = self.clock.now();

        // 清理当前运行态（保留已办结任务供审计）。
        for tok in snapshot.tokens.iter_mut() {
            if tok.state != TokenState::Ended {
                tok.state = TokenState::Ended;
                tok.updated_at = now;
            }
        }
        snapshot.tasks.retain(|t| t.completed);
        for s in snapshot.mi_scopes.iter_mut() {
            s.finished = true;
        }
        snapshot.jobs.clear();
        snapshot.candidates.clear();

        // 落点：新 Waiting 令牌 + 回派发起人的任务（直接建，绕过 run_to_wait 的候选解析）。
        let landing_name = def.node_by_bpmn(&landing).and_then(|n| n.name.clone());
        let token_id = Uuid::new_v4().to_string();
        snapshot.tokens.push(Token {
            id: token_id.clone(),
            instance_id: instance_id.to_string(),
            node_bpmn_id: landing.clone(),
            state: TokenState::Waiting,
            parent_id: None,
            created_at: now,
            updated_at: now,
        });
        let task_id = Uuid::new_v4().to_string();
        snapshot.tasks.push(Task {
            id: task_id.clone(),
            instance_id: instance_id.to_string(),
            token_id,
            node_bpmn_id: landing,
            name: landing_name,
            assignee: Some(by_user.to_string()),
            candidate_groups: None,
            element_value: None,
            owner_user_id: None,
            parent_task_id: None,
            delegation_state: None,
            completed: false,
            created_at: now,
            completed_at: None,
        });
        snapshot.delegations.push(TaskDelegation {
            id: Uuid::new_v4().to_string(),
            task_id,
            instance_id: instance_id.to_string(),
            kind: "WITHDRAW".to_string(),
            from_user_id: by_user.to_string(),
            to_user_id: by_user.to_string(),
            temp_task_id: None,
            reason: reason.map(|s| s.to_string()),
            created_at: now,
        });
        snapshot.instance.updated_at = now;
        self.store.save_snapshot(&snapshot).await?;
        // P3：撤回产生新任务（landing），但不会经过 MessageCatchEvent，flush 是幂等无害的。
        self.flush_pending(&snapshot).await;
        Ok(Self::result_of(&snapshot))
    }

    /// 判断实例是否可被 by_user 撤回（④，只读）——供 /withdrawable 端点点亮/置灰按钮。
    pub async fn can_withdraw(
        &self,
        instance_id: &str,
        by_user: &str,
    ) -> Result<(bool, Option<String>)> {
        let snapshot = self.store.load_snapshot(instance_id).await?;
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        match withdraw_denial(&snapshot, &def, by_user) {
            Some(reason) => Ok((false, Some(reason))),
            None => Ok((true, None)),
        }
    }

    /// 推进所有到期的定时器作业（M2.5 的显式推进入口）。
    ///
    /// 引擎不自带后台线程——由宿主（demo/服务）定期调用本方法。流程：
    /// 取 now → 跨实例查到期作业 → 按实例分组 → 逐实例 load→fire→run_to_wait→save。
    /// 返回本轮触发的作业列表（供日志/展示）。`limit` 限制单轮处理的作业数，防雪崩。
    pub async fn trigger_due_timers(&self, limit: usize) -> Result<Vec<FiredTimer>> {
        let now = self.clock.now();
        // 008：抢占式领取（SKIP LOCKED 租约），多副本下同一作业只被一个副本 fire。
        let due = self
            .store
            // worker id = 进程 PID 唯一标识本副本（租约到期前其它副本不会重复领取）。
            .acquire_due_jobs(&format!("timer-{}", std::process::id()), now, 60, limit)
            .await?;
        if due.is_empty() {
            return Ok(Vec::new());
        }

        // 按实例分组（同实例的多个到期作业一次 load/save 处理）。
        let mut by_instance: HashMap<String, Vec<String>> = HashMap::new();
        for d in due {
            by_instance.entry(d.instance_id).or_default().push(d.job_id);
        }

        let mut fired = Vec::new();
        for (instance_id, job_ids) in by_instance {
            let mut snapshot = self.store.load_snapshot(&instance_id).await?;
            // 终态实例不再触发（防御：正常终态时 jobs 已清）。
            if snapshot.instance.state.is_terminal() {
                continue;
            }
            let def = self.get_definition(&snapshot.instance.definition_key)?;

            let mut any_fired = false;
            for job_id in job_ids {
                // 作业可能已被前一次 fire 的副作用移除（如中断型移走令牌顺带清理）。
                let Some(job_idx) = snapshot.jobs.iter().position(|j| j.id == job_id) else {
                    continue;
                };
                let job = snapshot.jobs[job_idx].clone();
                if fire_timer(&def, &mut snapshot, &job, now)? {
                    any_fired = true;
                    fired.push(FiredTimer {
                        job_id: job.id.clone(),
                        instance_id: instance_id.clone(),
                        boundary_bpmn_id: job.boundary_bpmn_id.clone(),
                        cancel_activity: job.cancel_activity,
                        instance_state: snapshot.instance.state,
                    });
                }
            }

            if any_fired {
                self.run_to_wait(&def, &mut snapshot, now).await?;
                // 触发后修正 fired 里的实例状态（run_to_wait 可能让实例完成）。
                for f in fired.iter_mut().filter(|f| f.instance_id == instance_id) {
                    f.instance_state = snapshot.instance.state;
                }
                // 技术债 007+008：CAS 冲突 = 该实例已被另一副本推进（多副本抢占语义）——
                // 放弃本批触发（warn 可观测），不向调用方报错；用户路径冲突由 handler 映射 409。
                match self.store.save_snapshot(&snapshot).await {
                    Ok(()) => {}
                    Err(cmx_flow_model::StoreError::Conflict(msg)) => {
                        tracing::warn!(instance = %instance_id, error = %msg,
                            "定时器触发保存冲突：实例已被并发推进，放弃本批触发（多副本抢占语义）");
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
                self.flush_pending(&snapshot).await;
                // A10：若定时器赢得事件网关竞速，清理同令牌挂起的消息订阅（竞争对手）。
                // fire_timer 已清同令牌的其它 TimerJob，这里补清消息订阅。
                let _ = self.store.delete_subscriptions_by_instance(&instance_id).await;
            }
        }
        Ok(fired)
    }

    // ============================ 推进内核 ============================

    /// 推进循环：把所有 Active 令牌推进到等待态或结束，就地修改快照。
    ///
    /// `now` 由调用方在入口取一次并贯穿整个推进段，保证同一段内时间一致（可测时钟友好）。
    async fn run_to_wait(
        &self,
        def: &ProcessDefinition,
        snapshot: &mut InstanceSnapshot,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps > STEP_LIMIT {
                return Err(Error::StepLimitExceeded(STEP_LIMIT));
            }

            // 找第一个可推进令牌。
            let Some(idx) = snapshot
                .tokens
                .iter()
                .position(|t| t.state == TokenState::Active)
            else {
                break;
            };

            // 取出本步所需信息（owned），随即释放对 def 的借用，规避跨 await 借用纠缠。
            let node_bpmn = snapshot.tokens[idx].node_bpmn_id.clone();
            let node = def
                .node_by_bpmn(&node_bpmn)
                .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
            let kind = node.kind.clone();
            let node_name = node.name.clone();
            let outgoing = node.outgoing.clone();

            match kind {
                NodeKind::StartEvent | NodeKind::MessageStartEvent(_) => {
                    // 消息启动事件（A2）在发起时作为入口，run_to_wait 里视同普通 StartEvent 透传。
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::SubProcess => {
                    // 嵌入子流程透传节点（A5）：有出边则沿之前进（透传到内部 start / 内部 end
                    // 接子流程出口）；无出边（内部 end 且子流程无出口=顶层结束）则令牌 Ended。
                    if let Some(flow) = outgoing.first() {
                        transition_token(snapshot, idx, def, flow.target_bpmn_id.clone(), now);
                    } else {
                        close_activity(snapshot, idx, def, now);
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::Ended;
                        tok.updated_at = now;
                    }
                }

                NodeKind::ServiceTask(ref st) => {
                    // P1：异步 serviceTask — 建 AsyncJob，令牌停 WaitingAsync，继续推进其它令牌。
                    // A7：external_topic 非空 = 外部 Worker 作业（delegate_key 空、带 topic），由外部进程
                    // 按 topic 拉取；否则是进程内异步 delegate。external_topic 编译期已强制 is_async=true。
                    if st.is_async {
                        let token_id = snapshot.tokens[idx].id.clone();
                        let job = cmx_flow_model::AsyncJob {
                            id: Uuid::new_v4().to_string(),
                            instance_id: snapshot.instance.id.clone(),
                            token_id: token_id.clone(),
                            node_bpmn_id: node_bpmn.clone(),
                            delegate_key: st.delegate.clone(),
                            topic: st.external_topic.clone(),
                            max_retries: 3,
                            retries: 3,
                            retry_backoff_seconds: Some(30),
                            locked_by: None,
                            lock_expires_at: None,
                            created_at: now,
                        };
                        snapshot.async_jobs.push(job);
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::WaitingAsync;
                        tok.updated_at = now;
                        continue;
                    }
                    // 同步路径（原有逻辑）：执行 delegate（可读写实例变量）。
                    let delegate = self
                        .delegates
                        .get(&st.delegate)
                        .ok_or_else(|| Error::DelegateNotFound(st.delegate.clone()))?;
                    let instance_id = snapshot.instance.id.clone();
                    let exec_result = {
                        let mut ctx = DelegateContext {
                            instance_id: &instance_id,
                            node_bpmn_id: &node_bpmn,
                            variables: &mut snapshot.instance.variables,
                        };
                        delegate.execute(&mut ctx).await
                    };
                    match exec_result {
                        Ok(()) => {
                            // 成功：清掉该节点可能存在的 incident 痕迹，沿唯一出边离开。
                            clear_incident(&mut snapshot.instance.variables, &node_bpmn);
                            let target = choose_target(
                                &node_bpmn,
                                &kind,
                                &outgoing,
                                &snapshot.instance.variables,
                            )?;
                            transition_token(snapshot, idx, def, target, now);
                        }
                        Err(reason) => {
                            // A8：类型化 BPMN 业务异常 → 找宿主上匹配的错误边界事件，命中则路由。
                            // 未命中 / Generic 失败 → 回退 Incident（原 H2 行为，实例保留待人工重试）。
                            if let DelegateError::Bpmn { ref code, .. } = reason {
                                if let Some(target) = error_boundary_target(def, &node_bpmn, code) {
                                    tracing::info!(
                                        instance = %instance_id, node = %node_bpmn, code = %code,
                                        target = %target, "serviceTask 抛 BPMN 错误 → 走错误边界"
                                    );
                                    clear_incident(&mut snapshot.instance.variables, &node_bpmn);
                                    transition_token(snapshot, idx, def, target, now);
                                    continue;
                                }
                                // A3：边界未命中 → 查进程级错误事件子流程（中断型）。
                                if let Some(es_target) =
                                    error_subprocess_target(def, code)
                                {
                                    tracing::info!(
                                        instance = %instance_id, node = %node_bpmn, code = %code,
                                        target = %es_target,
                                        "serviceTask 抛 BPMN 错误 → 中断主流程走错误事件子流程"
                                    );
                                    clear_incident(&mut snapshot.instance.variables, &node_bpmn);
                                    // 中断型：结束主流程全部未结束令牌（含本令牌）+ 作废未办结任务，
                                    // 清理定时器/候选/会签域（复用 TerminateEndEvent 的收口语义）。
                                    for t in snapshot.tokens.iter_mut() {
                                        if t.state != TokenState::Ended {
                                            t.state = TokenState::Ended;
                                            t.updated_at = now;
                                        }
                                    }
                                    for t in snapshot.tasks.iter_mut() {
                                        if !t.completed {
                                            t.completed = true;
                                            t.completed_at = Some(now);
                                        }
                                    }
                                    snapshot.candidates.clear();
                                    snapshot.jobs.clear();
                                    for s in snapshot.mi_scopes.iter_mut() {
                                        s.finished = true;
                                    }
                                    // 新令牌置于错误处理分支入口（ErrorStartEvent 的出边目标），Active 推进。
                                    let tok_id = Uuid::new_v4().to_string();
                                    snapshot.tokens.push(Token {
                                        id: tok_id,
                                        instance_id: snapshot.instance.id.clone(),
                                        node_bpmn_id: es_target,
                                        state: TokenState::Active,
                                        parent_id: None,
                                        created_at: now,
                                        updated_at: now,
                                    });
                                    continue;
                                }
                            }
                            // 失败：**不再中断整体推进、不丢实例**——把本令牌停到 Incident 态，
                            // 记失败原因 + 累加重试次数（H2）。实例保持 Active，其余分支照跑；
                            // 人工 retry_incident 可恢复。这是生产可用性硬门槛。
                            let msg = reason.message().to_string();
                            let retries =
                                record_incident(&mut snapshot.instance.variables, &node_bpmn, &msg);
                            tracing::warn!(
                                instance = %instance_id, node = %node_bpmn, retries,
                                reason = %msg, "serviceTask 失败 → Incident（实例保留，待人工重试）"
                            );
                            let token_id = snapshot.tokens[idx].id.clone();
                            let tok = &mut snapshot.tokens[idx];
                            tok.state = TokenState::Incident;
                            tok.updated_at = now;
                            self.upsert_incident_aux(&snapshot, &node_bpmn, Some(&token_id), &msg, retries)
                                .await;
                        }
                    }
                }

                NodeKind::ExclusiveGateway => {
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::BusinessRuleTask(ref br) => {
                    // 决策表求值（A3）：查注册表 → 对当前变量求值 → 输出 merge 进实例变量 →
                    // 沿唯一出边继续（后续网关可用输出变量走条件分支）。同步、无 IO、不落等待态。
                    // 未注册决策表 → 视为「不写变量」宽容前进（避免硬失败卡流程；设计器校验挡在前）。
                    match self.get_decision(&br.decision_key) {
                        Some(table) => {
                            let res = evaluate_decision(&table, &snapshot.instance.variables)?;
                            // 变量历史（引擎派生）：决策输出逐 key 相对旧值 diff → pending_var_changes。
                            let derived = diff_derived_var_changes(
                                &snapshot.instance.id,
                                &res.outputs,
                                &snapshot.instance.variables,
                                "decision",
                                &node_bpmn,
                            );
                            snapshot.instance.variables.merge(res.outputs);
                            snapshot.pending_var_changes.extend(derived);
                            tracing::debug!(
                                node = %node_bpmn, decision = %br.decision_key,
                                matched = ?res.matched_rules, "决策表求值"
                            );
                        }
                        None => {
                            tracing::warn!(
                                node = %node_bpmn, decision = %br.decision_key,
                                "businessRuleTask 引用的决策表未注册，跳过求值"
                            );
                        }
                    }
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::ParallelGateway => {
                    // 并行网关 = 先 join（等入边令牌到齐）再 fork（分裂到所有出边）。
                    // 依 incoming_count 判断是否为合流点。
                    let incoming_count = def
                        .node_by_bpmn(&node_bpmn)
                        .map(|n| n.incoming_count)
                        .unwrap_or(1);
                    let instance_id = snapshot.instance.id.clone();

                    if incoming_count > 1 {
                        // —— join 相：本令牌先停到网关，标记 Joining —— //
                        {
                            let tok = &mut snapshot.tokens[idx];
                            tok.state = TokenState::Joining;
                            tok.updated_at = now;
                        }
                        // 数本网关已到齐的 Joining 令牌。
                        let arrived = snapshot
                            .tokens
                            .iter()
                            .filter(|t| {
                                t.state == TokenState::Joining && t.node_bpmn_id == node_bpmn
                            })
                            .count();
                        if arrived < incoming_count {
                            // 兄弟未到齐：本令牌驻留 Joining，转去处理其它 Active 令牌。
                            continue;
                        }
                        // 到齐：合并——保留一个令牌为幸存者，删除其余 Joining 兄弟。
                        let survivor_id = snapshot
                            .tokens
                            .iter()
                            .find(|t| t.state == TokenState::Joining && t.node_bpmn_id == node_bpmn)
                            .map(|t| t.id.clone())
                            .ok_or_else(|| {
                                Error::IllegalTokenState(format!(
                                    "并行网关 {node_bpmn} join 到齐但找不到幸存令牌"
                                ))
                            })?;
                        snapshot.tokens.retain(|t| {
                            !(t.state == TokenState::Joining
                                && t.node_bpmn_id == node_bpmn
                                && t.id != survivor_id)
                        });
                        let sidx = snapshot
                            .tokens
                            .iter()
                            .position(|t| t.id == survivor_id)
                            .ok_or_else(|| Error::IllegalTokenState("幸存令牌意外丢失".into()))?;
                        // A6：闭合幸存令牌在本网关的活动（join 汇合点），再 fork。
                        close_activity(snapshot, sidx, def, now);
                        fork_token(
                            &mut snapshot.tokens,
                            sidx,
                            &node_bpmn,
                            &outgoing,
                            &instance_id,
                            now,
                        )?;
                    } else {
                        // —— 纯 fork / 直通 —— //
                        // A6：闭合本令牌在网关的活动（fork 分裂点），再 fork。
                        close_activity(snapshot, idx, def, now);
                        fork_token(
                            &mut snapshot.tokens,
                            idx,
                            &node_bpmn,
                            &outgoing,
                            &instance_id,
                            now,
                        )?;
                    }
                }

                NodeKind::InclusiveGateway => {
                    // 包容网关：fork 按条件择「若干」出边；join 等所有可能到达的令牌到齐。
                    let incoming_count = def
                        .node_by_bpmn(&node_bpmn)
                        .map(|n| n.incoming_count)
                        .unwrap_or(1);
                    let instance_id = snapshot.instance.id.clone();

                    // join 相判定：入边 > 1 视为合流点（与并行一致的结构判据）。
                    if incoming_count > 1 {
                        // 本令牌先停 Joining。
                        {
                            let tok = &mut snapshot.tokens[idx];
                            tok.state = TokenState::Joining;
                            tok.updated_at = now;
                        }
                        // 放行条件：实例内再无「可能到达本网关」的其它非结束令牌
                        // （不含已停在本网关的 Joining 令牌）。对无环审批流正确。
                        let others_pending = snapshot.tokens.iter().any(|t| {
                            t.state != TokenState::Ended
                                && !(t.state == TokenState::Joining && t.node_bpmn_id == node_bpmn)
                                && can_reach(&def, &t.node_bpmn_id, &node_bpmn)
                        });
                        if others_pending {
                            // 还有分支在路上：本令牌驻留 Joining，转处理其它 Active 令牌。
                            continue;
                        }
                        // 到齐：合并所有停在本网关的 Joining 令牌为一个幸存者，再 fork。
                        let survivor_id = snapshot
                            .tokens
                            .iter()
                            .find(|t| t.state == TokenState::Joining && t.node_bpmn_id == node_bpmn)
                            .map(|t| t.id.clone())
                            .ok_or_else(|| {
                                Error::IllegalTokenState(format!(
                                    "包容网关 {node_bpmn} join 到齐但找不到幸存令牌"
                                ))
                            })?;
                        snapshot.tokens.retain(|t| {
                            !(t.state == TokenState::Joining
                                && t.node_bpmn_id == node_bpmn
                                && t.id != survivor_id)
                        });
                        let sidx = snapshot
                            .tokens
                            .iter()
                            .position(|t| t.id == survivor_id)
                            .ok_or_else(|| Error::IllegalTokenState("幸存令牌意外丢失".into()))?;
                        // A6：闭合幸存令牌在本包容网关的活动（join 汇合点）。
                        close_activity(snapshot, sidx, def, now);
                        fork_token_inclusive(
                            &mut snapshot.tokens,
                            sidx,
                            &node_bpmn,
                            &outgoing,
                            &instance_id,
                            &snapshot.instance.variables.clone(),
                            now,
                        )?;
                    } else {
                        // 纯 fork：按条件择若干出边分裂。
                        // A6：闭合本令牌在包容网关的活动（fork 分裂点）。
                        close_activity(snapshot, idx, def, now);
                        fork_token_inclusive(
                            &mut snapshot.tokens,
                            idx,
                            &node_bpmn,
                            &outgoing,
                            &instance_id,
                            &snapshot.instance.variables.clone(),
                            now,
                        )?;
                    }
                }

                NodeKind::UserTask(ref ut) => {
                    match &ut.multi_instance {
                        // —— 普通单实例用户任务（M1 路径 + M4.1 候选人解析） —— //
                        None => {
                            let token_id = snapshot.tokens[idx].id.clone();
                            let instance_id = snapshot.instance.id.clone();
                            let task_id = Uuid::new_v4().to_string();

                            // 候选人解析（M4.1）：有候选引用且已注入 resolver → 解析成用户集。
                            //   0 人：退回静态 assignee（宽容）；1 人：直派；≥2 人：落候选池待认领。
                            //   P0：带上下文（initiator/org），支持关系型候选（部门领导/发起人上级/本人）。
                            let rctx = Self::resolve_ctx_of(snapshot);
                            let resolved =
                                self.resolve_candidates(&ut.candidates, &rctx).await?;
                            let (assignee, candidates) = decide_assignment(
                                &task_id,
                                &instance_id,
                                &ut.candidates,
                                &resolved,
                                ut.assignee.clone(),
                            );

                            let task = Task {
                                id: task_id.clone(),
                                instance_id: instance_id.clone(),
                                token_id: token_id.clone(),
                                node_bpmn_id: node_bpmn.clone(),
                                name: node_name,
                                assignee,
                                candidate_groups: ut.candidate_groups.clone(),
                                element_value: None,
                                owner_user_id: None,
                                parent_task_id: None,
                                delegation_state: None,
                                completed: false,
                                created_at: now,
                                completed_at: None,
                            };
                            snapshot.tasks.push(task);
                            snapshot.candidates.extend(candidates);
                            let tok = &mut snapshot.tokens[idx];
                            tok.state = TokenState::Waiting;
                            tok.updated_at = now;
                            // 挂定时器：为附着在本 userTask 上的每个边界定时器建一个到期作业。
                            // 传实例变量供 timeDate/timeCycle/${expr} 定时器求值（A4/A5）。
                            attach_boundary_timers(
                                def,
                                &mut snapshot.jobs,
                                &instance_id,
                                &token_id,
                                &node_bpmn,
                                &snapshot.instance.variables,
                                now,
                            );
                        }
                        // —— 多实例（会签 / 或签）：展开子实例 —— //
                        Some(mi) => {
                            // 从变量取集合（元素快照定格，避免中途变量被改）。
                            let collection = read_mi_collection(
                                &snapshot.instance.variables,
                                &mi.collection_var,
                                &node_bpmn,
                            )?;
                            let total = collection.len();

                            // 空集合：MI 节点直接跳过（令牌沿唯一出边离开）。
                            if total == 0 {
                                tracing::warn!(
                                    node = %node_bpmn,
                                    var = %mi.collection_var,
                                    "多实例集合为空，跳过该节点"
                                );
                                let target = choose_target(
                                    &node_bpmn,
                                    &kind,
                                    &outgoing,
                                    &snapshot.instance.variables,
                                )?;
                                transition_token(snapshot, idx, def, target, now);
                                continue;
                            }

                            let instance_id = snapshot.instance.id.clone();
                            // 移除到达令牌：子实例用全新令牌承载，语义更清晰。
                            snapshot.tokens.remove(idx);

                            // 建域。顺序模式只展开第一个，next_index=1；并行全展开，next_index=total。
                            let expand_now = if mi.sequential { 1 } else { total };
                            let scope = MiScope {
                                id: Uuid::new_v4().to_string(),
                                instance_id: instance_id.clone(),
                                node_bpmn_id: node_bpmn.clone(),
                                sequential: mi.sequential,
                                total,
                                completed: 0,
                                next_index: expand_now,
                                collection: collection.clone(),
                                element_var: mi.element_var.clone(),
                                completion_condition: mi.completion_condition.clone(),
                                finished: false,
                            };

                            snapshot.mi_scopes.push(scope);
                            // 逐元素展开：每个子任务按其元素求值办理人（②）。
                            for (i, element) in
                                collection.iter().take(expand_now).enumerate()
                            {
                                self.expand_mi_element(
                                    snapshot,
                                    &node_bpmn,
                                    &node_name,
                                    ut,
                                    element.clone(),
                                    i,
                                    now,
                                )
                                .await?;
                            }
                        }
                    }
                }

                NodeKind::EndEvent => {
                    // A6：闭合 endEvent 自身活动（令牌到此即终）。
                    close_activity(snapshot, idx, def, now);
                    let tok = &mut snapshot.tokens[idx];
                    tok.state = TokenState::Ended;
                    tok.updated_at = now;
                }

                NodeKind::TerminateEndEvent => {
                    // 一票否决（A2）：本令牌 Ended，并杀掉本实例所有其它未结束令牌 + 丢弃未办结任务，
                    // 收口未完成的多实例域。之后 run_to_wait 收尾判据（全令牌 Ended）即把实例 Completed。
                    for (i, tok) in snapshot.tokens.iter_mut().enumerate() {
                        if i == idx || tok.state != TokenState::Ended {
                            tok.state = TokenState::Ended;
                            tok.updated_at = now;
                        }
                    }
                    // 未办结任务作废（保留已办结的供归档）；候选/定时器/会签域清理。
                    for t in snapshot.tasks.iter_mut() {
                        if !t.completed {
                            t.completed = true;
                            t.completed_at = Some(now);
                        }
                    }
                    snapshot.candidates.clear();
                    snapshot.jobs.clear();
                    for s in snapshot.mi_scopes.iter_mut() {
                        s.finished = true;
                    }
                    tracing::info!(
                        instance = %snapshot.instance.id,
                        "TerminateEndEvent 一票否决：终止全部令牌与待办"
                    );
                }

                NodeKind::BoundaryTimerEvent(_) => {
                    // 边界定时器被触发后，令牌被置于此节点转 Active；这里像直通节点一样
                    // 沿其唯一出边离开（走升级/催办分支）。正常推进不会「主动」到达边界事件，
                    // 只有 fire_timer 把令牌放到这里才会走到本臂。
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::BoundaryErrorEvent(_) => {
                    // 边界错误事件（A8）：serviceTask 抛 BPMN 错误时，error_boundary_target 把令牌置于
                    // 此节点转 Active。这里像直通节点一样沿唯一出边离开走错误处理分支。正常推进不会
                    // 主动到达（无入边），只有错误路由把令牌放来才走到本臂。
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::ErrorStartEvent(_) => {
                    // 错误启动事件（A3）：主流程抛 BPMN 错误且无边界捕获时，引擎把令牌置于此节点转
                    // Active（见错误路由）。像直通节点一样沿唯一出边进入事件子流程处理分支。无入边，
                    // 正常推进不会主动到达。
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    transition_token(snapshot, idx, def, target, now);
                }

                NodeKind::EventSubProcess => {
                    // 事件子流程（A3）：透传标记节点，无入边，正常推进不会到达。防御性：若令牌意外落此
                    // （不应发生），沿出边或直接结束，不 panic。
                    if let Some(flow) = outgoing.first() {
                        transition_token(snapshot, idx, def, flow.target_bpmn_id.clone(), now);
                    } else {
                        close_activity(snapshot, idx, def, now);
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::Ended;
                        tok.updated_at = now;
                    }
                }

                NodeKind::CallActivity(_) => {
                    // 子流程调用是等待态：令牌挂起为 WaitingSubflow，停止推进本令牌（提交点）。
                    // 实际启动子实例在 run_to_wait 返回后的 launch_pending_subflows 里做——
                    // 那里可安全地 async 启动子实例（可能递归含孙流程），不与本同步循环纠缠。
                    let tok = &mut snapshot.tokens[idx];
                    tok.state = TokenState::WaitingSubflow;
                    tok.updated_at = now;
                }

                NodeKind::EventBasedGateway => {
                    // 事件网关（A10）：「谁先到走谁」竞速网关。
                    // 令牌停在网关节点（WaitingEventGateway），引擎为每条出边的后继节点注册竞争事件：
                    //   - IntermediateTimerCatchEvent → 建 TimerJob（kind=IntermediateCatch）
                    //   - MessageCatchEvent → 写 MessageSubscription(Catch)
                    // 第一个触发的事件胜出：令牌从网关节点前进到胜出后继，其余竞争事件全部撤销。
                    let tok_id = snapshot.tokens[idx].id.clone();
                    let inst_id = snapshot.instance.id.clone();
                    let tenant = snapshot.instance.variables
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let gw_bpmn = node_bpmn.clone();
                    let mut arm_error: Option<Error> = None;

                    // 遍历每条出边，按后继节点类型注册竞争事件。
                    for sf in &outgoing {
                        let target_bpmn = &sf.target_bpmn_id;
                        let Some(target_node) = def.node_by_bpmn(target_bpmn) else {
                            continue;
                        };
                        match &target_node.kind {
                            NodeKind::IntermediateTimerCatchEvent(spec) => {
                                // 为定时竞争分支建一个 IntermediateCatch 类型的 TimerJob，
                                // boundary_bpmn_id 存目标捕获节点（触发时令牌前进到此节点再沿其出边）。
                                if let Err(e) = attach_intermediate_timer(
                                    &mut snapshot.jobs,
                                    &inst_id,
                                    &tok_id,
                                    target_bpmn,
                                    spec,
                                    &snapshot.instance.variables,
                                    now,
                                ) {
                                    arm_error = Some(e);
                                    break;
                                }
                            }
                            NodeKind::MessageCatchEvent(mc) => {
                                // 为消息竞争分支写 Catch 订阅（P3），
                                // node_bpmn_id 存目标捕获节点（触发时令牌前进到此节点）。
                                snapshot.pending_subs.push(MessageSubscription {
                                    id: Uuid::new_v4().to_string(),
                                    kind: MessageSubscriptionKind::Catch,
                                    message_name: mc.message_name.clone(),
                                    instance_id: Some(inst_id.clone()),
                                    token_id: Some(tok_id.clone()),
                                    node_bpmn_id: target_bpmn.clone(),
                                    correlation_var: mc.correlation_var.clone(),
                                    definition_key: None,
                                    tenant_id: tenant.clone(),
                                    created_at: now,
                                });
                            }
                            _ => {
                                tracing::warn!(
                                    gw = %gw_bpmn,
                                    target = %target_bpmn,
                                    "事件网关出边后继节点不是 Catch 事件，忽略"
                                );
                            }
                        }
                    }

                    if let Some(e) = arm_error {
                        // 任意竞争事件注册失败 → 令牌转 Incident，可 retry。
                        let retries = record_incident(
                            &mut snapshot.instance.variables,
                            &gw_bpmn,
                            &e.to_string(),
                        );
                        tracing::warn!(node = %gw_bpmn, retries, error = %e, "事件网关竞争事件注册失败");
                        let token_id = snapshot.tokens[idx].id.clone();
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::Incident;
                        tok.updated_at = now;
                        self.upsert_incident_aux(&snapshot, &gw_bpmn, Some(&token_id), &e.to_string(), retries)
                            .await;
                    } else {
                        // 所有竞争事件注册成功：令牌停在网关，等第一个事件触发。
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::WaitingEventGateway;
                        tok.updated_at = now;
                    }
                }

                NodeKind::MessageCatchEvent(mc) => {
                    // 消息中间捕获（A4）是等待态：令牌挂起为 WaitingMessage，停止推进（提交点），
                    // 等外部经 correlate_message 按消息名 + 相关键唤醒后沿唯一出边继续。
                    // P3：把订阅暂存入 pending_subs，调用方在 save_snapshot 后批量持久化到订阅表。
                    let tok_id = snapshot.tokens[idx].id.clone();
                    let inst_id = snapshot.instance.id.clone();
                    let tenant = snapshot.instance.variables
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    snapshot.pending_subs.push(MessageSubscription {
                        id: Uuid::new_v4().to_string(),
                        kind: MessageSubscriptionKind::Catch,
                        message_name: mc.message_name.clone(),
                        instance_id: Some(inst_id),
                        token_id: Some(tok_id),
                        node_bpmn_id: node_bpmn.clone(),
                        correlation_var: mc.correlation_var.clone(),
                        definition_key: None,
                        tenant_id: tenant,
                        created_at: now,
                    });
                    let tok = &mut snapshot.tokens[idx];
                    tok.state = TokenState::WaitingMessage;
                    tok.updated_at = now;
                }

                NodeKind::IntermediateTimerCatchEvent(spec) => {
                    // 中间定时捕获（A1）是等待态：为本令牌建一个 TimerJob，令牌挂起为 WaitingTimer，                    // 停止推进（提交点）。到期由 trigger_due_timers/fire_timer 就地转 Active 沿唯一出边前进。
                    let token_id = snapshot.tokens[idx].id.clone();
                    let instance_id = snapshot.instance.id.clone();
                    // 表达式求值失败（变量缺失/格式错）→ 令牌转 Incident，可 retry（不静默卡死）。
                    if let Err(e) = attach_intermediate_timer(
                        &mut snapshot.jobs,
                        &instance_id,
                        &token_id,
                        &node_bpmn,
                        &spec,
                        &snapshot.instance.variables,
                        now,
                    ) {
                        let retries =
                            record_incident(&mut snapshot.instance.variables, &node_bpmn, &e.to_string());
                        tracing::warn!(node = %node_bpmn, retries, error = %e, "中间定时器解析失败，令牌转 Incident");
                        let token_id = snapshot.tokens[idx].id.clone();
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::Incident;
                        tok.updated_at = now;
                        self.upsert_incident_aux(&snapshot, &node_bpmn, Some(&token_id), &e.to_string(), retries)
                            .await;
                    } else {
                        let tok = &mut snapshot.tokens[idx];
                        tok.state = TokenState::WaitingTimer;
                        tok.updated_at = now;
                    }
                }
            }
        }

        // 收尾：所有令牌 Ended → 实例完成。
        if !snapshot.tokens.is_empty()
            && snapshot.tokens.iter().all(|t| t.state == TokenState::Ended)
        {
            snapshot.instance.state = InstanceState::Completed;
            snapshot.instance.ended_at = Some(now);
            snapshot.instance.updated_at = now;
        } else {
            snapshot.instance.updated_at = now;
        }
        Ok(())
    }

    /// 从快照构建对外结果视图。
    /// 完成一个异步服务任务作业（P1）：worker 执行完 delegate 后调此方法，引擎把令牌转 Active 继续推进。
    pub async fn complete_async_job(
        &self,
        job_id: &str,
        variables: Variables,
    ) -> Result<Option<ExecutionResult>> {
        let pair = self
            .store
            .complete_async_job(job_id, Some(serde_json::to_value(&variables).unwrap_or_default()))
            .await?;
        let Some((instance_id, token_id)) = pair else {
            return Ok(None);
        };
        let mut snapshot = self.store.load_snapshot(&instance_id).await?;
        // 本作业已在 store 侧删除。若快照的 async_jobs 缓冲里仍留着它（内存实现按整快照存），
        // 必须先剔除——否则本段结束的 flush_pending_async_jobs 会把已完成作业重新写回抢占池，
        // 导致 worker 重复领取、delegate 二次执行。
        snapshot.async_jobs.retain(|j| j.id != job_id);
        let def = self.get_definition(&snapshot.instance.definition_key)?;
        let now = self.clock.now();
        // 先把 worker 写回的变量 merge 进实例变量（供出边条件求值）。
        for (k, v) in variables.iter() {
            snapshot.instance.variables.set(k, v.clone());
        }
        // 令牌离开异步 serviceTask 节点，沿出边前进并转 Active。必须真正移出该节点，
        // 否则 run_to_wait 会再次命中 is_async 分支把它重新挂为 WaitingAsync（死循环挂起）。
        let token_idx = snapshot
            .tokens
            .iter()
            .position(|t| t.id == token_id)
            .ok_or_else(|| {
                Error::IllegalTokenState(format!("异步作业 {job_id} 的令牌 {token_id} 不存在"))
            })?;
        if snapshot.tokens[token_idx].state != TokenState::WaitingAsync {
            return Err(Error::IllegalTokenState(format!(
                "令牌 {token_id} 非 WaitingAsync，无法完成异步作业"
            )));
        }
        let node_bpmn = snapshot.tokens[token_idx].node_bpmn_id.clone();
        // delegate 已由 worker 成功执行 → 清掉该节点可能存在的 incident 痕迹（与同步路径一致）。
        clear_incident(&mut snapshot.instance.variables, &node_bpmn);
        let node = def
            .node_by_bpmn(&node_bpmn)
            .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
        let outgoing = node.outgoing.clone();
        let kind = node.kind.clone();
        let target = choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
        let tok = &mut snapshot.tokens[token_idx];
        tok.node_bpmn_id = target;
        tok.state = TokenState::Active;
        tok.updated_at = now;
        self.run_to_wait(&def, &mut snapshot, now).await?;
        self.store.save_snapshot(&snapshot).await?;
        self.flush_pending(&snapshot).await;
        self.flush_pending_async_jobs(&snapshot).await;
        Ok(Some(Self::result_of(&snapshot)))
    }

    /// 外部 worker 抢占一批异步作业（P1/A7）——只锁定并返回作业，不在进程内执行 delegate。
    ///
    /// 供 REST `POST /async-jobs/acquire`（无 topic，取进程内作业）与 `POST /external-worker/jobs/acquire`
    /// （A7，按 topic 取外部作业）：跨语言 worker 拉取作业、在自己进程执行外部调用，再回调
    /// `complete`/`fail` 端点。`topic_filter`：None=进程内作业（topic IS NULL），Some(t)=该 topic 外部作业。
    /// 集群安全同 `run_async_jobs`（store 侧 SKIP LOCKED）。
    pub async fn acquire_async_jobs(
        &self,
        worker_id: &str,
        topic_filter: Option<&str>,
        lock_secs: i64,
        limit: usize,
    ) -> Result<Vec<cmx_flow_model::AsyncJob>> {
        Ok(self
            .store
            .acquire_async_jobs(worker_id, topic_filter, lock_secs, limit)
            .await?)
    }

    /// 后台 worker 抢占并执行一批异步服务任务作业（P1 执行器主循环的单次调用）。
    ///
    /// 集群安全：多个 worker 并发调用各自的 `acquire_async_jobs`（SKIP LOCKED）拿到互不相交
    /// 的作业集，逐个执行 delegate —— 成功则 `complete_async_job` 推进令牌，失败则 `fail_async_job`
    /// 记一次重试（耗尽转死信）。返回本次**成功完成**的作业数。此方法对标 `trigger_due_timers`：
    /// 由外层调度（定时轮询 / 消息触发）反复调用即成常驻执行器。
    ///
    /// A7：进程内 poller 只领 `topic IS NULL` 的作业（topic_filter=None）——外部 worker 作业永不被
    /// 进程内领走（否则无 delegate 会误判失败）。
    pub async fn run_async_jobs(
        &self,
        worker_id: &str,
        lock_secs: i64,
        limit: usize,
    ) -> Result<usize> {
        let jobs = self
            .store
            .acquire_async_jobs(worker_id, None, lock_secs, limit)
            .await?;
        let mut completed = 0usize;
        for job in jobs {
            // 载入宿主实例取当前变量供 delegate 读写。实例已不存在（被取消/删除）→ 跳过，
            // 锁到期后作业自然可被清理；不因单个孤儿作业中断整批。
            let snapshot = match self.store.load_snapshot(&job.instance_id).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut vars = snapshot.instance.variables.clone();
            let exec_result = match self.delegates.get(&job.delegate_key) {
                Some(delegate) => {
                    let mut ctx = DelegateContext {
                        instance_id: &job.instance_id,
                        node_bpmn_id: &job.node_bpmn_id,
                        variables: &mut vars,
                    };
                    delegate.execute(&mut ctx).await
                }
                None => Err(DelegateError::Generic(format!(
                    "未注册 delegate: {}",
                    job.delegate_key
                ))),
            };
            match exec_result {
                Ok(()) => {
                    // 成功：delegate 产出的变量交给 complete_async_job 合并并推进令牌。
                    self.complete_async_job(&job.id, vars).await?;
                    completed += 1;
                }
                Err(e) => {
                    // 失败：委托给公共 fail 路径（retries-1 + 释放锁；耗尽转 Incident）。
                    // 与外部 worker 的 REST fail 端点共用同一实现，避免行为分叉。
                    // 注：async serviceTask 的错误边界路由本轮不支持（A8 仅同步路径）——
                    //     async 失败统一走重试/死信，Bpmn 与 Generic 同等对待，取消息落库。
                    if let Err(fe) = self.fail_async_job(&job.id, e.message()).await {
                        tracing::warn!(job_id = %job.id, error = %fe, "标记异步作业失败出错");
                    }
                }
            }
        }
        Ok(completed)
    }

    /// 标记一个异步作业失败（P1）——重试次数 -1 并释放锁；重试耗尽则转 Incident。
    ///
    /// 供 in-process poller 与外部 worker 的 REST fail 端点共用。外部 worker 只持 `job_id`，
    /// 故先读作业取令牌坐标（`fail_async_job` 耗尽时会删行，坐标须提前捕获），再执行失败逻辑。
    /// 返回 `true` = 仍可重试（作业留库待重抢），`false` = 已耗尽死信（令牌转 Incident 或作业已不存在）。
    pub async fn fail_async_job(&self, job_id: &str, error: &str) -> Result<bool> {
        // 先捕获令牌坐标（耗尽删行前）。作业不存在 → 无可失败，视为已处理。
        let Some(job) = self.store.get_async_job(job_id).await? else {
            return Ok(false);
        };
        let still_retryable = self.store.fail_async_job(job_id, error).await?;
        if still_retryable {
            // 仍可重试：作业留库、锁已释放，令牌保持 WaitingAsync 待下一轮抢占。
            return Ok(true);
        }
        // 重试耗尽，作业已死信删除——令牌若仍停 WaitingAsync 会永久卡死（再无作业能唤醒它），
        // 故转 Incident：令牌停在故障节点、实例不丢，运维台可见失败原因，retry_incident 可重发。
        // 同时在死信表落一条托底记录（P2）：与 Incident 两视图一致——实例卡在故障节点、死信表存
        // 作业级明细（原 delegate/节点/令牌 + 末次错误），运维可 retry_dead_letter_job 精确重投。
        let dl = cmx_flow_model::DeadLetterJob {
            id: job.id.clone(),
            instance_id: job.instance_id.clone(),
            token_id: job.token_id.clone(),
            node_bpmn_id: job.node_bpmn_id.clone(),
            delegate_key: job.delegate_key.clone(),
            max_retries: job.max_retries,
            error: error.to_string(),
            original_created_at: job.created_at,
            dead_lettered_at: self.clock.now(),
            tenant_id: "default".to_string(),
        };
        if let Err(e) = self.store.upsert_dead_letter_job(&dl).await {
            tracing::warn!(job_id = %job.id, error = %e, "写入死信作业失败（非 fatal）");
        }
        if let Ok(mut snap) = self.store.load_snapshot(&job.instance_id).await {
            snap.async_jobs.retain(|j| j.id != job.id);
            let hit = snap
                .tokens
                .iter_mut()
                .find(|t| t.id == job.token_id)
                .filter(|t| t.state == TokenState::WaitingAsync)
                .map(|t| {
                    t.state = TokenState::Incident;
                    t.updated_at = self.clock.now();
                })
                .is_some();
            if hit {
                let retries = record_incident(&mut snap.instance.variables, &job.node_bpmn_id, error);
                snap.instance.updated_at = self.clock.now();
                self.store.save_snapshot(&snap).await?;
                self.upsert_incident_aux(&snap, &job.node_bpmn_id, Some(&job.token_id), error, retries)
                    .await;
            }
        }
        Ok(false)
    }

    /// 重投一条死信作业（P2）——运维把耗尽死信的作业重新投回抢占池。
    ///
    /// 重建一个 fresh `AsyncJob`（retries 恢复为原 max_retries）+ 令牌从 Incident 回 WaitingAsync，
    /// worker 下一轮 `acquire_async_jobs` 即可重抢执行。删除死信行 + 清 incident 痕迹。
    /// 幂等：死信作业不存在返回 `false`；宿主实例/令牌已不在（被取消）也安全返回 `false`。
    pub async fn retry_dead_letter_job(&self, job_id: &str) -> Result<bool> {
        let Some(dl) = self.store.get_dead_letter_job(job_id).await? else {
            return Ok(false);
        };
        let mut snap = match self.store.load_snapshot(&dl.instance_id).await {
            Ok(s) => s,
            // 宿主实例已不存在：删死信行避免悬挂，视为已处理。
            Err(_) => {
                let _ = self.store.delete_dead_letter_job(job_id).await;
                return Ok(false);
            }
        };
        // 令牌须仍在且停在故障节点（Incident 或 WaitingAsync）。已被其它路径推走则放弃重投。
        let Some(tok) = snap
            .tokens
            .iter_mut()
            .find(|t| t.id == dl.token_id && t.node_bpmn_id == dl.node_bpmn_id)
        else {
            let _ = self.store.delete_dead_letter_job(job_id).await;
            return Ok(false);
        };
        let now = self.clock.now();
        tok.state = TokenState::WaitingAsync;
        tok.updated_at = now;
        // 重建 AsyncJob（新 id，retries 满血）。
        let fresh = cmx_flow_model::AsyncJob {
            id: Uuid::new_v4().to_string(),
            instance_id: dl.instance_id.clone(),
            token_id: dl.token_id.clone(),
            node_bpmn_id: dl.node_bpmn_id.clone(),
            delegate_key: dl.delegate_key.clone(),
            // 死信重投重建进程内 delegate 作业（topic=None）。外部 worker 作业若耗尽死信，
            // 由外部侧自行重投更合理；此处保持简单（delegate_key 空 + topic None 会被 poller 领后无
            // delegate 再次失败，但 dead-letter 主要面向 delegate 作业，此路径可接受）。
            topic: None,
            max_retries: dl.max_retries,
            retries: dl.max_retries,
            retry_backoff_seconds: Some(30),
            locked_by: None,
            lock_expires_at: None,
            created_at: now,
        };
        snap.async_jobs.push(fresh.clone());
        clear_incident(&mut snap.instance.variables, &dl.node_bpmn_id);
        snap.instance.updated_at = now;
        self.store.save_snapshot(&snap).await?;
        self.store.upsert_async_job(&fresh).await?;
        self.store.delete_dead_letter_job(job_id).await?;
        tracing::info!(dead_letter = %job_id, new_job = %fresh.id, instance = %dl.instance_id, "死信作业已重投");
        Ok(true)
    }

    /// 丢弃一条死信作业（P2）——运维判定放弃，从死信表删除。宿主令牌保持 Incident（不自动推进）。
    /// 幂等：不存在也返回成功。
    pub async fn discard_dead_letter_job(&self, job_id: &str) -> Result<()> {
        self.store.delete_dead_letter_job(job_id).await?;
        Ok(())
    }

    /// 列出死信作业（P2 运维台）。
    pub async fn list_dead_letter_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<cmx_flow_model::DeadLetterJob>> {
        Ok(self.store.list_dead_letter_jobs(limit).await?)
    }

    fn result_of(snapshot: &InstanceSnapshot) -> ExecutionResult {
        let open_tasks = snapshot
            .tasks
            .iter()
            .filter(|t| !t.completed)
            .map(|t| TaskView {
                id: t.id.clone(),
                node_bpmn_id: t.node_bpmn_id.clone(),
                name: t.name.clone(),
                assignee: t.assignee.clone(),
            })
            .collect();
        ExecutionResult {
            instance_id: snapshot.instance.id.clone(),
            state: snapshot.instance.state,
            open_tasks,
        }
    }
}

/// 令牌离开当前节点、迁往目标节点，并闭合一条活动历史（A6）。
///
/// 这是**唯一**的节点迁移集中点：闭合离开节点的活动（enter = 令牌当前 updated_at = 它到达本
/// 节点的时刻；exit = now），把 ActivityRecord 追加到 `pending_activities`（推进段结束批量落库），
/// 再把令牌指向目标节点并刷新 updated_at（= 到达目标的 enter 时刻，供下次闭合用）。
/// 行为对 `move_token` 是超集：迁移语义完全一致，只多一个「记账」副作用。
/// `def` 用于解析离开节点的名/类型；节点不在定义中（理论不该发生）则跳过记账、仅迁移。
fn transition_token(
    snapshot: &mut InstanceSnapshot,
    idx: usize,
    def: &ProcessDefinition,
    target_bpmn_id: String,
    now: DateTime<Utc>,
) {
    close_activity(snapshot, idx, def, now);
    let tok = &mut snapshot.tokens[idx];
    tok.node_bpmn_id = target_bpmn_id;
    tok.updated_at = now;
}

/// 引擎派生变量变更 diff：把即将 merge 进实例的 `incoming` 逐 key 与 `old` 现值比对，
/// 变化的（含新增）产出一条 `VarChangeRecord`（未变的 key 跳过，避免噪声）。
///
/// 与 app 层 `diff_var_changes` 同构（此处针对引擎侧 `Variables`，`by` 恒为 system 故不带此字段）。
/// 在 merge **之前**调用——此时 `old` 尚为旧值；返回后调用方 merge 并 `extend` 到 pending。
fn diff_derived_var_changes(
    instance_id: &str,
    incoming: &Variables,
    old: &Variables,
    source: &str,
    node_bpmn_id: &str,
) -> Vec<cmx_flow_model::VarChangeRecord> {
    let mut out = Vec::new();
    for (k, v) in incoming.iter() {
        let old_v = old.get(k).map(|x| x.to_string());
        let new_v = Some(v.to_string());
        if old_v == new_v {
            continue;
        }
        out.push(cmx_flow_model::VarChangeRecord {
            instance_id: instance_id.to_string(),
            var_name: k.clone(),
            old_value: old_v,
            new_value: new_v,
            source: source.to_string(),
            node_bpmn_id: Some(node_bpmn_id.to_string()),
        });
    }
    out
}

/// 闭合令牌 `idx` 当前所在节点的活动历史（A6）——不迁移令牌，仅记账。
///
/// 供 `transition_token`（迁移前闭合离开节点）与令牌进入终态（Ended，无目标节点可迁）时调用。
/// 从令牌读 (node_bpmn_id, updated_at=enter)，配合 def 解析名/类型 + 找该令牌绑定任务的 assignee，
/// 生成一条 [entered, now] 的 ActivityRecord。enter==now（零时长穿透节点）也记，保留完整轨迹。
fn close_activity(
    snapshot: &mut InstanceSnapshot,
    idx: usize,
    def: &ProcessDefinition,
    now: DateTime<Utc>,
) {
    let tok = &snapshot.tokens[idx];
    let node_bpmn = tok.node_bpmn_id.clone();
    let entered_at = tok.updated_at;
    let token_id = tok.id.clone();
    let instance_id = tok.instance_id.clone();
    let (name, type_str) = match def.node_by_bpmn(&node_bpmn) {
        Some(n) => (n.name.clone(), n.kind.type_str()),
        None => (None, "unknown"),
    };
    // userTask 活动记 assignee（供按人聚合工作量）：找该令牌当前/最近绑定的任务。
    let assignee = snapshot
        .tasks
        .iter()
        .filter(|t| t.token_id == token_id && t.node_bpmn_id == node_bpmn)
        .filter_map(|t| t.assignee.clone())
        .next();
    let duration_ms = (now - entered_at).num_milliseconds().max(0);
    snapshot.pending_activities.push(cmx_flow_model::ActivityRecord {
        id: Uuid::new_v4().to_string(),
        instance_id,
        token_id,
        activity_bpmn_id: node_bpmn,
        activity_name: name,
        activity_type: type_str.to_string(),
        entered_at,
        exited_at: now,
        duration_ms,
        assignee,
        tenant_id: "default".to_string(),
    });
}

/// 记一条 incident 到实例变量 `__incident`（JSON 对象，按节点 bpmn_id 聚合）。返回该节点累计重试次数。
///
/// 结构：`{"__incident": {"<node>": {"reason": "...", "retries": N, "lastAt": "..."}}}`。
/// 不新建表——incident 视图由 tokens(Incident 态) + 本变量派生，持久化随实例变量一起走。
fn record_incident(vars: &mut Variables, node_bpmn: &str, reason: &str) -> i64 {
    let mut root = match vars.get("__incident") {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    let prev = root
        .get(node_bpmn)
        .and_then(|v| v.get("retries"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let retries = prev + 1;
    root.insert(
        node_bpmn.to_string(),
        serde_json::json!({ "reason": reason, "retries": retries }),
    );
    vars.set("__incident", serde_json::Value::Object(root));
    retries
}

/// 清掉某节点的 incident 痕迹（成功执行后）。若清空则删掉 `__incident` 变量本身。
fn clear_incident(vars: &mut Variables, node_bpmn: &str) {
    if let Some(serde_json::Value::Object(m)) = vars.get("__incident") {
        if !m.contains_key(node_bpmn) {
            return;
        }
        let mut root = m.clone();
        root.remove(node_bpmn);
        if root.is_empty() {
            vars.set("__incident", serde_json::Value::Null);
        } else {
            vars.set("__incident", serde_json::Value::Object(root));
        }
    }
}

/// 找某节点的「直接前驱用户任务」（退回默认目标）。
///
/// 沿全体节点的出边反查指向 `node_bpmn` 的上游节点：
///   - 恰有一个前驱且是 userTask → 返回它；
///   - 前驱是网关/其它非人工节点 → 继续向其上游回溯（跳过网关，找到最近的 userTask）；
///   - 无前驱 / 多前驱无法确定 / 回溯不到 userTask → 报错，要求显式指定 target。
/// 从 `node_bpmn` 沿入边**反向遍历**，收集全部可达的上游 userTask（穿透网关/事件/服务任务）。
///
/// 用 visited 集天然防环、防重（无需深度 guard）。`distance` = 首次到达的最小反向跳数。
/// 结果按 distance 升序、同距按 bpmn_id 排序，便于前端「上 1 步 / 上 2 步」稳定展示。
/// `is_direct_predecessor` 由调用方据 `predecessor_user_task` 标注（此处恒 false）。
fn reachable_upstream_user_tasks(def: &ProcessDefinition, node_bpmn: &str) -> Vec<RejectTarget> {
    // 反向邻接：谁的出边指向 cur。
    fn preds(def: &ProcessDefinition, cur: &str) -> Vec<String> {
        let mut out = Vec::new();
        for n in &def.nodes {
            for e in &n.outgoing {
                if e.target_bpmn_id == cur {
                    out.push(n.bpmn_id.clone());
                }
            }
        }
        out
    }
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(node_bpmn.to_string());
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    queue.push_back((node_bpmn.to_string(), 0));
    let mut out: Vec<RejectTarget> = Vec::new();
    while let Some((cur, dist)) = queue.pop_front() {
        for p in preds(def, &cur) {
            if !visited.insert(p.clone()) {
                continue; // 已访问 → 跳过（防环/防重）。
            }
            if let Some(node) = def.node_by_bpmn(&p) {
                if matches!(node.kind, NodeKind::UserTask(_)) {
                    out.push(RejectTarget {
                        bpmn_id: p.clone(),
                        name: node.name.clone(),
                        is_direct_predecessor: false,
                        distance: dist + 1,
                    });
                }
                // 无论是否 userTask，都继续向上游穿透（跳过网关/事件找更早的 userTask）。
                queue.push_back((p, dist + 1));
            }
        }
    }
    out.sort_by(|a, b| a.distance.cmp(&b.distance).then_with(|| a.bpmn_id.cmp(&b.bpmn_id)));
    out
}

fn predecessor_user_task(def: &ProcessDefinition, node_bpmn: &str) -> Result<String> {
    // 反向邻接：谁的出边指向 cur。
    fn preds(def: &ProcessDefinition, cur: &str) -> Vec<String> {
        let mut out = Vec::new();
        for n in &def.nodes {
            for e in &n.outgoing {
                if e.target_bpmn_id == cur {
                    out.push(n.bpmn_id.clone());
                }
            }
        }
        out
    }
    let mut cur = node_bpmn.to_string();
    let mut guard = 0usize;
    loop {
        guard += 1;
        if guard > 64 {
            return Err(Error::TaskNotActionable(
                "退回目标回溯过深，请显式指定目标节点".into(),
            ));
        }
        let ps = preds(def, &cur);
        if ps.len() != 1 {
            return Err(Error::TaskNotActionable(format!(
                "节点 {node_bpmn} 前驱不唯一或无前驱（{} 个），请显式指定退回目标",
                ps.len()
            )));
        }
        let p = &ps[0];
        let pn = def
            .node_by_bpmn(p)
            .ok_or_else(|| Error::IllegalTokenState(format!("节点 {p} 不在定义中")))?;
        if matches!(pn.kind, NodeKind::UserTask(_)) {
            return Ok(p.clone());
        }
        // 前驱是网关/事件等非人工节点：继续向上游回溯，跳过它。
        cur = p.clone();
    }
}

/// 撤回护栏（④）：返回 None = 可撤回；Some(reason) = 拒绝原因。
///
/// 规则：实例 ACTIVE + `by_user` 是发起人（实例变量 `initiator`）+ 策略判据。
/// 策略 strict（默认）需下游完全未办结；lenient（`cmx:withdrawPolicy="lenient"`）只需当前未办结。
fn withdraw_denial(
    snapshot: &InstanceSnapshot,
    def: &ProcessDefinition,
    by_user: &str,
) -> Option<String> {
    if snapshot.instance.state != InstanceState::Active {
        return Some("实例非进行中，不可取回".into());
    }
    match snapshot
        .instance
        .variables
        .get("initiator")
        .and_then(|v| v.as_str())
    {
        Some(u) if u == by_user => {}
        Some(_) => return Some("仅发起人可取回本流程".into()),
        None => return Some("流程无发起人记录，无法确认取回权限".into()),
    }
    let lenient = def.withdraw_policy.as_deref() == Some("lenient");
    if !lenient && snapshot.tasks.iter().any(|t| t.completed) {
        return Some("下游已有环节办结，不可取回（strict 策略）".into());
    }
    None
}

/// start 后首个 userTask（撤回落点）——从 start 沿出边 BFS 找第一个用户任务。无则 None。
fn first_user_task_from_start(def: &ProcessDefinition) -> Option<String> {
    let start_bpmn = def.node(def.start).bpmn_id.clone();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(start_bpmn.clone());
    let mut q: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    q.push_back(start_bpmn);
    while let Some(cur) = q.pop_front() {
        let node = def.node_by_bpmn(&cur)?;
        for e in &node.outgoing {
            if !visited.insert(e.target_bpmn_id.clone()) {
                continue;
            }
            if let Some(tn) = def.node_by_bpmn(&e.target_bpmn_id) {
                if matches!(tn.kind, NodeKind::UserTask(_)) {
                    return Some(e.target_bpmn_id.clone());
                }
                q.push_back(e.target_bpmn_id.clone());
            }
        }
    }
    None
}

/// 变量映射（M5）：按 mappings 把 src 里的变量拷成一个新 Variables。
///
/// `full_if_empty=true` 且 mappings 为空 → 全量拷贝 src（默认行为）。否则只拷映射命中的，
/// source 缺失则跳过该条。
fn map_vars(
    src: &Variables,
    mappings: &[cmx_flow_model::VarMapping],
    full_if_empty: bool,
) -> Variables {
    if mappings.is_empty() {
        return if full_if_empty {
            src.clone()
        } else {
            Variables::new()
        };
    }
    let mut out = Variables::new();
    for m in mappings {
        if let Some(v) = src.get(&m.source) {
            out.set(m.target.clone(), v.clone());
        }
    }
    out
}

// ============================ 候选人解析 / 认领（M4.1） ============================

/// 依据候选引用与解析结果，决定任务的 assignee 与候选池。
///
/// 三种情形：
/// - **无候选引用**（candidates 空）：静态 assignee 老路，无候选记录（M1 零回归）。
/// - **解析出 0 人**：宽容退回静态 assignee（可能 resolver 未注入，或角色暂无人）。
/// - **解析出 1 人**：直派该人（assignee = 该用户），无候选记录。
/// - **解析出 ≥2 人**：assignee 留空（待认领），每人一条 TaskCandidate 落候选池。
///
/// 返回 `(assignee, candidates)`。candidates 记录来源类型（取第一条引用的 kind 作代表，
/// 简化；精确到人的来源可后续增强）。
fn decide_assignment(
    task_id: &str,
    instance_id: &str,
    refs: &[cmx_flow_model::CandidateRef],
    resolved: &[String],
    static_assignee: Option<String>,
) -> (Option<String>, Vec<TaskCandidate>) {
    if refs.is_empty() || resolved.is_empty() {
        // 无候选或未解析出人：静态 assignee 老路。
        return (static_assignee, Vec::new());
    }
    if resolved.len() == 1 {
        // 单人直派。
        return (Some(resolved[0].clone()), Vec::new());
    }
    // 多人候选：落候选池待认领。来源类型取首条引用 kind 作代表。
    let candidate_type = refs.first().map(|r| r.kind).unwrap_or(CandidateKind::User);
    let candidate_ref = refs.first().map(|r| r.value.clone()).unwrap_or_default();
    let pool = resolved
        .iter()
        .map(|uid| TaskCandidate {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            instance_id: instance_id.to_string(),
            candidate_type,
            candidate_ref: candidate_ref.clone(),
            resolved_user_id: uid.clone(),
        })
        .collect();
    (None, pool)
}

/// 为一组用户追加抄送记录（M4.2）。去重：同实例+同节点+同被抄送人只留一条未读，避免重复知会。
#[allow(clippy::too_many_arguments)]
fn append_cc_records(
    cc_records: &mut Vec<CcRecord>,
    instance_id: &str,
    node_bpmn: Option<&str>,
    from_user: Option<&str>,
    reason: Option<&str>,
    to_users: &[String],
    now: DateTime<Utc>,
) {
    for uid in to_users {
        // 去重：本节点已抄送过该人则跳过（幂等，防重复办结/重复触发生成多条）。
        let dup = cc_records
            .iter()
            .any(|c| c.to_user_id == *uid && c.node_bpmn_id.as_deref() == node_bpmn);
        if dup {
            continue;
        }
        cc_records.push(CcRecord {
            id: Uuid::new_v4().to_string(),
            instance_id: instance_id.to_string(),
            node_bpmn_id: node_bpmn.map(|s| s.to_string()),
            to_user_id: uid.clone(),
            from_user_id: from_user.map(|s| s.to_string()),
            reason: reason.map(|s| s.to_string()),
            read_at: None,
            created_at: now,
        });
    }
}

/// 追加一条转签台账（M4.3）。
#[allow(clippy::too_many_arguments)]
fn push_delegation(
    delegations: &mut Vec<TaskDelegation>,
    instance_id: &str,
    task_id: &str,
    kind: &str,
    from_user: &str,
    to_user: &str,
    temp_task_id: Option<&str>,
    reason: Option<&str>,
    now: DateTime<Utc>,
) {
    delegations.push(TaskDelegation {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        instance_id: instance_id.to_string(),
        kind: kind.to_string(),
        from_user_id: from_user.to_string(),
        to_user_id: to_user.to_string(),
        temp_task_id: temp_task_id.map(|s| s.to_string()),
        reason: reason.map(|s| s.to_string()),
        created_at: now,
    });
}

// ============================ 定时器（M2.5） ============================

/// 找一个匹配某 errorCode 的错误边界事件的出边目标（A8）。
///
/// 在宿主 `host_bpmn` 上找错误边界（`boundary_errors_on` 已按「精确 code 优先于 catch-all」排序），
/// 第一个满足「error_code == 抛出 code 或 error_code 为 None(catch-all)」的边界，返回其唯一出边目标。
/// 无匹配边界 / 匹配边界无出边 → None（调用方回退 Incident）。
fn error_boundary_target(
    def: &ProcessDefinition,
    host_bpmn: &str,
    thrown_code: &str,
) -> Option<String> {
    for boundary in def.boundary_errors_on(host_bpmn) {
        if let NodeKind::BoundaryErrorEvent(be) = &boundary.kind {
            let matches = match &be.error_code {
                Some(code) => code == thrown_code,
                None => true, // catch-all
            };
            if matches {
                // 错误边界的唯一出边 = 错误处理分支入口。
                if let Some(flow) = boundary.outgoing.first() {
                    return Some(flow.target_bpmn_id.clone());
                }
            }
        }
    }
    None
}

/// 找一个匹配某 errorCode 的**错误事件子流程**处理分支入口（A3）。
///
/// `error_event_subprocess_for` 取匹配的 ErrorStartEvent（精确 code 优先于 catch-all），返回其
/// 唯一出边目标（= 事件子流程处理分支第一个节点）。无匹配 / 无出边 → None（调用方回退 Incident）。
/// 供 serviceTask 抛错时在**边界事件未命中后**兜底：中断主流程，令牌走进程级错误处理。
fn error_subprocess_target(def: &ProcessDefinition, thrown_code: &str) -> Option<String> {
    let node = def.error_event_subprocess_for(thrown_code)?;
    node.outgoing.first().map(|f| f.target_bpmn_id.clone())
}

/// 令牌到达挂有边界定时器的 userTask 时，为每个边界定时器建一条到期作业。
///
/// due_at = now + 时长。中断/非中断由 BoundaryTimer.cancel_activity 决定，冗余存进作业。
fn attach_boundary_timers(
    def: &ProcessDefinition,
    jobs: &mut Vec<TimerJob>,
    instance_id: &str,
    token_id: &str,
    host_bpmn: &str,
    vars: &Variables,
    now: DateTime<Utc>,
) {
    for boundary in def.boundary_timers_on(host_bpmn) {
        if let NodeKind::BoundaryTimerEvent(bt) = &boundary.kind {
            // 解析定时器定义（A4/A5）：相对时长/绝对时刻/循环/变量表达式 → (到期时刻, 剩余重复次数)。
            // 表达式求值失败（变量缺失/格式错）不阻断任务创建，记 warn 跳过本定时器。
            let (due_at, cycle_remaining) = match bt.spec.resolve_due(vars, now) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(node = %boundary.bpmn_id, error = %e, "边界定时器时长解析失败，跳过");
                    continue;
                }
            };
            let cycle_interval_seconds = match &bt.spec {
                TimerSpec::Cycle { interval_seconds, .. } => Some(*interval_seconds),
                _ => None,
            };
            jobs.push(TimerJob {
                id: Uuid::new_v4().to_string(),
                instance_id: instance_id.to_string(),
                token_id: token_id.to_string(),
                boundary_bpmn_id: boundary.bpmn_id.clone(),
                cancel_activity: bt.cancel_activity,
                due_at,
                created_at: now,
                kind: TimerJobKind::Boundary,
                cycle_interval_seconds,
                cycle_remaining,
            });
        }
    }
}

/// 令牌到达中间定时捕获事件（A1）时建一条到期作业，令牌挂起为 WaitingTimer。
///
/// 与边界定时器的区别：kind=IntermediateCatch，`boundary_bpmn_id` 存本捕获节点自身 bpmn_id
/// （触发时就地转 Active 沿唯一出边前进，不重定位到别的节点）。TimerSpec 覆盖相对/绝对/循环/表达式。
fn attach_intermediate_timer(
    jobs: &mut Vec<TimerJob>,
    instance_id: &str,
    token_id: &str,
    catch_bpmn: &str,
    spec: &TimerSpec,
    vars: &Variables,
    now: DateTime<Utc>,
) -> Result<()> {
    let (due_at, cycle_remaining) = spec.resolve_due(vars, now).map_err(Error::from)?;
    let cycle_interval_seconds = match spec {
        TimerSpec::Cycle { interval_seconds, .. } => Some(*interval_seconds),
        _ => None,
    };
    jobs.push(TimerJob {
        id: Uuid::new_v4().to_string(),
        instance_id: instance_id.to_string(),
        token_id: token_id.to_string(),
        boundary_bpmn_id: catch_bpmn.to_string(),
        cancel_activity: false,
        due_at,
        created_at: now,
        kind: TimerJobKind::IntermediateCatch,
        cycle_interval_seconds,
        cycle_remaining,
    });
    Ok(())
}

/// 触发一个到期定时器作业。返回 true = 确实触发（false = 作业已失效被跳过，如宿主令牌不存在）。
///
/// - **中断型**（cancel_activity=true）：作废宿主令牌的未办结任务、删宿主令牌的全部定时器、
///   把宿主令牌置于边界事件节点转 Active（run_to_wait 会沿边界出边推进到升级分支）。
/// - **非中断型**（false）：只删这一个作业（单次触发不重复），新建一个 Active 令牌置于边界
///   事件节点（旁路分支，如催办），宿主令牌与其任务原样保留继续等待。
fn fire_timer(
    def: &ProcessDefinition,
    snapshot: &mut InstanceSnapshot,
    job: &TimerJob,
    now: DateTime<Utc>,
) -> Result<bool> {
    // 宿主令牌还在吗？（可能已被办结/取消移走——作业失效，丢弃。）
    let host_exists = snapshot.tokens.iter().any(|t| t.id == job.token_id);
    if !host_exists {
        // 清理失效作业本身。
        snapshot.jobs.retain(|j| j.id != job.id);
        return Ok(false);
    }

    // —— 中间定时捕获（A1）：令牌停在捕获节点等待，到期就地转 Active 沿其唯一出边前进 —— //
    // —— 同时处理事件网关（A10）：网关令牌通过定时器分支胜出，取消其它竞争事件 —— //
    if job.kind == TimerJobKind::IntermediateCatch {
        // 删本作业（单次触发）。
        snapshot.jobs.retain(|j| j.id != job.id);
        // 检查宿主令牌是否停在事件网关（A10 竞速），还是普通中间捕获（A1 直接前进）。
        let is_event_gw = snapshot.tokens.iter().any(|t| {
            t.id == job.token_id && t.state == TokenState::WaitingEventGateway
        });
        if is_event_gw {
            // A10 定时器分支胜出：取消同令牌的所有其它竞争 TimerJob（其它定时器/消息分支的 Job）。
            // 令牌直接前进到胜出 catch 节点的唯一出边目标（跳过 catch 节点本身，
            // 避免 run_to_wait 把它再当 IntermediateTimerCatchEvent 重新挂起）。
            snapshot.jobs.retain(|j| j.token_id != job.token_id);
            let target = def
                .node_by_bpmn(&job.boundary_bpmn_id)
                .and_then(|n| n.outgoing.first())
                .map(|f| f.target_bpmn_id.clone());
            if let (Some(target), Some(tok)) = (target, snapshot.token_mut(&job.token_id)) {
                tok.node_bpmn_id = target;
                tok.state = TokenState::Active;
                tok.updated_at = now;
            }
        } else {
            // A1 普通中间捕获：令牌前进到捕获节点的唯一出边目标。
            let target = def
                .node_by_bpmn(&job.boundary_bpmn_id)
                .and_then(|n| n.outgoing.first())
                .map(|f| f.target_bpmn_id.clone());
            if let (Some(target), Some(tok)) = (target, snapshot.token_mut(&job.token_id)) {
                tok.node_bpmn_id = target;
                tok.state = TokenState::Active;
                tok.updated_at = now;
            }
        }
        return Ok(true);
    }

    if job.cancel_activity {
        // —— 边界·中断型 —— //
        // 1) 作废宿主令牌承载的未办结任务。
        snapshot
            .tasks
            .retain(|t| t.token_id != job.token_id || t.completed);
        // 2) 删宿主令牌的全部定时器作业（含本作业与同宿主的其它边界定时器）。
        snapshot.jobs.retain(|j| j.token_id != job.token_id);
        // 3) 宿主令牌改置于边界事件节点，转 Active（后续 run_to_wait 沿边界出边推进）。
        if let Some(tok) = snapshot.token_mut(&job.token_id) {
            tok.node_bpmn_id = job.boundary_bpmn_id.clone();
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }
    } else {
        // —— 边界·非中断型 —— //
        // 1) 删本作业。若为周期定时器（timeCycle）且仍有剩余次数，则按间隔重排下一次（A5 周期催办）。
        snapshot.jobs.retain(|j| j.id != job.id);
        if let Some(interval) = job.cycle_interval_seconds {
            let keep_going = job.cycle_remaining.map(|r| r > 0).unwrap_or(true);
            if keep_going {
                snapshot.jobs.push(TimerJob {
                    id: Uuid::new_v4().to_string(),
                    instance_id: job.instance_id.clone(),
                    token_id: job.token_id.clone(),
                    boundary_bpmn_id: job.boundary_bpmn_id.clone(),
                    cancel_activity: false,
                    due_at: now + Duration::seconds(interval),
                    created_at: now,
                    kind: TimerJobKind::Boundary,
                    cycle_interval_seconds: Some(interval),
                    cycle_remaining: job.cycle_remaining.map(|r| r.saturating_sub(1)),
                });
            }
        }
        // 2) 新建一个旁路 Active 令牌置于边界事件节点（宿主任务不动，继续等待）。
        let instance_id = snapshot.instance.id.clone();
        snapshot.tokens.push(Token {
            id: Uuid::new_v4().to_string(),
            instance_id,
            node_bpmn_id: job.boundary_bpmn_id.clone(),
            state: TokenState::Active,
            parent_id: Some(job.token_id.clone()),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(true)
}

/// 并行网关 fork：让 `idx` 令牌走第一条出边，为其余每条出边各克隆一个新 Active 令牌。
///
/// 所有出边无条件全取（AND 语义）。新令牌复用同一 instance，parent_id 记为幸存令牌 id，
/// 便于将来（M3）做 scope 归属；M1/M2 的 join 靠 node_bpmn_id + incoming_count 计数，不依赖
/// parent_id，故此处仅作血缘留痕。
/// 包容网关 fork：只对**条件满足**的出边分裂令牌（全不满足走 default）。
///
/// 与 `fork_token`（并行，全分裂）的区别：先按 `eval_condition` 过滤出边。至少产生一个令牌。
fn fork_token_inclusive(
    tokens: &mut Vec<Token>,
    idx: usize,
    gateway_bpmn: &str,
    outgoing: &[SequenceFlow],
    instance_id: &str,
    vars: &Variables,
    now: DateTime<Utc>,
) -> Result<()> {
    // 选中满足条件的非 default 边；无条件边视为恒真。
    let mut chosen: Vec<&SequenceFlow> = Vec::new();
    for flow in outgoing {
        if flow.is_default {
            continue;
        }
        let passed = match &flow.condition {
            Some(expr) => eval_condition(expr, vars)?,
            None => true,
        };
        if passed {
            chosen.push(flow);
        }
    }
    // 全不满足 → 走 default（若有）。
    if chosen.is_empty() {
        if let Some(def_flow) = outgoing.iter().find(|f| f.is_default) {
            chosen.push(def_flow);
        } else {
            return Err(Error::NoOutgoingFlow {
                gateway: gateway_bpmn.to_string(),
            });
        }
    }
    let survivor_id = tokens[idx].id.clone();
    // 幸存令牌走第一条选中边并恢复 Active。
    {
        let tok = &mut tokens[idx];
        tok.node_bpmn_id = chosen[0].target_bpmn_id.clone();
        tok.state = TokenState::Active;
        tok.updated_at = now;
    }
    // 其余选中边各生成一个新 Active 令牌。
    for flow in chosen.iter().skip(1) {
        tokens.push(Token {
            id: Uuid::new_v4().to_string(),
            instance_id: instance_id.to_string(),
            node_bpmn_id: flow.target_bpmn_id.clone(),
            state: TokenState::Active,
            parent_id: Some(survivor_id.clone()),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(())
}

/// 从 `from` 节点沿出边能否到达 `target`（有向可达性，含自身；用于包容网关 join 判定
/// 「是否还有分支可能到达本网关」）。BFS，带访问集防环。
fn can_reach(def: &ProcessDefinition, from: &str, target: &str) -> bool {
    if from == target {
        return true;
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![from.to_string()];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if let Some(node) = def.node_by_bpmn(&cur) {
            for e in &node.outgoing {
                if e.target_bpmn_id == target {
                    return true;
                }
                stack.push(e.target_bpmn_id.clone());
            }
        }
    }
    false
}

fn fork_token(
    tokens: &mut Vec<Token>,
    idx: usize,
    gateway_bpmn: &str,
    outgoing: &[SequenceFlow],
    instance_id: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let first = outgoing.first().ok_or_else(|| {
        Error::IllegalTokenState(format!("并行网关 {gateway_bpmn} 无出边可 fork"))
    })?;
    let survivor_id = tokens[idx].id.clone();

    // 幸存令牌走第一条出边并恢复 Active。
    {
        let tok = &mut tokens[idx];
        tok.node_bpmn_id = first.target_bpmn_id.clone();
        tok.state = TokenState::Active;
        tok.updated_at = now;
    }

    // 其余出边各生成一个新 Active 令牌。
    for flow in outgoing.iter().skip(1) {
        tokens.push(Token {
            id: Uuid::new_v4().to_string(),
            instance_id: instance_id.to_string(),
            node_bpmn_id: flow.target_bpmn_id.clone(),
            state: TokenState::Active,
            parent_id: Some(survivor_id.clone()),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(())
}

/// 依据节点类型与变量，决定令牌离开时应走的目标节点 bpmn_id。
///
/// - 排他网关：按出边顺序求值条件，命中第一个 true；否则走 default；再无则报错。
/// - 其余（start/task）：唯一出边（拓扑校验已保证恰好一条）。
fn choose_target(
    node_bpmn: &str,
    kind: &NodeKind,
    outgoing: &[SequenceFlow],
    vars: &Variables,
) -> Result<String> {
    match kind {
        NodeKind::ExclusiveGateway => {
            for flow in outgoing {
                if flow.is_default {
                    continue;
                }
                let passed = match &flow.condition {
                    Some(expr) => eval_condition(expr, vars)?,
                    None => true, // 网关上无条件的非 default 边：视为恒真
                };
                if passed {
                    return Ok(flow.target_bpmn_id.clone());
                }
            }
            if let Some(default_flow) = outgoing.iter().find(|f| f.is_default) {
                return Ok(default_flow.target_bpmn_id.clone());
            }
            Err(Error::NoOutgoingFlow {
                gateway: node_bpmn.to_string(),
            })
        }
        _ => outgoing
            .first()
            .map(|f| f.target_bpmn_id.clone())
            .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 无出边可离开"))),
    }
}

// ============================ 多实例（会签 / 或签）辅助 ============================

/// 从实例变量读取多实例的集合元素（要求是 JSON 数组）。缺失变量视为空集合（total=0）。
fn read_mi_collection(
    vars: &Variables,
    collection_var: &str,
    node_bpmn: &str,
) -> Result<Vec<Value>> {
    match vars.get(collection_var) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(arr)) => Ok(arr.clone()),
        Some(other) => Err(Error::MultiInstance(format!(
            "多实例节点 {node_bpmn} 的集合变量 '{collection_var}' 应为数组，实际为 {other}"
        ))),
    }
}

/// 办结一个多实例子任务后的处理：计数 → 判 completionCondition → 收口或（顺序）返回待展开元素。
///
/// 收口（scope.finished）：作废本域其余未办结任务与其令牌，留一个「代表令牌」沿 MI 节点
/// 唯一出边离开并置 Active，交回推进循环。与 M2 join 的「幸存者 + 删兄弟」模式同构。
/// 顺序或签未收口：**不在此展开**（展开需 async 逐元素解析办理人），返回 `PendingMiExpand`
/// 交由 `complete_task`（有 `&self`）调 `expand_mi_element` 完成展开（②）。
fn complete_mi_task(
    def: &ProcessDefinition,
    snapshot: &mut InstanceSnapshot,
    node_bpmn: &str,
    completed_token_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<PendingMiExpand>> {
    // 该子任务的令牌置 Ended（其位置已由 Task.completed 记录，令牌使命结束）。
    if let Some(tok) = snapshot.token_mut(completed_token_id) {
        tok.state = TokenState::Ended;
        tok.updated_at = now;
    }

    // 更新域计数。
    let (sequential, finished_now, next_element) = {
        // 用下标定位域，便于与 instance.variables 做不相交字段借用
        // （completionCondition 需同时读实例变量 + nrOf* 计数）。
        let scope_idx = snapshot
            .mi_scopes
            .iter()
            .position(|s| s.node_bpmn_id == node_bpmn && !s.finished)
            .ok_or_else(|| Error::MultiInstance(format!("节点 {node_bpmn} 无活动多实例域")))?;
        snapshot.mi_scopes[scope_idx].completed += 1;

        let scope = &snapshot.mi_scopes[scope_idx];
        // 求值 completionCondition（实例变量 + 注入 nrOf* 计数）。
        let hit = match &scope.completion_condition {
            Some(cond) => eval_completion_condition(cond, scope, &snapshot.instance.variables)?,
            None => false,
        };
        // 自然完成：全部子实例办结。
        let all_done = scope.completed >= scope.total;
        let finished_now = hit || all_done;

        let scope = &mut snapshot.mi_scopes[scope_idx];
        // 顺序模式且未收口：取下一个待展开元素（连同其下标，供 loopCounter/办理人求值）。
        let next_element = if !finished_now && scope.sequential && scope.next_index < scope.total {
            let idx = scope.next_index;
            let el = scope.collection[idx].clone();
            scope.next_index += 1;
            Some((el, idx))
        } else {
            None
        };

        if finished_now {
            scope.finished = true;
        }
        (scope.sequential, finished_now, next_element)
    };

    if finished_now {
        // —— 收口：作废本域其余未办结任务 + 其令牌，留代表令牌沿出边离开 —— //
        finish_mi_scope(def, snapshot, node_bpmn, now)?;
        return Ok(None);
    }
    if sequential {
        // —— 顺序或签：算出下一个待展开元素，交回调用方 async 展开（②）—— //
        if let Some((element, index)) = next_element {
            let node = def
                .node_by_bpmn(node_bpmn)
                .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
            let node_name = node.name.clone();
            let ut = match &node.kind {
                NodeKind::UserTask(ut) => ut.clone(),
                other => {
                    return Err(Error::MultiInstance(format!(
                        "节点 {node_bpmn} 非 userTask，实际 {other:?}"
                    )));
                }
            };
            return Ok(Some(PendingMiExpand {
                node_bpmn: node_bpmn.to_string(),
                node_name,
                ut,
                element,
                index,
            }));
        }
    }
    // 并行会签且未收口：其余兄弟任务仍在等待，无需动作。
    Ok(None)
}

/// 顺序或签「待展开的下一个子实例」——由 `complete_mi_task` 算出、`complete_task` async 展开（②）。
struct PendingMiExpand {
    node_bpmn: String,
    node_name: Option<String>,
    ut: UserTask,
    element: Value,
    index: usize,
}

/// 收口一个多实例域：清理本域残留（未办结任务作废、其令牌删除），留一个代表令牌沿 MI
/// 节点唯一出边离开并置 Active。
fn finish_mi_scope(
    def: &ProcessDefinition,
    snapshot: &mut InstanceSnapshot,
    node_bpmn: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    // 收集本域仍未办结的任务，及其令牌 id。
    let leftover_token_ids: Vec<String> = snapshot
        .tasks
        .iter()
        .filter(|t| t.node_bpmn_id == node_bpmn && !t.completed)
        .map(|t| t.token_id.clone())
        .collect();

    // 作废未办结任务（从活动任务集移除）。
    snapshot
        .tasks
        .retain(|t| t.node_bpmn_id != node_bpmn || t.completed);
    // 删除这些作废任务对应的令牌。
    snapshot
        .tokens
        .retain(|t| !leftover_token_ids.contains(&t.id));

    // 代表令牌沿 MI 节点唯一出边离开。
    let node = def
        .node_by_bpmn(node_bpmn)
        .ok_or_else(|| Error::IllegalTokenState(format!("节点 {node_bpmn} 不在定义中")))?;
    let target = node
        .outgoing
        .first()
        .map(|f| f.target_bpmn_id.clone())
        .ok_or_else(|| Error::IllegalTokenState(format!("多实例节点 {node_bpmn} 无出边可离开")))?;

    let instance_id = snapshot.instance.id.clone();
    snapshot.tokens.push(Token {
        id: Uuid::new_v4().to_string(),
        instance_id,
        node_bpmn_id: target,
        state: TokenState::Active,
        parent_id: None,
        created_at: now,
        updated_at: now,
    });
    Ok(())
}

/// 求值 completionCondition：以实例变量为底，叠加 MiScope 的 nrOf* 计数再求值。
///
/// 底：实例变量（业务提交的 rejected/approved 等可被条件读到）。
/// 叠加的内置变量：nrOfInstances / nrOfCompletedInstances / nrOfActiveInstances。
/// nrOf* 是临时求值作用域，不落库、不污染实例主变量（用一份克隆叠加，不改原变量集）。
fn eval_completion_condition(
    cond: &str,
    scope: &MiScope,
    instance_vars: &Variables,
) -> Result<bool> {
    let mut vars = instance_vars.clone();
    vars.set("nrOfInstances", json!(scope.total));
    vars.set("nrOfCompletedInstances", json!(scope.completed));
    vars.set("nrOfActiveInstances", json!(scope.active_count()));
    eval_condition(cond, &vars).map_err(Error::from)
}
