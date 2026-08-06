//! M5.1 端到端：PG 落库子流程（#[ignore] 门控）。
//!
//! 运行（fico 库即可）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-tests --test m5_1_pg -- --ignored --nocapture --test-threads=1
//!
//! 验证：主令牌 WAITING_SUBFLOW 落库、子实例 parent_instance_id/parent_token_id 落库、
//! find_child_instances 查子、子完成回写变量唤醒主、**用全新 Engine（模拟重启）**从库恢复后
//! 办结子任务仍能唤醒主流程。

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, RuntimeStore, Variables};
use cmx_flow_store_pg::PgRuntimeStore;
use serde_json::json;

const MAIN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_main" name="PG主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="call"/>
    <callActivity id="call" name="子流程" calledElement="pg_sub">
      <extensionElements>
        <flowable:in source="amount" target="subAmount"/>
        <flowable:out source="ok" target="ok"/>
      </extensionElements>
    </callActivity>
    <sequenceFlow id="s1" sourceRef="call" targetRef="cashier"/>
    <userTask id="cashier" name="出纳" flowable:assignee="出纳"/>
    <sequenceFlow id="s2" sourceRef="cashier" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

const SUB_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_sub" name="PG子流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="复核" flowable:assignee="财务"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m51_test".to_string();
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

async fn cleanup(db_id: &str, ids: &[String]) {
    for iid in ids {
        for sql in [
            format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{iid}'"),
            format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{iid}'"),
            format!("DELETE FROM cmx_flow_task WHERE instance_id = '{iid}'"),
            format!("DELETE FROM cmx_flow_token WHERE instance_id = '{iid}'"),
            format!("DELETE FROM cmx_flow_instance WHERE id = '{iid}'"),
        ] {
            let _ = cmx_database_pg::execute_sql(db_id, None, &sql).await;
        }
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_subflow_persists_and_recovers() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let main_def = compile(MAIN_BPMN).unwrap();
    let sub_def = compile(SUB_BPMN).unwrap();

    // —— 进程 1：起主流程 → 子实例落库，然后「退出」 —— //
    let (main_id, sub_id, review_task) = {
        let mut e1 = Engine::new(store.clone());
        e1.deploy(main_def.clone()).unwrap();
        e1.deploy(sub_def.clone()).unwrap();
        let mut vars = Variables::new();
        vars.set("amount", json!(88000));
        let started = e1
            .start_process("pg_main", vars, Some("PG-SUB-001".into()))
            .await
            .unwrap();
        let main_id = started.instance_id.clone();

        // 主令牌 WAITING_SUBFLOW 落库。
        let tok = query_sql(
            &db_id,
            None,
            &format!("SELECT state FROM cmx_flow_token WHERE instance_id = '{main_id}' AND state = 'WAITING_SUBFLOW'"),
            "tok",
        )
        .await
        .unwrap();
        assert_eq!(tok.row_count(), 1, "主令牌应以 WAITING_SUBFLOW 落库");

        // 子实例 parent 列落库。
        let children = e1.store().find_child_instances(&main_id).await.unwrap();
        assert_eq!(children.len(), 1);
        let sub_id = children[0].id.clone();
        assert_eq!(
            children[0].parent_instance_id.as_deref(),
            Some(main_id.as_str())
        );
        assert!(children[0].parent_token_id.is_some());
        // 输入变量映射落库。
        let sub_snap = e1.store().load_snapshot(&sub_id).await.unwrap();
        assert_eq!(
            sub_snap.instance.variables.get("subAmount"),
            Some(&json!(88000))
        );
        let review_task = sub_snap
            .tasks
            .iter()
            .find(|t| !t.completed)
            .unwrap()
            .id
            .clone();
        (main_id, sub_id, review_task)
    };

    // —— 进程 2（全新 Engine，模拟重启）：从库办结子任务 → 唤醒主流程 —— //
    let mut e2 = Engine::new(store.clone());
    e2.deploy(main_def).unwrap();
    e2.deploy(sub_def).unwrap();

    let mut out = Variables::new();
    out.set("ok", json!(true));
    e2.complete_task(&sub_id, &review_task, out).await.unwrap();

    // 子完成。
    assert_eq!(
        e2.store()
            .load_snapshot(&sub_id)
            .await
            .unwrap()
            .instance
            .state,
        InstanceState::Completed
    );
    // 主被唤醒 → 出纳打款 + 回写变量。
    let main_after = e2.store().load_snapshot(&main_id).await.unwrap();
    assert_eq!(main_after.instance.state, InstanceState::Active);
    let open: Vec<&str> = main_after
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["cashier"], "重启后子完成仍能唤醒主流程");
    assert_eq!(
        main_after.instance.variables.get("ok"),
        Some(&json!(true)),
        "输出变量回写"
    );

    // 出纳办结 → 主完成。
    let cashier = main_after
        .tasks
        .iter()
        .find(|t| !t.completed)
        .unwrap()
        .id
        .clone();
    let done = e2
        .complete_task(&main_id, &cashier, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);

    cleanup(&db_id, &[main_id, sub_id]).await;
}
