# S6 · 平台 center_client 对接（把独立流程微服务接回平台）

S6 是独立流程微服务路线（S0→S6）的**收口**：把 S0–S5 抽出的独立 flow-server 经平台既有 `center_client` 机制接回 CMXPortalManager，实现「后端一芯双壳」——同一引擎核，既能进程内嵌、也能独立部署，**平台前端与其余装配全零改**。

## 一句话

平台的 `/api/flow/*` 由**配置**决定走哪条路：`[center_client.urls].flow` 空=进程内嵌引擎（今天），非空=反代到独立 flow-server。切换只改一行配置。

## 三块改动

### ① flowengine 认证桥（`cmx-flow-app/src/auth.rs`）

独立 flow-server 收到平台转发请求时，要认出**服务身份**又要知道**真实办理人**。认证桥在既有 `auth` 中间件的 API Key 分支里，额外解**委托用户令牌**：

```
平台出站三层头（对齐 remote_importers::apply_auth_headers）：
  X-API-Key: cmx_sk_platform_to_flow      ← 平台服务身份（[auth].api_keys 命中）
  X-Delegated-User-Token: Bearer <用户JWT> ← 当前登录用户原始令牌（真实办理人）
  X-Request-Id: <链路ID>

认证桥逻辑：
  API Key 命中 → 服务身份已验
    ├ 带委托令牌 → 验签取 { user, tenant, roles+["service"] }   ← 租户取委托 claim
    └ 无委托令牌 → 纯服务调用 { tenant=key绑定, roles=["service"] } （S3 语义）
```

**关键**：多租户下一个服务 key 服务多个平台租户，故租户**优先取委托令牌的 tenant claim**，而非 key 绑定租户。委托令牌**始终验签**（`auth.jwt_secret` 须与平台签发方一致），验签失败退化为纯服务调用（不 401，服务身份已验）。

抽了 `decode_claims(token, cfg)` 供 Bearer 与委托令牌两路复用。5 个单测覆盖：解 claim、错密钥拒签、委托令牌覆盖 key 租户、裸令牌容忍、缺/坏令牌退化。

### ② 平台反代壳（`cmx-flow-api/src/proxy.rs` 新增 `FlowProxyModule`）

`FlowModule`（内嵌）的对偶：`impl ModuleRoutes`、同 `/flow/*` 前缀，但把 `/api/flow/{rest}` 转发到 `{flow_base}/api/flow/v1/{rest}`（**升级到 v1 正式契约**）。

- 捕获 `/flow` 与 `/flow/{*rest}`，query 透传、请求/响应体**双向流式**（`reqwest::Body::wrap_stream` + `Body::from_stream`），SSE `/events` 的 `text/event-stream` 逐块透传。
- 出站注入三层头（复用 `context_scope::current_original_token()` / `current_request_id()`）。
- 逐跳头（connection/keep-alive/transfer-encoding/host/content-length…）转发时剥掉。
- 远程不可达 → 502 信封 `{code:502,msg}`。

### ③ 平台配置开关（`web-server/src/routes.rs` + `main.rs`）

```rust
// routes.rs：读 [center_client.urls].flow 二选一
match flow_remote_base() {
    Some(base) => router.merge(FlowProxyModule::new(base, api_key).routes()), // 转发
    None       => router.merge(FlowModule.routes()),                           // 内嵌（默认）
}
// main.rs：代理态不起本进程引擎 poller（引擎在远程）
if routes::flow_is_proxied() { info!("独立微服务模式，不启内嵌 poller") }
else { spawn_timer_poller().await }
```

API Key 复用平台既有 `[service_auth].outgoing_api_key`（`load_outgoing_credential()`）。

## 平台侧配置（dev-local.toml）

```toml
# 独立模式：指向 flow-server（空/不配 = 内嵌，零回归）
[center_client]
mode = "http_url"
[center_client.urls]
flow = "http://localhost:8091"          # 非空 → FlowProxy 转发

[service_auth]
outgoing_api_key = "cmx_sk_platform_to_flow"   # 注入 X-API-Key
```

flow-server 侧对应（见 `flow-server.toml.example`）：
```
# flow-server.toml（或 env 覆盖 AUTH__*）：
[auth]
mode = "jwt"
jwt_secret = "<与平台一致>"
api_keys = "cmx_sk_platform_to_flow:default"
tenancy = "multi"
# multi 租户库 URL 模板仍走 env：FLOW_TENANT_DB_URL_TEMPLATE=postgres://…/flow_{tenant}
```

## 部署（deploy/）

- `Dockerfile` —— 多阶段，distroless 运行（无 shell、小攻击面），工具链锁 1.97.1。
- `docker-compose.yml` —— postgres + flow-server 最小拓扑，演示「独立部署」姿态。
- 三姿态：①平台内嵌（不配 flow url）②独立 + 平台反代（本文）③三方全 headless（只用 `/api/flow/v1/*` + SSE + OpenAPI，连平台都不要）。

## 拓扑

```
浏览器 ─同源 /api/flow/*─▶ 平台 web-server
                            └ FlowProxyModule ─X-API-Key + X-Delegated-User-Token─▶ flow-server:8091
                                                                                      └ 认证桥→真实用户+租户
[center_client.urls].flow 空 = 内嵌（今天）│ 非空 = 独立（S6）
```

## 不回归

- `[center_client.urls].flow` 空（默认）→ `FlowModule` 内嵌，等价今天，S0–S5 全照旧。
- 认证桥是 API Key 分支内**追加**委托令牌解析；无委托令牌时行为等价 S3。
- 反代是**新增并存**模块；FlowModule 一行未改。

## 身份/组织/表单集成（本 S6 之外的独立关切）

运行态调用链（本 S6）通了后，flow-server 查候选人/角色/组织仍走 S1 的 `AssigneeResolver` 三注入 trait：
- **mock**（demo）/ **pg**（直连库）/ **http**（回连平台 IAM 查询服务）。
- 若独立部署要回连平台身份：平台需暴露 `/api/iam/flow-identity/*`（candidates/user-org/ancestors/validate-claim 四端点镜像 trait），flow-server 配 `FLOW_IDENTITY_MODE=http` + `FLOW_IDENTITY_TARGET`（服务目录键，地址登记在 `[service_rpc.services]`）。**这组端点是平台侧新增，与本 S6 的运行态反代正交**，可按需后续补。
- **表单**：formKey 契约（F1–F3）已闭环，`/api/flow/v1/forms/{key}` 已在 S3 暴露；平台前端经反代取，第三方自研 UI 直接调。零改。
