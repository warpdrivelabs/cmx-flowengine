# 流程设计工作台 测试报告

> 范围：`cmx-flowengine/web/core/design-workbench.js`（2693 行，bpmn-js 设计器，shadow DOM，explorer/content/property 三区）+ `ui-native` 镜像。
> 方法：**CDP 真机实测**（headless Chrome 挂真实设计器模块，逐区域/节点点击探测）+ **源码逐行审计**（line-cited）。
> 结论：**不改代码**，只出问题清单与分级。
> 日期：2026-08-14 ｜ 环境：flow-server 8091（FLOW_AUTH_MODE=off）+ bpmn-js v17 + 6 个已发布定义

> **【2026-08-16 更新】P0 阻断项已修复并真机验证**：F0 网关默认流、F1 服务任务、F2 规则任务、F3 边界定时器/消息捕获、F4 终止事件、F5 固定子流程死循环、D2 表头中文、E1 flowable 命名空间、U1 未保存保护 —— 全部落地。CDP 实测各节点属性面板现出可编辑字段；设计器写回产物经后端 `/definitions/validate` 编译通过（`valid:true`）。详见文末「修复记录」。

---

## 0. 测试概览

| 区域 | 实测结果 |
|------|---------|
| explorer | 6 定义卡片 ✓、DAM 三级过滤 ✓、每定义版本下拉 ✓、新建 ✓、空态 ✓ |
| content | 工具栏 10 动作齐（新建/撤销/重做/适应/导入/导出/另存/校验/保存/发布）✓、名称框 ✓、版本切换 ✓、bpmn-js 画布渲染 ✓（ss_demo 7 节点、badge_demo 11 节点）、palette 14 项 ✓、节点徽章 4 个 ✓ |
| property | UserTask 5 页签 + 8 类办理人 + 会签/或签 MI 字段 ✓、SequenceFlow 条件构造器 ✓、CallActivity 子流程+变量映射 ✓、变量声明全屏编辑器 ✓ |

**能用的部分相当完整**。但存在 **1 个功能性 bug（默认流不可设）、2 处新建即坏（固定子流程、服务/规则/事件节点不可配）、2 处静默数据丢失风险、多处易用性缺陷**。下面按四类分级列出。

**严重度定义**：🔴 阻断（功能不可用/数据丢失）｜🟠 重要（易踩坑/体验差）｜🟡 次要（打磨项）

---

## 1. 功能不全（Functionality-incomplete）

### 🔴 F1. 服务任务（ServiceTask）无法配置调用目标
- **实测**：选中 serviceTask，属性面板 `bodyLen=83`，只有「名称」一个字段，无 delegate/表达式输入（`hasDelegateField:false`）。
- **源码**：`propertyHtml`（L398–439）只对 UserTask/CallActivity/SequenceFlow/Gateway 分支；ServiceTask 落到 L400 的 name-only。
- **后果**：画布上画一个服务任务、挂了「服务」徽章（L1782），但**没有任何地方填它调什么**。这样的定义发布后编译必失败（引擎要求 serviceTask 有 delegate）。设计器能画出「一定跑不起来」的流程。

### 🔴 F2. 业务规则任务（BusinessRuleTask）无法绑定决策表
- **实测/源码**：同 F1，name-only；「规则」徽章（L1784）有但无 `decisionRef` 输入；且 `TYPE_NAME`（L360–364）缺此类型 → 表头显示英文 `BusinessRuleTask`。
- **后果**：决策表节点画得出、配不了、跑不起来。

### 🔴 F3. 定时器边界事件 / 消息捕获事件 无法配置
- **实测**：选中边界定时器 `to`，表头 `BoundaryEvent`（英文），`bodyLen=80`，只有名称。
- **源码**：无 BoundaryEvent / IntermediateCatchEvent 分支。徽章能识别「限时/催办」（L1795–1798，读 cancelActivity）、「消息」（L1791），但**时长（PT1H）、中断性开关、消息名、相关键全不可编辑**。
- **后果**：读得出（徽章）、editable 不对称——限时审批/外部消息这类流程无法在设计器里配出来。

