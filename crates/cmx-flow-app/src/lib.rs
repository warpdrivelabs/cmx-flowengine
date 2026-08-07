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
pub mod engine;
pub mod handlers;
pub mod resp;
pub mod tenancy;
pub mod tenant;
pub mod views;

pub use auth::auth as auth_middleware;
pub use engine::{
    FLOW_DB_ID, FlowRuntime, IAM_DB_ID, current_flow_db_id, flow, flow_for_tenant,
    spawn_timer_poller,
};
pub use resp::{ApiResp, FlowError, Result};
pub use tenant::{TenantCtx, current_tenant, current_user};

use axum::Router;
use axum::routing::{delete, get, post};

/// 流程模块全部路由（设计态 + 运行态，端点前缀 `/flow/*`）。
///
/// 对任意 state 泛型 `S`（`Clone + Send + Sync + 'static`）成立，故平台壳（`S = CmxAppState`）
/// 与独立壳（`S = ()`）复用同一份路由 + handler。路由表与抽核前 `FlowModule::routes()` 逐条一致。
pub fn flow_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        // —— 定义（设计器：草稿/发布/装载） ——
        .route("/flow/definitions", get(handlers::get_definitions))
        .route(
            "/flow/design/definitions",
            get(handlers::list_design_definitions),
        )
        .route(
            "/flow/definitions/draft",
            post(handlers::save_definition_draft),
        )
        .route(
            "/flow/definitions/{key}",
            get(handlers::get_definition_detail),
        )
        .route(
            "/flow/definitions/{key}/publish",
            post(handlers::publish_definition),
        )
        // —— 版本管理（对标报表版本：列表/激活/删除） ——
        .route(
            "/flow/definitions/{key}/versions",
            get(handlers::list_definition_versions),
        )
        .route(
            "/flow/definitions/{key}/versions/{version}/activate",
            post(handlers::activate_definition_version),
        )
        .route(
            "/flow/definitions/{key}/versions/{version}",
            delete(handlers::delete_definition_version),
        )
        // —— 实例 ——
        .route(
            "/flow/instances",
            get(handlers::list_instances).post(handlers::start_instance),
        )
        .route("/flow/instances/{id}", get(handlers::get_instance))
        .route("/flow/instances/{id}/children", get(handlers::get_children))
        .route(
            "/flow/instances/{id}/cancel",
            post(handlers::cancel_instance),
        )
        // —— F1/F3：变量 / 单据关联 / 意见 ——
        .route(
            "/flow/instances/{id}/variables",
            get(handlers::get_instance_variables),
        )
        .route("/flow/instances/{id}/biz", get(handlers::get_instance_biz))
        .route(
            "/flow/instances/{id}/comments",
            get(handlers::get_instance_comments),
        )
        .route(
            "/flow/biz/{table}/{bizId}/instances",
            get(handlers::get_biz_instances),
        )
        // —— F4：表单注册表 + 发起态 ——
        .route(
            "/flow/forms",
            get(handlers::list_form_bindings).post(handlers::save_form_binding),
        )
        .route("/flow/forms/{key}", get(handlers::get_form_binding))
        .route("/flow/startable", get(handlers::list_startable_definitions))
        // —— 任务 ——
        .route("/flow/tasks/my", get(handlers::get_my_tasks))
        // —— 待办中心分页列表 ——
        .route("/flow/todos/initiated", get(handlers::get_initiated))
        .route("/flow/todos/cc", get(handlers::get_cc_todos))
        .route("/flow/todos/done", get(handlers::get_done_todos))
        .route("/flow/todos/filters", get(handlers::get_todo_filters))
        .route("/flow/tasks/{id}/complete", post(handlers::complete_task))
        .route("/flow/tasks/{id}/claim", post(handlers::claim_task))
        .route("/flow/tasks/{id}/transfer", post(handlers::transfer_task))
        .route("/flow/tasks/{id}/delegate", post(handlers::delegate_task))
        .route("/flow/tasks/{id}/addsign", post(handlers::add_sign_task))
        // —— 抄送 / 定时器 / 用户 ——
        .route("/flow/users", get(handlers::list_users))
        .route("/flow/cc", get(handlers::list_cc))
        .route("/flow/cc/{id}/read", post(handlers::mark_cc_read))
        .route("/flow/timers/trigger", post(handlers::trigger_timers))
        // —— 子流程组织路由（绑定管理 + 组织树） ——
        .route("/flow/orgs", get(handlers::list_orgs))
        .route(
            "/flow/subflow-bindings",
            post(handlers::upsert_subflow_binding),
        )
        .route(
            "/flow/subflow-bindings/{key}",
            get(handlers::list_subflow_bindings),
        )
        .route(
            "/flow/subflow-bindings/id/{id}",
            delete(handlers::delete_subflow_binding),
        )
}
