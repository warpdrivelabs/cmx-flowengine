# 流程引擎能力增强方案（四项）

> 状态：**方案稿，不动代码**。针对四项诉求给出现状核对、设计方案、数据/接口契约、落地步骤、工作量与风险。
> 日期：2026-08-14 ｜ 关联使用文档：[`usage/`](usage/README.md)
>
> 四项诉求：
> 1. **流程设计工作台**节点/边可视化设置不专业、不易用 → 设计器 UX 重做。
> 2. **动态多实例待办**：按表单里动态数量的产品，每个产品派生一个（角色/用户）待办（10 个产品 10 个任务，100 个产品 100 个任务）。
> 3. **退回**：任务可退回上一节点，或**选择任意上游节点**退回。
> 4. **取回（撤回）**：任务尚未被处理时，提交人可直接取回。

---

## 0. 现状核对（逐项对照源码）

先说清楚每项「已有多少、缺多少」，避免重复造轮子。

| 诉求 | 引擎侧现状 | 前端侧现状 | 结论 |
|------|-----------|-----------|------|
| ① 设计器 UX | 定义/发布/版本/校验端点齐全 | bpmn-js + 自研 BFS 自动布局 + 扁平 `data-prop` 属性输入框 | **纯前端体验问题**，后端不动 |
| ② 动态多实例 | `multiInstanceLoopCharacteristics` 已支持（按 `collection` 数组展开 N 个子任务）；但 `push_mi_sub_instance` 把**静态 `assignee` 原样拷给每个子任务**，无 `${...}` 插值、无逐元素候选人解析 | 无 | **半成品**：展开机制在，逐元素派人缺 |
| ③ 退回任意节点 | `reject_task(..., target_bpmn_id: Option, ...)` **已支持显式指定任意 userTask 作为退回目标**；省略则回直接前驱 | task-form 只发默认退回（`reason` 无 `targetBpmnId`），无节点选择器；也没有「哪些节点可退」的查询端点 | **后端就绪**，缺可退节点列举端点 + 选择器 UI |
| ④ 取回/撤回 | **完全没有**。只有 `cancel_process`（终止整个实例，非「拉回一个未办任务」） | 无 | **真缺口**，引擎 + 端点 + UI 全要补 |

> 关键澄清：
> - ③ 不是「引擎不支持」，而是「前端没暴露 + 没有可退节点列举」。工作量集中在前端 + 一个只读端点。
> - ② 的难点不在「展开 N 个」（已支持），而在「让第 i 个子任务派给第 i 个产品对应的角色/用户」。

---

## 一、动态多实例待办（按产品动态派生任务）

### 1.1 需求拆解

单据里有一个**动态长度**的明细数组（如 `products`），要求：

- 每个明细项派生**一个独立待办**（10 项 → 10 个，100 项 → 100 个）。
- 每个待办的**办理人由该明细项决定**：可能是「该产品负责角色」`role(该产品.ownerRole)`，或「该产品负责人」`user(该产品.ownerUser)`。
- 收口策略可配：全部办完 / 过半 / 任一驳回即止。

这正是 BPMN **多实例（multi-instance）** 的语义，引擎已具备展开骨架，缺的是**逐元素办理人解析**。

### 1.2 现状机制（已有的部分）

```xml
<bpmn:userTask id="product_review" name="产品审核" flowable:assignee="${approver}">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="products" flowable:elementVariable="product">
    <bpmn:completionCondition>${nrOfCompletedInstances == nrOfInstances}</bpmn:completionCondition>
  </bpmn:multiInstanceLoopCharacteristics>
</bpmn:userTask>
```

引擎令牌到达时：读 `products` 数组 → 按长度展开 N 个子任务，每个子任务用 `element_value` 携带对应的 `product` 元素。**但每个子任务的 `assignee` 现在是把定义里的静态字符串（含未求值的 `${approver}` 字面量）原样拷贝**——即 10 个待办的办理人全是字面量 `${approver}`，不会按产品分派。这是 gap。

