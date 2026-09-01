# 11 · 配置参考：环境变量与 toml 段全集

> 流程引擎（`cmx-flow-server`）全部可配置参数的**唯一参考真源**——基于源码逐条核对整理
>（`std::env::var` 直读点 + ConfigManager 装载的 toml 段）。其它文档提及参数处以本篇为准。
>
> 最后核对：2026-09-01（适配器 http 形态切 cmx-service-rpc 基座批次）。

## 11.1 30 秒速览：零配置可跑

默认形态 = **平台内嵌**（候选人/子流程路由/服务任务全走平台库直连，无 webhook，无多租户）：

```bash
./flow.sh        # 无任何 FLOW_* 环境变量即可启动，端口 8091
```

唯一硬前提：平台 toml `[[databases]]` 已注册两个库（见 §11.4.1）。其余全部可选。

## 11.2 配置加载链（两类入口，别混）

| 类别 | 读取方式 | 优先级 | 生效时机 |
|------|---------|--------|---------|
| **环境变量**（本篇 §11.3 的 `FLOW_*` / `SERVER__*`） | 代码 `std::env::var` 直读 | 进程环境唯一来源 | 多为启动装配期一次读定 |
| **toml 段**（§11.4 的 `[auth]` / `[service_rpc]` / `[server]` / `[[databases]]`） | ConfigManager 三源合并：`flow-server.toml` ← Nacos 远程 ← env 覆盖 | env > Nacos > toml（`段名__键` 命名，如 `AUTH__MODE`） | 首次使用时快照读定，改配置需重启 |

> 注意：`FLOW_*` 前缀的**适配器类**变量只走环境变量、不走 toml（历史约定）；`[auth]`/`[service_rpc]` 只走 toml 段（env 用 `AUTH__*`/`SERVICE_RPC__*` 前缀覆盖）。上表两类入口互不通用。

## 11.3 环境变量全集

### 11.3.1 外部适配器（默认全 `pg` = 零配置；`http` 形态经 cmx-service-rpc 基座）

`http` 形态的目标一律是**服务目录键**（登记在 toml `[service_rpc.services]`，见 §11.4.2；无注册中心时目录里配静态 url 直连），不是完整地址。协议与错误语义详见 [08 外部集成](08-external-integration.md)。

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `FLOW_IDENTITY_MODE` | 可选 | `pg` | 候选人解析：`pg`（直连平台 IAM 库）/ `http`（外部身份服务）/ `mock`（脱外部）/ `local`（内建身份模块 `fid_*` 表，见 [04](04-organization-and-identity.md)） |
| `FLOW_IDENTITY_TARGET` | **`MODE=http` 时必须** | — | 外部身份服务的服务目录键；缺失或基座未初始化 → 回退 mock 并告警 |
| `FLOW_SUBFLOW_MODE` | 可选 | `pg` | 子流程路由：`pg`（沿维度字典物化路径）/ `http`（外部组织服务）/ `mock` |
| `FLOW_SUBFLOW_TARGET` | **`MODE=http` 时必须** | — | 外部组织服务的服务目录键 |
| `FLOW_DELEGATE_MODE` | 可选 | `pg` | serviceTask 外包：`pg`（不注册额外 delegate，仅内置 `riskDelegate`）/ `http`（注册 `httpDelegate`，路径固定 `/delegate/run`）/ `mock`（`mockDelegate`） |
| `FLOW_DELEGATE_TARGET` | **`MODE=http` 时必须** | — | 外部逻辑服务的服务目录键 |
| `FLOW_DIMENSION_MODE` | 可选 | 非 `http`（库内继承） | RD5 维度层级解析：`http` = 继承步经外部服务读层级（独立部署不共享字典库时用）；默认直连字典表 |
| `FLOW_DIMENSION_TARGET` | **`MODE=http` 时必须** | — | 维度层级服务的服务目录键 |

