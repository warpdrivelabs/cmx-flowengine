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

/// 建表收敛：并行测试各自跑幂等 ALTER 会撞锁，进程内只做一次。
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
/// 同库串行：用例共享全局连接池/同库数据，并行互相干扰（连接 Closed / 行冲突），
/// 全程互斥串行执行（与 incident_retention_pg 同修法）。
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    SCHEMA_READY
        .get_or_init(|| async {
            webhook_store::ensure_schema(&db_id).await.expect("建表失败");
        })
        .await;
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
        route_source: "matched",
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-lease").await;
    webhook_store::insert_deliveries(&db, &[delivery(sub, "it-lease", "e-1", "i-9")])
        .await
        .unwrap();

    // claim 是 worker 级跨订阅抢占——共库 dev 环境下真实业务的到期 PENDING 行也会被
    // 抢到（结构性限制），断言一律按本用例 event_id 过滤。
    let c1 = webhook_store::claim_due_deliveries(&db, "worker-crash", 120, 10).await.unwrap();
    let c1: Vec<_> = c1.iter().filter(|d| d.event_id == "e-1").collect();
    assert_eq!(c1.len(), 1);
    // 模拟 worker1 长停顿：租约自然过期（仅本用例行——共库 dev 环境有真实业务行）。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET lock_expires_at = now() - interval '1s' \
         WHERE event_id = 'e-1'")
        .await;
    let c2 = webhook_store::claim_due_deliveries(&db, "worker2", 120, 10).await.unwrap();
    let c2: Vec<_> = c2.iter().filter(|d| d.event_id == "e-1").collect();
    assert_eq!(c2.len(), 1);
    assert_eq!(c2[0].id, c1[0].id);
    assert_eq!(c2[0].attempts, 2, "重抢应累加 attempts");
}

/// 退避→DEAD→retry 重发→skip 处置→purge 清理 全状态机。
#[tokio::test]
#[ignore]
async fn dead_retry_skip_purge_lifecycle() {
    let _guard = TEST_LOCK.lock().await;
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
    let mine: Vec<_> = rows.iter().filter(|r| r["eventId"] == "e-1").collect();
    assert_eq!(mine.len(), 1, "尝试未耗尽应回 PENDING");
    assert!(mine[0]["nextAttemptAt"].is_string(), "退避到期时间应已设置");

    // 退避到期（直接回拨，不空等）后再次抢占 → 尝试耗尽 → DEAD（仅本用例行）。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET next_attempt_at = now() - interval '1s' \
         WHERE event_id = 'e-1'")
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
    let _guard = TEST_LOCK.lock().await;
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

/// X3-T（W-03）：rebuild 确定性 event_id——同参数重复 rebuild 不再产生重复投递行
///（uk(subscription_id, event_id) 幂等真正生效；随机 uuid 时每次重跑都是重复投递）。
#[tokio::test]
#[ignore]
async fn rebuild_event_id_is_deterministic() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-rebuild").await;
    // 确定性 event_id 形态（与 webhook_admin rebuild 端点同式）。
    let mk = |n: u32| crate::DeliveryInsert {
        subscription_id: sub,
        subscription_name: format!("it-rebuild-{n}"),
        channel: "webhook".into(),
        event_id: format!("rb-{sub}-i9-cmp"),
        delivery_id: "i-9-t".into(),
        source: "rebuild",
        event_type: "instance.completed".into(),
        definition_key: Some("mdm_x".into()),
        business_key: None,
        instance_id: "i-9".into(),
        payload: serde_json::json!({ "event": "instance.completed", "instanceId": "i-9" }),
        initial_state: "PENDING",
        last_error: None,
        last_http_status: None,
        last_response_snippet: None,
        delivered: false,
        route_source: "matched",
    };
    // 两次「重跑 rebuild」（同 event_id、不同 subscription_name 快照）——uk 吸收第二次。
    webhook_store::insert_deliveries(&db, &[mk(1)]).await.unwrap();
    webhook_store::insert_deliveries(&db, &[mk(2)]).await.unwrap();
    let c = count_deliveries(&db, "SELECT COUNT(*) AS n FROM cmx_flow_webhook_delivery \
         WHERE subscription_id = $1 AND event_id = $2", vec![
        cmx_core::model::cell::DataValue::Int(sub),
        cmx_core::model::cell::DataValue::String(format!("rb-{sub}-i9-cmp")),
    ]).await;
    assert_eq!(c, 1, "确定性 event_id 下重复 rebuild 应被 uk 幂等吸收");
}

/// X3-T（X3-1/W-04）：purge 清孤儿行——订阅已物理删的投递行不再被订阅反查挡住。
#[tokio::test]
#[ignore]
async fn purge_cleans_orphan_rows_of_deleted_subscription() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-orphan").await;
    webhook_store::insert_deliveries(&db, &[delivery(sub, "it-orphan", "e-orphan", "i-o")])
        .await
        .unwrap();
    // 物理删订阅（先停用满足删除守卫）。
    webhook_store::set_subscription_active(&db, TEST_TENANT, sub, false).await.unwrap();
    webhook_store::delete_subscription(&db, TEST_TENANT, sub).await.unwrap();
    // 把孤儿行做成「DONE 且超期」——retention 条件命中。
    exec(&db, "UPDATE cmx_flow_webhook_delivery SET state = 'DONE', \
         created_at = now() - interval '40 days' WHERE event_id = 'e-orphan'").await;
    let n = webhook_store::purge_deliveries(&db, TEST_TENANT, 30, None).await.unwrap();
    assert!(n >= 1, "孤儿行（订阅已删）应可被 retention 清理（原 IN 子查询永久挡住）");
}

/// X3-T（W-02）：subscribe 对停用/通配行返回 0 行（handler 侧据此 400）。
#[tokio::test]
#[ignore]
async fn subscribe_returns_zero_rows_for_rejected_targets() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    // 通配行（definition_keys 空）——subscribe 应 0 行。
    let wildcard = upsert_test_sub(&db, "it-wildcard").await;
    let n = webhook_store::subscribe_definitions(&db, TEST_TENANT, "it-wildcard", &["mdm_x".into()])
        .await
        .unwrap();
    assert_eq!(n, 0, "通配行 subscribe 须 0 行（WILDCARD_IMMUTABLE 语义在 SQL 谓词）");
    // 停用行——同样 0 行。
    webhook_store::set_subscription_active(&db, TEST_TENANT, wildcard, false).await.unwrap();
    let n2 = webhook_store::subscribe_definitions(&db, TEST_TENANT, "it-wildcard", &["mdm_x".into()])
        .await
        .unwrap();
    assert_eq!(n2, 0, "停用订阅 subscribe 须 0 行");
}

async fn count_deliveries(db: &str, sql: &str, params: Vec<cmx_core::model::cell::DataValue>) -> i64 {
    let ds = cmx_database_pg::query_sql_with_params(
        db,
        None,
        sql,
        cmx_database_pg::SqlParams::DataValues(params),
        "it_count_dlv",
    )
    .await
    .expect("计数失败");
    ds.iter()
        .next()
        .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "n") {
            Some(cmx_core::model::cell::DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(-1)
}
