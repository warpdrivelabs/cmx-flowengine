# 10 · 流程变量声明

本篇讲**在流程定义态声明变量的元信息**（名称/类型/结构/说明），让设计器处处能下拉引用、发起时能按声明校验。核心区分：

> **声明 ≠ 赋值**。定义态声明的是变量的**契约**（叫什么、什么类型、什么结构、什么含义），**不含值**；值仍在运行态传入（发起 `POST /instances` 的 `variables`、办结、serviceTask、子流程回写）。见 [05 §5.8 变量模型](05-conditions-and-decisions.md)。

## 10.1 为什么要声明

不声明也能跑（变量是动态 JSON，向后兼容）。但声明后获得三个关键收益：

1. **设计态可见**：条件表达式、办理人、多实例集合、表单字段处处能**下拉**已声明的变量与字段，不用记忆手打、不怕拼错。
2. **对象/数组结构可表达**：`order.customer.level`、`products[].ownerUser` 这类嵌套路径能被设计器提示。
3. **发起校验**：必填/类型/枚举软校验 + 默认值注入，按策略 strict 拒绝或 lenient 提示。

## 10.2 声明什么（VarDecl）

每条变量声明的字段（JSON，camelCase）：

| 字段 | 说明 |
|------|------|
| `name` | 变量名（英文，表达式里的标识符） |
| `type` | `STRING`\|`NUMBER`\|`BOOLEAN`\|`DATE`\|`ENUM`\|`OBJECT`\|`ARRAY` |
| `label` | 展示标签（中文名，缺省回退 name） |
| `description` | 说明（业务含义；设计器 tooltip） |
| `required` | 是否必填（软校验） |
| `default` | 默认值（发起缺省时注入；可选） |
| `enumOptions` | 枚举候选（仅 `ENUM`） |
| `fields` | **对象字段结构**（仅 `OBJECT`，递归 VarDecl 数组） |
| `item` | **数组元素结构**（仅 `ARRAY`，递归单个 VarDecl；元素是对象则 `item.fields`） |

## 10.3 对象与数组怎么声明（关键）

`fields`（对象）和 `item`（数组元素）递归复用 VarDecl，可描述任意深度：

```jsonc
[
  { "name": "amount", "type": "NUMBER", "label": "申请金额", "description": "授信额度", "required": true },
  { "name": "region", "type": "ENUM", "label": "区域", "enumOptions": ["north", "south"], "default": "north" },

  { "name": "order", "type": "OBJECT", "label": "订单",
    "fields": [
      { "name": "total", "type": "NUMBER", "label": "订单总额" },
      { "name": "customer", "type": "OBJECT", "label": "客户",
        "fields": [ { "name": "level", "type": "ENUM", "label": "客户等级", "enumOptions": ["VIP", "normal"] } ] }
    ] },

  { "name": "products", "type": "ARRAY", "label": "产品明细", "description": "动态明细，每项派一个审核待办",
    "item": {
      "name": "product", "type": "OBJECT",
      "fields": [
        { "name": "sku", "type": "STRING", "label": "编码" },
        { "name": "ownerUser", "type": "STRING", "label": "负责人" },
        { "name": "ownerRole", "type": "STRING", "label": "负责角色" }
      ] }
  }
]
```

这份声明会被**摊平**成可下拉的路径列表：

```
amount               NUMBER  申请金额  (必填)
region               ENUM    区域      [north, south]
order                OBJECT  订单
order.total          NUMBER  订单总额
order.customer       OBJECT  客户
order.customer.level ENUM    客户等级  [VIP, normal]
products             ARRAY   产品明细  ← isCollection=true
products[].sku       STRING  编码       ← 数组元素字段用 [] 记号
products[].ownerUser STRING  负责人
products[].ownerRole STRING  负责角色
```

## 10.4 声明存哪

声明写进 BPMN 的 `<process><extensionElements><cmx:varSchema>`（JSON 文本），随定义/版本走，不建新表：

```xml
<bpmn:process id="credit_approval" name="信用审批" isExecutable="true"
              cmx:varValidation="strict">
  <bpmn:extensionElements>
    <cmx:varSchema>[{"name":"amount","type":"NUMBER","label":"金额","required":true},
      {"name":"products","type":"ARRAY","label":"明细","item":{"name":"product","type":"OBJECT",
       "fields":[{"name":"sku","type":"STRING"},{"name":"ownerUser","type":"STRING"}]}}]</cmx:varSchema>
  </bpmn:extensionElements>
  <bpmn:startEvent id="start"/>
  ...
</bpmn:process>
```

- 与 `cmx:startFormKey`、`cmx:candidates` 同一处理范式（前缀无关）。
- 坏 JSON 或结构违规（枚举无候选、默认值类型不符、名称重复）→ 编译报错，挡回非法定义。
- `cmx:varValidation`（process 属性）= 发起校验策略，见 §10.7。

## 10.5 在设计器里声明（推荐）

不用手写 XML。流程设计工作台选中**画布空白（流程属性）** → property 区「流程变量」→「编辑变量声明」打开**全屏编辑器**：

- 每条变量：名称 + 类型下拉 + 标签 + 必填 + 说明。
- 选**对象** → 出现「对象字段」子区，可加子字段（递归）。
- 选**数组** → 出现「元素类型」下拉 + 元素字段子区（元素为对象时递归加字段）。
- 选**枚举** → 出现「候选值」输入（逗号分隔）。
- 底部「发起校验」下拉选 strict/lenient/off。
- 「保存声明」→ 后端 shape 校验通过后写入定义（随「保存草稿/发布」落库）。

