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
pub mod ops;
pub mod biz_link;
pub mod collab;
pub mod conditions;
pub mod dashboard;
pub mod decisions;
pub mod engine;
pub mod events;
pub mod handlers;
pub mod identity;
pub mod observe;
pub mod openapi;
pub mod publish_gate;
pub mod resp;
pub mod simulate;
pub mod sse;
pub mod stats;
pub mod tenancy;
pub mod tenant;
pub mod views;
pub mod webhook_admin;
pub mod webhook_outbox;
pub mod webhook_store;

pub use auth::auth as auth_middleware;
/// 004 小项 fail-fast：启动期预热认证配置（auth.mode 缺失/非法即在启动阶段 panic）。
pub use auth::auth_config_warmup;
pub use observe::observe as observe_middleware;
pub use engine::{
    FLOW_DB_ID, default_flow_db_id, FlowRuntime, IAM_DB_ID, current_flow_db_id, flow, flow_for_tenant,
    spawn_async_job_poller, spawn_delivery_retention, spawn_incident_retry, spawn_timer_poller,
    spawn_webhook_delivery_poller,
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
            .route("/events", get(events::sse_events))
            // SSE 一次性票据（jwt 模式下 EventSource 无法带 header 的解法；POST 带 header 正常验签）。
            .route("/sse/ticket", post(sse::issue_ticket))
            // 设计器协同 M1（感知层）：协同 SSE + presence（仅 v1）。
            .route("/design/collab", get(collab::collab_sse))
            .route("/design/presence/join", post(collab::presence_join))
            .route("/design/presence/heartbeat", post(collab::presence_heartbeat))
            .route("/design/presence/select", post(collab::presence_select))
            .route("/design/presence/leave", post(collab::presence_leave))
            .route("/design/op", post(collab::presence_op)),
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
        .route("/definitions/validate", post(handlers::validate_definition))
        // —— 设计态模拟（试跑：路径/分支/办理人/决策，无持久实例） ——
        .route("/definitions/simulate", post(simulate::simulate_definition))
        .route("/definitions/{key}", get(handlers::get_definition_detail))
        .route("/definitions/{key}/variables", get(handlers::get_definition_variables))
        .route("/definitions/variables/validate", post(handlers::validate_definition_variables))
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
        .route(
            "/instances/{id}/activities",
            get(handlers::get_instance_activities),
        )
        // —— 实例迁移（A9：节点映射迁到新定义版本） ——
        .route(
            "/instances/{id}/migrate",
            post(handlers::migrate_instance),
        )
        .route(
            "/instances/{id}/migrate/validate",
            post(handlers::validate_migration),
        )
        .route("/instances/{id}/cancel", post(handlers::cancel_instance))
        .route("/instances/{id}/withdraw", post(handlers::withdraw_instance))
        .route("/instances/{id}/withdrawable", get(handlers::get_withdrawable))
        .route(
            "/instances/{id}/retry-incident",
            post(handlers::retry_incident),
        )
        .route(
            "/instances/{id}/set-variables",
            post(handlers::set_instance_variables),
        )
        .route("/instances/{id}/suspend", post(handlers::suspend_instance))
        .route("/instances/{id}/resume", post(handlers::resume_instance))
        .route("/instances/{id}/jump", post(handlers::jump_instance))
        // —— F1/F3：变量 / 单据关联 / 意见 ——
        .route(
            "/instances/{id}/variables",
            get(handlers::get_instance_variables),
        )
        // —— 变量变更历史 + TTL 归档 ——
        .route(
            "/instances/{id}/variables/history",
            get(handlers::get_instance_var_history),
        )
        .route("/admin/var-history/sweep", post(handlers::sweep_var_history))
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
        .route("/forms/delete", post(handlers::delete_form_binding))
        .route("/startable", get(handlers::list_startable_definitions))
        // —— 任务 ——
        .route("/tasks/my", get(handlers::get_my_tasks))
        // —— 待办中心分页列表 ——
        .route("/todos/initiated", get(handlers::get_initiated))
        .route("/todos/cc", get(handlers::get_cc_todos))
        .route("/todos/done", get(handlers::get_done_todos))
        .route("/todos/filters", get(handlers::get_todo_filters))
        .route("/tasks/{id}/complete", post(handlers::complete_task))
        .route("/tasks/{id}/reject", post(handlers::reject_task))
        .route("/tasks/{id}/reject-targets", get(handlers::get_reject_targets))
        .route("/tasks/{id}/claim", post(handlers::claim_task))
        .route("/tasks/{id}/transfer", post(handlers::transfer_task))
        .route("/tasks/{id}/delegate", post(handlers::delegate_task))
        .route("/tasks/{id}/addsign", post(handlers::add_sign_task))
        .route("/tasks/{id}/urge", post(handlers::urge_task))
        // —— A4：相关消息（外部回调唤醒等待中的流程） ——
        .route("/messages/correlate", post(handlers::correlate_message))
        // —— 抄送 / 定时器 / 用户 ——
        .route("/users", get(handlers::list_users))
        .route("/cc", get(handlers::list_cc))
        .route("/cc/{id}/read", post(handlers::mark_cc_read))
        .route("/timers/trigger", post(handlers::trigger_timers))
        // —— 外部 worker：异步 Job 执行器（P1；SKIP LOCKED 集群安全） ——
        .route("/async-jobs/acquire", post(handlers::acquire_async_jobs))
        .route(
            "/async-jobs/{id}/complete",
            post(handlers::complete_async_job),
        )
        .route("/async-jobs/{id}/fail", post(handlers::fail_async_job))
        // —— 外部 Worker Task（A7；按 topic 拉取，complete/fail 复用 async-jobs 端点） ——
        .route(
            "/external-worker/jobs/acquire",
            post(handlers::acquire_external_worker_jobs),
        )
        // —— 死信队列（P2；Job 重试耗尽的托底，运维台可见可重投可删除） ——
        .route("/dead-letter-jobs", get(handlers::list_dead_letter_jobs))
        .route(
            "/dead-letter-jobs/{id}/retry",
            post(handlers::retry_dead_letter_job),
        )
        .route(
            "/dead-letter-jobs/{id}",
            delete(handlers::discard_dead_letter_job),
        )
        // —— 子流程路由（绑定管理 + 维度/条目；RD2 泛化，org 为内建维度） ——
        .route("/orgs", get(handlers::list_orgs))
        .route("/dimensions", get(handlers::list_dimensions))
        .route("/dimension/{dimKey}/entries", get(handlers::list_dimension_entries))
        // —— 身份 / 维度回连端点（⑤ + RD5 服务端；独立部署经 Http* resolver 回连有 IAM 访问的提供方）——
        .route("/identity/resolve", post(handlers::resolve_identity))
        .route("/dimensions/ancestors", get(handlers::get_dimension_ancestors))
        .route("/subflow-bindings", post(handlers::upsert_subflow_binding))
        .route(
            "/subflow-bindings/{key}",
            get(handlers::list_subflow_bindings),
        )
        .route(
            "/subflow-bindings/id/{id}",
            delete(handlers::delete_subflow_binding),
        )
        // —— 分支条件（可视化构造器后端：求值/语法校验/函数目录，纯函数） ——
        .route("/conditions/eval", post(conditions::eval))
        .route("/conditions/validate", post(conditions::validate))
        .route("/conditions/functions", get(conditions::functions))
        // —— A3：决策表（注册 + 试算，businessRuleTask 引用） ——
        .route("/decisions", post(decisions::register_decision).get(decisions::list_decisions))
        .route("/decisions/{key}", delete(decisions::delete_decision).get(decisions::get_decision))
        .route("/decisions/evaluate", post(decisions::evaluate_decision))
        // —— 内建身份主数据（P0-c：仅 local 模式可写；external 只读/闲置） ——
        .route("/identity/mode", get(identity::get_mode))
        .route(
            "/identity/users/{id}/roles",
            post(identity::set_user_roles),
        )
        .route(
            "/identity/{entity}",
            get(identity::list).post(identity::upsert),
        )
        .route("/identity/{entity}/{id}", delete(identity::delete))
        // —— 监控大盘数据源（引擎全状态聚合，根路径大盘轮询） ——
        .route("/stats", get(stats::flow_stats))
        // —— A6：节点耗时/瓶颈/SLA 分析（基于 hi_task 归档） ——
        .route("/analytics/node-timing", get(stats::node_timing))
        // —— 大盘明细下钻（每数据点可点击 → 该维度明细行，每行全字段可展开） ——
        .route("/stats/detail", get(stats::stats_detail))
        // —— 客户端连接监控（连接数/协议/身份/方法/参数/返回值等全维度） ——
        .route("/clients", get(observe::client_stats))
        // —— 出站 webhook 订阅管理（001 方案 §五：11 端点随 M1；rebuild 随 M3） ——
        .route(
            "/webhook-subscriptions/query",
            post(webhook_admin::query_subscriptions),
        )
        .route(
            "/webhook-subscriptions/detail",
            get(webhook_admin::get_subscription_detail),
        )
        .route(
            "/webhook-subscriptions/save",
            post(webhook_admin::save_subscription),
        )
        .route(
            "/webhook-subscriptions/delete",
            post(webhook_admin::delete_subscription),
        )
        .route(
            "/webhook-subscriptions/set-active",
            post(webhook_admin::set_subscription_active),
        )
        .route(
            "/webhook-subscriptions/test",
            post(webhook_admin::test_subscription),
        )
        .route(
            "/webhook-subscriptions/channels",
            get(webhook_admin::list_channels),
        )
        // —— v2.4 三级路由：L3 定义订阅（自助 subscribe/unsubscribe，占位角色 + fail-close）
        //    + 运维计数（丢弃/幽灵/点查失败） ——
        .route(
            "/webhook-subscriptions/definitions/subscribe",
            post(webhook_admin::subscribe_definitions),
        )
        .route(
            "/webhook-subscriptions/definitions/unsubscribe",
            post(webhook_admin::unsubscribe_definitions),
        )
        .route(
            "/webhook-subscriptions/mon",
            get(webhook_admin::webhook_mon),
        )
        .route(
            "/webhook-deliveries/query",
            post(webhook_admin::query_deliveries),
        )
        .route(
            "/webhook-deliveries/retry",
            post(webhook_admin::retry_deliveries),
        )
        .route(
            "/webhook-deliveries/skip",
            post(webhook_admin::skip_deliveries),
        )
        .route(
            "/webhook-deliveries/purge",
            post(webhook_admin::purge_deliveries),
        )
        // —— 运维面（技术债 012/013，批次 6）：jobs/incidents 清单 + Prometheus 指标 ——
        .route("/instances/query", post(ops::query_instances))
        .route("/jobs/query", post(ops::query_jobs))
        // —— 001-M3：缺行补发（12 端点收口） ——
        .route(
            "/webhook-subscriptions/rebuild",
            post(webhook_admin::rebuild_subscription),
        )
        .route("/incidents/query", post(ops::query_incidents))
        .route("/metrics", get(ops::prometheus_metrics))
}

