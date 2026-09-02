//! 引擎派生变量历史（Next ①）：决策表输出 / 子流程回填 → `RuntimeStore::record_var_changes`。
//!
//! app 层 `diff_var_changes` 只捕获「调用方送入」的 start/complete/set-variables 三路径；引擎**内部**
//! 对实例变量的派生写入（businessRuleTask 决策输出 merge、callActivity 子流程 output 回填）此前不可见。
//! 本测试用一个包装 store 截获 `record_var_changes`，验证：
//!   ① 决策表输出 `approvalLevel` 作为一条 `source=decision` 的派生变更被记录；
//!   ② 子流程回填的输出变量作为 `source=subflow` 的派生变更被记录；
//!   ③ 未变化的 key 不产生噪声记录。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{Engine, InMemoryStore, RuntimeStore, Variables};
use cmx_flow_model::runtime::{
    ActivityRecord, AsyncJob, CcSummary, DeadLetterJob, DueJob, InstanceSnapshot, InstanceSummary,
    MessageSubscription, ProcessInstance, VarChangeRecord,
};
use cmx_flow_model::{DecisionRule, DecisionTable, HitPolicy, StoreResult};
use serde_json::json;

/// 包装 InMemoryStore，仅额外截获 record_var_changes 的入参供断言；其余方法透传。
#[derive(Clone)]
struct CapturingStore {
    inner: InMemoryStore,
    captured: Arc<Mutex<Vec<VarChangeRecord>>>,
}