### 1.3 方案：多实例逐元素办理人解析

在子任务展开时（`push_mi_sub_instance`），对**每个元素**求值办理人，支持三种写法：

**写法 A —— 元素字段插值（最直观）**

```xml
<bpmn:userTask id="product_review" name="产品审核"
    flowable:assignee="${product.ownerUser}">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="products" flowable:elementVariable="product"/>
</bpmn:userTask>
```

- `product` = 当前元素（`elementVariable`）。展开第 i 个时，把 `product` 注入求值作用域，`${product.ownerUser}` 求值成该产品负责人 id → 作 assignee 直派。

**写法 B —— 候选人表达式逐元素解析（派给角色/岗位/组织，可多人认领）**

```xml
<bpmn:userTask id="product_review" name="产品审核"
    cmx:candidates="role(${product.ownerRole})">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="products" flowable:elementVariable="product"/>
</bpmn:userTask>
```

- 每个子任务把 `role(${product.ownerRole})` 先插值成 `role(fin_reviewer)`，再交 `AssigneeResolver` 解析成用户集（0→静态兜底，1→直派，≥2→候选池待认领）。

**写法 C —— 集合即为办理人列表（最简单，退化情形）**

```xml
<!-- approvers = ["u1","u2","u3"]，每人一个待办 -->
<bpmn:userTask id="sign" flowable:assignee="${approver}">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="approvers" flowable:elementVariable="approver"/>
</bpmn:userTask>
```

这是当前 demo `expense_countersign` 想表达的写法（`assignee="${approver}"`），**今天实际不生效**（无插值），本方案一并修复。

### 1.4 求值作用域（关键设计）

展开第 i 个子实例时，办理人表达式的求值上下文 = **实例变量 + 一个临时叠加层**：

| 注入变量 | 值 |
|----------|-----|
| `<elementVariable>`（如 `product`） | 当前元素（数组第 i 项，可以是对象/字符串/数字） |
| `loopCounter` | 当前下标 i（0 基，业界惯例，附带） |

- 复用已有表达式引擎 `eval_condition` 的求值内核，但办理人求值需要「返回字符串/求值结果」而非布尔——扩展一个 `eval_value(expr, vars) -> Value`（表达式引擎已能算出中间 `Value`，只是目前只对外暴露布尔入口）。
- 插值发生在**展开时**（`push_mi_sub_instance`），元素已定格快照（现状：集合在进入 MI 节点时已快照到 `MiScope.collection`，避免中途变量被改），逐元素求值天然确定。

### 1.5 数据流示例（10 个产品）

```
单据提交，变量 products = [
  {"sku":"A","ownerRole":"fin_a"},
  {"sku":"B","ownerRole":"fin_b"},
  ... 共 10 项
]
        │
令牌到 product_review（多实例，collection=products）
        │  读 products → 长度 10 → 展开 10 个子任务
        ├─ 子任务1：element_value={sku:A,...}，assignee=解析 role(fin_a)
        ├─ 子任务2：element_value={sku:B,...}，assignee=解析 role(fin_b)
        │  ...
        └─ 子任务10：...
        │  各自独立办理，element_value 里带着自己的产品数据
        │  completionCondition 命中（如全办完）→ 收口 → 令牌沿出边继续
```

100 个产品同理展开 100 个，无需改定义——**数量由单据数据决定，不写死在流程图里**。

### 1.6 前端配合（设计器 + 待办中心）

- **设计器**：userTask 属性面板增「多实例」分组——开关（会签/或签）、集合变量名、元素变量名、办理人表达式（带 `${elementVar.field}` 提示）、完成条件（下拉常用模板：全部/过半/任一驳回）。见第四节 UX。
- **待办中心**：多实例子任务在列表里展示各自的 `element_value`（如产品 SKU/名称），让办理人看到「我这条是审哪个产品」。字段已在 `Task.element_value`，前端渲染即可。

