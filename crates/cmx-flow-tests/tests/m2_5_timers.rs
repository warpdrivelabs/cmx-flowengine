//! M2.5 端到端：边界定时器 / 作业执行器（内存 + TestClock，始终可跑）。
//!
//! 验证「限时未办自动 X」这类审批刚需——用可注入时钟确定性驱动，无需真实等待：
//! - 中断型超时：超时中断 userTask，令牌走升级分支
//! - 非中断型超时：超时发旁路催办令牌，原 userTask 不中断继续等
//! - 办结早于超时：办结后作业撤销，再拨快不触发
//! - 未到期不触发：拨快不足时长，trigger 空转
//! - 重启恢复：新 Engine（同 store）仍能触发（作业落库了）

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, TestClock, Variables};
use cmx_flow_model::RuntimeStore;

/// 中断型：经理审批超时 PT24H 自动升级到总监。
const INTERRUPTING_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="timed_approve" name="限时审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="manager"/>
    <userTask id="manager" name="经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="manager" targetRef="done"/>
    <boundaryEvent id="timeout" attachedToRef="manager">
      <timerEventDefinition><timeDuration>PT24H</timeDuration></timerEventDefinition>
    </boundaryEvent>
    <sequenceFlow id="s2" sourceRef="timeout" targetRef="director"/>
    <userTask id="director" name="总监审批" flowable:assignee="总监"/>
    <sequenceFlow id="s3" sourceRef="director" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 非中断型：审批挂 PT2H 催办（不中断），审批仍可正常办结。
const NON_INTERRUPTING_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="remind_approve" name="催办审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="审批" flowable:assignee="审批人"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="done"/>
    <boundaryEvent id="remind" attachedToRef="approve" cancelActivity="false">
      <timerEventDefinition><timeDuration>PT2H</timeDuration></timerEventDefinition>
    </boundaryEvent>
    <sequenceFlow id="s2" sourceRef="remind" targetRef="notify"/>
    <userTask id="notify" name="发送催办" flowable:assignee="助理"/>
    <sequenceFlow id="s3" sourceRef="notify" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 固定起始时刻（避免依赖真实 now，确定性）。
fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 17, 9, 0, 0).unwrap()
}

fn build(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore, TestClock) {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(bpmn).expect("应能编译");
    let mut engine = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    engine.deploy(def).expect("部署应成功");
    (engine, store, clock)
}

#[tokio::test]
async fn interrupting_timer_escalates_on_timeout() {
    let (engine, store, clock) = build(INTERRUPTING_BPMN);
    let started = engine
        .start_process("timed_approve", Variables::new(), None)
        .await
        .unwrap();
    // 停在经理审批，且挂了一个定时器作业。
    assert_eq!(started.open_tasks.len(), 1);
    assert_eq!(started.open_tasks[0].node_bpmn_id, "manager");
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1, "应挂一个边界定时器作业");
    assert_eq!(snap.jobs[0].due_at, t0() + Duration::hours(24));

    // 拨快 25 小时 → 超时 → 触发 → 令牌走升级分支到总监审批。
    clock.advance(Duration::hours(25));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "应触发一个定时器");
    assert!(fired[0].cancel_activity, "应为中断型");

    let after = store.load_snapshot(&started.instance_id).await.unwrap();
    // 原经理任务作废，升级到总监审批。
    let open: Vec<&str> = after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["director"], "应升级到总监审批，原经理任务作废");
    assert_eq!(after.instance.state, InstanceState::Active);
    assert!(after.jobs.is_empty(), "触发后定时器作业应清空");
}

#[tokio::test]
async fn non_interrupting_timer_spawns_reminder_without_canceling() {
    let (engine, store, clock) = build(NON_INTERRUPTING_BPMN);
    let started = engine
        .start_process("remind_approve", Variables::new(), None)
        .await
        .unwrap();
    let approve_task = started.open_tasks[0].clone();

    // 拨快 3 小时 → 催办触发。
    clock.advance(Duration::hours(3));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1);
    assert!(!fired[0].cancel_activity, "应为非中断型");

    let after = store.load_snapshot(&started.instance_id).await.unwrap();
    let open: Vec<&str> = after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    // 原审批仍在 + 新增催办任务（非中断）。
    assert!(open.contains(&"approve"), "原审批不应被中断");
    assert!(open.contains(&"notify"), "应新增催办旁路任务");
    assert!(after.jobs.is_empty(), "单次触发后作业已消费");

    // 原审批仍可正常办结。
    let done = engine
        .complete_task(&started.instance_id, &approve_task.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(
        done.state,
        InstanceState::Active,
        "催办任务未办结，实例仍活动"
    );
}

#[tokio::test]
async fn completing_before_timeout_cancels_the_timer() {
    let (engine, store, clock) = build(INTERRUPTING_BPMN);
    let started = engine
        .start_process("timed_approve", Variables::new(), None)
        .await
        .unwrap();
    let manager_task = started.open_tasks[0].clone();

    // 在超时前办结经理审批 → 定时器应被撤销。
    let done = engine
        .complete_task(&started.instance_id, &manager_task.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(
        done.state,
        InstanceState::Completed,
        "经理审批完成 → 流程结束"
    );
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(snap.jobs.is_empty(), "办结后定时器作业应撤销");

    // 再拨快 25 小时 → 无到期作业可触发。
    clock.advance(Duration::hours(25));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert!(fired.is_empty(), "已撤销的定时器不应再触发");
}

#[tokio::test]
async fn not_yet_due_does_not_fire() {
    let (engine, store, clock) = build(INTERRUPTING_BPMN);
    let started = engine
        .start_process("timed_approve", Variables::new(), None)
        .await
        .unwrap();

    // 只拨快 1 小时（时长 24h）→ 未到期。
    clock.advance(Duration::hours(1));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert!(fired.is_empty(), "未到期不应触发");

    // 实例不变，仍停在经理审批，作业仍在。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1);
    let open: Vec<&str> = snap
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["manager"]);
}

#[tokio::test]
async fn timer_survives_restart_and_fires_from_store() {
    // 用一个 store + 共享 TestClock，跨「两个 Engine」模拟重启。
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(INTERRUPTING_BPMN).unwrap();

    let instance_id = {
        let mut e1 = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
        e1.deploy(def.clone()).unwrap();
        let started = e1
            .start_process("timed_approve", Variables::new(), None)
            .await
            .unwrap();
        started.instance_id
    };

    // 「重启」：全新 Engine，同 store，同（共享）时钟。
    let mut e2 = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    e2.deploy(def).unwrap();

    // 拨快超时 → 新 Engine 从库恢复作业并触发。
    clock.advance(Duration::hours(25));
    let fired = e2.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "重启后仍能触发——作业落库了");

    let after = store.load_snapshot(&instance_id).await.unwrap();
    let open: Vec<&str> = after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["director"], "重启后触发升级到总监");
}
