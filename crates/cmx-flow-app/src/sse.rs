//! SSE 一次性票据（jwt 鉴权模式下 EventSource 无法带 Authorization header 的解法）。
//!
//! 浏览器原生 `EventSource` 不能设请求头，故 jwt 模式下 collab/events 两条 SSE 会 401。
//! 方案：客户端先用带 header 的普通 POST（正常验签）换一张**短期、一次性**的不透明票据，
//! 再把票据作为查询参数拼进 SSE URL（`?ticket=xxx`）。auth 中间件在 `Authorization` 头缺失时，
//! 对 SSE 白名单路径接受 `?ticket=`：查表→未过期→**消费即删**→用票据里的身份建租户 scope 放行。
//!
//! - 票据是随机不透明串（uuid v4），**不含** JWT 本身——JWT 绝不进 URL / 访问日志。
//! - TTL 短（默认 60s）、**单次消费**（resolve 即从表删）、访问时顺带 sweep 过期项。
//! - 进程内内存态（`Mutex<HashMap>`），多副本不共享——对齐 collab/events SSE 的单实例假设。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::Json;

use crate::resp::{ApiResp, Result};
use crate::tenant::{TenantCtx, current_roles, current_tenant, current_user};

/// 票据存活 TTL（秒）——只够客户端拿到后立刻发起 SSE 连接。
const TICKET_TTL_SECS: i64 = 60;

/// 一张已铸的票据：绑定请求时的身份 + 过期时刻（epoch 秒）。
#[derive(Clone)]
struct Ticket {
    tenant: String,
    user: Option<String>,
    roles: Vec<String>,
    exp: i64,
}

type TicketMap = HashMap<String, Ticket>;
static TICKETS: OnceLock<Mutex<TicketMap>> = OnceLock::new();
fn tickets() -> &'static Mutex<TicketMap> {
    TICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `POST /flow/v1/sse/ticket`：走 auth 层（带 header 正常验签），铸一张绑定当前身份的短期票据。
///
/// 返回 `{ ticket, ttlMs }`。客户端应立刻用它拼进 SSE URL。off 模式下也可调（default 租户），
/// 使前端无需探测鉴权模式——统一走「先铸票再连 SSE」一条路径。
pub async fn issue_ticket() -> Result<Json<ApiResp<serde_json::Value>>> {
    let ticket = uuid::Uuid::new_v4().to_string();
    let now = now_secs();
    let entry = Ticket {
        tenant: current_tenant(),
        user: current_user(),
        roles: current_roles(),
        exp: now + TICKET_TTL_SECS,
    };
    {
        let mut map = tickets().lock().unwrap();
        map.retain(|_, t| t.exp >= now); // 顺带 sweep 过期票
        map.insert(ticket.clone(), entry);
    }
    Ok(Json(ApiResp::ok(serde_json::json!({
        "ticket": ticket,
        "ttlMs": TICKET_TTL_SECS * 1000,
    }))))
}

/// 消费一张票据 → 身份上下文（auth 中间件在 header 缺失、路径为 SSE 白名单、query 带 ticket 时调）。
///
/// **单次消费**：命中即从表删（`remove`），故同票二次连接必失败。过期票视为无效并顺带清理。
/// 返回 `None`（未命中 / 已过期）→ 中间件维持 401。
pub fn consume_ticket(ticket: &str) -> Option<TenantCtx> {
    let now = now_secs();
    let mut map = tickets().lock().unwrap();
    map.retain(|_, t| t.exp >= now); // sweep 过期
    let t = map.remove(ticket)?; // 单次消费：取出即删
    if t.exp < now {
        return None;
    }
    Some(
        TenantCtx::new(t.tenant)
            .with_user(t.user)
            .with_roles(t.roles),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_is_single_use() {
        // 直接注一张票（绕开 issue_ticket 的 scope 依赖），验单次消费语义。
        let tk = "tk-single-use-test";
        {
            let mut map = tickets().lock().unwrap();
            map.insert(
                tk.into(),
                Ticket {
                    tenant: "tenantX".into(),
                    user: Some("u_x".into()),
                    roles: vec!["r1".into()],
                    exp: now_secs() + 60,
                },
            );
        }
        let ctx = consume_ticket(tk).expect("首次消费应成功");
        assert_eq!(ctx.tenant, "tenantX");
        assert_eq!(ctx.user.as_deref(), Some("u_x"));
        assert!(consume_ticket(tk).is_none(), "同票二次消费应失败");
    }

    #[test]
    fn expired_ticket_rejected() {
        let tk = "tk-expired-test";
        {
            let mut map = tickets().lock().unwrap();
            map.insert(
                tk.into(),
                Ticket {
                    tenant: "t".into(),
                    user: None,
                    roles: vec![],
                    exp: now_secs() - 1, // 已过期
                },
            );
        }
        assert!(consume_ticket(tk).is_none(), "过期票应拒绝");
    }
}
