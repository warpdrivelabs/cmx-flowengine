# 02 · 主流程定义

本篇讲**如何用 BPMN 2.0 XML 定义一条主流程**：支持的全部元素、每个元素读哪些属性、如何部署/发布/管理版本。子流程见 [03](03-subprocess.md)，分支条件见 [05](05-conditions-and-decisions.md)，审批人（候选人）见 [04](04-organization-and-identity.md)。

## 2.1 BPMN 是唯一交换格式

引擎把 BPMN 2.0 XML **编译**成中立 IR（`ProcessDefinition`），运行期不再接触 XML：

```
BPMN XML ──compile()──► ProcessDefinition(节点数组 + 边) ──deploy()──► 引擎
```

- 一份 `<process>` 编译成一份定义，`key = <process id>`。
- 编译遇到**不支持的活动类元素显式报错**（不静默丢弃），保证「能部署 = 能跑」。
- 节点在 IR 里是扁平数组（`NodeId` = 数组下标），但持久化只认 `bpmn_id`（稳定锚点）。

### 命名空间：前缀无关

元素和扩展属性都按**本地名**匹配，前缀随意：

- 元素：`<bpmn:userTask>`、`<userTask>`、`<bpmn2:userTask>` 等价。
- 扩展属性：`flowable:assignee`、`camunda:assignee`、`cmx:assignee`、无前缀 `assignee` 等价。

本文示例统一用 `bpmn:` + `flowable:` + `cmx:`（`cmx` 用于引擎私有扩展）。根元素恒为 `<definitions>`。

## 2.2 一份最小可跑流程

```xml
<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"
                  xmlns:flowable="http://flowable.org/bpmn"
                  targetNamespace="http://cmx/flow">
  <bpmn:process id="leave_request" name="请假审批" isExecutable="true">
    <bpmn:startEvent id="start" name="发起"/>
    <bpmn:sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <bpmn:userTask id="review" name="经理审批" flowable:assignee="manager"/>
    <bpmn:sequenceFlow id="f2" sourceRef="review" targetRef="done"/>
    <bpmn:endEvent id="done" name="结束"/>
  </bpmn:process>
</bpmn:definitions>
```

- 有且仅有一个 `startEvent`（none-start）。
- 每个节点必须有 `id`；`sequenceFlow` 必须有 `sourceRef`/`targetRef`（悬挂引用报 `DanglingReference`）。

## 2.3 `<process>` 根

| 属性 | 必填 | 说明 |
|------|------|------|
| `id` | ✅ | 成为定义 `key`（如 `leave_request`） |
| `name` | | 展示名 |
| `isExecutable` | | 一个 definitions 里有多个 process 时，优先选 `="true"` 的 |
| `startFormKey`（如 `cmx:startFormKey`） | | 发起表单绑定 key |

> 选取规则：扫描 `<definitions>` 下所有 `<process>`，返回第一个 `isExecutable="true"`，否则第一个。

## 2.4 节点元素总表

| BPMN 元素 | IR NodeKind | 是否等待态 | 说明 |
|-----------|-------------|-----------|------|
| `startEvent` | `StartEvent` | 否 | 起点，恰好一个 |
| `endEvent` | `EndEvent` | 否 | 终点，可多个 |
| `endEvent` + `<terminateEventDefinition>` | `TerminateEndEvent` | 否 | 终止型终点（一票否决全流程） |
| `userTask` | `UserTask` | **是** | 用户任务（可会签/或签，可挂边界定时器） |
| `serviceTask` | `ServiceTask` | 否 | 服务任务（调 delegate；失败转 Incident） |
| `businessRuleTask` | `BusinessRuleTask` | 否 | 业务规则任务（跑决策表） |
| `exclusiveGateway` | `ExclusiveGateway` | 否 | 排他网关（择一） |
| `parallelGateway` | `ParallelGateway` | 否 | 并行网关（AND fork/join） |
| `inclusiveGateway` | `InclusiveGateway` | 否 | 包容网关（OR fork/join） |
| `boundaryEvent` + timer | `BoundaryTimerEvent` | 否 | 边界定时器（超时升级/催办） |
| `intermediateCatchEvent` + message | `MessageCatchEvent` | **是** | 消息捕获（等外部回调） |
| `callActivity` | `CallActivity` | **是** | 调用子流程（见 03） |
| `subProcess` | `SubProcess`（编译期扁平化） | 否 | 嵌入子流程（见 03） |

> 等待态（`is_wait_state`）只有 `UserTask` 与 `CallActivity` 会外化成可查询的等待；`MessageCatchEvent` 令牌停 `WAITING_MESSAGE`。

