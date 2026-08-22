# cmx-flowengine · 阶段性总结

> **独立 BPMN 2.0 流程引擎微服务** · v0.1.12 · 截至 2026-08-22
> 一芯多壳 · 审批赛道为先 · 全链路真机验证 · 与平台编译期解耦

---

## 摘要（TL;DR）

`cmx-flowengine` 是从 `cmx-container` 抽出的**独立流程引擎微服务**：一个语义中立的令牌执行内核（`cmx-flow-model` → `-bpmn` → `-engine`），外裹持久化/适配器/身份/定义四注入层，再由平台中立应用核 `cmx-flow-app` 统一装配成路由；对外经**三种壳**交付——独立 server（`:8091`）、门户反代内嵌、可嵌 Web Component。

- **规模**：**12 个 crate**（+ 外部 worker SDK）、约 **25k** 域代码 + **9k** 测试代码；edition 2024、工具链 1.97.1。
- **能力**：BPMN 审批核心构件**全覆盖**（任务/网关/事件/多实例/子流程），人工审批全链路（会签·或签·转签·加签·委派·抄送·退回任意节点·取回·7 类办理人·身份双模），子流程**维度路由**（任意字典维度、三级解析、多挂载），异步可靠性（SKIP-LOCKED 执行器·死信队列·incident 重试·错误边界），实例迁移、活动历史、事件网关、事件子流程。
- **本轮增量（0822 · 设计器大前端三件套）**：设计器**模拟试跑**（facts→trace + 画布高亮，不建实例） · **版本 diff**（结构级 XML 对比，纯前端） · **协同 M1**（感知 + 防冲突：presence 在场 / 远端选中高亮 / 草稿保存乐观锁 / 草稿已更新通知）。详见 §六。**这补齐了上一轮（0821）延后的唯一「Next 大前端」项**。
- **上一轮（0821 · Next 后端 5 项）**：决策表发布**落库持久化** · 变量**历史 + TTL/归档** · **RD5 HTTP 维度解析器** · 外部 **job worker SDK**（新 crate） · 平台**身份/维度回连端点**。详见 §七。
- **质量**：`cargo test --workspace` **273 测试函数 · 0 失败**；后端全量回归 **159/159**、子流程 **60/60**、维度路由 **25/25**、子流程钻入 **23/23**、差旅业务 E2E **22/22**、新功能真机 E2E **18/18**、0821 后端 5 项 **5/5**；**本轮 0822 前端**：设计器模拟+diff **12/12**、新功能可视化验收 **10/10**、协同 M1 双用户真机 **6/6**，FE **9 套件全绿**（自包含只依赖 `:8091`）。数据只增不删、全程留库。
- **解耦**：门户改纯 HTTP 反代（`cmx-flow-api` 瘦成 proxy-only），`cargo tree` 已验证门户编译图**不再含任何引擎 crate**——改引擎源码不再触发门户重编。

---

## 一、架构总览：一芯多壳（One Core, Multi-Shell）

![架构总览 · 一芯多壳](@@fig-1-architecture@@)

自底向上分层，每层单一职责、依赖单向向下：

- **`cmx-flow-model`（语义中立内核）**：编译后 IR（`ProcessDefinition`/`FlowNode`/`NodeKind`）+ 运行态（`ProcessInstance`/`Token`/`Task`/`Variables`）+ 条件求值 + `RuntimeStore` trait。零 DB、零框架，可 wasm/嵌入。
- **`cmx-flow-bpmn`**：BPMN 2.0 XML → 中立 IR 的编译器，单入口 `compile(xml)`；不支持的元素**编译期显式报错**（非静默跳过）。
- **`cmx-flow-engine`**：令牌执行内核 `Engine<S: RuntimeStore>`，**等待态即提交点**（pause+persist）；`JavaDelegate`/`DelegateRegistry` 承载 serviceTask，泛型于存储。
- **四注入层**：`store-pg`（PG 持久化 + 幂等 DDL + 装配者解析 + 子流程路由）、`adapters`（三注入 trait 的 HTTP/Mock 实现）、`identity`（内置 `fid_*` 身份，独立部署免外部 IAM）、`def`（定义持久化：草稿/发布/版本）。
- **`cmx-flow-app`（平台中立应用核）**：引擎单例 + 全部 axum handler + `flow_routes::<S>()` 泛型路由 + 自持响应信封 + 业务关联表。**这是"一芯"** —— 独立 server 与门户反代壳共用它。
- **三壳**：① 独立 `cmx-flow-server`（`:8091`）② 平台反代壳 `cmx-flow-api`（门户内嵌，纯 HTTP 转发）③ 可嵌 Web Component（`web/elements`，框架无关）。

