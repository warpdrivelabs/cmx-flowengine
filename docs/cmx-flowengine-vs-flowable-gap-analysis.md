# cmx-flowengine vs Flowable · 完整对标与补充方案

> **对标基线** Flowable 6.7.x / 7.x 开源版本  
> **文档日期** 2026-08-19 · **只做分析，不动代码**  
> **目标** 逐维度找出 cmx-flowengine 相对 Flowable 的缺失项，按优先级形成补充方案

---

## 0. 结论速览

| 对标维度 | cmx-flowengine | Flowable | 差距等级 |
|---|---|---|:---:|
| BPMN 节点覆盖 | 13 类（子集） | ~100% BPMN 2.0 | 🔴 大 |
| 开始/结束事件类型 | 各 1 种（none/none） | 各 7-10 种 | 🔴 大 |
| 中间事件 | 仅边界定时器 | 全部（timer/message/signal/error/link/escalation/compensation/conditional） | 🔴 大 |
| 网关 | 排他+并行+包容（3 种） | 全部 5 种（+事件网关+复杂网关） | 🟡 中 |
| 子流程类型 | callActivity + 嵌入子过程（编译展平） | 嵌入/事件/事务/Ad-hoc/call（完整 5 类） | 🟡 中 |
| 多实例 | 并行会签+顺序或签+completionCondition | 同等，另支持 loopDataOutputRef | 🟢 接近 |
| 人工任务协同 | 完整（认领/转办/委派/加签/回退/取回/抄送/催办） | 同等（略少加签嵌套深度） | 🟢 接近 |
| 候选人类型 | 7 种（含关系型 OrgLeader/Initiator/InitiatorLeader） | 用户/群组（不含关系型） | 🟢 cmx 强项 |
| 服务任务集成 | HttpDelegate + 同步 | HTTP/Camel/Shell/Script/External-Worker/Web Service | 🔴 大 |
| 脚本任务 | 无（显式拒绝）→ 已有 cmx-rulesengine Rhai | scriptTask（JS/Groovy/JUEL/JSR-223） | 🟡 中 |
| 决策/规则(DMN) | 轻量决策表（已集成 cmx-rulesengine） | 完整 DMN 1.3（7 种命中策略+FEEL+DRD） | 🟡 中 |
| 定时器类型 | timeDuration（ISO-8601） | timeDuration + timeDate + timeCycle（含 Quartz Cron） | 🟡 中 |
| 历史/审计 | 实例历史+任务历史（2 表） | 5 级历史（none/activity/audit/full + 变量明细）+ 活动历史 | 🟡 中 |
| 实例迁移 | 无 | ProcessInstanceMigrationBuilder（含干运行验证） | 🔴 大 |
| 异步 Job Executor | 无（同步执行） | DefaultAsyncJobExecutor（SKIP LOCKED 集群安全） | 🔴 大 |
| 死信队列 | 无 | ACT_RU_DEADLETTER_JOB + 手动移回 | 🔴 大 |
| 外部 Worker | 无 | External-Worker Task（pull-based 异步） | 🔴 大 |
| 事件注册表 | SSE/Webhook 出站 | Event Registry（Kafka/RabbitMQ 双向） | 🔴 大 |
| 表单引擎 | formKey 绑定 + 变量 Schema + 待办中心 | 独立 Form Engine（字段类型/校验/版本/结果按钮） | 🟡 中 |
| CMMN 案例管理 | 无 | 完整 CMMN 1.1 引擎 | ⚪ 不在定位 |
| 内容/文档引擎 | 无 | Content Engine（文件存储/元数据/渲染） | ⚪ 可选 |
| 多租户 | db-per-tenant（物理隔离） | 单库多 tenant_id 列（逻辑隔离） | 🟢 cmx 更强 |
| 集群/HA | 无协调（单写者假设） | SKIP LOCKED 集群安全 job 抢占 | 🔴 大 |
| 持久化模型 | 全快照（delete+reinsert） | 增量（per-field update） | 🟡 中（大流程） |
| 热装载 | H1 已实现（发布即 deploy） | 运行时 createDeployment 即时生效 | 🟢 等同 |
| 版本化 | 草稿/发布/版本/装载 | 同等 | 🟢 等同 |
| 设计器 | bpmn-js 四区 + 专业属性面板 | Flowable Modeler（AngularJS Oryx） | 🟢 接近（cmx 属性面板更深） |
| 运维 UI | 监控大盘 + /_mon | Flowable Admin App（完整运维台） | 🟡 中 |
| REST API | ~67 端点 + v1 + OpenAPI | 完整 REST（所有引擎资源），含历史、Job、Deployment | 🟡 中 |
| 身份/IAM | 外接 IAM + 可选内建 fid_* | 内置 IDM Engine + LDAP + Spring Security OAuth2 | 🟢 等同（不同路线） |
| 前端集成 | Web Component + 微前端三壳 | 四个独立 Spring Boot UI App | 🟢 cmx 更灵活 |
| 语言/性能 | Rust（零成本抽象，内存安全） | Java（成熟生态，JVM 开销） | 🟢 cmx 优势 |
| 读路径（ZmcDataSet） | DataSet/query_sql 手工投影 | MyBatis/TypedQuery 强类型映射 | 🟡 中（B 级，无正确性影响）|

---

## 一、BPMN 元素覆盖对标

