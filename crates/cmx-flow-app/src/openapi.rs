//! OpenAPI 契约（S3 headless）：对外正式 API 文档 + swagger UI。
//!
//! **务实做法**：不给 21 个 handler 逐个加 `#[utoipa::path]` 注解（那要改 handlers.rs 且噪声大），
//! 而是用 utoipa 的 `OpenApiBuilder` **手工构建** paths——每条路由一个 PathItem，描述 method/tag/摘要。
//! 响应体统一是 `{code,msg,data}` 信封（见 resp.rs），故 schema 用通用对象即可，不逐 DTO 细化。
//! 供第三方看清有哪些端点、方法、路径参数；细粒度 schema 可后续增量补。
//!
//! 挂载：`GET /api/flow/v1/openapi.json`（规范）+ `GET /api/flow/v1/docs`（swagger UI）——均免认证。

use utoipa::openapi::path::{Operation, OperationBuilder, PathItem, Parameter, ParameterIn};
use utoipa::openapi::request_body::RequestBodyBuilder;
use utoipa::openapi::{
    ContentBuilder, HttpMethod, InfoBuilder, OpenApi, OpenApiBuilder, PathsBuilder, Ref,
    ResponseBuilder, ResponsesBuilder, ServerBuilder,
};

/// 构建 flow v1 的 OpenAPI 规范（手工 paths）。
pub fn flow_openapi() -> OpenApi {
    let mut paths = PathsBuilder::new();

    // 逐端点登记（method, path, tag, 摘要, 路径参数名...）。path 相对 /api/flow/v1。
    for (method, path, tag, summary, params) in ENDPOINTS {
        let mut op = OperationBuilder::new()
            .summary(Some(*summary))
            .tag(*tag)
            .responses(
                ResponsesBuilder::new()
                    .response(
                        "200",
                        ResponseBuilder::new()
                            .description("统一信封 {code,msg,data}")
                            .content(
                                "application/json",
                                ContentBuilder::new().build(),
                            ),
                    )
                    .build(),
            );
        for p in *params {
            op = op.parameter(
                Parameter::builder()
                    .name(*p)
                    .parameter_in(ParameterIn::Path)
                    .required(utoipa::openapi::Required::True)
                    .schema(Some(Ref::from_schema_name("string"))),
            );
        }
        // 写方法有请求体（通用 JSON）。
        let is_post = matches!(method, HttpMethod::Post);
        let op = if is_post {
            op.request_body(Some(
                RequestBodyBuilder::new()
                    .content("application/json", ContentBuilder::new().build())
                    .build(),
            ))
            .build()
        } else {
            op.build()
        };
        paths = paths.path(prefixed(path), path_item(method.clone(), op));
    }

    OpenApiBuilder::new()
        .info(
            InfoBuilder::new()
                .title("cmx-flow headless API")
                .version("1.0")
                .description(Some(
                    "流程引擎独立微服务对外契约 v1。响应统一 {code,msg,data}。\
                     认证：Authorization: Bearer <JWT> 或 X-Api-Key（服务间）。\
                     多租户：tenant 由 JWT claim / API Key 绑定决定。\
                     实时：GET /events 为 SSE 事件流。",
                )),
        )
        .servers(Some(vec![
            ServerBuilder::new().url("/api/flow/v1").build(),
        ]))
        .paths(paths.build())
        .build()
}

/// 把 path 加上 v1 前缀（OpenAPI 里写全路径，server 也带前缀，二者叠加时以 path 为准展示）。
fn prefixed(p: &str) -> String {
    format!("/api/flow/v1{p}")
}

/// 按 method 造 PathItem。
fn path_item(method: HttpMethod, op: Operation) -> PathItem {
    PathItem::new(method, op)
}

