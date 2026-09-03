//! 出站生命周期 webhook 的**对外契约层**（三契约头 + 事件载荷 + HMAC 签名 + HTTP 2xx 判定）。
//!
//! 001-M3：legacy 内存链路（mpsc 队列 + 后台串行 worker + 指数退避重试）已删除，
//! 投递统一走 outbox 持久化管线（`cmx-flow-app/src/event_outbox.rs`，租约抢占 +
//! 同订阅保序 + 死信处置）；本文件只保留**契约自包含部分**，供 `channel_webhook`
//! 组装请求复用。契约文档真源：`docs/usage/08-external-integration.md` §8.5/§8.6：
//!   - 目标 = 服务目录键 + 回调路径（订阅 `channel_config` 的 service_key/callback_path，
//!     键经 `[service_rpc.services]` 目录定位），或 `target_url` 完整 URL 直连
//!     （外部系统形态，不经目录；`channel_webhook` 双模式）；
//!   - 安全 = 每条请求带 [`SIGNATURE_HEADER`]：`sha256=<hex(HMAC-SHA256(body, secret))>`
//!     （对实际发送字节签名，接收端按订阅独立 secret 验签防伪造）+ 事件名 / 幂等投递头；
//!   - 成功判定 = **HTTP 2xx**（接收方协议不受 CMX 信封约束，响应体不解析）。

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

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

/// 一条出站事件（app 层构造；camelCase wire DTO，即对外契约的载荷格式）。
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
    /// 发起方业务系统标识（20260902 重构 additive：来自实例 system_id 快照；legacy 调用不上线）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_id: Option<String>,
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
            system_id: None,
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
    pub fn system_id(mut self, v: Option<String>) -> Self {
        self.system_id = v;
        self
    }

    /// 投递幂等 id（`{instanceId}-{taskId?}-{occurredAt}`，进 [`DELIVERY_HEADER`]）。
    ///
    /// 001 方案 §4.1 升级：有 taskId 时拼入——修复一次推进产生多个并行任务时多条事件
    /// 共用同一 delivery_id 的碰撞（同一 occurred_at 复用）。键更精确对接收方幂等无破坏
    /// （按该头去重的接入方只会更少误吞；升级说明见 docs/usage/08 §8.6）。
    pub fn delivery_id(&self) -> String {
        match self.task_id.as_deref() {
            Some(tid) if !tid.is_empty() => {
                format!("{}-{}-{}", self.instance_id, tid, self.occurred_at)
            }
            _ => format!("{}-{}", self.instance_id, self.occurred_at),
        }
    }
}

/// HMAC-SHA256(body, secret) → 签名头值 `sha256=<hex>`（小写 hex；对实际发送字节计算）。
pub fn sign_body(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC 接受任意长度密钥");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 001 升级：带 taskId 的 delivery_id 含任务号；无 taskId 退回两段形态。
        assert_eq!(ev.delivery_id(), "i-1-t-9-2026-08-31T10:00:00Z");
        let no_task = FlowEvent::new(FlowEventKind::InstanceStarted, "i-2", "t2".into());
        assert_eq!(no_task.delivery_id(), "i-2-t2");
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