> 单向借用 `cmx-container` 的 5 个基础库（`cmx-database-pg`/`cmx-core`/`cmx-web-chassis`/`cmx-web-monitor`/`cmx-service-base`），仅编译期 path 依赖、**无任何反向引用**。

**注入扩展点**（依赖反转，皆经 `engine` 装配）：`JavaDelegate`（服务任务）· `Clock`（可注入时钟，测试可控时间）· `AssigneeResolver`（办理人解析）· `SubflowRouter`（子流程/维度路由）+ **`DimensionResolver`（RD5，可选：维度层级经 HTTP 回连）**。

---

## 二、部署形态：三姿态，同一引擎核

![三种部署姿态](@@fig-2-deployment@@)

- **① 独立微服务**：门户 `:8080` 经 `FlowProxyModule` 把 `/api/flow/*` 反代到独立 `flow-server :8091`，引擎连 PostgreSQL（`fico` + `cmx`，多租户 `flow_<tenant>` db-per-tenant 物理隔离）。
- **② 可嵌组件**：第三方 App（React/Vue/原生）直接放 `<flow-designer>`/`<flow-todo>` 自定义元素，连 `flow-server` 的 v1 API。
- **③ Headless**：完全自建前端/系统，只用 `/api/flow/v1/*` + `/events`（SSE）+ OpenAPI/Swagger。

**编译期解耦（本阶段收口）**：门户侧 `cmx-flow-api` 已从"内嵌+反代双壳"瘦成 **proxy-only**，不再依赖 `cmx-flow-app`。`cargo tree -p cmx-portal-server -i cmx-flow-engine` 报 "did not match any packages"，`touch` 引擎源码后门户增量编译 **0.28s 零重编**。三引擎（flow / report / rules）现已**完全同构**：纯反代壳、零编译期依赖、`merge_*` 无内嵌分支。

**三层出站鉴权**：`X-API-Key`（服务身份）+ `X-Delegated-User-Token`（真实办理人 JWT，租户优先取委托 claim）+ `X-Request-Id`（链路）。

---

## 三、能力演进：全功能梳理

![能力演进时间线](@@fig-3-timeline@@)

八条能力轨，均已交付并纳入回归：

| 轨 | 代号 | 覆盖 |
|---|---|---|
| **引擎核心** | M1–M5.3 | 定义/发起 · 网关 · 边界定时器 · 多实例 · 抄送/转签 · 子流程 + 组织路由 + 多挂载 |
| **BPMN 补齐** | A1–A10 | 包容网关 · 终止 · 事件子流程 · 扁平子过程 · 活动历史 · 外部 Worker · 错误边界 · 实例迁移 · 事件网关 |
| **可靠性** | H1–H4 · P1–P4 | 热加载 · incident · 异步 Job · 运维视图 · SKIP-LOCKED 执行器 · 死信队列 · 令牌可视化 |
| **人工审批** | — | 会签/或签 · 转签/加签/委派 · 抄送 · 退回任意节点 · 取回 · 7 类办理人 · 身份双模 · 表单绑定 · 审批意见留痕 |
| **维度路由** | RD0–RD4 | 契约泛化 · 注入解析 · 精确/继承/兜底三级 · 多挂载 · 设计器 |
| **设计器** | — | 定义持久化 · 四区画布 · 属性面板 · 变量声明 · Excel 公式栏 · 子流程钻入式 · **模拟试跑 · 版本 diff · 协同 M1** |
| **微服务** | S0–S6 | 迁移 · 适配器 · 多租户 · headless · 前端抽核 · 组件 · 平台反代 |
| **后端补齐** | 0821 | 决策表落库持久化 · 变量历史+TTL/归档 · RD5 HTTP维度解析 · 外部 worker SDK · 身份/维度回连端点（详见 §七） |
| **大前端** | 0822 | 设计器模拟（facts→trace+画布高亮） · 版本 diff（结构级 XML 对比） · 协同 M1（感知+防冲突）（详见 §六） |

