//! 出站生命周期 webhook（方案 §4④ 的出站半）：flow 把实例/任务生命周期事件通知目标服务。
//!
//! 设计（用户定）：**app 层发**（handler 调引擎成功后 emit，引擎零改）+ **后台异步 + 重试**
//! （事件入内存队列，后台 task 逐个发，失败指数退避重试若干次，不阻塞 handler 响应）。
//!
//! **对外回调契约（自包含，无 SDK）**：订阅方（mdm 或任意外部/三方系统）不会共享本仓的
//! Rust crate，契约只存在于 HTTP 层——**文档是真源**（`docs/usage/08-external-integration.md`
//! §8.5），双端各自实现、集成测试对拍。这与服务间调用（内部微服务，cmx-service-rpc 基座 +
//! 标准信封）是两类东西：
//!   - 目标 = 服务目录键 + 回调路径（`FLOW_WEBHOOK_TARGETS` 条目 `键:路径`，如
//!     `mdm:/api/mdm/flow/callback`；键经 `[service_rpc.services]` 定位）；
//!   - 安全 = 每条请求带 [`SIGNATURE_HEADER`]：`sha256=<hex(HMAC-SHA256(body, signing_key))>`
//!     （对实际发送字节签名，接收端按共享密钥验签防伪造）+ 事件名 / 幂等投递头；
//!   - 成功判定 = **HTTP 2xx**（接收方协议不受 CMX 信封约束，响应体不解析）。
//!
//! 传输复用 cmx-service-rpc 基座（目录定位 / 超时 / 熔断），仅取其裸响应层（`execute`）。

use crate::config::WebhookTarget;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::mpsc;

use cmx_service_rpc::{RpcRequest, ServiceRpcHandle};

/// HMAC 签名头（值形如 `sha256=<hex(HMAC-SHA256(body, secret))>`，对 body 原始字节计算）。
pub const SIGNATURE_HEADER: &str = "x-cmx-flow-signature";

/// 事件名头（载荷 `event` 字段的冗余副本，便于接收方路由 / 过滤）。
pub const EVENT_HEADER: &str = "x-cmx-flow-event";

/// 投递幂等头（`{instanceId}-{occurredAt}`，接收方可据此去重）。
pub const DELIVERY_HEADER: &str = "x-cmx-flow-delivery";

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

/// 一条出站事件（app 层构造，塞进队列；camelCase wire DTO，即对外契约的载荷格式）。
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

    /// 投递幂等 id（`{instanceId}-{occurredAt}`，进 [`DELIVERY_HEADER`]）。
    pub fn delivery_id(&self) -> String {
        format!("{}-{}", self.instance_id, self.occurred_at)
    }
}

/// HMAC-SHA256(body, secret) → 签名头值 `sha256=<hex>`（小写 hex；对实际发送字节计算）。
pub fn sign_body(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC 接受任意长度密钥");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
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
    /// - `targets`：目标列表（服务目录键 + 回调路径，每事件投递给全部目标）。
    /// - `signing_key`：HMAC 签名密钥（须与接收端共享密钥一致）。
    /// - `max_retries`：单条单目标失败重试次数（指数退避 1s/2s/4s…）。
    ///
    /// 传输经全局 service_rpc 基座（装配链更早处 `init_infra` 已初始化）；基座未初始化时
    /// 降级 disabled（warn）——webhook 是通知非关键路径，不值得让它阻断服务启动。
    pub fn spawn_worker(
        targets: Vec<WebhookTarget>,
        signing_key: Option<String>,
        max_retries: u32,
    ) -> Self {
        if targets.is_empty() {
            return Self::disabled();
        }
        let Some(rpc) = cmx_service_rpc::global_arc() else {
            tracing::warn!("webhook 已配目标但 service_rpc 基座未初始化，webhook 关闭");
            return Self::disabled();
        };
        let (tx, mut rx) = mpsc::channel::<FlowEvent>(1024);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                for target in &targets {
                    deliver(&rpc, target, &event, signing_key.as_deref().unwrap_or(""), max_retries)
                        .await;
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

/// 投递一条事件到一个目标：序列化 → 签名（对实际发送字节）→ 基座裸 POST → 失败指数退避重试。
///
/// 成功判定 = HTTP 2xx（`execute` 取原始响应，不解析 body——接收方是任意外部系统，
/// 不受 CMX 信封约束）；非 2xx / 传输错误进重试。
async fn deliver(
    rpc: &ServiceRpcHandle,
    target: &WebhookTarget,
    event: &FlowEvent,
    secret: &str,
    max_retries: u32,
) {
    // body 用紧凑 JSON；签名对 body 字节，必须与发出的字节一致（Raw body 保证）。
    let body = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(event = %event.event, error = %e, "webhook 事件序列化失败，丢弃");
            return;
        }
    };
    let mut attempt = 0u32;
    loop {
        let req = RpcRequest::post(target.key.clone(), target.path.clone())
            .raw_body(body.clone(), "application/json")
            .header(EVENT_HEADER, event.event.clone())
            .header(DELIVERY_HEADER, event.delivery_id())
            .header(SIGNATURE_HEADER, sign_body(secret, &body));
        match rpc.execute(req).await {
            Ok(_) => {
                tracing::debug!(event = %event.event, key = %target.key, "webhook 投递成功");
                return;
            }
            Err(e) => {
                tracing::warn!(event = %event.event, key = %target.key, attempt, error = %e, "webhook 投递失败");
            }
        }
        if attempt >= max_retries {
            tracing::warn!(event = %event.event, key = %target.key, "webhook 重试耗尽，放弃");
            return;
        }
        // 指数退避：1s, 2s, 4s…
        let backoff = std::time::Duration::from_secs(1u64 << attempt.min(6));
        tokio::time::sleep(backoff).await;
        attempt += 1;
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

    #[test]
    fn event_kind_names() {
        assert_eq!(FlowEventKind::InstanceStarted.as_str(), "instance.started");
        assert_eq!(FlowEventKind::TaskReassigned.as_str(), "task.reassigned");
    }

    /// wire 形状：camelCase + None 字段不上线 + 幂等 id。
    #[test]
    fn event_wire_shape() {
        let ev = FlowEvent::new(FlowEventKind::TaskCompleted, "i-1", "2026-08-31T10:00:00Z".into())
            .state(Some("COMPLETED".into()))
            .definition_key(Some("mdm_cr_approval".into()))
            .task(Some("t-9".into()), Some("review_1".into()))
            .assignee(Some("u1".into()));
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["event"], "task.completed");
        assert_eq!(v["instanceId"], "i-1");
        assert_eq!(v["nodeBpmnId"], "review_1");
        assert_eq!(v["occurredAt"], "2026-08-31T10:00:00Z");
        assert!(v.get("businessKey").is_none(), "None 字段不上线");
        assert_eq!(ev.delivery_id(), "i-1-2026-08-31T10:00:00Z");
    }

    /// 签名形状与稳定性：`sha256=` 前缀 + 64 位小写 hex；同 body/密钥稳定，密钥变则变。
    #[test]
    fn sign_body_shape() {
        let body = br#"{"event":"instance.completed","instanceId":"i-1"}"#;
        let sig = sign_body("secret", body);
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), "sha256=".len() + 64);
        assert_eq!(sig, sign_body("secret", body));
        assert_ne!(sig, sign_body("other", body));
        assert_ne!(sig, sign_body("secret", b"other"));
    }
}
