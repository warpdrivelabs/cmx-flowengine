//! 出站生命周期 webhook（方案 §4④ 的出站半）：flow 把实例/任务生命周期事件 POST 通知第三方。
//!
//! 设计（用户定）：**app 层发**（handler 调引擎成功后 emit，引擎零改）+ **后台异步 + 重试**
//! （事件入内存队列，后台 task 逐个发，失败指数退避重试若干次，不阻塞 handler 响应）。
//!
//! 安全：每条请求带 `X-Cmx-Flow-Signature: sha256=<hex(HMAC-SHA256(body, signing_key))>` +
//! `X-Cmx-Flow-Event`/`X-Cmx-Flow-Delivery`，第三方按共享密钥验签防伪造。

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use tokio::sync::mpsc;

type HmacSha256 = Hmac<Sha256>;

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
    /// - `urls`：目标 URL 列表（每事件都 POST 给全部 URL）。
    /// - `signing_key`：HMAC 签名密钥（空则不签名）。
    /// - `max_retries`：单条单 URL 失败重试次数（指数退避 1s/2s/4s…）。
    pub fn spawn_worker(urls: Vec<String>, signing_key: Option<String>, max_retries: u32) -> Self {
        if urls.is_empty() {
            return Self::disabled();
        }
        let (tx, mut rx) = mpsc::channel::<FlowEvent>(1024);
        let http = reqwest::Client::new();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                for url in &urls {
                    deliver(&http, url, &event, signing_key.as_deref(), max_retries).await;
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

/// 投递一条事件到一个 URL：签名 → POST → 失败指数退避重试。
async fn deliver(
    http: &reqwest::Client,
    url: &str,
    event: &FlowEvent,
    signing_key: Option<&str>,
    max_retries: u32,
) {
    // body 用紧凑 JSON（签名对 body 字节，必须与发出的字节一致）。
    let body = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "webhook 事件序列化失败，跳过");
            return;
        }
    };
    let signature = signing_key.map(|k| sign(&body, k));
    let delivery_id = format!("{}-{}", event.instance_id, event.occurred_at);

    let mut attempt = 0u32;
    loop {
        let mut req = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-cmx-flow-event", &event.event)
            .header("x-cmx-flow-delivery", &delivery_id)
            .body(body.clone());
        if let Some(sig) = &signature {
            req = req.header("x-cmx-flow-signature", format!("sha256={sig}"));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(event = %event.event, url, "webhook 投递成功");
                return;
            }
            Ok(resp) => {
                tracing::warn!(event = %event.event, url, status = %resp.status(), attempt, "webhook 非 2xx");
            }
            Err(e) => {
                tracing::warn!(event = %event.event, url, error = %e, attempt, "webhook 请求失败");
            }
        }
        if attempt >= max_retries {
            tracing::warn!(event = %event.event, url, "webhook 重试耗尽，放弃");
            return;
        }
        // 指数退避：1s, 2s, 4s…
        let backoff = std::time::Duration::from_secs(1u64 << attempt.min(6));
        tokio::time::sleep(backoff).await;
        attempt += 1;
    }
}

/// HMAC-SHA256(body, key) → hex。
fn sign(body: &[u8], key: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC 接受任意长度密钥");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_hex() {
        let body = br#"{"event":"instance.started"}"#;
        let s = sign(body, "secret");
        // HMAC-SHA256 → 64 hex chars，确定性。
        assert_eq!(s.len(), 64);
        assert_eq!(s, sign(body, "secret"));
        assert_ne!(s, sign(body, "other"));
    }

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
}
