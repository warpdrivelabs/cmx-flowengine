//! 007 实例乐观锁（CAS）持久化层集成测试（#[ignore] 门控，需本地 PG）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
//!     cargo test -p cmx-flow-app --test cas_conflict_pg -- --ignored --nocapture
//!
//! 覆盖（技术债 007 细案，替代 E2E 打不进毫秒竞态窗口的 curl 并发模拟）：
//!   - CAS 冲突：快照持有旧 version、库中 version 已被「并发写者」推进 →
//!     save_snapshot 影响 0 行 → [`StoreError::Conflict`]，整体回滚不落任何子表写入。
//!   - 冲突不吞并发者数据：冲突后重新 load，读到的是并发者提交的 version +1。
//!   - 正常路径：以最新 version 再 save → 成功，version 再 +1。

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{
    execute_sql_with_params, get_default_pg_db_manager, DbConfig, DbType, SqlParams,
};
use cmx_flow_model::runtime::{InstanceState, InstanceSnapshot, ProcessInstance};
use cmx_flow_model::store::{RuntimeStore, StoreError};
use cmx_flow_store_pg::PgRuntimeStore;

const TEST_DB_ID: &str = "cmx_flow_cas_test";

/// 同库串行：用例共享全局连接池/同库数据，并行互相干扰（连接 Closed），全程互斥。
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    store.ensure_schema().await.expect("建表失败");
    Some(store)
}

/// 构造最小可落库快照（无令牌/任务——CAS 只走实例行 UPDATE 路径）。
fn snap(id: &str) -> InstanceSnapshot {
    let now = chrono::Utc::now();
    InstanceSnapshot {
        instance: ProcessInstance {
            id: id.to_string(),
            definition_key: "cas_it_def".into(),
            business_key: Some("CAS-1".into()),
            state: InstanceState::Active,
            variables: Default::default(),
            created_at: now,
            updated_at: now,
            ended_at: None,
            org_id: None,
            dimensions: Default::default(),
            parent_instance_id: None,
            parent_token_id: None,
            parent_node_bpmn_id: None,
            subscriber_id: None,
            system_id: Some("casit".into()),
        },
        tokens: vec![],
        tasks: vec![],
        mi_scopes: vec![],
        jobs: vec![],
        async_jobs: vec![],
        candidates: vec![],
        cc_records: vec![],
        delegations: vec![],
        version: 0,
        pending_subs: vec![],
        pending_activities: vec![],
        pending_var_changes: vec![],
    }
}

/// 直接 SQL 模拟并发写者：把库中 version 推进 +1（引擎侧等价于另一运行段已 CAS 提交）。
async fn bump_version(db_id: &str, instance_id: &str) {
    let sql = "UPDATE cmx_flow_instance SET version = version + 1 WHERE id = $1";
    let params = SqlParams::DataValues(vec![DataValue::String(instance_id.to_string())]);
    execute_sql_with_params(db_id, None, sql, params)
        .await
        .expect("模拟并发写者失败");
}

#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn cas_conflict_rejects_stale_snapshot() {
    let _guard = TEST_LOCK.lock().await;
    let Some(store) = setup_db().await else {
        eprintln!("TEST_PG_URL 未设置，跳过");
        return;
    };
    let id = format!("cas-it-{}", uuid::Uuid::new_v4());

    // 建实例 → load 读到 version=0。
    let s0 = snap(&id);
    store.create_snapshot(&s0).await.expect("创建实例失败");
    let loaded = store.load_snapshot(&id).await.expect("载入实例失败");
    assert_eq!(loaded.version, 0, "新建实例 version 应为 0");

    // 并发写者抢先提交（version 0→1）；本快照仍持旧期望值 0。
    bump_version(TEST_DB_ID, &id).await;
    let mut stale = loaded.clone();
    stale.instance.business_key = Some("CAS-1-stale".into());

    // 后写者 save：CAS 0 行 → Conflict（不静默覆盖并发者数据）。
    let err = store
        .save_snapshot(&stale)
        .await
        .expect_err("旧 version 保存必须被拒");
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "期望 Conflict，实得 {err:?}"
    );

    // 冲突后重读：并发者的 version=1 仍在，且本快照的过期写入未落库。
    let reloaded = store.load_snapshot(&id).await.expect("重载实例失败");
    assert_eq!(reloaded.version, 1, "并发写者的 version 提交不得被吞");
    assert_eq!(reloaded.instance.business_key.as_deref(), Some("CAS-1"), "过期快照的写入不得落库");

    // 以最新 version 重放保存 → 成功，version 再 +1（调用方按 Conflict 信号重载后重试的路径）。
    let mut fresh = reloaded.clone();
    fresh.instance.business_key = Some("CAS-1-fresh".into());
    store.save_snapshot(&fresh).await.expect("以最新 version 保存应成功");
    let final_snap = store.load_snapshot(&id).await.expect("终态载入失败");
    assert_eq!(final_snap.version, 2, "成功保存后 version 应 +1");
    assert_eq!(final_snap.instance.business_key.as_deref(), Some("CAS-1-fresh"));
}