### 1.1 开始事件

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| None（无类型） | ✅ | ✅ | 唯一支持类型 |
| Timer | ✅ | ❌ | 定时触发流程（定期报表/自动发起） |
| Message | ✅ | ❌ | 消息触发启动（外部系统回调发起） |
| Signal | ✅ | ❌ | 广播信号触发 |
| Error | ✅（事件子流程） | ❌ | 异常处理子流程 |
| Escalation | ✅（事件子流程） | ❌ | 升级触发子流程 |
| Compensation | ✅（事件子流程） | ❌ | 补偿触发 |
| Conditional | ✅ | ❌ | 条件变化触发 |

**补充优先级**：消息启动 ⭐⭐⭐，定时器启动 ⭐⭐（定期发起审批单）

### 1.2 结束事件

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| None | ✅ | ✅ | |
| Terminate（terminateAll） | ✅ | ✅（已在 M5 后补入） | 一票否决 |
| Error | ✅ | ❌ | 抛出业务异常向上捕获 |
| Escalation | ✅ | ❌ | 升级到上层流程 |
| Cancel（事务子流程） | ✅ | ❌ | 事务回滚 |
| Compensation | ✅ | ❌ | 触发补偿 |
| Signal | ✅ | ❌ | 广播信号 |
| Message | ✅ | ❌ | 发送消息 |

**补充优先级**：Error 结束 ⭐⭐⭐，Compensation ⭐⭐，其余 ⭐

### 1.3 中间事件（Intermediate）

cmx-flowengine **无任何中间事件**（既无 Catch 也无 Throw）。

| 类型（Catch） | Flowable | cmx-flowengine | 审批场景价值 |
|---|:---:|:---:|---|
| Timer | ✅ | ❌ | 等 N 天后自动推进/发提醒 ⭐⭐⭐ |
| Message | ✅ | ❌ | 等外部系统回调（如等付款确认） ⭐⭐⭐ |
| Signal | ✅ | ❌ | 等广播信号 ⭐⭐ |
| Conditional | ✅ | ❌ | 等某变量满足条件 ⭐⭐ |
| Link（catch） | ✅ | ❌ | 大流程跳转 ⭐ |

| 类型（Throw） | Flowable | cmx-flowengine | 审批场景价值 |
|---|:---:|:---:|---|
| Signal | ✅ | ❌ | 广播通知其它流程 ⭐⭐ |
| Escalation | ✅ | ❌ | 上报升级 ⭐⭐ |
| Compensation | ✅ | ❌ | 触发已完成步骤的补偿 ⭐⭐ |
| Message | ✅ | ❌ | 发送消息到另一流程 ⭐ |
| Link（throw） | ✅ | ❌ | 大流程跳转 ⭐ |

**核心缺失**：中间定时等待（"3天后自动推进"）和消息等待（"等外部回调"）是审批场景极高频诉求，当前完全无法建模。

### 1.4 边界事件

| 类型 | Flowable（中断/非中断） | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| Timer | ✅ / ✅ | ✅ / ✅ | 已完整实现 |
| Message | ✅ / ✅ | ❌ | 任务级消息回调 |
| Signal | ✅ / ✅ | ❌ | 任务级信号监听 |
| Error | ✅（仅中断） | ❌ | serviceTask 抛出业务错误 |
| Escalation | ✅ / ✅ | ❌ | 升级到上层 |
| Compensation | ✅（静态关联） | ❌ | 已完成活动的补偿 |
| Conditional | ✅ / ✅ | ❌ | 变量条件监听 |
| Cancel | ✅（事务子流程） | ❌ | 事务取消 |

### 1.5 网关

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| Exclusive（排他） | ✅ | ✅ | |
| Parallel（并行） | ✅ | ✅ | |
| Inclusive（包容） | ✅ | ✅（已在 A1 补入） | |
| Event-based（事件） | ✅ | ❌ | "等审批 or 等超时，谁先到走谁" ⭐⭐ |
| Complex（复杂） | ✅ | ❌ | 几乎无人使用，可不补 |

### 1.6 任务类型

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| User Task | ✅ | ✅ | |
| Service Task（class/expression/delegate） | ✅ | ✅（HttpDelegate） | |
| Service Task（External Worker） | ✅ | ❌ | pull-based 异步 worker ⭐⭐⭐ |
| Script Task（JS/Groovy/JUEL/JSR-223） | ✅ | ❌（Rhai 在 cmx-rulesengine） | ⭐⭐ |
| Business Rule Task（DMN） | ✅ | ✅（cmx-rulesengine 决策表） | 已实现 |
| HTTP Task（flowable:type="http"）| ✅ | ✅（HttpDelegate） | 功能等同 |
| Shell Task | ✅（默认禁用） | ❌ | 安全风险高，可不补 |
| Send Task | ✅ | ❌ | 消息/邮件发送 ⭐ |
| Receive Task | ✅ | ❌ | 等待消息（类 MessageCatch） ⭐⭐ |
| Manual Task | ✅（pass-through） | ❌ | 文档语义，可忽略 |

### 1.7 子流程类型

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| Call Activity | ✅ | ✅（含维度路由） | 强于 Flowable（多维度） |
| Embedded Subprocess | ✅（完整作用域） | ✅（编译展平，边界事件受限） | 展平导致边界事件作用域不完整 |
| Event Subprocess（中断/非中断） | ✅ | ❌ | "随时触发的异常处理块" ⭐⭐ |
| Transaction Subprocess | ✅ | ❌ | 补偿协议 ⭐ |
| Ad-hoc Subprocess | ✅ | ❌ | 活动顺序自由激活，审批少用 ⭐ |

---

## 二、服务集成对标

### 2.1 异步 Job Executor（Flowable 核心竞争力之一）

