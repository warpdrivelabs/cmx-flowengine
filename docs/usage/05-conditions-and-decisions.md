# 05 · 分支条件与决策表

本篇讲**流程怎么分叉**：条件表达式 DSL（运算符、内置函数、变量路径）、三种网关的分支语义、以及用决策表（DMN 风格）把复杂判断从流程图里抽出来。

## 5.1 条件表达式写在哪

分支条件写在 `sequenceFlow` 的 `<conditionExpression>` 里，配合网关：

```xml
<bpmn:exclusiveGateway id="gw" default="f_small"/>
<bpmn:sequenceFlow id="f_big" sourceRef="gw" targetRef="risk">
  <bpmn:conditionExpression>${amount &gt; 50000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="f_small" sourceRef="gw" targetRef="done"/>   <!-- default，无条件 -->
```

- `${ ... }` 或 `#{ ... }` 包裹可选（会被剥掉）。
- **空条件 = 无条件边，恒 true**（BPMN 语义）。
- XML 里 `>` 要写成 `&gt;`、`<` 写成 `&lt;`。
- 求值基于**实例变量**。

## 5.2 表达式 DSL

自研轻量表达式引擎（无外部脚本/沙箱，可静态检查）。入口 `eval_condition(expr, vars) -> bool`。

### 语法（EBNF）

```
expr    := or
or      := and ( "||" and )*
and     := cmp ( "&&" cmp )*
cmp     := add ( ("=="|"!="|"<"|"<="|">"|">=") add )?     // 非结合，每层至多一次比较
add     := mul ( ("+"|"-") mul )*
mul     := unary ( ("*"|"/") unary )*
unary   := "!" unary | "-" unary | primary
primary := number | string | bool | null | call | ident | "(" expr ")"
call    := ident "(" ( expr ( "," expr )* )? ")"
```

### 运算符

| 类别 | 运算符 |
|------|--------|
| 逻辑 | `&&`（或 `and`）、`||`（或 `or`）、`!`（或 `not`） |
| 比较 | `==` `!=` `<` `<=` `>` `>=` |
| 算术 | `+` `-` `*` `/` |
| 分组/调用 | `(` `)` `,` |

- 短路：`&&` / `||` 短路求值，返回布尔。
- **常见错误**：单个 `&` 报错（应 `&&`）；单个 `|` 报错；单个 `=` 报错（比较用 `==`）。
- `+`：两侧数值则相加；任一侧是字符串则**字符串拼接**。`-` `*` `/` 数值；`/ 0` 报「除数为零」。

### 字面量

| 类型 | 写法 | 说明 |
|------|------|------|
| 数字 | `100`、`0.5` | f64，无指数、无 token 内符号（负数用一元 `-`） |
| 字符串 | `'text'` 或 `"text"` | 转义 `\n`→换行、`\t`→制表、其它 `\x`→字面 `x`（故 `\\`/`\'`/`\"` 得字符本身） |
| 布尔 | `true` / `false` | |
| 空 | `null` | |

### 变量路径（支持点号嵌套）

裸标识符即变量：字母/`_` 开头，后续字母数字/`_`/`.`。查找规则：

1. **先整名精确匹配**（兼容字面上就叫 `order.amount` 的扁平 key）。
2. 无点且没找到 → `null`。
3. 点号下降：首段是顶层变量，逐段深入——对象按字段名、数组按数字下标（如 `items.0`）。任一段缺失 → `null`（不报错）。

```
${order.customer.level == 'VIP'}     // 嵌套对象
${items.0.qty > 10}                  // 数组下标
${amount > 50000 && urgent == true}  // 组合
```

### 真值判定 truthy

`null` / `false` / `0` / `""` / `[]` / `{}` → false；其余 → true。

### null 安全语义（重要）

- `==` / `!=`：结构比较（数字按 f64，其余结构相等）。
- `<` `<=` `>` `>=`：**任一侧为 null → 返回 false（不报错）**。例：`approvalLevel >= 3` 当 `approvalLevel` 未设时返回 false，走不到高级分支——这是防御性设计，避免变量缺失导致流程报错卡住。

> **无时间函数**：`NOW`/`DAYS`/`AGE` 等刻意排除（时钟是注入的扩展点，见定时器）。

## 5.3 内置函数（18 个）

`GET /api/flow/v1/conditions/functions` 可在线拉取目录。`arity` 为 null 表示可变参数。

