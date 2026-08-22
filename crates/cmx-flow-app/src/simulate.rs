//! 设计态流程模拟（simulate）——给样例变量**试跑**一份定义，算出令牌路径 / 网关分支 / userTask
//! 办理人 / businessRuleTask 决策输出，**不建持久实例、无副作用**。设计器「模拟」页签的后端。
//!
//! 复用引擎既有原语（零新业务逻辑）：`cmx_flow_bpmn::compile` 编译 XML→IR、`eval_condition` 求
//! sequenceFlow 条件（语义与引擎一致，勿在 JS 重写）、`decision::evaluate` 求决策表、
//! `AssigneeResolver::resolve_all_with` 解析候选人。网关三规则按引擎权威语义在此重表达：
//!   - 排他 exclusive：首个条件为真的非默认边，否则默认边；
//!   - 包容 inclusive：所有条件为真的非默认边，一个都不满足则默认边；
//!   - 并行 parallel：全部出边。
//! 遍历用工作队列（并行/包容 fork 探所有活分支），wait 态事件假设已触发继续（附 warning）。

use std::collections::{HashMap, VecDeque};

use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{AssigneeResolver, ResolveContext};
use cmx_flow_model::decision::evaluate as eval_decision;
use cmx_flow_model::{
    DecisionTable, FlowNode, NodeId, NodeKind, ProcessDefinition, SequenceFlow, Variables,
    eval_condition,
};
use cmx_flow_store_pg::PgIamAssigneeResolver;

use crate::engine::{current_iam_db_id, flow};
use crate::resp::{ApiResp, FlowError, Result};

/// 单条出边是否可走（None/空/"-" = 无条件恒真；否则求值，出错视为不可走）。
fn edge_open(sf: &SequenceFlow, vars: &Variables) -> bool {
    match sf.condition.as_deref().map(str::trim) {
        None | Some("") | Some("-") => true,
        Some(c) => eval_condition(c, vars).unwrap_or(false),
    }
}

/// 排他网关取边：首个条件为真的非默认边，否则默认边（对齐引擎 `choose_target`）。
fn exclusive_edges(node: &FlowNode, vars: &Variables) -> Vec<(String, NodeId)> {
    for sf in node.outgoing.iter().filter(|s| !s.is_default) {
        if edge_open(sf, vars) {
            return vec![(sf.bpmn_id.clone(), sf.target)];
        }
    }
    node.outgoing
        .iter()
        .find(|s| s.is_default)
        .map(|sf| vec![(sf.bpmn_id.clone(), sf.target)])
        .unwrap_or_default()
}

/// 包容网关取边：所有条件为真的非默认边；一个都不满足则默认边（对齐引擎 `fork_token_inclusive`）。
fn inclusive_edges(node: &FlowNode, vars: &Variables) -> Vec<(String, NodeId)> {
    let mut out: Vec<(String, NodeId)> = node
        .outgoing
        .iter()
        .filter(|s| !s.is_default && edge_open(s, vars))
        .map(|s| (s.bpmn_id.clone(), s.target))
        .collect();
    if out.is_empty()
        && let Some(sf) = node.outgoing.iter().find(|s| s.is_default)
    {
        out.push((sf.bpmn_id.clone(), sf.target));
    }
    out
}

