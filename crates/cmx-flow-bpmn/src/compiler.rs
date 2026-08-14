/*
 * @Describe: BPMN 2.0 XML → ProcessDefinition IR 编译器。
 *
 * 两趟编译：
 *   Pass 1：遍历 <process> 直接子元素，为每个受支持的流程节点建 FlowNode（此时出边空），
 *           同时建 bpmn_id → NodeId 索引。
 *   Pass 2：遍历 <sequenceFlow>，按 sourceRef/targetRef 用索引解析成 NodeId，挂到源节点的
 *           outgoing；解析 conditionExpression 与 default 标记。
 * 最后 validate() 自检并返回。
 *
 * 命名空间处理：BPMN 元素可能带 `bpmn:` / `bpmn2:` 等前缀。统一用 `node.tag_name().name()`
 * 取本地名（不含前缀）比较，做到前缀无关。扩展属性（flowable:/camunda: assignee 等）用
 * 「本地属性名匹配」抓取，忽略其命名空间，兼容三家方言。
 */

use std::collections::HashMap;

use cmx_flow_model::{
    BoundaryTimer, BusinessRule, CallActivity, CandidateKind, CandidateRef, FlowNode, MessageCatch,
    MultiInstance, NodeId, NodeKind, ProcessDefinition, SequenceFlow, ServiceTask, UserTask,
    VarMapping, candidate::parse_candidate_expr, duration::parse_iso8601_duration,
};
use roxmltree::{Document, Node};

use crate::error::{Error, Result};

/// 从 BPMN XML 字符串编译出一份流程定义。
///
/// 若 XML 含多个 `<process>`，取**第一个 isExecutable 或第一个出现**的。M1 不处理协作泳道。
pub fn compile(xml: &str) -> Result<ProcessDefinition> {
    let doc = Document::parse(xml)?;
    let process = find_process(&doc)?;

    let key = process
        .attribute("id")
        .ok_or_else(|| Error::MissingElement("process 缺少 id".into()))?
        .to_string();
    let name = process.attribute("name").map(|s| s.to_string());

    // —— Pass 1：建节点 + 索引（递归进嵌入 subProcess，A5 扁平化） —— //
    let mut nodes: Vec<FlowNode> = Vec::new();
    let mut index: HashMap<String, NodeId> = HashMap::new();
    collect_nodes(&process, &mut nodes, &mut index)?;

    // —— Pass 2：解析 sequenceFlow，挂出边（递归进 subProcess） —— //
    collect_flows(&process, &process, &mut nodes, &index)?;

    // —— Pass 3：嵌入 subProcess 合成接线（透传入口 + 内部 endEvent 接子流程出口，A5） —— //
    wire_subprocesses(&process, &mut nodes, &index)?;

    // —— 定位唯一 startEvent —— //
    let start = nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::StartEvent))
        .map(|n| n.id)
        .ok_or_else(|| Error::MissingElement("流程缺少 startEvent".into()))?;

    let def = ProcessDefinition {
        key,
        name,
        nodes,
        start,
        index,
        // F4：起点表单（process 级 cmx:startFormKey，前缀无关，同 userTask 的 formKey）。
        start_form_key: local_attr(&process, "startFormKey"),
    };
    def.validate()?;
    Ok(def)
}

