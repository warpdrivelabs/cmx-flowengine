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

use cmx_flow_model::{
    AssigneeResolver, CandidateKind, CcRecord, InstanceSnapshot, InstanceState, MiScope, NodeKind,
    ProcessDefinition, ProcessInstance, RuntimeStore, SequenceFlow, Task, TaskCandidate,
    TaskDelegation, TimerJob, Token, TokenState, UserTask, Variables, expr::eval_condition,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::clock::{Clock, SystemClock};
use crate::delegate::{DelegateContext, DelegateRegistry, JavaDelegate};
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
pub struct Engine<S: RuntimeStore> {
    store: S,
    definitions: HashMap<String, ProcessDefinition>,
    delegates: DelegateRegistry,
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
            definitions: HashMap::new(),
            delegates: DelegateRegistry::new(),
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
    async fn resolve_candidates(
        &self,
        candidates: &[cmx_flow_model::CandidateRef],
    ) -> Result<Vec<String>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        match &self.resolver {
            Some(r) => r.resolve_all(candidates).await.map_err(Error::from),
            None => {
                tracing::warn!(
                    count = candidates.len(),
                    "userTask 含候选引用但未注入 AssigneeResolver，退回静态 assignee"
                );
                Ok(Vec::new())
            }
        }
    }

    /// 部署一份流程定义。会做 M1 拓扑子集校验（引擎声明自己支持的形态）。
    pub fn deploy(&mut self, def: ProcessDefinition) -> Result<()> {
        Self::check_topology(&def)?;
        tracing::info!(key = %def.key, nodes = def.nodes.len(), "部署流程定义");
        self.definitions.insert(def.key.clone(), def);
        Ok(())
    }

    /// 注册一个 serviceTask delegate。
    pub fn register_delegate(
        &mut self,
        key: impl Into<String>,
        delegate: impl JavaDelegate + 'static,
    ) {
        self.delegates.register_delegate(key, delegate);
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
                NodeKind::UserTask(_) | NodeKind::ServiceTask(_) => out == 1,
                // 排他网关：择一，至少一条出边。
                NodeKind::ExclusiveGateway => out >= 1,
                // 并行网关：作为 fork 需多出边，作为纯 join 出边可为 1；统一要求 >= 1。
                NodeKind::ParallelGateway => out >= 1,
                // 边界定时器事件：触发后沿唯一出边走升级/催办分支，需恰好一条出边。
                NodeKind::BoundaryTimerEvent(_) => out == 1,
                // 调用活动：子流程完成后沿唯一出边继续，需恰好一条出边。
                NodeKind::CallActivity(_) => out == 1,
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
        let snap = self
            .start_process_inner(
                definition_key,
                variables,
                business_key,
                org_id,
                None,
                None,
                None,
            )
            .await?;
        // 若起始段就走到 callActivity，挂起了 WaitingSubflow 令牌，启动其子实例。
        let snap = self.launch_subflows_for(&snap.instance.id).await?;
        Ok(Self::result_of(&snap))
    }

    /// 启动实例（可指定组织与父实例链接）。顶层实例 parent 为 None；子流程由 callActivity
    /// 调用时传入 parent_instance/parent_token，建立父子关系。返回落库后的子实例快照。
    #[allow(clippy::too_many_arguments)]
    async fn start_process_inner(
        &self,
        definition_key: &str,
        variables: Variables,
        business_key: Option<String>,
        org_id: Option<String>,
        parent_instance_id: Option<String>,
        parent_token_id: Option<String>,
        parent_node_bpmn_id: Option<String>,
    ) -> Result<InstanceSnapshot> {
        let def = self
            .definitions
            .get(definition_key)
            .ok_or_else(|| Error::DefinitionNotFound(definition_key.to_string()))?;

        let now = self.clock.now();
        let instance_id = Uuid::new_v4().to_string();
        let start_bpmn = def.node(def.start).bpmn_id.clone();

        let instance = ProcessInstance {
            id: instance_id.clone(),
            definition_key: definition_key.to_string(),
            business_key,
            state: InstanceState::Active,
            variables,
            created_at: now,
            updated_at: now,
            ended_at: None,
            org_id,
            parent_instance_id,
            parent_token_id,
            parent_node_bpmn_id,
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
            candidates: Vec::new(),
            cc_records: Vec::new(),
            delegations: Vec::new(),
        };

        self.run_to_wait(def, &mut snapshot, now).await?;
        self.store.create_snapshot(&snapshot).await?;
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
    async fn launch_one_subflow(
        &self,
        parent_snap: &InstanceSnapshot,
        parent_token_id: &str,
        node_bpmn: &str,
    ) -> Result<()> {
        let def = self
            .definitions
            .get(&parent_snap.instance.definition_key)
            .ok_or_else(|| {
                Error::DefinitionNotFound(parent_snap.instance.definition_key.clone())
            })?;
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
        // 子流程组织默认继承主实例。
        let org = parent_snap.instance.org_id.clone();

        // 解析子流程定义 key：
        // - called_key 非空（M5.2 逻辑名）→ 走 SubflowRouter 按组织路由；
        // - 否则用 called_element 写死值（M5.1 行为）。
        let sub_key = match &ca.called_key {
            Some(key) if !key.is_empty() => match &self.subflow_router {
                Some(router) => router
                    .resolve(key, org.as_deref())
                    .await
                    .map_err(Error::from)?,
                None => {
                    return Err(Error::DefinitionNotFound(format!(
                        "callActivity {node_bpmn} 用逻辑 key '{key}' 但未注入 SubflowRouter"
                    )));
                }
            },
            _ => ca.called_element.clone(),
        };
        if !self.definitions.contains_key(&sub_key) {
            return Err(Error::DefinitionNotFound(format!(
                "子流程 {sub_key}（callActivity {node_bpmn} 调用）未部署"
            )));
        }

        // 输入变量映射：主 → 子。空映射 = 全量传递。
        let child_vars = map_vars(&parent_snap.instance.variables, &ca.input_vars, true);

        let child_snap = self
            .start_process_inner(
                &sub_key,
                child_vars,
                parent_snap.instance.business_key.clone(),
                org,
                Some(parent_snap.instance.id.clone()),
                Some(parent_token_id.to_string()),
                Some(node_bpmn.to_string()),
            )
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
            let def = self
                .definitions
                .get(&parent.instance.definition_key)
                .ok_or_else(|| Error::DefinitionNotFound(parent.instance.definition_key.clone()))?
                .clone();

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
            parent.instance.variables.merge(back);

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
        let def = self
            .definitions
            .get(&snapshot.instance.definition_key)
            .ok_or_else(|| Error::DefinitionNotFound(snapshot.instance.definition_key.clone()))?;

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
            let cc_users = self.resolve_candidates(&cc_refs).await?;
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
            // —— 多实例（会签/或签）：计数、判完成条件、收口或展开下一个 —— //
            complete_mi_task(def, &mut snapshot, &node_bpmn, &token_id, now)?;
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
            let tok = &mut snapshot.tokens[token_idx];
            tok.node_bpmn_id = target;
            tok.state = TokenState::Active;
            tok.updated_at = now;
        }

        self.run_to_wait(def, &mut snapshot, now).await?;
        self.store.save_snapshot(&snapshot).await?;

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
        let cc_users = self.resolve_candidates(cc_refs).await?;
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

    /// 取消 / 终止一个流程实例（撤单、作废等审批刚需）。
    ///
    /// 语义：硬终止——杀掉全部未结束令牌、丢弃未办结任务、收口未完成的多实例域，实例转
    /// `Terminated`。已办结任务保留以供归档；save_snapshot 的 `is_terminal()` 路径会把实例
    /// 与已办结任务归档进历史表。M3 不做补偿（不回滚已执行的 serviceTask 副作用）。
    ///
    /// 幂等：已处于终态的实例直接返回其视图。
    pub async fn cancel_process(
        &self,
        instance_id: &str,
        reason: Option<String>,
    ) -> Result<ExecutionResult> {
        let mut snapshot = self.store.load_snapshot(instance_id).await?;
        if snapshot.instance.state.is_terminal() {
            return Ok(Self::result_of(&snapshot));
        }

        let now = self.clock.now();
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
        snapshot.instance.state = InstanceState::Terminated;
        snapshot.instance.ended_at = Some(now);
        snapshot.instance.updated_at = now;

        self.store.save_snapshot(&snapshot).await?;
        Ok(Self::result_of(&snapshot))
    }

    /// 推进所有到期的定时器作业（M2.5 的显式推进入口）。
    ///
    /// 引擎不自带后台线程——由宿主（demo/服务）定期调用本方法。流程：
    /// 取 now → 跨实例查到期作业 → 按实例分组 → 逐实例 load→fire→run_to_wait→save。
    /// 返回本轮触发的作业列表（供日志/展示）。`limit` 限制单轮处理的作业数，防雪崩。
    pub async fn trigger_due_timers(&self, limit: usize) -> Result<Vec<FiredTimer>> {
        let now = self.clock.now();
        let due = self.store.find_due_jobs(now, limit).await?;
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
            let def = self
                .definitions
                .get(&snapshot.instance.definition_key)
                .ok_or_else(|| Error::DefinitionNotFound(snapshot.instance.definition_key.clone()))?
                .clone();

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
                self.store.save_snapshot(&snapshot).await?;
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
                NodeKind::StartEvent => {
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    move_token(&mut snapshot.tokens[idx], target, now);
                }

                NodeKind::ServiceTask(ref st) => {
                    // 执行 delegate（可读写实例变量）。
                    let delegate = self
                        .delegates
                        .get(&st.delegate)
                        .ok_or_else(|| Error::DelegateNotFound(st.delegate.clone()))?;
                    let instance_id = snapshot.instance.id.clone();
                    {
                        let mut ctx = DelegateContext {
                            instance_id: &instance_id,
                            node_bpmn_id: &node_bpmn,
                            variables: &mut snapshot.instance.variables,
                        };
                        delegate
                            .execute(&mut ctx)
                            .await
                            .map_err(Error::DelegateFailed)?;
                    }
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    move_token(&mut snapshot.tokens[idx], target, now);
                }

                NodeKind::ExclusiveGateway => {
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    move_token(&mut snapshot.tokens[idx], target, now);
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

                NodeKind::UserTask(ref ut) => {
                    match &ut.multi_instance {
                        // —— 普通单实例用户任务（M1 路径 + M4.1 候选人解析） —— //
                        None => {
                            let token_id = snapshot.tokens[idx].id.clone();
                            let instance_id = snapshot.instance.id.clone();
                            let task_id = Uuid::new_v4().to_string();

                            // 候选人解析（M4.1）：有候选引用且已注入 resolver → 解析成用户集。
                            //   0 人：退回静态 assignee（宽容）；1 人：直派；≥2 人：落候选池待认领。
                            let resolved = self.resolve_candidates(&ut.candidates).await?;
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
                            attach_boundary_timers(
                                def,
                                &mut snapshot.jobs,
                                &instance_id,
                                &token_id,
                                &node_bpmn,
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
                                move_token(&mut snapshot.tokens[idx], target, now);
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

                            for element in collection.iter().take(expand_now) {
                                push_mi_sub_instance(
                                    snapshot,
                                    &node_bpmn,
                                    &node_name,
                                    ut,
                                    element.clone(),
                                    now,
                                );
                            }
                            snapshot.mi_scopes.push(scope);
                        }
                    }
                }

                NodeKind::EndEvent => {
                    let tok = &mut snapshot.tokens[idx];
                    tok.state = TokenState::Ended;
                    tok.updated_at = now;
                }

                NodeKind::BoundaryTimerEvent(_) => {
                    // 边界定时器被触发后，令牌被置于此节点转 Active；这里像直通节点一样
                    // 沿其唯一出边离开（走升级/催办分支）。正常推进不会「主动」到达边界事件，
                    // 只有 fire_timer 把令牌放到这里才会走到本臂。
                    let target =
                        choose_target(&node_bpmn, &kind, &outgoing, &snapshot.instance.variables)?;
                    move_token(&mut snapshot.tokens[idx], target, now);
                }

                NodeKind::CallActivity(_) => {
                    // 子流程调用是等待态：令牌挂起为 WaitingSubflow，停止推进本令牌（提交点）。
                    // 实际启动子实例在 run_to_wait 返回后的 launch_pending_subflows 里做——
                    // 那里可安全地 async 启动子实例（可能递归含孙流程），不与本同步循环纠缠。
                    let tok = &mut snapshot.tokens[idx];
                    tok.state = TokenState::WaitingSubflow;
                    tok.updated_at = now;
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

/// 把令牌移动到目标节点，保持 Active。
fn move_token(token: &mut Token, target_bpmn_id: String, now: DateTime<Utc>) {
    token.node_bpmn_id = target_bpmn_id;
    token.updated_at = now;
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

/// 令牌到达挂有边界定时器的 userTask 时，为每个边界定时器建一条到期作业。
///
/// due_at = now + 时长。中断/非中断由 BoundaryTimer.cancel_activity 决定，冗余存进作业。
fn attach_boundary_timers(
    def: &ProcessDefinition,
    jobs: &mut Vec<TimerJob>,
    instance_id: &str,
    token_id: &str,
    host_bpmn: &str,
    now: DateTime<Utc>,
) {
    for boundary in def.boundary_timers_on(host_bpmn) {
        if let NodeKind::BoundaryTimerEvent(bt) = &boundary.kind {
            let due_at = now + Duration::seconds(bt.duration.seconds);
            jobs.push(TimerJob {
                id: Uuid::new_v4().to_string(),
                instance_id: instance_id.to_string(),
                token_id: token_id.to_string(),
                boundary_bpmn_id: boundary.bpmn_id.clone(),
                cancel_activity: bt.cancel_activity,
                due_at,
                created_at: now,
            });
        }
    }
}

/// 触发一个到期定时器作业。返回 true = 确实触发（false = 作业已失效被跳过，如宿主令牌不存在）。
///
/// - **中断型**（cancel_activity=true）：作废宿主令牌的未办结任务、删宿主令牌的全部定时器、
///   把宿主令牌置于边界事件节点转 Active（run_to_wait 会沿边界出边推进到升级分支）。
/// - **非中断型**（false）：只删这一个作业（单次触发不重复），新建一个 Active 令牌置于边界
///   事件节点（旁路分支，如催办），宿主令牌与其任务原样保留继续等待。
fn fire_timer(
    _def: &ProcessDefinition,
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

    if job.cancel_activity {
        // —— 中断型 —— //
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
        // —— 非中断型 —— //
        // 1) 只删本作业（单次触发；不重建，避免周期重发）。
        snapshot.jobs.retain(|j| j.id != job.id);
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

/// 为多实例展开一个子实例：建一个 Waiting 令牌 + 一条携带当前元素的 Task。
///
/// element_value 存进 Task（每个办理人看到各自的数据）。variables 注入在办结时由业务提交，
/// M3 简化为把元素放在 Task.element_value 供前端展示与业务读取。
fn push_mi_sub_instance(
    snapshot: &mut InstanceSnapshot,
    node_bpmn: &str,
    node_name: &Option<String>,
    ut: &UserTask,
    element: Value,
    now: DateTime<Utc>,
) {
    let instance_id = snapshot.instance.id.clone();
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
        id: Uuid::new_v4().to_string(),
        instance_id,
        token_id,
        node_bpmn_id: node_bpmn.to_string(),
        name: node_name.clone(),
        assignee: ut.assignee.clone(),
        candidate_groups: ut.candidate_groups.clone(),
        element_value: Some(element),
        owner_user_id: None,
        parent_task_id: None,
        delegation_state: None,
        completed: false,
        created_at: now,
        completed_at: None,
    });
}

/// 办结一个多实例子任务后的处理：计数 → 判 completionCondition → 收口或展开下一个。
///
/// 收口（scope.finished）：作废本域其余未办结任务与其令牌，留一个「代表令牌」沿 MI 节点
/// 唯一出边离开并置 Active，交回推进循环。与 M2 join 的「幸存者 + 删兄弟」模式同构。
fn complete_mi_task(
    def: &ProcessDefinition,
    snapshot: &mut InstanceSnapshot,
    node_bpmn: &str,
    completed_token_id: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    // 该子任务的令牌置 Ended（其位置已由 Task.completed 记录，令牌使命结束）。
    if let Some(tok) = snapshot.token_mut(completed_token_id) {
        tok.state = TokenState::Ended;
        tok.updated_at = now;
    }

    // 更新域计数。
    let (sequential, finished_now, next_element, element_var) = {
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
        // 顺序模式且未收口：取下一个待展开元素。
        let next_element = if !finished_now && scope.sequential && scope.next_index < scope.total {
            let el = scope.collection[scope.next_index].clone();
            scope.next_index += 1;
            Some(el)
        } else {
            None
        };

        if finished_now {
            scope.finished = true;
        }
        (
            scope.sequential,
            finished_now,
            next_element,
            scope.element_var.clone(),
        )
    };

    if finished_now {
        // —— 收口：作废本域其余未办结任务 + 其令牌，留代表令牌沿出边离开 —— //
        finish_mi_scope(def, snapshot, node_bpmn, now)?;
    } else if sequential {
        // —— 顺序或签：展开下一个子实例 —— //
        if let Some(element) = next_element {
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
            let _ = element_var; // 元素随 Task.element_value 承载，见 push_mi_sub_instance
            push_mi_sub_instance(snapshot, node_bpmn, &node_name, &ut, element, now);
        }
    }
    // 并行会签且未收口：其余兄弟任务仍在等待，无需动作。
    Ok(())
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
