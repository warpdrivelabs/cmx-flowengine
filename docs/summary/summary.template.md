# cmx-flowengine · 阶段性总结

> **独立 BPMN 2.0 流程引擎微服务** · v0.1.12 · 截至 2026-08-23
> 一芯多壳 · 审批赛道为先 · 全链路真机验证 · 与平台编译期解耦

---

## 摘要（TL;DR）

`cmx-flowengine` 是从 `cmx-container` 抽出的**独立流程引擎微服务**：一个语义中立的令牌执行内核（`cmx-flow-model` → `-bpmn` → `-engine`），外裹持久化/适配器/身份/定义四注入层，再由平台中立应用核 `cmx-flow-app` 统一装配成路由；对外经**三种壳**交付——独立 server（`:8091`）、门户反代内嵌、可嵌 Web Component。

- **规模**：**12 个 crate**（+ 外部 worker SDK）、约 **24.5k** 域代码 + **8.6k** 测试代码；edition 2024、工具链 1.97.1。
- **能力**：BPMN 审批核心构件**全覆盖**（任务/网关/事件/多实例/子流程），人工审批全链路（会签·或签·转签·加签·委派·抄送·退回任意节点·取回·7 类办理人·身份双模），子流程**维度路由**（任意字典维度、三级解析、多挂载），异步可靠性（SKIP-LOCKED 执行器·死信队列·incident 重试·错误边界），实例迁移、活动历史、事件网关、事件子流程。
- **本轮增量（0823 · 路线图 Next 四项落地 + 设计器主题化）**：**变量历史引擎派生捕获**（决策输出 / 子流程回填，此前不可见）· **决策表 / 变量历史前端可视化**（新只读查看器 native page + 时间线）· **协同 M2**（op-log 对象级 LWW 实时合并）· **令牌可视化优化**（SSE 实时 + 计数徽标 + 等待态细分色）；外加**设计器前端体验与主题化**——流程列表分页、大纲面板、缩略图、办理人类型修复、**科技感 light/dark 主题**（跟随门户 SAP 令牌，画布也全暗）。详见 §六。
- **上一轮（0822 · 设计器大前端三件套）**：设计器**模拟试跑**（facts→trace + 画布高亮）· **版本 diff**（结构级 XML 对比）· **协同 M1**（感知 + 防冲突）。详见 §七。
- **质量**：`cargo test --workspace` **276 测试函数 · 0 失败**；后端全量回归 **159/159**、子流程 **60/60**、维度路由 **25/25**、子流程钻入 **23/23**、差旅业务 E2E **22/22**；**本轮 0823**：变量历史引擎派生 `var_history_derived.rs` **3/3**、决策/变量历史可视化 **8/8**、协同 M2 对象级合并 **5/5**；**上轮 0822**：设计器模拟+diff **12/12**、新功能验收 **10/10**、协同 M1 双用户 **6/6**；FE 套件自包含只依赖 `:8091`。数据只增不删、全程留库。
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
| **后端补齐** | 0821 | 决策表落库持久化 · 变量历史+TTL/归档 · RD5 HTTP维度解析 · 外部 worker SDK · 身份/维度回连端点 |
| **大前端** | 0822 | 设计器模拟（facts→trace+画布高亮） · 版本 diff（结构级 XML 对比） · 协同 M1（感知+防冲突）（详见 §七） |
| **本轮增量** | 0823 | 变量历史引擎派生捕获 · 决策表/变量历史前端可视化 · 协同 M2（op-log 对象级） · 令牌可视化优化 · 设计器分页/大纲/缩略图/light-dark 主题化（详见 §六） |

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

- **Rust**：`cargo test --workspace` **276 测试函数 · 0 失败**（1 组 PG 集成需活库，标 ignore；166 集成 / 44 文件）；集成用例在 `cmx-flow-tests`（命名即里程碑：`m1..m5` / `a1..a10` / `p0..p6` / `h2` / `withdraw` / `reject_targets` / **`var_history_derived`**…）+ worker-sdk 单测/example。
- **后端 curl 回归**：`run-all.sh` **159/159**、`run-subflow.sh` **60/60**、维度路由 **25/25**、差旅业务 E2E **22/22**、新功能真机 E2E **18/18**（A7/A8/A9/P1/A3）。
- **本轮 0823 前端（Playwright/CDP）**：变量历史引擎派生 `var_history_derived.rs` **3/3**（CapturingStore 截 `record_var_changes`）、决策/变量历史可视化 `viz_decision_varhistory.cjs` **8/8**、协同 M2 对象级合并 `collab_m2_oplog.cjs` **5/5**；设计器**主题化 / 分页 / 滚动**经 targeted Playwright 验收——注入真 sap_horizon/_dark 令牌，三区 + 画布 + 运行时切换 **14/14**，分页翻页边界 + 点选保持滚动位置全绿。
- **上轮 0822 前端**：设计器**模拟+diff** `designer_simulate_diff.cjs` **12/12**、新功能可视化验收 `designer_features_capture.cjs` **10/10**、**协同 M1 双用户真机** `collab_presence.cjs` **6/6**（两 browser context = 两用户共享同一草稿）；加既有子流程钻入 **23/23**、designer/todo/rd4 等——经共享 `_harness.cjs` **自包含只依赖 `flow-server :8091`**（不再要门户 :8080）。
- **纪律**：测试数据**只增不删、全程留库**；所有测试脚本与报告归档在 `docs/full-test/`、`docs/biz-test/`。