## 2.5 事件

### startEvent / endEvent

```xml
<bpmn:startEvent id="start" name="提交申请"/>
<bpmn:endEvent id="done" name="审批完成"/>
```

只读 `id` / `name`。当前只支持 none-start（无消息/定时器起始）。

### 终止型终点 TerminateEndEvent

不是独立标签，而是 `<endEvent>` 内含 `<terminateEventDefinition>`：

```xml
<bpmn:endEvent id="reject" name="驳回终止">
  <bpmn:terminateEventDefinition/>
</bpmn:endEvent>
```

语义：**一票否决**——令牌到达即终止整个实例（结束所有令牌、作废所有待办、清候选人/定时器/多实例域），实例进 `COMPLETED`。用于「任一分支拒绝 → 整单结束」。

### 边界定时器 BoundaryTimerEvent

挂在 userTask 上，超时触发。见 §2.9。

### 消息捕获 MessageCatchEvent

`intermediateCatchEvent` + `messageEventDefinition`，令牌停在此等外部消息唤醒。详见 [08 §消息相关](08-external-integration.md)：

```xml
<bpmn:intermediateCatchEvent id="wait_ext" cmx:correlationVar="orderId">
  <bpmn:messageEventDefinition messageRef="verdictReceived"/>
</bpmn:intermediateCatchEvent>
```

| 读取项 | 来源（按优先级） |
|--------|----------------|
| 消息名 | `messageEventDefinition@messageRef` → 事件 `@message` → 事件 `@name` → 事件 `id` |
| 相关变量 | 事件 `@correlationVar`（如 `cmx:correlationVar`），非空才记 |

## 2.6 用户任务 userTask（最核心）

```xml
<bpmn:userTask id="review" name="部门审批"
    flowable:assignee="u_1001"
    flowable:candidateUsers="u_1001,u_1002"
    flowable:candidateGroups="finance"
    cmx:candidates="role(finance), position(cfo), orgLeader"
    cmx:cc="user(u_9)"
    cmx:formKey="review_form" cmx:formMode="approve" cmx:formFields="amount,remark">
  <!-- 可选：多实例（会签/或签），见 §2.8 -->
</bpmn:userTask>
```

读取的属性（全部前缀无关）：

| 属性 | 含义 | 详见 |
|------|------|------|
| `assignee` | 静态办理人（写死一个 id） | — |
| `candidateUsers` | 候选用户（逗号分隔 → 候选人 kind=User） | [04](04-organization-and-identity.md) |
| `candidateGroups` | 候选组（逗号分隔 → 候选人 kind=Role） | [04](04-organization-and-identity.md) |
| `candidates`（`cmx:`） | 混合候选人表达式（默认 kind=User） | [04](04-organization-and-identity.md) |
| `cc`（`cmx:`） | 抄送人表达式（默认 kind=User） | [07](07-task-operations.md) |
| `formKey` | 绑定的表单 key | [08](08-external-integration.md) |
| `formMode` | `edit` \| `readonly` \| `approve`（默认 approve） | — |
| `formFields` | 逗号分隔字段名列表 | — |

**办理人解析优先级**（令牌到达时）：

1. 若有候选人表达式（`candidates`/`candidateUsers`/`candidateGroups`）→ 交 `AssigneeResolver` 解析：
   - 解析出 0 人 → 回退静态 `assignee`（宽容降级）。
   - 解析出 1 人 → 直派 `task.assignee`。
   - 解析出 ≥2 人 → 落**候选池**，等某人 `claim` 认领。
2. 否则用静态 `assignee`。

## 2.7 服务任务 serviceTask

```xml
<bpmn:serviceTask id="calc_risk" name="计算风险等级"
                  flowable:delegateExpression="${riskDelegate}"/>
```

delegate key 按顺序取：`delegateExpression` → `class` → `delegate` → `type`；会剥掉外层 `${...}`/`#{...}` 包裹。**四者全无 → 编译报 `Unsupported`。**

- delegate 是可注入外呼实现（Rust `JavaDelegate` 或 `HttpDelegate`），读写实例变量。
- **失败不终止流程**：delegate 返回 `Err` → 令牌转 `INCIDENT` 态、实例仍 `ACTIVE`、原因/重试次数记进 `__incident` 变量，可 `retry_incident` 恢复。详见 [09 §Incident](09-operations-and-admin.md) 与 [08 §serviceTask 外呼](08-external-integration.md)。

## 2.8 业务规则任务 businessRuleTask

```xml
<bpmn:businessRuleTask id="matrix" name="定审批级别"
                       flowable:decisionRef="approval_matrix"/>
```

