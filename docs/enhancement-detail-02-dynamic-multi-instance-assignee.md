# ② 详细实现设计：动态多实例逐元素派人

> 状态：**引擎侧已落地实现（2026-08-14），单测 + 集成 + 真机 live 全绿**。父方案见 [`enhancement-proposal-designer-mi-reject-withdraw.md`](enhancement-proposal-designer-mi-reject-withdraw.md)。
> 落地：`cmx-flow-model` 新增 `eval_value`/`interpolate`；引擎 `expand_mi_element`（async 逐元素解析）取代 `push_mi_sub_instance`，并行/顺序两展开点共用，`complete_mi_task` 返 `PendingMiExpand` 交 `complete_task` async 展开；三种写法（元素插值/候选表达式插值/集合即人）全通。测试：model 46 + 引擎 94（新增 8 个 `mi_dynamic_assignee`）零回归、新代码 0 clippy；真机 live 部署 MI 流程 3 产品各派其负责人验证通过。
> **待补（UI 层，可并入 ①）**：待办中心展示 `element_value`（办理人看到审的是哪个产品）、设计器多实例属性分组。
> 目标：表单里动态数量的明细（如 10/100 个产品），每个明细派生一个待办，**办理人由该明细数据决定**（角色/岗位/用户）。
> 日期：2026-08-14

## 1. 一句话结论

多实例**展开骨架已具备**（按 `collection` 数组长度展开 N 个子任务，数量随单据数据变，不写死在图里）。唯一缺口：`push_mi_sub_instance` 把**静态 `assignee` 原样拷给每个子任务**——无 `${...}` 求值、无逐元素候选人解析。本项补两件事：

1. **表达式引擎**暴露 `eval_value` + 新增 `interpolate`（`${...}` 模板求值）。
2. **多实例展开**改为**逐元素**：把元素注入求值作用域，求值出该元素的办理人（直派或候选池），两个展开点（并行初展开 + 顺序续展开）都走同一 async 解析。

> 副带修复：demo `expense_countersign` 里 `flowable:assignee="${approver}"` 今天不生效（存字面量），本项一并修好。

## 2. 现状锚点（代码位置）

| 已有 | 位置 |
|------|------|
| MI 编译（collection/elementVariable/assignee/candidates 都已解析） | `crates/cmx-flow-bpmn/src/compiler.rs:347` `parse_multi_instance` + `parse_user_task` |
| MI 初展开（并行/顺序首个） | `crates/cmx-flow-engine/src/engine.rs:1849` run_to_wait 的 `UserTask.multi_instance = Some(mi)` 分支 |
| 子实例推入（**问题点**：拷静态 assignee） | `engine.rs:2489` `push_mi_sub_instance`（`assignee: ut.assignee.clone()`） |
| 顺序续展开（或签下一个） | `engine.rs:2604` `complete_mi_task` 内 `push_mi_sub_instance(...)` |
| 单实例候选人解析（可复用范式） | `engine.rs:1806` `resolve_ctx_of` + `1808` `resolve_candidates` + `2134` `decide_assignment` |
| 表达式求值内核（私有 Value 级） | `crates/cmx-flow-model/src/expr.rs:513` `fn eval(ast, vars) -> Result<Value>` |
| 表达式布尔入口（公开） | `expr.rs:43` `eval_condition` |
| 候选人表达式解析 | `crates/cmx-flow-model/src/candidate.rs:20` `parse_candidate_expr` |

> ⚠ 元素已定格：MI 进入时集合快照进 `MiScope.collection`（`engine.rs:1889`），逐元素求值天然确定，不受中途变量改动影响。

## 3. 三种写法（决策：全做，统一机制）

| # | 写法 | BPMN | 语义 |
|---|------|------|------|
| A 元素插值 | `flowable:assignee="${product.ownerUser}"` | 每个子任务办理人 = 该产品负责人字段 | 直派 |
| B 候选人表达式插值 | `cmx:candidates="role(${product.ownerRole})"` | 每个子任务候选 = 该产品负责角色 | 解析→0 兜底/1 直派/≥2 候选池 |
| C 集合即人 | `flowable:assignee="${approver}"`（`approvers=["u1","u2"]`） | 每人一个待办 | 直派（A 的标量退化） |

三种归约为同一步骤：**用元素作用域插值 → （若是候选人表达式）解析成用户 → 决定直派/候选池**。

完整定义示例（写法 A + B 混用）：

