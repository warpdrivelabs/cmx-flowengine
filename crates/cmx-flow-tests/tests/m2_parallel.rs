//! M2 端到端：并行网关 fork / join（内存态，始终可跑）。
//!
//! 验证并行网关的 AND 语义：
//! - fork：一个令牌分裂成多个，多分支齐头并进
//! - join：等所有分支到齐才放行下游（先到的分支阻塞在 Joining）
//! - 分支中的 userTask 各自独立等待、独立办结
//! - 全部合流并抵达结束后，实例 Completed

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};

/// 并行会签样例：发起 → fork ┬ 财务审批(userTask) ┐ join → 结束
///                              └ 法务审批(userTask) ┘
const PARALLEL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="parallel_sign" name="并行会签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork" name="分发"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="finance"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="legal"/>
    <userTask id="finance" name="财务审批" flowable:assignee="cfo"/>
    <userTask id="legal" name="法务审批" flowable:assignee="counsel"/>
    <sequenceFlow id="s3" sourceRef="finance" targetRef="join"/>
    <sequenceFlow id="s4" sourceRef="legal" targetRef="join"/>
    <parallelGateway id="join" name="合流"/>
    <sequenceFlow id="s5" sourceRef="join" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn build() -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let def = compile(PARALLEL_BPMN).expect("并行会签应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    (engine, store)
}

#[tokio::test]
async fn fork_creates_two_parallel_tasks() {
    let (engine, _store) = build();
    let started = engine
        .start_process("parallel_sign", Variables::new(), None)
        .await
        .unwrap();

    // fork 后应同时出现两个待办任务。
    assert_eq!(started.state, InstanceState::Active);
    assert_eq!(started.open_tasks.len(), 2, "fork 应产生 2 个并行任务");
    let nodes: Vec<&str> = started
        .open_tasks
        .iter()
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert!(nodes.contains(&"finance"));
    assert!(nodes.contains(&"legal"));
}

#[tokio::test]
async fn join_waits_for_all_branches_before_proceeding() {
    let (engine, store) = build();
    let started = engine
        .start_process("parallel_sign", Variables::new(), None)
        .await
        .unwrap();

    // 先办结「财务」这一支。
    let finance = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "finance")
        .unwrap()
        .clone();
    let after_first = engine
        .complete_task(&started.instance_id, &finance.id, Variables::new())
        .await
        .unwrap();

    // 只办结一支：实例仍 Active，legal 待办仍在；财务令牌应阻塞在 join（Joining）。
    assert_eq!(
        after_first.state,
        InstanceState::Active,
        "半数分支未办结，实例不应完成"
    );
    assert_eq!(after_first.open_tasks.len(), 1);
    assert_eq!(after_first.open_tasks[0].node_bpmn_id, "legal");

    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    let joining = snap
        .tokens
        .iter()
        .filter(|t| t.state == TokenState::Joining && t.node_bpmn_id == "join")
        .count();
    assert_eq!(joining, 1, "财务分支令牌应阻塞在 join 等待合流");

    // 再办结「法务」这一支 → 合流 → 结束。
    let legal = after_first.open_tasks[0].clone();
    let done = engine
        .complete_task(&started.instance_id, &legal.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed, "两支都办结后应完成");
    assert!(done.open_tasks.is_empty());

    // 落库校验：合流后只剩一个 Ended 令牌（兄弟已被合并删除）。
    let final_snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(final_snap.tokens.len(), 1, "合流后应只剩一个幸存令牌");
    assert_eq!(final_snap.tokens[0].state, TokenState::Ended);
}

#[tokio::test]
async fn join_order_independent() {
    // 反序办结（先 legal 后 finance），结果应一致——验证 join 不依赖到达顺序。
    let (engine, _store) = build();
    let started = engine
        .start_process("parallel_sign", Variables::new(), None)
        .await
        .unwrap();
    let legal = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "legal")
        .unwrap()
        .clone();
    let after_first = engine
        .complete_task(&started.instance_id, &legal.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(after_first.state, InstanceState::Active);

    let finance = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "finance")
        .unwrap()
        .clone();
    let done = engine
        .complete_task(&started.instance_id, &finance.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}
