# ⑤ 详细实现设计：流程变量设计态声明（名称/类型/说明/结构）

> 状态：**全部落地实现（2026-08-14），P1–P4 + 前端 P2/P3 全绿，真机（后端 curl + 设计器 CDP）验证通过**。关联 [`enhancement-proposal-designer-mi-reject-withdraw.md`](enhancement-proposal-designer-mi-reject-withdraw.md)、[②多实例](enhancement-detail-02-dynamic-multi-instance-assignee.md)。
> 落地摘要：P1 模型（`VarDecl` 加 description/fields/item 递归结构 + flatten_paths + 递归 validate_shape + camelCase JSON 契约）；编译器解析 `<cmx:varSchema>`（去 ProcessDefinition 的 Eq 派生以容 Value）；端点 `GET /definitions/{key}/variables` + validate；P4 发起态 materialize_defaults + validate_values（`cmx:varValidation` strict/lenient/off，新增 Error::VarValidation）；P2 设计器全屏变量编辑器（类型下拉 + 对象/数组递归子字段 + 必填/说明/枚举 + 策略；随 XML 走 openDiagram 读/getXml 注回绕 moddle 丢弃）；P3 四处下拉从 state.varPaths（条件构造器/多实例 collection/办理人表达式/表单）。测试：model 50 + bpmn 22 + 引擎 106（新增 var_schema_start 6）零回归、新代码 0 clippy；设计器 CDP（递归编辑+保存+摘要）+ 后端往返摊平全绿；前端两份镜像同步。
> 目标：流程**定义态**就声明好变量的名称、类型、说明、默认值、（对象/数组的）字段结构；设计器处处（条件构造器、办理人表达式、多实例 collection、表单字段）能下拉这些变量与字段，发起/办结时按声明校验。解决「只有发起时才知道有哪些变量、后期使用不友好」。
> 日期：2026-08-14

## 1. 问题与现状

### 痛点（用户原话）
- 任务节点/边的定义里要用变量，但**没地方看有哪些变量、叫什么、什么类型、什么含义**。
- 变量目前只在发起时随 `POST /instances` 传入 → 设计态两眼一抹黑，条件表达式/办理人表达式全靠手打字符串、拼错难查。
- 变量可能是**数组或对象**，需要能声明其**字段结构**（否则 `order.customer.level`、多实例元素的 `product.ownerUser` 无法在设计器里被提示/下拉）。

### 现状核对（对着源码）

| 能力 | 现状 |
|------|------|
| 变量运行态容器 `Variables` | ✅ `BTreeMap<String, JSON>`，落 jsonb，动态无 schema |
| 设计态声明模型 `VarSchema`/`VarDecl` | ✅ **已完整实现 + 8 测试**（类型/默认/必填/枚举/`validate_shape`/`materialize_defaults`/`validate_values`），`crates/cmx-flow-model/src/var_schema.rs` |
| `VarType` 覆盖对象/数组 | ⚠ 有 `Object`/`Array` 枚举值，但**只有顶层类型标记，无字段结构**（不知 `order` 有哪些字段、数组元素长什么样） |
| VarSchema 接线 | ❌ **零消费者**——只有 `lib.rs` re-export；不落库、设计器无面板、发起不校验 |
| 定义持久化 | 存 BPMN XML（`draft_xml`/`bpmn_xml`），无独立 schema 列；但 BPMN 有 `extensionElements` 可挂 |
| 设计器变量下拉 | ❌ 代码注释明言「变量声明工作台是 P1 前端，未落地」，条件构造器用 `input+datalist` 手填占位 |

**结论**：模型骨架已在，缺**两块**——(A) 让 `VarType::Object/Array` 能带字段结构；(B) 全链路接线（存 XML → 设计器面板 → 处处下拉 → 发起校验）。

## 2. 设计总览

