//! incident 台账 + retention 清理的持久化层集成测试（#[ignore] 门控，需本地 PG）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-app --test incident_retention_pg -- --ignored --nocapture
//!
//! 覆盖（审查修复方案 X1-T，018 gate 随批落）：
//!   - 自动重试扫描 SQL 可执行（O-01：DISTINCT + ORDER BY 非选择列在 PG 必报错，已改
//!     GROUP BY + max() 聚合等价改写——本用例即防回归锚点）；
//!   - incident 台账生命周期：upsert 幂等累加 retries / resolve_by_node 精确收账不误关
//!     同实例其它节点 / resolve_by_instance 全量收账（O-04/RA-03）；
//!   - retention 12 表清理：终态实例超期后 12 张运行态表全清、hi 归档保留（O-02——
//!     漏 incident/deadletter 两表会让实例删除后幽灵行无限累积）。

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{execute_sql_with_params, get_default_pg_db_manager, DbConfig, DbType, SqlParams};
use cmx_flow_model::runtime::IncidentRecord;
use cmx_flow_model::store::RuntimeStore;
use cmx_flow_store_pg::PgRuntimeStore;

const TEST_DB_ID: &str = "cmx_flow_incident_test";

/// 建表收敛：并行测试各自跑幂等 ALTER 会撞锁，进程内只做一次。
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
/// 同库串行：三用例共享全局连接池/同库数据，并行互相干扰（连接 Closed/行冲突），
/// 全程互斥串行执行。
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 串行包裹：测试体全程持锁。
async fn serialized(f: impl std::future::Future<Output = ()>) {
    let _g = TEST_LOCK.lock().await;
    f.await;
}

/// 注册测试数据源（TEST_PG_URL 未设 → None，调用方跳过）。
async fn setup_db() -> Option<PgRuntimeStore> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let manager = get_default_pg_db_manager();
    let cfg = DbConfig {
        db_type: DbType::Postgres,
        db_url: url,
        db_id: TEST_DB_ID.to_string(),
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
    let store = PgRuntimeStore::new(TEST_DB_ID);
    SCHEMA_READY
        .get_or_init(|| async {
            store.ensure_schema().await.expect("建表失败");
        })
        .await;
    Some(store)
}

async fn exec(db_id: &str, sql: &str, params: Vec<DataValue>) {
    execute_sql_with_params(db_id, None, sql, SqlParams::DataValues(params))
        .await
        .expect("执行 SQL 失败");
}

fn rec(instance: &str, node: &str, retries: i64) -> IncidentRecord {
    let now = chrono::Utc::now();
    IncidentRecord {
        instance_id: instance.to_string(),
        node_bpmn_id: node.to_string(),
        token_id: Some("tok-1".into()),
        definition_key: "it_def".into(),
        business_key: Some("BK".into()),
        reason: "it 故障".into(),
        retries,
        state: "OPEN".into(),
        created_at: now,
        updated_at: now,
    }
}

/// O-01 防回归锚点：重试扫描 SQL 在 PG 必须可执行（原 DISTINCT 形态每次必报错且被吞）。
#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn incident_scan_sql_executes() {
    let _guard = TEST_LOCK.lock().await;
    let Some(store) = setup_db().await else { return };
    store
        .upsert_incident(&rec("scan-iid", "n1", 1))
        .await
        .expect("造 OPEN 行失败");
    // 与 spawn_incident_retry 逐字节同款（GROUP BY + max 聚合改写）。
    let ds = cmx_database_pg::query_sql(
        TEST_DB_ID,
        None,
        "SELECT instance_id FROM cmx_flow_incident \
         WHERE state = 'OPEN' GROUP BY instance_id \
         ORDER BY max(updated_at) LIMIT 20",
        "it_scan_sql",
    )
    .await
    .expect("扫描 SQL 必须可执行（O-01 回归即失败于此）");
    let hit = ds
        .iter()
        .any(|row| matches!(row.get_by_name(ds.schema.as_ref(), "instance_id"),
             Some(DataValue::String(s)) if s == "scan-iid"));
    assert!(hit, "OPEN 行应被扫描命中");
    exec(TEST_DB_ID, "DELETE FROM cmx_flow_incident WHERE instance_id = $1",
         vec![DataValue::String("scan-iid".into())]).await;
}

