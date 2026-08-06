//! M3 端到端：PG 落库（多实例会签 + 重启恢复 + 取消归档，#[ignore] 门控）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-tests --test m3_pg -- --ignored --nocapture
//!
//! 验证要点（延续 M2/b「重启从库恢复」保证）：
//! - 会签展开的 N 个任务 + MiScope 正确落库；
//! - 用**全新 Engine**（模拟进程重启）从 PG 恢复，completionCondition 仍能正确求值；
//! - cancel_process 后实例归档进 hi_instance（Terminated）。

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, Variables};
use cmx_flow_model::RuntimeStore;
use cmx_flow_store_pg::PgRuntimeStore;
use serde_json::json;

const MAJORITY_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_countersign" name="PG过半会签" isExecutable="true">
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

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m3_test".to_string();
    let manager = get_default_pg_db_manager();
    let cfg = DbConfig {
        db_type: DbType::Postgres,
        db_url: url,
        db_id: db_id.clone(),
        db_name: None,
        db_schema: Some("public".to_string()),
        default: true,
        pool_config: Default::default(),
        health_check_interval: 60,
        health_check_timeout: 5,
        domain_code: None,
        application_code: None,
        module_code: None,
        source_type: Some("default".to_string()),
    };
    manager
        .register_data_source(cfg)
        .await
        .expect("注册数据源失败");
    Some(db_id)
}

async fn cleanup(db_id: &str, instance_id: &str) {
    for sql in [
        format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_mi_scope WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_token WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_instance WHERE id = '{instance_id}'"),
    ] {
        let _ = cmx_database_pg::execute_sql(db_id, None, &sql).await;
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_countersign_persists_and_recovers_after_restart() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");

    let def = compile(MAJORITY_BPMN).unwrap();

    // —— 进程 1：启动会签，办结 1 个（1/5 < 0.5，未达过半），落库后「退出」 —— //
    let (instance_id, first_task_id) = {
        let mut engine = Engine::new(store.clone());
        engine.deploy(def.clone()).unwrap();
        let mut vars = Variables::new();
        vars.set("approvers", json!(["a", "b", "c", "d", "e"]));
        let started = engine
            .start_process("pg_countersign", vars, Some("PG-CS-001".into()))
            .await
            .expect("启动应成功");
        assert_eq!(started.open_tasks.len(), 5, "5 人会签落库 5 个任务");

        // MiScope 落库校验。
        let mi = query_sql(
            &db_id,
            None,
            &format!(
                "SELECT total, completed, finished FROM cmx_flow_mi_scope WHERE instance_id = '{}'",
                started.instance_id
            ),
            "mi",
        )
        .await
        .expect("查询 mi_scope 失败");
        assert_eq!(mi.row_count(), 1, "应有一个 MiScope 落库");

        let t0 = started.open_tasks[0].clone();
        let mid = engine
            .complete_task(&started.instance_id, &t0.id, Variables::new())
            .await
            .unwrap();
        assert_eq!(mid.state, InstanceState::Active, "1/5 未过半，仍 Active");
        (started.instance_id, t0.id)
    };
    let _ = first_task_id;

    // —— 进程 2（全新 Engine，模拟重启）：从 PG 恢复，继续办结到过半 —— //
    {
        let mut engine2 = Engine::new(store.clone());
        engine2.deploy(def.clone()).unwrap();

        // 从库恢复快照，取剩余待办。
        let snap = engine2.store().load_snapshot(&instance_id).await.unwrap();
        assert_eq!(
            snap.tasks.iter().filter(|t| !t.completed).count(),
            4,
            "重启后应恢复 4 个未办结任务"
        );
        assert_eq!(snap.mi_scopes.len(), 1);
        assert_eq!(snap.mi_scopes[0].completed, 1, "已办结计数应从库恢复");

        // 再办结 2 个 → 累计 3/5 = 0.6 >= 0.5 → completionCondition 命中 → 完成。
        let open: Vec<String> = snap
            .tasks
            .iter()
            .filter(|t| !t.completed)
            .map(|t| t.id.clone())
            .collect();
        let after_2nd = engine2
            .complete_task(&instance_id, &open[0], Variables::new())
            .await
            .unwrap();
        assert_eq!(after_2nd.state, InstanceState::Active, "2/5 仍未过半");

        let done = engine2
            .complete_task(&instance_id, &open[1], Variables::new())
            .await
            .unwrap();
        assert_eq!(
            done.state,
            InstanceState::Completed,
            "重启后累计过半应完成——证明计数从库恢复且 completionCondition 正确求值"
        );
        assert!(done.open_tasks.is_empty());
    }

    // 归档校验：完成实例进 hi_instance。
    let hi = query_sql(
        &db_id,
        None,
        "SELECT state FROM cmx_flow_hi_instance WHERE business_key = 'PG-CS-001'",
        "hi",
    )
    .await
    .expect("查询历史实例失败");
    assert_eq!(hi.row_count(), 1, "完成实例应归档");

    cleanup(&db_id, &instance_id).await;
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_cancel_archives_to_history() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let def = compile(MAJORITY_BPMN).unwrap();
    let mut engine = Engine::new(store);
    engine.deploy(def).unwrap();

    let mut vars = Variables::new();
    vars.set("approvers", json!(["x", "y", "z"]));
    let started = engine
        .start_process("pg_countersign", vars, Some("PG-CANCEL-001".into()))
        .await
        .unwrap();
    assert_eq!(started.open_tasks.len(), 3);

    // 撤单。
    let canceled = engine
        .cancel_process(&started.instance_id, Some("测试撤回".into()))
        .await
        .unwrap();
    assert_eq!(canceled.state, InstanceState::Terminated);

    // 运行态：无未办结任务；MiScope 收口。
    let snap = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .unwrap();
    assert_eq!(snap.tasks.iter().filter(|t| !t.completed).count(), 0);
    assert!(snap.mi_scopes.iter().all(|s| s.finished));

    // 归档：Terminated 实例进 hi_instance。
    let hi = query_sql(
        &db_id,
        None,
        "SELECT state FROM cmx_flow_hi_instance WHERE business_key = 'PG-CANCEL-001'",
        "hi",
    )
    .await
    .expect("查询历史实例失败");
    assert_eq!(hi.row_count(), 1, "取消的实例应归档为 Terminated");

    cleanup(&db_id, &started.instance_id).await;
}