```
设计态：设计器「变量」面板 → 编辑 VarSchema（含对象/数组字段结构）
            │ 写进 BPMN <extensionElements><cmx:varSchema> （随定义/版本走，不建新表）
            ▼
编译：cmx-flow-bpmn 解析 <cmx:varSchema> → ProcessDefinition.var_schema
            │
消费（设计器联动）：条件构造器 / 办理人表达式 / 多实例 collection / 表单字段
            │ 都从 GET /definitions/{key}/variables 拉「变量+字段路径」下拉
            ▼
运行态：start_process 可选 materialize_defaults(注默认) + validate_values(软校验)
            引擎运行仍用动态 Variables，schema 不改变运行语义（向后兼容）
```

三条铁律不破：
- **引擎运行不依赖 schema**：不声明也能跑（今天行为），声明了只增强（下拉、默认、校验）。
- **schema 随定义走**：写进 BPMN XML，天然随版本/发布/装载，不建新表、不引第二事实源。
- **对象/数组字段结构是核心**：这是「变量可能是数组或对象」诉求的关键，必须一等公民。

## 3. 关键设计：对象/数组的字段结构（VarType 增强）

现状 `VarDecl` 对 `Object`/`Array` 只有顶层类型。补一个**可选的 `fields`（对象字段）/`item`（数组元素）**递归结构，让 schema 能描述任意深度：

### 增强后的 `VarDecl`（新增两字段，全部可选，向后兼容）

```rust
pub struct VarDecl {
    pub name: String,
    pub var_type: VarType,            // 已有：STRING/NUMBER/BOOLEAN/DATE/ENUM/OBJECT/ARRAY
    pub label: Option<String>,        // 已有：展示标签
    pub description: Option<String>,  // 【新增】说明（用户点名要的「说明」；设计器 tooltip）
    pub default: Option<Value>,       // 已有
    pub source: VarSource,            // 已有
    pub scope: VarScope,              // 已有
    pub required: bool,               // 已有
    pub enum_options: Vec<String>,    // 已有
    // 【新增】对象字段结构（var_type==Object 时有意义）：递归 VarDecl 描述每个字段。
    pub fields: Vec<VarDecl>,
    // 【新增】数组元素结构（var_type==Array 时有意义）：描述数组每个元素的形状。
    //   元素是对象 → item.var_type==Object + item.fields；元素是标量 → item.var_type==Number 等。
    pub item: Option<Box<VarDecl>>,
}
```

> 设计取舍：复用 `VarDecl` 自身递归（而非另造 FieldDecl），字段也能带 label/description/required/enum——一套控件复用。`item` 用 `Box` 破递归无限尺寸。`fields`/`item` 为空即退化为今天的顶层标记（老 schema JSON 原样可读，`#[serde(default)]`）。

### 声明示例（对象 + 对象数组）

```jsonc
[
  { "name": "amount", "type": "NUMBER", "label": "申请金额", "description": "本次申请的授信额度", "required": true },
  { "name": "urgent", "type": "BOOLEAN", "label": "加急", "default": false },
  { "name": "region", "type": "ENUM", "label": "区域", "enumOptions": ["north", "south"], "default": "north" },

  { "name": "order", "type": "OBJECT", "label": "订单", "description": "关联的销售订单",
    "fields": [
      { "name": "total", "type": "NUMBER", "label": "订单总额" },
      { "name": "customer", "type": "OBJECT", "label": "客户",
        "fields": [
          { "name": "level", "type": "ENUM", "label": "客户等级", "enumOptions": ["VIP", "normal"] }
        ] }
    ] },

  { "name": "products", "type": "ARRAY", "label": "产品明细", "description": "动态明细，每项派一个审核待办（②）",
    "item": {
      "name": "product", "type": "OBJECT",
      "fields": [
        { "name": "sku", "type": "STRING", "label": "编码" },
        { "name": "ownerUser", "type": "STRING", "label": "负责人", "description": "该产品的审核人 id" },
        { "name": "ownerRole", "type": "STRING", "label": "负责角色" }
      ] }
  }
]
```

### 由结构派生「可选变量路径」（设计器下拉的来源）