/// 递归建节点。scope 是 process 或 subProcess 元素；其活动类子元素建成节点，遇 subProcess 递归。
fn collect_nodes(
    scope: &Node,
    nodes: &mut Vec<FlowNode>,
    index: &mut HashMap<String, NodeId>,
) -> Result<()> {
    for child in scope.children().filter(Node::is_element) {
        let local = child.tag_name().name();
        let kind = match local {
            "startEvent" => Some(NodeKind::StartEvent),
            "endEvent" => Some(if has_terminate_definition(&child) {
                NodeKind::TerminateEndEvent
            } else {
                NodeKind::EndEvent
            }),
            "userTask" => Some(NodeKind::UserTask(parse_user_task(&child))),
            "serviceTask" => Some(parse_service_task(&child)?),
            "businessRuleTask" => Some(parse_business_rule_task(&child)?),
            "exclusiveGateway" => Some(NodeKind::ExclusiveGateway),
            "parallelGateway" => Some(NodeKind::ParallelGateway),
            "inclusiveGateway" => Some(NodeKind::InclusiveGateway),
            "boundaryEvent" => parse_boundary_event(&child)?,
            "intermediateCatchEvent" => parse_intermediate_catch(&child)?,
            "callActivity" => Some(NodeKind::CallActivity(parse_call_activity(&child)?)),
            // 嵌入子流程（A5）：本身建成透传节点，其内部节点递归提升进同一 arena。
            // 块级边界事件需嵌套作用域，本轮不支持——显式挡回。
            "subProcess" => {
                if scope_has_boundary_on(scope, child.attribute("id").unwrap_or("")) {
                    return Err(Error::Unsupported(format!(
                        "subProcess (id={:?}) 的块级边界事件需嵌套作用域，本轮不支持",
                        child.attribute("id")
                    )));
                }
                Some(NodeKind::SubProcess)
            }
            "sequenceFlow" => None,
            _ => {
                if is_flow_node_like(local) {
                    return Err(Error::Unsupported(format!(
                        "元素 <{local}> (id={:?})",
                        child.attribute("id")
                    )));
                }
                None
            }
        };

        if let Some(kind) = kind {
            let is_subprocess = matches!(kind, NodeKind::SubProcess);
            let bpmn_id = child
                .attribute("id")
                .ok_or_else(|| Error::MissingElement(format!("<{local}> 缺少 id")))?
                .to_string();
            let node_id = NodeId(nodes.len());
            index.insert(bpmn_id.clone(), node_id);
            nodes.push(FlowNode {
                id: node_id,
                bpmn_id,
                name: child.attribute("name").map(|s| s.to_string()),
                kind,
                incoming_count: 0,
                outgoing: Vec::new(),
            });
            if is_subprocess {
                collect_nodes(&child, nodes, index)?;
            }
        }
    }
    Ok(())
}

/// 递归挂出边。process 传顶层（供 source_default_of 查网关 default）；scope 是当前作用域。
fn collect_flows(
    process: &Node,
    scope: &Node,
    nodes: &mut [FlowNode],
    index: &HashMap<String, NodeId>,
) -> Result<()> {
    for child in scope.children().filter(Node::is_element) {
        let local = child.tag_name().name();
        if local == "subProcess" {
            collect_flows(process, &child, nodes, index)?;
            continue;
        }
        if local != "sequenceFlow" {
            continue;
        }
        let flow_id = child.attribute("id").unwrap_or("<anon-flow>").to_string();
        let source_ref = child.attribute("sourceRef").ok_or_else(|| {
            Error::MissingElement(format!("sequenceFlow {flow_id} 缺少 sourceRef"))
        })?;
        let target_ref = child.attribute("targetRef").ok_or_else(|| {
            Error::MissingElement(format!("sequenceFlow {flow_id} 缺少 targetRef"))
        })?;
        let source_id = *index.get(source_ref).ok_or_else(|| {
            Error::DanglingReference(format!(
                "sequenceFlow {flow_id} 的 sourceRef '{source_ref}' 无对应节点"
            ))
        })?;
        let target_id = *index.get(target_ref).ok_or_else(|| {
            Error::DanglingReference(format!(
                "sequenceFlow {flow_id} 的 targetRef '{target_ref}' 无对应节点"
            ))
        })?;
        let condition = parse_condition(&child);
        let is_default = source_default_of(process, source_ref)
            .map(|d| d == flow_id)
            .unwrap_or(false);
        nodes[source_id.0].outgoing.push(SequenceFlow {
            bpmn_id: flow_id,
            target: target_id,
            target_bpmn_id: target_ref.to_string(),
            condition,
            is_default,
        });
        nodes[target_id.0].incoming_count += 1;
    }
    Ok(())
}