| 函数 | 类别 | 参数数 | 行为 |
|------|------|--------|------|
| `LEN(x)`（别名 `LENGTH`） | string | 1 | 字符串字符数 / 数组长度 / 对象键数（null→0） |
| `UPPER(s)` | string | 1 | 转大写 |
| `LOWER(s)` | string | 1 | 转小写 |
| `TRIM(s)` | string | 1 | 去首尾空白 |
| `STARTSWITH(s, p)` | string | 2 | s 是否以 p 开头 |
| `ENDSWITH(s, p)` | string | 2 | s 是否以 p 结尾 |
| `CONTAINS(hay, needle)` | collection | 2 | 数组含元素 / 字符串含子串 / 对象含键 |
| `IN(x, a, b, ...)` | collection | 可变 | x 是否等于后续任一参数 |
| `IS_EMPTY(x)`（别名 `ISEMPTY`） | collection | 1 | 是否为空（null/空串/空数组/空对象） |
| `ABS(n)` | number | 1 | 绝对值 |
| `ROUND(n)` | number | 1 | 四舍五入 |
| `FLOOR(n)` | number | 1 | 向下取整 |
| `CEIL(n)`（别名 `CEILING`） | number | 1 | 向上取整 |
| `MIN(a, b, ...)` | number | 可变 | 最小值 |
| `MAX(a, b, ...)` | number | 可变 | 最大值 |
| `COALESCE(a, b, ...)` | logic | 可变 | 第一个非 null 参数 |
| `IF(cond, then, else)` | logic | 3 | 三元 |
| `NOT(x)` | logic | 1 | 逻辑取反（对 truthy 取反） |

函数名不区分大小写。未知函数报 `未知函数 ...`；定参数量不符报错。

例：

```
${IN(riskLevel, '高', '中')}                    // 风险中或高
${amount > 10000 && !IS_EMPTY(attachments)}     // 大额且有附件
${LEN(approvers) >= 3}                           // 至少 3 个审批人
${COALESCE(overrideLevel, approvalLevel) >= 2}   // 有覆盖用覆盖，否则用默认
${CONTAINS(tags, 'urgent')}                      // tags 数组含 urgent
```

## 5.4 在线校验与试算（免部署调试）

```bash
# 语法校验（不求值）
curl -X POST http://127.0.0.1:8091/api/flow/v1/conditions/validate \
  -H 'Content-Type: application/json' -d '{"expr":"${amount > 50000}"}'
# → {code:0, data:{valid:true}}   或   {valid:false, error:"..."}

# 代入变量试算
curl -X POST http://127.0.0.1:8091/api/flow/v1/conditions/eval \
  -H 'Content-Type: application/json' \
  -d '{"expr":"${amount > 50000 && urgent}","variables":{"amount":80000,"urgent":true}}'
# → {code:0, data:{expr, result:true, truthy:true}}
# 求值出错 → 业务错误 {code:1, msg:"条件求值失败: ..."}

# 列内置函数目录
curl http://127.0.0.1:8091/api/flow/v1/conditions/functions
# → {code:0, data:{functions:[{name,category,arity,desc}]}}
```

> 注意：`/validate` 的语法错误返回 `{valid:false}` 且 HTTP 200 code:0（软失败）；`/eval` 的求值错误返回业务错误 code:1。见 [06](06-rest-api-reference.md)。

## 5.5 三种网关的分支语义

| 网关 | fork（多出边） | join（多入边） |
|------|---------------|---------------|
| `exclusiveGateway` 排他 | **择一**：命中第一条满足条件的边；都不中走 default | 无合流语义（先到先过） |
| `inclusiveGateway` 包容 | **择若干**：走所有条件命中的边；都不中走 default | 等**所有实际到达的分支**到齐（可达性判断） |
| `parallelGateway` 并行 | **全走**：每条出边发一令牌（无条件，AND） | 等**所有入边令牌**到齐 |

### 排他网关（最常用）

```xml
<bpmn:exclusiveGateway id="gw" name="金额分级" default="f_small"/>
<bpmn:sequenceFlow id="f_big" sourceRef="gw" targetRef="director">
  <bpmn:conditionExpression>${amount &gt;= 100000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="f_mid" sourceRef="gw" targetRef="manager">
  <bpmn:conditionExpression>${amount &gt;= 10000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="f_small" sourceRef="gw" targetRef="done"/>
```