/// X3-T（C-05 扩展）：CAS 冲突整体回滚——事务内已执行的子表写入不落库。
/// 旁路剥离回归：mark_cc_read 的独立 UPDATE 不被旧快照 save 回滚（007 最恶性场景）。
#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn cas_conflict_rolls_back_subtables_and_keeps_cc_read() {
    let _guard = TEST_LOCK.lock().await;
    let Some(store) = setup_db().await else {
        eprintln!("TEST_PG_URL 未设置，跳过");
        return;
    };
    let id = format!("cas-cc-{}", uuid::Uuid::new_v4());

    let mut s0 = snap(&id);
    // 带一枚任务与一条未读抄送（构造「已读被并发回滚」的原始事故形态）。
    let now = chrono::Utc::now();
    s0.tasks.push(cmx_flow_model::runtime::Task {
        id: format!("{id}-t1"),
        instance_id: id.clone(),
        token_id: format!("{id}-tok"),
        node_bpmn_id: "n1".into(),
        name: Some("t".into()),
        assignee: None,
        candidate_groups: None,
        element_value: None,
        owner_user_id: None,
        parent_task_id: None,
        delegation_state: None,
        completed: false,
        created_at: now,
        completed_at: None,
    });
    s0.cc_records.push(cmx_flow_model::runtime::CcRecord {
        id: format!("{id}-cc1"),
        instance_id: id.clone(),
        node_bpmn_id: Some("n1".into()),
        to_user_id: "u1".into(),
        from_user_id: None,
        reason: None,
        read_at: None,
        created_at: now,
    });
    store.create_snapshot(&s0).await.expect("创建实例失败");
    let loaded = store.load_snapshot(&id).await.expect("载入失败");

    // 用户把抄送标为已读（旁路独立 UPDATE，不经快照）。
    let cc_id = format!("{id}-cc1");
    execute_sql_with_params(
        TEST_DB_ID,
        None,
        "UPDATE cmx_flow_cc SET read_at = now() WHERE id = $1",
        SqlParams::DataValues(vec![DataValue::String(cc_id.clone())]),
    )
    .await
    .expect("标已读失败");

    // 并发写者推进 version；旧快照（cc_records 仍是未读态 + 新增任务）save 冲突。
    bump_version(TEST_DB_ID, &id).await;
    let mut stale = loaded.clone();
    let mut extra_task = stale.tasks[0].clone();
    extra_task.id = format!("{id}-t2");
    stale.tasks.push(extra_task);
    let err = store.save_snapshot(&stale).await.expect_err("旧快照必须被拒");
    assert!(matches!(err, StoreError::Conflict(_)));

    // ① 冲突整体回滚：事务内重插的任务不落库（子表行数不变——build_save_ops 先删后插，
    //    冲突发生在 CAS 段，删/插全部回滚）。
    let reloaded = store.load_snapshot(&id).await.expect("重载失败");
    assert_eq!(reloaded.tasks.len(), 1, "冲突 save 的子表写入不得落库");

    // ② 旁路剥离回归：已读标记未被任何路径回滚（read_at 仍在）。
    let ds = cmx_database_pg::query_sql_with_params(
        TEST_DB_ID,
        None,
        "SELECT read_at IS NOT NULL AS r FROM cmx_flow_cc WHERE id = $1",
        SqlParams::DataValues(vec![DataValue::String(cc_id)]),
        "cas_cc_read_check",
    )
    .await
    .expect("查已读失败");
    let read = ds
        .iter()
        .next()
        .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "r") {
            Some(DataValue::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);
    assert!(read, "已读标记不得被旧快照 save 回滚（007 剥离动机本体）");
}

/// X3-T：转签台账 append-only 幂等（DO NOTHING）——重复 flush 不产生重复行。
#[tokio::test]
#[ignore = "需本地 PG：TEST_PG_URL=postgres://..."]
async fn delegation_append_is_idempotent() {
    let _guard = TEST_LOCK.lock().await;
    let Some(store) = setup_db().await else {
        eprintln!("TEST_PG_URL 未设置，跳过");
        return;
    };
    let id = format!("cas-dlg-{}", uuid::Uuid::new_v4());
    let mut s0 = snap(&id);
    let now = chrono::Utc::now();
    s0.delegations.push(cmx_flow_model::runtime::TaskDelegation {
        id: format!("{id}-d1"),
        task_id: format!("{id}-t1"),
        instance_id: id.clone(),
        kind: "TRANSFER".into(),
        from_user_id: "u1".into(),
        to_user_id: "u2".into(),
        temp_task_id: None,
        reason: None,
        created_at: now,
    });
    store.create_snapshot(&s0).await.expect("创建失败");

    // 连续两轮 load→save（每轮 save 的 ops 都全量 insert_delegation，ON CONFLICT DO NOTHING
    // 吸收同 id 重放）——幂等（不重复、不覆盖）。
    for _ in 0..2 {
        let loaded = store.load_snapshot(&id).await.expect("载入失败");
        store.save_snapshot(&loaded).await.expect("保存失败");
    }
    let ds = cmx_database_pg::query_sql_with_params(
        TEST_DB_ID,
        None,
        "SELECT COUNT(*) AS n FROM cmx_flow_task_delegation WHERE instance_id = $1",
        SqlParams::DataValues(vec![DataValue::String(id)]),
        "cas_dlg_count",
    )
    .await
    .expect("计数失败");
    let n = ds
        .iter()
        .next()
        .and_then(|row| match row.get_by_name(ds.schema.as_ref(), "n") {
            Some(DataValue::Int(v)) => Some(*v),
            _ => None,
        })
        .unwrap_or(-1);
    assert_eq!(n, 1, "台账重复 flush 不得重复行（ON CONFLICT DO NOTHING）");
}