> **文案纠偏**：仓库 `README` 仍写"现状 S0 骨架"，技术方案的"能力边界"也称"无事件网关/无实例迁移/无 SKIP-LOCKED"——**均已陈旧**。实际能力已达 **S6 收口**，事件网关（A10）、实例迁移（A9）、SKIP-LOCKED 异步执行器（P1）皆已交付并测试通过。

---

## 四、BPMN 2.0 能力地图

![BPMN 2.0 能力地图](@@fig-4-bpmn-map@@)

审批赛道为先——**核心构件全覆盖**，未支持项要么审批场景极少用、要么按设计外置：

- **全覆盖**：userTask / serviceTask（同步·异步·外部 Worker）/ businessRuleTask / callActivity；排他·并行·包容·事件四网关；none/消息启动、终止结束、边界定时（中断/非中断）、边界错误、中间消息/定时捕获；多实例会签/或签 + 完成条件 + 逐元素动态派人；子流程同步/内嵌/维度路由/多挂载/变量映射。
- **部分**：事件子流程（仅错误触发·中断型）、令牌可视化（前端 bpmn-js 高亮，非引擎核）。
- **暂不支持（按需/择机）**：scriptTask、send/receive/manualTask、复杂网关、定时启动、错误结束事件、信号/补偿/升级/条件/link 事件、loopCardinality（按设计走集合驱动）。完整 FEEL/DMN 规则能力由 **cmx-rulesengine** 承担。

---

## 五、测试与质量

![测试覆盖 · 真机验证](@@fig-5-tests@@)

- **Rust**：`cargo test --workspace` **273 测试函数 · 0 失败**（1 组 PG 集成需活库，标 ignore）；集成用例在 `cmx-flow-tests`（命名即里程碑：`m1..m5` / `a1..a10` / `p0..p6` / `h2` / `withdraw` / `reject_targets`…）+ worker-sdk 单测/example。
- **后端 curl 回归**：`run-all.sh` **159/159**、`run-subflow.sh` **60/60**、维度路由 **25/25**、差旅业务 E2E **22/22**、新功能真机 E2E **18/18**（A7/A8/A9/P1/A3）、0821 后端 5 项真机 **5/5**。
- **前端 Playwright/CDP（本轮 0822 大前端全绿）**：设计器**模拟+diff** `designer_simulate_diff.cjs` **12/12**、设计器新功能可视化验收 `designer_features_capture.cjs` **10/10**（每功能一张带名截图）、**协同 M1 双用户真机** `collab_presence.cjs` **6/6**（两 browser context = 两用户共享同一草稿）；加既有子流程钻入 **23/23**、designer/wip/todo/rd4 等——**FE 共 9 套件全绿**，经共享 `_harness.cjs` **自包含只依赖 `flow-server :8091`**（不再要门户 :8080）。
- **纪律**：测试数据**只增不删、全程留库**；所有测试脚本与报告归档在 `docs/full-test/`、`docs/biz-test/`。

---

## 六、本轮增量 · 设计器大前端三件套（0822）