一个纯函数把 schema 摊平成**点路径列表**，供设计器所有变量下拉直接用：

```
flatten_paths(schema) →
  amount            (NUMBER, 申请金额)
  urgent            (BOOLEAN, 加急)
  region            (ENUM, 区域)
  order             (OBJECT, 订单)
  order.total       (NUMBER, 订单总额)
  order.customer    (OBJECT, 客户)
  order.customer.level (ENUM, 客户等级)
  products          (ARRAY, 产品明细)
  products[].sku    (STRING, 编码)          ← 数组元素字段用 [] 记号
  products[].ownerUser (STRING, 负责人)
  products[].ownerRole (STRING, 负责角色)
```

- 条件构造器：变量下拉 = 这些路径（选 `order.customer.level` 自动生成 `${order.customer.level == ...}`）。
- 多实例 collection 下拉：只列 `type==ARRAY` 的路径（`products`）。
- 多实例逐元素派人（②）：选了 collection=`products` → elementVariable 默认 `product` → 办理人下拉列 `product.ownerUser`/`product.ownerRole`（即 `products[]` 的字段）。
- 表单字段映射：列全部叶子路径。

## 4. 变量声明写进 BPMN（存哪）

写进 `<process>` 的 `<extensionElements>`，一个 `<cmx:varSchema>` 承载 JSON（与 candidates/inVars 的 `$attrs` 纪律一致——避免注册 bpmn-js moddle 扩展）：

```xml
<bpmn:process id="credit_approval" name="信用审批" isExecutable="true">
  <bpmn:extensionElements>
    <cmx:varSchema>[
      {"name":"amount","type":"NUMBER","label":"申请金额","required":true},
      {"name":"products","type":"ARRAY","label":"产品明细",
       "item":{"name":"product","type":"OBJECT","fields":[
         {"name":"sku","type":"STRING","label":"编码"},
         {"name":"ownerUser","type":"STRING","label":"负责人"}]}}
    ]</cmx:varSchema>
  </bpmn:extensionElements>
  <bpmn:startEvent id="start"/>
  ...
</bpmn:process>
```

为什么写进 XML 而不建新表：
- 定义持久化本就是「一份 XML = 一个版本」，schema 随 XML 天然获得版本化/发布/装载/回滚，**零第二事实源**。
- 与现有 `cmx:startFormKey`（process 级）、`cmx:candidates`（节点级）同一处理范式。
- 独立部署 / headless 三方拿到 XML 即拿到 schema，无需额外接口。

> 载体是 JSON 文本（非展开成众多 XML 子元素）：对象/数组的递归结构用 JSON 表达最自然，且编译器一次 `serde_json::from_str` 到位。缺点是 XML 里不可读——可接受（面向机器 + 设计器 UI，不手写）。

## 5. 编译器：解析进 IR

`ProcessDefinition` 加一个字段（与 `start_form_key`/`withdraw_policy` 并列）：

```rust
pub struct ProcessDefinition {
    ...
    pub var_schema: Option<VarSchema>,   // 【新增】设计态变量声明（None = 未声明，向后兼容）
}
```

编译器 `compile()`：读 `<process>/<extensionElements>/<cmx:varSchema>` 文本 → `serde_json::from_str::<VarSchema>()`。

- 解析失败（坏 JSON）→ 编译报错（挡回非法定义，与拓扑校验同级）。
- 可选：编译时跑 `var_schema.validate_shape()`，shape 违规也挡回（重复名/枚举无候选/默认值类型不符）。
- 无 `<cmx:varSchema>` → `None`，零回归。

## 6. 端点

### 定义态

| 端点 | 作用 |
|------|------|
| `GET /definitions/{key}/variables` | 取该定义的变量声明 + **摊平路径列表**（设计器下拉的唯一数据源）。`?version=N` 看指定版本 |
| `POST /definitions/{key}/variables/validate` | 校验一份 VarSchema 的 shape（设计器保存前）→ `{valid, violations:[{var,code,message}]}` |

