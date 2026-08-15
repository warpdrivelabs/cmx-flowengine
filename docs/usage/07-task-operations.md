# 07 · 任务操作：提交 / 回退 / 取回 / 转签

本篇是**办理人视角**的场景化说明：一个待办任务能做哪些动作、语义如何、REST 怎么调。所有动作都返回最新 `instance_view`（见 [06 §6.6](06-rest-api-reference.md)）。

## 7.1 动作全景

| 动作 | 端点 | 语义 | 令牌是否推进 | 台账 kind |
|------|------|------|-------------|-----------|
| 办结（提交） | `POST /tasks/{id}/complete` | 同意/完成，流程往下走 | ✅ 沿出边推进 | — |
| 退回（驳回） | `POST /tasks/{id}/reject` | 打回前一步重办 | ↩ 回退到目标节点 | `REJECT` |
| 认领（取回） | `POST /tasks/{id}/claim` | 候选池里据为己有 | ❌ | — |
| 转办 | `POST /tasks/{id}/transfer` | 彻底换人（原人退出） | ❌ | `TRANSFER` |
| 委派 | `POST /tasks/{id}/delegate` | 代办不换主 | ❌ | `DELEGATE` |
| 加签 | `POST /tasks/{id}/addsign` | 插入临时办理人 | ❌ | `ADDSIGN_BEFORE/AFTER` |
| 催办 | `POST /tasks/{id}/urge` | 提醒办理人（抄送） | ❌ | `URGE` |

> 「取回」在本引擎语义里对应两种：① 多人候选场景的**认领**（claim，从候选池取为己有）；② 管理员**跳转**（jump，把令牌拉回某节点，见 [09](09-operations-and-admin.md)）。日常办理人语境下的「取回待办」即 claim。

台账（`delegations`）统一记录 REJECT/TRANSFER/DELEGATE/ADDSIGN_*/URGE/JUMP，可在实例详情 `delegations` 字段追溯「谁在什么任务上做了什么、为什么」。

## 7.2 办结（提交）complete

最常用动作：同意并推进。

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/complete \
  -H 'Content-Type: application/json' \
  -d '{"instanceId":"<iid>", "comment":"同意", "decision":"agree", "variables":{"remark":"额度合理"}}'
```

请求（camelCase）：

| 字段 | 说明 |
|------|------|
| `instanceId` | 必填 |
| `variables` | 办结时合入的变量（同名覆盖，供后续网关/子流程用） |
| `decision` | 审批意见（记为变量 `lastDecision`） |
| `comment` | 审批评论（F3，落 `cmx_flow_task_comment`） |

语义：合入变量 → 标记任务 completed → 令牌沿选中出边推进 → `run_to_wait` 停到下一个等待态或完成。三种分派场景各有处理：

- **普通任务**：令牌离开 userTask 往下走。
- **多实例子任务**（会签/或签）：计数 + 求完成条件，命中即收口，否则顺序模式展开下一个。
- **加签临时任务**：办结后恢复父任务（不推进流程），见 §7.6。

被拒场景：实例 `SUSPENDED` 或任务被加签挂起（`SUSPENDED`）→ 报 `TaskNotActionable`。

### 会签办结示例

`expense_countersign` 的财务会签按 `approvers:["u1","u2","u3"]` 展开 3 个并行待办，完成条件 `nrOfCompletedInstances/nrOfInstances >= 0.5`（过半）：

```bash
# 办结第 1 个（1/3 未过半，继续等）
curl -X POST $BASE/tasks/<t1>/complete -d '{"instanceId":"<iid>"}' -H 'Content-Type: application/json'
# 办结第 2 个（2/3 过半 → 收口，剩余作废 → 流程继续）
curl -X POST $BASE/tasks/<t2>/complete -d '{"instanceId":"<iid>"}' -H 'Content-Type: application/json'
```

## 7.3 退回 / 驳回 reject

把任务打回**前一步**重新办理。

```bash
# 默认退回（回直接前驱用户任务）
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<finTaskId>/reject \
  -H 'Content-Type: application/json' \
  -d '{"instanceId":"<iid>", "fromUser":"财务", "reason":"金额存疑", "variables":{"rejectReason":"金额存疑"}}'

# 退回到指定节点
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<finTaskId>/reject \
  -H 'Content-Type: application/json' \
  -d '{"instanceId":"<iid>", "fromUser":"财务", "targetBpmnId":"mgr", "reason":"请核对金额"}'
