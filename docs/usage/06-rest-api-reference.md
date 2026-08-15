# 06 · REST API 调用说明

生产服务 `cmx-flow-server`（默认 `http://<host>:8091`）的完整 HTTP 契约。本篇是**逐端点参考**；任务办理动作的场景化用法见 [07](07-task-operations.md)，外部集成见 [08](08-external-integration.md)，运维见 [09](09-operations-and-admin.md)。

## 6.1 基址与双前缀

所有业务路由**同时**挂在两个前缀下：

| 前缀 | 定位 | 建议 |
|------|------|------|
| `/api/flow/v1/...` | 正式契约（headless 版本化） | ✅ 新集成一律用 v1 |
| `/api/flow/...` | 历史兼容（零回归并存） | 老代码可继续用 |

例：办结任务 = `POST /api/flow/v1/tasks/{id}/complete`（= `POST /api/flow/tasks/{id}/complete`）。

**唯一例外**：SSE 事件流 `GET /api/flow/v1/events` 只在 v1 下。

## 6.2 统一信封

### 成功

```json
{ "code": 0, "msg": "success", "data": <T> }
```

`code:0` = 成功；`data` 为 `null` 时该键从 JSON 里省略。

### 错误

```json
{ "code": 1, "msg": "错误描述" }     // 无 data 键
```

| 类型 | HTTP 状态 | code | 何时 |
|------|-----------|------|------|
| 业务错误 Business | 200 | 1 | 大多数校验/操作失败（如「任务不可办结」「决策求值失败」） |
| 未找到 NotFound | 404 | 4 | 资源不存在 |
| 内部错误 Internal | 500 | 5 | 服务器内部 |
| 未认证 | 401 | 401 | 鉴权失败（`{"code":401,"msg":...}`） |

> ⚠ **软失败仍是 HTTP 200 code:0**：BPMN 校验、条件语法校验等把结果放在 `data.valid` 里（`{valid:false, error}`），不走错误信封。集成时判 `data.valid` 而非 HTTP 状态。

## 6.3 鉴权

`FLOW_AUTH_MODE`：

| 模式 | 说明 | 请求头 |
|------|------|--------|
| `off`（默认） | 无验签，租户/用户取头 | `X-Tenant: <t>`、`X-User: <u>` |
| `jwt` | 必须带 JWT | `Authorization: Bearer <jwt>`（HS256/RS256） |

- **服务间调用**：`X-Api-Key: <key>`（由 `FLOW_API_KEYS="k1:tenantA,k2:tenantB"` 映射到租户），命中即免 JWT。
- **委托用户令牌**：`X-Delegated-User-Token: Bearer <jwt>`（S6，始终验签，把待办归到真实登录用户）。
- 401 体：`{"code":401,"msg":"缺少 Authorization: Bearer <token>" | "JWT 校验失败: ..." | "无效 API Key"}`。

鉴权细节（JWT claim、多租户、平台反代）见 [08](08-external-integration.md)。

### 免认证路径

`/`（大盘 HTML）、`/api/flow/v1/docs`（Swagger UI）、`/api/flow/v1/openapi.json`、前端页面路由、`/_mon*`（技术监控）。**无 `/health` 端点**——健康探测用 `GET /api/flow/v1/definitions`。

## 6.4 请求体命名法（务必注意）

大多数请求体是 **camelCase**（如 `instanceId`）。但**以下端点是 snake_case**（历史原因，字段名即 JSON 键）：

| 端点 | snake_case 字段 |
|------|----------------|
| `POST /tasks/{id}/claim` | `instance_id`, `user_id` |
| `POST /tasks/{id}/transfer` | `instance_id`, `from_user`, `to_user`, `reason` |
| `POST /tasks/{id}/delegate` | `instance_id`, `from_user`, `to_user`, `reason` |
| `POST /tasks/{id}/addsign` | `instance_id`, `from_user`, `to_user`, `before`, `reason` |
| `GET /cc`（query） | `user`, `unread` |
| `GET /stats/detail`（query） | `dim`, `key`, `limit` |
| `GET /definitions/{key}`（query） | `version` |

