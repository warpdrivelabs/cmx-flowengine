/*
 * @Describe: 流程视图组装（纯函数，从 cmx-flow-demo 逐字移植，行为不变）。
 *
 * ProcessDefinition/InstanceSnapshot/InstanceSummary → 前端 JSON。前端依赖字段名
 * （key/name/nodes/edges/startable、id/state/tokens/tasks/activeNodes/openTasks 等），保持一致。
 */

use serde_json::{Value, json};

use cmx_flow_model::{
    CandidateKind, InstanceSnapshot, InstanceState, InstanceSummary, NodeKind, ProcessDefinition,
    TokenState,
};

/// 单份流程定义 → JSON（key/名/节点/边/是否可发起）。
pub fn definition_view(def: &ProcessDefinition) -> Value {
    let nodes: Vec<Value> = def
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.bpmn_id,
                "name": n.name,
                "kind": node_kind_str(&n.kind),
                "multiInstance": node_multi_instance(&n.kind),
                "boundaryTimer": node_boundary_timer(&n.kind),
                "calledElement": node_called_element(&n.kind),
            })
        })
        .collect();
    let edges: Vec<Value> = def
        .nodes
        .iter()
        .flat_map(|n| {
            n.outgoing.iter().map(move |f| {
                json!({
                    "from": n.bpmn_id,
                    "to": f.target_bpmn_id,
                    "condition": f.condition,
                    "isDefault": f.is_default,
                })
            })
        })
        .collect();
    // 子流程（被 callActivity 调用的）不在发起列表；其余为可发起顶层流程。
    let startable = !matches!(def.key.as_str(), "fin_review_hq" | "fin_review_branch");
    json!({ "key": def.key, "name": def.name, "nodes": nodes, "edges": edges, "startable": startable })
}

/// 节点若为 callActivity，返回其调用的子流程定义 key（供前端画子流程内嵌路径图）。
fn node_called_element(kind: &NodeKind) -> Value {
    if let NodeKind::CallActivity(ca) = kind {
        let target = if !ca.called_element.is_empty() {
            ca.called_element.clone()
        } else {
            ca.called_key.clone().unwrap_or_default()
        };
        return json!(target);
    }
    Value::Null
}

/// 节点若为多实例 userTask，返回其会签/或签摘要。
fn node_multi_instance(kind: &NodeKind) -> Value {
    if let NodeKind::UserTask(ut) = kind
        && let Some(mi) = &ut.multi_instance
    {
        return json!({
            "sequential": mi.sequential,
            "collectionVar": mi.collection_var,
            "completionCondition": mi.completion_condition,
        });
    }
    Value::Null
}

/// 节点若为边界定时器事件，返回其宿主/时长/中断性摘要。
fn node_boundary_timer(kind: &NodeKind) -> Value {
    if let NodeKind::BoundaryTimerEvent(bt) = kind {
        return json!({
            "attachedTo": bt.attached_to_bpmn_id,
            "seconds": bt.duration.seconds,
            "cancelActivity": bt.cancel_activity,
        });
    }
    Value::Null
}

