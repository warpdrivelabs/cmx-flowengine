//! M4.1 端到端：PG 落库 + IAM 候选人解析（#[ignore] 门控）。
//!
//! 运行（注意：IAM 表 cmx_role/cmx_user_role 在 **cmx** 库，故指向 cmx 而非 fico）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-tests --test m4_1_pg -- --ignored --nocapture --test-threads=1
//!
//! 前置：先在 cmx 库跑 docs/sql/migrations/20260718_001_cmx_flow_identity.up.sql（建候选人池等表）。
//!
//! 验证 PgIamAssigneeResolver 真的从 IAM 表解析用户：
//! - 播种 cmx_role + cmx_user_role（角色→多用户）
//! - role(finance) 解析出 2 人 → 落候选池 → claim → 办结
//! - 候选记录正确落 cmx_flow_task_candidate

use std::sync::Arc;

use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, Variables};
use cmx_flow_store_pg::{PgIamAssigneeResolver, PgRuntimeStore};

const ROLE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_role_approve" name="PG角色审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="财务审批" flowable:candidateGroups="m4_finance"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m41_test".to_string();
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

/// 播种一个角色 + 两个用户 + 两条用户角色关联（幂等：先删后插）。
async fn seed_iam(db_id: &str) {
    // 清理旧种子。
    for sql in [
        "DELETE FROM cmx_user_role WHERE role_id = 'm4_role_fin'",
        "DELETE FROM cmx_role WHERE id = 'm4_role_fin'",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
    // 角色。
    let _ = execute_sql(
        db_id,
        None,
        "INSERT INTO cmx_role (id, code, name, archived) VALUES ('m4_role_fin','m4_finance','财务角色',0) \
         ON CONFLICT (id) DO NOTHING",
    )
    .await;
    // 用户角色关联（用户表可能已有 u_ma/u_mb，无则关联仍能被解析器 JOIN 出——这里只需关联行）。
    for (id, uid) in [("m4_ur_a", "u_ma"), ("m4_ur_b", "u_mb")] {
        let _ = execute_sql(
            db_id,
            None,
            &format!(
                "INSERT INTO cmx_user_role (id, user_id, role_id, archived) \
                 VALUES ('{id}','{uid}','m4_role_fin',0) ON CONFLICT (id) DO NOTHING"
            ),
        )
        .await;
    }
}

async fn cleanup(db_id: &str, instance_id: &str) {
    for sql in [
        format!("DELETE FROM cmx_flow_task_candidate WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_job WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_mi_scope WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_token WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_instance WHERE id = '{instance_id}'"),
    ] {
        let _ = execute_sql(db_id, None, &sql).await;
    }
    // 清理 IAM 种子。
    let _ = execute_sql(
        db_id,
        None,
        "DELETE FROM cmx_user_role WHERE role_id = 'm4_role_fin'",
    )
    .await;
    let _ = execute_sql(db_id, None, "DELETE FROM cmx_role WHERE id = 'm4_role_fin'").await;
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_role_resolves_to_candidate_pool_and_claim() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    seed_iam(&db_id).await;

    let def = compile(ROLE_BPMN).unwrap();
    let mut engine = Engine::new(store);
    engine.set_resolver(Arc::new(PgIamAssigneeResolver::new(&db_id)));
    engine.deploy(def).unwrap();

    // 启动 → role(m4_finance) 解析出 2 人 → 候选池。
    let started = engine
        .start_process(
            "pg_role_approve",
            Variables::new(),
            Some("PG-M41-001".into()),
        )
        .await
        .expect("启动应成功");
    assert_eq!(started.open_tasks.len(), 1);
    assert!(started.open_tasks[0].assignee.is_none(), "多人应待认领");
    let task_id = started.open_tasks[0].id.clone();

    // 候选记录落库校验。
    let cands = query_sql(
        &db_id,
        None,
        &format!(
            "SELECT resolved_user_id FROM cmx_flow_task_candidate WHERE instance_id = '{}'",
            started.instance_id
        ),
        "cands",
    )
    .await
    .expect("查询候选失败");
    assert_eq!(cands.row_count(), 2, "role 应解析出 2 个候选人");

    // u_ma 认领 → 办结 → 完成。
    let claimed = engine
        .claim_task(&started.instance_id, &task_id, "u_ma")
        .await
        .unwrap();
    assert_eq!(claimed.open_tasks[0].assignee.as_deref(), Some("u_ma"));

    // 认领后候选池清空（库中亦无）。
    let remain = query_sql(
        &db_id,
        None,
        &format!(
            "SELECT id FROM cmx_flow_task_candidate WHERE instance_id = '{}'",
            started.instance_id
        ),
        "remain",
    )
    .await
    .unwrap();
    assert_eq!(remain.row_count(), 0, "认领后候选记录应清空");

    let done = engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();
    assert_eq!(done.state, InstanceState::Completed);

    cleanup(&db_id, &started.instance_id).await;
}