其余全部 camelCase。逐端点在下文标注。

## 6.5 全量路由表（62 条）

METHOD | PATH（相对前缀，前缀 = `/api/flow/v1` 或 `/api/flow`） | 说明 | 详见

### 定义

| M | PATH | 说明 |
|---|------|------|
| GET | `/definitions` | 列已部署定义（画图用） |
| GET | `/design/definitions` | 列定义（设计器视图，含版本历史） |
| POST | `/definitions/draft` | 存草稿 |
| POST | `/definitions/validate` | 校验 BPMN |
| GET | `/definitions/{key}` | 定义详情（`?version=N`） |
| POST | `/definitions/{key}/publish` | 发布（版本+1，热装载） |
| GET | `/definitions/{key}/versions` | 列版本 |
| POST | `/definitions/{key}/versions/{version}/activate` | 激活版本 |
| DELETE | `/definitions/{key}/versions/{version}` | 删版本 |
| GET | `/startable` | 列可发起定义 |

### 实例

| M | PATH | 说明 |
|---|------|------|
| GET | `/instances` | 列实例 |
| POST | `/instances` | 发起实例 |
| GET | `/instances/{id}` | 实例详情 |
| GET | `/instances/{id}/children` | 子实例 |
| POST | `/instances/{id}/cancel` | 取消/撤单 |
| POST | `/instances/{id}/retry-incident` | 重试异常 |
| POST | `/instances/{id}/set-variables` | 改变量 |
| POST | `/instances/{id}/suspend` | 挂起 |
| POST | `/instances/{id}/resume` | 恢复 |
| POST | `/instances/{id}/jump` | 跳转 |
| GET | `/instances/{id}/variables` | 变量 |
| GET | `/instances/{id}/biz` | 关联业务单据 |
| GET | `/instances/{id}/comments` | 审批意见 |
| GET | `/biz/{table}/{bizId}/instances` | 按业务单据反查实例 |

### 任务与待办

| M | PATH | 说明 |
|---|------|------|
| GET | `/tasks/my` | 我的待办 |
| GET | `/todos/initiated` | 我发起的 |
| GET | `/todos/cc` | 抄送我的 |
| GET | `/todos/done` | 我已办 |
| GET | `/todos/filters` | 待办筛选器（定义/节点） |
| POST | `/tasks/{id}/complete` | 办结（提交） |
| POST | `/tasks/{id}/reject` | 退回/驳回 |
| POST | `/tasks/{id}/claim` | 认领 |
| POST | `/tasks/{id}/transfer` | 转办 |
| POST | `/tasks/{id}/delegate` | 委派 |
| POST | `/tasks/{id}/addsign` | 加签 |
| POST | `/tasks/{id}/urge` | 催办 |

### 消息 / 抄送 / 定时器 / 用户

| M | PATH | 说明 |
|---|------|------|
| POST | `/messages/correlate` | 相关消息（唤醒等待令牌） |
| GET | `/users` | 列用户（IAM） |
| GET | `/cc` | 抄送我的（旧） |
| POST | `/cc/{id}/read` | 标记抄送已读 |
| POST | `/timers/trigger` | 手动触发到期定时器 |

### 表单 / 组织 / 子流程绑定

| M | PATH | 说明 |
|---|------|------|
| GET | `/forms` | 列表单绑定 |
| POST | `/forms` | 存表单绑定 |
| GET | `/forms/{key}` | 单个表单绑定 |
| GET | `/orgs` | 列组织树 |
| POST | `/subflow-bindings` | 存子流程组织绑定 |
| GET | `/subflow-bindings/{key}` | 列绑定 |
| DELETE | `/subflow-bindings/id/{id}` | 删绑定 |

### 条件 / 决策

| M | PATH | 说明 |
|---|------|------|
| POST | `/conditions/eval` | 条件试算 |
| POST | `/conditions/validate` | 条件校验 |
| GET | `/conditions/functions` | 内置函数目录 |
| POST | `/decisions` | 注册决策表 |
| POST | `/decisions/evaluate` | 决策表试算 |