### 1.7 落地范围与工作量

| 层 | 改动 | 说明 |
|----|------|------|
| `cmx-flow-model/expr.rs` | 加 `eval_value` 公开入口 | 复用现有求值内核，只是暴露非布尔结果 |
| `cmx-flow-engine/engine.rs` | `push_mi_sub_instance` 逐元素求值 assignee + 逐元素候选人解析 | 核心改动；解析需 async（候选人解析是 async），涉及展开循环签名调整 |
| `cmx-flow-bpmn` | 无需改 | `collection`/`elementVariable`/`assignee`/`candidates` 已解析 |
| 前端设计器 | 多实例属性分组 | UX |
| 前端待办中心 | 展示 element_value | 小改 |
| 测试 | 逐元素派人 E2E（角色/用户/插值三写法 + 空集合 + 100 项压测） | 对齐现有 m3 测试风格 |

**工作量**：引擎 2–3 天（含 async 展开重构 + 测试），前端 1 天。**风险**：中——展开循环从同步拷贝变 async 逐元素解析，要保证空集合、单元素、解析失败（0 人兜底）等边界不回归；元素为标量（非对象）时 `${product}` 直接取值、`${product.field}` 返 null 的语义要测。

---

## 二、退回：上一步 / 任意上游节点

### 2.1 现状

引擎 `reject_task` **早已支持退回任意 userTask**：

```rust
reject_task(instance_id, task_id, from_user,
            target_bpmn_id: Option<&str>,   // ← Some("任意上游userTask") 即可
            reason, variables)
```

- `Some(节点)`：校验是 userTask → 令牌回退到该节点重开待办。
- `None`：回退到**直接前驱** userTask（沿入边回溯，跳过网关；多前驱则报错要求显式指定）。

前端 task-form 现在只发 `None`（「退回上一步」按钮，body 无 `targetBpmnId`）。所以「退回任意节点」= **补前端 + 补一个「可退节点」列举端点**，引擎主体不动。

### 2.2 方案

**新增只读端点：列举某任务的可退回目标**

```
GET /api/flow/v1/tasks/{taskId}/reject-targets?instanceId=<iid>
→ { code:0, data:{ targets:[
      { bpmnId:"mgr", name:"经理审批", isDirectPredecessor:true,  distance:1 },
      { bpmnId:"apply", name:"申请填报", isDirectPredecessor:false, distance:2 }
    ] } }
```

计算逻辑（引擎/应用层新增一个只读辅助，不改运行态）：

- 从当前任务节点沿**入边反向遍历**（跳过网关/事件），收集所有可达的**上游 userTask**。
- `isDirectPredecessor` 标记默认目标（即 `None` 时会去的那个），前端默认选中它。
- `distance` = 反向跳数，供前端按「上 1 步 / 上 2 步」排序展示。

> 注意环路：反向遍历要去重 + 深度上限（引擎 `predecessor_user_task` 已有 64 层 guard，可复用同思路），避免退回目标含自身或死循环。

**前端 task-form 退回交互升级**

- 「退回」按钮从单一动作变成两态：
  - 快捷「退回上一步」（默认目标，一键，保持现习惯）。
  - 「退回到…」下拉/弹窗：拉 `reject-targets`，列出全部可退上游 userTask（带节点名 + 距离），选中一个 + 填意见 → 发 `reject` 带 `targetBpmnId`。

```
┌ 审批控制台 ─────────────────┐
│  [同意办结]  [驳回]           │
│  退回 ▾                       │
│   ├─ ⤺ 退回上一步（经理审批）  │  ← 默认，一键
│   ├─ ⤺ 退回到 申请填报         │
│   └─ ⤺ 退回到 …（可退节点列表）│
└──────────────────────────────┘
```

### 2.3 语义澄清（驳回 vs 退回，避免混淆）

现有 UI 有两个按钮容易混：