decision key 按顺序取 `decisionRef` → `decision` → `decisionRefBinding`，须非空（否则 `Unsupported`）。令牌到达时跑对应决策表，把输出写进实例变量，供后续网关分支。决策表定义与注册见 [05 §决策表](05-conditions-and-decisions.md)。

> 宽容语义：决策表未注册 → 不写变量、不硬失败，令牌照常过（后续网关按缺省分支走）。

## 2.9 网关

### 排他网关 exclusiveGateway（择一）

```xml
<bpmn:exclusiveGateway id="gw_amount" name="金额判断" default="f_small"/>
<bpmn:sequenceFlow id="f_big" sourceRef="gw_amount" targetRef="risk">
  <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &gt; 50000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="f_small" sourceRef="gw_amount" targetRef="done"/>
```

- 按出边顺序求值条件，**命中第一条**即走；都不命中走 `default` 指向的边。
- `default` 是网关属性，值 = 缺省边的 `sequenceFlow id`（**不是**在边上写 `default="true"`）。
- 至多一条 `default` 边。都不命中且无 default → 报 `NoOutgoingFlow`。

### 并行网关 parallelGateway（AND）

```xml
<bpmn:parallelGateway id="fork"/>   <!-- >1 出边 → fork：全部分支并发 -->
<bpmn:parallelGateway id="join"/>   <!-- >1 入边 → join：等所有入边令牌到齐 -->
```

- fork：为每条出边发一个令牌（AND 分叉）。
- join：先到的令牌以 `JOINING` 态阻塞落库，等入边令牌全到齐 → 合并为一个幸存令牌继续。这是**结构性阻塞**，无需外部触发。

### 包容网关 inclusiveGateway（OR）

```xml
<bpmn:inclusiveGateway id="forkIncl" default="fc"/>
<bpmn:sequenceFlow id="fa" sourceRef="forkIncl" targetRef="taskA">
  <bpmn:conditionExpression>${amount &gt; 1000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="fb" sourceRef="forkIncl" targetRef="taskB">
  <bpmn:conditionExpression>${urgent == true}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="fc" sourceRef="forkIncl" targetRef="taskC"/>
<bpmn:inclusiveGateway id="joinIncl"/>
```

- fork：走**所有条件命中**的出边（可能多条）；都不命中走 default。
- join：等**所有实际到达的分支**到齐再放行（用可达性判断，不是死等固定数量）。

排他 vs 包容 vs 并行速记：**排他=择一，包容=择若干（按条件），并行=全走（无条件）**。

## 2.10 边界定时器（超时升级 / 催办）

挂在 userTask 上，令牌停在该任务时起一个到期作业：

```xml
<bpmn:userTask id="manager" name="部门经理审批" flowable:assignee="部门经理"/>

<!-- 非中断型：20 秒后发催办，经理任务不中断 -->
<bpmn:boundaryEvent id="remind" attachedToRef="manager" cancelActivity="false">
  <bpmn:timerEventDefinition><bpmn:timeDuration>PT20S</bpmn:timeDuration></bpmn:timerEventDefinition>
</bpmn:boundaryEvent>
<bpmn:sequenceFlow id="r1" sourceRef="remind" targetRef="notify"/>

<!-- 中断型（缺省）：30 秒未办则中断经理审批，升级到总监 -->
<bpmn:boundaryEvent id="timeout" attachedToRef="manager">
  <bpmn:timerEventDefinition><bpmn:timeDuration>PT30S</bpmn:timeDuration></bpmn:timerEventDefinition>
</bpmn:boundaryEvent>
<bpmn:sequenceFlow id="t1" sourceRef="timeout" targetRef="director"/>
```

| 读取项 | 说明 |
|--------|------|
| `attachedToRef` | 宿主 userTask 的 id（必填，缺则 `MissingElement`） |
| `<timerEventDefinition>/<timeDuration>` | ISO 8601 相对时长（必填，缺则 `Unsupported`） |
| `cancelActivity` | `!= "false"` → 中断型（缺省 true）；`false` → 非中断型 |

- **中断型**（cancelActivity=true）：超时中断宿主任务，令牌走定时器出边（如升级到总监）。
- **非中断型**（cancelActivity=false）：超时发一个旁路令牌（如发催办），宿主任务继续等。
- 一个 userTask 可挂多个边界定时器（上例：20s 非中断催办 + 30s 中断升级）。
- 令牌离开宿主（办结/取消/被中断）即撤销其定时器作业。

