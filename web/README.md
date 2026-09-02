# ⚠️ 本目录是**发布产物**——禁止直接修改！

> 真源：`cmx-container/assets/flow/web/`（`ui-native/` + `ui-html/`，工作区唯一真源）。
> 同步：改真源后执行工作区根 `./scripts/publish-assets.sh flow`，本目录 `ui-native/`、`ui-html/`
> **整目录替换**为拷贝——在此手改的任何内容下次发布即被覆盖（可用工作区根
> `python scripts/check-asset-ownership.py` 自检是否误改了产物）。
>
> 技术债 015（2026-09-02 批次 7）清理记录：`core/`（无消费方孤儿、与 ui-native 有意分叉）、
> `demo/`、`elements/`、`index.js`（可嵌 Web Component 壳②实验）、`menu-manifest.json`、
> `menu-source/`（死档案，菜单真源在 cmx-container 侧 menu-pages）已删除——历史上均为
> 零运行时消费方（全仓 grep 实证），其 README 声明的真源路径
> `cmx-container/data/native-pages/sources/portal/flow/` 已不存在。
> 壳②可嵌组件如需恢复，从 git 历史取回 `elements/` + `index.js`（独立重立真源）。

---

# 原说明（历史存档，其中的真源/结构声明已过期）

# cmx-flow 可嵌 Web Component（S5）

把流程微服务的三类前端能力封成**框架无关 custom element**，第三方系统 `<script>` 引入 + 放标签即用，可嵌进 React / Vue / Angular / 原生任意框架。对标 `cmx-mega-sheet` 的 `<cmx-megasheet>`（零运行时框架依赖、shadow DOM 隔离）。

这是「前端一芯三壳」的**壳②**：
- 壳① 门户内嵌 —— native-pages 三页跑在 CMXPortalManager（保留现状，见 cmx-container）。
- **壳② 可嵌组件 —— 本目录**（`<flow-designer>` / `<flow-todo>` / `<flow-task-form>`）。
- 壳③ headless —— 三方全自研前端，只用 `/api/flow/v1/*` + SSE + OpenAPI（见 flow-server S3）。

三壳共用**同一个核**（S4 抽好的 native-page 模块，导出 `{ configure, mount }`）。

## 组件

| 标签 | 能力 | 对应 headless API |
|---|---|---|
| `<flow-todo>` | 待办中心（分类/列表/轨迹三区） | `GET /tasks/my`、`/todos/*`、SSE `/events` |
| `<flow-designer>` | 流程设计器（定义列表/bpmn 画布/属性三区） | `GET/POST /definitions…` |
| `<flow-task-form>` | 任务表单 + 审批控制台（单据/审批两区） | `POST /tasks/{id}/complete`、`/instances…` |

## 用法

```html
<script type="module" src="https://flow.example/web/index.js"></script>

<!-- 待办中心：配 api-base 指向流程微服务，token 走 Bearer -->
<flow-todo api-base="https://flow.example" token="eyJhbGci..." tenant="t1"></flow-todo>

<!-- 设计器：bpmn-base 指向自托管的 bpmn-js dist -->
<flow-designer api-base="https://flow.example" token="eyJ..."
               bpmn-base="https://flow.example/web/vendor/bpmn-js"></flow-designer>

<!-- 任务表单：由任务上下文驱动（宿主打开某待办时填） -->
<flow-task-form api-base="https://flow.example" token="eyJ..."
                task-id="123" instance-id="456" form-key="pay.review" form-mode="approve">
</flow-task-form>
```

也可按需只引单个组件：`import 'https://flow.example/web/elements/flow-todo.js'`。

## 属性

**通用**（三组件都有）：

| 属性 | 说明 | 缺省 |
|---|---|---|
| `api-base` | 所有 `/api/*` 请求前缀，指向流程微服务 | 空=同源相对路径 |
| `token` | JWT，注入 `Authorization: Bearer <token>` | 无=走同源 cookie |
| `tenant` | 多租户，注入 `X-Tenant` | 无 |
| `user` | 当前办理人身份 | 无=核默认（localStorage 兜底） |
| `credentials` | fetch 凭证模式 | 有 token 时 `omit`，否则 `same-origin` |

**flow-designer 额外**：`bpmn-base`（bpmn-js UMD + 字体/CSS 资产根；第三方须自托管 bpmn-js dist 并指向它）。

**flow-task-form 额外**（任务上下文）：`task-id` `instance-id` `form-key` `form-mode`(approve\|edit\|readonly) `mode`(task\|start) `biz-table` `biz-id` `domain` `application` `module` `file` `api-path` `definition-key` `definition-name` `start-form-key` `title` `view-only`。

## 事件

组件把门户特有的「开 Tab / 关 Tab / 办结广播」链塌缩成 DOM 事件，宿主监听即可自决交互（`bubbles+composed`，可在 document 上听）：

| 事件 | 触发 | detail |
|---|---|---|
| `flow-open-task` | flow-todo 点「办理/查看」 | `{ workNode, initialContext }` |
| `flow-designer-open-task` | flow-designer 内打开任务（如有） | 同上 |
| `flow-task-done` | flow-task-form 办结/发起成功 | `{ taskId, instanceId }` 或 `{ started, definitionKey }` |
| `flow-close` | flow-task-form 办结后请求收起 | `{ nodeId }` |

```js
document.querySelector('flow-todo')
  .addEventListener('flow-open-task', (e) => openMyTaskPanel(e.detail.workNode))
```

## 主题与图标（可选）

组件本体不强依赖 UI5。若宿主要 SAP 主题 + `<ui5-icon>`，自行加载 `cmx-ui5-runtime` dist（预打包 ui5 web components + icon + 主题），现有 `--sap*` token 与 `<ui5-icon>` 零改生效。不想要 UI5 时组件降级为纯 HTML 仍可用（demo 已证）。

## 目录

```
web/
  index.js            桶入口：一行引入注册全部三组件
  elements/
    base-element.js   FlowElementBase：属性→configure、区域 host→mount、回调→CustomEvent
    flow-todo.js      <flow-todo>
    flow-designer.js  <flow-designer>
    flow-task-form.js <flow-task-form>
  core/               ← S4 抽好的 native-page 核（vendor 自 cmx-container，勿手改）
    todo-center.js
    task-form.js
    design-workbench.js
  demo/index.html     集成示例（三组件切换 + 事件日志 + 配 api-base/token）
```

> **core/ 是 vendor 副本**：源在 `cmx-container/data/native-pages/sources/portal/flow/*.js`（S4 已把耦合点抽成 CFG 接缝）。同步 = 直接 `cp` 覆盖（勿在此手改，避免与门户壳漂移；镜像 cmx-megasheet vendor 模式的同一纪律）。