### 身份（local 模式）

| M | PATH | 说明 |
|---|------|------|
| GET | `/identity/mode` | 身份模式 |
| GET | `/identity/{entity}` | 列（orgs/roles/positions/users） |
| POST | `/identity/{entity}` | upsert |
| DELETE | `/identity/{entity}/{id}` | 软删 |
| POST | `/identity/users/{id}/roles` | 设用户角色 |

### 统计 / 监控 / 事件

| M | PATH | 说明 |
|---|------|------|
| GET | `/stats` | 聚合统计（大盘） |
| GET | `/analytics/node-timing` | 节点耗时分析 |
| GET | `/stats/detail` | 统计下钻 |
| GET | `/clients` | 客户端遥测 |
| GET | `/events` | **SSE 事件流（仅 v1）** |

## 6.6 三个共享响应视图

多数端点返回同一套视图，先在此定义，后文不再重复展开。

### `instance_view`（实例详情，办理动作后都返回它）

```json
{
  "id": "…", "definitionKey": "…", "businessKey": "…",
  "state": "ACTIVE|SUSPENDED|COMPLETED|TERMINATED",
  "variables": { … },
  "parentInstanceId": null, "waitingSubflow": false,
  "hasIncident": false,
  "incidents": [ {"nodeBpmnId":"…","reason":"…","retries":1,"since":"…"} ],
  "tokens": [ {"nodeBpmnId":"…","state":"WAITING|ACTIVE|JOINING|WAITING_SUBFLOW|WAITING_MESSAGE|INCIDENT|ENDED"} ],
  "tasks": [ {
    "id":"…","nodeBpmnId":"…","name":"…","assignee":"…",
    "ownerUserId":null,"parentTaskId":null,"delegationState":null,
    "elementValue":null,
    "candidates":[ {"userId":"…","type":"USER|ROLE|POSITION|ORG|ORG_LEADER|INITIATOR|INITIATOR_LEADER","ref":"…"} ],
    "completed": false
  } ],
  "activeNodes": ["…"],
  "jobs": [ {"boundaryBpmnId":"…","cancelActivity":true,"dueAt":"…"} ],
  "ccRecords": [ {"toUserId":"…","nodeBpmnId":"…","read":false} ],
  "delegations": [ {"kind":"TRANSFER|DELEGATE|ADDSIGN_BEFORE|ADDSIGN_AFTER|REJECT|URGE|JUMP","fromUserId":"…","toUserId":"…","reason":"…","createdAt":"…"} ],
  "openTasks": [ /* tasks 里 completed==false 的子集 */ ]
}
```

### `summary_view`（列表用）

```json
{ "id","definitionKey","businessKey","applicant","amount","riskLevel",
  "state","openTaskCount" }
```

### `definition_view`（画图用）

```json
{ "key","name",
  "nodes":[{"id","name","kind","multiInstance","boundaryTimer","calledElement"}],
  "edges":[{"from","to","condition","isDefault"}],
  "startable": true }
```

## 6.7 逐端点：定义

### `POST /definitions/draft` — 存草稿（camelCase）

```jsonc
// 请求
{ "name":"请假审批", "domain":"hr", "application":"leave", "module":"approve",
  "category":"审批类", "bpmnXml":"<?xml ...>", "updatedBy":"designer" }
// 响应 data
{ "key":"leave_request", "name":"请假审批", "state":"DRAFT", "activeVersion":0 }
```

### `POST /definitions/validate` — 校验（camelCase）

请求 `{ "bpmnXml":"<?xml ...>" }` → `data`：`{valid:true, key}` 或 `{valid:false, error}`（均 HTTP 200 code:0）。

### `GET /definitions/{key}` — 详情

query `?version=N`（可选，snake 无关，键名 `version`）。`data`：`{key,name,domain,application,module,category,state,activeVersion,shownVersion,versions:[{version,note,publishedAt,publishedBy}],bpmnXml,updatedAt}`。

