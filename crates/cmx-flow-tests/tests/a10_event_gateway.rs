//! A10 端到端：事件网关（EventBasedGateway）——「谁先到走谁」竞速测试。
//!
//! 四个核心场景：
//! - 定时器先到：令牌走超时分支，消息竞争事件被取消
//! - 消息先到：令牌走消息分支，定时器竞争作业被取消
//! - 双定时器竞速：更早到期的那条分支胜出
//! - 令牌状态：到达事件网关时状态为 WaitingEventGateway

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, TestClock, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};

/// 「审批 or 超时」竞速：
///   提交 → 事件网关 → [分支A: 等消息 payment_confirmed] vs [分支B: 等3天超时]
///   → (两分支汇入) 下一步
const EBG_MSG_VS_TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="race" name="竞速流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="submit"/>
    <userTask id="submit" name="提交" flowable:assignee="申请人"/>
    <sequenceFlow id="s1" sourceRef="submit" targetRef="gw"/>
    <eventBasedGateway id="gw" name="等消息或超时"/>
    <sequenceFlow id="s_msg" sourceRef="gw" targetRef="catch_msg"/>
    <sequenceFlow id="s_tmr" sourceRef="gw" targetRef="catch_tmr"/>
    <intermediateCatchEvent id="catch_msg" name="等收款确认">
      <messageEventDefinition messageRef="payment_confirmed"/>
    </intermediateCatchEvent>
    <intermediateCatchEvent id="catch_tmr" name="等3天超时">
      <timerEventDefinition><timeDuration>PT72H</timeDuration></timerEventDefinition>
    </intermediateCatchEvent>
    <sequenceFlow id="s_msg_next" sourceRef="catch_msg" targetRef="notify"/>
    <sequenceFlow id="s_tmr_next" sourceRef="catch_tmr" targetRef="notify"/>
    <userTask id="notify" name="后续处理" flowable:assignee="经办人"/>
    <sequenceFlow id="s_done" sourceRef="notify" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 双定时器竞速：分支A=PT24H，分支B=PT48H → A先到胜出。
const EBG_DUAL_TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="dual_timer_race" name="双定时竞速" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="gw"/>
    <eventBasedGateway id="gw"/>
    <sequenceFlow id="s_a" sourceRef="gw" targetRef="catch_a"/>
    <sequenceFlow id="s_b" sourceRef="gw" targetRef="catch_b"/>
    <intermediateCatchEvent id="catch_a" name="24小时">
      <timerEventDefinition><timeDuration>PT24H</timeDuration></timerEventDefinition>
    </intermediateCatchEvent>
    <intermediateCatchEvent id="catch_b" name="48小时">
      <timerEventDefinition><timeDuration>PT48H</timeDuration></timerEventDefinition>
    </intermediateCatchEvent>
    <sequenceFlow id="s_a_next" sourceRef="catch_a" targetRef="result_a"/>
    <sequenceFlow id="s_b_next" sourceRef="catch_b" targetRef="result_b"/>
    <userTask id="result_a" name="走A分支" flowable:assignee="u_a"/>
    <userTask id="result_b" name="走B分支" flowable:assignee="u_b"/>
    <sequenceFlow id="s_ra" sourceRef="result_a" targetRef="done"/>
    <sequenceFlow id="s_rb" sourceRef="result_b" targetRef="done"/>
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
    engine.deploy(def).expect("部署成功");
    (engine, store, clock)
}

#[tokio::test]
async fn ebg_token_parks_as_waiting_event_gateway() {
    let (engine, store, _clock) = build(EBG_MSG_VS_TIMER_BPMN);
    let started = engine
        .start_process("race", Variables::new(), None)
        .await
        .unwrap();
    // 办结提交 → token 到事件网关，挂起为 WaitingEventGateway。
    let submit = &started.open_tasks[0];
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    let gw_tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "gw").unwrap();
    assert_eq!(gw_tok.state, TokenState::WaitingEventGateway, "事件网关令牌状态应为 WaitingEventGateway");
    // 应有一个 IntermediateCatch TimerJob（定时竞争分支）。
    assert_eq!(snap.jobs.len(), 1, "应有一个定时竞争作业");
    assert_eq!(snap.jobs[0].due_at, t0() + Duration::hours(72), "作业 due_at 应为 PT72H");
}

