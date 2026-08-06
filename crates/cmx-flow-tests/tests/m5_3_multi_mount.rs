//! M5.3 端到端：一个主流程「多处挂载」不同类型子流程（内存态，始终可跑）。
//!
//! 回答诉求：主流程下多处挂载、可按组织机构定义的不同类型子流程，是否成立。
//! 三种模式：
//! - 串行多挂载：start → callA(order_review) → callB(legal_review) → end
//!   两个不同类型子流程顺序展开；B 不早于 A 完成才启动；各自输出变量独立回写。
//! - 并行多挂载：fork ┬ callX(finance_sub) ┐ join
//!                    └ callY(risk_sub)     ┘
//!   两个不同类型子流程「同时」挂起（两子实例并存）；各自回链，join 汇聚。
//! - 组织路由多挂载：两挂载都用逻辑 key(cmx:calledKey)，同一组织把两个不同 called_key
//!   解析成两个不同具体子流程——直接兑现「按组织机构定义的不同类型子流程」。
//!
//! 关键不变量：父子链按「令牌」而非「实例」挂钩，故同一实例内多个挂载点互不串扰。
//! 断言一律按子实例 definition_key 归位（find_child_instances 返回顺序不保证）。
// 上方文档含 ASCII 流程图，缩进为示意对齐，非 Markdown 列表；放行 rustdoc 缩进 lint。
#![allow(clippy::doc_overindented_list_items)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    Engine, InMemoryStore, InstanceState, RouteError, RouteResult, RuntimeStore, SubflowRouter,
    TokenState, Variables,
};
use cmx_flow_model::ProcessInstance;
use serde_json::json;

/// 取某实例当前唯一未办结任务的 id。
async fn open_task(store: &InMemoryStore, iid: &str) -> String {
    let snap = store.load_snapshot(iid).await.unwrap();
    snap.tasks
        .iter()
        .find(|t| !t.completed)
        .unwrap_or_else(|| panic!("实例 {iid} 应有未办结任务"))
        .id
        .clone()
}

/// 把某主实例的全部子实例按 definition_key 归位（返回顺序不保证，必须按 key 取）。
async fn children_by_key(store: &InMemoryStore, main_id: &str) -> HashMap<String, ProcessInstance> {
    let children = store.find_child_instances(main_id).await.unwrap();
    let mut map = HashMap::new();
    for c in children {
        assert!(
            map.insert(c.definition_key.clone(), c.clone()).is_none(),
            "同一挂载类型 {} 出现重复子实例",
            c.definition_key
        );
    }
    map
}

/// 数某实例处于 WaitingSubflow 的令牌数（= 已挂起但等子流程回来的挂载点数）。
async fn waiting_subflow_count(store: &InMemoryStore, iid: &str) -> usize {
    store
        .load_snapshot(iid)
        .await
        .unwrap()
        .tokens
        .iter()
        .filter(|t| t.state == TokenState::WaitingSubflow)
        .count()
}

// ───────────────────────── 模式一：串行多挂载 ─────────────────────────

/// 主流程：start → callA(order_review) → callB(legal_review) → end
/// 两个挂载点各带独立输出映射，指向两个不同类型子流程。
const SERIAL_MAIN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="serial_main" name="串行多挂载主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="callA"/>
    <callActivity id="callA" name="订单复核" calledElement="order_review">
      <extensionElements>
        <flowable:out source="aResult" target="orderResult"/>
      </extensionElements>
    </callActivity>
    <sequenceFlow id="s1" sourceRef="callA" targetRef="callB"/>
    <callActivity id="callB" name="法务复核" calledElement="legal_review">
      <extensionElements>
        <flowable:out source="bResult" target="legalResult"/>
      </extensionElements>
    </callActivity>
    <sequenceFlow id="s2" sourceRef="callB" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

const ORDER_REVIEW: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="order_review" name="订单复核子流程" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="订单专员复核" flowable:assignee="order_clerk"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

const LEGAL_REVIEW: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="legal_review" name="法务复核子流程" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="法务顾问复核" flowable:assignee="counsel"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

