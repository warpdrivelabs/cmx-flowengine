# 04 · 组织机架 · 用户 · 角色 · 岗位对接

本篇讲**「谁能办这个任务」**：如何把审批人写成角色/岗位/组织/关系型，引擎怎么把它解析成具体用户，以及三种身份后端（内置 / 平台 PG / 外部 HTTP）如何对接。

## 4.1 核心理念：身份是可注入扩展点

引擎内核**不硬编码任何主数据**。userTask 上的审批人可以是静态用户 id，也可以是**候选人表达式**（`role(finance)`、`position(cfo)`、`orgLeader`、`initiator` …）。令牌到达任务时，引擎调用注入的 `AssigneeResolver` 把表达式解析成真实用户集。

```
userTask 候选人表达式 ──令牌到达──► AssigneeResolver.resolve() ──► [用户id...]
                                          │
              ┌───────────────────────────┼───────────────────────────┐
        LocalAssigneeResolver      PgIamAssigneeResolver        HttpAssigneeResolver
        （内置 fid_* 表）           （平台 cmx_* 表）             （外部 IAM HTTP）
```

解析结果：**0 人**→回退静态 assignee；**1 人**→直派；**≥2 人**→落候选池待认领（`claim`）。

## 4.2 候选人表达式语法

写在 userTask 属性上（前缀无关）：

```xml
<bpmn:userTask id="review" name="审批"
    flowable:candidateUsers="u1,u2"
    flowable:candidateGroups="finance,legal"
    cmx:candidates="position(cfo), org(d_fin), orgLeader"
    cmx:cc="role(dept_head)"/>
```

语法：逗号分隔的项，每项是 `kind(value)` 或裸 token。

### 七种候选人类型 CandidateKind

| 类型 | 写法 | 含义 | 解析成 |
|------|------|------|--------|
| User | `user(u1)` / `u(u1)` | 具体用户 id | 该 id 本身（不查库） |
| Role | `role(finance)` / `group(finance)` | 角色 code | 拥有该角色的所有用户 |
| Position | `position(cfo)` / `pos(cfo)` / `post(cfo)` | 岗位 code | 在该岗位上的所有用户 |
| Org | `org(d_fin)` / `dept(d_fin)` / `department(d_fin)` | 组织 id | 该组织**及其子树**的所有用户 |
| OrgLeader | `orgLeader(d_fin)` / `leader` / `deptLeader` | 组织领导 | 该组织的 `leader_user_id`；省略参数则取实例组织 |
| Initiator | `initiator` / `starter` | 流程发起人 | 发起人本人（值忽略） |
| InitiatorLeader | `initiatorLeader` / `starterLeader` | 发起人上级 | 发起人所属组织的领导 |

**前缀同义词**（大小写不敏感）：见上表第二列。未知前缀如 `foo(bar)` → 回退到调用点的默认类型，整串作为值。

**裸 token 规则**：

- 裸关系型关键字（`initiator` / `initiatorLeader` / `orgLeader`）→ 该类型 + **空值**（锚点来自运行期上下文）。
- 其它裸 token → 默认类型 + token 作为值。

**调用点默认类型**：

| 来源属性 | 默认 CandidateKind |
|----------|-------------------|
| `candidateUsers` | User |
| `candidateGroups` | Role |
| `candidates`（`cmx:`） | User |
| `cc`（`cmx:`） | User |

所以 `candidateGroups="finance,legal"` = 两条 Role；`candidateUsers="u1,u2"` = 两条 User；`cmx:candidates="cfo"` 裸 token = 一条 User(cfo)（要岗位得写 `position(cfo)`）。

### 关系型候选人（P0）

关系型解析依赖运行期上下文 `ResolveContext { initiator, org_id }`：

- `initiator` = 发起人 user id（引擎从实例变量 `initiator` 取）。
- `org_id` = 实例所属组织（发起时传的 `orgId`）。

例：

```xml
<!-- 发起人的直属上级审批 -->
<bpmn:userTask id="approve" name="上级审批" cmx:candidates="initiatorLeader"/>

<!-- 本实例组织的领导审批 -->
<bpmn:userTask id="dept" name="部门领导审批" cmx:candidates="orgLeader"/>

<!-- 指定组织 d_fin 的领导审批 -->
<bpmn:userTask id="fin" name="财务领导审批" cmx:candidates="orgLeader(d_fin)"/>
```

