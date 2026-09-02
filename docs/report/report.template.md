# cmx-flowengine 流程引擎 · 实现方案与开源全景对比

> Rust 原生 BPMN 2.0 流程引擎 · 令牌持久化执行 · 一芯多壳 · 与 Camunda / Flowable / Zeebe / Temporal 全维度对比
> 版本 v0.1.12 · edition 2024 · 工具链 1.97.1 · Apache-2.0 · 报告日期 2026-09-01（能力口径为当前实测源码，非陈旧 gap 文档）

{{FIG:overview}}

---

## 摘要

**cmx-flowengine** 是一套用 **Rust** 从零构建的 **BPMN 2.0 流程引擎**，采用「一芯多壳」架构：语义中立的执行内核零框架/零平台依赖，外围可装配成**独立微服务**、**门户内嵌反代壳**、**可嵌 Web Component**、**Headless REST/SSE 契约**四种部署姿态。引擎为**令牌（Token）持久化执行**模型——每一个 BPMN 等待态即一次 PostgreSQL 事务提交点，配合 `SELECT … FOR UPDATE SKIP LOCKED` 的异步作业执行器实现集群安全的水平取件。

- **规模**：12 个 crate、约 **25,012** 行域代码 + **9,351** 行测试代码、71 个源文件；**279** 个 Rust 测试函数（0 失败），后端/前端 E2E 全绿。
- **BPMN 覆盖**：**19** 个 `NodeKind` 变体、约 **28** 项能力「已建模 + 执行 + 集成测试」，覆盖网关（排他/并行/包容/事件）、边界与中间事件（定时/消息/错误）、多实例（会签/或签/完成条件/动态办理人）、调用活动子流程、业务规则任务等。
- **企业能力**：异步作业执行器（SKIP LOCKED）· 死信队列 · 外部 Worker + SDK · 注入式时钟定时器 · 实例迁移（dry-run 校验）· 活动历史/SLA · Incident 重试 · **db-per-tenant** 多租户 · JWT/API-Key 认证 · Headless v1 + SSE + HMAC Webhook + OpenAPI/Swagger。
- **设计态**：bpmn-js 四区设计器 + 模拟试跑 + 版本 diff + **实时协同建模（M1 感知 / M2 对象级 / M3 结构级）** + 决策表设计器 + Camunda-Operate 级运维台（令牌活标记 + **历史回放**）。
- **定位**：一套**干净、审批扎实、生产可靠**的轻量级 BPMN 引擎，深耕人工审批赛道；以真 **Apache-2.0** 开源，无生产许可门槛，无社区版 EOL 风险。

---

## 一、引言与定位

企业流程/编排引擎大致分三条赛道，cmx-flowengine 的定位在此坐标中清晰：

| 赛道 | 代表 | 范式 | cmx 的关系 |
| --- | --- | --- | --- |
| **A. 人工审批（Human-centric）** | Flowable · Camunda 7 · Activiti · 钉钉/飞书审批 | BPMN 可视化 + 人工任务 | **同赛道正面竞争**，深耕并已达上乘 |
| **B. 云原生编排（Cloud-native）** | Camunda 8/Zeebe · Netflix Conductor · AWS Step Functions | 事件溯源 + 水平扩展 | 择机借鉴其可靠性/伸缩，不照搬 |
| **C. 代码优先持久执行（Durable-code）** | Temporal · Cadence · Restate | 工作流即代码 + 确定性重放 | 不同物种，不追 |

**战略取舍**：把 A 赛道「做透 + 做到生产可靠」，选择性吸收 B 赛道的可靠性/伸缩手段（SKIP LOCKED 执行器、死信队列、Incident），不去追 C 赛道的代码优先范式。这一取舍决定了 cmx 的能力边界与差异化——它不是一个「什么都做但都浅」的通用引擎，而是一个「审批场景端到端可用、且工程质量过硬」的引擎。

与几乎所有主流 BPMN 引擎（Camunda / Flowable / Activiti / jBPM 均为 **Java/JVM**）不同，cmx 选择 **Rust**：无 GC、内存安全、单静态二进制、低内存占用、快速冷启动——这在 BPMN 引擎生态中是罕见的技术路线。

---

## 二、总体架构（一芯多壳）

{{FIG:arch}}

引擎遵循严格的分层依赖，核心内核（`cmx-flow-model` + `cmx-flow-engine`）**零框架、零平台依赖**，可 wasm/嵌入；平台耦合与基础设施均通过 trait 注入与单向 `path` 借用隔离在外围。