Flowable 使用 `DefaultAsyncJobExecutor`：
- **SKIP LOCKED** 安全集群抢占，多节点无协调服务
- `flowable:async="true"` 任意任务/子流程异步执行
- `flowable:exclusive="true"` 同实例只跑一个 Job，防并发修改
- `flowable:asyncBefore`/`asyncAfter` 精细化控制异步边界
- Job 可配重试次数+退避 TimeCycle（`R3/PT5M`）

**cmx-flowengine 现状**：同步执行，一个慢外部调用阻塞整个 `run_to_wait`，无重试，失败 → Incident。

**缺失影响**：任何需要调用外部系统的 serviceTask（发邮件/调三方 API/写 ERP）都有超时风险阻塞引擎。

### 2.2 External Worker Task（外部 Worker 协议）

Flowable 7.x 的 `flowable:type="external-worker"`：
- worker 主动 **pull** 任务（`acquireExternalWorkerJobs(topic, lockDuration, maxJobs)`）
- worker 做完后 **push** 结果或失败（`complete(jobId, vars)` / `fail(jobId, error, retries, retryTimer)`）
- 支持多语言 worker（Java/Python/Node/Go）
- 支持重试次数+退避

**cmx-flowengine 缺失**：无此机制，外部系统只能通过 HttpDelegate 被动接受推送，无法主动拉取任务。外部 Worker 是现代工作流引擎的核心集成范式。

### 2.3 Camel / 消息队列集成

Flowable 通过 `flowable-camel` 和 Event Registry 支持：
- Apache Camel 路由（900+ 组件）
- Kafka 双向（inbound channel → trigger process / outbound event）
- RabbitMQ 双向

**cmx-flowengine**：仅 Webhook 出站 + SSE 事件流。无消息队列入站触发流程的机制。

---

## 三、定时器与事件对标

### 3.1 定时器类型

| 类型 | Flowable | cmx-flowengine | 备注 |
|---|:---:|:---:|---|
| timeDuration（PT1H） | ✅ | ✅ | 已实现 |
| timeDate（2026-01-01T09:00:00） | ✅ | ❌ | 指定绝对时间点 ⭐⭐ |
| timeCycle（R5/PT10M，Quartz Cron） | ✅ | ❌ | 重复触发（定期审批） ⭐⭐ |
| 定时器表达式引用变量 | ✅（UEL） | ❌ | `${due_date}` 动态截止日期 ⭐⭐⭐ |

**缺失影响**：所有"在指定日期前审批"、"每月一号自动发起"、"截止日期从变量读取"的场景均无法建模。

### 3.2 消息相关性（Correlation）机制

Flowable 的消息相关性：
- `correlationKey`（业务键）绑定消息到特定实例
- `RuntimeService.messageEventReceived(name, executionId, variables)` 点对点唤醒
- `correlateMessage(messageName, businessKey, correlationVariables)` 按业务键+变量相关
- 租户感知：`messageEventReceivedWithTenantId()`

**cmx-flowengine 现状**：`MessageCatchEvent` 节点类型已存在（`WaitingMessage` 令牌态），`correlate_message` 方法已实现（`engine.rs:1608`），REST 端点 `/messages/correlate` 已有。但**无消息订阅表**，重启后等待中的消息订阅可能丢失。

**需补充**：`cmx_flow_message_subscription` 持久化表（存消息名+实例+相关性变量），确保重启后消息订阅不丢失。

---

## 四、历史与审计对标

### 4.1 历史级别

| 历史级别 | Flowable | cmx-flowengine |
|---|---|---|
| none | ✅ | 无配置开关（默认写历史） |
| activity（进出时刻） | ✅ | ❌（无活动历史表） |
| audit（任务/身份/最终变量值） | ✅ | 部分（任务历史+台账） |
| full（每次变量变更明细） | ✅ | ❌ |

### 4.2 历史表对比

| 历史表 | Flowable | cmx-flowengine |
|---|---|---|
| 历史流程实例（含 duration_ms） | ACT_HI_PROCINST | cmx_flow_hi_instance ✅ |
| 历史任务（含办理时长） | ACT_HI_TASKINST | cmx_flow_hi_task ✅ |
| **历史活动实例**（每节点进出时刻） | **ACT_HI_ACTINST** | ❌ 缺失 |
| **历史变量实例**（最终值） | **ACT_HI_VARINST** | ❌ 缺失 |
| **历史变量明细**（full 级每次写入） | **ACT_HI_DETAIL** | ❌ 缺失 |
| 历史身份链接（办理人变更记录） | ACT_HI_IDENTITYLINK | cmx_flow_task_delegation 部分覆盖 |
| 评论 | ACT_HI_COMMENT | cmx_flow_task_comment ✅ |

**活动历史是 SLA 看板、审计回放、瓶颈分析的基础**，缺失导致所有时效分析只能到任务粒度，无法到节点粒度。

### 4.3 异步历史写入（Flowable 7.x）

Flowable 7.x 支持 `AsyncHistoryListener`：历史事件经消息队列异步写入，不在主事务里，降低同步写历史的性能开销。cmx-flowengine 无此机制（当前历史写在 save_snapshot 同步事务里）。

---

## 五、实例迁移对标

Flowable 的 `ProcessInstanceMigrationBuilder`：
- `mapActivityToActivity(sourceId, targetId)` 节点映射
- `addLocalVariable(name, value)` 迁移时注入变量
- `validateMigrationOfProcessInstance(id)` 干运行（只验证不执行）
- `migrateProcessInstance(id)` 单实例迁移
- 批量迁移所有旧版实例

