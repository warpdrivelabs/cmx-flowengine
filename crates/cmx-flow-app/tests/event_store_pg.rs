//! 事件订阅域存储层 + 投递链路集成测试（#[ignore] 门控，需本地 PG）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-app --test event_store_pg -- --ignored --nocapture
//!
//! 覆盖（重构方案 §九）：uk 幂等（rebuild/test 确定性 event_id 去重）、租约抢占互斥与
//! 持有者守卫、同订阅者保序（退避阻塞 / 终态不阻塞）、退避→DEAD→retry/skip/purge、
//! 订阅者删除守卫（停用 + 无未终态行）、分组删除守卫（组内有定义拒绝）。每测试用独立
//! 租户 + 独立订阅者/分组，互不干扰。

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager};

use serde_json::json;

use cmx_flow_app::event_store::{self, DeliveryInsert, DlvFilter, GroupUpsert, SubRule, SubscriberUpsert};

const TEST_TENANT: &str = "evt-it";

/// 建表收敛：并行测试各自跑幂等 ALTER 会撞锁，进程内只做一次。
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
/// 同库串行：用例共享全局连接池/同库数据，并行互相干扰（连接 Closed / 行冲突）。
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 注册测试数据源（TEST_PG_URL 未设 → None，调用方跳过）。
async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_evt_test".to_string();
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
            event_store::ensure_schema(&db_id).await.expect("建表失败");
        })
        .await;
    Some(db_id)
}

/// 建表 + 清空本测试租户的旧数据（幂等重跑）。
async fn fresh(db_id: &str) {
    event_store::ensure_schema(db_id).await.expect("建表失败");
    for sql in [
        "DELETE FROM cmx_flow_event_delivery WHERE subscriber_id IN \
         (SELECT id FROM cmx_flow_event_subscriber WHERE tenant_id = $1)",
        "DELETE FROM cmx_flow_event_subscriber WHERE tenant_id = $1",
        "DELETE FROM cmx_flow_def_group WHERE name LIKE 'it-grp-%'",
        "UPDATE cmx_flow_definition SET group_id = NULL WHERE key LIKE 'it_def_%'",
        "DELETE FROM cmx_flow_definition WHERE key LIKE 'it_def_%'",
    ] {
        execute_sql_with_params_wrap(db_id, sql, TEST_TENANT).await;
    }
}

async fn execute_sql_with_params_wrap(db_id: &str, sql: &str, tenant: &str) {
    use cmx_database_pg::{SqlParams, execute_sql_with_params};
    let params = SqlParams::DataValues(vec![DataValue::String(tenant.to_string())]);
    execute_sql_with_params(db_id, None, sql, params).await.expect("清理旧数据失败");
}

/// 建一个带一条网关规则（全匹配）的订阅者。
async fn upsert_test_sub(db_id: &str, name: &str) -> i64 {
    event_store::upsert_subscriber(
        db_id,
        TEST_TENANT,
        &SubscriberUpsert {
            id: None,
            name: name.to_string(),
            description: None,
            channel: "webhook".into(),
            channel_config: json!({
                "service_key": "mdm",
                "callback_path": "/api/mdm/flow/callback",
                "secret": "it-secret",
            }),
            rules: vec![SubRule {
                name: "全量".into(),
                enabled: true,
                event_types: vec![],
                group_ids: vec![],
                key_patterns: vec![],
            }],
            retry_max: 3,
            active: true,
            created_by: Some("it".into()),
        },
    )
    .await
    .expect("建订阅者失败")
}

fn delivery(sub_id: i64, sub_name: &str, event_id: &str, instance: &str) -> DeliveryInsert {
    DeliveryInsert {
        subscriber_id: sub_id,
        subscriber_name: sub_name.to_string(),
        channel: "webhook".into(),
        event_id: event_id.to_string(),
        delivery_id: format!("{instance}-t1-{event_id}"),
        source: "emit",
        event_type: "instance.started".into(),
        definition_key: Some("it_def_1".into()),
        business_key: None,
        instance_id: instance.to_string(),
        payload: json!({ "event": "instance.started", "instanceId": instance }),
        initial_state: "PENDING",
        last_error: None,
        last_http_status: None,
        last_response_snippet: None,
        delivered: false,
        matched_rule: Some("全量".into()),
    }
}

/// 直接执行一条 SQL（测试 manipulations 用）。
async fn exec(db_id: &str, sql: &str) {
    execute_sql(db_id, None, sql).await.expect("执行 SQL 失败");
}

