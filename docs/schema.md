# cmx-flow 流程引擎 · 数据库表结构

> 里程碑：M1（顺序审批）+ M2（并行网关 · 历史归档）+ M3（多实例会签/或签 · 实例取消）+ M2.5（边界定时器）+ M4.1（角色/岗位候选人）+ M4.2（抄送）+ M4.3（转签）+ M5.1（子流程 callActivity）+ M5.2（子流程组织路由）
> 数据库：PostgreSQL · schema：`public` · 表前缀：`cmx_flow_` · **无外键约束**（关联字段 + 索引替代）
> 迁移文件见文末「说明」。

## 概述

流程引擎把运行态拆成 **RU（运行态）** 与 **HI（历史态）** 两组表，对齐 Flowable 的 RU/HI 分离：

| 组 | 表 | 职责 |
|---|---|---|
| **RU** | `cmx_flow_instance` | 流程实例（聚合根：定义、业务键、状态、变量） |
| **RU** | `cmx_flow_token` | 令牌（流经流程图的执行指针） |
| **RU** | `cmx_flow_task` | 用户任务（等待态外化；M3 增 `element_value`；M4.3 增 owner/parent/delegation_state） |
| **RU** | `cmx_flow_mi_scope` | 多实例执行域（M3：会签/或签计数与游标账本） |
| **RU** | `cmx_flow_job` | 定时器作业（M2.5：边界定时器到期表） |
| **RU** | `cmx_flow_task_candidate` | 任务候选人池（M4.1：多人候选待认领） |
| **RU** | `cmx_flow_cc` | 抄送记录（M4.2：只读知会 + 已读追踪） |
| **RU** | `cmx_flow_task_delegation` | 转签台账（M4.3：转办/加签/委派流转链） |
| **HI** | `cmx_flow_hi_instance` | 历史实例（终态归档，含存续时长） |
| **HI** | `cmx_flow_hi_task` | 历史任务（办结归档，含办理时长） |

**身份体系（M4.1，非 flow 专属，复用/补齐 IAM）**：`cmx_user` / `cmx_role` / `cmx_user_role`（**已有，复用**）+ `cmx_org`（部门树）/ `cmx_position`（岗位）/ `cmx_user_position`（**M4.1 新建**）。办理人从静态字符串升级为候选人表达式 `role()`/`position()`/`org()`/`user()`，由 `PgIamAssigneeResolver` 在令牌到达时解析成真实用户。

**核心机制**：
- 引擎以「实例 + 其全部令牌 + 其任务 + 其多实例域 + 其定时器作业 + 其候选人池」为**聚合**原子读写。每个 BPMN 等待态（如用户任务）对应**一次数据库事务提交**——「等待态即提交点」。
- 实例进入终态（`COMPLETED` / `TERMINATED`）时，**同一事务内**幂等归档到 HI 表（`ON CONFLICT DO UPDATE`）。
- **多实例（会签/或签）**把一个逻辑 userTask 展开成多个并发/顺序子任务，用 `cmx_flow_mi_scope` 记账；随聚合快照全删重插，重启后完成条件仍能正确求值。
- **边界定时器（M2.5）**：令牌到达挂有边界定时器的 userTask 时，为每个定时器建一条 `cmx_flow_job`（记 `due_at`）。引擎用**可注入时钟** + 显式 `trigger_due_timers()` 推进——`find_due_jobs` 按 `due_at` 跨实例扫描到期作业，触发后令牌走定时器出边（中断型）或发旁路令牌（非中断型）。作业随聚合持久化，重启后定时器不丢。
- **候选人解析（M4.1）**：userTask 配候选人表达式时，令牌到达 → `AssigneeResolver` 解析成用户集：单人直派 `task.assignee`，多人落 `cmx_flow_task_candidate` 待 `claim` 认领。`AssigneeResolver` 是继 JavaDelegate、Clock 之后引擎的第三个可注入扩展点。
- 所有主键为 `VARCHAR(64)`（引擎生成的 UUID）；所有时间列为 `TIMESTAMPTZ`（对应 Rust `DateTime<Utc>`）；变量为 `JSONB`。

