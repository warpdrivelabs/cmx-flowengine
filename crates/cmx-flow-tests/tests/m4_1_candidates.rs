//! M4.1 端到端：候选人解析 + 认领（内存 + 假 resolver，始终可跑）。
//!
//! 验证「办理人从静态字符串升级为角色/岗位/部门解析」：
//! - 单人解析 → 直派 assignee
//! - 多人解析 → 落候选池待认领，claim 后据为己有
//! - 无 resolver → 宽容退回静态 assignee（M1 老路）
//! - 认领校验：非候选人不能认领、已认领不能被他人抢

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    AssigneeResolver, CandidateRef, Engine, InMemoryStore, InstanceState, ResolveResult, Variables,
};
use cmx_flow_model::{CandidateKind, RuntimeStore};

/// 假解析器：内存映射「引用 → 用户列表」，测试用，确定性。
#[derive(Default)]
struct FakeResolver {
    // key: "ROLE:finance" / "USER:u1" / "POSITION:cfo"，value: 用户 id 列表
    map: HashMap<String, Vec<String>>,
}

impl FakeResolver {
    fn with(mut self, key: &str, users: &[&str]) -> Self {
        self.map.insert(
            key.to_string(),
            users.iter().map(|s| s.to_string()).collect(),
        );
        self
    }
    fn key(kind: CandidateKind, value: &str) -> String {
        let k = match kind {
            CandidateKind::User => "USER",
            CandidateKind::Role => "ROLE",
            CandidateKind::Position => "POSITION",
            CandidateKind::Org => "ORG",
        };
        format!("{k}:{value}")
    }
}

#[async_trait]
impl AssigneeResolver for FakeResolver {
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>> {
        // User 引用直接返回自身（模拟"user(id) 就是该用户"）。
        if candidate.kind == CandidateKind::User {
            return Ok(vec![candidate.value.clone()]);
        }
        Ok(self
            .map
            .get(&Self::key(candidate.kind, &candidate.value))
            .cloned()
            .unwrap_or_default())
    }
}

/// 单角色审批：发起 → 审批(candidateGroups=finance) → 结束
const ROLE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="role_approve" name="角色审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="财务审批" flowable:candidateGroups="finance"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_with_resolver(
    bpmn: &str,
    resolver: FakeResolver,
) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let def = compile(bpmn).expect("应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    engine.set_resolver(Arc::new(resolver));
    (engine, store)
}

#[tokio::test]
async fn single_resolved_user_is_directly_assigned() {
    // 角色 finance 只有一人 → 直派。
    let resolver = FakeResolver::default().with("ROLE:finance", &["u_cfo"]);
    let (engine, store) = engine_with_resolver(ROLE_BPMN, resolver);
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    assert_eq!(started.open_tasks.len(), 1);
    assert_eq!(
        started.open_tasks[0].assignee.as_deref(),
        Some("u_cfo"),
        "单人应直派"
    );

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(snap.candidates.is_empty(), "单人直派不产生候选池");
}

#[tokio::test]
async fn multiple_resolved_users_go_to_candidate_pool() {
    // 角色 finance 有三人 → 落候选池，assignee 空。
    let resolver = FakeResolver::default().with("ROLE:finance", &["u_a", "u_b", "u_c"]);
    let (engine, store) = engine_with_resolver(ROLE_BPMN, resolver);
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    assert_eq!(started.open_tasks.len(), 1);
    assert!(
        started.open_tasks[0].assignee.is_none(),
        "多人应待认领，assignee 空"
    );

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.candidates.len(), 3, "三人候选池");
    let users: Vec<&str> = snap
        .candidates
        .iter()
        .map(|c| c.resolved_user_id.as_str())
        .collect();
    assert!(users.contains(&"u_a") && users.contains(&"u_b") && users.contains(&"u_c"));
    assert!(
        snap.candidates
            .iter()
            .all(|c| c.candidate_type == CandidateKind::Role)
    );
}

#[tokio::test]
async fn claim_assigns_and_clears_pool() {
    let resolver = FakeResolver::default().with("ROLE:finance", &["u_a", "u_b", "u_c"]);
    let (engine, store) = engine_with_resolver(ROLE_BPMN, resolver);
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();

    // u_b 认领 → assignee=u_b，候选池清空。
    let after = engine
        .claim_task(&started.instance_id, &task_id, "u_b")
        .await
        .unwrap();
    assert_eq!(after.open_tasks[0].assignee.as_deref(), Some("u_b"));
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(snap.candidates.is_empty(), "认领后候选池清空");

    // 认领后可正常办结 → 完成。
    let done = engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}

#[tokio::test]
async fn non_candidate_cannot_claim() {
    let resolver = FakeResolver::default().with("ROLE:finance", &["u_a", "u_b"]);
    let (engine, _store) = engine_with_resolver(ROLE_BPMN, resolver);
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();

    // u_x 不在候选池，认领应失败。
    let err = engine
        .claim_task(&started.instance_id, &task_id, "u_x")
        .await;
    assert!(err.is_err(), "非候选人不能认领");
}

#[tokio::test]
async fn claimed_task_cannot_be_stolen() {
    let resolver = FakeResolver::default().with("ROLE:finance", &["u_a", "u_b"]);
    let (engine, _store) = engine_with_resolver(ROLE_BPMN, resolver);
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();

    engine
        .claim_task(&started.instance_id, &task_id, "u_a")
        .await
        .unwrap();
    // u_a 幂等再认领 OK。
    assert!(
        engine
            .claim_task(&started.instance_id, &task_id, "u_a")
            .await
            .is_ok()
    );
    // u_b 想抢 → 失败。
    assert!(
        engine
            .claim_task(&started.instance_id, &task_id, "u_b")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn no_resolver_falls_back_to_static_assignee() {
    // 含 candidateGroups 但未注入 resolver → 退回静态 assignee（这里 assignee 也空 → 任务无办理人但不崩）。
    let store = InMemoryStore::new();
    let def = compile(ROLE_BPMN).unwrap();
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).unwrap();
    // 注意：不 set_resolver。
    let started = engine
        .start_process("role_approve", Variables::new(), None)
        .await
        .unwrap();
    // 宽容降级：不报错，任务照常生成（assignee 空，候选池空）。
    assert_eq!(started.state, InstanceState::Active);
    assert_eq!(started.open_tasks.len(), 1);
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(snap.candidates.is_empty(), "无 resolver 不落候选池");
}

#[tokio::test]
async fn mixed_expression_positions_and_users() {
    // 混合表达式：position(cfo) 解析 1 人 + user(u_fixed) → 共 2 人候选。
    const MIX_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                 xmlns:cmx="http://cmx/flow">
      <process id="mix" isExecutable="true">
        <startEvent id="s"/>
        <sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
        <userTask id="t" name="混合审批" cmx:candidates="position(cfo), user(u_fixed)"/>
        <sequenceFlow id="f1" sourceRef="t" targetRef="done"/>
        <endEvent id="done"/>
      </process></definitions>"#;
    let resolver = FakeResolver::default().with("POSITION:cfo", &["u_cfo"]);
    let (engine, store) = engine_with_resolver(MIX_BPMN, resolver);
    let started = engine
        .start_process("mix", Variables::new(), None)
        .await
        .unwrap();
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    // u_cfo（岗位）+ u_fixed（指定）= 2 人候选。
    assert_eq!(snap.candidates.len(), 2);
    let users: Vec<&str> = snap
        .candidates
        .iter()
        .map(|c| c.resolved_user_id.as_str())
        .collect();
    assert!(users.contains(&"u_cfo") && users.contains(&"u_fixed"));
}
