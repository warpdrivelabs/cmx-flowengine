//! JWT 认证中间件（S2，claim 信任模式）。
//!
//! flow 只信任 token 声明（不连远程 IdP）：中间件验 JWT 签名（HS256/RS256，密钥来自环境变量），
//! 解出 `tenant`/`sub`(user)/`roles` claim，包进 [`crate::tenant::scope`]——请求内所有 DB 走该租户库。
//!
//! 模式（`FLOW_AUTH_MODE`）：
//!   - `off`（默认）：不验签。租户取 `X-Tenant` 头或默认租户；用户取 `X-User` 头。开发/单租户零门槛。
//!   - `jwt`：强制验签。`Authorization: Bearer <jwt>` 缺失/坏签/过期 → 401。
//!
//! 密钥（jwt 模式）：
//!   - `FLOW_JWT_ALG = HS256 | RS256`（默认 HS256）。
//!   - HS256：`FLOW_JWT_SECRET`（对称密钥）。
//!   - RS256：`FLOW_JWT_PUBLIC_KEY`（PEM 公钥）。
//!   - `FLOW_JWT_TENANT_CLAIM`（默认 "tenant"）/`FLOW_JWT_ROLES_CLAIM`（默认 "roles"）自定 claim 名。

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use serde_json::Value;

use crate::tenant::{DEFAULT_TENANT, TenantCtx, scope};

/// 认证模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Off,
    Jwt,
}

/// 进程级认证配置（一次从环境读定）。
struct AuthConfig {
    mode: AuthMode,
    alg: Algorithm,
    decoding_key: Option<DecodingKey>,
    tenant_claim: String,
    roles_claim: String,
}

static AUTH: OnceLock<AuthConfig> = OnceLock::new();

fn auth_config() -> &'static AuthConfig {
    AUTH.get_or_init(|| {
        let mode = match std::env::var("FLOW_AUTH_MODE").as_deref() {
            Ok(m) if m.trim().eq_ignore_ascii_case("jwt") => AuthMode::Jwt,
            _ => AuthMode::Off,
        };
        let alg = match std::env::var("FLOW_JWT_ALG").as_deref() {
            Ok(a) if a.trim().eq_ignore_ascii_case("RS256") => Algorithm::RS256,
            _ => Algorithm::HS256,
        };
        // 解码密钥（jwt 模式才需要；off 模式不用）。
        let decoding_key = if mode == AuthMode::Jwt {
            match alg {
                Algorithm::RS256 => std::env::var("FLOW_JWT_PUBLIC_KEY").ok().and_then(|pem| {
                    DecodingKey::from_rsa_pem(pem.as_bytes())
                        .map_err(|e| tracing::error!(error = %e, "FLOW_JWT_PUBLIC_KEY 解析失败"))
                        .ok()
                }),
                _ => std::env::var("FLOW_JWT_SECRET")
                    .ok()
                    .map(|s| DecodingKey::from_secret(s.as_bytes())),
            }
        } else {
            None
        };
        if mode == AuthMode::Jwt && decoding_key.is_none() {
            tracing::error!("FLOW_AUTH_MODE=jwt 但缺密钥（FLOW_JWT_SECRET / FLOW_JWT_PUBLIC_KEY），所有请求将 401");
        }
        AuthConfig {
            mode,
            alg,
            decoding_key,
            tenant_claim: std::env::var("FLOW_JWT_TENANT_CLAIM")
                .unwrap_or_else(|_| "tenant".to_string()),
            roles_claim: std::env::var("FLOW_JWT_ROLES_CLAIM")
                .unwrap_or_else(|_| "roles".to_string()),
        }
    })
}

/// JWT claim 壳（宽松：只取需要的，其余忽略）。
#[derive(Debug, Deserialize)]
struct Claims {
    /// 用户 id（标准 sub）。
    #[serde(default)]
    sub: Option<String>,
    /// 其余 claim 动态取（tenant/roles 名可配）。
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

/// axum 中间件：解析身份 → 建租户 scope → 放行。
///
/// 加在 flow 路由外层（flow-server / 平台壳各自 `.layer(from_fn(auth))`）。
pub async fn auth(req: Request, next: Next) -> Response {
    let cfg = auth_config();
    let ctx = match cfg.mode {
        AuthMode::Off => ctx_from_headers(&req),
        AuthMode::Jwt => match verify_jwt(&req, cfg) {
            Ok(ctx) => ctx,
            Err(resp) => return resp,
        },
    };
    // 请求全程在租户 scope 内：handler / biz_link 的 current_flow_db_id() 解析到该租户库。
    scope(ctx, next.run(req)).await
}

/// off 模式：从 X-Tenant / X-User 头取（缺省默认租户 / 无用户）。
fn ctx_from_headers(req: &Request) -> TenantCtx {
    let tenant = header_str(req, "x-tenant").unwrap_or_else(|| DEFAULT_TENANT.to_string());
    let user = header_str(req, "x-user");
    TenantCtx::new(tenant).with_user(user)
}

/// jwt 模式：验签 + 解 claim。失败返回 401 响应。
fn verify_jwt(req: &Request, cfg: &AuthConfig) -> Result<TenantCtx, Response> {
    let key = cfg
        .decoding_key
        .as_ref()
        .ok_or_else(|| unauthorized("服务未配置 JWT 密钥"))?;

    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")))
        .ok_or_else(|| unauthorized("缺少 Authorization: Bearer <token>"))?;

    let mut validation = Validation::new(cfg.alg);
    // flow 只信任 token 声明；不校验 aud（由签发方约束）。exp 默认校验（过期即 401）。
    validation.validate_aud = false;

    let data = decode::<Claims>(token, key, &validation)
        .map_err(|e| unauthorized(&format!("JWT 校验失败: {e}")))?;
    let claims = data.claims;

    // tenant claim（配置名）→ 字符串；缺失回退默认租户（也可改成强制要求）。
    let tenant = claims
        .extra
        .get(&cfg.tenant_claim)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_TENANT.to_string());
    // roles claim → Vec<String>（数组或逗号分隔字符串都容忍）。
    let roles = match claims.extra.get(&cfg.roles_claim) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(s)) => s.split(',').map(|r| r.trim().to_string()).collect(),
        _ => Vec::new(),
    };

    Ok(TenantCtx::new(tenant).with_user(claims.sub).with_roles(roles))
}

fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// 401 响应（形状对齐 resp 的错误信封 {code,msg}）。
fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "code": 401, "msg": msg })),
    )
        .into_response()
}

