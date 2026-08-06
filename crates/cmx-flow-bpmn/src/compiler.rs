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
    BoundaryTimer, CallActivity, CandidateKind, CandidateRef, FlowNode, MultiInstance, NodeId,
    NodeKind, ProcessDefinition, SequenceFlow, ServiceTask, UserTask, VarMapping,
    candidate::parse_candidate_expr, duration::parse_iso8601_duration,
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

    // —— Pass 1：建节点 + 索引 —— //
    let mut nodes: Vec<FlowNode> = Vec::new();
    let mut index: HashMap<String, NodeId> = HashMap::new();

    for child in process.children().filter(Node::is_element) {
        let local = child.tag_name().name();
        let kind = match local {
            "startEvent" => Some(NodeKind::StartEvent),
            "endEvent" => Some(NodeKind::EndEvent),
            "userTask" => Some(NodeKind::UserTask(parse_user_task(&child))),
            "serviceTask" => Some(parse_service_task(&child)?),
            "exclusiveGateway" => Some(NodeKind::ExclusiveGateway),
            "parallelGateway" => Some(NodeKind::ParallelGateway),
            "boundaryEvent" => parse_boundary_event(&child)?,
            "callActivity" => Some(NodeKind::CallActivity(parse_call_activity(&child)?)),
            // 顺序流在 Pass 2 处理；其余元素（extensionElements/laneSet/文档等）跳过。
            "sequenceFlow" => None,
            _ => {
                // 未知的「活动类」元素给出明确的不支持提示，而不是静默丢弃，
                // 避免流程被悄悄改变语义。纯装饰/元信息元素则忽略。
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
                incoming_count: 0, // Pass 2 统计
                outgoing: Vec::new(),
            });
        }
    }

    // —— Pass 2：解析 sequenceFlow，挂出边 —— //
    for child in process.children().filter(Node::is_element) {
        if child.tag_name().name() != "sequenceFlow" {
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

        // default 标记：网关的 default 属性等于此边 id 时，此边为 default。
        let is_default = source_default_of(&process, source_ref)
            .map(|d| d == flow_id)
            .unwrap_or(false);

        nodes[source_id.0].outgoing.push(SequenceFlow {
            bpmn_id: flow_id,
            target: target_id,
            target_bpmn_id: target_ref.to_string(),
            condition,
            is_default,
        });
        // 统计目标节点入边数（并行网关 join 判断令牌是否到齐所需）。
        nodes[target_id.0].incoming_count += 1;
    }

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
            | "businessRuleTask"
            | "sendTask"
            | "receiveTask"
            | "manualTask"
            | "subProcess"
            | "inclusiveGateway"
            | "eventBasedGateway"
            | "complexGateway"
            | "intermediateCatchEvent"
            | "intermediateThrowEvent"
            | "boundaryEvent"
    )
}