| Crate | 层 | 职责 | 源 LOC |
| --- | --- | --- | ---: |
| `cmx-flow-model` | L0 内核 | 语义中立 IR（`ProcessDefinition`/`FlowNode`/`Token`/19 `NodeKind`）+ 运行态 DTO + **全部 trait 契约** + 自研 `${}` 表达式求值器 | 4,474 |
| `cmx-flow-bpmn` | L1 | BPMN 2.0 XML → 中立 IR 编译器（`roxmltree`，前缀无关；不支持元素**编译期显式报错**） | 1,551 |
| `cmx-flow-engine` | L1 | 令牌执行内核 `Engine<S: RuntimeStore>`（`run_to_wait` 步进）+ `JavaDelegate`/`DelegateRegistry` + `InMemoryStore` | 5,016 |
| `cmx-flow-def` | L2 | 流程定义持久化（草稿/发布/版本/装载，存 XML 非 IR） | 887 |
| `cmx-flow-store-pg` | L2 | `RuntimeStore` 的 PostgreSQL 实现 + IAM 候选人 resolver + 子流程组织 router | 3,081 |
| `cmx-flow-identity` | L2 | 可选内置身份模块（`fid_*` 表），`FLOW_IDENTITY_MODE=local` 时启用 | 533 |
| `cmx-flow-adapters` | L2 | 三注入 trait 的 **HTTP + Mock** 实现（`AssigneeResolver`/`SubflowRouter`/`DimensionResolver`/`Delegate`） | 1,004 |
| `cmx-flow-app` | **L3 芯** | 平台中立应用核：引擎单例 + 全 axum handler + 状态泛型路由 `flow_routes::<S>()` + 模拟/协同/决策端点 | 6,926 |
| `cmx-flow-server` | L4 壳 | 独立微服务二进制（chassis `ServiceSpec`，:8091） | 162 |
| `cmx-flow-demo` | L4 壳 | 自包含 axum 演示（内联 SPA，:8090） | 1,109 |
| `cmx-flow-worker-sdk` | 侧 | 外部作业 Worker 客户端 SDK（纯 reqwest，零 flow 依赖） | 256 |
| `cmx-flow-tests` | 侧 | 引擎端到端集成测试（默认内存态，无外部依赖，166 测试） | — |

**双重「一芯多壳」**：① 后端 `cmx-flow-app` 是芯，`cmx-flow-server`（独立）与 `cmx-flow-api`（门户内嵌，位于 sibling `cmx-container`）是两个壳，经 `flow_routes::<S>()` 对 `State` 泛型复用同一套 handler；② 前端「一芯三壳」：门户 native page 嵌入 / 框架无关 Web Components / Headless REST+SSE。门户仅以纯 HTTP 反代引擎，`cargo tree` 已验证门户不依赖引擎源码。

**七个可注入 trait**（详见 §七）实现了 mock / http / pg 三模装配，是引擎「框架无关 + 平台可插」的关键接缝。

---

## 三、领域模型与 BPMN 2.0 覆盖

{{FIG:bpmn}}

**建模格式**：BPMN 2.0 XML 是唯一的编辑/交换格式。`cmx-flow-def` 持久化 **XML + 版本 + 草稿/发布状态**，装载时重新编译成 IR（「存 XML 而非 IR：XML 是标准交换格式，IR 随代码演进」）。编译器 `compile(xml)` 基于 `roxmltree` DOM，**前缀无关**（自动剥离 `bpmn:`/`flowable:`/`camunda:`/`cmx:`），不支持的活动型元素**硬拒绝、绝不静默丢弃**。

**IR 表示**：arena（`Vec<FlowNode>` + `HashMap` 索引）+ 枚举分派，替代 Java 引擎的 `ActivityBehavior` 继承树。`NodeKind` 19 个变体即 BPMN 元素分类学；`Token{node_bpmn_id, state, parent_id}` 以稳定的 `bpmn_id` 锚定位置（跨进程安全）；`TokenState` 11 态（Active/Waiting/Joining/WaitingSubflow/WaitingMessage/WaitingTimer/WaitingAsync/WaitingEventGateway/Incident/Ended）。

**覆盖要点**（约 28 项已建模 + 执行 + 集成测试，每变体一份 `tests/*.rs`）：

- **事件**：空/消息开始、空/终止（一票否决）结束、中间捕获（定时/消息）、边界（定时·中断/非中断催办、错误·中断）、事件子过程（错误·中断）。
- **网关**：排他 XOR、并行 AND、包容 OR（可达性合并）、事件网关（竞速首胜）。
- **任务**：用户任务、服务任务（同步/异步 Job/外部 Worker）、业务规则任务（决策表）、调用活动（子流程 + 变量映射 + 组织维度路由）。
- **多实例**：并行会签、串行或签、完成条件（提前结束 + `nrOfInstances` 等注入）、动态办理人。
- **数据**：过程变量 + 变量历史、`<cmx:varSchema>` 声明式变量 schema（软校验）、维度/组织路由。