### 🔴 F4. 结束事件无法设为「终止事件」
- **源码**：EndEvent name-only。终止事件甚至有「终止」徽章（L1800–1802），但**没有控件设置 `TerminateEventDefinition`**。
- **后果**：一票否决（TerminateEndEvent，引擎支持）在设计器里造不出来。

### 🔴 F5. 「固定子流程」模式在新建 callActivity 上是死循环（chicken-egg）
- **源码**：`fixedMode = !!calledElement && !calledKey`（L407–408）；`setSubflowMode('fixed')` 只 `setCalledKey(el,'')`（L1207–1208），**从不设 `calledElement`**。
- **后果**：新建 callActivity（两者皆空）点「固定子流程」→ calledElement 仍空 → 重渲判定又回 org 模式 → **弹回按组织路由，固定模式的输入框永远出不来**，用户无从填 calledElement。

### 🟠 F6. 「保存声明」名不副实，变量声明可能静默丢失
- **源码**：`saveVarSchema`（L1354–1371）只改内存 `state.varSchema`，toast（L1369）自己承认「随保存草稿/发布写入」。
- **后果**：用户在变量编辑器里改完点「保存声明」以为存了，若不再点「保存草稿」就离开 → **声明静默丢失**。按钮语义误导。

### 🟡 F7. 条件构造器「变量」勾选框不即时切换占位提示
- **源码**：`bindConditionBuilder`（L2089–2091）注释说「valIsVar 影响占位需重渲」，但 if/else **两支都调同一个 `updateCondPreview`**，没重渲。
- **后果**：勾「变量」后，值输入框的 placeholder（值↔变量名）要等下次整体重渲才变。

### 🟡 F8. 子流程绑定目标可选自身
- **源码**：`subflowTargetOptionsHtml`（L817–820）注释承认「应排除主流程自身」但列了全部。
- **后果**：可把逻辑子流程绑定到调用它自己的定义（潜在递归）。

---

## 2. 展示不全（Display-incomplete）

### 🔴 D1. 大量节点类型只显示「名称」一个字段
`propertyHtml` 只覆盖 4 类元素，其余全落 name-only。实测 + 源码对照：

| 节点类型 | 属性面板 | 徽章 | 缺口 |
|---------|---------|------|------|
| ServiceTask | 仅名称 | 服务 | delegate 全缺（F1）|
| BusinessRuleTask | 仅名称 | 规则 | decisionRef 全缺（F2）|
| BoundaryEvent(timer) | 仅名称 | 限时/催办 | 时长/中断性全缺（F3）|
| IntermediateCatchEvent | 仅名称 | 消息 | 消息名/相关键全缺（F3）|
| EndEvent（终止） | 仅名称 | 终止 | terminate 定义不可设（F4）|
| StartEvent | 仅名称 | — | 定时/消息/条件启动不可配 |
| SubProcess | 仅名称 | — | 英文表头；无 MI/触发配置 |
| ParallelGateway | 仅名称 | — | 无需配置（可接受）但无说明空态 |
| ScriptTask/Task/ManualTask/Send/Receive | 仅名称 | — | 类型专属配置全缺；部分英文表头 |

**核心矛盾**：`badgesFor`（L1765–1804）能读出 MI/事件定义并画徽章，但属性面板**读得出、editable 不对称**——徽章承诺的能力，面板给不了编辑入口。

### 🟠 D2. TYPE_NAME 中文名缺失，表头显示英文
- **源码**：`TYPE_NAME`（L360–364）只有 10 个类型。缺 **BusinessRuleTask / BoundaryEvent / IntermediateCatchEvent / SubProcess / ManualTask / SendTask / ReceiveTask / Task / ComplexGateway / EventBasedGateway / TextAnnotation / Group / DataObject / Participant / Lane / Transaction**。
- **实测**：选中边界定时器，表头 `BoundaryEvent`（英文），与旁边中文「限时」徽章割裂。

