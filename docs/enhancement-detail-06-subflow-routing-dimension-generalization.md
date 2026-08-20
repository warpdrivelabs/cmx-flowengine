# ⑥ 详细设计：子流程路由维度泛化 —— 从「按组织机构」到「按任意字典」

> 状态：**方案设计（不动代码）**。日期：2026-08-18。
> 关联 [M5.2 组织路由](m5-design.html)、[②多实例](enhancement-detail-02-dynamic-multi-instance-assignee.md)、[⑤变量声明](enhancement-detail-05-variable-schema-declaration.md)。
> 目标：主流程里每个 callActivity（子流程挂载点）当前**只能按组织机构**选子流程。本方案把「组织机构」这一**写死的路由维度**泛化成**任意字典**——
> - **不同主流程**的挂载点可用**不同**字典（报销流按「组织机构」，风控流按「风险等级」，采购流按「采购品类」）；
> - **同一主流程**的**不同挂载点**也可用**不同**字典（挂载 A 按组织、挂载 B 按法人公司）；
> - 维度**不必是组织机构**——可以是 `cf_*` 里的任意字典（平级或自分级）。

---

## 目录

1. [问题与现状（对着源码核对）](#1-问题与现状对着源码核对)
2. [设计总览与三条铁律](#2-设计总览与三条铁律)
3. [核心概念：路由维度（RoutingDimension）](#3-核心概念路由维度routingdimension)
4. [维度值从哪来：实例的「维度上下文」](#4-维度值从哪来实例的维度上下文)
5. [模型层变更](#5-模型层变更)
6. [路由契约 SubflowRouter 的泛化](#6-路由契约-subflowrouter-的泛化)
7. [PgSubflowRouter：三层解析泛化到任意字典](#7-pgsubflowrouter三层解析泛化到任意字典)
8. [绑定表与维度字典读取](#8-绑定表与维度字典读取)
9. [BPMN / 编译器变更](#9-bpmn--编译器变更)
10. [引擎 launch_one_subflow 变更](#10-引擎-launch_one_subflow-变更)
11. [五个 SubflowRouter 实现的同步](#11-五个-subflowrouter-实现的同步)
12. [App 端点与设计器变更](#12-app-端点与设计器变更)
13. [数据模型与存储变更汇总](#13-数据模型与存储变更汇总)
14. [分阶段路线图](#14-分阶段路线图)
15. [风险与取舍](#15-风险与取舍)
16. [附录：三个完整示例](#16-附录三个完整示例)

---

## 1. 问题与现状（对着源码核对）

### 痛点（用户原话）
- 每个有子流程的主流程，**按组织机构设置子流程**时——
  - 组织机构字典**可以不同主流程用不同的**；
  - 甚至**同一主流程不同子流程设置点**也可以设置**不同的**组织机构字典；
  - 而且**不一定非得是组织机构字典，可以是任意字典**。

一句话：**「按组织路由」应该是「按某个字典路由」的一个特例**，字典是谁、由主流程/挂载点各自决定。

### 现状核对（逐条对源码）

| 能力 | 现状 | 位置 |
|------|------|------|
| 路由契约 | `SubflowRouter::resolve(called_key, **org_id: Option<&str>**)` —— 维度**写死为组织 id** | `cmx-flow-model/src/subflow.rs:46` |
| 维度值来源 | `let org = parent_snap.instance.**org_id**.clone()` —— 取实例行上的 org_id 标量列 | `cmx-flow-engine/src/engine.rs:591` |
| 维度是否分挂载点 | ❌ **实例级**：`org_id` 是 `cmx_flow_instance` 一列，发起时设一次、全子实例继承同一个；一个实例内**所有** callActivity 都用**同一个** org | `ddl.rs:26`、`engine.rs:591` |
| 挂载点身份 | ✅ `node_bpmn`（callActivity 的 bpmn id）**已在 `launch_one_subflow` 调用点在手**，但只用于去重/incident，**路由时被丢弃** | `engine.rs:576,599` |
| 绑定表 | `cmx_flow_subflow_binding(called_key, **org_id**, target_definition_key, enabled)` —— 维度列写死 `org_id` | `ddl.rs:171-180` |
| 三层解析 | 精确 `org_id=?` → 继承 `JOIN cmx_org ... self.path LIKE anc.path||'%'` → 兜底 `org_id IS NULL` | `subflow_router.rs:59-103` |
| 维度树来源 | 写死 `SELECT id,name,parent_id,path FROM **cmx_org** WHERE archived=0` | `subflow_router.rs:299-300` |
| 设计器 | callActivity 属性面板：模式卡「按组织路由 / 固定子流程」+ 组织下拉（**扁平 `<select>` 按 path 缩进**）+ 目标子流程下拉 | `web/core/design-workbench.js:1004-1036,1434-1441` |
| 字典系统 | ✅ `cf_*` 物理表 + `/api/dct/*` 服务已就绪；自分级字典有 `parent_id`+`full_path`+`level_no`+`is_leaf`（与 `cmx_org` 同构） | `cmx-dct/*`、`base_dct_meta_v1.json:245-336` |

**结论**：`cmx_org` 只是众多「自分级字典」中的一个特例。把路由维度从「组织 id」抽象成「某字典的一个条目 id」，即可满足全部诉求。所有需要泛化的接缝**已在源码里全部定位**（下文逐一给出），且**引擎中立性、五壳同构、向后兼容**都能保住。

### 关键事实：`cmx_org` 与自分级 `cf_*` 是同构的

这是本方案可行的地基——组织路由的「沿树向上继承」之所以成立，靠的是 `cmx_org.path` 物化路径；而**任意自分级字典同样有物化路径**：

| | 组织 `cmx_org` | 自分级字典 `cf_*`（如 `cf_gl_account`） |
|--|---------------|--------------------------------------|
| 主键 | `id` | `id`（或 code-PK 字典用 `code`） |
| 父引用 | `parent_id` | `parent_id`（`dictMeta.parentField`） |
| 物化路径 | `path` | `full_path`（`hierarchyFieldSet`） |
| 路径形态 | **斜杠**分隔、**id** 段、有前导 `/`：`/df_root/df_bj` | **点**分隔、**code** 段、无前导：`1000000.1100000` |
| 前缀可继承 | ✅ `LIKE anc||'%'` | ✅ `LIKE anc||'%'` |

**⚠️ 落地必须处理的差异**：路径分隔符 / 段来源 / 有无前导符**因字典而异**，故泛化后的路由配置**必须携带路径列名 + 分隔符**（不能沿用 `.path` 硬编码）。同时现有 `self.path LIKE anc.path||'%'` 有**潜在边界 bug**（`/a` 会错配 `/ab`），泛化时应**追加分隔符**修正为 `LIKE anc.path || '<delim>%'`。

---

## 2. 设计总览与三条铁律

```
设计态（每个 callActivity 挂载点）：
   模式卡「按维度路由 / 固定子流程」
     └ 维度路由：选【路由维度】(= 某字典 dictCode) + 逻辑 key(cmx:calledKey)
                  └ 配置绑定：维度条目(字典树/表选一条) → 目标子流程定义
     │ 写进 BPMN：cmx:calledKey="fin_review" + cmx:dimKey="org"（新增，缺省=org 向后兼容）
     ▼
编译：cmx-flow-bpmn 解析 → CallActivity{ called_key, dim_key(新), ... }
     │
发起态：POST /instances 传 dimensions:{ "org":"df_bj", "risk_level":"R3", ... }
     │ 落 cmx_flow_instance.dimensions jsonb（org_id 保留为 "org" 维度的快捷别名，兼容）
     ▼
运行态 launch_one_subflow：读挂载点 ca.dim_key → 取实例 dimensions[dim_key] 维度值
     → router.resolve(called_key, dim_key, dim_value) → 目标子流程 key
     ▼
解析（PgSubflowRouter）：查 cmx_flow_subflow_binding(called_key, dim_key, dim_value)
     精确 → 沿该维度字典 full_path 继承 → 兜底(dim_value IS NULL) → NoBinding→Incident
```

**三条铁律**（对齐 flow 引擎既有纪律，一条都不破）：

1. **引擎中立**：引擎只认「维度 key + 维度值 + 逻辑 key」三个字符串，**永不认识任何字典/组织/DB**。字典解析全在可注入的 `SubflowRouter` 实现里，与 M5.2 一样。引擎 crate 零新依赖。
2. **向后兼容**：不写 `cmx:dimKey` 的 callActivity **默认维度 = `org`**，行为与今天**逐字节一致**；老绑定（`org_id` 列）平滑迁移为 `dim_key='org'` 的绑定。M5.1 写死 `calledElement` 的路径完全不动。
3. **五壳同构**：`SubflowRouter` 有 5 个实现（契约/Pg/Http/Mock/测试 FakeRouter），签名变更**一次性**同步全部，保「一芯多壳」不漂移。

---

## 3. 核心概念：路由维度（RoutingDimension）

把今天写死的「组织」抽象成一个**路由维度**：

> **路由维度** = 「用哪个字典来区分、字典里的哪一条来定位子流程」。

一个路由维度由三要素刻画（都是字符串，引擎只搬运）：

| 要素 | 含义 | 例（组织） | 例（风险等级） |
|------|------|-----------|--------------|
| `dimKey` | 维度的稳定标识（= 该维度绑定的**字典 dictCode**，或一个别名） | `org` | `risk_level` |
| `dimValue` | 运行实例在该维度上的**取值**（= 字典某条目的 id/code） | `df_bj`（北京分公司 org id） | `R3`（风险等级 code） |
| 维度字典元信息 | 该字典是否自分级、路径列名、分隔符（决定能否「沿树继承」） | `cmx_org` / `path` / `/` | `cf_risk_level` / `full_path` / `.` |

**`org` 是一个内建维度**：`dimKey="org"` 约定映射到 `cmx_org` 表 + `path` 列 + `/` 分隔（保住今天的行为）。其余维度都指向某个 `cf_*` 字典。

**为什么维度绑到 dictCode 而非 tableName**：DCT 的稳定身份是 `dictMeta.dictCode`（`tableName` 是次级），`/api/dct/meta?dict=<dictCode>` 一把拿到 `selfHierarchy`/`parentField`/`tableName`/路径列，所以**设计态选维度 = 选 dictCode**，运行态元信息由 dictCode 解析。

---

## 4. 维度值从哪来：实例的「维度上下文」

**今天**：`cmx_flow_instance.org_id` 一个标量列，发起时从 `StartReq.orgId` 设一次，全子实例继承（`engine.rs:591` `let org = parent.instance.org_id`）。

**问题**：一个实例只有**一个** org 值 → 无法表达「挂载 A 按组织=北京、挂载 B 按法人=某公司」，因为挂载 A/B 读的是同一个标量。（这正是 [M5.3 memory line17] 记的既有边界：「同实例多挂载各按本实例组织解析**不同类型**子流程可以；各挂载走**不同维度**做不到——需给 binding 加挂载点级维度覆盖，暂无需求未做」。本方案就是补这个缺口。）

**方案**：把实例的单一 `org_id` 升级成**维度上下文** `dimensions: Map<String, String>`——一个实例可同时携带多个维度取值：

```jsonc
// 发起报销流，该实例同时具备组织、法人、风险等级三个维度取值
POST /instances { "definitionKey":"reimburse", "dimensions": {
    "org": "df_bj", "legal_entity": "LE_0021", "risk_level": "R3"
}}
```

- 挂载点 A（`cmx:dimKey="org"`）→ 读 `dimensions["org"]="df_bj"` → 按组织字典解析；
- 挂载点 B（`cmx:dimKey="legal_entity"`）→ 读 `dimensions["legal_entity"]="LE_0021"` → 按法人字典解析。

**存储**：`cmx_flow_instance` 加一列 `dimensions jsonb`（幂等 ALTER）。`org_id` 列**保留**——作为 `dimensions["org"]` 的快捷别名/兼容投影（发起时若只传 `orgId` 则等价 `dimensions={"org":orgId}`；读时 `dimensions` 缺省回退 `org_id`）。子实例仍**整体继承**父的 `dimensions`（默认继承规则不变，只是从一个值变成一张表）。

**为什么是 jsonb 而非多列**：维度是**动态的、随主流程/字典扩展的**，不能预定义列。jsonb 对齐 `variables` 列的既有做法（`cmx_flow_instance.variables jsonb`），零 schema 演进成本。

> **可选简化（P0 落地档）**：若首期只需「不同主流程/不同挂载点用不同字典、但一个实例仍是单一组织维度取值」，可先**不加 `dimensions` 列**，让所有维度暂时复用 `org_id` 标量列的值（即维度 key 决定「查哪个字典」，但取值仍来自实例的单一 org 上下文）。这满足「维度可不同、字典可不同」，但不满足「同实例内挂载 A/B 取不同维度值」。`dimensions` 列是解锁后者的关键，建议 P1 补齐。见 §14 路线图。

---

## 5. 模型层变更

### 5.1 CallActivity 加 `dim_key`（挂载点声明用哪个维度）

`cmx-flow-model/src/ir.rs:110` 的 `CallActivity` 加一个可选字段（与 `called_key` 同款 `skip_serializing_if`，向后兼容）：

```rust
pub struct CallActivity {
    pub called_element: String,          // 已有：M5.1 写死 key
    pub called_key: Option<String>,      // 已有：M5.2 逻辑名
    /// 【新增】路由维度 key（= 某字典 dictCode，或内建 "org"）。
    /// None/"" → 默认 "org"（向后兼容，等价今天的组织路由）。
    pub dim_key: Option<String>,
    pub input_vars: Vec<VarMapping>,     // 已有
    pub output_vars: Vec<VarMapping>,    // 已有
}
```

### 5.2 实例加 `dimensions`（维度上下文）

`cmx-flow-model/src/runtime.rs:105` 的 `ProcessInstance`：`org_id: Option<String>` 保留，**加** `dimensions: BTreeMap<String, String>`（默认空；`org_id` 存在时视作 `{"org": org_id}` 的投影）。序列化 camelCase。

### 5.3 路由错误 RouteError 带维度信息

`cmx-flow-model/src/subflow.rs:21` 的 `RouteError::NoBinding` 把 `org: Option<String>` 泛化成 `dim_key: String` + `dim_value: Option<String>`（归因文案更准：「维度 risk_level=R3 找不到 fin_review 的绑定」）。

---

## 6. 路由契约 SubflowRouter 的泛化

`cmx-flow-model/src/subflow.rs:46` 唯一契约方法签名变更——**从两参到三参**：

```rust
#[async_trait]
pub trait SubflowRouter: Send + Sync {
    /// 把「逻辑子流程 key + 路由维度 + 维度取值」解析成具体子流程定义 key。
    /// - called_key：callActivity 的逻辑名（如 fin_review）
    /// - dim_key：路由维度（= 字典 dictCode 或内建 "org"）
    /// - dim_value：主实例在该维度上的取值（None = 无上下文，回退默认绑定）
    async fn resolve(
        &self,
        called_key: &str,
        dim_key: &str,
        dim_value: Option<&str>,
    ) -> RouteResult<String>;
}
```

**兼容策略**：`dim_key` 恒非空（缺省由上游填 `"org"`），故老实现「按组织解析」= 收到 `dim_key="org"` 时走 `cmx_org` 分支，行为不变。

> 契约仅此一处签名变更。因是「一芯多壳」，5 个实现同步（§11），1 个调用点同步（§10）。

---

## 7. PgSubflowRouter：三层解析泛化到任意字典

`cmx-flow-store-pg/src/subflow_router.rs:59` 的三层解析（精确/继承/兜底）**结构不变**，只把「维度物理事实」参数化：

### 7.1 维度 → 物理事实的解析

新增一个内部映射：`dim_key` → `DimSpec{ table, id_col, path_col, delim, archived_filter }`：

| dim_key | table | id_col | path_col | delim | 说明 |
|---------|-------|--------|----------|-------|------|
| `org`（内建） | `cmx_org` | `id` | `path` | `/` | 保住今天行为 |
| 任意 `<dictCode>` | `cf_*`（查 dictMeta.tableName） | `id`/`code`（pk） | `full_path` | `.` | 自分级字典 |

`org` 硬编码内建；其余走 dictCode → dictMeta（经 `/api/dct/meta` 或直读定义 JSON / 直连 `cmx-dct-store-pg::resolve_dict`，见 §8）拿 `tableName`/`parentField`/`selfHierarchy`/pk。

### 7.2 三层 SQL（参数化后）

```sql
-- 绑定表加 dim_key 列后，三层都按 (called_key, dim_key, dim_value) 查：
-- 1) 精确
SELECT target_definition_key FROM cmx_flow_subflow_binding
 WHERE called_key=$k AND dim_key=$d AND dim_value=$v AND enabled;

-- 2) 继承（仅当该维度字典 selfHierarchy=true；平级字典跳过此层）
--    把 cmx_org/path 换成 <DimSpec.table>/<DimSpec.path_col>，并追加分隔符修边界 bug
SELECT b.target_definition_key
  FROM cmx_flow_subflow_binding b
  JOIN <table> anc  ON anc.<id_col> = b.dim_value
  JOIN <table> self ON self.<id_col> = $v
 WHERE b.called_key=$k AND b.dim_key=$d AND b.enabled
   AND self.<path_col> LIKE anc.<path_col> || '<delim>' || '%'   -- 追加 delim 修 /a 错配 /ab
 ORDER BY length(anc.<path_col>) DESC LIMIT 1;

-- 3) 兜底
SELECT target_definition_key FROM cmx_flow_subflow_binding
 WHERE called_key=$k AND dim_key=$d AND dim_value IS NULL AND enabled;
```

**平级字典**（`selfHierarchy=false`，如 `cf_currency`）：**天然跳过第 2 层**（无路径列可继承），只走精确 + 兜底。这正确——平级字典无「上级」概念。

> ⚠️ 现有 `subflow_router.rs` 用 `format!` 拼 SQL + `esc()` 转义。泛化后 `table`/列名来自 dictMeta（受信、非用户自由输入），仍应**白名单校验**（表名匹配 `^cf_[a-z0-9_]+$` 或 `cmx_org`）防注入面扩大。

---

## 8. 绑定表与维度字典读取

### 8.1 绑定表 `cmx_flow_subflow_binding` 加维度列

`ddl.rs:171` 建表 + 幂等 ALTER：

```sql
ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_key   VARCHAR(64)  NOT NULL DEFAULT 'org';
ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_value VARCHAR(128);  -- 原 org_id 的泛化
-- 数据迁移：老行 org_id → dim_value，dim_key 全填 'org'（一条 UPDATE）
UPDATE cmx_flow_subflow_binding SET dim_value = org_id WHERE dim_value IS NULL;
```

**两种落地取舍**：
- **(A) 加列并存**（推荐，无损）：保留 `org_id` 列（兼容/回滚），新增 `dim_key`+`dim_value`；`org_id` 作为 `dim_key='org'` 时 `dim_value` 的镜像。
- **(B) 语义换列**：`org_id` 直接改名 `dim_value` + 加 `dim_key`。更干净但需迁移器同步改。首期建议 (A)。

绑定去重键从 `(called_key, org_id)` → `(called_key, dim_key, dim_value)`；`binding_id` FNV-1a 派生公式（`handlers.rs:1935`）从 `"{key}|{org|__default__}"` → `"{key}|{dim_key}|{dim_value|__default__}"`。

### 8.2 维度字典读取：三种接法（选一，取决于部署形态）

flow-engine 需要**运行期**（`PgSubflowRouter` 继承层查 dictMeta + 路径列）与**设计态**（列维度字典的条目供绑定选择）读取字典。三种接法对应 flow 的三种部署：

| 接法 | 适用 | 做法 | 取舍 |
|------|------|------|------|
| **① 直连同库 SQL** | flow 与 DCT 同库（今天 demo：都在 fico/cmx 库） | `PgSubflowRouter` 直接 `SELECT ... FROM cf_*`，dictMeta 直读定义 JSON | 最快、无 HTTP；耦合库 |
| **② 链 `cmx-dct-store-pg`** | flow 内嵌壳（cmx-container 内 cmx-rule-api 同款） | 依赖 `cmx-dct-store-pg`，调 `resolve_dict`/`DctHierService::expand`（已实现 `HierService`） | 进程内、类型安全、复用继承 CTE；跨 workspace path 依赖 |
| **③ HTTP `/api/dct`** | flow 独立微服务（:8091，S6 平台对接） | 新增 `HttpDimensionResolver`：`GET /api/dct/meta?dict=` + `POST /api/dct/data/search?dict=&{parentId}` | 完全解耦、对齐 `HttpSubflowRouter` §11③；**无 ancestor/LIKE 端点**，「继承」需 (a)加端点 或 (b)客户端逐级 walk parent_id |

**建议**：维持 M5.2 的「可注入」纪律——定义一个 `DimensionCatalog` trait（列字典条目/取 dictMeta/判自分级），Pg 实现走①/②，独立微服务走③。引擎不认它，只有 `SubflowRouter` 实现认。

> **③ 的继承缺口**：`/api/dct` 现无「取某条目全部祖先」或「按 full_path 前缀查」的端点。要在独立微服务里保「沿树继承」，最干净是给 `cmx-dct-api` 补一个 `GET /api/dct/ancestors?dict=&id=` 或让 search 支持 `pathPrefix` 过滤（后端一条 `full_path LIKE` ）。这是 DCT 侧的小增量，非 flow 侧。

---

## 9. BPMN / 编译器变更

### 9.1 自定义属性 `cmx:dimKey`

`cmx-flow-bpmn/src/compiler.rs:506` `parse_call_activity` 加一行（与 `calledKey` 同款 `local_attr`）：

```rust
let dim_key = local_attr(node, "dimKey");   // cmx:dimKey；None → 默认 org
```

callActivity 的完整自定义属性族变成：`cmx:calledKey`（逻辑名）、`cmx:dimKey`（路由维度，**新增**）、`cmx:inVars`/`cmx:outVars`（变量映射简写）。

### 9.2 设计器写 `$attrs` 的坑（M5.2 已踩，复用其解法）

`cmx:dimKey` 同 `cmx:calledKey`——bpmn-js 无注册 moddle 扩展，落在 `businessObject.$attrs['cmx:dimKey']`，**读写必须走 `$attrs`**（`getDimKey`/`setDimKey` helper，`updateProperties({'cmx:dimKey': v})`），不能用 `.get()/.set()`。这与 M5.2 的 `getCalledKey/setCalledKey`（`design-workbench.js:1696`）同款，直接照抄。

---

## 10. 引擎 launch_one_subflow 变更

`cmx-flow-engine/src/engine.rs:572-616` 的 `launch_one_subflow` 是**唯一** `resolve` 调用点。改动极小：

```rust
// 【改】维度 key：挂载点声明的 dim_key，缺省 "org"
let dim_key = ca.dim_key.as_deref().filter(|s| !s.is_empty()).unwrap_or("org");

// 【改】维度值：从实例的维度上下文取；"org" 维度兼容回退 org_id 标量列
let dim_value = parent_snap.instance.dimensions.get(dim_key).cloned()
    .or_else(|| if dim_key == "org" { parent_snap.instance.org_id.clone() } else { None });

let sub_key = match &ca.called_key {
    Some(key) if !key.is_empty() => match &self.subflow_router {
        // 【改】三参调用
        Some(router) => match router.resolve(key, dim_key, dim_value.as_deref()).await {
            Ok(k) => k,
            Err(e) => return self.mark_subflow_incident(&inst_id, node_bpmn, &e.to_string()).await,
        },
        None => return self.mark_subflow_incident(...).await,   // 不变
    },
    _ => ca.called_element.clone(),   // M5.1 写死路径，完全不动
};
```

- **`node_bpmn` 已在手**（`engine.rs:576` 参数）→ dim_key 从 `ca`（该节点的 CallActivity）取，**天然按挂载点**，无需新增管道。这就是把「已在调用点却被丢弃的挂载点身份」利用起来。
- 子实例继承：`start_process_inner` 传的 `org` 改传整个 `dimensions`（默认整体继承，语义不变）。
- 失败仍转 Incident（`mark_subflow_incident`），不抛错、不留僵尸——M5.2 语义不动。

---

## 11. 五个 SubflowRouter 实现的同步

签名 `resolve(called_key, org_id)` → `resolve(called_key, dim_key, dim_value)` 触及**恰好 5 个实现 + 1 个调用点**（agent 已测绘）：

| # | 实现 | 位置 | 改动 |
|---|------|------|------|
| 1 | 契约 trait | `cmx-flow-model/src/subflow.rs:46` | 签名 + RouteError 泛化 |
| 2 | `PgSubflowRouter` | `cmx-flow-store-pg/src/subflow_router.rs:59` | 三层 SQL 参数化（§7）+ DimSpec 映射 |
| 3 | `HttpSubflowRouter` | `cmx-flow-adapters/src/subflow.rs:44` | 请求体 `{calledKey, **dimKey**, **dimValue**}`；对端 `/subflow/resolve` 契约同步 |
| 4 | `MockSubflowRouter` | `cmx-flow-adapters/src/mock.rs:79` | 固定映射键从 `key@org` → `key@dim_key@dim_value` |
| 5 | 测试 `FakeRouter` | `cmx-flow-tests/tests/m5_2*/m5_3*.rs` | 同 Mock |
| — | 调用点 | `cmx-flow-engine/src/engine.rs:599` | §10 |

**兼容垫片**（可选）：给 trait 加一个 `resolve_org(called_key, org)` 默认方法转调 `resolve(called_key, "org", org)`，减少一次性改测试的量。不推荐长留，过渡期用。

---

## 12. App 端点与设计器变更

### 12.1 端点泛化（`cmx-flow-app`）

| 今天 | 泛化后 |
|------|--------|
| `GET /flow/orgs` → `cmx_org` 扁平树 | `GET /flow/dimensions` 列**可选维度**（内建 org + 已配置的 dictCode 清单）；`GET /flow/dimension/{dimKey}/entries` 列某维度字典的条目（自分级带 parentId 懒下钻，走 DimensionCatalog） |
| `GET /flow/subflow-bindings/{key}` | 同名，返回项加 `dimKey`/`dimValue`/`dimValueName`（原 `orgId`/`orgName` 泛化） |
| `POST /flow/subflow-bindings` body `{calledKey, orgId?, targetKey, ...}` | `{calledKey, **dimKey**, **dimValue?**, targetKey, ...}` |
| `DELETE /flow/subflow-bindings/id/{id}` | 不变 |

`binding_view`（`handlers.rs:1882`）的 `orgId/orgName/isDefault` → `dimKey/dimValue/dimValueName/isDefault`。

### 12.2 设计器（`design-workbench.js`，两份镜像同步）

callActivity 属性面板（`design-workbench.js:1004-1036`）「按组织路由」模式卡升级为「**按维度路由**」：

1. **维度选择器**（新增）：模式=维度路由时，先出一个「路由维度」下拉——选项 = `GET /flow/dimensions`（内建「组织机构(org)」置顶 + 各 dictCode 字典名）。选中即写 `cmx:dimKey`。
2. **逻辑 key 输入**：`cmx:calledKey`（不变）。
3. **配置绑定按钮**：弹绑定对话框，其中的「组织下拉」泛化为「**维度条目选择器**」——数据源从写死 `/flow/orgs` 改成 `/flow/dimension/{当前dimKey}/entries`：
   - 自分级字典（org/gl_account…）：仍用「扁平 `<select>` 按 path/full_path 深度缩进」（`orgOptionsHtml` 泛化成 `dimEntryOptionsHtml`，缩进按 `full_path` 段数）；
   - 平级字典（currency…）：直接平铺条目，无缩进；
   - 首项恒为「— 默认（兜底）绑定 —」（`dimValue=null`）。
4. 目标子流程下拉不变（复用 `state.definitions`）。

> 维度选择器缺省选「组织机构」→ 不改任何东西的老流程，面板呈现与今天一致（向后兼容的 UI 体现）。

---

## 13. 数据模型与存储变更汇总

| 层 | 变更 | 破坏性 |
|----|------|--------|
| `CallActivity` IR | 加 `dim_key: Option<String>` | 非破坏（skip_serializing_if） |
| `ProcessInstance` | 加 `dimensions: Map<String,String>`；`org_id` 保留为投影 | 非破坏 |
| `RouteError::NoBinding` | `org` → `dim_key`+`dim_value` | 内部类型 |
| `SubflowRouter::resolve` | 两参 → 三参 | 5 实现 + 1 调用点一次性同步 |
| BPMN | callActivity 加 `cmx:dimKey` 解析 | 非破坏（缺省 org） |
| `cmx_flow_instance` | 加列 `dimensions jsonb`（幂等 ALTER） | 非破坏 |
| `cmx_flow_subflow_binding` | 加列 `dim_key`+`dim_value`；`org_id` 保留镜像；一条 UPDATE 迁老数据 | 非破坏（默认 'org'） |
| App 端点 | `/flow/orgs`→`/flow/dimensions`+`/dimension/{k}/entries`；绑定体加 dimKey/dimValue | 可保 `/flow/orgs` 别名过渡 |
| 设计器 | 维度选择器 + 维度条目选择器泛化 | 两份镜像同步 |
| DCT 侧（可选，仅③独立部署） | `cmx-dct-api` 补 ancestor/pathPrefix 端点 | 新增，不改老端点 |

**硬约束沿用**：`cmx_flow_` 前缀、禁外键用索引、DDL 幂等、绑定表在 IAM_DB_ID（因 org 维度 JOIN `cmx_org`）。

---

## 14. 分阶段路线图

| 阶段 | 交付 | 验收 | 依赖 |
|------|------|------|------|
| **RD0 · 契约泛化** | `SubflowRouter` 三参 + `RouteError` 泛化 + 5 实现同步 + `CallActivity.dim_key` + BPMN 解析 | 全 flow 测试零回归（`dim_key` 缺省 org，行为不变）；`resolve("k","org",v)` == 今天 | 无 |
| **RD1 · Pg 维度解析** | `PgSubflowRouter` DimSpec 映射 + 三层 SQL 参数化 + 平级跳继承 + 边界 bug 修 + 白名单校验 | 内存 FakeRouter：org 维度/新字典维度/平级字典/继承/兜底全绿；PG ignore：cf_* 自分级继承实测 | RD0 |
| **RD2 · 绑定表 + 端点** | 绑定表加 dim_key/dim_value + 迁移；`DimensionCatalog` trait + Pg 实现；端点泛化 | curl：配 org 绑定 + 配某 cf_* 字典绑定 → 各自解析；老 org 绑定迁移后仍解析 | RD1 |
| **RD3 · 实例维度上下文** | `cmx_flow_instance.dimensions jsonb` + `StartReq.dimensions` + 继承 + org 兼容投影 | 发起传多维度 → 同实例挂载 A 按 org、挂载 B 按 legal_entity 解析出不同子流程 | RD2 |
| **RD4 · 设计器** | 维度选择器 + 维度条目选择器泛化（自分级缩进/平级平铺）+ `cmx:dimKey` 的 `$attrs` 读写 | CDP：选维度=组织→默认行为；选维度=某字典→列该字典条目配绑定落库；两份镜像同步 | RD2 |
| **RD5 · 独立微服务维度读取（可选）** | `HttpDimensionResolver` + `cmx-dct-api` 补 ancestor/pathPrefix 端点 | 独立 :8091 flow 经 HTTP 读 dct 维度 + 继承解析 | RD1、平台对接 |

**关键顺序**：RD0（契约）是全前置，一次性把签名/五壳/BPMN 打通，`dim_key` 缺省 org 保零回归；RD1–RD4 是「把 org 特例展开成任意字典」的增量。RD3（实例多维度上下文）是解锁「同实例不同挂载走不同维度**取值**」的关键——若首期只需「不同主流程/挂载点用不同**字典**」，RD3 可延后（见 §4 可选简化）。

---

## 15. 风险与取舍

| 项 | 取舍 / 缓解 |
|----|------------|
| **契约签名变更波及面** | 恰 5 实现 + 1 调用点（agent 测绘确认）；一次性同步，可加 `resolve_org` 默认垫片过渡；引擎 crate 零新依赖 |
| **路径分隔符/段来源因字典而异** | 维度配置携带 `path_col`+`delim`+`id/code 段来源`（§3/§7）；org=`/`+id 段、cf_*=`.`+code 段，不硬编码 `.path` |
| **现有 LIKE 边界 bug（`/a` 错配 `/ab`）** | 泛化时追加分隔符 `LIKE anc||delim||'%'` 一并修（§1/§7）；org 侧也顺带修正 |
| **平级字典无继承** | 明确语义：平级字典**只精确+兜底**，无「上级」概念，天然跳过继承层；不是缺陷 |
| **独立微服务无 ancestor 端点** | ③ HTTP 接法的继承需 DCT 侧补 `pathPrefix`/`ancestors` 端点（小增量），或客户端逐级 walk parent_id；①②同库/内嵌无此问题 |
| **SQL 注入面（表名/列名来自 dictMeta）** | dictMeta 受信非用户自由输入，仍白名单校验表名 `^cf_[a-z0-9_]+$`\|`cmx_org`；值参数仍 esc/绑参 |
| **实例 dimensions 膨胀** | jsonb 动态、对齐 variables 列做法；子实例整体继承；一般 ≤ 3-5 个维度 |
| **向后兼容** | `dim_key` 缺省 org、`dimensions` 缺省回退 `org_id`、绑定 `dim_key` 默认 'org' + 一条 UPDATE 迁移、`/flow/orgs` 保别名——老定义/老实例/老绑定/老 UI 全不破 |
| **维度字典被删/改** | 运行期 dictCode 解析不到 → NoBinding → Incident（可 retry），不崩；对齐 M5.2 无解语义 |

**坚决不做**：
- 不让引擎认识字典/组织/DB（维度解析永远在可注入实现里，保中立可测可 wasm）。
- 不引第二事实源（维度声明随 BPMN 走、绑定在 binding 表、字典在 DCT，各司其职）。
- 不破 M5.1 写死 `calledElement` 路径、不破 M5.2 org 路由的现有行为。

---

## 16. 附录：三个完整示例

### 示例 A · 不同主流程用不同维度字典

**报销主流程**（按组织，向后兼容，`dimKey` 省略=org）：
```xml
<callActivity id="call_fin" name="财务复核" cmx:calledKey="fin_review"/>
```
**风控主流程**（按风险等级字典 `risk_level`）：
```xml
<callActivity id="call_risk" name="风险审查" cmx:calledKey="risk_review" cmx:dimKey="risk_level"/>
```
发起：`POST /instances {definitionKey:"risk_flow", dimensions:{risk_level:"R3"}}`
→ `launch_one_subflow` 读 `dim_key="risk_level"`、`dim_value="R3"`
→ `resolve("risk_review","risk_level","R3")` → 查 `cf_risk_level` 字典绑定 → 高风险专用子流程。

### 示例 B · 同一主流程不同挂载点用不同维度

**采购主流程**，两个挂载点各按不同字典：
```xml
<callActivity id="call_a" name="预算审批" cmx:calledKey="budget_appr" cmx:dimKey="org"/>
<callActivity id="call_b" name="品类合规" cmx:calledKey="cat_compliance" cmx:dimKey="purchase_category"/>
```
发起：`POST /instances {definitionKey:"purchase", dimensions:{org:"df_bj", purchase_category:"CAT_IT"}}`
→ 挂载 A 读 `dimensions["org"]="df_bj"` → 按组织树解析（北京分公司的预算子流程）；
→ 挂载 B 读 `dimensions["purchase_category"]="CAT_IT"` → 按采购品类字典解析（IT 品类合规子流程）。
**这是今天做不到的**（M5.3 memory line17 记的缺口），RD3 解锁。

### 示例 C · 自分级字典的继承（法人公司 `legal_entity`）

法人公司是自分级字典（`cf_legal_entity`，`full_path` 点分 code 段：`GROUP.CN.CN_EAST`）。
绑定：只给 `GROUP.CN`（中国区，dim_value=`LE_CN`）配了 `fin_review → fin_review_cn`。
发起实例 `dimensions:{legal_entity:"LE_CN_EAST"}`（华东，是中国区的下级）：
→ 精确查 `LE_CN_EAST` 无绑定
→ **继承**：`JOIN cf_legal_entity`，`self.full_path='GROUP.CN.CN_EAST' LIKE anc.full_path||'.%'` → 命中祖先 `GROUP.CN`（`LE_CN`）的绑定 → `fin_review_cn`
→ 与组织路由「北京无绑定→继承总部」**完全同构**，只是把 `cmx_org.path`(`/`) 换成 `cf_legal_entity.full_path`(`.`)。

---

*本方案基于 cmx-flowengine 现有架构（SubflowRouter 第 4 扩展点、launch_one_subflow 唯一调用点、cmx_flow_subflow_binding 绑定表、CallActivity IR、M5.3 (parent_token_id,parent_node_bpmn_id) 多挂载）与 cmx-container DCT 字典系统（dictCode 身份、cf_* 物理表、自分级 parent_id+full_path、/api/dct 服务）设计，全部接缝已对源码测绘定位。落地时以源码为准，dim_key 缺省 org 保向后兼容。*
