//! M1 端到端：内存态流程推进（始终可跑，无外部依赖）。
//!
//! 验证核心不变量：
//! - serviceTask 的 delegate 同步执行并写回变量（计算 days）
//! - 令牌推进到 userTask 即停在等待态、外化为任务、落库（提交点）
//! - complete_task 恢复推进
//! - exclusiveGateway 按变量走条件边 / default
//! - 全部令牌 Ended → 实例 Completed

use std::sync::Arc;

use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    DelegateContext, Engine, InMemoryStore, InstanceState, JavaDelegate, Variables,
};
use cmx_flow_tests::LEAVE_REQUEST_BPMN;
use serde_json::json;

/// serviceTask delegate：把 hours 变量换算成 days 写回。
struct CalcDaysDelegate;

#[async_trait::async_trait]
impl JavaDelegate for CalcDaysDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String> {
        let hours = ctx
            .variables
            .get("hours")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let days = (hours / 8.0).ceil() as i64;
        ctx.variables.set("days", json!(days));
        Ok(())
    }
}

/// 组装一个部署好定义 + 注册好 delegate 的内存引擎。
fn build_engine() -> Engine<InMemoryStore> {
    let def = compile(LEAVE_REQUEST_BPMN).expect("样例流程应能编译");
    let mut engine = Engine::new(InMemoryStore::new());
    engine.deploy(def).expect("部署应成功");
    engine.register_delegate("calcDaysDelegate", CalcDaysDelegate);
    engine
}

#[tokio::test]
async fn short_leave_takes_default_branch_single_approval() {
    let engine = build_engine();

    // hours=16 → days=2 → 不 > 3 → 走 default（仅经理审批）。
    let mut vars = Variables::new();
    vars.set("hours", json!(16));
    let started = engine
        .start_process("leave_request", vars, Some("LR-001".into()))
        .await
        .expect("启动应成功");

    // 停在经理审批。
    assert_eq!(started.state, InstanceState::Active);
    assert_eq!(started.open_tasks.len(), 1);
    let review = &started.open_tasks[0];
    assert_eq!(review.node_bpmn_id, "review");
    assert_eq!(review.assignee.as_deref(), Some("manager"));

    // 经理办结 → 走 default → 直达结束。
    let done = engine
        .complete_task(&started.instance_id, &review.id, Variables::new())
        .await
        .expect("办结应成功");

    assert_eq!(done.state, InstanceState::Completed, "短假单次审批后应完成");
    assert!(done.open_tasks.is_empty(), "完成后不应有未办任务");
}

#[tokio::test]
async fn long_leave_takes_conditional_branch_two_approvals() {
    let engine = build_engine();

    // hours=40 → days=5 → > 3 → 需总监审批。
    let mut vars = Variables::new();
    vars.set("hours", json!(40));
    let started = engine
        .start_process("leave_request", vars, Some("LR-002".into()))
        .await
        .expect("启动应成功");

    // 第一关：经理审批。
    let review = started.open_tasks[0].clone();
    assert_eq!(review.node_bpmn_id, "review");
    let after_manager = engine
        .complete_task(&started.instance_id, &review.id, Variables::new())
        .await
        .expect("经理办结应成功");

    // 因 days=5 > 3，应进入总监审批，实例仍 Active。
    assert_eq!(after_manager.state, InstanceState::Active);
    assert_eq!(after_manager.open_tasks.len(), 1);
    let director = &after_manager.open_tasks[0];
    assert_eq!(director.node_bpmn_id, "director");
    assert_eq!(director.assignee.as_deref(), Some("director"));

    // 第二关：总监审批 → 结束。
    let done = engine
        .complete_task(&after_manager.instance_id, &director.id, Variables::new())
        .await
        .expect("总监办结应成功");
    assert_eq!(done.state, InstanceState::Completed, "长假两级审批后应完成");
    assert!(done.open_tasks.is_empty());
}

#[tokio::test]
async fn complete_at_gateway_can_be_overridden_by_completion_variables() {
    // 验证：userTask 完成时提交的变量能影响其后网关的分支判断。
    let engine = build_engine();

    // 启动时 hours=8 → days=1（default 分支）。但经理办结时改写 days=10。
    let mut vars = Variables::new();
    vars.set("hours", json!(8));
    let started = engine
        .start_process("leave_request", vars, None)
        .await
        .unwrap();
    let review = started.open_tasks[0].clone();

    // 办结时提交 days=10，覆盖实例变量 → 应改走总监分支。
    let mut completion = Variables::new();
    completion.set("days", json!(10));
    let after = engine
        .complete_task(&started.instance_id, &review.id, completion)
        .await
        .unwrap();

    assert_eq!(after.state, InstanceState::Active);
    assert_eq!(
        after.open_tasks[0].node_bpmn_id, "director",
        "办结时提交的 days=10 应驱动网关走总监分支"
    );
}

#[tokio::test]
async fn snapshot_persists_across_reload_from_store() {
    // 验证令牌等待态确实落进了 store：用一个独立 handle 载入快照检查。
    let store = InMemoryStore::new();
    let def = compile(LEAVE_REQUEST_BPMN).unwrap();
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).unwrap();
    engine.register_delegate("calcDaysDelegate", CalcDaysDelegate);

    let mut vars = Variables::new();
    vars.set("hours", json!(16));
    let started = engine
        .start_process("leave_request", vars, None)
        .await
        .unwrap();

    // 直接从 store 载入，应看到一个 Waiting 令牌停在 review + 一条未办任务。
    use cmx_flow_model::{RuntimeStore, TokenState};
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.tokens.len(), 1);
    assert_eq!(snap.tokens[0].state, TokenState::Waiting);
    assert_eq!(snap.tokens[0].node_bpmn_id, "review");
    assert_eq!(snap.tasks.iter().filter(|t| !t.completed).count(), 1);

    // days 应已由 serviceTask 写入变量并持久化。
    assert_eq!(
        snap.instance.variables.get("days").and_then(|v| v.as_i64()),
        Some(2)
    );

    // Arc 让编译器别抱怨未用（保持与真实消费一致的共享持有形态）。
    let _shared: Arc<()> = Arc::new(());
}
