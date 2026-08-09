/**
 * <flow-designer> —— 可嵌流程设计器（S5）。三区：定义列表 / bpmn 画布 / 节点属性。
 *
 * 用法：
 *   <flow-designer api-base="https://flow.example" token="eyJ..."
 *                  bpmn-base="https://flow.example/web/vendor/bpmn-js"></flow-designer>
 *
 * 属性：api-base · token · tenant · user · credentials · bpmn-base（bpmn-js 资产根，
 *       缺省用核默认门户 vendor 路径；第三方部署须指向自己托管的 bpmn-js dist）。
 *
 * 注意：设计器画布依赖 bpmn-js UMD + 字体/CSS 资产。第三方集成需自行托管 bpmn-js dist 并配
 *       bpmn-base 指向它（本组件不打包 bpmn-js，保持零重依赖，与 cmx-mega-sheet 同策略）。
 */
import { FlowElementBase, defineFlowElement } from './base-element.js'
import * as designerCore from '../core/design-workbench.js'

export class FlowDesigner extends FlowElementBase {
  static coreModule = designerCore
  static regions = ['explorer', 'content', 'property']
  static eventName = 'flow-designer'
  static observedAttributes = ['api-base', 'token', 'tenant', 'user', 'credentials', 'bpmn-base']
}

defineFlowElement('flow-designer', FlowDesigner)