### 11.3.2 路由与发布闸

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `FLOW_ROUTING_DIMENSIONS` | 可选 | 空（仅内建 `org` 维度） | 自分级路由维度注册（RD2），JSON 数组，每项形如 `{"dimKey":"legal_entity","name":"法人公司","table":"cf_legal_entity","idCol":"id","pathCol":"full_path","delim":".","nameCol":"name","parentCol":"parent_id"}`（`parentCol` 有值 = 自分级表）。解析失败忽略并告警 |
| `FLOW_PUBLISH_STRICT_REQUIRED` | 可选 | 开（含无配置回落从紧） | 发布闸（D2）：声明了必填变量的定义发布时必须配 strict 校验策略。`off` = 显式关闭 |

### 11.3.3 出站 Webhook（生命周期事件 → mdm 等）

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `FLOW_WEBHOOK_TARGETS` | 可选 | 空 = 关闭 | 逗号分隔的目标**服务目录键**（如 `mdm`）；经 cmx-mdm-sdk 契约投递到 `/api/mdm/flow/callback` |
| `FLOW_WEBHOOK_SIGNING_KEY` | **启用 webhook 时必须** | 空 | HMAC-SHA256 签名密钥，**须与接收端 mdm 的 `[mdm.flow].webhook_secret` 一致**；空 = 仍签名但接收端按空密钥拒收 |
| `FLOW_WEBHOOK_MAX_RETRIES` | 可选 | `3` | 单条投递失败重试次数 |

### 11.3.4 多租户（按需启用）

租户库连接串模板/显式 URL，`{tenant}` 占位符（详见 [08 §多租户](08-external-integration.md)）：

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `FLOW_TENANT_DB_URL_TEMPLATE` | 多租户时可选 | — | 租户流程库模板，如 `postgres://…/flow_{tenant}` |
| `FLOW_TENANT_IAM_URL_TEMPLATE` | 多租户时可选 | — | 租户 IAM 库模板；缺省 = 复用默认 IAM 库 |
| `FLOW_TENANT_<T>_PG_URL` | 多租户时可选 | — | 租户 `T`（大写）流程库显式 URL，优先于模板 |
| `FLOW_TENANT_<T>_IAM_URL` | 多租户时可选 | — | 租户 `T` 的 IAM 库显式 URL |

### 11.3.5 服务与端口

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `SERVER__PORT` | 可选 | `8091` | HTTP 端口（chassis `[server]` 段的 env 覆盖形态；未显式配且 toml 为 8080 时回落 8091） |
| `CONFIG_FILE` | 可选 | cwd 的 `flow-server.toml` | 生效的 toml 配置文件路径（平台统一约定；本仓 `.env` dev 环境实际指向 `./flow-server-dev.toml`） |

### 11.3.6 测试与调试（生产勿动）

| 环境变量 | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `FLOW_ENABLE_E2E_DELEGATES` | 可选 | 关 | 注册 E2E 测试 delegate（`e2eOkDelegate`/`e2eBpmnErr`/`e2eAlwaysFail`，验异步/BPMN 错误/死信路径）。**生产环境勿开** |

> `cmx-flow-demo`（演示 bin）另读 `DEMO_PORT` / `DEMO_IAM_PG_URL` / `FICO_PG_URL`，属演示专用，不在产品配置面内。

## 11.4 平台 toml 段（ConfigManager 装载）

### 11.4.1 `[[databases]]` — 数据源注册（**唯一硬前提**）

流程引擎需要的两个库标识是**编译期常量**（非环境变量）：

| 常量 | 值 | 用途 |
|------|-----|------|
| `FLOW_DB_ID` | `fico-db` | 运行态 + 定义 store（`cmx_flow_*` 表） |
| `IAM_DB_ID` | `primary` | 候选人 resolver + 子流程 router（`cmx_user`/`cmx_role`/`cmx_org`） |

两库须在生效 toml 的 `[[databases]]` 段注册（连接串、`source_type` 等字段见平台 `CONFIG_MANUAL.md`）；多租户另见 §11.3.4。