/// 嵌入子流程合成接线（A5）：subProcess 透传节点接到其内部 startEvent；内部 endEvent 接到
/// subProcess 的出口目标。递归处理嵌套子流程。
fn wire_subprocesses(
    scope: &Node,
    nodes: &mut Vec<FlowNode>,
    index: &HashMap<String, NodeId>,
) -> Result<()> {
    for child in scope.children().filter(Node::is_element) {
        if child.tag_name().name() != "subProcess" {
            continue;
        }
        let sp_bpmn = child.attribute("id").unwrap_or("").to_string();
        let sp_id = *index
            .get(&sp_bpmn)
            .ok_or_else(|| Error::DanglingReference(format!("subProcess {sp_bpmn} 无对应节点")))?;

        let inner_start = child
            .children()
            .filter(Node::is_element)
            .find(|n| n.tag_name().name() == "startEvent")
            .and_then(|n| n.attribute("id"))
            .ok_or_else(|| {
                Error::MissingElement(format!("subProcess {sp_bpmn} 缺少内部 startEvent"))
            })?;
        let inner_start_id = *index.get(inner_start).ok_or_else(|| {
            Error::DanglingReference(format!("subProcess {sp_bpmn} 内部 start 无节点"))
        })?;

        // subProcess 的出口目标（父作用域 outgoing，已由 collect_flows 挂上）。
        let exit_targets: Vec<SequenceFlow> = nodes[sp_id.0].outgoing.clone();
        // subProcess 节点改为只透传到内部 start。
        nodes[sp_id.0].outgoing = vec![SequenceFlow {
            bpmn_id: format!("{sp_bpmn}__to_inner_start"),
            target: inner_start_id,
            target_bpmn_id: inner_start.to_string(),
            condition: None,
            is_default: false,
        }];
        nodes[inner_start_id.0].incoming_count += 1;

        // 内部 endEvent → 透传接到 subProcess 出口目标（无出口则保持 EndEvent）。
        let inner_ends: Vec<String> = child
            .children()
            .filter(Node::is_element)
            .filter(|n| n.tag_name().name() == "endEvent")
            .filter_map(|n| n.attribute("id").map(|s| s.to_string()))
            .collect();
        for end_bpmn in &inner_ends {
            let end_id = *index.get(end_bpmn).ok_or_else(|| {
                Error::DanglingReference(format!("subProcess {sp_bpmn} 内部 end {end_bpmn} 无节点"))
            })?;
            if !exit_targets.is_empty() {
                nodes[end_id.0].kind = NodeKind::SubProcess;
                nodes[end_id.0].outgoing = exit_targets.clone();
                for e in &exit_targets {
                    nodes[e.target.0].incoming_count += 1;
                }
            }
        }

        wire_subprocesses(&child, nodes, index)?;
    }
    Ok(())
}

/// 判断某 scope 内是否有 boundaryEvent 附着在给定 subProcess id 上（块级边界，本轮不支持）。
fn scope_has_boundary_on(scope: &Node, sp_id: &str) -> bool {
    if sp_id.is_empty() {
        return false;
    }
    scope.children().filter(Node::is_element).any(|n| {
        n.tag_name().name() == "boundaryEvent" && n.attribute("attachedToRef") == Some(sp_id)
    })
}

/// 找到要编译的 `<process>` 元素。
fn find_process<'a>(doc: &'a Document<'_>) -> Result<Node<'a, 'a>> {
    let root = doc.root_element(); // <definitions>
    // 优先 isExecutable="true"，否则第一个 process。
    let mut first: Option<Node> = None;
    for child in root.children().filter(Node::is_element) {
        if child.tag_name().name() == "process" {
            if first.is_none() {
                first = Some(child);
            }
            if child.attribute("isExecutable") == Some("true") {
                return Ok(child);
            }
        }
    }
    first.ok_or_else(|| Error::MissingElement("definitions 下没有 process".into()))
}