/// 端点清单：(HTTP 方法, 路径[相对 v1], tag, 摘要, 路径参数名列表)。
/// 与 lib.rs flow_routes_inner 的 21 路由 + SSE 对齐。
type Ep = (HttpMethod, &'static str, &'static str, &'static str, &'static [&'static str]);
const ENDPOINTS: &[Ep] = &[
    // 定义（设计态）
    (HttpMethod::Get, "/definitions", "定义", "引擎已装载定义（画图用）", &[]),
    (HttpMethod::Get, "/design/definitions", "定义", "设计器定义列表（草稿+已发布）", &[]),
    (HttpMethod::Post, "/definitions/draft", "定义", "存草稿（试编译校验）", &[]),
    (HttpMethod::Get, "/definitions/{key}", "定义", "定义详情（含 BPMN XML）", &["key"]),
    (HttpMethod::Post, "/definitions/{key}/publish", "定义", "发布（版本+1）", &["key"]),
    (HttpMethod::Get, "/definitions/{key}/versions", "定义", "版本列表", &["key"]),
    (HttpMethod::Post, "/definitions/{key}/versions/{version}/activate", "定义", "激活指定版本", &["key", "version"]),
    // 实例（运行态）
    (HttpMethod::Get, "/instances", "实例", "列实例", &[]),
    (HttpMethod::Post, "/instances", "实例", "起实例", &[]),
    (HttpMethod::Get, "/instances/{id}", "实例", "实例详情/轨迹", &["id"]),
    (HttpMethod::Get, "/instances/{id}/children", "实例", "子实例", &["id"]),
    (HttpMethod::Post, "/instances/{id}/cancel", "实例", "撤单/取消", &["id"]),
    (HttpMethod::Get, "/instances/{id}/variables", "实例", "实例变量", &["id"]),
    (HttpMethod::Get, "/instances/{id}/comments", "实例", "审批意见历史", &["id"]),
    // 任务 + 待办
    (HttpMethod::Get, "/tasks/my", "待办", "我的待办（跨实例）", &[]),
    (HttpMethod::Get, "/todos/initiated", "待办", "我发起的", &[]),
    (HttpMethod::Get, "/todos/cc", "待办", "抄送我的", &[]),
    (HttpMethod::Get, "/todos/done", "待办", "我已办", &[]),
    (HttpMethod::Post, "/tasks/{id}/complete", "待办", "办结任务", &["id"]),
    (HttpMethod::Post, "/tasks/{id}/claim", "待办", "认领候选任务", &["id"]),
    (HttpMethod::Post, "/tasks/{id}/transfer", "待办", "转办", &["id"]),
    (HttpMethod::Post, "/tasks/{id}/delegate", "待办", "委派", &["id"]),
    (HttpMethod::Post, "/tasks/{id}/addsign", "待办", "加签", &["id"]),
    // 表单 + 发起态
    (HttpMethod::Get, "/forms", "表单", "表单绑定列表", &[]),
    (HttpMethod::Post, "/forms", "表单", "upsert 表单绑定", &[]),
    (HttpMethod::Get, "/forms/{key}", "表单", "取表单绑定", &["key"]),
    (HttpMethod::Get, "/startable", "表单", "可发起流程列表", &[]),
    // 实时
    (HttpMethod::Get, "/events", "实时", "SSE 生命周期事件流", &[]),
    // 事件订阅管理（20260902 重构：19 端点——订阅者 9 + 投递 5 + 分组 3 + 定义分组 2）
    (HttpMethod::Post, "/event-subscribers/query", "事件订阅", "订阅者分页列表（secret 掩码 + 健康度投影）", &[]),
    (HttpMethod::Get, "/event-subscribers/detail", "事件订阅", "订阅者详情（id 走 query；secret 掩码）", &[]),
    (HttpMethod::Post, "/event-subscribers/save", "事件订阅", "新建/更新订阅者（rules 整体校验；secret 掩码回传=沿用旧值）", &[]),
    (HttpMethod::Post, "/event-subscribers/delete", "事件订阅", "删除订阅者（仅停用态且无待投/死信行可删）", &[]),
    (HttpMethod::Post, "/event-subscribers/set-active", "事件订阅", "启停订阅者（停用即不再生成投递行）", &[]),
    (HttpMethod::Post, "/event-subscribers/test", "事件订阅", "测试投递（不校验规则，验证回调连通与签名；1 分钟 3 次限流）", &[]),
    (HttpMethod::Get, "/event-subscribers/channels", "事件订阅", "可用通道及 channel_config schema", &[]),
    (HttpMethod::Post, "/event-subscribers/rules/preview", "事件订阅", "规则预演：规则集 → 命中定义清单（后端权威、死组标注）+ 样例事件命中", &[]),
    (HttpMethod::Post, "/event-subscribers/rebuild", "事件订阅", "缺行补发：时间窗内终态实例事件按订阅者规则重放进投递管线（幂等）", &[]),
    (HttpMethod::Post, "/event-deliveries/query", "事件订阅", "投递流水分页（订阅者/状态/定义key/命中规则过滤）", &[]),
    (HttpMethod::Post, "/event-deliveries/stats", "事件订阅", "投递统计 KPI（时间窗 + emit 口径成功率 + emit 可观测计数）", &[]),
    (HttpMethod::Post, "/event-deliveries/retry", "事件订阅", "死信重发（DEAD/租约过期 IN_FLIGHT → PENDING）", &[]),
    (HttpMethod::Post, "/event-deliveries/skip", "事件订阅", "死信处置（DEAD/PENDING → SKIPPED 留痕）", &[]),
    (HttpMethod::Post, "/event-deliveries/purge", "事件订阅", "清理 DONE/SKIPPED 行（beforeDays 默认 7）", &[]),
    (HttpMethod::Get, "/definition-groups", "事件订阅", "流程分组列表（含每组定义数）", &[]),
    (HttpMethod::Post, "/definition-groups/save", "事件订阅", "新建/更新分组（排序即改 sortNo；启停只影响定义页展示位）", &[]),
    (HttpMethod::Post, "/definition-groups/delete", "事件订阅", "删除分组（组内有定义 → 400）", &[]),
    (HttpMethod::Post, "/definitions/query", "事件订阅", "定义分页列表（key/name/state/groupId/未分组过滤；附分组名）", &[]),
    (HttpMethod::Post, "/definitions/set-group", "事件订阅", "批量设置定义分组（groupId null = 移出分组）", &[]),
    // 运维面（技术债 012/013 批次 6）+ 实例分页（016）
    (HttpMethod::Post, "/instances/query", "实例", "实例清单分页查询（合规 POST+body；存量 GET /instances 保留）", &[]),
    (HttpMethod::Post, "/jobs/query", "运维", "定时器作业清单（跨实例，含租约占用方）", &[]),
    (HttpMethod::Post, "/incidents/query", "运维", "跨实例故障清单（OPEN/RESOLVED 过滤）", &[]),
    (HttpMethod::Get, "/metrics", "运维", "Prometheus text 业务指标（投递/故障/死信/运行实例/业务失败计数）", &[]),
];

// **存量 OpenAPI 缺口清单**（技术债 016 止损口径：新增端点强制收录如上；存量 v1 的缺口
// 罗列如下备查，整体补录属「从路由表派生生成」专项，不混入批次 7）：
// - 实例干预类：`POST /instances/{id}/suspend|resume|jump|withdraw|set-variables`、
//   `POST /instances/{id}/migrate`、`POST /instances/{id}/migrate/validate`、
//   `POST /instances/{id}/retry-incident`、`POST /instances/{id}/correlate`、
//   `GET /instances/{id}/variables|comments|biz|variables/history|activities`、`GET /instances/{id}/children`
// - 任务类：`GET /tasks/my`、`GET /tasks/{id}/reject-targets`、`POST /tasks/{id}/reject`、`POST /tasks/{id}/urge`
// - 死信/异步作业：`GET /dead-letter-jobs`、`POST /dead-letter-jobs/{id}/retry`、
//   `DELETE /dead-letter-jobs/{id}`、`POST /async-jobs/acquire`、`POST /async-jobs/{id}/complete`、
//   `POST /async-jobs/{id}/fail`、`POST /external-worker/acquire`
// - 消息/定义管理：`POST /messages/{...}`、`POST /definitions/draft|publish|activate|validate|simulate`、
//   `GET|DELETE /decisions/{key}`、`POST /decisions/evaluate`
// - 子流程路由：`GET /subflow-bindings/{key}`、`POST /subflow-bindings/save`、`POST /subflow-bindings/delete`
// - 运维/身份：`GET /stats`、`GET /_mon*`、`POST /identity/resolve`、`GET /users`、`GET /orgs`、
//   `GET /dimensions/*`、协同 `POST /design/collab/*`、SSE presence 系
// - 删除/缺陷类（v2 整改对象）：5 个 DELETE 端点、45 个 Path Variable 模式