路线图「设计器：模拟 / 协同 / diff」的**最后一块**——上一轮（0821）是唯一延后的 Next 大前端项，本轮全部交付并真机验证。前端为 `web/core/design-workbench.js` 与 `web/ui-native/flow/design-workbench.js` **字节一致双份**（`cmp -s` 校验）；沿用「未提交待评审」惯例。

| # | 项 | 交付 | 真机验证（截图确认） |
|---|---|---|---|
| ① | **模拟试跑（simulate）** | 后端 `POST /flow/definitions/simulate`（复用 `compile` + `eval_condition` + `decision::evaluate` + 装配者解析，走 IR 重表达网关三规则，**不建实例无副作用**）；前端 property「模拟」页签：facts 表单 + JSON override → 运行 → trace 列表 + 画布高亮（走过节点/边/任务 `flow-sim-*`） | 大额 `{amount:30000}` → 走 `f_big` 分支、办理人含 **director**、7 节点可达结束；小额 `{amount:5000}` → 走 `f_small`、**director 缺席**——排他网关分支正确；切走页签高亮自清 |
| ② | **版本 diff** | **纯前端零后端**：「对比」按钮 → property「差异」页签，选从/到版本（草稿=当前画布 `getXml`，否则 `?version=N`）→ `DOMParser` 解析（仅 BPMN_MODEL_NS，自动排除 DI 布局层）→ 按 element id 结构 diff（added/removed/modified + changedFields）→ 列表 + 画布高亮（新增绿 / 修改琥珀）+ 点行定位 | `dd_diff_demo` v1→v2「4 处差异 · +2 新增 · ~2 修改」：新增任务B/连线 e3、修改任务A(名称)/连线 e2(终点)；画布任务B 绿、任务A改 琥珀；同版本守卫报错；退出清理 |
| ③ | **协同 M1 · 感知 + 防冲突** | 新 `crates/cmx-flow-app/src/collab.rs`：独立进程内 `broadcast` 通道 + 自由 JSON payload（与生命周期 `FlowEvent` 解耦），SSE 按 `(tenant, defKey)` 过滤；**内存 presence 注册表**（`Mutex<HashMap>` + sweep-on-build TTL=25s）；4 端点 join/heartbeat/select/leave。**防冲突**：`updated_at` 当 etag 乐观锁（`SaveDraftOutcome::Conflict`，免 DDL），冲突走 `code=0 + data.conflict`。前端：presence 头像条 / 远端选中高亮（`flow-collab-sel` 紫虚线）/ 保存冲突确认框 / 草稿已更新通知条 | 双用户（两 browser context）共享同一草稿：**各见 2 头像**（onopen 补 join 修 roster 竞态）；A 选中 → B 画布远端高亮；A 保存 → B 收「u_alice 更新了草稿·载入最新」；B 过期 base 保存 → 冲突确认框（防静默覆盖）；全程无 pageerror |

> **协同范围（M1 明示）**：只做**感知（presence/远端选中）+ 防冲突（乐观锁）**，**不做模型级实时合并**（op-log 对象级合并 = M2、结构级 = M3，留后续里程碑，对标报表协同 B 档渐进路线）。**已知限制**：presence 内存态 + SSE 进程内 bus → 单实例；EventSource 不能带 header → 仅 `off` 模式（本机 dev 默认；jwt 模式浏览器 SSE 鉴权留 M2）。

---

## 七、上一轮增量 · Next 后端 5 项（0821）

均真机验证 + `run-all` 159/159 零回归 + Rust workspace 绿；沿用「未提交待评审」惯例。

