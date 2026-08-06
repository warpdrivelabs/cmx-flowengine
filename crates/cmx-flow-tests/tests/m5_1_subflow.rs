//! M5.1 端到端：callActivity 子流程（内存态，始终可跑）。
//!
//! 验证「主流程调用独立子流程并同步等待」：
//! - 子流程含 userTask：主令牌 WaitingSubflow 挂起，子跑完回主
//! - 变量映射：输入(主→子) + 输出(子→主)
//! - 子流程立即完成（无等待态）：主流程一口气走完
//! - 子含会签（M3）：子流程能用全部既有能力
//! - 嵌套：子流程里还能调孙流程

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, TokenState, Variables};
use serde_json::json;

/// 主流程：start → 部门经理审批(userTask) → [财务复核·子流程] → 出纳打款(userTask) → end
const MAIN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="expense_main" name="报销主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="mgr"/>
    <userTask id="mgr" name="部门经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="mgr" targetRef="call_fin"/>
    <callActivity id="call_fin" name="财务复核" calledElement="fin_review">
      <extensionElements>
        <flowable:in source="amount" target="reviewAmount"/>
        <flowable:out source="finResult" target="finResult"/>
      </extensionElements>
    </callActivity>
    <sequenceFlow id="s2" sourceRef="call_fin" targetRef="cashier"/>
    <userTask id="cashier" name="出纳打款" flowable:assignee="出纳"/>
    <sequenceFlow id="s3" sourceRef="cashier" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 子流程 fin_review：start → 财务复核(userTask) → end
const SUB_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="fin_review" name="财务复核子流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="财务复核" flowable:assignee="财务"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 子流程（立即完成版，无 userTask）：start → end
const SUB_INSTANT_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="fin_review" name="秒过子流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with(main: &str, sub: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine
        .deploy(compile(main).expect("主流程编译"))
        .expect("部署主流程");
    engine
        .deploy(compile(sub).expect("子流程编译"))
        .expect("部署子流程");
    (engine, store)
}

/// 取某实例当前唯一未办结任务的 id。
async fn open_task(store: &InMemoryStore, iid: &str) -> String {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks.iter().find(|t| !t.completed).unwrap().id.clone()
}

#[tokio::test]
async fn subflow_runs_and_returns_to_main() {
    let (engine, store) = engine_with(MAIN_BPMN, SUB_BPMN);
    let mut vars = Variables::new();
    vars.set("amount", json!(50000));
    let started = engine
        .start_process("expense_main", vars, Some("EXP-1".into()))
        .await
        .unwrap();
    let main_id = started.instance_id.clone();

    // 停在部门经理审批。
    assert_eq!(started.open_tasks.len(), 1);
    assert_eq!(started.open_tasks[0].node_bpmn_id, "mgr");

    // 经理办结 → 令牌到 callActivity → 启子实例，主令牌 WaitingSubflow。
    let mgr_task = started.open_tasks[0].id.clone();
    let after = engine
        .complete_task(&main_id, &mgr_task, Variables::new())
        .await
        .unwrap();
    // 主流程没有待办任务了（子流程接手），但也没完成。
    assert_eq!(after.state, InstanceState::Active);
    assert!(after.open_tasks.is_empty(), "主流程任务空，子流程接手");

    let main_snap = store.load_snapshot(&main_id).await.unwrap();
    assert!(
        main_snap
            .tokens
            .iter()
            .any(|t| t.state == TokenState::WaitingSubflow),
        "主令牌应 WaitingSubflow"
    );

    // 找到子实例。
    let children = store.find_child_instances(&main_id).await.unwrap();
    assert_eq!(children.len(), 1, "应有一个子实例");
    let sub_id = children[0].id.clone();
    assert_eq!(children[0].definition_key, "fin_review");
    // 输入变量映射：amount → reviewAmount。
    let sub_snap = store.load_snapshot(&sub_id).await.unwrap();
    assert_eq!(
        sub_snap.instance.variables.get("reviewAmount"),
        Some(&json!(50000))
    );
    // 子流程停在财务复核。
    assert_eq!(sub_snap.tasks.iter().filter(|t| !t.completed).count(), 1);

    // 办结子流程的财务复核任务（带输出变量 finResult）→ 子完成 → 唤醒主 → 主到出纳打款。
    let review_task = open_task(&store, &sub_id).await;
    let mut out = Variables::new();
    out.set("finResult", json!("approved"));
    engine
        .complete_task(&sub_id, &review_task, out)
        .await
        .unwrap();

    // 子实例完成。
    let sub_final = store.load_snapshot(&sub_id).await.unwrap();
    assert_eq!(sub_final.instance.state, InstanceState::Completed);

    // 主实例被唤醒，走到出纳打款，且回写了输出变量。
    let main_after = store.load_snapshot(&main_id).await.unwrap();
    assert_eq!(main_after.instance.state, InstanceState::Active);
    let open: Vec<&str> = main_after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["cashier"], "主流程应推进到出纳打款");
    assert_eq!(
        main_after.instance.variables.get("finResult"),
        Some(&json!("approved")),
        "输出变量回写主流程"
    );

    // 出纳办结 → 主流程完成。
    let cashier_task = open_task(&store, &main_id).await;
    let done = engine
        .complete_task(&main_id, &cashier_task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}

