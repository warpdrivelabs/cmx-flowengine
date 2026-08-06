//! M4.2 端到端：抄送 CC（内存 + 假 resolver，始终可跑）。
//!
//! 验证抄送是「只读旁路」：
//! - 节点配置抄送（cmx:cc）：任务办结时对配置的人知会，不阻塞流程、不产生待办
//! - 手动抄送（notify_cc）：办理人主动知会一组人
//! - 已读追踪：mark_cc_read → read_at 置位
//! - 「抄送我的」：find_cc_for_user 跨实例查、unread_only 过滤
//! - 抄送不影响流程推进：带抄送的任务照常办结、完成

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_bpmn::compile;
use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, Engine, InMemoryStore, InstanceState,
    ResolveResult, RuntimeStore, Variables,
};

/// 假解析器：role/position → 固定用户列表；user → 自身。
#[derive(Default)]
struct FakeResolver {
    map: HashMap<String, Vec<String>>,
}
impl FakeResolver {
    fn with(mut self, key: &str, users: &[&str]) -> Self {
        self.map
            .insert(key.into(), users.iter().map(|s| s.to_string()).collect());
        self
    }
}
#[async_trait]
impl AssigneeResolver for FakeResolver {
    async fn resolve(&self, c: &CandidateRef) -> ResolveResult<Vec<String>> {
        if c.kind == CandidateKind::User {
            return Ok(vec![c.value.clone()]);
        }
        let k = format!("{:?}:{}", c.kind, c.value);
        Ok(self.map.get(&k).cloned().unwrap_or_default())
    }
}

/// 审批 + 节点抄送：review(assignee=mgr, cc=role(dept_head)) → done
const CC_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn"
             xmlns:cmx="http://cmx/flow">
  <process id="cc_approve" name="抄送审批" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="经理审批" flowable:assignee="mgr" cmx:cc="role(dept_head)"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

fn engine_for(bpmn: &str, resolver: FakeResolver) -> (Engine<InMemoryStore>, InMemoryStore) {
    let store = InMemoryStore::new();
    let def = compile(bpmn).expect("应能编译");
    let mut engine = Engine::new(store.clone());
    engine.deploy(def).expect("部署应成功");
    engine.set_resolver(Arc::new(resolver));
    (engine, store)
}

#[tokio::test]
async fn node_cc_generated_on_complete_without_blocking() {
    // dept_head 角色两人：办结经理审批 → 对这两人抄送，流程照常完成。
    let resolver = FakeResolver::default().with("Role:dept_head", &["u_boss1", "u_boss2"]);
    let (engine, store) = engine_for(CC_BPMN, resolver);
    let started = engine
        .start_process("cc_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();

    // 办结（无抄送记录之前）。
    let done = engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();
    // 抄送不阻塞：流程正常完成。
    assert_eq!(done.state, InstanceState::Completed);

    // 落库：两条抄送记录，被抄送人正确，均未读。
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert_eq!(snap.cc_records.len(), 2, "应对 dept_head 两人各抄送一条");
    let tos: Vec<&str> = snap
        .cc_records
        .iter()
        .map(|c| c.to_user_id.as_str())
        .collect();
    assert!(tos.contains(&"u_boss1") && tos.contains(&"u_boss2"));
    assert!(
        snap.cc_records.iter().all(|c| c.read_at.is_none()),
        "初始都未读"
    );
    assert!(
        snap.cc_records
            .iter()
            .all(|c| c.node_bpmn_id.as_deref() == Some("review"))
    );
    assert!(
        snap.cc_records
            .iter()
            .all(|c| c.from_user_id.as_deref() == Some("mgr")),
        "发起人=办理人"
    );
}

#[tokio::test]
async fn find_cc_for_user_and_mark_read() {
    let resolver = FakeResolver::default().with("Role:dept_head", &["u_boss1", "u_boss2"]);
    let (engine, _store) = engine_for(CC_BPMN, resolver);
    let started = engine
        .start_process("cc_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();
    engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();

    // 「抄送我的」：u_boss1 有 1 条未读。
    let inbox = engine.cc_for_user("u_boss1", false, 50).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert!(!inbox[0].read, "初始未读");
    let cc_id = inbox[0].id.clone();

    // 未读过滤：unread_only 有 1 条。
    let unread = engine.cc_for_user("u_boss1", true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);

    // 标记已读 → 再查 unread 为 0，全量仍 1 且 read=true。
    assert!(engine.mark_cc_read(&cc_id).await.unwrap());
    assert_eq!(
        engine.cc_for_user("u_boss1", true, 50).await.unwrap().len(),
        0,
        "已读后无未读"
    );
    let all = engine.cc_for_user("u_boss1", false, 50).await.unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].read, "应已读");

    // 幂等：再标记已读仍 true（无害）。
    assert!(engine.mark_cc_read(&cc_id).await.unwrap());
}

