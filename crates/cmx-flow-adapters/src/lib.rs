//! cmx-flow-adapters —— cmx-flow 引擎的外部服务适配器（方案 §4/S1）。
//!
//! 三个注入 trait 的「接外部 HTTP」实现 + 各自 Mock 默认：
//!   - [`HttpAssigneeResolver`] / [`MockAssigneeResolver`]  —— `AssigneeResolver`（候选人/身份）
//!   - [`HttpSubflowRouter`]    / [`MockSubflowRouter`]     —— `SubflowRouter`（子流程组织路由）
//!   - [`HttpDelegate`]         / [`MockDelegate`]          —— `JavaDelegate`（serviceTask 外包）
//!
//! 选择由 [`config::AdapterConfig`] 的 mode(mock|http|pg) 从环境变量决定；pg 实现在
//! cmx-flow-store-pg（本 crate 不含），三选一的装配在 cmx-flow-app::engine。
//!
//! http 形态统一走 cmx-service-rpc 基座（目标 = 服务目录键，无注册中心时目录登记静态
//! url 直连）；对端协议保持自定义裸 JSON。本 crate 依赖引擎 trait 层 + 基座，不直接
//! 依赖任何 HTTP 客户端库。

pub mod channel;
pub mod channel_mq;
pub mod channel_webhook;
pub mod config;
pub mod delegate;
pub mod identity;
pub mod mock;
pub mod subflow;
pub mod webhook;

pub use channel::{
    ChannelRegistry, DeliveryChannel, DeliveryOutcome, DeliveryTask, global_registry,
};
pub use channel_webhook::WebhookChannel;
pub use config::{AdapterConfig, AdapterMode, WebhookConfig, WebhookTarget};
pub use delegate::HttpDelegate;
pub use identity::HttpAssigneeResolver;
pub use mock::{MockAssigneeResolver, MockDelegate, MockSubflowRouter};
pub use subflow::{HttpDimensionResolver, HttpSubflowRouter, MockDimensionResolver};
pub use webhook::{FlowEvent, FlowEventKind, DELIVERY_HEADER, EVENT_HEADER, SIGNATURE_HEADER};