/// 全部出边（并行网关 fork / 透传节点唯一出边 / 事件网关展开所有竞速分支）。
fn all_edges(node: &FlowNode) -> Vec<(String, NodeId)> {
    node.outgoing
        .iter()
        .map(|s| (s.bpmn_id.clone(), s.target))
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateReq {
    /// 直接给 BPMN XML（设计器发当前画布 XML，含未保存改动）。优先于 key+version。
    #[serde(default)]
    bpmn_xml: Option<String>,
    /// 或按 key + version 取已存版本 XML。
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    version: Option<i32>,
    /// 样例变量对象。
    #[serde(default)]
    variables: Value,
    /// 发起人 user_id（INITIATOR / orgLeader 类候选解析锚点）。
    #[serde(default)]
    initiator: Option<String>,
    /// 实例组织 id（部门领导默认锚点）。
    #[serde(default)]
    org_id: Option<String>,
}

/// POST /flow/definitions/simulate —— 设计态试跑，返回路径 + 分支 + 办理人 + 决策 trace。
pub async fn simulate_definition(Json(req): Json<SimulateReq>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;

    // 1) 取 XML：内联优先，否则 key+version 取已存版本。
    let xml = match req.bpmn_xml.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(x) => x.to_string(),
        None => {
            let k = req
                .key
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| FlowError::business("simulate 需 bpmnXml 或 key+version"))?;
            let v = req
                .version
                .ok_or_else(|| FlowError::business("按 key 模拟须给 version"))?;
            rt.def_svc
                .get_version(k, v)
                .await
                .map_err(|e| FlowError::business(format!("取版本失败: {e}")))?
                .map(|r| r.bpmn_xml)
                .ok_or_else(|| FlowError::business(format!("定义 {k} v{v} 不存在")))?
        }
    };

    // 2) 编译 XML → IR（丢弃编译不过的定义，报可读错误）。
    let def = compile(&xml).map_err(|e| FlowError::business(format!("编译失败: {e}")))?;

    // 3) 决策表 key→表（本轮①已落库；从 decision_store 装载，供 businessRuleTask 查表）。
    let mut decisions: HashMap<String, DecisionTable> = HashMap::new();
    if let Ok((tables, _errs)) = rt.decision_store.load_all().await {
        for t in tables {
            decisions.insert(t.key.clone(), t);
        }
    }

    // 4) 办理人解析器 + 上下文 + 变量。
    let resolver = PgIamAssigneeResolver::new(current_iam_db_id());
    let mut vars = Variables::from_json(req.variables.clone());
    if let Some(init) = req.initiator.as_deref()
        && vars.get("initiator").is_none()
    {
        vars.set("initiator", json!(init));
    }
    let initiator = req
        .initiator
        .clone()
        .or_else(|| vars.get("initiator").and_then(|v| v.as_str()).map(String::from));
    let ctx = ResolveContext {
        initiator,
        org_id: req.org_id.clone(),
    };

    // 5) 遍历。
    let trace = walk(&def, &mut vars, &decisions, &resolver, &ctx).await;
    Ok(Json(ApiResp::ok(trace)))
}