**时长格式**（`P[nD]T[nH][nM][nS]`，见 [05 §附录](05-conditions-and-decisions.md)）：`PT30S` / `PT10M` / `PT1H` / `PT1H30M` / `P1D` / `P2DT3H4M5S`。大小写不敏感。**不支持** `timeDate`（绝对时刻）/ `timeCycle`（循环）/ 周（`nW`）/ 月年。

**推进机制**：引擎**无后台线程**，定时器靠宿主显式推进——`cmx-flow-server` 内建每 5 秒调一次 `trigger_due_timers`，也可手动 `POST /timers/trigger`。见 [09](09-operations-and-admin.md)。

## 2.11 顺序流与条件

```xml
<bpmn:sequenceFlow id="f1" sourceRef="gw1" targetRef="review">
  <bpmn:conditionExpression>${amount &gt; 10000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
```

- `<conditionExpression>` 文本 = 分支条件表达式（`${...}` 包裹可选）。空条件 = 无条件边，恒通过。
- `is_default` 由源节点的 `default` 属性等于本边 id 决定。
- 条件表达式 DSL（运算符、内置函数、变量路径）详见 [05](05-conditions-and-decisions.md)。

## 2.12 不支持 / 会被拒的元素

编译器对「像活动节点但不支持」的元素**显式报错**（`Unsupported`），不静默忽略：

**黑名单**（直接报 `Unsupported`）：`task`、`scriptTask`、`sendTask`、`receiveTask`、`manualTask`、`eventBasedGateway`、`complexGateway`、`intermediateThrowEvent`、裸 `boundaryEvent`。

其它显式拒绝：

- `boundaryEvent` 非定时器类型（error/message/signal）→ `Unsupported`。
- `boundaryEvent` 定时器但用 `timeDate`/`timeCycle`（无 `timeDuration`）→ `Unsupported`。
- `intermediateCatchEvent` 非消息类型（timer/signal）→ `Unsupported`。
- `serviceTask` 无 delegate/class/delegate/type → `Unsupported`。
- `businessRuleTask` 无 decisionRef/decision/decisionRefBinding → `Unsupported`。
- `subProcess` 挂了块级边界事件 → `Unsupported`（见 [03](03-subprocess.md)）。
- `callActivity` 既无 calledElement 也无 calledKey → `MissingElement`。

> 未知的**非活动**元素（如 `bpmndi` 图形信息、`extensionElements` 里的自定义节点）静默跳过，不影响编译。

## 2.13 定义校验规则

`ProcessDefinition::validate()` 强制：

- 节点非空；`start` 必须指向 `StartEvent`。
- 每条出边 `target` 必须在 arena 内（无悬挂）。
- `exclusiveGateway` 至多一条 `is_default` 边。
- `EndEvent` / `TerminateEndEvent` 不能有出边。

## 2.14 部署、校验、发布、版本管理

生产服务提供完整的定义生命周期端点（信封见 [06](06-rest-api-reference.md)）：

### 校验 BPMN（不落库）

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/definitions/validate \
  -H 'Content-Type: application/json' \
  -d '{"bpmnXml":"<?xml ...>"}'
# 成功 {code:0, data:{valid:true, key:"leave_request"}}
# 失败 {code:0, data:{valid:false, error:"..."}}   ← 注意：软失败仍 HTTP 200
```

### 存草稿（先试编译挡回非法 BPMN）

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/definitions/draft \
  -H 'Content-Type: application/json' \
  -d '{
    "name":"请假审批",
    "domain":"hr", "application":"leave", "module":"approve", "category":"审批类",
    "bpmnXml":"<?xml ...>",
    "updatedBy":"designer"
  }'
# → {code:0, data:{key, name, state, activeVersion}}
```

### 发布（草稿 → 版本 +1，热装载）

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/definitions/leave_request/publish \
  -H 'Content-Type: application/json' \
  -d '{"note":"上线 v2", "publishedBy":"admin"}'