#[tokio::test]
async fn ebg_timer_wins_race() {
    let (engine, store, clock) = build(EBG_MSG_VS_TIMER_BPMN);
    let started = engine
        .start_process("race", Variables::new(), None)
        .await
        .unwrap();
    let submit = &started.open_tasks[0];
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // 拨快 > 72h → 定时器竞争分支胜出。
    clock.advance(Duration::hours(73));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "应触发一个定时竞争事件");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // 令牌应在"后续处理"任务（经过 catch_tmr → notify）。
    let open: Vec<&str> = snap.tasks.iter().filter(|t| !t.completed).map(|t| t.node_bpmn_id.as_str()).collect();
    assert_eq!(open, vec!["notify"], "定时器胜出后应推进到 notify 任务");
    // 所有竞争作业应已清空。
    assert!(snap.jobs.is_empty(), "定时器胜出后所有竞争作业应清空");
}

#[tokio::test]
async fn ebg_message_wins_race() {
    let (engine, store, clock) = build(EBG_MSG_VS_TIMER_BPMN);
    let started = engine
        .start_process("race", Variables::new(), None)
        .await
        .unwrap();
    let submit = &started.open_tasks[0];
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // 在定时器到期前投递消息 → 消息竞争分支胜出。
    clock.advance(Duration::hours(1));
    engine
        .correlate_message(
            Some(&started.instance_id),
            "payment_confirmed",
            None,
            Variables::new(),
        )
        .await
        .unwrap();

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // 令牌应在"后续处理"任务（经过 catch_msg → notify）。
    let open: Vec<&str> = snap.tasks.iter().filter(|t| !t.completed).map(|t| t.node_bpmn_id.as_str()).collect();
    assert_eq!(open, vec!["notify"], "消息胜出后应推进到 notify 任务");
    // 定时器竞争作业应已取消。
    assert!(snap.jobs.is_empty(), "消息胜出后定时器竞争作业应取消");
}

#[tokio::test]
async fn ebg_timer_does_not_fire_before_due() {
    let (engine, store, clock) = build(EBG_MSG_VS_TIMER_BPMN);
    let started = engine
        .start_process("race", Variables::new(), None)
        .await
        .unwrap();
    let submit = &started.open_tasks[0];
    engine
        .complete_task(&started.instance_id, &submit.id, Variables::new())
        .await
        .unwrap();

    // 只拨快 1h（< 72h）→ 不应触发。
    clock.advance(Duration::hours(1));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert!(fired.is_empty(), "未到期不应触发");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 1, "定时竞争作业仍应存在");
    let gw_tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "gw").unwrap();
    assert_eq!(gw_tok.state, TokenState::WaitingEventGateway, "事件网关令牌仍应等待");
}

#[tokio::test]
async fn ebg_dual_timer_earlier_wins() {
    let (engine, store, clock) = build(EBG_DUAL_TIMER_BPMN);
    let started = engine
        .start_process("dual_timer_race", Variables::new(), None)
        .await
        .unwrap();

    // 发起后就到了网关（无用户任务），应有两个竞争 TimerJob。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.jobs.len(), 2, "双定时器竞速应有 2 个竞争作业");

    // 拨快 25h → PT24H 的 catch_a 胜出。
    clock.advance(Duration::hours(25));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "应触发一个定时器（先到的）");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // 应到 result_a（走 A 分支）。
    let open: Vec<&str> = snap.tasks.iter().filter(|t| !t.completed).map(|t| t.node_bpmn_id.as_str()).collect();
    assert_eq!(open, vec!["result_a"], "PT24H 应先胜出走 A 分支");
    // 另一个 PT48H 竞争作业应已取消。
    assert!(snap.jobs.is_empty(), "胜出后另一竞争作业应取消");
}