/// 工作队列遍历 IR，累积 trace（并行/包容 fork 探所有活分支；wait 态假设已触发继续）。
async fn walk(
    def: &ProcessDefinition,
    vars: &mut Variables,
    decisions: &HashMap<String, DecisionTable>,
    resolver: &PgIamAssigneeResolver,
    ctx: &ResolveContext,
) -> Value {
    let mut path: Vec<String> = Vec::new();
    let mut flows: Vec<String> = Vec::new();
    let mut gateways: Vec<Value> = Vec::new();
    let mut user_tasks: Vec<Value> = Vec::new();
    let mut service_tasks: Vec<Value> = Vec::new();
    let mut subflows: Vec<Value> = Vec::new();
    let mut decision_steps: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut end_reached = false;

    let mut visits: HashMap<usize, u32> = HashMap::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    queue.push_back(def.start);
    let mut steps = 0u32;

    while let Some(nid) = queue.pop_front() {
        steps += 1;
        if steps > 2000 {
            warnings.push("模拟步数超上限(2000)，路径可能不完整（是否有环？）".into());
            break;
        }
        let vc = visits.entry(nid.0).or_insert(0);
        *vc += 1;
        if *vc > 50 {
            continue; // 环/重复到达保护
        }
        let node = def.node(nid);
        path.push(node.bpmn_id.clone());
        let name = node.name.clone().unwrap_or_default();

        // 取边（网关按规则；其余透传全部出边）。
        let edges: Vec<(String, NodeId)> = match &node.kind {
            NodeKind::ExclusiveGateway => {
                let e = exclusive_edges(node, vars);
                if e.is_empty() {
                    warnings.push(format!("排他网关 {} 无可走分支（无匹配条件且无默认边）", node.bpmn_id));
                }
                gateways.push(json!({ "node": node.bpmn_id, "type": "exclusive", "taken": e.iter().map(|(f,_)| f.clone()).collect::<Vec<_>>() }));
                e
            }
            NodeKind::InclusiveGateway => {
                let e = inclusive_edges(node, vars);
                gateways.push(json!({ "node": node.bpmn_id, "type": "inclusive", "taken": e.iter().map(|(f,_)| f.clone()).collect::<Vec<_>>() }));
                e
            }
            NodeKind::ParallelGateway => {
                let e = all_edges(node);
                gateways.push(json!({ "node": node.bpmn_id, "type": "parallel", "taken": e.iter().map(|(f,_)| f.clone()).collect::<Vec<_>>() }));
                e
            }
            NodeKind::EventBasedGateway => {
                warnings.push(format!("事件网关 {}：真机为竞速等待，模拟展示所有分支", node.bpmn_id));
                all_edges(node)
            }
            NodeKind::UserTask(ut) => {
                let assignees = if !ut.candidates.is_empty() {
                    resolver.resolve_all_with(&ut.candidates, ctx).await.unwrap_or_default()
                } else if let Some(a) = ut.assignee.clone().filter(|s| !s.is_empty()) {
                    vec![a]
                } else {
                    Vec::new()
                };
                if assignees.is_empty() {
                    warnings.push(format!("用户任务 {} 未解析到办理人", node.bpmn_id));
                }
                user_tasks.push(json!({ "node": node.bpmn_id, "name": name, "assignees": assignees }));
                all_edges(node)
            }
            NodeKind::ServiceTask(st) => {
                service_tasks.push(json!({ "node": node.bpmn_id, "name": name, "delegate": st.delegate, "externalTopic": st.external_topic }));
                all_edges(node)
            }
            NodeKind::CallActivity(ca) => {
                let target = ca.called_key.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| ca.called_element.clone());
                subflows.push(json!({ "node": node.bpmn_id, "name": name, "target": target, "byKey": ca.called_key.is_some() }));
                all_edges(node)
            }
            NodeKind::BusinessRuleTask(br) => {
                match decisions.get(&br.decision_key) {
                    Some(table) => match eval_decision(table, vars) {
                        Ok(res) => {
                            let matched = res.matched_rules.clone();
                            let outs = res.outputs.to_json();
                            vars.merge(res.outputs);
                            decision_steps.push(json!({ "node": node.bpmn_id, "decisionKey": br.decision_key, "matchedRules": matched, "outputs": outs }));
                        }
                        Err(e) => warnings.push(format!("决策表 {} 求值失败: {e}", br.decision_key)),
                    },
                    None => warnings.push(format!("决策表 {} 未落库/未找到（先在设计器注册发布）", br.decision_key)),
                }
                all_edges(node)
            }
            NodeKind::EndEvent => {
                end_reached = true;
                Vec::new()
            }
            NodeKind::TerminateEndEvent => {
                end_reached = true;
                warnings.push(format!("到达终止事件 {}（真机会一票否决终止全实例）", node.bpmn_id));
                Vec::new()
            }
            NodeKind::MessageCatchEvent(_) | NodeKind::IntermediateTimerCatchEvent(_) => {
                warnings.push(format!("{} 为等待态（真机停等外部消息/定时器），模拟假设已触发继续", node.bpmn_id));
                all_edges(node)
            }
            // 透传：StartEvent / MessageStartEvent / SubProcess / EventSubProcess / 边界事件 / ErrorStart…
            _ => all_edges(node),
        };

        for (flow_id, target) in edges {
            flows.push(flow_id);
            queue.push_back(target);
        }
    }

    json!({
        "path": path,
        "flows": flows,
        "gateways": gateways,
        "userTasks": user_tasks,
        "serviceTasks": service_tasks,
        "subflows": subflows,
        "decisions": decision_steps,
        "endReached": end_reached,
        "warnings": warnings,
    })
}
