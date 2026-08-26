/*
 * cmx-flow 独立流程微服务 HTTP 服务器（方案 S0「flow-http 脱平台」）。
 *
 * 采用通用骨架 cmx-web-chassis：与 report-server / mdm-server 同一套启动/日志/中间件/优雅关闭/
 * banner。main 只填 ServiceSpec——flow 路由 + 两个启动钩子（注册数据源、起引擎定时器 poller）+
 * flow 专属 banner/配色，交 chassis::run 装配。零 cmx-api 依赖。
 *
 * 配置（flow-server.toml，路径由 CONFIG_FILE 指定；[server] 框架键 env 覆盖 SERVER__*，与 ConfigManager `__` 约定同名）：
 *   [server] host/port/log_dir/log_level/graceful_timeout_secs（默认 0.0.0.0:8091）
 *   [[databases]] 标准数据源段 ×2（fico-db=运行态+定义库 default=true；primary=IAM 候选人/组织库；
 *   缺段/缺库启动失败，无内置 URL 兜底）
 *   [auth] 段 → cmx-flow-app 认证中间件 ConfigManager 直读（env 覆盖 AUTH__*）
 *   适配器仍走 env：FLOW_IDENTITY_MODE / FLOW_SUBFLOW_MODE / FLOW_DELEGATE_MODE + *_URL（S1）
 *
 * 用法：
 *   cargo run -p cmx-flow-server   # 读 cwd 的 flow-server.toml（或 CONFIG_FILE 指定）
 *   curl http://127.0.0.1:8091/api/flow/definitions
 */

use cmx_flow_app::openapi::flow_openapi;
use cmx_flow_app::{
    FLOW_DB_ID, IAM_DB_ID, flow_routes, flow_routes_v1, spawn_async_job_poller, spawn_timer_poller,
};
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

#[tokio::main]
async fn main() -> cmx_web_chassis::Result<()> {
    // 统一启动契约（与门户 cmx-platform-app 一致）：自动读 cwd 的 .env（CONFIG_FILE 等）。
    // 必须在 ChassisConfig::load / init_infra（都读 env）之前，故置于 main 首行。
    dotenvy::dotenv().ok();

    // 基础设施装配（与门户 run_platform 同一制度）：本地 toml ← Nacos 远程配置中心 ← env
    // 三源 ConfigManager + 注册中心客户端（自注册 + 实例缓存 + 30s 服务列表同步）。开关默认
    // 全关（未开 NACOS_ENABLED 时走 Mock，纯本地 toml+env，行为与接入前一致）；开启后
    // create 阶段强依赖 Nacos 可达，失败即中止启动（register 阶段失败仅 warn）。
    cmx_service_base::init_infra()
        .await
        .map_err(|e| cmx_web_chassis::ChassisError::Config(format!("基础设施初始化失败: {e}")))?;

    // 框架级配置：[server] 段 + SERVER__* env 覆盖（与 ConfigManager `__` 约定同名）+ 可选 flow-server.toml，默认端口 8091。
    let mut cfg = ChassisConfig::load("flow", "flow-server.toml");
    if std::env::var("SERVER__PORT").is_err() && cfg.port == 8080 {
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
        // 资产目录遵循规范 v2；错误体经 FlowError 保持历史 code=4 语义。
        .merge(cmx_form::serve::frontend_pages_routes::<(), cmx_flow_app::FlowError>(
            cmx_form::serve::PageServeConfig::from_assets(),
        ));
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
        // 钩子① 注册数据源——平台封装：BaseConfig（标准 [[databases]] 段，ConfigManager 三源
        // 合并）+ 共享注册原语 register_pg_datasources。要求有 default 库 + fico-db/primary
        // 两个 db_id 齐备（引擎单例按常量寻址）；缺段/缺库启动失败（无内置 URL 兜底）。
        // 注册建池即首连验证——库不可达同样终止启动（fail-fast）。
        .init("datasources", |_meta| {
            Box::pin(async {
                let base = cmx_service_base::BaseConfig::from_config_manager()
                    .map_err(|e| anyhow::anyhow!("读取 [[databases]] 配置失败: {e}"))?;
                if base.databases.is_empty() {
                    return Err(anyhow::anyhow!(
                        "flow-server.toml 未配置 [[databases]]（需 db_id=\"{FLOW_DB_ID}\"(default) + \"{IAM_DB_ID}\" 两库）"
                    ));
                }
                if !base.databases.iter().any(|d| d.default) {
                    return Err(anyhow::anyhow!("[[databases]] 缺少 default=true 的库"));
                }
                for id in [FLOW_DB_ID, IAM_DB_ID] {
                    if !base.databases.iter().any(|d| d.db_id == id) {
                        return Err(anyhow::anyhow!(
                            "[[databases]] 缺少 db_id=\"{id}\"（引擎按常量寻址）"
                        ));
                    }
                }
                let ids: Vec<&str> = base.databases.iter().map(|d| d.db_id.as_str()).collect();
                cmx_service_base::register_pg_datasources(&base.databases)
                    .await
                    .map_err(|e| anyhow::anyhow!("注册数据源失败: {e}"))?;
                tracing::info!(databases = ?ids, "✅ 流程引擎 tokio-pg 数据源已注册（[[databases]] 配置驱动）");
                Ok(())
            })
        })
        // 钩子② 起定时器 poller（内部 flow() 构建引擎单例：建表 + 注入 resolver/router +
        // 装载定义）与异步 Job 执行器（SKIP LOCKED 集群安全）。DB 不可达已在钩子① 探活
        // fail-fast；此处失败（建表/装载等）同样终止启动——带病启动端点只会全返错。
        .init("engine", |_meta| {
            Box::pin(async {
                spawn_timer_poller()
                    .await
                    .map_err(|e| anyhow::anyhow!("流程引擎初始化失败: {e}"))?;
                spawn_async_job_poller()
                    .await
                    .map_err(|e| anyhow::anyhow!("异步 Job 执行器启动失败: {e}"))?;
                Ok(())
            })
        });

    let result = run(spec).await;
    // serve 结束（收到关闭信号或自然退出）：注销注册中心实例后再返回——不用 `?` 提前返回，
    // 否则 Err 路径会跳过注销（实例要等 Nacos 心跳超时才摘除）。
    cmx_service_base::shutdown_infra().await;
    result
}