#[tokio::test]
async fn serial_multi_mount_runs_two_different_subflows_in_order() {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    for xml in [SERIAL_MAIN, ORDER_REVIEW, LEGAL_REVIEW] {
        engine.deploy(compile(xml).unwrap()).unwrap();
    }
    let started = engine
        .start_process("serial_main", Variables::new(), Some("SER-1".into()))
        .await
        .unwrap();
    let main_id = started.instance_id.clone();

    // 起步即到挂载点 A → 只启动 order_review，主令牌 WaitingSubflow。挂载点 B 尚未触及。
    assert_eq!(waiting_subflow_count(&store, &main_id).await, 1);
    let c1 = children_by_key(&store, &main_id).await;
    assert_eq!(c1.len(), 1, "串行：此刻只有第一个挂载的子实例");
    assert!(c1.contains_key("order_review"), "挂载 A 应为 order_review");
    assert!(!c1.contains_key("legal_review"), "挂载 B 尚未启动");
    assert_eq!(c1["order_review"].state, InstanceState::Active);

    // 办结订单复核（带输出 aResult）→ order_review 完成 → 主推进到挂载点 B → 启动 legal_review。
    let a_sub = c1["order_review"].id.clone();
    let a_task = open_task(&store, &a_sub).await;
    let mut a_out = Variables::new();
    a_out.set("aResult", json!("order-ok"));
    engine.complete_task(&a_sub, &a_task, a_out).await.unwrap();

    let c2 = children_by_key(&store, &main_id).await;
    assert_eq!(c2.len(), 2, "串行：挂载 A 完成后挂载 B 的子实例出现");
    assert_eq!(c2["order_review"].state, InstanceState::Completed);
    assert_eq!(c2["legal_review"].state, InstanceState::Active);
    assert_eq!(
        waiting_subflow_count(&store, &main_id).await,
        1,
        "此刻挂在 B"
    );

    // 挂载 A 的输出已按 A 的映射独立回写主流程。
    let main_mid = store.load_snapshot(&main_id).await.unwrap();
    assert_eq!(
        main_mid.instance.variables.get("orderResult"),
        Some(&json!("order-ok")),
        "挂载 A 输出映射 aResult→orderResult 独立回写"
    );
    assert_eq!(
        main_mid.instance.variables.get("legalResult"),
        None,
        "挂载 B 尚未回写"
    );

    // 办结法务复核（带输出 bResult）→ legal_review 完成 → 主流程走到 end 完成。
    let b_sub = c2["legal_review"].id.clone();
    let b_task = open_task(&store, &b_sub).await;
    let mut b_out = Variables::new();
    b_out.set("bResult", json!("legal-ok"));
    engine.complete_task(&b_sub, &b_task, b_out).await.unwrap();

    let main_final = store.load_snapshot(&main_id).await.unwrap();
    assert_eq!(
        main_final.instance.state,
        InstanceState::Completed,
        "两挂载依次跑完 → 主流程完成"
    );
    // 两个挂载点的输出互不覆盖，各自归位。
    assert_eq!(
        main_final.instance.variables.get("orderResult"),
        Some(&json!("order-ok"))
    );
    assert_eq!(
        main_final.instance.variables.get("legalResult"),
        Some(&json!("legal-ok"))
    );
}

// ───────────────────────── 模式二：并行多挂载 ─────────────────────────

/// 主流程：start → fork ┬ callX(finance_sub) ┐ join → end
///                       └ callY(risk_sub)    ┘
/// 并行网关同时分叉出两个挂载点，两个不同类型子流程并存。
const PARALLEL_MAIN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="parallel_main" name="并行多挂载主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="fork"/>
    <parallelGateway id="fork" name="分发"/>
    <sequenceFlow id="s1" sourceRef="fork" targetRef="callX"/>
    <sequenceFlow id="s2" sourceRef="fork" targetRef="callY"/>
    <callActivity id="callX" name="财务子流程" calledElement="finance_sub"/>
    <callActivity id="callY" name="风控子流程" calledElement="risk_sub"/>
    <sequenceFlow id="s3" sourceRef="callX" targetRef="join"/>
    <sequenceFlow id="s4" sourceRef="callY" targetRef="join"/>
    <parallelGateway id="join" name="合流"/>
    <sequenceFlow id="s5" sourceRef="join" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

const FINANCE_SUB: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="finance_sub" name="财务子流程" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="财务审批" flowable:assignee="cfo"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

const RISK_SUB: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="risk_sub" name="风控子流程" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="风控评估" flowable:assignee="risk_officer"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#;

