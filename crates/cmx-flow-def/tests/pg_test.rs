//! PgDefinitionStore PG 门控实测（#[ignore]，需 TEST_PG_URL）。
//!
//! 运行（fico 库）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-def --test pg_test -- --ignored --nocapture --test-threads=1
//!
//! 验证真机：建表 → 存草稿 → 发布 v1 → 再发布 v2 → load_published 取到 v2 → 清理。

use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager};
use cmx_flow_def::{DefinitionService, PgDefinitionStore};

const KEY: &str = "pgtest_leave_request";

const BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pgtest_leave_request" name="请假申请" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="经理审批" flowable:assignee="mgr"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_def_test".to_string();
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

async fn cleanup(db_id: &str) {
    let _ = execute_sql(
        db_id,
        None,
        &format!("DELETE FROM cmx_flow_definition_version WHERE def_key = '{KEY}'"),
    )
    .await;
    let _ = execute_sql(
        db_id,
        None,
        &format!("DELETE FROM cmx_flow_definition WHERE key = '{KEY}'"),
    )
    .await;
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL(fico 库)，通过 TEST_PG_URL 提供"]
async fn pg_draft_publish_load_roundtrip() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let svc = DefinitionService::new(PgDefinitionStore::new(&db_id));
    svc.ensure_schema().await.expect("建表应成功");
    cleanup(&db_id).await; // 清掉上次残留，自隔离

    // 存草稿 → key 取自 process id。
    let rec = svc
        .save_draft(
            "请假申请",
            Some("fi".into()),
            Some("cmxfico".into()),
            Some("hr".into()),
            None,
            BPMN,
            Some("tester".into()),
        )
        .await
        .expect("存草稿应成功");
    assert_eq!(rec.key, KEY);

    // 发布 v1。
    let v1 = svc
        .publish(KEY, None, Some("tester".into()))
        .await
        .expect("发布应成功");
    assert_eq!(v1, 1);

    // 再存草稿 + 再发布 v2。
    svc.save_draft("请假申请v2", None, None, None, None, BPMN, None)
        .await
        .unwrap();
    let v2 = svc.publish(KEY, None, None).await.unwrap();
    assert_eq!(v2, 2);

    // 主记录已发布，active_version = 2。
    let got = svc.get(KEY).await.unwrap().unwrap();
    assert_eq!(got.active_version, Some(2));

    // load_published 取到 v2 且可编译。
    let (defs, errors) = svc.load_published_definitions().await.unwrap();
    assert!(errors.is_empty(), "编译无错");
    let mine = defs.iter().find(|d| d.key == KEY).expect("应装载到本定义");
    assert_eq!(mine.key, KEY);

    // 历史版本 v1 仍在。
    assert!(
        svc.get_version(KEY, 1).await.unwrap().is_some(),
        "旧版本保留"
    );

    cleanup(&db_id).await;
}
