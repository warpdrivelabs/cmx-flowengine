//! 出站生命周期 webhook（方案 §4④ 的出站半）：flow 把实例/任务生命周期事件通知目标服务。
//!
//! 设计（用户定）：**app 层发**（handler 调引擎成功后 emit，引擎零改）+ **后台异步 + 重试**
//! （事件入内存队列，后台 task 逐个发，失败指数退避重试若干次，不阻塞 handler 响应）。
//!
//! 投递经 `cmx-mdm-sdk` 契约（`POST /api/mdm/flow/callback`）：目标 = 服务目录键
//! （`FLOW_WEBHOOK_TARGETS`，`[service_rpc.services]` 定位）；安全 = 每条请求带
//! `x-cmx-flow-signature: sha256=<hex(HMAC-SHA256(body, signing_key))>`（对实际发送字节签名，
//! 接收端按共享密钥验签防伪造）+ 事件名 / 幂等投递头。签名与信封契约两端同源（cmx-mdm-sdk）。

use serde::Serialize;
use tokio::sync::mpsc;

/// 生命周期事件类型（对齐方案 §6 SSE 命名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEventKind {
    InstanceStarted,
    InstanceCompleted,
    InstanceTerminated,
    TaskCreated,
    TaskCompleted,
    TaskReassigned,
}

impl FlowEventKind {
    /// 事件名（进 payload.event + X-Cmx-Flow-Event 头）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstanceStarted => "instance.started",
            Self::InstanceCompleted => "instance.completed",
            Self::InstanceTerminated => "instance.terminated",
            Self::TaskCreated => "task.created",
            Self::TaskCompleted => "task.completed",
            Self::TaskReassigned => "task.reassigned",
        }
    }
}

/// 一条出站事件（app 层构造，塞进队列）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowEvent {
    /// 事件名（instance.started 等）。
    pub event: String,
    /// 实例 id。
    pub instance_id: String,
    /// 实例状态（ACTIVE/COMPLETED/…，可空）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// 定义 key（可空）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_key: Option<String>,
    /// 业务键（可空）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_key: Option<String>,
    /// 任务 id（task.* 事件带）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// 节点 bpmn_id（task.* 事件带）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_bpmn_id: Option<String>,
    /// 办理人（task.created/reassigned 带）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// 租户（S3 SSE 按此过滤，只推本租户事件；单租户为 "default"）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// 事件时间（RFC3339）。
    pub occurred_at: String,
}

impl FlowEvent {
    /// 用事件类型 + 实例 id 起一条（其余字段链式补）。
    pub fn new(kind: FlowEventKind, instance_id: impl Into<String>, occurred_at: String) -> Self {
        Self {
            event: kind.as_str().to_string(),
            instance_id: instance_id.into(),
            state: None,
            definition_key: None,
            business_key: None,
            task_id: None,
            node_bpmn_id: None,
            assignee: None,
            tenant: None,
            occurred_at,
        }
    }
    pub fn state(mut self, v: Option<String>) -> Self {
        self.state = v;
        self
    }
    pub fn definition_key(mut self, v: Option<String>) -> Self {
        self.definition_key = v;
        self
    }
    pub fn business_key(mut self, v: Option<String>) -> Self {
        self.business_key = v;
        self
    }
    pub fn task(mut self, task_id: Option<String>, node_bpmn_id: Option<String>) -> Self {
        self.task_id = task_id;
        self.node_bpmn_id = node_bpmn_id;
        self
    }
    pub fn assignee(mut self, v: Option<String>) -> Self {
        self.assignee = v;
        self
    }
    pub fn tenant(mut self, v: Option<String>) -> Self {
        self.tenant = v;
        self
    }
}

/// webhook 发送器句柄：app 层持有它，`emit` 非阻塞入队。
///
/// 内部一个 mpsc 队列 + 一个后台 task 消费（`spawn_worker` 起）。队列满或无订阅端时 emit 静默丢弃
/// （webhook 是通知，非关键路径——不阻塞业务、不因第三方拖垮 flow）。
#[derive(Clone)]
pub struct WebhookSender {
    tx: Option<mpsc::Sender<FlowEvent>>,
}