## 逻辑关系（无物理外键）

```
cmx_flow_instance (聚合根)
     │ id (PK)
     ├──< cmx_flow_token   token.instance_id → instance.id   (一实例多令牌)
     │         │ id (PK)
     │         └──< task.token_id → token.id                 (一令牌一等待任务)
     ├──< cmx_flow_task    task.instance_id → instance.id    (一实例多任务)
     ├──< cmx_flow_mi_scope scope.instance_id → instance.id  (一实例多多实例域，M3)
     │        node_bpmn_id 关联同节点的子任务/子令牌
     └──< cmx_flow_job     job.instance_id → instance.id     (一实例多定时器作业，M2.5)
              job.token_id → token.id  (作业挂在宿主令牌上)

实例终态归档（同事务）：
     cmx_flow_instance ──► cmx_flow_hi_instance   (幂等 upsert)
     cmx_flow_task(completed) ──► cmx_flow_hi_task
```

---

## 1. `cmx_flow_instance` — 流程实例表（RU · 聚合根）

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 实例唯一标识（UUID） |
| 2 | `definition_key` 🔍 | varchar(128) | NOT NULL | — | 流程定义 key（BPMN process id） |
| 3 | `business_key` 🔍 | varchar(128) | NULL | — | 业务键，对接业务单据 |
| 4 | `state` 🔍 | varchar(16) | NOT NULL | — | 实例状态（见枚举） |
| 5 | `variables` | jsonb | NOT NULL | `'{}'` | 实例级流程变量（动态 KV） |
| 6 | `created_at` | timestamptz | NOT NULL | — | 创建时间 |
| 7 | `updated_at` | timestamptz | NOT NULL | — | 最近更新时间 |
| 8 | `ended_at` | timestamptz | NULL | — | 完成/终止时间 |
| 9 | `org_id` | varchar(64) | NULL | — | **M5**：所属组织（M5.2 子流程组织路由依据；M5.1 恒空） |
| 10 | `parent_instance_id` 🔍 | varchar(64) | NULL | — | **M5**：父实例 id（子流程实例指向主实例；主实例为 NULL） |
| 11 | `parent_token_id` | varchar(64) | NULL | — | **M5**：父实例中挂起等待的令牌 id（子完成时精确唤醒） |

**索引**：`PK(id)` · `idx_..._defkey(definition_key)` · `idx_..._bizkey(business_key)` · `idx_..._state(state)` · `idx_..._parent(parent_instance_id)`

**`state` 枚举**：`ACTIVE`（活动中）· `COMPLETED`（已完成）· `TERMINATED`（已终止）

**子流程（M5.1）**：`callActivity` 节点调用一份独立部署的子流程并**同步等待**——主令牌转 `WAITING_SUBFLOW` 挂起，子实例（`parent_instance_id`/`parent_token_id` 指回主）跑完后按 `parent_token_id` 精确唤醒主令牌、回写输出变量、继续推进。子流程复用完整推进内核（可含会签/定时器/转签等全部能力），可嵌套。

---

## 2. `cmx_flow_token` — 令牌表（RU）