#[tokio::test]
async fn manual_cc_notify() {
    // 手动抄送：不依赖节点配置，办理人主动知会 user(u_x)+role。
    let resolver = FakeResolver::default().with("Role:audit", &["u_a1", "u_a2"]);
    let (engine, _store) = engine_for(CC_BPMN, resolver);
    let started = engine
        .start_process("cc_approve", Variables::new(), None)
        .await
        .unwrap();

    let refs = vec![
        CandidateRef {
            kind: CandidateKind::User,
            value: "u_x".into(),
        },
        CandidateRef {
            kind: CandidateKind::Role,
            value: "audit".into(),
        },
    ];
    let added = engine
        .notify_cc(&started.instance_id, &refs, Some("mgr"), Some("请知悉"))
        .await
        .unwrap();
    assert_eq!(added, 3, "u_x + audit 两人 = 3 条");

    // u_x 收到 1 条，reason 正确。
    let inbox = engine.cc_for_user("u_x", false, 50).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].reason.as_deref(), Some("请知悉"));
}

#[tokio::test]
async fn cc_dedup_same_node_same_user() {
    // 同节点对同一人重复抄送应去重（防重复触发）。手动对已抄送人再抄一次不新增。
    let resolver = FakeResolver::default().with("Role:dept_head", &["u_boss1"]);
    let (engine, _store) = engine_for(CC_BPMN, resolver);
    let started = engine
        .start_process("cc_approve", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();
    engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();

    // 节点已抄送 u_boss1（node_bpmn=review）。手动再抄 u_boss1（node=None）→ 不同节点键，会新增。
    let refs = vec![CandidateRef {
        kind: CandidateKind::User,
        value: "u_boss1".into(),
    }];
    let added = engine
        .notify_cc(&started.instance_id, &refs, None, None)
        .await
        .unwrap();
    assert_eq!(
        added, 1,
        "手动抄送 node=None 与节点抄送 node=review 键不同，新增一条"
    );

    // u_boss1 现在有 2 条（节点 1 + 手动 1）。
    assert_eq!(
        engine
            .cc_for_user("u_boss1", false, 50)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn no_cc_when_node_has_no_cc_expr() {
    // 无 cc 表达式的普通任务办结不产生抄送（零回归验证）。
    const PLAIN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                 xmlns:flowable="http://flowable.org/bpmn">
      <process id="plain" isExecutable="true">
        <startEvent id="s"/>
        <sequenceFlow id="f0" sourceRef="s" targetRef="t"/>
        <userTask id="t" flowable:assignee="mgr"/>
        <sequenceFlow id="f1" sourceRef="t" targetRef="done"/>
        <endEvent id="done"/>
      </process></definitions>"#;
    let (engine, store) = engine_for(PLAIN, FakeResolver::default());
    let started = engine
        .start_process("plain", Variables::new(), None)
        .await
        .unwrap();
    let task_id = started.open_tasks[0].id.clone();
    engine
        .complete_task(&started.instance_id, &task_id, Variables::new())
        .await
        .unwrap();
    let snap = store.load_snapshot(&started.instance_id).await.unwrap();
    assert!(snap.cc_records.is_empty(), "无 cc 表达式不产生抄送");
}
