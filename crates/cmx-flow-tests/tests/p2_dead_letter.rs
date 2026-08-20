//! P2 死信队列端到端测试。
//!
//! 验证：
//! - 异步 Job 重试耗尽 → 落死信表 + 令牌转 Incident（两视图一致）
//! - retry_dead_letter_job → 删死信行 + 令牌回 WaitingAsync + 建新 AsyncJob（可被重抢）
//! - 重投后 worker 再跑（delegate 这次成功）→ 令牌推进到 userTask
//! - discard_dead_letter_job → 删死信行，令牌保持 Incident

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables,
};
use cmx_flow_model::{RuntimeStore, TokenState};

/// 恒失败 delegate：用于把异步 Job 打到重试耗尽。
struct AlwaysFail;

#[async_trait::async_trait]
impl JavaDelegate for AlwaysFail {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        Err("外部服务持续不可达".to_string().into())
    }
}

/// 可切换 delegate：先失败后成功（模拟外部系统恢复），验证重投后能真正跑通。
struct FlipDelegate {
    fail: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl JavaDelegate for FlipDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        if self.fail.load(Ordering::SeqCst) {
            Err("暂时失败".to_string().into())
        } else {
            Ok(())
        }
    }
}

fn async_bpmn() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="dl_svc" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="异步服务" flowable:class="extCall" flowable:async="true"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approval"/>
    <userTask id="approval" name="审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approval" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#
        .to_string()
}

/// 把实例的异步 Job 打到重试耗尽（默认 retries=3 → 三次 run_async_jobs）。
async fn exhaust(engine: &Engine<InMemoryStore>) {
    for _ in 0..3 {
        engine
            .run_async_jobs("worker-1", 60, 10)
            .await
            .expect("run_async_jobs 不应外抛（delegate 失败内部消化）");
    }
}

#[tokio::test]
async fn exhausted_job_goes_to_dead_letter_and_incident() {
    let def = compile(&async_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署");
    engine.register_delegate("extCall", AlwaysFail);

    let started = engine
        .start_process("dl_svc", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();

    exhaust(&engine).await;

    // 死信表有一条。
    let dl = engine.list_dead_letter_jobs(50).await.expect("列死信");
    assert_eq!(dl.len(), 1, "耗尽应落一条死信");
    assert_eq!(dl[0].instance_id, iid);
    assert_eq!(dl[0].node_bpmn_id, "svc");
    assert_eq!(dl[0].delegate_key, "extCall");
    assert!(dl[0].error.contains("不可达"), "应记末次失败原因");

    // 令牌 Incident（两视图一致）。
    let snap = engine.store().load_snapshot(&iid).await.expect("快照");
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::Incident, "耗尽后令牌应 Incident");
    // 抢占池已空（作业已死信删除）。
    assert!(snap.async_jobs.is_empty(), "抢占池不应再有该作业");
}

#[tokio::test]
async fn retry_dead_letter_reschedules_and_runs_to_completion() {
    let def = compile(&async_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署");
    let fail = Arc::new(AtomicBool::new(true));
    engine.register_delegate("extCall", FlipDelegate { fail: fail.clone() });

    let started = engine
        .start_process("dl_svc", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();

    exhaust(&engine).await;
    let dl = engine.list_dead_letter_jobs(50).await.expect("列死信");
    assert_eq!(dl.len(), 1);
    let dl_id = dl[0].id.clone();

    // 外部系统恢复：delegate 改为成功。
    fail.store(false, Ordering::SeqCst);

    // 重投：删死信行 + 令牌回 WaitingAsync + 建新 job。
    let retried = engine.retry_dead_letter_job(&dl_id).await.expect("重投");
    assert!(retried, "应重投成功");
    assert!(
        engine.list_dead_letter_jobs(50).await.unwrap().is_empty(),
        "重投后死信行应删除"
    );
    let snap = engine.store().load_snapshot(&iid).await.expect("快照");
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::WaitingAsync, "重投后令牌回 WaitingAsync");
    assert_eq!(snap.async_jobs.len(), 1, "应重建一个可抢占的 AsyncJob");

    // worker 再跑一轮：这次 delegate 成功 → 令牌推进到 userTask。
    engine.run_async_jobs("worker-1", 60, 10).await.expect("再跑");
    let snap2 = engine.store().load_snapshot(&iid).await.expect("快照");
    assert_eq!(snap2.instance.state, InstanceState::Active);
    let open: Vec<_> = snap2.tasks.iter().filter(|t| !t.completed).collect();
    assert_eq!(open.len(), 1, "应推进到 userTask");
    assert_eq!(open[0].node_bpmn_id, "approval");
    // 死信表仍空。
    assert!(engine.list_dead_letter_jobs(50).await.unwrap().is_empty());
}

#[tokio::test]
async fn discard_dead_letter_removes_row_keeps_incident() {
    let def = compile(&async_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署");
    engine.register_delegate("extCall", AlwaysFail);

    let started = engine
        .start_process("dl_svc", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();

    exhaust(&engine).await;
    let dl = engine.list_dead_letter_jobs(50).await.expect("列死信");
    let dl_id = dl[0].id.clone();

    // 丢弃：删死信行。
    engine.discard_dead_letter_job(&dl_id).await.expect("丢弃");
    assert!(
        engine.list_dead_letter_jobs(50).await.unwrap().is_empty(),
        "丢弃后死信行应删除"
    );
    // 令牌保持 Incident（不自动推进）。
    let snap = engine.store().load_snapshot(&iid).await.expect("快照");
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::Incident, "丢弃不改变令牌 Incident 态");

    // 重投一个不存在的死信 id → false（幂等）。
    assert!(
        !engine.retry_dead_letter_job("nonexistent").await.unwrap(),
        "重投不存在的死信应返回 false"
    );
}