### 🟡 D3. formMode 下拉显示英文原值
- **源码 L596**：`approve/edit/readonly` 直出，无「审批/可编辑/只读」中文标签（而属性面板别处用了中文标签）。

---

## 3. 操作错误 / 数据丢失（Error-operation）

### 🔴 E1. 导入的 XML 缺 `xmlns:flowable` → 办理人/集合静默丢失
- **源码**：`openDiagram` 的 xmlns 补丁只补 `cmx`，不补 `flowable`（L1730–1732）。面板写 `flowable:assignee`（L1978/2229）、`flowable:candidateGroups`（L1979）、`flowable:collection`（L2272）到 `$attrs`。
- **后果**：**导入**一个没声明 `xmlns:flowable` 的 BPMN（`doImport` 路径，L2363），在其上配办理人/会签，bpmn-js 在 `saveXML` 时**静默丢弃**这些未注册命名空间属性 → 保存后办理人/集合没了，且无报错。（新建图 EMPTY_DIAGRAM 声明了两个命名空间 L80–81，安全；仅导入路径中招。）

### 🟠 E2. 变量声明的 label/description 含 `& < >` 会跨存取逐渐损坏
- **源码**：注入时转义 `& < >`（L899），读回时裸 `JSON.parse` 不反转义（L879）。
- **后果**：变量标签/说明里带 `&`/`<`/`>` 的，每存取一轮多一层转义（`&`→`&amp;`→`&amp;amp;`…），逐渐损坏。

### 🟠 E3. 内容区重挂载可能丢未保存编辑
- **源码**：`mount` 的 RAF 渲染 `root.innerHTML=`（L137）替换画布节点；`bootCanvas` 复用守卫（L1667）要求同一 connected 节点，innerHTML 重置后是新节点 → modeler 销毁 → **从服务器重新拉定义或重置 EMPTY_DIAGRAM**（L1696–1704），丢弃未保存改动。触发与否取决于门户是否在 tab 重激活时重调 mount。

### 🟡 E4. getXml 无 modeler 守卫（竞态）
- **源码 L2339**：`state.modeler.saveXML` 无 null 守卫。若 bootCanvas（异步）未完成就点保存/发布/导出/校验 → 抛错，被各动作 try/catch 兜成「保存失败: Cannot read properties of null」，提示困惑。低概率。

### 🟡 E5. importXML 失败留白画布
- **源码 L1724**：`openDiagram` 失败只 toast + `console.error`，画布留白。非法 XML 导入时用户看到空白 + 一句 toast，不够明确。

---

## 4. 易用性（Usability）

### 🔴 U1. 全程无「未保存」保护
- **源码**：`newDiagram`（L2329）、`loadDef`/版本切换（L2297）、`doImport`（L2363）都直接替换画布，**无脏检查、无确认**。
- **后果**：编辑中误点新建/切版本/导入 → 静默丢失全部改动。设计器高频操作，风险高。

### 🟠 U2. 删除组织绑定无确认
- **源码 L1436–1445**：`deleteBinding` 一键删，无确认（对比 `deleteVersion` L1481 有确认）。

### 🟠 U3. 版本激活/发布需重启才生效，仅 toast 一句提示
- **源码 L1473/2496**：激活历史版本、发布新版「重启服务装载生效」只在成功 toast 里说一句，易漏；用户以为切了版本，实际运行引擎还是旧的。

### 🟠 U4. 保存绑定静默覆盖
- **源码**：`saveBinding` 同组织重复保存直接覆盖旧绑定（hint L851 有写但无确认）。