```xml
<bpmn:userTask id="product_review" name="产品审核"
    flowable:assignee="${product.ownerUser}"
    cmx:candidates="role(${product.ownerRole})">
  <bpmn:multiInstanceLoopCharacteristics isSequential="false"
       flowable:collection="products" flowable:elementVariable="product">
    <bpmn:completionCondition>${nrOfCompletedInstances == nrOfInstances}</bpmn:completionCondition>
  </bpmn:multiInstanceLoopCharacteristics>
</bpmn:userTask>
```

发起变量：

```json
{ "products": [
    { "sku":"A", "ownerUser":"u_a", "ownerRole":"fin_a" },
    { "sku":"B", "ownerUser":"u_b", "ownerRole":"fin_b" }
] }
```

→ 展开 2 个子任务，各自 `element_value` 带 A/B 产品数据，办理人分别按 A/B 的字段解析。100 个产品同理展 100 个，**无需改定义**。

## 4. 表达式引擎改动（`cmx-flow-model/src/expr.rs`）

### 4.1 暴露 Value 级求值

```rust
/// 求值一个表达式为 Value（非布尔）。复用现有私有 eval 内核，仅暴露入口。
pub fn eval_value(expr: &str, vars: &Variables) -> Result<Value>
```

内部 = 解析 AST（现有）→ 调私有 `eval(ast, vars)`（`expr.rs:513`，已返回 `Value`）。零新求值逻辑。

### 4.2 新增模板插值

```rust
/// 把模板串里的每个 ${expr} 求值并字符串化后替换；无 ${} 则原样返回（静态字面量零改动）。
pub fn interpolate(template: &str, vars: &Variables) -> Result<String>
```

- 扫描 `${ ... }` 片段（支持整串即一个表达式 `${approver}`，也支持嵌入 `user_${product.id}`）。
- 每个片段 `eval_value` → 按 §4.3 字符串化 → 替换。
- 无 `${}` → 原样返回 ⇒ **静态 assignee 完全向后兼容**。
- `${` 无闭合 → 报错（表达式错误信封）。

### 4.3 Value → 办理人字符串规则

| Value | 结果 |
|-------|------|
| String(s) | s |
| Number(n) / Bool(b) | 其 to_string |
| Null | 空串（→ 视作无此办理人，走兜底） |
| Array / Object | 空串 + warn（办理人不该是复合值，宽容不 panic） |

## 5. 引擎展开改动（`cmx-flow-engine/src/engine.rs`）

### 5.1 新增 async 逐元素展开方法

抽一个 Engine 方法，两个展开点共用：

```rust
async fn expand_mi_element(
    &self,
    snapshot: &mut InstanceSnapshot,
    node_bpmn: &str,
    node_name: &Option<String>,
    ut: &UserTask,          // 从 def 借用（只读，可跨 await）或克隆
    element: Value,         // 当前元素
    index: usize,           // loopCounter
    now: DateTime<Utc>,
) -> Result<()>
```

内部步骤（**先 async 解析、后改 snapshot**，规避跨 await 的 &mut 别名）：

1. **建元素作用域**：`let mut scope = snapshot.instance.variables.clone();` 再 `scope.set(mi.element_var, element.clone())`、`scope.set("loopCounter", json!(index))`。（元素变量名 shadow 实例同名变量，符合预期。）
2. **建解析上下文**：`let rctx = Self::resolve_ctx_of(snapshot);`（读快照，得 owned `ResolveContext`）。
3. **插值静态 assignee**：`let assignee = ut.assignee.as_deref().map(|a| interpolate(a, &scope)).transpose()?;`（无 assignee → None；无 `${}` → 原样）。
4. **插值候选人 refs**：对 `ut.candidates` 每条，`interpolate(&ref.value, &scope)` → 具体 `CandidateRef{kind, value}`（关系型空值 refs 插值后仍空，不受影响）。
5. **解析候选人**（async，只用 `&self` + `rctx`，此刻**不借 snapshot**）：`let resolved = self.resolve_candidates(&interpolated_refs, &rctx).await?;`
6. **决定分派**：`let (assignee, candidates) = decide_assignment(&task_id, &instance_id, &interpolated_refs, &resolved, assignee);`（0 人→兜底插值后的静态 assignee；1→直派；≥2→候选池）。
7. **改 snapshot**：push 一个 `Token{Waiting}` + `Task{assignee, element_value: Some(element), candidate_groups: ut.candidate_groups.clone(), ...}` + `candidates`。

