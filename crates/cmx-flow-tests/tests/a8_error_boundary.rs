//! A8 错误边界事件端到端测试（内存，始终可跑）。
//!
//! 验证：
//! - serviceTask 抛类型化 BPMN 错误 → 命中匹配 errorCode 的边界 → 令牌走错误处理分支
//! - catch-all 边界（无 errorCode）兜任意 BPMN 错误
//! - Generic 失败（普通 Err）不被错误边界捕获 → 仍走 Incident
//! - 有 Bpmn 错误但无匹配边界 → 回退 Incident

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, DelegateError, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables,
};
use cmx_flow_model::{RuntimeStore, TokenState};

/// 抛指定 BPMN errorCode 的 delegate。
struct BpmnErrDelegate {
    code: &'static str,
}
#[async_trait::async_trait]
impl JavaDelegate for BpmnErrDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), DelegateError> {
        Err(DelegateError::bpmn(self.code, "业务异常"))
    }
}

/// 抛普通（Generic）失败的 delegate。
struct GenericErrDelegate;
#[async_trait::async_trait]
impl JavaDelegate for GenericErrDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), DelegateError> {
        Err("外部服务超时".into())
    }
}

/// start → svc(serviceTask) → approve(userTask) → end
/// svc 上挂错误边界 `boundaryEvent`（errorRef=CODE）→ handle(userTask) → endErr
fn bpmn_with_error_boundary(error_ref_attr: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a8" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="外部调用" flowable:class="extCall"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approve"/>
    <userTask id="approve" name="正常审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approve" targetRef="end"/>
    <endEvent id="end"/>
    <boundaryEvent id="onErr" attachedToRef="svc">
      <errorEventDefinition {error_ref_attr}/>
    </boundaryEvent>
    <sequenceFlow id="s3" sourceRef="onErr" targetRef="handle"/>
    <userTask id="handle" name="异常处理" flowable:assignee="ops"/>
    <sequenceFlow id="s4" sourceRef="handle" targetRef="endErr"/>
    <endEvent id="endErr"/>
  </process>
</definitions>"#
    )
}

async fn run(bpmn: &str, delegate: impl JavaDelegate + 'static) -> (InMemoryStore, String) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(bpmn).expect("编译")).expect("部署");
    engine.register_delegate("extCall", delegate);
    let started = engine
        .start_process("a8", Variables::new(), None)
        .await
        .expect("启动");
    (store, started.instance_id)
}

#[tokio::test]
async fn matching_bpmn_error_routes_to_boundary_branch() {
    // 边界声明 errorRef="E_CREDIT"，delegate 抛 E_CREDIT → 命中，走异常处理分支。
    let bpmn = bpmn_with_error_boundary(r#"errorRef="E_CREDIT""#);
    let (store, iid) = run(&bpmn, BpmnErrDelegate { code: "E_CREDIT" }).await;

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Active);
    // 令牌应停在异常处理任务，不在正常审批。
    let tok = snap.tokens.iter().find(|t| t.state == TokenState::Waiting).unwrap();
    assert_eq!(tok.node_bpmn_id, "handle", "应路由到异常处理分支");
    let open: Vec<_> = snap.tasks.iter().filter(|t| !t.completed).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].node_bpmn_id, "handle");
    assert_eq!(open[0].assignee.as_deref(), Some("ops"));
    // 无 Incident 令牌。
    assert!(!snap.tokens.iter().any(|t| t.state == TokenState::Incident));
}

#[tokio::test]
async fn catch_all_boundary_catches_any_code() {
    // 边界无 errorRef（catch-all），delegate 抛任意 code → 命中。
    let bpmn = bpmn_with_error_boundary("");
    let (store, iid) = run(&bpmn, BpmnErrDelegate { code: "ANYTHING" }).await;

    let snap = store.load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.state == TokenState::Waiting).unwrap();
    assert_eq!(tok.node_bpmn_id, "handle", "catch-all 应捕获任意 code");
    assert!(!snap.tokens.iter().any(|t| t.state == TokenState::Incident));
}

#[tokio::test]
async fn generic_error_not_caught_goes_incident() {
    // 边界声明 errorRef="E_CREDIT"，但 delegate 抛 Generic（非 Bpmn）→ 不被捕获 → Incident。
    let bpmn = bpmn_with_error_boundary(r#"errorRef="E_CREDIT""#);
    let (store, iid) = run(&bpmn, GenericErrDelegate).await;

    let snap = store.load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::Incident, "Generic 失败仍走 Incident");
    // 不应有令牌停在异常处理分支。
    assert!(!snap.tokens.iter().any(|t| t.node_bpmn_id == "handle"));
}

#[tokio::test]
async fn bpmn_error_no_matching_boundary_goes_incident() {
    // 边界声明 errorRef="E_CREDIT"，delegate 抛不同 code "E_OTHER" → 无匹配 → Incident。
    let bpmn = bpmn_with_error_boundary(r#"errorRef="E_CREDIT""#);
    let (store, iid) = run(&bpmn, BpmnErrDelegate { code: "E_OTHER" }).await;

    let snap = store.load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(
        tok.state,
        TokenState::Incident,
        "code 不匹配的 Bpmn 错误回退 Incident"
    );
    assert!(!snap.tokens.iter().any(|t| t.node_bpmn_id == "handle"));
}
