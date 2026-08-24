//! 多租户配置 + 租户数据源懒注册（db-per-tenant，S2）。
//!
//! 把「租户 → db_id → 连接 URL」的策略收在一处，engine.rs 的 `flow_for_tenant` 用它派生 db_id
//! 并在首访某租户时懒注册数据源。**默认 single 模式 = 只有默认租户、用宿主预注册的 fico-db/primary，
//! 行为完全等价 S1 之前的单库形态（零回归）**。
//!
//! 配置（模式经 ConfigManager 直读；multi 的 URL 模板仍走 env，R3 未实现、保持 env-only）：
//!   - `auth.tenancy = single | multi`（toml [auth] 段 ← env `AUTH__TENANCY` 覆盖；默认 single）。
//!     single 下所有请求都归默认租户。
//!   - multi 下租户库 URL 由二选一给出：
//!       · `FLOW_TENANT_DB_URL_TEMPLATE = "postgres://…/flow_{tenant}"`（`{tenant}` 占位）
//!       · 或 `FLOW_TENANT_<T>_PG_URL`（每租户显式，覆盖模板）
//!     IAM 库同理：`FLOW_TENANT_IAM_URL_TEMPLATE` / `FLOW_TENANT_<T>_IAM_URL`（缺省=复用默认 IAM 库）。

use std::sync::OnceLock;

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager};

use crate::engine::{FLOW_DB_ID, IAM_DB_ID};
use crate::tenant::DEFAULT_TENANT;

/// 多租户配置（进程级，一次从环境读定）。
#[derive(Debug, Clone)]
pub struct TenancyConfig {
    /// 是否多租户（false = 单租户，只默认租户）。
    pub multi: bool,
    /// 租户库 URL 模板（`{tenant}` 占位；multi 时用）。
    pub flow_url_template: Option<String>,
    /// IAM 库 URL 模板（`{tenant}` 占位；缺省则各租户复用默认 IAM 库）。
    pub iam_url_template: Option<String>,
}

static TENANCY: OnceLock<TenancyConfig> = OnceLock::new();

impl TenancyConfig {
    /// 全局配置（首次读环境）。
    pub fn global() -> &'static TenancyConfig {
        TENANCY.get_or_init(Self::from_env)
    }

    fn from_env() -> Self {
        let multi = cmx_utils::ConfigManager::try_global()
            .and_then(|cm| cm.get_string("auth.tenancy").ok())
            .map(|v| v.trim().eq_ignore_ascii_case("multi"))
            .unwrap_or(false);
        Self {
            multi,
            flow_url_template: env_opt("FLOW_TENANT_DB_URL_TEMPLATE"),
            iam_url_template: env_opt("FLOW_TENANT_IAM_URL_TEMPLATE"),
        }
    }

    /// 租户 → 运行态库 db_id。默认租户 = 原 FLOW_DB_ID（零回归）；其它租户 = `flow_<tenant>`。
    pub fn flow_db_id(&self, tenant: &str) -> String {
        if !self.multi || tenant == DEFAULT_TENANT {
            FLOW_DB_ID.to_string()
        } else {
            format!("flow_{}", sanitize(tenant))
        }
    }

    /// 租户 → IAM 库 db_id。默认租户 = 原 IAM_DB_ID；multi 下若无 IAM 模板则各租户仍复用默认 IAM 库
    /// （身份/组织常是跨租户共享的外部系统；需独立可配 IAM 模板）。
    pub fn iam_db_id(&self, tenant: &str) -> String {
        if !self.multi || tenant == DEFAULT_TENANT || self.iam_url_template.is_none() {
            IAM_DB_ID.to_string()
        } else {
            format!("iam_{}", sanitize(tenant))
        }
    }

    /// 该租户运行态库连接 URL（显式 env 覆盖 > 模板）。默认租户返回 None（宿主已预注册）。
    fn flow_url(&self, tenant: &str) -> Option<String> {
        if !self.multi || tenant == DEFAULT_TENANT {
            return None;
        }
        env_opt(&format!("FLOW_TENANT_{}_PG_URL", tenant.to_uppercase()))
            .or_else(|| self.flow_url_template.as_ref().map(|t| fill(t, tenant)))
    }

    fn iam_url(&self, tenant: &str) -> Option<String> {
        if !self.multi || tenant == DEFAULT_TENANT {
            return None;
        }
        env_opt(&format!("FLOW_TENANT_{}_IAM_URL", tenant.to_uppercase()))
            .or_else(|| self.iam_url_template.as_ref().map(|t| fill(t, tenant)))
    }
}

/// 懒注册某租户的运行态库 + IAM 库数据源（若配了 URL 且尚未注册）。
///
/// 默认租户 / single 模式：URL 为 None → 不注册（宿主 flow-server bootstrap 已注册 fico-db/primary）。
/// multi 非默认租户：按 URL 注册 flow_<tenant>（IAM 有独立模板才注册 iam_<tenant>，否则复用默认）。
/// 已注册的 db_id 再注册是幂等的（register_data_source 覆盖同 id 配置）。
pub async fn register_tenant_datasources(
    tenant: &str,
    flow_db_id: &str,
    iam_db_id: &str,
) -> Result<(), String> {
    let cfg = TenancyConfig::global();
    if let Some(url) = cfg.flow_url(tenant) {
        register(flow_db_id, &url).await?;
    }
    if let Some(url) = cfg.iam_url(tenant) {
        register(iam_db_id, &url).await?;
    }
    Ok(())
}

/// 注册一个 PG 数据源（对齐 flow-server bootstrap 的形态）。
async fn register(db_id: &str, url: &str) -> Result<(), String> {
    let cfg = DbConfig {
        db_type: DbType::Postgres,
        db_url: url.to_string(),
        db_id: db_id.to_string(),
        db_name: None,
        db_schema: Some("public".to_string()),
        default: false,
        pool_config: Default::default(),
        health_check_interval: 60,
        health_check_timeout: 5,
        domain_code: None,
        application_code: None,
        module_code: None,
        source_type: Some("tenant".to_string()),
    };
    get_default_pg_db_manager()
        .register_data_source(cfg)
        .await
        .map_err(|e| format!("注册数据源 {db_id} 失败: {e}"))
}

/// `{tenant}` 占位替换。
fn fill(template: &str, tenant: &str) -> String {
    template.replace("{tenant}", &sanitize(tenant))
}

/// 清洗租户名为安全 db 标识（字母数字下划线；防注入/坏名进 db_id）。
fn sanitize(tenant: &str) -> String {
    tenant
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn env_opt(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.trim().is_empty())
}
