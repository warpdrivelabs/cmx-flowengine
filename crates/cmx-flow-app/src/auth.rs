//! JWT 认证中间件（S2，claim 信任模式）。
//!
//! flow 只信任 token 声明（不连远程 IdP）：中间件验 JWT 签名（HS256/RS256，密钥经 ConfigManager
//! 读 flow-server.toml 的 `[auth]` 段 ← env `AUTH__*` 覆盖），解出 `tenant`/`sub`(user)/`roles`
//! claim，包进 [`crate::tenant::scope`]——请求内所有 DB 走该租户库。
//!
//! 模式（`auth.mode`）：
//!   - `off`（默认）：不验签。租户取 `X-Tenant` 头或默认租户；用户取 `X-User` 头。开发/单租户零门槛。
//!   - `jwt`：强制验签。`Authorization: Bearer <jwt>` 缺失/坏签/过期 → 401。
//!
//! 密钥（jwt 模式；同 `[auth]` 段）：
//!   - `auth.jwt_alg = HS256 | RS256`（默认 HS256）。
//!   - HS256：`auth.jwt_secret`（对称密钥）。
//!   - RS256：`auth.jwt_public_key`（PEM 公钥）。
//!   - `auth.jwt_tenant_claim`（默认 "tenant"）/`auth.jwt_roles_claim`（默认 "roles"）自定 claim 名。
//!   - `auth.api_keys = "k1:tenantA,k2:tenantB"`（服务间 key:租户映射，S3）。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// 进程级认证配置（一次经 ConfigManager 读定；启动后改配置需重启）。
struct AuthConfig {
    mode: AuthMode,
    alg: Algorithm,
    decoding_key: Option<DecodingKey>,
    tenant_claim: String,
    roles_claim: String,
    /// 服务间 API Key → 租户映射（S3）。`auth.api_keys="k1:tenantA,k2:tenantB"`。
    api_keys: std::collections::HashMap<String, String>,
}

static AUTH: OnceLock<AuthConfig> = OnceLock::new();

/// auth 中间件是否已在本进程处理过请求（即宿主确实挂载了本中间件）。
///
/// 任务端点授权（T0b）的 fail-open/fail-close 判据：宿主未挂载本中间件（平台内嵌形态）时
/// `current_user()` 恒 None——此形态维持平台 mw_auth 边界、flow 层放行（现状兼容）；
/// 已挂载仍拿不到用户（纯服务调用 / 委托令牌验签失败）则按端点语义收紧。
static AUTH_ACTIVE: AtomicBool = AtomicBool::new(false);

/// auth 中间件是否生效（宿主挂载且有请求流过）。
pub fn auth_middleware_active() -> bool {
    AUTH_ACTIVE.load(Ordering::Relaxed)
}

fn auth_config() -> &'static AuthConfig {
    AUTH.get_or_init(|| {
        // ConfigManager 直读 [auth] 段（toml ← AUTH__* env 覆盖）；未初始化（单测/独立组件场景）
        // → 空配置 = off 模式（对齐 cmx-mdm-app auth.rs 蓝本）。
        let get = |key: &str| {
            cmx_utils::ConfigManager::try_global()
                .and_then(|cm| cm.get_string(key).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let mode = match get("auth.mode").as_deref() {
            Some(m) if m.eq_ignore_ascii_case("jwt") => AuthMode::Jwt,
            _ => AuthMode::Off,
        };
        let alg = match get("auth.jwt_alg").as_deref() {
            Some(a) if a.eq_ignore_ascii_case("RS256") => Algorithm::RS256,
            _ => Algorithm::HS256,
        };
        // 解码密钥（jwt 模式才需要；off 模式不用）。
        let decoding_key = if mode == AuthMode::Jwt {
            match alg {
                Algorithm::RS256 => get("auth.jwt_public_key").and_then(|pem| {
                    DecodingKey::from_rsa_pem(pem.as_bytes())
                        .map_err(|e| tracing::error!(error = %e, "auth.jwt_public_key 解析失败"))
                        .ok()
                }),
                _ => get("auth.jwt_secret").map(|s| DecodingKey::from_secret(s.as_bytes())),
            }
        } else {
            None
        };
        if mode == AuthMode::Jwt && decoding_key.is_none() {
            tracing::error!("auth.mode=jwt 但缺密钥（auth.jwt_secret / auth.jwt_public_key），所有请求将 401");
        }
        AuthConfig {
            mode,
            alg,
            decoding_key,
            tenant_claim: get("auth.jwt_tenant_claim").unwrap_or_else(|| "tenant".to_string()),
            roles_claim: get("auth.jwt_roles_claim").unwrap_or_else(|| "roles".to_string()),
            api_keys: parse_api_keys(get("auth.api_keys").unwrap_or_default()),
        }
    })
}

/// 解析 `auth.api_keys="k1:tenantA,k2:tenantB"` → {k1→tenantA, k2→tenantB}。
/// 无冒号的 key 绑定默认租户。
fn parse_api_keys(raw: String) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.split_once(':') {
            Some((k, t)) => {
                map.insert(k.trim().to_string(), t.trim().to_string());
            }
            None => {
                map.insert(entry.to_string(), DEFAULT_TENANT.to_string());
            }
        }
    }
    map
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
    // 标记中间件已生效（T0b 授权的 fail-open/fail-close 判据）。
    AUTH_ACTIVE.store(true, Ordering::Relaxed);
    let cfg = auth_config();
    // 先查 X-Api-Key（服务间 M2M，S3）：命中即以该 key 绑定的租户建 scope，免 JWT。
    if let Some(key) = header_str(&req, "x-api-key") {
        match cfg.api_keys.get(&key) {
            Some(key_tenant) => {
                // S6 认证桥：服务身份已验（API Key 合法）。若平台再带上**委托用户令牌**
                // （X-Delegated-User-Token: Bearer <终端用户 JWT>，对齐平台 remote_importers 的
                // 三层出站鉴权），则解它取真实办理人 + 租户——否则退化为纯服务调用（S3 语义）。
                //
                // 关键：多租户下一个服务 key 服务多个平台租户，故**租户优先取委托令牌的 claim**，
                // 而非 key 绑定的租户（key_tenant 仅作无委托令牌时的回退）。
                let ctx = match delegated_user_ctx(&req, cfg) {
                    Some(mut ctx) => {
                        // 委托令牌解出用户/租户；追加 "service" 角色标记本跳是经服务代理来的。
                        ctx.roles.push("service".to_string());
                        ctx
                    }
                    None => {
                        TenantCtx::new(key_tenant.clone()).with_roles(vec!["service".to_string()])
                    }
                };
                return scope(ctx, next.run(req)).await;
            }
            None => return unauthorized("无效 API Key"),
        }
    }
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

