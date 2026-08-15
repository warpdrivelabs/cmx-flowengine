//! ④ 端到端：撤回 / 取回（withdraw_process，内存态，始终可跑）。
//!
//! 验证：
//!   1) 下游未处理时，发起人取回 → 令牌回 start 后首个 userTask、回派发起人、实例仍 ACTIVE、
//!      WITHDRAW 台账；发起人改后 complete 沿正常流程重新前进（可改后重交）；
//!   2) strict（默认）下游已办结 → 拒绝；
//!   3) 非发起人取回 → 拒绝；
//!   4) 无发起人记录 → 拒绝；
//!   5) lenient 策略：下游已办结仍可取回；
//!   6) can_withdraw 只读判据返回原因。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, Variables};
use serde_json::json;

/// strict（默认，无 withdrawPolicy）：start → 经理 → 财务 → end。
const STRICT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="wd_strict" name="两级审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="mgr"/>
    <userTask id="mgr" name="经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="mgr" targetRef="fin"/>
    <userTask id="fin" name="财务审批" flowable:assignee="财务"/>
    <sequenceFlow id="s2" sourceRef="fin" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// lenient：process 带 cmx:withdrawPolicy="lenient"。
const LENIENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="wd_lenient" name="宽松两级" isExecutable="true" cmx:withdrawPolicy="lenient">
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
    let mut e = Engine::new(store.clone());
    e.deploy(compile(xml).expect("编译")).expect("部署");
    (e, store)
}

async fn start_with_initiator(e: &Engine<InMemoryStore>, key: &str, initiator: &str) -> String {
    let mut vars = Variables::new();
    vars.set("initiator", json!(initiator));
    e.start_process(key, vars, None).await.unwrap().instance_id
}

async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<(String, Option<String>)> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| (t.id.clone(), t.assignee.clone()))
}

#[tokio::test]
async fn withdraw_returns_to_initiator_and_can_resubmit() {
    let (e, store) = engine_with(STRICT);
    let iid = start_with_initiator(&e, "wd_strict", "u_z").await;
    // 起步停经理（静态办理人「经理」）。
    let (_mgr_id, mgr_assignee) = open_task_at(&store, &iid, "mgr").await.unwrap();
    assert_eq!(mgr_assignee.as_deref(), Some("经理"));

    // 发起人 u_z 取回。
    e.withdraw_process(&iid, "u_z", Some("填错了")).await.unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Active, "取回后实例仍进行中");
    // 落点 = 首个 userTask(mgr)，回派发起人 u_z。
    let (new_mgr_id, new_assignee) = open_task_at(&store, &iid, "mgr").await.unwrap();
    assert_eq!(new_assignee.as_deref(), Some("u_z"), "取回落点回派发起人");
    // 只剩一个待办（旧的作废）。
    assert_eq!(snap.tasks.iter().filter(|t| !t.completed).count(), 1);
    // WITHDRAW 台账。
    assert!(snap.delegations.iter().any(|d| d.kind == "WITHDRAW"));

    // 发起人改后重交 → 沿正常流程前进到财务。
    e.complete_task(&iid, &new_mgr_id, Variables::new()).await.unwrap();
    assert!(open_task_at(&store, &iid, "fin").await.is_some(), "重交后到财务");
}

#[tokio::test]
async fn strict_denies_when_downstream_completed() {
    let (e, store) = engine_with(STRICT);
    let iid = start_with_initiator(&e, "wd_strict", "u_z").await;
    // 经理办结 → 到财务（下游已办结一环）。
    let (mgr_id, _) = open_task_at(&store, &iid, "mgr").await.unwrap();
    e.complete_task(&iid, &mgr_id, Variables::new()).await.unwrap();

    // strict：已有环节办结 → 取回被拒。
    let r = e.withdraw_process(&iid, "u_z", None).await;
    assert!(r.is_err(), "strict 下游已办结应拒绝取回");
    let (ok, reason) = e.can_withdraw(&iid, "u_z").await.unwrap();
    assert!(!ok);
    assert!(reason.unwrap().contains("已"), "原因提示下游已处理");
}

#[tokio::test]
async fn denies_non_initiator() {
    let (e, store) = engine_with(STRICT);
    let iid = start_with_initiator(&e, "wd_strict", "u_z").await;
    let _ = open_task_at(&store, &iid, "mgr").await.unwrap();
    let r = e.withdraw_process(&iid, "someone_else", None).await;
    assert!(r.is_err(), "非发起人不可取回");
    let (ok, reason) = e.can_withdraw(&iid, "someone_else").await.unwrap();
    assert!(!ok);
    assert!(reason.unwrap().contains("发起人"));
}

#[tokio::test]
async fn denies_when_no_initiator_recorded() {
    let (e, _store) = engine_with(STRICT);
    // 不带 initiator 变量发起。
    let iid = e
        .start_process("wd_strict", Variables::new(), None)
        .await
        .unwrap()
        .instance_id;
    let r = e.withdraw_process(&iid, "anybody", None).await;
    assert!(r.is_err(), "无发起人记录不可取回");
}

#[tokio::test]
async fn lenient_allows_withdraw_after_completion() {
    let (e, store) = engine_with(LENIENT);
    let iid = start_with_initiator(&e, "wd_lenient", "u_z").await;
    // 经理办结 → 财务活动。
    let (mgr_id, _) = open_task_at(&store, &iid, "mgr").await.unwrap();
    e.complete_task(&iid, &mgr_id, Variables::new()).await.unwrap();
    assert!(open_task_at(&store, &iid, "fin").await.is_some());

    // lenient：仍可取回 → 回落点 mgr、回派发起人。
    e.withdraw_process(&iid, "u_z", None).await.unwrap();
    let (_, assignee) = open_task_at(&store, &iid, "mgr").await.unwrap();
    assert_eq!(assignee.as_deref(), Some("u_z"), "lenient 取回也回派发起人");
    // 财务待办已作废。
    assert!(open_task_at(&store, &iid, "fin").await.is_none());
}

#[tokio::test]
async fn can_withdraw_true_when_untouched() {
    let (e, store) = engine_with(STRICT);
    let iid = start_with_initiator(&e, "wd_strict", "u_z").await;
    let _ = open_task_at(&store, &iid, "mgr").await.unwrap();
    let (ok, reason) = e.can_withdraw(&iid, "u_z").await.unwrap();
    assert!(ok, "未处理时发起人可取回");
    assert!(reason.is_none());
}