#[tokio::test]
async fn parallel_multi_mount_runs_two_different_subflows_concurrently() {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    for xml in [PARALLEL_MAIN, FINANCE_SUB, RISK_SUB] {
        engine.deploy(compile(xml).unwrap()).unwrap();
    }
    let started = engine
        .start_process("parallel_main", Variables::new(), Some("PAR-1".into()))
        .await
        .unwrap();
    let main_id = started.instance_id.clone();

    // 关键差异：fork 同时分叉 → 两个挂载点「同时」挂起 → 两个不同类型子实例并存。
    assert_eq!(
        waiting_subflow_count(&store, &main_id).await,
        2,
        "并行：两个挂载点同时 WaitingSubflow"
    );
    let c = children_by_key(&store, &main_id).await;
    assert_eq!(c.len(), 2, "并行：两个子实例同时存在");
    assert!(c.contains_key("finance_sub"), "挂载 X = finance_sub");
    assert!(c.contains_key("risk_sub"), "挂载 Y = risk_sub");
    assert_eq!(c["finance_sub"].state, InstanceState::Active);
    assert_eq!(c["risk_sub"].state, InstanceState::Active);

    // 先办结财务子流程 → finance_sub 完成，其令牌到 join 等待；主流程仍 Active（风控分支未到）。
    let fin_sub = c["finance_sub"].id.clone();
    let fin_task = open_task(&store, &fin_sub).await;
    engine
        .complete_task(&fin_sub, &fin_task, Variables::new())
        .await
        .unwrap();

    assert_eq!(
        store.load_snapshot(&fin_sub).await.unwrap().instance.state,
        InstanceState::Completed
    );
    let main_mid = store.load_snapshot(&main_id).await.unwrap();
    assert_eq!(
        main_mid.instance.state,
        InstanceState::Active,
        "只回来一个分支，主流程仍在合流处等待"
    );
    // 风控子流程不受影响，仍在办理。
    assert_eq!(
        store
            .load_snapshot(&c["risk_sub"].id)
            .await
            .unwrap()
            .instance
            .state,
        InstanceState::Active,
        "另一挂载子流程互不串扰，继续办理"
    );

    // 再办结风控子流程 → risk_sub 完成 → 两分支到齐 join → 主流程完成。
    let risk_sub_id = c["risk_sub"].id.clone();
    let risk_task = open_task(&store, &risk_sub_id).await;
    engine
        .complete_task(&risk_sub_id, &risk_task, Variables::new())
        .await
        .unwrap();

    assert_eq!(
        store
            .load_snapshot(&risk_sub_id)
            .await
            .unwrap()
            .instance
            .state,
        InstanceState::Completed
    );
    assert_eq!(
        store.load_snapshot(&main_id).await.unwrap().instance.state,
        InstanceState::Completed,
        "两并行挂载都回来 → join 放行 → 主流程完成"
    );
}

// ─────────────────── 模式三：组织路由多挂载（原始诉求）───────────────────

/// 假路由器：(called_key, org) → 具体子流程 key 的固定映射。
#[derive(Default)]
struct FakeRouter {
    map: HashMap<String, String>, // "key@org" → target
}
impl FakeRouter {
    fn bind(mut self, key: &str, org: &str, target: &str) -> Self {
        self.map.insert(format!("{key}@{org}"), target.into());
        self
    }
}
#[async_trait]
impl SubflowRouter for FakeRouter {
    async fn resolve(&self, called_key: &str, org_id: Option<&str>) -> RouteResult<String> {
        if let Some(org) = org_id
            && let Some(t) = self.map.get(&format!("{called_key}@{org}"))
        {
            return Ok(t.clone());
        }
        Err(RouteError::NoBinding {
            called_key: called_key.into(),
            org: org_id.map(|s| s.into()),
        })
    }
}

/// 主流程：start → callA(逻辑 key tier1_review) → callB(逻辑 key tier2_review) → end
/// 两挂载点都写逻辑 key，运行期按实例组织各自路由。
const ROUTED_MAIN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:cmx="http://cmx/flow">
  <process id="routed_multi_main" name="组织路由多挂载主流程" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="callA"/>
    <callActivity id="callA" name="一级复核" cmx:calledKey="tier1_review"/>
    <sequenceFlow id="s1" sourceRef="callA" targetRef="callB"/>
    <callActivity id="callB" name="二级复核" cmx:calledKey="tier2_review"/>
    <sequenceFlow id="s2" sourceRef="callB" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