/// SSE 白名单：这些 GET 端点是浏览器原生 EventSource 连接，无法带 Authorization header，
/// 故 jwt 模式下允许它们改用 `?ticket=` 一次性票据鉴权（见 [`crate::sse`]）。仅这两条豁免。
fn is_sse_ticket_path(path: &str) -> bool {
    path.ends_with("/design/collab") || path.ends_with("/events")
}

/// 从 query string 取 `ticket` 参数值（不引入额外依赖，手工扫 `k=v&`）。
fn ticket_from_query(req: &Request) -> Option<String> {
    let q = req.uri().query()?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("ticket=") {
            let decoded = urldecode(v);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

/// 极简 percent-decode（票据是 uuid，仅可能含 `%` 转义；容忍非法转义原样保留）。
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// off 模式：从 X-Tenant / X-User 头取（缺省默认租户 / 无用户）。
fn ctx_from_headers(req: &Request) -> TenantCtx {
    let tenant = header_str(req, "x-tenant").unwrap_or_else(|| DEFAULT_TENANT.to_string());
    let user = header_str(req, "x-user");
    TenantCtx::new(tenant).with_user(user)
}

/// jwt 模式：从 `Authorization: Bearer` 取令牌验签 + 解 claim。失败返回 401 响应。
///
/// 例外：SSE 白名单路径（EventSource 无法带 header）在 header 缺失时改用 `?ticket=` 一次性票据。
fn verify_jwt(req: &Request, cfg: &AuthConfig) -> Result<TenantCtx, Response> {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")));
    match bearer {
        Some(token) => decode_claims(token, cfg).map_err(|e| unauthorized(&format!("JWT 校验失败: {e}"))),
        None => {
            // header 缺失：SSE 白名单路径接受一次性票据（浏览器 EventSource 场景）。
            if is_sse_ticket_path(req.uri().path()) {
                if let Some(ticket) = ticket_from_query(req) {
                    if let Some(ctx) = crate::sse::consume_ticket(&ticket) {
                        return Ok(ctx);
                    }
                    return Err(unauthorized("SSE 票据无效或已过期"));
                }
            }
            Err(unauthorized("缺少 Authorization: Bearer <token>"))
        }
    }
}

/// S6 认证桥：从 `X-Delegated-User-Token: Bearer <jwt>` 解出委托的终端用户上下文。
///
/// 平台经 FlowProxyModule 出站时，把当前登录用户的原始 JWT 放此头（对齐 remote_importers 的
/// `apply_auth_headers`）。这里**始终验签**（无论 auth.mode）——委托令牌是终端用户身份的
/// 唯一凭据，不能无签信任。无密钥（未配 JWT）或验签失败 → 返回 None（退化为纯服务调用，不 401，
/// 因服务身份本身已由 API Key 验过）。
fn delegated_user_ctx(req: &Request, cfg: &AuthConfig) -> Option<TenantCtx> {
    let token = req
        .headers()
        .get("x-delegated-user-token")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")).or(Some(s)))?;
    match decode_claims(token, cfg) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            tracing::warn!(error = %e, "X-Delegated-User-Token 验签失败，退化为纯服务调用");
            None
        }
    }
}