### `POST /definitions/{key}/publish` — 发布（camelCase）

请求 `{note?, publishedBy?}` → `data`：`{key, version, hotLoaded:true, note}`。

### `GET /design/definitions` — 设计器列表

`data`：`{definitions:[{key,name,domain,application,module,state,activeVersion,versionCount,versions:[…],startable}]}`。

### `GET /startable` — 可发起定义

`data`：`{definitions:[{key,name,startFormKey}]}`。

## 6.8 逐端点：实例

### `POST /instances` — 发起（camelCase，含通用 + 兼容字段）

```jsonc
{
  "definitionKey": "credit_approval",   // 缺省 "credit_approval"
  "businessKey": "CR-2026-001",
  "orgId": "df_root",                    // 子流程路由/关系型候选人上下文
  "variables": { "applicant":"张三", "amount":80000, "initiator":"u_zhang" },
  "bizLink": { "bizTable":"t_loan", "bizId":"L-001", "bizKey":"CR-001", "role":"main" },
  // 兼容旧 demo 的扁平字段（可不用，推荐用 variables）：
  "applicant": "张三", "amount": 80000, "approvers": ["u1","u2"]
}
```

→ `instance_view`。`bizLink` 用于绑定业务单据（见 [08](08-external-integration.md)）；绑定失败会回滚（取消）实例。

### `GET /instances` — 列实例

`data`：`{instances:[summary_view]}`。

### `GET /instances/{id}` — 详情 → `instance_view`

### `GET /instances/{id}/children` — 子实例 → `{children:[instance_view]}`

### `GET /instances/{id}/variables` — 变量（裸对象）

`data` = 实例变量 JSON 对象本身（不再套一层）。

### `GET /instances/{id}/biz` — 关联单据 → `{links:[{bizTable,bizId,bizKey,role}]}`

### `GET /instances/{id}/comments` — 审批意见 → `{comments:[{taskId,nodeBpmnId,userId,decision,comment,createdAt}]}`

### `GET /biz/{table}/{bizId}/instances` — 按单据反查 → `{instances:[{instanceId,definitionKey,state,businessKey,role}]}`

实例的运维动作（cancel / retry-incident / set-variables / suspend / resume / jump）见 [09](09-operations-and-admin.md)。

## 6.9 逐端点：任务办理

以下端点全部返回 `instance_view`。**注意命名法**（camel vs snake 已在 §6.4 标注）。场景化说明见 [07](07-task-operations.md)。

### `POST /tasks/{id}/complete` — 办结（camelCase）

```jsonc
{ "instanceId":"…", "variables":{…}, "decision":"agree", "comment":"同意" }
```

### `POST /tasks/{id}/reject` — 退回（camelCase）

```jsonc
{ "instanceId":"…", "fromUser":"财务", "targetBpmnId":"mgr", "reason":"金额存疑", "variables":{…} }
```

`targetBpmnId` 省略 → 退回直接前驱用户任务。

### `POST /tasks/{id}/claim` — 认领（⚠ snake_case）

```jsonc
{ "instance_id":"…", "user_id":"u_staff" }
```

### `POST /tasks/{id}/transfer` — 转办（⚠ snake_case）

```jsonc
{ "instance_id":"…", "from_user":"张三", "to_user":"李四", "reason":"我出差" }
```

### `POST /tasks/{id}/delegate` — 委派（⚠ snake_case，同 transfer 结构）

### `POST /tasks/{id}/addsign` — 加签（⚠ snake_case）

```jsonc
{ "instance_id":"…", "from_user":"张三", "to_user":"王五", "before":true, "reason":"请先过目" }
```

`before` 缺省 true（向前加签）。

### `POST /tasks/{id}/urge` — 催办（camelCase）

```jsonc
{ "instanceId":"…", "fromUser":"boss", "message":"尽快处理" }
```

## 6.10 逐端点：待办清单

