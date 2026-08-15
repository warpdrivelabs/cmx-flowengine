# ③ 详细实现设计：退回到任意上游节点

> 状态：**已落地实现（2026-08-14）**，引擎 + 端点 + 前端 + 测试全绿。父方案见 [`enhancement-proposal-designer-mi-reject-withdraw.md`](enhancement-proposal-designer-mi-reject-withdraw.md)。
> 落地：引擎 `reject_targets` 方法 + `reachable_upstream_user_tasks` 反向遍历 + `RejectTarget`/`RejectTargetInfo` DTO；应用层 `GET /tasks/{id}/reject-targets`；前端 task-form「退回到…」选择器（两份镜像同步）；新增 5 个引擎测试，全套 86 通过零回归，新代码 0 clippy。
> 目标：办理人退回任务时，除「退回上一步」外，可**选择任意合法上游 userTask** 作为退回目标。
> 日期：2026-08-14

## 1. 一句话结论

引擎的 `reject_task` **早已支持**显式指定任意 userTask 作退回目标（`target_bpmn_id: Option<&str>`），退回语义、台账、变量合并、多实例挡回都已实现并有测试（p6）。本项**不改引擎退回逻辑**，只补两处：

1. **一个只读端点** `GET /tasks/{id}/reject-targets`——列出该任务所有合法退回目标（供前端渲染选择器）。
2. **前端 task-form**——把单一「退回上一步」升级为「退回 ▾」下拉（默认目标一键 + 选任意上游节点）。

## 2. 现状锚点（代码位置）

| 已有 | 位置 |
|------|------|
| 引擎退回（支持显式目标） | `crates/cmx-flow-engine/src/engine.rs:990` `reject_task(...)` |
| 直接前驱回溯（跳网关，64 层 guard） | `engine.rs:2056` `predecessor_user_task(def, node_bpmn)` |
| 退回端点（现只发默认目标） | `crates/cmx-flow-app/src/handlers.rs:918` `reject_task` + `RejectReq{target_bpmn_id}` |
| 端点响应视图 | `handlers.rs:129` `load_view(rt, iid)` |
| 定义查节点 | `crates/cmx-flow-model/src/ir.rs:332` `node_by_bpmn` |
| 路由注册 | `crates/cmx-flow-app/src/lib.rs:148` `/tasks/{id}/reject` |
| 前端退回 | `web/core/task-form.js:397` `退回上一步`（发 reject 无 targetBpmnId） |

> ⚠ 前端两份镜像：`web/core/task-form.js` 与 `web/ui-native/flow/task-form.js` 逐字节同步，改动两份都要动。

## 3. 引擎侧：新增只读辅助（不碰运行态）

退回目标 = 从当前任务节点**沿入边反向可达的上游 userTask 集合**。新增一个纯函数（`engine.rs`，与 `predecessor_user_task` 同区），不改任何令牌/存储：

```
fn reachable_upstream_user_tasks(def, node_bpmn) -> Vec<RejectTarget>
```

### 算法

- BFS/DFS 反向遍历入边（复用 `predecessor_user_task` 里的 `preds()` 反向邻接思路）。
- 遇 userTask → 收入结果（记 `distance` = 反向跳数）；遇网关/事件/服务任务 → 继续向上游穿透。
- **去重 + 深度上限**（沿用 64 层 guard 防环）；`distance` 取首次到达的最小跳数。
- 结果里标出 `is_direct_predecessor`（= `predecessor_user_task` 返回的那个默认目标，前端默认选中）。

### 返回结构（引擎内 DTO）

```
RejectTarget {
  bpmn_id: String,
  name: Option<String>,          // 节点 name
  is_direct_predecessor: bool,   // 默认退回目标（None 时会去的）
  distance: usize,               // 反向跳数，供前端「上 1 步/上 2 步」排序
}
```

### 边界

| 情形 | 行为 |
|------|------|
| 当前任务是首个 userTask（上游只有 start） | 返回空数组（无可退目标）→ 前端置灰退回 |
| 上游有分叉/合流网关 | 穿透网关继续找 userTask，可能返回多个 |
| 环路 | 去重 + 深度上限，不重复不死循环 |
| 当前任务属会签/或签域 | 该任务不可退（引擎 reject 已挡回）；端点可直接返回空 + 标记 `rejectable:false` |

## 4. 应用层：新增端点

### 路由（`lib.rs`，紧挨现有 reject）

```
.route("/tasks/{id}/reject-targets", get(handlers::get_reject_targets))
```

自动双前缀（`/api/flow/v1/...` + `/api/flow/...`），随整个 `flow_routes` 一起认证。

### handler（`handlers.rs`，只读，仿 `get_todo_filters` 风格）

```
GET /api/flow/v1/tasks/{taskId}/reject-targets?instanceId=<iid>
```

逻辑：`flow()` 取运行时 → `load_snapshot(instanceId)` 定位任务节点 → `get_definition` → 调 `reachable_upstream_user_tasks` → `ApiResp::ok`。

### 响应契约

```json
{
  "code": 0, "msg": "success",
  "data": {
    "taskId": "…",
    "currentNode": "fin",
    "rejectable": true,
    "defaultTarget": "mgr",
    "targets": [
      { "bpmnId": "mgr",   "name": "经理审批", "isDirectPredecessor": true,  "distance": 1 },
      { "bpmnId": "apply", "name": "申请填报", "isDirectPredecessor": false, "distance": 2 }
    ]
  }
}
```