## 4.3 三种身份后端

`FLOW_IDENTITY_MODE` 环境变量决定用哪个（注意 `local` 是特殊值，其余走适配器）：

| 值 | 后端 | 数据源 | 何时用 |
|----|------|--------|--------|
| `local` | `LocalAssigneeResolver` | 内置 `fid_*` 表 | 独立部署、开箱即用、不想接外部 IAM |
| `pg`（默认） | `PgIamAssigneeResolver` | 平台 `cmx_*` 表 | 内嵌平台 / 回连平台库 |
| `http` | `HttpAssigneeResolver` | 外部 IAM HTTP 服务 | 独立部署、对接企业既有 IAM |
| `mock` | `MockAssigneeResolver` | 无（确定性假数据） | 测试 / demo |

> `local` 模式下额外开放身份管理 CRUD 端点 + 四区身份工作台；其余模式该 crate 不接入（零回归）。

## 4.4 内置身份模块（`fid_*` 表，local 模式）

独立部署、无外部 IAM 时用内置身份。6 张表，前缀 `fid_`（flow-identity，刻意不叫 `cmx_user` 以免冒充平台 IAM），无外键，`ensure_schema()` 幂等自建。

### 表结构

**`fid_org`** —— 组织树（`parent_id` + `path` 物化路径；`leader_user_id` 支撑关系型解析）

```sql
CREATE TABLE IF NOT EXISTS fid_org (
    id             VARCHAR(64)  PRIMARY KEY,
    code           VARCHAR(100) NOT NULL,
    name           VARCHAR(200) NOT NULL,
    parent_id      VARCHAR(64),
    path           VARCHAR(500),       -- 物化路径，如 /root/fin/ap；子树匹配靠前缀
    leader_user_id VARCHAR(64),        -- 组织领导，orgLeader/initiatorLeader 解析用
    sort_order     INTEGER      NOT NULL DEFAULT 0,
    archived       INTEGER      NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now()
);
```

**`fid_role`** / **`fid_position`** —— 角色 / 岗位（结构相同）

```sql
CREATE TABLE IF NOT EXISTS fid_role (
    id VARCHAR(64) PRIMARY KEY, code VARCHAR(100) NOT NULL, name VARCHAR(200) NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- fid_position 同构
```

**`fid_user`** —— 用户（`org_id` 指向所属组织，一对一）

```sql
CREATE TABLE IF NOT EXISTS fid_user (
    id VARCHAR(64) PRIMARY KEY, username VARCHAR(100) NOT NULL, name VARCHAR(200),
    org_id VARCHAR(64),
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

**`fid_user_role`** / **`fid_user_position`** —— 用户↔角色 / 用户↔岗位（多对多，复合主键）

```sql
CREATE TABLE IF NOT EXISTS fid_user_role (
    user_id VARCHAR(64) NOT NULL, role_id VARCHAR(64) NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, role_id)
);
-- fid_user_position 同构（user_id, position_id）
```

> 组织是 1:N（用户经 `fid_user.org_id` 属于恰好一个组织，无 user_org 关联表）；角色/岗位是 M:N。

### 关系型解析逻辑（`LocalAssigneeResolver`）

| 候选人 | 解析 SQL 逻辑 |
|--------|--------------|
| `user(id)` | 直接返回 id，不查库 |
| `role(code)` | `fid_user_role ⋈ fid_role`（按 **code** 匹配，非 id），取 user_id |
| `position(code)` | `fid_user_position ⋈ fid_position`（按 code），取 user_id |
| `org(id)` | `fid_user.org_id ∈ 子树`（`fid_org.path LIKE root.path‖'%' OR id=root`），取 user_id |
| `orgLeader(id)` | `fid_org.leader_user_id`（id 非空且非''） |
| `orgLeader`（空值） | 取 `ctx.org_id` 的领导 |
| `initiator` | `ctx.initiator` |
| `initiatorLeader` | 发起人所属组织（`fid_user.org_id ⋈ fid_org`）的 `leader_user_id` |

组织无领导时返空（宽容语义，不报错）。

### 身份 CRUD API（local 模式）

`entity` ∈ `orgs | roles | positions | users`。

```bash
# 身份模式探测
curl http://127.0.0.1:8091/api/flow/v1/identity/mode
# → {code:0, data:{mode:"local", editable:true}}   external 模式 editable:false 只读