### `GET /tasks/my` — 我的待办（query，camelCase）

| query 参数 | 默认 | 说明 |
|-----------|------|------|
| `assignee` | 必填 | 办理人 |
| `kind` | `todo` | `todo`（我的待办）\| `claimable`（待认领）\| `all` |
| `keyword` | | 关键词 |
| `definitionKey` / `nodeBpmnId` | | 过滤 |
| `page` / `pageSize` | 1 / 20 | 分页 |

`data`：`{tasks:[{taskId,instanceId,nodeBpmnId,nodeName,definitionKey,definitionName,businessKey,formKey,formMode,formFields,bizTable,bizId,applicant,amount,claimable,createdAt}], total, page, pageSize}`。

### `GET /todos/initiated` | `/todos/cc` | `/todos/done` — （query，camelCase）

query `{user?, keyword?, definitionKey?, nodeBpmnId?, state?, page?, pageSize?}`。`data`：`{tasks:[{taskId,instanceId,nodeBpmnId,nodeName,definitionKey,businessKey,state,currentNode,formKey,formMode,bizTable,bizId,applicant,amount,createdAt}], total, page, pageSize}`。

### `GET /todos/filters` — 筛选器 → `{definitions:[{key,name,nodes:[{id,name}]}]}`（仅 userTask 节点）

## 6.11 逐端点：表单绑定

### `GET /forms` → `{bindings:[form_binding]}`
### `POST /forms` — 存绑定（camelCase）

```jsonc
{ "formKey":"pay.review", "kind":"native",
  "nativePage":"portal.fi.pay-review", "nativeView":"content",
  "htmlPage":null, "bizTable":"t_pay", "domain":"fi","application":"gl","module":"pay",
  "file":null, "pkField":"id", "title":"付款复核", "workspaceNode":null }
```

→ `{formKey}`。`form_binding` = `{formKey,kind,nativePage,nativeView,htmlPage,bizTable,domain,application,module,file,pkField,title,workspaceNode}`。

### `GET /forms/{key}` → 单个 `form_binding` 或 `null`

## 6.12 逐端点：条件 / 决策 / 身份 / 组织 / 子流程绑定

这些在对应主题篇有完整示例，此处只列签名：

| 端点 | 请求 | 响应 data |
|------|------|-----------|
| `POST /conditions/eval` | `{expr, variables}`（snake 无关，键 expr/variables） | `{expr, result, truthy}`；错误→业务错 |
| `POST /conditions/validate` | `{expr}` | `{valid}` 或 `{valid:false,error}` |
| `GET /conditions/functions` | — | `{functions:[{name,category,arity,desc}]}` |
| `POST /decisions` | 完整决策表 JSON | `{key, rules}`；校验不过→业务错 |
| `POST /decisions/evaluate` | `{table, variables}` | `{matchedRules, outputs}` |
| `GET /identity/mode` | — | `{mode, editable}` |
| `GET /identity/{entity}` | — | `{items:[…]}` |
| `POST /identity/{entity}` | 实体 JSON | `{id}`；external 模式→业务错 |
| `DELETE /identity/{entity}/{id}` | — | `{deleted}` |
| `POST /identity/users/{id}/roles` | `{roleIds:[…]}` | `{userId, roleCount}` |
| `GET /orgs` | — | `{orgs:[{id,name,parentId,path}]}` |
| `POST /subflow-bindings` | `{calledKey, orgId?, targetKey, enabled}` | `{id}` |
| `GET /subflow-bindings/{key}` | — | `{calledKey, bindings:[…]}` |
| `DELETE /subflow-bindings/id/{id}` | — | `{deleted}` |

详见 [05](05-conditions-and-decisions.md)（条件/决策）、[04](04-organization-and-identity.md)（身份）、[03](03-subprocess.md)（子流程绑定/组织）。

## 6.13 逐端点：消息 / 抄送 / 定时器 / 用户

