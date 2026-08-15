# 01 · 概述与架构

## 1.1 cmx-flowengine 是什么

cmx-flowengine 是一款用 Rust 编写的 **BPMN 2.0 流程引擎微服务**。定位：

- **框架无关**：引擎内核（`cmx-flow-model` / `cmx-flow-engine`）零业务框架依赖，纯库。
- **可独立部署**：`cmx-flow-server` 是自包含二进制，落 PostgreSQL，默认端口 8091。
- **多租户**：db-per-tenant 物理隔离，JWT/API-Key 鉴权。
- **主数据外部化**：身份/组织/表单不写死在引擎里，是可注入扩展点——可用内置身份模块开箱即用，也可对接外部 IAM。
- **前端一芯三壳**：同一套前端核，三种交付形态（门户内嵌 / 可嵌 Web Component / 纯 headless）。

它对标 Flowable / Camunda 的核心执行语义（令牌、RU/HI 分离、边界事件、多实例、子流程），但用「可静态检查、无脚本沙箱、无反射」的受控方式实现。

## 1.2 crate 布局

```
crates/
  cmx-flow-model      引擎语义中立内核：BPMN IR + 运行态 DTO + RuntimeStore 契约（零 cmx 依赖）
  cmx-flow-bpmn       BPMN 2.0 XML → IR 编译器
  cmx-flow-engine     令牌执行内核（“等待态即提交点”）+ delegate 注册表 + InMemoryStore
  cmx-flow-def        流程定义持久化（草稿 / 发布 / 版本 / 装载）
  cmx-flow-store-pg   RuntimeStore 的 PostgreSQL 实现 + 平台 IAM 候选人 resolver + 子流程组织 router
  cmx-flow-adapters   外部适配器（HTTP/Mock 身份·路由·delegate + 出站 Webhook）
  cmx-flow-identity   可选内置身份模块（fid_* 表 + LocalAssigneeResolver）
  cmx-flow-app        平台中立应用核：引擎单例、handler、路由、鉴权、多租户、SSE、统计
  cmx-flow-server     独立可执行服务（基于 cmx-web-chassis）
  cmx-flow-demo       自包含 axum 演示（内联 SPA，端口 8090）
  cmx-flow-tests      引擎端到端测试（默认内存态，无外部依赖）
```

依赖方向（单向，无环）：

```
model ← bpmn ← engine ← {store-pg, adapters, identity, def} ← app ← {server, demo}
                                                                 ↑
                          cmx-flow-tests 直接测 engine（内存 store）
```

## 1.3 三层：设计态 / 运行态 / 消费态

```
┌─ 设计态 ────────────────┐   ┌─ 运行态 ─────────────┐   ┌─ 消费态 ────────────────┐
│ BPMN XML（设计器产物）    │   │ 令牌执行内核           │   │ 待办中心 / 审批表单        │
│  ↓ compile()            │──►│  实例 · 令牌 · 任务      │──►│ 转签 / 抄送 / 催办         │
│ ProcessDefinition (IR)  │   │  RU 表 ↔ HI 归档       │   │ 运维台 / 统计大盘 / SSE    │
│  ↓ deploy()             │   │  可注入：身份/路由/delegate│   │ 外部系统 REST / Webhook   │
└────────────────────────┘   └──────────────────────┘   └────────────────────────┘
```

- **设计态**：BPMN 2.0 XML 是唯一交换格式。设计器产出 XML → `compile()` 成中立 IR → `deploy()` 进引擎。引擎运行期不再接触 XML。见 [02 主流程定义](02-process-definition.md)。
- **运行态**：`start_process` 创建实例 + 起始令牌，`run_to_wait` 循环推进到下一个等待态并落库。见 §1.5、§1.6。
- **消费态**：REST API + 前端工作台 + 外部集成。见 [06](06-rest-api-reference.md)、[07](07-task-operations.md)、[08](08-external-integration.md)。

## 1.4 “等待态即提交点”（核心执行模型）

引擎以「实例 + 其全部令牌 + 任务 + 多实例域 + 定时器作业 + 候选人池 + 抄送 + 转签台账」为**聚合**，原子读写：

```
一次运行段 = load_snapshot(实例) → run_to_wait(推进到所有令牌停下) → 一次 save_snapshot()
```

每个 BPMN 等待态（用户任务、callActivity 子流程、消息捕获）恰好对应**一次数据库事务提交**。这带来：

- **崩溃恢复**：进程重启后从库装载聚合，用同一套推进代码继续，无需内存态。
- **令牌位置是稳定锚点**：令牌记 `node_bpmn_id`（BPMN 节点 id），不是内存 arena 下标——重启安全。
- `run_to_wait` 有 `STEP_LIMIT = 10_000` 步保护，疑似无等待死循环会报 `StepLimitExceeded`。