# 列组织
curl http://127.0.0.1:8091/api/flow/v1/identity/orgs
# → {code:0, data:{items:[{id,code,name,parentId,path,leaderUserId,sortOrder}]}}

# 建/改组织（upsert by id）
curl -X POST http://127.0.0.1:8091/api/flow/v1/identity/orgs \
  -H 'Content-Type: application/json' \
  -d '{"id":"fin","code":"FIN","name":"财务部","parentId":null,"leaderUserId":"u_leader","sortOrder":1}'
# → {code:0, data:{id:"fin"}}

# 建/改用户（org_id 指定所属组织）
curl -X POST http://127.0.0.1:8091/api/flow/v1/identity/users \
  -H 'Content-Type: application/json' \
  -d '{"id":"u_staff","username":"staff","name":"王职员","orgId":"fin"}'

# 给用户设角色（全量覆盖）
curl -X POST http://127.0.0.1:8091/api/flow/v1/identity/users/u_staff/roles \
  -H 'Content-Type: application/json' \
  -d '{"roleIds":["r_finance","r_reviewer"]}'
# → {code:0, data:{userId:"u_staff", roleCount:2}}

# 软删（archived=1）
curl -X DELETE http://127.0.0.1:8091/api/flow/v1/identity/users/u_staff
# → {code:0, data:{deleted:"u_staff"}}
```

| 端点 | 作用 |
|------|------|
| `GET /identity/mode` | `{mode:"local"\|"external", editable}` |
| `GET /identity/{entity}` | 列非归档行 `{items:[...]}` |
| `POST /identity/{entity}` | upsert（external 模式返业务错误，只读） |
| `DELETE /identity/{entity}/{id}` | 软删 |
| `POST /identity/users/{id}/roles` | 全量设角色，body `{roleIds:[...]}` |

**能力边界**：内置模块提供组织/角色/岗位/用户 CRUD + 用户设角色。用户设岗位（`fid_user_position`）目前**无写入端点**（表被解析器读，但 store 只暴露 `set_user_roles`）；组织领导经组织 upsert 的 `leaderUserId` 设置。`path` 简化写成 `/{id}`，需要真实层级路径时应用侧重算（demo 用 `UPDATE fid_org SET path=...` 手工修）。

### 上手示例（对齐 P0-b 测试）

```
1. 建组织：财务部 fin(领导=u_leader) → 子部门 应付组 ap(parentId=fin)
2. 修 path：fin='/fin'，ap='/fin/ap'（让子树前缀匹配成立）
3. 建角色 finance；建用户 u_leader(org=fin)、u_staff(org=ap)
4. u_staff 设角色 [finance]
→ role(finance)     解析出 u_staff
→ org(fin)          解析出子树全用户 {u_staff, u_leader}
→ orgLeader(fin)    解析出 u_leader
→ initiator(=u_staff)         解析出 u_staff
→ initiatorLeader（ap 无领导）  解析出空 → 给 ap 设领导后 = 该领导
```

## 4.5 平台 IAM 后端（`cmx_*` 表，pg 模式，默认）

内嵌平台或回连平台库时用 `PgIamAssigneeResolver`（连 `IAM_DB_ID`，默认 `primary`/demo 里是 `cmx` 库）：

| 候选人 | 平台表 |
|--------|--------|
| Role | `cmx_role` + `cmx_user_role` |
| Position | `cmx_position` + `cmx_user_position` |
| Org | `cmx_org`（部门树，含 `path`）+ `cmx_user.org_id`（含子树） |

角色/岗位/组织表复用平台既有 IAM（`cmx_role`/`cmx_user_role` 沿用不重造，`cmx_org`/`cmx_position`/`cmx_user_position` 由 flow M4.1 补建）。这些表可与 flow 运行态表同库或分库。

## 4.6 外部 IAM 后端（HTTP 模式）

独立部署、对接企业既有 IAM 时，设 `FLOW_IDENTITY_MODE=http` + `FLOW_IDENTITY_URL`，引擎用 `HttpAssigneeResolver` 外呼：

**请求** `POST {FLOW_IDENTITY_URL}/identity/resolve`：

```json
{
  "kind": "ROLE",          // USER|ROLE|POSITION|ORG|ORG_LEADER|INITIATOR|INITIATOR_LEADER
  "value": "finance",      // 用户id/角色code/岗位code/组织id（关系型为""）
  "initiator": "u_boss",   // 可选，关系型解析用（None 时不带）
  "orgId": "d_fin"         // 可选，关系型解析用
}
```

**响应**：

```json
{ "userIds": ["u1", "u2"] }
```

短路优化（不发 HTTP）：`kind=User` 直接返回 `[value]`；`kind=Initiator` 直接返回 `ctx.initiator`。其余打 HTTP。

错误处理：`4xx` → `InvalidRef`（身份服务拒绝）；其它非 2xx / 请求失败 / 解析失败 → `Backend`。

> 企业接入模板：实现一个 `/identity/resolve` 端点，输入候选人类型+值+上下文，返回用户 id 数组，即可让 flow 用你的组织架构做审批人解析，无需把组织数据同步进 flow。

## 4.7 认领（多人候选 → 据为己有）

候选人解析出 ≥2 人时任务落**候选池**，任一候选人认领后据为己有、其余候选作废：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/tasks/<taskId>/claim \
  -H 'Content-Type: application/json' \
  -d '{"instance_id":"<iid>","user_id":"u_staff"}'
```