| 动作 | 现在 | 语义 |
|------|------|------|
| **驳回** | `complete(decision=reject)` | 是一次**办结**，决策位=拒绝，令牌沿出边**往下**走（通常流向「结束/拒绝分支」） |
| **退回** | `reject_task` | 令牌**往回**走到上游节点重办，不是办结 |

方案：文案明确区分——「驳回」=否决并结束/走拒绝分支；「退回」=打回重办。退回子菜单统一挂在「退回 ▾」下。

### 2.4 落地范围与工作量

| 层 | 改动 |
|----|------|
| `cmx-flow-app` | 新增 `GET /tasks/{id}/reject-targets` 只读端点 + 反向可达 userTask 计算 |
| 前端 task-form | 「退回 ▾」下拉 + 目标选择器 + 发 `targetBpmnId` |
| 测试 | 可退目标列举（直线/带网关/分叉多前驱/环路）+ 退回到指定节点 E2E（引擎侧已有 p6 覆盖，补端点层） |

**工作量**：后端 0.5 天（纯只读计算，引擎退回逻辑复用），前端 0.5–1 天。**风险**：低——引擎退回语义已测（p6），本项主要是暴露与选择。

---

## 三、取回（撤回）：提交人拉回未处理的任务

### 3.1 现状：真缺口

引擎**没有** withdraw/取回。只有 `cancel_process`（终止整个实例 → `TERMINATED`），语义完全不同：

- 取回 ≠ 撤单。**取回** = 流程刚提交、下一节点**还没人处理**时，提交人把它「收回来」，实例回到发起人手里（可改可重交），流程**不终止**。
- 撤单 = 整个流程作废终止。

### 3.2 关键设计决策：取回到哪、什么条件能取回

**可取回的前提（安全护栏）**：

1. 只有**发起人**（或有权限者）能取回自己发起的实例。
2. 目标任务**尚未被处理**——即当前待办**一个都没被办结过**（一旦下游动过手，取回会破坏已发生的审批事实，应禁止或转为「协商撤回」）。
   - 判据：当前活动 userTask 全部未 `completed`，且该实例除发起外**无任何办结历史**（`hi_task` 无本实例记录 / 无非发起节点的 completed 任务）。严格程度可配（见 3.4）。
3. 实例处于 `ACTIVE`（非挂起/终态）。

**取回后到哪**：两种策略，推荐 A。

| 策略 | 行为 | 适用 |
|------|------|------|
| **A. 回到发起节点（推荐）** | 令牌拉回「起始后的第一个 userTask」或一个专门的「发起人修改」节点，作废当前待办，实例仍 `ACTIVE`，发起人可改数据后重新提交（沿正常流程再走） | 大多数「填错了想改」场景 |
| B. 直接终止（等于撤单） | = `cancel_process`，实例 `TERMINATED` | 「不想办了」——已有 cancel，不必新做 |

本方案做 **A**（B 已有）。

### 3.3 方案：新增 `withdraw` 引擎动作 + 端点

**引擎新增**（对齐 `reject_task` 的结构，复用令牌回退机制）：

```
withdraw_process(instance_id, by_user, reason) -> ExecutionResult
```

语义：

1. 校验：`by_user` 是发起人（实例变量 `initiator` 或业务归属）；实例 `ACTIVE`；**当前无已办结的下游任务**（护栏 3.1-#2）。不满足 → 报错（明确原因）。
2. 作废所有当前未办结待办（含候选池、其令牌），清相关定时器作业。
3. 令牌回到**发起节点之后的第一个 userTask**（「发起人节点」）——若流程未显式建发起人节点，退化为「回到 start 的首个 userTask」；令牌置 `Active` → `run_to_wait` 重开该待办给发起人。
4. 记台账 `WITHDRAW`（`from_user`=发起人，`reason`）。
5. 发 SSE/webhook `task.reassigned` 或新增 `instance.withdrawn` 事件。

