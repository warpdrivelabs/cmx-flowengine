//! A5 端到端：嵌入式子流程（subProcess 扁平化，内存态）。
//!
//! 验证：主流程 start → apply → [subProcess: s0 → review → s1] → notify → end。
//!   嵌入子流程被扁平化进父图；令牌透传进内部 review 待办，办结后透传出子流程到 notify。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, Variables};

const EMBEDDED_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="embedded_flow" name="含嵌入子流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="e0" sourceRef="start" targetRef="apply"/>
    <userTask id="apply" name="申请" flowable:assignee="u"/>
    <sequenceFlow id="e1" sourceRef="apply" targetRef="block"/>
    <subProcess id="block" name="审核块">
      <startEvent id="bstart"/>
      <sequenceFlow id="b0" sourceRef="bstart" targetRef="review"/>
      <userTask id="review" name="块内复核" flowable:assignee="r"/>
      <sequenceFlow id="b1" sourceRef="review" targetRef="bend"/>
      <endEvent id="bend"/>
    </subProcess>
    <sequenceFlow id="e2" sourceRef="block" targetRef="notify"/>
    <userTask id="notify" name="通知" flowable:assignee="n"/>
    <sequenceFlow id="e3" sourceRef="notify" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(EMBEDDED_FLOW).expect("编译")).expect("部署");
    (engine, store)
}

async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<String> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| t.id.clone())
}

#[test]
fn compiles_embedded_subprocess_flattened() {
    let def = compile(EMBEDDED_FLOW).expect("编译应成功");
    use cmx_flow_model::NodeKind;
    // 内部节点被提升进平铺 arena。
    assert!(def.nodes.iter().any(|n| n.bpmn_id == "review"), "内部 review 应在 arena");
    assert!(def.nodes.iter().any(|n| n.bpmn_id == "bstart"), "内部 bstart 应在 arena");
    // subProcess 本身是透传节点。
    let block = def.nodes.iter().find(|n| n.bpmn_id == "block").unwrap();
    assert!(matches!(block.kind, NodeKind::SubProcess));
}

#[tokio::test]
async fn token_flows_through_embedded_block() {
    let (engine, store) = engine_with();
    let started = engine
        .start_process("embedded_flow", Variables::new(), Some("EMB-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    // 停在 apply。
    let apply = open_task_at(&store, &iid, "apply").await.expect("应有 apply 待办");
    engine.complete_task(&iid, &apply, Variables::new()).await.unwrap();

    // 办结 apply → 透传进子流程块 → 停在内部 review 待办。
    let review = open_task_at(&store, &iid, "review").await.expect("应透传进块内 review");
    engine.complete_task(&iid, &review, Variables::new()).await.unwrap();

    // 办结 review → 透传出子流程块 → 停在 notify 待办。
    assert!(
        open_task_at(&store, &iid, "notify").await.is_some(),
        "块办结后应透传到 notify"
    );

    // 办结 notify → 流程完成。
    let notify = open_task_at(&store, &iid, "notify").await.unwrap();
    engine.complete_task(&iid, &notify, Variables::new()).await.unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Completed, "全程办结应 Completed");
}

#[tokio::test]
async fn block_level_boundary_is_rejected() {
    // 附着在 subProcess 上的块级边界事件（整块超时）需嵌套作用域，本轮显式不支持。
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="bad" isExecutable="true">
    <startEvent id="s"/>
    <sequenceFlow id="f0" sourceRef="s" targetRef="blk"/>
    <subProcess id="blk">
      <startEvent id="bs"/>
      <sequenceFlow id="bf" sourceRef="bs" targetRef="be"/>
      <endEvent id="be"/>
    </subProcess>
    <boundaryEvent id="bnd" attachedToRef="blk">
      <timerEventDefinition><timeDuration>PT1H</timeDuration></timerEventDefinition>
    </boundaryEvent>
    <sequenceFlow id="f1" sourceRef="blk" targetRef="e"/>
    <endEvent id="e"/>
  </process>
</definitions>"#;
    assert!(compile(xml).is_err(), "块级边界事件应显式报不支持");
}