**表达式语言**：一套**自研、手写、零依赖的递归下降求值器**（`expr.rs`），**刻意不用 FEEL、不用 JS、不用 rhai**——支持 `|| && == != < <= > >= + - * /`、一元 `! -`、括号、`${}` 剥离、点式嵌套路径（`order.amount`），内建 `TRIM/UPPER/LOWER/LEN/CONTAINS/IN/COALESCE/IF` 等 ~21 个纯函数（**刻意不含时间函数**——时间由注入式 `Clock` 提供以保证确定性）。缺失变量 → null → falsy，永不 panic；`validate_syntax` 支撑设计器保存前校验。**定时器表达式为 ISO 8601**（`PT24H` / RFC3339 / `R3/PT1H` / `${dueDate}`）。

**诚实的缺口**（编译期显式报错，不静默降级）：补偿（边界/抛出/活动）、信号事件与信号关联（仅支持消息关联）、升级事件、事务子过程、复杂网关、脚本/发送/接收/手工任务、全部中间抛出事件、定时/信号/条件开始事件。嵌入子过程与事件子过程为**编译期扁平化**（不支持块级边界事件；事件子过程仅错误·中断触发）。

---

## 四、执行引擎（令牌持久化语义）

{{FIG:token}}

**核心步进函数 `run_to_wait`** 是一个单线程循环：反复取出首个 `Active` 令牌，克隆其 `kind`/`outgoing` 以跨 `await` 断开对 `&def` 的借用，`match NodeKind` 前进恰好一步；直到无 `Active` 令牌时停止，若全部令牌 `Ended` 则实例落 `Completed`。`STEP_LIMIT` 守卫防止跑飞。

- **分叉/合并**：并行 AND 无条件克隆令牌到全部出边，合并按结构（`incoming_count > 1`）检测——到达令牌停为 `Joining`，计数达 `incoming_count` 时保留单 survivor；包容 OR 取全部条件真出边，合并用 `can_reach` BFS 可达性探测（无其它令牌可达才释放，对无环审批流正确）；事件网关武装定时 + 消息后继，首个到达胜出并取消其余。
- **持久化 = 提交点**：每个「运行段」= 装载快照 → 内存推进至等待态/结束 → **一次 `save_snapshot` DB 事务**。等待态（UserTask、CallActivity、WaitingMessage/Timer/Async/EventGateway）即精确的提交点。运行态以 `InstanceSnapshot` 原子聚合落库（instance + tokens + tasks + mi_scopes + jobs + candidates + cc + delegations）。
- **集群安全（HA）**：异步作业经 `UPDATE … WHERE id IN (SELECT … FOR UPDATE SKIP LOCKED)` 取件，N worker 获得不相交作业集；异步作业写在**独立侧表**（刻意排除出快照的删+插，避免并发保存抹掉锁）。到期定时器由 `trigger_due_timers` 跨实例扫描。
- **失败隔离**：delegate 抛 `DelegateError::Bpmn{code}` → 匹配错误边界 / 事件子过程；`Generic` → 令牌停 `Incident`（留存）→ `retry_incident`；异步重试耗尽 → **死信队列** → 运维 retry/discard。
- **持久化模型**：`cmx_flow_*` 表，运行态 RU 与历史 HI 分离、幂等 DDL、无外键；历史 `hi_activity` 记录逐节点进/出 + `duration_ms`，驱动 SLA/回放。

---

## 五、企业运行时能力

{{FIG:runtime}}

