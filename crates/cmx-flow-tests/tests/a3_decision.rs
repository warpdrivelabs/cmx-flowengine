//! A3 端到端：轻量决策表 + businessRuleTask（内存态）。
//!
//! 验证：start → rule(决策表:金额→审批级别) → gateway(按级别分支) → 相应审批 → end。
//!   大额 → 决策表输出 approvalLevel=3 → 走高级审批分支。
//!   小额 → approvalLevel=1 → 走普通分支。

use std::collections::BTreeMap;

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use cmx_flow_model::{DecisionRule, DecisionTable, HitPolicy};
use serde_json::json;

/// start → rule(businessRuleTask) → gw(exclusive) →(level>=3) high →(default) normal → end。
const RULE_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="rule_flow" name="决策审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="rule"/>
    <businessRuleTask id="rule" name="定审批级别" flowable:decisionRef="approval_matrix"/>
    <sequenceFlow id="s1" sourceRef="rule" targetRef="gw"/>
    <exclusiveGateway id="gw" default="toNormal"/>
    <sequenceFlow id="toHigh" sourceRef="gw" targetRef="high">
      <conditionExpression>${approvalLevel >= 3}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="toNormal" sourceRef="gw" targetRef="normal"/>
    <userTask id="high" name="董事会审批" flowable:assignee="board"/>
    <userTask id="normal" name="经理审批" flowable:assignee="mgr"/>
    <sequenceFlow id="h1" sourceRef="high" targetRef="done"/>
    <sequenceFlow id="n1" sourceRef="normal" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn approval_matrix() -> DecisionTable {
    let rule = |cond: &str, level: i64| DecisionRule {
        conditions: vec![cond.to_string()],
        outputs: {
            let mut m = BTreeMap::new();
            m.insert("approvalLevel".to_string(), json!(level));
            m
        },
    };
    DecisionTable {
        key: "approval_matrix".into(),
        inputs: vec!["amount".into()],
        outputs: vec!["approvalLevel".into()],
        hit_policy: HitPolicy::First,
        rules: vec![rule("amount > 100000", 3), rule("-", 1)],
    }
}

fn engine_with() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(RULE_FLOW).expect("编译")).expect("部署");
    engine.register_decision(approval_matrix());
    (engine, store)
}

fn open_node(snap: &cmx_flow_engine::InstanceSnapshot) -> Option<String> {
    snap.tasks
        .iter()
        .find(|t| !t.completed)
        .map(|t| t.node_bpmn_id.clone())
}

#[test]
fn compiles_business_rule_task() {
    let def = compile(RULE_FLOW).expect("编译应成功");
    use cmx_flow_model::NodeKind;
    let n = def.nodes.iter().find(|n| n.bpmn_id == "rule").unwrap();
    match &n.kind {
        NodeKind::BusinessRuleTask(br) => assert_eq!(br.decision_key, "approval_matrix"),
        _ => panic!("rule 应为业务规则任务"),
    }
}

#[tokio::test]
async fn large_amount_routes_to_high_via_decision() {
    let (engine, store) = engine_with();
    let mut vars = Variables::new();
    vars.set("amount", json!(500000));
    let started = engine
        .start_process("rule_flow", vars, Some("R-1".into()))
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // 决策表写了 approvalLevel=3 → 走 high 分支。
    assert_eq!(snap.instance.variables.get("approvalLevel"), Some(&json!(3)));
    assert_eq!(open_node(&snap).as_deref(), Some("high"), "大额应到董事会审批");
}

#[tokio::test]
async fn small_amount_routes_to_normal_via_default() {
    let (engine, store) = engine_with();
    let mut vars = Variables::new();
    vars.set("amount", json!(500));
    let started = engine
        .start_process("rule_flow", vars, None)
        .await
        .unwrap();
    let iid = started.instance_id.clone();

    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.variables.get("approvalLevel"), Some(&json!(1)));
    assert_eq!(open_node(&snap).as_deref(), Some("normal"), "小额应到经理审批");
}

#[tokio::test]
async fn unregistered_decision_is_tolerant() {
    // 决策表未注册 → 不写变量、不硬失败，走 default 分支（宽容）。
    let store = InMemoryStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(RULE_FLOW).expect("编译")).expect("部署");
    // 不注册决策表。
    let started = engine
        .start_process("rule_flow", Variables::new(), None)
        .await
        .unwrap();
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // approvalLevel 未写 → gateway 条件 false → 走 default normal。
    assert_eq!(open_node(&snap).as_deref(), Some("normal"), "未注册决策表宽容走 default");
}
