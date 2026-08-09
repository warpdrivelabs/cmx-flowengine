//! 客户端连接监控 —— 已上提到通用 crate [`cmx_web_monitor`]（对标 chassis 的通用性：
//! 报表引擎/主数据等所有基于 chassis 的服务共用同一套请求遥测 + 系统指标 + DB 池监控）。
//!
//! 本模块保留为**薄再导出层**，让既有引用点（`crate::observe::client_stats` 路由、
//! `crate::observe::sse_connect/disconnect` in events.rs、`observe_middleware` in lib.rs）零改动。
//! 身份读取（tenant/user/roles）经 [`crate::tenant::identity_snapshot`] 注入到通用 crate。

pub use cmx_web_monitor::{client_stats, observe, sse_connect, sse_disconnect};
