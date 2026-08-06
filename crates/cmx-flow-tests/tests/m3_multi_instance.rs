//! M3 端到端：多实例会签 / 或签（内存态，始终可跑）。
//!
//! 覆盖「人本审批流」的标志性能力——多实例（multiInstance）：
//! - 并行会签：一个 userTask 按集合展开成 N 个并行待办，全部办结即通过
//! - completionCondition 提前收口：办结过半即通过，剩余任务作废
//! - 顺序或签：逐个展开办理，任一驳回即（按条件）终止
//! - 空集合边界：直接跳过 MI 节点

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, Variables};
use cmx_flow_model::RuntimeStore;
use serde_json::json;

/// 并行会签（全体通过才收口）：发起 → [会签 N 人] → 结束
const PARALLEL_ALL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="countersign_all" name="全员会签" isExecutable="true">
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

/// 并行会签 + 过半即通过。
const PARALLEL_MAJORITY_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="countersign_majority" name="过半会签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="sign"/>
    <userTask id="sign" name="会签" flowable:assignee="${approver}">
      <multiInstanceLoopCharacteristics isSequential="false"
           flowable:collection="approvers" flowable:elementVariable="approver">
        <completionCondition>${nrOfCompletedInstances/nrOfInstances &gt;= 0.5}</completionCondition>
      </multiInstanceLoopCharacteristics>
    </userTask>
    <sequenceFlow id="s1" sourceRef="sign" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 顺序或签 + 任一驳回即终止会签。
const SEQUENTIAL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="or_sign" name="顺序或签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="chain"/>
    <userTask id="chain" name="逐级审批">
      <multiInstanceLoopCharacteristics isSequential="true"
           flowable:collection="approvers">
        <completionCondition>${rejected == true}</completionCondition>
      </multiInstanceLoopCharacteristics>
    </userTask>
    <sequenceFlow id="s1" sourceRef="chain" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_for(bpmn: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let def = compile(bpmn).expect("应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    (engine, store)
}

#[tokio::test]
async fn parallel_countersign_all_must_complete() {
    let (engine, _store) = engine_for(PARALLEL_ALL_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["cfo", "counsel", "hr"]));
    let started = engine
        .start_process("countersign_all", vars, None)
        .await
        .unwrap();

    // 会签展开成 3 个并行待办。
    assert_eq!(started.state, InstanceState::Active);
    assert_eq!(started.open_tasks.len(), 3, "3 人会签应产生 3 个并行任务");
    assert!(started.open_tasks.iter().all(|t| t.node_bpmn_id == "sign"));

    // 逐个办结前两个：实例仍 Active。
    let mut inst = started.clone();
    for _ in 0..2 {
        let t = inst.open_tasks[0].clone();
        inst = engine
            .complete_task(&started.instance_id, &t.id, Variables::new())
            .await
            .unwrap();
        assert_eq!(inst.state, InstanceState::Active, "未全部办结不应完成");
    }
    // 办结最后一个 → 全员到齐 → 完成。
    let last = inst.open_tasks[0].clone();
    let done = engine
        .complete_task(&started.instance_id, &last.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed, "3/3 办结应完成");
    assert!(done.open_tasks.is_empty());
}

#[tokio::test]
async fn parallel_countersign_majority_short_circuits() {
    let (engine, store) = engine_for(PARALLEL_MAJORITY_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["a", "b", "c", "d", "e"])); // 5 人
    let started = engine
        .start_process("countersign_majority", vars, None)
        .await
        .unwrap();
    assert_eq!(started.open_tasks.len(), 5, "5 人会签 5 个任务");

    // 办结 2 个：2/5 = 0.4 < 0.5，未达过半，仍 Active。
    let mut inst = started.clone();
    for _ in 0..2 {
        let t = inst.open_tasks[0].clone();
        inst = engine
            .complete_task(&started.instance_id, &t.id, Variables::new())
            .await
            .unwrap();
    }
    assert_eq!(inst.state, InstanceState::Active);
    assert_eq!(inst.open_tasks.len(), 3, "还剩 3 个待办");

    // 办结第 3 个：3/5 = 0.6 >= 0.5 → 命中 completionCondition → 提前收口。
    let third = inst.open_tasks[0].clone();
    let done = engine
        .complete_task(&started.instance_id, &third.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed, "过半应提前完成");
    assert!(done.open_tasks.is_empty(), "剩余 2 个任务应被作废");

    // 落库校验：作废任务已从活动任务集移除（仅剩 3 个已办结）。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    let open = snap.tasks.iter().filter(|t| !t.completed).count();
    assert_eq!(open, 0, "收口后无未办结任务");
    assert_eq!(snap.tasks.iter().filter(|t| t.completed).count(), 3);
    // MI 域应标记 finished。
    assert!(snap.mi_scopes.iter().all(|s| s.finished), "域应已收口");
}