声明后，流程属性区显示变量摘要 chips（标签 + 类型），一眼看全本流程有哪些变量。

## 10.6 声明的变量在哪里能用（四处下拉）

声明后，设计器四处从摊平路径直接下拉（仍保留手填逃生舱）：

| 位置 | 用法 |
|------|------|
| **分支条件构造器**（边条件） | 变量列 datalist 列出全部路径，选 `order.customer.level` 自动填入表达式 |
| **多实例集合**（会签/或签的 collection） | 只列 `type==ARRAY` 的路径（如 `products`） |
| **办理人表达式**（多实例逐元素派人） | 提示 `${elementVar.field}` 写法，引用 `products[]` 的字段（如 `${product.ownerUser}`，见 [02 §2.16](02-process-definition.md)、[04](04-organization-and-identity.md)） |
| **表单可写字段** | 列叶子路径 |

## 10.7 发起校验（策略可配）

发起 `POST /instances` 时，若定义声明了 schema：

1. **默认值物化**：对有 `default` 且未传的变量注入默认值（已传不覆盖）。
2. **软校验**：必填缺失 / 类型不符 / 枚举越界 → 产生违规。

策略由 process 级 `cmx:varValidation` 决定：

| 策略 | 行为 |
|------|------|
| `strict` | 违规 → **拒绝发起**，返回业务错误（`变量校验未过: 必填变量 'amount' 缺失`） |
| `lenient`（默认/缺省） | 违规仅 warn，**照常发起**（默认值仍注入） |
| `off` | 完全不校验 |

> 引擎运行内核**不看 schema**——校验只在发起边界做，变量运行态仍是动态 JSON，语义零变化。未声明变量一律放行（schema 不是白名单）。

## 10.8 REST

### 读变量声明 + 摊平路径

```bash
curl 'http://127.0.0.1:8091/api/flow/v1/definitions/credit_approval/variables'
# → {code:0, data:{
#     key, schema:[VarDecl...],
#     paths:[{path,type,label,description,required,isCollection,enumOptions}...]
#   }}
# ?version=N 看指定版本
```

`paths` 是设计器下拉的数据源；`isCollection:true` 标记数组变量（多实例 collection 下拉用）。

### 校验一份声明（设计器保存前）

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/definitions/variables/validate \
  -H 'Content-Type: application/json' \
  -d '{"schema":[{"name":"amount","type":"NUMBER","required":true}]}'
# → {code:0, data:{valid:true, violations:[]}}
# 违规 → {valid:false, violations:[{var, code, message}]}
```

### 发起（strict 校验示例）

```bash
# 声明了 amount 必填 + cmx:varValidation=strict → 缺失被拒
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"credit_approval","variables":{}}'
# → {code:1, msg:"变量校验未过: 必填变量 'amount' 缺失"}

# 满足声明（region 走默认 north）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"credit_approval","variables":{"amount":80000}}'
# → {code:0, data:{..., variables:{amount:80000, region:"north", ...}}}
```

## 10.9 完整示例：产品动态审核（声明驱动多实例）

声明 `products` 为对象数组，userTask 用 `collection=products` + `assignee=${product.ownerUser}` 逐元素派人（见 [02 §2.16](02-process-definition.md)）：

```xml
<bpmn:process id="product_review" name="产品审核" isExecutable="true">
  <bpmn:extensionElements>
    <cmx:varSchema>[{"name":"products","type":"ARRAY","label":"产品明细",
      "item":{"name":"product","type":"OBJECT","fields":[
        {"name":"sku","type":"STRING","label":"编码"},
        {"name":"ownerUser","type":"STRING","label":"负责人"}]}}]</cmx:varSchema>
  </bpmn:extensionElements>
  <bpmn:startEvent id="start"/>
  <bpmn:sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
  <bpmn:userTask id="review" name="产品审核" flowable:assignee="${product.ownerUser}">
    <bpmn:multiInstanceLoopCharacteristics isSequential="false"
         flowable:collection="products" flowable:elementVariable="product"/>
  </bpmn:userTask>
  <bpmn:sequenceFlow id="s1" sourceRef="review" targetRef="done"/>
  <bpmn:endEvent id="done"/>
</bpmn:process>
```

发起时传值（数量动态，10 个产品 10 个待办）：

```bash
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"product_review","variables":{"products":[
       {"sku":"A","ownerUser":"u_a"},
       {"sku":"B","ownerUser":"u_b"}]}}'
# 每个产品派给各自负责人，待办卡片显示各自的 element_value（见待办中心）
```

设计器里配这个 userTask 时：多实例集合下拉能选到已声明的 `products`；办理人表达式提示可用 `${product.ownerUser}`（`products[]` 的字段）。

## 10.10 不做什么

- 声明**不含值**，不改运行态 `Variables` 仍是动态 JSON 的事实。
- schema **不是白名单**：未声明的变量运行时照常放行。
- 不引入 JSON Schema / 外部校验库（自持类型系统，零依赖）。

---

上一篇 ← [09 运维与管理](09-operations-and-admin.md) ｜ 回到 [索引](README.md)
