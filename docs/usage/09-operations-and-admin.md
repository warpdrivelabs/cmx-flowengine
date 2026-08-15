# 09 · 运维与管理

本篇面向**运维/管理员**：流程卡住怎么办、异常怎么恢复、如何人工干预（挂起/恢复/跳转/改变量/取消）、定时器如何推进、统计与监控。所有实例干预动作返回最新 `instance_view`（见 [06 §6.6](06-rest-api-reference.md)）。

## 9.1 运维动作全景

| 动作 | 端点 | 用途 | 台账 |
|------|------|------|------|
| 挂起 | `POST /instances/{id}/suspend` | 暂停实例，冻结所有办理 | — |
| 恢复 | `POST /instances/{id}/resume` | 解冻 | — |
| 跳转 | `POST /instances/{id}/jump` | 把令牌强制拉到某 userTask | `JUMP` |
| 改变量 | `POST /instances/{id}/set-variables` | 修正实例变量（不推进） | — |
| 重试异常 | `POST /instances/{id}/retry-incident` | serviceTask 失败后重跑 | — |
| 取消 | `POST /instances/{id}/cancel` | 撤单/终止 | — |

内置**流程运维台**（`portal.flow.ops-console`）：explorer 实例列表（异常高亮）、content 详情 + 干预工具条、property 异常明细 + 流转台账。

## 9.2 挂起 / 恢复

临时冻结实例（如排查问题、等外部条件）：

```bash
# 挂起 → 实例 SUSPENDED，所有办理动作被拒
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/suspend

# 恢复 → ACTIVE，可继续办理
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/resume
```

语义（对齐 A7 测试）：

- 挂起：`ACTIVE → SUSPENDED`，令牌/任务保留；挂起态办结任务报错。
- 恢复：`SUSPENDED → ACTIVE`，然后推进任何 `Active` 令牌。
- **幂等**：恢复未挂起的实例无操作；重复挂起无害。

## 9.3 跳转 jump（强制改变令牌位置）

管理员把令牌强制移到某个 userTask（跳过/回退若干环节）：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/jump \
  -H 'Content-Type: application/json' \
  -d '{"targetBpmnId":"fin", "reason":"运维跳过经理环节"}'
```

请求（camelCase）：`{targetBpmnId, reason?}`。

语义（对齐 A7 测试）：

- 目标**必须是 userTask**（跳到 endEvent/不存在节点 → 报错）。
- 作废所有未完成任务、清候选人/定时器作业、收口多实例域、结束所有非目标令牌，把第一个非 ended 令牌移到目标 `Active`，`run_to_wait`。
- 记 `JUMP` 台账（发起人记为 `admin`）。
- 仅 `Active` 实例可跳（否则 `TaskNotActionable`）。
- 简化：单主令牌跳转。

> 跳转是「大锤」——会作废现有待办、重排令牌。日常「打回上一步」用 [07](07-task-operations.md) 的 reject（更精细、留 REJECT 台账、走正常回退语义）。

## 9.4 改变量 set-variables

修正实例变量而不推进流程（配合重试异常用）：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/set-variables \
  -H 'Content-Type: application/json' \
  -d '{"variables":{"amount":75000,"fixed":true}}'
```

请求（camelCase）：`{variables}`（必填）。语义：合入变量（同名覆盖），保存。**不推进令牌**——安全的最小干预。

## 9.5 Incident：serviceTask 失败的安全网

serviceTask 的 delegate 失败时，引擎**不终止流程、不丢实例**，而是把该令牌挂在 `INCIDENT` 态，等人工修复重试。这是生产可用性硬门槛。

### 机制（对齐 H2 测试）

```
serviceTask delegate 返回 Err
  → 令牌转 INCIDENT（停在故障节点）
  → 实例仍 ACTIVE（其它分支照常跑，整个运行不中止）
  → 原因 + 重试次数记进实例变量 __incident
      __incident = { "<节点bpmn_id>": { "reason": "外部依赖不可用", "retries": 1 } }
```