- 按出边**顺序**求值，命中第一条即走（10万走 director，5万走 manager，500 走 default done）。
- `default` 是网关属性，值 = 缺省边 id；至多一条；都不中且无 default → `NoOutgoingFlow`。

### 包容网关（多条件并行）

```xml
<bpmn:inclusiveGateway id="fork" default="fc"/>
<bpmn:sequenceFlow id="fa" sourceRef="fork" targetRef="taskA">
  <bpmn:conditionExpression>${amount &gt; 1000}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="fb" sourceRef="fork" targetRef="taskB">
  <bpmn:conditionExpression>${urgent == true}</bpmn:conditionExpression>
</bpmn:sequenceFlow>
<bpmn:sequenceFlow id="fc" sourceRef="fork" targetRef="taskC"/>
<!-- ...A/B/C → joinIncl（等实际到达的分支到齐）... -->
<bpmn:inclusiveGateway id="joinIncl"/>
```

`amount=5000, urgent=true` → A、B 都命中（C 不走）→ join 等 A、B 两个到齐；`amount=10, urgent=false` → 都不中 → 走 default C → join 等 C 一个。

### 并行网关（无条件全分叉）

见 [02 §2.9](02-process-definition.md)。fork 全发、join 全等，忽略条件。

## 5.6 决策表（DMN 风格，把复杂判断抽出流程图）

当分支判断复杂（多输入、多档位），把它从网关条件里抽成**决策表**，用 `businessRuleTask` 触发，输出写进变量供后续网关分支。

### 5.6.1 模型

```
DecisionTable {
  key:        String,            // 唯一标识，businessRuleTask decisionRef 引用它
  inputs:     [String],          // 输入变量名（文档/校验用）
  outputs:    [String],          // 输出变量名（文档/校验用）
  hit_policy: "FIRST" | "COLLECT",   // 命中策略，默认 FIRST
  rules: [ DecisionRule {
    conditions: [String],        // 每个输入一个条件（用 §5.2 的表达式 DSL）
    outputs:    { 输出名: JSON值 }
  } ]
}
```

- **命中策略**：`FIRST` = 命中第一条即停；`COLLECT` = 所有命中都应用（同名输出后者覆盖）。
- **空单元格或 `"-"`** = 「任意」（该条件恒真，跳过）——即 default 行。
- **无匹配** = 不写变量、不报错（后续网关按缺省分支走，宽容）。

### 5.6.2 决策表 JSON

```json
{
  "key": "approval_matrix",
  "inputs": ["amount"],
  "outputs": ["approvalLevel", "needBoard"],
  "hit_policy": "FIRST",
  "rules": [
    { "conditions": ["amount > 100000"], "outputs": { "approvalLevel": 3, "needBoard": true } },
    { "conditions": ["amount > 10000"],  "outputs": { "approvalLevel": 2, "needBoard": false } },
    { "conditions": ["-"],               "outputs": { "approvalLevel": 1, "needBoard": false } }
  ]
}
```

### 5.6.3 注册与调用（REST）

```bash
# 注册决策表
curl -X POST http://127.0.0.1:8091/api/flow/v1/decisions \
  -H 'Content-Type: application/json' \
  -d '{"key":"approval_matrix","inputs":["amount"],"outputs":["approvalLevel"],
       "hit_policy":"FIRST",
       "rules":[{"conditions":["amount > 100000"],"outputs":{"approvalLevel":3}},
                {"conditions":["-"],"outputs":{"approvalLevel":1}}]}'
# → {code:0, data:{key:"approval_matrix", rules:2}}
# 校验不过 → 业务错误 {code:1, msg:"决策表校验未过: ..."}

# 试算（不接流程，直接给输入看输出）
curl -X POST http://127.0.0.1:8091/api/flow/v1/decisions/evaluate \
  -H 'Content-Type: application/json' \
  -d '{"table":{...同上...},"variables":{"amount":500000}}'
# → {code:0, data:{matchedRules:[0], outputs:{approvalLevel:3}}}
```

> 决策表也可在引擎装配期用 `engine.register_decision(table)` 内置（demo 即如此）。校验规则（`validate()`）：key 非空；≥1 条规则；当 `inputs` 非空时，每条规则的 `conditions` 数量须等于 `inputs` 数量。

### 5.6.4 在流程里用（businessRuleTask + 网关）

完整示例 `rule_flow`：决策表定审批级别，网关按级别分支。

