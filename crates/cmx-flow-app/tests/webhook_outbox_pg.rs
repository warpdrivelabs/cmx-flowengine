//! 出站 webhook 订阅/投递存储层 + 投递链路集成测试（#[ignore] 门控，需本地 PG）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-app --test webhook_outbox_pg -- --ignored --nocapture
//!
//! 覆盖（001 方案 §九）：uk 幂等（同事件只落一行）、租约抢占互斥与持有者守卫、
//! 租约过期自愈重抢、同订阅保序（退避阻塞 / 终态不阻塞）、退避→DEAD→retry/skip/purge、
//! 首启 env 导入幂等。每测试用独立租户 + 独立订阅，互不干扰。

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{
    DbConfig, DbType, execute_sql, get_default_pg_db_manager,
};
use serde_json::json;

use cmx_flow_app::webhook_store::{self, DeliveryInsert, DlvFilter, SubFilter, SubUpsert};

const TEST_TENANT: &str = "wh-it";

/// 注册测试数据源（TEST_PG_URL 未设 → None，调用方跳过）。
async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_wh_test".to_string();
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
    manager.register_data_source(cfg).await.expect("注册测试数据源失败");
    Some(db_id)
}

/// 建表 + 清空本测试租户的旧数据（幂等重跑）。
async fn fresh(db_id: &str) {
    webhook_store::ensure_schema(db_id).await.expect("建表失败");
    for t in [
        "DELETE FROM cmx_flow_webhook_delivery WHERE subscription_id IN \
         (SELECT id FROM cmx_flow_webhook_subscription WHERE tenant_id = $1)",
        "DELETE FROM cmx_flow_webhook_subscription WHERE tenant_id = $1",
    ] {
        execute_sql_with_params_wrap(db_id, t, TEST_TENANT).await;
    }
}

async fn execute_sql_with_params_wrap(db_id: &str, sql: &str, tenant: &str) {
    use cmx_database_pg::{SqlParams, execute_sql_with_params};
    let params = SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]);
    execute_sql_with_params(db_id, None, sql, params).await.expect("清理旧数据失败");
}

async fn upsert_test_sub(db_id: &str, name: &str) -> i64 {
    webhook_store::upsert_subscription(
        db_id,
        TEST_TENANT,
        &SubUpsert {
            id: None,
            name: name.to_string(),
            channel: "webhook".into(),
            channel_config: json!({
                "service_key": "mdm",
                "callback_path": "/api/mdm/flow/callback",
                "secret": "it-secret",
            }),
            definition_keys: vec![],
            event_types: vec![],
            active: true,
            retry_max: 3,
            created_by: Some("it".into()),
        },
    )
    .await
    .expect("建订阅失败")
}

fn delivery(sub_id: i64, sub_name: &str, event_id: &str, instance: &str) -> DeliveryInsert {
    DeliveryInsert {
        subscription_id: sub_id,
        subscription_name: sub_name.to_string(),
        channel: "webhook".into(),
        event_id: event_id.to_string(),
        delivery_id: format!("{instance}-t1-{event_id}"),
        source: "emit",
        event_type: "instance.started".into(),
        definition_key: Some("it_def".into()),
        business_key: None,
        instance_id: instance.to_string(),
        payload: json!({ "event": "instance.started", "instanceId": instance }),
        initial_state: "PENDING",
        last_error: None,
        last_http_status: None,
        last_response_snippet: None,
        delivered: false,
    }
}

/// 直接改一行的状态/租约（测试 manipulations 用）。
async fn exec(db_id: &str, sql: &str) {
    execute_sql(db_id, None, sql).await.expect("执行 SQL 失败");
}

