/**
 * FlowElementBase —— 三个可嵌流程 Web Component 的共用基类（S5）。
 *
 * 把 S4 抽好核的 native-page 模块（core/*.js，导出 { configure, mount, default.views }）包成
 * 框架无关 custom element：第三方 `<script type=module>` 引入 + 放标签 + 配 `api-base`/`token`
 * 等属性即用，可嵌进 React/Vue/Angular/原生任意框架。对标 cmx-mega-sheet 的 <cmx-megasheet>。
 *
 * 三件事：
 *  ① 属性 → configure()：api-base/token/tenant/user/bpmn-base 映射成核的 CFG 覆盖
 *     （apiBase 前缀请求、authHeaders 注入 Bearer、getUser 定身份、bpmnBase 指资产）。
 *  ② 区域 host → mount()：核的 mount(ctx,view) 读 ctx.host.renderRoot 渲染。每个区域造一个
 *     真实 shadow 内 <div>，令 div.renderRoot=div 自身（hostRoot 取它）、div.isConnected 天然为真
 *     （refreshView 遍历 state.hosts 靠它判活），把 (host,props) 交给 mount。
 *  ③ 塌缩回调 → CustomEvent：S4 已把门户特有的开 Tab/关 Tab/办结广播链塌缩成 CFG 回调；
 *     这里把它们再派发成 DOM 事件（flow-open-task / flow-close / flow-task-done），
 *     宿主监听即可自决怎么开/怎么收，headless 里则根本不需要。
 *
 * 纯 vanilla、shadow DOM、零框架依赖（与 cmx-mega-sheet 同硬约束）。
 */

// 无 DOM 环境（node/SSR 导入 barrel）兜底基类：`extends HTMLElement` 在无 HTMLElement 时定义即抛，
// 会让整个 index.js 无法 import。用运行时可用的 HTMLElement，缺失退回空类（元素不注册）。
const ElementBase =
  typeof HTMLElement !== 'undefined' ? HTMLElement : /** @type {any} */ (class {})

/** 组件外壳基础样式（shadow 内，三区默认横向布局；单区元素只显一区自然铺满）。 */
const BASE_CSS = `
:host{display:block;min-height:240px;box-sizing:border-box;
  font-family:-apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC','Microsoft YaHei',sans-serif;}
:host([hidden]){display:none}
.flow-shell{display:flex;width:100%;height:100%;min-height:240px;overflow:hidden}
.flow-region{overflow:auto;min-width:0;min-height:0;box-sizing:border-box}
/* 三区（explorer|content|property）默认宽度配比；单区时该区 flex:1 铺满 */
.flow-region-explorer{flex:0 0 240px;border-right:1px solid var(--flow-line,#e2e6ec)}
.flow-region-content{flex:1 1 auto}
.flow-region-property{flex:0 0 320px;border-left:1px solid var(--flow-line,#e2e6ec)}
.flow-shell[data-single] .flow-region{flex:1 1 auto;border:0}
`

export class FlowElementBase extends ElementBase {
  // 子类须提供：
  //   static coreModule  —— { configure, mount, default:{views,defaultView} }（vendor 的核）
  //   static regions     —— 要渲染的区域数组，如 ['explorer','content','property'] 或 ['content','property']
  //   static eventName    —— (可选) 该元素特有事件前缀，缺省 'flow'
  //   this.collectProps() —— (可选) 从属性收集 ctx.props（task-form 用 task-id 等）

  _built = false
  /** @type {Array<HTMLElement>} 各区域 host（真实 shadow 内 div，renderRoot 指自身）。 */
  _hosts = []

  connectedCallback() {
    if (this._built) return
    this._built = true
    this._applyConfigure()
    this._buildShadow()
    this._mountRegions()
  }

  disconnectedCallback() {
    // 核以模块级 state.hosts 弱引用式管理，refreshView 会自动剔除 isConnected=false 的 host；
    // 这里把 host 从 DOM 摘掉即可（下次 refreshView 清理）。不强制销毁，允许重连复用。
    for (const h of this._hosts) { try { h.remove() } catch { /* ignore */ } }
    this._hosts = []
    this._built = false
  }