/// 台账生命周期：幂等累加 / 按节点精确收账 / 按实例全量收账。
#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn incident_upsert_and_resolve_lifecycle() {
    let _guard = TEST_LOCK.lock().await;
    let Some(store) = setup_db().await else { return };
    store.upsert_incident(&rec("lc-iid", "node_a", 1)).await.unwrap();
    // 幂等累加：同 (instance, node) 再 upsert → retries 覆盖为新值、state 回 OPEN。
    store.upsert_incident(&rec("lc-iid", "node_a", 2)).await.unwrap();
    // 同实例另一故障节点（并行分支）。
    store.upsert_incident(&rec("lc-iid", "node_b", 1)).await.unwrap();

    // complete_async_job 成功路径（X1-6③）：只关 node_a，不误关 node_b。
    store.resolve_incident_by_node("lc-iid", "node_a").await.unwrap();
    let a = incident_state(&store, "lc-iid", "node_a").await;
    let b = incident_state(&store, "lc-iid", "node_b").await;
    assert_eq!(a.as_deref(), Some("RESOLVED"), "node_a 应已收账");
    assert_eq!(b.as_deref(), Some("OPEN"), "node_b 不应被误关");

    // cancel 终态化路径（X1-6①）：实例级全量收账。
    store.resolve_incidents_by_instance("lc-iid").await.unwrap();
    let b2 = incident_state(&store, "lc-iid", "node_b").await;
    assert_eq!(b2.as_deref(), Some("RESOLVED"), "实例级应收账全部 OPEN");

    // RESOLVED 后再 upsert（新故障）→ 回 OPEN 且 retries 累加为新值。
    store.upsert_incident(&rec("lc-iid", "node_b", 3)).await.unwrap();
    let b3 = incident_state(&store, "lc-iid", "node_b").await;
    assert_eq!(b3.as_deref(), Some("OPEN"), "复发应回 OPEN");
    exec(TEST_DB_ID, "DELETE FROM cmx_flow_incident WHERE instance_id = $1",
         vec![DataValue::String("lc-iid".into())]).await;
}

async fn incident_state(_store: &PgRuntimeStore, iid: &str, node: &str) -> Option<String> {
    let ds = cmx_database_pg::query_sql_with_params(
        TEST_DB_ID,
        None,
        "SELECT state FROM cmx_flow_incident WHERE instance_id = $1 AND node_bpmn_id = $2",
        SqlParams::DataValues(vec![
            DataValue::String(iid.to_string()),
            DataValue::String(node.to_string()),
        ]),
        "it_state",
    )
    .await
    .expect("查 incident 失败");
    ds.iter()
        .next()
        .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "state") {
            Some(DataValue::String(s)) => Some(s.clone()),
            _ => None,
        })
}