```xml
<bpmn:process id="rule_flow" name="决策审批" isExecutable="true">
  <bpmn:startEvent id="start"/>
  <bpmn:sequenceFlow id="s0" sourceRef="start" targetRef="rule"/>

  <bpmn:businessRuleTask id="rule" name="定审批级别" flowable:decisionRef="approval_matrix"/>
  <bpmn:sequenceFlow id="s1" sourceRef="rule" targetRef="gw"/>

  <bpmn:exclusiveGateway id="gw" default="toNormal"/>
  <bpmn:sequenceFlow id="toHigh" sourceRef="gw" targetRef="high">
    <bpmn:conditionExpression>${approvalLevel &gt;= 3}</bpmn:conditionExpression>
  </bpmn:sequenceFlow>
  <bpmn:sequenceFlow id="toNormal" sourceRef="gw" targetRef="normal"/>

  <bpmn:userTask id="high" name="董事会审批" flowable:assignee="board"/>
  <bpmn:userTask id="normal" name="经理审批" flowable:assignee="mgr"/>
  <bpmn:sequenceFlow id="h1" sourceRef="high" targetRef="done"/>
  <bpmn:sequenceFlow id="n1" sourceRef="normal" targetRef="done"/>
  <bpmn:endEvent id="done"/>
</bpmn:process>
```

运行：

```bash
# 大额 → 决策表写 approvalLevel=3 → 网关走 high（董事会）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -d '{"definitionKey":"rule_flow","variables":{"amount":500000}}' -H 'Content-Type: application/json'

# 小额 → approvalLevel=1 → 走 default normal（经理）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -d '{"definitionKey":"rule_flow","variables":{"amount":500}}' -H 'Content-Type: application/json'
```

> 决策表未注册时宽容：不写 `approvalLevel` → 网关条件 `approvalLevel >= 3` 因 null 返 false → 走 default normal。

## 5.7 决策表 vs 网关条件怎么选

| 场景 | 推荐 |
|------|------|
| 一两条简单分支（金额一刀切） | 网关条件边 `${amount > X}` |
| 多输入、多档位的矩阵判断（金额×风险×地区 → 级别） | 决策表 businessRuleTask，输出级别再用网关分支 |
| 判断逻辑常变、业务方维护 | 决策表（REST 注册，改表不改流程图） |

## 5.8 变量模型

流程变量是 `BTreeMap<String, JSON值>`（有序，确定性序列化），对应 PG `jsonb`。表达式、决策表、条件全基于它求值。

- 发起时通过 `variables` 传入（见各章示例）。
- 办结/退回/相关消息时 merge 新变量（同名覆盖）。
- serviceTask delegate、决策表输出、子流程回写都写它。

### 变量 Schema（可选的设计态声明）

引擎提供可选的变量 schema（`VarSchema`），用于设计态声明变量形状、默认值、必填、枚举——校验与默认值注入，非运行时白名单（未声明变量照常放行）：

| 概念 | 取值 |
|------|------|
| `VarType` | STRING / NUMBER / BOOLEAN / DATE / ENUM / OBJECT / ARRAY |
| `VarSource` | MANUAL / FORM_FIELD / SERVICE_RETURN / START_PARAM / SUBFLOW_RETURN |
| `VarScope` | INSTANCE / TASK |
| `VarDecl` | `{name, type, label?, default?, source, scope, required, enumOptions[]}` |

作用：`materialize_defaults` 注入缺省值（仅当变量缺失/为 null，不覆盖已有）；`validate_values` 校验必填/类型/枚举。这是设计态辅助能力，运行核不强制。

## 5.9 附录：ISO 8601 时长（定时器用）

边界定时器 `<timeDuration>` 用 ISO 8601 相对时长，`parse_iso8601_duration` 归一成秒：

- **格式**：`P[nD]T[nH][nM][nS]`，每段可选，至少一段非零。
- **例**：`PT30S`（30秒）、`PT10M`（10分）、`PT1H`、`PT1H30M`、`P1D`、`P1DT2H`、`P2DT3H4M5S`。
- 大小写不敏感（`P`/`T`/单位字母），首尾空白被 trim。
- **不支持**：周（`nW`）、月/年、小数、负数、`H/M/S` 出现在 `T` 之前、缺数字的单位（`PTH`）、零/空总量（`P0D`/`PT`）。

---

上一篇 ← [04 组织机架 · 用户 · 角色 · 岗位对接](04-organization-and-identity.md) ｜ 下一篇 → [06 REST API 调用说明](06-rest-api-reference.md)
