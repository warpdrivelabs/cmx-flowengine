# 08 · 外部系统集成

本篇讲**流程引擎与外部系统双向对接**：流程调外部（serviceTask 外呼、子流程/身份外呼）、外部调流程（REST 发起/办理、消息唤醒）、事件订阅（Webhook / SSE），以及鉴权、多租户、平台反代、配置。

## 8.1 集成全景

```
                      ┌────────────── cmx-flow-server ──────────────┐
外部系统 ──REST────────► /api/flow/v1/*  (发起/办理/查询)              │
外部系统 ──消息────────► POST /messages/correlate (唤醒等待中的流程)    │
        ◄──Webhook──── FLOW_WEBHOOK_TARGETS (生命周期事件推送, 服务目录键)   │
        ◄──SSE──────── GET /api/flow/v1/events (实时事件流)           │
                      │                                              │
        ◄──外呼──────── serviceTask → HttpDelegate (调外部算逻辑)      │
        ◄──外呼──────── 身份解析 → HttpAssigneeResolver               │
        ◄──外呼──────── 子流程路由 → HttpSubflowRouter                │
                      └──────────────────────────────────────────────┘
```

## 8.2 鉴权（外部系统怎么带凭证）

> ⚠️ 本节旧版写的 `FLOW_AUTH_MODE` / `FLOW_JWT_*` 环境变量**已废弃**——鉴权收编至
> `cmx-engine-kit` 后统一经 ConfigManager 读 toml `[auth]` 段（env 覆盖前缀 `AUTH__`）。
> 完整键表（mode / jwt_alg / jwt_secret / jwt_public_key / jwt_tenant_claim / jwt_roles_claim /
> api_keys）见 [11 配置参考 §11.4.3](11-configuration-reference.md)。

### 模式 `auth.mode`

| 模式 | 说明 |
|------|------|
| `off`（默认） | 无验签；租户取 `X-Tenant` 头，用户取 `X-User` 头。适合内网/单租户/开发 |
| `jwt` | 强制验签；缺/坏/过期 `Authorization: Bearer` → 401 |

claim 宽容解析：`sub`→用户、租户 claim→租户（缺省 `default`）、角色 claim→角色数组（接受 JSON 数组或逗号串）。`exp` 默认校验（过期即失败）。SSE 票据白名单（`/design/collab`、`/events`）为引擎内置，无需配置。

### 请求头一览

| 头 | 用途 |
|----|------|
| `Authorization: Bearer <jwt>` | 用户 JWT（jwt 模式）；`bearer` 小写也接受 |
| `X-Api-Key: <key>` | 服务间 M2M；`FLOW_API_KEYS="k1:tenantA,k2:tenantB"` 映射到租户，命中即免 JWT |
| `X-Tenant: <t>` | off 模式的租户选择 |
| `X-User: <u>` | off 模式的用户 id |
| `X-Delegated-User-Token: Bearer <jwt>` | S6 委托端用户令牌（始终验签，把待办归到真实用户；裸 token 也容忍） |

### 中间件顺序

1. 有 `X-Api-Key` → 查 key 映射：命中则建服务上下文（若同时带**验签通过的** `X-Delegated-User-Token`，用其 `{user,tenant,roles}` + 追加角色 `service`；否则用 key 绑定的租户 + 角色 `service`）；未命中 → `401 {"code":401,"msg":"无效 API Key"}`。
2. 无 key → 按模式：`off` 取头，`jwt` 验签（失败 401）。
3. 请求在租户 scope 内执行。

> 委托令牌**无论 `FLOW_AUTH_MODE` 都验签**（它是唯一的端用户凭证）；验签失败或缺失 → **降级为纯服务调用（不 401）**，因为服务身份已由 API Key 证明。**租户优先取委托 claim**（一个服务 key 可服务多租户）。

### 401 体

```json
{ "code": 401, "msg": "缺少 Authorization: Bearer <token>" }   // HTTP 401
```

## 8.3 外部调流程（REST）

外部系统作为客户端发起/办理/查询，就是调 [06](06-rest-api-reference.md) 的端点。典型集成：

```bash
# 业务系统提交单据时发起流程，并绑定业务单据
curl -X POST http://flow:8091/api/flow/v1/instances \
  -H 'Authorization: Bearer <serviceJWT>' \
  -H 'Content-Type: application/json' \
  -d '{
    "definitionKey":"credit_approval",
    "businessKey":"CR-2026-001",
    "orgId":"df_root",
    "variables":{"applicant":"张三","amount":80000,"initiator":"u_zhang"},
    "bizLink":{"bizTable":"t_loan","bizId":"L-001","role":"main"}
  }'
```

