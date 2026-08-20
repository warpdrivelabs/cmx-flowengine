//! A6 活动历史端到端测试（内存 + TestClock，始终可跑）。
//!
//! 验证：
//! - 顺序流每个节点闭合一条活动，含 enter/exit/duration/type/name
//! - userTask 等待时长跨推进段正确（TestClock 推进 N 小时 → duration_ms 匹配）
//! - 并行网关多令牌各记各的分支活动
//! - 活动按进入时刻升序返回

use std::sync::Arc;

use chrono::Duration;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, TestClock, Variables};
use cmx_flow_model::RuntimeStore;

fn t0() -> chrono::DateTime<chrono::Utc> {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

fn build(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore, TestClock) {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let mut engine = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    let def = compile(bpmn).expect("应编译");
    engine.deploy(def).expect("部署");
    (engine, store, clock)
}

/// start → 审批(userTask) → end。
const LINEAR_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a6_linear" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approval"/>
    <userTask id="approval" name="审批" flowable:assignee="user1"/>
    <sequenceFlow id="s1" sourceRef="approval" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#;

/// start → 并行分裂 → (A 会审 / B 复核) → 汇合 → end。
const PARALLEL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a6_parallel" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="taskA"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="taskB"/>
    <userTask id="taskA" name="会审" flowable:assignee="ua"/>
    <userTask id="taskB" name="复核" flowable:assignee="ub"/>
    <sequenceFlow id="s3" sourceRef="taskA" targetRef="join"/>
    <sequenceFlow id="s4" sourceRef="taskB" targetRef="join"/>
    <parallelGateway id="join"/>
    <sequenceFlow id="s5" sourceRef="join" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn linear_flow_records_activity_per_node() {
    let (engine, store, clock) = build(LINEAR_BPMN);
    let started = engine
        .start_process("a6_linear", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();

    // 启动后令牌停在 approval：start 已闭合（穿透），approval 活动仍开着（未记）。
    let acts = store.list_activities_by_instance(&iid).await.unwrap();
    assert_eq!(acts.len(), 1, "仅 start 闭合");
    assert_eq!(acts[0].activity_bpmn_id, "start");
    assert_eq!(acts[0].activity_type, "startEvent");

    // 用户在 approval 停留 5 小时后办结。
    clock.advance(Duration::hours(5));
    let task_id = started.open_tasks[0].id.clone();
    engine
        .complete_task(&iid, &task_id, Variables::new())
        .await
        .expect("办结");

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Completed);

    let acts = store.list_activities_by_instance(&iid).await.unwrap();
    // start + approval + end。
    let ids: Vec<&str> = acts.iter().map(|a| a.activity_bpmn_id.as_str()).collect();
    assert_eq!(ids, vec!["start", "approval", "end"], "三节点按序");

    let approval = acts.iter().find(|a| a.activity_bpmn_id == "approval").unwrap();
    assert_eq!(approval.activity_type, "userTask");
    assert_eq!(approval.activity_name.as_deref(), Some("审批"));
    assert_eq!(approval.assignee.as_deref(), Some("user1"), "userTask 记办理人");
    assert_eq!(
        approval.duration_ms,
        Duration::hours(5).num_milliseconds(),
        "等待时长跨段正确（5 小时）"
    );
    // start 穿透：零时长。
    let start = acts.iter().find(|a| a.activity_bpmn_id == "start").unwrap();
    assert_eq!(start.duration_ms, 0, "start 穿透零时长");
}

#[tokio::test]
async fn parallel_flow_records_each_branch() {
    let (engine, store, clock) = build(PARALLEL_BPMN);
    let started = engine
        .start_process("a6_parallel", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();

    // 两个并行 userTask 各停留不同时长后办结。
    clock.advance(Duration::hours(2));
    let ta = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "taskA")
        .unwrap()
        .id
        .clone();
    engine
        .complete_task(&iid, &ta, Variables::new())
        .await
        .expect("办结A");
    clock.advance(Duration::hours(1));
    let tb = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "taskB")
        .unwrap()
        .id
        .clone();
    engine
        .complete_task(&iid, &tb, Variables::new())
        .await
        .expect("办结B");

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Completed);

    let acts = store.list_activities_by_instance(&iid).await.unwrap();
    // 两分支活动都在。
    let a = acts.iter().find(|x| x.activity_bpmn_id == "taskA").unwrap();
    let b = acts.iter().find(|x| x.activity_bpmn_id == "taskB").unwrap();
    assert_eq!(a.assignee.as_deref(), Some("ua"));
    assert_eq!(b.assignee.as_deref(), Some("ub"));
    assert_eq!(a.duration_ms, Duration::hours(2).num_milliseconds(), "A 停 2h");
    assert_eq!(
        b.duration_ms,
        Duration::hours(3).num_milliseconds(),
        "B 停 3h（2h 到 A 办结 + 1h 到 B 办结）"
    );
    // fork/join 网关活动也各记一条。
    assert!(acts.iter().any(|x| x.activity_bpmn_id == "fork"));
    assert!(acts.iter().any(|x| x.activity_bpmn_id == "join"));

    // 升序不变式：entered_at 单调不减。
    for w in acts.windows(2) {
        assert!(w[0].entered_at <= w[1].entered_at, "按进入时刻升序");
    }
}
