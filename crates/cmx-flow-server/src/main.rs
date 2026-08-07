/*
 * cmx-flow 独立流程微服务 HTTP 服务器（方案 S0「flow-http 脱平台」）。
 *
 * 采用通用骨架 cmx-web-chassis：与 report-server / mdm-server 同一套启动/日志/中间件/优雅关闭/
 * banner。main 只填 ServiceSpec——flow 路由 + 两个启动钩子（注册数据源、起引擎定时器 poller）+
 * flow 专属 banner/配色，交 chassis::run 装配。零 cmx-api 依赖。
 *
 * 配置（chassis 框架级用 FLOW_ 前缀；flow 专属用各自变量）：
 *   FLOW_HOST / FLOW_PORT（默认 0.0.0.0:8091）/ FLOW_LOG_DIR / FLOW_LOG_LEVEL / FLOW_CONFIG(toml)
 *   FLOW_PG_URL / IAM_PG_URL（数据源）
 *   FLOW_IDENTITY_MODE / FLOW_SUBFLOW_MODE / FLOW_DELEGATE_MODE + *_URL（S1 适配器选择）
 *
 * 用法：
 *   FLOW_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
 *   IAM_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
 *     cargo run -p cmx-flow-server
 *   curl http://127.0.0.1:8091/api/flow/definitions
 */

use cmx_database_pg::{DbConfig, DbType, get_default_pg_db_manager};
use cmx_flow_app::{FLOW_DB_ID, IAM_DB_ID, flow_routes, spawn_timer_poller};
use cmx_web_chassis::{BannerSpec, ChassisConfig, ServiceSpec, run};

/// flow 专属字符画（区别于平台/默认 banner）。
const FLOW_ART: &str = r#"
   ██████╗███╗   ███╗██╗  ██╗    ███████╗██╗      ██████╗ ██╗    ██╗
  ██╔════╝████╗ ████║╚██╗██╔╝    ██╔════╝██║     ██╔═══██╗██║    ██║
  ██║     ██╔████╔██║ ╚███╔╝     █████╗  ██║     ██║   ██║██║ █╗ ██║
  ██║     ██║╚██╔╝██║ ██╔██╗     ██╔══╝  ██║     ██║   ██║██║███╗██║
  ╚██████╗██║ ╚═╝ ██║██╔╝ ██╗    ██║     ███████╗╚██████╔╝╚███╔███╔╝
   ╚═════╝╚═╝     ╚═╝╚═╝  ╚═╝    ╚═╝     ╚══════╝ ╚═════╝  ╚══╝╚══╝
"#;

#[tokio::main]
async fn main() -> cmx_web_chassis::Result<()> {
    // 框架级配置：FLOW_ 前缀环境变量 + 可选 flow-server.toml，默认端口 8091。
    let mut cfg = ChassisConfig::load("flow", "FLOW", "flow-server.toml");
    if std::env::var("FLOW_PORT").is_err() && cfg.port == 8080 {
        cfg.port = 8091; // 未显式配端口时用 flow 默认（避开平台 8080 / demo 8090）。
    }

    // flow 专属 banner：青绿 → 蓝 渐变。
    let banner = BannerSpec::defaults("flow")
        .art(FLOW_ART)
        .tagline("  cmx-flow 流程引擎微服务 · cmx-web-chassis ")
        .stops(vec![(0, 230, 170), (0, 140, 255), (90, 90, 255)]);

    let spec = ServiceSpec::<()>::new("flow", cfg)
        .banner(banner)
        // 认证中间件（S2）：FLOW_AUTH_MODE=off(默认,X-Tenant 头/默认租户) | jwt(验签取 claim)。
        // 建租户 scope，请求内 DB 走该租户库（db-per-tenant）。
        .router(flow_routes::<()>().layer(axum::middleware::from_fn(cmx_flow_app::auth_middleware)))
        .state(())
        // 钩子① 注册数据源（db_id 对齐 core 常量，否则引擎单例找不到源）。
        .init("datasources", |_meta| {
            Box::pin(async {
                let flow_url = std::env::var("FLOW_PG_URL").unwrap_or_else(|_| {
                    "postgres://postgres:postgres@127.0.0.1:5432/fico".to_string()
                });
                register_datasource(FLOW_DB_ID, &flow_url, true).await?;
                let iam_url = std::env::var("IAM_PG_URL").unwrap_or_else(|_| {
                    "postgres://postgres:postgres@127.0.0.1:5432/cmx".to_string()
                });
                register_datasource(IAM_DB_ID, &iam_url, false).await?;
                tracing::info!(flow_db = FLOW_DB_ID, iam_db = IAM_DB_ID, "✅ 数据源已注册");
                Ok(())
            })
        })
        // 钩子② 起定时器 poller（内部 flow() fail-fast 构建引擎单例：建表 + 注入 resolver/router +
        // 装载定义）。非致命：DB/schema 不可用只 warn，服务仍起（端点返错便于诊断）。
        .init("engine", |_meta| {
            Box::pin(async {
                if let Err(e) = spawn_timer_poller().await {
                    tracing::warn!(error = %e, "流程引擎初始化失败（DB/schema 不可用？端点将返错）");
                }
                Ok(())
            })
        });

    run(spec).await
}

/// 注册一个 PG 数据源（对齐 cmx-flow-demo 的注册形态）。返回 anyhow::Result 供钩子 `?`。
async fn register_datasource(db_id: &str, url: &str, default: bool) -> anyhow::Result<()> {
    let cfg = DbConfig {
        db_type: DbType::Postgres,
        db_url: url.to_string(),
        db_id: db_id.to_string(),
        db_name: None,
        db_schema: Some("public".to_string()),
        default,
        pool_config: Default::default(),
        health_check_interval: 60,
        health_check_timeout: 5,
        domain_code: None,
        application_code: None,
        module_code: None,
        source_type: Some("default".to_string()),
    };
    get_default_pg_db_manager()
        .register_data_source(cfg)
        .await
        .map_err(|e| anyhow::anyhow!("注册数据源 {db_id} 失败: {e}"))?;
    Ok(())
}