**cmx-flowengine**：无实例迁移。旧版实例只能按旧定义跑完。长周期审批（年度合同/季度预算）改流程定义后存量实例无法受益，业务痛点明显。

---

## 六、DMN 决策表对标

### 6.1 命中策略覆盖

| 命中策略 | Flowable DMN | cmx-rulesengine |
|---|:---:|:---:|
| UNIQUE（唯一） | ✅ | ✅ |
| FIRST（第一条） | ✅ | ✅ |
| ANY（任意一致） | ✅ | ✅ |
| RULE ORDER（规则顺序） | ✅ | ✅ |
| PRIORITY（优先级） | ✅ | ✅ |
| OUTPUT ORDER（输出顺序） | ✅ | ✅ |
| COLLECT + SUM/MIN/MAX/COUNT | ✅ | ✅ |

cmx-rulesengine 已实现完整 11 种命中策略，覆盖 Flowable DMN 全部 7 种。

### 6.2 FEEL 表达式支持

| FEEL 特性 | Flowable | cmx-rulesengine |
|---|---|---|
| 比较运算符 | ✅（`<3`、`>=10`） | ✅ |
| 区间（`[1..10]`、`(1..10]`） | ✅ | ✅（cmx-rule-feel） |
| not()、in 测试 | ✅ | ✅ |
| 日期/时间字面量 | ✅ | ✅ |
| 字符串函数 | ✅ | ✅（21 内置函数+Rhai 扩展） |
| 完整 FEEL（context/函数定义） | 部分 | 部分（受控 DSL） |

### 6.3 决策需求图（DRD）

| 特性 | Flowable | cmx-flowengine |
|---|:---:|:---:|
| 多决策编排（拓扑顺序求值） | ✅ | ❌ |
| 决策服务（decisionService 分组） | ✅ | ❌ |
| 图形化 DRD 设计器 | ✅（Modeler） | ❌ |

**cmx-flowengine 补充建议**：DRD 是复杂审批矩阵分层建模的关键（"金额决策依赖部门级别决策"）。中优先级，可在 cmx-rulesengine 层实现。

---

## 七、表单引擎对标

### 7.1 字段类型

| 字段类型 | Flowable Form Engine | cmx-flowengine |
|---|:---:|:---:|
| 文本/多行文本 | ✅ | ✅（VarSchema String） |
| 整数/小数 | ✅ | ✅（Number） |
| 日期/日期时间 | ✅ | ✅（Date） |
| 布尔值（复选框） | ✅ | ✅（Boolean） |
| 下拉（枚举/REST 数据源） | ✅ | ✅（Enum） |
| 单选按钮 | ✅ | ❌ |
| 文件上传 | ✅（接 Content Engine） | ❌ |
| 人员选择器 | ✅（people 类型） | ✅（IAM 联动选择器） |
| 表达式（计算字段） | ✅ | ❌ |
| 内容字段 | ✅（接 Content Engine） | ❌ |

### 7.2 表单功能

| 功能 | Flowable | cmx-flowengine |
|---|:---:|:---:|
| 独立 Form Engine + 版本化 | ✅ | ❌（formKey 是外部引用，无内建 Form Engine） |
| submitStartFormData（发起时提交表单） | ✅ | ✅（StartReq.variables） |
| submitTaskFormData（办理时一步提交） | ✅ | ✅（complete_task） |
| 校验（必填/长度/范围/正则） | ✅（表单级） | ✅（VarSchema，进程级） |
| 结果按钮（Approve/Reject → 驱动网关） | ✅ | ✅（LastDecision 变量驱动排他网关） |
| 表单版本与定义分离 | ✅ | 未完全分离（formKey 绑定于流程定义） |

---

## 八、运维与可观测性对标

### 8.1 Flowable Admin App 对比 cmx-flowengine 运维台

| 功能 | Flowable Admin App | cmx-flowengine |
|---|:---:|:---:|
| 实例列表（多条件过滤） | ✅ | ✅（GET /instances） |
| 单实例 Token 位置可视化 | ✅（高亮流程图） | ❌（仅 API 返回 token 位置） |
| 单实例变量查看/修改 | ✅ | ✅（set-variables API） |
| 单实例活动历史时间线 | ✅ | ❌（无活动历史表） |
| Job 列表（Timer/Async/Deadletter） | ✅ | 仅定时器 |
| 死信 Job 手动重试/删除 | ✅ | ❌（无死信队列） |
| Deployment 管理 | ✅ | 仅 API |
| 节点耗时分析（SLA 看板） | ✅（依赖活动历史） | ✅（/analytics/node-timing，依赖 hi_task） |
| Incident 视图 | ✅ | ✅（Incident token 态可查） |
| 批量操作（改派/取消/迁移） | ✅（Enterprise 完整版） | ❌ |

---

## 九、缺失项优先级汇总

### P 级（生产硬门槛，不补不可上生产）

| # | 缺失项 | 影响 |
|---|---|---|
| P1 | **异步 Job Executor**（+SKIP LOCKED 集群安全） | serviceTask 慢调用阻塞引擎；无集群 HA |
| P2 | **Job 重试 + 死信队列** | 失败 Job 无托底，数据会丢 |
| P3 | **消息订阅持久化**（`cmx_flow_message_subscription` 表） | 重启后 MessageCatchEvent 等待实例丢订阅 |
| P4 | **单实例 Token 位置可视化**（运维台） | 出问题无法诊断"卡在哪"，运维盲区 |

