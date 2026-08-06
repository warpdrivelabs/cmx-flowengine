//! M5.2 端到端：PG 组织路由（#[ignore] 门控）。
//!
//! 运行（cmx 库，有 cmx_org；绑定表由 ensure_schema 建在同库）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-tests --test m5_2_pg -- --ignored --nocapture --test-threads=1
//!
//! 验证 PgSubflowRouter 三层解析：精确绑定 / 沿 cmx_org.path 向上继承 / 默认兜底。
//! 组织树：df_root(/df_root) → df_bj(/df_root/df_bj)；总部绑 fin_review_hq，北京不绑（应继承总部）。

use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager};
use cmx_flow_engine::SubflowRouter;
use cmx_flow_store_pg::{PgRuntimeStore, PgSubflowRouter};

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m52_test".to_string();
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

async fn seed(db_id: &str) {
    // 组织树：df_root（根）→ df_bj（北京，挂在总部下）。path 为物化路径。
    for sql in [
        // 先清掉本 key 的所有既有绑定，保证测试自隔离：共享库里 demo 也 seed 了 fin_review
        // 的默认绑定（org NULL → fin_review_hq），会与本测试的默认绑定在 LIMIT 1 里争抢。
        "DELETE FROM cmx_flow_subflow_binding WHERE called_key = 'fin_review'",
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_root','df_root','演示总部',NULL,'/df_root',0) ON CONFLICT (id) DO UPDATE SET path='/df_root'",
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_bj','df_bj','北京分公司','df_root','/df_root/df_bj',0) ON CONFLICT (id) DO UPDATE SET path='/df_root/df_bj'",
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_sh','df_sh','上海分公司','df_root','/df_root/df_sh',0) ON CONFLICT (id) DO UPDATE SET path='/df_root/df_sh'",
        // 绑定：总部绑 fin_review→hq；上海精确绑 fin_review→branch；默认（org NULL）绑 fin_review→default。
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_hq','fin_review','df_root','fin_review_hq',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_hq', enabled=TRUE",
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_sh','fin_review','df_sh','fin_review_branch',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_branch', enabled=TRUE",
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_def','fin_review',NULL,'fin_review_default',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_default', enabled=TRUE",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
}

async fn cleanup(db_id: &str) {
    for sql in [
        "DELETE FROM cmx_flow_subflow_binding WHERE id IN ('df_b_hq','df_b_sh','df_b_def')",
        "DELETE FROM cmx_org WHERE id IN ('df_root','df_bj','df_sh')",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL(cmx 库，有 cmx_org)，通过 TEST_PG_URL 提供"]
async fn pg_router_exact_inherit_default() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    // ensure_schema 建 cmx_flow_subflow_binding（与 cmx_org 同库）。
    PgRuntimeStore::new(&db_id)
        .ensure_schema()
        .await
        .expect("建表应成功");
    seed(&db_id).await;

    let router = PgSubflowRouter::new(&db_id);

    // 1) 精确：上海有自己的绑定 → fin_review_branch。
    let sh = router.resolve("fin_review", Some("df_sh")).await.unwrap();
    assert_eq!(sh, "fin_review_branch", "上海精确绑定");

    // 2) 继承：北京没绑，沿 path 向上找到总部 → fin_review_hq。
    let bj = router.resolve("fin_review", Some("df_bj")).await.unwrap();
    assert_eq!(bj, "fin_review_hq", "北京应继承总部绑定");

    // 3) 精确（总部自己）：df_root → fin_review_hq。
    let hq = router.resolve("fin_review", Some("df_root")).await.unwrap();
    assert_eq!(hq, "fin_review_hq", "总部精确绑定");

    // 4) 兜底：未知组织（不在树里）→ 默认绑定 fin_review_default。
    let unknown = router
        .resolve("fin_review", Some("no_such_org"))
        .await
        .unwrap();
    assert_eq!(unknown, "fin_review_default", "未知组织回退默认");

    // 5) 无组织 → 默认。
    let none = router.resolve("fin_review", None).await.unwrap();
    assert_eq!(none, "fin_review_default", "无组织回退默认");

    // 6) 无解：未知 key 且无默认 → 报错。
    let err = router.resolve("no_such_key", Some("df_bj")).await;
    assert!(err.is_err(), "未知 key 无绑定应报错");

    cleanup(&db_id).await;
}
