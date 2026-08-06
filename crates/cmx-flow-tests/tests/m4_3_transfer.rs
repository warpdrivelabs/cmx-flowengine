//! M4.3 端到端：转签家族（转办 / 委派 / 加签，内存态，始终可跑）。
//!
//! - 转办：换人，assignee+owner 都改，原人退出
//! - 委派：代办不换主，owner 保持、assignee 改代理人
//! - 前/后加签：插临时任务，原任务挂起，临时办结后原任务恢复
//! - 嵌套加签：临时任务再被加签
//! - 挂起守卫：被加签挂起的原任务不可直接办结
//! - 台账：每步记入 delegations

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, InstanceState, RuntimeStore, Variables};

/// 单任务审批：start → review(assignee=张三) → done
const BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="approve" name="审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="审批" flowable:assignee="张三"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn start() -> (Engine<InMemoryStore>, InMemoryStore, String, String) {
    let store = InMemoryStore::new();
    let def = compile(BPMN).expect("应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    let s = engine
        .start_process("approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = s.open_tasks[0].id.clone();
    (engine, store, s.instance_id, task_id)
}

#[tokio::test]
async fn transfer_swaps_person_completely() {
    let (engine, store, iid, task) = start().await;
    engine
        .transfer_task(&iid, &task, "张三", "李四", Some("我出差"))
        .await
        .unwrap();

    let snap = store.load_snapshot(&iid).await.unwrap();
    let t = snap.tasks.iter().find(|t| t.id == task).unwrap();
    assert_eq!(t.assignee.as_deref(), Some("李四"), "转办后办理人=李四");
    assert_eq!(
        t.owner_user_id.as_deref(),
        Some("李四"),
        "转办彻底换人，owner 也归李四"
    );
    assert_eq!(snap.delegations.len(), 1);
    assert_eq!(snap.delegations[0].kind, "TRANSFER");

    // 李四可正常办结 → 完成。
    let done = engine
        .complete_task(&iid, &task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}

#[tokio::test]
async fn delegate_keeps_owner() {
    let (engine, store, iid, task) = start().await;
    engine
        .delegate_task(&iid, &task, "张三", "李四", Some("帮我看下"))
        .await
        .unwrap();

    let snap = store.load_snapshot(&iid).await.unwrap();
    let t = snap.tasks.iter().find(|t| t.id == task).unwrap();
    assert_eq!(t.assignee.as_deref(), Some("李四"), "代理人=李四");
    assert_eq!(
        t.owner_user_id.as_deref(),
        Some("张三"),
        "委派 owner 仍是张三"
    );
    assert_eq!(t.delegation_state.as_deref(), Some("DELEGATED"));
    assert_eq!(snap.delegations[0].kind, "DELEGATE");

    // 代理人办结 → 完成（M4.3 简化：不回 owner 确认）。
    let done = engine
        .complete_task(&iid, &task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}

#[tokio::test]
async fn add_sign_before_inserts_temp_then_resumes() {
    let (engine, store, iid, task) = start().await;
    // 向前加签：先让王五审。
    let after = engine
        .add_sign(&iid, &task, "张三", "王五", true, Some("请先过目"))
        .await
        .unwrap();
    // 现在有两个未办结任务：原任务(挂起) + 临时任务(王五)。
    let snap = store.load_snapshot(&iid).await.unwrap();
    let orig = snap.tasks.iter().find(|t| t.id == task).unwrap();
    assert_eq!(
        orig.delegation_state.as_deref(),
        Some("SUSPENDED"),
        "原任务挂起"
    );
    let temp = snap
        .tasks
        .iter()
        .find(|t| t.parent_task_id.as_deref() == Some(task.as_str()))
        .unwrap();
    assert_eq!(temp.assignee.as_deref(), Some("王五"));
    assert_eq!(temp.delegation_state.as_deref(), Some("ADDSIGN"));
    let temp_id = temp.id.clone();
    // 对外视图：两个待办。
    assert_eq!(after.open_tasks.len(), 2);

    // 挂起守卫：原任务不可直接办结。
    assert!(
        engine
            .complete_task(&iid, &task, Variables::new())
            .await
            .is_err(),
        "挂起任务不可办结"
    );

    // 王五办结临时任务 → 原任务恢复（不推进流程）。
    let mid = engine
        .complete_task(&iid, &temp_id, Variables::new())
        .await
        .unwrap();
    assert_eq!(mid.state, InstanceState::Active, "临时办结不推进流程");
    let snap2 = store.load_snapshot(&iid).await.unwrap();
    let orig2 = snap2.tasks.iter().find(|t| t.id == task).unwrap();
    assert!(orig2.delegation_state.is_none(), "原任务恢复（解除挂起）");
    assert_eq!(
        snap2.tasks.iter().filter(|t| !t.completed).count(),
        1,
        "只剩原任务待办"
    );

    // 张三办结原任务 → 完成。
    let done = engine
        .complete_task(&iid, &task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
    assert_eq!(
        store.load_snapshot(&iid).await.unwrap().delegations[0].kind,
        "ADDSIGN_BEFORE"
    );
}

#[tokio::test]
async fn add_sign_after_recorded_and_resumes() {
    let (engine, store, iid, task) = start().await;
    engine
        .add_sign(&iid, &task, "张三", "王五", false, None)
        .await
        .unwrap();
    let snap = store.load_snapshot(&iid).await.unwrap();
    assert_eq!(snap.delegations[0].kind, "ADDSIGN_AFTER");
    let temp_id = snap
        .tasks
        .iter()
        .find(|t| t.parent_task_id.is_some())
        .unwrap()
        .id
        .clone();

    // 临时办结 → 恢复 → 原任务办结 → 完成。
    engine
        .complete_task(&iid, &temp_id, Variables::new())
        .await
        .unwrap();
    let done = engine
        .complete_task(&iid, &task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}

#[tokio::test]
async fn nested_add_sign() {
    let (engine, store, iid, task) = start().await;
    // 张三加签王五。
    engine
        .add_sign(&iid, &task, "张三", "王五", true, None)
        .await
        .unwrap();
    let temp1 = store
        .load_snapshot(&iid)
        .await
        .unwrap()
        .tasks
        .iter()
        .find(|t| t.parent_task_id.as_deref() == Some(task.as_str()))
        .unwrap()
        .id
        .clone();
    // 王五再加签赵六（临时任务嵌套加签）。
    engine
        .add_sign(&iid, &temp1, "王五", "赵六", true, None)
        .await
        .unwrap();

    let snap = store.load_snapshot(&iid).await.unwrap();
    // temp1 挂起，temp2(赵六) 待办，原任务仍挂起。
    let temp1_t = snap.tasks.iter().find(|t| t.id == temp1).unwrap();
    assert_eq!(
        temp1_t.delegation_state.as_deref(),
        Some("SUSPENDED"),
        "temp1 被嵌套加签而挂起"
    );
    let temp2 = snap
        .tasks
        .iter()
        .find(|t| t.parent_task_id.as_deref() == Some(temp1.as_str()))
        .unwrap()
        .id
        .clone();
    assert_eq!(
        snap.tasks.iter().filter(|t| !t.completed).count(),
        3,
        "原+temp1+temp2 三个未办结"
    );
    assert_eq!(snap.delegations.len(), 2);

    // 赵六办结 temp2 → temp1 恢复。
    engine
        .complete_task(&iid, &temp2, Variables::new())
        .await
        .unwrap();
    let s2 = store.load_snapshot(&iid).await.unwrap();
    assert!(
        s2.tasks
            .iter()
            .find(|t| t.id == temp1)
            .unwrap()
            .delegation_state
            .is_none(),
        "temp1 恢复"
    );
    // 王五办结 temp1 → 原任务恢复。
    engine
        .complete_task(&iid, &temp1, Variables::new())
        .await
        .unwrap();
    let s3 = store.load_snapshot(&iid).await.unwrap();
    assert!(
        s3.tasks
            .iter()
            .find(|t| t.id == task)
            .unwrap()
            .delegation_state
            .is_none(),
        "原任务恢复"
    );
    // 张三办结 → 完成。
    let done = engine
        .complete_task(&iid, &task, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);
}
