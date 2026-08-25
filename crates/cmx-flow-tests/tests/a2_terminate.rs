//! A2 端到端：终止结束事件（terminateEndEvent，一票否决，内存态）。
//!
//! 验证：并行网关 fork 两分支——A 分支到用户任务(否决)，B 分支到终止事件。
//!   - 起流程 → fork → A 停 userTask 等待、B 直达 terminateEndEvent；
//!   - B 到达终止事件 → **杀掉 A 分支令牌 + 丢 A 的待办** → 实例立即 Completed（一票否决）。
//! 对照：普通 endEvent 不会杀兄弟分支（需两分支都完成才 Completed）。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, TokenState, Variables};

/// 并行两分支：fork → (approve userTask) + (veto → terminateEndEvent)。
/// 走到 veto 的 B 分支立即终止全流程。为让 B 直达终止事件、A 停等待，B 分支无 userTask。
const VETO_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="veto_flow" name="一票否决" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="approve"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="term"/>
    <userTask id="approve" name="部门审批" flowable:assignee="mgr"/>
    <sequenceFlow id="s3" sourceRef="approve" targetRef="done"/>
    <endEvent id="done"/>
    <endEvent id="term">
      <terminateEventDefinition/>
    </endEvent>
  </process>
</definitions>"#;

/// 对照流程：B 分支用普通 endEvent（不终止），验证兄弟分支不受影响。
const NORMAL_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="normal_flow" name="普通结束" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="approve"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="e2"/>
    <userTask id="approve" name="部门审批" flowable:assignee="mgr"/>
    <sequenceFlow id="s3" sourceRef="approve" targetRef="done"/>
    <endEvent id="done"/>
    <endEvent id="e2"/>
  </process>
</definitions>"#;

fn engine_with(xml: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(xml).expect("编译")).expect("部署");
    (engine, store)
}

#[test]
fn compiles_terminate_end_event() {
    // 编译器应把含 <terminateEventDefinition> 的 endEvent 识别为 TerminateEndEvent。
    let def = compile(VETO_FLOW).expect("编译应成功");
    use cmx_flow_model::NodeKind;
    let term = def
        .nodes
        .iter()
        .find(|n| n.bpmn_id == "term")
        .expect("有 term 节点");
    assert!(matches!(term.kind, NodeKind::TerminateEndEvent), "term 应为终止事件");
    // 普通 done 仍是 EndEvent。
    let done = def.nodes.iter().find(|n| n.bpmn_id == "done").unwrap();
    assert!(matches!(done.kind, NodeKind::EndEvent), "done 应为普通结束");
}

#[tokio::test]
async fn terminate_kills_sibling_branch_and_completes() {
    let (engine, store) = engine_with(VETO_FLOW);
    let started = engine
        .start_process("veto_flow", Variables::new(), Some("VETO-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // B 分支到达终止事件 → 一票否决：实例 Completed。
    assert_eq!(
        snap.instance.state,
        InstanceState::Completed,
        "一票否决后实例应 Completed"
    );
    // 所有令牌 Ended（含被杀的 A 分支令牌）。
    assert!(
        snap.tokens.iter().all(|t| t.state == TokenState::Ended),
        "所有令牌应 Ended"
    );
    // A 分支的 approve 待办被作废（无未办结任务）。
    assert!(
        !snap.tasks.iter().any(|t| !t.completed),
        "一票否决后不应有未办结待办"
    );
}

#[tokio::test]
async fn normal_end_does_not_kill_sibling() {
    // 对照：普通 endEvent 不杀兄弟——A 分支仍停在 approve 待办，实例仍 Active。
    let (engine, store) = engine_with(NORMAL_FLOW);
    let started = engine
        .start_process("normal_flow", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(
        snap.instance.state,
        InstanceState::Active,
        "普通结束不终止兄弟分支，实例仍 Active"
    );
    assert!(
        snap.tasks.iter().any(|t| t.node_bpmn_id == "approve" && !t.completed),
        "approve 待办仍在"
    );
}