impl WebhookSender {
    /// 空发送器（webhook 关闭时用；emit 是 no-op）。
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// 起一个后台发送 worker，返回句柄。
    ///
    /// - `targets`：目标服务键列表（`[service_rpc.services]` 的键，每事件投递给全部目标）。
    /// - `signing_key`：HMAC 签名密钥（须与接收端共享密钥一致）。
    /// - `max_retries`：单条单目标失败重试次数（指数退避 1s/2s/4s…）。
    pub fn spawn_worker(targets: Vec<String>, signing_key: Option<String>, max_retries: u32) -> Self {
        if targets.is_empty() {
            return Self::disabled();
        }
        let (tx, mut rx) = mpsc::channel::<FlowEvent>(1024);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                for target in &targets {
                    deliver(target, &event, signing_key.as_deref(), max_retries).await;
                }
            }
        });
        Self { tx: Some(tx) }
    }

    /// 非阻塞发一条事件。队列满/无订阅端则丢弃（只 warn）。
    pub fn emit(&self, event: FlowEvent) {
        if let Some(tx) = &self.tx {
            if let Err(e) = tx.try_send(event) {
                tracing::warn!(error = %e, "webhook 事件入队失败（队列满/关闭），丢弃");
            }
        }
    }

    /// 是否启用（有后台 worker）。
    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }
}

/// 投递一条事件到一个目标服务键：契约投递（cmx-mdm-sdk：序列化 → 签名 → POST 回调端点）→
/// 失败指数退避重试（重试/退避策略归本发送器；SDK 只做单次投递）。
async fn deliver(
    target: &str,
    event: &FlowEvent,
    signing_key: Option<&str>,
    max_retries: u32,
) {
    let sdk_event = cmx_mdm_sdk::FlowEvent::from(event.clone());
    let secret = signing_key.unwrap_or("");
    let mut attempt = 0u32;
    loop {
        match cmx_mdm_sdk::deliver_flow_event(&sdk_event, secret).await {
            Ok(()) => {
                tracing::debug!(event = %event.event, target, "webhook 投递成功");
                return;
            }
            Err(e) => {
                tracing::warn!(event = %event.event, target, attempt, error = %e, "webhook 投递失败");
            }
        }
        if attempt >= max_retries {
            tracing::warn!(event = %event.event, target, "webhook 重试耗尽，放弃");
            return;
        }
        // 指数退避：1s, 2s, 4s…
        let backoff = std::time::Duration::from_secs(1u64 << attempt.min(6));
        tokio::time::sleep(backoff).await;
        attempt += 1;
    }
}

/// 内部事件 → 契约事件（wire DTO 同构，字段一一对应）。
impl From<FlowEvent> for cmx_mdm_sdk::FlowEvent {
    fn from(e: FlowEvent) -> Self {
        Self {
            event: e.event,
            instance_id: e.instance_id,
            state: e.state,
            definition_key: e.definition_key,
            business_key: e.business_key,
            task_id: e.task_id,
            node_bpmn_id: e.node_bpmn_id,
            assignee: e.assignee,
            tenant: e.tenant,
            occurred_at: e.occurred_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_sender_emit_is_noop() {
        let s = WebhookSender::disabled();
        assert!(!s.is_enabled());
        s.emit(FlowEvent::new(FlowEventKind::InstanceStarted, "i1", "t".into())); // 不 panic
    }

    /// 内部事件 → 契约 DTO 字段对齐（wire 同构，防漂移）。
    #[test]
    fn sdk_event_conversion_aligns() {
        let ev = FlowEvent::new(FlowEventKind::TaskCompleted, "i-1", "t0".into())
            .state(Some("COMPLETED".into()))
            .definition_key(Some("mdm_cr_approval".into()))
            .business_key(Some("CR-1".into()))
            .task(Some("t-9".into()), Some("review_1".into()))
            .assignee(Some("u1".into()))
            .tenant(Some("default".into()));
        let sdk = cmx_mdm_sdk::FlowEvent::from(ev);
        assert_eq!(sdk.event, "task.completed");
        assert_eq!(sdk.instance_id, "i-1");
        assert_eq!(sdk.state.as_deref(), Some("COMPLETED"));
        assert_eq!(sdk.definition_key.as_deref(), Some("mdm_cr_approval"));
        assert_eq!(sdk.task_id.as_deref(), Some("t-9"));
        assert_eq!(sdk.node_bpmn_id.as_deref(), Some("review_1"));
        assert_eq!(sdk.occurred_at, "t0");
    }

    #[test]
    fn event_kind_names() {
        assert_eq!(FlowEventKind::InstanceStarted.as_str(), "instance.started");
        assert_eq!(FlowEventKind::TaskReassigned.as_str(), "task.reassigned");
    }
}
