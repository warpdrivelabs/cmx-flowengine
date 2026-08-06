//! M5.2 端到端：子流程按组织路由（内存 + 假 router，始终可跑）。
//!
//! 验证「同一主流程按实例组织跑不同子流程」——核心诉求：
//! - callActivity 用逻辑 key（cmx:calledKey），运行期由 SubflowRouter 按 org 解析成具体子流程
//! - 组织 A → 子流程 X，组织 B → 子流程 Y，同一份主流程定义
//! - 无路由器却用逻辑 key → 报错
//! - 路由无解 → 报错

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    Engine, InMemoryStore, InstanceState, RouteError, RouteResult, RuntimeStore, SubflowRouter,
    Variables,
};

/// 假路由器：按 (called_key, org) → 具体子流程 key 的固定映射；缺省回退 (called_key, "*")。
#[derive(Default)]
struct FakeRouter {
    map: HashMap<String, String>, // "key@org" 或 "key@*" → target
}
impl FakeRouter {
    fn bind(mut self, key: &str, org: &str, target: &str) -> Self {
        self.map.insert(format!("{key}@{org}"), target.into());
        self
    }
    fn default_bind(mut self, key: &str, target: &str) -> Self {
        self.map.insert(format!("{key}@*"), target.into());
        self
    }
}
#[async_trait]
impl SubflowRouter for FakeRouter {
    async fn resolve(&self, called_key: &str, org_id: Option<&str>) -> RouteResult<String> {
        if let Some(org) = org_id
            && let Some(t) = self.map.get(&format!("{called_key}@{org}"))
        {
            return Ok(t.clone());
        }
        if let Some(t) = self.map.get(&format!("{called_key}@*")) {
            return Ok(t.clone());
        }
        Err(RouteError::NoBinding {
            called_key: called_key.into(),
            org: org_id.map(|s| s.into()),
        })
    }
}

/// 主流程：start → callActivity(逻辑 key fin_review) → end
const MAIN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:cmx="http://cmx/flow">
  <process id="routed_main" name="路由主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="call"/>
    <callActivity id="call" name="财务复核" cmx:calledKey="fin_review"/>
    <sequenceFlow id="s1" sourceRef="call" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 子流程 A（总部三级，这里简化为一个 userTask 标记来源）
const SUB_HQ: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="fin_review_hq" name="总部财务复核" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="总部三级复核" flowable:assignee="hq"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

/// 子流程 B（分公司单签）
const SUB_BRANCH: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="fin_review_branch" name="分公司财务复核" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="分公司经理单签" flowable:assignee="branch"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

fn engine_with_router(router: FakeRouter) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    for xml in [MAIN_BPMN, SUB_HQ, SUB_BRANCH] {
        engine.deploy(compile(xml).unwrap()).unwrap();
    }
    engine.set_subflow_router(Arc::new(router));
    (engine, store)
}

/// 取某主实例的（唯一）子实例定义 key + 首个待办任务名。
async fn child_of(store: &InMemoryStore, main_id: &str) -> (String, String) {
    let children = store.find_child_instances(main_id).await.unwrap();
    assert_eq!(children.len(), 1, "应有一个子实例");
    let sub = store.load_snapshot(&children[0].id).await.unwrap();
    let task = sub
        .tasks
        .iter()
        .find(|t| !t.completed)
        .map(|t| t.name.clone().unwrap_or_default())
        .unwrap_or_default();
    (sub.instance.definition_key.clone(), task)
}

#[tokio::test]
async fn same_main_different_org_runs_different_subflow() {
    // 总部 org=hq → fin_review_hq；分公司 org=bj → fin_review_branch。
    let router = FakeRouter::default()
        .bind("fin_review", "hq", "fin_review_hq")
        .bind("fin_review", "bj", "fin_review_branch");
    let (engine, store) = engine_with_router(router);

    // 总部发起。
    let hq = engine
        .start_process_org(
            "routed_main",
            Variables::new(),
            Some("HQ-1".into()),
            Some("hq".into()),
        )
        .await
        .unwrap();
    let (hq_sub, hq_task) = child_of(&store, &hq.instance_id).await;
    assert_eq!(hq_sub, "fin_review_hq", "总部应路由到总部子流程");
    assert_eq!(hq_task, "总部三级复核");

    // 分公司发起——同一份主流程定义 routed_main，不同子流程。
    let bj = engine
        .start_process_org(
            "routed_main",
            Variables::new(),
            Some("BJ-1".into()),
            Some("bj".into()),
        )
        .await
        .unwrap();
    let (bj_sub, bj_task) = child_of(&store, &bj.instance_id).await;
    assert_eq!(bj_sub, "fin_review_branch", "分公司应路由到分公司子流程");
    assert_eq!(bj_task, "分公司经理单签");
}

#[tokio::test]
async fn falls_back_to_default_binding() {
    // 未给 org 精确绑定时，回退默认绑定。
    let router = FakeRouter::default().default_bind("fin_review", "fin_review_hq");
    let (engine, store) = engine_with_router(router);
    // org=未知，走默认。
    let inst = engine
        .start_process_org(
            "routed_main",
            Variables::new(),
            None,
            Some("unknown_org".into()),
        )
        .await
        .unwrap();
    let (sub, _) = child_of(&store, &inst.instance_id).await;
    assert_eq!(sub, "fin_review_hq", "无精确绑定应回退默认");
}

#[tokio::test]
async fn routed_subflow_completes_and_returns_to_main() {
    // 完整链路：路由 → 子跑完 → 回主 → 主完成。
    let router = FakeRouter::default().bind("fin_review", "bj", "fin_review_branch");
    let (engine, store) = engine_with_router(router);
    let started = engine
        .start_process_org("routed_main", Variables::new(), None, Some("bj".into()))
        .await
        .unwrap();
    let main_id = started.instance_id.clone();

    let children = store.find_child_instances(&main_id).await.unwrap();
    let sub_id = children[0].id.clone();
    let task = store.load_snapshot(&sub_id).await.unwrap().tasks[0]
        .id
        .clone();
    engine
        .complete_task(&sub_id, &task, Variables::new())
        .await
        .unwrap();

    assert_eq!(
        store.load_snapshot(&sub_id).await.unwrap().instance.state,
        InstanceState::Completed
    );
    assert_eq!(
        store.load_snapshot(&main_id).await.unwrap().instance.state,
        InstanceState::Completed,
        "子完成后主流程完成"
    );
}

#[tokio::test]
async fn logical_key_without_router_errors() {
    // 用逻辑 key 但未注入路由器 → 报错。
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    for xml in [MAIN_BPMN, SUB_HQ, SUB_BRANCH] {
        engine.deploy(compile(xml).unwrap()).unwrap();
    }
    // 不 set_subflow_router
    let r = engine
        .start_process_org("routed_main", Variables::new(), None, Some("hq".into()))
        .await;
    assert!(r.is_err(), "逻辑 key 无路由器应报错");
}

#[tokio::test]
async fn no_binding_errors() {
    // 路由器在，但该 org 无任何绑定（含默认）→ 报错。
    let router = FakeRouter::default().bind("fin_review", "hq", "fin_review_hq");
    let (engine, _store) = engine_with_router(router);
    // org=sh 无绑定，也无默认。
    let r = engine
        .start_process_org("routed_main", Variables::new(), None, Some("sh".into()))
        .await;
    assert!(r.is_err(), "无绑定应报错");
}
