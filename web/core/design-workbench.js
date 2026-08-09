/**
 * 流程设计工作台 —— native_pages 四区工作台（对标报表设计工作台 portal.rpt.design-workbench）。
 *
 * explorer：流程定义列表（GET /api/flow/definitions）。点选加载到画布。
 * content ：bpmn-js 画布 + 工具条（工具条在 content 内、画布上方）。存草稿/发布/校验/撤销/适应。
 * property：选中节点的属性（办理人/候选角色/子流程/条件），modeling.updateProperties 写回。
 *
 * 后端：cmx-flow-api 的 /api/flow/*（definitions CRUD）。引擎在 web-server 内单例运行。
 *
 * bpmn-js 进 shadow-DOM host 的三处处理（native-page 每区一个 shadow root）：
 *   1. UMD <script> 在 shadow root 不执行 → 注入 document.head（promise 缓存），设 window.BpmnJS；
 *   2. document.head 的 CSS 不跨 shadow → 3 个 bpmn <link> 注入 host.renderRoot 内；
 *   3. container:'#sel' 在 shadow 不解析 → new BpmnJS({container: <renderRoot 里的 DOM 节点>})。
 * auto-layout（内置无坐标 BPMN）：内置分层布局 layoutXml（BFS 分层排坐标+补 BPMNEdge），零远程依赖。
 */

// —— S4 抽核：配置接缝 ——（详见 todo-center.js 同名 CFG 注释）
// 设计器无门户 Tab 链，耦合只两处：apiBase（/api/* 前缀）+ bpmnBase（bpmn-js 静态资产根）。
// 门户壳默认：同源 fetch + 资产走门户 /portal/vendor/bpmn-js（打包进 CMXPortalManager，不访问远程 CDN）。
// 组件壳/headless 壳 configure({ apiBase, authHeaders, bpmnBase })：资产可指向自建 CDN 或组件包内路径。
const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
  bpmnBase: '/portal/vendor/bpmn-js',        // bpmn-js UMD + 字体/CSS 资产根
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

// bpmn-js 本地资产根（每次读 CFG.bpmnBase，故 configure() 覆盖对后续加载即时生效；门户默认
// 打包进 CMXPortalManager 经 /portal/ 静态托管，无远程 CDN）。
function bpmnBase () { return CFG.bpmnBase }

const state = {
  definitions: [],
  selectedKey: null,
  selectedElement: null,
  name: '新建流程',
  loading: false,
  message: '',
  modeler: null,       // 单一模块级 bpmn-js modeler
  canvasHost: null,    // 当前挂 modeler 的 content host
  hosts: new Set(),
  // DAM 三段（域/应用/模块）——对标数据字典定义工作台 explorer 顶部选择器。
  dam: { domains: [], apps: [], modules: [] },   // /api/registry/dam 选项
  fDomain: '', fApp: '', fModule: '',            // explorer 过滤选择
  defDam: { domain: '', application: '', module: '' }, // 当前画布定义的 DAM 归属（随存草稿落库）
  // 版本管理（对标报表版本：每条定义可选版本；publish=新增版本、activate=设当前、delete=删历史）。
  selectedVersion: {},   // { [key]: version|null } 用户为某定义选中的版本（覆盖服务端 active）
  shownVersion: null,    // 当前画布加载的是哪个版本（null=草稿/当前）
  versionDialog: null,   // { key } 打开版本管理对话框时置，null=关闭
  versionError: '',      // 对话框内错误提示
  // 子流程组织路由（对标 M5.2：callActivity 写逻辑 key，运行期按组织解析成具体子流程）。
  orgs: [],              // 组织树扁平表（/api/flow/orgs），懒加载一次
  bindingDialog: null,   // { calledKey } 打开组织绑定对话框时置
  bindings: [],          // 当前 calledKey 的组织绑定列表
  bindingError: '',      // 绑定对话框内错误
}

