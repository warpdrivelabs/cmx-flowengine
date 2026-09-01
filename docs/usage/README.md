# cmx-flowengine 使用说明书

> 独立部署的 Rust BPMN 2.0 流程引擎微服务 —— 完整、面面俱到的使用文档。
>
> 适用版本：S0–S6 全线（引擎内核 M1–M5.3 + A1–A7 + H2/H4 能力）。
> 文档基于源码逐条核对生成，端点/字段/表结构均取自代码，非泛化描述。

## 这套文档是什么

cmx-flowengine 是一款**框架无关、可独立部署、支持多租户**的流程引擎。它把「设计态（BPMN 建模）→ 运行态（令牌执行）→ 消费态（待办/审批/外部集成）」三段打通，身份/组织/表单可外部化对接，也可用内置身份模块开箱即用。

本套文档按主题拆成 9 篇，建议按顺序阅读，也可按需查阅：

| # | 文档 | 覆盖主题 | 面向读者 |
|---|------|---------|---------|
| 01 | [概述与架构](01-overview-and-architecture.md) | crate 布局、一芯三壳、RU/HI、状态机、部署形态、快速开始 | 所有人（先读） |
| 02 | [主流程定义](02-process-definition.md) | BPMN 元素全参考、节点/网关/事件/任务、部署/发布/版本 | 流程设计者、集成方 |
| 03 | [子流程定义](03-subprocess.md) | callActivity（写死/逻辑 key）、嵌入子流程、变量映射、按组织路由 | 流程设计者 |
| 04 | [组织机架 · 用户 · 角色 · 岗位对接](04-organization-and-identity.md) | 内置 `fid_*` 身份、候选人表达式、关系型解析、外部 IAM（HTTP/PG）对接 | 集成方、管理员 |
| 05 | [分支条件与决策表](05-conditions-and-decisions.md) | 表达式 DSL、内置函数、排他/包容/并行网关、DMN 决策表 | 流程设计者 |
| 06 | [REST API 调用说明](06-rest-api-reference.md) | 统一信封、鉴权、全量路由表、逐端点请求/响应 | 集成开发者 |
| 07 | [任务操作：提交/回退/取回/转签](07-task-operations.md) | 办结、退回、认领、转办、委派、加签、催办、待办清单 | 集成开发者、办理人前端 |
| 08 | [外部系统集成](08-external-integration.md) | serviceTask 外呼、Webhook、消息相关、SSE、鉴权头、多租户、平台反代 | 集成开发者、运维 |
| 09 | [运维与管理](09-operations-and-admin.md) | 挂起/恢复/跳转、异常(Incident)重试、改变量、定时器、统计与监控 | 运维、管理员 |
| 10 | [流程变量声明](10-variable-declaration.md) | 设计态声明变量名称/类型/结构/说明、对象/数组字段、四处下拉、发起校验 | 流程设计者、集成方 |
| 11 | [配置参考](11-configuration-reference.md) | 环境变量与 toml 段全集（适配器/webhook/多租户/鉴权/服务目录）、必要性、默认值 | 部署者、运维、集成方 |

## 30 秒速览

```bash
# 1) 启动（需本地 PostgreSQL，含 fico + cmx 两库；sibling 目录 ../cmx-container/ 需存在）
cd cmx-flowengine
./flow.sh                       # 或 cargo run -p cmx-flow-server；默认端口 8091

# 2) 健康探测（列已部署定义）
curl http://127.0.0.1:8091/api/flow/v1/definitions

# 3) 发起一个流程实例
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"credit_approval","variables":{"applicant":"张三","amount":80000}}'

# 4) 查我的待办
curl 'http://127.0.0.1:8091/api/flow/v1/tasks/my?assignee=经理'

# 5) 办结任务（同意）
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/complete \
  -H 'Content-Type: application/json' \
  -d '{"instanceId":"<instanceId>","comment":"同意"}'
```

## 关键约定（贯穿全文）

- **两套 URL 前缀并存**：正式契约 `/api/flow/v1/*`，历史兼容 `/api/flow/*`。新集成一律用 **v1**。
- **统一信封**：成功 `{"code":0,"msg":"success","data":...}`；业务错误 `{"code":1,"msg":...}`（HTTP 200）；未找到 `{"code":4,...}`（HTTP 404）；未认证 `{"code":401,...}`（HTTP 401）。
- **等待态即提交点**：每个 BPMN 等待态（用户任务、子流程、消息等待）对应一次数据库事务提交，进程重启后可从库精确恢复。
- **引擎不硬编码主数据**：身份/组织是可注入扩展点（内置 `fid_*` 默认 + 可选外部 HTTP/PG IAM）。
- **引擎无后台线程**：定时器由宿主显式推进（`trigger_due_timers` / 内建 5 秒 poller）。

## 三大能力面速查

| 你想做… | 看这篇 | 核心端点 / 语法 |
|---------|--------|----------------|
| 画一条审批流并部署 | 02 | BPMN XML + `POST /definitions/draft` → `/publish` |
| 让某节点按金额分两条路 | 05 | `exclusiveGateway` + `${amount > 50000}` 条件边 |
| 会签（多人并行审）/ 或签（逐个审） | 02 | `multiInstanceLoopCharacteristics` + `completionCondition` |
| 财务复核抽成子流程、总部/分公司走不同版本 | 03 | `callActivity cmx:calledKey` + 子流程组织绑定 |
| 审批人写成「财务组」「部门经理岗」「发起人上级」 | 04 | `candidateGroups` / `cmx:candidates="position(...), orgLeader"` |
| 提交 / 退回 / 转办 / 加签 | 07 | `POST /tasks/{id}/complete|reject|transfer|addsign` |
| serviceTask 调用外部系统算风险 | 08 | `serviceTask delegateExpression` + HttpDelegate |
| 外部系统回调唤醒等待中的流程 | 08 | `intermediateCatchEvent` + `POST /messages/correlate` |
| 实时订阅流程事件 | 08 | SSE `GET /api/flow/v1/events` / Webhook |
| serviceTask 失败了怎么办 | 09 | Incident 机制 + `POST /instances/{id}/retry-incident` |

## 名词表

| 术语 | 含义 |
|------|------|
| 定义（Definition） | 一份 BPMN 流程模板，`key` = `<process id>` |
| 实例（Instance） | 定义的一次运行；聚合根，含变量/令牌/任务 |
| 令牌（Token） | 流经流程图的执行指针，锚在某节点 `node_bpmn_id` |
| 任务（Task） | 令牌停在 userTask 时产生的待办 |
| 候选人（Candidate） | 「谁能办这个任务」的引用（用户/角色/岗位/组织/关系型） |
| Delegate | serviceTask 的外呼实现（Rust trait `JavaDelegate` 或 HTTP） |
| Incident | serviceTask 失败时令牌挂起的异常态，可人工重试恢复 |
| RU / HI | 运行态表 / 历史归档表（对齐 Flowable） |
| 一芯三壳 | 同一引擎核，三种前端外壳：门户内嵌 / 可嵌 Web Component / headless |

---

_文档目录：`cmx-flowengine/docs/usage/`。数据库表结构详见 [`../schema.md`](../schema.md)；架构方案见 [`../standalone-microservice-design.html`](../standalone-microservice-design.html)。_
