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

use cmx_database_pg::{DbConfig, DbType};
use cmx_flow_app::openapi::flow_openapi;
use cmx_flow_app::{FLOW_DB_ID, IAM_DB_ID, flow_routes, flow_routes_v1, spawn_timer_poller};
use cmx_web_chassis::{BannerSpec, ChassisConfig, ServiceSpec, run};
use utoipa_swagger_ui::SwaggerUi;

/// flow 专属字符画（MEGA FLOW，区别于平台/默认 banner）。
const FLOW_ART: &str = r#"
███╗   ███╗███████╗ ██████╗  █████╗     ███████╗██╗      ██████╗ ██╗    ██╗
████╗ ████║██╔════╝██╔════╝ ██╔══██╗    ██╔════╝██║     ██╔═══██╗██║    ██║
██╔████╔██║█████╗  ██║  ███╗███████║    █████╗  ██║     ██║   ██║██║ █╗ ██║
██║╚██╔╝██║██╔══╝  ██║   ██║██╔══██║    ██╔══╝  ██║     ██║   ██║██║███╗██║
██║ ╚═╝ ██║███████╗╚██████╔╝██║  ██║    ██║     ███████╗╚██████╔╝╚███╔███╔╝
╚═╝     ╚═╝╚══════╝ ╚═════╝ ╚═╝  ╚═╝    ╚═╝     ╚══════╝ ╚═════╝  ╚══╝╚══╝
"#;

/// flow-server.toml 里的认证 + 数据源段（全可选；缺省即不注入，回退 auth.rs 默认/内置）。
///
/// 背景：auth.rs / tenant.rs 全走 `std::env::var("FLOW_*")` 懒读，chassis 的 toml 只解析框架级
/// 字段（端口/日志），不碰认证。故这里补一层：把 toml 的 `[auth]`/`[datasource]` 段读出，在任何
/// 请求前 `set_var` 注入对应 FLOW_* 环境变量——**仅当该 env 未由外部显式设置时**，保持既有
/// 「环境变量覆盖 toml」优先级。实现「用 toml 配置文件承载认证」而不改 auth.rs。
#[derive(serde::Deserialize, Default)]
struct FlowFileConfig {
    #[serde(default)]
    auth: AuthSection,
    #[serde(default)]
    datasource: DatasourceSection,
}

#[derive(serde::Deserialize, Default)]
struct AuthSection {
    mode: Option<String>,             // → FLOW_AUTH_MODE
    jwt_alg: Option<String>,          // → FLOW_JWT_ALG
    jwt_secret: Option<String>,       // → FLOW_JWT_SECRET
    jwt_public_key: Option<String>,   // → FLOW_JWT_PUBLIC_KEY
    jwt_tenant_claim: Option<String>, // → FLOW_JWT_TENANT_CLAIM
    jwt_roles_claim: Option<String>,  // → FLOW_JWT_ROLES_CLAIM
    api_keys: Option<String>,         // → FLOW_API_KEYS
    tenancy: Option<String>,          // → FLOW_TENANCY
}

#[derive(serde::Deserialize, Default)]
struct DatasourceSection {
    flow_pg_url: Option<String>, // → FLOW_PG_URL
    iam_pg_url: Option<String>,  // → IAM_PG_URL
}

/// 读 flow-server.toml 的 `[auth]`/`[datasource]` 段，注入 FLOW_* 环境变量。
/// 路径来源与 chassis 统一：`CONFIG_FILE`（与门户一致）→ 回退 `FLOW_CONFIG` → 默认 `flow-server.toml`。
/// env 已显式设置的键不覆盖（env 优先）。
fn apply_toml_env() {
    let path = std::env::var("CONFIG_FILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("FLOW_CONFIG").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "flow-server.toml".to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return, // 文件不存在：跳过，回退纯环境变量/默认
    };
    let file: FlowFileConfig = match toml::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "flow-server.toml 解析失败，认证段忽略，回退环境变量");
            return;
        }
    };
    // env 未设时才注入（保持「环境变量覆盖 toml」优先级）。
    let set_if_absent = |key: &str, val: &Option<String>| {
        if let Some(v) = val {
            if !v.trim().is_empty() && std::env::var(key).is_err() {
                // SAFETY: 启动早期、单线程、任何请求/引擎初始化之前设置进程环境变量。
                unsafe { std::env::set_var(key, v) }
            }
        }
    };
    set_if_absent("FLOW_AUTH_MODE", &file.auth.mode);
    set_if_absent("FLOW_JWT_ALG", &file.auth.jwt_alg);
    set_if_absent("FLOW_JWT_SECRET", &file.auth.jwt_secret);
    set_if_absent("FLOW_JWT_PUBLIC_KEY", &file.auth.jwt_public_key);
    set_if_absent("FLOW_JWT_TENANT_CLAIM", &file.auth.jwt_tenant_claim);
    set_if_absent("FLOW_JWT_ROLES_CLAIM", &file.auth.jwt_roles_claim);
    set_if_absent("FLOW_API_KEYS", &file.auth.api_keys);
    set_if_absent("FLOW_TENANCY", &file.auth.tenancy);
    set_if_absent("FLOW_PG_URL", &file.datasource.flow_pg_url);
    set_if_absent("IAM_PG_URL", &file.datasource.iam_pg_url);
}

