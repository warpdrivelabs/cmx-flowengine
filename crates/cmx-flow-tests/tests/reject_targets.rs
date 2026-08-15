//! ③ 端到端：reject_targets —— 列举任务的合法退回目标（内存态，始终可跑）。
//!
//! 验证：
//!   1) 两级审批，财务任务可退回目标 = [经理]（默认=经理，直接前驱）；
//!   2) 首个用户任务（上游只有 start）→ 无可退目标，rejectable=false；
//!   3) 三级审批，末节点可退 [中(1), 前(2)]，按距离升序，默认=直接前驱；
//!   4) 上游隔着排他网关也能穿透找到 userTask，默认目标跳网关取直接前驱 userTask；
//!   5) 会签/或签域内任务 → rejectable=false、目标空。

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use serde_json::json;

const TWO_LEVEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="two_level" name="两级审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="mgr"/>
    <userTask id="mgr" name="经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="mgr" targetRef="fin"/>
    <userTask id="fin" name="财务审批" flowable:assignee="财务"/>
    <sequenceFlow id="s2" sourceRef="fin" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

const THREE_LEVEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="three_level" name="三级审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="a"/>
    <userTask id="a" name="初审" flowable:assignee="a"/>
    <sequenceFlow id="s1" sourceRef="a" targetRef="b"/>
    <userTask id="b" name="复审" flowable:assignee="b"/>
    <sequenceFlow id="s2" sourceRef="b" targetRef="c"/>
    <userTask id="c" name="终审" flowable:assignee="c"/>
    <sequenceFlow id="s3" sourceRef="c" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 上游隔着排他网关：start → a → gw → b。退回 b 应穿透 gw 找到 a。
const GATEWAY_MID: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="gw_mid" name="含网关" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="a"/>
    <userTask id="a" name="申请" flowable:assignee="a"/>
    <sequenceFlow id="s1" sourceRef="a" targetRef="gw"/>
    <exclusiveGateway id="gw"/>
    <sequenceFlow id="s2" sourceRef="gw" targetRef="b"/>
    <userTask id="b" name="审批" flowable:assignee="b"/>
    <sequenceFlow id="s3" sourceRef="b" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 会签多实例：start → sign(MI over approvers) → done。
const MI_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="mi_flow" name="会签" isExecutable="true">
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

fn engine_with(xml: &str) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(xml).expect("编译")).expect("部署");
    (engine, store)
}

async fn open_task_at(store: &InMemoryStore, iid: &str, node: &str) -> Option<String> {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed && t.node_bpmn_id == node)
        .map(|t| t.id.clone())
}

#[tokio::test]
async fn targets_for_second_level_is_previous_user_task() {
    let (engine, store) = engine_with(TWO_LEVEL);
    let iid = engine
        .start_process("two_level", Variables::new(), None)
        .await
        .unwrap()
        .instance_id;
    // 办结经理 → 到财务。
    let mgr = open_task_at(&store, &iid, "mgr").await.unwrap();
    engine.complete_task(&iid, &mgr, Variables::new()).await.unwrap();
    let fin = open_task_at(&store, &iid, "fin").await.unwrap();

    let info = engine.reject_targets(&iid, &fin).await.unwrap();
    assert!(info.rejectable, "财务任务应可退回");
    assert_eq!(info.current_node, "fin");
    assert_eq!(info.default_target.as_deref(), Some("mgr"), "默认退回目标=经理");
    assert_eq!(info.targets.len(), 1, "只有经理一个可退目标");
    let t = &info.targets[0];
    assert_eq!(t.bpmn_id, "mgr");
    assert_eq!(t.name.as_deref(), Some("经理审批"));
    assert!(t.is_direct_predecessor, "经理是直接前驱");
    assert_eq!(t.distance, 1);
}

#[tokio::test]
async fn first_user_task_has_no_reject_targets() {
    let (engine, store) = engine_with(TWO_LEVEL);
    let iid = engine
        .start_process("two_level", Variables::new(), None)
        .await
        .unwrap()
        .instance_id;
    // 首个用户任务经理：上游只有 start（非 userTask）→ 无可退目标。
    let mgr = open_task_at(&store, &iid, "mgr").await.unwrap();
    let info = engine.reject_targets(&iid, &mgr).await.unwrap();
    assert!(!info.rejectable, "首个用户任务无处可退");
    assert!(info.targets.is_empty());
    assert!(info.default_target.is_none(), "无唯一直接前驱 userTask");
}

#[tokio::test]
async fn three_level_targets_ordered_by_distance() {
    let (engine, store) = engine_with(THREE_LEVEL);
    let iid = engine
        .start_process("three_level", Variables::new(), None)
        .await
        .unwrap()
        .instance_id;
    // 推进到 c。
    let a = open_task_at(&store, &iid, "a").await.unwrap();
    engine.complete_task(&iid, &a, Variables::new()).await.unwrap();
    let b = open_task_at(&store, &iid, "b").await.unwrap();
    engine.complete_task(&iid, &b, Variables::new()).await.unwrap();
    let c = open_task_at(&store, &iid, "c").await.unwrap();

    let info = engine.reject_targets(&iid, &c).await.unwrap();
    assert!(info.rejectable);
    let ids: Vec<&str> = info.targets.iter().map(|t| t.bpmn_id.as_str()).collect();
    assert_eq!(ids, vec!["b", "a"], "按距离升序：先 b(1) 再 a(2)");
    assert_eq!(info.targets[0].distance, 1);
    assert_eq!(info.targets[1].distance, 2);
    assert_eq!(info.default_target.as_deref(), Some("b"), "默认=直接前驱 b");
    assert!(info.targets[0].is_direct_predecessor, "b 是直接前驱");
    assert!(!info.targets[1].is_direct_predecessor, "a 不是直接前驱");
}

#[tokio::test]
async fn targets_traverse_through_gateway() {
    let (engine, store) = engine_with(GATEWAY_MID);
    let iid = engine
        .start_process("gw_mid", Variables::new(), None)
        .await
        .unwrap()
        .instance_id;
    // 办结 a → 令牌穿网关到 b。
    let a = open_task_at(&store, &iid, "a").await.unwrap();
    engine.complete_task(&iid, &a, Variables::new()).await.unwrap();
    let b = open_task_at(&store, &iid, "b").await.unwrap();

    let info = engine.reject_targets(&iid, &b).await.unwrap();
    assert!(info.rejectable);
    let ids: Vec<&str> = info.targets.iter().map(|t| t.bpmn_id.as_str()).collect();
    assert_eq!(ids, vec!["a"], "穿透网关找到 a（网关自身不列为目标）");
    assert_eq!(info.default_target.as_deref(), Some("a"), "默认目标跳网关取 a");
    assert!(info.targets[0].is_direct_predecessor);
}

#[tokio::test]
async fn multi_instance_task_not_rejectable() {
    let (engine, store) = engine_with(MI_FLOW);
    let mut vars = Variables::new();
    vars.set("approvers", json!(["u1", "u2"]));
    let iid = engine
        .start_process("mi_flow", vars, None)
        .await
        .unwrap()
        .instance_id;
    // 会签展开的任一子任务。
    let sub = open_task_at(&store, &iid, "sign").await.unwrap();
    let info = engine.reject_targets(&iid, &sub).await.unwrap();
    assert!(!info.rejectable, "会签域内任务不支持退回");
    assert!(info.targets.is_empty());
}
