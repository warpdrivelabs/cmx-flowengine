# 03 · 子流程定义

引擎支持两种子流程：

| 类型 | BPMN 元素 | 机制 | 独立实例? | 典型场景 |
|------|-----------|------|-----------|---------|
| **调用式子流程** | `callActivity` | 调用另一份独立部署的流程定义，主令牌挂起同步等待 | ✅ 有独立子实例 | 财务复核、信用评估等可复用、可按组织路由的子流程 |
| **嵌入式子流程** | `subProcess` | 编译期扁平化进父图 | ❌ 无独立实例 | 把一组节点打包成一个逻辑块 |

## 3.1 调用式子流程 callActivity

### 3.1.1 两种指定方式

**方式 A：写死子流程 key（`calledElement`）**

```xml
<bpmn:callActivity id="call_fin" name="财务复核" calledElement="fin_review_hq">
  <bpmn:extensionElements>
    <flowable:in source="amount" target="reviewAmount"/>
    <flowable:out source="finResult" target="finResult"/>
  </bpmn:extensionElements>
</bpmn:callActivity>
```

**方式 B：逻辑 key（`cmx:calledKey`），运行期按组织路由到具体子流程**

```xml
<bpmn:callActivity id="call_fin" name="财务复核(按组织路由)" cmx:calledKey="fin_review">
  <bpmn:extensionElements>
    <flowable:in source="amount" target="reviewAmount"/>
    <flowable:out source="finResult" target="finResult"/>
  </bpmn:extensionElements>
</bpmn:callActivity>
```

规则：

- `calledElement`（具体定义 key）与 `cmx:calledKey`（逻辑名）**至少一个**，都无则 `MissingElement`。
- 用逻辑 key 时，运行期由 `SubflowRouter` 按发起组织解析成具体子流程 key（见 §3.3）。
- `cmx:calledKey` 用于「同一主流程，不同组织跑不同子流程版本」。

### 3.1.2 变量映射（父子传参）

子流程有独立变量空间，通过 `<in>`/`<out>` 显式映射：

```xml
<flowable:in  source="amount"    target="reviewAmount"/>   <!-- 主→子：把主变量 amount 传给子变量 reviewAmount -->
<flowable:out source="finResult" target="finResult"/>       <!-- 子→主：子完成后把子变量 finResult 写回主 -->
```

| 项 | 规则 |
|----|------|
| `<in>` / `<out>` | 按本地名匹配，可直接在 callActivity 下，也可嵌在 `<extensionElements>` 里（递归查找） |
| `source` | 源变量名（可退回 `sourceExpression`） |
| `target` | 目标变量名（缺省 = `source`） |
| 空的 `<in>` 列表 | 全量透传所有主变量给子 |
| 空的 `<out>` 列表 | 子的全部变量写回主 |

**属性简写**（仅当结构化 `<in>`/`<out>` 为空时生效，不合并）：

```xml
<bpmn:callActivity id="c2" calledElement="sub"
    cmx:inVars="amount:subAmount, applicant" cmx:outVars="finResult:result"/>
<!-- source:target 对，逗号分隔；省略 :target 则 target=source -->
```

### 3.1.3 运行语义

```
主令牌到 callActivity → 转 WAITING_SUBFLOW 挂起（提交点）
   → 解析子流程 key（calledKey 走 router，否则 calledElement）
   → 映射输入变量 → 创建独立子实例（parent_instance_id/parent_token_id 指回主）
   → 子实例跑（复用完整推进内核，可含会签/定时器/转签/再套子流程）
   → 子完成 → 按 parent_token_id 精确唤醒主令牌 → 回写输出变量 → 主令牌沿 callActivity 出边继续
```

- 子流程是**完整独立实例**，能力无阉割（可再含会签、定时器、转签，甚至再调子流程，**可嵌套多层**）。
- 子实例复用完整推进内核；`complete_subflow` 逐层向上唤醒（父完成 → 唤醒祖父）。
- 一个主流程可挂多处 callActivity（串行/并行），每处一个子实例；串行多挂载用 `(token, node)` 去重键正确起子。