/// uk 幂等：同 (subscription_id, event_id) 重复写入只落一行；claim → DONE 全链路；
/// 同租约持有期内二次抢占互斥。
#[tokio::test]
#[ignore]
async fn uk_dedup_and_claim_chain() {
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-dedup").await;

    // 重复 emit 同一 event_id：uk 幂等，第二次被吞。
    let row = delivery(sub, "it-dedup", "evt-1", "i-1");
    let n1 = webhook_store::insert_deliveries(&db, &[row.clone()]).await.unwrap();
    let n2 = webhook_store::insert_deliveries(&db, &[row]).await.unwrap();
    assert_eq!((n1, n2), (1, 0), "同事件重复 emit 应被 uk 吞");

    // 抢占：attempts +1，租约打上。
    let claimed = webhook_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 1);

    // 租约持有期内二次抢占互斥（另一 worker 拿不到同一行）。
    let again = webhook_store::claim_due_deliveries(&db, "w2", 120, 10).await.unwrap();
    assert!(again.is_empty(), "租约有效期内不得重抢");

    // 持有者守卫：非持有者落结果 0 行命中；持有者成功。
    assert!(!webhook_store::finish_done(&db, claimed[0].id, "w2").await.unwrap());
    assert!(webhook_store::finish_done(&db, claimed[0].id, "w1").await.unwrap());

    let (rows, total) =
        webhook_store::query_deliveries(&db, TEST_TENANT, &DlvFilter::default()).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0]["state"], json!("DONE"));
}

/// 同订阅保序（决议 16）：退避等待阻塞后续；终态（DEAD）不阻塞。
#[tokio::test]
#[ignore]
async fn ordering_guard_backoff_and_terminal() {
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-order").await;

    // 两行同订阅：row1 退避未到期、row2 到期可投。
    webhook_store::insert_deliveries(
        &db,
        &[delivery(sub, "it-order", "e-1", "i-1"), delivery(sub, "it-order", "e-2", "i-1")],
    )
    .await
    .unwrap();
    exec(
        &db,
        "UPDATE cmx_flow_webhook_delivery SET next_attempt_at = now() + interval '1 hour' \
         WHERE event_id = 'e-1'",
    )
    .await;

    // row1 退避中：既不可投，也压住 row2（有序优先）。
    let claimed = webhook_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert!(claimed.is_empty(), "退避等待应阻塞同订阅后续");

    // row1 退避到期：先投 row1（seq 小者优先）。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET next_attempt_at = now() - interval '1s' \
         WHERE event_id = 'e-1'")
    .await;
    let claimed = webhook_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].event_id, "e-1");

    // row1 → DEAD（终态不阻塞）：row2 立即可投。
    webhook_store::finish_dead(
        &db,
        claimed[0].id,
        "w1",
        &webhook_store::Diagnostics { error: "x".into(), http_status: Some(404), snippet: None },
    )
    .await
    .unwrap();
    let claimed2 = webhook_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed2.len(), 1);
    assert_eq!(claimed2[0].event_id, "e-2", "终态不阻塞同订阅后续");
}

/// 租约过期自愈 + at-least-once：worker1 崩溃留过期租约 → worker2 重抢（attempts 累加）。
#[tokio::test]
#[ignore]
async fn lease_expiry_reclaim() {
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-lease").await;
    webhook_store::insert_deliveries(&db, &[delivery(sub, "it-lease", "e-1", "i-9")])
        .await
        .unwrap();

    let c1 = webhook_store::claim_due_deliveries(&db, "worker-crash", 120, 10).await.unwrap();
    assert_eq!(c1.len(), 1);
    // 模拟 worker1 长停顿：租约自然过期。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET lock_expires_at = now() - interval '1s'")
        .await;
    let c2 = webhook_store::claim_due_deliveries(&db, "worker2", 120, 10).await.unwrap();
    assert_eq!(c2.len(), 1);
    assert_eq!(c2[0].id, c1[0].id);
    assert_eq!(c2[0].attempts, 2, "重抢应累加 attempts");
}