| 能力 | 机制 | 状态 |
| --- | --- | --- |
| **异步作业执行器** | `serviceTask flowable:async` → 独立 `cmx_flow_async_job` 侧表；`FOR UPDATE SKIP LOCKED` 轮询，N worker 互斥取件 | 实现 + 测试（`p1_async_job`） |
| **死信队列 → Incident** | 重试耗尽写 `cmx_flow_deadletter_job` + 令牌转 Incident；`retry_dead_letter_job` 重建作业 | 实现 + 测试（`p2_dead_letter`/`h2_incident`） |
| **外部 Worker** | `serviceTask type=external-worker topic=X` + `cmx-flow-worker-sdk`（acquire/complete/fail，X-API-Key，锁 TTL） | 实现 + 测试（`a7_external_worker` + SDK 单测） |
| **定时器 + 注入式时钟** | `Arc<dyn Clock>`（Prod `SystemClock` / 测试 `TestClock` 可快进）；边界（中断/非中断）+ 中间捕获定时器，ISO-8601 | 实现 + 测试（`m2_5_timers`/`a1_intermediate_timer`） |
| **实例迁移** | `MigrationPlan{target, activity_mappings}` → `validate_migration` dry-run（4 违规码）→ 重写令牌 `node_bpmn_id` | 实现 + 测试（`a9_migration`） |
| **活动历史 / SLA** | RU/HI 分离，`hi_activity` 记 enter/exit + `duration_ms`；`analytics/node-timing` 算瓶颈/SLA | 实现 + 测试（`a6_activity_history`） |
| **Incident 管理** | 失败停令牌于 `Incident` + `__incident` 变量；`retry_incident` 重激活，`set_instance_variables` 先修数据 | 实现 + 测试（`h2_incident`） |
| **多租户（db-per-tenant）** | 租户→`db_id` 映射，首次访问**懒注册**数据源；租户由 JWT claim / API-Key 绑定，`task_local` 作用域隔离每次查询与 SSE | 实现（默认 single 零回归） |
| **认证（off/JWT/API-Key）** | JWT HS256/RS256 + 服务间 `X-API-Key`（key→tenant）+ `X-Delegated-User-Token`（真实办理人）；SSE **一次性票据**（60s TTL）绕过 EventSource 无头限制 | 实现 + 测试（`sse_jwt_ticket`） |
| **Headless v1 契约** | `/flow/v1/*` REST + 按租户 SSE `/events` + HMAC-SHA256 签名 Webhook（指数退避重试）+ 手写 OpenAPI + Swagger `/docs` | 实现 + 测试（webhook 单测） |
| **平台对接** | chassis 启动 + Nacos 自注册（默认 Mock）；自投递 native/html 页对齐门户信封供 F3 反代；`FlowProxyModule` 一行配置切换内嵌/独立 | 实现（门户侧壳在 sibling 仓） |

---

## 六、设计态与前端

- **流程设计器（bpmn-js）**：四区 native-page 工作台，bpmn-js `Modeler` 注入 shadow-DOM；属性面板经 `modeling.updateProperties` 回写；草稿/发布/校验/撤销/自适应视图；后端草稿（试编译）、发布（版本+1）、版本列表/激活/删除。
- **模拟试跑 + 版本 diff**：模拟复用引擎原语（`compile`/`eval_condition`/`decision::evaluate`/`AssigneeResolver`）以样本变量试跑 → 令牌路径 / 网关分支 / 办理人 / 决策输出，**无持久实例、无副作用**；版本 diff 为纯前端 DOMParser，产出 `{added, removed, modified}`。
- **决策表（查看 + 编辑）**：`DecisionTable` 模型（FIRST/COLLECT 命中策略），条件格为 `expr.rs` DSL、输出为 JSON；`businessRuleTask` 引用；含只读查看器 + 可编辑设计器（增删行列 + fx 向导 + 热注册入库）。
- **运维台 + 令牌回放**：Camunda-Operate 式单实例视图，bpmn-js 只读 + **活令牌标记**（活节点高亮）+ Incident 高亮 + 变量表 + 干预工具条（retry-incident / set-variables / cancel）；**令牌回放**用时间滑块回溯 `hi_activity` 历史。
- **实时协同建模（M1/M2/M3）**：进程内 tokio-broadcast 按 `(tenant, defKey)` 键控。M1 = 在场花名册（TTL + 远端选中高亮）+ 草稿保存乐观锁（`updated_at` etag → 冲突提示）；M2 = **对象级 op-log**（单调 seq + LWW 属性增量合并 + 回声守卫）；M3 = 结构级操作（`createShape`/`createConnection`/`removeElements`/`moveShape`）转发。
- **可嵌 Web Components**：`FlowElementBase` 把 S4 抽出的核心模块封装成框架无关自定义元素 `<flow-designer>` / `<flow-task-form>` / `<flow-todo>`（属性→`configure()`，shadow 宿主→`mount()`）。
- **监控大盘**：`/` 自包含 HTML（canvas 图表 + light/dark + 5s 轮询 `/stats`），`flow_stats` 聚合计数/状态分布/定时作业/协同指标/30 天时间线（按租户）；技术面板 `/_mon` 经 `cmx-web-monitor` 挂载。
- **表单集成**：`cmx_flow_biz_link`（单据↔实例双向）、`cmx_flow_task_comment`（逐步审批意见）、`cmx_flow_form_binding`（formKey→workspace/html/native）；待办中心（5 类）+ 任务表单宿主（5 级 `openWorkNode` 兜底）。

---

## 七、可扩展性（7 注入 trait + 三模适配器）

契约定义在零依赖内核，实现在装配期按 env 模式注入（`AdapterConfig` mock / http / pg）：