const EMPTY_DIAGRAM = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"
  xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI"
  xmlns:dc="http://www.omg.org/spec/DD/20100524/DC"
  xmlns:flowable="http://flowable.org/bpmn"
  xmlns:cmx="http://cmx/flow"
  id="Definitions_1" targetNamespace="http://cmx/flow">
  <bpmn:process id="new_process" name="新建流程" isExecutable="true">
    <bpmn:startEvent id="start" name="开始"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="D1"><bpmndi:BPMNPlane id="P1" bpmnElement="new_process">
    <bpmndi:BPMNShape id="start_di" bpmnElement="start">
      <dc:Bounds x="180" y="160" width="36" height="36"/>
    </bpmndi:BPMNShape>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`

const esc = (s) => String(s ?? '')
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  .replace(/"/g, '&quot;').replace(/'/g, '&#39;')
const enc = encodeURIComponent

async function apiJson (url, options = {}) {
  // S4：apiBase 前缀（门户空串=同源）+ CFG.authHeaders/fetchInit。
  const full = (CFG.apiBase && url.charAt(0) === '/') ? CFG.apiBase + url : url
  const res = await fetch(full, {
    ...CFG.fetchInit,
    ...options,
    headers: { Accept: 'application/json', ...CFG.authHeaders(), ...(options.headers || {}) },
  })
  let j = null
  try { j = await res.json() } catch {}
  if (!res.ok || (j && typeof j.code === 'number' && j.code !== 0)) {
    throw new Error((j && (j.msg || j.error)) || `HTTP ${res.status}`)
  }
  return j && typeof j === 'object' && 'data' in j ? j.data : j
}

function toast (msg) {
  state.message = msg
  // 简易：写到每个 host 的 toast 区
  for (const host of Array.from(state.hosts)) {
    const root = hostRoot(host)
    const t = root?.querySelector?.('.flow-toast')
    if (t) { t.textContent = msg; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2600) }
  }
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

// ————————————————————— native-page 视图入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__flowView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'explorer') { loadDefs(); if (!state.dam.domains.length) loadDam() } })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}

function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__flowView !== view) continue
    const root = hostRoot(host)
    if (!root) continue
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
}

// content 区「外壳」就地重渲染：只换工具栏 + 版本对话框，绝不动画布 DOM（保护 bpmn-js 实例）。
// 版本徽标/下拉、版本管理弹窗都靠它刷新——用 refreshView('content') 会销毁重建画布。
function refreshContentChrome () {
  for (const host of Array.from(state.hosts)) {
    if (!host || host.__flowView !== 'content' || !host.isConnected) continue
    const root = hostRoot(host)
    if (!root) continue
    const tb = root.querySelector('[data-flow-toolbar]')
    if (tb) { tb.innerHTML = toolbarInnerHtml(); bindToolbar(root) }
    const dh = root.querySelector('[data-vdialog-host]')
    if (dh) { dh.innerHTML = versionDialogHtml(); bindVersionDialog(root) }
  }
}

function viewHtml (view) {
  if (view === 'explorer') return explorerHtml()
  if (view === 'property') return propertyHtml()
  return contentHtml()
}

// ————————————————————— explorer 区 —————————————————————

// DAM 注册表选项（域/应用/模块）。加载一次，级联过滤在前端做（与数据字典定义工作台同源同构）。
async function loadDam () {
  try {
    const dam = await apiJson('/api/registry/dam?active_only=true')
    state.dam = { domains: dam.domains || [], apps: dam.apps || dam.applications || [], modules: dam.modules || [] }
    refreshView('explorer')
    refreshView('property')
  } catch { /* 无注册表则不显示选项 */ }
}

const damLabel = (o) => `${o.name || o.title || o.id || o.module} (${o.id || o.module})`
function damOptionsHtml (list, cur, blank) {
  return `<option value="" ${!cur ? 'selected' : ''}>${blank}</option>` +
    list.map((o) => {
      const v = o.id || o.module
      return `<option value="${esc(v)}" ${v === cur ? 'selected' : ''}>${esc(damLabel(o))}</option>`
    }).join('')
}

/** DAM 三段选择器（explorer 顶部；级联：选域过滤应用，选应用过滤模块）。 */
function damFilterHtml () {
  const apps = (state.dam.apps || []).filter((a) => !state.fDomain || a.domain === state.fDomain)
  const modules = (state.dam.modules || []).filter((m) =>
    (!state.fDomain || m.domain === state.fDomain) &&
    (!state.fApp || (m.application || m.app) === state.fApp))
  return `<div class="flow-dam">
    <div class="flow-dam-row">
      <select data-dam="domain" title="域">${damOptionsHtml(state.dam.domains || [], state.fDomain, '全部域')}</select>
      <select data-dam="app" title="应用">${damOptionsHtml(apps, state.fApp, '全部应用')}</select>
    </div>
    <select data-dam="module" title="模块">${damOptionsHtml(modules, state.fModule, '全部模块')}</select>
  </div>`
}

/** 按 DAM 过滤后的定义列表。 */
function filteredDefs () {
  return state.definitions.filter((d) =>
    (!state.fDomain || d.domain === state.fDomain) &&
    (!state.fApp || d.application === state.fApp) &&
    (!state.fModule || d.module === state.fModule))
}

function explorerHtml () {
  const defs = filteredDefs()
  const body = state.loading
    ? `<cmx-empty-state icon="busy" title="加载流程定义..." size="sm"></cmx-empty-state>`
    : (defs.length
        ? defs.map(defItemHtml).join('')
        : (state.definitions.length
            ? `<cmx-empty-state icon="tree" title="当前 DAM 过滤下无匹配定义" size="sm"></cmx-empty-state>`
            : `<cmx-empty-state icon="tree" title="暂无流程定义" description="点下方新建" size="sm"></cmx-empty-state>`))
  return `<section class="flow flow-explorer">
    <div class="flow-head compact">
      <div><b>流程定义</b><span>cmx-flow / definitions</span></div>
      <button class="flow-icon-btn" data-act="refresh" title="刷新"><ui5-icon name="refresh"></ui5-icon></button>
    </div>
    ${damFilterHtml()}
    <div class="flow-def-list">${body}</div>
    <button class="flow-btn block" data-act="new">＋ 新建流程</button>
  </section>`
}

// 解析某定义当前应展示的版本：用户选择 → 服务端 activeVersion → 最新版本 → 空（草稿）。
function defVersion (d) {
  const vers = Array.isArray(d.versions) ? d.versions : []
  const sel = state.selectedVersion[d.key]
  if (sel != null) return sel
  if (d.activeVersion != null) return d.activeVersion
  return vers.length ? vers[0].version : null
}
function versionLabel (v) { return v == null ? '草稿' : ('v' + v) }

function defItemHtml (d) {
  const active = state.selectedKey === d.key
  const dam = [d.domain, d.application, d.module].filter(Boolean).join('/')
  const vers = Array.isArray(d.versions) ? d.versions : []
  const cur = defVersion(d)
  const verOptions = vers.length
    ? vers.map((v) => `<option value="${v.version}" ${v.version === cur ? 'selected' : ''}>v${v.version}${v.version === d.activeVersion ? ' · 当前' : ''}</option>`).join('')
    : `<option value="">草稿（未发布）</option>`
  return `<div class="flow-def-wrap ${active ? 'active' : ''}">
    <button class="flow-def ${active ? 'active' : ''}" data-key="${esc(d.key)}">
      <span class="flow-def-ic"><ui5-icon name="workflow-tasks"></ui5-icon></span>
      <span class="flow-def-main"><b>${esc(d.name || d.key)}</b><small>${esc(d.key)}${dam ? ' · ' + esc(dam) : ''}</small></span>
    </button>
    <div class="flow-def-ver">
      <ui5-icon name="history" class="flow-def-ver-ic"></ui5-icon>
      <select class="flow-def-vsel" data-ver-key="${esc(d.key)}" title="选择版本">${verOptions}</select>
      <span class="flow-def-vcount">${vers.length ? vers.length + ' 版本' : '—'}</span>
    </div>
  </div>`
}

// ————————————————————— content 区（画布 + 工具条） —————————————————————

function contentHtml () {
  return `<section class="flow flow-content">
    <div class="flow-toolbar" data-flow-toolbar>${toolbarInnerHtml()}</div>
    <div class="flow-canvas-wrap"><div class="flow-canvas" data-flow-canvas></div><div data-vdialog-host>${versionDialogHtml()}</div></div>
    <div class="flow-toast"></div>
  </section>`
}

// 工具栏内容（可就地重渲染，不碰画布 DOM）。含名称、版本切换、版本管理、编辑动作。
function toolbarInnerHtml () {
  const d = state.definitions.find((x) => x.key === state.selectedKey)
  const vers = d && Array.isArray(d.versions) ? d.versions : []
  const shown = state.shownVersion
  // 版本切换下拉（含草稿项）——选项自带「· 当前」标记，不再单列徽标，省工具栏宽度。
  const verSel = state.selectedKey
    ? `<span class="flow-tb-div"></span>
       <ui5-icon name="history" class="flow-tb-ic" title="版本"></ui5-icon>
       <select class="flow-ver-sel" data-ver-switch title="切换版本载入画布">
         <option value="" ${shown == null ? 'selected' : ''}>草稿</option>
         ${vers.map((v) => `<option value="${v.version}" ${v.version === shown ? 'selected' : ''}>v${v.version}${v.version === d.activeVersion ? ' · 当前' : ''}</option>`).join('')}
       </select>
       <button class="flow-btn slim" data-act="versions" title="版本管理"><ui5-icon name="settings"></ui5-icon></button>`
    : ''
  return `<input class="flow-name" data-name value="${esc(state.name)}" placeholder="流程名称">
    <button class="flow-btn" data-act="new">＋ 新建</button>
    ${verSel}
    <span class="flow-sp"></span>
    <button class="flow-btn" data-act="undo" title="撤销">↶</button>
    <button class="flow-btn" data-act="redo" title="重做">↷</button>
    <button class="flow-btn" data-act="fit" title="适应">⤢</button>
    <button class="flow-btn" data-act="validate">校验</button>
    <button class="flow-btn primary" data-act="save">保存草稿</button>
    <button class="flow-btn ok" data-act="publish">发布新版</button>`
}

// ————————————————————— 版本管理对话框（content 区内浮层） —————————————————————

function versionDialogHtml () {
  if (!state.versionDialog) return ''
  const d = state.definitions.find((x) => x.key === state.versionDialog.key)
  if (!d) return ''
  const vers = Array.isArray(d.versions) ? d.versions : []
  const rows = vers.length
    ? vers.map((v) => {
        const isCur = v.version === d.activeVersion
        return `<div class="flow-vrow ${isCur ? 'cur' : ''}">
          <div class="flow-vrow-main">
            <b>v${v.version}</b>${isCur ? '<span class="flow-vtag">当前生效</span>' : ''}
            <em>${esc(v.note || '无变更说明')}</em>
            <small>${esc((v.publishedAt || '').slice(0, 19).replace('T', ' '))}${v.publishedBy ? ' · ' + esc(v.publishedBy) : ''}</small>
          </div>
          <div class="flow-vrow-act">
            <button class="flow-btn slim ${isCur ? 'is-cur' : ''}" data-vactivate="${v.version}" ${isCur ? 'disabled' : ''}>${isCur ? '当前' : '设为当前'}</button>
            <button class="flow-btn slim danger" data-vdelete="${v.version}" ${isCur ? 'disabled' : ''} title="${isCur ? '当前版本不可删' : '删除此版本'}"><ui5-icon name="delete"></ui5-icon></button>
          </div>
        </div>`
      }).join('')
    : `<div class="flow-vempty"><ui5-icon name="flag"></ui5-icon><b>暂无已发布版本</b><span>在画布上编辑后点「发布新版」生成 v1。</span></div>`
  return `<div class="flow-dialog-mask" data-vdialog-mask>
    <section class="flow-dialog">
      <div class="flow-dialog-head">
        <span class="flow-dialog-ic"><ui5-icon name="history"></ui5-icon></span>
        <div><b>版本管理</b><em>${esc(d.name || d.key)} · ${esc(d.key)}</em></div>
        <button class="flow-icon-btn" data-vdialog-close title="关闭"><ui5-icon name="decline"></ui5-icon></button>
      </div>
      ${state.versionError ? `<div class="flow-dialog-err">${esc(state.versionError)}</div>` : ''}
      <div class="flow-dialog-body">
        <div class="flow-vlist-head"><b>版本序列</b><span>${vers.length ? vers.length + ' 个版本' : '无'}</span></div>
        <div class="flow-vlist">${rows}</div>
        <div class="flow-vnew">
          <div class="flow-vnew-title"><ui5-icon name="add"></ui5-icon> 发布新版本</div>
          <label class="flow-field"><span>变更说明</span><input data-vnote placeholder="说明本次发布调整了什么（可空）"></label>
          <div class="flow-hint">从当前画布内容发布：先「保存草稿」再发布，版本号自动 +1。</div>
          <button class="flow-btn ok block" data-vpublish><ui5-icon name="accept"></ui5-icon> 保存草稿并发布新版</button>
        </div>
      </div>
    </section>
  </div>`
}

// ————————————————————— property 区 —————————————————————

const TYPE_NAME = {
  StartEvent: '开始事件', EndEvent: '结束事件', UserTask: '用户任务', ServiceTask: '服务任务',
  ExclusiveGateway: '排他网关', ParallelGateway: '并行网关', InclusiveGateway: '包容网关',
  CallActivity: '子流程调用', SequenceFlow: '顺序流', Process: '流程', ScriptTask: '脚本任务',
}
function typeName (el) {
  const t = (el && el.type || '').replace('bpmn:', '')
  return TYPE_NAME[t] || t
}

function propertyHtml () {
  const el = state.selectedElement
  if (!el || el.type === 'bpmn:Process' || el.labelTarget) {
    return `<section class="flow flow-prop">
      <div class="flow-prop-head"><b>流程属性</b><small>${esc(state.selectedKey || '未保存')}</small></div>
      <div class="flow-prop-body">
        ${sec('DAM 归属')}
        ${damDefField('域 (domain)', 'domain', state.dam.domains || [], state.defDam.domain)}
        ${damDefField('应用 (application)', 'application',
          (state.dam.apps || []).filter((a) => !state.defDam.domain || a.domain === state.defDam.domain), state.defDam.application)}
        ${damDefField('模块 (module)', 'module',
          (state.dam.modules || []).filter((m) =>
            (!state.defDam.domain || m.domain === state.defDam.domain) &&
            (!state.defDam.application || (m.application || m.app) === state.defDam.application)), state.defDam.module)}
        <div class="flow-hint">随「保存草稿」落库；explorer 顶部按此三段过滤定义列表。</div>
        <div class="flow-sec">元素属性</div>
        <cmx-empty-state icon="detail-view" title="点击画布上的节点" description="在此配置属性" size="sm"></cmx-empty-state>
      </div>
    </section>`
  }
  const b = el.businessObject || {}
  let h = ''
  h += field('名称', 'name', b.name || '', '')
  if (el.type === 'bpmn:UserTask') {
    h += sec('办理人')
    h += field('指派办理人 (assignee)', 'assignee', b.get?.('flowable:assignee') || '', '单个用户 id，如 mgr')
    h += field('候选角色 (candidateGroups)', 'candidateGroups', b.get?.('flowable:candidateGroups') || '', '支持 role(FIN)，逗号分隔')
    h += sec('表单绑定')
    h += field('绑定表单 (cmx:formKey)', 'formKey', getFormAttr(b, 'formKey'), '如 pay.review；办理人点开待办时渲染此表单')
    h += selectField('表单模式 (formMode)', 'formMode', ['approve', 'edit', 'readonly'], getFormAttr(b, 'formMode') || 'approve')
    h += field('可写字段 (formFields)', 'formFields', getFormAttr(b, 'formFields'), '逗号分隔；限制本环节可改哪些字段（可空）')
    // 完整工作台表单：用已有工作区节点对话框编辑一个多区/多视图 workspace，绑到本节点 formKey。
    // 办理/查看时打开该工作台，property 区叠加审批/只读任务视图。需先填 formKey。
    {
      const fk = getFormAttr(b, 'formKey')
      const wsId = wsNodeIdFor(state.selectedKey, el.id)
      h += `<div class="flow-field">
        <button class="flow-btn primary block" data-edit-ws-node ${fk ? '' : 'disabled'}>
          <ui5-icon name="form"></ui5-icon> 编辑表单工作台</button>
        <div class="flow-hint">${fk
          ? `打开工作区节点对话框编辑「${esc(wsId)}」（content=业务表单/菜单、property=业务视图）；保存后自动把 formKey=<b>${esc(fk)}</b> 绑定到该工作台。`
          : '先填「绑定表单 (cmx:formKey)」再编辑工作台。'}</div>
      </div>`
    }
  }
  if (el.type === 'bpmn:CallActivity') {
    const calledKey = getCalledKey(b)
    const calledElement = b.get?.('calledElement') || ''
    // 模式判定：有 calledElement 且无 calledKey → 固定；否则默认按组织路由（真实业务的常态）。
    const fixedMode = !!calledElement && !calledKey
    h += sec('子流程')
    h += `<div class="flow-mode">
      <button class="flow-mode-opt ${!fixedMode ? 'on' : ''}" data-sub-mode="org">
        <b>按组织路由</b><small>各组织跑各自子流程（推荐）</small></button>
      <button class="flow-mode-opt ${fixedMode ? 'on' : ''}" data-sub-mode="fixed">
        <b>固定子流程</b><small>所有组织同一个（少见）</small></button>
    </div>`
    if (fixedMode) {
      h += field('固定子流程 key (calledElement)', 'calledElement', calledElement, '写死具体子流程定义 key，不区分组织')
    } else {
      h += field('逻辑子流程名 (cmx:calledKey)', 'calledKey', calledKey, '如 fin_review；运行期按发起组织解析到具体子流程')
      const nBind = state.bindingDialog?.calledKey === calledKey ? state.bindings.length : null
      h += `<div class="flow-field">
        <button class="flow-btn primary block" data-open-bindings ${calledKey ? '' : 'disabled'}>
          <ui5-icon name="org-chart"></ui5-icon> 配置组织绑定${nBind != null ? `（${nBind}）` : ''}</button>
        <div class="flow-hint">${calledKey ? '为各组织指定该逻辑子流程对应的具体流程；未匹配的组织沿组织树向上继承，最后落到默认绑定。' : '先填逻辑子流程名，再配置组织绑定。'}</div>
      </div>`
    }
  }
  if (el.type === 'bpmn:SequenceFlow') {
    h += sec('流转条件')
    const ce = b.conditionExpression
    h += field('条件表达式', 'condition', (ce && ce.body) || '', '如 ${amount>5000}')
  }
  return `<section class="flow flow-prop">
    <div class="flow-prop-head"><b>${esc(typeName(el))}</b><small>${esc((el.type || '').replace('bpmn:', ''))} · ${esc(el.id)}</small></div>
    <div class="flow-prop-body">${h}</div>
    ${bindingDialogHtml()}
  </section>`
}

// ————————————————————— 组织绑定对话框（property 区内浮层） —————————————————————

// 组织下拉选项：按 path 缩进呈现层级；已被其他绑定占用的组织仍可选（覆盖）。
function orgOptionsHtml (selected) {
  const opts = state.orgs.map((o) => {
    const depth = Math.max(0, String(o.path || '').split('/').filter(Boolean).length - 1)
    const indent = '　'.repeat(depth)
    return `<option value="${esc(o.id)}" ${o.id === selected ? 'selected' : ''}>${indent}${esc(o.name)}</option>`
  }).join('')
  return `<option value="">— 默认（兜底）绑定 —</option>${opts}`
}

// 子流程定义下拉：从已加载定义列表取（排除主流程自身无意义，这里全列）。
function subflowTargetOptionsHtml (selected) {
  return `<option value="">选择目标子流程…</option>` + state.definitions.map((d) =>
    `<option value="${esc(d.key)}" ${d.key === selected ? 'selected' : ''}>${esc(d.name || d.key)} (${esc(d.key)})</option>`).join('')
}

function bindingDialogHtml () {
  if (!state.bindingDialog) return ''
  const key = state.bindingDialog.calledKey
  const rows = state.bindings.length
    ? state.bindings.map((bd) => `<div class="flow-vrow ${bd.isDefault ? 'cur' : ''}">
        <div class="flow-vrow-main">
          <b>${bd.isDefault ? '默认（兜底）' : esc(bd.orgName || bd.orgId)}</b>${bd.isDefault ? '<span class="flow-vtag">fallback</span>' : ''}${!bd.enabled ? '<span class="flow-vtag off">停用</span>' : ''}
          <em>→ ${esc(bd.targetKey)}</em>
        </div>
        <div class="flow-vrow-act">
          <button class="flow-btn slim danger" data-bind-del="${esc(bd.id)}" title="删除绑定"><ui5-icon name="delete"></ui5-icon></button>
        </div>
      </div>`).join('')
    : `<div class="flow-vempty"><ui5-icon name="org-chart"></ui5-icon><b>暂无组织绑定</b><span>下方为组织指定子流程；建议先加一条「默认兜底」。</span></div>`
  return `<div class="flow-dialog-mask" data-bind-mask>
    <section class="flow-dialog">
      <div class="flow-dialog-head">
        <span class="flow-dialog-ic"><ui5-icon name="org-chart"></ui5-icon></span>
        <div><b>组织绑定</b><em>逻辑子流程 ${esc(key)}</em></div>
        <button class="flow-icon-btn" data-bind-close title="关闭"><ui5-icon name="decline"></ui5-icon></button>
      </div>
      ${state.bindingError ? `<div class="flow-dialog-err">${esc(state.bindingError)}</div>` : ''}
      <div class="flow-dialog-body">
        <div class="flow-vlist-head"><b>已配置绑定</b><span>${state.bindings.length} 条</span></div>
        <div class="flow-vlist">${rows}</div>
        <div class="flow-vnew">
          <div class="flow-vnew-title"><ui5-icon name="add"></ui5-icon> 新增/更新绑定</div>
          <label class="flow-field"><span>组织机构</span><select data-bind-org>${orgOptionsHtml('')}</select></label>
          <label class="flow-field"><span>目标子流程</span><select data-bind-target>${subflowTargetOptionsHtml('')}</select></label>
          <div class="flow-hint">同一组织重复保存会覆盖旧绑定。留「默认（兜底）」= 所有未单独配置的组织都走它。</div>
          <button class="flow-btn primary block" data-bind-save><ui5-icon name="accept"></ui5-icon> 保存绑定</button>
        </div>
      </div>
    </section>
  </div>`
}

function field (label, prop, val, hint) {
  return `<div class="flow-field"><label>${esc(label)}</label>` +
    `<input data-prop="${esc(prop)}" value="${esc(val)}">` +
    (hint ? `<div class="flow-hint">${esc(hint)}</div>` : '') + `</div>`
}
function selectField (label, prop, opts, cur, hint) {
  const o = opts.map((v) => `<option value="${esc(v)}" ${v === cur ? 'selected' : ''}>${esc(v)}</option>`).join('')
  return `<div class="flow-field"><label>${esc(label)}</label>` +
    `<select data-prop="${esc(prop)}">${o}</select>` +
    (hint ? `<div class="flow-hint">${esc(hint)}</div>` : '') + `</div>`
}
function sec (t) { return `<div class="flow-sec">${esc(t)}</div>` }

// cmx:calledKey 是自定义命名空间属性，bpmn-js 未注册 moddle 扩展，故落在 businessObject.$attrs
// （不是 .get('cmx:calledKey')）。读写都走 $attrs 才对；引擎 compile 认 cmx:calledKey。
function getCalledKey (b) {
  return (b && b.$attrs && (b.$attrs['cmx:calledKey'] || b.$attrs.calledKey)) || ''
}
function setCalledKey (el, value) {
  const b = el.businessObject
  const attrs = { ...(b.$attrs || {}) }
  if (value) attrs['cmx:calledKey'] = value
  else { delete attrs['cmx:calledKey']; delete attrs.calledKey }
  state.modeler.get('modeling').updateProperties(el, { $attrs: attrs })
}

// F2 表单绑定属性同 calledKey：cmx:formKey / cmx:formMode / cmx:formFields 落 $attrs。
// name 传 'formKey'|'formMode'|'formFields'，读写都走 cmx: 前缀（兼容裸名）。
function getFormAttr (b, name) {
  return (b && b.$attrs && (b.$attrs['cmx:' + name] || b.$attrs[name])) || ''
}
function setFormAttr (el, name, value) {
  const b = el.businessObject
  const attrs = { ...(b.$attrs || {}) }
  if (value) attrs['cmx:' + name] = value
  else { delete attrs['cmx:' + name]; delete attrs[name] }
  state.modeler.get('modeling').updateProperties(el, { $attrs: attrs })
}

// 本节点表单工作台的确定性 id（清洗成 SAFE_ID：仅 [A-Za-z0-9._-]，无冒号/尖括号）。
function wsNodeIdFor (defKey, nodeId) {
  const clean = (s) => String(s || '').replace(/[^A-Za-z0-9._-]+/g, '_')
  return `flow-form-${clean(defKey || 'def')}-${clean(nodeId || 'node')}`.slice(0, 128)
}

// 向门户 shell 派发 portal-help-action（穿透 shadow bubble 到 portal-app）。sourceEl 在门户 DOM 内。
function dispatchPortalAction (sourceEl, detail) {
  const ev = () => new CustomEvent('portal-help-action', { detail, bubbles: true, composed: true })
  try { if (sourceEl?.dispatchEvent) { sourceEl.dispatchEvent(ev()); return true } } catch {}
  try { document.dispatchEvent(ev()); return true } catch {}
  return false
}

// 点「编辑表单工作台」：① 打开已有工作区节点对话框编辑该 workspace node（门户 shell 处理，存 nodes.json）；
// ② 监听保存事件，把 formKey → workspaceNode 落 /api/flow/forms 注册表（kind='workspace'）。
async function openWsNodeEditor (sourceEl) {
  const el = state.selectedElement
  if (!el) return
  const fk = getFormAttr(el.businessObject, 'formKey')
  if (!fk) { toast('请先填「绑定表单 (cmx:formKey)」'); return }
  const wsId = wsNodeIdFor(state.selectedKey, el.id)
  // 保存成功后自动把 formKey 绑到该工作台（一次即可；用 once 监听）。
  const onSaved = async (e) => {
    const savedId = e?.detail?.id
    if (savedId && savedId !== wsId) return   // 不是本节点的工作台
    try {
      await apiJson('/api/flow/forms', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ formKey: fk, kind: 'workspace', workspaceNode: wsId, title: `流程表单工作台 · ${fk}` }),
      })
      toast(`已绑定 formKey=${fk} → 工作台 ${wsId}`)
    } catch (err) { toast('绑定注册表失败: ' + err.message) }
  }
  try { document.addEventListener('workspace-node-saved', onSaved, { once: true }) } catch {}
  const ok = dispatchPortalAction(sourceEl, { kind: 'editWorkspaceNode', id: wsId })
  if (!ok) { try { document.removeEventListener('workspace-node-saved', onSaved) } catch {}; toast('无法打开工作区节点对话框') }
}

/** property 区的 DAM 归属下拉（写 state.defDam，保存草稿时随请求落库）。 */
function damDefField (label, prop, list, cur) {
  return `<div class="flow-field"><label>${esc(label)}</label>` +
    `<select data-def-dam="${esc(prop)}">${damOptionsHtml(list, cur, '（未设置）')}</select></div>`
}

// ————————————————————— 事件绑定 —————————————————————

function bind (root, view, host) {
  if (view === 'explorer') {
    root.querySelector('[data-act="refresh"]')?.addEventListener('click', () => loadDefs())
    root.querySelectorAll('[data-act="new"]').forEach((b) => b.addEventListener('click', () => newDiagram()))
    root.querySelectorAll('.flow-def').forEach((el) => el.addEventListener('click', () => loadDef(el.dataset.key)))
    // 版本下拉：记住用户为该定义选的版本；若正是当前打开的定义，就地切换画布内容。
    root.querySelectorAll('[data-ver-key]').forEach((sel) => sel.addEventListener('change', (ev) => {
      ev.stopPropagation()
      const key = sel.dataset.verKey
      const v = sel.value === '' ? null : Number(sel.value)
      state.selectedVersion[key] = v
      if (state.selectedKey === key) loadDef(key, v)
    }))
    // 版本区点击不冒泡到卡片（避免误触发用旧版本 loadDef）。
    root.querySelectorAll('.flow-def-ver').forEach((el) => el.addEventListener('click', (ev) => ev.stopPropagation()))
    // DAM 三段级联过滤：选域清空应用/模块，选应用清空模块。
    root.querySelectorAll('[data-dam]').forEach((sel) => sel.addEventListener('change', () => {
      const kind = sel.dataset.dam
      const val = sel.value || ''
      if (kind === 'domain') { state.fDomain = val; state.fApp = ''; state.fModule = '' }
      else if (kind === 'app') { state.fApp = val; state.fModule = '' }
      else if (kind === 'module') { state.fModule = val }
      refreshView('explorer')
    }))
    return
  }
  if (view === 'content') {
    state.canvasHost = host
    bindToolbar(root)
    bindVersionDialog(root)
    // 启动画布（一次）
    bootCanvas(root, host)
    return
  }
  if (view === 'property') {
    root.querySelectorAll('[data-prop]').forEach((inp) => {
      inp.addEventListener('change', () => applyProp(inp.dataset.prop, inp.value))
    })
    // DAM 归属下拉：写 state.defDam（级联清空下级），保存草稿时随请求落库。
    root.querySelectorAll('[data-def-dam]').forEach((sel) => sel.addEventListener('change', () => {
      const kind = sel.dataset.defDam
      const val = sel.value || ''
      if (kind === 'domain') { state.defDam = { domain: val, application: '', module: '' } }
      else if (kind === 'application') { state.defDam.application = val; state.defDam.module = '' }
      else if (kind === 'module') { state.defDam.module = val }
      refreshView('property')
    }))
    // 子流程模式切换（按组织路由 / 固定）：改 BPMN 属性并重渲属性面板。
    root.querySelectorAll('[data-sub-mode]').forEach((btn) => btn.addEventListener('click', () => {
      setSubflowMode(btn.dataset.subMode)
    }))
    // 打开组织绑定对话框（挂在 property 区）。
    root.querySelector('[data-open-bindings]')?.addEventListener('click', () => {
      const el = state.selectedElement
      const key = el ? getCalledKey(el.businessObject) : ''
      if (key) openBindingDialog(key)
    })
    // 打开工作区节点对话框编辑本节点的表单工作台。
    root.querySelector('[data-edit-ws-node]')?.addEventListener('click', (e) => openWsNodeEditor(e.currentTarget))
    bindBindingDialog(root)
  }
}

// 切子流程模式：org → 清 calledElement（保留/等待 calledKey）；fixed → 清 calledKey。
function setSubflowMode (mode) {
  const el = state.selectedElement
  if (!el || !state.modeler) return
  const modeling = state.modeler.get('modeling')
  try {
    if (mode === 'fixed') {
      setCalledKey(el, '')
    } else {
      modeling.updateProperties(el, { calledElement: undefined })
    }
  } catch (e) { toast('切换失败: ' + e.message) }
  refreshView('property')
}

// content 工具栏事件（可重复调用：refreshContentChrome 重渲工具栏后要重绑）。
function bindToolbar (root) {
  const nameInput = root.querySelector('[data-name]')
  if (nameInput) nameInput.addEventListener('change', () => { state.name = nameInput.value })
  root.querySelector('[data-act="new"]')?.addEventListener('click', () => newDiagram())
  root.querySelector('[data-act="undo"]')?.addEventListener('click', () => state.modeler?.get('commandStack').undo())
  root.querySelector('[data-act="redo"]')?.addEventListener('click', () => state.modeler?.get('commandStack').redo())
  root.querySelector('[data-act="fit"]')?.addEventListener('click', () => state.modeler?.get('canvas').zoom('fit-viewport', 'auto'))
  root.querySelector('[data-act="validate"]')?.addEventListener('click', () => doValidate())
  root.querySelector('[data-act="save"]')?.addEventListener('click', () => doSave())
  root.querySelector('[data-act="publish"]')?.addEventListener('click', () => doPublish())
  root.querySelector('[data-ver-switch]')?.addEventListener('change', (e) => {
    const v = e.target.value === '' ? null : Number(e.target.value)
    if (state.selectedKey) loadDef(state.selectedKey, v)
  })
  root.querySelector('[data-act="versions"]')?.addEventListener('click', () => openVersionDialog())
}

// 版本管理对话框事件（每次重渲对话框后重绑）。
function bindVersionDialog (root) {
  root.querySelector('[data-vdialog-mask]')?.addEventListener('click', (e) => {
    if (e.target === e.currentTarget) closeVersionDialog()
  })
  root.querySelector('[data-vdialog-close]')?.addEventListener('click', () => closeVersionDialog())
  root.querySelectorAll('[data-vactivate]').forEach((b) => b.addEventListener('click', () => activateVersion(Number(b.dataset.vactivate))))
  root.querySelectorAll('[data-vdelete]').forEach((b) => b.addEventListener('click', () => deleteVersion(Number(b.dataset.vdelete))))
  root.querySelector('[data-vpublish]')?.addEventListener('click', () => {
    const note = root.querySelector('[data-vnote]')?.value || ''
    doPublish(note)
  })
}

// 组织绑定对话框事件（挂 property 区，每次重渲 property 后重绑）。
function bindBindingDialog (root) {
  root.querySelector('[data-bind-mask]')?.addEventListener('click', (e) => {
    if (e.target === e.currentTarget) closeBindingDialog()
  })
  root.querySelector('[data-bind-close]')?.addEventListener('click', () => closeBindingDialog())
  root.querySelector('[data-bind-save]')?.addEventListener('click', () => saveBinding(root))
  root.querySelectorAll('[data-bind-del]').forEach((b) => b.addEventListener('click', () => deleteBinding(b.dataset.bindDel)))
}

// ————————————————————— 组织绑定动作 —————————————————————

async function loadOrgs () {
  if (state.orgs.length) return
  try {
    const d = await apiJson('/api/flow/orgs')
    state.orgs = d.orgs || []
  } catch (e) { toast('加载组织失败: ' + e.message) }
}

async function openBindingDialog (calledKey) {
  state.bindingDialog = { calledKey }
  state.bindingError = ''
  state.bindings = []
  await loadOrgs()
  await reloadBindings(calledKey)
  refreshView('property')
}

function closeBindingDialog () {
  state.bindingDialog = null
  state.bindingError = ''
  refreshView('property')
}

async function reloadBindings (calledKey) {
  try {
    const d = await apiJson('/api/flow/subflow-bindings/' + enc(calledKey))
    state.bindings = d.bindings || []
  } catch (e) { state.bindingError = '加载绑定失败: ' + e.message; state.bindings = [] }
}

async function saveBinding (root) {
  const key = state.bindingDialog?.calledKey
  if (!key) return
  const orgId = root.querySelector('[data-bind-org]')?.value || ''
  const targetKey = root.querySelector('[data-bind-target]')?.value || ''
  if (!targetKey) { state.bindingError = '请选择目标子流程'; refreshView('property'); return }
  try {
    await apiJson('/api/flow/subflow-bindings', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ calledKey: key, orgId: orgId || null, targetKey, enabled: true }),
    })
    state.bindingError = ''
    await reloadBindings(key)
    refreshView('property')
    toast('绑定已保存')
  } catch (e) { state.bindingError = '保存绑定失败: ' + e.message; refreshView('property') }
}

async function deleteBinding (id) {
  const key = state.bindingDialog?.calledKey
  if (!key || !id) return
  try {
    await apiJson('/api/flow/subflow-bindings/id/' + enc(id), { method: 'DELETE' })
    await reloadBindings(key)
    refreshView('property')
    toast('绑定已删除')
  } catch (e) { state.bindingError = '删除绑定失败: ' + e.message; refreshView('property') }
}

// ————————————————————— 版本管理动作 —————————————————————

function openVersionDialog () {
  if (!state.selectedKey) { toast('请先选择一个流程定义'); return }
  state.versionDialog = { key: state.selectedKey }
  state.versionError = ''
  refreshContentChrome()
}
function closeVersionDialog () {
  state.versionDialog = null
  state.versionError = ''
  refreshContentChrome()
}

async function activateVersion (version) {
  const key = state.versionDialog?.key || state.selectedKey
  if (!key) return
  try {
    await apiJson('/api/flow/definitions/' + enc(key) + '/versions/' + version + '/activate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}',
    })
    state.versionError = ''
    await loadDefs()                 // 刷新版本列表（activeVersion 变了）
    state.selectedVersion[key] = version
    await loadDef(key, version)       // 画布切到新当前版本
    refreshContentChrome()
    toast('v' + version + ' 已设为当前版本（重启服务装载生效）')
  } catch (e) { state.versionError = '设为当前失败: ' + e.message; refreshContentChrome() }
}

async function deleteVersion (version) {
  const key = state.versionDialog?.key || state.selectedKey
  if (!key) return
  const C = (typeof globalThis !== 'undefined' && globalThis.__cmxDataComp) || {}
  const ok = typeof C.cmxConfirm === 'function'
    ? await C.cmxConfirm({ message: '确认删除版本 v' + version + '？该操作不可恢复。', intent: 'danger', confirmText: '删除' })
    : window.confirm('确认删除版本 v' + version + '？该操作不可恢复。')
  if (!ok) return
  try {
    await apiJson('/api/flow/definitions/' + enc(key) + '/versions/' + version, { method: 'DELETE' })
    state.versionError = ''
    await loadDefs()
    refreshContentChrome()
    toast('已删除版本 v' + version)
  } catch (e) { state.versionError = '删除失败: ' + e.message; refreshContentChrome() }
}

// ————————————————————— bpmn-js 生命周期 —————————————————————

// —— 中文语言包（bpmn-js 官方 translate 扩展点；词条按本地 v17.11.1 bundle 实际串提取核对）——
// bpmn-js 无官方发布语言包，i18n 机制即注入自定义 translate 函数。零远程,内置词典。
const ZH_CN = {
  // 调色板
  'Activate hand tool': '抓手工具',
  'Activate lasso tool': '框选工具',
  'Activate create/remove space tool': '增删空间',
  'Activate global connect tool': '全局连线',
  'Create start event': '创建开始事件',
  'Create end event': '创建结束事件',
  'Create gateway': '创建网关',
  'Create task': '创建任务',
  'Create intermediate/boundary event': '创建中间/边界事件',
  'Create expanded sub-process': '创建展开子流程',
  'Create data object reference': '创建数据对象引用',
  'Create data store reference': '创建数据存储引用',
  'Create pool/participant': '创建池/参与者',
  'Create group': '创建分组',
  // 上下文菜单（追加/连线/删除）
  'Append task': '追加任务',
  'Append gateway': '追加网关',
  'Append end event': '追加结束事件',
  'Append intermediate/boundary event': '追加中间/边界事件',
  'Append receive task': '追加接收任务',
  'Add text annotation': '添加文本注释',
  'Connect to other element': '连接到其他元素',
  'Connect using association': '用关联连接',
  'Connect using data input association': '用数据输入关联连接',
  'Change element': '更改元素类型',
  'Delete': '删除',
  'Search in diagram': '在图中搜索',
  'Toggle non-interrupting': '切换非中断',
  // 泳道
  'Add lane above': '在上方加泳道',
  'Add lane below': '在下方加泳道',
  'Divide into two lanes': '拆成两条泳道',
  'Divide into three lanes': '拆成三条泳道',
  // 元素类型（更改元素弹窗）
  'Start event': '开始事件',
  'Intermediate throw event': '中间抛出事件',
  'End event': '结束事件',
  'Message start event': '消息开始事件',
  'Timer start event': '定时开始事件',
  'Conditional start event': '条件开始事件',
  'Signal start event': '信号开始事件',
  'Exclusive gateway': '排他网关',
  'Parallel gateway': '并行网关',
  'Inclusive gateway': '包容网关',
  'Complex gateway': '复杂网关',
  'Event-based gateway': '事件网关',
  'Task': '任务',
  'User task': '用户任务',
  'Service task': '服务任务',
  'Send task': '发送任务',
  'Receive task': '接收任务',
  'Manual task': '手工任务',
  'Business rule task': '业务规则任务',
  'Script task': '脚本任务',
  'Call activity': '子流程调用',
  'Sub-process (collapsed)': '子流程（折叠）',
  'Sub-process (expanded)': '子流程（展开）',
  'Transaction': '事务',
  'Event sub-process': '事件子流程',
  'Empty pool/participant': '空池/参与者',
  'Message intermediate catch event': '消息中间捕获事件',
  'Message intermediate throw event': '消息中间抛出事件',
  'Timer intermediate catch event': '定时中间捕获事件',
  'Escalation intermediate throw event': '升级中间抛出事件',
  'Conditional intermediate catch event': '条件中间捕获事件',
  'Link intermediate catch event': '链接中间捕获事件',
  'Link intermediate throw event': '链接中间抛出事件',
  'Compensation intermediate throw event': '补偿中间抛出事件',
  'Signal intermediate catch event': '信号中间捕获事件',
  'Signal intermediate throw event': '信号中间抛出事件',
  'Message end event': '消息结束事件',
  'Escalation end event': '升级结束事件',
  'Error end event': '错误结束事件',
  'Cancel end event': '取消结束事件',
  'Compensation end event': '补偿结束事件',
  'Signal end event': '信号结束事件',
  'Terminate end event': '终止结束事件',
  'Message boundary event': '消息边界事件',
  'Timer boundary event': '定时边界事件',
  'Escalation boundary event': '升级边界事件',
  'Conditional boundary event': '条件边界事件',
  'Error boundary event': '错误边界事件',
  'Cancel boundary event': '取消边界事件',
  'Signal boundary event': '信号边界事件',
  'Compensation boundary event': '补偿边界事件',
  'Message boundary event (non-interrupting)': '消息边界事件（非中断）',
  'Timer boundary event (non-interrupting)': '定时边界事件（非中断）',
  'Escalation boundary event (non-interrupting)': '升级边界事件（非中断）',
  'Conditional boundary event (non-interrupting)': '条件边界事件（非中断）',
  'Signal boundary event (non-interrupting)': '信号边界事件（非中断）',
  // 多实例/循环标记
  'Loop': '循环',
  'Parallel multi-instance': '并行多实例（会签）',
  'Sequential multi-instance': '顺序多实例（或签）',
  'Ad-hoc': '即席',
  // 对齐分布
  'Align elements': '对齐元素',
  'Distribute elements horizontally': '水平分布',
  'Distribute elements vertically': '垂直分布',
  // 校验/错误提示（常见）
  'flow elements must be children of pools/participants': '流程元素必须放在池/参与者内',
  'no parent for {element} in {parent}': '{element} 在 {parent} 中没有父级',
  'no shape type specified': '未指定形状类型',
  'flow elements must be children of pools/participants or lanes': '流程元素必须放在池/参与者或泳道内',
  'out of bounds release': '越界释放',
  'more than {count} child lanes': '子泳道超过 {count} 条',
  'element required': '缺少元素',
  'diagram not part of bpmn:Definitions': '图不属于 bpmn:Definitions',
  'no diagram to display': '没有可显示的图',
  'no process or collaboration to display': '没有可显示的流程或协作',
  'element {element} referenced by {referenced}#{property} not yet drawn': '元素 {element}（被 {referenced}#{property} 引用）尚未绘制',
  'already rendered {element}': '{element} 已绘制',
  'failed to import {element}': '导入 {element} 失败',
}

/** bpmn-js translate 模块：查词典命中翻译,未命中回退英文;{占位符} 替换保持原语义。 */
const ZH_TRANSLATE_MODULE = {
  translate: ['value', function (template, replacements) {
    replacements = replacements || {}
    let str = ZH_CN[template] || template
    return str.replace(/\{([^}]+)\}/g, (_, k) => (replacements[k] !== undefined ? replacements[k] : `{${k}}`))
  }],
}

let _bpmnLoad = null
function ensureBpmnJs () {
  if (window.BpmnJS) return Promise.resolve()
  if (_bpmnLoad) return _bpmnLoad
  _bpmnLoad = new Promise((resolve, reject) => {
    const s = document.createElement('script')
    s.src = `${bpmnBase()}/bpmn-modeler.production.min.js`
    s.onload = resolve
    s.onerror = () => reject(new Error('bpmn-js 加载失败（检查网络/CDN）'))
    document.head.appendChild(s)
  })
  return _bpmnLoad
}

function injectBpmnCss (root) {
  // Chrome 限制：@font-face 声明在 shadow root 内不生效，必须落在主文档 → 字体声明注入 document.head（一次）。
  if (!document.head.querySelector('style[data-bpmn-font]')) {
    const st = document.createElement('style')
    st.setAttribute('data-bpmn-font', '1')
    const bb = bpmnBase()
    st.textContent = `@font-face{font-family:'bpmn';font-style:normal;font-weight:normal;
      src:url('${bb}/assets/bpmn-font/font/bpmn.woff2') format('woff2'),
          url('${bb}/assets/bpmn-font/font/bpmn.woff') format('woff'),
          url('${bb}/assets/bpmn-font/font/bpmn.ttf') format('truetype');}`
    document.head.appendChild(st)
  }
  if (root.querySelector('link[data-bpmn-css]')) return
  for (const f of ['assets/diagram-js.css', 'assets/bpmn-js.css', 'assets/bpmn-font/css/bpmn.css']) {
    const l = document.createElement('link')
    l.rel = 'stylesheet'
    l.href = `${bpmnBase()}/${f}`
    l.setAttribute('data-bpmn-css', '1')
    root.appendChild(l)
  }
}

async function bootCanvas (root, host) {
  try {
    injectBpmnCss(root)
    await ensureBpmnJs()
    const canvasEl = root.querySelector('[data-flow-canvas]')
    if (!canvasEl) return
    // 已有 modeler 且容器仍连通 → 复用（切区回来不重建），但要 resized 一次（tab 切回尺寸变了）。
    if (state.modeler && state.canvasEl && state.canvasEl.isConnected && state.canvasEl === canvasEl) {
      try { state.modeler.get('canvas').resized() } catch {}
      return
    }
    // 容器变了（首次或切走又回来）→ 新建 modeler 挂到新容器
    if (state.modeler) { try { state.modeler.destroy() } catch {} state.modeler = null }
    // 关键：bpmn-js 初始化/导入前容器必须有非零尺寸，否则 Canvas.getLayer 读 viewbox 报
    // "reading 'root-0'" + SVGMatrix non-finite。native-page tab 首帧容器可能 0 尺寸，等它布局好。
    await waitForSize(canvasEl)
    state.modeler = new window.BpmnJS({
      container: canvasEl,
      keyboard: { bindTo: document },
      // 中文界面：经 bpmn-js 官方 translate 扩展点注入内置词典（零远程,词条对照 v17.11.1 bundle 提取）。
      additionalModules: [ZH_TRANSLATE_MODULE],
    })
    state.canvasEl = canvasEl
    state.modeler.on('selection.changed', (e) => {
      state.selectedElement = e.newSelection.length === 1 ? e.newSelection[0] : null
      refreshView('property')
    })
    state.modeler.on('element.changed', (e) => {
      if (state.selectedElement && state.selectedElement.id === e.element.id) refreshView('property')
    })
    // 装当前选中定义 或 空白
    if (state.selectedKey) {
      try {
        const d = await apiJson('/api/flow/definitions/' + enc(state.selectedKey))
        if (d.bpmnXml) await openDiagram(d.bpmnXml)
        else await openDiagram(EMPTY_DIAGRAM)
      } catch { await openDiagram(EMPTY_DIAGRAM) }
    } else {
      await openDiagram(EMPTY_DIAGRAM)
    }
  } catch (err) {
    toast('画布启动失败: ' + err.message)
    console.error(err)
  }
}

// 等容器有非零尺寸（native-page tab 首帧可能 0×0）。最多等 ~2s，超时也放行（用兜底尺寸）。
function waitForSize (el, tries = 40) {
  return new Promise((resolve) => {
    let n = 0
    const chk = () => {
      const r = el.getBoundingClientRect()
      if ((r.width > 20 && r.height > 20) || n++ >= tries) { resolve(); return }
      requestAnimationFrame(chk)
    }
    chk()
  })
}

async function openDiagram (xml) {
  if (!state.modeler) return
  try {
    // 兼容旧图：DB 里此前存的图可能没声明 xmlns:cmx（本次修复前铸的），
    // 一旦用户在其上重新绑定表单/子流程键，cmx:* 属性会在 saveXML 时被 moddle 静默丢弃。
    // 载入前补声明该命名空间即可让 cmx:formKey / cmx:calledKey 等正常回写。幂等，已声明则不动。
    if (!/xmlns:cmx\s*=/.test(xml)) {
      xml = xml.replace(/(<bpmn:definitions\b)/, '$1 xmlns:cmx="http://cmx/flow"')
    }
    const noDI = !/BPMNDiagram|BPMNPlane/i.test(xml)
    let toImport = xml
    if (noDI) { toImport = layoutXml(xml) }
    await state.modeler.importXML(toImport)
    if (noDI) relayoutConnections()
    // 下一帧再 fit：importXML 后 bpmn-js 的图形 bbox 要等一帧才算准，同帧 fit 会读到空 bbox 只框住起点。
    requestAnimationFrame(() => fitView())
  } catch (err) { toast('加载失败: ' + err.message); console.error(err) }
}

// 安全适应视口：容器尺寸非有限时 zoom fit-viewport 会抛 SVGMatrix non-finite。
// native-page tab 首帧/动画期间容器尺寸会变，故用 ResizeObserver 持续跟随，尺寸每变一次就重新 fit，
// 稳定后（连续几帧不变）停止。这样无论 tab 何时定尺寸，图都能正确缩放铺满。
function fitView () {
  if (!state.modeler) return
  const canvas = state.modeler.get('canvas')
  const el = state.canvasEl
  const fit = () => {
    if (!state.modeler || !el || !el.isConnected) return false
    const r = el.getBoundingClientRect()
    if (r.width < 20 || r.height < 20) return false
    try { canvas.resized(); canvas.zoom('fit-viewport', 'auto') } catch {}
    return true
  }
  fit()
  // 观察容器尺寸变化，稳定前持续重新 fit（tab 布局/动画期间尺寸会跳变）。
  try {
    let lastW = 0, lastH = 0, stable = 0
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect()
      if (r.width === lastW && r.height === lastH) { if (++stable >= 3) ro.disconnect(); return }
      lastW = r.width; lastH = r.height; stable = 0
      fit()
    })
    ro.observe(el)
    setTimeout(() => ro.disconnect(), 6000)
  } catch {}
}

// 内置分层布局（零依赖，替代 bpmn-auto-layout——那库只发 CDN ESM 且不产 BPMNEdge）：
// 解析纯语义 BPMN → BFS 从 startEvent 分层（rank）→ 纵向排坐标（同层横向摊开）→
// 生成完整 BPMNDiagram（BPMNShape + BPMNEdge），bpmn-js 可直接渲染。
function layoutXml (xml) {
  try {
    const doc = new DOMParser().parseFromString(xml, 'text/xml')
    const proc = doc.querySelector('process, [*|process]')
    if (!proc) return xml
    const local = (n) => n.localName || n.tagName
    // 收集节点（除 sequenceFlow 外的一层子元素）与边。
    const nodes = new Map() // id -> {kind}
    const flows = []        // {id, src, tgt}
    for (const el of Array.from(proc.children)) {
      const tag = local(el)
      const id = el.getAttribute('id')
      if (!id) continue
      if (tag === 'sequenceFlow') {
        flows.push({ id, src: el.getAttribute('sourceRef'), tgt: el.getAttribute('targetRef') })
      } else if (tag !== 'documentation' && tag !== 'extensionElements') {
        nodes.set(id, { kind: tag })
      }
    }
    if (!nodes.size) return xml
    // 节点尺寸：事件 36×36、网关 50×50、其余任务 100×80。
    const dim = (kind) => /Event$/i.test(kind) ? { w: 36, h: 36 }
      : /Gateway$/i.test(kind) ? { w: 50, h: 50 } : { w: 100, h: 80 }
    // BFS 分层：从 startEvent（无则任取入度 0 节点）。
    const out = new Map(); const indeg = new Map()
    nodes.forEach((_, id) => { out.set(id, []); indeg.set(id, 0) })
    for (const f of flows) {
      if (out.has(f.src) && nodes.has(f.tgt)) { out.get(f.src).push(f.tgt); indeg.set(f.tgt, (indeg.get(f.tgt) || 0) + 1) }
    }
    let roots = Array.from(nodes.entries()).filter(([, v]) => v.kind === 'startEvent').map(([id]) => id)
    if (!roots.length) roots = Array.from(nodes.keys()).filter((id) => !indeg.get(id))
    if (!roots.length) roots = [Array.from(nodes.keys())[0]]
    const rank = new Map(); const q = []
    roots.forEach((r) => { rank.set(r, 0); q.push(r) })
    while (q.length) {
      const id = q.shift()
      for (const nx of out.get(id) || []) {
        const nr = rank.get(id) + 1
        if (!rank.has(nx) || nr > rank.get(nx)) {
          if (!rank.has(nx)) q.push(nx)
          rank.set(nx, Math.min(nr, nodes.size)) // 防环上溢
        }
      }
    }
    nodes.forEach((_, id) => { if (!rank.has(id)) rank.set(id, 0) }) // 孤儿放第 0 层
    // 布坐标：纵向层间 120px，同层节点横向 160px 摊开，层内水平居中对齐到 x=260 轴。
    const layers = new Map()
    rank.forEach((r, id) => { if (!layers.has(r)) layers.set(r, []); layers.get(r).push(id) })
    const pos = new Map()
    const V_GAP = 120; const H_GAP = 160; const CX = 260; let y = 40
    Array.from(layers.keys()).sort((a, b) => a - b).forEach((r) => {
      const ids = layers.get(r)
      let maxH = 0
      ids.forEach((id, i) => {
        const d = dim(nodes.get(id).kind)
        const x = CX + (i - (ids.length - 1) / 2) * H_GAP - d.w / 2
        pos.set(id, { x: Math.round(x), y, ...d })
        maxH = Math.max(maxH, d.h)
      })
      y += maxH + V_GAP - 40 // 层高 + 间距（间距按最矮 40 起算的补差）
      y = Math.round(y)
    })
    // 生成 BPMNDiagram（shape + edge）。
    const NS_DI = 'http://www.omg.org/spec/BPMN/20100524/DI'
    const NS_DC = 'http://www.omg.org/spec/DD/20100524/DC'
    const defs = doc.documentElement
    defs.setAttribute('xmlns:bpmndi', NS_DI)
    defs.setAttribute('xmlns:dc', NS_DC)
    defs.setAttribute('xmlns:di', NS_DI)
    const dia = doc.createElementNS(NS_DI, 'bpmndi:BPMNDiagram'); dia.setAttribute('id', 'AutoDiagram')
    const plane = doc.createElementNS(NS_DI, 'bpmndi:BPMNPlane')
    plane.setAttribute('id', 'AutoPlane'); plane.setAttribute('bpmnElement', proc.getAttribute('id') || 'process')
    dia.appendChild(plane)
    pos.forEach((p, id) => {
      const sh = doc.createElementNS(NS_DI, 'bpmndi:BPMNShape')
      sh.setAttribute('id', id + '_di'); sh.setAttribute('bpmnElement', id)
      const b = doc.createElementNS(NS_DC, 'dc:Bounds')
      b.setAttribute('x', p.x); b.setAttribute('y', p.y); b.setAttribute('width', p.w); b.setAttribute('height', p.h)
      sh.appendChild(b); plane.appendChild(sh)
    })
    for (const f of flows) {
      const a = pos.get(f.src); const b = pos.get(f.tgt)
      if (!a || !b) continue
      const edge = doc.createElementNS(NS_DI, 'bpmndi:BPMNEdge')
      edge.setAttribute('id', f.id + '_di'); edge.setAttribute('bpmnElement', f.id)
      // 直连：源底边中点 → 目标顶边中点（同层横连则左右边）。导入后 relayoutConnections 精修。
      const down = b.y >= a.y + a.h
      const p1 = down ? { x: a.x + a.w / 2, y: a.y + a.h } : { x: b.x > a.x ? a.x + a.w : a.x, y: a.y + a.h / 2 }
      const p2 = down ? { x: b.x + b.w / 2, y: b.y } : { x: b.x > a.x ? b.x : b.x + b.w, y: b.y + b.h / 2 }
      for (const p of [p1, p2]) {
        const wp = doc.createElementNS(NS_DI, 'di:waypoint')
        wp.setAttribute('x', Math.round(p.x)); wp.setAttribute('y', Math.round(p.y)); edge.appendChild(wp)
      }
      plane.appendChild(edge)
    }
    defs.appendChild(dia)
    return new XMLSerializer().serializeToString(doc)
  } catch (e) { console.warn('内置布局失败，原样导入:', e); return xml }
}

// 导入后用 bpmn-js 原生 layoutConnection 精确重布线（端点裁到节点边框）。
function relayoutConnections () {
  try {
    const er = state.modeler.get('elementRegistry')
    const modeling = state.modeler.get('modeling')
    er.getAll().forEach((el) => {
      if (el.type === 'bpmn:SequenceFlow' && el.source && el.target) modeling.layoutConnection(el, {})
    })
  } catch (e) { console.warn('精确布线失败:', e) }
}

function applyProp (prop, value) {
  const el = state.selectedElement
  if (!el || !state.modeler) return
  const modeling = state.modeler.get('modeling')
  const moddle = state.modeler.get('moddle')
  try {
    if (prop === 'name') modeling.updateProperties(el, { name: value })
    else if (prop === 'assignee') modeling.updateProperties(el, { 'flowable:assignee': value || undefined })
    else if (prop === 'candidateGroups') modeling.updateProperties(el, { 'flowable:candidateGroups': value || undefined })
    else if (prop === 'calledElement') modeling.updateProperties(el, { calledElement: value || undefined })
    else if (prop === 'calledKey') setCalledKey(el, value)
    else if (prop === 'formKey' || prop === 'formMode' || prop === 'formFields') setFormAttr(el, prop, value)
    else if (prop === 'condition') {
      if (value) modeling.updateProperties(el, { conditionExpression: moddle.create('bpmn:FormalExpression', { body: value }) })
      else modeling.updateProperties(el, { conditionExpression: undefined })
    }
  } catch (err) { toast('设置失败: ' + err.message) }
}

// ————————————————————— 数据/动作 —————————————————————

async function loadDefs () {
  state.loading = true; refreshView('explorer')
  try {
    // 设计器列表来源定义库（草稿+已发布全列），而非引擎运行态已装载定义。
    const d = await apiJson('/api/flow/design/definitions')
    state.definitions = (d.definitions || []).filter((x) => x.startable !== false)
  } catch (e) { toast('加载定义失败: ' + e.message); state.definitions = [] }
  state.loading = false; refreshView('explorer')
}

// 加载定义到画布。version 传具体版本号则载入该历史版本（只读快照，可另存草稿覆盖）；
// 不传/undefined 用该定义当前应展示版本（见 defVersion），null 明确取草稿。
async function loadDef (key, version) {
  try {
    const d = state.definitions.find((x) => x.key === key)
    let v = version
    if (v === undefined) v = d ? defVersion(d) : null
    const q = v == null ? '' : ('?version=' + enc(v))
    const detail = await apiJson('/api/flow/definitions/' + enc(key) + q)
    if (!detail.bpmnXml) { toast('该版本无 XML'); return }
    state.selectedKey = key
    state.name = detail.name || key
    state.defDam = { domain: detail.domain || '', application: detail.application || '', module: detail.module || '' }
    state.shownVersion = detail.shownVersion ?? v ?? null
    if (v != null) state.selectedVersion[key] = v
    refreshView('explorer')       // explorer 高亮当前选中 + 版本下拉同步
    refreshContentChrome()        // content 工具栏版本徽标/下拉同步（不销毁画布：openDiagram 会就地导入）
    refreshView('property')       // property 区 DAM 归属同步
    await openDiagram(detail.bpmnXml)   // 就地把 XML 导入现有 modeler
    toast('已加载: ' + (detail.name || key) + ' · ' + versionLabel(state.shownVersion))
  } catch (e) { toast('加载失败: ' + e.message) }
}

// 就地更新 content 工具条里的名称输入框，不重渲染整个 content（避免销毁 bpmn-js 画布 DOM）。
function syncNameInput () {
  for (const host of Array.from(state.hosts)) {
    if (host.__flowView !== 'content') continue
    const root = hostRoot(host)
    const inp = root?.querySelector?.('[data-name]')
    if (inp) inp.value = state.name
  }
}

async function newDiagram () {
  state.selectedKey = null
  state.name = '新建流程'
  // 新建流程默认继承 explorer 当前 DAM 过滤（在哪个模块下看，就归到哪个模块）。
  state.defDam = { domain: state.fDomain || '', application: state.fApp || '', module: state.fModule || '' }
  refreshView('explorer'); refreshView('property'); syncNameInput()
  await openDiagram(EMPTY_DIAGRAM)
  toast('已新建空白流程')
}

async function getXml () {
  const { xml } = await state.modeler.saveXML({ format: true })
  return xml
}

async function doValidate () {
  try {
    const xml = await getXml()
    if (!/bpmn:process|<process/i.test(xml)) { toast('校验失败: 无 process'); return }
    toast('前端结构 OK，点保存触发后端编译校验')
  } catch (e) { toast('校验失败: ' + e.message) }
}

async function doSave () {
  try {
    const xml = await getXml()
    const r = await apiJson('/api/flow/definitions/draft', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        name: state.name || '未命名流程',
        domain: state.defDam.domain || null,
        application: state.defDam.application || null,
        module: state.defDam.module || null,
        bpmnXml: xml,
      }),
    })
    state.selectedKey = r.key
    toast('草稿已保存: ' + r.key + '（后端编译校验通过）')
    await loadDefs()
    refreshContentChrome()
  } catch (e) { toast('保存失败: ' + e.message) }
}

async function doPublish (note) {
  if (!state.selectedKey) { toast('请先保存草稿再发布'); return }
  try {
    // 先存草稿（把画布最新内容落库），再发布——工具栏「发布新版」与对话框发布同路径。
    await doSaveSilent()
    const r = await apiJson('/api/flow/definitions/' + enc(state.selectedKey) + '/publish', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ note: (typeof note === 'string' && note) ? note : null }),
    })
    state.selectedVersion[state.selectedKey] = r.version
    state.shownVersion = r.version
    state.versionError = ''
    await loadDefs()
    refreshContentChrome()
    toast('已发布 ' + r.key + ' v' + r.version + '（重启服务装载新版）')
  } catch (e) { state.versionError = '发布失败: ' + e.message; toast('发布失败: ' + e.message); refreshContentChrome() }
}

// 存草稿但不弹 toast/不重载列表（供发布前静默保存复用）。返回保存结果。
async function doSaveSilent () {
  const xml = await getXml()
  const r = await apiJson('/api/flow/definitions/draft', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      name: state.name || '未命名流程',
      domain: state.defDam.domain || null,
      application: state.defDam.application || null,
      module: state.defDam.module || null,
      bpmnXml: xml,
    }),
  })
  state.selectedKey = r.key
  return r
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  /* bpmn 图标字体的 @font-face 已注入主文档 head（shadow 内声明 font-face 无效，Chrome 限制）。 */
  .flow{--brand:#0969da;--brand-d:#0a4d8c;--brand-soft:#ddf4ff;--ink:#1f2328;--muted:#656d76;--line:#d0d7de;--line-soft:#eaeef2;--ok:#1a7f37;--new:#bc4c00;--flow-bar-h:47px;
    font:13px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;color:var(--ink);height:100%;box-sizing:border-box;display:flex;flex-direction:column}
  .flow *{box-sizing:border-box}
  /* 三区顶部标题/工具栏统一高度（explorer/property 标题区 = content 工具栏，--flow-bar-h）。 */
  .flow-head{height:var(--flow-bar-h);flex:0 0 auto;display:flex;align-items:center;justify-content:space-between;padding:0 12px;border-bottom:1px solid var(--line-soft)}
  .flow-head b{font-size:13px} .flow-head span{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-icon-btn{border:1px solid var(--line);background:#fff;border-radius:7px;width:28px;height:28px;cursor:pointer;display:grid;place-items:center}
  .flow-def-list{flex:1;overflow:auto;padding:8px}
  .flow-dam{display:flex;flex-direction:column;gap:6px;padding:8px 10px;border-bottom:1px solid var(--line-soft);background:#fafbfc}
  .flow-dam-row{display:grid;grid-template-columns:1fr 1fr;gap:6px}
  .flow-dam select,.flow-field select{width:100%;height:28px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:7px;padding:0 6px;background:#fff;color:var(--ink)}
  .flow-dam select:focus,.flow-field select:focus{outline:none;border-color:var(--brand);box-shadow:0 0 0 3px var(--brand-soft)}
  /* explorer 定义卡 + 版本行 */
  .flow-def-wrap{border:1px solid transparent;border-radius:8px;margin-bottom:3px}
  .flow-def-wrap.active{background:var(--brand-soft);border-color:#aacdf5}
  .flow-def{width:100%;display:flex;align-items:center;gap:8px;padding:8px 10px;border:0;border-radius:8px;background:transparent;cursor:pointer;text-align:left}
  .flow-def-wrap:not(.active) .flow-def:hover{background:var(--line-soft)}
  .flow-def-ic{width:22px;height:22px;display:grid;place-items:center;color:var(--brand-d)}
  .flow-def-main{min-width:0} .flow-def-main b{display:block;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-def-main small{display:block;font-size:10.5px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-def-ver{display:flex;align-items:center;gap:6px;padding:5px 10px 7px 40px}
  .flow-def-ver-ic{color:var(--muted);width:14px;height:14px;flex:0 0 auto}
  .flow-def-vsel{flex:1;min-width:0;height:26px;font:inherit;font-size:11.5px;border:1px solid var(--line);border-radius:6px;padding:0 6px;background:#fff;color:var(--ink)}
  .flow-def-vcount{font-size:10px;color:var(--muted);white-space:nowrap;flex:0 0 auto}
  .flow-btn{font:inherit;font-size:12.5px;border:1px solid var(--line);background:#fff;color:var(--ink);border-radius:7px;padding:6px 12px;cursor:pointer;font-weight:600}
  .flow-btn:hover{background:var(--line-soft)} .flow-btn.primary{background:var(--brand);border-color:var(--brand);color:#fff}
  .flow-btn.ok{background:var(--ok);border-color:var(--ok);color:#fff} .flow-btn.block{margin:8px 0 0;width:100%;justify-content:center;display:flex;align-items:center;gap:6px}
  .flow-btn[disabled]{opacity:.5;cursor:not-allowed}
  .flow-btn.slim{padding:4px 9px;font-size:11.5px} .flow-btn.slim.is-cur{background:var(--ok);border-color:var(--ok);color:#fff}
  .flow-btn.danger{color:#b3261e;border-color:#e6b3ae} .flow-btn.danger:hover{background:#fdeceb}
  .flow-content{height:100%}
  .flow-toolbar{height:var(--flow-bar-h);flex:0 0 auto;display:flex;align-items:center;gap:6px;padding:0 10px;border-bottom:1px solid var(--line);background:#fff;flex-wrap:nowrap;overflow-x:auto;overflow-y:hidden}
  .flow-toolbar>.flow-btn,.flow-toolbar>.flow-name,.flow-toolbar>.flow-ver-badge,.flow-toolbar>.flow-ver-sel,.flow-toolbar>.flow-tb-div{flex:0 0 auto}
  .flow-name{font:inherit;font-size:12.5px;border:1px solid var(--line);border-radius:7px;padding:6px 10px;width:130px}
  .flow-tb-div{width:1px;height:20px;background:var(--line);margin:0 1px}
  .flow-tb-ic{color:var(--muted);width:15px;height:15px}
  .flow-ver-badge{font-size:11.5px;font-weight:700;color:var(--brand-d);background:var(--brand-soft);border:1px solid #aacdf5;border-radius:6px;padding:3px 8px;white-space:nowrap}
  .flow-ver-badge.cur{color:#0a5c2b;background:#e6f4ea;border-color:#a6d8b6}
  .flow-ver-sel{height:28px;max-width:140px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:6px;padding:0 6px;background:#fff;color:var(--ink)}
  .flow-sp{flex:1}
  .flow-canvas-wrap{position:relative;flex:1;min-height:0;background:radial-gradient(circle,#e5e9ee 1px,transparent 1px) 0 0/22px 22px}
  .flow-canvas{position:absolute;inset:0}
  /* 版本管理对话框（content 画布区内浮层） */
  .flow-dialog-mask{position:absolute;inset:0;z-index:30;display:flex;align-items:center;justify-content:center;background:rgba(13,29,46,.32);padding:18px}
  .flow-dialog{width:min(560px,100%);max-height:100%;overflow:hidden;display:flex;flex-direction:column;background:#fff;border:1px solid #aacdf5;border-radius:10px;box-shadow:0 18px 46px rgba(0,0,0,.22)}
  .flow-dialog-head{display:flex;align-items:center;gap:10px;padding:12px 14px;border-bottom:1px solid var(--line-soft);background:linear-gradient(135deg,var(--brand-soft),#fff)}
  .flow-dialog-ic{width:32px;height:32px;border-radius:8px;background:var(--brand);color:#fff;display:grid;place-items:center;flex:0 0 auto}
  .flow-dialog-head>div{flex:1;min-width:0} .flow-dialog-head b{display:block;font-size:14px} .flow-dialog-head em{display:block;font-style:normal;font-size:11px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-dialog-err{margin:10px 14px 0;border:1px solid #e6b3ae;background:#fdeceb;color:#a10000;border-radius:7px;padding:7px 9px;font-size:12px}
  .flow-dialog-body{padding:12px 14px;overflow:auto}
  .flow-vlist-head{display:flex;align-items:baseline;justify-content:space-between;margin-bottom:8px} .flow-vlist-head b{font-size:13px} .flow-vlist-head span{font-size:11px;color:var(--muted)}
  .flow-vlist{display:flex;flex-direction:column;gap:7px;max-height:260px;overflow:auto}
  .flow-vrow{display:flex;align-items:center;gap:10px;border:1px solid var(--line);border-radius:8px;padding:8px 10px;background:#fafbfc}
  .flow-vrow.cur{border-color:#a6d8b6;background:#f0f9f3}
  .flow-vrow-main{flex:1;min-width:0} .flow-vrow-main b{font-size:13px;font-family:ui-monospace,Menlo,monospace} .flow-vtag{margin-left:6px;font-size:10px;font-weight:700;color:#0a5c2b;background:#e6f4ea;border:1px solid #a6d8b6;border-radius:5px;padding:1px 6px}
  .flow-vtag.off{color:#8a6d00;background:#fdf3d0;border-color:#e6d38a}
  /* 子流程模式切换（按组织路由 / 固定） */
  .flow-mode{display:grid;grid-template-columns:1fr 1fr;gap:8px;margin-bottom:12px}
  .flow-mode-opt{text-align:left;border:1.5px solid var(--line);border-radius:9px;background:#fff;padding:8px 10px;cursor:pointer}
  .flow-mode-opt b{display:block;font-size:12.5px;color:var(--ink)} .flow-mode-opt small{display:block;font-size:10.5px;color:var(--muted);margin-top:2px}
  .flow-mode-opt.on{border-color:var(--brand);background:var(--brand-soft)} .flow-mode-opt.on b{color:var(--brand-d)}
  .flow-vrow-main em{display:block;font-style:normal;font-size:11.5px;color:var(--ink);margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-vrow-main small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .flow-vrow-act{display:flex;align-items:center;gap:6px;flex:0 0 auto}
  .flow-vempty{border:1px dashed var(--line);border-radius:8px;padding:20px;text-align:center;color:var(--muted)} .flow-vempty ui5-icon{color:var(--brand)} .flow-vempty b{display:block;margin-top:6px;color:var(--ink)}
  .flow-vnew{margin-top:14px;border-top:1px solid var(--line-soft);padding-top:12px}
  .flow-vnew-title{display:flex;align-items:center;gap:6px;font-weight:700;font-size:12.5px;color:var(--brand-d);margin-bottom:8px}
  .flow-prop-head{height:var(--flow-bar-h);flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 14px;border-bottom:1px solid var(--line-soft)} .flow-prop-head b{font-size:14px} .flow-prop-head small{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-prop-body{padding:12px 14px;overflow:auto}
  .flow-field{margin-bottom:12px} .flow-field label{display:block;font-size:11px;font-weight:700;color:var(--muted);text-transform:uppercase;margin-bottom:4px}
  .flow-field input{width:100%;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:7px;padding:7px 10px}
  .flow-field input:focus{outline:none;border-color:var(--brand);box-shadow:0 0 0 3px var(--brand-soft)}
  .flow-hint{font-size:11px;color:var(--muted);margin-top:3px} .flow-sec{font-size:11px;font-weight:800;color:var(--brand-d);text-transform:uppercase;margin:14px 0 8px;padding-bottom:5px;border-bottom:1px solid var(--line-soft)}
  .flow-empty{display:flex;flex-direction:column;align-items:center;gap:8px;color:var(--muted);font-size:12.5px;padding:36px 16px;text-align:center}
  .flow-toast{position:absolute;left:50%;bottom:18px;transform:translateX(-50%);background:#0d1117;color:#fff;padding:9px 16px;border-radius:9px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s;z-index:20}
  .flow-toast.show{opacity:1}
  `
}

// 门户壳 export default（CFG 默认值=今天：同源 fetch + /portal/vendor/bpmn-js 资产）；
// S5 组件壳 import { configure, mount } 覆盖 apiBase/authHeaders/bpmnBase 后自挂 shadowRoot。
export { configure, mount }
export default {
  defaultView: 'content',
  views: {
    async explorer (ctx) { return mount(ctx, 'explorer') },
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
