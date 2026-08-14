//! A1 端到端：包容网关（inclusiveGateway，OR fork/join，内存态）。
//!
//! 验证：包容 fork 按条件择「若干」出边；join 等所有实际到达的分支到齐再放行。
//!   流程：start → forkIncl →(cond A: amount>1000)→ taskA
//!                          →(cond B: urgent==true)→ taskB
//!                          →(default)→ taskC
//!                   taskA/B/C → joinIncl → final(userTask) → end
//!   用例1：amount=5000, urgent=true → A、B 都命中（C 不走）→ 两分支 → join 等两个 → final。
//!   用例2：amount=10, urgent=false → 都不命中 → 走 default C → 一分支 → join → final。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use serde_json::json;

const INCL_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="incl_flow" name="包容网关" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="forkIncl"/>
    <inclusiveGateway id="forkIncl" default="fc"/>
    <sequenceFlow id="fa" sourceRef="forkIncl" targetRef="taskA">
      <conditionExpression>${amount &gt; 1000}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="fb" sourceRef="forkIncl" targetRef="taskB">
      <conditionExpression>${urgent == true}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="fc" sourceRef="forkIncl" targetRef="taskC"/>
    <userTask id="taskA" name="财务审批" flowable:assignee="fin"/>
    <userTask id="taskB" name="加急审批" flowable:assignee="urg"/>
    <userTask id="taskC" name="常规审批" flowable:assignee="reg"/>
    <sequenceFlow id="ja" sourceRef="taskA" targetRef="joinIncl"/>
    <sequenceFlow id="jb" sourceRef="taskB" targetRef="joinIncl"/>
    <sequenceFlow id="jc" sourceRef="taskC" targetRef="joinIncl"/>
    <inclusiveGateway id="joinIncl"/>
    <sequenceFlow id="jf" sourceRef="joinIncl" targetRef="final"/>
    <userTask id="final" name="终审" flowable:assignee="boss"/>
    <sequenceFlow id="fe" sourceRef="final" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(INCL_FLOW).expect("编译")).expect("部署");
    (engine, store)
}

fn open_nodes(snap: &cmx_flow_engine::InstanceSnapshot) -> Vec<String> {
    let mut v: Vec<String> = snap
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.clone())
        .collect();
    v.sort();
    v
}

async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<String> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| t.id.clone())
}

#[test]
fn compiles_inclusive_gateway() {
    let def = compile(INCL_FLOW).expect("编译应成功");
    use cmx_flow_model::NodeKind;
    let fork = def.nodes.iter().find(|n| n.bpmn_id == "forkIncl").unwrap();
    assert!(matches!(fork.kind, NodeKind::InclusiveGateway), "forkIncl 应为包容网关");
}

#[tokio::test]
async fn inclusive_fork_multiple_branches_and_join() {
    let (engine, store) = engine_with();
    // amount=5000(>1000 ✓) + urgent=true ✓ → A、B 命中，C(default) 不走。
    let mut vars = Variables::new();
    vars.set("amount", json!(5000));
    vars.set("urgent", json!(true));
    let started = engine
        .start_process("incl_flow", vars, Some("INCL-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // fork 到 A、B 两分支（不含 C）。
    assert_eq!(open_nodes(&snap), vec!["taskA", "taskB"], "应 fork 到 A、B 两分支");

    // 办结 A → join 不应放行（B 还没到）。
    let ta = open_task_at(&store, &iid, "taskA").await.unwrap();
    engine.complete_task(&iid, &ta, Variables::new()).await.unwrap();
    assert!(
        open_task_at(&store, &iid, "final").await.is_none(),
        "只 A 到 join，B 未到，不应放行到 final"
    );

    // 办结 B → 两分支到齐 → join 放行 → final 待办出现。
    let tb = open_task_at(&store, &iid, "taskB").await.unwrap();
    engine.complete_task(&iid, &tb, Variables::new()).await.unwrap();
    assert!(
        open_task_at(&store, &iid, "final").await.is_some(),
        "两分支到齐后应放行到 final"
    );
}

#[tokio::test]
async fn inclusive_fork_default_when_no_condition_matches() {
    let (engine, store) = engine_with();
    // amount=10(不>1000) + urgent=false → A、B 都不命中 → 走 default C。
    let mut vars = Variables::new();
    vars.set("amount", json!(10));
    vars.set("urgent", json!(false));
    let started = engine
        .start_process("incl_flow", vars, None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(open_nodes(&snap), vec!["taskC"], "全不命中应走 default C");

    // 办结 C → 单分支 → join 立即放行 → final。
    let tc = open_task_at(&store, &iid, "taskC").await.unwrap();
    engine.complete_task(&iid, &tc, Variables::new()).await.unwrap();
    assert!(
        open_task_at(&store, &iid, "final").await.is_some(),
        "单分支 join 应放行到 final"
    );
}
