//! A3 事件子流程（错误触发，中断型）端到端测试。
//!
//! 验证：
//! - serviceTask 抛 BPMN 错误、无边界捕获 → 进程级错误事件子流程捕获 → 处理分支任务激活，主流程令牌终止
//! - catch-all 错误事件子流程兜任意 code
//! - 边界事件优先于事件子流程（都能匹配时走边界）
//! - Generic 失败不被事件子流程捕获 → Incident

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, DelegateError, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables,
};
use cmx_flow_model::{RuntimeStore, TokenState};

struct BpmnErr {
    code: &'static str,
}
#[async_trait::async_trait]
impl JavaDelegate for BpmnErr {
    async fn execute(&self, _c: &mut DelegateContext<'_>) -> Result<(), DelegateError> {
        Err(DelegateError::bpmn(self.code, "业务异常"))
    }
}

struct GenericErr;
#[async_trait::async_trait]
impl JavaDelegate for GenericErr {
    async fn execute(&self, _c: &mut DelegateContext<'_>) -> Result<(), DelegateError> {
        Err("普通失败".into())
    }
}

// start → svc(serviceTask) → approve(userTask) → end
// + 事件子流程(triggeredByEvent)：errorStartEvent(errorRef=E_RISK) → handle(userTask) → endH
fn bpmn(error_ref: &str, with_boundary: bool) -> String {
    let boundary = if with_boundary {
        r#"<boundaryEvent id="onErr" attachedToRef="svc"><errorEventDefinition errorRef="E_RISK"/></boundaryEvent>
           <sequenceFlow id="sb" sourceRef="onErr" targetRef="bhandle"/>
           <userTask id="bhandle" name="边界处理" flowable:assignee="bops"/>
           <sequenceFlow id="sb2" sourceRef="bhandle" targetRef="endB"/>
           <endEvent id="endB"/>"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a3" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="风控调用" flowable:class="extCall"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="approve"/>
    <userTask id="approve" name="正常审批" flowable:assignee="user1"/>
    <sequenceFlow id="s2" sourceRef="approve" targetRef="end"/>
    <endEvent id="end"/>
    {boundary}
    <subProcess id="evsub" triggeredByEvent="true">
      <startEvent id="estart"><errorEventDefinition {error_ref}/></startEvent>
      <sequenceFlow id="es0" sourceRef="estart" targetRef="handle"/>
      <userTask id="handle" name="异常处理" flowable:assignee="ops"/>
      <sequenceFlow id="es1" sourceRef="handle" targetRef="endH"/>
      <endEvent id="endH"/>
    </subProcess>
  </process>
</definitions>"#
    )
}

async fn run(xml: &str, del: impl JavaDelegate + 'static) -> (InMemoryStore, String) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(xml).expect("编译")).expect("部署");
    engine.register_delegate("extCall", del);
    let started = engine
        .start_process("a3", Variables::new(), None)
        .await
        .expect("启动");
    (store, started.instance_id)
}

#[tokio::test]
async fn error_routes_to_event_subprocess_and_interrupts_main() {
    let xml = bpmn(r#"errorRef="E_RISK""#, false);
    let (store, iid) = run(&xml, BpmnErr { code: "E_RISK" }).await;

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Active);
    // 令牌应停在事件子流程处理任务，主流程 svc/approve 令牌已终止。
    let waiting: Vec<_> = snap
        .tokens
        .iter()
        .filter(|t| t.state == TokenState::Waiting)
        .collect();
    assert_eq!(waiting.len(), 1, "只剩事件子流程处理分支一个等待令牌");
    assert_eq!(waiting[0].node_bpmn_id, "handle");
    // 无 svc/approve 上的活动/等待令牌。
    assert!(!snap.tokens.iter().any(|t| t.node_bpmn_id == "approve" && t.state != TokenState::Ended));
    // 开放任务只有异常处理。
    let open: Vec<_> = snap.tasks.iter().filter(|t| !t.completed).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].node_bpmn_id, "handle");
    assert_eq!(open[0].assignee.as_deref(), Some("ops"));
    assert!(!snap.tokens.iter().any(|t| t.state == TokenState::Incident));
}

#[tokio::test]
async fn catch_all_event_subprocess_catches_any_code() {
    // errorStartEvent 无 errorRef → catch-all。
    let xml = bpmn("", false);
    let (store, iid) = run(&xml, BpmnErr { code: "WHATEVER" }).await;
    let snap = store.load_snapshot(&iid).await.unwrap();
    let waiting: Vec<_> = snap.tokens.iter().filter(|t| t.state == TokenState::Waiting).collect();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].node_bpmn_id, "handle", "catch-all 应捕获任意 code");
}

#[tokio::test]
async fn boundary_takes_priority_over_event_subprocess() {
    // 同时有边界(E_RISK)与事件子流程(E_RISK)，抛 E_RISK → 走边界(不中断主流程整体，走边界分支)。
    let xml = bpmn(r#"errorRef="E_RISK""#, true);
    let (store, iid) = run(&xml, BpmnErr { code: "E_RISK" }).await;
    let snap = store.load_snapshot(&iid).await.unwrap();
    let open: Vec<_> = snap.tasks.iter().filter(|t| !t.completed).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].node_bpmn_id, "bhandle", "应走边界处理而非事件子流程");
    assert_eq!(open[0].assignee.as_deref(), Some("bops"));
}

#[tokio::test]
async fn generic_error_not_caught_by_event_subprocess() {
    let xml = bpmn(r#"errorRef="E_RISK""#, false);
    let (store, iid) = run(&xml, GenericErr).await;
    let snap = store.load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::Incident, "Generic 失败仍 Incident");
    assert!(!snap.tokens.iter().any(|t| t.node_bpmn_id == "handle"));
}

#[tokio::test]
async fn code_mismatch_falls_through_to_incident() {
    // 事件子流程只捕 E_RISK，抛 E_OTHER → 无匹配 → Incident。
    let xml = bpmn(r#"errorRef="E_RISK""#, false);
    let (store, iid) = run(&xml, BpmnErr { code: "E_OTHER" }).await;
    let snap = store.load_snapshot(&iid).await.unwrap();
    let tok = snap.tokens.iter().find(|t| t.node_bpmn_id == "svc").unwrap();
    assert_eq!(tok.state, TokenState::Incident);
    assert!(!snap.tokens.iter().any(|t| t.node_bpmn_id == "handle"));
}