令牌 = 流经流程图的执行指针。`node_bpmn_id` 记录其当前占据的节点（稳定锚点，非 arena 下标）。`parent_id` 记录并行网关 fork 的血缘。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 令牌唯一标识（UUID） |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id（逻辑关联 instance.id） |
| 3 | `node_bpmn_id` | varchar(128) | NOT NULL | — | 当前所在节点的 BPMN id |
| 4 | `state` | varchar(16) | NOT NULL | — | 令牌状态（见枚举） |
| 5 | `parent_id` | varchar(64) | NULL | — | 父令牌 id（并行 fork 血缘） |
| 6 | `created_at` | timestamptz | NOT NULL | — | 创建时间 |
| 7 | `updated_at` | timestamptz | NOT NULL | — | 最近更新时间 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)`

**`state` 枚举**：`ACTIVE`（可推进）· `WAITING`（停在 userTask 等外部触发）· `JOINING`（停在并行网关 join 等兄弟令牌到齐）· `WAITING_SUBFLOW`（停在 callActivity 等子实例完成，M5）· `ENDED`（抵达结束事件）

> **M2 新增 `JOINING`**：并行网关合流时，先到的分支令牌以 `JOINING` 状态阻塞落库；当入边令牌全部到齐，合并为一个幸存令牌继续推进。这是「结构性阻塞」，无需外部触发。

---

## 3. `cmx_flow_task` — 用户任务表（RU）

令牌停在 userTask 时创建一条任务，外部办结后令牌恢复推进。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 任务唯一标识（UUID） |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 3 | `token_id` | varchar(64) | NOT NULL | — | 产生该任务的令牌 id |
| 4 | `node_bpmn_id` | varchar(128) | NOT NULL | — | 对应 userTask 节点 BPMN id |
| 5 | `name` | varchar(255) | NULL | — | 任务名（取自节点 name） |
| 6 | `assignee` 🔍 | varchar(128) | NULL | — | 办理人 |
| 7 | `candidate_groups` | varchar(512) | NULL | — | 候选组（逗号分隔，未解析） |
| 8 | `element_value` | jsonb | NULL | — | **M3**：多实例子任务携带的当前元素（会签每人各自数据；单实例为 NULL） |
| 9 | `completed` | boolean | NOT NULL | `false` | 是否已办结 |
| 10 | `created_at` | timestamptz | NOT NULL | — | 创建时间 |
| 11 | `completed_at` | timestamptz | NULL | — | 办结时间 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._assignee(assignee)` · `idx_..._open(assignee, completed)` ←「我的待办」复合索引

---

## 4. `cmx_flow_mi_scope` — 多实例执行域表（RU · M3）

多实例（multiInstance）把一个逻辑 userTask 展开成多个子任务：**并行 = 会签**（齐头并进），**顺序 = 或签**（逐个办理）。本表记账每一次展开的运行态：总数、已完成、顺序游标、完成条件。随聚合快照全删重插，与令牌/任务同生命周期——故进程重启后 `completionCondition` 仍能基于恢复的计数正确求值。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 域唯一标识（UUID） |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 3 | `node_bpmn_id` | varchar(128) | NOT NULL | — | 对应 multiInstance 节点 BPMN id（关联同节点子任务/子令牌） |
| 4 | `sequential` | boolean | NOT NULL | `false` | `true`=顺序(或签)；`false`=并行(会签) |
| 5 | `total` | integer | NOT NULL | — | 子实例总数（`nrOfInstances`） |
| 6 | `completed` | integer | NOT NULL | `0` | 已办结子实例数（`nrOfCompletedInstances`） |
| 7 | `next_index` | integer | NOT NULL | `0` | 顺序模式下一个待展开元素下标；并行模式恒 = total |
| 8 | `collection` | jsonb | NOT NULL | `'[]'` | 展开用的元素快照（定格避免中途变量被改） |
| 9 | `element_var` | varchar(128) | NULL | — | 子任务携带当前元素的变量名（`elementVariable`） |
| 10 | `completion_condition` | varchar(512) | NULL | — | 完成条件表达式（命中即提前收口剩余子实例） |
| 11 | `finished` | boolean | NOT NULL | `false` | 本域是否已收口（完成条件命中或全部完成） |

**索引**：`PK(id)` · `idx_..._instance(instance_id)`

**完成条件内置变量**（求值时以实例变量为底叠加，不落库）：`nrOfInstances` / `nrOfCompletedInstances` / `nrOfActiveInstances`。例：`${nrOfCompletedInstances/nrOfInstances >= 0.5}`（过半即通过）、`${rejected == true}`（任一驳回即终止或签）。

---

## 5. `cmx_flow_job` — 定时器作业表（RU · M2.5）

