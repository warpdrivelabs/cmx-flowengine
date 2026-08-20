//! A9 实例迁移端到端测试（内存）。
//!
//! 验证：
//! - 节点映射迁移：令牌重定位到目标定义对应节点 + 实例 definition_key 改指向
//! - 迁移后实例在新定义上照常推进（办结迁移后的任务 → 走新定义出边）
//! - 校验挡未映射的活动节点 / 目标节点不存在 / 目标定义未部署
//! - 校验通过为空违规（ok=true）

use std::collections::BTreeMap;

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, MigrationPlan, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};

// 源定义 v1：start → review(userTask) → end
const SRC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="mig_v1" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="审核v1" flowable:assignee="u1"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#;

// 目标定义 v2：start → approve(userTask) → extra(userTask) → end（多一个环节）
const TGT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="mig_v2" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="审批v2" flowable:assignee="u1"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="extra"/>
    <userTask id="extra" name="附加环节" flowable:assignee="u2"/>
    <sequenceFlow id="s2" sourceRef="extra" targetRef="end"/>
    <endEvent id="end"/>
  </process>
</definitions>"#;

async fn setup() -> (Engine<InMemoryStore>, InMemoryStore, String) {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(SRC).expect("编译src")).expect("部署src");
    engine.deploy(compile(TGT).expect("编译tgt")).expect("部署tgt");
    let started = engine
        .start_process("mig_v1", Variables::new(), None)
        .await
        .expect("启动");
    let iid = started.instance_id.clone();
    // 停在 review。
    (engine, store, iid)
}

fn plan(target: &str, pairs: &[(&str, &str)]) -> MigrationPlan {
    let mut m = BTreeMap::new();
    for (a, b) in pairs {
        m.insert(a.to_string(), b.to_string());
    }
    MigrationPlan { target_definition_key: target.to_string(), activity_mappings: m }
}

#[tokio::test]
async fn migrate_rewrites_token_and_definition() {
    let (engine, store, iid) = setup().await;
    // review(v1) → approve(v2)。
    let p = plan("mig_v2", &[("review", "approve")]);
    let v = engine.validate_migration(&iid, &p).await.unwrap();
    assert!(v.ok, "应校验通过: {:?}", v.violations);

    engine.migrate_instance(&iid, &p).await.expect("迁移");
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.instance.definition_key, "mig_v2", "定义指向已改");
    let tok = snap.tokens.iter().find(|t| t.state == TokenState::Waiting).unwrap();
    assert_eq!(tok.node_bpmn_id, "approve", "令牌重定位到目标节点");
    // 任务节点同步重写。
    let task = snap.tasks.iter().find(|t| !t.completed).unwrap();
    assert_eq!(task.node_bpmn_id, "approve");
}

#[tokio::test]
async fn migrated_instance_advances_on_new_definition() {
    let (engine, store, iid) = setup().await;
    let p = plan("mig_v2", &[("review", "approve")]);
    engine.migrate_instance(&iid, &p).await.expect("迁移");

    // 办结迁移后的 approve 任务 → 应走 v2 出边到 extra（v1 里 review 后直接 end）。
    let snap = store.load_snapshot(&iid).await.unwrap();
    let task_id = snap.tasks.iter().find(|t| !t.completed).unwrap().id.clone();
    engine.complete_task(&iid, &task_id, Variables::new()).await.expect("办结");

    let snap2 = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap2.instance.state, InstanceState::Active, "还没结束(v2 多一环)");
    let open = snap2.tasks.iter().find(|t| !t.completed).unwrap();
    assert_eq!(open.node_bpmn_id, "extra", "在新定义上推进到 extra 环节");
}

#[tokio::test]
async fn validate_blocks_unmapped_activity() {
    let (engine, _store, iid) = setup().await;
    // 空映射 → review 无映射。
    let p = plan("mig_v2", &[]);
    let v = engine.validate_migration(&iid, &p).await.unwrap();
    assert!(!v.ok);
    assert!(v.violations.iter().any(|x| format!("{:?}", x.code) == "UnmappedActivity"));
    // 迁移应被拒。
    assert!(engine.migrate_instance(&iid, &p).await.is_err());
}

#[tokio::test]
async fn validate_blocks_missing_target_node() {
    let (engine, _store, iid) = setup().await;
    let p = plan("mig_v2", &[("review", "nonexistent")]);
    let v = engine.validate_migration(&iid, &p).await.unwrap();
    assert!(!v.ok);
    assert!(v.violations.iter().any(|x| format!("{:?}", x.code) == "TargetNodeMissing"));
}

#[tokio::test]
async fn validate_blocks_undeployed_target() {
    let (engine, _store, iid) = setup().await;
    let p = plan("no_such_def", &[("review", "approve")]);
    let v = engine.validate_migration(&iid, &p).await.unwrap();
    assert!(!v.ok);
    assert!(v.violations.iter().any(|x| format!("{:?}", x.code) == "TargetNotDeployed"));
}
