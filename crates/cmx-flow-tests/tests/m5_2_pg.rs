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
        // 绑定：总部绑 fin_review→hq；上海精确绑 fin_review→branch；默认（dim_value NULL）绑 fin_review→default。
        // RD0：绑定维度用 (dim_key='org', dim_value=组织id)；org_id 列保留镜像（兼容）。
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, dim_key, dim_value, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_hq','fin_review','df_root','org','df_root','fin_review_hq',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_hq', enabled=TRUE, dim_key='org', dim_value='df_root'",
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, dim_key, dim_value, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_sh','fin_review','df_sh','org','df_sh','fin_review_branch',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_branch', enabled=TRUE, dim_key='org', dim_value='df_sh'",
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, dim_key, dim_value, target_definition_key, enabled, created_at, updated_at) VALUES ('df_b_def','fin_review',NULL,'org',NULL,'fin_review_default',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_default', enabled=TRUE, dim_key='org', dim_value=NULL",
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
    let sh = router.resolve("fin_review", "org", Some("df_sh")).await.unwrap();
    assert_eq!(sh, "fin_review_branch", "上海精确绑定");

    // 2) 继承：北京没绑，沿 path 向上找到总部 → fin_review_hq。
    let bj = router.resolve("fin_review", "org", Some("df_bj")).await.unwrap();
    assert_eq!(bj, "fin_review_hq", "北京应继承总部绑定");

    // 3) 精确（总部自己）：df_root → fin_review_hq。
    let hq = router.resolve("fin_review", "org", Some("df_root")).await.unwrap();
    assert_eq!(hq, "fin_review_hq", "总部精确绑定");

    // 4) 兜底：未知组织（不在树里）→ 默认绑定 fin_review_default。
    let unknown = router
        .resolve("fin_review", "org", Some("no_such_org"))
        .await
        .unwrap();
    assert_eq!(unknown, "fin_review_default", "未知组织回退默认");

    // 5) 无组织 → 默认。
    let none = router.resolve("fin_review", "org", None).await.unwrap();
    assert_eq!(none, "fin_review_default", "无组织回退默认");

    // 6) 无解：未知 key 且无默认 → 报错。
    let err = router.resolve("no_such_key", "org", Some("df_bj")).await;
    assert!(err.is_err(), "未知 key 无绑定应报错");

    cleanup(&db_id).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// RD1：任意自分级字典维度的泛化继承（对标 DCT cf_* 的点分 full_path）。
// 建一张小自分级表 rd1_dim（点分路径、code 段、无前导），注册 DimSpec，验证「沿该维度字典
// 物化路径向上继承」与组织维度同构——只是分隔符从 '/' 换成 '.'。
// ─────────────────────────────────────────────────────────────────────────────

async fn seed_rd1(db_id: &str) {
    for sql in [
        // 自分级维度字典（模拟 cf_legal_entity）：full_path 点分 code 段、无前导。
        "CREATE TABLE IF NOT EXISTS rd1_dim (id VARCHAR(64) PRIMARY KEY, full_path VARCHAR(512) NOT NULL)",
        "DELETE FROM rd1_dim WHERE id IN ('LE_CN','LE_CN_EAST')",
        "INSERT INTO rd1_dim (id, full_path) VALUES ('LE_CN','GROUP.CN')",
        "INSERT INTO rd1_dim (id, full_path) VALUES ('LE_CN_EAST','GROUP.CN.CN_EAST')",
        // 只给中国区(LE_CN)配绑定；华东(LE_CN_EAST，其下级)应继承之。dim_key='legal_entity'。
        "DELETE FROM cmx_flow_subflow_binding WHERE called_key = 'fin_review_le'",
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, dim_key, dim_value, target_definition_key, enabled, created_at, updated_at) VALUES ('rd1_b_cn','fin_review_le','legal_entity','LE_CN','fin_review_cn',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET dim_value='LE_CN', target_definition_key='fin_review_cn', enabled=TRUE",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
}

async fn cleanup_rd1(db_id: &str) {
    for sql in [
        "DELETE FROM cmx_flow_subflow_binding WHERE called_key = 'fin_review_le'",
        "DROP TABLE IF EXISTS rd1_dim",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL(cmx 库)，通过 TEST_PG_URL 提供"]
async fn pg_router_generalized_dim_inheritance() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    PgRuntimeStore::new(&db_id).ensure_schema().await.expect("建表应成功");
    seed_rd1(&db_id).await;

    // 注册 legal_entity 维度规格：表 rd1_dim、pk id、路径列 full_path、点分隔。
    let mut router = PgSubflowRouter::new(&db_id);
    router.register_dim(
        "legal_entity",
        cmx_flow_store_pg::DimSpec {
            table: "rd1_dim".into(),
            id_col: "id".into(),
            path_col: "full_path".into(),
            delim: ".".into(),
        },
    );

    // 1) 精确：中国区自己有绑定 → fin_review_cn。
    let cn = router.resolve("fin_review_le", "legal_entity", Some("LE_CN")).await.unwrap();
    assert_eq!(cn, "fin_review_cn", "中国区精确绑定");

    // 2) 继承：华东没绑，沿点分 full_path 向上找到中国区 → fin_review_cn（与组织维度同构）。
    let east = router.resolve("fin_review_le", "legal_entity", Some("LE_CN_EAST")).await.unwrap();
    assert_eq!(east, "fin_review_cn", "华东应沿 full_path 继承中国区绑定");

    // 3) 边界 bug 验证：另建一个 code 前缀相近但非真祖先的节点，不应错配。
    //    LE_CN 的 full_path=GROUP.CN；构造 GROUP.CNX（前缀 'GROUP.CN' 但下一段非分隔）。
    let _ = execute_sql(&db_id, None, "INSERT INTO rd1_dim (id, full_path) VALUES ('LE_CNX','GROUP.CNX') ON CONFLICT (id) DO NOTHING").await;
    let cnx = router.resolve("fin_review_le", "legal_entity", Some("LE_CNX")).await;
    // GROUP.CNX 不是 GROUP.CN 的下级（追加分隔符 '.' 后 'GROUP.CNX' 不匹配 'GROUP.CN.%'）→ 无解。
    assert!(cnx.is_err(), "GROUP.CNX 不应错配 GROUP.CN 的绑定（边界 bug 已修）");
    let _ = execute_sql(&db_id, None, "DELETE FROM rd1_dim WHERE id='LE_CNX'").await;

    cleanup_rd1(&db_id).await;
}
