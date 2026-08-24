//! P6 端到端：退回 / 驳回（reject_task，内存态，始终可跑）。
//!
//! 验证：两级审批流 start → 经理 → 财务 → end。
//!   1) 办结经理任务 → 令牌到财务，出现财务待办；
//!   2) 财务退回（默认目标=直接前驱经理）→ 令牌回经理节点，重现经理待办、财务任务作废；
//!   3) 退回携带意见变量 merge 进实例、REJECT 台账留痕；
//!   4) 显式退回到指定节点；
//!   5) 无前驱/多前驱时报错要求显式指定。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use serde_json::json;

/// 两级审批：start → 经理(mgr) → 财务(fin) → end。
const TWO_LEVEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="two_level" name="两级审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="mgr"/>
    <userTask id="mgr" name="经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="mgr" targetRef="fin"/>
    <userTask id="fin" name="财务审批" flowable:assignee="财务"/>
    <sequenceFlow id="s2" sourceRef="fin" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with(xml: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(xml).expect("编译")).expect("部署");
    (engine, store)
}

/// 取某实例当前在某节点的未办结任务 id（无则 None）。
async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<String> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| t.id.clone())
}

#[tokio::test]
async fn reject_returns_to_previous_user_task() {
    let (engine, store) = engine_with(TWO_LEVEL);
    let started = engine
        .start_process("two_level", Variables::new(), Some("R-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    // 停在经理审批。
    let mgr_task = open_task_at(&store, &iid, "mgr").await.expect("应有经理待办");
    // 经理办结 → 令牌到财务。
    engine
        .complete_task(&iid, &mgr_task, Variables::new())
        .await
        .unwrap();
    let fin_task = open_task_at(&store, &iid, "fin").await.expect("应有财务待办");
    assert!(open_task_at(&store, &iid, "mgr").await.is_none(), "经理任务已办结");

    // 财务退回（默认目标=直接前驱经理），带驳回意见变量。
    let mut vars = Variables::new();
    vars.set("rejectReason", json!("金额存疑"));
    engine
        .reject_task(&iid, &fin_task, "财务", None, Some("请核对金额"), vars)
        .await
        .unwrap();

    // 令牌回经理节点，重现经理待办；财务任务已作废（completed）。
    let snap = store.load_snapshot(&iid).await.unwrap();
    let mgr_again = open_task_at(&store, &iid, "mgr").await;
    assert!(mgr_again.is_some(), "退回后应重现经理待办");
    assert!(mgr_again != Some(mgr_task.clone()), "重现的是新任务，非原任务");
    assert!(
        open_task_at(&store, &iid, "fin").await.is_none(),
        "财务任务应已作废"
    );
    // 意见变量已 merge。
    assert_eq!(snap.instance.variables.get("rejectReason"), Some(&json!("金额存疑")));
    // REJECT 台账留痕。
    assert!(
        snap.delegations.iter().any(|d| d.kind == "REJECT"),
        "应有 REJECT 台账"
    );
}

#[tokio::test]
async fn reject_to_explicit_target() {
    let (engine, store) = engine_with(TWO_LEVEL);
    let started = engine
        .start_process("two_level", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();
    let mgr_task = open_task_at(&store, &iid, "mgr").await.unwrap();
    engine.complete_task(&iid, &mgr_task, Variables::new()).await.unwrap();
    let fin_task = open_task_at(&store, &iid, "fin").await.unwrap();

    // 显式退回到 mgr。
    engine
        .reject_task(&iid, &fin_task, "财务", Some("mgr"), None, Variables::new())
        .await
        .unwrap();
    assert!(open_task_at(&store, &iid, "mgr").await.is_some(), "应回到 mgr");
}

#[tokio::test]
async fn reject_to_non_usertask_target_errors() {
    let (engine, store) = engine_with(TWO_LEVEL);
    let started = engine
        .start_process("two_level", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();
    let mgr_task = open_task_at(&store, &iid, "mgr").await.unwrap();
    engine.complete_task(&iid, &mgr_task, Variables::new()).await.unwrap();
    let fin_task = open_task_at(&store, &iid, "fin").await.unwrap();

    // 退回到 start（非 userTask）→ 报错。
    let r = engine
        .reject_task(&iid, &fin_task, "财务", Some("start"), None, Variables::new())
        .await;
    assert!(r.is_err(), "退回到非用户任务应报错");

    // 退回不存在的节点 → 报错。
    let r2 = engine
        .reject_task(&iid, &fin_task, "财务", Some("nope"), None, Variables::new())
        .await;
    assert!(r2.is_err(), "退回到不存在节点应报错");
}

#[tokio::test]
async fn reject_first_task_no_predecessor_errors() {
    // 经理是第一个 userTask，其前驱是 startEvent（回溯不到 userTask）→ 默认退回报错。
    let (engine, store) = engine_with(TWO_LEVEL);
    let started = engine
        .start_process("two_level", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();
    let mgr_task = open_task_at(&store, &iid, "mgr").await.unwrap();

    let r = engine
        .reject_task(&iid, &mgr_task, "经理", None, None, Variables::new())
        .await;
    assert!(r.is_err(), "首个用户任务无前驱用户任务，默认退回应报错要求显式指定");
}