- 实例详情里 `hasIncident:true`，`incidents:[{nodeBpmnId, reason, retries, since}]`。
- `retry_incident` 重跑：仍失败 → 重试次数累加、仍 Incident；修好（如改数据）→ 令牌越过故障节点继续到下一等待态。
- 成功执行时清除该节点的 incident 痕迹（`__incident` 里对应键移除）。

> 数据准确性说明：`__incident` 实际形状是 `{reason, retries}`（源码注释里提到的 `lastAt` 字段实际不存在）。

### 重试

```bash
# 不改数据直接重试（仍失败则 retries+1）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/retry-incident \
  -H 'Content-Type: application/json' -d '{"variables":{}}'

# 修正数据后重试（成功 → 越过故障节点继续）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/retry-incident \
  -H 'Content-Type: application/json' -d '{"variables":{"fixed":true}}'
```

请求（camelCase）：`{variables}`（合入的修正变量）。语义：合入变量 → 把**所有** `INCIDENT` 令牌翻回 `Active` → 重跑 `run_to_wait`。无 incident 令牌时幂等无操作。

### 典型运维流程

```
1. 大盘/运维台发现异常实例（hasIncident:true）
2. 看 incidents[].reason 定位故障（如「外部依赖不可用」）
3. 修外部依赖，或用 set-variables 修正数据
4. retry-incident 重试 → 恢复推进
5. 仍失败则看 retries 累加，继续排查
```

## 9.6 取消 / 撤单

终止实例：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances/<iid>/cancel \
  -H 'Content-Type: application/json' -d '{"reason":"申请人撤回"}'
```

请求：`{reason?}`（snake 无关，键 `reason`）。语义：结束所有非 ended 令牌、作废未完成任务（保留已完成供归档）、收口多实例域、清作业/候选人，记 `_cancelReason` 变量，实例转 `TERMINATED` + `ended_at`，同事务归档到 HI 表。已是终态则幂等。

## 9.7 定时器推进

引擎**无后台线程**，边界定时器（见 [02 §2.10](02-process-definition.md)）靠宿主推进：

- **自动**：`cmx-flow-server` 内建 poller，每 5 秒调一次 `trigger_due_timers`，跨所有租户运行时扫描到期作业。
- **手动**：立即触发（免等 5 秒轮询）：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/timers/trigger
# → {code:0, data:{firedCount:1, fired:[{instanceId,boundaryBpmnId,cancelActivity,instanceState}]}}
```

语义：`clock.now()` → 扫 `due_at <= now` 的作业（按 due_at 索引跨实例）→ 逐个触发：

- **中断型**（cancelActivity=true）：作废宿主令牌的待办、删其全部定时器作业、令牌移到边界节点。
- **非中断型**（false）：只删本作业，在边界节点起一个旁路 `Active` 令牌（parent=宿主令牌），宿主继续等。

令牌离开宿主（办结/取消/被中断）即撤销其定时器作业。

## 9.8 统计大盘

`GET /api/flow/v1/stats` 一次聚合全景（子查询失败降级为 0，保证总能出盘）：

```bash
curl http://127.0.0.1:8091/api/flow/v1/stats
```

`data` 结构：

| 分组 | 字段 |
|------|------|
| `overview` | totalInstances / activeInstances / totalTasks / openTasks / doneTasks / totalDefinitions / distinctUsers / pendingTimers |
| 分布数组 | instancesByState / byDefinition / topAssignees / timeline / byOrg / nodeBottleneck / performance（各 `[{label,value}]`） |
| `archive` | hiInstances / hiTasks / avgInstanceSecs / avgTaskSecs |
| `collaboration` | cc / comments / delegations / subprocesses / multiInstance |
| `runtime` | tenant / dbId / engine |

`cmx-flow-server` 根路径 `/`（免认证）是**内置监控大盘**（自包含单页，5 秒轮询 `/stats`，light/dark 双主题、环图/折线/榜单/KPI）。

## 9.9 下钻与节点耗时

