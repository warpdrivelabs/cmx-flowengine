//! M4.2 端到端：PG 落库抄送（#[ignore] 门控）。
//!
//! 运行（用 fico 库即可：cc 用 user() 引用，解析不依赖 IAM 角色表）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-tests --test m4_2_pg -- --ignored --nocapture --test-threads=1
//!
//! 验证：节点抄送落 cmx_flow_cc、find_cc_for_user 跨实例查、mark_cc_read 置已读、
//! 实例完成后抄送记录仍可查（未被清理）。

use std::sync::Arc;

use async_trait::async_trait;
use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, Engine, ResolveResult, Variables,
};
use cmx_flow_store_pg::PgRuntimeStore;

/// 直通解析器：user() → 自身；其它 → 空（本 PG 测试只用 user 引用，不碰 IAM）。
struct UserOnlyResolver;
#[async_trait]
impl AssigneeResolver for UserOnlyResolver {
    async fn resolve(&self, c: &CandidateRef) -> ResolveResult<Vec<String>> {
        if c.kind == CandidateKind::User {
            Ok(vec![c.value.clone()])
        } else {
            Ok(vec![])
        }
    }
}

const CC_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="pg_cc_approve" name="PG抄送审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="经理审批" flowable:assignee="mgr" cmx:cc="user(u_boss_a), user(u_boss_b)"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m42_test".to_string();
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
        format!("DELETE FROM cmx_flow_cc WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_task_candidate WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_job WHERE instance_id = '{instance_id}'"),
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
async fn pg_node_cc_persists_and_queryable() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let def = compile(CC_BPMN).unwrap();
    let mut engine = Engine::new(store);
    engine.set_resolver(Arc::new(UserOnlyResolver));
    engine.deploy(def).unwrap();

    let started = engine
        .start_process("pg_cc_approve", Variables::new(), Some("PG-CC-001".into()))
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();

    // 办结 → 抄送生成 + 流程完成。
    let done = engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, cmx_flow_engine::InstanceState::Completed);

    // 抄送记录落库（两条）。
    let cc = query_sql(
        &db_id,
        None,
        &format!(
            "SELECT id, to_user_id, read_at FROM cmx_flow_cc WHERE instance_id = '{}'",
            started.instance_id
        ),
        "cc",
    )
    .await
    .expect("查询抄送失败");
    assert_eq!(cc.row_count(), 2, "应落 2 条抄送");

    // find_cc_for_user：u_boss_a 有 1 条未读。
    let inbox = engine.cc_for_user("u_boss_a", true, 50).await.unwrap();
    assert_eq!(inbox.len(), 1, "u_boss_a 有 1 条未读抄送");
    assert_eq!(inbox[0].definition_key, "pg_cc_approve");
    assert_eq!(inbox[0].business_key.as_deref(), Some("PG-CC-001"));
    let cc_id = inbox[0].id.clone();

    // 标记已读 → 未读为 0。
    assert!(engine.mark_cc_read(&cc_id).await.unwrap());
    assert_eq!(
        engine
            .cc_for_user("u_boss_a", true, 50)
            .await
            .unwrap()
            .len(),
        0
    );

    // 库中该记录 read_at 已置位。
    let read = query_sql(
        &db_id,
        None,
        &format!("SELECT read_at FROM cmx_flow_cc WHERE id = '{cc_id}' AND read_at IS NOT NULL"),
        "read",
    )
    .await
    .unwrap();
    assert_eq!(read.row_count(), 1, "read_at 应已置位");

    cleanup(&db_id, &started.instance_id).await;
}
