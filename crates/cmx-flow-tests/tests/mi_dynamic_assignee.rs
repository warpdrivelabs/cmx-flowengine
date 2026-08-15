//! ② 端到端：动态多实例逐元素派人（内存态，始终可跑）。
//!
//! 验证三种写法 + 边界：
//!   A 元素插值   `assignee="${product.ownerUser}"` → 每个产品直派其负责人；
//!   B 候选表达式 `candidates="role(${product.ownerRole})"` → 逐元素解析角色（1 直派 / ≥2 候选池）；
//!   C 集合即人   `assignee="${approver}"`（approvers=[u1,u2]）→ 每人一个待办（修复历史失效写法）；
//!   顺序或签逐元素解析；空集合跳过；静态字面量 assignee 向后兼容；求值 null → 无人不 panic。

use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    AssigneeResolver, CandidateRef, Engine, InMemoryStore, InstanceState, ResolveResult, RuntimeStore,
    Variables,
};
use cmx_flow_model::CandidateKind;
use serde_json::json;

/// 测试解析器：Role code → 用户集（r_a 单人、r_multi 多人）；User → 自身。
struct TestResolver;

#[async_trait]
impl AssigneeResolver for TestResolver {
    async fn resolve(&self, c: &CandidateRef) -> ResolveResult<Vec<String>> {
        Ok(match c.kind {
            CandidateKind::User => vec![c.value.clone()],
            CandidateKind::Role => match c.value.as_str() {
                "r_a" => vec!["u_a".into()],
                "r_b" => vec!["u_b".into()],
                "r_multi" => vec!["ux".into(), "uy".into()],
                _ => vec![],
            },
            _ => vec![],
        })
    }
}

fn engine(xml: &str, with_resolver: bool) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut e = Engine::new(store.clone());
    e.deploy(compile(xml).expect("编译")).expect("部署");
    if with_resolver {
        e.set_resolver(Arc::new(TestResolver));
    }
    (e, store)
}

async fn tasks_at<'a>(store: &InMemoryStore, iid: &str, node: &str) -> Vec<(String, Option<String>)> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .filter(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| (t.id.clone(), t.assignee.clone()))
        .collect()
}

/// A：元素插值直派。products=[{ownerUser}] → 每产品一任务派其负责人。
const A_ELEMENT_INTERP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="a_flow" name="产品审核" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="产品审核" flowable:assignee="${product.ownerUser}">
      <multiInstanceLoopCharacteristics isSequential="false"
           flowable:collection="products" flowable:elementVariable="product"/>
    </userTask>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn element_interpolation_assigns_per_product() {
    let (e, store) = engine(A_ELEMENT_INTERP, false);
    let mut vars = Variables::new();
    vars.set("products", json!([
        {"sku":"A","ownerUser":"u_a"},
        {"sku":"B","ownerUser":"u_b"},
        {"sku":"C","ownerUser":"u_c"},
    ]));
    let iid = e.start_process("a_flow", vars, None).await.unwrap().instance_id;
    let mut got: Vec<String> = tasks_at(&store, &iid, "review")
        .await
        .into_iter()
        .map(|(_, a)| a.unwrap_or_default())
        .collect();
    got.sort();
    assert_eq!(got, vec!["u_a", "u_b", "u_c"], "3 个产品各派其负责人");
    // element_value 随任务携带对应产品。
    let snap = store.load_snapshot(&iid).await.unwrap();
    let a_task = snap.tasks.iter().find(|t| t.assignee.as_deref() == Some("u_a")).unwrap();
    assert_eq!(a_task.element_value.as_ref().unwrap()["sku"], json!("A"));
}

/// C：集合即人（历史失效写法修复）。approvers=[u1,u2] → 两待办。
const C_COLLECTION_PEOPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="c_flow" name="会签" isExecutable="true">
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

#[tokio::test]
async fn collection_is_people_assigns_each() {
    let (e, store) = engine(C_COLLECTION_PEOPLE, false);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["u1", "u2"]));
    let iid = e.start_process("c_flow", vars, None).await.unwrap().instance_id;
    let mut got: Vec<String> = tasks_at(&store, &iid, "sign")
        .await
        .into_iter()
        .map(|(_, a)| a.unwrap_or_default())
        .collect();
    got.sort();
    assert_eq!(got, vec!["u1", "u2"], "每人一个待办（assignee=${{approver}} 真插值）");
}

/// B：候选表达式插值。candidates=role(${product.ownerRole})。
const B_CANDIDATE_INTERP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="b_flow" name="按角色审" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="产品审核" cmx:candidates="role(${product.ownerRole})">
      <multiInstanceLoopCharacteristics isSequential="false"
           flowable:collection="products" flowable:elementVariable="product"/>
    </userTask>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn candidate_expr_single_user_direct_assign() {
    let (e, store) = engine(B_CANDIDATE_INTERP, true);
    let mut vars = Variables::new();
    // r_a→单人 u_a；r_b→单人 u_b。
    vars.set("products", json!([
        {"sku":"A","ownerRole":"r_a"},
        {"sku":"B","ownerRole":"r_b"},
    ]));
    let iid = e.start_process("b_flow", vars, None).await.unwrap().instance_id;
    let mut got: Vec<String> = tasks_at(&store, &iid, "review")
        .await
        .into_iter()
        .map(|(_, a)| a.unwrap_or_default())
        .collect();
    got.sort();
    assert_eq!(got, vec!["u_a", "u_b"], "单人角色逐元素直派");
}

