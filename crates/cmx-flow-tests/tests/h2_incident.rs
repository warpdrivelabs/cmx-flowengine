//! H2 端到端：Incident 机制（serviceTask 失败挂起 + 人工重试恢复，内存态）。
//!
//! 验证生产可用性硬门槛：
//!   1) serviceTask delegate 失败 → 令牌停 Incident 态，**实例不丢、不终止**（仍 Active）；
//!   2) 失败原因 + 重试次数记进实例变量 `__incident`；
//!   3) retry_incident 重跑：仍失败 → 重试次数累加、仍 Incident；
//!   4) 修正数据后 retry_incident → delegate 成功 → 令牌离开、流程继续到下一等待态/完成。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, Engine, InMemoryStore, InstanceState, JavaDelegate, RuntimeStore, TokenState,
    Variables,
};
use serde_json::json;

/// 一个「按变量决定成功/失败」的 delegate：变量 `fixed==true` 才成功，否则报错。
/// 另记调用次数，验证重试真的重跑了 delegate。
struct FlakyDelegate {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl JavaDelegate for FlakyDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let fixed = ctx
            .variables
            .get("fixed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if fixed {
            ctx.variables.set("svcDone", json!(true));
            Ok(())
        } else {
            Err("外部依赖不可用（模拟故障）".into())
        }
    }
}

/// start → serviceTask(svc) → userTask(review) → end。
const SVC_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="svc_flow" name="含服务任务" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="调外部" flowable:delegateExpression="flaky"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="review"/>
    <userTask id="review" name="人工复核" flowable:assignee="mgr"/>
    <sequenceFlow id="s2" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn build(calls: Arc<AtomicUsize>) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(SVC_FLOW).expect("编译")).expect("部署");
    engine.register_delegate("flaky", FlakyDelegate { calls });
    (engine, store)
}

#[tokio::test]
async fn service_failure_creates_incident_and_survives() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (engine, store) = build(calls.clone());

    // 起流程：svc 立即执行 → 失败 → Incident。
    let started = engine
        .start_process("svc_flow", Variables::new(), Some("H2-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "delegate 应被调一次");

    let snap = store.load_snapshot(&iid).await.unwrap();
    // 实例仍 Active（未丢、未终止）。
    assert_eq!(snap.instance.state, InstanceState::Active, "实例应保留为 Active");
    // 有一个 Incident 令牌停在 svc。
    let inc = snap.tokens.iter().find(|t| t.state == TokenState::Incident);
    assert!(inc.is_some(), "应有 Incident 令牌");
    assert_eq!(inc.unwrap().node_bpmn_id, "svc", "Incident 停在 svc 节点");
    // review 任务不应存在（流程没越过 svc）。
    assert!(
        !snap.tasks.iter().any(|t| t.node_bpmn_id == "review" && !t.completed),
        "svc 未过，不应有 review 待办"
    );
    // __incident 变量记了原因 + 重试次数。
    let incident = snap.instance.variables.get("__incident").cloned().unwrap();
    assert_eq!(incident["svc"]["retries"], json!(1));
    assert!(incident["svc"]["reason"].as_str().unwrap().contains("故障"));
}

#[tokio::test]
async fn retry_without_fix_increments_and_stays_incident() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (engine, store) = build(calls.clone());
    let started = engine
        .start_process("svc_flow", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    // 不修数据直接重试 → 仍失败，重试次数累加。
    engine.retry_incident(&iid, Variables::new()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "重试应重跑 delegate");

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert!(
        snap.tokens.iter().any(|t| t.state == TokenState::Incident),
        "未修数据，仍 Incident"
    );
    let incident = snap.instance.variables.get("__incident").cloned().unwrap();
    assert_eq!(incident["svc"]["retries"], json!(2), "重试次数累加到 2");
}

#[tokio::test]
async fn retry_with_fix_recovers_and_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (engine, store) = build(calls.clone());
    let started = engine
        .start_process("svc_flow", Variables::new(), None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    // 修正数据（fixed=true）后重试 → delegate 成功 → 令牌越过 svc → 停在 review 待办。
    let mut fix = Variables::new();
    fix.set("fixed", json!(true));
    engine.retry_incident(&iid, fix).await.unwrap();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // 无 Incident 令牌了。
    assert!(
        !snap.tokens.iter().any(|t| t.state == TokenState::Incident),
        "修复后不应再有 Incident"
    );
    // svcDone 变量已写（delegate 成功副作用）。
    assert_eq!(snap.instance.variables.get("svcDone"), Some(&json!(true)));
    // 流程推进到 review 待办。
    assert!(
        snap.tasks.iter().any(|t| t.node_bpmn_id == "review" && !t.completed),
        "修复后应推进到 review 待办"
    );
    // __incident 已清除该节点（成功执行时 clear_incident）。
    let inc = snap.instance.variables.get("__incident");
    assert!(
        inc.is_none() || inc == Some(&json!(null)) || inc.unwrap().get("svc").is_none(),
        "成功后应清掉 svc 的 incident 痕迹"
    );
}

#[tokio::test]
async fn retry_without_incident_is_idempotent_noop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (engine, store) = build(calls.clone());
    // 用 fixed=true 起流程 → svc 直接成功 → 停 review，无 incident。
    let mut vars = Variables::new();
    vars.set("fixed", json!(true));
    let started = engine
        .start_process("svc_flow", vars, None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();
    let before = store.load_snapshot(&iid).await.unwrap();

    // 无 incident 时重试 → 幂等无操作。
    engine.retry_incident(&iid, Variables::new()).await.unwrap();
    let after = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(before.tasks.len(), after.tasks.len(), "无 incident 重试不改状态");
}
