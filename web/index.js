/**
 * cmx-flow 可嵌 Web Component 桶入口（S5）。
 *
 * 第三方系统一行引入即注册全部三个流程组件（框架无关，可嵌 React/Vue/Angular/原生）：
 *   <script type="module" src="https://flow.example/web/index.js"></script>
 *   <flow-todo api-base="https://flow.example" token="…"></flow-todo>
 *   <flow-designer api-base="https://flow.example" bpmn-base="…/vendor/bpmn-js"></flow-designer>
 *   <flow-task-form api-base="…" task-id="…" instance-id="…" form-key="…"></flow-task-form>
 *
 * 三组件对应流程微服务的三类前端能力（对齐 headless API §6）：
 *   flow-designer  → 设计态（GET/POST /definitions…）
 *   flow-todo      → 待办态（GET /tasks/my、/todos/*、SSE /events）
 *   flow-task-form → 运行态办理（POST /tasks/{id}/complete、/instances…）
 *
 * 每个 element 模块在 import 时自注册（customElements.define，幂等）。也可按需只引单个：
 *   import 'https://flow.example/web/elements/flow-todo.js'
 *
 * headless 姿态（三方全自研前端）根本不需要本文件——直接调 /api/flow/v1/* + SSE 即可。
 */
export { FlowTodo } from './elements/flow-todo.js'
export { FlowDesigner } from './elements/flow-designer.js'
export { FlowTaskForm } from './elements/flow-task-form.js'
export { FlowElementBase, defineFlowElement } from './elements/base-element.js'
