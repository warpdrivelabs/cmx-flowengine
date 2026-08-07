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
//! 本 crate 只依赖引擎 trait 层 + reqwest，与引擎核一样中立可测。

pub mod config;
pub mod delegate;
pub mod identity;
pub mod mock;
pub mod subflow;
pub mod webhook;

pub use config::{AdapterConfig, AdapterMode, WebhookConfig};
pub use delegate::HttpDelegate;
pub use identity::HttpAssigneeResolver;
pub use mock::{MockAssigneeResolver, MockDelegate, MockSubflowRouter};
pub use subflow::HttpSubflowRouter;
pub use webhook::{FlowEvent, FlowEventKind, WebhookSender};