#[tokio::test]
async fn instant_subflow_flows_through() {
    // 子流程无等待态，秒过 → 主流程一口气从 callActivity 走到出纳打款。
    let (engine, store) = engine_with(MAIN_BPMN, SUB_INSTANT_BPMN);
    let started = engine
        .start_process("expense_main", Variables::new(), None)
        .await
        .unwrap();
    let main_id = started.instance_id.clone();
    let mgr_task = started.open_tasks[0].id.clone();
    let after = engine
        .complete_task(&main_id, &mgr_task, Variables::new())
        .await
        .unwrap();

    // 子流程立即完成，主流程直接到出纳打款。
    assert_eq!(after.state, InstanceState::Active);
    assert_eq!(after.open_tasks.len(), 1);
    assert_eq!(after.open_tasks[0].node_bpmn_id, "cashier");

    // 子实例存在且已完成。
    let children = store.find_child_instances(&main_id).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].state, InstanceState::Completed);
}

#[tokio::test]
async fn subflow_with_countersign() {
    // 子流程用 M3 会签：验证子流程能用全部既有能力。
    const MAIN2: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="m2" isExecutable="true">
        <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="c"/>
        <callActivity id="c" calledElement="cs_sub"/>
        <sequenceFlow id="f1" sourceRef="c" targetRef="e"/>
        <endEvent id="e"/>
      </process></definitions>"#;
    const SUB2: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                 xmlns:flowable="http://flowable.org/bpmn">
      <process id="cs_sub" isExecutable="true">
        <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="sign"/>
        <userTask id="sign" name="会签" flowable:assignee="${approver}">
          <multiInstanceLoopCharacteristics isSequential="false"
               flowable:collection="approvers" flowable:elementVariable="approver"/>
        </userTask>
        <sequenceFlow id="f1" sourceRef="sign" targetRef="e"/>
        <endEvent id="e"/>
      </process></definitions>"#;
    let (engine, store) = engine_with(MAIN2, SUB2);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["a", "b", "c"]));
    let started = engine.start_process("m2", vars, None).await.unwrap();
    let main_id = started.instance_id.clone();

    // 主流程起步即到 callActivity → 子实例展开 3 人会签。
    let children = store.find_child_instances(&main_id).await.unwrap();
    assert_eq!(children.len(), 1);
    let sub_id = children[0].id.clone();
    let sub = store.load_snapshot(&sub_id).await.unwrap();
    assert_eq!(
        sub.tasks.iter().filter(|t| !t.completed).count(),
        3,
        "子流程 3 人会签"
    );

    // 办结全部会签 → 子完成 → 主完成。
    for _ in 0..3 {
        let t = open_task(&store, &sub_id).await;
        engine
            .complete_task(&sub_id, &t, Variables::new())
            .await
            .unwrap();
    }
    let main_final = store.load_snapshot(&main_id).await.unwrap();
    assert_eq!(
        main_final.instance.state,
        InstanceState::Completed,
        "会签子流程完成后主流程完成"
    );
}

#[tokio::test]
async fn nested_subflows() {
    // 主 → 子 → 孙，三层嵌套，孙完成逐层唤醒回主。
    const L1: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="l1" isExecutable="true">
        <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="c"/>
        <callActivity id="c" calledElement="l2"/>
        <sequenceFlow id="f1" sourceRef="c" targetRef="e"/><endEvent id="e"/>
      </process></definitions>"#;
    const L2: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="l2" isExecutable="true">
        <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="c"/>
        <callActivity id="c" calledElement="l3"/>
        <sequenceFlow id="f1" sourceRef="c" targetRef="e"/><endEvent id="e"/>
      </process></definitions>"#;
    const L3: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                 xmlns:flowable="http://flowable.org/bpmn">
      <process id="l3" isExecutable="true">
        <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
        <userTask id="t" name="孙审批" flowable:assignee="u"/>
        <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
      </process></definitions>"#;
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    for xml in [L1, L2, L3] {
        engine.deploy(compile(xml).unwrap()).unwrap();
    }
    let started = engine
        .start_process("l1", Variables::new(), None)
        .await
        .unwrap();
    let l1_id = started.instance_id.clone();

    // 逐层下钻到孙流程。
    let l2 = store.find_child_instances(&l1_id).await.unwrap();
    assert_eq!(l2.len(), 1);
    let l3 = store.find_child_instances(&l2[0].id).await.unwrap();
    assert_eq!(l3.len(), 1);
    let l3_id = l3[0].id.clone();
    assert_eq!(
        store
            .load_snapshot(&l3_id)
            .await
            .unwrap()
            .tasks
            .iter()
            .filter(|t| !t.completed)
            .count(),
        1
    );

    // 办结孙审批 → 逐层唤醒 → l1 完成。
    let t = open_task(&store, &l3_id).await;
    engine
        .complete_task(&l3_id, &t, Variables::new())
        .await
        .unwrap();
    assert_eq!(
        store.load_snapshot(&l3_id).await.unwrap().instance.state,
        InstanceState::Completed
    );
    assert_eq!(
        store.load_snapshot(&l2[0].id).await.unwrap().instance.state,
        InstanceState::Completed
    );
    assert_eq!(
        store.load_snapshot(&l1_id).await.unwrap().instance.state,
        InstanceState::Completed,
        "顶层逐层唤醒后完成"
    );
}