### 3.1.4 查子实例

```bash
curl http://127.0.0.1:8091/api/flow/v1/instances/<mainInstanceId>/children
# → {code:0, data:{children:[ <每个子实例的 instance_view> ]}}
```

主实例详情里 `waitingSubflow:true` 标记它正等子流程；子实例 `parentInstanceId` 指回主。

## 3.2 完整示例：报销主流程 + 按组织路由的财务复核子流程

**主流程 subflow_main**（用逻辑 key）：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"
                  xmlns:flowable="http://flowable.org/bpmn"
                  xmlns:cmx="http://cmx/flow"
                  targetNamespace="http://cmx/flow/demo">
  <bpmn:process id="subflow_main" name="报销主流程(含子流程)" isExecutable="true">
    <bpmn:startEvent id="start" name="提交报销"/>
    <bpmn:sequenceFlow id="f0" sourceRef="start" targetRef="mgr"/>

    <bpmn:userTask id="mgr" name="部门经理审批" flowable:assignee="部门经理"/>
    <bpmn:sequenceFlow id="f1" sourceRef="mgr" targetRef="call_fin"/>

    <bpmn:callActivity id="call_fin" name="财务复核(按组织路由)" cmx:calledKey="fin_review">
      <bpmn:extensionElements>
        <flowable:in source="amount" target="reviewAmount"/>
        <flowable:out source="finResult" target="finResult"/>
      </bpmn:extensionElements>
    </bpmn:callActivity>
    <bpmn:sequenceFlow id="f2" sourceRef="call_fin" targetRef="cashier"/>

    <bpmn:userTask id="cashier" name="出纳打款" flowable:assignee="出纳"/>
    <bpmn:sequenceFlow id="f3" sourceRef="cashier" targetRef="done"/>
    <bpmn:endEvent id="done" name="报销完成"/>
  </bpmn:process>
</bpmn:definitions>
```

**子流程 A：总部三级复核 fin_review_hq**

```xml
<bpmn:process id="fin_review_hq" name="总部财务复核" isExecutable="true">
  <bpmn:startEvent id="start" name="进入复核"/>
  <bpmn:sequenceFlow id="f0" sourceRef="start" targetRef="fin1"/>
  <bpmn:userTask id="fin1" name="财务初审" flowable:assignee="财务专员"/>
  <bpmn:sequenceFlow id="f1" sourceRef="fin1" targetRef="fin2"/>
  <bpmn:userTask id="fin2" name="财务经理复核" flowable:assignee="财务经理"/>
  <bpmn:sequenceFlow id="f2" sourceRef="fin2" targetRef="done"/>
  <bpmn:endEvent id="done" name="复核完成"/>
</bpmn:process>
```

**子流程 B：分公司单签 fin_review_branch**

```xml
<bpmn:process id="fin_review_branch" name="分公司财务复核" isExecutable="true">
  <bpmn:startEvent id="start" name="进入复核"/>
  <bpmn:sequenceFlow id="f0" sourceRef="start" targetRef="fin"/>
  <bpmn:userTask id="fin" name="分公司财务经理单签" flowable:assignee="分公司财务经理"/>
  <bpmn:sequenceFlow id="f1" sourceRef="fin" targetRef="done"/>
  <bpmn:endEvent id="done" name="复核完成"/>
</bpmn:process>
```

三份定义都部署。主流程的 `call_fin` 用逻辑 key `fin_review`，运行期按发起组织路由到 A 或 B。

## 3.3 子流程组织路由（同主流程，按组织跑不同子流程）

核心诉求：**一份主流程定义，总部走三级复核、分公司走单签**——用逻辑 key + 组织绑定表实现。

### 3.3.1 绑定表 cmx_flow_subflow_binding

| 列 | 说明 |
|----|------|
| `called_key` | 逻辑子流程 key（如 `fin_review`） |
| `org_id` | 适用组织（`NULL` = 默认兜底绑定） |
| `target_definition_key` | 解析到的具体子流程 key |
| `enabled` | 是否启用 |

### 3.3.2 三层解析（`PgSubflowRouter`）

发起实例带 `orgId`，令牌到 callActivity 时按逻辑 key + 组织解析：

```
① 精确：本组织 (called_key, org_id) 的绑定
   ↓ 无