边界定时器的「到期待触发」表。令牌到达挂有边界定时器的 userTask 时，为每个定时器建一条作业（记 `due_at`）。引擎用**可注入时钟** + 显式 `trigger_due_timers()` 推进（不自带后台线程）：`find_due_jobs` 按 `due_at` 索引跨实例扫描到期作业，触发后令牌走定时器出边（中断型）或发旁路令牌（非中断型）。令牌离开宿主（办结/取消/被中断）即撤销其作业。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 作业唯一标识（UUID） |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 3 | `token_id` | varchar(64) | NOT NULL | — | 挂载令牌 id（停在宿主 userTask）；令牌离开即撤销 |
| 4 | `boundary_bpmn_id` | varchar(128) | NOT NULL | — | 触发时令牌要去的边界事件节点 bpmn_id |
| 5 | `cancel_activity` | boolean | NOT NULL | `true` | true=中断型（中断宿主任务）；false=非中断型（发旁路令牌） |
| 6 | `due_at` 🔍 | timestamptz | NOT NULL | — | 到期时刻（宿主到达时刻 + 时长） |
| 7 | `created_at` | timestamptz | NOT NULL | — | 作业创建时间 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._due(due_at)` ← 跨实例到期扫描（find_due_jobs）

**时长**：来自 BPMN `<timeDuration>` 的 ISO 8601 相对时长（`PT24H`/`PT30M`/`P1D` 等），编译期手写解析为秒。M2.5 只支持 `timeDuration`（相对时长），不支持 `timeDate`（绝对时刻）/ `timeCycle`（循环）。

---

## 6. `cmx_flow_task_candidate` — 任务候选人池表（RU · M4.1）

候选人表达式解析出「多人候选」时落此表：任一候选人 `claim`（认领）后写回 `task.assignee`、本任务候选记录清空。单人直派任务不产生候选记录（走 M1 老路）。随聚合快照全删重插，与任务同生命周期。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 候选记录 id |
| 2 | `task_id` 🔍 | varchar(64) | NOT NULL | — | 所属任务 id |
| 3 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 4 | `candidate_type` | varchar(16) | NOT NULL | — | 来源：USER / ROLE / POSITION / ORG |
| 5 | `candidate_ref` | varchar(128) | NOT NULL | — | 候选引用原值（role code / position code / org id） |
| 6 | `resolved_user_id` 🔍 | varchar(64) | NOT NULL | — | 解析出的具体用户 id（供「我的待办」按用户查） |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._user(resolved_user_id)` ←「我的待办」按用户 · `idx_..._task(task_id)`

**候选人表达式**：BPMN 里写成 `flowable:candidateGroups="finance"`（→ Role）、`flowable:candidateUsers="u1,u2"`（→ User）、或自定义 `cmx:candidates="position(cfo), org(d_fin)"`（混合）。由 `PgIamAssigneeResolver` 解析：Role 走 `cmx_role`+`cmx_user_role`，Position 走 `cmx_position`+`cmx_user_position`，Org 走 `cmx_org`+`cmx_user.org_id`（含子树）。

**相关身份表（非 flow 专属，M4.1 新建于通用命名空间）**：`cmx_org`（部门树，含物化路径 `path`）· `cmx_position`（岗位）· `cmx_user_position`（用户-岗位多对多）。角色沿用既有 `cmx_role` / `cmx_user_role`，不重造。

---

## 7. `cmx_flow_cc` — 抄送记录表（RU · M4.2）