> 与退回（reject）的区别：退回是**审批人**把任务打回**上一审批节点**；取回是**发起人**把整条流程拉回**发起处**。二者共用「令牌回退 + 作废当前待办 + 台账」的底层机制，实现可复用 `reject_task` 的骨架。

**新增端点**：

```
POST /api/flow/v1/instances/{id}/withdraw
body: { "user":"u_zhang", "reason":"填错金额，取回修改" }
→ instance_view（回到发起人待办）
```

配套只读端点（前端判断「取回」按钮是否可点）：

```
GET /api/flow/v1/instances/{id}/withdrawable?user=<uid>
→ { code:0, data:{ withdrawable:true, reason:null } }
   // 或 { withdrawable:false, reason:"下游已审批，不可取回" }
```

### 3.4 严格度可配

不同企业对「什么算还没处理」宽严不同，做成策略开关（定义级或全局）：

| 级别 | 判据 | 场景 |
|------|------|------|
| 严格（默认） | 下一节点**完全未读未办**（无 completed、无 claim、无 CC 已读） | 财务/合规，动过就不能悄悄取回 |
| 宽松 | 只要当前待办**未 completed** 即可取回（允许已被查看/认领但未办结） | 一般 OA |

放进定义元数据或 `flow-server` 配置，`withdrawable` 端点与 `withdraw` 动作共用同一判据函数。

### 3.5 前端（待办中心「我发起的」 + 任务表单）

- 「我发起的」列表（`/todos/initiated`）每条按 `withdrawable` 展示「取回」按钮（不可取回则置灰 + tooltip 说明原因）。
- 点取回 → 确认框（填原因）→ 调 `withdraw` → 刷新，流程回到自己待办可改。

### 3.6 落地范围与工作量

| 层 | 改动 |
|----|------|
| `cmx-flow-engine` | 新增 `withdraw_process` + 护栏判据 `is_withdrawable`（复用 reject 令牌回退骨架） |
| `cmx-flow-model` | `TaskDelegation.kind` 增 `WITHDRAW`（字符串，无需 schema 改）；可选新增 `instance.withdrawn` 事件 |
| `cmx-flow-app` | `POST /instances/{id}/withdraw` + `GET /instances/{id}/withdrawable` |
| 前端 | 「我发起的」取回按钮 + 确认框 |
| 测试 | 取回成功（未处理）/ 拒绝（下游已办）/ 非发起人拒绝 / 取回后可重交 E2E |

**工作量**：引擎 1.5–2 天，端点 0.5 天，前端 0.5 天。**风险**：中——护栏判据要严谨（尤其「已被处理」的定义，错判会让已审批流程被非法拉回）；「发起节点」在没有专门发起人节点的流程里如何定位需明确退化规则。

---

## 四、流程设计工作台 UX 重做

### 4.1 现状痛点（对照代码）

- 画布：bpmn-js modeler + **自研 BFS 分层布局**（导入无坐标 BPMN 时补坐标）——布局朴素，节点易重叠、边走线不美观。
- 属性面板：一堆扁平 `data-prop` 输入框（`applyProp(prop, value)` 直写 BPMN 属性），无分组、无引导、无校验反馈，自定义属性（`cmx:calledKey`/`cmx:candidates`/变量映射）散落，用户要懂 BPMN 属性名才会填。
- 节点/边样式：用 bpmn-js 默认渲染，未按「审批流」业务语义定制图标/配色（用户看不出「这是会签、那是定时器」）。

### 4.2 方案（分层，纯前端，后端零改）

**A. 画布可视化专业化**

- **业务化节点渲染**：为 userTask/serviceTask/businessRuleTask/callActivity/边界定时器/会签节点定制图标 + 徽章（会签叠三条杠、定时器画钟、子流程画嵌套框、消息等待画信封），一眼看出节点类型。bpmn-js 支持自定义 renderer。
- **更好的自动布局**：把自研 BFS 换成成熟布局（如 dagre 分层），减少交叉与重叠；提供「一键整理布局」按钮。
- **调色板精简**：palette 只留审批流常用元素（开始/结束/用户任务/网关/服务/子流程/边界定时器/消息），隐藏 BPMN 全集里用不到的，降低选择成本。

