//! M2.5 端到端：PG 落库（边界定时器作业 + find_due_jobs + 中断触发归档，#[ignore] 门控）。
//!
//! 运行：
//!   TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
//!     cargo test -p cmx-flow-tests --test m2_5_pg -- --ignored --nocapture --test-threads=1
//!
//! 关键：引擎注入 TestClock，find_due_jobs 用引擎传入的 now（非 DB now），故 TestClock 的
//! 时间流逝驱动 PG 里的到期判定——确定性、无需真实等待。

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager, query_sql};
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InstanceState, TestClock, Variables};
use cmx_flow_model::RuntimeStore;
use cmx_flow_store_pg::PgRuntimeStore;

const INTERRUPTING_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="pg_timed_approve" name="PG限时审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="manager"/>
    <userTask id="manager" name="经理审批" flowable:assignee="经理"/>
    <sequenceFlow id="s1" sourceRef="manager" targetRef="done"/>
    <boundaryEvent id="timeout" attachedToRef="manager">
      <timerEventDefinition><timeDuration>PT24H</timeDuration></timerEventDefinition>
    </boundaryEvent>
    <sequenceFlow id="s2" sourceRef="timeout" targetRef="director"/>
    <userTask id="director" name="总监审批" flowable:assignee="总监"/>
    <sequenceFlow id="s3" sourceRef="director" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 17, 9, 0, 0).unwrap()
}

async fn setup_db() -> Option<String> {
    let url = std::env::var("TEST_PG_URL").ok()?;
    let db_id = "cmx_flow_m25_test".to_string();
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

async fn cleanup(db_id: &str, instance_id: &str) {
    for sql in [
        format!("DELETE FROM cmx_flow_hi_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_hi_instance WHERE id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_job WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_mi_scope WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_task WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_token WHERE instance_id = '{instance_id}'"),
        format!("DELETE FROM cmx_flow_instance WHERE id = '{instance_id}'"),
    ] {
        let _ = cmx_database_pg::execute_sql(db_id, None, &sql).await;
    }
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_boundary_timer_persists_and_fires_after_restart() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let def = compile(INTERRUPTING_BPMN).unwrap();
    let clock = TestClock::new(t0());

    // —— 进程 1：起流程，停在经理审批，定时器作业落库，然后「退出」 —— //
    let instance_id = {
        let mut e1 = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
        e1.deploy(def.clone()).unwrap();
        let started = e1
            .start_process(
                "pg_timed_approve",
                Variables::new(),
                Some("PG-TIMER-001".into()),
            )
            .await
            .expect("启动应成功");
        assert_eq!(started.open_tasks.len(), 1);

        // 作业落库校验。
        let jobs = query_sql(
            &db_id,
            None,
            &format!(
                "SELECT id, cancel_activity, due_at FROM cmx_flow_job WHERE instance_id = '{}'",
                started.instance_id
            ),
            "jobs",
        )
        .await
        .expect("查询作业失败");
        assert_eq!(jobs.row_count(), 1, "边界定时器作业应落库");
        started.instance_id
    };

    // —— 进程 2（全新 Engine，模拟重启）：拨快超时 → 从库触发 —— //
    let mut e2 = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    e2.deploy(def).unwrap();
    clock.advance(Duration::hours(25));
    let fired = e2.trigger_due_timers(100).await.unwrap();
    assert_eq!(fired.len(), 1, "重启后应从库触发定时器");
    assert!(fired[0].cancel_activity);

    // 运行态：升级到总监审批，作业清空。
    let snap = e2.store().load_snapshot(&instance_id).await.unwrap();
    let open: Vec<&str> = snap
        .tasks
        .iter()
        .filter(|t| !t.completed)
        .map(|t| t.node_bpmn_id.as_str())
        .collect();
    assert_eq!(open, vec!["director"], "应升级到总监审批");
    assert!(snap.jobs.is_empty(), "触发后作业应清空");

    // job 表也应无残留。
    let remain = query_sql(
        &db_id,
        None,
        &format!("SELECT id FROM cmx_flow_job WHERE instance_id = '{instance_id}'"),
        "remain",
    )
    .await
    .unwrap();
    assert_eq!(remain.row_count(), 0, "触发后 job 表应无残留");

    cleanup(&db_id, &instance_id).await;
}

#[tokio::test]
#[ignore = "需要本地 PostgreSQL，通过 TEST_PG_URL 提供"]
async fn pg_complete_before_timeout_removes_job_from_db() {
    let Some(db_id) = setup_db().await else {
        eprintln!("跳过：未设置 TEST_PG_URL");
        return;
    };
    let store = PgRuntimeStore::new(&db_id);
    store.ensure_schema().await.expect("建表应成功");
    let def = compile(INTERRUPTING_BPMN).unwrap();
    let clock = TestClock::new(t0());
    let mut engine = Engine::with_clock(store.clone(), Arc::new(clock.clone()));
    engine.deploy(def).unwrap();

    let started = engine
        .start_process(
            "pg_timed_approve",
            Variables::new(),
            Some("PG-TIMER-002".into()),
        )
        .await
        .unwrap();
    let task = started.open_tasks[0].clone();

    // 超时前办结 → 作业撤销 → job 表清空。
    engine
        .complete_task(&started.instance_id, &task.id, Variables::new())
        .await
        .unwrap();
    let jobs = query_sql(
        &db_id,
        None,
        &format!(
            "SELECT id FROM cmx_flow_job WHERE instance_id = '{}'",
            started.instance_id
        ),
        "jobs",
    )
    .await
    .unwrap();
    assert_eq!(jobs.row_count(), 0, "办结后 job 应从库撤销");

    // 拨快后 find_due_jobs 跨实例也查不到它。
    clock.advance(Duration::hours(25));
    let fired = engine.trigger_due_timers(100).await.unwrap();
    assert!(
        !fired.iter().any(|f| f.instance_id == started.instance_id),
        "已撤销作业不应被 find_due_jobs 命中"
    );

    // 完成实例应归档。
    let snap = engine
        .store()
        .load_snapshot(&started.instance_id)
        .await
        .unwrap();
    assert_eq!(snap.instance.state, InstanceState::Completed);

    cleanup(&db_id, &started.instance_id).await;
}