```

请求（camelCase）：`{instanceId, fromUser?, targetBpmnId?, reason?, variables?}`。

语义（对齐 P6 测试）：

- 标记当前任务 completed，记 `REJECT` 台账（`reason` 为驳回意见），合入 `variables`。
- 令牌**回退**到目标节点，`run_to_wait` 在那里重建待办。
- **目标选择**：显式 `targetBpmnId`（必须是 userTask）；否则回溯到最近的**单一上游 userTask**（跳过网关）。
- 退回后，前一步是**新任务**（非原任务复活）；被驳回的任务作废。

错误：

- 目标非 userTask（如退到 `startEvent`）或不存在 → 报错。
- 首个 userTask 无前驱 userTask（前面只有 startEvent）→ 默认退回报错，要求显式指定 `targetBpmnId`。
- 多实例任务不支持退回 → `TaskNotActionable`。

**两级审批退回示例**（`start → 经理 → 财务 → end`）：经理办结 → 到财务；财务退回（默认）→ 令牌回经理节点、重现经理待办、财务任务作废、`rejectReason` 变量已合入。

## 7.4 认领 claim（取回候选任务）

多人候选（角色/岗位/组织解析出 ≥2 人）时任务在**候选池**，任一候选人认领后独占，其余候选作废。

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/claim \
  -H 'Content-Type: application/json' \
  -d '{"instance_id":"<iid>", "user_id":"u_staff"}'
```

> ⚠ **snake_case**：`instance_id` / `user_id`。

语义：校验 `user_id` 在候选池 → 写 `task.assignee = user_id` → 清空该任务候选记录。**不推进令牌**（只是认领，还得办结）。同一人重复认领幂等；他人已认领 → `ClaimFailed`。

配合「待认领」清单：

```bash
curl "http://127.0.0.1:8091/api/flow/v1/tasks/my?assignee=u_staff&kind=claimable"
```

## 7.5 转办 transfer vs 委派 delegate

两者都换办理人但语义不同：

| | 转办 TRANSFER | 委派 DELEGATE |
|--|--------------|---------------|
| assignee | → 新人 | → 代理人 |
| owner_user_id | → 新人（彻底换人） | 保持原主 |
| delegation_state | 清空 | `DELEGATED` |
| 原人 | 退出 | 仍是主人（代办不换主） |
| 语义 | 「这活以后归你」 | 「帮我看一下」 |

```bash
# 转办：张三 → 李四（彻底换人）
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/transfer \
  -H 'Content-Type: application/json' \
  -d '{"instance_id":"<iid>", "from_user":"张三", "to_user":"李四", "reason":"我出差"}'

# 委派：张三 → 李四（李四代办，主仍张三）
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/delegate \
  -H 'Content-Type: application/json' \
  -d '{"instance_id":"<iid>", "from_user":"张三", "to_user":"李四", "reason":"帮我看下"}'
```

> ⚠ 两者请求体都是 **snake_case**：`instance_id` / `from_user` / `to_user` / `reason`。

语义（对齐 M4.3 测试）：都不推进令牌，都清候选、记台账、发 `task.reassigned` SSE 事件。转办后 assignee+owner 都是李四；委派后 assignee=李四、owner=张三、delegation_state=DELEGATED。代理人办结即完成（M4.3 简化，不回主确认）。

## 7.6 加签 addsign（插入临时办理人）

在当前任务前/后插入一个临时办理人，原任务挂起，临时办结后原任务恢复。

```bash
# 向前加签：先让王五审，再回张三
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/addsign \
  -H 'Content-Type: application/json' \
  -d '{"instance_id":"<iid>", "from_user":"张三", "to_user":"王五", "before":true, "reason":"请先过目"}'
```

> ⚠ **snake_case**：`instance_id` / `from_user` / `to_user` / `before` / `reason`。`before` 缺省 true。

语义（对齐 M4.3 测试）：

- 原任务挂起：`delegation_state = SUSPENDED`（不可直接办结，办结会报错）。
- 新建临时任务：`assignee = 王五`，`parent_task_id = 原任务`，`delegation_state = ADDSIGN`，名字加后缀「（加签）」。
- 临时任务与原任务**共享令牌**——流程不推进。
- 记台账 `ADDSIGN_BEFORE` 或 `ADDSIGN_AFTER`（`before` 只影响台账类型，运行行为相同）。
- **可嵌套**：临时任务可再被加签（王五再加签赵六）。

流转：

```
加签后：原任务(SUSPENDED) + 临时任务(王五 ADDSIGN) 两个待办
  → 王五办结临时任务 → 原任务解除挂起（恢复）→ 只剩原任务待办
  → 张三办结原任务 → 流程推进
```