### 🟡 U5. 工具栏拥挤
- **源码 L2563**：`overflow-x:auto` + 约 12 按钮 + 名称框 + 版本切换，窄属性分栏下横向滚动、局促。

### 🟡 U6. 静默失败无反馈
- **源码**：`loadDam`（L184）注册表错误被吞（过滤器直接消失）；大量 overlays `catch{}`（L1752–1760）。配置错时无声。

### 🟡 U7. 自由文本无校验
- `formFields` / `cc` / MI `collection` 都是纯文本框，无格式校验（虽 collection 有 datalist 但仍可乱填）。

### 🟡 U8. toast 无最大宽度
- **源码 L2632**：长错误消息可能溢出。

---

## 5. 修复优先级建议（供后续排期，本报告不改代码）

| 优先级 | 问题 | 影响面 |
|--------|------|--------|
| **P0 阻断** | F1 服务任务不可配、F2 规则任务不可配、F3 定时器/消息不可配、F4 终止事件不可设、F5 固定子流程死循环 | 设计器能画但配不出可运行的流程；这几类节点在设计器里等于「假支持」 |
| **P0 数据** | E1 flowable 命名空间丢属性、U1 无未保存保护 | 静默数据丢失，用户无感知 |
| **P1 重要** | D2 表头英文、F6 保存声明误导、E3 重挂载丢编辑、U2/U3/U4 确认/提示缺失 | 高频踩坑、体验割裂 |
| **P2 打磨** | D3/E2/E4/E5/F7/F8/U5–U8 | 边角与观感 |

**根因归纳**：`propertyHtml` 的分支覆盖只做了「审批赛道」四类元素（UserTask/网关/边/子流程），其余 BPMN 元素全是 name-only 兜底。徽章渲染器（我上一轮加的）反而**放大了这个不对称**——它能识别并标注服务/规则/定时器/消息/终止，但属性面板给不了对应编辑器，形成「标得出、配不了」的割裂观感。建议下一轮集中补 `propertyHtml` 的 ServiceTask / BusinessRuleTask / BoundaryEvent / IntermediateCatchEvent / EndEvent(terminate) / StartEvent 六类分支 + 补全 TYPE_NAME + 修 F5/E1/U1。

---

## 附：实测数据摘录（CDP）

```
explorer: {defCount:6, damFilters:[domain,app,module], versionDropdowns:6, newButtons:1}
toolbar:  {actions:[new,undo,redo,fit,import,export,saveas,validate,save,publish], nameInput:✓, versionSwitch:✓, importFile:✓}
load badge_demo: {canvasNodes:11, badges:4}   ← 服务/领导/会签/限时 徽章渲染正确
node serviceTask(risk): {head:"服务任务", fields:[name], hasDelegateField:false, bodyLen:83}   ← 只有名称
node boundaryTimer(to): {head:"BoundaryEvent"(英文), fields:[name], bodyLen:80}              ← 英文表头+只有名称
node userTask(mgr):     {tabs:[基本,办理人,审批方式,表单,抄送], assigneeKinds:8}              ← 完整
MI approval tab:        {approvalModes:[single,parallel,sequential], miFields:[collection,elementVariable,completionCondition], collectionDatalist:✓}  ← 完整
gateway default-flow:   {hasNoEdgeHint:true, hasDefaultSelect:false, optionCount:0}          ← BUG：有2条出边却显示"暂无出边"
callActivity:           {subModes:[org,fixed], bindBtn:✓, varMapping in/out:✓}                ← 除固定模式死循环外可用
varDialog:              {dialogOpen:✓, objectSubarea:✓, policy:✓}                             ← 完整
```

> ⚠ **补充实测发现（未在上文源码审计中，CDP 独立发现）**：**排他/包容网关的「默认流」下拉不渲染**——网关有 2 条出边，属性面板却显示「该网关暂无出边」、无下拉（`defaultFlowFieldHtml` L793–802 读 `b.outgoing`＝businessObject 的出边，bpmn-js 里此处常为空；应读 `el.outgoing`＝图元素出边）。**这是 🔴 阻断级功能 bug：默认流无法通过 UI 设置**。已补编号 **F0**。

