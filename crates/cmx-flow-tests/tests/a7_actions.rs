//! A7 端到端：挂起/恢复/跳转/催办（运行时动作，内存态）。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, Variables};

/// start → 经理(mgr) → 财务(fin) → end。
const FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a7_flow" name="A7" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="mgr"/>
    <userTask id="mgr" name="经理" flowable:assignee="m"/>
    <sequenceFlow id="s1" sourceRef="mgr" targetRef="fin"/>
    <userTask id="fin" name="财务" flowable:assignee="f"/>
    <sequenceFlow id="s2" sourceRef="fin" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(FLOW).expect("编译")).expect("部署");
    (engine, store)
}

async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<String> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| t.id.clone())
}

#[tokio::test]
async fn suspend_blocks_complete_then_resume_allows() {
    let (engine, store) = engine_with();
    let started = engine.start_process("a7_flow", Variables::new(), None).await.unwrap();
    let iid = started.instance_id.clone();
    let mgr = open_task_at(&store, &iid, "mgr").await.unwrap();

    // 挂起 → 实例 Suspended。
    engine.suspend_process(&iid).await.unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Suspended);

    // 挂起态办结 → 报错。
    let r = engine.complete_task(&iid, &mgr, Variables::new()).await;
    assert!(r.is_err(), "挂起态不能办结");

    // 恢复 → Active，可办结。
    engine.resume_process(&iid).await.unwrap();
    let snap2 = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap2.instance.state, InstanceState::Active);
    engine.complete_task(&iid, &mgr, Variables::new()).await.expect("恢复后应可办结");
    assert!(open_task_at(&store, &iid, "fin").await.is_some(), "办结后到财务");
}

#[tokio::test]
async fn suspend_resume_idempotent() {
    let (engine, store) = engine_with();
    let started = engine.start_process("a7_flow", Variables::new(), None).await.unwrap();
    let iid = started.instance_id.clone();
    // resume 一个未挂起的实例 → 幂等无操作，仍 Active。
    engine.resume_process(&iid).await.unwrap();
    assert_eq!(store.load_snapshot(&iid).await.unwrap().instance.state, InstanceState::Active);
    // 挂起两次 → 幂等。
    engine.suspend_process(&iid).await.unwrap();
    engine.suspend_process(&iid).await.unwrap();
    assert_eq!(store.load_snapshot(&iid).await.unwrap().instance.state, InstanceState::Suspended);
}

#[tokio::test]
async fn jump_moves_token_to_target_node() {
    let (engine, store) = engine_with();
    let started = engine.start_process("a7_flow", Variables::new(), None).await.unwrap();
    let iid = started.instance_id.clone();
    // 起步停在 mgr。跳转到 fin（跳过 mgr）。
    assert!(open_task_at(&store, &iid, "mgr").await.is_some());
    engine.jump_to(&iid, "fin", Some("运维跳过经理环节")).await.unwrap();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // mgr 待办作废，fin 待办出现。
    assert!(open_task_at(&store, &iid, "mgr").await.is_none(), "mgr 待办应作废");
    assert!(open_task_at(&store, &iid, "fin").await.is_some(), "应跳到 fin");
    // JUMP 台账留痕。
    assert!(snap.delegations.iter().any(|d| d.kind == "JUMP"), "应有 JUMP 台账");
}

#[tokio::test]
async fn jump_to_non_usertask_errors() {
    let (engine, store) = engine_with();
    let started = engine.start_process("a7_flow", Variables::new(), None).await.unwrap();
    let iid = started.instance_id.clone();
    // 跳到 done(endEvent) → 报错；跳到不存在节点 → 报错。
    assert!(engine.jump_to(&iid, "done", None).await.is_err());
    assert!(engine.jump_to(&iid, "nope", None).await.is_err());
    let _ = store; // 保持签名一致
}

#[tokio::test]
async fn urge_records_cc_and_ledger() {
    let (engine, store) = engine_with();
    let started = engine.start_process("a7_flow", Variables::new(), None).await.unwrap();
    let iid = started.instance_id.clone();
    let mgr = open_task_at(&store, &iid, "mgr").await.unwrap();

    engine.urge_task(&iid, &mgr, "boss", Some("尽快处理")).await.unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    // 催办给办理人 m 落一条 CC。
    assert!(
        snap.cc_records.iter().any(|c| c.to_user_id == "m"),
        "催办应给办理人 m 落抄送"
    );
    // URGE 台账留痕。
    assert!(snap.delegations.iter().any(|d| d.kind == "URGE"), "应有 URGE 台账");
    // 不改流程：mgr 待办仍在。
    assert!(open_task_at(&store, &iid, "mgr").await.is_some(), "催办不改流程，mgr 待办仍在");
}