#[tokio::main]
async fn main() -> cmx_web_chassis::Result<()> {
    // 统一启动契约（与门户 cmx-platform-app 一致）：自动读 cwd 的 .env → FLOW_* 环境变量。
    // 必须在 ChassisConfig::load / apply_toml_env（都读 env）之前，故置于 main 首行。
    dotenvy::dotenv().ok();

    // 全局 ConfigManager 装配（**所有能力中心共用的唯一那段代码**，在 cmx-service-base）：
    // CONFIG_FILE 指定的 toml + env → ConfigManager::global()。flow/portal/report/mdm 同一制度，
    // 不再各写一套。非致命：配置源缺失只 warn（各处仍有 env/默认兜底），不阻塞启动。
    if let Err(e) = cmx_service_base::init_config_manager() {
        tracing::warn!(error = %e, "全局 ConfigManager 初始化失败，回退 env/默认兜底");
    }

    // 框架级配置：FLOW_ 前缀环境变量 + 可选 flow-server.toml，默认端口 8091。
    let mut cfg = ChassisConfig::load("flow", "FLOW", "flow-server.toml");
    // 认证/数据源：读同一 toml 的 [auth]/[datasource] 段 → set_var 注入 FLOW_*（env 未设时）。
    // 必须在 auth_middleware 首次触发（AuthConfig 懒构造读 env）之前——此处 main 顶部即满足。
    apply_toml_env();
    if std::env::var("FLOW_PORT").is_err() && cfg.port == 8080 {
        cfg.port = 8091; // 未显式配端口时用 flow 默认（避开平台 8080 / demo 8090）。
    }

    // flow 专属 banner：青绿 → 蓝 渐变。
    let banner = BannerSpec::defaults("flow")
        .art(FLOW_ART)
        .tagline("  MEGA Flow · 流程引擎微服务 · cmx-web-chassis ")
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
        // 前端页只读投递（native + html）：流程微服务自持自己的 3 native + 1 html 页，字节对齐门户
        // 信封，供门户 F3 反代取页请求；独立运行时也自投递自己的界面。免认证（静态内容，且门户
        // 反代注入服务身份），故挂在 authed 之外、与 swagger 同层。
        .merge(cmx_flow_app::frontend_pages::frontend_pages_routes::<()>());
    let app_router = axum::Router::new()
        // 根路径 → 业务监控大盘（流程域：实例/待办/定义/协作…；免认证，轮询 /api/flow/v1/stats）。
        .route("/", axum::routing::get(cmx_flow_app::dashboard::dashboard))
        // Swagger UI：用**完整外部路径** `/api/flow/v1/docs` 挂在 /api nest 之外。
        // 若挂进 api_router（nest 到 /api），SwaggerUi 内部的尾斜杠重定向按自身 base(`/flow/v1/docs`)
        // 生成 → 丢失外层 `/api` 前缀 → `/api/flow/v1/docs` 跳到 `/flow/v1/docs/`(404)。用完整路径即修复。
        .merge(
            SwaggerUi::new("/api/flow/v1/docs")
                .url("/api/flow/v1/openapi.json", flow_openapi()),
        )
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
        // 经 cmx-service-base 共享注册原语（与 portal 同一 register_pg_datasources），
        // URL 仍从 flow 既有的 FLOW_PG_URL/IAM_PG_URL env 读（带默认兜底），保留 flow 契约。
        .init("datasources", |_meta| {
            Box::pin(async {
                let flow_url = std::env::var("FLOW_PG_URL").unwrap_or_else(|_| {
                    "postgres://postgres:postgres@127.0.0.1:5432/fico".to_string()
                });
                let iam_url = std::env::var("IAM_PG_URL").unwrap_or_else(|_| {
                    "postgres://postgres:postgres@127.0.0.1:5432/cmx".to_string()
                });
                let configs = vec![
                    flow_db_config(FLOW_DB_ID, &flow_url, true),
                    flow_db_config(IAM_DB_ID, &iam_url, false),
                ];
                cmx_service_base::register_pg_datasources(&configs)
                    .await
                    .map_err(|e| anyhow::anyhow!("注册数据源失败: {e}"))?;
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

/// 构造一个 flow PG 数据源配置（db_id/default 语义 flow 特有，url 从 env 来）。注册交
/// cmx-service-base 的共享 `register_pg_datasources` 原语。
fn flow_db_config(db_id: &str, url: &str, default: bool) -> DbConfig {
    DbConfig {
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
    }
}