## 1.5 状态机

**实例状态 `InstanceState`**（SCREAMING_SNAKE_CASE 落库）：

| 状态 | 含义 | 终态? |
|------|------|-------|
| `ACTIVE` | 至少一个活动令牌，或在等外部触发 | 否 |
| `SUSPENDED` | 管理员挂起；令牌/任务保留，所有办理动作被拒直到恢复 | 否 |
| `COMPLETED` | 所有令牌抵达结束事件并被消费 | 是 |
| `TERMINATED` | 被外部取消/终止 | 是 |

**令牌状态 `TokenState`**：

| 状态 | 含义 | 靠谁唤醒 |
|------|------|---------|
| `ACTIVE` | 可推进，下一轮被选中执行 | 引擎自身 |
| `WAITING` | 停在用户任务，等外部办结 | `complete_task` 等 |
| `JOINING` | 停在并行网关合流，等兄弟令牌到齐 | 结构性，无需外部触发 |
| `WAITING_SUBFLOW` | 停在 callActivity，等子实例完成 | 子完成回调 |
| `WAITING_MESSAGE` | 停在消息捕获事件，等外部消息 | `correlate_message` |
| `INCIDENT` | serviceTask 失败，令牌挂起（实例不丢） | `retry_incident` |
| `ENDED` | 抵达结束事件，等实例收尾 | 引擎收尾 |

## 1.6 RU / HI 表分离

对齐 Flowable，运行态（RU）与历史态（HI）分表；实例进终态时**同事务**幂等归档到 HI。表前缀 `cmx_flow_`，无物理外键（索引替代）。

| 组 | 表 | 职责 |
|---|---|---|
| RU | `cmx_flow_instance` | 实例（聚合根：定义/业务键/状态/变量/组织/父实例链） |
| RU | `cmx_flow_token` | 令牌 |
| RU | `cmx_flow_task` | 用户任务（含 owner/parent/delegation_state 转签三列） |
| RU | `cmx_flow_mi_scope` | 多实例域（会签/或签计数账本） |
| RU | `cmx_flow_job` | 定时器作业（边界定时器到期表） |
| RU | `cmx_flow_task_candidate` | 任务候选人池 |
| RU | `cmx_flow_cc` | 抄送记录 |
| RU | `cmx_flow_task_delegation` | 转签台账 |
| RU | `cmx_flow_biz_link` | 业务单据 ↔ 实例关联 |
| RU | `cmx_flow_task_comment` | 审批意见 |
| RU | `cmx_flow_form_binding` | 表单绑定 |
| HI | `cmx_flow_hi_instance` | 历史实例（含存续时长） |
| HI | `cmx_flow_hi_task` | 历史任务（含办理时长） |
| 定义态 | `cmx_flow_subflow_binding` | 子流程组织路由绑定（见 [03](03-subprocess.md)） |
| 身份 | `fid_*`（6 张） | 内置身份模块（见 [04](04-organization-and-identity.md)） |

> 完整列定义见 [`../schema.md`](../schema.md)。DDL 由 `cmx-flow-store-pg` 的 `ensure_schema()` 自举（幂等 `CREATE TABLE IF NOT EXISTS`）。

## 1.7 四个可注入扩展点

引擎内核不知道任何具体 IAM / 组织 / 外部系统。四处 trait 在装配时注入实现：

| 扩展点 | trait | 作用 | 默认/可选实现 |
|--------|-------|------|--------------|
| serviceTask 外呼 | `JavaDelegate` | 节点调外部逻辑，读写变量 | 进程内 Rust 实现 / `HttpDelegate`（HTTP） |
| 时钟 | `Clock` | 定时器求「现在」 | `SystemClock` / `TestClock`（可注入） |
| 候选人解析 | `AssigneeResolver` | 把 `role()`/`org()`/`initiator` 解析成用户 | `LocalAssigneeResolver`(fid_*) / `PgIamAssigneeResolver`(cmx_*) / `HttpAssigneeResolver` / Mock |
| 子流程路由 | `SubflowRouter` | 按组织把逻辑 key 解析成具体子流程 | `PgSubflowRouter` / `HttpSubflowRouter` / Mock |

适配器模式由环境变量选择（`mock` | `http` | `pg`，默认 `pg`）。详见 [08 §外部集成](08-external-integration.md)。

## 1.8 前端一芯三壳

同一个前端核（`web/core/*.js`：待办中心、任务表单、设计工作台、身份工作台、运维台），三种交付：