/// 四个具体子流程：上海一级/二级 + 北京一级/二级，各一个 userTask 标记来源。
fn concrete_sub(id: &str, name: &str, assignee: &str) -> String {
    format!(
        r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="{id}" name="{name}" isExecutable="true">
    <startEvent id="s"/><sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
    <userTask id="t" name="{name}" flowable:assignee="{assignee}"/>
    <sequenceFlow id="f1" sourceRef="t" targetRef="e"/><endEvent id="e"/>
  </process></definitions>"#
    )
}

#[tokio::test]
async fn routed_multi_mount_resolves_two_types_by_org() {
    let store = InMemoryStore::new();
    let mut engine = Engine::new(store.clone());
    engine.deploy(compile(ROUTED_MAIN).unwrap()).unwrap();
    for (id, name, who) in [
        ("sh_tier1", "上海一级复核", "sh_l1"),
        ("sh_tier2", "上海二级复核", "sh_l2"),
        ("bj_tier1", "北京一级复核", "bj_l1"),
        ("bj_tier2", "北京二级复核", "bj_l2"),
    ] {
        engine
            .deploy(compile(&concrete_sub(id, name, who)).unwrap())
            .unwrap();
    }

    // 同一份主流程、两个挂载点逻辑 key；组织 shanghai 各自解析到上海的两个不同子流程类型。
    let router = FakeRouter::default()
        .bind("tier1_review", "shanghai", "sh_tier1")
        .bind("tier2_review", "shanghai", "sh_tier2")
        .bind("tier1_review", "beijing", "bj_tier1")
        .bind("tier2_review", "beijing", "bj_tier2");
    engine.set_subflow_router(Arc::new(router));

    // —— 上海发起 ——
    let sh = engine
        .start_process_org(
            "routed_multi_main",
            Variables::new(),
            Some("SH-1".into()),
            Some("shanghai".into()),
        )
        .await
        .unwrap();
    let sh_id = sh.instance_id.clone();

    // 挂载点 A 按上海组织路由到 sh_tier1（一个具体类型）。
    let a = children_by_key(&store, &sh_id).await;
    assert_eq!(a.len(), 1, "串行：先只有挂载 A 的子实例");
    assert!(a.contains_key("sh_tier1"), "挂载 A(tier1)@上海 → sh_tier1");

    // 办结一级 → 主推进到挂载点 B → 按上海组织路由到 sh_tier2（另一个具体类型）。
    let a_task = open_task(&store, &a["sh_tier1"].id).await;
    engine
        .complete_task(&a["sh_tier1"].id, &a_task, Variables::new())
        .await
        .unwrap();

    let b = children_by_key(&store, &sh_id).await;
    assert_eq!(b.len(), 2, "挂载 A 完成后挂载 B 子实例出现");
    assert_eq!(b["sh_tier1"].state, InstanceState::Completed);
    assert!(b.contains_key("sh_tier2"), "挂载 B(tier2)@上海 → sh_tier2");
    assert_eq!(b["sh_tier2"].state, InstanceState::Active);

    // 同一主流程实例、两个挂载点，按同一组织解析到两个不同类型子流程——诉求成立。
    // 办结二级 → 上海主流程完成。
    let b_task = open_task(&store, &b["sh_tier2"].id).await;
    engine
        .complete_task(&b["sh_tier2"].id, &b_task, Variables::new())
        .await
        .unwrap();
    assert_eq!(
        store.load_snapshot(&sh_id).await.unwrap().instance.state,
        InstanceState::Completed,
        "上海：两挂载各自路由并跑完 → 主流程完成"
    );

    // —— 北京发起：同一份主流程定义，两挂载解析到北京自己的两个子流程类型 ——
    let bj = engine
        .start_process_org(
            "routed_multi_main",
            Variables::new(),
            Some("BJ-1".into()),
            Some("beijing".into()),
        )
        .await
        .unwrap();
    let bj_id = bj.instance_id.clone();
    let bja = children_by_key(&store, &bj_id).await;
    assert!(
        bja.contains_key("bj_tier1"),
        "挂载 A(tier1)@北京 → bj_tier1（与上海不同类型）"
    );
    assert!(!bja.contains_key("sh_tier1"), "北京实例不应出现上海子流程");
}