### 业务单据 ↔ 实例关联

`bizLink` 把业务单据和流程实例双向挂钩，之后可互查：

```bash
# 从实例查关联单据
curl http://flow:8091/api/flow/v1/instances/<iid>/biz
# → {links:[{bizTable,bizId,bizKey,role}]}

# 从业务单据查它触发的流程实例
curl http://flow:8091/api/flow/v1/biz/t_loan/L-001/instances
# → {instances:[{instanceId,definitionKey,state,businessKey,role}]}
```

关联落 `cmx_flow_biz_link` 表（幂等 insert，`ON CONFLICT DO NOTHING`）。

## 8.4 流程调外部：serviceTask 外呼

serviceTask 用 delegate 调外部逻辑。两种实现：进程内 Rust `JavaDelegate`，或 `HttpDelegate` 外呼 HTTP 服务。

### BPMN 侧

```xml
<bpmn:serviceTask id="calc_risk" name="计算风险等级"
                  flowable:delegateExpression="${riskDelegate}"/>
```

delegate key（`riskDelegate`）指向注册的实现。

### HttpDelegate 外呼契约

设 `FLOW_DELEGATE_MODE=http` + `FLOW_DELEGATE_TARGET`（服务目录键，地址登记在 `[service_rpc.services]`），`serviceTask` 会外呼（delegate 注册名 `httpDelegate`；因 BPMN serviceTask 只带 key 无地址槽，目标键在 delegate 实例上，一 key 一目标；路径固定 `/delegate/run`）：

**请求** `POST {目标服务}/delegate/run?node=<nodeBpmnId>&instance=<instanceId>`：

```json
{ "amount": 80000, "applicant": "张三", "...": "当前实例全部变量" }
```

**响应**（两种形状都接受）：

```json
{ "variables": { "riskLevel": "高" } }
```

或裸变量对象：

```json
{ "riskLevel": "高" }
```

- 对象 → `merge` 回实例变量（同名覆盖）。非对象 → 忽略（no-op，不算错）。
- 错误：请求失败 → `外部 delegate 请求失败`；非 2xx → `外部 delegate 返回 <status>: <body>`；解析失败 → 解析错误。这些错误使令牌转 **Incident**（见 §8.9）。

> 进程内始终注册 `riskDelegate`（按 amount 算 riskLevel），是最简单的 Rust delegate 示例。

### 外部 delegate 服务模板

实现一个 `POST` 端点，读实例变量、干活、回写变量：

```python
# 伪代码：外部风控服务
@app.post("/delegate/risk")
def risk(vars: dict, node: str, instance: str):
    amount = vars.get("amount", 0)
    level = "高" if amount > 50000 else "中" if amount > 10000 else "低"
    return {"variables": {"riskLevel": level}}
```

## 8.5 外部调流程：消息相关（唤醒等待中的流程）

流程走到 `intermediateCatchEvent`（消息捕获）时令牌停 `WAITING_MESSAGE`，等外部系统回调唤醒。

### BPMN 侧

```xml
<bpmn:intermediateCatchEvent id="msgWait" name="等外部裁决" cmx:correlationVar="orderId">
  <bpmn:messageEventDefinition messageRef="verdictReceived"/>
</bpmn:intermediateCatchEvent>
```

- `messageRef` = 消息名（外部回调时要对上）。
- `cmx:correlationVar` = 相关键取自哪个实例变量（如 `orderId`），用于跨实例定位。

### 外部回调

```bash
# 方式 A：知道实例 id（点对点）
curl -X POST http://flow:8091/api/flow/v1/messages/correlate \
  -H 'Content-Type: application/json' \
  -d '{"messageName":"verdictReceived","instanceId":"<iid>","correlationKey":"ORD-1",
       "variables":{"verdict":"approved"}}'

# 方式 B：不知实例 id，靠相关键跨实例定位
curl -X POST http://flow:8091/api/flow/v1/messages/correlate \
  -H 'Content-Type: application/json' \
  -d '{"messageName":"verdictReceived","correlationKey":"ORD-1","variables":{"verdict":"approved"}}'
```

请求（camelCase）：`{messageName, instanceId?, correlationKey?, variables}`。

语义（对齐 A4 测试）：

- 匹配 = 令牌 `WAITING_MESSAGE` 停在消息名匹配的捕获事件上，且（若节点声明了 `correlationVar`）该实例变量 == `correlationKey`。
- 命中 → 合入 `variables` → 令牌沿唯一出边推进 → `run_to_wait`。
- 方式 B 跨实例扫描 `Active` 实例，找第一个匹配的。
- 无匹配（相关键不对 / 消息名不对）→ 报错 `TaskNotActionable`。

