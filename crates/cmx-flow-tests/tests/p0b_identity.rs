//! P0-b 端到端：内建身份模块（fid_* 表）—— 建表 + CRUD + 关系型解析（#[ignore] 门控）。
//!
//! 运行（用任意可写 PG 库即可，本模块自建 fid_* 表，不碰 cmx_* / cr_* / cv_*）：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-tests --test p0b_identity -- --ignored --nocapture --test-threads=1
//!
//! 验证：ensure_schema 建 6 张 fid_* 表；org/role/user CRUD 往返；LocalAssigneeResolver 解析
//! role → 用户、org → 子树用户、关系型（部门领导/发起人上级/本人）→ 正确用户。

use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager};
use cmx_flow_identity::{Entity, IdentityStore, LocalAssigneeResolver};
use cmx_flow_model::{AssigneeResolver, CandidateKind, CandidateRef, ResolveContext};
use serde_json::json;

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_p0b_identity_test".to_string();
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

/// 清掉测试数据（本测试用固定 id 前缀 p0b_）。
async fn cleanup(db_id: &str) {
    for sql in [
        "DELETE FROM fid_user_role WHERE user_id LIKE 'p0b_%'",
        "DELETE FROM fid_user_position WHERE user_id LIKE 'p0b_%'",
        "DELETE FROM fid_user WHERE id LIKE 'p0b_%'",
        "DELETE FROM fid_role WHERE id LIKE 'p0b_%'",
        "DELETE FROM fid_position WHERE id LIKE 'p0b_%'",
        "DELETE FROM fid_org WHERE id LIKE 'p0b_%'",
    ] {
        let _ = execute_sql(db_id, None, sql).await;
    }
}

#[tokio::test]
#[ignore = "需要 TEST_PG_URL"]
async fn identity_schema_crud_and_relationship_resolution() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = IdentityStore::new(&db_id);
    store.ensure_schema().await.expect("建表失败");
    cleanup(&db_id).await;

    // —— 1) 组织：财务部（领导 = p0b_leader），子部门 应付组 —— //
    store
        .upsert(
            Entity::Org,
            &json!({"id":"p0b_fin","code":"P0BFIN","name":"财务部","leaderUserId":"p0b_leader"}),
        )
        .await
        .expect("建组织失败");
    // 手工修 path 便于子树匹配（upsert 简化写 /id，这里让子部门 path 以父为前缀）。
    execute_sql(&db_id, None, "UPDATE fid_org SET path='/p0b_fin' WHERE id='p0b_fin'")
        .await
        .unwrap();
    store
        .upsert(
            Entity::Org,
            &json!({"id":"p0b_ap","code":"P0BAP","name":"应付组","parentId":"p0b_fin"}),
        )
        .await
        .expect("建子组织失败");
    execute_sql(&db_id, None, "UPDATE fid_org SET path='/p0b_fin/p0b_ap' WHERE id='p0b_ap'")
        .await
        .unwrap();

    // —— 2) 角色 + 用户 —— //
    store
        .upsert(Entity::Role, &json!({"id":"p0b_role_fin","code":"finance","name":"财务角色"}))
        .await
        .expect("建角色失败");
    // 领导（挂财务部）、职员（挂应付子组）。
    store
        .upsert(Entity::User, &json!({"id":"p0b_leader","username":"leader","name":"李领导","orgId":"p0b_fin"}))
        .await
        .expect("建领导失败");
    store
        .upsert(Entity::User, &json!({"id":"p0b_staff","username":"staff","name":"王职员","orgId":"p0b_ap"}))
        .await
        .expect("建职员失败");
    // 职员挂 finance 角色。
    store
        .set_user_roles("p0b_staff", &["p0b_role_fin".to_string()])
        .await
        .expect("设角色失败");

    // —— 3) CRUD 读回校验 —— //
    let orgs = store.list(Entity::Org).await.expect("列组织失败");
    assert!(orgs.iter().any(|o| o["id"] == "p0b_fin"), "组织应含财务部");
    let users = store.list(Entity::User).await.expect("列用户失败");
    assert!(users.iter().any(|u| u["id"] == "p0b_staff"), "用户应含职员");

    // —— 4) LocalAssigneeResolver 解析 —— //
    let r = LocalAssigneeResolver::new(&db_id);

    // role(finance) → 职员
    let by_role = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
        .await
        .unwrap();
    assert!(by_role.contains(&"p0b_staff".to_string()), "role(finance) 应含职员，实际 {by_role:?}");

    // org(p0b_fin) → 子树全用户（含子组的职员 + 领导）
    let by_org = r
        .resolve(&CandidateRef { kind: CandidateKind::Org, value: "p0b_fin".into() })
        .await
        .unwrap();
    assert!(by_org.contains(&"p0b_staff".to_string()), "org 子树应含职员");
    assert!(by_org.contains(&"p0b_leader".to_string()), "org 子树应含领导");

    // orgLeader(p0b_fin) → 领导
    let ol = r
        .resolve(&CandidateRef { kind: CandidateKind::OrgLeader, value: "p0b_fin".into() })
        .await
        .unwrap();
    assert_eq!(ol, vec!["p0b_leader"], "部门领导应为 p0b_leader");

    // 关系型 with context：发起人=职员 → 发起人本人 = 职员
    let ctx = ResolveContext::new(Some("p0b_staff".into()), Some("p0b_ap".into()));
    let init = r
        .resolve_with(&CandidateRef { kind: CandidateKind::Initiator, value: String::new() }, &ctx)
        .await
        .unwrap();
    assert_eq!(init, vec!["p0b_staff"], "发起人本人");

    // 发起人上级 = 职员所属组织(应付组)的领导。应付组自身无 leader，故为空——
    // 验证「组织无领导时返空」这一宽容语义。
    let il = r
        .resolve_with(&CandidateRef { kind: CandidateKind::InitiatorLeader, value: String::new() }, &ctx)
        .await
        .unwrap();
    assert!(il.is_empty(), "应付组无领导 → 发起人上级为空，实际 {il:?}");

    // 给应付组设领导后再解析发起人上级 = 该领导。
    execute_sql(&db_id, None, "UPDATE fid_org SET leader_user_id='p0b_leader' WHERE id='p0b_ap'")
        .await
        .unwrap();
    let il2 = r
        .resolve_with(&CandidateRef { kind: CandidateKind::InitiatorLeader, value: String::new() }, &ctx)
        .await
        .unwrap();
    assert_eq!(il2, vec!["p0b_leader"], "应付组领导 = p0b_leader");

    // —— 5) 软删除 —— //
    store.delete(Entity::User, "p0b_staff").await.expect("删用户失败");
    let users2 = store.list(Entity::User).await.expect("列用户失败");
    assert!(!users2.iter().any(|u| u["id"] == "p0b_staff"), "软删后不应再列出职员");

    cleanup(&db_id).await;
    println!("✅ P0-b 内建身份：建表 + CRUD + role/org/关系型解析 全通过");
}