#[tokio::test]
async fn candidate_expr_multi_user_pool_per_element() {
    let (e, store) = engine(B_CANDIDATE_INTERP, true);
    let mut vars = Variables::new();
    // 两个产品都用 r_multi（→ ux,uy 两人）→ 每个子任务各自落候选池。
    vars.set("products", json!([
        {"sku":"A","ownerRole":"r_multi"},
        {"sku":"B","ownerRole":"r_multi"},
    ]));
    let iid = e.start_process("b_flow", vars, None).await.unwrap().instance_id;
    let snap = store.load_snapshot(&iid).await.unwrap();
    let review_tasks: Vec<_> = snap.tasks.iter().filter(|t| t.node_bpmn_id == "review" && !t.completed).collect();
    assert_eq!(review_tasks.len(), 2, "两个产品两个子任务");
    for t in &review_tasks {
        assert!(t.assignee.is_none(), "多人候选 → 不直派");
        let pool: Vec<_> = snap.candidates.iter().filter(|c| c.task_id == t.id).collect();
        let mut users: Vec<&str> = pool.iter().map(|c| c.resolved_user_id.as_str()).collect();
        users.sort();
        assert_eq!(users, vec!["ux", "uy"], "每个子任务各自候选池 [ux,uy]");
    }
}

/// 顺序或签：逐元素解析。
const SEQ_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="seq_flow" name="或签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="sign"/>
    <userTask id="sign" name="或签" flowable:assignee="${approver}">
      <multiInstanceLoopCharacteristics isSequential="true"
           flowable:collection="approvers" flowable:elementVariable="approver"/>
    </userTask>
    <sequenceFlow id="s1" sourceRef="sign" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn sequential_resolves_each_element_in_turn() {
    let (e, store) = engine(SEQ_FLOW, false);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["u1", "u2"]));
    let iid = e.start_process("seq_flow", vars, None).await.unwrap().instance_id;
    // 顺序：先只展开第一个 → u1。
    let first = tasks_at(&store, &iid, "sign").await;
    assert_eq!(first.len(), 1, "顺序模式一次只展一个");
    assert_eq!(first[0].1.as_deref(), Some("u1"), "首个 = u1");
    // 办结 u1 → 展开第二个 u2（逐元素解析）。
    e.complete_task(&iid, &first[0].0, Variables::new()).await.unwrap();
    let second = tasks_at(&store, &iid, "sign").await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].1.as_deref(), Some("u2"), "下一个 = u2（元素独立解析）");
}

/// 静态字面量 assignee（无 ${}）→ 向后兼容（每子任务都是同一字面量）。
const STATIC_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="static_flow" name="静态会签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="sign"/>
    <userTask id="sign" name="会签" flowable:assignee="mgr">
      <multiInstanceLoopCharacteristics isSequential="false"
           flowable:collection="approvers" flowable:elementVariable="approver"/>
    </userTask>
    <sequenceFlow id="s1" sourceRef="sign" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn static_literal_assignee_backward_compatible() {
    let (e, store) = engine(STATIC_FLOW, false);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["a", "b"]));
    let iid = e.start_process("static_flow", vars, None).await.unwrap().instance_id;
    let got: Vec<String> = tasks_at(&store, &iid, "sign")
        .await
        .into_iter()
        .map(|(_, a)| a.unwrap_or_default())
        .collect();
    assert_eq!(got, vec!["mgr", "mgr"], "静态字面量原样，两子任务都 mgr");
}

#[tokio::test]
async fn empty_collection_skips_node() {
    let (e, store) = engine(C_COLLECTION_PEOPLE, false);
    let mut vars = Variables::new();
    vars.set("approvers", json!([]));
    let iid = e.start_process("c_flow", vars, None).await.unwrap().instance_id;
    // 空集合 → 跳过 MI 节点 → 直接完成。
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.state, InstanceState::Completed, "空集合跳过节点直达完成");
    assert!(tasks_at(&store, &iid, "sign").await.is_empty());
}

#[tokio::test]
async fn null_assignee_expr_is_lenient() {
    // ${product.missing} 求值 null → 无 assignee，不 panic。
    let (e, store) = engine(A_ELEMENT_INTERP, false);
    let mut vars = Variables::new();
    vars.set("products", json!([{"sku":"A"}])); // 无 ownerUser
    let iid = e.start_process("a_flow", vars, None).await.unwrap().instance_id;
    let got = tasks_at(&store, &iid, "review").await;
    assert_eq!(got.len(), 1, "仍建任务");
    assert!(got[0].1.is_none(), "求值 null → 无 assignee（宽容）");
}
