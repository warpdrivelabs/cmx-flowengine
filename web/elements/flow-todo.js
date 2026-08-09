/**
 * <flow-todo> —— 可嵌待办中心（S5）。三区：待办分类 / 列表 / 轨迹意见。
 *
 * 用法：
 *   <script type="module" src="https://flow.example/web/index.js"></script>
 *   <flow-todo api-base="https://flow.example" token="eyJ..." tenant="t1"></flow-todo>
 *   el.addEventListener('flow-open-task', e => openMyTaskUI(e.detail.workNode))  // 宿主自决怎么开
 *
 * 属性：api-base（请求前缀）· token（Bearer）· tenant（X-Tenant）· user（办理人身份）·
 *       credentials（默认 token 时 omit，否则 same-origin）。
 * 事件：flow-open-task（点「办理/查看」派发，detail={workNode,initialContext}，替代门户开 Tab）。
 */
import { FlowElementBase, defineFlowElement } from './base-element.js'
import * as todoCore from '../core/todo-center.js'

export class FlowTodo extends FlowElementBase {
  static coreModule = todoCore
  static regions = ['explorer', 'content', 'property']
  static eventName = 'flow'
  static observedAttributes = ['api-base', 'token', 'tenant', 'user', 'credentials']
}

defineFlowElement('flow-todo', FlowTodo)