抄送是流程的**只读旁路**：不阻塞流程、不产生待办、不影响令牌推进。每个被抄送人一条，带已读追踪。触发：节点配置 `cmx:cc="role(dept_head)"`（任务办结时）或办理人手动抄送（`notify_cc`）。随聚合快照全删重插；实例终态时记录保留供审计。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 抄送记录 id |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 3 | `node_bpmn_id` | varchar(128) | NULL | — | 抄送发生的节点（手动抄送为 NULL） |
| 4 | `to_user_id` 🔍 | varchar(64) | NOT NULL | — | 被抄送人 user id |
| 5 | `from_user_id` | varchar(64) | NULL | — | 抄送发起人（办理人；节点自动抄送可空） |
| 6 | `reason` | varchar(500) | NULL | — | 抄送说明 |
| 7 | `read_at` | timestamptz | NULL | — | 已读时刻（NULL = 未读） |
| 8 | `created_at` | timestamptz | NOT NULL | — | 抄送时刻 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._to_user(to_user_id, read_at)` ←「抄送我的」+ 未读过滤

**引擎 API**：`notify_cc`（手动抄送）· `mark_cc_read`（标记已读，幂等）· `cc_for_user(user, unread_only, limit)`（跨实例查「抄送我的」）。被抄送人同样由 `AssigneeResolver` 从表达式解析（`role()`/`user()` 等）。

---

## 8. `cmx_flow_task_delegation` — 转签台账表（RU · M4.3）

转办 / 加签 / 委派的流转链台账，可追溯「谁在什么任务上把它转给谁、为什么」。随聚合快照全删重插。配合 `cmx_flow_task` 新增的三列（`owner_user_id` / `parent_task_id` / `delegation_state`）实现转签语义。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 台账记录 id |
| 2 | `task_id` 🔍 | varchar(64) | NOT NULL | — | 被操作的任务 id |
| 3 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 4 | `kind` | varchar(20) | NOT NULL | — | TRANSFER / ADDSIGN_BEFORE / ADDSIGN_AFTER / DELEGATE |
| 5 | `from_user_id` | varchar(64) | NOT NULL | — | 操作发起人（原办理人） |
| 6 | `to_user_id` | varchar(64) | NOT NULL | — | 目标人 |
| 7 | `temp_task_id` | varchar(64) | NULL | — | 加签产生的临时任务 id（转办/委派为 NULL） |
| 8 | `reason` | varchar(500) | NULL | — | 转签意见 |
| 9 | `created_at` | timestamptz | NOT NULL | — | 操作时刻 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._task(task_id)`

**三种动作语义**（`cmx_flow_task` 的 `delegation_state` 驱动）：
- **转办 TRANSFER**：`assignee`+`owner_user_id` 都改新人（彻底换人，原人退出）。
- **委派 DELEGATE**：`owner_user_id` 保持原主，`assignee` 改代理人，`delegation_state=DELEGATED`（代办不换主）。
- **加签 ADDSIGN_BEFORE/AFTER**：原任务 `delegation_state=SUSPENDED`（挂起，不可直接办结），建临时任务（`parent_task_id` 指向原任务、`delegation_state=ADDSIGN`）；临时任务与原任务**共享令牌**（流程不推进），临时办结后原任务恢复。**可嵌套**（临时任务再被加签）。

**引擎 API**：`transfer_task` · `delegate_task` · `add_sign(before)`。

---

## 9. `cmx_flow_hi_instance` — 历史实例表（HI）

实例终态时归档，与热运行态解耦，供审计/查询。幂等 upsert。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 实例 id（与 RU 同值） |
| 2 | `definition_key` 🔍 | varchar(128) | NOT NULL | — | 流程定义 key |
| 3 | `business_key` 🔍 | varchar(128) | NULL | — | 业务键 |
| 4 | `state` | varchar(16) | NOT NULL | — | 终态：COMPLETED / TERMINATED |
| 5 | `variables` | jsonb | NOT NULL | `'{}'` | 归档时的实例变量快照 |
| 6 | `created_at` | timestamptz | NOT NULL | — | 实例创建时间 |
| 7 | `ended_at` | timestamptz | NULL | — | 实例结束时间 |
| 8 | `duration_ms` | bigint | NULL | — | 存续时长（ended − created，毫秒） |
| 9 | `archived_at` | timestamptz | NOT NULL | — | 归档写入时刻 |

**索引**：`PK(id)` · `idx_..._defkey(definition_key)` · `idx_..._bizkey(business_key)`

---

## 10. `cmx_flow_hi_task` — 历史任务表（HI）

办结任务归档，含办理时长，供工时分析/审计。幂等 upsert。

| # | 列名 | 类型 | 可空 | 默认 | 说明 |
|---|------|------|------|------|------|
| 1 | `id` 🔑 | varchar(64) | NOT NULL | — | 任务 id（与 RU 同值） |
| 2 | `instance_id` 🔍 | varchar(64) | NOT NULL | — | 所属实例 id |
| 3 | `node_bpmn_id` | varchar(128) | NOT NULL | — | 对应 userTask 节点 BPMN id |
| 4 | `name` | varchar(255) | NULL | — | 任务名 |
| 5 | `assignee` 🔍 | varchar(128) | NULL | — | 办理人 |
| 6 | `created_at` | timestamptz | NOT NULL | — | 任务创建时间 |
| 7 | `completed_at` | timestamptz | NULL | — | 办结时间 |
| 8 | `duration_ms` | bigint | NULL | — | 办理时长（completed − created，毫秒） |
| 9 | `archived_at` | timestamptz | NOT NULL | — | 归档写入时刻 |