/// 退避→DEAD→retry 重发→skip 处置→purge 清理 全状态机。
#[tokio::test]
#[ignore]
async fn dead_retry_skip_purge_lifecycle() {
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-lifecycle").await; // retry_max = 3
    webhook_store::insert_deliveries(&db, &[delivery(sub, "it-lifecycle", "e-1", "i-2")])
        .await
        .unwrap();

    // 尝试 1（< retry_max）→ PENDING + 退避到期时间。
    let c = webhook_store::claim_due_deliveries(&db, "w", 120, 10).await.unwrap();
    webhook_store::finish_retry_or_dead(
        &db,
        c[0].id,
        "w",
        c[0].attempts,
        3,
        chrono::Duration::seconds(5),
        &webhook_store::Diagnostics { error: "boom".into(), http_status: Some(500), snippet: None },
    )
    .await
    .unwrap();
    let (rows, _) = webhook_store::query_deliveries(
        &db,
        TEST_TENANT,
        &DlvFilter { state: Some("PENDING".into()), ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "尝试未耗尽应回 PENDING");
    assert!(rows[0]["nextAttemptAt"].is_string(), "退避到期时间应已设置");

    // 退避到期（直接回拨，不空等）后再次抢占 → 尝试耗尽 → DEAD。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET next_attempt_at = now() - interval '1s'")
        .await;
    // 尝试耗尽 → DEAD。
    let c = webhook_store::claim_due_deliveries(&db, "w", 120, 10).await.unwrap();
    webhook_store::finish_retry_or_dead(
        &db,
        c[0].id,
        "w",
        3, // attempts >= retry_max(3)
        3,
        chrono::Duration::seconds(5),
        &webhook_store::Diagnostics { error: "boom".into(), http_status: Some(500), snippet: None },
    )
    .await
    .unwrap();
    let (rows, _) = webhook_store::query_deliveries(
        &db,
        TEST_TENANT,
        &DlvFilter { state: Some("DEAD".into()), ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "尝试耗尽应进 DEAD");

    // 人工重发：DEAD → PENDING、attempts 归零。
    let reset = webhook_store::retry_deliveries(&db, TEST_TENANT, &[], Some(sub), Some("DEAD"))
        .await
        .unwrap();
    assert_eq!(reset, 1);
    let c = webhook_store::claim_due_deliveries(&db, "w", 120, 10).await.unwrap();
    assert_eq!(c[0].attempts, 1, "重发后 attempts 从 0 重新起算");
    webhook_store::finish_dead(
        &db,
        c[0].id,
        "w",
        &webhook_store::Diagnostics { error: "again".into(), http_status: None, snippet: None },
    )
    .await
    .unwrap();

    // 处置：DEAD → SKIPPED（留痕），重复 skip 不再命中。
    let skipped = webhook_store::skip_deliveries(&db, TEST_TENANT, &[c[0].id]).await.unwrap();
    assert_eq!(skipped, 1);
    assert_eq!(
        webhook_store::skip_deliveries(&db, TEST_TENANT, &[c[0].id]).await.unwrap(),
        0,
        "SKIPPED 非 DEAD，二次处置不命中"
    );

    // 清理：回拨 created_at 后按保留期清 DONE/SKIPPED。
    exec(
        &db,
        "UPDATE cmx_flow_webhook_delivery SET created_at = now() - interval '30 days'",
    )
    .await;
    let purged = webhook_store::purge_deliveries(&db, TEST_TENANT, 7, None).await.unwrap();
    assert_eq!(purged, 1, "超保留期的 SKIPPED 行应被清理");
}

/// 首启 env 导入（决议 17）：空表才种、name 确定性、secret 沿用全局密钥、幂等。
#[tokio::test]
#[ignore]
async fn import_env_subscriptions_idempotent() {
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let targets = vec![
        cmx_flow_adapters::WebhookTarget {
            key: "mdm".into(),
            path: "/api/mdm/flow/callback".into(),
        },
        cmx_flow_adapters::WebhookTarget {
            key: "erp".into(),
            path: "/api/erp/hook".into(),
        },
    ];
    let n1 = webhook_store::import_env_subscriptions(&db, TEST_TENANT, &targets, Some("global-key"))
        .await
        .unwrap();
    assert_eq!(n1, 2, "空表首次导入应种 2 条");
    // 幂等：再跑返回 0；并发/重启不重复。
    let n2 = webhook_store::import_env_subscriptions(&db, TEST_TENANT, &targets, Some("global-key"))
        .await
        .unwrap();
    assert_eq!(n2, 0, "非空表绝不复位用户改动");

    let (rows, total) =
        webhook_store::list_subscriptions(&db, TEST_TENANT, &SubFilter::default()).await.unwrap();
    assert_eq!(total, 2);
    let by_name: std::collections::HashMap<String, serde_json::Value> = rows
        .into_iter()
        .map(|r| (r["name"].as_str().unwrap_or_default().to_string(), r))
        .collect();
    let mdm = &by_name["env-mdm"];
    assert_eq!(mdm["source"], json!("env"));
    assert_eq!(mdm["channelConfig"]["secret"], json!("global-key"), "导入行沿用全局密钥");
    assert_eq!(mdm["channelConfig"]["service_key"], json!("mdm"));
}