| 端点 | 请求 | 响应 data |
|------|------|-----------|
| `POST /messages/correlate` | `{messageName, instanceId?, correlationKey?, variables}`（camelCase） | `instance_view` |
| `GET /users` | — | `{users:[{id,name}]}` |
| `GET /cc` | query `{user, unread}`（⚠ snake） | `{cc:[{id,instanceId,businessKey,definitionKey,nodeBpmnId,reason,read,createdAt}]}` |
| `POST /cc/{id}/read` | — | `{ok}` |
| `POST /timers/trigger` | — | `{firedCount, fired:[{instanceId,boundaryBpmnId,cancelActivity,instanceState}]}` |

## 6.14 逐端点：统计 / 监控

### `GET /stats` — 聚合大盘（无参，子查询失败降级为 0）

```jsonc
{ "overview":{ "totalInstances","activeInstances","totalTasks","openTasks","doneTasks",
               "totalDefinitions","distinctUsers","pendingTimers" },
  "instancesByState":[{label,value}], "byDefinition":[…], "topAssignees":[…],
  "timeline":[…], "byOrg":[…], "nodeBottleneck":[…], "performance":[…],
  "archive":{ "hiInstances","hiTasks","avgInstanceSecs","avgTaskSecs" },
  "collaboration":{ "cc","comments","delegations","subprocesses","multiInstance" },
  "runtime":{ "tenant","dbId","engine":"cmx-flow" } }
```

### `GET /analytics/node-timing` — 节点耗时（query，camelCase）

query `{definitionKey?, slaSecs?}`（slaSecs 默认 86400）。`data`：`{slaSecs, definitionKey, totalTasks, slaBreachedTotal, nodes:[{node,name,cnt,avg_ms,max_ms,min_ms,sla_breached}]}`。

### `GET /stats/detail` — 下钻（query，⚠ snake：dim/key/limit）

`dim` 白名单：`instanceState | definition | instances | assignee | openTasks | doneTasks | tasks | timelineDay | definitions | users | cc | comment | delegation | subprocess | multiInstance | org | node | hiInstance | hiTask | performance | timer`。未知 dim → 业务错。`limit` 夹在 [1,1000] 默认 200。`data`：`{dim, key, title, count, rows:[…]}`。

### `GET /clients` — 客户端遥测（监控用）

监控与运维详见 [09](09-operations-and-admin.md)；SSE 见 [08](08-external-integration.md)。

## 6.15 OpenAPI / Swagger

- Swagger UI：`GET /api/flow/v1/docs`（免认证）。
- 规范 JSON：`GET /api/flow/v1/openapi.json`（免认证）。

> ⚠ OpenAPI 是**手工维护的子集**（约 27 条，非全部 62 条路由）——`reject/urge/suspend/resume/jump/retry-incident/set-variables/conditions/decisions/identity/stats/subflow-bindings/users/cc/timers` 等端点真实存在且可用，但未收进 Swagger。**以本文档为完整契约**，Swagger 仅作交互试调。

## 6.16 curl 端到端最小闭环

```bash
BASE=http://127.0.0.1:8091/api/flow/v1

# 发起
IID=$(curl -s -X POST $BASE/instances -H 'Content-Type: application/json' \
  -d '{"definitionKey":"credit_approval","variables":{"applicant":"张三","amount":80000}}' \
  | jq -r '.data.id')

# 查我的待办拿 taskId（credit_approval 首个 userTask assignee=经理）
TID=$(curl -s "$BASE/tasks/my?assignee=经理" | jq -r '.data.tasks[0].taskId')

# 办结（同意）
curl -s -X POST $BASE/tasks/$TID/complete -H 'Content-Type: application/json' \
  -d "{\"instanceId\":\"$IID\",\"comment\":\"同意\"}" | jq '.data.state'

# 看实例详情
curl -s $BASE/instances/$IID | jq '.data | {state, openTasks: [.openTasks[].name]}'
```

---

上一篇 ← [05 分支条件与决策表](05-conditions-and-decisions.md) ｜ 下一篇 → [07 任务操作：提交/回退/取回/转签](07-task-operations.md)
