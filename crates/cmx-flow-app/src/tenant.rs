//! 请求级租户上下文（多租户 db-per-tenant，S2）。
//!
//! 镜像平台 `cmx-traits::auth::context_scope` 的 `task_local!` 模式：认证中间件在请求入口建
//! scope，请求生命周期内任意 `.await` 点都能无参读当前租户/用户/角色，无需层层透传。
//!
//! ⚠️ **task_local 不跨 `tokio::spawn`**：后台任务（webhook worker / timer poller）读不到，
//! 须显式捕获租户。故 poller 遍历运行时缓存、webhook emit 只在 handler（有 scope）内做。
//!
//! **单租户零回归**：无 scope（未装认证中间件 / 后台任务 / 默认部署）时 `current_tenant()`
//! 回退 [`DEFAULT_TENANT`]，其 db_id = 原 `FLOW_DB_ID`——行为完全等价 S1 之前的单库形态。

use tokio::task_local;

/// 默认租户名（无租户上下文时的回退；其 db_id 映射到既有 FLOW_DB_ID，保单租户零回归）。
pub const DEFAULT_TENANT: &str = "default";

task_local! {
    /// 当前请求的租户上下文。仅在认证中间件 [`scope`] 作用域内有值。
    static TENANT: TenantCtx;
}

/// 请求级租户上下文快照（认证中间件一次性填充，请求内只读）。
#[derive(Debug, Clone)]
pub struct TenantCtx {
    /// 租户标识（决定用哪个租户库）。
    pub tenant: String,
    /// 当前用户 id（JWT sub；可空——auth off 时无）。授权比对（assignee/initiator）用此。
    pub user: Option<String>,
    /// 当前用户名（JWT `username` claim；可空——旧令牌/第三方精简令牌无）。留痕/审计
    /// 展示用 [`current_display_user`] 取「用户名优先、id 兜底」，勿拿 user 直接当姓名。
    pub username: Option<String>,
    /// 当前用户昵称（JWT `nickname` claim；可空——旧令牌未签发该 claim）。展示名首选：
    /// [`current_display_nickname`] 供审批留痕/快照列取「昵称优先、username 兜底」。
    pub nickname: Option<String>,
    /// 当前用户角色（JWT roles；可空）。
    pub roles: Vec<String>,
}

impl TenantCtx {
    /// 用租户名构建（user/username/roles 空）。
    pub fn new(tenant: impl Into<String>) -> Self {
        Self {
            tenant: tenant.into(),
            user: None,
            username: None,
            nickname: None,
            roles: Vec::new(),
        }
    }
    pub fn with_user(mut self, user: Option<String>) -> Self {
        self.user = user;
        self
    }
    pub fn with_username(mut self, username: Option<String>) -> Self {
        self.username = username;
        self
    }
    pub fn with_nickname(mut self, nickname: Option<String>) -> Self {
        self.nickname = nickname;
        self
    }
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }
}

/// 在给定租户上下文的作用域内执行 future（认证中间件在请求入口调用）。
pub async fn scope<F, R>(ctx: TenantCtx, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    TENANT.scope(ctx, fut).await
}

/// 当前租户名。无 scope 时回退 [`DEFAULT_TENANT`]（单租户零回归）。
pub fn current_tenant() -> String {
    TENANT
        .try_with(|c| c.tenant.clone())
        .unwrap_or_else(|_| DEFAULT_TENANT.to_string())
}

/// 当前用户 id（无 scope / 未认证时 None）。
pub fn current_user() -> Option<String> {
    TENANT.try_with(|c| c.user.clone()).ok().flatten()
}

/// 留痕/审计展示用操作人名：优先 `username` claim（如 "admin"），无则回退用户 id——
/// 平台 AccessClaims 自带 `username`，旧令牌/第三方精简令牌缺省时退 id 保证不空。
pub fn current_display_user() -> Option<String> {
    TENANT
        .try_with(|c| {
            c.username
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| c.user.clone())
        })
        .ok()
        .flatten()
}

/// 昵称优先的展示名：`nickname` claim → `username` claim，均无则 None（不回退 id——
/// 供审批意见 nick_name 快照列等场景，宁缺勿假）。昵称为空/旧令牌未签发时自然落到 username。
pub fn current_display_nickname() -> Option<String> {
    TENANT
        .try_with(|c| {
            c.nickname
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| c.username.clone().filter(|s| !s.trim().is_empty()))
        })
        .ok()
        .flatten()
}

/// 当前用户角色（无 scope 时空）。
pub fn current_roles() -> Vec<String> {
    TENANT.try_with(|c| c.roles.clone()).unwrap_or_default()
}

/// 是否处于租户 scope 内（认证中间件已建立）。
pub fn in_scope() -> bool {
    TENANT.try_with(|_| ()).is_ok()
}

/// 身份快照 —— 供通用监控 crate [`cmx_web_monitor`] 的 observe 中间件读取当前请求身份。
///
/// 注册方式：flow-server 启动时 `cmx_web_monitor::set_identity_provider(identity_snapshot)`。
/// observe 夹在认证之后（scope 已建），故这里能读到 tenant/user/roles；无 scope 返 None（记为匿名）。
pub fn identity_snapshot() -> Option<cmx_web_monitor::Identity> {
    if !in_scope() {
        return None;
    }
    Some(cmx_web_monitor::Identity {
        tenant: current_tenant(),
        user: current_user(),
        roles: current_roles(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_scope_falls_back_to_default() {
        // 无 scope：回退默认租户（单租户零回归的核心保证）。
        assert_eq!(current_tenant(), DEFAULT_TENANT);
        assert_eq!(current_user(), None);
        assert!(current_roles().is_empty());
        assert!(!in_scope());
    }

    #[tokio::test]
    async fn scope_threads_tenant() {
        let ctx = TenantCtx::new("acme")
            .with_user(Some("u_1".into()))
            .with_roles(vec!["approver".into()]);
        scope(ctx, async {
            assert!(in_scope());
            assert_eq!(current_tenant(), "acme");
            assert_eq!(current_user(), Some("u_1".to_string()));
            assert_eq!(current_roles(), vec!["approver".to_string()]);
        })
        .await;
        // 出 scope 后回退默认。
        assert_eq!(current_tenant(), DEFAULT_TENANT);
    }
}