/// retention 12 表清理（O-02/X1-7）：超期终态实例全表清、hi 归档保留。
#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn purge_cleans_twelve_tables_and_keeps_hi() {
    let _guard = TEST_LOCK.lock().await;
    let Some(_store) = setup_db().await else { return };
    let iid = "purge-iid";
    // 清残留 → 造一个 31 天前终态实例 + 12 表各行 + hi 归档。
    cleanup_purge_fixture(iid).await;
    exec(TEST_DB_ID,
        "INSERT INTO cmx_flow_instance (id, definition_key, state, variables, version, created_at, updated_at, ended_at) \
         VALUES ($1, 'it_def', 'COMPLETED', '{}', 3, now() - interval '40 days', now() - interval '31 days', now() - interval '31 days')",
        vec![DataValue::String(iid.into())]).await;
    // 11 张子表各造一行（列集对齐真实 DDL 的 NOT NULL 最小集）。
    for (sql, idc) in [
        ("INSERT INTO cmx_flow_token (id, instance_id, node_bpmn_id, state, created_at, updated_at) VALUES ($1, $2, 'n1', 'ENDED', now(), now())", "tok"),
        ("INSERT INTO cmx_flow_task (id, instance_id, token_id, node_bpmn_id, completed, created_at) VALUES ($1, $2, 'tok-purge', 'n1', true, now())", "task"),
        ("INSERT INTO cmx_flow_mi_scope (id, instance_id, node_bpmn_id, total, finished) VALUES ($1, $2, 'n1', 1, true)", "mi"),
        ("INSERT INTO cmx_flow_job (id, instance_id, token_id, boundary_bpmn_id, kind, due_at, created_at) VALUES ($1, $2, 'tok-purge', 'n1', 'TIMER', now(), now())", "job"),
        ("INSERT INTO cmx_flow_task_candidate (id, task_id, instance_id, candidate_type, candidate_ref, resolved_user_id) VALUES ($1, 'task-purge', $2, 'USER', 'u1', 'u1')", "cand"),
        ("INSERT INTO cmx_flow_cc (id, instance_id, node_bpmn_id, to_user_id, read_at, created_at) VALUES ($1, $2, 'n1', 'u1', now(), now())", "cc"),
        ("INSERT INTO cmx_flow_task_delegation (id, task_id, instance_id, kind, from_user_id, to_user_id, created_at) VALUES ($1, 'task-purge', $2, 'TRANSFER', 'u1', 'u2', now())", "dele"),
        ("INSERT INTO cmx_flow_message_subscription (id, kind, message_name, node_bpmn_id, instance_id, token_id, tenant_id, created_at) VALUES ($1, 'CATCH', 'm', 'n1', $2, 'tok-purge', 'default', now())", "msg"),
        ("INSERT INTO cmx_flow_async_job (id, instance_id, token_id, node_bpmn_id, delegate_key, max_retries, retries, created_at) VALUES ($1, $2, 'tok-purge', 'n1', 'k', 3, 3, now())", "aj"),
        ("INSERT INTO cmx_flow_incident (id, instance_id, node_bpmn_id, definition_key, reason, retries, state, created_at, updated_at) VALUES ($1, $2, 'n1', 'it_def', 'r', 1, 'OPEN', now(), now())", "inc"),
        ("INSERT INTO cmx_flow_deadletter_job (id, instance_id, token_id, node_bpmn_id, delegate_key, max_retries, error, original_created_at, dead_lettered_at, tenant_id) VALUES ($1, $2, 'tok-purge', 'n1', 'k', 3, 'e', now(), now(), 'default')", "dl"),
    ] {
        let row_id = format!("{idc}-purge");
        exec(TEST_DB_ID, sql, vec![
            DataValue::String(row_id),
            DataValue::String(iid.to_string()),
        ]).await;
    }
    // hi 归档行（应保留；列集对齐 hi 表真实 DDL——无 updated_at，有 archived_at）。
    exec(TEST_DB_ID,
        "INSERT INTO cmx_flow_hi_instance (id, definition_key, state, version, created_at, ended_at, archived_at) \
         VALUES ($1, 'it_def', 'COMPLETED', 3, now(), now(), now())",
        vec![DataValue::String(iid.into())]).await;

    let (n, rows) = cmx_flow_app::engine::purge_terminal_instances(TEST_DB_ID, 30)
        .await
        .expect("retention 清理失败");
    assert!(n >= 1, "至少清掉本用例实例");
    assert!(rows >= 12, "12 表行全删（含补齐的 incident/deadletter）");

    // 12 表行数归零。
    for t in [
        "cmx_flow_token", "cmx_flow_task", "cmx_flow_mi_scope", "cmx_flow_job",
        "cmx_flow_task_candidate", "cmx_flow_cc", "cmx_flow_task_delegation",
        "cmx_flow_message_subscription", "cmx_flow_async_job", "cmx_flow_incident",
        "cmx_flow_deadletter_job", "cmx_flow_instance",
    ] {
        let c = count_rows(t, iid).await;
        assert_eq!(c, 0, "{t} 应已清空");
    }
    // hi 归档保留（审计不断链）。
    let hi = count_rows("cmx_flow_hi_instance", iid).await;
    assert_eq!(hi, 1, "hi 归档行应保留");
    cleanup_purge_fixture(iid).await;
}

async fn count_rows(table: &str, iid: &str) -> i64 {
    // instance/hi_instance 两表主键即 id、无 instance_id 列。
    let sql = if table.ends_with("_instance") {
        format!("SELECT COUNT(*) AS c FROM {table} WHERE id = $1")
    } else {
        format!("SELECT COUNT(*) AS c FROM {table} WHERE instance_id = $1")
    };
    let ds = cmx_database_pg::query_sql_with_params(
        TEST_DB_ID, None, &sql,
        SqlParams::DataValues(vec![DataValue::String(iid.to_string())]),
        "it_count",
    )
    .await
    .expect("count 失败");
    ds.iter()
        .next()
        .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "c") {
            Some(DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(0)
}

async fn cleanup_purge_fixture(iid: &str) {
    for t in [
        "cmx_flow_token", "cmx_flow_task", "cmx_flow_mi_scope", "cmx_flow_job",
        "cmx_flow_task_candidate", "cmx_flow_cc", "cmx_flow_task_delegation",
        "cmx_flow_message_subscription", "cmx_flow_async_job", "cmx_flow_incident",
        "cmx_flow_deadletter_job", "cmx_flow_instance", "cmx_flow_hi_instance",
    ] {
        let sql = if t.ends_with("_instance") {
            format!("DELETE FROM {t} WHERE id = $1")
        } else {
            format!("DELETE FROM {t} WHERE instance_id = $1")
        };
        let _ = execute_sql_with_params(TEST_DB_ID, None, &sql,
            SqlParams::DataValues(vec![DataValue::String(iid.to_string())])).await;
    }
}