> ⚠ claim 的请求体用 **snake_case**（`instance_id`/`user_id`），是少数几个非 camelCase 端点之一，详见 [06](06-rest-api-reference.md) 与 [07](07-task-operations.md)。

任务详情里 `candidates` 展示当前候选池：

```json
"candidates":[{"userId":"u1","type":"ROLE","ref":"finance"},
              {"userId":"u2","type":"ROLE","ref":"finance"}]
```

## 4.8 完整示例：候选人认领审批（candidate_approval）

```xml
<bpmn:process id="candidate_approval" name="候选人认领审批" isExecutable="true">
  <bpmn:startEvent id="start" name="提交报销"/>
  <bpmn:sequenceFlow id="f0" sourceRef="start" targetRef="manager"/>

  <!-- 岗位候选：解析「部门经理」岗的人（demo 播种 1 人 → 直派） -->
  <bpmn:userTask id="manager" name="部门经理初审" cmx:candidates="position(df_mgr)"/>
  <bpmn:sequenceFlow id="f1" sourceRef="manager" targetRef="finance"/>

  <!-- 角色候选 + 抄送：财务组多人 → 落候选池待认领；办结时抄送财务组 -->
  <bpmn:userTask id="finance" name="财务组审批" flowable:candidateGroups="df_finance"
                 cmx:cc="role(df_finance)"/>
  <bpmn:sequenceFlow id="f2" sourceRef="finance" targetRef="done"/>
  <bpmn:endEvent id="done" name="审批完成"/>
</bpmn:process>
```

- `manager` 用 `position(df_mgr)` → 岗位「部门经理」（1 人）→ 直派。
- `finance` 用 `candidateGroups="df_finance"` → 角色「财务组」（多人）→ 落候选池，某人 claim 后独占；`cmx:cc="role(df_finance)"` 使办结时抄送整个财务组。

## 4.9 发起人上下文怎么传

关系型候选人（`initiator`/`initiatorLeader`）依赖 `ResolveContext`：

- **发起人**：发起流程时把 `initiator` 放进变量，或由前端/网关注入当前登录用户；引擎从实例变量 `initiator` 取。
- **组织**：发起时传 `orgId`（同时驱动子流程组织路由，见 [03](03-subprocess.md)）。

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"leave_request",
       "orgId":"ap",
       "variables":{"initiator":"u_staff","days":3}}'
# 后续 initiatorLeader 节点 → 解析出 u_staff 所属组织(ap)的领导
```

---

上一篇 ← [03 子流程定义](03-subprocess.md) ｜ 下一篇 → [05 分支条件与决策表](05-conditions-and-decisions.md)