| # | 项 | 交付 | 真机验证 |
|---|---|---|---|
| ① | **决策表发布落库持久化** | `PgDecisionStore`（`cmx_flow_decision`，flow 租户库，整表 JSON）；注册**先落库再热注册**；启动装载回引擎；GET 列表 + DELETE（含 `unregister_decision`） | 注册→DB 行→**重启 flow-server→boot 装载 count→决策表存活**；delete 后 0 行 |
| ② | **变量历史 + TTL/归档** | `PgVarHistoryStore`；app 层 `diff_var_changes` 在 **start/complete/set-variables 三路径**捕获调用方变量变更（old/new/source/node/by）；`GET .../variables/history` + `POST /admin/var-history/sweep`（TTL 删旧） | 三路径全捕获、往返正确；sweep 端点生效 |
| ③ | **RD5 · HTTP 维度解析器** | model 加 `DimensionResolver` trait；adapters `HttpDimensionResolver`/`Mock`；注入 `PgSubflowRouter` 继承步——注入后逐祖先查**本地**绑定表（不直连字典表 JOIN）；env `FLOW_DIMENSION_MODE=http`+URL 触发，默认零回归 | org=`rd5_child` **无 PG org 行**仍经 HTTP 解析 → `[zongbu]` → hq（证走 resolver）；默认模式 S3C PG-JOIN 继承仍绿 |
| ④ | **外部 job worker SDK** | 新 crate `cmx-flow-worker-sdk`（纯 HTTP 客户端，零引擎依赖）：`WorkerClient` acquire(按 topic)/complete/fail/poll_once/run 长轮询 | example E2E：部署 external-worker BPMN→发起→SDK 抢占→handler→complete 回写变量→令牌推进到 approve |
| ⑤ | **平台身份/维度回连端点** | `cmx-flow-app` 加 `POST /identity/resolve`（镜像 AssigneeResolver，复用 `PgIamAssigneeResolver`）+ `GET /dimensions/ancestors`（复用新 `PgSubflowRouter.ancestors`）；**返裸 JSON**（`{userIds}`/`{ancestors}`，不包 `{code,msg,data}` 信封，契约对齐 Http* resolver） | 直连解析与 `PgIamAssigneeResolver` 逐字节一致；**round-trip**：flow 起 `FLOW_IDENTITY_MODE=http` 指向自身→引擎经 `HttpAssigneeResolver` 回连 → role(finance) 候选=`[u_fin1,2,3]` |

---

## 八、未来计划

![未来计划 · Now / Next / Later](@@fig-6-roadmap@@)

- **Now（已巩固）**：审批全链路 · 子流程维度路由 RD0–4 · 异步可靠性（incident/死信/SKIP-LOCKED）· S0–S6 独立微服务 + 平台反代 · 四区设计器 + 子流程钻入 · 一芯多壳前端 · 0821 后端 5 项（决策表落库 · 变量历史+TTL · RD5 维度 · worker SDK · 身份回连）· **本轮 0822 设计器大前端（模拟 / diff / 协同 M1）**。
- **Next（近期择机）**：变量历史引擎派生捕获（决策/子流程回填）· 决策表 / 变量历史前端可视化 · **协同 M2（op-log 对象级实时合并）** · 令牌可视化优化。
- **Later（B 级 · 长期择机）**：补偿事件（B2）· 全事件体系 信号/升级/条件/link（B4）· 完整 FEEL/DMN + DRD 序列化（B6）· 水平扩展执行（B8 · 暂不追，审批量级单进程够用）· 附件（接文档服务）· ZmcDataSet 数据集导出端点。
- **正交 / 坚决不做**：引擎不认字典/组织/DB（维度经注入 resolver，保持唯一事实源）· 完整规则能力配 `cmx-rulesengine` · 不引第二事实源、不破 M5.x 现有行为。

---

## 附一、里程碑代号速查