/// 解析 userTask 的 assignee / candidateGroups（跨 flowable/camunda 方言，按本地属性名匹配），
/// 以及可选的 multiInstanceLoopCharacteristics（会签 / 或签）、候选人 / 抄送表达式（M4）。
fn parse_user_task(node: &Node) -> UserTask {
    UserTask {
        assignee: local_attr(node, "assignee"),
        candidate_groups: local_attr(node, "candidateGroups"),
        multi_instance: parse_multi_instance(node),
        candidates: parse_candidates(node),
        cc: parse_cc(node),
        // F2：表单绑定（cmx:formKey / formMode / formFields，前缀无关，同 assignee）。
        form_key: local_attr(node, "formKey"),
        form_mode: local_attr(node, "formMode"),
        form_fields: local_attr(node, "formFields")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 汇总 userTask 上的候选人来源为结构化引用（M4.1）。
///
/// 合并三个来源（按此优先序拼接，去重交给运行期 resolve_all）：
/// - `flowable:candidateUsers`（逗号 id 列表）→ 一串 User 引用
/// - `flowable:candidateGroups`（逗号 code 列表）→ 一串 Role 引用（Flowable 里 group 即角色）
/// - 自定义 `cmx:candidates` / `candidates` 表达式（`role(x),position(y),org(z),user(w)` 混合）
///
/// 单纯的静态 `assignee`（无上述任何一项）不产生候选引用——走 M1 直派老路，零改动。
fn parse_candidates(node: &Node) -> Vec<CandidateRef> {
    let mut out = Vec::new();
    if let Some(users) = local_attr(node, "candidateUsers") {
        out.extend(parse_candidate_expr(&users, CandidateKind::User));
    }
    if let Some(groups) = local_attr(node, "candidateGroups") {
        out.extend(parse_candidate_expr(&groups, CandidateKind::Role));
    }
    // 自定义混合表达式：属性名 candidates（含命名空间前缀如 cmx:candidates 也按本地名匹配）。
    if let Some(expr) = local_attr(node, "candidates") {
        out.extend(parse_candidate_expr(&expr, CandidateKind::User));
    }
    out
}

/// 解析 userTask 的抄送表达式（M4.2 预留；M4.1 已能编译落库，引擎消费在 M4.2 接）。
///
/// 属性名 `cc`（本地名匹配，兼容 `cmx:cc`）。语法同候选人表达式，裸值按 User。
fn parse_cc(node: &Node) -> Vec<CandidateRef> {
    match local_attr(node, "cc") {
        Some(expr) => parse_candidate_expr(&expr, CandidateKind::User),
        None => Vec::new(),
    }
}

/// 解析 `<multiInstanceLoopCharacteristics>` 子元素 → MultiInstance（无则 None）。
///
/// 支持形态（前缀/方言无关）：
/// - `isSequential="true"` → 顺序（或签）；缺省 / false → 并行（会签）。
/// - 集合来源：flowable `flowable:collection` 属性，或 `<loopDataInputRef>` 子元素文本。
/// - `elementVariable` 属性 → element_var。
/// - `<completionCondition>` 子元素文本 → completion_condition。
fn parse_multi_instance(task: &Node) -> Option<MultiInstance> {
    let mi = task
        .children()
        .filter(Node::is_element)
        .find(|n| n.tag_name().name() == "multiInstanceLoopCharacteristics")?;

    let sequential = mi
        .attributes()
        .find(|a| a.name() == "isSequential")
        .map(|a| a.value() == "true")
        .unwrap_or(false);

    // 集合：优先 flowable/camunda 的 collection 属性，退回 <loopDataInputRef> 子元素文本。
    let collection_var = local_attr(&mi, "collection")
        .or_else(|| child_text(&mi, "loopDataInputRef"))
        .unwrap_or_default();

    let element_var = local_attr(&mi, "elementVariable");
    let completion_condition = child_text(&mi, "completionCondition");

    Some(MultiInstance {
        sequential,
        collection_var,
        element_var,
        completion_condition,
    })
}

/// 判断 endEvent 是否为终止事件（含 `<terminateEventDefinition>` 子元素，前缀/方言无关）。
fn has_terminate_definition(node: &Node) -> bool {
    node.children()
        .filter(Node::is_element)
        .any(|n| n.tag_name().name() == "terminateEventDefinition")
}

/// 解析 intermediateCatchEvent：仅支持 messageEventDefinition（A4 消息中间捕获）。
///
/// 消息名取 `messageEventDefinition` 的 `messageRef` 或本事件的 `cmx:message`/`name` 属性；
/// 相关键变量取 `cmx:correlationVar` 属性（可空）。非消息类型（timer/signal 等）显式报不支持。
fn parse_intermediate_catch(node: &Node) -> Result<Option<NodeKind>> {
    let has_message = node
        .children()
        .filter(Node::is_element)
        .any(|n| n.tag_name().name() == "messageEventDefinition");
    if !has_message {
        return Err(Error::Unsupported(format!(
            "intermediateCatchEvent (id={:?}) 仅支持 messageEventDefinition（A4），其它类型待补",
            node.attribute("id")
        )));
    }
    // 消息名：优先 messageEventDefinition.messageRef，退回事件的 cmx:message / name 属性 / id。
    let msg_ref = node
        .children()
        .filter(Node::is_element)
        .find(|n| n.tag_name().name() == "messageEventDefinition")
        .and_then(|n| local_attr(&n, "messageRef"));
    let message_name = msg_ref
        .or_else(|| local_attr(node, "message"))
        .or_else(|| node.attribute("name").map(|s| s.to_string()))
        .unwrap_or_else(|| node.attribute("id").unwrap_or("message").to_string());
    let correlation_var = local_attr(node, "correlationVar").filter(|s| !s.is_empty());
    Ok(Some(NodeKind::MessageCatchEvent(MessageCatch {
        message_name,
        correlation_var,
    })))
}

/// 解析 boundaryEvent：仅支持 timerEventDefinition + timeDuration（M2.5 边界定时器）。
///
/// 定时器边界返回 Some(BoundaryTimerEvent)；非定时器类型（error/message/signal 等）显式报
/// 「不支持」，避免静默改变语义。attachedToRef（宿主）由引擎运行期按 bpmn_id 关联，编译期只存。
fn parse_boundary_event(node: &Node) -> Result<Option<NodeKind>> {
    // 必须有 attachedToRef（宿主）。
    let attached_to = local_attr(node, "attachedToRef").ok_or_else(|| {
        Error::MissingElement(format!(
            "boundaryEvent (id={:?}) 缺少 attachedToRef",
            node.attribute("id")
        ))
    })?;

    // 找 timerEventDefinition 子元素。
    let timer_def = node
        .children()
        .filter(Node::is_element)
        .find(|n| n.tag_name().name() == "timerEventDefinition");
    let Some(timer_def) = timer_def else {
        // 非定时器边界事件（error/message/signal 等）——M2.5 不支持，显式报错。
        return Err(Error::Unsupported(format!(
            "boundaryEvent (id={:?}) 非定时器类型，M2.5 仅支持 timerEventDefinition",
            node.attribute("id")
        )));
    };

    // 取 timeDuration 子元素文本（M2.5 只支持相对时长；timeDate/timeCycle 不支持）。
    let duration_text = child_text(&timer_def, "timeDuration").ok_or_else(|| {
        Error::Unsupported(format!(
            "boundaryEvent (id={:?}) 的定时器仅支持 <timeDuration>（相对时长），未找到",
            node.attribute("id")
        ))
    })?;
    let duration = parse_iso8601_duration(&duration_text)
        .map_err(|e| Error::Unsupported(format!("边界定时器时长解析失败: {e}")))?;

    // cancelActivity 缺省为 true（BPMN 默认中断型）。
    let cancel_activity = node
        .attributes()
        .find(|a| a.name() == "cancelActivity")
        .map(|a| a.value() != "false")
        .unwrap_or(true);

    Ok(Some(NodeKind::BoundaryTimerEvent(BoundaryTimer {
        attached_to_bpmn_id: attached_to,
        duration,
        cancel_activity,
    })))
}

/// 解析 callActivity（M5）：被调子流程 key + 输入/输出变量映射。
///
/// M5.1 支持标准 `calledElement`（写死子流程 key）；预留 `cmx:calledKey`（M5.2 逻辑路由名）。
/// 变量映射用 flowable/camunda 风格的 `<extensionElements>` 下 `<in source= target=>` /
/// `<out source= target=>` 子元素（本地名匹配，忽略命名空间前缀，兼容各方言）。
fn parse_call_activity(node: &Node) -> Result<CallActivity> {
    let called_element = local_attr(node, "calledElement").unwrap_or_default();
    let called_key = local_attr(node, "calledKey");
    // 二者至少有一个：M5.1 通常给 calledElement；M5.2 给 calledKey。
    if called_element.is_empty() && called_key.is_none() {
        return Err(Error::MissingElement(format!(
            "callActivity (id={:?}) 需指定 calledElement 或 cmx:calledKey",
            node.attribute("id")
        )));
    }
    let input_vars = parse_var_mappings(node, "in");
    let output_vars = parse_var_mappings(node, "out");
    // 兜底：设计器用 cmx:inVars / cmx:outVars 属性存映射（`source:target` 逗号分隔），规避
    // bpmn-js moddle 扩展注册。仅当结构化 <in>/<out> 缺失时读属性，二者不叠加（避免重复）。
    let input_vars = if input_vars.is_empty() {
        parse_attr_var_mappings(node, "inVars")
    } else {
        input_vars
    };
    let output_vars = if output_vars.is_empty() {
        parse_attr_var_mappings(node, "outVars")
    } else {
        output_vars
    };
    Ok(CallActivity {
        called_element,
        called_key,
        input_vars,
        output_vars,
    })
}

/// 递归收集 callActivity 下所有指定本地名（in / out）的变量映射子元素。
///
/// 兼容两种放置：直接子元素，或包在 `<extensionElements>` 里。source==target 用 source 一个属性时同名。
fn parse_var_mappings(node: &Node, local: &str) -> Vec<VarMapping> {
    let mut out = Vec::new();
    collect_var_mappings(node, local, &mut out);
    out
}

/// 从 `cmx:inVars` / `cmx:outVars` 属性解析变量映射（设计器落点，规避 moddle 扩展）。
///
/// 格式：`source:target` 对，逗号分隔；省略 `:target` 时 target = source。
/// 例：`amount:subAmount, applicant` → [{amount→subAmount}, {applicant→applicant}]。
fn parse_attr_var_mappings(node: &Node, local: &str) -> Vec<VarMapping> {
    let raw = match local_attr(node, local) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (source, target) = match pair.split_once(':') {
            Some((s, t)) => (s.trim().to_string(), t.trim().to_string()),
            None => (pair.to_string(), pair.to_string()),
        };
        if source.is_empty() {
            continue;
        }
        let target = if target.is_empty() { source.clone() } else { target };
        out.push(VarMapping { source, target });
    }
    out
}

fn collect_var_mappings(node: &Node, local: &str, out: &mut Vec<VarMapping>) {
    for child in node.children().filter(Node::is_element) {
        let name = child.tag_name().name();
        if name == local {
            let source =
                local_attr(&child, "source").or_else(|| local_attr(&child, "sourceExpression"));
            if let Some(source) = source {
                let target = local_attr(&child, "target").unwrap_or_else(|| source.clone());
                out.push(VarMapping { source, target });
            }
        } else if name == "extensionElements" {
            // 下钻一层找 in/out。
            collect_var_mappings(&child, local, out);
        }
    }
}

/// 取某元素下指定本地名子元素的文本内容（trim 后非空才返回）。
fn child_text(parent: &Node, local_name: &str) -> Option<String> {
    for child in parent.children().filter(Node::is_element) {
        if child.tag_name().name() == local_name {
            let text: String = child
                .children()
                .filter_map(|n| n.text())
                .collect::<String>()
                .trim()
                .to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// 解析 serviceTask 的 delegate 键。
///
/// 依次尝试：flowable/camunda 的 `delegateExpression`、`class`，再退回自定义 `delegate`。
/// 三者都无则报「不支持」——serviceTask 必须能定位执行体，否则引擎无从执行。
fn parse_service_task(node: &Node) -> Result<NodeKind> {
    let delegate = local_attr(node, "delegateExpression")
        .or_else(|| local_attr(node, "class"))
        .or_else(|| local_attr(node, "delegate"))
        .or_else(|| local_attr(node, "type"));
    match delegate {
        Some(d) => Ok(NodeKind::ServiceTask(ServiceTask {
            delegate: normalize_delegate(&d),
        })),
        None => Err(Error::Unsupported(format!(
            "serviceTask (id={:?}) 未指定 delegate/class/delegateExpression",
            node.attribute("id")
        ))),
    }
}

/// 解析 businessRuleTask（A3）：取决策表 key（flowable/camunda decisionRef，或 cmx:decision）。
fn parse_business_rule_task(node: &Node) -> Result<NodeKind> {
    let decision_key = local_attr(node, "decisionRef")
        .or_else(|| local_attr(node, "decision"))
        .or_else(|| local_attr(node, "decisionRefBinding"));
    match decision_key.filter(|s| !s.trim().is_empty()) {
        Some(k) => Ok(NodeKind::BusinessRuleTask(BusinessRule {
            decision_key: k.trim().to_string(),
        })),
        None => Err(Error::Unsupported(format!(
            "businessRuleTask (id={:?}) 未指定 decisionRef（决策表 key）",
            node.attribute("id")
        ))),
    }
}

/// 归一化 delegate 键：剥掉可能的 `${...}` 包裹，得到纯注册键。
fn normalize_delegate(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 3 && (s.starts_with("${") || s.starts_with("#{")) && s.ends_with('}') {
        s[2..s.len() - 1].trim().to_string()
    } else {
        s.to_string()
    }
}

/// 解析 sequenceFlow 的 conditionExpression 文本（子元素文本）。
fn parse_condition(flow: &Node) -> Option<String> {
    for child in flow.children().filter(Node::is_element) {
        if child.tag_name().name() == "conditionExpression" {
            let text: String = child
                .children()
                .filter_map(|n| n.text())
                .collect::<String>()
                .trim()
                .to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// 取某源节点（网关）的 `default` 属性值（default flow 的边 id）。
fn source_default_of(process: &Node, source_ref: &str) -> Option<String> {
    process
        .children()
        .filter(Node::is_element)
        .find(|n| n.attribute("id") == Some(source_ref))
        .and_then(|n| n.attribute("default"))
        .map(|s| s.to_string())
}

/// 按「本地属性名」取属性值，忽略命名空间前缀，兼容 flowable:/camunda:/无前缀。
fn local_attr(node: &Node, local_name: &str) -> Option<String> {
    node.attributes()
        .find(|a| a.name() == local_name)
        .map(|a| a.value().to_string())
}

/// 判断一个本地元素名是否「像流程节点」——用于对未支持的活动类元素给出明确报错，
/// 而不是静默忽略（静默忽略会改变流程语义，是危险的）。
fn is_flow_node_like(local: &str) -> bool {
    matches!(
        local,
        "task"
            | "scriptTask"
            | "sendTask"
            | "receiveTask"
            | "manualTask"
            | "eventBasedGateway"
            | "complexGateway"
            | "intermediateThrowEvent"
            | "boundaryEvent"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmx_flow_model::NodeKind;

    // 含 callActivity 的最小主流程；call_inner 插在 calledElement 之后，可放属性 + 自定义子元素。
    // 约定：call_inner 需自行闭合开标签（给出 `>` 或属性后接 `>` + 子元素）。
    fn main_with_call(call_open_and_children: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="main" name="主" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="c"/>
    <callActivity id="c" name="子调用" calledElement="sub" {call_open_and_children}
    </callActivity>
    <sequenceFlow id="s1" sourceRef="c" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#
        )
    }

    fn call_activity_of(xml: &str) -> CallActivity {
        let def = compile(xml).expect("编译应成功");
        for node in &def.nodes {
            if let NodeKind::CallActivity(ca) = &node.kind {
                return ca.clone();
            }
        }
        panic!("未找到 callActivity");
    }

    #[test]
    fn attr_var_mappings_parsed() {
        // cmx:inVars / cmx:outVars 属性写法（设计器落点）；开标签用自闭前的 `>` 收口。
        let xml = main_with_call(
            r#"cmx:inVars="amount:subAmount, applicant" cmx:outVars="finResult:result">"#,
        );
        let ca = call_activity_of(&xml);
        assert_eq!(ca.input_vars.len(), 2);
        assert_eq!(ca.input_vars[0].source, "amount");
        assert_eq!(ca.input_vars[0].target, "subAmount");
        // 省略 :target → target = source
        assert_eq!(ca.input_vars[1].source, "applicant");
        assert_eq!(ca.input_vars[1].target, "applicant");
        assert_eq!(ca.output_vars.len(), 1);
        assert_eq!(ca.output_vars[0].source, "finResult");
        assert_eq!(ca.output_vars[0].target, "result");
    }

    #[test]
    fn structured_in_out_still_win_over_attr() {
        // 同时给结构化 <in>/<out> 和属性时，结构化优先（属性仅兜底）。
        let inner = r#"cmx:inVars="ignored:ignored">
      <extensionElements>
        <flowable:in source="realIn" target="realInT"/>
      </extensionElements>"#;
        let xml = main_with_call(inner);
        let ca = call_activity_of(&xml);
        assert_eq!(ca.input_vars.len(), 1);
        assert_eq!(ca.input_vars[0].source, "realIn");
        assert_eq!(ca.input_vars[0].target, "realInT");
    }

    #[test]
    fn no_mappings_is_empty() {
        let xml = main_with_call(">");
        let ca = call_activity_of(&xml);
        assert!(ca.input_vars.is_empty());
        assert!(ca.output_vars.is_empty());
    }
}