> 声明的写入不单独开端点——它随 `POST /definitions/draft`（存草稿）一起，因为 schema 就在 BPMN XML 里，存草稿即存 schema。

响应 `GET /definitions/{key}/variables`：

```json
{ "code":0, "data": {
    "key": "credit_approval",
    "schema": [ /* 原始 VarDecl 树 */ ],
    "paths": [
      { "path":"amount", "type":"NUMBER", "label":"申请金额", "description":"...", "required":true },
      { "path":"order.customer.level", "type":"ENUM", "label":"客户等级", "enumOptions":["VIP","normal"] },
      { "path":"products", "type":"ARRAY", "label":"产品明细", "isCollection":true },
      { "path":"products[].ownerUser", "type":"STRING", "label":"负责人" }
    ]
} }
```

### 运行态（发起校验，可选开关）

发起时对 `variables` 跑 schema 校验，把 `VarViolation` 转成结构化错误：

- `POST /instances` 增可选行为：若定义有 schema →
  1. `materialize_defaults`（注入声明的默认值）；
  2. `validate_values`（必填/类型/枚举软校验）。
- 严格度可配（同 ④ 的 `cmx:withdrawPolicy` 思路）：process 级 `cmx:varValidation="strict|lenient|off"`。
  - `strict`：违规 → 拒绝发起（返回 violations）。
  - `lenient`（默认）：违规仅 warn，照常发起（当前动态行为 + 提示）。
  - `off`：完全不校验。
- 这样「声明」既能是纯提示（lenient），也能是硬约束（strict），按流程自选。

## 7. 设计器 UI

### 7.1 新增「变量」面板（流程级属性）

选中画布空白（`bpmn:Process`）时，property 区除现有 DAM 归属外，加「流程变量」分组：

```
┌ 流程变量 ──────────────────────────────┐
│ [+ 新增变量]                             │
│ ┌────────────────────────────────────┐ │
│ │ amount    数值   申请金额   必填 ✎ 🗑 │ │
│ │ urgent    布尔   加急               │ │
│ │ region    枚举   区域   [north,south]│ │
│ │ order     对象   订单        ▸ 展开   │ │  ← 展开编辑子字段
│ │ products  数组   产品明细    ▸ 展开   │ │  ← 展开编辑元素结构
│ └────────────────────────────────────┘ │
└─────────────────────────────────────────┘
```

单条变量编辑（对象/数组可递归展开子字段）：

```
名称  [products____]        类型 [数组 ▾]
标签  [产品明细____]        必填 ☐
说明  [动态明细，每项派一个审核待办]
─ 数组元素结构（类型=数组）─
  元素类型 [对象 ▾]
  字段：
    [sku_______] [字符串▾] [编码_____] ✎🗑
    [ownerUser_] [字符串▾] [负责人___] ✎🗑
    [+ 字段]
```

写回 = 序列化整个 VarSchema 树成 JSON → 存进 `<cmx:varSchema>`（同 candidates 的 `$attrs` 写回纪律）。保存前调 `/variables/validate`，违规红字定位到具体变量。

### 7.2 处处消费变量下拉（把声明用起来——最大价值）

改造现有四处「手打字符串」为「从声明下拉 + 仍可手填」：

| 位置 | 现状 | 改造 |
|------|------|------|
| **条件构造器**（边 `conditionExpression`） | `input+datalist` 占位提示 | 变量下拉 = `flatten_paths`（选 `order.customer.level` 自动填表达式左侧）；仍保留手填逃生舱 |
| **多实例 collection**（②） | 手打集合变量名 | 下拉只列 `type==ARRAY` 的路径 |
| **多实例逐元素派人**（②，我已补引导） | 文本提示 `${item.field}` | 选了 collection → 元素字段下拉（`products[]` 的字段），点选生成 `${product.ownerUser}` |
| **表单字段映射** | 手填字段名 | 下拉叶子路径 |

数据源统一：设计器载入定义时拉一次 `GET /definitions/{key}/variables`，缓存 `state.varPaths`，四处组件共用。