impl CapturingStore {
    fn new() -> Self {
        Self {
            inner: InMemoryStore::new(),
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn captured(&self) -> Vec<VarChangeRecord> {
        self.captured.lock().unwrap().clone()
    }
}

#[async_trait]
impl RuntimeStore for CapturingStore {
    async fn create_snapshot(&self, s: &InstanceSnapshot) -> StoreResult<()> {
        self.inner.create_snapshot(s).await
    }
    async fn load_snapshot(&self, id: &str) -> StoreResult<InstanceSnapshot> {
        self.inner.load_snapshot(id).await
    }
    async fn save_snapshot(&self, s: &InstanceSnapshot) -> StoreResult<()> {
        self.inner.save_snapshot(s).await
    }
    async fn list_instances(&self, limit: usize) -> StoreResult<Vec<InstanceSummary>> {
        self.inner.list_instances(limit).await
    }
    async fn acquire_due_jobs(
        &self,
        worker_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        lease_secs: i64,
        limit: usize,
    ) -> StoreResult<Vec<DueJob>> {
        self.inner.acquire_due_jobs(worker_id, now, lease_secs, limit).await
    }
    async fn find_cc_for_user(
        &self,
        u: &str,
        unread: bool,
        limit: usize,
    ) -> StoreResult<Vec<CcSummary>> {
        self.inner.find_cc_for_user(u, unread, limit).await
    }
    async fn mark_cc_read(
        &self,
        id: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> StoreResult<bool> {
        self.inner.mark_cc_read(id, at).await
    }
    async fn find_child_instances(&self, pid: &str) -> StoreResult<Vec<ProcessInstance>> {
        self.inner.find_child_instances(pid).await
    }
    async fn upsert_message_subscription(&self, sub: &MessageSubscription) -> StoreResult<()> {
        self.inner.upsert_message_subscription(sub).await
    }
    async fn find_catch_subscription(
        &self,
        m: &str,
        c: Option<&str>,
        t: &str,
    ) -> StoreResult<Option<MessageSubscription>> {
        self.inner.find_catch_subscription(m, c, t).await
    }
    async fn find_start_subscription(
        &self,
        m: &str,
        t: &str,
    ) -> StoreResult<Option<MessageSubscription>> {
        self.inner.find_start_subscription(m, t).await
    }
    async fn delete_message_subscription(&self, id: &str) -> StoreResult<()> {
        self.inner.delete_message_subscription(id).await
    }
    async fn delete_subscriptions_by_instance(&self, id: &str) -> StoreResult<()> {
        self.inner.delete_subscriptions_by_instance(id).await
    }
    async fn delete_start_subscriptions_by_def(&self, k: &str) -> StoreResult<()> {
        self.inner.delete_start_subscriptions_by_def(k).await
    }
    async fn upsert_async_job(&self, j: &AsyncJob) -> StoreResult<()> {
        self.inner.upsert_async_job(j).await
    }
    async fn acquire_async_jobs(
        &self,
        w: &str,
        tf: Option<&str>,
        ls: i64,
        limit: usize,
    ) -> StoreResult<Vec<AsyncJob>> {
        self.inner.acquire_async_jobs(w, tf, ls, limit).await
    }
    async fn complete_async_job(
        &self,
        id: &str,
        rv: Option<serde_json::Value>,
    ) -> StoreResult<Option<(String, String)>> {
        self.inner.complete_async_job(id, rv).await
    }
    async fn fail_async_job(&self, id: &str, e: &str) -> StoreResult<bool> {
        self.inner.fail_async_job(id, e).await
    }
    async fn delete_async_jobs_by_instance(&self, id: &str) -> StoreResult<()> {
        self.inner.delete_async_jobs_by_instance(id).await
    }
    async fn get_async_job(&self, id: &str) -> StoreResult<Option<AsyncJob>> {
        self.inner.get_async_job(id).await
    }
    async fn upsert_dead_letter_job(&self, j: &DeadLetterJob) -> StoreResult<()> {
        self.inner.upsert_dead_letter_job(j).await
    }
    async fn list_dead_letter_jobs(&self, limit: usize) -> StoreResult<Vec<DeadLetterJob>> {
        self.inner.list_dead_letter_jobs(limit).await
    }
    async fn get_dead_letter_job(&self, id: &str) -> StoreResult<Option<DeadLetterJob>> {
        self.inner.get_dead_letter_job(id).await
    }
    async fn delete_dead_letter_job(&self, id: &str) -> StoreResult<()> {
        self.inner.delete_dead_letter_job(id).await
    }
    async fn upsert_hi_activity(&self, a: &ActivityRecord) -> StoreResult<()> {
        self.inner.upsert_hi_activity(a).await
    }
    async fn list_activities_by_instance(
        &self,
        id: &str,
    ) -> StoreResult<Vec<ActivityRecord>> {
        self.inner.list_activities_by_instance(id).await
    }
    // 被测方法：截获后仍透传（透传是 no-op 默认，无副作用，但保持契约完整）。
    async fn record_var_changes(&self, changes: &[VarChangeRecord]) -> StoreResult<()> {
        self.captured.lock().unwrap().extend_from_slice(changes);
        self.inner.record_var_changes(changes).await
    }
}

// ───────────────────────── ① 决策输出派生 ─────────────────────────

const RULE_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="rule_flow" name="决策审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="rule"/>
    <businessRuleTask id="rule" name="定审批级别" flowable:decisionRef="approval_matrix"/>
    <sequenceFlow id="s1" sourceRef="rule" targetRef="high"/>
    <userTask id="high" name="审批" flowable:assignee="board"/>
    <sequenceFlow id="h1" sourceRef="high" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn approval_matrix() -> DecisionTable {
    let rule = |cond: &str, level: i64| DecisionRule {
        conditions: vec![cond.to_string()],
        outputs: {
            let mut m = BTreeMap::new();
            m.insert("approvalLevel".to_string(), json!(level));
            m
        },
    };
    DecisionTable {
        key: "approval_matrix".into(),
        inputs: vec!["amount".into()],
        outputs: vec!["approvalLevel".into()],
        hit_policy: HitPolicy::First,
        rules: vec![rule("amount > 100000", 3), rule("-", 1)],
    }
}

#[tokio::test]
async fn decision_output_captured_as_derived_var_change() {
    let store = CapturingStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(RULE_FLOW).unwrap()).unwrap();
    engine.register_decision(approval_matrix());

    let mut vars = Variables::new();
    vars.set("amount", json!(500000));
    engine
        .start_process("rule_flow", vars, Some("R-1".into()))
        .await
        .unwrap();

    let cap = store.captured();
    let hit = cap
        .iter()
        .find(|c| c.var_name == "approvalLevel")
        .expect("决策输出 approvalLevel 应被记录为派生变更");
    assert_eq!(hit.source, "decision", "来源应标记 decision");
    assert_eq!(hit.node_bpmn_id.as_deref(), Some("rule"), "应记在 businessRuleTask 节点");
    assert_eq!(hit.old_value, None, "首次写入 old 为空");
    assert_eq!(hit.new_value.as_deref(), Some("3"), "新值为决策输出 3");
}

#[tokio::test]
async fn decision_unchanged_value_produces_no_noise() {
    // amount 已是 500000 且流程再无写入；approvalLevel 首次写入必记，但重复求值同值不应重复记。
    // 这里用单节点流程验证「只记真实变化」：approvalLevel 由无到 3 记一条，仅此一条 decision 记录。
    let store = CapturingStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(RULE_FLOW).unwrap()).unwrap();
    engine.register_decision(approval_matrix());
    let mut vars = Variables::new();
    vars.set("amount", json!(500000));
    engine
        .start_process("rule_flow", vars, None)
        .await
        .unwrap();
    let decision_records: Vec<_> = store
        .captured()
        .into_iter()
        .filter(|c| c.source == "decision")
        .collect();
    assert_eq!(decision_records.len(), 1, "仅一条真实决策派生变更，无重复噪声");
}

// ───────────────────────── ② 子流程回填派生 ─────────────────────────

const PARENT_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="vh_parent" name="父" isExecutable="true">
    <startEvent id="ps"/>
    <sequenceFlow id="pf0" sourceRef="ps" targetRef="call"/>
    <callActivity id="call" name="子" calledElement="vh_child">
      <extensionElements>
        <flowable:out source="childOut" target="backVar"/>
      </extensionElements>
    </callActivity>
    <sequenceFlow id="pf1" sourceRef="call" targetRef="pe"/>
    <endEvent id="pe"/>
  </process>
</definitions>"#;

// 子流程：起点→serviceTask 无（保持无等待态秒过）；childOut 由父继承传入（in 映射），原样 out 回填。
const CHILD_FLOW: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="vh_child" name="子体" isExecutable="true">
    <startEvent id="cs"/>
    <sequenceFlow id="cf0" sourceRef="cs" targetRef="ce"/>
    <endEvent id="ce"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn subflow_backfill_captured_as_derived_var_change() {
    let store = CapturingStore::new();
    let engine = Engine::new(store.clone());
    engine.deploy(compile(PARENT_FLOW).unwrap()).unwrap();
    engine.deploy(compile(CHILD_FLOW).unwrap()).unwrap();

    // 父带 childOut=7；子流程整体继承 → 回填 out 映射 childOut→backVar，父得 backVar=7（派生）。
    let mut vars = Variables::new();
    vars.set("childOut", json!(7));
    engine
        .start_process("vh_parent", vars, Some("P-1".into()))
        .await
        .unwrap();

    let sub: Vec<_> = store
        .captured()
        .into_iter()
        .filter(|c| c.source == "subflow")
        .collect();
    let hit = sub
        .iter()
        .find(|c| c.var_name == "backVar")
        .expect("子流程回填 backVar 应被记录为 source=subflow 派生变更");
    assert_eq!(hit.node_bpmn_id.as_deref(), Some("call"), "应记在 callActivity 节点");
    assert_eq!(hit.new_value.as_deref(), Some("7"), "回填值为子流程输出 7");
}
