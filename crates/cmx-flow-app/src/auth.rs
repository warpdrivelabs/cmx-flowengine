//! JWT 认证中间件——已收编至 `cmx-engine-kit::auth::jwt`（唯一真源，flow 超集语义）。
//!
//! 本模块保留 `auth` 薄包装（内嵌本仓 SSE 票据白名单与票据消费回调），flow-server /
//! 平台壳各自 `.layer(from_fn(auth))` 挂载零改动。模式（off/jwt）、密钥（HS256/RS256）、
//! claim 宽容解析（tenant/roles/username/nickname）、API-Key 委托桥等行为契约见真源：
//! `../cmx-container/crates/libs/cmx-engine-kit/src/auth/jwt.rs`。

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

pub use cmx_engine_kit::auth::jwt::{auth_config_warmup, auth_middleware_active};

use cmx_engine_kit::auth::jwt::{self, JwtSpec};

/// 本仓专属参数：SSE 一次性票据白名单（EventSource 无法带 Authorization header 的两条豁免：
/// `/design/collab`、`/events`）与票据消费回调（issue/consume 状态在本仓 [`crate::sse`] 模块）。
static SPEC: JwtSpec =
    JwtSpec::new("flow", &["/design/collab", "/events"], Some(crate::sse::consume_ticket));

/// JWT 认证中间件（解析身份 → 建租户 scope → 放行；签名不变）。
pub async fn auth(req: Request, next: Next) -> Response {
    jwt::auth_mw(req, next, &SPEC).await
}
