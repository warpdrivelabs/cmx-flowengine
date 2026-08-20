//! A7 外部 Worker Task 端到端测试（内存，始终可跑）。
//!
//! 验证：
//! - serviceTask type=external-worker → 令牌停 WaitingAsync，建带 topic 的作业（delegate 空）
//! - 进程内 poller（run_async_jobs / topic=None）**不**领走外部作业（topic 隔离）
//! - 外部 worker 按 topic acquire → 领到作业 → complete_async_job 推进令牌
//! - 按不同 topic acquire 领不到（主题隔离）

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables,
};
use cmx_flow_model::{RuntimeStore, TokenState};
use serde_json::json;

/// 进程内 delegate（用于验证 poller 领的是进程内作业，不碰外部作业）。
struct InProcDelegate;
#[async_trait::async_trait]
impl JavaDelegate for InProcDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        ctx.variables.set("inProcRan", json!(true));
        Ok(())
    }
}

/// start → svc(external-worker, topic=pay) → approve(userTask) → end
const EXT_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a7" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="外部支付" flowable:type="external-worker" flowable:topic="pay"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approve"/>
    <userTask id="approve" name="审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approve" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#;

async fn start(store: &InMemoryStore) -> (Engine<InMemoryStore>, String) {
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(EXT_BPMN).expect("编译")).expect("部署");
    engine.register_delegate("someInProc", InProcDelegate);
    let started = engine
        .start_process("a7", Variables::new(), None)
        .await
        .expect("启动");
    (engine, started.instance_id)
}

#[tokio::test]
async fn external_worker_task_parks_with_topic_job() {
    let store = InMemoryStore::new();
    let (engine, iid) = start(&store).await;

    let snap = engine.store().load_snapshot(&iid).await.unwrap();
    // 令牌 WaitingAsync。
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::WaitingAsync);
    // 作业带 topic、delegate 空。
    assert_eq!(snap.async_jobs.len(), 1);
    assert_eq!(snap.async_jobs[0].topic.as_deref(), Some("pay"));
    assert_eq!(snap.async_jobs[0].delegate_key, "", "外部作业 delegate 应为空");
}

#[tokio::test]
async fn in_process_poller_does_not_grab_external_job() {
    let store = InMemoryStore::new();
    let (engine, iid) = start(&store).await;

    // 进程内 poller 跑几轮：不应领走外部作业（topic 隔离），令牌仍 WaitingAsync。
    for _ in 0..3 {
        let done = engine.run_async_jobs("in-proc", 60, 10).await.unwrap();
        assert_eq!(done, 0, "poller 不应处理外部 topic 作业");
    }
    let snap = engine.store().load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::WaitingAsync, "外部作业未被误领，令牌仍等待");
    // 作业未被锁定（poller 没碰它）。
    assert!(snap.async_jobs[0].locked_by.is_none());
}

#[tokio::test]
async fn external_worker_acquires_by_topic_and_completes() {
    let store = InMemoryStore::new();
    let (engine, iid) = start(&store).await;

    // 错误 topic：领不到。
    let none = engine
        .acquire_async_jobs("w-ext", Some("email"), 60, 10)
        .await
        .unwrap();
    assert!(none.is_empty(), "不同 topic 不应领到");

    // 正确 topic：领到一个。
    let jobs = engine
        .acquire_async_jobs("w-ext", Some("pay"), 60, 10)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "按 pay topic 应领到");
    assert_eq!(jobs[0].topic.as_deref(), Some("pay"));
    let job_id = jobs[0].id.clone();

    // worker 执行完外部调用 → complete → 令牌推进到 userTask。
    let mut vars = Variables::new();
    vars.set("payResult", json!("ok"));
    let result = engine
        .complete_async_job(&job_id, vars)
        .await
        .unwrap()
        .expect("应返回结果");
    assert_eq!(result.state, InstanceState::Active);
    assert_eq!(result.open_tasks.len(), 1);
    assert_eq!(result.open_tasks[0].node_bpmn_id, "approve");

    // 变量已回写。
    let snap = engine.store().load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.variables.get("payResult"), Some(&json!("ok")));
}

#[tokio::test]
async fn acquired_external_job_locked_not_reacquired() {
    let store = InMemoryStore::new();
    let (engine, _iid) = start(&store).await;

    let first = engine
        .acquire_async_jobs("w1", Some("pay"), 60, 10)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    // 第二个 worker 同 topic 立即再抢：锁未过期，领不到。
    let second = engine
        .acquire_async_jobs("w2", Some("pay"), 60, 10)
        .await
        .unwrap();
    assert!(second.is_empty(), "已锁定作业不应被重复领取");
}