> 借用纪律：第 5 步 await 之前已把要用的都 clone 成 owned（scope/rctx/refs），await 期间不持有 snapshot 的读借用；`&mut snapshot` 作为方法参数由 future 持有，无别名，合法。`ut` 从 `def`（只读）借用，可跨 await；若生命周期难处理则 clone（`complete_mi_task` 已有 `ut.clone()` 先例）。

### 5.2 改并行/首个展开点（run_to_wait MI 分支 ~1895）

现状：

```rust
for element in collection.iter().take(expand_now) {
    push_mi_sub_instance(snapshot, &node_bpmn, &node_name, ut, element.clone(), now);
}
```

改为：

```rust
for (i, element) in collection.iter().take(expand_now).enumerate() {
    self.expand_mi_element(snapshot, &node_bpmn, &node_name, ut, element.clone(), i, now).await?;
}
```

（`run_to_wait` 已是 `async fn ... &self`，可直接 await。循环内不再持 `snapshot.tokens[idx]` 等借用——初展开前已 `snapshot.tokens.remove(idx)` 并建好 scope，循环体只调方法。）

### 5.3 改顺序续展开点（complete_mi_task ~2604）

`complete_mi_task` 是同步自由函数、无 `&self`（拿不到 resolver），不能在其内 async 解析。改法：**它只算出「下一个待展开元素」，把 async push 交回调用方 `complete_task`**。

- `complete_mi_task` 签名改为返回 `Result<Option<PendingMiExpand>>`，其中
  `PendingMiExpand { node_bpmn, node_name, ut, element, index }`（`index = scope.next_index - 1`）。
- 现有 §2543 段已算出 `next_element`；把「就地 `push_mi_sub_instance`」改成「返回 `Some(PendingMiExpand{...})`」；收口/并行分支返回 `None`。
- `complete_task`（async，有 `&self`）在调用 `complete_mi_task` 后：`if let Some(p) = pending { self.expand_mi_element(snapshot, &p.node_bpmn, &p.node_name, &p.ut, p.element, p.index, now).await?; }`。

> 这样保持自由函数同步、把唯一需要 async 的一步（候选人解析）上浮到有 `&self` 的方法，借用与生命周期最简。

### 5.4 `push_mi_sub_instance` 去留

`expand_mi_element` 取代它的职责。可删除，或保留为「无表达式的纯 push」被 `expand_mi_element` 内部复用（把 push 那段抽成小 helper）。建议：把「push token+task」抽成 `push_mi_task(snapshot, ..., assignee, candidates, element)` 小 helper，`expand_mi_element` 解析完调它——职责清晰、易测。

## 6. BPMN 编译器（`cmx-flow-bpmn`）

**无需改**。已验证：`parse_user_task` 读 `assignee` + `candidates`（含 `candidateGroups`/`candidateUsers`/`cmx:candidates`）不论是否 MI；`parse_multi_instance` 读 `collection`/`elementVariable`/`completionCondition`。

**需验证（写测试即可）**：`parse_candidate_expr("role(${product.ownerRole})", Role)` 要把 `${product.ownerRole}` 原样保留进 `CandidateRef.value`（解析只取括号内子串，预期不破坏 `${}`）。

## 7. 前端配合（最小可用）

引擎改完即可用；前端补两处让它好用（完整设计器 UX 归 ① 文档）：

- **待办中心**（`web/core/todo-center.js` + `ui-native` 镜像）：MI 子任务卡片展示 `element_value`（如产品 SKU/名称），让办理人看清「我这条审哪个产品」。字段已在 `Task.element_value`，渲染即可。
- **设计器**（design-workbench，属于 ① 但②要用到最小项）：userTask 属性面板「多实例」分组——会签/或签开关、集合变量、元素变量、办理人表达式（带 `${elementVar.field}` 输入提示）、完成条件预设下拉（全部完成 / 过半 / 任一驳回）。

## 8. 边界与语义