---

## 六、本轮增量（0823）

两条线：**① 路线图「Next」四项全部落地**（引擎派生变量历史 · 决策表/变量历史前端可视化 · 协同 M2 · 令牌可视化优化）+ **② 设计器前端体验与主题化**。前端三 native page（`design-workbench` / `ops-console` / 新 `decision-viewer`）均 `web/core` 与 `web/ui-native/flow` **字节一致双份**；沿用「未提交待评审」惯例。**契约提示**：改引擎/store 须 **rebuild + 重启 flow-server** 才生效（旧 binary 缺 `GET /decisions/{key}` 返 405）。

### ① 路线图「Next」四项落地

| # | 项 | 交付 | 真机验证 |
|---|---|---|---|
| ① | **变量历史·引擎派生捕获** | 此前 app 层只捕获调用方送入的 start/complete/set-variables 三路径；引擎**内部** merge（businessRuleTask 决策输出、callActivity 子流程 output 回填）不可见。照抄 A6 活动历史模式：model 加 `VarChangeRecord` + `pending_var_changes`（serde skip）、store `record_var_changes`（**默认 no-op**，仅 PG 落 `cmx_flow_var_history`，`by=system`、source=`decision`/`subflow`）+ `flush_pending_var_changes`（8 处 flush 站点配对，**补 `complete_subflow` 遗漏的 flush** = 修真 bug）；engine `diff_derived_var_changes`（merge 前逐 key diff） | decision→`approvalLevel None→3 by=system node=rule`；subflow→`backVar None→99 by=system node=sub`；`var_history_derived.rs` **3/3**（CapturingStore 包 InMemoryStore 截 `record_var_changes`） |
| ② | **决策表 / 变量历史前端可视化** | 变量历史 → ops-console property 时间线（源徽标 start/complete/manual/**derived** 紫）。决策表 → **新 native page `portal.flow.decision-viewer`**（三区只读：explorer 列表、content「输入蓝｜输出绿」逐行规则 + 命中策略徽标 + 试算命中行琥珀高亮、property 内联试算 facts→evaluate）。后端加 **`GET /decisions/{key}`**（`PgDecisionStore::get`，`DecisionTable` 派生 Serialize 直返） | `viz_decision_varhistory.cjs` **8/8**：决策网格渲染、命中策略徽标、试算命中高亮、变量历史时间线源徽标齐验 |
| ③ | **协同 M2 · op-log 对象级合并** | 续 M1：一端改节点属性 → 广播 → 另一端就地合并。后端 `collab.rs` 加 **`POST /design/op`**（盖 per-(tenant,defKey) 单调 seq 广播）。前端 `applyProp` 拆 `applyPropTo`（本端交互 + 远端回放共用写回），`onRemoteOp` 按 `${elementId}::${prop}`→seq 做**对象级 LWW**（只应用更大 seq），`__applying`/`__loadingDiagram` echo-guard + origin sessionId 忽略自回声。**结构级增删/移动留 M3** | `collab_m2_oplog.cjs` 双向真机 **5/5**：一端改属性 → 另一端合并、旧 seq 不覆盖新值、自回声忽略 |
| ④ | **令牌可视化优化** | P4 只读画布增强（ops-console）：**生命周期 SSE 实时刷新**（`/api/flow/v1/events` EventSource，属当前实例则去抖 reload + 重高亮，图例「实时/手动」脉冲点）+ **节点令牌计数徽标**（bpmn-js overlays，并行/会签多令牌右上角计数）+ **等待态细分色**（subflow 紫 / timer 青 / async 橙 / msg 品红）+ 状态图例 + 适配按钮 | 随 next4 前端套件真机验收；**simplify 复盘顺带修 4 处 latent bug**——`reject_task`/`retry_incident`/`jump_to`/`resume_process` 四路径 `run_to_wait+save` 后**从不 flush** → 丢 A6 活动历史 + P3 订阅，各补 `flush_pending` |

### ② 设计器前端体验与主题化

均改 `web/core/design-workbench.js`（+ `web/ui-native/flow/` 镜像），经 targeted Playwright 真机验收：

| # | 项 | 交付 | 验证 |
|---|---|---|---|
| ① | **流程列表分页** | explorer 定义列表加首页/上页/下页/末页图标条 + 页码；DAM 过滤/刷新回首页；点选流程**保持滚动位置不跳**（宿主/内层滚动容器随重渲染显式复位） | 3 页 24 条：翻页 / 边界禁用 / 过滤回首页 **24/24**；点最后一行不跳到第一行 **8/8** |
| ② | **大纲面板 + 缩略图 + 去水印** | explorer 下部大纲联动画布（节点/网关/边，双向选中，主/子流程通用、可拖高、可折叠）；content 右下缩略图预览（视口方框可拖动平移）；去 bpmn.io 水印 | 大纲双向选中 9 检 / 缩略图平移 6 检 / 主+子流程全绿 |
| ③ | **办理人类型修复** | property 办理人类型此前只能选「指定人员」；根因 `updateProperties({$attrs})` 嵌套 + 类型从写入值反推 → 重写 6 函数为前缀属性名写入 + `assigneeKind` 显式记忆 | 角色/岗位可选可存、往返 `role(cfo)` 正确 |
| ④ | **科技感 + light/dark 主题** | 全 chrome 令牌化（派生自门户 `--sap*`，light/dark 自动翻、零 JS，对齐同族 `todo-center`）；**BPMN 画布也全暗**——diagram-js `.djs-parent` 变量重定义（palette/context-pad/popup）+ 渲染器烘焙的节点描边/填充 `!important` 覆盖 + `data-theme` JS 门闩 + `cmx-portal-theme-change` 监听 | 注入真 sap_horizon/_dark 令牌，三区 + 画布 + 运行时切换 **14/14**；探针确认暗色描边亮(245,246,247)、填充深(29,35,42) |

---

## 七、上一轮增量 · 设计器大前端三件套（0822）

设计器「模拟 / 协同 / diff」三件套，均真机验证（截图确认）；前端 `web/core` 与 `web/ui-native/flow` **字节一致双份**。

| # | 项 | 交付 | 真机验证 |
|---|---|---|---|
| ① | **模拟试跑（simulate）** | 后端 `POST /flow/definitions/simulate`（复用 `compile` + `eval_condition` + `decision::evaluate` + 装配者解析，走 IR 重表达网关三规则，**不建实例无副作用**）；前端 property「模拟」页签：facts 表单 + JSON override → trace 列表 + 画布高亮 | 大额 `{amount:30000}` → `f_big` 分支、办理人含 **director**；小额 `{amount:5000}` → `f_small`、**director 缺席**——排他网关分支正确；切走页签高亮自清 |
| ② | **版本 diff** | **纯前端零后端**：property「差异」页签选从/到版本（草稿=当前画布 `getXml`）→ `DOMParser`（仅 BPMN_MODEL_NS，排除 DI 布局层）→ 按 element id 结构 diff（added/removed/modified + changedFields）→ 列表 + 画布高亮（新增绿 / 修改琥珀）+ 点行定位 | `dd_diff_demo` v1→v2「4 处差异 · +2 新增 · ~2 修改」；同版本守卫报错；退出清理 |
| ③ | **协同 M1 · 感知 + 防冲突** | `crates/cmx-flow-app/src/collab.rs`：进程内 `broadcast` + 自由 JSON payload，SSE 按 `(tenant, defKey)` 过滤；内存 presence（`Mutex<HashMap>` + sweep-on-build TTL=25s）；`updated_at` 当 etag 乐观锁（`SaveDraftOutcome::Conflict`，免 DDL）。前端 presence 头像条 / 远端选中高亮 / 冲突确认框 / 草稿更新通知 | 双用户（两 browser context）共享草稿：各见 2 头像、A 选中→B 远端高亮、A 保存→B 收更新通知、B 过期 base 保存→冲突确认框；无 pageerror。**M1 只做感知+防冲突，模型级实时合并 = 本轮 0823 协同 M2** |

> **更早一轮 · 0821 后端 5 项**（已并入里程碑表 / 时间线）：决策表**落库持久化** · 变量**历史 + TTL/归档** · **RD5 HTTP 维度解析器**（`DimensionResolver` 注入，无 PG org 行仍经 HTTP 解析祖先）· 外部 **job worker SDK**（新 crate，零引擎依赖）· 平台**身份/维度回连端点**（`POST /identity/resolve` + `GET /dimensions/ancestors`，返裸 JSON）——均真机绿、`run-all` **159/159** 零回归。

---

## 八、未来计划

![未来计划 · Now / Next / Later](@@fig-6-roadmap@@)

- **Now（已巩固）**：审批全链路 · 子流程维度路由 RD0–5 · 异步可靠性（incident/死信/SKIP-LOCKED）· S0–S6 独立微服务 + 平台反代 · 四区设计器 + 子流程钻入 + **light/dark 主题化** · 一芯多壳前端 · 0821 后端 5 项 · 0822 设计器大前端（模拟 / diff / 协同 M1）· **本轮 0823：变量历史引擎派生捕获 · 决策/变量历史前端可视化 · 协同 M2（op-log 对象级实时合并）· 令牌可视化优化**。
- **Next（近期择机）**：**协同 M3（结构级增删/移动合并）** · 决策表**可编辑**设计器（现为只读查看器）· 令牌可视化 · 生命周期回放 · SSE **JWT 鉴权**（协同非 `off` 模式，解 EventSource 不能带 header）。
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
| **0823 本轮** | 变量历史引擎派生捕获 · 决策/变量历史前端可视化 · 协同 M2（op-log 对象级合并） · 令牌可视化优化 · 设计器分页/大纲/缩略图/light-dark 主题化 | ✅（真机绿） |

**注入扩展点**：`JavaDelegate`（M1）· `Clock`（M2.5）· `AssigneeResolver`（M4）· `SubflowRouter`（M5.2/RD0）+ **`DimensionResolver`（RD5，可选）**。

---

## 附二、Crate 清单

| Crate | 层 | 职责 | ~LOC |
|---|---|---|---|
| `cmx-flow-model` | 模型/IR | 语义中立内核：IR + 运行态 + `RuntimeStore` trait + 条件求值 + `VarChangeRecord`（变量历史） | 4,474 |
| `cmx-flow-bpmn` | 编译器 | BPMN 2.0 XML → 中立 IR，`compile(xml)`，不支持元素显式报错 | 1,507 |
| `cmx-flow-engine` | 引擎核 | 令牌执行内核 `Engine<S>`，等待态即提交点，Delegate 注册表 + 变量历史派生捕获（`diff_derived_var_changes`/`flush_pending`） | 5,016 |
| `cmx-flow-def` | 定义/设计 | 定义持久化：BPMN XML + 版本 + 草稿/发布 + **草稿保存乐观锁（`SaveDraftOutcome`）** | 887 |
| `cmx-flow-store-pg` | 持久化 | PG `RuntimeStore` + 幂等 DDL + 装配者解析 + 子流程路由 + 决策表/变量历史存储（`decision_store`/`var_history` + 派生 `record_var_changes`） | 3,081 |
| `cmx-flow-adapters` | 适配器 | 三注入 trait 的 HTTP/Mock 实现 + webhook + `HttpDimensionResolver`（RD5） | 1,005 |
| `cmx-flow-identity` | 身份 | 内置 `fid_*` 身份（独立部署免外部 IAM） | 533 |
| `cmx-flow-app` | 应用核 | 一芯：引擎单例 + 全 handler + `flow_routes::<S>` + 信封 + 身份/维度回连 + 模拟 `simulate` + 协同 `collab`（M1/M2）+ **决策查看 `GET /decisions/{key}`** | 7,508 |
| `cmx-flow-server` | 独立壳 | 独立 HTTP server bin（`:8091`），装配于 `cmx-web-chassis` | 248 |
| `cmx-flow-worker-sdk` | 客户端 | 外部 job worker 客户端 SDK（纯 HTTP，零引擎依赖） | 256 |
| `cmx-flow-demo` | 演示 | 自包含 axum demo（`:8090`），内嵌前端 + 样例定义 | 1,109 |
| `cmx-flow-tests` | 测试 | M1→P6 集成/E2E 载体（44 集成文件、166 集成用例 ~8.6k LOC） | 8,554（tests） |

---

<sub>本文档图表为自包含 base64 SVG（浅色卡面，浅/深色渲染器均可读），源图与生成脚本在 `docs/summary/assets/`。数据取自 v0.1.12 源码与 `docs/full-test/` 测试报告实测口径。</sub>