### 11.4.2 `[service_rpc]` — 服务间调用目录（http 适配器与 webhook 的地址真源）

本篇 §11.3.1 / §11.3.3 所有 `_TARGET`/`TARGETS` 键在此登记地址：

```toml
[service_rpc.services]
# 无注册中心：静态 url 直连（回滚形态）
identity_svc = { url = "http://192.168.1.5:9000" }
# 有注册中心：服务发现选例
mdm          = { discovery = "cmx-mdm-server" }
# 键级覆盖：timeout_ms / retry_max / transport
```

env 覆盖形态：`SERVICE_RPC__SERVICES__<KEY>__URL`（值带 scheme）。段缺失 = 空目录（零出站形态，合法）；配了 `discovery` 但注册中心未启用且无 `url` → 启动 fail-fast。

### 11.4.3 `[auth]` — 鉴权（引擎族 B：JWT / API-Key）

> ⚠️ 历史文档曾写 `FLOW_AUTH_MODE` / `FLOW_JWT_*` 环境变量——**已废弃**：鉴权收编至
> `cmx-engine-kit` 后统一走 ConfigManager 读 `[auth]` 段（env 覆盖前缀 `AUTH__`）。

| toml 键（`auth.*`） | 必要性 | 默认 | 说明 |
|----------|--------|------|------|
| `mode` | 可选 | `off` | `off`：不验签，租户取 `X-Tenant` 头、用户取 `X-User` 头（内网/开发）；`jwt`：强制验签，缺/坏/过期 `Authorization: Bearer` → 401 |
| `jwt_alg` | `mode=jwt` 时必须 | `HS256` | `HS256` 或 `RS256` |
| `jwt_secret` | HS256 时必须 | — | 对称密钥 |
| `jwt_public_key` | RS256 时必须 | — | PEM 公钥 |
| `jwt_tenant_claim` | 可选 | `tenant` | 租户 claim 名 |
| `jwt_roles_claim` | 可选 | `roles` | 角色 claim 名 |
| `api_keys` | 可选 | 空 | 服务间 API-Key，`"k1:tenantA,k2:tenantB"` 映射（M2M 调用免 JWT，键与租户参与 scope） |

claim 宽容解析：`sub`→用户；租户 claim 缺省 `default`；角色接受 JSON 数组或逗号串；`exp` 默认校验。SSE 票据白名单（`/design/collab`、`/events`）为引擎内置，无需配置。

### 11.4.4 `[server]` — HTTP 服务底盘（chassis）

端口/主机等框架级配置，`SERVER__*` env 覆盖（见 §11.3.5）；细节见 `cmx-web-chassis` 文档。

## 11.5 跨服务联动约定

| 约定 | 两端 |
|------|------|
| webhook 签名密钥一致 | 本侧 `FLOW_WEBHOOK_SIGNING_KEY` ↔ mdm 侧 `[mdm.flow].webhook_secret` |
| 服务间调用鉴权 | 出站自动注入 `X-API-Key`（`[service_auth].outgoing_api_key`）+ `X-Delegated-User-Token`（有用户上下文时透传原始 JWT）+ `X-Request-Id` |
| 门户反代 | 门户 `[service_rpc.services].flow` 登记本服务地址（url 或 discovery） |

## 11.6 变更速记

- **2026-09-01**：适配器 http 形态切 cmx-service-rpc 基座——`FLOW_{IDENTITY,SUBFLOW,DELEGATE,DIMENSION}_URL` 改名 `_TARGET`（值从完整地址改为服务目录键，地址登记迁 `[service_rpc.services]`）；delegate 外呼路径固定 `/delegate/run`。
- 鉴权 env（`FLOW_AUTH_MODE`/`FLOW_JWT_*`）废弃 → `[auth]` 段 + `AUTH__*` 覆盖（收编 `cmx-engine-kit` 时统一）。
