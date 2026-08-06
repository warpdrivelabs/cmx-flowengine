//! M1 端到端：PG 落库全链路（#[ignore] 门控，需本地 PG）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-tests --test m1_pg -- --ignored --nocapture
//!
//! 验证：引擎 + PgRuntimeStore 组合下，start/complete 的每个等待态提交点都真实落库，
//! 跨「重新 load」后仍能恢复推进，最终实例 Completed。

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{DelegateContext, Engine, InstanceState, JavaDelegate, Variables};
use cmx_flow_store_pg::PgRuntimeStore;
use cmx_flow_tests::LEAVE_REQUEST_BPMN;
use serde_json::json;

struct CalcDaysDelegate;

#[async_trait::async_trait]
impl JavaDelegate for CalcDaysDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String> {
        let hours = ctx
            .variables
            .get("hours")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        ctx.variables
            .set("days", json!((hours / 8.0).ceil() as i64));
        Ok(())
    }
}

/// 注册测试数据源到 cmx-database-pg 全局 manager，返回 db_id。
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
        .expect("注册测试数据源失败");
    Some(db_id)
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_full_lifecycle_long_leave() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };

    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");

    let def = compile(LEAVE_REQUEST_BPMN).unwrap();
    let mut engine = Engine::new(store);
    engine.deploy(def).unwrap();
    engine.register_delegate("calcDaysDelegate", CalcDaysDelegate);

    // 长假：hours=40 → days=5 → 走总监分支，两级审批。
    let mut vars = Variables::new();
    vars.set("hours", json!(40));
    let started = engine
        .start_process("leave_request", vars, Some("PG-LR-001".into()))
        .await
        .expect("启动应成功");
    assert_eq!(started.state, InstanceState::Active);
    let review = started.open_tasks[0].clone();
    assert_eq!(review.node_bpmn_id, "review");

    // 经理办结（此步会 load→推进→save，全程 PG）。
    let after_mgr = engine
        .complete_task(&started.instance_id, &review.id, Variables::new())
        .await
        .expect("经理办结应成功");
    assert_eq!(after_mgr.state, InstanceState::Active);
    let director = after_mgr.open_tasks[0].clone();
    assert_eq!(director.node_bpmn_id, "director");

    // 总监办结 → 完成。
    let done = engine
        .complete_task(&after_mgr.instance_id, &director.id, Variables::new())
        .await
        .expect("总监办结应成功");
    assert_eq!(done.state, InstanceState::Completed);
    assert!(done.open_tasks.is_empty());

    // 直接从 store 复核最终落库状态。
    use cmx_flow_model::{RuntimeStore, TokenState};
    let snap = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .expect("最终快照应可载入");
    assert_eq!(snap.instance.state, InstanceState::Completed);
    assert!(snap.tokens.iter().all(|t| t.state == TokenState::Ended));
    assert_eq!(
        snap.instance.variables.get("days").and_then(|v| v.as_i64()),
        Some(5)
    );
}