**索引**：`PK(id)` · `idx_..._instance(instance_id)` · `idx_..._assignee(assignee)`

---

## Rust 类型对应

| PG 类型 | Rust 类型 | 备注 |
|---|---|---|
| `varchar` | `String` / `Option<String>` | |
| `timestamptz` | `DateTime<Utc>` / `Option<...>` | 写 NULL 须带类型标记 `NullTyped(Timestamp)` |
| `bigint` | `i64` / `Option<i64>` | 写 NULL 须 `NullTyped(Int)` |
| `jsonb` | `Variables`（`BTreeMap<String, Value>`） | |
| `boolean` | `bool` | |
| `state` 文本列 | `InstanceState` / `TokenState` 枚举 | SCREAMING_SNAKE_CASE 字符串 |

## 定义态配置表（非实例聚合）

`cmx_flow_subflow_binding`（M5.2）——**子流程组织绑定**，不是实例运行态，而是「同一主流程按组织跑不同子流程」的路由配置。主流程 callActivity 写逻辑 key（`cmx:calledKey`，如 `fin_review`），各组织把「逻辑 key + 本组织 → 具体子流程定义 key」绑定在此表。

| 列名 | 类型 | 说明 |
|------|------|------|
| `id` 🔑 | varchar(64) | 绑定 id |
| `called_key` 🔍 | varchar(128) | 逻辑子流程 key |
| `org_id` 🔍 | varchar(64) | 适用组织（NULL = 默认兜底绑定） |
| `target_definition_key` | varchar(128) | 解析到的具体子流程定义 key |
| `enabled` | boolean | 是否启用（FALSE 不参与解析） |

**运行期解析**（`PgSubflowRouter`，与 `cmx_org` 同库）三层：① **精确**（本组织绑定）→ ② **继承**（沿 `cmx_org.path` 向上找最近祖先绑定，path 最长优先）→ ③ **兜底**（`org_id IS NULL` 默认绑定）。全无 → 报错。是继 `AssigneeResolver`（M4.1）之后引擎的第 4 个可注入扩展点。

## 说明

- 🔑 = 主键，🔍 = 建有索引。
- 表结构目前由 `cmx-flow-store-pg` 内置 DDL 自举（`ensure_schema()`），并已同步到 `docs/sql/migrations/` 与 `docs/sql/init/init_ddl.sql`。
- 迁移文件：`20260717_001_cmx_flow_engine_tables.{up,down}.sql`（M1+M2）、`20260717_002_cmx_flow_multi_instance.{up,down}.sql`（M3）、`20260717_003_cmx_flow_job.{up,down}.sql`（M2.5）、`20260718_001_cmx_flow_identity.{up,down}.sql`（M4.1）、`20260718_002_cmx_flow_cc.{up,down}.sql`（M4.2）、`20260718_003_cmx_flow_delegation.{up,down}.sql`（M4.3）、`20260718_004_cmx_flow_subflow.{up,down}.sql`（M5.1）、`20260718_005_cmx_flow_subflow_binding.{up,down}.sql`（M5.2）。
- M3 的 `element_value` 补列在 001 已建表的库上以 `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` 幂等补齐。
- M4.1 的 IAM 表（`cmx_role`/`cmx_user_role`）**复用既有**，仅新建 `cmx_org`/`cmx_position`/`cmx_user_position`/`cmx_flow_task_candidate`；这些通用身份表位于 **cmx** 库（IAM 所在），与 flow 运行态表可同库或分库部署。

_生成日期：2026-07-18 · cmx-flow 流程引擎 M1+M2+M3+M2.5+M4.1+M4.2+M4.3+M5.1+M5.2_