### A 级（审批赛道完整度，补了才算"完整审批引擎"）

| # | 缺失项 | 审批场景价值 |
|---|---|---|
| A1 | **中间定时等待事件**（`intermediateCatchEvent` timer） | "等 3 天后自动提醒/推进" ⭐⭐⭐ |
| A2 | **消息启动事件**（`startEvent` message） | "外部系统触发发起审批" ⭐⭐⭐ |
| A3 | **事件子流程**（`eventSubProcess`） | "随时可触发的异常/撤单处理" ⭐⭐ |
| A4 | **定时器变量表达式**（`${due_date}` 动态截止日期） | 截止日期从流程变量读取 ⭐⭐⭐ |
| A5 | **timeCycle + timeDate 定时器** | 定期发起/指定时间到期 ⭐⭐ |
| A6 | **活动历史表**（`cmx_flow_hi_activity`） | 节点级时效分析/审计回放/SLA 看板 ⭐⭐⭐ |
| A7 | **外部 Worker Task**（pull-based 异步） | 多语言外部服务集成，现代集成范式 ⭐⭐⭐ |
| A8 | **Error 边界事件 + Error 结束事件** | serviceTask 抛出业务异常被捕获处理 ⭐⭐ |
| A9 | **实例迁移**（最小版：节点映射） | 存量实例应用新流程定义 ⭐⭐ |
| A10 | **事件网关**（event-based gateway） | "等审批 or 等超时，谁先到走谁" ⭐⭐ |

### B 级（世界级完整度，中长期）

| # | 缺失项 | 说明 |
|---|---|---|
| B1 | Event Registry（Kafka/RabbitMQ 双向） | 消息队列驱动流程 |
| B2 | 补偿事件（compensation boundary + throw） | 已完成步骤回滚（已打款冲销） |
| B3 | 事件子流程 Error/Escalation 类型 | 异常处理完整性 |
| B4 | 信号事件（broadcast 跨实例） | 跨流程广播通知 |
| B5 | DRD 决策需求图 | 多决策编排 |
| B6 | 表单引擎独立化（版本化+字段类型扩展） | 与流程定义解耦 |
| B7 | 变量历史（`cmx_flow_hi_var`） | 合规审计每次变量变更 |
| B8 | 异步历史写入 | 大流程高并发下历史写入性能 |
| B9 | CMMN 案例管理 | 非线性动态审批场景 |
| B10 | Content Engine（文件存储） | 附件/文档管理 |
| B11 | 读路径接 ZmcDataSet（零拷贝） | 大批量待办/实例列表减少中间序列化开销 |
| B12 | 复杂网关（Complex Gateway） | 几乎无工程用例，不建议追 |

### cmx-flowengine 相对 Flowable 的优势点（勿丢）

| 优势 | 说明 |
|---|---|
| **db-per-tenant 物理隔离** | Flowable 是单库 tenant_id 列（逻辑隔离），cmx 是物理隔离，数据更安全 |
| **关系型候选人解析** | OrgLeader/Initiator/InitiatorLeader 7 种候选人类型，Flowable 只有用户/群组 |
| **子流程维度路由泛化** | 任意字典维度路由（RD0-RD4），Flowable 无此机制 |
| **一芯多壳部署** | 同代码内嵌平台或独立微服务或无头契约，Flowable 是独立 Spring Boot 应用 |
| **Web Component 前端** | 框架无关，可嵌入任意宿主，Flowable UI 是 AngularJS 单体应用 |
| **Rust 性能与内存安全** | 无 GC 停顿，内存安全，Flowable JVM 有 GC 开销 |
| **SSE 实时事件流（租户隔离）** | Flowable 靠轮询或 WebSocket，cmx 有 per-tenant broadcast SSE |
| **OpenAPI + 无头契约** | Flowable 的 REST 是 Swagger 2.0，cmx 是 utoipa OpenAPI，设计更干净 |

---

## 十、补充方案路线

### 近期（P 级，生产化）

**目标**：消除生产不可上的风险点。

1. **异步 Job Executor**：`cmx_flow_job` 表加 `locked_by`/`lock_expires` 列，SKIP LOCKED 抢占，serviceTask 异步执行 + 超时退避，不阻塞 `run_to_wait`。
2. **死信队列**：`cmx_flow_deadletter_job` 表，Job 耗尽重试 → 移入，运维台可见可手动重试/删除。
3. **消息订阅持久化**：新增 `cmx_flow_message_subscription`（instance_id, execution_id, message_name, correlation_vars, tenant_id），引擎重启后恢复等待中的 MessageCatchEvent。
4. **单实例 Token 可视化**：ops-console 增加"当前令牌位置+高亮流程图"视图（前端用 bpmn-js overlay API，后端无新表，用现有 token/instance API）。

### 中期（A 级，审批赛道完整度）

**目标**：把审批引擎做到 Flowable 人本层同等完整度。

