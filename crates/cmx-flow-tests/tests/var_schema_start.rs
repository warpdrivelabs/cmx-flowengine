//! ⑤ 端到端：变量声明的发起态默认值物化 + 软校验（内存态，始终可跑）。
//!
//! 验证：
//!   1) 声明了 default 的变量，发起时缺省 → 自动注入；已传则不覆盖；
//!   2) strict 策略：必填缺失 → 拒绝发起；
//!   3) lenient（默认）：违规仅 warn，照常发起；
//!   4) off：完全不校验；
//!   5) 未声明 var_schema → 零回归（任意变量放行）。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use serde_json::json;

fn flow(schema_json: &str, validation_attr: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="vs_flow" name="变量声明流程" isExecutable="true" {validation_attr}>
    <extensionElements><cmx:varSchema>{schema_json}</cmx:varSchema></extensionElements>
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="t"/>
    <userTask id="t" name="办理" flowable:assignee="u"/>
    <sequenceFlow id="s1" sourceRef="t" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#
    )
}

fn engine(xml: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut e = Engine::new(store.clone());
    e.deploy(compile(xml).expect("编译")).expect("部署");
    (e, store)
}

const SCHEMA: &str = r#"[
  {"name":"amount","type":"NUMBER","label":"金额","required":true},
  {"name":"region","type":"ENUM","enumOptions":["north","south"],"default":"north"},
  {"name":"remark","type":"STRING","default":"n/a"}
]"#;

#[tokio::test]
async fn materializes_defaults_on_start() {
    let (e, store) = engine(&flow(SCHEMA, "")); // lenient 默认
    let mut vars = Variables::new();
    vars.set("amount", json!(1000));
    vars.set("region", json!("south")); // 已传，不应被默认覆盖
    let iid = e.start_process("vs_flow", vars, None).await.unwrap().instance_id;
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.variables.get("region"), Some(&json!("south")), "已传值保留");
    assert_eq!(snap.instance.variables.get("remark"), Some(&json!("n/a")), "缺省注入默认");
    assert_eq!(snap.instance.variables.get("amount"), Some(&json!(1000)));
}

#[tokio::test]
async fn strict_rejects_missing_required() {
    let (e, _store) = engine(&flow(SCHEMA, r#"cmx:varValidation="strict""#));
    // amount 必填缺失 → strict 拒绝。
    let r = e.start_process("vs_flow", Variables::new(), None).await;
    assert!(r.is_err(), "strict 下必填缺失应拒绝发起");
}

#[tokio::test]
async fn lenient_starts_despite_violation() {
    let (e, store) = engine(&flow(SCHEMA, "")); // 默认 lenient
    // amount 缺失但 lenient → 照常发起。
    let started = e.start_process("vs_flow", Variables::new(), None).await;
    assert!(started.is_ok(), "lenient 下有违规仍发起");
    let iid = started.unwrap().instance_id;
    // 默认值仍注入。
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.variables.get("region"), Some(&json!("north")));
}

#[tokio::test]
async fn off_skips_validation() {
    let (e, _store) = engine(&flow(SCHEMA, r#"cmx:varValidation="off""#));
    // off：必填缺失也不校验 → 发起成功。
    assert!(e.start_process("vs_flow", Variables::new(), None).await.is_ok());
}

#[tokio::test]
async fn strict_passes_when_valid() {
    let (e, _store) = engine(&flow(SCHEMA, r#"cmx:varValidation="strict""#));
    let mut vars = Variables::new();
    vars.set("amount", json!(500));
    vars.set("region", json!("north"));
    assert!(e.start_process("vs_flow", vars, None).await.is_ok(), "满足声明应通过");
}

#[tokio::test]
async fn no_schema_zero_regression() {
    // 无 varSchema 的流程：任意变量放行（旧行为）。
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="plain" name="无声明" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="t"/>
    <userTask id="t" name="办理" flowable:assignee="u"/>
    <sequenceFlow id="s1" sourceRef="t" targetRef="done"/><endEvent id="done"/>
  </process>
</definitions>"#;
    let (e, _store) = engine(xml);
    let mut vars = Variables::new();
    vars.set("anything", json!({"free": "form"}));
    assert!(e.start_process("plain", vars, None).await.is_ok());
}