> 用途：等第三方系统异步裁决（风控、外部审批、支付回调）。发起时把相关键（如订单号）放进变量，外部系统回调时带同一相关键即可精确唤醒对应实例。

## 8.6 事件订阅：Webhook（出站推送）

引擎在生命周期节点触发出站 Webhook（应用层发，引擎核不变）。

### 配置

| 环境变量 | 说明 |
|----------|------|
| `FLOW_WEBHOOK_TARGETS` | 逗号分隔的目标**服务目录键**（`[service_rpc.services]` 键，如 "mdm"）；空 = 禁用 |
| `FLOW_WEBHOOK_SIGNING_KEY` | HMAC-SHA256 签名密钥（可选；须与接收端 `[mdm.flow].webhook_secret` 一致） |
| `FLOW_WEBHOOK_MAX_RETRIES` | 重试次数（默认 3） |

### 事件类型

`instance.started`、`instance.completed`、`instance.terminated`、`task.created`、`task.completed`、`task.reassigned`。

### 请求

`POST` 到每个 URL：

```
content-type: application/json
x-cmx-flow-event: instance.started
x-cmx-flow-delivery: <instanceId>-<occurredAt>
x-cmx-flow-signature: sha256=<hex(HMAC-SHA256(body, key))>   # 配了签名密钥才有
```

**载荷**（camelCase，None 字段省略）：

```json
{
  "event": "instance.started",
  "instanceId": "…",
  "state": "ACTIVE",
  "definitionKey": "…",
  "businessKey": "…",
  "taskId": "…",         // task.* 事件才有
  "nodeBpmnId": "…",     // task.* 事件才有
  "assignee": "…",       // task.created / task.reassigned 才有
  "tenant": "default",
  "occurredAt": "2026-…T…Z"
}
```

投递：mpsc 队列（容量 1024）+ 后台任务，指数退避（1s,2s,4s… 上限 1<<6）重试至 `FLOW_WEBHOOK_MAX_RETRIES`。队列满/无订阅者 → 静默丢弃（非关键路径，绝不阻塞业务）。

### 验签（接收端）

```python
import hmac, hashlib
def verify(body_bytes, sig_header, key):
    expected = "sha256=" + hmac.new(key.encode(), body_bytes, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, sig_header)
```

## 8.7 事件订阅：SSE（实时流）

`GET /api/flow/v1/events`（**仅 v1**）返回 `text/event-stream`：

```bash
curl -N -H 'Authorization: Bearer <jwt>' \
  'http://flow:8091/api/flow/v1/events?user=u_zhang'
```

- **帧格式**：`event: <事件名>`（如 `instance.started`）+ `data: <FlowEvent JSON>`（载荷同 Webhook §8.6）。
- **租户隔离**：按鉴权 scope 的 `current_tenant()` 过滤，只推同租户事件（无 tenant 的当作 `default`）。**租户从不取自 query**（防跨租户泄露）。
- **可选过滤** `?user=<id>`：把 `task.*` 事件收窄到 `assignee` 匹配的；实例事件（无 assignee）照推。
- **保活**：每 15 秒发 `keep-alive` 文本。慢订阅者 Lagged 丢最旧。
- 进程内 `broadcast` 总线（容量 512）；无订阅者静默丢弃。

> **双发**：每个生命周期事件同时发 Webhook（若启用）和 SSE（始终）。所以 Webhook 关着 SSE 也能用。

前端浏览器：

```js
const es = new EventSource('/api/flow/v1/events?user=u_zhang', { withCredentials: true });
es.addEventListener('task.created', e => refreshTodo(JSON.parse(e.data)));
es.addEventListener('instance.completed', e => toast('流程完成', JSON.parse(e.data)));
```

## 8.8 多租户

### 模式 `FLOW_TENANCY`

| 模式 | 说明 |
|------|------|
| `single`（默认） | 全部请求 → `default` 租户，用宿主预注册的 `fico-db`/`primary`。等价于单库（零回归） |
| `multi` | db-per-tenant 物理隔离 |

### 租户 → 数据库映射

| | 规则 |
|--|------|
| 流程库 `flow_db_id(tenant)` | 非 multi 或 default → `FLOW_DB_ID`（`fico-db`）；否则 → `flow_<sanitize(tenant)>` |
| IAM 库 `iam_db_id(tenant)` | default/single/无 IAM 模板 → `IAM_DB_ID`（`primary`）；否则 → `iam_<tenant>` |

