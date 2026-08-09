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
use cmx_flow_app::openapi::flow_openapi;
use cmx_flow_app::{FLOW_DB_ID, IAM_DB_ID, flow_routes, flow_routes_v1, spawn_timer_poller};
use cmx_web_chassis::{BannerSpec, ChassisConfig, ServiceSpec, run};
use utoipa_swagger_ui::SwaggerUi;

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

    // 路由（S3 headless + 监控大盘）：
    //   - 根路径 /  → 引擎监控大盘 HTML（自包含单页，轮询 /api/flow/v1/stats）。
    //   - v1 正式契约 /api/flow/v1/*（含 SSE /events、/stats）+ 旧 /api/flow/*（兼容并存），二者经认证中间件。
    //   - OpenAPI 文档 /api/flow/v1/openapi.json + swagger UI /api/flow/v1/docs（**免认证**，公开文档）。
    //
    // chassis 默认把 router nest 到 /api 下；这里改用 nest_api(false) 自己 nest，好让根大盘 `/` 逃出 /api。
    let authed = flow_routes_v1::<()>()
        .merge(flow_routes::<()>())
        // 可观测中间件（内层，夹在认证之后）：next.run 时 auth 已建 scope，故能采集身份。
        .layer(axum::middleware::from_fn(cmx_flow_app::observe_middleware))
        // 认证中间件（外层，先跑）：建租户/用户 scope。
        .layer(axum::middleware::from_fn(cmx_flow_app::auth_middleware));
    let api_router = axum::Router::new()
        .merge(authed)
        .merge(SwaggerUi::new("/flow/v1/docs").url("/flow/v1/openapi.json", flow_openapi()));
    let app_router = axum::Router::new()
        // 根路径 → 业务监控大盘（流程域：实例/待办/定义/协作…；免认证，轮询 /api/flow/v1/stats）。
        .route("/", axum::routing::get(cmx_flow_app::dashboard::dashboard))
        .nest("/api", api_router);

    // 通用技术监控（chassis 默认在 /_mon 挂技术页 + 起系统采样器）：这里注入 flow 的身份读取器，
    // 好让请求遥测面板显示租户/用户/角色（读 tenant.rs 的 task_local scope；不在 scope 时匿名）。
    // 业务监控（/）与技术监控（/_mon）由此分立：前者流程域指标，后者系统/DB/请求域指标。
    cmx_web_monitor::set_service_name("cmx-flow 流程引擎");
    cmx_web_monitor::set_identity_provider(cmx_flow_app::identity_snapshot);
    // 拓扑来源：独立 flow-server 自身即引擎，流程能力为「进程内内嵌」（无下游反代）。
    cmx_web_monitor::set_topology_provider(|| {
        vec![cmx_web_monitor::ServiceDep {
            key: "flow".into(),
            label: "流程引擎".into(),
            mode: "embedded".into(),
            target: None,
            proxiable: true,
        }]
    });

    let spec = ServiceSpec::<()>::new("flow", cfg)
        .banner(banner)
        .nest_api(false) // 已自行 nest /api，避免 chassis 再包一层。
        .router(app_router)
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