| Trait | 用途 | 默认/Mock | 真实实现 |
| --- | --- | --- | --- |
| `RuntimeStore` | 驱动无关运行态持久化；save = 一次等待态提交点 | `InMemoryStore` | `PgRuntimeStore` |
| `JavaDelegate` | serviceTask 执行体（名承 Flowable）；`Bpmn{code}` → 错误边界 | `MockDelegate` | `HttpDelegate` + 业务 delegate |
| `Clock` | 只回答「现在」（UTC），支撑确定性定时器测试 | `SystemClock` | `TestClock`（advance/set） |
| `AssigneeResolver` | 候选人引用 → 用户 ID 列表（User/Role/Position/Org + 关系型 OrgLeader/Initiator） | `MockAssigneeResolver` | `HttpAssigneeResolver` / `PgIamAssigneeResolver` / `LocalAssigneeResolver` |
| `SubflowRouter` | 逻辑子流程键 + 维度 → 具体定义键（精确 → 沿路径继承 → NULL 兜底） | `MockSubflowRouter` | `HttpSubflowRouter` / `PgSubflowRouter` |
| `DimensionResolver` | 可选：返回维度的祖先值链，将层级外部化 | `MockDimensionResolver` | `HttpDimensionResolver` |
| `DefinitionStore` | 流程定义原子读写（草稿/版本/装载） | — | `PgDefinitionStore` |

这套接缝使引擎既能作为**内存态纯库**跑单测（`InMemoryStore` + `Mock*`），也能装配为**生产 PG 多租户服务**（`Pg*`），或**跨服务 HTTP 外部化身份/组织/委托**（`Http*`）——身份、组织、表单全部外部化，引擎本身不绑定任何 IAM。

---

## 八、部署姿态

{{FIG:deploy}}

同一 `cmx-flow-app` 核，四种落地：① **独立微服务**（`cmx-flow-server` :8091，门户经 `FlowProxy` 纯 HTTP 反代 `/api/flow/*`，db-per-tenant 物理隔离）；② **可嵌组件**（第三方 App 直嵌 `<flow-designer>` 等自定义元素，直连 v1 API）；③ **Headless**（自建前端/系统消费 REST + SSE + OpenAPI）；④ **自包含 demo**（`cmx-flow-demo` :8090 内联 SPA，供本地评估）。三层出站鉴权：`X-API-Key`（服务身份）+ `X-Delegated-User-Token`（真实办理人）+ `X-Request-Id`。

---

## 九、与主流开源流程引擎全方位对比

这是本报告的核心。**重要口径说明**：仓库内既有的 `vs-flowable` 与 `vs-world-class` gap 文档成文较早（v0.1.12 之前），其「待补」清单仍把 A1–A10 / P1–P4 列为缺口；而这些**均已交付并测试**（包容/事件网关、消息开始、终止、错误边界、中间定时/消息捕获、SKIP-LOCKED 异步执行器、死信、Incident+重试、活动历史、外部 Worker、实例迁移）。下文一律以**当前实测能力**为准。

{{FIG:compare}}

### 9.1 对比对象

- **Camunda 7**（Java）：最成熟、BPMN 覆盖最全的传统引擎（含补偿/事务子过程/CMMN），可嵌入 + 独立，作业执行器 + 外部任务 + 多租户（schema/表/库）。**社区版（CE）已于 2025-10-14 EOL**（v7.24 终版、仓库归档），企业版延至 2030。
- **Camunda 8 / Zeebe**（Go/Java）：云原生、**事件溯源分区日志**（Raft + RocksDB），gRPC，历史经 Exporter 落 Elasticsearch，互联网级吞吐。**源码可得（Camunda License v1.0，非 OSI 开源）；自 8.6 起核心引擎生产使用需付费许可**。
- **Flowable**（Java 17+）：Activiti 分支，**完全 Apache-2.0 开源**核心（BPMN + CMMN + DMN 三引擎），可嵌 + 独立 + Spring，异步执行器 + 外部 Worker，最新 7.2.0（2025）。OSS BPMN 覆盖与 Camunda 7 并列最全。
- **Temporal**（Go，**MIT**）：**代码优先持久执行**（非 BPMN），多语言 SDK，确定性重放，互联网级。不同范式，无 BA 面向的可视化建模。
- 另：**Activiti**（Apache-2.0，与 Flowable/Camunda 7 同源，动能已弱）、**jBPM/KIE**（Apache-2.0，BPMN + DMN + Drools + 案例管理）、**Netflix/Orkes Conductor**（Apache-2.0，JSON-DSL 编排，非 BPMN）。

### 9.2 全维度对比矩阵

上图（figure）为 5 引擎 × 15 维度的对比。下表进一步展开到 6 引擎、更细维度，供逐项核对：

