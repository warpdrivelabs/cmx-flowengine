//! A1/A4/A5 端到端：中间定时捕获事件 + 变量表达式定时器 + timeDate/timeCycle（内存 + TestClock）。
//!
//! 补齐 Flowable 对标缺口——审批场景「等 N 天后自动推进」的核心建模：
//! - A1 中间定时捕获：token 到 intermediateCatchEvent(timer) 挂起 WaitingTimer，到期后沿出边前进
//! - A4 变量表达式定时器：`${dueDate}` 从实例变量求值出时长/时刻（动态截止日期）
//! - A5 timeDate：绝对时刻到期；timeCycle：非中断边界周期催办（到期重排下一次）

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, TestClock, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};

/// 中间定时捕获：提交 → 等待 3 天（PT72H）→ 自动推进到通知任务。
const INTERMEDIATE_TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="wait_then_advance" name="等待推进" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="submit"/>
    <userTask id="submit" name="提交" flowable:assignee="申请人"/>
    <sequenceFlow id="s1" sourceRef="submit" targetRef="wait"/>
    <intermediateCatchEvent id="wait" name="等3天">
      <timerEventDefinition><timeDuration>PT72H</timeDuration></timerEventDefinition>
    </intermediateCatchEvent>
    <sequenceFlow id="s2" sourceRef="wait" targetRef="notify"/>
    <userTask id="notify" name="通知" flowable:assignee="经办人"/>
    <sequenceFlow id="s3" sourceRef="notify" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// A4 变量表达式：中间定时器时长从实例变量 `${waitDuration}` 读取（动态截止）。
const EXPR_TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="expr_wait" name="变量截止" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="wait"/>
    <intermediateCatchEvent id="wait" name="等到变量截止">
      <timerEventDefinition><timeDuration>${waitDuration}</timeDuration></timerEventDefinition>
    </intermediateCatchEvent>
    <sequenceFlow id="s1" sourceRef="wait" targetRef="after"/>
    <userTask id="after" name="到期办理" flowable:assignee="经办人"/>
    <sequenceFlow id="s2" sourceRef="after" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// A5 timeDate：中间定时器等到绝对时刻。
const TIMEDATE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="until_date" name="定时到期" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="wait"/>
    <intermediateCatchEvent id="wait" name="等到指定日期">
      <timerEventDefinition><timeDate>2026-07-20T09:00:00Z</timeDate></timerEventDefinition>
    </intermediateCatchEvent>
    <sequenceFlow id="s1" sourceRef="wait" targetRef="after"/>
    <userTask id="after" name="到期办理" flowable:assignee="经办人"/>
    <sequenceFlow id="s2" sourceRef="after" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// A5 timeCycle：非中断边界周期催办（R3/PT2H，最多 3 次），审批不中断。
const TIMECYCLE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="cycle_remind" name="周期催办" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="审批" flowable:assignee="审批人"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="done"/>
    <boundaryEvent id="remind" attachedToRef="approve" cancelActivity="false">
      <timerEventDefinition><timeCycle>R3/PT2H</timeCycle></timerEventDefinition>
    </boundaryEvent>
    <sequenceFlow id="s2" sourceRef="remind" targetRef="notify"/>
    <userTask id="notify" name="催办" flowable:assignee="助理"/>
    <sequenceFlow id="s3" sourceRef="notify" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 17, 9, 0, 0).unwrap()
}

fn build(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore, TestClock) {
    let store = InMemoryStore::new();
    let clock = TestClock::new(t0());
    let def = compile(bpmn).expect("应能编译");
    let engine = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    engine.deploy(def).expect("部署应成功");
    (engine, store, clock)
}

#[tokio::test]
async fn intermediate_timer_parks_then_advances_on_due() {
    let (engine, store, clock) = build(INTERMEDIATE_TIMER_BPMN);
    let started = engine
        .start_process("wait_then_advance", Variables::new(), None)
        .await
        .unwrap();
    // 停在提交任务。
    assert_eq!(started.open_tasks.len(), 1);
    let submit = started.open_tasks[0].clone();
    assert_eq!(submit.node_bpmn_id, "submit");

    // 办结提交 → token 到中间定时捕获，挂 WaitingTimer + 建一个到期作业。
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1, "应挂一个中间定时作业");
    assert_eq!(snap.jobs[0].due_at, t0() + Duration::hours(72));
    let waiting = snap.tokens.iter().find(|t| t.node_bpmn_id == "wait").unwrap();
    assert_eq!(waiting.state, TokenState::WaitingTimer, "应挂起为 WaitingTimer");
    assert!(
        snap.tasks.iter().all(|t| t.completed),
        "等待期间无未办结任务（提交已办结、通知未创建）"
    );

    // 未到期不触发。
    clock.advance(Duration::hours(1));
    assert!(engine.trigger_due_timers(100).await.unwrap().is_empty(), "1h < 72h 不应触发");

    // 拨快过 72h → 触发 → token 沿出边前进到通知任务。
    clock.advance(Duration::hours(72));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "应触发一个中间定时器");

    let after = store.load_snapshot(&started.instance_id).await.unwrap();
    let open: Vec<&str> = after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["notify"], "到期后自动推进到通知任务");
    assert!(after.jobs.is_empty(), "触发后中间定时作业应消费");
    assert_eq!(after.instance.state, InstanceState::Active);
}