**B. 属性面板向导化（重点）**

把扁平输入框重构成**按节点类型分组的表单向导**：

```
选中「用户任务」→ 属性面板：
┌ 基本 ───────────────────┐
│ 节点名称 [____]           │
│ 表单     [选择表单 ▾]     │
├ 办理人 ─────────────────┤
│ ○ 指定人   [选用户 ▾]     │
│ ○ 角色     [选角色 ▾]     │
│ ○ 岗位     [选岗位 ▾]     │
│ ○ 关系型   [发起人上级▾]  │
│ ○ 多实例   [见下]          │
├ 多实例（会签/或签）───────┤
│ ☑ 启用  ○会签 ○或签        │
│ 集合变量 [products____]    │
│ 元素变量 [product_____]    │
│ 办理人   [role(${product.ownerRole})] ← 带提示 │
│ 完成条件 [全部完成 ▾]      │
├ 超时 ───────────────────┤
│ ☑ 边界定时器  时长[PT24H]  │
│    ○中断(升级) ○非中断(催办)│
└──────────────────────────┘
```

- 办理人从「填字符串」变成**选择器**（下拉选角色/岗位/用户，从 `/identity/*` 或 `/users` 拉），用户不再手写 `role(xxx)`。
- 关系型（发起人上级/部门领导）做成预设选项。
- 多实例、超时、条件都做成**结构化分组**，隐藏底层 BPMN 属性名。
- 网关：条件边用**条件构造器**（字段+运算符+值，已有 `bindConditionBuilder` 雏形，扩展成可视化规则行）+ 决策表引用选择器 + 默认流下拉（已有）。

**C. 边（分支条件）可视化**

- 选中一条边 → 面板显示「条件构造器」：`字段 [amount] 运算符 [>] 值 [50000]`，实时预览 `${amount > 50000}`，并可「试算」（调 `/conditions/eval` 代入样例变量）。
- 默认流用图标标注（bpmn-js 默认流有斜杠标记，确保渲染出来）。
- 边上显示条件摘要标签（如「金额>5万」），画布上一眼看清分支含义。

**D. 校验即时反馈**

- 存草稿/发布前调 `/definitions/validate`，把错误**定位到具体节点**（高亮画布上出错的节点 + 面板红字提示），而非只弹一句「校验失败」。

### 4.3 落地范围与工作量

| 层 | 改动 |
|----|------|
| 前端设计器 `design-workbench.js` | 自定义 renderer（业务图标/徽章）、属性面板向导化重构、办理人/角色选择器、多实例/超时/条件分组、边条件构造器、校验定位 |
| 可选：引入 dagre | 替换自研 BFS 布局（或保留 BFS 作兜底） |
| 后端 | **零改动**（全部基于既有端点：`/identity/*`、`/users`、`/conditions/eval`、`/definitions/validate`、`/decisions`） |

**工作量**：前端 4–6 天（属性向导是大头；自定义 renderer 2 天）。**风险**：低（纯前端、后端不动、可增量上线：先做属性向导，再做 renderer，再做布局）。**注意**：`design-workbench.js` 有 `core/` 与 `ui-native/flow/` **两份逐字节镜像副本**（vendor 纪律），改动必须同步两份，否则门户与独立壳漂移。

---

## 五、总览：优先级、工作量、依赖

