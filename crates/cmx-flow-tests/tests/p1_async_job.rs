//! P1 异步 Job Executor 端到端测试。
//!
//! 验证：
//! - serviceTask(flowable:async="true") 令牌停为 WaitingAsync，建 AsyncJob
//! - complete_async_job 推进令牌继续执行
//! - 同步 serviceTask 行为不变

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{DelegateContext, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};
use serde_json::json;

/// 简单 delegate：写一个标记变量。
struct MarkDelegate;

#[async_trait::async_trait]
impl JavaDelegate for MarkDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        ctx.variables.set("delegateRan", json!(true));
        Ok(())
    }
}

/// 恒失败 delegate：模拟外部调用一直报错，用于验证重试耗尽 → Incident 的死信路径。
struct FailDelegate;

#[async_trait::async_trait]
impl JavaDelegate for FailDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        Err("外部服务不可达".to_string().into())
    }
}

fn async_service_bpmn() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="async_svc" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="异步服务" flowable:class="testDelegate" flowable:async="true"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approval"/>
    <userTask id="approval" name="审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approval" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#.to_string()
}

fn sync_service_bpmn() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="sync_svc" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="同步服务" flowable:class="testDelegate"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approval"/>
    <userTask id="approval" name="审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approval" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#.to_string()
}

#[tokio::test]
async fn async_service_task_parks_as_waiting_async() {
    let def = compile(&async_service_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署应成功");
    engine.register_delegate("testDelegate", MarkDelegate);

    let result = engine
        .start_process("async_svc", Variables::new(), None)
        .await
        .expect("启动应成功");

    // 实例 Active（还没完成）。
    assert_eq!(result.state, InstanceState::Active);
    // 异步任务不产生 open_task（令牌是 WaitingAsync，不是 Waiting）。
    assert_eq!(result.open_tasks.len(), 0, "异步任务不应产生 open_task");

    // 验证令牌状态是 WaitingAsync。
    let snapshot = engine
        .store()
        .load_snapshot(&result.instance_id)
        .await
        .expect("应能加载快照");
    let tok = snapshot
        .tokens
        .iter()
        .find(|t| t.node_bpmn_id == "svc")
        .expect("应有 svc 令牌");
    assert_eq!(tok.state, TokenState::WaitingAsync, "令牌应为 WaitingAsync");
    // 验证 AsyncJob 已建。
    assert_eq!(snapshot.async_jobs.len(), 1, "应建一个 AsyncJob");
    assert_eq!(snapshot.async_jobs[0].delegate_key, "testDelegate");
}

#[tokio::test]
async fn async_job_complete_advances_token() {
    let def = compile(&async_service_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署应成功");
    engine.register_delegate("testDelegate", MarkDelegate);

    let started = engine
        .start_process("async_svc", Variables::new(), None)
        .await
        .expect("启动应成功");

    // 取到 job_id。
    let snapshot = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .expect("加载快照");
    let job_id = snapshot.async_jobs[0].id.clone();

    // 完成 job。
    let mut result_vars = Variables::new();
    result_vars.set("asyncResult", json!("done"));
    let result = engine
        .complete_async_job(&job_id, result_vars)
        .await
        .expect("complete_async_job 应成功");
    let result = result.expect("应返回 Some 结果");

    // 令牌应继续推进到 userTask。
    assert_eq!(result.state, InstanceState::Active);
    assert_eq!(result.open_tasks.len(), 1, "应推进到 userTask");
    assert_eq!(result.open_tasks[0].node_bpmn_id, "approval");
}

#[tokio::test]
async fn async_job_exhausts_retries_to_incident_then_recovers() {
    let def = compile(&async_service_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署应成功");
    engine.register_delegate("testDelegate", FailDelegate);

    let started = engine
        .start_process("async_svc", Variables::new(), None)
        .await
        .expect("启动应成功");
    let iid = started.instance_id.clone();

    // 初始 retries=3：三次抢占执行都失败，第 3 次耗尽 → 死信删除 + 令牌转 Incident。
    for _ in 0..3 {
        engine
            .run_async_jobs("worker-1", 60, 10)
            .await
            .expect("run_async_jobs 不应报错（delegate 失败在内部消化）");
    }

    let snap = engine.store().load_snapshot(&iid).await.expect("加载快照");
    let tok = snap
        .tokens
        .iter()
        .find(|t| t.node_bpmn_id == "svc")
        .expect("应有 svc 令牌");
    assert_eq!(
        tok.state,
        TokenState::Incident,
        "重试耗尽后令牌应转 Incident（不静默卡死）"
    );
    assert!(snap.async_jobs.is_empty(), "死信作业应已删除");

    // 恢复：retry_incident 重新激活令牌 → 经 run_to_wait 再次停为 WaitingAsync 并建新作业。
    engine
        .retry_incident(&iid, Variables::new())
        .await
        .expect("retry_incident 应成功");
    let snap2 = engine.store().load_snapshot(&iid).await.expect("加载快照");
    let tok2 = snap2
        .tokens
        .iter()
        .find(|t| t.node_bpmn_id == "svc")
        .expect("应有 svc 令牌");
    assert_eq!(
        tok2.state,
        TokenState::WaitingAsync,
        "重试 incident 后异步服务任务应重新挂起为 WaitingAsync"
    );
    assert_eq!(snap2.async_jobs.len(), 1, "应重建一个可被 worker 重抢的 AsyncJob");
}

#[tokio::test]
async fn sync_service_task_unchanged() {
    let def = compile(&sync_service_bpmn()).expect("应编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署应成功");
    engine.register_delegate("testDelegate", MarkDelegate);

    let result = engine
        .start_process("sync_svc", Variables::new(), None)
        .await
        .expect("启动应成功");

    // 同步 serviceTask 直接推进到 userTask。
    assert_eq!(result.state, InstanceState::Active);
    assert_eq!(result.open_tasks.len(), 1, "同步服务任务应直接推进到 userTask");
    assert_eq!(result.open_tasks[0].node_bpmn_id, "approval");
}