#[tokio::test]
async fn expression_timer_resolves_duration_from_variable() {
    let (engine, store, clock) = build(EXPR_TIMER_BPMN);
    // 发起时给变量 waitDuration = "PT48H"（动态截止日期）。
    let mut vars = Variables::new();
    vars.set("waitDuration", serde_json::json!("PT48H"));
    let started = engine.start_process("expr_wait", vars, None).await.unwrap();

    // token 直接到中间定时捕获，挂 WaitingTimer，due = t0 + 48h（变量求值）。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1, "应挂一个变量表达式定时作业");
    assert_eq!(
        snap.jobs[0].due_at,
        t0() + Duration::hours(48),
        "due 应由 ${{waitDuration}}=PT48H 求值得出"
    );

    // 47h 不触发，48h 触发。
    clock.advance(Duration::hours(47));
    assert!(engine.trigger_due_timers(100).await.unwrap().is_empty());
    clock.advance(Duration::hours(2));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1);

    let after = store.load_snapshot(&started.instance_id).await.unwrap();
    let open: Vec<&str> = after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["after"], "变量截止到期后推进");
}

#[tokio::test]
async fn timedate_timer_fires_at_absolute_instant() {
    let (engine, store, clock) = build(TIMEDATE_BPMN);
    let started = engine.start_process("until_date", Variables::new(), None).await.unwrap();

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1);
    // 绝对时刻 2026-07-20T09:00:00Z。
    assert_eq!(snap.jobs[0].due_at, Utc.with_ymd_and_hms(2026, 7, 20, 9, 0, 0).unwrap());

    // t0=7/17 09:00，拨到 7/20 08:00 未到，7/20 10:00 到。
    clock.set(Utc.with_ymd_and_hms(2026, 7, 20, 8, 0, 0).unwrap());
    assert!(engine.trigger_due_timers(100).await.unwrap().is_empty(), "早于绝对时刻不触发");
    clock.set(Utc.with_ymd_and_hms(2026, 7, 20, 10, 0, 0).unwrap());
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "过绝对时刻应触发");

    let after = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(after.tasks.iter().any(|t| t.node_bpmn_id == "after" && !t.completed));
}

#[tokio::test]
async fn timecycle_boundary_rearms_up_to_repeat_limit() {
    let (engine, store, clock) = build(TIMECYCLE_BPMN);
    let started = engine.start_process("cycle_remind", Variables::new(), None).await.unwrap();
    let approve = started.open_tasks[0].clone();
    assert_eq!(approve.node_bpmn_id, "approve");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1, "应挂一个周期催办作业");
    assert_eq!(snap.jobs[0].cycle_interval_seconds, Some(7200), "R3/PT2H → 间隔 2h");
    assert_eq!(snap.jobs[0].cycle_remaining, Some(2), "首次消耗一次，剩余 2");

    // 第 1 次到期（+2h）：发催办令牌 + 重排下一次（剩 1）。
    clock.advance(Duration::hours(2));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "第 1 次催办触发");
    let s1 = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(s1.jobs.len(), 1, "周期定时器重排，仍有一个待触发作业");
    assert_eq!(s1.jobs[0].cycle_remaining, Some(1), "重排后剩余 1");
    assert!(
        s1.tasks.iter().any(|t| t.node_bpmn_id == "notify" && !t.completed),
        "第 1 次催办任务已生成"
    );
    assert!(
        s1.tasks.iter().any(|t| t.node_bpmn_id == "approve" && !t.completed),
        "原审批不中断（非中断型）"
    );

    // 第 2 次（+2h，剩 0）。
    clock.advance(Duration::hours(2));
    assert_eq!(engine.trigger_due_timers(100).await.unwrap().len(), 1, "第 2 次催办");
    let s2 = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(s2.jobs[0].cycle_remaining, Some(0), "重排后剩余 0");

    // 第 3 次（+2h，剩 0 → 用尽不再重排）。
    clock.advance(Duration::hours(2));
    assert_eq!(engine.trigger_due_timers(100).await.unwrap().len(), 1, "第 3 次催办");
    let s3 = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(s3.jobs.is_empty(), "R3 用尽后不再重排周期作业");

    // 第 4 次拨快 → 无作业可触发（周期已封顶 3 次）。
    clock.advance(Duration::hours(2));
    assert!(engine.trigger_due_timers(100).await.unwrap().is_empty(), "超过 R3 上限不再触发");
}