- `rejectable:false`（会签域/无上游）时 `targets:[]`、`defaultTarget:null`。
- 错误（任务不存在等）走统一信封（`not_found`/业务错），复用 `engine_err`/`FlowError`。

### 为何要 `instanceId` query

`reject-targets` 需要按实例定位任务节点（task_id 全局唯一但 `load_snapshot` 按实例聚合）。与现有 `complete`/`reject` 一致（都要 `instanceId`）。

## 5. 前端：task-form 退回交互升级

### 现状

`task-form.js:280` 一个按钮「退回上一步」→ 直接发 `reject`（body 无 `targetBpmnId`）。

### 目标交互

```
┌ 审批控制台 ──────────────────────┐
│ [同意办结]  [驳回]                 │
│ [退回 ▾]                           │
│   ┌────────────────────────────┐  │
│   │ ⤺ 退回上一步（经理审批）★    │  │ ← 默认目标，一键
│   │ ⤺ 退回到 申请填报（上2步）    │  │
│   │ ⤺ 退回到 …                   │  │
│   └────────────────────────────┘  │
└──────────────────────────────────┘
```

### 流程

1. 打开退回下拉时（或渲染审批控制台时）拉 `GET /tasks/{id}/reject-targets?instanceId=…`。
2. `rejectable:false` → 退回按钮置灰 + tooltip（「会签任务需整域处理」/「已是首环节，无处可退」）。
3. 列出 `targets`：默认目标（`isDirectPredecessor`）标 ★ 置顶，其余按 `distance` 升序（上 1 步、上 2 步…）显示节点名。
4. 选中目标 + 填意见 → 发：

```
POST /api/flow/v1/tasks/{id}/reject
{ "instanceId":"…", "fromUser":"<当前用户>", "targetBpmnId":"apply", "reason":"退回补充材料" }
```

5. 「退回上一步」快捷项 = 不带 `targetBpmnId`（走引擎默认前驱），保持老习惯零学习成本。

### 文案：区分「驳回」vs「退回」

沿用现有两按钮，但明确语义（父方案 2.3）：

- **驳回** = `complete(decision=reject)`，办结 + 决策拒绝，令牌**往下**走拒绝分支。
- **退回** = `reject_task`，令牌**往回**走重办，非办结。

退回子菜单统一在「退回 ▾」下，避免与「驳回」混淆。

## 6. 测试计划

### 引擎单测（`cmx-flow-tests`，内存态 always-runnable）

新增 `reject_targets.rs`：

| 用例 | 断言 |
|------|------|
| 直线两级审批 `start→mgr→fin` | fin 的 targets = [mgr(direct,dist1)]；mgr 的 targets = []（上游只 start） |
| 三级 `start→a→b→c` | c 的 targets = [b(dist1,direct), a(dist2)] 有序 |
| 上游带网关 `start→a→gw→b` | b 穿透 gw → targets 含 a |
| 上游分叉多前驱 | 返回多个 userTask（不报错，供选择） |
| 环路 | 去重、不死循环、深度不超限 |
| 会签域任务 | rejectable:false, targets:[] |

退回到指定节点的运行语义已由现有 `p6_reject.rs` 覆盖（`reject_to_explicit_target`），无需重测。

### 端点层（curl / 集成）

- `GET /tasks/{id}/reject-targets` 返回结构 + 默认目标标记正确。
- 退回到 targets 里的非默认节点 → 令牌确实回到该节点、原待办作废、REJECT 台账留痕。

## 7. 落地清单与工作量

| 层 | 文件 | 改动 |
|----|------|------|
| 引擎 | `cmx-flow-engine/src/engine.rs` | 新增 `reachable_upstream_user_tasks` + `RejectTarget` DTO（纯只读，复用 `preds`） |
| 引擎导出 | `cmx-flow-engine/src/lib.rs` | 导出 `RejectTarget`（若端点层要用） |
| 应用层 | `cmx-flow-app/src/handlers.rs` | 新增 `get_reject_targets` handler + query 结构 |
| 应用层 | `cmx-flow-app/src/lib.rs` | 注册 `/tasks/{id}/reject-targets` |
| 应用层（可选） | `cmx-flow-app/src/openapi.rs` | 收进 OpenAPI（手工子集，可选） |
| 前端 | `web/core/task-form.js` **+** `web/ui-native/flow/task-form.js` | 退回下拉 + 目标选择器 + 发 targetBpmnId（两份镜像同步） |
| 测试 | `cmx-flow-tests/tests/reject_targets.rs` | 上游可达列举用例 |
| 文档 | `usage/07-task-operations.md` §退回 + `usage/06-rest-api-reference.md` | 补 reject-targets 端点 |

**工作量**：引擎 0.5 天（纯只读遍历 + 测试）+ 应用层 0.25 天 + 前端 0.5–1 天 = **约 1.5 天**。
**风险**：低——引擎退回主体已测，本项是「暴露 + 选择」，无运行态改动，无破坏性变更；唯一注意点是前端两份镜像同步。

## 8. 不做什么（范围边界）

- **不改**引擎 `reject_task` 的退回语义/签名（已够用）。
- **不做**跨实例/跨分支的复杂退回（只在当前流程图上游可达范围内）。
- **不做**退回到网关/服务任务/事件（无意义，引擎已挡回，只列 userTask）。
- 会签/或签域内单任务退回仍**不支持**（引擎明确挡回，本端点如实返回 `rejectable:false`）。

---

_确认后即可进入实现。下一项详细设计：② 动态多实例派人。_