② 继承：沿 cmx_org.path 向上找最近祖先组织的绑定（path 最长优先）
   ↓ 无
③ 兜底：org_id IS NULL 的默认绑定
   ↓ 无 → 报错 NoBinding
```

### 3.3.3 配置绑定（REST）

```bash
# 总部 fin_review → 总部三级
curl -X POST http://127.0.0.1:8091/api/flow/v1/subflow-bindings \
  -H 'Content-Type: application/json' \
  -d '{"calledKey":"fin_review","orgId":"df_root","targetKey":"fin_review_hq","enabled":true}'

# 上海分公司 fin_review → 分公司单签
curl -X POST http://127.0.0.1:8091/api/flow/v1/subflow-bindings \
  -H 'Content-Type: application/json' \
  -d '{"calledKey":"fin_review","orgId":"df_sh","targetKey":"fin_review_branch","enabled":true}'

# 默认兜底（org_id 省略/空 → 默认绑定）→ 总部三级
curl -X POST http://127.0.0.1:8091/api/flow/v1/subflow-bindings \
  -H 'Content-Type: application/json' \
  -d '{"calledKey":"fin_review","targetKey":"fin_review_hq","enabled":true}'
```

| 操作 | 端点 |
|------|------|
| 新增/更新绑定 | `POST /subflow-bindings` — body `{calledKey, orgId?, targetKey, enabled}` → `{id}` |
| 列某逻辑 key 的绑定 | `GET /subflow-bindings/{calledKey}` → `{calledKey, bindings:[{id,calledKey,orgId,orgName,targetKey,enabled,remark,isDefault}]}` |
| 删绑定 | `DELETE /subflow-bindings/id/{id}` → `{deleted:id}` |
| 列组织树（配绑定用） | `GET /orgs` → `{orgs:[{id,name,parentId,path}]}` |

### 3.3.4 发起时选组织

```bash
# 总部发起 → 路由到 fin_review_hq（三级复核）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"subflow_main","orgId":"df_root","variables":{"applicant":"张三","amount":8000}}'

# 上海发起 → 路由到 fin_review_branch（单签）
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"subflow_main","orgId":"df_sh","variables":{"applicant":"李四","amount":8000}}'

# 北京发起（未精确绑定）→ 沿 path 继承总部 → fin_review_hq
curl -X POST http://127.0.0.1:8091/api/flow/v1/instances \
  -H 'Content-Type: application/json' \
  -d '{"definitionKey":"subflow_main","orgId":"df_bj","variables":{"applicant":"王五","amount":8000}}'
