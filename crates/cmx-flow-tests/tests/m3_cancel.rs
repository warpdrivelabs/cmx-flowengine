//! M3 端到端：实例取消 / 终止（内存态，始终可跑）。
//!
//! 审批流刚需——撤单 / 作废。验证 cancel_process：
//! - 跑到 userTask 等待态 → cancel → 实例 Terminated、无未办结任务
//! - 会签中途 cancel → 全部并行任务作废、MI 域收口
//! - 幂等：对已终止实例再 cancel 无副作用

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};
use serde_json::json;

/// 单任务审批：发起 → 审批(userTask) → 结束
const SIMPLE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="simple_approve" name="单人审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="审批" flowable:assignee="manager"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 会签：发起 → [会签 N 人] → 结束
const COUNTERSIGN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="cs" name="会签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="sign"/>
    <userTask id="sign" name="会签" flowable:assignee="${approver}">
      <multiInstanceLoopCharacteristics isSequential="false"
           flowable:collection="approvers" flowable:elementVariable="approver"/>
    </userTask>
    <sequenceFlow id="s1" sourceRef="sign" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_for(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let def = compile(bpmn).expect("应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    (engine, store)
}

#[tokio::test]
async fn cancel_waiting_instance_terminates_it() {
    let (engine, store) = engine_for(SIMPLE_BPMN);
    let started = engine
        .start_process("simple_approve", Variables::new(), None)
        .await
        .unwrap();
    assert_eq!(started.state, InstanceState::Active);
    assert_eq!(started.open_tasks.len(), 1);

    // 撤单。
    let canceled = engine
        .cancel_process(&started.instance_id, Some("申请人撤回".into()))
        .await
        .unwrap();
    assert_eq!(
        canceled.state,
        InstanceState::Terminated,
        "取消后应为终止态"
    );
    assert!(canceled.open_tasks.is_empty(), "无未办结任务");

    // 落库校验：实例终止、令牌全 Ended、无未办结任务、原因入库。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Terminated);
    assert!(snap.instance.ended_at.is_some());
    assert!(snap.tokens.iter().all(|t| t.state == TokenState::Ended));
    assert_eq!(snap.tasks.iter().filter(|t| !t.completed).count(), 0);
    assert_eq!(
        snap.instance
            .variables
            .get("_cancelReason")
            .and_then(|v| v.as_str()),
        Some("申请人撤回")
    );
}

#[tokio::test]
async fn cancel_countersign_voids_all_parallel_tasks() {
    let (engine, store) = engine_for(COUNTERSIGN_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["a", "b", "c"]));
    let started = engine.start_process("cs", vars, None).await.unwrap();
    assert_eq!(started.open_tasks.len(), 3, "会签 3 个并行任务");

    // 中途撤单。
    let canceled = engine
        .cancel_process(&started.instance_id, None)
        .await
        .unwrap();
    assert_eq!(canceled.state, InstanceState::Terminated);
    assert!(canceled.open_tasks.is_empty(), "3 个会签任务应全部作废");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.tasks.iter().filter(|t| !t.completed).count(), 0);
    assert!(snap.mi_scopes.iter().all(|s| s.finished), "MI 域应收口");
}

#[tokio::test]
async fn cancel_is_idempotent() {
    let (engine, _store) = engine_for(SIMPLE_BPMN);
    let started = engine
        .start_process("simple_approve", Variables::new(), None)
        .await
        .unwrap();
    let first = engine
        .cancel_process(&started.instance_id, None)
        .await
        .unwrap();
    assert_eq!(first.state, InstanceState::Terminated);
    // 再次取消：幂等，仍返回终止态，不报错。
    let second = engine
        .cancel_process(&started.instance_id, None)
        .await
        .unwrap();
    assert_eq!(second.state, InstanceState::Terminated);
}

#[tokio::test]
async fn cannot_complete_task_after_cancel() {
    let (engine, _store) = engine_for(SIMPLE_BPMN);
    let started = engine
        .start_process("simple_approve", Variables::new(), None)
        .await
        .unwrap();
    let task = started.open_tasks[0].clone();
    engine
        .cancel_process(&started.instance_id, None)
        .await
        .unwrap();

    // 取消后原任务已作废，办结应失败。
    let err = engine
        .complete_task(&started.instance_id, &task.id, Variables::new())
        .await;
    assert!(err.is_err(), "取消后的任务不可再办结");
}