  /** 属性变更（api-base/token/...）→ 重新 configure + 重挂。观察见子类 observedAttributes。 */
  attributeChangedCallback(name, oldVal, newVal) {
    if (!this._built || oldVal === newVal) return
    this._applyConfigure()
    // 重挂各区域（核 mount 幂等重渲染；重配 apiBase/token 后需要新请求）
    this._remountRegions()
  }

  // ── ① 属性 → configure() ────────────────────────────────
  _applyConfigure() {
    const core = /** @type {any} */ (this.constructor).coreModule
    if (!core || typeof core.configure !== 'function') return
    const apiBase = this.getAttribute('api-base') || ''
    const token = this.getAttribute('token') || ''
    const tenant = this.getAttribute('tenant') || ''
    const user = this.getAttribute('user') || ''
    const bpmnBase = this.getAttribute('bpmn-base') || ''
    const creds = this.getAttribute('credentials') || (token ? 'omit' : 'same-origin')

    const cfg = {
      apiBase,
      fetchInit: { credentials: creds },
      authHeaders: () => {
        const h = {}
        if (token) h.Authorization = 'Bearer ' + token
        if (tenant) h['X-Tenant'] = tenant
        return h
      },
    }
    // getUser：显式 user 属性优先；否则不覆盖（核默认 localStorage 兜底）
    if (user) cfg.getUser = () => user
    // bpmnBase：仅设计器核有此键；给了才覆盖（否则用核默认门户 vendor 路径）
    if (bpmnBase) cfg.bpmnBase = bpmnBase

    // 塌缩回调 → CustomEvent（③）。核只在设了对应回调时才走组件路径，否则回退门户默认链。
    const ev = /** @type {any} */ (this.constructor).eventName || 'flow'
    cfg.onOpenTask = (detail) => this._emit(ev + '-open-task', detail)
    cfg.onTaskDone = (detail) => this._emit(ev + '-task-done', detail)
    cfg.onClose = (nodeId) => this._emit(ev + '-close', { nodeId })

    core.configure(cfg)
  }

  _emit(name, detail) {
    try { this.dispatchEvent(new CustomEvent(name, { detail: detail || {}, bubbles: true, composed: true })) }
    catch { /* ignore */ }
  }

  // ── ② 区域 host → mount() ───────────────────────────────
  _buildShadow() {
    const shadow = this.shadowRoot || this.attachShadow({ mode: 'open' })
    const style = document.createElement('style')
    style.textContent = BASE_CSS
    shadow.appendChild(style)
    const shell = document.createElement('div')
    shell.className = 'flow-shell'
    const regions = /** @type {any} */ (this.constructor).regions || ['content']
    if (regions.length === 1) shell.setAttribute('data-single', '')
    for (const view of regions) {
      const host = document.createElement('div')
      host.className = 'flow-region flow-region-' + view
      // hostRoot(host) = host.renderRoot；令其指向自身 → 核直接把内容渲进这个 div。
      host.renderRoot = host
      shell.appendChild(host)
      host.__flowRegion = view
      this._hosts.push(host)
    }
    shadow.appendChild(shell)
    this._shell = shell
  }

  _mountRegions() {
    const core = /** @type {any} */ (this.constructor).coreModule
    if (!core || typeof core.mount !== 'function') return
    const props = (typeof this.collectProps === 'function') ? this.collectProps() : undefined
    for (const host of this._hosts) {
      const ctx = props !== undefined ? { host, props } : { host }
      try { core.mount(ctx, host.__flowRegion) } catch (e) { console.error('[flow-element] mount 失败', host.__flowRegion, e) }
    }
  }

  _remountRegions() {
    // 属性变更后重渲染：核的 mount 幂等（重置 innerHTML + rebind），重挂即可拿新配置发请求。
    this._mountRegions()
  }

  /** 逃生舱：拿到 vendor 核模块（configure/mount + 其它公开函数），宿主可做高级操作。 */
  getCore() { return /** @type {any} */ (this.constructor).coreModule }
}

/** 安全注册 custom element（重复定义/无 customElements 环境跳过）。 */
export function defineFlowElement(tag, cls) {
  if (typeof customElements !== 'undefined' && !customElements.get(tag)) {
    customElements.define(tag, cls)
  }
}
