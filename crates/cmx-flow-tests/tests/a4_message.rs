//! A4 端到端：消息中间捕获事件 + 相关性（外部回调唤醒，内存态）。
//!
//! 验证：start → userTask(apply) → messageCatch(等外部裁决) → userTask(finalize) → end。
//!   1) 起流程办结 apply → 令牌停在 msgWait（WaitingMessage），无 finalize 待办；
//!   2) correlate_message(显式实例 id) → 唤醒 → 推进到 finalize 待办；带入变量已 merge；
//!   3) correlate_message(跨实例按相关键 orderId) → 找到对的实例唤醒；
//!   4) 无匹配 correlate → 报错。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, TokenState, Variables};
use serde_json::json;

const MSG_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="msg_flow" name="等外部裁决" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="apply"/>
    <userTask id="apply" name="申请" flowable:assignee="u"/>
    <sequenceFlow id="s1" sourceRef="apply" targetRef="msgWait"/>
    <intermediateCatchEvent id="msgWait" name="等外部裁决" cmx:correlationVar="orderId">
      <messageEventDefinition messageRef="verdictReceived"/>
    </intermediateCatchEvent>
    <sequenceFlow id="s2" sourceRef="msgWait" targetRef="finalize"/>
    <userTask id="finalize" name="终审" flowable:assignee="boss"/>
    <sequenceFlow id="s3" sourceRef="finalize" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(MSG_FLOW).expect("编译")).expect("部署");
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
fn compiles_message_catch_event() {
    let def = compile(MSG_FLOW).expect("编译应成功");
    use cmx_flow_model::NodeKind;
    let n = def.nodes.iter().find(|n| n.bpmn_id == "msgWait").unwrap();
    match &n.kind {
        NodeKind::MessageCatchEvent(mc) => {
            assert_eq!(mc.message_name, "verdictReceived");
            assert_eq!(mc.correlation_var.as_deref(), Some("orderId"));
        }
        _ => panic!("msgWait 应为消息捕获事件"),
    }
}

#[tokio::test]
async fn token_parks_at_message_then_correlate_wakes() {
    let (engine, store) = engine_with();
    let mut vars = Variables::new();
    vars.set("orderId", json!("ORD-1"));
    let started = engine
        .start_process("msg_flow", vars, Some("MSG-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    // 办结 apply → 令牌到 msgWait 挂起。
    let apply = open_task_at(&store, &iid, "apply").await.unwrap();
    engine.complete_task(&iid, &apply, Variables::new()).await.unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert!(
        snap.tokens.iter().any(|t| t.state == TokenState::WaitingMessage && t.node_bpmn_id == "msgWait"),
        "令牌应停在 msgWait 等消息"
    );
    assert!(open_task_at(&store, &iid, "finalize").await.is_none(), "未收消息，无 finalize 待办");

    // 相关消息（显式实例 id + 相关键 + 带入变量）。
    let mut mv = Variables::new();
    mv.set("verdict", json!("approved"));
    engine
        .correlate_message(Some(&iid), "verdictReceived", Some("ORD-1"), mv)
        .await
        .unwrap();

    // 唤醒 → 推进到 finalize 待办；变量 merge。
    let snap2 = store.load_snapshot(&iid).await.unwrap();
    assert!(open_task_at(&store, &iid, "finalize").await.is_some(), "收消息后应到 finalize");
    assert_eq!(snap2.instance.variables.get("verdict"), Some(&json!("approved")));
    assert!(
        !snap2.tokens.iter().any(|t| t.state == TokenState::WaitingMessage),
        "唤醒后不应再有等消息令牌"
    );
}

#[tokio::test]
async fn correlate_across_instances_by_key() {
    let (engine, store) = engine_with();
    // 两个实例，orderId 不同。
    let mut v1 = Variables::new();
    v1.set("orderId", json!("A"));
    let i1 = engine.start_process("msg_flow", v1, None).await.unwrap().instance_id;
    let mut v2 = Variables::new();
    v2.set("orderId", json!("B"));
    let i2 = engine.start_process("msg_flow", v2, None).await.unwrap().instance_id;
    // 各自办结 apply → 都停在 msgWait。
    for iid in [&i1, &i2] {
        let a = open_task_at(&store, iid, "apply").await.unwrap();
        engine.complete_task(iid, &a, Variables::new()).await.unwrap();
    }

    // 跨实例相关键 B → 只唤醒 i2。
    engine
        .correlate_message(None, "verdictReceived", Some("B"), Variables::new())
        .await
        .unwrap();
    assert!(open_task_at(&store, &i2, "finalize").await.is_some(), "i2(orderId=B) 应被唤醒");
    assert!(open_task_at(&store, &i1, "finalize").await.is_none(), "i1(orderId=A) 不应被唤醒");
}

#[tokio::test]
async fn correlate_no_match_errors() {
    let (engine, store) = engine_with();
    let mut vars = Variables::new();
    vars.set("orderId", json!("X"));
    let iid = engine.start_process("msg_flow", vars, None).await.unwrap().instance_id;
    let a = open_task_at(&store, &iid, "apply").await.unwrap();
    engine.complete_task(&iid, &a, Variables::new()).await.unwrap();

    // 相关键不匹配 → 报错。
    let r = engine.correlate_message(Some(&iid), "verdictReceived", Some("WRONG"), Variables::new()).await;
    assert!(r.is_err(), "相关键不匹配应报错");
    // 消息名不匹配 → 报错。
    let r2 = engine.correlate_message(Some(&iid), "otherMsg", Some("X"), Variables::new()).await;
    assert!(r2.is_err(), "消息名不匹配应报错");
}