/// 验签一个 JWT 字符串 → 租户上下文（tenant/user/roles claim）。供 Bearer 与委托令牌两路复用。
///
/// 需已配解码密钥（`decoding_key`）；未配时 Err（jwt 模式启动即告警，off 模式无委托令牌路径）。
fn decode_claims(token: &str, cfg: &AuthConfig) -> Result<TenantCtx, String> {
    let key = cfg
        .decoding_key
        .as_ref()
        .ok_or_else(|| "服务未配置 JWT 密钥".to_string())?;

    let mut validation = Validation::new(cfg.alg);
    // flow 只信任 token 声明；不校验 aud（由签发方约束）。exp 默认校验（过期即失败）。
    validation.validate_aud = false;

    let data = decode::<Claims>(token, key, &validation).map_err(|e| e.to_string())?;
    let claims = data.claims;

    // tenant claim（配置名）→ 字符串；缺失回退默认租户。
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

    // username claim → 展示名（平台 AccessClaims 自带；缺省 None，留痕经
    // current_display_user 回退用户 id，避免把 id 当姓名写台账）。
    let username = claims
        .extra
        .get("username")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // nickname claim → 昵称（平台 2026-08 起签发；旧令牌无 → None，展示经
    // current_display_nickname 自然落到 username）。
    let nickname = claims
        .extra
        .get("nickname")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(TenantCtx::new(tenant)
        .with_user(claims.sub)
        .with_username(username)
        .with_nickname(nickname)
        .with_roles(roles))
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

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};

    /// 构造一个 HS256 测试用 AuthConfig（不碰进程 OnceLock / 环境变量）。
    fn hs256_cfg(secret: &str) -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Jwt,
            alg: Algorithm::HS256,
            decoding_key: Some(DecodingKey::from_secret(secret.as_bytes())),
            tenant_claim: "tenant".to_string(),
            roles_claim: "roles".to_string(),
            api_keys: std::collections::HashMap::new(),
        }
    }

    /// 签一个 HS256 JWT（含 sub/tenant/roles + 远期 exp）。
    fn sign(secret: &str, sub: &str, tenant: &str, roles: &[&str]) -> String {
        let claims = serde_json::json!({
            "sub": sub,
            "tenant": tenant,
            "roles": roles,
            "exp": 4_102_444_800u64, // 2100-01-01，避免过期
        });
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn decode_claims_extracts_user_tenant_roles() {
        let cfg = hs256_cfg("s6-secret");
        let token = sign("s6-secret", "u_alice", "tenantA", &["approver", "finance"]);
        let ctx = decode_claims(&token, &cfg).expect("应验签通过");
        assert_eq!(ctx.tenant, "tenantA");
        assert_eq!(ctx.user.as_deref(), Some("u_alice"));
        assert_eq!(ctx.roles, vec!["approver".to_string(), "finance".to_string()]);
    }

    #[test]
    fn decode_claims_rejects_wrong_secret() {
        let cfg = hs256_cfg("right-secret");
        let token = sign("WRONG-secret", "u_bob", "t1", &[]);
        assert!(decode_claims(&token, &cfg).is_err(), "错密钥应验签失败");
    }

    #[test]
    fn delegated_user_ctx_honors_token_over_key_tenant() {
        // S6 桥核心：委托令牌带 Bearer 前缀，解出的 tenant 覆盖 API Key 绑定租户。
        let cfg = hs256_cfg("s6-secret");
        let token = sign("s6-secret", "u_carol", "tenantB", &["clerk"]);
        let req = Request::builder()
            .header("x-delegated-user-token", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let ctx = delegated_user_ctx(&req, &cfg).expect("应解出委托用户");
        assert_eq!(ctx.tenant, "tenantB"); // 取委托令牌的 claim，非 key 绑定租户
        assert_eq!(ctx.user.as_deref(), Some("u_carol"));
        assert_eq!(ctx.roles, vec!["clerk".to_string()]);
    }

    #[test]
    fn delegated_user_ctx_accepts_bare_token_without_bearer() {
        // 容忍无 Bearer 前缀（宿主直接放裸 JWT）。
        let cfg = hs256_cfg("s6-secret");
        let token = sign("s6-secret", "u_dan", "t2", &[]);
        let req = Request::builder()
            .header("x-delegated-user-token", token)
            .body(axum::body::Body::empty())
            .unwrap();
        let ctx = delegated_user_ctx(&req, &cfg).expect("裸令牌也应解出");
        assert_eq!(ctx.user.as_deref(), Some("u_dan"));
    }

    #[test]
    fn delegated_user_ctx_none_when_absent_or_bad() {
        let cfg = hs256_cfg("s6-secret");
        // 缺头 → None（退化纯服务调用）
        let req = Request::builder().body(axum::body::Body::empty()).unwrap();
        assert!(delegated_user_ctx(&req, &cfg).is_none());
        // 坏令牌 → None（不 401，服务身份已验）
        let bad = Request::builder()
            .header("x-delegated-user-token", "Bearer not.a.jwt")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(delegated_user_ctx(&bad, &cfg).is_none());
    }
}

