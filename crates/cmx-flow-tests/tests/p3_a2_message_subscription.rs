//! P3/A2 端到端：消息订阅持久化 + 消息启动事件（内存 + TestClock，始终可跑）。
//!
//! - P3：令牌到 MessageCatchEvent 时写入 pending_subs，flush 后 store 可查；
//!       correlate_message 后订阅被清理；cancel 后订阅被清理。
//! - A2：deploy_and_subscribe 写入 Start 订阅；start_by_message 按消息名找定义发起实例；
//!       重复部署只保留最新订阅（幂等）。

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, TestClock, Variables};
use cmx_flow_model::{MessageSubscriptionKind, RuntimeStore};

// ── 消息捕获流程（P3）：提交 → 等外部回调 → 完成 ──────────────────────────────
const CATCH_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="wait_payment" name="等待付款" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="submit"/>
    <userTask id="submit" name="提交申请" flowable:assignee="申请人"/>
    <sequenceFlow id="s1" sourceRef="submit" targetRef="wait_pay"/>
    <intermediateCatchEvent id="wait_pay" name="等待付款确认">
      <messageEventDefinition messageRef="payment_confirmed"/>
    </intermediateCatchEvent>
    <sequenceFlow id="s2" sourceRef="wait_pay" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

// ── 消息启动流程（A2）：消息触发发起审批 ─────────────────────────────────────────
const START_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="order_approval" name="订单审批" isExecutable="true">
    <startEvent id="start" name="接收订单">
      <messageEventDefinition messageRef="order_received"/>
    </startEvent>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="审批" flowable:assignee="审批人"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 17, 9, 0, 0).unwrap()
}

fn build_engine(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(bpmn).expect("应能编译");
    let engine = Engine::with_clock(store.clone(), Arc::new(clock));
    engine.deploy(def).expect("部署成功");
    (engine, store)
}

// ── P3 测试 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn p3_catch_subscription_written_on_waiting_message() {
    let (engine, store) = build_engine(CATCH_BPMN);
    let started = engine
        .start_process("wait_payment", Variables::new(), None)
        .await
        .unwrap();

    // 办结提交任务 → token 到 MessageCatchEvent → pending_subs flush → 订阅可查。
    let submit = started.open_tasks[0].clone();
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // 订阅表里应有一条 Catch 记录（message_name = "payment_confirmed"）。
    let sub = store
        .find_catch_subscription("payment_confirmed", None, "default")
        .await
        .unwrap();
    assert!(sub.is_some(), "应有 Catch 订阅记录");
    let sub = sub.unwrap();
    assert_eq!(sub.kind, MessageSubscriptionKind::Catch);
    assert_eq!(sub.message_name, "payment_confirmed");
    assert_eq!(sub.instance_id.as_deref(), Some(started.instance_id.as_str()));
}

#[tokio::test]
async fn p3_catch_subscription_cleared_after_correlate() {
    let (engine, store) = build_engine(CATCH_BPMN);
    let started = engine
        .start_process("wait_payment", Variables::new(), None)
        .await
        .unwrap();
    let submit = started.open_tasks[0].clone();
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // correlate → 令牌唤醒，订阅应删除。
    engine
        .correlate_message(
            Some(&started.instance_id),
            "payment_confirmed",
            None,
            Variables::new(),
        )
        .await
        .unwrap();

    let sub = store
        .find_catch_subscription("payment_confirmed", None, "default")
        .await
        .unwrap();
    assert!(sub.is_none(), "correlate 后订阅应被清理");
}

#[tokio::test]
async fn p3_catch_subscription_cleared_on_cancel() {
    let (engine, store) = build_engine(CATCH_BPMN);
    let started = engine
        .start_process("wait_payment", Variables::new(), None)
        .await
        .unwrap();
    let submit = started.open_tasks[0].clone();
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // cancel → 实例终止，订阅应删除。
    engine
        .cancel_process(&started.instance_id, Some("测试取消".into()))
        .await
        .unwrap();

    let sub = store
        .find_catch_subscription("payment_confirmed", None, "default")
        .await
        .unwrap();
    assert!(sub.is_none(), "取消后订阅应被清理");
}

// ── A2 测试 ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a2_deploy_writes_start_subscription() {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(START_BPMN).expect("应能编译");
    let engine = Engine::with_clock(store.clone(), Arc::new(clock));

    // deploy_and_subscribe 写 Start 订阅。
    engine
        .deploy_and_subscribe(def, "default")
        .await
        .unwrap();

    let sub = store
        .find_start_subscription("order_received", "default")
        .await
        .unwrap();
    assert!(sub.is_some(), "应有 Start 订阅");
    let sub = sub.unwrap();
    assert_eq!(sub.kind, MessageSubscriptionKind::Start);
    assert_eq!(sub.definition_key.as_deref(), Some("order_approval"));
}

#[tokio::test]
async fn a2_start_by_message_launches_instance() {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(START_BPMN).expect("应能编译");
    let engine = Engine::with_clock(store.clone(), Arc::new(clock));
    engine.deploy_and_subscribe(def, "default").await.unwrap();

    // start_by_message 按消息名找定义，发起实例。
    let mut vars = Variables::new();
    vars.set("order_id", serde_json::json!("ORD-001"));
    let result = engine
        .start_by_message("order_received", "default", vars, None, Default::default())
        .await
        .unwrap();

    assert_eq!(result.state, InstanceState::Active);
    // 实例停在审批任务。
    assert_eq!(result.open_tasks.len(), 1);
    assert_eq!(result.open_tasks[0].node_bpmn_id, "approve");
}

#[tokio::test]
async fn a2_redeploy_is_idempotent() {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let engine = Engine::with_clock(store.clone(), Arc::new(clock));

    // 部署两次 → 只保留最新（delete_start_subscriptions_by_def + 重写）。
    let def1 = compile(START_BPMN).expect("应能编译");
    engine.deploy_and_subscribe(def1, "default").await.unwrap();
    let def2 = compile(START_BPMN).expect("应能编译");
    engine.deploy_and_subscribe(def2, "default").await.unwrap();

    // 仍可正常发起（只有一个 Start 订阅不会冲突）。
    let result = engine
        .start_by_message("order_received", "default", Variables::new(), None, Default::default())
        .await
        .unwrap();
    assert_eq!(result.state, InstanceState::Active);
}

#[tokio::test]
async fn a2_start_by_unknown_message_returns_error() {
    let (engine, _store) = build_engine(START_BPMN);
    // 未用 deploy_and_subscribe，所以无 Start 订阅 → 应报 DefinitionNotFound。
    let result = engine
        .start_by_message("no_such_message", "default", Variables::new(), None, Default::default())
        .await;
    assert!(result.is_err(), "未知消息名应返回错误");
}