| 项 | 缺口性质 | 引擎改 | 端点改 | 前端改 | 工作量 | 优先级 | 风险 |
|----|---------|:---:|:---:|:---:|-------|-------|------|
| ② 动态多实例派人 | 半成品（展开在，派人缺） | ✅ | — | ✅ | 引擎 2–3d + 前端 1d | **高**（真实业务刚需） | 中 |
| ③ 退回任意节点 | 后端就绪，缺暴露 | — | ✅只读 | ✅ | 后端 0.5d + 前端 1d | **高**（低成本高收益） | 低 |
| ④ 取回/撤回 | 真缺口 | ✅ | ✅ | ✅ | 引擎 2d + 端点 0.5d + 前端 0.5d | 中 | 中 |
| ① 设计器 UX | 纯前端体验 | — | — | ✅ | 前端 4–6d | 中（体验，非阻塞） | 低 |

### 建议排期

1. **第一批（低成本先落）**：③ 退回任意节点（后端已就绪，1.5 天见效）。
2. **第二批（业务刚需）**：② 动态多实例派人（核心能力补全）。
3. **第三批**：④ 取回（护栏设计需评审「已处理」判据后再动手）。
4. **并行/持续**：① 设计器 UX（增量迭代，先属性向导，后 renderer/布局）。

### 跨项共性

- ②③④ 都要在 `usage/` 文档补章节（[07 任务操作](usage/07-task-operations.md) 增退回选节点/取回，[02](usage/02-process-definition.md)/[04](usage/04-organization-and-identity.md) 增动态多实例派人写法）。
- ②③④ 的 E2E 测试都对齐现有 `cmx-flow-tests` 风格（内存态 always-runnable + PG 门控）。
- ①②③④ 的前端改动都需同步 `core/` 与 `ui-native/flow/` 两份镜像。

### 已拍板决策（2026-08-14）

| # | 开放问题 | 决策 | 影响 |
|---|---------|------|------|
| 1 | 取回「已处理」判据严格度（3.4） | **定义级可配**：两套判据（严格/宽松）都实现，每个流程定义自选 | `withdrawable` 判据函数读定义元数据的策略位 |
| 2 | 取回落点（3.2） | **回发起人处，可改后重交**（策略 A）；无专门发起人节点时退化到「start 后首个 userTask」 | `withdraw_process` 令牌回退到发起人节点 |
| 3 | 多实例派人写法（1.3） | **三种全做**：元素插值 `${product.ownerUser}` / 候选人表达式插值 `role(${product.ownerRole})` / 集合即人 | 共用底层 `eval_value` 求值内核，一次做齐 |
| 4 | 设计器布局（4.2-A） | **引入 dagre 分层布局**（自托管资产，不访问远程 CDN，与现有 bpmn-js 资产同处） | 前端加一个自托管依赖；保留 BFS 作离线兜底 |

### 详细实现设计（逐项展开）

按建议排期，各项确认后出独立的详细实现设计文档：

- ③ 退回任意节点 → [`enhancement-detail-03-reject-to-any-node.md`](enhancement-detail-03-reject-to-any-node.md)（**已落地实现，全绿**）
- ② 动态多实例派人 → [`enhancement-detail-02-dynamic-multi-instance-assignee.md`](enhancement-detail-02-dynamic-multi-instance-assignee.md)（**已落地实现 + UI（待办中心展 element_value）+ 真机 live**）
- ④ 取回/撤回 → **已落地实现**（引擎 `withdraw_process`/`can_withdraw` + 定义级 `cmx:withdrawPolicy` strict/lenient + 端点 `/withdraw`、`/withdrawable` + 待办中心「取回」按钮 + 6 测试 + 真机 live；落点回发起人）
- ① 设计器 UX → **部分落地**：经核对，设计器属性面板**已相当专业**（userTask 分页签 + 8 类办理人含关系型 + IAM 联动选择器 + 会签/或签 + 条件构造器 + 子流程组织路由）；本轮补 MI 逐元素派人引导（②桥）。**dagre 布局离线不可得**（无 vendored 包，bpmn-auto-layout 仅 CDN），保留零依赖 BFS；自定义节点徽章渲染器（可视化"不专业"的真实剩余缺口）需专门浏览器 QA，建议单独立项。

---

_本方案不含任何代码改动。确认方向后再逐项进入实现。_
