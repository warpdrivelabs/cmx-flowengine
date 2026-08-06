//! M2 端到端：PG 落库（并行网关 + 历史归档，#[ignore] 门控）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-tests --test m2_pg -- --ignored --nocapture

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, Variables};
use cmx_flow_model::{RuntimeStore, TokenState};
use cmx_flow_store_pg::PgRuntimeStore;

const PARALLEL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_parallel_sign" name="PG并行会签" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="finance"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="legal"/>
    <userTask id="finance" name="财务审批" flowable:assignee="cfo"/>
    <userTask id="legal" name="法务审批" flowable:assignee="counsel"/>
    <sequenceFlow id="s3" sourceRef="finance" targetRef="join"/>
    <sequenceFlow id="s4" sourceRef="legal" targetRef="join"/>
    <parallelGateway id="join"/>
    <sequenceFlow id="s5" sourceRef="join" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_test".to_string();
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

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_parallel_fork_join_and_history() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");

    let def = compile(PARALLEL_BPMN).unwrap();
    let mut engine = Engine::new(store);
    engine.deploy(def).unwrap();

    // 启动 → fork 出两个并行任务。
    let started = engine
        .start_process(
            "pg_parallel_sign",
            Variables::new(),
            Some("PG-PAR-001".into()),
        )
        .await
        .expect("启动应成功");
    assert_eq!(started.open_tasks.len(), 2, "fork 应产生 2 个并行任务");

    // 办结财务 → 该分支阻塞在 join（Joining），实例仍 Active。
    let finance = started
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "finance")
        .unwrap()
        .clone();
    let mid = engine
        .complete_task(&started.instance_id, &finance.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(mid.state, InstanceState::Active);

    // 复核：PG 中确有一个 JOINING 令牌。
    let snap_mid = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .unwrap();
    assert_eq!(
        snap_mid
            .tokens
            .iter()
            .filter(|t| t.state == TokenState::Joining)
            .count(),
        1,
        "财务分支应作为 JOINING 令牌落库"
    );

    // 办结法务 → 合流 → 完成。
    let legal = mid
        .open_tasks
        .iter()
        .find(|t| t.node_bpmn_id == "legal")
        .unwrap()
        .clone();
    let done = engine
        .complete_task(&started.instance_id, &legal.id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);

    // 运行态：合流后仅剩一个 Ended 令牌。
    let snap_final = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .unwrap();
    assert_eq!(snap_final.tokens.len(), 1);
    assert_eq!(snap_final.tokens[0].state, TokenState::Ended);

    // 历史归档：hi_instance 应有该实例（COMPLETED），hi_task 应有两条办结任务。
    let hi_inst = query_sql(
        &db_id,
        None,
        "SELECT id, state, duration_ms FROM cmx_flow_hi_instance WHERE business_key = 'PG-PAR-001'",
        "hi_inst",
    )
    .await
    .expect("查询历史实例失败");
    assert_eq!(hi_inst.row_count(), 1, "完成的实例应归档到 hi_instance");

    let hi_tasks = query_sql(
        &db_id,
        None,
        &format!(
            "SELECT id FROM cmx_flow_hi_task WHERE instance_id = '{}'",
            started.instance_id
        ),
        "hi_task",
    )
    .await
    .expect("查询历史任务失败");
    assert_eq!(hi_tasks.row_count(), 2, "两个办结任务应归档到 hi_task");

    // 清理，保持可重复运行。
    for sql in [
        format!(
            "DELETE FROM cmx_flow_hi_task WHERE instance_id = '{}'",
            started.instance_id
        ),
        format!(
            "DELETE FROM cmx_flow_hi_instance WHERE id = '{}'",
            started.instance_id
        ),
        format!(
            "DELETE FROM cmx_flow_task WHERE instance_id = '{}'",
            started.instance_id
        ),
        format!(
            "DELETE FROM cmx_flow_token WHERE instance_id = '{}'",
            started.instance_id
        ),
        format!(
            "DELETE FROM cmx_flow_instance WHERE id = '{}'",
            started.instance_id
        ),
    ] {
        let _ = cmx_database_pg::execute_sql(&db_id, None, &sql).await;
    }
}