| 维度 | cmx-flowengine | Camunda 7 | Camunda 8 / Zeebe | Flowable | jBPM | Temporal |
| --- | --- | --- | --- | --- | --- | --- |
| 语言/运行时 | **Rust**（无 GC/单二进制） | Java/JVM | Go + Java | Java 17 | Java | Go |
| 建模标准 | BPMN 2.0（子集） | BPMN+DMN+CMMN | BPMN+DMN | BPMN+DMN+CMMN | BPMN+DMN+案例 | 代码优先（无可视） |
| 执行模型 | 令牌·持久化 | 令牌·关系库 | 事件溯源·分区日志 | 令牌·关系库 | 令牌·关系库 | 溯源·确定重放 |
| 持久化 | PostgreSQL | 关系库 | RocksDB + 日志 | 关系库 | 关系库 | 可插拔 |
| BPMN 元素广度 | 子集·28 能力 | 近乎完整 | 子集（无补偿等） | 近乎完整 | 近乎完整 | N/A |
| DMN/决策 | 规则引擎 + 决策表 | 完整 DMN | DMN | 完整 DMN | 完整 DMN + Drools | 代码内 |
| 人工审批 | 完整·7 类办理人（含关系型） | 完整 | 基础任务 | 完整 | 完整 + 案例 | 无 |
| 异步作业执行器 | SKIP LOCKED | 线程池 + 锁 | 分区并行 | SKIP LOCKED | 作业执行器 | 分片队列 |
| 外部 Worker | ✓ + Rust SDK | ✓ 外部任务 | ✓ Job Worker | ✓ | ✓ | Activity Worker |
| 死信/重试 | DLQ + Incident | 死信 + Incident | Incident | 死信队列 | 重试 | 重试策略 |
| 实例迁移 | ✓ dry-run 校验 | ✓ Migration API | ✓ 8.5+ | ✓ | ✓ | Reset/Patch |
| 多租户 | **db-per-tenant** | 库/schema/表 | ✓ 8.3+ | ✓ | ✓ | Namespace |
| 设计器 | bpmn-js 四区·**内置** | 桌面 Modeler | Web Modeler（云） | 网页 Modeler | Business Central | 无 |
| **实时协同建模** ★ | **✓ SSE M1/M2/M3** | – | 云 SaaS 版 | – | – | N/A |
| 可嵌入形态 | 库/服务/**Web 组件** | 库/服务 | 仅集群 | 库/服务 | 服务 | 仅集群 |
| 监控/运维 | 运维台 + **令牌回放** | Cockpit | Operate | Admin | Business Central | Web UI |
| 水平扩展/吞吐 | HA worker·ERP 级 | 集群作业器 | **互联网级** | 集群 | 集群 | **互联网级** |
| 语言 SDK | Rust worker-sdk + REST | Java + REST | 多语言 gRPC | Java + REST | Java + REST | **多语言** |
| **许可/开放性** ★ | **Apache-2.0（无限制）** | Apache 但 **CE 已 EOL** | **源码可得·生产收费** | Apache-2.0 | Apache-2.0 | MIT |
| 生态/成熟度 | 年轻·集成平台一体 | 成熟·庞大社区 | 成熟·云生态 | 成熟 | 成熟·规则强 | 成熟·多语言 |

### 9.3 逐维度详解

- **语言与运行时**：cmx 是当前 BPMN 生态里**罕见的 Rust 引擎**。相较 Camunda/Flowable/Activiti/jBPM 的 JVM（堆内存、GC 停顿、较重冷启动），cmx 单静态二进制、无 GC、低内存、快启动——契合作为 ERP 微服务密集部署。
- **执行模型与持久化**：cmx 与 Camunda 7/Flowable/jBPM 同属**令牌 + 关系库**范式（cmx 用 PostgreSQL，「等待态即提交点」）；Zeebe/Temporal 是**事件溯源 + 日志/分区**，天然横向扩展与高吞吐，但运维复杂度与依赖（Raft/ES）更重。cmx 的 `SKIP LOCKED` + 侧表锁隔离，是在关系库范式内取得的集群安全折中。
- **BPMN 广度**：cmx 覆盖约 28 项（审批够用），Camunda 7/Flowable 近乎完整（补偿、事务子过程、全事件类型、CMMN），Zeebe 是 BPMN 子集（无补偿等），Temporal 无 BPMN。cmx 的缺口（补偿/信号/升级/事务子过程/复杂网关）已在路线图 Next/Later。
- **DMN/决策**：Camunda 7/Flowable/jBPM 提供**认证的 DMN 引擎（FEEL + DRD + 多命中策略）**；cmx 走**自研规则引擎（`cmx-rulesengine`，FEEL 子集）+ 决策表设计器**——满足业务判定的 80/20，复杂决策交由规则引擎，而非在流程引擎内实现完整 DMN 标准。
- **人工审批**：cmx 是**强项**——会签/或签/加签/转办/委派/抄送/退回任意节点/取回 + **7 类办理人（含关系型 OrgLeader/Initiator）** + 身份双模，与 Camunda 7/Flowable 并列，明显强于 Zeebe（仅基础任务）与 Temporal（无人工任务）。
- **可靠性四件套**：异步执行器 + 死信 + Incident + 实例迁移，cmx 均已具备且经测试，与 Camunda 7/Flowable 对齐；Zeebe/Temporal 在此之上还有 backpressure/failover/确定性恢复等分布式语义。
- **多租户**：cmx **db-per-tenant 物理隔离**（比 Camunda 7 的单库 tenant_id 逻辑隔离更强），Zeebe 多租户较晚（8.3+）。
- **设计器与实时协同** ★：cmx 把 **bpmn-js 四区设计器 + 模拟 + diff + 实时协同（M1/M2/M3）内置于开源引擎**；而 Camunda 的实时协同是**云 SaaS（Web Modeler）**，Flowable Modeler 为单用户——**开源引擎自带多人实时协同建模在业界少见**，是 cmx 的差异化亮点。
- **可嵌入形态** ★：cmx = 库 + 服务 + **框架无关 Web Component** 三形态；Camunda 7/Flowable = 库 + 服务；Zeebe/Temporal = 仅集群。Web Component 级前端嵌入不常见。
- **可观测**：cmx 运维台含**令牌活标记 + 历史回放**（对标 Camunda Cockpit/Operate），并有大盘 + `/_mon` 技术面板。
- **伸缩天花板**：cmx 面向 **ERP 级**（HA worker + db-per-tenant），非互联网级；Zeebe/Temporal 才是百万级吞吐王者。这是取舍，不是缺陷。
- **生态成熟度（诚实差距）**：Camunda/Flowable 有多年生产打磨、连接器市场、庞大社区；cmx 年轻，但作为**一体化 Rust 平台**（流程 + 规则 + 本体 + 数据权限 + 报表同栈）另有集成优势。

### 9.4 许可与开放性

{{FIG:openness}}

许可是被低估但关键的差异维度：

- **cmx-flowengine = 真 Apache-2.0（OSI 批准）**：可自托管、可商用、可魔改，**无生产许可门槛、无 EOL 风险**。
- **Camunda 8 / Zeebe = 源码可得（Camunda License v1.0，非 OSI）**：8.6 起核心引擎「生产使用」需付费许可；仅连接器 SDK/客户端/Exporter 仍 Apache-2.0。「源码可得」≠「开源」。
- **Camunda 7 CE = Apache-2.0 但已 EOL**（2025-10-14 v7.24 终版、停更、停安全补丁），企业版付费延至 2030。
- **Flowable / Activiti / jBPM / Conductor = Apache-2.0**（真开源）；**Temporal = MIT**（但非 BPMN）。

对「需要自托管、可商用、长期可持续的 BPMN 引擎」而言，cmx 与 Flowable 站在最有利的一侧。

### 9.5 差异化优势 vs 诚实差距

**cmx 的差异化优势**：① 唯一严肃的 **Rust** BPMN 引擎（无 GC/单二进制/低占用）；② 开源引擎**内置多人实时协同建模**；③ **Web Component** 级可嵌形态；④ 真 **Apache-2.0** 无限制（对比 Camunda 的 EOL/源码可得）；⑤ **一体化 Rust 平台**（流程/规则/本体/数据权限/报表同栈）；⑥ 干净的 **7-trait** 扩展点 + 三模适配。

**诚实差距**：① BPMN 广度不及 Camunda 7/Flowable（缺补偿/信号/事务子过程/复杂网关/CMMN）；② 无认证 DMN/CMMN 标准引擎（用自研规则引擎替代）；③ 吞吐/伸缩不及 Zeebe/Temporal（非分区/事件溯源，ERP 级而非互联网级）；④ 生态/社区/连接器市场成熟度远逊于 Camunda/Flowable；⑤ 语言客户端 SDK 少于 Temporal/Zeebe。

---

## 十、测试与质量

{{FIG:tests}}

| 类别 | 数量 | 说明 |
| --- | ---: | --- |
| Rust 测试函数 | **279** | 93 单测（`src/`）+ 186 集成（`tests/`）；0 失败；25 个 PG 活库集成默认 `#[ignore]` |
| 后端全量回归 | **159/159** | `run-all.sh`，12 套件（设计/办理人/主子流程/退回/取回/转签/抄送/会签/网关/条件决策/异常/MI-B） |
| 子流程专项 | **60/60** | `run-subflow.sh`，4 套件（多挂载/组织路由/变量映射嵌套/边界） |
| 维度路由 | **25/25** | `dimtest_routing.sh`，RD0–RD4 |
| 新功能真机 E2E | **18/18** | `e2e-new-features.sh`，A7/A9/P1/A8/A3（PG 后端 :8091，数据留库供运维台查） |
| 差旅报销业务 E2E | **22/22** | `biz-test`，5 大场景 |
| 子流程钻入（门户级） | **23/23** | `subflow_drilldown.cjs`（Playwright/CDP） |
| 设计器模拟+diff / 协同 | 12/12 · 32/32 · … | `designer_simulate_diff.cjs` / `collab_*.cjs`（22 个 CDP 脚本，前端 15/17 套件全绿） |

测试纪律：只增不删、数据留库；涉及 PG 参数序列化的 bug 必须真机 E2E（内存态不序列化参数、PG 集成测试 `#[ignore]`，是单测盲区——历史上「无 org 发起」的 jsonb 裸 NULL 序列化 bug 唯真机能抓）。

---

## 十一、能力演进时间线

{{FIG:timeline}}

七条能力轨全部已交付并纳入回归：引擎核心（M1–M5.3）、BPMN 补齐（A1–A10）、可靠性（H1–H4 · P1–P4）、人工审批、维度路由（RD0–RD5）、设计器/大前端、微服务化（S0–S6）。里程碑代号取自源码/测试文件命名。

---

## 十二、路线图 Now / Next / Later

{{FIG:roadmap}}

**Now（已交付并测试）**：令牌引擎 + 持久化、A1–A10、P1–P4、会签/或签、子流程 + 维度路由、多租户、Headless、设计器 + 协同、运维台 + 回放、实例迁移/外部 Worker/死信。
**Next（审批完整度）**：信号/升级事件、补偿、FEEL/DMN 子集深化、更多 delegate/连接器、变量历史归档增强。
**Later（世界级·择机）**：事务子过程、CMMN、事件注册中心（Kafka/RabbitMQ）、水平扩展（分区执行）、增量快照、多语言 Worker SDK。

策略：审批赛道够用即止；优先「把审批引擎做透 + 做到生产可靠」而非盲目追通用广度（更优 ROI）。*Next/Later 为规划项，非承诺排期。*

---

## 十三、结语

cmx-flowengine 已是一套**审批扎实、工程质量过硬、生产可靠**的 Rust BPMN 引擎：从令牌执行内核、集群安全的异步执行器、死信与 Incident，到 db-per-tenant 多租户、Headless 契约、内置实时协同设计器与运维台，端到端可用且经 279 Rust 测试 + 多层 E2E 真机验证。它在**人工审批赛道**已达上乘，并以真 Apache-2.0 开源、Web Component 可嵌、一体化 Rust 平台形成差异化；同时诚实地把 BPMN 广度（补偿/事务子过程/CMMN）与互联网级伸缩留在路线图上，按业务诉求择机推进——**不是玩具，也不假装是世界级通用引擎，而是把一件事做到位**。

---

## 附录：来源与方法

- **能力/代码事实**：均来自对当前工作树的直接核对（`crates/cmx-flow-*/src`、`tests/`、`docs/full-test/`），非陈旧文档。关键文件：`cmx-flow-model/src/{ir.rs,runtime.rs,expr.rs}`、`cmx-flow-bpmn/src/compiler.rs`、`cmx-flow-engine/src/engine.rs`、`cmx-flow-store-pg/src/{ddl.rs,store.rs}`、`cmx-flow-app/src/{engine.rs,collab.rs,simulate.rs,openapi.rs}`、`cmx-flow-worker-sdk/src/lib.rs`。
- **规模/测试口径**：`cargo` 树实测——12 crate、25,012 src LOC、9,351 test LOC、279 测试函数（25 `#[ignore]`）；E2E 159/159 · 60/60 · 25/25 · 18/18 · 22/22。
- **竞品事实**（2026-08 核对）：Camunda 7 CE EOL 2025-10-14（企业版延至 2030）；Camunda 8/Zeebe 源码可得（Camunda License v1.0），8.6 起核心引擎生产需付费许可；Flowable 7.x 完全 Apache-2.0（Java 17+）；Temporal MIT；Activiti/jBPM/Conductor OSS 均 Apache-2.0。
- **图表**：全部为自绘、内嵌 base64 的 SVG（CVD-安全调色板、状态以「图标+文字」编码不靠色区分）；工具链见 `docs/report/assets/`（`gen.mjs` 生成 · `shot.mjs` Chrome 渲图查版 · `build.mjs` base64 内嵌为 `<img>`）。
