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
];