| 系列 | 交付内容 | 状态 |
|---|---|---|
| **M1–M5.3** | 核心内核：定义/网关/多实例/定时器/子流程（+组织路由+多挂载） | ✅ |
| **A1–A10** | 包容网关·终止·决策表·消息·扁平子过程·活动历史·外部Worker/运行时干预·错误边界·实例迁移·事件网关 | ✅ |
| **P0–P6** | 关系型办理人+内置身份·异步Job(SKIP-LOCKED)·死信队列·消息订阅持久化·令牌可视化(前端)·退回 | ✅（P4 前端） |
| **H2** | incident 机制（失败可见 + retry_incident） | ✅ |
| **RD0–RD4** | 子流程路由维度泛化（任意字典维度、三级解析、多挂载、设计器） | ✅ |
| **S0–S6** | 独立微服务：迁移→适配器→多租户→headless→前端抽核→组件→平台反代 | ✅ |
| **表单 F1–F5** | biz_link 单据↔实例 · formKey 绑定 · 审批意见 · startForm + 表单注册表 | ✅（F5 文档） |
| **增强 ②③④⑤** | 逐元素动态派人 · 退回任意节点 · 取回(严格/宽松) · 变量声明(默认值+软校验) | ✅ |
| **0821 后端** | 决策表落库持久化 · 变量历史+TTL/归档 · RD5 HTTP维度解析 · 外部 worker SDK · 身份/维度回连端点 | ✅（真机绿） |
| **0822 大前端** | 设计器模拟试跑 · 版本 diff（结构级） · 协同 M1（感知+防冲突） | ✅（真机绿） |

**注入扩展点**：`JavaDelegate`（M1）· `Clock`（M2.5）· `AssigneeResolver`（M4）· `SubflowRouter`（M5.2/RD0）+ **`DimensionResolver`（RD5，可选）**。

---

## 附二、Crate 清单

| Crate | 层 | 职责 | ~LOC |
|---|---|---|---|
| `cmx-flow-model` | 模型/IR | 语义中立内核：IR + 运行态 + `RuntimeStore` trait + 条件求值 | 4,425 |
| `cmx-flow-bpmn` | 编译器 | BPMN 2.0 XML → 中立 IR，`compile(xml)`，不支持元素显式报错 | 1,507 |
| `cmx-flow-engine` | 引擎核 | 令牌执行内核 `Engine<S>`，等待态即提交点，Delegate 注册表 | 4,930 |
| `cmx-flow-def` | 定义/设计 | 定义持久化：BPMN XML + 版本 + 草稿/发布 + **草稿保存乐观锁（`SaveDraftOutcome`）** | 887 |
| `cmx-flow-store-pg` | 持久化 | PG `RuntimeStore` + 幂等 DDL + 装配者解析 + 子流程路由 + 决策表/变量历史存储（`decision_store`/`var_history`） | 3,034 |
| `cmx-flow-adapters` | 适配器 | 三注入 trait 的 HTTP/Mock 实现 + webhook + `HttpDimensionResolver`（RD5） | 1,005 |
| `cmx-flow-identity` | 身份 | 内置 `fid_*` 身份（独立部署免外部 IAM） | 533 |
| `cmx-flow-app` | 应用核 | 一芯：引擎单例 + 全 handler + `flow_routes::<S>` + 信封 + 身份/维度回连端点 + **模拟 `simulate` + 协同 `collab`（本轮）** | 7,422 |
| `cmx-flow-server` | 独立壳 | 独立 HTTP server bin（`:8091`），装配于 `cmx-web-chassis` | 248 |
| `cmx-flow-worker-sdk` | 客户端 | 外部 job worker 客户端 SDK（纯 HTTP，零引擎依赖） | 256 |
| `cmx-flow-demo` | 演示 | 自包含 axum demo（`:8090`），内嵌前端 + 样例定义 | 1,109 |
| `cmx-flow-tests` | 测试 | M1→P6 集成/E2E 载体（47 集成文件 ~9k LOC） | 8,959（tests） |

---

<sub>本文档图表为自包含 base64 SVG（浅色卡面，浅/深色渲染器均可读），源图与生成脚本在 `docs/summary/assets/`。数据取自 v0.1.12 源码与 `docs/full-test/` 测试报告实测口径。</sub>