- `sanitize`：非 `[A-Za-z0-9_]` → `_`（防注入/坏名）。**不转小写**（`tenantB` → `flow_tenantB`，库名大小写敏感）。
- 连接 URL：显式 `FLOW_TENANT_<T>_PG_URL`（T 大写）优先于模板 `FLOW_TENANT_DB_URL_TEMPLATE="postgres://…/flow_{tenant}"`；IAM 同理 `FLOW_TENANT_<T>_IAM_URL` / `FLOW_TENANT_IAM_URL_TEMPLATE`。
- 非 default 租户在首次访问时**懒注册**数据源，per-tenant `OnceCell` 缓存运行时（build 单飞，map 锁只护取 cell）。

### 租户来源

- `jwt` 模式：JWT 的租户 claim（`FLOW_JWT_TENANT_CLAIM`，默认 `tenant`）。
- `off` 模式：`X-Tenant` 头。
- API Key：`FLOW_API_KEYS` 里 key 绑定的租户（有委托令牌则优先取委托 claim）。

请求在 `task_local` 的租户 scope 内跑，`current_tenant()` 无 scope 时回退 `default`。⚠ `task_local` 不跨 `tokio::spawn`——定时器 poller 遍历运行时缓存，Webhook emit 只在 handler 内（有 scope）。

## 8.9 serviceTask 失败与 Incident

外部 delegate 失败**不终止流程**：令牌转 `INCIDENT`，实例仍 `ACTIVE`，可人工重试。这是生产可用性硬门槛。详见 [09 §Incident](09-operations-and-admin.md)。要点：

- delegate 返回 `Err` → 令牌停 `INCIDENT`、原因+重试次数记进变量 `__incident`（形状 `{"<节点>": {"reason":"…","retries":N}}`）。
- 其它分支照常跑，整个运行不中止，实例不丢。
- `POST /instances/{id}/retry-incident`（可带修正变量）重跑：仍失败则重试次数累加，修好则令牌越过继续。

## 8.10 适配器模式选择

三个外部适配器各有 `mock|http|pg` 模式（默认全 `pg` = 内嵌/回连平台库，零回归）；`http` 形态统一走 cmx-service-rpc 基座——目标为服务目录键（`[service_rpc.services]` 登记，无注册中心时配静态 url 直连；鉴权/超时/重试/熔断由基座承载），协议保持自定义裸 JSON。**全量参数（必要性/默认值/env 与 toml 全集）见 [11 配置参考](11-configuration-reference.md)**，下表为速查：

| 适配器 | 模式变量 | 目标变量（服务目录键） | 作用 |
|--------|---------|---------|------|
| 身份解析 | `FLOW_IDENTITY_MODE`（+`local`） | `FLOW_IDENTITY_TARGET` | 候选人 → 用户（见 [04](04-organization-and-identity.md)） |
| 子流程路由 | `FLOW_SUBFLOW_MODE` | `FLOW_SUBFLOW_TARGET` | 逻辑 key → 子流程（见 [03](03-subprocess.md)） |
| serviceTask | `FLOW_DELEGATE_MODE` | `FLOW_DELEGATE_TARGET` | 服务任务外呼（§8.4） |
| 维度层级（RD5，仅 pg 路由下） | `FLOW_DIMENSION_MODE` | `FLOW_DIMENSION_TARGET` | 继承链祖先解析（§3） |

- `mock`：独立跑无外部依赖（测试）。
- `http`：调外部服务（服务目录键 → 基座传输）。
- `pg`：回连平台库（实现在 `cmx-flow-store-pg`）。

**HttpSubflowRouter 契约**：`POST {目标服务}/subflow/resolve`，body `{calledKey, orgId?}` → `{targetKey}`；404 或 targetKey 空 → `NoBinding`；其它非成功 → `Backend`。

**HttpAssigneeResolver 契约**：见 [04 §4.6](04-organization-and-identity.md)。

## 8.11 表单绑定（对接前端单据）

userTask 的 `formKey` 绑定到具体前端页面/字段，`/forms` 端点管理映射：

```bash
curl -X POST http://flow:8091/api/flow/v1/forms \
  -H 'Content-Type: application/json' \
  -d '{"formKey":"pay.review","kind":"native",
       "nativePage":"portal.fi.pay-review","nativeView":"content",
       "bizTable":"t_pay","pkField":"id","title":"付款复核"}'
```

`kind` = `native`（门户 native-page）或 `html`（HTML 页）。办理时前端按 formKey 查绑定，渲染对应单据 + 审批区。字段见 [06 §6.11](06-rest-api-reference.md)。

## 8.12 平台反代对接（S6）