```

### 3.3.5 组织继承演示

demo 播种的组织树：`df_root(总部) → df_bj(北京) / df_sh(上海)`，`cmx_org.path` 物化路径 `/df_root`、`/df_root/df_bj`、`/df_root/df_sh`。绑定：

| called_key | org_id | target | 说明 |
|-----------|--------|--------|------|
| fin_review | df_root | fin_review_hq | 总部精确 |
| fin_review | df_sh | fin_review_branch | 上海精确 |
| fin_review | (NULL) | fin_review_hq | 默认兜底 |

结果：总部→hq（精确）、上海→branch（精确）、**北京→hq（无精确绑定，沿 path 继承总部）**、未知组织→hq（兜底）。

### 3.3.6 错误情形

- 用逻辑 key 但**未注入路由器** → 报错。
- 路由器在但该组织无任何绑定（含默认）→ `RouteError::NoBinding`。

### 3.3.7 路由器后端选择

`FLOW_SUBFLOW_MODE` 环境变量（默认 `pg`）：

- `pg` → `PgSubflowRouter`，读 `cmx_flow_subflow_binding` + `cmx_org.path` 三层解析。
- `http` → `HttpSubflowRouter`，`POST {FLOW_SUBFLOW_URL}/subflow/resolve`，body `{calledKey, orgId}` → `{targetKey}`（详见 [08](08-external-integration.md)）。
- `mock` → 固定映射（测试用）。

## 3.4 嵌入式子流程 subProcess

把一组节点打包成父图里的一个逻辑块，**编译期扁平化**：

```xml
<bpmn:process id="embedded_flow" name="含嵌入子流程" isExecutable="true">
  <bpmn:startEvent id="start"/>
  <bpmn:sequenceFlow id="e0" sourceRef="start" targetRef="apply"/>
  <bpmn:userTask id="apply" name="申请" flowable:assignee="u"/>
  <bpmn:sequenceFlow id="e1" sourceRef="apply" targetRef="block"/>

  <bpmn:subProcess id="block" name="审核块">
    <bpmn:startEvent id="bstart"/>
    <bpmn:sequenceFlow id="b0" sourceRef="bstart" targetRef="review"/>
    <bpmn:userTask id="review" name="块内复核" flowable:assignee="r"/>
    <bpmn:sequenceFlow id="b1" sourceRef="review" targetRef="bend"/>
    <bpmn:endEvent id="bend"/>
  </bpmn:subProcess>

  <bpmn:sequenceFlow id="e2" sourceRef="block" targetRef="notify"/>
  <bpmn:userTask id="notify" name="通知" flowable:assignee="n"/>
  <bpmn:sequenceFlow id="e3" sourceRef="notify" targetRef="done"/>
  <bpmn:endEvent id="done"/>
</bpmn:process>
```

编译期处理（3 遍编译的 Pass 3 `wire_subprocesses`）：

- 内部节点（`bstart`/`review`/`bend`）被**提升进父 arena**。
- `subProcess` 节点变成透传节点：进块 → 内部 `startEvent`；内部 `endEvent` → 透传出块到 subProcess 的出边目标。
- 内部 `startEvent` 必填（缺则 `MissingElement`）。

运行：令牌透传进 `review` 待办，办结后透传出块到 `notify`，全程**同一实例**（无独立子实例）。

### 限制：块级边界事件不支持

给 `subProcess` 挂边界事件（整块超时）需要嵌套作用域，**显式不支持**，编译报 `Unsupported`：

```xml
<!-- ✗ 会报错 -->
<bpmn:subProcess id="blk">...</bpmn:subProcess>
<bpmn:boundaryEvent id="bnd" attachedToRef="blk">
  <bpmn:timerEventDefinition><bpmn:timeDuration>PT1H</bpmn:timeDuration></bpmn:timerEventDefinition>
</bpmn:boundaryEvent>
```

需要「整块超时」时，改用 callActivity 调用式子流程（子实例可独立管理超时），或在块内单节点上挂边界定时器。

## 3.5 callActivity vs subProcess 怎么选

| 维度 | callActivity（调用式） | subProcess（嵌入式） |
|------|----------------------|---------------------|
| 复用 | ✅ 子流程独立部署，多主流程可复用 | ❌ 内联在一份定义里 |
| 按组织路由 | ✅ 逻辑 key + 绑定表 | ❌ |
| 独立实例/变量空间 | ✅ 有，可独立查询/管理 | ❌ 同一实例 |
| 块级边界事件（整块超时） | ✅（子实例自管） | ❌ 不支持 |
| 嵌套 | ✅ 可多层 | ✅ 扁平化 |
| 适用 | 可复用、可路由、需独立生命周期的子流程 | 纯粹的图形分组 |

---

上一篇 ← [02 主流程定义](02-process-definition.md) ｜ 下一篇 → [04 组织机架 · 用户 · 角色 · 岗位对接](04-organization-and-identity.md)