### 统计下钻

```bash
curl 'http://127.0.0.1:8091/api/flow/v1/stats/detail?dim=definition&key=credit_approval&limit=50'
```

query（⚠ snake：`dim`/`key`/`limit`）。`dim` 白名单：`instanceState | definition | instances | assignee | openTasks | doneTasks | tasks | timelineDay | definitions | users | cc | comment | delegation | subprocess | multiInstance | org | node | hiInstance | hiTask | performance | timer`。`limit` 夹 [1,1000] 默认 200。→ `{dim, key, title, count, rows:[…]}`。

### 节点耗时分析（找瓶颈 / SLA 超时）

```bash
curl 'http://127.0.0.1:8091/api/flow/v1/analytics/node-timing?definitionKey=credit_approval&slaSecs=86400'
```

query（camelCase）：`{definitionKey?, slaSecs?}`（默认 86400=1天）。→ `{slaSecs, definitionKey, totalTasks, slaBreachedTotal, nodes:[{node,name,cnt,avg_ms,max_ms,min_ms,sla_breached}]}`。用于识别哪个环节办理最慢、多少超 SLA。

## 9.10 技术监控 `/_mon`

chassis 自带技术监控（免认证）：

| 端点 | 内容 |
|------|------|
| `GET /_mon` | 技术监控页（系统内存/CPU/load/网络/磁盘 + DB 池 + 请求遥测） |
| `GET /_mon/tech-stats` | 技术指标 JSON |
| `GET /_mon/deps` | 依赖拓扑（flow 是单个内嵌依赖 `{key:"flow", mode:"embedded", proxiable:true}`） |
| `GET /api/flow/v1/clients` | 客户端遥测（业务侧） |

## 9.11 崩溃恢复

「等待态即提交点」保证崩溃安全：

- 每个等待态对应一次 `save_snapshot`，进程重启后从库装载聚合，用同一套推进代码继续。
- 令牌记 `node_bpmn_id`（稳定锚点），非内存下标——重启后位置精确。
- 定时器作业随聚合持久化，重启后不丢；多实例计数、子流程父子链、候选池、转签台账全部落库。
- 重启时 flow-server 的 `engine` 钩子重新装载已发布定义、重建定时器 poller。

无需额外操作——重启即恢复。

## 9.12 运维动作与令牌关系速记

```
suspend/resume  ──► 冻结/解冻，不动令牌位置
jump            ──► 令牌强制到目标 userTask（作废现有待办，大锤）
set-variables   ──► 改变量，不动令牌
retry-incident  ──► INCIDENT 令牌翻回 Active 重跑
cancel          ──► 所有令牌结束，实例 TERMINATED
timers/trigger  ──► 推进到期定时器（中断型移令牌/非中断型起旁路令牌）
```

## 9.13 常见运维问答

| 现象 | 排查 |
|------|------|
| 实例卡住不动 | 看 `tokens[].state`：`WAITING`=等办理；`INCIDENT`=serviceTask 失败（看 `incidents`）；`WAITING_MESSAGE`=等外部消息（`correlate`）；`WAITING_SUBFLOW`=等子流程（看 children） |
| serviceTask 一直失败 | `incidents[].reason` 定位；修依赖或 `set-variables` 后 `retry-incident` |
| 定时器不触发 | 确认 poller 在跑（日志）或手动 `POST /timers/trigger`；检查 `jobs[].dueAt` |
| 审批人为空 | 候选人解析出 0 人 → 回退静态 assignee 也为空；查身份模式（`/identity/mode`）与数据（见 [04](04-organization-and-identity.md)） |
| 网关走错分支 | 用 `/conditions/eval` 代入实例变量试算条件（见 [05](05-conditions-and-decisions.md)） |
| 想跳过某环节 | 单实例用 `jump`；批量/结构性改动应改定义重发布 |

---

上一篇 ← [08 外部系统集成](08-external-integration.md) ｜ 下一篇 → [10 流程变量声明](10-variable-declaration.md)