## 8. 运行态整合（可选、不破坏）

`start_process` 增两步（仅当定义有 schema）：

```
let vars = if let Some(schema) = &def.var_schema {
    let filled = schema.materialize_defaults(&input_vars);   // 注默认
    // 按 cmx:varValidation 策略决定 validate_values 的违规如何处理
    filled
} else { input_vars };
```

- 引擎核 `run_to_wait` 等**完全不看 schema**——变量仍是 JSON，语义零变化。
- 校验只在**边界**（发起端点）做，不侵入执行内核。

## 9. 落地清单与工作量

| 层 | 文件 | 改动 |
|----|------|------|
| 模型 | `cmx-flow-model/src/var_schema.rs` | `VarDecl` 加 `description`/`fields`/`item`；新增 `flatten_paths()` 摊平；`validate_shape` 递归校验子字段 |
| 模型导出 | `cmx-flow-model/src/lib.rs` | 导出 `flatten_paths` + 路径 DTO |
| IR | `cmx-flow-model/src/ir.rs` | `ProcessDefinition` 加 `var_schema: Option<VarSchema>` |
| 编译器 | `cmx-flow-bpmn/src/compiler.rs` | 解析 `<cmx:varSchema>` JSON → IR；可选 shape 校验挡回 |
| 引擎 | `cmx-flow-engine/src/engine.rs` | `start_process` 可选 `materialize_defaults`+`validate_values`（按 `cmx:varValidation`） |
| 应用层 | `cmx-flow-app` | `GET /definitions/{key}/variables`、`POST /definitions/{key}/variables/validate` |
| 设计器 | `design-workbench.js`（+镜像） | 变量面板（含对象/数组递归编辑）；四处下拉改读 `varPaths` |
| 测试 | 模型单测（含 fields/item/flatten）；编译器解析测试；发起校验 E2E | |
| 文档 | `usage/` 补「变量声明」章 + 更新 05/02 | |

**工作量**：模型 1 天（递归结构 + flatten + 测试）+ 编译器 0.5 天 + 端点 0.5 天 + 引擎 0.5 天 + 设计器 **3–4 天**（变量面板递归编辑是大头 + 四处下拉改造）= **约 6 天**。
**风险**：中——
- `VarDecl` 递归（`fields`/`item`）的序列化/校验要测全（深层嵌套、数组套对象套数组）。
- 设计器变量面板的递归编辑 UI 较复杂（对象/数组展开、子字段增删）。
- 发起校验的严格度开关别误伤存量流程（默认 lenient，strict 须显式声明）。
- 编译期 shape 校验挡回要给清晰错误（否则设计者不知哪条变量非法）。

## 10. 分期建议（可增量上线）

1. **P1（模型+编译+读取，1.5 天）**：`VarDecl` 加 description/fields/item + flatten_paths + 编译器解析 + `GET /variables`。**先让声明能存能读**。
2. **P2（设计器面板，3 天）**：变量面板（含对象/数组递归编辑）+ 保存校验。**让人能可视化声明**。
3. **P3（处处下拉，1 天）**：条件构造器/多实例/表单四处改读 varPaths。**让声明用起来**（价值兑现）。
4. **P4（发起校验，0.5 天）**：materialize_defaults + validate_values + `cmx:varValidation` 开关。**让声明有约束力**。

P1+P3 就能解决用户核心痛点（设计态看得到变量名/类型/说明 + 表达式里下拉），P2 是体验大头，P4 是加分项。

## 11. 不做什么

- 不改运行态 `Variables` 仍是动态 JSON 的事实（schema 是设计态 sidecar）。
- 不做 schema 作为运行时白名单（未声明变量仍放行——向后兼容）。
- 不引入 JSON Schema / 外部校验库（自持 VarType 足够，保持零依赖、可 wasm）。
- 不做变量的跨流程共享/全局字典（每定义自持；跨流程复用是后续「变量模板」话题）。

---

_确认后可按 P1→P4 增量落地。_