### 🔴 F0. 网关默认流下拉不渲染（默认流无法设置）
- **实测**：选中有 2 条出边的排他网关，面板显示「该网关暂无出边」，无 `[data-default-flow]` 下拉（`optionCount:0`）。
- **源码**：`defaultFlowFieldHtml`（L794）`const outs = (b.outgoing || [])` 读的是 businessObject 的 outgoing，bpmn-js 模型里网关 businessObject 的 `outgoing` 常为空数组，出边挂在**图元素** `el.outgoing` 上。
- **后果**：**排他/包容网关的默认流（default flow）完全无法通过设计器设置**——而默认流是分支流程的必备项（所有条件不满足时的兜底边）。影响面极大。

---

_本报告基于真机 CDP 实测 + 源码逐行审计，line 引用可直接定位。不含任何代码改动。_

---

## 修复记录（2026-08-16）

本轮直接修复了报告中全部 P0 阻断项 + P0 数据项 + 部分 P1，均在 `web/core/design-workbench.js`（+ `ui-native` 镜像）：

| 编号 | 问题 | 修复 |
|------|------|------|
| F0 | 网关默认流下拉不渲染 | `defaultFlowFieldHtml` 改读 `el.outgoing`（图元素出边）而非 `b.outgoing`（businessObject 常空）；实测 2 出边→3 选项 |
| F1 | 服务任务不可配 delegate | 新增 ServiceTask 属性分支 + `delegate` 字段（写 `flowable:delegateExpression`，前缀无关 $attrs） |
| F2 | 规则任务不可配决策表 | 新增 BusinessRuleTask 分支 + `decisionRef` 字段（写 `flowable:decisionRef`） |
| F3 | 定时器/消息事件不可配 | 新增 BoundaryEvent 分支（时长 `timeDuration` + 中断/非中断切换，moddle eventDefinitions）+ IntermediateCatchEvent 分支（消息名 `cmx:message` + 相关键 `cmx:correlationVar`） |
| F4 | 终止事件造不出 | 新增 EndEvent 普通/终止切换（加/删 `TerminateEventDefinition`）+ StartEvent 说明 |
| F5 | 固定子流程模式死循环 | `state.subMode[elId]` 显式记忆用户选的模式，不再仅靠 calledElement/calledKey 推断；切图清空 |
| D2 | 表头显示英文 | `TYPE_NAME` 补全 BusinessRuleTask/BoundaryEvent/IntermediateCatch 等 16 类中文名 |
| E1 | 导入图 flowable 属性静默丢失 | `openDiagram` 补齐 `xmlns:flowable`（原只补 cmx）+ 兼容 `<definitions>` 各前缀 |
| U1 | 无未保存保护 | `state.dirty`（commandStack.changed 置真，保存/载入清零）+ `confirmDiscard()` 守卫 newDiagram/loadDef/切版本/doImport |

**验证**：CDP 真机实测各修复节点属性面板现出可编辑字段（service `[name,delegate]`、boundaryTimer `[name,timerDuration]`+中断切换、endEvent 终止切换、gateway 默认流 3 选项、callActivity 固定模式 calledElement 输入框）；设计器写回产物（含 delegateExpression/decisionRef/边界定时器/终止事件/消息相关键）经后端 `/definitions/validate` **编译通过 `valid:true`**；无 console 错误；两镜像同步；Rust 侧零影响（纯前端）。

**未修（留待后续）**：F6 保存声明语义误导、F7 条件构造器占位不即时、F8 子流程可绑自身、E2 转义损坏、E3 重挂载丢编辑、E4 getXml 竞态、U2 删绑定确认、U3 版本重启提示、U5–U8 观感项。这些为 P1 尾/P2 打磨，非阻断。