# → {code:0, data:{key, version, hotLoaded:true, note}}
```

> `hotLoaded:true` 表示发布即生效（引擎用 `RwLock` 内部可变，`deploy(&self)` 同 key 覆盖，运行中实例下次按 key 读到新版），**无需重启**。

### 版本管理

| 操作 | 端点 |
|------|------|
| 列全部定义（设计器视图，含版本历史） | `GET /design/definitions` |
| 取定义详情（可带 `?version=N` 看指定版本 XML） | `GET /definitions/{key}` |
| 列版本 | `GET /definitions/{key}/versions` |
| 激活某历史版本 | `POST /definitions/{key}/versions/{version}/activate` |
| 删除某版本 | `DELETE /definitions/{key}/versions/{version}` |
| 列可发起的顶层定义（供发起页下拉，含 `startFormKey`） | `GET /startable` |

### 查已部署定义（画图用）

```bash
curl http://127.0.0.1:8091/api/flow/v1/definitions
# → {code:0, data:{definitions:[
#      {key, name,
#       nodes:[{id, name, kind, multiInstance, boundaryTimer, calledElement}],
#       edges:[{from, to, condition, isDefault}],
#       startable}
#    ]}}
```

## 2.15 完整示例：信用额度审批（credit_approval）

综合 serviceTask + userTask + 排他网关 + 条件边 + default 的真实种子流程：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"
                  xmlns:flowable="http://flowable.org/bpmn"
                  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
                  targetNamespace="http://cmx/flow/demo">
  <bpmn:process id="credit_approval" name="信用额度审批" isExecutable="true">
    <bpmn:startEvent id="start" name="提交申请"/>
    <bpmn:sequenceFlow id="f0" sourceRef="start" targetRef="calc_risk"/>

    <bpmn:serviceTask id="calc_risk" name="计算风险等级"
                      flowable:delegateExpression="${riskDelegate}"/>
    <bpmn:sequenceFlow id="f1" sourceRef="calc_risk" targetRef="manager"/>

    <bpmn:userTask id="manager" name="客户经理初审" flowable:assignee="经理"/>
    <bpmn:sequenceFlow id="f2" sourceRef="manager" targetRef="gw_amount"/>

    <bpmn:exclusiveGateway id="gw_amount" name="金额判断" default="f_small"/>
    <bpmn:sequenceFlow id="f_big" sourceRef="gw_amount" targetRef="risk">
      <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${amount &gt; 50000}</bpmn:conditionExpression>
    </bpmn:sequenceFlow>
    <bpmn:sequenceFlow id="f_small" sourceRef="gw_amount" targetRef="done"/>

    <bpmn:userTask id="risk" name="风控审批" flowable:assignee="风控专员"/>
    <bpmn:sequenceFlow id="f3" sourceRef="risk" targetRef="director"/>
    <bpmn:userTask id="director" name="分行行长审批" flowable:assignee="行长"/>
    <bpmn:sequenceFlow id="f4" sourceRef="director" targetRef="done"/>

    <bpmn:endEvent id="done" name="审批完成"/>
  </bpmn:process>
</bpmn:definitions>
```

拓扑：`提交 → 计算风险(serviceTask) → 客户经理初审 → 金额判断`，大额（>5万）走「风控 → 行长」两级，小额直接结束。

## 2.16 完整示例：报销会签（多实例）

会签（并行多实例）+ 或签（顺序多实例）的定义语法：

```xml
<bpmn:userTask id="finance_sign" name="财务会签" flowable:assignee="${approver}">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="approvers" flowable:elementVariable="approver">
    <bpmn:completionCondition>${nrOfCompletedInstances/nrOfInstances &gt;= 0.5}</bpmn:completionCondition>
  </bpmn:multiInstanceLoopCharacteristics>
</bpmn:userTask>
```

`<multiInstanceLoopCharacteristics>` 读取：

| 项 | 来源 | 含义 |
|----|------|------|
| `isSequential` | 属性 | `true`=顺序（或签，逐个办）；`false`/缺省=并行（会签，齐头并进） |
| 集合 | `collection` 属性 **或** 子元素 `<loopDataInputRef>` 文本 | 展开依据的数组变量名（如 `approvers`） |
| `elementVariable` | 属性 | 每个子任务携带当前元素的变量名（如 `approver`） |
| `<completionCondition>` | 子元素文本 | 完成条件，命中即提前收口剩余子实例 |

**完成条件内置计数变量**（求值时叠加，不落库）：`nrOfInstances`（总数）、`nrOfCompletedInstances`（已完成）、`nrOfActiveInstances`（进行中）。

常用完成条件：

- `${nrOfCompletedInstances/nrOfInstances >= 0.5}` —— 过半通过（会签）。
- `${rejected == true}` —— 任一驳回即终止（或签一票否决）。

发起时把集合作为变量传入：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"expense_countersign",
       "variables":{"applicant":"李四","amount":30000,
                    "approvers":["u1","u2","u3"]}}'
# 财务会签节点按 approvers 展开成 3 个并行待办；办结 2 个（过半）即通过。
```

## 2.17 完整示例：限时审批（边界定时器）

见 §2.10 的 `timed_approval`——经理审批挂 20s 非中断催办 + 30s 中断升级两个边界定时器。

---

上一篇 ← [01 概述与架构](01-overview-and-architecture.md) ｜ 下一篇 → [03 子流程定义](03-subprocess.md)