平台（cmx-container）以「一芯双壳」接入独立 flow-server：内嵌 ↔ 独立切换只看一行配置。

- **配置驱动**：平台 `/api/flow/*` 由 `[center_client.urls].flow` 决定——空 = 内嵌引擎（`FlowModule`，零回归）；非空 = 反代到独立 flow-server（`FlowProxyModule`）。
- **反代壳**：`FlowProxyModule` 同 `/flow/*` 前缀，转发 `/api/flow/{rest}` → `{flow_base}/api/flow/v1/{rest}`（升 v1）。双向流式（含 SSE 透传），注入三层出站头，剥逐跳头，不可达 → `502`。
- **三层出站头**（平台 → flow）：`X-API-Key`（平台服务身份，取自 `[service_auth].outgoing_api_key`）、`X-Delegated-User-Token: Bearer <用户JWT>`、`X-Request-Id`。
- **认证桥**：flow-server 的 `auth.rs` 在 API-Key 分支额外解 `X-Delegated-User-Token` 还原真实用户（租户优先取委托 claim）；委托令牌始终验签（`FLOW_JWT_SECRET` 须与平台签发方一致）；验签失败退化纯服务调用（不 401）。
- 代理模式下平台跳过本地定时器 poller（`flow_is_proxied()`）。

三种部署形态：① 平台内嵌（不配 flow url）；② 独立 + 平台反代（配 flow url）；③ 纯 headless（三方只用 `/api/flow/v1/*` + SSE + OpenAPI）。详见 [`../s6-platform-integration.md`](../s6-platform-integration.md)。

## 8.13 配置文件

加载顺序：`.env`（dotenvy 从 cwd）→ `flow-server.toml`（`CONFIG_FILE` → 默认 `./flow-server.toml`）→ 环境变量覆盖（env 优先级最高；框架键 `SERVER__*`、业务键 `AUTH__*` 族，均与 ConfigManager `__` 约定同名）。

`flow-server.toml` 示例：

```toml
# —— chassis 框架级（[server] 段；env 覆盖 SERVER__HOST/SERVER__PORT/...，与 ConfigManager `__` 约定同名）——
[server]
host = "0.0.0.0"
port = 8091
log_dir = "logs"
log_level = "info"
graceful_timeout_secs = 10

# —— 数据源（标准 [[databases]] 段；BaseConfig 直读；缺段启动失败）——
[[databases]]
db_id = "fico-db"
db_type = "postgres"
db_url = "postgres://postgres:postgres@127.0.0.1:5432/fico"   # 运行态 + 定义库
default = true

[[databases]]
db_id = "primary"
db_type = "postgres"
db_url = "postgres://postgres:postgres@127.0.0.1:5432/cmx"    # 候选人/组织库
default = false

# —— 认证（[auth] 扁平段；ConfigManager 直读，env 覆盖 AUTH__*）——
[auth]
mode = "jwt"                    # off（默认）| jwt。生产务必 jwt
jwt_alg = "HS256"               # HS256 | RS256
jwt_secret = "<HS256 密钥>"      # ★须 = 平台签发 JWT 的密钥（否则委托令牌验签 401）
jwt_tenant_claim = "tenant"
jwt_roles_claim = "roles"
api_keys = "svckey:default"     # key:租户，逗号分隔；★须 = 平台 [service_auth].outgoing_api_key
# tenancy = "single"            # single | multi
```

flow 专属仍用环境变量：`FLOW_IDENTITY_MODE`/`FLOW_SUBFLOW_MODE`/`FLOW_DELEGATE_MODE`（+对应 URL）、`FLOW_WEBHOOK_*`、多租户 `FLOW_TENANT_*`。

### ⚠ 安全须知

- `.env` 已 `.gitignore`（含密钥，切勿提交）。
- **`flow-server.toml` 被 git 跟踪**，仓库内的示例带了开发用 HS256 密钥与 API Key——**生产环境务必用环境变量或未跟踪的配置文件覆盖**，不要沿用仓库里的开发密钥。
- 平台反代对接时，两个值必须对齐（否则平台请求被判 401 → 门户跳登录）：`[auth].api_keys` 的 key = 平台 `[service_auth].outgoing_api_key`；`[auth].jwt_secret` = 平台签发 JWT 的密钥。

## 8.14 部署

`deploy/` 提供：多阶段 `Dockerfile`（distroless，工具链固定）、`docker-compose.yml`（postgres + flow-server 最小拓扑）。启动/构建命令见 [01 §1.10](01-overview-and-architecture.md)。

---

上一篇 ← [07 任务操作](07-task-operations.md) ｜ 下一篇 → [09 运维与管理](09-operations-and-admin.md)