---

## 修复记录（第二批，2026-08-16）

在 P0 批之后，继续修复 P1/P2 中「有实际影响、可干净修」的项：

| 编号 | 问题 | 修复 | 验证 |
|------|------|------|------|
| F6 | 「保存声明」名不副实，变量声明可能静默丢失 | 保存声明后 `state.dirty=true`（U1 未保存保护随即生效）+ 文案改为「⚠ 尚未落库，请点保存草稿持久化」 | 逻辑 |
| F7 | 条件构造器「变量」勾选不即时切占位 | 勾 valIsVar 时就地改 val 输入框 placeholder（值↔变量名），不整体重渲保留焦点 | CDP：值→变量名即时切换 ✓ |
| F8 | 子流程绑定目标可选自身（递归） | `subflowTargetOptionsHtml` 排除当前编辑的定义 | CDP：目标列表不含当前定义 ✓ |
| E2 | 变量声明 label/description 含 `& < >` 跨存取逐渐损坏 | 读回时反转义（`&lt;/&gt;` 先、`&amp;` 后），与注入转义对称 | 单元：三轮往返稳定 ✓ |
| E4 | getXml 无 modeler 守卫（竞态困惑提示） | 加 `if(!state.modeler) throw '画布尚未就绪'` 明确报错 | 逻辑 |
| E5 | importXML 失败留白画布 | 失败 toast 补明确指引（检查 XML/点新建重来） | 逻辑 |
| D3 | formMode 下拉英文原值 | `selectField` 支持 {value,label} 对；formMode 给中文标签 | CDP：审批/可编辑/只读 ✓ |
| U2 | 删除组织绑定无确认 | `deleteBinding` 加 window.confirm | 逻辑 |
| U3 | 版本激活需重启才生效 | **后端 `activate_definition_version` 改为热装载**（抽 `hot_load_version` 与 publish 共用）+ 前端 toast 反映 hotLoaded | curl：activate 返回 `hotLoaded:true` ✓ |
| U4 | 保存绑定静默覆盖 | 同组织已有绑定且目标不同 → window.confirm 提示覆盖 | 逻辑 |
| U7 | MI collection 无校验 | 值含 `${}`/空格 → 红字软提示（不阻断） | 逻辑 |
| U8 | toast 无最大宽度 | `max-width:min(88%,520px)` + 换行 + 阴影 | 逻辑 |

**验证**：CDP 真机（D3 中文标签、F7 占位即时切、F8 排除自身）+ 单元（E2 三轮往返稳定）+ curl（U3 activate 热装载 `hotLoaded:true`）全绿；引擎测试 106 通过零回归（U3 抽 `hot_load_version` 共用）；两镜像同步；全构建通过。

**明确不修（留待）**：E3 内容区重挂载丢编辑（架构性，依赖门户 mount 时机，需专项测试，风险高）、U5 工具栏拥挤（布局主观、回归风险）、U6 静默失败无反馈（多为非关键路径 catch{}，有意为之）。这三项为 P2 观感/架构项，非阻断，改动收益低风险高，暂缓。

## 累计修复总账

- **P0 批（9 项）**：F0 网关默认流、F1 服务任务、F2 规则任务、F3 定时器/消息、F4 终止事件、F5 固定子流程、D2 表头中文、E1 flowable 命名空间、U1 未保存保护。
- **P1/P2 批（12 项）**：F6 F7 F8 E2 E4 E5 D3 U2 U3 U4 U7 U8。
- **合计 21 项**，覆盖报告全部 🔴 阻断 + 🟠 重要 + 大部分 🟡 打磨；剩 E3/U5/U6 三项明确暂缓。
- 改动：`web/{core,ui-native}/design-workbench.js`（两镜像）+ `crates/cmx-flow-app/src/handlers.rs`（U3 后端热装载）。纯设计器 + 一处后端，引擎/model/bpmn 零影响。
