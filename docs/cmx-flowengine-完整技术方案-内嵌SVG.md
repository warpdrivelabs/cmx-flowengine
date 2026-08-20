# cmx-flowengine 流程引擎 · 完整技术方案

> **版本** v0.1.12 · **文档日期** 2026-08-19 · **定位** 面向人本审批场景的轻量级 BPMN 子集流程引擎
>
> 语言 Rust（edition 2024）· 约 27,000 行源码 · 11 个 crate · 120+ 集成测试 · 一芯多壳可独立部署可平台内嵌
>
> 本文基于源码逐项核对生成，功能/性能描述均以代码为准（不含未实现能力的过度承诺）。

---

## 目录

1. [产品定位与设计铁律](#一产品定位与设计铁律)
2. [总体架构：一芯多壳](#二总体架构一芯多壳)
3. [Crate 分层与依赖](#三crate-分层与依赖)
4. [令牌执行模型](#四令牌执行模型)
5. [节点类型全景（13 类）](#五节点类型全景13-类)
6. [能力里程碑 M1–M5](#六能力里程碑-m1m5)
7. [子流程与路由维度泛化](#七子流程与路由维度泛化)
8. [可注入扩展点](#八可注入扩展点)
9. [数据模型与持久化](#九数据模型与持久化)
10. [多租户隔离](#十多租户隔离)
11. [无头契约与集成](#十一无头契约与集成)
12. [性能特性](#十二性能特性)
13. [前端微前端三壳](#十三前端微前端三壳)
14. [API 接口全景](#十四api-接口全景)
15. [部署形态](#十五部署形态)
16. [能力边界与路线](#十六能力边界与路线)

---

## 一、产品定位与设计铁律

cmx-flowengine 是一款用 **Rust** 实现的 **BPMN 2.0 子集**工作流引擎，聚焦**人本审批**赛道（对标 Flowable 的人本层、钉钉/飞书审批引擎），而非通用编排或云原生大规模流程引擎。它的核心价值是：**语义中立的执行内核 + 一芯多壳的部署弹性 + 审批场景的完整人机协同能力**。

### 设计铁律

| 铁律 | 含义 | 体现 |
|------|------|------|
| **内核语义中立** | 执行内核不认识任何数据库、组织、框架 | `cmx-flow-model` 零 DB/cmx 依赖，可 wasm 嵌入 |
| **等待态即提交点** | 每个 BPMN 等待态 = 一个 DB 事务 | `save_snapshot` 单事务，一段推进一次提交 |
| **永不 panic 落 trace** | 任何节点失败挂 Incident 而非崩溃/丢实例 | serviceTask/子流程失败 → 令牌转 Incident，可 retry |
| **BPMN 只是交换格式** | 引擎跑干净 IR，不在运行期解析 XML | `cmx-flow-bpmn` 编译期 XML→IR |
| **扩展点可注入** | 外部能力经 trait 注入，内核不硬编码 | Clock/Delegate/AssigneeResolver/SubflowRouter |
| **向后兼容** | 新能力默认关闭或缺省等价旧行为 | 多租户默认 single、维度默认 org |

### 定位坐标



<svg viewBox="0 0 760 300" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="760" height="300" fill="#fbfcfe"/>
  <!-- axes -->
  <line x1="80" y1="250" x2="720" y2="250" stroke="#9aa5b5" stroke-width="1.5"/>
  <line x1="80" y1="250" x2="80" y2="30" stroke="#9aa5b5" stroke-width="1.5"/>
  <text x="726" y="254" font-size="12" fill="#5a6b7b">通用编排能力</text>
  <text x="60" y="26" font-size="12" fill="#5a6b7b" text-anchor="end">人机协同深度</text>
  <!-- quadrant labels -->
  <text x="200" y="70" font-size="11" fill="#b0bac9">通用 BPMN 引擎</text>
  <text x="530" y="70" font-size="11" fill="#b0bac9">云原生编排 (Temporal/Camunda8)</text>
  <!-- competitors -->
  <circle cx="560" cy="150" r="7" fill="#c9d2e0"/>
  <text x="574" y="154" font-size="11" fill="#5a6b7b">Camunda / Flowable 全栈</text>
  <circle cx="600" cy="210" r="7" fill="#c9d2e0"/>
  <text x="614" y="214" font-size="11" fill="#5a6b7b">Temporal / Argo</text>
  <circle cx="300" cy="215" r="7" fill="#c9d2e0"/>
  <text x="314" y="219" font-size="11" fill="#5a6b7b">钉钉/飞书 审批</text>
  <!-- us -->
  <circle cx="270" cy="105" r="12" fill="#0a6ed1"/>
  <text x="290" y="100" font-size="13" fill="#0a6ed1" font-weight="700">cmx-flowengine</text>
  <text x="290" y="118" font-size="10.5" fill="#5a6b7b">审批深 · 编排轻 · Rust 内核 · 可嵌可独立</text>
  <text x="90" y="285" font-size="10.5" fill="#8b97b3">聚焦象限：人机协同深、通用编排适度——审批赛道的干净轻量引擎</text>
</svg>



---

## 二、总体架构：一芯多壳

「一芯多壳」是 cmx-flowengine 最核心的架构决策：**一个平台中立的应用核 `cmx-flow-app`**，被多个「壳」复用——平台内嵌壳、独立微服务壳、无头契约壳。核心通过泛型路由 `flow_routes::<S>()` 对任意宿主状态 `S` 成立，handler 不绑 `State` 提取器，因此**两个壳复用同一套 handler + 同一张路由表，零业务漂移**。



<svg viewBox="0 0 820 470" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="470" fill="#fbfcfe"/>
  <text x="410" y="28" font-size="15" font-weight="700" fill="#1c2530" text-anchor="middle">一芯多壳（One-Core / Multi-Shell）</text>

  <!-- consumers -->
  <text x="410" y="58" font-size="11" fill="#8b97b3" text-anchor="middle">━━━━━━━━━━ 消费方 ━━━━━━━━━━</text>
  <rect x="60" y="72" width="200" height="40" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="160" y="90" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">平台门户</text>
  <text x="160" y="105" font-size="10" fill="#5a6b7b" text-anchor="middle">内嵌 / 反代</text>
  <rect x="310" y="72" width="200" height="40" rx="8" fill="#eafaf1" stroke="#178a5a"/>
  <text x="410" y="90" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="600">独立部署</text>
  <text x="410" y="105" font-size="10" fill="#5a6b7b" text-anchor="middle">:8091 微服务</text>
  <rect x="560" y="72" width="200" height="40" rx="8" fill="#fdf0f0" stroke="#d1394a"/>
  <text x="660" y="90" font-size="12" fill="#d1394a" text-anchor="middle" font-weight="600">第三方系统</text>
  <text x="660" y="105" font-size="10" fill="#5a6b7b" text-anchor="middle">v1 API + SSE</text>

  <!-- shells -->
  <text x="410" y="146" font-size="11" fill="#8b97b3" text-anchor="middle">━━━━━━━━━━ 壳（Shell） ━━━━━━━━━━</text>
  <rect x="60" y="160" width="200" height="56" rx="8" fill="#fff" stroke="#0a6ed1" stroke-width="1.5"/>
  <text x="160" y="182" font-size="12.5" fill="#1c2530" text-anchor="middle" font-weight="700">壳① cmx-flow-api</text>
  <text x="160" y="200" font-size="10" fill="#5a6b7b" text-anchor="middle">flow_routes::&lt;CmxAppState&gt;()</text>
  <rect x="310" y="160" width="200" height="56" rx="8" fill="#fff" stroke="#178a5a" stroke-width="1.5"/>
  <text x="410" y="182" font-size="12.5" fill="#1c2530" text-anchor="middle" font-weight="700">壳② cmx-flow-server</text>
  <text x="410" y="200" font-size="10" fill="#5a6b7b" text-anchor="middle">flow_routes::&lt;()&gt; + chassis</text>
  <rect x="560" y="160" width="200" height="56" rx="8" fill="#fff" stroke="#d1394a" stroke-width="1.5"/>
  <text x="660" y="182" font-size="12.5" fill="#1c2530" text-anchor="middle" font-weight="700">壳③ 无头契约</text>
  <text x="660" y="200" font-size="10" fill="#5a6b7b" text-anchor="middle">/v1 + SSE + OpenAPI</text>

  <!-- arrows to core -->
  <path d="M160 216 L160 250 L400 250" fill="none" stroke="#9aa5b5" stroke-width="1.3"/>
  <path d="M410 216 L410 250" fill="none" stroke="#9aa5b5" stroke-width="1.3"/>
  <path d="M660 216 L660 250 L420 250" fill="none" stroke="#9aa5b5" stroke-width="1.3"/>

  <!-- core -->
  <rect x="210" y="262" width="400" height="60" rx="10" fill="#0a6ed1"/>
  <text x="410" y="288" font-size="15" fill="#fff" text-anchor="middle" font-weight="700">一芯 · cmx-flow-app</text>
  <text x="410" y="308" font-size="10.5" fill="#dceafd" text-anchor="middle">引擎单例 + 67 handler + flow_routes::&lt;S&gt; + biz-link + 响应信封</text>

  <!-- engine core -->
  <text x="410" y="352" font-size="11" fill="#8b97b3" text-anchor="middle">━━━━━━━━━━ 执行内核 ━━━━━━━━━━</text>
  <rect x="150" y="366" width="230" height="44" rx="8" fill="#eef3fb" stroke="#5a6b7b"/>
  <text x="265" y="384" font-size="12" fill="#1c2530" text-anchor="middle" font-weight="600">cmx-flow-engine</text>
  <text x="265" y="400" font-size="9.5" fill="#5a6b7b" text-anchor="middle">令牌内核 · 泛型 RuntimeStore · 零 DB</text>
  <rect x="440" y="366" width="230" height="44" rx="8" fill="#eef3fb" stroke="#5a6b7b"/>
  <text x="555" y="384" font-size="12" fill="#1c2530" text-anchor="middle" font-weight="600">cmx-flow-model</text>
  <text x="555" y="400" font-size="9.5" fill="#5a6b7b" text-anchor="middle">IR + 运行态 DTO · 可 wasm 嵌入</text>

  <text x="410" y="440" font-size="10.5" fill="#8b97b3" text-anchor="middle">同一 handler / 同一路由表 / 同一引擎内核 —— 三壳零业务漂移</text>
  <text x="410" y="458" font-size="10" fill="#b0bac9" text-anchor="middle">壳只负责「装配 + 鉴权 + 状态类型」，业务逻辑 100% 在核</text>
</svg>



**三种部署姿态**：
- **① 平台内嵌**：`cmx-flow-api` 作为 cmx-container 平台的一个模块，`flow_routes::<CmxAppState>()` 挂进平台路由，与平台共享进程/鉴权。
- **② 独立微服务**：`cmx-flow-server` 独立进程（:8091），经 `cmx-web-chassis` 装配，零平台依赖；平台经反向代理壳 `FlowProxyModule` 接回（配置 `[center_client.urls].flow` 决定内嵌还是反代）。
- **③ 完全无头**：第三方直接消费 `/api/flow/v1/*` + SSE 事件流 + OpenAPI，自建 UI。

---

## 三、Crate 分层与依赖

11 个 crate 严格分层，依赖单向向下。内核层（model/engine）**零基础设施依赖**，可 wasm 嵌入；持久化/适配层实现内核定义的 trait；应用层装配 handler；壳层只做部署装配。



<svg viewBox="0 0 820 520" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="520" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">Crate 分层依赖图（依赖向下）</text>

  <!-- Layer: shells -->
  <rect x="30" y="44" width="760" height="66" rx="8" fill="#fdf0f0" opacity="0.5"/>
  <text x="40" y="62" font-size="10.5" fill="#d1394a" font-weight="600">壳层 · 部署装配</text>
  <rect x="60" y="70" width="150" height="34" rx="6" fill="#fff" stroke="#d1394a"/>
  <text x="135" y="91" font-size="11" fill="#1c2530" text-anchor="middle">cmx-flow-server</text>
  <rect x="230" y="70" width="150" height="34" rx="6" fill="#fff" stroke="#d1394a"/>
  <text x="305" y="91" font-size="11" fill="#1c2530" text-anchor="middle">cmx-flow-demo</text>
  <rect x="400" y="70" width="180" height="34" rx="6" fill="#fff" stroke="#d1394a" stroke-dasharray="4 3"/>
  <text x="490" y="91" font-size="10.5" fill="#5a6b7b" text-anchor="middle">cmx-flow-api（cmx-container）</text>
  <rect x="600" y="70" width="150" height="34" rx="6" fill="#fff" stroke="#5a6b7b" stroke-dasharray="4 3"/>
  <text x="675" y="91" font-size="10.5" fill="#5a6b7b" text-anchor="middle">cmx-web-chassis</text>

  <!-- Layer: app -->
  <rect x="30" y="122" width="760" height="60" rx="8" fill="#eaf1fb" opacity="0.6"/>
  <text x="40" y="140" font-size="10.5" fill="#0a6ed1" font-weight="600">应用层 · 一芯</text>
  <rect x="300" y="146" width="220" height="30" rx="6" fill="#0a6ed1"/>
  <text x="410" y="166" font-size="12" fill="#fff" text-anchor="middle" font-weight="700">cmx-flow-app（handler + 路由 + 装配）</text>

  <!-- Layer: adapters/persistence -->
  <rect x="30" y="194" width="760" height="66" rx="8" fill="#fff8ec" opacity="0.7"/>
  <text x="40" y="212" font-size="10.5" fill="#c26a00" font-weight="600">适配 / 持久化层 · 实现内核 trait</text>
  <rect x="55" y="220" width="150" height="34" rx="6" fill="#fff" stroke="#c26a00"/>
  <text x="130" y="241" font-size="10.5" fill="#1c2530" text-anchor="middle">cmx-flow-store-pg</text>
  <rect x="215" y="220" width="150" height="34" rx="6" fill="#fff" stroke="#c26a00"/>
  <text x="290" y="241" font-size="10.5" fill="#1c2530" text-anchor="middle">cmx-flow-adapters</text>
  <rect x="375" y="220" width="140" height="34" rx="6" fill="#fff" stroke="#c26a00"/>
  <text x="445" y="241" font-size="10.5" fill="#1c2530" text-anchor="middle">cmx-flow-def</text>
  <rect x="525" y="220" width="150" height="34" rx="6" fill="#fff" stroke="#c26a00"/>
  <text x="600" y="241" font-size="10.5" fill="#1c2530" text-anchor="middle">cmx-flow-identity</text>

  <!-- Layer: bpmn -->
  <rect x="30" y="272" width="760" height="56" rx="8" fill="#f0f5f0" opacity="0.7"/>
  <text x="40" y="290" font-size="10.5" fill="#178a5a" font-weight="600">编译层</text>
  <rect x="320" y="292" width="180" height="30" rx="6" fill="#fff" stroke="#178a5a"/>
  <text x="410" y="312" font-size="11" fill="#1c2530" text-anchor="middle">cmx-flow-bpmn（XML→IR）</text>

  <!-- Layer: core -->
  <rect x="30" y="340" width="760" height="76" rx="8" fill="#eef3fb"/>
  <text x="40" y="358" font-size="10.5" fill="#1c2530" font-weight="600">内核层 · 语义中立 · 零基础设施依赖 · 可 wasm</text>
  <rect x="200" y="366" width="180" height="40" rx="6" fill="#0a6ed1"/>
  <text x="290" y="384" font-size="12" fill="#fff" text-anchor="middle" font-weight="700">cmx-flow-engine</text>
  <text x="290" y="399" font-size="9" fill="#dceafd" text-anchor="middle">令牌执行内核</text>
  <rect x="440" y="366" width="180" height="40" rx="6" fill="#093f7a"/>
  <text x="530" y="384" font-size="12" fill="#fff" text-anchor="middle" font-weight="700">cmx-flow-model</text>
  <text x="530" y="399" font-size="9" fill="#dceafd" text-anchor="middle">IR + DTO + 契约 trait</text>

  <!-- infra -->
  <rect x="30" y="428" width="760" height="60" rx="8" fill="#f4f6f9"/>
  <text x="40" y="446" font-size="10.5" fill="#5a6b7b" font-weight="600">基础设施（跨 workspace 复用 cmx-container）</text>
  <rect x="90" y="452" width="150" height="30" rx="6" fill="#fff" stroke="#9aa5b5"/>
  <text x="165" y="472" font-size="10" fill="#5a6b7b" text-anchor="middle">cmx-database-pg</text>
  <rect x="260" y="452" width="130" height="30" rx="6" fill="#fff" stroke="#9aa5b5"/>
  <text x="325" y="472" font-size="10" fill="#5a6b7b" text-anchor="middle">cmx-core</text>
  <rect x="410" y="452" width="160" height="30" rx="6" fill="#fff" stroke="#9aa5b5"/>
  <text x="490" y="472" font-size="10" fill="#5a6b7b" text-anchor="middle">cmx-web-monitor</text>
  <rect x="590" y="452" width="160" height="30" rx="6" fill="#fff" stroke="#9aa5b5"/>
  <text x="670" y="472" font-size="10" fill="#5a6b7b" text-anchor="middle">cmx-service-base</text>

  <!-- flow arrows -->
  <path d="M410 176 L410 194" fill="none" stroke="#9aa5b5" stroke-width="1.2" marker-end="url(#ad)"/>
  <path d="M410 254 L410 272" fill="none" stroke="#9aa5b5" stroke-width="1.2" marker-end="url(#ad)"/>
  <path d="M410 322 L410 340" fill="none" stroke="#9aa5b5" stroke-width="1.2" marker-end="url(#ad)"/>
  <defs><marker id="ad" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
</svg>



| Crate | 职责 | 关键约束 |
|-------|------|---------|
| **cmx-flow-model** | 语义中立内核：IR（arena+索引）、运行态 DTO、变量、受控条件表达式引擎、决策表、`RuntimeStore` 契约 trait | **零 cmx/DB 依赖，可 wasm** |
| **cmx-flow-bpmn** | BPMN 2.0 XML → IR 编译器（roxmltree 零拷贝 DOM） | 命名空间无关，不支持元素显式报错 |
| **cmx-flow-engine** | 令牌执行内核；不变式「等待态=提交点」；泛型于 `RuntimeStore` | 只依赖 model 的中立契约 |
| **cmx-flow-def** | 流程定义持久化（草稿/发布/版本/装载） | 两表：definition + version |
| **cmx-flow-store-pg** | PostgreSQL `RuntimeStore` 实现 + PG 版解析器/路由器 | 聚合快照单事务读写 |
| **cmx-flow-adapters** | 外部服务适配（S1）：三注入 trait 的 Http+Mock + webhook 发送器 | 只依赖引擎 trait 层 |
| **cmx-flow-identity** | 可选内建身份模块（`fid_*` 表），`FLOW_IDENTITY_MODE=local` 才激活 | 绝不冒充平台 IAM |
| **cmx-flow-app** | 一芯：引擎单例 + 全 handler + `flow_routes::<S>()` + biz-link | 不依赖 cmx-api |
| **cmx-flow-server** | 独立 bin 壳（:8091），填一份 ServiceSpec | 零平台依赖 |
| **cmx-flow-demo** | 自包含 axum 演示（:8090），内联 SPA | — |
| **cmx-flow-tests** | E2E 测试；默认内存态，PG 测试 `#[ignore]` 门控 | 120+ 测试函数 |

---

## 四、令牌执行模型

引擎的核心抽象是 **令牌（Token）**——BPMN 语义里流经流程图的执行指针。令牌持 `node_bpmn_id`（稳定 BPMN id 锚点，非内存索引，故跨进程重启安全）+ 状态。执行主循环 `run_to_wait` 反复取第一个 `Active` 令牌推进其节点，直到再无活动令牌——**每个等待态恰好对应一次 DB 事务提交**。

### 4.1 一段推进的生命周期



<svg viewBox="0 0 820 320" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="320" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">一段推进（Run Segment）= 一次原子提交</text>

  <rect x="40" y="60" width="150" height="52" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="115" y="82" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">① 装载快照</text>
  <text x="115" y="99" font-size="9.5" fill="#5a6b7b" text-anchor="middle">load_snapshot</text>

  <rect x="230" y="60" width="180" height="52" rx="8" fill="#eef8f2" stroke="#178a5a"/>
  <text x="320" y="80" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="600">② 内存推进</text>
  <text x="320" y="96" font-size="9.5" fill="#5a6b7b" text-anchor="middle">run_to_wait 循环取 Active 令牌</text>
  <text x="320" y="108" font-size="9" fill="#8b97b3" text-anchor="middle">STEP_LIMIT=10000 防死循环</text>

  <rect x="450" y="60" width="150" height="52" rx="8" fill="#fff8ec" stroke="#c26a00"/>
  <text x="525" y="82" font-size="12" fill="#c26a00" text-anchor="middle" font-weight="600">③ 落到等待态</text>
  <text x="525" y="99" font-size="9.5" fill="#5a6b7b" text-anchor="middle">userTask/callActivity</text>

  <rect x="640" y="60" width="140" height="52" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="710" y="82" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">④ 单事务落库</text>
  <text x="710" y="99" font-size="9.5" fill="#5a6b7b" text-anchor="middle">save_snapshot</text>

  <path d="M190 86 L228 86" stroke="#9aa5b5" stroke-width="1.3" marker-end="url(#a2)"/>
  <path d="M410 86 L448 86" stroke="#9aa5b5" stroke-width="1.3" marker-end="url(#a2)"/>
  <path d="M600 86 L638 86" stroke="#9aa5b5" stroke-width="1.3" marker-end="url(#a2)"/>
  <defs><marker id="a2" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>

  <!-- snapshot content -->
  <rect x="120" y="150" width="580" height="120" rx="10" fill="#fff" stroke="#c9ced4"/>
  <text x="410" y="172" font-size="12" fill="#1c2530" text-anchor="middle" font-weight="700">InstanceSnapshot（原子持久化单元 = 聚合根）</text>
  <rect x="140" y="186" width="120" height="30" rx="5" fill="#eef3fb"/><text x="200" y="206" font-size="10" fill="#1c2530" text-anchor="middle">instance</text>
  <rect x="270" y="186" width="120" height="30" rx="5" fill="#eef3fb"/><text x="330" y="206" font-size="10" fill="#1c2530" text-anchor="middle">tokens[]</text>
  <rect x="400" y="186" width="120" height="30" rx="5" fill="#eef3fb"/><text x="460" y="206" font-size="10" fill="#1c2530" text-anchor="middle">tasks[]</text>
  <rect x="530" y="186" width="150" height="30" rx="5" fill="#eef3fb"/><text x="605" y="206" font-size="10" fill="#1c2530" text-anchor="middle">mi_scopes[] · jobs[]</text>
  <rect x="140" y="224" width="160" height="30" rx="5" fill="#eef3fb"/><text x="220" y="244" font-size="10" fill="#1c2530" text-anchor="middle">candidates[]</text>
  <rect x="310" y="224" width="160" height="30" rx="5" fill="#eef3fb"/><text x="390" y="244" font-size="10" fill="#1c2530" text-anchor="middle">cc_records[]</text>
  <rect x="480" y="224" width="200" height="30" rx="5" fill="#eef3fb"/><text x="580" y="244" font-size="10" fill="#1c2530" text-anchor="middle">delegations[]</text>
  <text x="410" y="292" font-size="10" fill="#8b97b3" text-anchor="middle">重启恢复 = 从 PG 重建聚合（MI 计数 / 定时器到期 / 父子唤醒全部可恢复）——可靠性试金石</text>
</svg>



**IR 设计要点**：arena `Vec<FlowNode>` + `NodeId(usize)` 索引（无 `Rc<RefCell>` 图）；`NodeKind` 枚举分派（非继承树）；`bpmn_id` 作稳定持久化锚点。

### 4.2 令牌状态机



<svg viewBox="0 0 820 300" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="300" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">TokenState 状态机</text>

  <!-- Active center -->
  <circle cx="410" cy="150" r="42" fill="#0a6ed1"/>
  <text x="410" y="148" font-size="13" fill="#fff" text-anchor="middle" font-weight="700">Active</text>
  <text x="410" y="164" font-size="9" fill="#dceafd" text-anchor="middle">推进中</text>

  <!-- Waiting -->
  <circle cx="180" cy="80" r="36" fill="#fff8ec" stroke="#c26a00" stroke-width="1.5"/>
  <text x="180" y="78" font-size="11" fill="#c26a00" text-anchor="middle" font-weight="600">Waiting</text>
  <text x="180" y="93" font-size="8.5" fill="#8b97b3" text-anchor="middle">userTask</text>

  <!-- Joining -->
  <circle cx="180" cy="220" r="36" fill="#eef8f2" stroke="#178a5a" stroke-width="1.5"/>
  <text x="180" y="218" font-size="11" fill="#178a5a" text-anchor="middle" font-weight="600">Joining</text>
  <text x="180" y="233" font-size="8.5" fill="#8b97b3" text-anchor="middle">网关合并</text>

  <!-- WaitingSubflow -->
  <circle cx="640" cy="70" r="40" fill="#f3eefb" stroke="#7d4fd1" stroke-width="1.5"/>
  <text x="640" y="66" font-size="10.5" fill="#7d4fd1" text-anchor="middle" font-weight="600">Waiting</text>
  <text x="640" y="80" font-size="10.5" fill="#7d4fd1" text-anchor="middle" font-weight="600">Subflow</text>
  <text x="640" y="95" font-size="8.5" fill="#8b97b3" text-anchor="middle">callActivity</text>

  <!-- WaitingMessage -->
  <circle cx="640" cy="200" r="40" fill="#eaf6fb" stroke="#0a8ec2" stroke-width="1.5"/>
  <text x="640" y="196" font-size="10.5" fill="#0a8ec2" text-anchor="middle" font-weight="600">Waiting</text>
  <text x="640" y="210" font-size="10.5" fill="#0a8ec2" text-anchor="middle" font-weight="600">Message</text>
  <text x="640" y="225" font-size="8.5" fill="#8b97b3" text-anchor="middle">消息捕获</text>

  <!-- Incident -->
  <circle cx="410" cy="265" r="30" fill="#fdf0f0" stroke="#d1394a" stroke-width="1.5"/>
  <text x="410" y="263" font-size="10.5" fill="#d1394a" text-anchor="middle" font-weight="600">Incident</text>
  <text x="410" y="277" font-size="8" fill="#8b97b3" text-anchor="middle">失败挂起</text>

  <!-- Ended -->
  <circle cx="410" cy="40" r="26" fill="#eceff3" stroke="#5a6b7b" stroke-width="1.5"/>
  <text x="410" y="44" font-size="10.5" fill="#5a6b7b" text-anchor="middle" font-weight="600">Ended</text>

  <!-- arrows -->
  <path d="M375 120 Q280 90 214 88" fill="none" stroke="#c26a00" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M210 100 Q300 140 370 145" fill="none" stroke="#178a5a" stroke-width="1.3" marker-end="url(#a3)" stroke-dasharray="3 2"/>
  <text x="255" y="130" font-size="9" fill="#5a6b7b">complete→</text>
  <path d="M378 130 Q280 200 214 210" fill="none" stroke="#178a5a" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M448 128 Q560 90 604 82" fill="none" stroke="#7d4fd1" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M610 90 Q520 150 448 152" fill="none" stroke="#7d4fd1" stroke-width="1.3" marker-end="url(#a3)" stroke-dasharray="3 2"/>
  <text x="530" y="118" font-size="9" fill="#5a6b7b">子完成→</text>
  <path d="M446 168 Q560 190 604 195" fill="none" stroke="#0a8ec2" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M410 108 L410 66" fill="none" stroke="#5a6b7b" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M410 192 L410 236" fill="none" stroke="#d1394a" stroke-width="1.3" marker-end="url(#a3)"/>
  <path d="M436 250 Q470 190 448 172" fill="none" stroke="#178a5a" stroke-width="1.3" marker-end="url(#a3)" stroke-dasharray="3 2"/>
  <text x="455" y="225" font-size="9" fill="#5a6b7b">retry→</text>
  <defs><marker id="a3" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
  <text x="410" y="295" font-size="9.5" fill="#8b97b3" text-anchor="middle">实例状态 InstanceState：Active / Suspended / Completed / Terminated</text>
</svg>



---

## 五、节点类型全景（13 类）

引擎支持 **13 类 BPMN 节点**，覆盖审批场景的全部编排原语。其中仅 **userTask** 与 **callActivity** 是等待态（落库挂起），其余在一段推进内同步执行完毕。



<svg viewBox="0 0 820 500" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="500" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">13 类节点 · 按语义分组</text>

  <!-- 事件 -->
  <rect x="30" y="44" width="760" height="88" rx="8" fill="#eef8f2" opacity="0.5"/>
  <text x="42" y="62" font-size="11" fill="#178a5a" font-weight="700">事件 Events</text>
  <g font-size="10.5">
    <circle cx="110" cy="98" r="20" fill="#fff" stroke="#178a5a" stroke-width="2"/><text x="110" y="102" text-anchor="middle" fill="#178a5a">开始</text>
    <text x="110" y="128" text-anchor="middle" fill="#5a6b7b" font-size="9">StartEvent</text>
    <circle cx="230" cy="98" r="20" fill="#fff" stroke="#d1394a" stroke-width="3"/><text x="230" y="102" text-anchor="middle" fill="#d1394a">结束</text>
    <text x="230" y="128" text-anchor="middle" fill="#5a6b7b" font-size="9">EndEvent</text>
    <circle cx="360" cy="98" r="20" fill="#fdf0f0" stroke="#d1394a" stroke-width="3"/><text x="360" y="102" text-anchor="middle" fill="#d1394a" font-size="9">终止</text>
    <text x="360" y="128" text-anchor="middle" fill="#5a6b7b" font-size="9">Terminate（一票否决）</text>
    <circle cx="530" cy="98" r="20" fill="#fff" stroke="#c26a00" stroke-width="2" stroke-dasharray="3 2"/><text x="530" y="102" text-anchor="middle" fill="#c26a00" font-size="9">定时</text>
    <text x="530" y="128" text-anchor="middle" fill="#5a6b7b" font-size="9">BoundaryTimer（中断/非中断）</text>
    <circle cx="690" cy="98" r="20" fill="#eaf6fb" stroke="#0a8ec2" stroke-width="2"/><text x="690" y="102" text-anchor="middle" fill="#0a8ec2" font-size="9">消息</text>
    <text x="690" y="128" text-anchor="middle" fill="#5a6b7b" font-size="9">MessageCatch ⏸</text>
  </g>

  <!-- 任务 -->
  <rect x="30" y="144" width="760" height="108" rx="8" fill="#eaf1fb" opacity="0.5"/>
  <text x="42" y="162" font-size="11" fill="#0a6ed1" font-weight="700">任务 Tasks</text>
  <rect x="70" y="176" width="150" height="50" rx="8" fill="#fff8ec" stroke="#c26a00" stroke-width="2"/>
  <text x="145" y="197" font-size="12" fill="#c26a00" text-anchor="middle" font-weight="600">用户任务 ⏸</text>
  <text x="145" y="214" font-size="9" fill="#5a6b7b" text-anchor="middle">UserTask（等待态）</text>
  <rect x="240" y="176" width="150" height="50" rx="8" fill="#fff" stroke="#0a6ed1"/>
  <text x="315" y="197" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">服务任务</text>
  <text x="315" y="214" font-size="9" fill="#5a6b7b" text-anchor="middle">ServiceTask（delegate）</text>
  <rect x="410" y="176" width="150" height="50" rx="8" fill="#fff" stroke="#7d4fd1"/>
  <text x="485" y="197" font-size="12" fill="#7d4fd1" text-anchor="middle" font-weight="600">规则任务</text>
  <text x="485" y="214" font-size="9" fill="#5a6b7b" text-anchor="middle">BusinessRule（决策表）</text>
  <rect x="580" y="176" width="160" height="50" rx="8" fill="#f3eefb" stroke="#7d4fd1" stroke-width="2"/>
  <text x="660" y="197" font-size="12" fill="#7d4fd1" text-anchor="middle" font-weight="600">调用活动 ⏸</text>
  <text x="660" y="214" font-size="9" fill="#5a6b7b" text-anchor="middle">CallActivity（子流程）</text>
  <text x="145" y="244" font-size="9" fill="#178a5a" text-anchor="middle">支持多实例会签/或签 + 边界定时器</text>

  <!-- 网关 -->
  <rect x="30" y="264" width="760" height="104" rx="8" fill="#fff8ec" opacity="0.45"/>
  <text x="42" y="282" font-size="11" fill="#c26a00" font-weight="700">网关 Gateways</text>
  <g>
    <path d="M150 300 l28 28 l-28 28 l-28 -28 z" fill="#fff" stroke="#c26a00" stroke-width="2"/>
    <text x="150" y="332" font-size="14" fill="#c26a00" text-anchor="middle">×</text>
    <text x="150" y="362" font-size="10" fill="#5a6b7b" text-anchor="middle">排他 · 择一</text>
    <path d="M390 300 l28 28 l-28 28 l-28 -28 z" fill="#fff" stroke="#0a6ed1" stroke-width="2"/>
    <text x="390" y="333" font-size="15" fill="#0a6ed1" text-anchor="middle">+</text>
    <text x="390" y="362" font-size="10" fill="#5a6b7b" text-anchor="middle">并行 · 全分裂/全合并</text>
    <path d="M630 300 l28 28 l-28 28 l-28 -28 z" fill="#fff" stroke="#178a5a" stroke-width="2"/>
    <circle cx="630" cy="328" r="10" fill="none" stroke="#178a5a" stroke-width="2"/>
    <text x="630" y="362" font-size="10" fill="#5a6b7b" text-anchor="middle">包容 · 择若干</text>
  </g>

  <!-- 结构 -->
  <rect x="30" y="380" width="760" height="66" rx="8" fill="#f4f6f9"/>
  <text x="42" y="398" font-size="11" fill="#5a6b7b" font-weight="700">结构 Structure</text>
  <rect x="250" y="404" width="300" height="32" rx="8" fill="#fff" stroke="#9aa5b5"/>
  <text x="400" y="424" font-size="11" fill="#1c2530" text-anchor="middle">SubProcess 嵌入子过程（编译期展平进父 arena）</text>

  <text x="410" y="470" font-size="10" fill="#8b97b3" text-anchor="middle">⏸ = 等待态节点（落库挂起，等外部事件）· 编译器对不支持元素显式报错，绝不静默吞掉</text>
  <text x="410" y="488" font-size="9.5" fill="#b0bac9" text-anchor="middle">cmx: 扩展属性：calledKey · dimKey · candidates · cc · message · decision · formKey · varSchema · withdrawPolicy …</text>
</svg>



### 条件表达式引擎

分支条件走**手写受控 DSL**（非 FEEL）：递归下降文法（`||`/`&&`/比较/算术/一元/函数调用/括号），剥离 `${...}`/`#{...}` 包裹，返回 JSON 真值。支持点路径访问（`order.amount`）+ 21 个内置纯函数（LEN/UPPER/CONTAINS/IN/ABS/ROUND/MIN/MAX/COALESCE/IF/NOT…）。**刻意不含时间函数**——时间只经可注入 `Clock` 进入，保确定性可测。空表达式 = true（无条件边）。


---

## 六、能力里程碑 M1–M5

引擎能力沿 M1→M5 逐里程碑演进，每一步都以「审批场景真实诉求」驱动。下图为能力演进时间线。



<svg viewBox="0 0 820 440" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="440" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">能力演进时间线</text>

  <!-- spine -->
  <line x1="70" y1="70" x2="70" y2="400" stroke="#c9d2e0" stroke-width="2"/>

  <!-- M1 -->
  <circle cx="70" cy="90" r="7" fill="#0a6ed1"/>
  <rect x="90" y="72" width="700" height="36" rx="6" fill="#eaf1fb"/>
  <text x="104" y="88" font-size="12" fill="#0a6ed1" font-weight="700">M1 顺序审批 · 内核地基</text>
  <text x="104" y="102" font-size="10" fill="#5a6b7b">令牌内核 · arena/枚举/聚合快照 · 受控表达式 · serviceTask delegate · 排他网关+默认边</text>

  <circle cx="70" cy="138" r="7" fill="#0a6ed1"/>
  <rect x="90" y="120" width="700" height="36" rx="6" fill="#eef8f2"/>
  <text x="104" y="136" font-size="12" fill="#178a5a" font-weight="700">M2 · M2.5 并行网关 · 历史归档 · 边界定时器</text>
  <text x="104" y="150" font-size="10" fill="#5a6b7b">parallelGateway fork/join · RU/HI 分表终态归档 · 中断/非中断定时器 · 可注入时钟 · find_due_jobs</text>

  <circle cx="70" cy="186" r="7" fill="#0a6ed1"/>
  <rect x="90" y="168" width="700" height="36" rx="6" fill="#fff8ec"/>
  <text x="104" y="184" font-size="12" fill="#c26a00" font-weight="700">M3 多实例 会签/或签 · 实例取消</text>
  <text x="104" y="198" font-size="10" fill="#5a6b7b">并行会签(N人齐审) + 顺序或签(逐个) · completionCondition 提前结束 · MiScope 账本 · 取消归档</text>

  <circle cx="70" cy="234" r="7" fill="#0a6ed1"/>
  <rect x="90" y="216" width="700" height="52" rx="6" fill="#f3eefb"/>
  <text x="104" y="232" font-size="12" fill="#7d4fd1" font-weight="700">M4 抄送 · 转签 · 角色岗位（M4.1/4.2/4.3）</text>
  <text x="104" y="246" font-size="10" fill="#5a6b7b">M4.1 角色/岗位/组织/用户候选人 + 候选池认领 · M4.2 抄送只读知会+已读追踪</text>
  <text x="104" y="260" font-size="10" fill="#5a6b7b">M4.3 转办/委派/加签(可嵌套) + 完整转签台账</text>

  <circle cx="70" cy="298" r="7" fill="#0a6ed1"/>
  <rect x="90" y="280" width="700" height="52" rx="6" fill="#eaf6fb"/>
  <text x="104" y="296" font-size="12" fill="#0a8ec2" font-weight="700">M5 子流程（M5.1/5.2/5.3）</text>
  <text x="104" y="310" font-size="10" fill="#5a6b7b">M5.1 callActivity 同步子流程(父子实例/逐层唤醒/变量映射/可嵌套)</text>
  <text x="104" y="324" font-size="10" fill="#5a6b7b">M5.2 按组织路由 · M5.3 一主流程多挂载</text>

  <circle cx="70" cy="362" r="7" fill="#178a5a"/>
  <rect x="90" y="344" width="700" height="52" rx="6" fill="#eef8f2" stroke="#178a5a" stroke-dasharray="4 3"/>
  <text x="104" y="360" font-size="12" fill="#178a5a" font-weight="700">RD0–RD4 路由维度泛化（承 M5）</text>
  <text x="104" y="374" font-size="10" fill="#5a6b7b">子流程路由从「写死组织」泛化为「任意字典」· 同实例不同挂载点可按不同维度路由</text>
  <text x="104" y="388" font-size="10" fill="#5a6b7b">已真机双维度端到端 25/25 绿（组织机构 + 产品）</text>
</svg>



### 6.1 多实例（会签/或签）

把一个逻辑 userTask 展开成多个并发/顺序子任务，用 `cmx_flow_mi_scope` 记账（total/completed/next_index/collection 快照）。

- **会签（parallel）**：一次展开全部元素，N 人齐头并进；`completionCondition` 可提前结束（注入 `nrOfInstances`/`nrOfCompletedInstances`/`nrOfActiveInstances`）。命中条件即作废剩余任务、发一个代表令牌前进。
- **或签（sequential）**：逐个展开，前一个办完再展开下一个。
- **集合驱动**：由 JSON 数组变量驱动展开（非 loopCardinality）；每元素办理人支持三种写法——元素插值 `${product.ownerUser}`、候选表达式插值 `role(${product.ownerRole})`、用户 id 集合。

### 6.2 边界定时器（M2.5）



<svg viewBox="0 0 820 250" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="250" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="13" font-weight="700" fill="#1c2530" text-anchor="middle">中断型 vs 非中断型边界定时器</text>

  <!-- interrupting -->
  <text x="200" y="56" font-size="11" fill="#d1394a" font-weight="600" text-anchor="middle">中断型（催办升级）</text>
  <rect x="110" y="70" width="120" height="46" rx="8" fill="#fff8ec" stroke="#c26a00" stroke-width="2"/>
  <text x="170" y="90" font-size="11" fill="#c26a00" text-anchor="middle">审批任务</text>
  <text x="170" y="105" font-size="9" fill="#8b97b3" text-anchor="middle">userTask</text>
  <circle cx="230" cy="116" r="12" fill="#fff" stroke="#d1394a" stroke-width="2"/>
  <text x="230" y="120" font-size="9" fill="#d1394a" text-anchor="middle">⏰</text>
  <path d="M230 128 L230 165" stroke="#d1394a" stroke-width="1.5" marker-end="url(#a4)"/>
  <rect x="170" y="168" width="120" height="40" rx="8" fill="#fdf0f0" stroke="#d1394a"/>
  <text x="230" y="192" font-size="10.5" fill="#d1394a" text-anchor="middle">升级到上级</text>
  <text x="130" y="192" font-size="9" fill="#8b97b3">原任务作废→</text>

  <!-- non-interrupting -->
  <text x="600" y="56" font-size="11" fill="#0a8ec2" font-weight="600" text-anchor="middle">非中断型（旁路提醒）</text>
  <rect x="510" y="70" width="120" height="46" rx="8" fill="#fff8ec" stroke="#c26a00" stroke-width="2"/>
  <text x="570" y="90" font-size="11" fill="#c26a00" text-anchor="middle">审批任务</text>
  <text x="570" y="105" font-size="9" fill="#8b97b3" text-anchor="middle">继续等待 ✓</text>
  <circle cx="630" cy="116" r="12" fill="#fff" stroke="#0a8ec2" stroke-width="2" stroke-dasharray="3 2"/>
  <text x="630" y="120" font-size="9" fill="#0a8ec2" text-anchor="middle">⏰</text>
  <path d="M642 116 L690 116" stroke="#0a8ec2" stroke-width="1.5" marker-end="url(#a4)" stroke-dasharray="3 2"/>
  <rect x="690" y="96" width="100" height="40" rx="8" fill="#eaf6fb" stroke="#0a8ec2"/>
  <text x="740" y="120" font-size="10.5" fill="#0a8ec2" text-anchor="middle">发催办提醒</text>
  <text x="570" y="145" font-size="9" fill="#8b97b3" text-anchor="middle">原任务不动，旁路发新令牌</text>

  <defs><marker id="a4" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
  <text x="410" y="235" font-size="10" fill="#8b97b3" text-anchor="middle">引擎无后台线程——纯函数，宿主经 trigger_due_timers(limit) 显式驱动；作业随聚合落库，重启不丢</text>
</svg>



### 6.3 人机协同动作全集

审批赛道的完整人工干预能力，每个动作都写 `TaskDelegation` 台账留痕：



<svg viewBox="0 0 820 320" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="320" fill="#fbfcfe"/>
  <text x="410" y="24" font-size="13" font-weight="700" fill="#1c2530" text-anchor="middle">人工干预动作</text>
  <g font-size="10.5">
    <rect x="40" y="44" width="170" height="58" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="125" y="66" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">认领 Claim</text>
    <text x="125" y="84" font-size="9.5" fill="#5a6b7b" text-anchor="middle">候选池 → 指派本人</text>
    <text x="125" y="97" font-size="8.5" fill="#8b97b3" text-anchor="middle">幂等，已被他人认领报错</text>

    <rect x="230" y="44" width="170" height="58" rx="8" fill="#eef8f2" stroke="#178a5a"/>
    <text x="315" y="66" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="600">转办 Transfer</text>
    <text x="315" y="84" font-size="9.5" fill="#5a6b7b" text-anchor="middle">彻底移交</text>
    <text x="315" y="97" font-size="8.5" fill="#8b97b3" text-anchor="middle">assignee+owner 都换人</text>

    <rect x="420" y="44" width="170" height="58" rx="8" fill="#fff8ec" stroke="#c26a00"/>
    <text x="505" y="66" font-size="12" fill="#c26a00" text-anchor="middle" font-weight="600">委派 Delegate</text>
    <text x="505" y="84" font-size="9.5" fill="#5a6b7b" text-anchor="middle">代办不转所有权</text>
    <text x="505" y="97" font-size="8.5" fill="#8b97b3" text-anchor="middle">owner 保留，办完归还</text>

    <rect x="610" y="44" width="170" height="58" rx="8" fill="#f3eefb" stroke="#7d4fd1"/>
    <text x="695" y="66" font-size="12" fill="#7d4fd1" text-anchor="middle" font-weight="600">加签 AddSign</text>
    <text x="695" y="84" font-size="9.5" fill="#5a6b7b" text-anchor="middle">临时增审批人</text>
    <text x="695" y="97" font-size="8.5" fill="#8b97b3" text-anchor="middle">前/后加签，可嵌套</text>

    <rect x="40" y="116" width="170" height="58" rx="8" fill="#fdf0f0" stroke="#d1394a"/>
    <text x="125" y="138" font-size="12" fill="#d1394a" text-anchor="middle" font-weight="600">回退 Reject</text>
    <text x="125" y="156" font-size="9.5" fill="#5a6b7b" text-anchor="middle">退到任意上游节点</text>
    <text x="125" y="169" font-size="8.5" fill="#8b97b3" text-anchor="middle">reject_targets 列可选</text>

    <rect x="230" y="116" width="170" height="58" rx="8" fill="#eaf6fb" stroke="#0a8ec2"/>
    <text x="315" y="138" font-size="12" fill="#0a8ec2" text-anchor="middle" font-weight="600">取回 Withdraw</text>
    <text x="315" y="156" font-size="9.5" fill="#5a6b7b" text-anchor="middle">发起人撤回</text>
    <text x="315" y="169" font-size="8.5" fill="#8b97b3" text-anchor="middle">strict/lenient 策略</text>

    <rect x="420" y="116" width="170" height="58" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="505" y="138" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="600">抄送 CC</text>
    <text x="505" y="156" font-size="9.5" fill="#5a6b7b" text-anchor="middle">只读知会</text>
    <text x="505" y="169" font-size="8.5" fill="#8b97b3" text-anchor="middle">节点级/手动 + 已读追踪</text>

    <rect x="610" y="116" width="170" height="58" rx="8" fill="#eef8f2" stroke="#178a5a"/>
    <text x="695" y="138" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="600">催办 Urge</text>
    <text x="695" y="156" font-size="9.5" fill="#5a6b7b" text-anchor="middle">提醒办理人</text>
    <text x="695" y="169" font-size="8.5" fill="#8b97b3" text-anchor="middle">写台账</text>
  </g>
  <text x="410" y="204" font-size="11" fill="#c26a00" font-weight="600" text-anchor="middle">— 运维/管理动作 —</text>
  <g font-size="10.5">
    <rect x="90" y="216" width="150" height="44" rx="8" fill="#fff" stroke="#9aa5b5"/>
    <text x="165" y="234" font-size="11" fill="#1c2530" text-anchor="middle">自由跳转 Jump</text>
    <text x="165" y="250" font-size="8.5" fill="#8b97b3" text-anchor="middle">管理员跳任意 userTask</text>
    <rect x="255" y="216" width="150" height="44" rx="8" fill="#fff" stroke="#9aa5b5"/>
    <text x="330" y="234" font-size="11" fill="#1c2530" text-anchor="middle">挂起/恢复</text>
    <text x="330" y="250" font-size="8.5" fill="#8b97b3" text-anchor="middle">级联子实例</text>
    <rect x="420" y="216" width="150" height="44" rx="8" fill="#fff" stroke="#9aa5b5"/>
    <text x="495" y="234" font-size="11" fill="#1c2530" text-anchor="middle">改变量 SetVars</text>
    <text x="495" y="250" font-size="8.5" fill="#8b97b3" text-anchor="middle">运维修数据</text>
    <rect x="585" y="216" width="150" height="44" rx="8" fill="#fdf0f0" stroke="#d1394a"/>
    <text x="660" y="234" font-size="11" fill="#d1394a" text-anchor="middle">Incident 重试</text>
    <text x="660" y="250" font-size="8.5" fill="#8b97b3" text-anchor="middle">修绑定后 retry</text>
  </g>
  <text x="410" y="295" font-size="10" fill="#8b97b3" text-anchor="middle">候选人类型 CandidateKind（7 种）：用户/角色/岗位/组织/部门领导/发起人/发起人上级</text>
  <text x="410" y="312" font-size="9.5" fill="#b0bac9" text-anchor="middle">办理人解析是「令牌到达时快照」——避免角色变更导致待办凭空消失</text>
</svg>



### 6.4 失败处理：永不 panic 落 trace（Incident 模型）

serviceTask delegate 失败**不中断推进、不丢实例**：令牌转 `Incident` 态，`record_incident` 把原因 + 累计重试次数写进实例变量 `__incident`（按节点 bpmn_id 键的 JSON，无新表）。实例仍 `Active`，其它分支继续跑，`hasIncident` 对运维台可见。`retry_incident` 重新激活所有 Incident 令牌重跑；子流程解析失败复用同机制（未路由/未部署/无路由器 → Incident 而非丢僵尸实例），修好绑定后 retry 即可穿过 callActivity。


---

## 七、子流程与路由维度泛化

子流程是审批场景的高频诉求：**同一主流程，不同维度（组织/法人/产品…）跑不同的中间审批链**。主流程 callActivity 不写死调哪个子流程，而写一个**逻辑 key**，运行期由 `SubflowRouter` 按维度取值解析成具体子流程定义。

### 7.1 路由维度泛化（RD0–RD4）

M5.2 最初把路由维度**写死为组织机构**。RD0–RD4 把它泛化为**任意字典**：一个 `dimKey` 选择用哪个字典（内建 `org`，或任意 `cf_*` 字典），`dimValue` 是实例在该维度上的取值。核心突破——**同一主流程实例，不同挂载点可按不同维度路由**。



<svg viewBox="0 0 820 400" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="400" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">同实例双维度路由（报销主流程）</text>

  <!-- main flow -->
  <circle cx="70" cy="90" r="16" fill="#fff" stroke="#178a5a" stroke-width="2"/><text x="70" y="94" font-size="9" fill="#178a5a" text-anchor="middle">开始</text>
  <rect x="120" y="72" width="100" height="36" rx="6" fill="#fff8ec" stroke="#c26a00"/><text x="170" y="94" font-size="10" fill="#c26a00" text-anchor="middle">填报销单</text>
  <rect x="250" y="66" width="140" height="48" rx="8" fill="#f3eefb" stroke="#7d4fd1" stroke-width="2"/>
  <text x="320" y="86" font-size="10.5" fill="#7d4fd1" text-anchor="middle" font-weight="600">①预算审批</text>
  <text x="320" y="102" font-size="9" fill="#5a6b7b" text-anchor="middle">dimKey=org</text>
  <rect x="420" y="66" width="140" height="48" rx="8" fill="#eaf6fb" stroke="#0a8ec2" stroke-width="2"/>
  <text x="490" y="86" font-size="10.5" fill="#0a8ec2" text-anchor="middle" font-weight="600">②合规审查</text>
  <text x="490" y="102" font-size="9" fill="#5a6b7b" text-anchor="middle">dimKey=product</text>
  <rect x="590" y="72" width="100" height="36" rx="6" fill="#fff8ec" stroke="#c26a00"/><text x="640" y="94" font-size="10" fill="#c26a00" text-anchor="middle">出纳打款</text>
  <circle cx="740" cy="90" r="16" fill="#fff" stroke="#d1394a" stroke-width="3"/><text x="740" y="94" font-size="9" fill="#d1394a" text-anchor="middle">结束</text>

  <line x1="86" y1="90" x2="118" y2="90" stroke="#9aa5b5" stroke-width="1.3"/>
  <line x1="220" y1="90" x2="248" y2="90" stroke="#9aa5b5" stroke-width="1.3"/>
  <line x1="390" y1="90" x2="418" y2="90" stroke="#9aa5b5" stroke-width="1.3"/>
  <line x1="560" y1="90" x2="588" y2="90" stroke="#9aa5b5" stroke-width="1.3"/>
  <line x1="690" y1="90" x2="722" y2="90" stroke="#9aa5b5" stroke-width="1.3"/>

  <!-- instance dims -->
  <rect x="250" y="140" width="310" height="34" rx="6" fill="#eef3fb"/>
  <text x="405" y="161" font-size="10.5" fill="#1c2530" text-anchor="middle">实例维度上下文 dimensions = {"org":"上海","product":"信用卡"}</text>

  <!-- org dim resolution -->
  <path d="M320 114 L320 195" stroke="#7d4fd1" stroke-width="1.3" stroke-dasharray="4 3" marker-end="url(#a5)"/>
  <rect x="200" y="200" width="240" height="150" rx="8" fill="#faf7ff" stroke="#7d4fd1"/>
  <text x="320" y="220" font-size="11" fill="#7d4fd1" text-anchor="middle" font-weight="600">org 维度（cmx_org 斜杠路径）</text>
  <text x="320" y="240" font-size="9.5" fill="#5a6b7b" text-anchor="middle">按 dimValue=上海 查绑定</text>
  <rect x="230" y="250" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="320" y="268" font-size="9.5" fill="#1c2530" text-anchor="middle">① 精确：上海→上海预算子</text>
  <rect x="230" y="282" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="320" y="300" font-size="9.5" fill="#1c2530" text-anchor="middle">② 继承：沿 path 向上找祖先</text>
  <rect x="230" y="314" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="320" y="332" font-size="9.5" fill="#1c2530" text-anchor="middle">③ 兜底：默认绑定</text>

  <!-- product dim resolution -->
  <path d="M490 114 L490 195" stroke="#0a8ec2" stroke-width="1.3" stroke-dasharray="4 3" marker-end="url(#a5)"/>
  <rect x="480" y="200" width="240" height="150" rx="8" fill="#f2fafd" stroke="#0a8ec2"/>
  <text x="600" y="220" font-size="11" fill="#0a8ec2" text-anchor="middle" font-weight="600">product 维度（cf_product 点路径）</text>
  <text x="600" y="240" font-size="9.5" fill="#5a6b7b" text-anchor="middle">按 dimValue=信用卡 查绑定</text>
  <rect x="510" y="250" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="600" y="268" font-size="9.5" fill="#1c2530" text-anchor="middle">① 精确：信用卡→信用卡合规</text>
  <rect x="510" y="282" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="600" y="300" font-size="9.5" fill="#1c2530" text-anchor="middle">② 继承：沿 full_path 向上</text>
  <rect x="510" y="314" width="180" height="26" rx="5" fill="#fff" stroke="#c9ced4"/><text x="600" y="332" font-size="9.5" fill="#1c2530" text-anchor="middle">③ 兜底：通用合规</text>

  <defs><marker id="a5" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
  <text x="410" y="380" font-size="10" fill="#8b97b3" text-anchor="middle">org 用斜杠 id 路径、product 用点 code 路径——分隔符/段来源因字典而异，DimSpec 参数化承载</text>
</svg>



**三层解析**（精确 → 沿维度字典物化路径继承 → 默认兜底）对**任意自分级字典同构**：组织用 `cmx_org.path`（斜杠/id 段），产品用 `cf_product.full_path`（点/code 段），仅分隔符和段来源不同，由 `DimSpec{table, id_col, path_col, delim}` 参数化承载。平级字典无「上级」概念，天然只走精确+兜底。

**RD 阶梯**：RD0 契约泛化（`resolve` 三参 + `CallActivity.dim_key` + BPMN 解析）→ RD1 PgSubflowRouter DimSpec + 自分级继承 → RD2 绑定表 `dim_key`/`dim_value` + 端点 → RD3 实例维度上下文 → RD4 设计器。已真机双维度端到端 **25/25** 绿。

### 7.2 多挂载去重（M5.3）

一个主流程多处挂载不同子流程时，去重键是 `(parent_token_id, parent_node_bpmn_id)`——一个令牌串行经过多个 callActivity，每个挂载点独立起子流程。子流程复用完整内核，故子流程也能有多实例/定时器/转签。

---

## 八、可注入扩展点

引擎内核零外部依赖，外部能力经 **trait 注入**（4 个）或 **注册表按 key**（2 个）接入。选择模式经 `AdapterConfig::from_env()`（mock/http/pg，默认全 pg）。



<svg viewBox="0 0 820 380" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="380" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">扩展点与三档实现</text>

  <!-- engine -->
  <rect x="310" y="46" width="200" height="46" rx="10" fill="#0a6ed1"/>
  <text x="410" y="66" font-size="12.5" fill="#fff" text-anchor="middle" font-weight="700">执行内核</text>
  <text x="410" y="82" font-size="9.5" fill="#dceafd" text-anchor="middle">只认 trait / 注册表，不认实现</text>

  <!-- 4 trait injected -->
  <g>
    <rect x="30" y="130" width="180" height="70" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="120" y="150" font-size="11.5" fill="#0a6ed1" text-anchor="middle" font-weight="700">Clock</text>
    <text x="120" y="167" font-size="9" fill="#5a6b7b" text-anchor="middle">定时器时钟（M2.5）</text>
    <text x="120" y="184" font-size="8.5" fill="#8b97b3" text-anchor="middle">System / Test</text>

    <rect x="220" y="130" width="180" height="70" rx="8" fill="#eef8f2" stroke="#178a5a"/>
    <text x="310" y="150" font-size="11.5" fill="#178a5a" text-anchor="middle" font-weight="700">JavaDelegate</text>
    <text x="310" y="167" font-size="9" fill="#5a6b7b" text-anchor="middle">serviceTask 执行（M1）</text>
    <text x="310" y="184" font-size="8.5" fill="#8b97b3" text-anchor="middle">Http / Mock / 内置</text>

    <rect x="420" y="130" width="180" height="70" rx="8" fill="#fff8ec" stroke="#c26a00"/>
    <text x="510" y="150" font-size="11.5" fill="#c26a00" text-anchor="middle" font-weight="700">AssigneeResolver</text>
    <text x="510" y="167" font-size="9" fill="#5a6b7b" text-anchor="middle">候选人解析（M4.1）</text>
    <text x="510" y="184" font-size="8.5" fill="#8b97b3" text-anchor="middle">Pg / Http / Mock / Local</text>

    <rect x="610" y="130" width="180" height="70" rx="8" fill="#f3eefb" stroke="#7d4fd1"/>
    <text x="700" y="150" font-size="11.5" fill="#7d4fd1" text-anchor="middle" font-weight="700">SubflowRouter</text>
    <text x="700" y="167" font-size="9" fill="#5a6b7b" text-anchor="middle">子流程路由（M5.2）</text>
    <text x="700" y="184" font-size="8.5" fill="#8b97b3" text-anchor="middle">Pg / Http / Mock</text>
  </g>
  <text x="410" y="120" font-size="10.5" fill="#0a6ed1" text-anchor="middle" font-weight="600">— 4 个 trait 注入（Option&lt;Arc&lt;dyn&gt;&gt;）—</text>

  <!-- 2 registry -->
  <text x="410" y="240" font-size="10.5" fill="#c26a00" text-anchor="middle" font-weight="600">— 2 个注册表按 key —</text>
  <rect x="180" y="252" width="200" height="60" rx="8" fill="#fff" stroke="#5a6b7b"/>
  <text x="280" y="272" font-size="11" fill="#1c2530" text-anchor="middle" font-weight="600">DelegateRegistry</text>
  <text x="280" y="289" font-size="9" fill="#5a6b7b" text-anchor="middle">serviceTask 按 delegate 键分派</text>
  <text x="280" y="303" font-size="8.5" fill="#8b97b3" text-anchor="middle">register_delegate(key, impl)</text>
  <rect x="440" y="252" width="200" height="60" rx="8" fill="#fff" stroke="#5a6b7b"/>
  <text x="540" y="272" font-size="11" fill="#1c2530" text-anchor="middle" font-weight="600">决策表（DMN 子集）</text>
  <text x="540" y="289" font-size="9" fill="#5a6b7b" text-anchor="middle">businessRuleTask 按 key 求值</text>
  <text x="540" y="303" font-size="8.5" fill="#8b97b3" text-anchor="middle">register_decision(key, table)</text>

  <path d="M120 130 L360 92" stroke="#c9d2e0" stroke-width="1"/>
  <path d="M310 130 L390 92" stroke="#c9d2e0" stroke-width="1"/>
  <path d="M510 130 L430 92" stroke="#c9d2e0" stroke-width="1"/>
  <path d="M700 130 L460 92" stroke="#c9d2e0" stroke-width="1"/>

  <text x="410" y="345" font-size="10.5" fill="#8b97b3" text-anchor="middle">引擎泛型于 RuntimeStore——连持久化都是注入的契约，内核无 DB 知识</text>
  <text x="410" y="365" font-size="9.5" fill="#b0bac9" text-anchor="middle">默认全 pg（零回归）· 独立微服务可切 http 接外部服务 · 测试用 mock 脱外部</text>
</svg>



`AssigneeResolver` 两档 API：`resolve`（非关系型 User/Role/Position/Org）+ `resolve_with(ctx)`（关系型 OrgLeader/Initiator/InitiatorLeader，用 `ResolveContext{initiator, org_id}`）。


---

## 九、数据模型与持久化

PostgreSQL 存储，硬约束：`cmx_flow_` 前缀、**无外键**（索引替代）、DDL 幂等。表分 **RU（运行态）** 与 **HI（历史态）** 两类；终态实例在同一事务归档到 HI 表（含存续时长），供 SLA/时效分析。



<svg viewBox="0 0 820 470" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="470" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">数据模型（聚合根 + 子实体，无物理外键）</text>

  <!-- aggregate root -->
  <rect x="310" y="50" width="200" height="52" rx="10" fill="#0a6ed1"/>
  <text x="410" y="72" font-size="12.5" fill="#fff" text-anchor="middle" font-weight="700">cmx_flow_instance</text>
  <text x="410" y="90" font-size="9" fill="#dceafd" text-anchor="middle">聚合根 · variables/dimensions jsonb</text>

  <!-- RU children -->
  <g font-size="9.5">
    <rect x="30" y="150" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="105" y="169" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_token</text>
    <text x="105" y="184" fill="#8b97b3" text-anchor="middle">令牌=当前节点+态</text>

    <rect x="200" y="150" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="275" y="169" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_task</text>
    <text x="275" y="184" fill="#8b97b3" text-anchor="middle">用户任务(等待态)</text>

    <rect x="370" y="150" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="445" y="169" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_mi_scope</text>
    <text x="445" y="184" fill="#8b97b3" text-anchor="middle">会签/或签账本</text>

    <rect x="540" y="150" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="615" y="169" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_job</text>
    <text x="615" y="184" fill="#8b97b3" text-anchor="middle">定时器到期作业</text>

    <rect x="115" y="208" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="190" y="227" font-size="10" fill="#1c2530" text-anchor="middle" font-weight="600">task_candidate</text>
    <text x="190" y="242" fill="#8b97b3" text-anchor="middle">候选池(认领)</text>

    <rect x="285" y="208" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="360" y="227" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_cc</text>
    <text x="360" y="242" fill="#8b97b3" text-anchor="middle">抄送+已读</text>

    <rect x="455" y="208" width="150" height="46" rx="7" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="530" y="227" font-size="10" fill="#1c2530" text-anchor="middle" font-weight="600">task_delegation</text>
    <text x="530" y="242" fill="#8b97b3" text-anchor="middle">转签台账</text>
  </g>
  <text x="70" y="130" font-size="10.5" fill="#0a6ed1" font-weight="600">RU 运行态（随聚合快照全删重插）</text>

  <!-- lines to root -->
  <g stroke="#c9d2e0" stroke-width="1" fill="none">
    <path d="M105 150 L360 102"/><path d="M275 150 L390 102"/><path d="M445 150 L430 102"/><path d="M615 150 L460 102"/>
    <path d="M190 208 L370 104"/><path d="M360 208 L410 104"/><path d="M530 208 L450 104"/>
  </g>

  <!-- HI -->
  <text x="140" y="290" font-size="10.5" fill="#178a5a" font-weight="600">HI 历史态（终态归档）</text>
  <rect x="120" y="300" width="180" height="44" rx="7" fill="#eef8f2" stroke="#178a5a"/>
  <text x="210" y="319" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_hi_instance</text>
  <text x="210" y="334" font-size="9" fill="#8b97b3" text-anchor="middle">历史实例 + duration_ms</text>
  <rect x="320" y="300" width="180" height="44" rx="7" fill="#eef8f2" stroke="#178a5a"/>
  <text x="410" y="319" font-size="10.5" fill="#1c2530" text-anchor="middle" font-weight="600">cmx_flow_hi_task</text>
  <text x="410" y="334" font-size="9" fill="#8b97b3" text-anchor="middle">历史任务 + 办理时长</text>
  <path d="M410 102 L210 298" stroke="#178a5a" stroke-width="1" stroke-dasharray="4 3" fill="none" marker-end="url(#a6)"/>
  <text x="290" y="200" font-size="9" fill="#178a5a">终态同事务归档→</text>

  <!-- design-time -->
  <text x="560" y="290" font-size="10.5" fill="#c26a00" font-weight="600">定义态 / 配置态</text>
  <rect x="540" y="300" width="130" height="44" rx="7" fill="#fff8ec" stroke="#c26a00"/>
  <text x="605" y="319" font-size="9.5" fill="#1c2530" text-anchor="middle">definition</text>
  <text x="605" y="334" font-size="8.5" fill="#8b97b3" text-anchor="middle">+ version 版本</text>
  <rect x="685" y="300" width="115" height="44" rx="7" fill="#fff8ec" stroke="#c26a00"/>
  <text x="742" y="319" font-size="9" fill="#1c2530" text-anchor="middle">subflow</text>
  <text x="742" y="334" font-size="8.5" fill="#8b97b3" text-anchor="middle">_binding 绑定</text>

  <defs><marker id="a6" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#178a5a"/></marker></defs>

  <!-- snapshot txn -->
  <rect x="120" y="376" width="580" height="72" rx="10" fill="#fff" stroke="#c9ced4"/>
  <text x="410" y="396" font-size="11.5" fill="#1c2530" text-anchor="middle" font-weight="700">聚合快照单事务持久化</text>
  <text x="410" y="415" font-size="9.5" fill="#5a6b7b" text-anchor="middle">save_snapshot = 一个事务：update instance + 子实体 delete-all-then-reinsert</text>
  <text x="410" y="432" font-size="9.5" fill="#8b97b3" text-anchor="middle">子实体数量小（审批多为个位数），全删重插最简且原子 · 一次提交 = 一个 BPMN 等待态提交点</text>
</svg>



---

## 十、多租户隔离

**db-per-tenant** 物理隔离（S2）。默认 `single` 模式零回归；`multi` 模式下每租户一套库/引擎/定义/webhook。



<svg viewBox="0 0 820 320" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="320" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">多租户 db-per-tenant + task_local 作用域</text>

  <!-- request -->
  <rect x="40" y="60" width="140" height="40" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="110" y="78" font-size="11" fill="#0a6ed1" text-anchor="middle" font-weight="600">HTTP 请求</text>
  <text x="110" y="93" font-size="9" fill="#5a6b7b" text-anchor="middle">JWT/API-Key</text>

  <rect x="220" y="60" width="160" height="40" rx="8" fill="#fff8ec" stroke="#c26a00"/>
  <text x="300" y="78" font-size="11" fill="#c26a00" text-anchor="middle" font-weight="600">auth 中间件</text>
  <text x="300" y="93" font-size="9" fill="#5a6b7b" text-anchor="middle">建 task_local 作用域</text>

  <rect x="420" y="60" width="180" height="40" rx="8" fill="#eef8f2" stroke="#178a5a"/>
  <text x="510" y="78" font-size="11" fill="#178a5a" text-anchor="middle" font-weight="600">current_tenant()</text>
  <text x="510" y="93" font-size="9" fill="#5a6b7b" text-anchor="middle">全链路 await 无参透传</text>

  <path d="M180 80 L218 80" stroke="#9aa5b5" stroke-width="1.3" marker-end="url(#a7)"/>
  <path d="M380 80 L418 80" stroke="#9aa5b5" stroke-width="1.3" marker-end="url(#a7)"/>

  <!-- runtime cache -->
  <rect x="220" y="130" width="380" height="44" rx="8" fill="#eef3fb"/>
  <text x="410" y="150" font-size="11" fill="#1c2530" text-anchor="middle" font-weight="600">per-tenant 引擎缓存（OnceCell 单飞）</text>
  <text x="410" y="166" font-size="9" fill="#8b97b3" text-anchor="middle">map 锁只护取/插空 cell · 昂贵 build 不持锁 · 冷启租户不阻塞其它</text>
  <path d="M510 100 L410 128" stroke="#9aa5b5" stroke-width="1.2" marker-end="url(#a7)"/>

  <!-- tenant DBs -->
  <g>
    <rect x="90" y="212" width="180" height="80" rx="8" fill="#fff" stroke="#0a6ed1"/>
    <text x="180" y="234" font-size="11" fill="#0a6ed1" text-anchor="middle" font-weight="700">租户 A</text>
    <text x="180" y="252" font-size="9" fill="#5a6b7b" text-anchor="middle">flow_A 库</text>
    <text x="180" y="267" font-size="9" fill="#5a6b7b" text-anchor="middle">独立引擎/定义/webhook</text>
    <text x="180" y="282" font-size="8.5" fill="#8b97b3" text-anchor="middle">cmx_flow_* 表</text>

    <rect x="320" y="212" width="180" height="80" rx="8" fill="#fff" stroke="#178a5a"/>
    <text x="410" y="234" font-size="11" fill="#178a5a" text-anchor="middle" font-weight="700">租户 B</text>
    <text x="410" y="252" font-size="9" fill="#5a6b7b" text-anchor="middle">flow_B 库</text>
    <text x="410" y="267" font-size="9" fill="#5a6b7b" text-anchor="middle">物理隔离</text>
    <text x="410" y="282" font-size="8.5" fill="#8b97b3" text-anchor="middle">cmx_flow_* 表</text>

    <rect x="550" y="212" width="180" height="80" rx="8" fill="#fff" stroke="#c26a00"/>
    <text x="640" y="234" font-size="11" fill="#c26a00" text-anchor="middle" font-weight="700">default（single）</text>
    <text x="640" y="252" font-size="9" fill="#5a6b7b" text-anchor="middle">fico-db / primary</text>
    <text x="640" y="267" font-size="9" fill="#5a6b7b" text-anchor="middle">零回归缺省形态</text>
  </g>
  <path d="M340 174 L180 210" stroke="#c9d2e0" stroke-width="1" fill="none"/>
  <path d="M410 174 L410 210" stroke="#c9d2e0" stroke-width="1" fill="none"/>
  <path d="M480 174 L640 210" stroke="#c9d2e0" stroke-width="1" fill="none"/>

  <defs><marker id="a7" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
  <text x="410" y="312" font-size="9.5" fill="#b0bac9" text-anchor="middle">注意：task_local 不跨 tokio::spawn——webhook worker / 定时器 poller 显式捕获租户</text>
</svg>



---

## 十一、无头契约与集成

面向第三方的对外契约四件套（S3–S6）：**v1 前缀 + SSE 事件流 + Webhook + OpenAPI**，加平台集成的 **反向代理壳 + 委托令牌桥**。



<svg viewBox="0 0 820 360" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="360" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">对外契约与事件</text>

  <!-- engine core emit -->
  <rect x="310" y="150" width="200" height="60" rx="10" fill="#0a6ed1"/>
  <text x="410" y="176" font-size="12" fill="#fff" text-anchor="middle" font-weight="700">cmx-flow-app</text>
  <text x="410" y="194" font-size="9" fill="#dceafd" text-anchor="middle">引擎调用成功后 emit FlowEvent</text>

  <!-- SSE -->
  <rect x="40" y="60" width="200" height="60" rx="8" fill="#eef8f2" stroke="#178a5a"/>
  <text x="140" y="82" font-size="11.5" fill="#178a5a" text-anchor="middle" font-weight="700">SSE 事件流</text>
  <text x="140" y="99" font-size="9" fill="#5a6b7b" text-anchor="middle">GET /v1/events</text>
  <text x="140" y="112" font-size="8.5" fill="#8b97b3" text-anchor="middle">broadcast · 按租户隔离</text>
  <path d="M330 160 Q220 130 210 122" stroke="#178a5a" stroke-width="1.3" fill="none" marker-end="url(#a8)"/>

  <!-- Webhook -->
  <rect x="580" y="60" width="200" height="60" rx="8" fill="#fff8ec" stroke="#c26a00"/>
  <text x="680" y="82" font-size="11.5" fill="#c26a00" text-anchor="middle" font-weight="700">Webhook 出站</text>
  <text x="680" y="99" font-size="9" fill="#5a6b7b" text-anchor="middle">异步队列 + 指数退避重试</text>
  <text x="680" y="112" font-size="8.5" fill="#8b97b3" text-anchor="middle">HMAC-SHA256 签名</text>
  <path d="M490 160 Q600 130 620 122" stroke="#c26a00" stroke-width="1.3" fill="none" marker-end="url(#a8)"/>

  <!-- OpenAPI -->
  <rect x="40" y="250" width="200" height="60" rx="8" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="140" y="272" font-size="11.5" fill="#0a6ed1" text-anchor="middle" font-weight="700">OpenAPI / Swagger</text>
  <text x="140" y="289" font-size="9" fill="#5a6b7b" text-anchor="middle">/v1/openapi.json · /v1/docs</text>
  <text x="140" y="302" font-size="8.5" fill="#8b97b3" text-anchor="middle">免鉴权，utoipa 手建</text>
  <path d="M330 200 Q220 240 210 248" stroke="#0a6ed1" stroke-width="1.3" fill="none" marker-end="url(#a8)"/>

  <!-- auth bridge -->
  <rect x="580" y="250" width="200" height="60" rx="8" fill="#f3eefb" stroke="#7d4fd1"/>
  <text x="680" y="272" font-size="11.5" fill="#7d4fd1" text-anchor="middle" font-weight="700">鉴权 / 委托令牌</text>
  <text x="680" y="289" font-size="9" fill="#5a6b7b" text-anchor="middle">JWT + API-Key + 委托桥</text>
  <text x="680" y="302" font-size="8.5" fill="#8b97b3" text-anchor="middle">X-Delegated-User-Token</text>
  <path d="M490 200 Q600 240 620 248" stroke="#7d4fd1" stroke-width="1.3" fill="none" marker-end="url(#a8)"/>

  <defs><marker id="a8" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
  <text x="410" y="240" font-size="9.5" fill="#8b97b3" text-anchor="middle">事件在 app 层 emit（引擎不动）· SSE 与 Webhook 共用同一 FlowEvent</text>
  <text x="410" y="340" font-size="9.5" fill="#b0bac9" text-anchor="middle">平台集成：配置 [center_client.urls].flow 空=内嵌 FlowModule，非空=反代 FlowProxyModule（双向流式 + SSE 透传）</text>
</svg>



**生命周期事件**：`instance.started/completed/terminated`、`task.created/completed/reassigned`。**S6 委托令牌桥**：API-Key 分支额外解 `X-Delegated-User-Token`（始终验签），租户取自委托令牌的 claim 而非 key 绑定的租户——让平台以「服务身份 + 终端用户身份」双重上下文调用独立引擎。


---

## 十二、性能特性

引擎性能建立在 **Rust 零成本抽象 + tokio 全异步 + 聚合快照最小化 IO + 纯函数无后台线程** 四个支点上。



<svg viewBox="0 0 820 400" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="400" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">性能支点</text>

  <g>
    <rect x="40" y="50" width="360" height="90" rx="10" fill="#eaf1fb" stroke="#0a6ed1"/>
    <text x="60" y="74" font-size="12.5" fill="#0a6ed1" font-weight="700">① 语言与运行时</text>
    <text x="60" y="94" font-size="9.5" fill="#5a6b7b">· Rust edition 2024，release opt-level=3 + LTO thin</text>
    <text x="60" y="110" font-size="9.5" fill="#5a6b7b">· tokio 全异步；axum 0.8；tower-http 压缩</text>
    <text x="60" y="126" font-size="9.5" fill="#5a6b7b">· arena+索引 IR，无 Rc/RefCell 图，枚举分派</text>

    <rect x="420" y="50" width="360" height="90" rx="10" fill="#eef8f2" stroke="#178a5a"/>
    <text x="440" y="74" font-size="12.5" fill="#178a5a" font-weight="700">② 数据访问</text>
    <text x="440" y="94" font-size="9.5" fill="#5a6b7b">· 连接池（deadpool，默认 max=10）</text>
    <text x="440" y="110" font-size="9.5" fill="#5a6b7b">· 聚合快照：一实例 3 条 SELECT 重建，非事件回放</text>
    <text x="440" y="126" font-size="9.5" fill="#5a6b7b">· HTTP 适配器复用单 reqwest::Client（连接复用）</text>

    <rect x="40" y="156" width="360" height="90" rx="10" fill="#fff8ec" stroke="#c26a00"/>
    <text x="60" y="180" font-size="12.5" fill="#c26a00" font-weight="700">③ 执行模型</text>
    <text x="60" y="200" font-size="9.5" fill="#5a6b7b">· 等待态即提交点：一段推进一次事务，无中间落盘</text>
    <text x="60" y="216" font-size="9.5" fill="#5a6b7b">· 引擎纯函数无后台线程，可确定性测试</text>
    <text x="60" y="232" font-size="9.5" fill="#5a6b7b">· 条件/决策请求驱动求值（无 poller）</text>

    <rect x="420" y="156" width="360" height="90" rx="10" fill="#f3eefb" stroke="#7d4fd1"/>
    <text x="440" y="180" font-size="12.5" fill="#7d4fd1" font-weight="700">④ 定时器</text>
    <text x="440" y="200" font-size="9.5" fill="#5a6b7b">· 5s tokio interval poller，遍历已建租户运行时</text>
    <text x="440" y="216" font-size="9.5" fill="#5a6b7b">· find_due_jobs 按 due_at 索引跨实例扫描</text>
    <text x="440" y="232" font-size="9.5" fill="#5a6b7b">· trigger 带 limit 上限，防到期作业雪崩</text>
  </g>

  <!-- monitor -->
  <rect x="120" y="270" width="580" height="60" rx="10" fill="#eef3fb"/>
  <text x="410" y="292" font-size="12" fill="#1c2530" text-anchor="middle" font-weight="700">可观测性（cmx-web-monitor · /_mon）</text>
  <text x="410" y="311" font-size="9.5" fill="#5a6b7b" text-anchor="middle">系统采样（sysinfo 3s）+ DB 池状态（零查询开销）+ 请求遥测 + 服务拓扑探测</text>
  <text x="410" y="325" font-size="9" fill="#8b97b3" text-anchor="middle">节点时效分析 /analytics/node-timing 按 node_bpmn_id 聚合，支持 SLA 阈值</text>

  <text x="410" y="360" font-size="10.5" fill="#d1394a" text-anchor="middle" font-weight="600">⚠ 扩展边界（如实披露）</text>
  <text x="410" y="378" font-size="9.5" fill="#8b97b3" text-anchor="middle">无 SKIP LOCKED / 分布式协调——定时器 poller 假设每租户库单写者，多副本 poller 需自行加分布式锁</text>
  <text x="410" y="393" font-size="9" fill="#b0bac9" text-anchor="middle">当前为单实例 + PG 轮询模型；水平扩展读可多副本，定时器写侧需 leader 选举（未内建）</text>
</svg>



**关键性能数字与设计**：

| 维度 | 特性 |
|------|------|
| 编译产物 | release `opt-level=3` + `lto="thin"`；distroless 运行镜像无 shell |
| 连接池 | deadpool，默认 `max_connections=10`（测试 1）；池状态零开销暴露给监控 |
| 持久化 IO | 一段推进 = 一次事务；聚合子实体全删重插（审批场景个位数行，原子且最简） |
| 恢复模型 | 快照重建（3 SELECT），非事件回放；重启从 PG 恢复半成品流程 |
| 定时器 | 纯函数引擎 + 外部 5s poller；`find_due_jobs` 按 `due_at` 索引；`limit` 防雪崩 |
| 条件/决策 | 请求驱动纯函数求值，无 poller、无后台线程 |
| HTTP 适配 | 单 `reqwest::Client` 连接复用；User/Initiator 本地短路省往返 |

---

## 十三、前端微前端三壳

「前端一芯三壳」——共享一份 S4 抽取的 native-page 核（`web/core/`），三种消费形态：



<svg viewBox="0 0 820 300" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="300" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">前端微前端三壳</text>

  <rect x="40" y="56" width="230" height="180" rx="10" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="155" y="80" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="700">壳① 门户内嵌 native 页</text>
  <text x="155" y="102" font-size="9.5" fill="#5a6b7b" text-anchor="middle">design-workbench（bpmn-js 设计器）</text>
  <text x="155" y="120" font-size="9.5" fill="#5a6b7b" text-anchor="middle">todo-center 待办中心</text>
  <text x="155" y="138" font-size="9.5" fill="#5a6b7b" text-anchor="middle">task-form 办理表单</text>
  <text x="155" y="156" font-size="9.5" fill="#5a6b7b" text-anchor="middle">identity-workbench 身份台</text>
  <text x="155" y="174" font-size="9.5" fill="#5a6b7b" text-anchor="middle">ops-console 运维台</text>
  <text x="155" y="200" font-size="8.5" fill="#8b97b3" text-anchor="middle">API-backed（非静态）</text>
  <text x="155" y="214" font-size="8.5" fill="#8b97b3" text-anchor="middle">rev=xxhash64 字节对齐门户信封</text>

  <rect x="295" y="56" width="230" height="180" rx="10" fill="#eef8f2" stroke="#178a5a"/>
  <text x="410" y="80" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="700">壳② Web Component</text>
  <text x="410" y="102" font-size="9.5" fill="#5a6b7b" text-anchor="middle">&lt;flow-todo&gt;</text>
  <text x="410" y="120" font-size="9.5" fill="#5a6b7b" text-anchor="middle">&lt;flow-designer&gt;</text>
  <text x="410" y="138" font-size="9.5" fill="#5a6b7b" text-anchor="middle">&lt;flow-task-form&gt;</text>
  <text x="410" y="162" font-size="8.5" fill="#8b97b3" text-anchor="middle">框架无关自定义元素</text>
  <text x="410" y="178" font-size="8.5" fill="#8b97b3" text-anchor="middle">shadow DOM 隔离</text>
  <text x="410" y="194" font-size="8.5" fill="#8b97b3" text-anchor="middle">属性 api-base/token/tenant/user</text>
  <text x="410" y="212" font-size="8.5" fill="#8b97b3" text-anchor="middle">链塌缩为冒泡 CustomEvent</text>

  <rect x="550" y="56" width="230" height="180" rx="10" fill="#fdf0f0" stroke="#d1394a"/>
  <text x="665" y="80" font-size="12" fill="#d1394a" text-anchor="middle" font-weight="700">壳③ 完全无头</text>
  <text x="665" y="106" font-size="9.5" fill="#5a6b7b" text-anchor="middle">第三方自建 UI</text>
  <text x="665" y="128" font-size="9.5" fill="#5a6b7b" text-anchor="middle">只消费 /api/flow/v1/*</text>
  <text x="665" y="150" font-size="9.5" fill="#5a6b7b" text-anchor="middle">+ SSE 事件流</text>
  <text x="665" y="172" font-size="9.5" fill="#5a6b7b" text-anchor="middle">+ OpenAPI 契约</text>
  <text x="665" y="200" font-size="8.5" fill="#8b97b3" text-anchor="middle">零前端依赖</text>

  <text x="410" y="264" font-size="10.5" fill="#8b97b3" text-anchor="middle">共享内核 web/core/（S4 抽取的 vendor 拷贝，勿手改）· 四耦合点经 CFG 配置接缝</text>
  <text x="410" y="284" font-size="9.5" fill="#b0bac9" text-anchor="middle">bpmn-js 设计器 · UI5 双主题 · 门户 F3 反代透明（字节对齐信封）</text>
</svg>



---

## 十四、API 接口全景

约 67 个端点，两前缀（`/flow/*` 兼容 + `/flow/v1/*` 正式）。按域分组：

| 域 | 端点（择要） |
|----|-------------|
| **定义** | `GET/POST /definitions`、`/definitions/draft`、`/definitions/validate`、`/definitions/{key}/publish`、`/definitions/{key}/versions`、`.../activate`、`/definitions/{key}/variables` |
| **实例** | `POST /instances`、`/instances/{id}`、`/cancel`、`/suspend`、`/resume`、`/jump`、`/withdraw`、`/set-variables`、`/retry-incident`、`/children`、`/comments`、`/variables`、`/biz` |
| **任务** | `/tasks/my`、`/tasks/{id}/complete`、`/claim`、`/transfer`、`/delegate`、`/addsign`、`/reject`、`/reject-targets`、`/urge` |
| **待办中心** | `/todos/initiated`、`/todos/cc`、`/todos/done`、`/todos/filters`、`/startable` |
| **抄送** | `/cc`、`/cc/{id}/read` |
| **子流程路由** | `/orgs`、`/dimensions`、`/dimension/{dimKey}/entries`、`/subflow-bindings`、`.../{key}`、`.../id/{id}` |
| **决策/条件** | `/decisions`、`/decisions/evaluate`、`/conditions/eval`、`/conditions/validate`、`/conditions/functions` |
| **表单/身份** | `/forms/{key}`、`/identity/{entity}`、`/identity/mode`、`/users`、`/clients` |
| **消息/定时器** | `/messages/correlate`、`/timers/trigger` |
| **监控/契约** | `/stats`、`/stats/detail`、`/analytics/node-timing`、`/events`（SSE）、`/openapi.json`、`/docs` |

响应信封统一 `{code, msg, data}`——成功 `code:0`，业务失败 HTTP 200 + `code:1`。

---

## 十五、部署形态



<svg viewBox="0 0 820 280" xmlns="http://www.w3.org/2000/svg" font-family="system-ui,-apple-system,PingFang SC,sans-serif">
  <rect width="820" height="280" fill="#fbfcfe"/>
  <text x="410" y="26" font-size="14" font-weight="700" fill="#1c2530" text-anchor="middle">三种部署姿态</text>

  <!-- posture 1 -->
  <rect x="30" y="50" width="240" height="200" rx="10" fill="#eaf1fb" stroke="#0a6ed1"/>
  <text x="150" y="74" font-size="12" fill="#0a6ed1" text-anchor="middle" font-weight="700">① 平台内嵌</text>
  <rect x="60" y="90" width="180" height="40" rx="6" fill="#fff" stroke="#0a6ed1"/>
  <text x="150" y="114" font-size="10" fill="#1c2530" text-anchor="middle">cmx-container 平台进程</text>
  <rect x="80" y="140" width="140" height="34" rx="6" fill="#eef3fb"/>
  <text x="150" y="161" font-size="9.5" fill="#5a6b7b" text-anchor="middle">FlowModule 内嵌</text>
  <text x="150" y="200" font-size="9" fill="#8b97b3" text-anchor="middle">同进程同鉴权</text>
  <text x="150" y="216" font-size="9" fill="#8b97b3" text-anchor="middle">urls.flow 为空</text>
  <text x="150" y="236" font-size="8.5" fill="#b0bac9" text-anchor="middle">最省资源，强耦合</text>

  <!-- posture 2 -->
  <rect x="290" y="50" width="240" height="200" rx="10" fill="#eef8f2" stroke="#178a5a"/>
  <text x="410" y="74" font-size="12" fill="#178a5a" text-anchor="middle" font-weight="700">② 独立 + 反代</text>
  <rect x="320" y="90" width="180" height="34" rx="6" fill="#fff" stroke="#178a5a"/>
  <text x="410" y="111" font-size="9.5" fill="#1c2530" text-anchor="middle">平台 FlowProxyModule</text>
  <path d="M410 124 L410 150" stroke="#178a5a" stroke-width="1.3" marker-end="url(#a9)"/>
  <rect x="320" y="152" width="180" height="40" rx="6" fill="#fff" stroke="#178a5a"/>
  <text x="410" y="172" font-size="10" fill="#1c2530" text-anchor="middle">flow-server :8091</text>
  <text x="410" y="186" font-size="8.5" fill="#8b97b3" text-anchor="middle">独立进程/库</text>
  <text x="410" y="214" font-size="9" fill="#8b97b3" text-anchor="middle">双向流式 SSE 透传</text>
  <text x="410" y="232" font-size="8.5" fill="#b0bac9" text-anchor="middle">独立伸缩，平台无感</text>

  <!-- posture 3 -->
  <rect x="550" y="50" width="240" height="200" rx="10" fill="#fdf0f0" stroke="#d1394a"/>
  <text x="670" y="74" font-size="12" fill="#d1394a" text-anchor="middle" font-weight="700">③ 完全无头</text>
  <rect x="580" y="90" width="180" height="34" rx="6" fill="#fff" stroke="#d1394a"/>
  <text x="670" y="111" font-size="9.5" fill="#1c2530" text-anchor="middle">第三方系统</text>
  <path d="M670 124 L670 150" stroke="#d1394a" stroke-width="1.3" marker-end="url(#a9)"/>
  <rect x="580" y="152" width="180" height="40" rx="6" fill="#fff" stroke="#d1394a"/>
  <text x="670" y="172" font-size="10" fill="#1c2530" text-anchor="middle">flow-server :8091</text>
  <text x="670" y="186" font-size="8.5" fill="#8b97b3" text-anchor="middle">v1 API + SSE + OpenAPI</text>
  <text x="670" y="214" font-size="9" fill="#8b97b3" text-anchor="middle">API-Key + 委托令牌</text>
  <text x="670" y="232" font-size="8.5" fill="#b0bac9" text-anchor="middle">多租户 SaaS 场景</text>

  <defs><marker id="a9" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto"><path d="M0 0 L8 4 L0 8 z" fill="#9aa5b5"/></marker></defs>
</svg>



**容器化**：多阶段 Dockerfile（rust builder → distroless/cc-debian12 无 shell 运行时，配置全 env，EXPOSE 8091）；docker-compose（postgres 18 + fico/cmx 双库 + flow-server）。启动经 `cmx-web-chassis::run(spec)` 装配：分层日志（控制台 + 滚动 JSON 文件）→ 有序异步初始化钩子（数据源注册 + 定时器 poller）→ 路由装配 → 监控挂载 → 优雅停机。

---

## 十六、能力边界与路线

### 已实现能力总览

| 类别 | 能力 |
|------|------|
| **节点** | 13 类：开始/结束/终止、用户/服务/规则任务、排他/并行/包容网关、边界定时器、消息捕获、调用活动、嵌入子过程 |
| **多实例** | 会签（并行）+ 或签（顺序）+ completionCondition + 集合驱动 |
| **人机协同** | 认领/转办/委派/加签（可嵌套）/回退任意节点/取回/抄送/催办 |
| **候选人** | 用户/角色/岗位/组织/部门领导/发起人/发起人上级（7 类）+ 候选池认领 |
| **子流程** | 同步子流程 + 按任意维度字典路由 + 多挂载 + 变量映射 |
| **定时器** | 中断/非中断边界定时器 + 可注入时钟 |
| **运维** | 跳转/挂起恢复/改变量/Incident 重试/节点时效分析 |
| **契约** | v1 API + SSE + Webhook + OpenAPI + 多租户 + 委托令牌 |

### 诚实的能力边界（不过度承诺）

- **事件系统**：仅边界定时器 + 消息捕获；无信号/错误/补偿/升级/条件/链接事件。
- **网关**：无事件网关、复杂网关。
- **规则**：决策表是 DMN 子集 + 手写受控 DSL 条件，非完整 FEEL/DMN（如需完整规则能力，配 cmx-rulesengine）。
- **扩展性**：单实例 + PG 轮询模型；无 SKIP LOCKED / 分布式 poller 协调 / 实例版本迁移。
- **读路径**：当前走 DataSet/query_sql，未接零拷贝 ZmcDataSet（待办/实例列表是手工投影小 JSON）。

### 定位总结

> cmx-flowengine 是一款**设计干净、审批扎实的轻量 BPMN 子集引擎**，在人本审批赛道（对标 Flowable 人本层）提供完整的人机协同 + 灵活的子流程维度路由 + 一芯多壳的部署弹性。它不追求通用编排或云原生大规模，而是把「审批场景做深做透 + Rust 内核可靠可嵌」作为核心竞争力。

---

*本文档基于 cmx-flowengine v0.1.12 源码逐项核对生成。功能/性能描述以代码为准，图示为内嵌 SVG（可离线渲染）。相关详细设计见 docs/ 下各专题文档（SUMMARY.html 里程碑总览、schema.md 表结构、enhancement-detail-06 维度路由、standalone-microservice-design.html 独立微服务、full-test/ 测试报告）。*