嵌套示例：张三加签王五 → 王五加签赵六 → 三个未办结（原 + temp1 + temp2）；赵六办结 → temp1 恢复；王五办结 temp1 → 原任务恢复；张三办结 → 完成。

## 7.7 催办 urge

提醒办理人尽快处理，不改流程。

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/urge \
  -H 'Content-Type: application/json' \
  -d '{"instanceId":"<iid>", "fromUser":"boss", "message":"尽快处理"}'
```

请求（camelCase）：`{instanceId, fromUser?, message?}`。

语义（对齐 A7 测试）：给当前办理人**落一条抄送**（CC）+ 记 `URGE` 台账。不动令牌、不改任务，待办仍在。

## 7.8 抄送（知会旁路）

抄送是**只读旁路**：不阻塞流程、不产生待办、不影响令牌。两种来源：

1. **节点自动抄送**：userTask 配 `cmx:cc="role(dept_head)"`，任务办结时自动抄送。
2. **手动抄送**：办理人主动抄送（引擎 `notify_cc`）。

查「抄送我的」与标记已读：

```bash
# 抄送我的（新式，走待办清单）
curl "http://127.0.0.1:8091/api/flow/v1/todos/cc?user=u_head"

# 抄送我的（旧式）
curl "http://127.0.0.1:8091/api/flow/v1/cc?user=u_head&unread=true"
# → {code:0, data:{cc:[{id,instanceId,businessKey,definitionKey,nodeBpmnId,reason,read,createdAt}]}}

# 标记已读
curl -X POST http://127.0.0.1:8091/api/flow/v1/cc/<ccId>/read
# → {code:0, data:{ok:true}}
```

> ⚠ 旧式 `/cc` 的 query 是 snake_case（`user`/`unread`）。

抄送记录带已读追踪（`read_at`），实例终态时保留供审计。被抄送人同样由 `AssigneeResolver` 从表达式解析（`role()`/`user()` 等，见 [04](04-organization-and-identity.md)）。

## 7.9 待办中心：五类清单

| 清单 | 端点 | 含义 |
|------|------|------|
| 我的待办 | `GET /tasks/my?assignee=X&kind=todo` | 直派给我 + 我已认领的 |
| 待认领 | `GET /tasks/my?assignee=X&kind=claimable` | 我在候选池、可认领的 |
| 我发起的 | `GET /todos/initiated?user=X` | 我发起的实例 |
| 抄送我的 | `GET /todos/cc?user=X` | 抄送给我的 |
| 我已办 | `GET /todos/done?user=X` | 我办结过的 |

全部支持分页（`page`/`pageSize`）+ 过滤（`keyword`/`definitionKey`/`nodeBpmnId`/`state`）。筛选器选项用 `GET /todos/filters`。字段详见 [06 §6.10](06-rest-api-reference.md)。

## 7.10 审批意见与轨迹

- 办结时带 `comment` → 落 `cmx_flow_task_comment`。
- 查实例全部意见：`GET /instances/{id}/comments` → `{comments:[{taskId,nodeBpmnId,userId,decision,comment,createdAt}]}`。
- 流转轨迹（转签/退回/催办）：实例详情 `delegations` 字段，或运维台 property 区（见 [09](09-operations-and-admin.md)）。

## 7.11 前端集成：任务表单宿主

内置 `portal.flow.task-form` 页由待办中心「办理」动态打开：content 区渲染业务单据 + 审批意见区（同意/驳回按钮），property 区显示流程轨迹。办结走 `/tasks/{id}/complete`，成功广播 `cmx-flow-task-done` 让待办中心刷新。

可嵌 Web Component `<flow-task-form>` 也提供同能力，第三方系统 `<script>` 引入即用（见 [`../../web/README.md`](../../web/README.md)）。表单绑定（formKey → 页面/字段）由 `/forms` 端点管理，见 [06 §6.11](06-rest-api-reference.md) 与 [08](08-external-integration.md)。

## 7.12 动作与令牌关系速记

```
complete   ──► 令牌沿出边前进（唯一推进流程的动作）
reject     ──► 令牌回退到目标节点
claim      ──► 不动令牌（只认领）
transfer   ──► 不动令牌（换人）
delegate   ──► 不动令牌（代办）
addsign    ──► 不动令牌（插临时任务，共享令牌）
urge       ──► 不动令牌（发抄送提醒）
```

只有 `complete` 推进流程；其余是「谁来办 / 打回重办 / 提醒」的编排，令牌位置不变（reject 除外，它回退）。

---

上一篇 ← [06 REST API 调用说明](06-rest-api-reference.md) ｜ 下一篇 → [08 外部系统集成](08-external-integration.md)
