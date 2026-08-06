//! M4.3 端到端：PG 落库转签（#[ignore] 门控，用 fico 库）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-tests --test m4_3_pg -- --ignored --nocapture --test-threads=1
//!
//! 验证：task 补列 owner/parent/delegation_state 落库、cmx_flow_task_delegation 台账落库、
//! 加签临时任务重启后可恢复并办结。

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, RuntimeStore, Variables};
use cmx_flow_store_pg::PgRuntimeStore;

const BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_transfer" name="PG转签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="审批" flowable:assignee="张三"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m43_test".to_string();
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

async fn cleanup(db_id: &str, iid: &str) {
    for sql in [
        format!("DELETE FROM cmx_flow_task_delegation WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_cc WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{iid}'"),
        format!("DELETE FROM cmx_flow_task_candidate WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_job WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_mi_scope WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_task WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_token WHERE instance_id = '{iid}'"),
        format!("DELETE FROM cmx_flow_instance WHERE id = '{iid}'"),
    ] {
        let _ = cmx_database_pg::execute_sql(db_id, None, &sql).await;
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_add_sign_persists_and_resumes_after_restart() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let def = compile(BPMN).unwrap();

    // 进程 1：起流程 → 前加签王五 → 退出。
    let (iid, task_id, temp_id) = {
        let mut e1 = Engine::new(store.clone());
        e1.deploy(def.clone()).unwrap();
        let s = e1
            .start_process("pg_transfer", Variables::new(), Some("PG-TR-001".into()))
            .await
            .unwrap();
        let task_id = s.open_tasks[0].id.clone();
        e1.add_sign(
            &s.instance_id,
            &task_id,
            "张三",
            "王五",
            true,
            Some("请先审"),
        )
        .await
        .unwrap();

        // 台账落库校验。
        let d = query_sql(
            &db_id,
            None,
            &format!(
                "SELECT kind, temp_task_id FROM cmx_flow_task_delegation WHERE instance_id = '{}'",
                s.instance_id
            ),
            "deleg",
        )
        .await
        .expect("查台账失败");
        assert_eq!(d.row_count(), 1, "应落一条转签台账");

        // 原任务 SUSPENDED 落库校验。
        let susp = query_sql(&db_id, None,
            &format!("SELECT id FROM cmx_flow_task WHERE id = '{task_id}' AND delegation_state = 'SUSPENDED'"),
            "susp").await.unwrap();
        assert_eq!(susp.row_count(), 1, "原任务应 SUSPENDED 落库");

        let snap = e1.store().load_snapshot(&s.instance_id).await.unwrap();
        let temp_id = snap
            .tasks
            .iter()
            .find(|t| t.parent_task_id.is_some())
            .unwrap()
            .id
            .clone();
        (s.instance_id, task_id, temp_id)
    };

    // 进程 2（重启）：从库恢复 → 王五办结临时 → 原任务恢复 → 张三办结 → 完成。
    let mut e2 = Engine::new(store.clone());
    e2.deploy(def).unwrap();
    // 恢复的加签结构完整。
    let snap = e2.store().load_snapshot(&iid).await.unwrap();
    assert_eq!(
        snap.tasks.iter().filter(|t| !t.completed).count(),
        2,
        "重启后恢复原+临时两任务"
    );
    assert_eq!(
        snap.tasks
            .iter()
            .find(|t| t.id == task_id)
            .unwrap()
            .delegation_state
            .as_deref(),
        Some("SUSPENDED")
    );

    e2.complete_task(&iid, &temp_id, Variables::new())
        .await
        .unwrap();
    // 原任务恢复。
    let s2 = e2.store().load_snapshot(&iid).await.unwrap();
    assert!(
        s2.tasks
            .iter()
            .find(|t| t.id == task_id)
            .unwrap()
            .delegation_state
            .is_none()
    );
    let done = e2
        .complete_task(&iid, &task_id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);

    cleanup(&db_id, &iid).await;
}