/// uk 幂等：同 (subscriber_id, event_id) 重复写入只落一行；claim → DONE 全链路；
/// 同租约持有期内二次抢占互斥。
#[tokio::test]
#[ignore]
async fn uk_dedup_and_claim_chain() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-dedup").await;

    // 同一确定性 event_id 重复写入（rebuild 重复点击形态）：uk 幂等，第二次被吞。
    let row = delivery(sub, "it-dedup", "rb-1-i1-cmp", "i-1");
    let n1 = event_store::insert_deliveries(&db, &[row.clone()]).await.unwrap();
    let n2 = event_store::insert_deliveries(&db, &[row]).await.unwrap();
    assert_eq!((n1, n2), (1, 0), "同事件确定性 id 重复写入应被 uk 吞");

    // 抢占：attempts +1，租约打上。
    let claimed = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 1);

    // 租约持有期内二次抢占互斥（另一 worker 拿不到同一行）。
    let again = event_store::claim_due_deliveries(&db, "w2", 120, 10).await.unwrap();
    assert!(again.is_empty(), "租约有效期内不得重抢");

    // 持有者守卫：非持有者落结果 0 行命中；持有者成功。
    assert!(!event_store::finish_done(&db, claimed[0].id, "w2").await.unwrap());
    assert!(event_store::finish_done(&db, claimed[0].id, "w1").await.unwrap());

    let (rows, total) =
        event_store::query_deliveries(&db, TEST_TENANT, &DlvFilter::default()).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0]["state"], json!("DONE"));
    assert_eq!(rows[0]["matchedRule"], json!("全量"), "命中规则名快照随行落库");
}

/// 同订阅者保序：退避等待阻塞后续；终态（DEAD）不阻塞。
#[tokio::test]
#[ignore]
async fn ordering_guard_backoff_and_terminal() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-order").await;

    // 两行同订阅者：row1 退避未到期、row2 到期可投。
    event_store::insert_deliveries(
        &db,
        &[delivery(sub, "it-order", "e-1", "i-1"), delivery(sub, "it-order", "e-2", "i-1")],
    )
    .await
    .unwrap();
    exec(
        &db,
        "UPDATE cmx_flow_event_delivery SET next_attempt_at = now() + interval '1 hour' \
         WHERE event_id = 'e-1'",
    )
    .await;

    // row1 退避中：既不可投，也压住 row2（有序优先）。
    let claimed = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert!(claimed.is_empty(), "退避等待应阻塞同订阅者后续");

    // row1 退避到期：先投 row1（seq 小者优先）。
    exec(&db, "UPDATE cmx_flow_event_delivery SET next_attempt_at = now() - interval '1s' \
         WHERE event_id = 'e-1'")
    .await;
    let claimed = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].event_id, "e-1");

    // row1 → DEAD（终态不阻塞）：row2 立即可投。
    event_store::finish_dead(
        &db,
        claimed[0].id,
        "w1",
        &event_store::Diagnostics { error: "x".into(), http_status: Some(404), snippet: None },
    )
    .await
    .unwrap();
    let claimed2 = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(claimed2.len(), 1, "终态行不阻塞同订阅者后续");
    assert_eq!(claimed2[0].event_id, "e-2");
}

/// 退避→尝试耗尽→DEAD→retry 重置→skip 处置→purge 清理 全链。
#[tokio::test]
#[ignore]
async fn dead_letter_retry_skip_purge() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-dead").await;
    event_store::insert_deliveries(&db, &[delivery(sub, "it-dead", "e-1", "i-1")]).await.unwrap();

    // 首抢失败（retry_max=3 含首发）：回 PENDING 退避。
    let c1 = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(c1.len(), 1);
    let ok = event_store::finish_retry_or_dead(
        &db,
        c1[0].id,
        "w1",
        c1[0].attempts,
        3,
        chrono::Duration::seconds(1),
        &event_store::Diagnostics { error: "500".into(), http_status: Some(500), snippet: None },
    )
    .await
    .unwrap();
    assert!(ok);

    // 连续抢失败到耗尽（退避 1s；等 1.2s 过窗）。
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let c2 = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(c2.len(), 1);
    assert_eq!(c2[0].attempts, 2);
    event_store::finish_retry_or_dead(
        &db,
        c2[0].id,
        "w1",
        c2[0].attempts,
        3,
        chrono::Duration::seconds(1),
        &event_store::Diagnostics { error: "500".into(), http_status: Some(500), snippet: None },
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let c3 = event_store::claim_due_deliveries(&db, "w1", 120, 10).await.unwrap();
    assert_eq!(c3.len(), 1);
    assert_eq!(c3[0].attempts, 3);
    // attempts(3) ≥ retry_max(3) → DEAD。
    event_store::finish_retry_or_dead(
        &db,
        c3[0].id,
        "w1",
        c3[0].attempts,
        3,
        chrono::Duration::seconds(1),
        &event_store::Diagnostics { error: "500".into(), http_status: Some(500), snippet: Some("boom".into()) },
    )
    .await
    .unwrap();

    let (rows, _) = event_store::query_deliveries(
        &db,
        TEST_TENANT,
        &DlvFilter { state: Some("DEAD".into()), ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "尝试耗尽进死信");
    assert_eq!(rows[0]["lastHttpStatus"], json!(500));

    // retry：DEAD → PENDING、attempts 归零（可再抢）。
    let n = event_store::retry_deliveries(&db, TEST_TENANT, &[c3[0].id], None, None)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let again = event_store::claim_due_deliveries(&db, "w2", 120, 10).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempts, 1, "重发后重置完整重试预算");

    // skip：处置留痕（先失败一次回 PENDING，再人工弃投）。
    event_store::finish_retry_or_dead(
        &db,
        again[0].id,
        "w2",
        1,
        3,
        chrono::Duration::seconds(1),
        &event_store::Diagnostics { error: "x".into(), http_status: None, snippet: None },
    )
    .await
    .unwrap();
    let n = event_store::skip_deliveries(&db, TEST_TENANT, &[again[0].id]).await.unwrap();
    assert_eq!(n, 1);

    // purge：SKIPPED/DONE 终态行按天清理（before_days=1 只清 1 天前的——先把行龄改老）。
    exec(&db, "UPDATE cmx_flow_event_delivery SET created_at = now() - interval '2 days'").await;
    let n = event_store::purge_deliveries(&db, 1, None).await.unwrap();
    assert!(n >= 1, "终态行应被清理");
}

