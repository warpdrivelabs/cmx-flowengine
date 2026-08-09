//! cmx-flow-app —— cmx-flow 流程引擎的**平台中立应用层**（抽核，方案 §5「一芯」）。
//!
//! 聚合：引擎单例（[`engine`]）+ 业务集成表（[`biz_link`]）+ 视图组装（[`views`]）+
//! 响应信封（[`resp`]，自持不借 cmx-api）+ 全部 axum handler（[`handlers`]）+ 路由表
//! [`flow_routes`]。
//!
//! **一芯三壳**：本 crate 是「芯」。handler 丢弃了原来绑定不用的
//! `State/CmxSvrContext` 两提取器，故与宿主 AppState 类型无关——[`flow_routes::<S>()`] 对
//! 任意 state 泛型 `S` 成立：
//!   - 平台壳 `cmx-flow-api`（cmx-container 内）：`flow_routes::<cmx_api::CmxAppState>()`；
//!   - 独立壳 `cmx-flow-server`（本 workspace）：`flow_routes::<()>()`。
//! 两壳复用同一 handler + 同一路由表，**零业务漂移**。

pub mod auth;
pub mod biz_link;
pub mod dashboard;
pub mod engine;
pub mod events;
pub mod handlers;
pub mod observe;
pub mod openapi;
pub mod resp;
pub mod stats;
pub mod tenancy;
pub mod tenant;
pub mod views;

pub use auth::auth as auth_middleware;
pub use observe::observe as observe_middleware;
pub use engine::{
    FLOW_DB_ID, FlowRuntime, IAM_DB_ID, current_flow_db_id, flow, flow_for_tenant,
    spawn_timer_poller,
};
pub use resp::{ApiResp, FlowError, Result};
pub use tenant::{TenantCtx, current_tenant, current_user, identity_snapshot};

use axum::Router;
use axum::routing::{delete, get, post};

/// 流程模块全部路由，**旧前缀 `/flow/*`**（兼容既有前端/平台壳，零回归）。
///
/// 对任意 state 泛型 `S`（`Clone + Send + Sync + 'static`）成立，故平台壳（`S = CmxAppState`）
/// 与独立壳（`S = ()`）复用同一份路由 + handler。
pub fn flow_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().nest("/flow", flow_routes_inner::<S>())
}

/// 流程模块全部路由，**v1 正式契约前缀 `/flow/v1/*`**（S3 headless）。
///
/// 与 [`flow_routes`] 同一张路由表 + handler，只是前缀不同——第三方据此稳定集成，破坏性变更进 v2。
/// 额外挂 SSE 事件流 `/flow/v1/events`（仅 v1 有）。
pub fn flow_routes_v1<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().nest(
        "/flow/v1",
        flow_routes_inner::<S>()
            // SSE 实时事件流（仅 v1；第三方 UI 增量刷新替轮询）。
            .route("/events", get(events::sse_events)),
    )
}

/// 路由表本体（路径相对前缀，不含 `/flow`）。由 [`flow_routes`]/[`flow_routes_v1`] 各自 nest 加前缀。
fn flow_routes_inner<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        // —— 定义（设计器：草稿/发布/装载） ——
        .route("/definitions", get(handlers::get_definitions))
        .route("/design/definitions", get(handlers::list_design_definitions))
        .route("/definitions/draft", post(handlers::save_definition_draft))
        .route("/definitions/{key}", get(handlers::get_definition_detail))
        .route(
            "/definitions/{key}/publish",
            post(handlers::publish_definition),
        )
        // —— 版本管理（对标报表版本：列表/激活/删除） ——
        .route(
            "/definitions/{key}/versions",
            get(handlers::list_definition_versions),
        )
        .route(
            "/definitions/{key}/versions/{version}/activate",
            post(handlers::activate_definition_version),
        )
        .route(
            "/definitions/{key}/versions/{version}",
            delete(handlers::delete_definition_version),
        )
        // —— 实例 ——
        .route(
            "/instances",
            get(handlers::list_instances).post(handlers::start_instance),
        )
        .route("/instances/{id}", get(handlers::get_instance))
        .route("/instances/{id}/children", get(handlers::get_children))
        .route("/instances/{id}/cancel", post(handlers::cancel_instance))
        // —— F1/F3：变量 / 单据关联 / 意见 ——
        .route(
            "/instances/{id}/variables",
            get(handlers::get_instance_variables),
        )
        .route("/instances/{id}/biz", get(handlers::get_instance_biz))
        .route(
            "/instances/{id}/comments",
            get(handlers::get_instance_comments),
        )
        .route(
            "/biz/{table}/{bizId}/instances",
            get(handlers::get_biz_instances),
        )
        // —— F4：表单注册表 + 发起态 ——
        .route(
            "/forms",
            get(handlers::list_form_bindings).post(handlers::save_form_binding),
        )
        .route("/forms/{key}", get(handlers::get_form_binding))
        .route("/startable", get(handlers::list_startable_definitions))
        // —— 任务 ——
        .route("/tasks/my", get(handlers::get_my_tasks))
        // —— 待办中心分页列表 ——
        .route("/todos/initiated", get(handlers::get_initiated))
        .route("/todos/cc", get(handlers::get_cc_todos))
        .route("/todos/done", get(handlers::get_done_todos))
        .route("/todos/filters", get(handlers::get_todo_filters))
        .route("/tasks/{id}/complete", post(handlers::complete_task))
        .route("/tasks/{id}/claim", post(handlers::claim_task))
        .route("/tasks/{id}/transfer", post(handlers::transfer_task))
        .route("/tasks/{id}/delegate", post(handlers::delegate_task))
        .route("/tasks/{id}/addsign", post(handlers::add_sign_task))
        // —— 抄送 / 定时器 / 用户 ——
        .route("/users", get(handlers::list_users))
        .route("/cc", get(handlers::list_cc))
        .route("/cc/{id}/read", post(handlers::mark_cc_read))
        .route("/timers/trigger", post(handlers::trigger_timers))
        // —— 子流程组织路由（绑定管理 + 组织树） ——
        .route("/orgs", get(handlers::list_orgs))
        .route("/subflow-bindings", post(handlers::upsert_subflow_binding))
        .route(
            "/subflow-bindings/{key}",
            get(handlers::list_subflow_bindings),
        )
        .route(
            "/subflow-bindings/id/{id}",
            delete(handlers::delete_subflow_binding),
        )
        // —— 监控大盘数据源（引擎全状态聚合，根路径大盘轮询） ——
        .route("/stats", get(stats::flow_stats))
        // —— 大盘明细下钻（每数据点可点击 → 该维度明细行，每行全字段可展开） ——
        .route("/stats/detail", get(stats::stats_detail))
        // —— 客户端连接监控（连接数/协议/身份/方法/参数/返回值等全维度） ——
        .route("/clients", get(observe::client_stats))
}