1. **中间定时等待事件**：复用已有 Timer 基建（Clock 注入 + find_due_jobs），新增 `intermediateTimerEvent` 节点类型，token 到达 → 建 Job，Job 到期 → token 前进。
2. **消息启动事件**：`startEvent` 加 `messageEventDefinition`，`RuntimeService` 加 `startProcessByMessage(name, tenantId, vars)` API，查已部署定义的消息启动订阅。
3. **定时器表达式变量**：定时器 `timeDuration`/`timeDate` 支持 `${expr}` 求值（复用条件表达式引擎），读取实例变量里的截止日期。
4. **timeCycle + timeDate**：手写解析器已有 `duration.rs`，扩展支持 ISO-8601 重复格式和绝对时间点。
5. **活动历史表**：新增 `cmx_flow_hi_activity`（instance_id, activity_id, name, type, start_time, end_time, duration_ms, assignee, tenant_id），每个节点进出时记录一行，驱动节点级 SLA 看板。
6. **外部 Worker Task**：`flowable:type="external-worker"` 对应新 NodeKind `ExternalWorkerTask`，token 到达 → 建一个 Job（topic+lockDuration），worker 轮询 `GET /external-worker/jobs?topic=xxx`，完成后 `POST /external-worker/jobs/{id}/complete`。
7. **Error 边界事件**：`BoundaryErrorEvent` 节点类型，serviceTask 抛出 `BpmnError(errorCode)` → 捕获最近 error boundary → token 走出错边。
8. **事件子流程**（最小版）：`eventSubProcess` with `errorStartEvent`，serviceTask 失败 → 触发事件子流程处理（撤单/降级）。
9. **实例迁移**（最小版）：`MigrationSpec { source_def_id, target_def_id, activity_mappings }` + `validate_migration` + `migrate_instance`，重写 token 的 `node_bpmn_id` 按映射表，实例绑到新定义版本。
10. **事件网关**：`EventBasedGateway` token 到达后等待第一个触发的事件（Timer Job 或 MessageSubscription），命中者 token 前进，其余取消。

### 长期（B 级，世界级完整度）

**目标**：向 Flowable 完整度靠拢，按业务诉求择机实施。

按 B1→B10 顺序，优先 B7（变量历史，合规高频）和 B5（DRD，复杂审批矩阵）。

---

## 十一、已知能力边界专项剖析与落地方案

本节将 cmx-flowengine 在技术方案文档中已公开声明的五项能力边界逐一做横向剖析，并给出具体落地建议。这五项不是隐藏缺陷，而是明确的设计决策——但在与 Flowable 对标时，每一项都有对应的可操作补充路径。

---

### 11.1 事件系统：仅边界定时器 + 消息捕获

**现状**：cmx-flowengine 的事件支持仅有两类——中断/非中断边界定时器（M2.5 实现）和消息捕获事件（`MessageCatchEvent` token 态 + `/messages/correlate` 端点，但无持久化订阅表）。

**缺失全谱**：

| 事件类别 | Flowable 支持 | cmx 现状 | 审批场景频率 |
|---|:---:|:---:|---|
| 边界定时器（中断/非中断） | ✅ | ✅ | 极高 |
| 消息捕获（边界/中间/启动） | ✅ | ⚠️ 部分（无订阅持久化） | 高 |
| 信号事件（broadcast 跨实例） | ✅ | ❌ | 中 |
| 错误事件（boundary/end/eventSubProcess） | ✅ | ❌ | 高（serviceTask 异常托底） |
| 补偿事件（已完成活动回滚） | ✅ | ❌ | 中 |
| 升级事件（escalation） | ✅ | ❌ | 中 |
| 条件事件（变量条件触发） | ✅ | ❌ | 低 |
| 链接事件（大流程页面跳转） | ✅ | ❌ | 低 |

**核心痛点**：消息捕获有逻辑实现但无持久化，服务重启后等待中的 `WaitingMessage` 令牌失去订阅锚点——这是 P 级缺陷（P3）。信号和错误事件的缺失使跨实例广播和 serviceTask 异常捕获无法建模。

**落地方案**：

- **P 级（消息订阅持久化）**：新增 `cmx_flow_message_subscription` 表（`instance_id, execution_id, message_name, correlation_vars JSONB, tenant_id, created_at`）。`WaitingMessage` 令牌写入时同步插入一行，令牌被唤醒或取消时删除。引擎启动时从表中恢复活跃订阅。关键约束：`(message_name, tenant_id)` 建联合索引，相关性变量匹配走 jsonb 包含查询。
- **A 级（错误边界事件）**：`BoundaryErrorEvent` 映射新 `NodeKind`，serviceTask 的 `JavaDelegate` 改为返回 `Result<Outcome, BpmnError(code, message)>`。引擎在 execute_service_task 处捕获 BpmnError → 查找当前节点的错误边界 → 命中则令牌走错误出边，否则转 Incident。错误码匹配支持精确和通配符（`*`）。
- **B 级（信号事件）**：信号是跨实例广播，需要 `cmx_flow_signal_subscription` 表 + 广播端点 `POST /signals/throw/{name}`，遍历所有等待该信号的令牌一次性唤醒。多租户下信号隔离于同一租户内。优先级低于消息，信号缺少相关性，语义较弱。
- **补偿/升级/条件**：这三类在审批赛道用量极少，建议 B 级长期择机，不进近期路线。

---

### 11.2 网关：无事件网关、复杂网关

**现状**：支持三类网关——排他（XOR）、并行（AND）、包容（OR）。缺失事件网关（Event-based Gateway）和复杂网关（Complex Gateway）。

**差距影响**：

- **事件网关**：「等审批 or 等超时，谁先到走谁」是审批场景的**高频诉求**。当前只能用并行网关 + 回收令牌的 workaround（多条件辐射）——但这不是正确的 BPMN 语义，且无法精确模拟「第一个到达的事件决定路径」。事件网关是 A 级缺失（A10）。
- **复杂网关**：由 JavaScript/Groovy 脚本驱动的条件判断，现实中几乎无人使用，Flowable 自身也不推荐。可列为 B12 但不建议追。

**落地方案（事件网关，A 级）**：