/// 订阅者删除守卫：启用态拒绝；停用但有未终态/死信行拒绝；干净停用可删。
#[tokio::test]
#[ignore]
async fn subscriber_delete_guard() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let sub = upsert_test_sub(&db, "it-guard").await;
    event_store::insert_deliveries(&db, &[delivery(sub, "it-guard", "e-1", "i-1")]).await.unwrap();

    // 启用态 → 拒绝。
    let err = event_store::delete_subscriber(&db, TEST_TENANT, sub).await.unwrap_err();
    assert!(err.contains("启用"), "{err}");

    // 停用但有 PENDING 行 → 拒绝。
    event_store::set_subscriber_active(&db, TEST_TENANT, sub, false).await.unwrap();
    let err = event_store::delete_subscriber(&db, TEST_TENANT, sub).await.unwrap_err();
    assert!(err.contains("待投") || err.contains("死信"), "{err}");

    // 清干净（skip 终态）后可删；名字快照仍在流水可查。
    let row_id = event_store::query_deliveries(
        &db,
        TEST_TENANT,
        &DlvFilter { subscriber_id: Some(sub), ..Default::default() },
    )
    .await
    .unwrap()
    .0[0]["id"]
    .as_i64()
    .unwrap();
    event_store::skip_deliveries(&db, TEST_TENANT, &[row_id]).await.unwrap();
    event_store::delete_subscriber(&db, TEST_TENANT, sub).await.unwrap();
    let (rows, _) = event_store::query_deliveries(
        &db,
        TEST_TENANT,
        &DlvFilter { subscriber_id: Some(sub), ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "流水凭快照保留可查");
    assert_eq!(rows[0]["subscriberName"], json!("it-guard"));
}

/// 分组删除守卫：组内有定义拒绝；空组可删。定义 set-group 链路一并覆盖。
#[tokio::test]
#[ignore]
async fn group_guard_and_set_group() {
    let _guard = TEST_LOCK.lock().await;
    let Some(db) = setup_db().await else { return };
    fresh(&db).await;
    let gid = event_store::upsert_group(
        &db,
        &GroupUpsert {
            id: None,
            name: "it-grp-1".into(),
            sort_no: 0,
            enabled: true,
            remark: None,
        },
    )
    .await
    .unwrap();
    // 空组可删（先删再重建，验证守卫两分支）。
    event_store::delete_group(&db, gid).await.unwrap();
    let gid = event_store::upsert_group(
        &db,
        &GroupUpsert { id: None, name: "it-grp-1".into(), sort_no: 1, enabled: true, remark: None },
    )
    .await
    .unwrap();

    // 挂一个测试定义进组。
    exec(
        &db,
        &format!(
            "INSERT INTO cmx_flow_definition (key, name, state, group_id, updated_at) \
             VALUES ('it_def_1', 'IT测试定义', 'DRAFT', {gid}, now()) \
             ON CONFLICT (key) DO UPDATE SET group_id = EXCLUDED.group_id"
        ),
    )
    .await;
    let err = event_store::delete_group(&db, gid).await.unwrap_err();
    assert!(err.contains("流程定义"), "{err}");
    // 移出分组后可删。
    exec(&db, "UPDATE cmx_flow_definition SET group_id = NULL WHERE key = 'it_def_1'").await;
    event_store::delete_group(&db, gid).await.unwrap();
}
