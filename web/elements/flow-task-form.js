/**
 * <flow-task-form> —— 可嵌任务表单/审批控制台（S5）。两区：业务单据 content / 审批轨迹 property。
 *
 * 与 todo/designer 不同：本元素**由任务上下文驱动**（不自取列表），故属性直接对应核 propsOf 的字段，
 * 经 collectProps() 收成 ctx.props 交给 mount。宿主（自研待办 UI）打开某任务时放此标签并配 task-id 等。
 *
 * 用法：
 *   <flow-task-form api-base="https://flow.example" token="eyJ..."
 *                   task-id="123" instance-id="456" form-key="pay.review"
 *                   form-mode="approve"></flow-task-form>
 *   el.addEventListener('flow-task-done', e => refreshMyList())   // 办结/发起成功
 *   el.addEventListener('flow-close', e => closeMyPanel())        // 办结后请求收起
 *
 * 属性→props：task-id/instance-id/form-key/form-mode(approve|edit|readonly)/mode(task|start)/
 *   biz-table/biz-id/domain/application/module/file/api-path/definition-key/definition-name/
 *   start-form-key/title/view-only。
 */
import { FlowElementBase, defineFlowElement } from './base-element.js'
import * as taskFormCore from '../core/task-form.js'

export class FlowTaskForm extends FlowElementBase {
  static coreModule = taskFormCore
  static regions = ['content', 'property']
  static eventName = 'flow'
  static observedAttributes = [
    'api-base', 'token', 'tenant', 'user', 'credentials',
    // 任务上下文属性变更 → 重挂（换任务）
    'mode', 'task-id', 'instance-id', 'form-key', 'form-mode', 'view-only',
    'biz-table', 'biz-id', 'domain', 'application', 'module', 'file', 'api-path',
    'definition-key', 'definition-name', 'start-form-key', 'title',
  ]

  /** 属性 → 核 propsOf 读的 props（连字符属性映射成核的驼峰键）。 */
  collectProps() {
    const g = (n) => this.getAttribute(n) || ''
    return {
      mode: g('mode') || 'task',
      formKey: g('form-key'),
      formMode: g('form-mode') || 'approve',
      viewOnly: this.hasAttribute('view-only'),
      taskId: g('task-id'),
      instanceId: g('instance-id'),
      bizTable: g('biz-table'),
      bizId: g('biz-id'),
      domain: g('domain'), application: g('application'), module: g('module'), file: g('file'),
      apiPath: g('api-path'),
      definitionKey: g('definition-key'), definitionName: g('definition-name'), startFormKey: g('start-form-key'),
      title: g('title') || '任务表单',
    }
  }
}

defineFlowElement('flow-task-form', FlowTaskForm)