#[tokio::test]
async fn sequential_or_sign_expands_one_at_a_time() {
    let (engine, store) = engine_for(SEQUENTIAL_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["组长", "经理", "总监"]));
    let started = engine.start_process("or_sign", vars, None).await.unwrap();

    // 顺序模式：一次只有一个待办。
    assert_eq!(started.open_tasks.len(), 1, "或签一次只展开一个");
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.mi_scopes.len(), 1);
    assert!(snap.mi_scopes[0].sequential);
    assert_eq!(snap.mi_scopes[0].total, 3);

    // 第一个同意（rejected 不置）→ 展开第二个。
    let t1 = started.open_tasks[0].clone();
    let inst = engine
        .complete_task(&started.instance_id, &t1.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(inst.state, InstanceState::Active);
    assert_eq!(inst.open_tasks.len(), 1, "应展开第二个");

    // 第二个驳回 → completionCondition(rejected==true) 命中 → 提前结束。
    let t2 = inst.open_tasks[0].clone();
    let mut reject = Variables::new();
    reject.set("rejected", json!(true));
    let done = engine
        .complete_task(&started.instance_id, &t2.id, reject)
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed, "驳回应终止或签并收口");
    assert!(done.open_tasks.is_empty(), "第三个不应再展开");

    let final_snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // 只办结了 2 个（第三个从未展开）。
    assert_eq!(final_snap.tasks.iter().filter(|t| t.completed).count(), 2);
    assert_eq!(final_snap.mi_scopes[0].completed, 2);
}

#[tokio::test]
async fn sequential_or_sign_all_pass_completes_naturally() {
    // 或签无人驳回：逐个走完 3 个 → 自然完成。
    let (engine, _store) = engine_for(SEQUENTIAL_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["a", "b", "c"]));
    let started = engine.start_process("or_sign", vars, None).await.unwrap();

    let mut inst = started.clone();
    for i in 0..3 {
        assert_eq!(inst.open_tasks.len(), 1, "第 {i} 步应只有一个待办");
        let t = inst.open_tasks[0].clone();
        inst = engine
            .complete_task(&started.instance_id, &t.id, Variables::new())
            .await
            .unwrap();
    }
    assert_eq!(inst.state, InstanceState::Completed, "全部走完应完成");
}

#[tokio::test]
async fn empty_collection_skips_multi_instance_node() {
    let (engine, _store) = engine_for(PARALLEL_ALL_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!([])); // 空集合
    let started = engine
        .start_process("countersign_all", vars, None)
        .await
        .unwrap();
    // 空集合 → MI 节点直接跳过 → 直达结束。
    assert_eq!(
        started.state,
        InstanceState::Completed,
        "空集合应跳过并完成"
    );
    assert!(started.open_tasks.is_empty());
}

#[tokio::test]
async fn element_value_carried_on_each_countersign_task() {
    // 每个会签子任务应携带各自的 element_value（会签每人看到各自的数据）。
    let (engine, store) = engine_for(PARALLEL_ALL_BPMN);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["cfo", "counsel"]));
    let started = engine
        .start_process("countersign_all", vars, None)
        .await
        .unwrap();
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    let elems: Vec<String> = snap
        .tasks
        .iter()
        .filter_map(|t| {
            t.element_value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    assert!(elems.contains(&"cfo".to_string()));
    assert!(elems.contains(&"counsel".to_string()));
}