| 壳 | 形态 | 用途 |
|----|------|------|
| ① 门户内嵌 | native-pages 跑在 CMXPortalManager | 平台内使用（cmx-container） |
| ② 可嵌 Web Component | `<flow-todo>` / `<flow-designer>` / `<flow-task-form>` | 第三方系统 `<script>` 引入即用（React/Vue/原生皆可），见 [`../../web/README.md`](../../web/README.md) |
| ③ headless | 三方全自研前端 | 只用 `/api/flow/v1/*` + SSE + OpenAPI |

## 1.9 部署形态

| 形态 | 说明 | 开关 |
|------|------|------|
| ① 平台内嵌 | 引擎作为库跑在平台 web-server 内（`FlowModule`） | 平台 `[center_client.urls].flow` 未配 |
| ② 独立 + 平台反代 | flow-server 独立跑，平台 `FlowProxyModule` 反代 `/api/flow/*` | 平台配了 `[center_client.urls].flow` |
| ③ 纯 headless | 三方系统直连 flow-server，无平台 | 直接调 `/api/flow/v1/*` |

「后端一芯双壳」：内嵌↔独立切换只看一行配置，引擎核不变。详见 [08](08-external-integration.md) 与 [`../s6-platform-integration.md`](../s6-platform-integration.md)。

## 1.10 快速开始

### 前置

- PostgreSQL，含 `fico`（运行态 + 定义库）和 `cmx`（候选人/组织库，pg 模式）两库。
- sibling 目录 `../cmx-container/` 存在（跨 workspace `path` 复用基础设施 crate `cmx-database-pg`/`cmx-core`，**无需 nora 私仓**）。
- Rust 工具链由 `rust-toolchain.toml` 固定。

### 构建与测试

```bash
cargo build                       # 编译全部 crate
cargo test -p cmx-flow-tests      # 引擎端到端测试（内存态，无需 PG）
```

### 运行独立服务（生产形态，端口 8091）

```bash
# 方式 A：flow.sh（自动读 .env，cd 到 workspace 根）
./flow.sh                         # debug/增量
./flow.sh --release               # release

# 方式 B：手动带环境变量
FLOW_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
IAM_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
  cargo run -p cmx-flow-server

# 健康探测
curl http://127.0.0.1:8091/api/flow/v1/definitions
```

启动做的事（`cmx-flow-server` 有序初始化钩子）：
1. `dotenvy` 自动加载 cwd 的 `.env`（须从 `cmx-flowengine/` 目录启动）。
2. 加载 `flow-server.toml`（`CONFIG_FILE` → `FLOW_CONFIG` → 默认 `./flow-server.toml`），toml 的 `[auth]`/`[datasource]` 段注入 `FLOW_*` 环境变量（env 优先）。
3. 钩子 `datasources`：注册 `FLOW_DB_ID`(fico-db) + `IAM_DB_ID`(primary) 数据源。
4. 钩子 `engine`：建表 + 注入 resolver/router + 装载已发布定义 + 起 5 秒定时器 poller。
5. 挂路由：`/`（大盘，免认证）、`/api/flow/v1/*` + `/api/flow/*`（认证）、`/api/flow/v1/docs`（Swagger，免认证）、`/_mon`（技术监控）。

### 运行演示（独立 playground，端口 8090）

```bash
FICO_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
IAM_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
  cargo run -p cmx-flow-demo
# 浏览器打开 http://127.0.0.1:8090
```

> ⚠ **demo 与生产服务的 API 不同**：demo（8090）用简化的 `/api/*` 路由和 `{error}` 错误体，自带内联 SPA 与 7 个种子流程，适合快速体验；**生产集成一律以 `cmx-flow-server`（8091）的 `/api/flow/v1/*` + `{code,msg,data}` 信封为准**（本文档 06 章）。demo 内置的 7 个种子流程（信用审批、报销会签、限时审批、候选人审批、子流程主/子）是本文档全篇的示例来源。

## 1.11 内置前端工作台（native-pages）

| 页面 id | 名称 | 四区职责 | 数据源 |
|---------|------|---------|--------|
| `portal.flow.design-workbench` | 流程设计工作台 | explorer 定义列表 / content bpmn-js 画布 / property 节点属性 | `/definitions*` |
| `portal.flow.todo-center` | 待办中心 | explorer 分类（待办/待认领/我发起/抄送我/我已办）/ content 卡片 / property 轨迹 | `/tasks/my`、`/todos/*` |
| `portal.flow.task-form` | 任务表单宿主 | content 单据审阅 + 审批意见 / property 轨迹 | `/tasks/{id}/complete` |
| `portal.flow.identity-workbench` | 身份管理工作台 | explorer 组织/角色/岗位/用户 / content 编辑 / property 说明 | `/identity/*`（仅 local 模式可写） |
| `portal.flow.ops-console` | 流程运维台 | explorer 实例（异常高亮）/ content 干预工具条 / property 异常明细 | `/instances/*` |

---

下一篇 → [02 · 主流程定义](02-process-definition.md)