| 情形 | 行为 |
|------|------|
| 空集合 | 跳过 MI 节点（现有行为，保留） |
| 元素是标量（"u1"） | `${approver}` → 取元素本身；`${approver.x}` → null → 兜底 |
| 元素是对象 | `${product.ownerUser}` → 点路径下降（现有嵌套查找） |
| assignee 表达式求值 null/空 | 该子任务无直派 assignee（若也无候选 → 无人，warn；同单实例 0 兜底语义） |
| 候选表达式解析 0 人 | `decide_assignment` 回退到（插值后的）静态 assignee，无则无人 |
| 候选解析 ≥2 人 | 落候选池，逐元素独立认领（每个产品各自一个候选池） |
| 静态字面量 assignee（无 `${}`） | `interpolate` 原样返回 ⇒ **完全向后兼容**，老流程零回归 |
| completionCondition | 不变（`nrOf*` 计数注入，过半/任一驳回照旧） |
| 顺序或签 | 下一个元素在办结时解析（§5.3），各元素各自派人 |
| 100 元素 | 展开 100 子任务；每元素一次候选解析（并发在 run_to_wait 顺序内，注意解析次数=元素数，见风险） |

## 9. 测试计划（`cmx-flow-tests`，对齐 m3 风格）

新增 `mi_dynamic_assignee.rs`（内存态 always-runnable，用 Mock/内存 resolver）：

| 用例 | 断言 |
|------|------|
| A 元素插值（对象数组） | N 个子任务，各 assignee = 对应元素 `ownerUser` |
| B 候选表达式插值（角色→1 人） | 每子任务直派该产品角色的唯一用户 |
| B 候选表达式插值（角色→≥2 人） | 每子任务各自落候选池，可分别 claim |
| C 集合即人（`${approver}`，字符串数组） | approvers=["u1","u2"] → 两任务派 u1/u2（修复现状） |
| 顺序或签逐元素 | next 元素展开时用自己的解析结果 |
| 空集合 | 跳过节点（回归保护） |
| 静态字面量 assignee | 与今天完全一致（回归保护） |
| assignee 求值 null | 无 assignee + warn，不 panic |
| completionCondition 过半 | 过半收口，剩余作废（回归保护） |
| 100 元素压测 | 展 100 任务、element_value 正确 |

表达式引擎单测（`expr.rs`）：`eval_value` 各类型；`interpolate`（整串/嵌入/无 `${}`/未闭合/嵌套路径/标量元素）。

编译器单测：`parse_candidate_expr` 保留 `${}`。

## 10. 落地清单与工作量

| 层 | 文件 | 改动 |
|----|------|------|
| 表达式 | `cmx-flow-model/src/expr.rs` | `eval_value` 暴露 + `interpolate` 新增 |
| 表达式导出 | `cmx-flow-model/src/lib.rs` | 导出 `eval_value` / `interpolate` |
| 引擎 | `cmx-flow-engine/src/engine.rs` | `expand_mi_element` async 方法 + `push_mi_task` helper；改并行展开循环（await）；`complete_mi_task` 返回 `PendingMiExpand`，`complete_task` 续展开 |
| 编译器 | `cmx-flow-bpmn` | 无改动（补一个解析测试） |
| 前端 | `web/core/todo-center.js` + `ui-native` 镜像 | 展示 element_value |
| 前端 | design-workbench（②最小项，完整归①） | MI 属性分组 |
| 测试 | `cmx-flow-tests/tests/mi_dynamic_assignee.rs` + expr 单测 | 见 §9 |
| 文档 | `usage/02` §多实例 + `usage/04` §候选人 | 补逐元素派人写法 |

**工作量**：表达式 0.5 天 + 引擎 1.5–2 天（async 展开重构 + 两展开点 + 测试）+ 前端 0.5–1 天 = **约 3 天**。
**风险**：**中**——
- run_to_wait / complete 路径引入 async 逐元素解析，借用与生命周期要小心（§5.1 已给纪律）；空集合/单元素/顺序续展/静态兜底等回归边界必须测全。
- 每元素一次候选解析：100 元素 = 100 次 resolver 调用（`Pg`/`Http` 模式下是 100 次 DB/HTTP）。若同一角色被多元素复用，可加**每次运行段内的解析缓存**（key=CandidateRef）降调用数——列为可选优化，不阻塞首版。

## 11. 不做什么

- 不改 MI 展开的**数量语义**（仍按集合长度，本就动态）。
- 不引入通用脚本/沙箱——`interpolate` 只调既有受控表达式引擎（无时间/IO/反射）。
- 不做子实例间的复杂依赖/顺序编排（顺序=或签逐个，并行=会签齐头，维持现语义）。
- 完整设计器 MI 可视化归 ① 文档；本项只给「能配出来」的最小面板项。

---

_确认后即可进入实现。剩余详细设计：④ 取回/撤回、① 设计器 UX。_