事件网关的实现需要与事件系统协同：
1. 新增 `EventBasedGateway` 节点类型，token 到达时不立即分叉，而是在所有出边对应的事件上注册等待（TimerJob 或 MessageSubscription）。
2. 第一个事件触发时，令牌走该分支并**取消**其余分支的等待（删除对应 TimerJob / MessageSubscription 行）。
3. 前置依赖：需要先完成 P3（消息订阅持久化），事件网关依赖消息和定时器两类等待态的幂等取消机制。
4. 设计器侧：bpmn-js 已支持事件网关图形，属性面板无需额外配置，只需后端解析 `eventBasedGateway` 标签。

**复杂网关**（B12，不建议）：条件逻辑已被包容网关 + FEEL/DSL 条件表达式覆盖，不单独追。

---

### 11.3 规则：DMN 子集 + 手写受控 DSL，非完整 FEEL/DMN

**现状**：cmx-flowengine 内建两套条件求值——BPMN 流转条件走手写递归下降 DSL（支持 21 个内置纯函数，不含时间函数），决策表走轻量 `DecisionTable` IR（11 种命中策略，FEEL 区间子集）。如需完整规则能力，配套 cmx-rulesengine。

**与 Flowable DMN 的差距**：

| 能力 | Flowable DMN 1.3 | cmx 内建 | 配 cmx-rulesengine |
|---|:---:|:---:|:---:|
| 11 种命中策略 | ✅ | ✅（子集）| ✅ 全量 |
| 完整 FEEL 表达式 | ✅ | ❌ DSL 子集 | ✅ |
| 决策需求图（DRD） | ✅ | ❌ | ❌（JDM 决策图，非 DRD） |
| 决策服务分组 | ✅ | ❌ | ❌ |
| 图形化 DMN 设计器 | ✅ | ❌ | ✅（规则引擎 F3 设计器） |
| 定时器变量表达式（`${due_date}`） | ✅ | ❌ | — |
| 流程变量读写 | ✅ | ✅ | ✅（BusinessRuleTask 结果写回） |

**设计意图说明**：内建 DSL **刻意不含时间函数**（时间只经可注入 `Clock` 进入）以保持确定性可测；内建决策表不追 FEEL 完整度而走受控子集，边界清晰、可审计。这是正确的设计取舍——审批场景 80% 的规则用简单比较和逻辑运算就够了，20% 复杂规则配 cmx-rulesengine 解决。

**落地方案**：

- **定时器变量表达式（A4，高价值）**：在 `duration.rs` 里，timeDuration 解析时检测 `${...}` 包裹，若存在则走现有条件 DSL 对实例变量求值得出 ISO-8601 字符串再解析为 Duration。不改 DSL 语法，只在定时器层加一个求值入口。这使「截止日期从变量读取」可用，是高频诉求。
- **BusinessRuleTask + cmx-rulesengine 集成**：`BusinessRuleTask` 的 `decisionRef` 属性走 `HttpDelegate` 调 cmx-rulesengine 的 `/api/rules/v1/{tenant}/evaluate` 端点，结果写回流程变量。这已在架构上可行（cmx-flow-adapters 层扩展即可），无需改内核。
- **DRD**：cmx-rulesengine 有 JDM 决策图（拓扑求值），不是 DMN DRD（基于 decision requirement graph 的 XML 格式），但功能等价。缺口是 cmx-rulesengine 的决策图尚未对接 Flowable 标准 DMN 文件格式的 DRD 序列化。B 级，长期。

---

### 11.4 扩展性：单实例 + PG 轮询，无 SKIP LOCKED / 分布式 poller 协调 / 实例版本迁移

**现状**：引擎假设单写者（single-writer assumption）。定时器用 5s PG 轮询（`find_due_jobs`），无集群抢占保护。serviceTask 同步执行，一个慢外部调用阻塞整个 `run_to_wait` 循环。无实例迁移机制。

**三个子问题的差距与方案**：

**① 集群 HA / 分布式 poller（P1）**

Flowable 的 `DefaultAsyncJobExecutor` 用 `SELECT ... FOR UPDATE SKIP LOCKED` 确保多节点只有一个 worker 获取同一个 Job，天然集群安全。cmx 当前的轮询逻辑：

```
find_due_jobs → 取出一批到期 Job → 逐个执行
```

多节点部署下同一 Job 会被多个节点同时取出并执行——对 serviceTask 是幂等问题，对定时器是重复推进问题。

**落地方案**：`cmx_flow_job` 表加两列：`locked_by VARCHAR`（锁定节点 id）和 `lock_expires TIMESTAMPTZ`。轮询时：`UPDATE cmx_flow_job SET locked_by=$node_id, lock_expires=now()+$timeout WHERE id IN (SELECT id FROM ... FOR UPDATE SKIP LOCKED LIMIT $n)`——利用 PG 的 SKIP LOCKED 保证每个 Job 只被一个节点锁定。执行完成后清除 `locked_by`；锁过期未清的 Job（节点崩溃）由 reaper 定时重置。这不需要引入 ZooKeeper / etcd，纯 PG 实现集群安全。

**② 异步 Job Executor / serviceTask 不阻塞引擎（P1 延伸）**

当前 serviceTask 在 `run_to_wait` 同步执行 HttpDelegate，慢调用（网络超时、下游故障）会阻塞整个推进循环。