/// 详细实例视图：状态 + 变量 + 令牌当前位置 + 待办任务。
pub fn instance_view(snap: &InstanceSnapshot) -> Value {
    let tokens: Vec<Value> = snap
        .tokens
        .iter()
        .map(|t| json!({ "nodeBpmnId": t.node_bpmn_id, "state": token_state_str(t.state) }))
        .collect();
    let tasks: Vec<Value> = snap
        .tasks
        .iter()
        .map(|t| {
            // 该任务的候选人（M4.1）：从候选池筛出，供前端展示"谁可认领"。
            let cands: Vec<Value> = snap
                .candidates
                .iter()
                .filter(|c| c.task_id == t.id)
                .map(|c| {
                    json!({
                        "userId": c.resolved_user_id,
                        "type": candidate_kind_str(c.candidate_type),
                        "ref": c.candidate_ref,
                    })
                })
                .collect();
            json!({
                "id": t.id,
                "nodeBpmnId": t.node_bpmn_id,
                "name": t.name,
                "assignee": t.assignee,
                "ownerUserId": t.owner_user_id,
                "parentTaskId": t.parent_task_id,
                "delegationState": t.delegation_state,
                "elementValue": t.element_value,
                "candidates": cands,
                "completed": t.completed,
            })
        })
        .collect();
    // 活动节点（供前端高亮）：非 Ended 令牌所在节点。
    let mut active: Vec<String> = snap
        .tokens
        .iter()
        .filter(|t| !matches!(t.state, TokenState::Ended))
        .map(|t| t.node_bpmn_id.clone())
        .collect();
    active.sort();
    active.dedup();

    // 定时器作业（供前端画倒计时）。
    let jobs: Vec<Value> = snap
        .jobs
        .iter()
        .map(|j| {
            json!({
                "boundaryBpmnId": j.boundary_bpmn_id,
                "cancelActivity": j.cancel_activity,
                "dueAt": j.due_at.to_rfc3339(),
            })
        })
        .collect();

    // 抄送记录（供实例详情展示"已知会谁"）。
    let cc_records: Vec<Value> = snap
        .cc_records
        .iter()
        .map(|c| {
            json!({
                "toUserId": c.to_user_id,
                "nodeBpmnId": c.node_bpmn_id,
                "read": c.read_at.is_some(),
            })
        })
        .collect();

    // 转签台账（供实例详情画流转链）。
    let delegations: Vec<Value> = snap
        .delegations
        .iter()
        .map(|d| {
            json!({
                "kind": d.kind,
                "fromUserId": d.from_user_id,
                "toUserId": d.to_user_id,
                "reason": d.reason,
                "createdAt": d.created_at.to_rfc3339(),
            })
        })
        .collect();

    json!({
        "id": snap.instance.id,
        "definitionKey": snap.instance.definition_key,
        "businessKey": snap.instance.business_key,
        "state": instance_state_str(snap.instance.state),
        "variables": snap.instance.variables.to_json(),
        "parentInstanceId": snap.instance.parent_instance_id,
        "waitingSubflow": snap.tokens.iter().any(|t| matches!(t.state, TokenState::WaitingSubflow)),
        "tokens": tokens,
        "tasks": tasks,
        "activeNodes": active,
        "jobs": jobs,
        "ccRecords": cc_records,
        "delegations": delegations,
        "openTasks": tasks.iter().filter(|t| t["completed"] == json!(false)).cloned().collect::<Vec<_>>(),
    })
}

/// 摘要视图（列表用）——直接映射 store 返回的 InstanceSummary。
pub fn summary_view(s: &InstanceSummary) -> Value {
    let vars = s.variables.to_json();
    json!({
        "id": s.id,
        "definitionKey": s.definition_key,
        "businessKey": s.business_key,
        "applicant": vars.get("applicant").cloned().unwrap_or(Value::Null),
        "amount": vars.get("amount").cloned().unwrap_or(Value::Null),
        "riskLevel": vars.get("riskLevel").cloned().unwrap_or(Value::Null),
        "state": instance_state_str(s.state),
        "openTaskCount": s.open_task_count,
    })
}

pub fn node_kind_str(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::StartEvent => "startEvent",
        NodeKind::EndEvent => "endEvent",
        NodeKind::UserTask(_) => "userTask",
        NodeKind::ServiceTask(_) => "serviceTask",
        NodeKind::ExclusiveGateway => "exclusiveGateway",
        NodeKind::ParallelGateway => "parallelGateway",
        NodeKind::BoundaryTimerEvent(_) => "boundaryTimerEvent",
        NodeKind::CallActivity(_) => "callActivity",
    }
}

pub fn instance_state_str(s: InstanceState) -> &'static str {
    match s {
        InstanceState::Active => "ACTIVE",
        InstanceState::Completed => "COMPLETED",
        InstanceState::Terminated => "TERMINATED",
    }
}

fn candidate_kind_str(k: CandidateKind) -> &'static str {
    match k {
        CandidateKind::User => "USER",
        CandidateKind::Role => "ROLE",
        CandidateKind::Position => "POSITION",
        CandidateKind::Org => "ORG",
    }
}

fn token_state_str(s: TokenState) -> &'static str {
    match s {
        TokenState::Active => "ACTIVE",
        TokenState::Waiting => "WAITING",
        TokenState::Joining => "JOINING",
        TokenState::WaitingSubflow => "WAITING_SUBFLOW",
        TokenState::Ended => "ENDED",
    }
}