**落地方案**：`flowable:async="true"` 属性已在 BPMN 语义里存在。在 IR 层加 `async: bool` 字段；`execute_service_task` 时若 `async=true`，不直接执行 delegate，而是向 `cmx_flow_job` 表插入一个 `ServiceTaskJob`，令牌暂停在 `WaitingJob` 态，由异步 poller 触发执行。执行完成后唤醒令牌继续推进。这样慢外部调用不占 `run_to_wait` 的同步路径。重试次数和退避策略由 Job 的 `retries` 和 `retry_timer` 字段控制。

**③ 实例版本迁移（A9）**

长周期审批（年度合同）改流程定义后，存量实例仍绑旧版本，无法受益于新定义。

**落地方案（最小版）**：
- 定义 `MigrationSpec { source_definition_id, target_definition_id, activity_mappings: Vec<{source_node, target_node}> }`
- `validate_migration(instance_id, spec)` 干运行：检查所有活跃 token 的 `node_bpmn_id` 是否有映射、目标节点在新定义里存在、类型兼容
- `migrate_instance(instance_id, spec)` 执行：在一个事务里更新 `cmx_flow_instance.definition_id` 指向新版本，按映射表重写所有活跃 token 的 `node_bpmn_id`
- 未映射的活跃 token 默认拒绝迁移（需要 `force: true` 覆盖，并警告）
- 不支持正在执行 Job 的实例（`has_active_jobs` 检查）

---

### 11.5 读路径：DataSet/query_sql，未接零拷贝 ZmcDataSet

**现状**：所有列表接口（待办、实例列表、任务列表）走 `query_sql` + `ZmcDataSet` 投影（手工字段映射到小 JSON），中间有一次 Rust 结构体到 `serde_json::Value` 的序列化。相比 cmx 其他子系统已实现的 `ZmcDataSet` 零拷贝路径，当前实现在**大批量场景**（数千条实例列表）有额外内存开销，但不影响正确性，也不是生产阻断项。

**与 Flowable 的差距**：Flowable 的 MyBatis 映射走强类型 `HistoricTaskInstanceEntity`，序列化在 Jackson 层；cmx 的目标是比 Flowable 更低的内存占用（Rust 无 GC，零拷贝 DataSet），但当前还没有对齐到这一目标。

**影响范围**：

| 接口 | 当前路径 | 目标路径 | 影响等级 |
|---|---|---|---|
| `GET /tasks/my`（我的待办） | query_sql → Vec<TaskRow> → JSON | ZmcDataSet 零拷贝 | B 级（高并发时有收益） |
| `GET /instances`（实例列表） | query_sql → Vec<InstanceRow> → JSON | ZmcDataSet 零拷贝 | B 级 |
| `GET /todos/*`（待办中心） | 手工投影小 JSON | ZmcDataSet | B 级 |
| 单实例详情（`GET /instances/{id}`） | 聚合快照 SELECT 3 次 | 不变（正确且高效） | 不需改 |

**落地方案**：

接入 `ZmcDataSet` 需要：
1. 在 `cmx-flow-store-pg` 里，把列表查询从 `query_sql → Vec<T> → serde_json` 改为直接构造 `ZmcDataSet` schema + 行数据（列名数组 + 行值二维数组），绕过中间结构体。
2. handler 层把 `ZmcDataSet` 包进现有信封 `{code:0, data: {...}}`，保持前端 API 契约不变。
3. 关键依赖：`cmx-flow-app` 当前不依赖 `cmx-rowsource`（零拷贝层），接入要在 `cmx-flow-store-pg` 引入这个 crate 依赖——需确认不引入 cmx-api 方向的依赖环。
4. 优先级策略：先接最重的 `/todos/my`（I/O 最大、并发最高），其余可按需逐步迁移。这是可以独立分批推进的局部优化，不影响任何功能语义。

**结论**：这是 B 级性能优化，正确性无风险。在 P/A 级缺陷全部补齐之后，高并发场景下这项优化可以提供有意义的内存和吞吐量收益。

---

### 11.6 五项边界的优先级与依赖关系

```
P 级（阻产）
  └─ P1 异步 Job Executor + SKIP LOCKED ← §11.4①②
  └─ P2 Job 重试 + 死信队列
  └─ P3 消息订阅持久化表 ← §11.1（消息捕获持久化）
  └─ P4 Token 位置可视化

A 级（审批完整度）
  └─ A1 中间定时等待（依赖 §11.1 定时器 + §11.4 异步 Job）
  └─ A2 消息启动事件（依赖 P3 消息订阅）
  └─ A4 定时器变量表达式 ← §11.3（DSL 求值扩展）
  └─ A8 Error 边界事件 ← §11.1（错误事件落地）
  └─ A10 事件网关 ← §11.2（依赖 P3 + A1/A2 已就绪）
  └─ A9 实例迁移 ← §11.4③

B 级（长期）
  └─ B11 读路径 ZmcDataSet ← §11.5
  └─ B12 复杂网关（不建议追）← §11.2
```

关键依赖链：**P3（消息订阅持久化）→ A2（消息启动）→ A10（事件网关）**。事件网关是 A 级里依赖最多的单项，建议把它放在 A 级末尾，P3 和 A1 就绪后再做。

---

## 附录：对标源文档

- Flowable 6.7 / 7.x 官方文档、GitHub 源码（flowable/flowable-engine）
- cmx-flowengine 源码核对（v0.1.12，`crates/cmx-flow-model/src/ir.rs`，`engine.rs` 等）
- 现有差距分析：`docs/flow-engine-gap-analysis-vs-world-class.md`（2026-08-13）

---

*本文档只做分析，不动代码。cmx-flowengine v0.1.12 · 2026-08-19*
