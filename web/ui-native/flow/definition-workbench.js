/**
 * 流程定义工作台 —— 流程设计工作台的副本变体（portal.flow.definition-workbench），
 * 由"流程定义管理"页的工作台/新建入口打开（initialContext: definitionKey / groupId 预填）。
 *
 * 与原版（portal.flow.design-workbench）的唯一差异：explorer 不渲染定义列表
 * （DAM 过滤/列表/分页/新建按钮整体移除，列表职责在流程定义管理页），保留画布结构大纲。
 *
 * explorer：画布结构大纲（节点/网关/边，双向选中联动）。
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
// 门户壳默认：同源 fetch + 资产走门户 vendor/bpmn-js（打包进 CMXPortalManager，不访问远程 CDN）。
// 组件壳/headless 壳 configure({ apiBase, authHeaders, bpmnBase })：资产可指向自建 CDN 或组件包内路径。
const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
  bpmnBase: '',        // 空 = 按部署形态自动推断（见 bpmnBase()）
  // 协同 M1：当前用户（门户从 localStorage 兜底，组件壳/headless 由宿主注入）。范式同 todo-center.js。
  getUser: () => { try { return window.localStorage.getItem('cmx_user_id') || window.localStorage.getItem('cmx_username') || 'admin' } catch { return 'admin' } },
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }
function currentUser () { try { return CFG.getUser() || 'admin' } catch { return 'admin' } }

// bpmn-js 本地资产根（每次读 CFG.bpmnBase，故 configure()/bpmn-base 属性覆盖对后续加载即时生效）。
// 缺省按部署形态自动推断：生产门户挂在 /portal/ 基座（CMXPortalManager dist 经后端静态托管），
// Vite dev server 基座是 /（public/ 资产挂根路径）。写死任一边都会在另一边加载失败——dev 下
// script 404、CSS 被 SPA fallback 冒充 200 html 静默失效——故按当前路径前缀推断，显式配置优先。
function bpmnBase () {
  if (CFG.bpmnBase) return CFG.bpmnBase
  return location.pathname.startsWith('/portal/') ? '/portal/vendor/bpmn-js' : '/vendor/bpmn-js'
}

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
  // 大纲面板（explorer 下部）：联动展示画布所有节点/网关/边，双向选中。主流程与子流程共用。
  outline: {
    open: true,          // 是否展开大纲
    height: 260,         // 大纲区高度（px，上下各半可拖拽的下半）
    groups: { node: true, gateway: true, edge: true }, // 三组各自展开/折叠
    tick: 0,             // 画布结构变化计数（增删元素时 ++，触发大纲重算）
  },
  // 缩略图预览（content 画布右下角）：小方框标当前视口，可拖动平移画布。主流程/子流程共用同一 modeler。
  minimap: {
    collapsed: false,    // 是否折叠成小标题条
    vb: null,            // 当前缩略图 SVG 的 viewBox { x, y, w, h }（= 全图内容边界+留白，供坐标换算）
  },
  // 主题：chrome 走 --sap* 令牌自动翻（零 JS）；仅 BPMN 画布需要这个布尔来切暗色覆盖（见 applyCanvasTheme）。
  dark: false,
  // DAM 三段（域/应用/模块）——对标数据字典定义工作台 explorer 顶部选择器。
  dam: { domains: [], apps: [], modules: [] },   // /api/registry/dam 选项
  fDomain: '', fApp: '', fModule: '',            // explorer 过滤选择
  // 流程列表分页（explorer 主流程列表，0 基）——首页/上页/下页/末页。过滤/刷新时重置为 0。
  defPage: 0, defPageSize: 8,
  defDam: { domain: '', application: '', module: '' }, // 当前画布定义的 DAM 归属（随存草稿落库）
  // 版本管理（对标报表版本：每条定义可选版本；publish=新增版本、activate=设当前、delete=删历史）。
  selectedVersion: {},   // { [key]: version|null } 用户为某定义选中的版本（覆盖服务端 active）
  shownVersion: null,    // 当前画布加载的是哪个版本（null=草稿/当前）
  versionDialog: null,   // { key } 打开版本管理对话框时置，null=关闭
  versionError: '',      // 对话框内错误提示
  // 子流程路由（M5.2 组织路由 → RD4 维度泛化：callActivity 写逻辑 key + 维度 key，运行期按维度取值解析）。
  orgs: [],              // 组织树扁平表（/api/flow/orgs），懒加载一次
  dims: null,            // 可选路由维度（/api/flow/dimensions），懒加载一次；org 内建置顶
  dimEntries: {},        // { [dimKey]: [{id,name,parentId,path}] } 各维度条目缓存
  bindingDialog: null,   // { calledKey, dimKey } 打开维度绑定对话框时置
  bindings: [],          // 当前 calledKey 的维度绑定列表
  bindingError: '',      // 绑定对话框内错误
  // 分支条件可视化构造器（P2-c：把单个 ${expr} 文本框升级为行式构造器）。
  fnCatalog: null,       // 内置函数目录（/api/flow/conditions/functions），懒加载一次并缓存
  condRows: [],          // 当前 SequenceFlow 的条件行 [{ lp, fn, varName, op, val, valIsVar, conn }]
  condAdvanced: false,   // true=直接编辑原始表达式（escape hatch），false=行式构造器
  condTest: null,        // 试算结果 { ok, result, error }（打 /conditions/eval）
  condTestVars: '',      // 试算变量 JSON 文本（用户填样例变量）
  // UserTask 属性面板分页签（P3：把 6 个裸文本框重构成专业分页签）。
  utTab: 'assignee',     // 当前激活页签：basic|assignee|approval|form|cc
  idnCache: {},          // 身份选择器缓存 { orgs:[], roles:[], positions:[], users:[] }（懒加载）
  idnMode: null,         // 身份模式探测结果（'local'|'external'|null=未探）
  subMode: {},           // callActivity 子流程模式显式记忆 { [elId]: 'org'|'fixed' }（修 F5 新建死循环）
  assigneeKind: {},      // userTask 办理人类型显式记忆 { [elId]: 'user'|'role'|'position'|... }（修「切类型值暂空→反推回落 user」死循环，同 F5）
  dirty: false,          // U1：画布有未保存编辑（commandStack 变化置真，保存/载入清零
  // 流程分组（20260902 重构）：新建定义首存时随草稿落库（仅 INSERT 生效；改挂走定义管理页）。
  groupId: null,         // 当前编辑定义的目标分组 id（null = 未分组）
  groups: [],            // 分组下拉数据源 [{id,name,enabled}]
  ctxApplied: false,     // initialContext（definitionKey/groupId 预填）只消费一次）

  // ⑤ 变量声明（设计态）。随定义 XML 走：openDiagram 从 <cmx:varSchema> 读入，getXml 注回。
  varSchema: [],         // VarDecl[] 树（含对象 fields / 数组 item）
  varValidation: '',     // 校验策略 strict|lenient|off（''=lenient 默认），随 process cmx:varValidation
  varDialog: false,      // 全屏变量编辑器是否打开
  varError: '',          // 变量编辑器内校验错误
  varPaths: [],          // 摊平路径缓存（下拉数据源）；editSchema 变更时重算

  // 子流程设计（explorer 只显主流程 + callActivity 钻入式子流程编辑，复用 content 单一画布）。
  showSubflows: false,   // explorer 是否临时显示子流程（默认关，只显主流程）
  subNav: null,          // 钻入式子流程编辑状态：null=在主流程；否则 { calledKey|null, fixedKey|null,
                         //   variants:[], activeTargetKey, mainKey, mainXml, mainSelId, mainName, mainDam,
                         //   mainShownVersion, mainDirty, subName, pendingBindOrg }
  propTab: 'node',       // property 区页签：node=节点属性 | model=数据模型（变量声明）| sim=模拟
  // 设计态模拟（试跑，后端 /definitions/simulate）：facts 表单 / 发起人·组织 / JSON override /
  //   结果 trace / 已高亮的画布元素 id（清除用）。
  sim: { facts: {}, initiator: '', org: '', raw: '', result: null, running: false, marked: [] },
  // 版本对比（diff，纯前端）：va/vb=选中的两版本('' =草稿/当前画布)；result={added,removed,modified}；
  //   running 期间禁重入；marked=已高亮的画布元素 id。null=未进入对比。
  diff: null,
  // 协同 M1（感知+防冲突）+ M2（对象级属性合并）：仅编辑草稿时启用。sessionId 每标签页一个；
  //   roster 在场者；es SSE；hbTimer 心跳；selTimer 选中去抖；baseUpdatedAt 载入草稿时间戳（乐观锁基线）；
  //   marked 远端选中已高亮的元素 id；notice 「他人更新了草稿」提示；user 当前用户。
  //   M2：__applying 远端 op 回放中（echo-guard，不回广播）；opSeen `${elementId}::${prop}`→已应用最大 seq（LWW）。
  collab: { on: false, defKey: null, sessionId: null, user: null, roster: [], es: null, hbTimer: null, selTimer: null, baseUpdatedAt: null, marked: [], notice: null, __applying: false, opSeen: {} },
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

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

// 钻入式子流程编辑复用 content 唯一 modeler，故「当前编辑的 modeler」恒为 state.modeler。
// （保留此间接层：属性读写/图载入/导出/徽章/fit 都经它，历史参数化契约不变。）
function activeModeler () {
  return state.modeler
}
// 「脏」标记读写（钻入式主/子共用同一 state.dirty，因同时只编辑一个）。
function setActiveDirty (v) {
  state.dirty = v
}

const { apiJson: _sharedApiJson, openSseStream } = globalThis.__cmxDataComp // 共享 fetch 封装 + fetch 流式 SSE（cmx-data-comp/lib；SSE 走 fetch 可带 Authorization 头——门户全局拦截器自动注入，本页 X-User 由下方包装层注入）；经 CFG 转发保留组件壳 configure() 契约
// 本页多注入一个 X-User 头（流程设计工作台历史约定），用户自带的 options.headers 仍可覆盖。
async function apiJson (url, options = {}) {
  return _sharedApiJson(url, { ...options, headers: { 'X-User': currentUser(), ...(options.headers || {}) } }, CFG)
}

const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

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
    applyCanvasTheme(root)   // .flow 每次重建，补挂 data-theme 供暗色画布覆盖
  }
  requestAnimationFrame(() => {
    render()
    if (view === 'explorer') { loadDefs(); if (!state.dam.domains.length) loadDam() }
    if (view === 'content') {
      loadGroups()
      if (!state.ctxApplied) {
        state.ctxApplied = true
        try {
          // 门户 openNode({initialContext}) → workspace.context 逐键注入（定义管理页「新建/工作台」入口）。
          const wctx = host && host.workspace && host.workspace.context
          const get = (k) => (wctx && typeof wctx.get === 'function' ? wctx.get(k) : undefined)
          const dk = get('definitionKey') || get('flowDefinitionKey')
          const gid = get('groupId')
          if (dk) loadDef(String(dk))
          else if (gid != null && gid !== '') state.groupId = Number(gid)
        } catch { /* initialContext 非法不阻断工作台 */ }
      }
    }
  })
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
    applyCanvasTheme(root)
  }
}

// 点击流程时保持列表滚动位置不动：refreshView('explorer') 会重建 innerHTML，
// 内层 .flow-def-list 的 scrollTop 会被重置为 0（外层宿主容器的滚动则会保留）。
// 故重渲染前抓取各滚动容器位置，返回一个在重渲染后原样复位的函数——点选前后视图纹丝不动。
function preserveExplorerScroll () {
  const saved = []
  for (const host of Array.from(state.hosts)) {
    if (!host || host.__flowView !== 'explorer' || !host.isConnected) continue
    const root = hostRoot(host)
    if (!root) continue
    const list = root.querySelector?.('.flow-def-list')
    saved.push({ host, listTop: list ? list.scrollTop : 0, rootTop: root.scrollTop || 0, hostTop: host.scrollTop || 0 })
  }
  return () => {
    for (const s of saved) {
      if (!s.host || !s.host.isConnected) continue
      const root = hostRoot(s.host)
      if (!root) continue
      const list = root.querySelector?.('.flow-def-list')
      if (list) list.scrollTop = s.listTop
      if (typeof root.scrollTop === 'number') root.scrollTop = s.rootTop
      if (s.host !== root && typeof s.host.scrollTop === 'number') s.host.scrollTop = s.hostTop
    }
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
  if (view === 'explorer') return state.subNav ? subflowExplorerHtml() : explorerHtml()  // 子流程模式：变体导航
  if (view === 'property') return propertyHtml()
  return contentHtml()
}

// ————————————————————— 主题（跟随门户 SAP 主题） —————————————————————
// chrome 全部走 --sap* 令牌，light/dark 由门户改写令牌值自动翻，零 JS 零重渲染。
// 唯 BPMN 画布（bpmn-js 自绘 SVG + <link> 注入的 diagram-js.css）不吃 --sap*，
// 故用一个「是否暗色」布尔，在 .flow 上打 data-theme=dark，触发下方暗色画布覆盖块。
// 探测法复刻 packages/cmx-data-comp/src/lib/cmx-theme-detect.js：读 :root 的
// --sapBackgroundColor 相对亮度（<128 视为暗），回退系统 prefers-color-scheme。
function detectDarkMode () {
  if (typeof getComputedStyle === 'undefined') return false
  try {
    const bg = getComputedStyle(document.documentElement).getPropertyValue('--sapBackgroundColor').trim()
    if (bg) {
      let r, g, b
      const hex = bg.match(/^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})/i)
      if (hex) { r = parseInt(hex[1], 16); g = parseInt(hex[2], 16); b = parseInt(hex[3], 16) }
      else {
        const rgb = bg.match(/rgba?\((\d+)[,\s]+(\d+)[,\s]+(\d+)/)
        if (rgb) { r = +rgb[1]; g = +rgb[2]; b = +rgb[3] }
      }
      if (r != null) return (0.299 * r + 0.587 * g + 0.114 * b) < 128
    }
  } catch (_) {}
  try { return window.matchMedia('(prefers-color-scheme: dark)').matches } catch { return false }
}

// 重渲染后给该根内 .flow 补挂 data-theme（.flow 每次重建 innerHTML，故必须渲染后统一补）。
function applyCanvasTheme (root) {
  if (!root || !root.querySelector) return
  state.dark = detectDarkMode()
  const flow = root.querySelector('.flow')
  if (flow) flow.setAttribute('data-theme', state.dark ? 'dark' : 'light')
}

// 门户切主题时：chrome 令牌自动翻，无需重渲染；这里只把画布 data-theme 翻过来 + 重绘缩略图克隆。
function refreshCanvasThemeAll () {
  state.dark = detectDarkMode()
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) continue
    const root = hostRoot(host)
    if (root) applyCanvasTheme(root)
  }
  try { scheduleMinimapRender() } catch (_) {}
}
try {
  if (typeof window !== 'undefined' && !window.__flowThemeWired) {
    window.__flowThemeWired = true
    window.addEventListener('cmx-portal-theme-change', () => refreshCanvasThemeAll())
  }
} catch (_) {}


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

/** 按 DAM 过滤后的定义列表（默认只主流程；showSubflows 时含子流程）。 */
function filteredDefs () {
  return mainDefs().filter((d) =>
    (!state.fDomain || d.domain === state.fDomain) &&
    (!state.fApp || d.application === state.fApp) &&
    (!state.fModule || d.module === state.fModule))
}

function explorerHtml () {
  const all = filteredDefs()
  const size = state.defPageSize
  const pages = Math.max(1, Math.ceil(all.length / size))
  if (state.defPage > pages - 1) state.defPage = pages - 1   // 过滤/删除后越界回退
  if (state.defPage < 0) state.defPage = 0
  const pageDefs = all.slice(state.defPage * size, state.defPage * size + size)
  const body = state.loading
    ? `<cmx-empty-state icon="busy" title="加载流程定义..." size="sm"></cmx-empty-state>`
    : (all.length
        ? pageDefs.map(defItemHtml).join('')
        : (state.definitions.length
            ? `<cmx-empty-state icon="tree" title="当前 DAM 过滤下无匹配定义" size="sm"></cmx-empty-state>`
            : `<cmx-empty-state icon="tree" title="暂无流程定义" description="点下方新建" size="sm"></cmx-empty-state>`))
  // 定义工作台变体：不渲染定义列表（上方 body/分页仅 kept for 子流程导航变体），只留结构大纲。
  void body; void pages; void pageDefs
  return `<section class="flow flow-explorer">
    <div class="flow-head compact">
      <div><b>流程大纲</b><span>cmx-flow / 画布结构</span></div>
      <button class="flow-icon-btn" data-act="refresh" title="刷新"><ui5-icon name="refresh"></ui5-icon></button>
    </div>
    ${outlineHtml()}
  </section>`
}

// 主流程列表分页条：首页/上页/下页/末页（图标按钮）+「第 X / Y 页 · 共 N 条」。边界自动禁用。
function defPagerHtml (total, page, pages) {
  const atFirst = page <= 0
  const atLast = page >= pages - 1
  return `<div class="flow-pager">
    <button class="flow-pg-btn icon" data-page="first" ${atFirst ? 'disabled' : ''} title="首页" aria-label="首页">«</button>
    <button class="flow-pg-btn icon" data-page="prev" ${atFirst ? 'disabled' : ''} title="上一页" aria-label="上一页">‹</button>
    <span class="flow-pg-info">${page + 1}/${pages}<small>共 ${total} 条</small></span>
    <button class="flow-pg-btn icon" data-page="next" ${atLast ? 'disabled' : ''} title="下一页" aria-label="下一页">›</button>
    <button class="flow-pg-btn icon" data-page="last" ${atLast ? 'disabled' : ''} title="末页" aria-label="末页">»</button>
  </div>`
}

// 跳到第 n 页（0 基，自动夹取到 [0, pages-1]）。无变化则不重渲染。
function gotoDefPage (n) {
  const size = state.defPageSize
  const pages = Math.max(1, Math.ceil(filteredDefs().length / size))
  const p = Math.max(0, Math.min(n, pages - 1))
  if (p === state.defPage) return
  state.defPage = p
  refreshView('explorer')
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
    <div class="flow-canvas-wrap"><div class="flow-canvas" data-flow-canvas tabindex="0"></div><div class="flow-collab-bar" data-collab-bar></div><div class="flow-collab-notice" data-collab-notice></div><div data-vdialog-host>${versionDialogHtml()}</div></div>
    <div class="flow-toast"></div>
  </section>`
}

// 工具栏内容（可就地重渲染，不碰画布 DOM）。含名称、版本切换、版本管理、编辑动作。
function toolbarInnerHtml () {
  // 子流程钻入模式：置顶面包屑 + 常驻「← 返回主流程」（在始终可见的工具栏里，永不被门户窄区裁剪）。
  if (state.subNav) return subToolbarInnerHtml()
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
       <button class="flow-btn slim" data-act="versions" title="版本管理"><ui5-icon name="settings"></ui5-icon></button>
       <button class="flow-btn slim" data-act="diff" title="版本对比（结构 diff + 画布高亮）" ${vers.length ? '' : 'disabled'}><ui5-icon name="compare"></ui5-icon> 对比</button>`
    : ''
  return `<input class="flow-name" data-name value="${esc(state.name)}" placeholder="流程名称">
    <select class="flow-group" data-group title="流程分组（新建首存时生效；已有定义改挂分组请走流程定义管理页）">
      <option value="">未分组</option>
      ${(state.groups || []).map((g) => `<option value="${g.id}" ${String(state.groupId) === String(g.id) ? 'selected' : ''}>${esc(g.name)}</option>`).join('')}
    </select>
    <button class="flow-btn" data-act="new">＋ 新建</button>
    ${verSel}
    <span class="flow-sp"></span>
    <button class="flow-btn" data-act="undo" title="撤销">↶</button>
    <button class="flow-btn" data-act="redo" title="重做">↷</button>
    <button class="flow-btn" data-act="fit" title="适应">⤢</button>
    <span class="flow-tb-div"></span>
    <button class="flow-btn" data-act="import" title="导入 BPMN XML 文件为新草稿"><ui5-icon name="upload"></ui5-icon> 导入</button>
    <button class="flow-btn" data-act="export" title="导出当前画布为 BPMN XML 文件"><ui5-icon name="download"></ui5-icon> 导出</button>
    <button class="flow-btn" data-act="saveas" title="另存为一个新流程（复制）"><ui5-icon name="duplicate"></ui5-icon> 另存为</button>
    <span class="flow-tb-div"></span>
    <button class="flow-btn" data-act="validate">校验</button>
    <button class="flow-btn primary" data-act="save">保存草稿</button>
    <button class="flow-btn ok" data-act="publish">发布新版</button>
    <input type="file" data-import-file accept=".bpmn,.xml,text/xml" style="display:none">`
}

// 子流程钻入模式的工具栏：面包屑（主流程 › 子流程）+ 常驻「← 返回主流程」+ 名称/撤销/重做/适应/校验/保存/发布。
// 隐藏 新建/版本/导入/导出/另存为（这些属主流程语境，子流程里易误操作）。返回按钮在此常驻工具栏，永不被裁。
function subToolbarInnerHtml () {
  const sn = state.subNav
  const subLabel = state.name || sn.subName || (sn.activeTargetKey ? sn.activeTargetKey : '＋ 新建子流程')
  return `<button class="flow-btn back" data-act="back-to-main" title="返回主流程（丢弃未保存的子流程修改前会确认）"><ui5-icon name="nav-back"></ui5-icon> 返回主流程</button>
    <span class="flow-crumb">
      <ui5-icon name="workflow-tasks" class="flow-crumb-ic"></ui5-icon>
      <span class="flow-crumb-main">${esc(sn.mainName || sn.mainKey || '主流程')}</span>
      <span class="flow-crumb-sep">›</span>
      <ui5-icon name="process" class="flow-crumb-ic sub"></ui5-icon>
      <span class="flow-crumb-sub" title="${esc(sn.calledKey ? '逻辑子流程 ' + sn.calledKey : '固定子流程')}">${esc(subLabel)}</span>
    </span>
    <span class="flow-tb-div"></span>
    <input class="flow-name" data-name value="${esc(state.name)}" placeholder="子流程名称">
    <span class="flow-sp"></span>
    <button class="flow-btn" data-act="undo" title="撤销">↶</button>
    <button class="flow-btn" data-act="redo" title="重做">↷</button>
    <button class="flow-btn" data-act="fit" title="适应">⤢</button>
    <span class="flow-tb-div"></span>
    <button class="flow-btn" data-act="validate">校验</button>
    <button class="flow-btn primary" data-act="save">保存子流程草稿</button>
    <button class="flow-btn ok" data-act="publish">发布子流程</button>`
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
  BusinessRuleTask: '业务规则任务', BoundaryEvent: '边界事件', IntermediateCatchEvent: '中间捕获事件',
  IntermediateThrowEvent: '中间抛出事件', SubProcess: '子流程', ManualTask: '手工任务',
  SendTask: '发送任务', ReceiveTask: '接收任务', Task: '任务', ComplexGateway: '复杂网关',
  EventBasedGateway: '事件网关', TextAnnotation: '文本注解', Group: '分组', DataObjectReference: '数据对象',
  Participant: '参与者', Lane: '泳道', Transaction: '事务',
}
function typeName (el) {
  const t = (el && el.type || '').replace('bpmn:', '')
  return TYPE_NAME[t] || t
}

// ————————————————————— 大纲面板（explorer 下部，联动画布） —————————————————————

// 元素归类：'edge'（顺序流/连线）| 'gateway'（各类网关）| 'node'（其余可见 shape）。
// 过滤掉 label 伪元素、根 Process、无 id 的内部元素。
function outlineCategory (el) {
  if (!el || !el.type || el.labelTarget) return null
  const t = el.type
  if (t === 'bpmn:Process' || t === 'bpmn:Collaboration' || t === 'label') return null
  if (t === 'bpmn:SequenceFlow' || el.waypoints) return 'edge'
  if (t.includes('Gateway')) return 'gateway'
  if (el.width == null) return null // 非 shape（如未落地元素）跳过
  return 'node'
}

// 从当前 modeler 的 elementRegistry 收集大纲元素，按三类分组。无画布返回空。
function outlineGroups () {
  const empty = { node: [], gateway: [], edge: [] }
  const m = state.modeler
  if (!m) return empty
  let all
  try { all = m.get('elementRegistry').getAll() } catch { return empty }
  const g = { node: [], gateway: [], edge: [] }
  for (const el of all) {
    const cat = outlineCategory(el)
    if (!cat) continue
    g[cat].push(el)
  }
  return g
}

// 一条边的可读标签：优先边名，否则 源名→目标名。
function edgeLabel (el) {
  const bo = el.businessObject
  if (bo && bo.name) return bo.name
  const nm = (e) => (e && e.businessObject && (e.businessObject.name || e.businessObject.id)) || '?'
  if (el.source || el.target) return `${nm(el.source)} → ${nm(el.target)}`
  return el.id
}

// 一个大纲项的显示名：节点/网关用元素名（回退类型名），边用 edgeLabel。
function outlineItemLabel (el, cat) {
  if (cat === 'edge') return edgeLabel(el)
  const bo = el.businessObject
  return (bo && bo.name) || typeName(el)
}

// 各类的小图标（ui5 icon name）。
const OUTLINE_ICON = {
  'bpmn:StartEvent': 'begin', 'bpmn:EndEvent': 'process', 'bpmn:UserTask': 'employee',
  'bpmn:ServiceTask': 'settings', 'bpmn:ScriptTask': 'syntax', 'bpmn:BusinessRuleTask': 'tree',
  'bpmn:CallActivity': 'workflow-tasks', 'bpmn:SubProcess': 'process',
  'bpmn:BoundaryEvent': 'alarm', 'bpmn:IntermediateCatchEvent': 'message-information',
  'bpmn:ManualTask': 'employee', 'bpmn:SendTask': 'outbox', 'bpmn:ReceiveTask': 'inbox',
}
function outlineIcon (el, cat) {
  if (cat === 'gateway') return 'decision'
  if (cat === 'edge') return 'arrow-right'
  return OUTLINE_ICON[el.type] || 'circle-task'
}

function outlineGroupHtml (cat, label, items) {
  const open = state.outline.groups[cat]
  const selId = state.selectedElement && state.selectedElement.id
  const rows = items.length
    ? items.map((el) => {
        const on = el.id === selId
        return `<button class="flow-ol-item ${on ? 'on' : ''}" data-ol-id="${esc(el.id)}" title="${esc(el.id)}">
          <ui5-icon name="${outlineIcon(el, cat)}" class="flow-ol-ic"></ui5-icon>
          <span class="flow-ol-name">${esc(outlineItemLabel(el, cat))}</span>
        </button>`
      }).join('')
    : `<div class="flow-ol-empty">—</div>`
  return `<div class="flow-ol-group">
    <button class="flow-ol-ghead" data-ol-group="${cat}">
      <ui5-icon name="${open ? 'navigation-down-arrow' : 'navigation-right-arrow'}" class="flow-ol-caret"></ui5-icon>
      <b>${label}</b><span class="flow-ol-count">${items.length}</span>
    </button>
    ${open ? `<div class="flow-ol-items">${rows}</div>` : ''}
  </div>`
}

// 大纲面板整体 HTML（嵌入主流程 / 子流程两个 explorer 变体的下部）。
function outlineHtml () {
  const ol = state.outline
  const g = outlineGroups()
  const total = g.node.length + g.gateway.length + g.edge.length
  const caret = ol.open ? 'navigation-down-arrow' : 'navigation-right-arrow'
  const body = ol.open
    ? (state.modeler
        ? (total
            ? `<div class="flow-ol-body">
                ${outlineGroupHtml('node', '节点', g.node)}
                ${outlineGroupHtml('gateway', '网关', g.gateway)}
                ${outlineGroupHtml('edge', '边', g.edge)}
              </div>`
            : `<div class="flow-ol-hint">画布暂无元素</div>`)
        : `<div class="flow-ol-hint">载入流程后显示大纲</div>`)
    : ''
  return `<div class="flow-outline ${ol.open ? 'open' : 'closed'}" ${ol.open ? `style="height:${ol.height}px"` : ''}>
    <div class="flow-ol-resize" data-ol-resize title="拖拽调整大纲高度"></div>
    <button class="flow-ol-head" data-ol-toggle>
      <ui5-icon name="${caret}" class="flow-ol-caret"></ui5-icon>
      <b>大纲</b><span class="flow-ol-total">${total}</span>
      <span class="flow-ol-sub">${state.subNav ? '子流程' : '主流程'}</span>
    </button>
    ${body}
  </div>`
}

// 大纲事件绑定（主/子 explorer 共用）：折叠面板、折叠分组、点击项联动画布、拖拽调高。
function bindOutline (root) {
  // 面板整体折叠。
  root.querySelector('[data-ol-toggle]')?.addEventListener('click', () => {
    state.outline.open = !state.outline.open
    refreshView('explorer')
  })
  // 分组折叠。
  root.querySelectorAll('[data-ol-group]').forEach((b) => b.addEventListener('click', () => {
    const cat = b.dataset.olGroup
    state.outline.groups[cat] = !state.outline.groups[cat]
    refreshView('explorer')
  }))
  // ★ 大纲 → 画布：点击项在画布里选中 + 滚动到可见。
  root.querySelectorAll('[data-ol-id]').forEach((b) => b.addEventListener('click', () => {
    selectInCanvas(b.dataset.olId)
  }))
  // 拖拽调整大纲高度（上下各半）。
  const rz = root.querySelector('[data-ol-resize]')
  if (rz) {
    rz.addEventListener('mousedown', (e) => {
      e.preventDefault()
      const startY = e.clientY
      const startH = state.outline.height
      const panel = rz.closest('.flow-outline')
      const onMove = (ev) => {
        // 向上拖 = 变高（面板在下部，顶边上移增高）。
        const h = Math.max(120, Math.min(560, startH + (startY - ev.clientY)))
        state.outline.height = h
        if (panel) panel.style.height = h + 'px'
      }
      const onUp = () => {
        document.removeEventListener('mousemove', onMove)
        document.removeEventListener('mouseup', onUp)
      }
      document.addEventListener('mousemove', onMove)
      document.addEventListener('mouseup', onUp)
    })
  }
}

// 大纲点击 → 在画布中选中该元素并滚动到可见。选择变化会触发 selection.changed → 大纲高亮同步。
function selectInCanvas (id) {
  const m = state.modeler
  if (!m || !id) return
  try {
    const el = m.get('elementRegistry').get(id)
    if (!el) return
    m.get('selection').select(el)
    // 滚动到可见（连线也支持）。scrollToElement 在 v17 可用；失败则忽略。
    try { m.get('canvas').scrollToElement(el) } catch {}
  } catch {}
}

// 画布 → 大纲同步：只就地重渲各 explorer host 里的大纲 DOM，不动上部定义列表、不碰画布。
// 去抖到下一帧（selection/结构事件很密）。找不到大纲容器时退回整区 refreshView('explorer')。
function refreshOutline () {
  if (state.__outlineRaf) return
  state.__outlineRaf = requestAnimationFrame(() => {
    state.__outlineRaf = null
    let patched = false
    for (const host of Array.from(state.hosts)) {
      if (!host || host.__flowView !== 'explorer' || !host.isConnected) continue
      const hroot = hostRoot(host)
      const cur = hroot && hroot.querySelector('.flow-outline')
      if (!cur) continue
      const tmp = document.createElement('div')
      tmp.innerHTML = outlineHtml()
      const next = tmp.firstElementChild
      if (next) { cur.replaceWith(next); patched = true }
    }
    // 大纲 DOM 就地替换后需重新绑定其事件（点击/折叠/拖拽）。
    if (patched) {
      for (const host of Array.from(state.hosts)) {
        if (!host || host.__flowView !== 'explorer' || !host.isConnected) continue
        const hroot = hostRoot(host)
        if (hroot) bindOutline(hroot)
      }
    } else {
      refreshView('explorer')
    }
  })
}

// ————————————————————— 缩略图预览（content 画布右下角，可拖动视口） —————————————————————
//
// 自建（离线无官方 diagram-js-minimap）：克隆画布 SVG 缩放成缩略图 + 一个视口小方框标当前可见区域，
// 拖动方框即平移画布。主流程/子流程共用同一 modeler，故挂一次即两者皆生效。
// 架构：内容克隆只在图变化时重建（载入/增删，去抖）；视口框只在 viewbox 变化时重定位（pan/zoom 高频，轻量）。

// 取当前 content host 里的画布外壳（.flow-canvas-wrap），缩略图挂在其内（右下角绝对定位）。
function minimapWrap () {
  for (const host of Array.from(state.hosts)) {
    if (!host || host.__flowView !== 'content' || !host.isConnected) continue
    const hroot = hostRoot(host)
    const wrap = hroot && hroot.querySelector('.flow-canvas-wrap')
    if (wrap) return wrap
  }
  return null
}
function minimapEl () {
  const wrap = minimapWrap()
  return wrap && wrap.querySelector('.flow-minimap')
}

// 挂载缩略图 DOM（幂等：已存在则跳过）。在 bootCanvas 建好 modeler 后调用。
function mountMinimap () {
  const wrap = minimapWrap()
  if (!wrap || wrap.querySelector('.flow-minimap')) return
  const mm = document.createElement('div')
  mm.className = 'flow-minimap' + (state.minimap.collapsed ? ' collapsed' : '')
  mm.innerHTML = `
    <div class="flow-mm-head" data-mm-toggle title="折叠 / 展开缩略图">
      <ui5-icon name="map-2" class="flow-mm-ic"></ui5-icon>
      <b>缩略图</b>
      <span class="flow-mm-caret">${state.minimap.collapsed ? '▢' : '—'}</span>
    </div>
    <div class="flow-mm-stage">
      <div class="flow-mm-diagram"></div>
      <svg class="flow-mm-overlay" preserveAspectRatio="xMidYMid meet"><rect class="flow-mm-vp" x="0" y="0" width="0" height="0" rx="1"></rect></svg>
    </div>`
  wrap.appendChild(mm)
  wireMinimap(mm)
  renderMinimapContent()
}

function wireMinimap (mm) {
  // 折叠 / 展开。
  mm.querySelector('[data-mm-toggle]')?.addEventListener('click', () => {
    state.minimap.collapsed = !state.minimap.collapsed
    mm.classList.toggle('collapsed', state.minimap.collapsed)
    const caret = mm.querySelector('.flow-mm-caret')
    if (caret) caret.textContent = state.minimap.collapsed ? '▢' : '—'
    if (!state.minimap.collapsed) { renderMinimapContent() } // 展开时补渲一次
  })
  // 视口框拖动 / 点击跳转：在缩略图舞台上按下即把画布视口中心移到该点，拖动持续平移。
  const stage = mm.querySelector('.flow-mm-stage')
  if (!stage) return
  let dragging = false
  const onDown = (e) => {
    if (state.minimap.collapsed) return
    e.preventDefault(); e.stopPropagation()
    dragging = true
    minimapPanTo(e.clientX, e.clientY)
    document.addEventListener('mousemove', onMove)
    document.addEventListener('mouseup', onUp)
  }
  const onMove = (e) => { if (dragging) minimapPanTo(e.clientX, e.clientY) }
  const onUp = () => { dragging = false; document.removeEventListener('mousemove', onMove); document.removeEventListener('mouseup', onUp) }
  stage.addEventListener('mousedown', onDown)
}

// 重建缩略图内容：克隆画布 SVG，重置视口变换，用全图内容边界（viewbox.inner）作 viewBox 框定。
function renderMinimapContent () {
  const m = state.modeler
  const mm = minimapEl()
  if (!m || !mm || state.minimap.collapsed) return
  let canvas, vb
  try { canvas = m.get('canvas'); vb = canvas.viewbox() } catch { return }
  const inner = vb && vb.inner
  const holder = mm.querySelector('.flow-mm-diagram')
  if (!holder) return
  if (!inner || !inner.width || !inner.height) { holder.innerHTML = ''; state.minimap.vb = null; return }
  const src = canvas.getContainer().querySelector('svg')
  if (!src) return
  const clone = src.cloneNode(true)
  const vp = clone.querySelector('.viewport')
  if (vp) vp.removeAttribute('transform')        // 用 viewBox 框定，去掉当前 pan/zoom 变换
  clone.querySelectorAll('defs').forEach((d) => d.remove()) // 去 defs 避免 marker id 重复；边退化为直线，缩略图足够
  const pad = Math.max(inner.width, inner.height) * 0.06 + 12
  const vx = inner.x - pad, vy = inner.y - pad, vw = inner.width + 2 * pad, vh = inner.height + 2 * pad
  clone.setAttribute('viewBox', `${vx} ${vy} ${vw} ${vh}`)
  clone.setAttribute('preserveAspectRatio', 'xMidYMid meet')
  clone.removeAttribute('width'); clone.removeAttribute('height')
  clone.setAttribute('width', '100%'); clone.setAttribute('height', '100%')
  clone.style.pointerEvents = 'none'
  holder.innerHTML = ''
  holder.appendChild(clone)
  state.minimap.vb = { x: vx, y: vy, w: vw, h: vh }
  updateMinimapViewport()
}

// 重定位视口小方框（覆盖层 SVG 用与缩略图相同的 viewBox，故方框坐标 = 画布 viewbox 的 x/y/w/h）。
function updateMinimapViewport () {
  const m = state.modeler
  const mm = minimapEl()
  const mmvb = state.minimap.vb
  if (!m || !mm || !mmvb || state.minimap.collapsed) return
  const overlay = mm.querySelector('.flow-mm-overlay')
  const rect = mm.querySelector('.flow-mm-vp')
  if (!overlay || !rect) return
  overlay.setAttribute('viewBox', `${mmvb.x} ${mmvb.y} ${mmvb.w} ${mmvb.h}`)
  let vb
  try { vb = m.get('canvas').viewbox() } catch { return }
  rect.setAttribute('x', vb.x); rect.setAttribute('y', vb.y)
  rect.setAttribute('width', Math.max(1, vb.width)); rect.setAttribute('height', Math.max(1, vb.height))
}

// 缩略图坐标（屏幕像素）→ 画布坐标，把画布视口中心移到该点（保持缩放，仅平移）。
function minimapPanTo (clientX, clientY) {
  const m = state.modeler
  const mm = minimapEl()
  if (!m || !mm) return
  const overlay = mm.querySelector('.flow-mm-overlay')
  if (!overlay || !overlay.getScreenCTM) return
  const ctm = overlay.getScreenCTM()
  if (!ctm) return
  const pt = overlay.createSVGPoint()
  pt.x = clientX; pt.y = clientY
  const p = pt.matrixTransform(ctm.inverse()) // 缩略图 viewBox = 画布坐标，直接得画布坐标
  try {
    const canvas = m.get('canvas')
    const vb = canvas.viewbox()
    canvas.viewbox({ x: p.x - vb.width / 2, y: p.y - vb.height / 2, width: vb.width, height: vb.height })
  } catch {}
}

// 去抖重建缩略图内容（结构变化/载入触发，事件密，合并到下一帧）。
function scheduleMinimapRender () {
  if (state.__mmRaf) return
  state.__mmRaf = requestAnimationFrame(() => { state.__mmRaf = null; renderMinimapContent() })
}
// 去抖重定位视口框（viewbox 变化高频）。
function scheduleMinimapViewport () {
  if (state.__mmVpRaf) return
  state.__mmVpRaf = requestAnimationFrame(() => { state.__mmVpRaf = null; updateMinimapViewport() })
}
function propertyHtml () {
  const tab = state.propTab
  const tb = (k, ic, label) => `<button class="flow-ptab ${tab === k ? 'on' : ''}" data-ptab="${k}"><ui5-icon name="${ic}"></ui5-icon> ${label}</button>`
  const tabs = `<div class="flow-ptabs">
    ${tb('node', 'detail-view', '节点属性')}
    ${tb('model', 'course-book', `数据模型${state.varSchema.length ? `（${state.varSchema.length}）` : ''}`)}
    ${tb('sim', 'play', '模拟')}
    ${state.diff ? tb('diff', 'compare', '差异') : ''}
  </div>`
  const body = tab === 'model' ? dataModelPanelHtml()
    : tab === 'sim' ? simPanelHtml()
      : tab === 'diff' && state.diff ? diffPanelHtml()
        : nodePropBodyHtml()
  return `<div class="flow-prop-wrap">${tabs}${body}</div>`
}

// 「数据模型」页签主体：内联的流程变量声明编辑器（复用 varRowHtml / state.varSchema / bindVarDialog 的
// data-var-* 事件）。与旧全屏遮罩同能力，但常驻可见、可本地测试。
function dataModelPanelHtml () {
  const rows = state.varSchema.length
    ? state.varSchema.map((d, i) => varRowHtml(d, String(i), 0)).join('')
    : `<div class="flow-vempty"><ui5-icon name="add-product"></ui5-icon><b>还没有变量</b><span>点下方「新增变量」声明本流程用到的变量；对象/数组可展开定义字段结构。</span></div>`
  const vv = state.varValidation || 'lenient'
  const ctxName = state.subNav ? (state.name || state.subNav.subName || '子流程') : (state.name || state.selectedKey || '未保存')
  return `<section class="flow flow-prop">
    <div class="flow-prop-head"><b>数据模型 · 流程变量</b><small>${esc(ctxName)}${state.subNav ? ' · 子流程' : ''}</small></div>
    <div class="flow-prop-body">
      ${state.varError ? `<div class="flow-dialog-err">${esc(state.varError)}</div>` : ''}
      <div class="flow-hint">声明本流程用到的变量（名称/类型/结构/说明），值在发起时传；声明后可在条件、办理人、会签、表单里直接下拉引用。${state.subNav ? '当前编辑的是<b>子流程</b>的变量。' : ''}</div>
      <div class="flow-var-list">${rows}</div>
      <button class="flow-btn primary block" data-var-add><ui5-icon name="add"></ui5-icon> 新增变量</button>
      <div class="flow-var-foot">
        <label class="flow-var-policy">发起校验
          <select data-var-policy>
            <option value="lenient" ${vv === 'lenient' ? 'selected' : ''}>宽松（违规仅提示）</option>
            <option value="strict" ${vv === 'strict' ? 'selected' : ''}>严格（违规拒绝发起）</option>
            <option value="off" ${vv === 'off' ? 'selected' : ''}>关闭（不校验）</option>
          </select>
        </label>
        <button class="flow-btn primary" data-var-save><ui5-icon name="accept"></ui5-icon> 保存声明</button>
      </div>
    </div>
  </section>`
}

// ═══════════════ 「模拟」页签：设计态试跑（后端 /definitions/simulate，无持久实例、无副作用） ═══════════════
// 给样例变量试跑当前画布：算令牌路径 / 网关分支 / userTask 办理人 / businessRuleTask 决策输出，
// 结果渲成 trace 列表 + 画布高亮（复用 ops-console 的 canvas.addMarker 技法，见 styleCss 的 .flow-sim-*）。
function simPanelHtml () {
  recomputeVarPaths()
  const scalars = (state.varPaths || []).filter((p) => !['OBJECT', 'ARRAY'].includes(p.type))
  const factField = (p) => {
    const v = state.sim.facts[p.path] ?? ''
    const head = `<span>${esc(p.label || p.path)} <code>${esc(p.path)}</code>${p.type === 'NUMBER' ? ' <em>数</em>' : ''}</span>`
    if (p.type === 'BOOLEAN') {
      return `<label class="flow-sim-f">${head}<select data-sim-fact="${esc(p.path)}"><option value="" ${v === '' ? 'selected' : ''}>—</option><option value="true" ${v === 'true' ? 'selected' : ''}>true</option><option value="false" ${v === 'false' ? 'selected' : ''}>false</option></select></label>`
    }
    if (p.type === 'ENUM' && (p.enumOptions || []).length) {
      return `<label class="flow-sim-f">${head}<select data-sim-fact="${esc(p.path)}"><option value="">—</option>${p.enumOptions.map((o) => `<option value="${esc(o)}" ${v === String(o) ? 'selected' : ''}>${esc(o)}</option>`).join('')}</select></label>`
    }
    return `<label class="flow-sim-f">${head}<input data-sim-fact="${esc(p.path)}" value="${esc(v)}" placeholder="输入 ${esc(p.path)}"></label>`
  }
  const facts = scalars.length
    ? scalars.map(factField).join('')
    : '<div class="flow-hint">未声明标量变量。可在「数据模型」页签声明，或用下方 JSON 直接给。</div>'
  const r = state.sim.result
  return `<section class="flow flow-prop">
    <div class="flow-prop-head"><b>模拟 · 试跑</b><small>${esc(state.name || state.selectedKey || '当前画布')}</small></div>
    <div class="flow-prop-body">
      <div class="flow-hint">给样例变量试跑当前画布：算令牌路径、网关分支、办理人、决策输出——<b>不建实例、无副作用</b>。</div>
      <div class="flow-sim-facts">${facts}</div>
      <div class="flow-sim-ctx">
        <label class="flow-sim-f"><span>发起人 <code>initiator</code></span><input data-sim-init value="${esc(state.sim.initiator)}" placeholder="如 u_applicant"></label>
        <label class="flow-sim-f"><span>组织 <code>orgId</code></span><input data-sim-org value="${esc(state.sim.org)}" placeholder="如 fin_bj"></label>
      </div>
      <details class="flow-sim-adv" ${state.sim.raw ? 'open' : ''}><summary>高级：JSON 变量（合并覆盖上方表单）</summary>
        <textarea data-sim-raw rows="3" placeholder='{"amount":30000}'>${esc(state.sim.raw)}</textarea></details>
      <button class="flow-btn primary block" data-sim-run ${state.sim.running ? 'disabled' : ''}><ui5-icon name="${state.sim.running ? 'pending' : 'play'}"></ui5-icon> ${state.sim.running ? '模拟中…' : '运行模拟'}</button>
      ${r ? simResultHtml(r) : '<div class="flow-hint" style="margin-top:10px">运行后：画布高亮走过的路径，此处列出网关分支 / 办理人 / 决策 / 提示。</div>'}
    </div>
  </section>`
}

// trace 结果渲染：可达摘要 + 网关分支 / 办理人 / 决策 / 子流程分组 + warnings。
function simResultHtml (r) {
  const warn = (r.warnings || []).length ? `<div class="flow-sim-warn">${r.warnings.map((w) => `<div><ui5-icon name="alert"></ui5-icon> ${esc(w)}</div>`).join('')}</div>` : ''
  const row = (ic, node, txt, bad) => `<div class="flow-sim-row ${bad ? 'bad' : ''}"><ui5-icon name="${ic}"></ui5-icon><b>${esc(node)}</b><span>${txt}</span></div>`
  const gws = (r.gateways || []).map((g) => row('decision', g.node, `${esc(g.type)} → ${(g.taken || []).map(esc).join(', ') || '（无分支）'}`, !(g.taken || []).length)).join('')
  const uts = (r.userTasks || []).map((u) => row('employee', u.node, (u.assignees || []).length ? `办理人：${u.assignees.map(esc).join(', ')}` : '<em>未解析到办理人</em>', !(u.assignees || []).length)).join('')
  const dcs = (r.decisions || []).map((d) => row('table-view', d.node, `${esc(d.decisionKey)} → 命中 ${(d.matchedRules || []).length} 条 · ${esc(JSON.stringify(d.outputs || {}))}`)).join('')
  const subs = (r.subflows || []).map((s) => row('process', s.node, `子流程 → ${esc(s.target || '?')}`)).join('')
  const sts = (r.serviceTasks || []).map((s) => row('activity-2', s.node, esc(s.externalTopic ? `外部 topic：${s.externalTopic}` : s.delegate || '服务任务'))).join('')
  return `<div class="flow-sim-res">
    ${warn}
    <div class="flow-sim-sum"><span class="flow-sim-pill ${r.endReached ? 'ok' : 'no'}">${r.endReached ? '✓ 可达结束' : '⚠ 未达结束'}</span><span>${(r.path || []).length} 节点 · ${(r.flows || []).length} 条边</span></div>
    ${gws ? `<div class="flow-sim-sec">网关分支</div>${gws}` : ''}
    ${uts ? `<div class="flow-sim-sec">办理人</div>${uts}` : ''}
    ${dcs ? `<div class="flow-sim-sec">决策</div>${dcs}` : ''}
    ${subs ? `<div class="flow-sim-sec">子流程</div>${subs}` : ''}
    ${sts ? `<div class="flow-sim-sec">服务任务</div>${sts}` : ''}
  </div>`
}

// facts 智能转型（仿 rules simulator typedInput）：数字/布尔/JSON 自动识别，否则原字符串；空串=不传。
function simCoerce (s) {
  const t = String(s).trim()
  if (t === '') return undefined
  if (t === 'true') return true
  if (t === 'false') return false
  if (/^-?\d+(\.\d+)?$/.test(t)) return Number(t)
  if ((t[0] === '{' && t.slice(-1) === '}') || (t[0] === '[' && t.slice(-1) === ']')) { try { return JSON.parse(t) } catch { /* 原样 */ } }
  return s
}
// 汇总模拟变量：表单标量 → JSON override 合并 → initiator 兜底注入。
function buildSimVars () {
  const out = {}
  for (const [k, v] of Object.entries(state.sim.facts)) { const c = simCoerce(v); if (c !== undefined) out[k] = c }
  const raw = (state.sim.raw || '').trim()
  if (raw) { try { const j = JSON.parse(raw); if (j && typeof j === 'object') Object.assign(out, j) } catch { /* 高级框非法 JSON 忽略 */ } }
  if (state.sim.initiator && out.initiator === undefined) out.initiator = state.sim.initiator
  return out
}
async function runSimulation () {
  if (state.sim.running) return
  let xml
  try { xml = await getXml() } catch (e) { toast('取画布 XML 失败: ' + e.message); return }
  state.sim.running = true; refreshView('property')
  try {
    const body = { bpmnXml: xml, variables: buildSimVars(), initiator: state.sim.initiator || undefined, orgId: state.sim.org || undefined }
    const res = await apiJson('/api/flow/definitions/simulate', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
    state.sim.result = res
    applySimMarkers(res)
  } catch (e) {
    toast('模拟失败: ' + e.message)
    state.sim.result = { warnings: ['模拟失败: ' + e.message], path: [], flows: [] }
    clearSimMarkers()
  }
  state.sim.running = false; refreshView('property')
}
// 画布高亮：走过节点(flow-sim-hit)/边(flow-sim-flow)/userTask(flow-sim-task) 各一色。marker CSS 见 styleCss。
function clearSimMarkers () {
  const m = state.modeler; if (!m) return
  let canvas; try { canvas = m.get('canvas') } catch { return }
  for (const id of state.sim.marked) {
    try { canvas.removeMarker(id, 'flow-sim-hit'); canvas.removeMarker(id, 'flow-sim-flow'); canvas.removeMarker(id, 'flow-sim-task') } catch { /* 元素已删 */ }
  }
  state.sim.marked = []
}
function applySimMarkers (r) {
  clearSimMarkers()
  const m = state.modeler; if (!m) return
  let canvas, reg
  try { canvas = m.get('canvas'); reg = m.get('elementRegistry') } catch { return }
  const mark = (id, cls) => { if (id && reg.get(id)) { try { canvas.addMarker(id, cls); state.sim.marked.push(id) } catch { /* ignore */ } } }
  for (const id of (r.flows || [])) mark(id, 'flow-sim-flow')
  for (const id of (r.path || [])) mark(id, 'flow-sim-hit')
  for (const u of (r.userTasks || [])) mark(u.node, 'flow-sim-task')
}

// ═══════════════ 「差异」页签：同一定义两版本的结构 diff（纯前端，DOMParser 解析 + 画布高亮） ═══════════════
const BPMN_MODEL_NS = 'http://www.omg.org/spec/BPMN/20100524/MODEL'
function typeNameFromLocal (ln) {
  const t = (ln || '').charAt(0).toUpperCase() + (ln || '').slice(1)
  return TYPE_NAME[t] || ln
}
// 解析一份 BPMN XML → { id: {id,type,name,attrs,sig} }，仅取 BPMN 模型命名空间的流程元素（自动排除 DI 布局层）。
function parseFlowElements (xml) {
  const map = {}
  let doc
  try { doc = new DOMParser().parseFromString(xml, 'text/xml') } catch { return map }
  const skip = new Set(['definitions', 'process', 'collaboration', 'extensionElements', 'laneSet', 'lane'])
  for (const el of Array.from(doc.getElementsByTagName('*'))) {
    if (el.namespaceURI !== BPMN_MODEL_NS) continue           // 跳过 bpmndi/omgdc/omgdi 布局层
    const ln = el.localName || ''
    if (skip.has(ln)) continue
    const id = el.getAttribute && el.getAttribute('id')
    if (!id) continue                                         // incoming/outgoing/conditionExpression 无 id → 计入父的子文本
    const attrs = {}
    for (const a of Array.from(el.attributes)) { if (a.localName !== 'id') attrs[a.localName] = a.value }
    let kids = ''
    for (const c of Array.from(el.children)) {
      if (c.namespaceURI === BPMN_MODEL_NS && (c.localName === 'incoming' || c.localName === 'outgoing')) continue // 顺序无关
      kids += (c.localName || '') + ':' + (c.textContent || '').replace(/\s+/g, ' ').trim() + ';'
    }
    const ordered = Object.keys(attrs).sort().map((k) => k + '=' + attrs[k]).join('&')
    map[id] = { id, type: ln, name: attrs.name || '', attrs, sig: ln + '§' + ordered + '§' + kids }
  }
  return map
}
// 结构 diff（按 id）：added=B 有 A 无 / removed=A 有 B 无 / modified=同 id 但签名变（附变化字段）。
function diffFlow (a, b) {
  const added = [], removed = [], modified = []
  for (const id of Object.keys(b)) if (!a[id]) added.push(b[id])
  for (const id of Object.keys(a)) if (!b[id]) removed.push(a[id])
  for (const id of Object.keys(a)) if (b[id] && a[id].sig !== b[id].sig) modified.push({ ...b[id], from: a[id], changes: changedFields(a[id], b[id]) })
  return { added, removed, modified }
}
// 人读的变化字段（名称/条件/办理人/子流程 key/决策… + 其余增删属性键）。
function changedFields (fa, fb) {
  const out = []
  const LABEL = { name: '名称', assignee: '办理人', candidateUsers: '候选人', candidateGroups: '候选组', sourceRef: '起点', targetRef: '终点', default: '默认流', calledElement: '子流程', decisionRef: '决策表' }
  for (const k of new Set([...Object.keys(fa.attrs), ...Object.keys(fb.attrs)])) {
    if ((fa.attrs[k] || '') !== (fb.attrs[k] || '')) out.push(LABEL[k] || k)
  }
  if (fa.sig.split('§')[2] !== fb.sig.split('§')[2]) out.push('条件/扩展')
  return [...new Set(out)]
}
// 版本选项（草稿 + 已发布版本）。
function diffVersionOptions () {
  const d = state.definitions.find((x) => x.key === state.selectedKey)
  const vers = d && Array.isArray(d.versions) ? d.versions : []
  const opts = [{ v: '', label: '草稿 / 当前画布' }]
  for (const v of vers) opts.push({ v: String(v.version), label: 'v' + v.version + (v.version === d.activeVersion ? ' · 当前' : '') })
  return opts
}
// 取某侧 XML：'' = 当前画布（getXml，含未存改动）；否则按版本号取已存 XML。
async function diffVersionXml (sel) {
  if (sel === '' || sel == null) return await getXml()
  const detail = await apiJson('/api/flow/definitions/' + enc(state.selectedKey) + '?version=' + enc(sel))
  if (!detail || !detail.bpmnXml) throw new Error('v' + sel + ' 无 XML')
  return detail.bpmnXml
}
function openDiff () {
  if (!state.selectedKey) { toast('请先选择一个流程定义'); return }
  const d = state.definitions.find((x) => x.key === state.selectedKey)
  const vers = d && Array.isArray(d.versions) ? d.versions : []
  // 默认：从=当前生效版本（无则最旧），到=草稿/当前画布。
  const from = vers.length ? String(d.activeVersion || vers[vers.length - 1].version) : ''
  state.diff = { va: from, vb: '', result: null, running: false, marked: [], error: '' }
  clearSimMarkers()
  state.propTab = 'diff'
  refreshView('property')
}
function exitDiff () {
  clearDiffMarkers()
  state.diff = null
  if (state.propTab === 'diff') state.propTab = 'node'
  refreshView('property')
}
async function runDiff () {
  const dd = state.diff
  if (!dd || dd.running) return
  if (dd.va === dd.vb) { dd.error = '请选择两个不同的版本'; refreshView('property'); return }
  dd.running = true; dd.error = ''; refreshView('property')
  try {
    const [xa, xb] = await Promise.all([diffVersionXml(dd.va), diffVersionXml(dd.vb)])
    dd.result = diffFlow(parseFlowElements(xa), parseFlowElements(xb))
    applyDiffMarkers(dd.result)
  } catch (e) {
    dd.error = '对比失败: ' + e.message
    dd.result = null
    clearDiffMarkers()
  }
  dd.running = false
  refreshView('property')
}
// 画布高亮：新增=绿(flow-diff-add) / 修改=琥珀(flow-diff-chg)（删除项不在当前画布，仅列表）。
function clearDiffMarkers () {
  const m = state.modeler; if (!m || !state.diff) return
  let canvas; try { canvas = m.get('canvas') } catch { return }
  for (const id of state.diff.marked) {
    try { canvas.removeMarker(id, 'flow-diff-add'); canvas.removeMarker(id, 'flow-diff-chg') } catch { /* 已删 */ }
  }
  state.diff.marked = []
}
function applyDiffMarkers (res) {
  clearDiffMarkers()
  const m = state.modeler; if (!m || !state.diff) return
  let canvas, reg; try { canvas = m.get('canvas'); reg = m.get('elementRegistry') } catch { return }
  const mark = (id, cls) => { if (id && reg.get(id)) { try { canvas.addMarker(id, cls); state.diff.marked.push(id) } catch { /* ignore */ } } }
  for (const e of (res.added || [])) mark(e.id, 'flow-diff-add')
  for (const e of (res.modified || [])) mark(e.id, 'flow-diff-chg')
}
// 点击差异行 → 选中并居中该元素（存在于当前画布时）。
function centerOnElement (id) {
  const m = state.modeler; if (!m) return
  let reg, el; try { reg = m.get('elementRegistry'); el = reg.get(id) } catch { return }
  if (!el) { toast('该元素不在当前画布'); return }
  try { m.get('selection').select(el) } catch { /* ignore */ }
  try { m.get('canvas').scrollToElement(el) } catch { /* 旧版无此 API */ }
}
function diffPanelHtml () {
  const dd = state.diff
  if (!dd) return ''
  const opts = diffVersionOptions()
  const sel = (which, cur) => `<select data-diff-${which}>${opts.map((o) => `<option value="${esc(o.v)}" ${o.v === cur ? 'selected' : ''}>${esc(o.label)}</option>`).join('')}</select>`
  const r = dd.result
  const label = (v) => v === '' ? '草稿' : ('v' + v)
  const IC = { userTask: 'employee', serviceTask: 'activity-2', exclusiveGateway: 'decision', parallelGateway: 'decision', inclusiveGateway: 'decision', eventBasedGateway: 'decision', sequenceFlow: 'chain-link', callActivity: 'process', businessRuleTask: 'table-view', startEvent: 'circle-task', endEvent: 'circle-task', subProcess: 'process' }
  const row = (e, kind) => {
    const nm = e.name || typeNameFromLocal(e.type)
    const chg = kind === 'chg' && e.changes && e.changes.length ? ` <span class="flow-diff-fields">${e.changes.map(esc).join(' · ')}</span>` : ''
    return `<div class="flow-diff-row ${kind}" data-diff-goto="${esc(e.id)}"><ui5-icon name="${IC[e.type] || 'circle-task'}"></ui5-icon><b>${esc(nm)}</b><code>${esc(e.id)}</code>${chg}</div>`
  }
  const group = (title, arr, kind) => arr.length ? `<div class="flow-sim-sec">${title}（${arr.length}）</div>${arr.map((e) => row(e, kind)).join('')}` : ''
  let res
  if (dd.running) res = '<div class="flow-hint" style="margin-top:10px">对比中…</div>'
  else if (r) {
    const total = r.added.length + r.removed.length + r.modified.length
    res = `<div class="flow-sim-res">
      <div class="flow-sim-sum"><span class="flow-sim-pill ${total ? 'no' : 'ok'}">${total ? total + ' 处差异' : '✓ 无结构差异'}</span><span>+${r.added.length} 新增 · −${r.removed.length} 删除 · ~${r.modified.length} 修改</span></div>
      ${group('新增', r.added, 'add')}
      ${group('修改', r.modified, 'chg')}
      ${group('删除', r.removed, 'del')}
    </div>`
  } else res = '<div class="flow-hint" style="margin-top:10px">选「从 / 到」两版本后点「对比」：结构差异列于此，新增/修改在画布高亮（绿/琥珀）。</div>'
  return `<section class="flow flow-prop">
    <div class="flow-prop-head"><b>版本对比 · 结构 diff</b><small>${esc(state.name || state.selectedKey || '')} · ${esc(label(dd.va))} → ${esc(label(dd.vb))}</small></div>
    <div class="flow-prop-body">
      <div class="flow-hint">对比同一定义两个版本的节点与连线：新增 / 删除 / 修改（名称·条件·办理人·子流程 key 等）。基于当前画布高亮，删除项仅列于下方。</div>
      <div class="flow-diff-pick">
        <label class="flow-sim-f"><span>从（旧）</span>${sel('va', dd.va)}</label>
        <ui5-icon name="arrow-right" class="flow-diff-arrow"></ui5-icon>
        <label class="flow-sim-f"><span>到（新）</span>${sel('vb', dd.vb)}</label>
      </div>
      ${dd.error ? `<div class="flow-dialog-err">${esc(dd.error)}</div>` : ''}
      <div class="flow-diff-btns">
        <button class="flow-btn primary" data-diff-run ${dd.running ? 'disabled' : ''}><ui5-icon name="compare"></ui5-icon> ${dd.running ? '对比中…' : '对比'}</button>
        <button class="flow-btn" data-diff-exit><ui5-icon name="decline"></ui5-icon> 退出对比</button>
      </div>
      ${res}
    </div>
  </section>`
}

// 「节点属性」页签主体：选中元素的属性面板 / 未选中时的流程属性（DAM + 变量摘要）。整块含既有对话框。
function nodePropBodyHtml () {
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
        ${sec('流程变量')}
        <div class="flow-var-summary">
          ${state.varSchema.length
            ? state.varSchema.map((d) => `<span class="flow-var-chip" title="${esc(d.description || d.label || d.name)}"><b>${esc(d.label || d.name)}</b><em>${esc(VAR_TYPE_LABEL[d.type] || d.type || '')}</em></span>`).join('')
            : '<div class="flow-hint">未声明变量。声明后可在条件、办理人、会签、表单里直接下拉引用。</div>'}
        </div>
        <button class="flow-btn primary block" data-open-vars><ui5-icon name="course-book"></ui5-icon> 编辑变量声明${state.varSchema.length ? `（${state.varSchema.length}）` : ''}</button>
        <div class="flow-sec">元素属性</div>
        <cmx-empty-state icon="detail-view" title="点击画布上的节点" description="在此配置属性" size="sm"></cmx-empty-state>
      </div>
      ${varDialogHtml()}
    </section>`
  }
  const b = el.businessObject || {}
  let h = ''
  h += field('名称', 'name', b.name || '', '')
  if (el.type === 'bpmn:UserTask') {
    h += userTaskTabsHtml(el, b)
  }
  if (el.type === 'bpmn:ServiceTask') {
    h += sec('服务实现')
    h += field('委托实现 (delegateExpression)', 'delegate', getServiceDelegate(b),
      '引擎按此键在 delegate 注册表查执行体，或 http 模式外呼。如 ${riskDelegate}')
    h += `<div class="flow-hint">服务任务必须指定委托实现，否则发布时编译失败。前缀无关（flowable/camunda/cmx 皆可）。</div>`
  }
  if (el.type === 'bpmn:BusinessRuleTask') {
    h += sec('决策表')
    h += field('决策表 key (decisionRef)', 'decisionRef', getRuleDecisionRef(b),
      '如 approval_matrix；令牌到达时跑该决策表，输出写回变量供后续网关分支')
    h += `<div class="flow-hint">业务规则任务必须指定决策表 key，否则发布时编译失败。决策表经 /decisions 注册。</div>`
  }
  if (el.type === 'bpmn:BoundaryEvent') {
    h += sec('边界定时器')
    h += field('超时时长 (timeDuration)', 'timerDuration', getTimerDuration(b),
      'ISO 8601 相对时长：PT24H(24时)、PT30M(30分)、P1D(1天)、PT1H30M。仅支持相对时长')
    const interrupting = getBoundaryInterrupting(b)
    h += `<div class="flow-field"><label>触发方式</label>
      <div class="flow-mode">
        <button class="flow-mode-opt ${interrupting ? 'on' : ''}" data-boundary-mode="interrupt">
          <b>中断型</b><small>超时中断宿主任务，令牌走升级分支</small></button>
        <button class="flow-mode-opt ${!interrupting ? 'on' : ''}" data-boundary-mode="noninterrupt">
          <b>非中断型</b><small>超时发旁路令牌（催办），宿主任务继续</small></button>
      </div>
      <div class="flow-hint">边界事件须先在画布上「附着」到某任务（拖到任务边框）。</div></div>`
  }
  if (el.type === 'bpmn:IntermediateCatchEvent') {
    h += sec('消息捕获')
    h += field('消息名 (message)', 'messageName', getMessageName(b),
      '外部系统回调时用此名唤醒。如 verdictReceived')
    h += field('相关键变量 (correlationVar)', 'correlationVar', getCorrelationVar(b),
      '可空；跨实例定位时按此实例变量匹配相关键。如 orderId')
    h += `<div class="flow-hint">令牌到此挂起等外部消息（POST /messages/correlate 唤醒）。</div>`
  }
  if (el.type === 'bpmn:EndEvent') {
    const isTerminate = hasEventDefLocal(b, 'TerminateEventDefinition')
    h += sec('结束类型')
    h += `<div class="flow-mode">
      <button class="flow-mode-opt ${!isTerminate ? 'on' : ''}" data-end-mode="normal">
        <b>普通结束</b><small>本分支结束，其它分支继续</small></button>
      <button class="flow-mode-opt ${isTerminate ? 'on' : ''}" data-end-mode="terminate">
        <b>终止结束</b><small>一票否决：终止整个流程实例</small></button>
    </div>`
  }
  if (el.type === 'bpmn:StartEvent') {
    h += `<div class="flow-hint">起点事件。发起表单绑定在「流程属性」的 cmx:startFormKey（选中画布空白配置）。</div>`
  }
  if (el.type === 'bpmn:CallActivity') {
    const calledKey = getCalledKey(b)
    const calledElement = b.get?.('calledElement') || ''
    const dimKey = getFormAttr(b, 'dimKey') || 'org'
    // 模式判定：显式记 state.subMode[el.id]（解决新建节点两者皆空时固定模式弹回的死循环 F5）。
    const explicit = state.subMode[el.id]
    const fixedMode = explicit ? explicit === 'fixed' : (!!calledElement && !calledKey)
    h += sec('子流程')
    h += `<div class="flow-mode">
      <button class="flow-mode-opt ${!fixedMode ? 'on' : ''}" data-sub-mode="org">
        <b>按维度路由</b><small>各维度取值跑各自子流程（推荐）</small></button>
      <button class="flow-mode-opt ${fixedMode ? 'on' : ''}" data-sub-mode="fixed">
        <b>固定子流程</b><small>所有情形同一个（少见）</small></button>
    </div>`
    if (fixedMode) {
      h += field('固定子流程 key (calledElement)', 'calledElement', calledElement, '写死具体子流程定义 key，不区分维度')
    } else {
      // RD4：路由维度选择器（cmx:dimKey）。默认组织机构；可选其它已注册字典。
      const dimOpts = dimSelectOptionsHtml(dimKey)
      h += `<label class="flow-field"><span>路由维度 (cmx:dimKey)</span><select data-prop="dimKey">${dimOpts}</select><div class="flow-hint">按哪个字典区分子流程：组织机构 / 法人公司 / 其它已注册维度。各挂载点可选不同维度。</div></label>`
      h += field('逻辑子流程名 (cmx:calledKey)', 'calledKey', calledKey, '如 fin_review；运行期按发起实例的该维度取值解析到具体子流程')
      const nBind = state.bindingDialog?.calledKey === calledKey ? state.bindings.length : null
      h += `<div class="flow-field">
        <button class="flow-btn block" data-open-bindings ${calledKey ? '' : 'disabled'}>
          <ui5-icon name="org-chart"></ui5-icon> 配置维度绑定${nBind != null ? `（${nBind}）` : ''}</button>
        <div class="flow-hint">${calledKey ? '为该维度各取值指定对应的具体子流程；未匹配的取值沿维度字典树向上继承，最后落到默认绑定。' : '先填逻辑子流程名，再配置维度绑定。'}</div>
      </div>`
    }
    // ★ 主入口：打开全屏子流程编辑器（固定=直接编辑；组织路由=变体侧栏+编辑；绑定+设计合一）。
    const canEdit = fixedMode ? !!calledElement : !!calledKey
    h += `<div class="flow-field">
      <button class="flow-btn primary block" data-edit-subflow ${canEdit ? '' : 'disabled'}>
        <ui5-icon name="edit"></ui5-icon> 编辑子流程${!fixedMode && calledKey ? '（各组织变体）' : ''}</button>
      <div class="flow-hint">${canEdit ? '打开全屏子流程设计器，界面同主流程；' + (!fixedMode ? '左侧可切换各组织变体、为未配置组织新建子流程。' : '编辑该固定子流程。') + ' 也可双击本节点打开。' : '先填' + (fixedMode ? '固定子流程 key' : '逻辑子流程名') + '，再编辑。'}</div>
    </div>`
    // 变量映射（P5）：主↔子变量传递。in=启动子实例时主→子拷贝；out=子完成回归时子→主。
    h += varMappingHtml(b)
  }
  if (el.type === 'bpmn:SequenceFlow') {
    h += sec('流转条件')
    const ce = b.conditionExpression
    const raw = (ce && ce.body) || ''
    h += conditionBuilderHtml(raw)
  }
  if (el.type === 'bpmn:ExclusiveGateway' || el.type === 'bpmn:InclusiveGateway') {
    h += sec('默认流 (default)')
    h += defaultFlowFieldHtml(el, b)
  }
  return `<section class="flow flow-prop">
    <div class="flow-prop-head"><b>${esc(typeName(el))}</b><small>${esc((el.type || '').replace('bpmn:', ''))} · ${esc(el.id)}</small></div>
    <div class="flow-prop-body">${h}</div>
    ${bindingDialogHtml()}
  </section>`
}

// ————————————————————— UserTask 属性面板分页签（P3） —————————————————————
//
// 设计：把原来平铺的 6 个裸文本框重构成分页签（基本/办理人/审批方式/表单/抄送）。办理人页签
// 用「类型单选 + IAM 联动选择器」替代手打 role(FIN)——选择器读 /identity/{entity}（内建身份）
// 或退化为文本框（外接身份无列表时）。所有写回仍走既有 $attrs / flowable:* 契约，编译器不变。

const UT_TABS = [
  { key: 'basic', label: '基本' },
  { key: 'assignee', label: '办理人' },
  { key: 'approval', label: '审批方式' },
  { key: 'form', label: '表单' },
  { key: 'cc', label: '抄送' },
]

// 办理人类型：值写回哪个属性 + 用什么表达式前缀。
const ASSIGNEE_KINDS = [
  { key: 'user', label: '指定人员', ent: 'users', expr: (v) => v, attr: 'assignee', hint: '直派给某个用户' },
  { key: 'role', label: '角色', ent: 'roles', expr: (v) => `role(${v})`, attr: 'candidates', hint: '该角色下的用户进候选池' },
  { key: 'position', label: '岗位', ent: 'positions', expr: (v) => `position(${v})`, attr: 'candidates', hint: '该岗位下的用户进候选池' },
  { key: 'org', label: '部门', ent: 'orgs', expr: (v) => `org(${v})`, attr: 'candidates', hint: '该部门（含子树）下的用户' },
  { key: 'orgLeader', label: '部门领导', ent: 'orgs', expr: (v) => (v ? `orgLeader(${v})` : 'orgLeader'), attr: 'candidates', hint: '指定部门或实例组织的领导' },
  { key: 'initiator', label: '发起人本人', ent: null, expr: () => 'initiator', attr: 'candidates', hint: '流程发起人自己办理' },
  { key: 'initiatorLeader', label: '发起人上级', ent: null, expr: () => 'initiatorLeader', attr: 'candidates', hint: '发起人所属部门的领导' },
  { key: 'expr', label: '表达式', ent: null, expr: (v) => v, attr: 'candidates', hint: '手写候选表达式（逃生舱）' },
]

// 读当前 userTask 的办理人现状 → 推断类型 + 值（用于回填单选与选择器）。
function readAssignee (b) {
  const assignee = (b.get?.('flowable:assignee')) || (b.$attrs && (b.$attrs['flowable:assignee'] || b.$attrs.assignee)) || ''
  const groups = (b.get?.('flowable:candidateGroups')) || (b.$attrs && (b.$attrs['flowable:candidateGroups'] || b.$attrs.candidateGroups)) || ''
  const cands = (b.$attrs && (b.$attrs['cmx:candidates'] || b.$attrs.candidates)) || ''
  if (assignee) return { kind: 'user', value: assignee }
  if (groups) return { kind: 'role', value: groups } // candidateGroups 历史 = 角色
  const expr = cands.trim()
  if (!expr) return { kind: 'user', value: '' }
  // 多项（逗号分隔混合）→ 表达式类型（逃生舱），不误判成单一 role()。
  if (expr.includes(',')) return { kind: 'expr', value: expr }
  // 单条表达式尽力反解类型。
  const m = expr.match(/^(\w+)\((.*)\)$/)
  if (m) {
    const k = m[1].toLowerCase()
    if (k === 'role') return { kind: 'role', value: m[2] }
    if (k === 'position' || k === 'pos') return { kind: 'position', value: m[2] }
    if (k === 'org' || k === 'dept') return { kind: 'org', value: m[2] }
    if (k === 'orgleader') return { kind: 'orgLeader', value: m[2] }
  }
  if (expr === 'initiator') return { kind: 'initiator', value: '' }
  if (expr === 'orgLeader') return { kind: 'orgLeader', value: '' }
  if (expr === 'initiatorLeader') return { kind: 'initiatorLeader', value: '' }
  return { kind: 'expr', value: expr }
}

// 身份选择器：有缓存列表 → 下拉；否则文本框（外接身份或未加载）。data-idn-pick 触发写回。
function idnSelectHtml (ent, curValue) {
  if (!ent) return ''
  const list = state.idnCache[ent]
  if (!Array.isArray(list)) {
    // 未加载 → 文本框 + 懒加载（bind 时触发 ensureIdn）。
    return `<input data-idn-text value="${esc(curValue)}" placeholder="输入 code/id（选择器加载中…）">`
  }
  if (!list.length) {
    return `<input data-idn-text value="${esc(curValue)}" placeholder="无内建数据，手填 code/id">`
  }
  const opts = list.map((it) => {
    const val = ent === 'users' ? it.id : it.code
    const label = ent === 'users' ? (it.name || it.username || it.id) : `${it.name || it.code}（${it.code}）`
    return `<option value="${esc(val)}" ${val === curValue ? 'selected' : ''}>${esc(label)}</option>`
  }).join('')
  return `<select data-idn-pick><option value="">— 选择 —</option>${opts}</select>`
}

function userTaskTabsHtml (el, b) {
  const tabs = UT_TABS.map((t) => `<button class="flow-uttab ${state.utTab === t.key ? 'on' : ''}" data-uttab="${t.key}">${esc(t.label)}</button>`).join('')
  let body = ''
  if (state.utTab === 'basic') {
    body = `<div class="flow-hint" style="margin-bottom:8px">节点 id：<code>${esc(el.id)}</code>。名称在上方「名称」栏改。</div>`
    body += field('文档/说明 (documentation)', 'documentation', (b.documentation && b.documentation[0] && b.documentation[0].text) || '', '本环节办理说明（可空）')
  } else if (state.utTab === 'assignee') {
    body = assigneeTabHtml(b)
  } else if (state.utTab === 'approval') {
    body = approvalTabHtml(b)
  } else if (state.utTab === 'form') {
    body = formTabHtml(el, b)
  } else if (state.utTab === 'cc') {
    body = ccTabHtml(b)
  }
  return `<div class="flow-uttabs">${tabs}</div><div class="flow-utbody">${body}</div>`
}

function assigneeTabHtml (b) {
  const inferred = readAssignee(b)
  // 修「办理人类型只能选指定人员」：切到需值类型（角色/岗位/部门…）时值暂空，纯靠 readAssignee
  // 反推会回落 user（L536）→ 类型弹回。故属性全空时以 state 里记忆的显式选择为准（同 F5 subMode）。
  const elId = state.selectedElement && state.selectedElement.id
  const isEmpty = inferred.kind === 'user' && inferred.value === ''
  const cur = (isEmpty && elId && state.assigneeKind[elId])
    ? { kind: state.assigneeKind[elId], value: '' }
    : inferred
  // ②：多实例节点 → 引导逐元素派人写法（用「表达式」类型引用 elementVariable 字段）。
  const mi = readMultiInstance(b)
  let miHint = ''
  if (mi) {
    const ev = mi.elementVariable || 'item'
    const col = mi.collection || '(集合变量)'
    const q = (s) => '${' + ev + s + '}'
    miHint = `<div style="border:1px solid var(--line-soft,#eaeef2);border-radius:8px;padding:8px 10px;margin-bottom:10px;background:#f6f8fa;font-size:12px;line-height:1.8">
      <b><ui5-icon name="multiselect-all"></ui5-icon> 会签/或签逐元素派人（②）</b>
      <div>本节点按「<code>${esc(col)}</code>」展开，每个元素派一人。选「表达式」类型，引用元素 <code>${esc(ev)}</code> 的字段：</div>
      <div>· 直派该元素的负责人：<code>${esc(q('.ownerUser'))}</code></div>
      <div>· 按该元素的角色解析：<code>role(${esc(q('.ownerRole'))})</code></div>
      <div>· 集合本身即人员列表：<code>${esc(q(''))}</code></div>
    </div>`
  }
  const kinds = ASSIGNEE_KINDS.map((k) => `<button class="flow-akind ${cur.kind === k.key ? 'on' : ''}" data-akind="${k.key}" title="${esc(k.hint)}">${esc(k.label)}</button>`).join('')
  const def = ASSIGNEE_KINDS.find((k) => k.key === cur.kind) || ASSIGNEE_KINDS[0]
  let picker = ''
  if (def.key === 'initiator' || def.key === 'initiatorLeader') {
    picker = `<div class="flow-hint">无需选择：运行期按流程发起人自动解析。</div>`
  } else if (def.key === 'expr') {
    picker = `<input data-akind-val value="${esc(cur.value)}" placeholder="如 role(fin),position(cfo)，逗号分隔混合；多实例可用 ${'${'}item.field}">`
  } else if (def.key === 'orgLeader') {
    picker = `<div class="flow-hint" style="margin-bottom:5px">选部门 = 该部门领导；留空 = 实例发起组织的领导。</div>${idnSelectHtml('orgs', cur.value)}`
  } else {
    picker = idnSelectHtml(def.ent, cur.value)
  }
  return `${miHint}<div class="flow-field"><label>办理人类型</label><div class="flow-akinds">${kinds}</div>
    <div class="flow-hint">${esc(def.hint)}</div></div>
    <div class="flow-field"><label>${esc(def.label)}${def.ent ? '（联动身份库）' : ''}</label>${picker}</div>`
}

function approvalTabHtml (b) {
  const mi = readMultiInstance(b)
  const modes = [
    { key: 'single', label: '单人', hint: '一个办理人办结即过' },
    { key: 'parallel', label: '并行会签', hint: '所有人同时办，按完成条件收口' },
    { key: 'sequential', label: '顺序或签', hint: '逐个办理' },
  ]
  const cur = !mi ? 'single' : (mi.sequential ? 'sequential' : 'parallel')
  const opts = modes.map((m) => `<button class="flow-akind ${cur === m.key ? 'on' : ''}" data-approval="${m.key}" title="${esc(m.hint)}">${esc(m.label)}</button>`).join('')
  let detail = ''
  if (cur !== 'single') {
    const col = mi ? mi.collection : ''
    // U7：常见错误——把集合写成表达式 ${xxx} 或带空格。给软提示（不阻断）。
    const colWarn = /[$\s{}]/.test(col) ? '<div class="flow-hint" style="color:var(--red,#cf222e)">⚠ 集合变量应是纯变量名（如 approvers），不要写 ${} 或空格</div>' : ''
    detail = `<div class="flow-field"><label>集合变量 (collection)</label>
        <input data-mi-f="collection" value="${esc(col)}" placeholder="如 approvers；其值为数组，按元素展开办理人" list="flow-mi-collections">
        <datalist id="flow-mi-collections">${varPathOptionsHtml({ collectionOnly: true })}</datalist>
        ${colWarn}
        <div class="flow-hint">会签/或签按此数组变量的元素个数展开子任务。${state.varPaths.some((p) => p.isCollection) ? '↑ 可从已声明的数组变量选。' : '（声明数组类型变量后可下拉选）'}</div></div>
      <div class="flow-field"><label>元素变量 (elementVariable)</label>
        <input data-mi-f="elementVariable" value="${esc(mi ? mi.elementVariable : '')}" placeholder="如 approver；每个子任务把当前元素写入此变量"></div>
      <div class="flow-field"><label>完成条件 (completionCondition)</label>
        <input data-mi-f="completionCondition" value="${esc(mi ? mi.completionCondition : '')}" placeholder="如 \${nrOfCompletedInstances/nrOfInstances >= 0.5}（过半）">
        <div class="flow-hint">命中即提前收口剩余子任务；可读内置计数 nrOfInstances 等。</div></div>`
  }
  return `<div class="flow-field"><label>审批方式</label><div class="flow-akinds">${opts}</div></div>${detail}`
}

function formTabHtml (el, b) {
  let h = field('绑定表单 (cmx:formKey)', 'formKey', getFormAttr(b, 'formKey'), '如 pay.review；办理人点开待办时渲染此表单')
  h += selectField('表单模式 (formMode)', 'formMode',
    [{ value: 'approve', label: '审批（同意/驳回）' }, { value: 'edit', label: '可编辑' }, { value: 'readonly', label: '只读' }],
    getFormAttr(b, 'formMode') || 'approve')
  h += field('可写字段 (formFields)', 'formFields', getFormAttr(b, 'formFields'), '逗号分隔；限制本环节可改哪些字段（可空）')
  const fk = getFormAttr(b, 'formKey')
  const wsId = wsNodeIdFor(state.selectedKey, el.id)
  h += `<div class="flow-field">
    <button class="flow-btn primary block" data-edit-ws-node ${fk ? '' : 'disabled'}>
      <ui5-icon name="form"></ui5-icon> 编辑表单工作台</button>
    <div class="flow-hint">${fk
      ? `打开工作区节点对话框编辑「${esc(wsId)}」（content=业务表单/菜单、property=业务视图）；保存后自动把 formKey=<b>${esc(fk)}</b> 绑定到该工作台。`
      : '先填「绑定表单 (cmx:formKey)」再编辑工作台。'}</div>
  </div>`
  return h
}

function ccTabHtml (b) {
  const cc = (b.$attrs && (b.$attrs['cmx:cc'] || b.$attrs.cc)) || ''
  return `<div class="flow-field"><label>抄送 (cmx:cc)</label>
    <input data-prop="cc" value="${esc(cc)}" placeholder="如 role(audit),user(u_ceo)；办结时知会这些人（只读旁路）">
    <div class="flow-hint">抄送不阻塞流程，收件人在「抄送我的」看到只读副本。支持 role()/position()/org()/user() 混合。</div></div>`
}

// 读 userTask 的 multiInstance 现状（从 businessObject.loopCharacteristics）。
function readMultiInstance (b) {
  const lc = b.loopCharacteristics
  if (!lc) return null
  return {
    sequential: !!lc.isSequential,
    collection: lc.collection || lc.get?.('collection') || (lc.$attrs && (lc.$attrs['flowable:collection'] || lc.$attrs.collection)) || '',
    elementVariable: lc.elementVariable || lc.get?.('elementVariable') || '',
    completionCondition: (lc.completionCondition && (lc.completionCondition.body || lc.completionCondition)) || '',
  }
}

// ————————————————————— 子流程变量映射（P5） —————————————————————
//
// 存 cmx:inVars / cmx:outVars 属性（`source:target` 逗号分隔），规避 bpmn-js moddle 扩展注册
// （与 candidates/cc 同一 $attrs 写回纪律）。编译器 parse_attr_var_mappings 兜底读它，且结构化
// <in>/<out> 若存在仍优先（见 compiler.rs）。in=主→子（启动拷贝），out=子→主（回归拷贝）。

// 属性串 "a:b, c" → 行数组 [{source,target}]。
function parseVarMap (raw) {
  return String(raw || '').split(',').map((s) => s.trim()).filter(Boolean).map((pair) => {
    const i = pair.indexOf(':')
    if (i < 0) return { source: pair, target: pair }
    return { source: pair.slice(0, i).trim(), target: pair.slice(i + 1).trim() || pair.slice(0, i).trim() }
  }).filter((m) => m.source)
}
// 行数组 → 属性串（省略 target===source 的冒号）。
function serializeVarMap (rows) {
  return rows.filter((r) => (r.source || '').trim())
    .map((r) => { const s = r.source.trim(); const t = (r.target || '').trim(); return (!t || t === s) ? s : `${s}:${t}` })
    .join(', ')
}
function getVarMapAttr (b, name) {
  return (b && b.$attrs && (b.$attrs['cmx:' + name] || b.$attrs[name])) || ''
}

function varMapRowsHtml (dir, rows) {
  if (!rows.length) return `<div class="flow-vm-empty">无映射（${dir === 'in' ? '启动时全量传主实例变量' : '完成时全量回写'}）</div>`
  return rows.map((r, i) => `<div class="flow-vm-row" data-vm-dir="${dir}" data-vm-i="${i}">
    <input class="flow-vm-src" data-vm-f="source" value="${esc(r.source)}" placeholder="${dir === 'in' ? '主实例变量' : '子实例变量'}">
    <span class="flow-vm-arr">→</span>
    <input class="flow-vm-tgt" data-vm-f="target" value="${esc(r.target)}" placeholder="${dir === 'in' ? '子实例变量' : '主实例变量'}">
    <button class="flow-icon-btn danger" data-vm-del="${dir}:${i}" title="删除"><ui5-icon name="decline"></ui5-icon></button>
  </div>`).join('')
}

function varMappingHtml (b) {
  const inRows = parseVarMap(getVarMapAttr(b, 'inVars'))
  const outRows = parseVarMap(getVarMapAttr(b, 'outVars'))
  return `${sec('变量映射')}
    <div class="flow-vm">
      <div class="flow-vm-head"><b>输入 (主 → 子)</b><button class="flow-btn slim" data-vm-add="in"><ui5-icon name="add"></ui5-icon> 映射</button></div>
      <div class="flow-vm-rows">${varMapRowsHtml('in', inRows)}</div>
      <div class="flow-vm-head" style="margin-top:10px"><b>输出 (子 → 主)</b><button class="flow-btn slim" data-vm-add="out"><ui5-icon name="add"></ui5-icon> 映射</button></div>
      <div class="flow-vm-rows">${varMapRowsHtml('out', outRows)}</div>
      <div class="flow-hint">留空 = 全量传递。填了则只传列出的变量。source→target 改变量名；同名可只填左侧。</div>
    </div>`
}

// ————————————————————— 分支条件可视化构造器（P2-c） —————————————————————
//
// 设计哲学：原始 ${expr} 文本框是**唯一真源 + 逃生舱**，永远保留可编辑；行式构造器只是它
// 之上的可视化外壳——每次改行即编译成表达式串走既有 applyProp('condition') 的 moddle 写回，
// 契约（写回路径、BPMN 产物）完全不变。函数列读后端 /conditions/functions（勿前端另维护）。

const COND_OPS = [
  { v: '>', t: '大于 >' }, { v: '>=', t: '大于等于 ≥' },
  { v: '<', t: '小于 <' }, { v: '<=', t: '小于等于 ≤' },
  { v: '==', t: '等于 =' }, { v: '!=', t: '不等于 ≠' },
]

// 一条条件行的默认形态。
function newCondRow () {
  return { lp: false, fn: '', varName: '', op: '>', val: '', valIsVar: false, rp: false, conn: '&&' }
}

// 把一行编译成片段：[前括号] [fn(]var[)] op val。val 按是否变量决定加不加引号。
function compileCondRow (r) {
  let lhs = r.varName || ''
  if (r.fn) lhs = `${r.fn}(${lhs})`
  let rhs = r.val ?? ''
  if (!r.valIsVar) {
    // 非变量：数字/布尔原样，否则加单引号当字符串。
    const n = String(rhs).trim()
    const isNumOrBool = n !== '' && (!isNaN(Number(n)) || n === 'true' || n === 'false')
    rhs = isNumOrBool ? n : `'${String(rhs).replace(/'/g, "\\'")}'`
  }
  let frag = `${lhs} ${r.op} ${rhs}`
  if (r.lp) frag = '( ' + frag
  if (r.rp) frag = frag + ' )'
  return frag
}

// 全部行 → 一条 ${expr} 表达式（行间用各自 conn 连接，最后一行 conn 忽略）。
function compileCondRows (rows) {
  if (!rows.length) return ''
  const parts = rows.map((r, i) => {
    const frag = compileCondRow(r)
    return i < rows.length - 1 ? `${frag} ${r.conn}` : frag
  })
  return '${' + parts.join(' ') + '}'
}

// 变量下拉选项：来自已声明的流程变量（state.varPaths，随定义 XML 的 <cmx:varSchema> 摊平而来）。
// datalist 提示 + 保留用户手填（未声明的变量仍可手打）。⑤ P3：条件构造器变量列联动。
function condVarDatalistHtml () {
  return varPathOptionsHtml()
}

// 摊平路径 → <option> 列表（datalist 用）。label/type 作提示文本。scalar-only 参数时过滤掉对象/数组。
function varPathOptionsHtml (opts = {}) {
  const paths = state.varPaths || []
  const list = opts.collectionOnly
    ? paths.filter((p) => p.isCollection)
    : (opts.leafOnly ? paths.filter((p) => p.type !== 'OBJECT' && p.type !== 'ARRAY') : paths)
  return list.map((p) => `<option value="${esc(p.path)}">${esc(p.label || p.path)}${p.type ? ` · ${esc(VAR_TYPE_LABEL[p.type] || p.type)}` : ''}</option>`).join('')
}

function fnOptionsHtml (cur) {
  const cat = state.fnCatalog || []
  let opts = `<option value="" ${!cur ? 'selected' : ''}>（无函数）</option>`
  opts += cat.map((f) => `<option value="${esc(f.name)}" ${f.name === cur ? 'selected' : ''}>${esc(f.name)}</option>`).join('')
  return opts
}

function condRowHtml (r, i, last) {
  const opOpts = COND_OPS.map((o) => `<option value="${esc(o.v)}" ${o.v === r.op ? 'selected' : ''}>${esc(o.t)}</option>`).join('')
  return `<div class="flow-cond-row" data-cond-i="${i}">
    <label class="flow-cond-paren" title="前括号"><input type="checkbox" data-cond-f="lp" ${r.lp ? 'checked' : ''}>(</label>
    <select class="flow-cond-fn" data-cond-f="fn" title="函数">${fnOptionsHtml(r.fn)}</select>
    <input class="flow-cond-var" data-cond-f="varName" value="${esc(r.varName)}" placeholder="变量 如 order.amount" list="flow-cond-vars">
    <select class="flow-cond-op" data-cond-f="op" title="比较符">${opOpts}</select>
    <span class="flow-cond-val-wrap">
      <input class="flow-cond-val" data-cond-f="val" value="${esc(r.val)}" placeholder="${r.valIsVar ? '变量名' : '值'}">
      <label class="flow-cond-isvar" title="比较值是另一个变量"><input type="checkbox" data-cond-f="valIsVar" ${r.valIsVar ? 'checked' : ''}>变量</label>
    </span>
    <label class="flow-cond-paren" title="后括号"><input type="checkbox" data-cond-f="rp" ${r.rp ? 'checked' : ''}>)</label>
    ${last ? '' : `<select class="flow-cond-conn" data-cond-f="conn" title="连接符">
      <option value="&&" ${r.conn === '&&' ? 'selected' : ''}>并且 AND</option>
      <option value="||" ${r.conn === '||' ? 'selected' : ''}>或者 OR</option></select>`}
    <button class="flow-icon-btn danger" data-cond-del="${i}" title="删除此行"><ui5-icon name="decline"></ui5-icon></button>
  </div>`
}

function conditionBuilderHtml (raw) {
  const compiled = compileCondRows(state.condRows)
  const testCls = state.condTest ? (state.condTest.ok ? (state.condTest.result ? 'ok' : 'off') : 'err') : ''
  const testMsg = state.condTest
    ? (state.condTest.ok ? (state.condTest.result ? '成立 → 走此边' : '不成立 → 不走') : ('错误：' + (state.condTest.error || '')))
    : ''
  const rowsHtml = state.condRows.length
    ? state.condRows.map((r, i) => condRowHtml(r, i, i === state.condRows.length - 1)).join('')
    : `<div class="flow-cond-empty">无条件（此边恒可走）。点「+ 条件行」添加，或切「高级」直接写表达式。</div>`
  return `<div class="flow-cond" data-cond-builder>
    <div class="flow-cond-mode">
      <button class="flow-cond-tab ${!state.condAdvanced ? 'on' : ''}" data-cond-mode="visual">可视化</button>
      <button class="flow-cond-tab ${state.condAdvanced ? 'on' : ''}" data-cond-mode="adv">高级</button>
    </div>
    <datalist id="flow-cond-vars">${condVarDatalistHtml()}</datalist>
    ${state.condAdvanced ? `
      <div class="flow-field">
        <input data-prop="condition" value="${esc(raw)}" placeholder="如 \${order.amount > 5000 && approved}">
        <div class="flow-hint">直接编辑表达式（逃生舱）。支持嵌套路径 order.amount、函数 IN()/CONTAINS() 等。</div>
      </div>` : `
      <div class="flow-cond-rows">${rowsHtml}</div>
      <button class="flow-btn slim" data-cond-add><ui5-icon name="add"></ui5-icon> 条件行</button>
      <div class="flow-cond-preview"><span>预览</span><code>${esc(compiled || '（空）')}</code></div>
      <div class="flow-cond-test">
        <input data-cond-testvars value="${esc(state.condTestVars)}" placeholder='试算变量 JSON 如 {"order":{"amount":9000}}'>
        <button class="flow-btn slim" data-cond-test><ui5-icon name="play"></ui5-icon> 试算</button>
        ${testMsg ? `<span class="flow-cond-testres ${testCls}">${esc(testMsg)}</span>` : ''}
      </div>`}
  </div>`
}

// 网关默认流：下拉选一条出边作为 default（无条件兜底）。写 BPMN 的 default 属性。
function defaultFlowFieldHtml (el, b) {
  // F0：出边挂在**图元素** el.outgoing 上（SequenceFlow 连线元素），businessObject.outgoing 常为空。
  // 从图元素取，回退 businessObject（兼容极端情形）。
  const edges = (el && el.outgoing && el.outgoing.length) ? el.outgoing : (b.outgoing || [])
  const outs = edges.map((f) => {
    const bo = f.businessObject || f
    return { id: bo.id || f.id, name: bo.name || bo.id || f.id }
  })
  const cur = b.default ? b.default.id : ''
  if (!outs.length) return `<div class="flow-hint">该网关暂无出边。先在画布上从网关拉出连线到目标节点，再回此设默认流。</div>`
  const opts = `<option value="">（不指定）</option>` +
    outs.map((o) => `<option value="${esc(o.id)}" ${o.id === cur ? 'selected' : ''}>${esc(o.name)}</option>`).join('')
  return `<div class="flow-field"><label>默认流出边</label>
    <select data-default-flow>${opts}</select>
    <div class="flow-hint">所有条件都不满足时走默认流。默认流本身不应再设条件。</div></div>`
}

// ————————————————————— 组织绑定对话框（property 区内浮层） —————————————————————

// 组织下拉选项：按 path 缩进呈现层级；已被其他绑定占用的组织仍可选（覆盖）。
function orgOptionsHtml (selected) {
  // RD4：按当前对话框维度取条目；缩进按物化路径段数（org 用 '/'、cf_* 用 '.'，都能算深度）。
  const dk = state.bindingDialog?.dimKey || 'org'
  const entries = state.dimEntries[dk] || state.orgs
  const opts = entries.map((o) => {
    const p = String(o.path || '')
    const depth = Math.max(0, p.split(/[/.]/).filter(Boolean).length - 1)
    const indent = '　'.repeat(depth)
    return `<option value="${esc(o.id)}" ${o.id === selected ? 'selected' : ''}>${indent}${esc(o.name)}</option>`
  }).join('')
  return `<option value="">— 默认（兜底）绑定 —</option>${opts}`
}

// 子流程定义下拉：从已加载定义列表取（排除主流程自身无意义，这里全列）。
function subflowTargetOptionsHtml (selected) {
  // F8：排除当前正在编辑的定义自身（子流程绑定到自己会造成递归调用）。
  const list = state.definitions.filter((d) => d.key !== state.selectedKey)
  return `<option value="">选择目标子流程…</option>` + list.map((d) =>
    `<option value="${esc(d.key)}" ${d.key === selected ? 'selected' : ''}>${esc(d.name || d.key)} (${esc(d.key)})</option>`).join('')
}

function bindingDialogHtml () {
  if (!state.bindingDialog) return ''
  const key = state.bindingDialog.calledKey
  const dimKey = state.bindingDialog.dimKey || 'org'
  const dimName = (state.dims || []).find((d) => d.dimKey === dimKey)?.name || (dimKey === 'org' ? '组织机构' : dimKey)
  const rows = state.bindings.length
    ? state.bindings.map((bd) => `<div class="flow-vrow ${bd.isDefault ? 'cur' : ''}">
        <div class="flow-vrow-main">
          <b>${bd.isDefault ? '默认（兜底）' : esc(bd.dimValueName || bd.dimValue || bd.orgName || bd.orgId)}</b>${bd.isDefault ? '<span class="flow-vtag">fallback</span>' : ''}${!bd.enabled ? '<span class="flow-vtag off">停用</span>' : ''}
          <em>→ ${esc(bd.targetKey)}</em>
        </div>
        <div class="flow-vrow-act">
          <button class="flow-btn slim danger" data-bind-del="${esc(bd.id)}" title="删除绑定"><ui5-icon name="delete"></ui5-icon></button>
        </div>
      </div>`).join('')
    : `<div class="flow-vempty"><ui5-icon name="org-chart"></ui5-icon><b>暂无维度绑定</b><span>下方为该维度取值指定子流程；建议先加一条「默认兜底」。</span></div>`
  return `<div class="flow-dialog-mask" data-bind-mask>
    <section class="flow-dialog">
      <div class="flow-dialog-head">
        <span class="flow-dialog-ic"><ui5-icon name="org-chart"></ui5-icon></span>
        <div><b>维度绑定 · ${esc(dimName)}</b><em>逻辑子流程 ${esc(key)}</em></div>
        <button class="flow-icon-btn" data-bind-close title="关闭"><ui5-icon name="decline"></ui5-icon></button>
      </div>
      ${state.bindingError ? `<div class="flow-dialog-err">${esc(state.bindingError)}</div>` : ''}
      <div class="flow-dialog-body">
        <div class="flow-vlist-head"><b>已配置绑定</b><span>${state.bindings.length} 条</span></div>
        <div class="flow-vlist">${rows}</div>
        <div class="flow-vnew">
          <div class="flow-vnew-title"><ui5-icon name="add"></ui5-icon> 新增/更新绑定</div>
          <label class="flow-field"><span>${esc(dimName)}</span><select data-bind-org>${orgOptionsHtml('')}</select></label>
          <label class="flow-field"><span>目标子流程</span><select data-bind-target>${subflowTargetOptionsHtml('')}</select></label>
          <div class="flow-hint">同一取值重复保存会覆盖旧绑定。留「默认（兜底）」= 所有未单独配置的取值都走它。</div>
          <button class="flow-btn primary block" data-bind-save><ui5-icon name="accept"></ui5-icon> 保存绑定</button>
        </div>
      </div>
    </section>
  </div>`
}

// ═══════════════════ ⑤ 变量声明（设计态 VarSchema）═══════════════════
//
// 变量声明随定义 XML 走（写进 <process><extensionElements><cmx:varSchema>JSON</cmx:varSchema>）。
// bpmn-js 不认这个扩展元素会在 saveXML 丢弃，故模块外维护 state.varSchema：openDiagram 从 XML
// 读入、getXml 注回。全屏编辑器可视化编辑；对象/数组递归子字段；四处下拉从摊平路径取。

const VAR_TYPES = [
  { v: 'STRING', label: '字符串' }, { v: 'NUMBER', label: '数值' },
  { v: 'BOOLEAN', label: '布尔' }, { v: 'DATE', label: '日期' },
  { v: 'ENUM', label: '枚举' }, { v: 'OBJECT', label: '对象' }, { v: 'ARRAY', label: '数组' },
]
const VAR_TYPE_LABEL = Object.fromEntries(VAR_TYPES.map((t) => [t.v, t.label]))

// 从 XML 抽 <cmx:varSchema> JSON → state.varSchema；同时读 process 的 cmx:varValidation。空 → []。
function readVarSchemaFromXml (xml) {
  state.varSchema = []
  state.varValidation = ''
  try {
    const m = xml.match(/<(?:\w+:)?varSchema\b[^>]*>([\s\S]*?)<\/(?:\w+:)?varSchema>/)
    if (m && m[1].trim()) {
      // E2：注入时把 & < > 转义成实体（injectVarSchemaIntoXml），读回须反转义，否则 label/description
      // 里的 & < > 每存取一轮多一层转义（&→&amp;→&amp;amp;）逐渐损坏。&amp; 最后解，避免二次替换。
      const raw = m[1].trim().replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&')
      const parsed = JSON.parse(raw)
      if (Array.isArray(parsed)) state.varSchema = parsed
    }
    const vv = xml.match(/<(?:bpmn:)?process\b[^>]*\bcmx:varValidation="([^"]*)"/)
    if (vv) state.varValidation = vv[1]
  } catch (e) { console.warn('读取 varSchema 失败:', e) }
  recomputeVarPaths()
}

// 把 state.varSchema 注回 XML 的 <process><extensionElements>。空 schema → 移除既有声明。
function injectVarSchemaIntoXml (xml) {
  // 先移除已有 <cmx:varSchema>…</cmx:varSchema>（避免重复）。
  let out = xml.replace(/\s*<(?:\w+:)?varSchema\b[^>]*>[\s\S]*?<\/(?:\w+:)?varSchema>/g, '')
  // 写/清 process 的 cmx:varValidation 属性。
  out = out.replace(/(<(?:bpmn:)?process\b[^>]*?)\s*cmx:varValidation="[^"]*"/, '$1')
  if (state.varValidation) {
    out = out.replace(/(<(?:bpmn:)?process\b)(\s)/, `$1 cmx:varValidation="${esc(state.varValidation)}"$2`)
  }
  if (!state.varSchema.length) return out
  const json = JSON.stringify(state.varSchema)
  const frag = `<cmx:varSchema>${json.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')}</cmx:varSchema>`
  // 有 extensionElements → 插入其内；否则在 <process ...> 开标签后新建一个。
  if (/<(?:bpmn:)?extensionElements\b[^>]*>/.test(out)) {
    out = out.replace(/(<(?:bpmn:)?extensionElements\b[^>]*>)/, `$1${frag}`)
  } else {
    out = out.replace(/(<(?:bpmn:)?process\b[^>]*>)/, `$1<bpmn:extensionElements>${frag}</bpmn:extensionElements>`)
  }
  return out
}

// 摊平 state.varSchema → 点路径列表（下拉数据源）。对象 a.b、数组元素 a[].b。
function recomputeVarPaths () {
  const out = []
  const walk = (decls, prefix, depth) => {
    if (depth > 16) return
    for (const d of decls || []) {
      if (!d || !d.name) continue
      const path = prefix ? `${prefix}.${d.name}` : d.name
      out.push({ path, type: d.type, label: d.label || d.name, description: d.description || '', isCollection: d.type === 'ARRAY', enumOptions: d.enumOptions || [] })
      if (d.type === 'OBJECT') walk(d.fields, path, depth + 1)
      if (d.type === 'ARRAY' && d.item && d.item.type === 'OBJECT') walk(d.item.fields, `${path}[]`, depth + 1)
    }
  }
  walk(state.varSchema, '', 0)
  state.varPaths = out
}

// 全屏变量编辑器 HTML。
function varDialogHtml () {
  if (!state.varDialog) return ''
  const rows = state.varSchema.length
    ? state.varSchema.map((d, i) => varRowHtml(d, String(i), 0)).join('')
    : `<div class="flow-vempty"><ui5-icon name="add-product"></ui5-icon><b>还没有变量</b><span>点下方「新增变量」声明本流程用到的变量；对象/数组可展开定义字段结构。</span></div>`
  const vv = state.varValidation || 'lenient'
  return `<div class="flow-dialog-mask" data-var-mask>
    <section class="flow-dialog flow-dialog-lg">
      <div class="flow-dialog-head">
        <span class="flow-dialog-ic"><ui5-icon name="course-book"></ui5-icon></span>
        <div><b>流程变量声明</b><em>${esc(state.name || state.selectedKey || '未保存')} · 定义名称/类型/结构/说明（值在发起时传）</em></div>
        <button class="flow-icon-btn" data-var-close title="关闭"><ui5-icon name="decline"></ui5-icon></button>
      </div>
      ${state.varError ? `<div class="flow-dialog-err">${esc(state.varError)}</div>` : ''}
      <div class="flow-dialog-body flow-var-body">
        <div class="flow-var-list">${rows}</div>
        <button class="flow-btn primary block" data-var-add><ui5-icon name="add"></ui5-icon> 新增变量</button>
      </div>
      <div class="flow-var-foot">
        <label class="flow-var-policy">发起校验
          <select data-var-policy>
            <option value="lenient" ${vv === 'lenient' ? 'selected' : ''}>宽松（违规仅提示）</option>
            <option value="strict" ${vv === 'strict' ? 'selected' : ''}>严格（违规拒绝发起）</option>
            <option value="off" ${vv === 'off' ? 'selected' : ''}>关闭（不校验）</option>
          </select>
        </label>
        <div class="flow-var-foot-act">
          <button class="flow-btn" data-var-close>取消</button>
          <button class="flow-btn primary" data-var-save><ui5-icon name="accept"></ui5-icon> 保存声明</button>
        </div>
      </div>
    </section>
  </div>`
}

// 单条变量行（path = 树内定位路径，如 "0" / "0.fields.1" / "2.item.fields.0"；depth 控缩进）。
function varRowHtml (d, path, depth) {
  const t = d.type || 'STRING'
  const isObj = t === 'OBJECT'
  const isArr = t === 'ARRAY'
  const isEnum = t === 'ENUM'
  const typeOpts = VAR_TYPES.map((o) => `<option value="${o.v}" ${o.v === t ? 'selected' : ''}>${o.label}</option>`).join('')
  let sub = ''
  if (isEnum) {
    sub = `<div class="flow-var-sub"><label class="flow-field"><span>候选值（逗号分隔）</span>
      <input data-var-enum="${path}" value="${esc((d.enumOptions || []).join(', '))}" placeholder="如 north, south"></label></div>`
  } else if (isObj) {
    const fields = (d.fields || []).map((f, i) => varRowHtml(f, `${path}.fields.${i}`, depth + 1)).join('')
    sub = `<div class="flow-var-sub"><div class="flow-var-subhead"><ui5-icon name="tree"></ui5-icon> 对象字段
      <button class="flow-btn slim" data-var-addfield="${path}"><ui5-icon name="add"></ui5-icon> 字段</button></div>
      ${fields || '<div class="flow-hint">暂无字段，点「字段」添加</div>'}</div>`
  } else if (isArr) {
    const it = d.item || { name: 'item', type: 'OBJECT', fields: [] }
    const itemTypeOpts = VAR_TYPES.map((o) => `<option value="${o.v}" ${o.v === (it.type || 'OBJECT') ? 'selected' : ''}>${o.label}</option>`).join('')
    let itemFields = ''
    if ((it.type || 'OBJECT') === 'OBJECT') {
      const fields = (it.fields || []).map((f, i) => varRowHtml(f, `${path}.item.fields.${i}`, depth + 1)).join('')
      itemFields = `<div class="flow-var-subhead"><ui5-icon name="tree"></ui5-icon> 元素字段
        <button class="flow-btn slim" data-var-addfield="${path}.item"><ui5-icon name="add"></ui5-icon> 字段</button></div>
        ${fields || '<div class="flow-hint">暂无字段，点「字段」添加</div>'}`
    }
    sub = `<div class="flow-var-sub">
      <label class="flow-field"><span>元素类型</span><select data-var-itemtype="${path}">${itemTypeOpts}</select></label>
      ${itemFields}</div>`
  }
  return `<div class="flow-var-row" style="--d:${depth}">
    <div class="flow-var-main">
      <input class="flow-var-name" data-var-name="${path}" value="${esc(d.name || '')}" placeholder="变量名（英文）">
      <select class="flow-var-type" data-var-type="${path}">${typeOpts}</select>
      <input class="flow-var-label" data-var-label="${path}" value="${esc(d.label || '')}" placeholder="标签（中文名）">
      <label class="flow-var-req" title="必填"><input type="checkbox" data-var-req="${path}" ${d.required ? 'checked' : ''}>必填</label>
      <button class="flow-btn slim danger" data-var-del="${path}" title="删除"><ui5-icon name="delete"></ui5-icon></button>
    </div>
    <input class="flow-var-desc" data-var-desc="${path}" value="${esc(d.description || '')}" placeholder="说明（这个变量是什么、从哪来）">
    ${sub}
  </div>`
}

function field (label, prop, val, hint) {
  return `<div class="flow-field"><label>${esc(label)}</label>` +
    `<input data-prop="${esc(prop)}" value="${esc(val)}">` +
    (hint ? `<div class="flow-hint">${esc(hint)}</div>` : '') + `</div>`
}function selectField (label, prop, opts, cur, hint) {
  // opts 支持纯字符串数组，或 {value,label} 对象数组（D3：给 formMode 等中文标签）。
  const o = opts.map((v) => {
    const val = (v && typeof v === 'object') ? v.value : v
    const lab = (v && typeof v === 'object') ? v.label : v
    return `<option value="${esc(val)}" ${val === cur ? 'selected' : ''}>${esc(lab)}</option>`
  }).join('')
  return `<div class="flow-field"><label>${esc(label)}</label>` +
    `<select data-prop="${esc(prop)}">${o}</select>` +
    (hint ? `<div class="flow-hint">${esc(hint)}</div>` : '') + `</div>`
}
function sec (t) { return `<div class="flow-sec">${esc(t)}</div>` }

// —— ⑤ 变量树导航/变更（path 如 "0" / "0.fields.1" / "2.item.fields.0"）——
//
// path 语义：数字 = 数组下标；"fields" = 进入当前 decl 的 fields 数组；"item" = 进入数组的 item decl。
// 故 path 结构恒为 [idx] 或 [...,"fields",idx] 或 [...,"item"] 或 [...,"item","fields",idx]。

// 定位到容器数组（decl 所在的 Vec）+ 该 decl 的下标；"item" 结尾则返回 {itemOf: 父数组decl}。
function varLocate (path) {
  const parts = path.split('.')
  let arr = state.varSchema   // 当前容器数组
  let node = null             // 当前 decl
  let i = 0
  while (i < parts.length) {
    const seg = parts[i]
    if (seg === 'fields') { arr = node.fields || (node.fields = []); i++; continue }
    if (seg === 'item') {
      node = node.item || (node.item = { name: 'item', type: 'OBJECT', fields: [] })
      arr = null
      i++
      // "item" 是路径末段（元素类型编辑）→ 返回 item 节点，无容器下标。
      if (i === parts.length) return { arr: null, idx: -1, node }
      continue
    }
    const idx = Number(seg)
    node = arr[idx]
    if (i === parts.length - 1) return { arr, idx, node }
    i++
  }
  return { arr, idx: -1, node }
}

function normalizeDecl (d) {
  // 切换类型时清理不相关字段，保持数据干净。
  if (d.type !== 'ENUM') delete d.enumOptions
  if (d.type !== 'OBJECT') delete d.fields
  if (d.type !== 'ARRAY') delete d.item
  if (d.type === 'OBJECT' && !d.fields) d.fields = []
  if (d.type === 'ARRAY' && !d.item) d.item = { name: 'item', type: 'OBJECT', fields: [] }
}


// cmx:calledKey 是自定义命名空间属性，bpmn-js 未注册 moddle 扩展，故落在 businessObject.$attrs
// （不是 .get('cmx:calledKey')）。读写都走 $attrs 才对；引擎 compile 认 cmx:calledKey。
function getCalledKey (b) {
  return (b && b.$attrs && (b.$attrs['cmx:calledKey'] || b.$attrs.calledKey)) || ''
}
function setCalledKey (el, value) {
  // 直传前缀属性名增删 $attrs（undefined=删），勿传 { $attrs:{...} }（会嵌套、清不掉旧值）。
  activeModeler().get('modeling').updateProperties(el, {
    'cmx:calledKey': value || undefined,
    calledKey: undefined,
  })
}

// F2 表单绑定属性同 calledKey：cmx:formKey / cmx:formMode / cmx:formFields 落 $attrs。
// name 传 'formKey'|'formMode'|'formFields'，读写都走 cmx: 前缀（兼容裸名）。
function getFormAttr (b, name) {
  return (b && b.$attrs && (b.$attrs['cmx:' + name] || b.$attrs[name])) || ''
}
function setFormAttr (el, name, value) {
  activeModeler().get('modeling').updateProperties(el, {
    ['cmx:' + name]: value || undefined,
    [name]: undefined,
  })
}

// —— F1/F2 服务任务 delegate / 规则任务 decisionRef：走 $attrs（前缀无关，编译器按本地名读）——
function getServiceDelegate (b) {
  const a = b && b.$attrs
  return (a && (a['flowable:delegateExpression'] || a.delegateExpression || a['flowable:class'] || a.class ||
    a['cmx:delegate'] || a.delegate || a.type)) || ''
}
function setAttrKey (el, key, value, clearKeys) {
  const props = {}
  ;(clearKeys || []).forEach((k) => { props[k] = undefined })
  props[key] = value || undefined // 设在清之后：key 若也在 clearKeys 里，以设值为准
  activeModeler().get('modeling').updateProperties(el, props)
}
function getRuleDecisionRef (b) {
  const a = b && b.$attrs
  return (a && (a['flowable:decisionRef'] || a.decisionRef || a['cmx:decision'] || a.decision)) || ''
}

// —— F3 消息捕获：消息名/相关键走 cmx: 属性（编译器支持 cmx:message / cmx:correlationVar 回退）——
function getMessageName (b) {
  const a = b && b.$attrs
  // 优先结构化 messageEventDefinition.messageRef（若有），否则 cmx:message 属性。
  const def = (b.eventDefinitions || []).find((d) => (d.$type || '').endsWith('MessageEventDefinition'))
  const ref = def && (def.messageRef && (def.messageRef.name || def.messageRef.id))
  return ref || (a && (a['cmx:message'] || a.message)) || ''
}
function getCorrelationVar (b) {
  const a = b && b.$attrs
  return (a && (a['cmx:correlationVar'] || a.correlationVar)) || ''
}

// —— F3 边界定时器：读时长（timerEventDefinition/timeDuration）+ 中断性（cancelActivity）——
function getTimerDuration (b) {
  const def = (b.eventDefinitions || []).find((d) => (d.$type || '').endsWith('TimerEventDefinition'))
  const td = def && def.timeDuration
  return (td && (td.body || (typeof td === 'string' ? td : ''))) || ''
}
function getBoundaryInterrupting (b) {
  return b.cancelActivity !== false   // 缺省 true = 中断型
}
// businessObject 是否含某类 eventDefinition（本地名尾匹配）。
function hasEventDefLocal (b, defName) {
  return (b.eventDefinitions || []).some((d) => (d.$type || '').endsWith(defName))
}

// —— F3 写边界定时器时长：确保有 TimerEventDefinition + FormalExpression timeDuration ——
function setTimerDuration (el, value) {
  const modeling = activeModeler().get('modeling')
  const moddle = activeModeler().get('moddle')
  const b = el.businessObject
  let defs = (b.eventDefinitions || []).slice()
  let timer = defs.find((d) => (d.$type || '').endsWith('TimerEventDefinition'))
  if (!timer) { timer = moddle.create('bpmn:TimerEventDefinition', {}); defs = [timer] }
  if (value) timer.timeDuration = moddle.create('bpmn:FormalExpression', { body: value })
  else timer.timeDuration = undefined
  modeling.updateProperties(el, { eventDefinitions: defs })
}
// —— F3 写中断性 ——
function setBoundaryInterrupting (el, interrupting) {
  activeModeler().get('modeling').updateProperties(el, { cancelActivity: interrupting })
}
// —— F4 写终止/普通结束：加/删 TerminateEventDefinition ——
function setEndTerminate (el, terminate) {
  const modeling = activeModeler().get('modeling')
  const moddle = activeModeler().get('moddle')
  const b = el.businessObject
  const kept = (b.eventDefinitions || []).filter((d) => !(d.$type || '').endsWith('TerminateEventDefinition'))
  const defs = terminate ? [...kept, moddle.create('bpmn:TerminateEventDefinition', {})] : kept
  modeling.updateProperties(el, { eventDefinitions: defs.length ? defs : undefined })
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
      // 已有绑定先 GET 合并原行（只更新 workspaceNode/title），防整行 upsert 覆盖管理页
      // 维护的 console/bizTable/pkField 等字段；查不到（data=null）或查询失败按新注册 4 字段全量。
      let body = { formKey: fk, kind: 'workspace', workspaceNode: wsId, title: `流程表单工作台 · ${fk}` }
      try {
        const b = await apiJson('/api/flow/forms/' + enc(fk))
        if (b && b.formKey) { const { seeded, ...rest } = b; body = { ...rest, formKey: fk, kind: 'workspace', workspaceNode: wsId, title: body.title } }
      } catch { /* 注册表查询失败 → 按新注册，不阻断 */ }
      await apiJson('/api/flow/forms', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
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
    // 子流程模式：变体导航（返回主流程 / 切变体 / 为组织新建变体）。
    if (state.subNav) {
      root.querySelector('[data-sub-back]')?.addEventListener('click', () => backToMain())
      root.querySelectorAll('[data-sub-variant]').forEach((b) => b.addEventListener('click', () => selectSubVariant(b.dataset.subVariant)))
      root.querySelector('[data-sub-newvar-org]')?.addEventListener('change', (ev) => {
        const v = ev.target.value
        if (v === '') return
        subNewVariantForOrg(v === '__default__' ? '' : v)
      })
      bindOutline(root) // 子流程 explorer 下部也有大纲
      return
    }
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
    // 「显示子流程」开关：临时在主列表里也列出子流程（默认只显主流程）。
    root.querySelector('[data-show-subflows]')?.addEventListener('change', (ev) => {
      state.showSubflows = ev.target.checked
      state.defPage = 0            // 列表规模变化，回首页
      refreshView('explorer')
    })
    // 流程列表分页：首页/上页/下页/末页。
    root.querySelector('[data-page="first"]')?.addEventListener('click', () => gotoDefPage(0))
    root.querySelector('[data-page="prev"]')?.addEventListener('click', () => gotoDefPage(state.defPage - 1))
    root.querySelector('[data-page="next"]')?.addEventListener('click', () => gotoDefPage(state.defPage + 1))
    root.querySelector('[data-page="last"]')?.addEventListener('click', () => gotoDefPage(1e9))
    // DAM 三段级联过滤：选域清空应用/模块，选应用清空模块。
    root.querySelectorAll('[data-dam]').forEach((sel) => sel.addEventListener('change', () => {
      const kind = sel.dataset.dam
      const val = sel.value || ''
      if (kind === 'domain') { state.fDomain = val; state.fApp = ''; state.fModule = '' }
      else if (kind === 'app') { state.fApp = val; state.fModule = '' }
      else if (kind === 'module') { state.fModule = val }
      state.defPage = 0            // 过滤条件变化，回首页
      refreshView('explorer')
    }))
    bindOutline(root) // 主流程 explorer 下部大纲
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
    bindProperty(root)
  }
}

// 属性面板事件绑定（抽出，供正常渲染与子流程编辑器就地刷新复用）。
function bindProperty (root) {
  // property 区页签切换（节点属性 | 数据模型 | 模拟）。切走「模拟」时清画布高亮。
  root.querySelectorAll('[data-ptab]').forEach((b) => b.addEventListener('click', () => {
    const prev = state.propTab
    state.propTab = b.dataset.ptab
    if (prev === 'sim' && b.dataset.ptab !== 'sim') clearSimMarkers()
    refreshView('property')
  }))
  // 模拟页签：facts 表单 / 发起人·组织 / JSON override / 运行。
  root.querySelectorAll('[data-sim-fact]').forEach((inp) => inp.addEventListener('change', () => { state.sim.facts[inp.dataset.simFact] = inp.value }))
  root.querySelector('[data-sim-init]')?.addEventListener('change', (e) => { state.sim.initiator = e.target.value })
  root.querySelector('[data-sim-org]')?.addEventListener('change', (e) => { state.sim.org = e.target.value })
  root.querySelector('[data-sim-raw]')?.addEventListener('change', (e) => { state.sim.raw = e.target.value })
  root.querySelector('[data-sim-run]')?.addEventListener('click', () => runSimulation())
  // 差异页签：从/到 版本选择 / 对比 / 退出 / 点行定位。
  root.querySelector('[data-diff-va]')?.addEventListener('change', (e) => { if (state.diff) state.diff.va = e.target.value })
  root.querySelector('[data-diff-vb]')?.addEventListener('change', (e) => { if (state.diff) state.diff.vb = e.target.value })
  root.querySelector('[data-diff-run]')?.addEventListener('click', () => runDiff())
  root.querySelector('[data-diff-exit]')?.addEventListener('click', () => exitDiff())
  root.querySelectorAll('[data-diff-goto]').forEach((el) => el.addEventListener('click', () => centerOnElement(el.dataset.diffGoto)))
  root.querySelectorAll('[data-prop]').forEach((inp) => {
    inp.addEventListener('change', () => applyProp(inp.dataset.prop, inp.value))
  })
  bindConditionBuilder(root)
  bindUserTaskTabs(root)
  // 网关默认流下拉。
  root.querySelector('[data-default-flow]')?.addEventListener('change', (e) => setDefaultFlow(e.target.value))
  // DAM 归属下拉：写 state.defDam（级联清空下级），保存草稿时随请求落库。
  root.querySelectorAll('[data-def-dam]').forEach((sel) => sel.addEventListener('change', () => {
    const kind = sel.dataset.defDam
    const val = sel.value || ''
    if (kind === 'domain') { state.defDam = { domain: val, application: '', module: '' } }
    else if (kind === 'application') { state.defDam.application = val; state.defDam.module = '' }
    else if (kind === 'module') { state.defDam.module = val }
    refreshProp()
  }))
  // 子流程模式切换（按组织路由 / 固定）：改 BPMN 属性并重渲属性面板。
  root.querySelectorAll('[data-sub-mode]').forEach((btn) => btn.addEventListener('click', () => {
    setSubflowMode(btn.dataset.subMode)
  }))
  // F3 边界定时器触发方式（中断/非中断）。
  root.querySelectorAll('[data-boundary-mode]').forEach((btn) => btn.addEventListener('click', () => {
    const el = state.selectedElement
    if (el) { setBoundaryInterrupting(el, btn.dataset.boundaryMode === 'interrupt'); refreshProp() }
  }))
  // F4 结束类型（普通/终止）。
  root.querySelectorAll('[data-end-mode]').forEach((btn) => btn.addEventListener('click', () => {
    const el = state.selectedElement
    if (el) { setEndTerminate(el, btn.dataset.endMode === 'terminate'); refreshProp() }
  }))
  // 打开维度绑定对话框（挂在 property 区）。
  root.querySelector('[data-open-bindings]')?.addEventListener('click', () => {
    const el = state.selectedElement
    const key = el ? getCalledKey(el.businessObject) : ''
    if (key) openBindingDialog(key)
  })
  // RD4：CallActivity 面板首次渲染时懒加载可选维度，回来重渲让维度下拉出全部选项。
  if (state.selectedElement?.type === 'bpmn:CallActivity' && !state.dims) {
    loadDims().then(() => { if (state.selectedElement?.type === 'bpmn:CallActivity') refreshView('property') })
  }
  // ★ 钻入式进入子流程编辑（主入口）。
  root.querySelector('[data-edit-subflow]')?.addEventListener('click', () => {
    const el = state.selectedElement
    if (el && el.type === 'bpmn:CallActivity') openSubflow(el)
  })
  // 打开工作区节点对话框编辑本节点的表单工作台。
  root.querySelector('[data-edit-ws-node]')?.addEventListener('click', (e) => openWsNodeEditor(e.currentTarget))
  // ⑤ 变量声明：跳到「数据模型」页签编辑（内联，不再开全屏遮罩）+ 编辑器内 data-var-* 事件。
  root.querySelector('[data-open-vars]')?.addEventListener('click', () => { state.propTab = 'model'; refreshView('property') })
  bindVarDialog(root)
  bindVarMapping(root)
  bindBindingDialog(root)
}

// 属性区刷新：钻入式无浮层，恒整区重渲（画布在 content 区，不受影响）。
function refreshProp () {
  refreshView('property')
}

// 切子流程模式：org → 清 calledElement（保留/等待 calledKey）；fixed → 清 calledKey。
function setSubflowMode (mode) {
  const el = state.selectedElement
  if (!el || !activeModeler()) return
  const modeling = activeModeler().get('modeling')
  // F5：显式记住用户选的模式（新建节点两者皆空时，仅靠 calledElement/calledKey 无法判定，会弹回 org）。
  state.subMode[el.id] = mode
  try {
    if (mode === 'fixed') {
      setCalledKey(el, '')   // 清逻辑 key；calledElement 由用户在固定模式输入框填
    } else {
      modeling.updateProperties(el, { calledElement: undefined })
    }
  } catch (e) { toast('切换失败: ' + e.message) }
  refreshProp()
}

// ————————————————————— 子流程变量映射：绑定 + 写回（P5） —————————————————————

function bindVarMapping (root) {
  const el = state.selectedElement
  if (!el || el.type !== 'bpmn:CallActivity') return
  // 加映射行。
  root.querySelectorAll('[data-vm-add]').forEach((btn) => btn.addEventListener('click', () => {
    const dir = btn.dataset.vmAdd
    const rows = curVarRows(el, dir)
    rows.push({ source: '', target: '' })
    writeVarMap(el, dir, rows)
    refreshProp()
  }))
  // 删映射行。
  root.querySelectorAll('[data-vm-del]').forEach((btn) => btn.addEventListener('click', () => {
    const [dir, i] = btn.dataset.vmDel.split(':')
    const rows = curVarRows(el, dir)
    rows.splice(Number(i), 1)
    writeVarMap(el, dir, rows)
    refreshProp()
  }))
  // 行内 source/target 变更。
  root.querySelectorAll('.flow-vm-row').forEach((rowEl) => {
    const dir = rowEl.dataset.vmDir
    const i = Number(rowEl.dataset.vmI)
    rowEl.querySelectorAll('[data-vm-f]').forEach((inp) => inp.addEventListener('change', () => {
      const rows = curVarRows(el, dir)
      if (!rows[i]) return
      rows[i][inp.dataset.vmF] = inp.value
      writeVarMap(el, dir, rows)
    }))
  })
}

// 读当前方向的映射行（从 $attrs 解析）。
function curVarRows (el, dir) {
  return parseVarMap(getVarMapAttr(el.businessObject, dir === 'in' ? 'inVars' : 'outVars'))
}

// 写回某方向映射到 cmx:inVars / cmx:outVars 属性（空则删属性）。
function writeVarMap (el, dir, rows) {
  const name = dir === 'in' ? 'inVars' : 'outVars'
  const val = serializeVarMap(rows)
  try {
    activeModeler().get('modeling').updateProperties(el, {
      ['cmx:' + name]: val || undefined,
      [name]: undefined,
    })
  } catch (e) { toast('设置变量映射失败: ' + e.message) }
}

// content 工具栏事件（可重复调用：refreshContentChrome 重渲工具栏后要重绑）。
function bindToolbar (root) {
  const nameInput = root.querySelector('[data-name]')
  if (nameInput) nameInput.addEventListener('change', () => { state.name = nameInput.value })
  const grpSel = root.querySelector('[data-group]')
  if (grpSel) grpSel.addEventListener('change', () => { state.groupId = grpSel.value ? Number(grpSel.value) : null })
  root.querySelector('[data-act="new"]')?.addEventListener('click', () => newDiagram())
  root.querySelector('[data-act="undo"]')?.addEventListener('click', () => state.modeler?.get('commandStack').undo())
  root.querySelector('[data-act="redo"]')?.addEventListener('click', () => state.modeler?.get('commandStack').redo())
  root.querySelector('[data-act="fit"]')?.addEventListener('click', () => state.modeler?.get('canvas').zoom('fit-viewport', 'auto'))
  root.querySelector('[data-act="validate"]')?.addEventListener('click', () => doValidate())
  root.querySelector('[data-act="save"]')?.addEventListener('click', () => state.subNav ? subSave() : doSave())
  root.querySelector('[data-act="publish"]')?.addEventListener('click', () => state.subNav ? subPublish() : doPublish())
  // 子流程钻入模式：返回主流程（还原暂存的主 XML + 选中节点）。
  root.querySelector('[data-act="back-to-main"]')?.addEventListener('click', () => backToMain())
  // P4：导出/导入/另存为。
  root.querySelector('[data-act="export"]')?.addEventListener('click', () => doExport())
  root.querySelector('[data-act="saveas"]')?.addEventListener('click', () => doSaveAs())
  const fileInp = root.querySelector('[data-import-file]')
  root.querySelector('[data-act="import"]')?.addEventListener('click', () => fileInp?.click())
  fileInp?.addEventListener('change', (e) => doImport(e.target.files && e.target.files[0]))
  root.querySelector('[data-ver-switch]')?.addEventListener('change', (e) => {
    const v = e.target.value === '' ? null : Number(e.target.value)
    if (state.selectedKey) loadDef(state.selectedKey, v)
  })
  root.querySelector('[data-act="versions"]')?.addEventListener('click', () => openVersionDialog())
  root.querySelector('[data-act="diff"]')?.addEventListener('click', () => openDiff())
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

// ————————————————————— 维度绑定动作（RD4） —————————————————————

async function loadOrgs () {
  if (state.orgs.length) return
  try {
    const d = await apiJson('/api/flow/orgs')
    state.orgs = d.orgs || []
  } catch (e) { toast('加载组织失败: ' + e.message) }
}

// 加载可选路由维度（org 内建 + 已注册字典）。失败兜底仅 org（不破旧流程）。
async function loadDims () {
  if (state.dims) return
  try {
    const d = await apiJson('/api/flow/dimensions')
    state.dims = d.dimensions || [{ dimKey: 'org', name: '组织机构', builtin: true }]
  } catch (e) {
    state.dims = [{ dimKey: 'org', name: '组织机构', builtin: true }]
  }
}

// 加载某维度的条目（org 复用 orgs；其余走 /api/flow/dimension/{k}/entries），缓存。
async function loadDimEntries (dimKey) {
  const dk = dimKey || 'org'
  if (state.dimEntries[dk]) return state.dimEntries[dk]
  if (dk === 'org') { await loadOrgs(); state.dimEntries.org = state.orgs; return state.orgs }
  try {
    const d = await apiJson('/api/flow/dimension/' + encodeURIComponent(dk) + '/entries')
    state.dimEntries[dk] = d.entries || []
  } catch (e) { state.dimEntries[dk] = []; toast('加载维度条目失败: ' + e.message) }
  return state.dimEntries[dk]
}

// 维度选择器选项（面板 cmx:dimKey 下拉）。org 置顶。
function dimSelectOptionsHtml (selected) {
  const dims = state.dims || [{ dimKey: 'org', name: '组织机构' }]
  return dims.map((d) =>
    `<option value="${esc(d.dimKey)}" ${d.dimKey === selected ? 'selected' : ''}>${esc(d.name || d.dimKey)}${d.builtin ? '（内建）' : ''}</option>`).join('')
}

async function openBindingDialog (calledKey) {
  // 取当前选中 callActivity 的维度（cmx:dimKey，缺省 org）。
  const b = state.selectedElement?.businessObject
  const dimKey = (b && getFormAttr(b, 'dimKey')) || 'org'
  state.bindingDialog = { calledKey, dimKey }
  state.bindingError = ''
  state.bindings = []
  await loadDims()
  await loadDimEntries(dimKey)
  await reloadBindings(calledKey)
  refreshView('property')
}

function closeBindingDialog () {
  state.bindingDialog = null
  state.bindingError = ''
  refreshView('property')
}

// ═══════════════════ 子流程钻入式编辑（复用 content 单一 modeler，无浮层）═══════════════════
//
// 从主流程 callActivity「编辑子流程」/双击进入：content 那一个画布就地切成子流程，主流程 XML 暂存
// 于 state.subNav.mainXml；工具栏出现面包屑 +「← 返回主流程」（常驻工具栏，永不被裁）；explorer 区
// 列组织变体可切换/新建。返回时还原主流程 XML + 选中节点。单一 state.modeler，四区各司其职。
// 彻底去掉此前 position:fixed 浮层在门户窄 property 区被裁的失控。
// - 固定子流程（calledElement）：explorer 无变体列表，直接编辑该子流程。
// - 组织路由（cmx:calledKey）：explorer 列各组织变体（默认兜底/各组织→目标子流程）点选切换；
//   未配置的组织可新建子流程并自动建绑定。绑定管理与子流程设计合二为一。

// 进入子流程编辑（el = 主流程里选中的 callActivity 元素）。
async function openSubflow (el) {
  if (!el || el.type !== 'bpmn:CallActivity' || !state.modeler) return
  const b = el.businessObject
  const calledKey = getCalledKey(b)
  const calledElement = b.get?.('calledElement') || ''
  if (!calledKey && !calledElement) { toast('该节点未配置子流程：先在属性面板填「固定子流程 key」或「逻辑子流程名」'); return }
  let mainXml
  try { mainXml = await getXml() } catch (e) { toast('暂存主流程失败: ' + e.message); return }
  state.subNav = {
    calledKey: calledKey || null,
    fixedKey: (!calledKey && calledElement) ? calledElement : null,
    variants: [],
    activeTargetKey: null,
    mainKey: state.selectedKey,
    mainXml,
    mainSelId: el.id,
    mainName: state.name,
    mainDam: { ...state.defDam },
    mainShownVersion: state.shownVersion,
    mainDirty: state.dirty,
    subName: '',
    pendingBindOrg: undefined,
    error: '',
  }
  if (calledKey) {
    await loadOrgs()
    await reloadSubVariants(calledKey)
    const def = state.subNav.variants.find((v) => v.isDefault) || state.subNav.variants.find((v) => v.targetKey)
    if (def && def.targetKey) await loadSubflowIntoContent(def.targetKey)
    else await loadSubflowNew()   // calledKey 尚无任何绑定 → 空模板等用户配
  } else {
    await loadSubflowIntoContent(calledElement)
  }
}

// 把某子流程定义载入 content 画布（复用 state.modeler）。目标不存在 → 空模板，process id 设为该 key。
async function loadSubflowIntoContent (targetKey) {
  const sn = state.subNav; if (!sn) return
  sn.activeTargetKey = targetKey
  sn.pendingBindOrg = undefined
  let xml; let name = targetKey; let exists = false
  try {
    const detail = await apiJson('/api/flow/definitions/' + enc(targetKey))
    if (detail && detail.bpmnXml) { xml = detail.bpmnXml; name = detail.name || targetKey; exists = true }
  } catch { /* 不存在 → 空模板 */ }
  if (!exists) xml = rewriteProcessId(EMPTY_DIAGRAM, targetKey)
  state.selectedKey = exists ? targetKey : targetKey
  state.name = name; sn.subName = name
  state.defDam = { domain: '', application: '', module: '' }; state.shownVersion = null
  await openDiagram(xml)
  state.dirty = !exists
  refreshContentChrome(); refreshView('explorer'); refreshView('property')
}

// 为「新增组织变体」/无绑定进入空模板（保存时 subXmlWithUniqueKey 生成唯一 key + auto-bind）。
async function loadSubflowNew (orgId) {
  const sn = state.subNav; if (!sn) return
  sn.activeTargetKey = null
  sn.pendingBindOrg = (orgId === undefined) ? undefined : (orgId || '')
  state.selectedKey = null; state.name = ''; sn.subName = ''
  state.defDam = { domain: '', application: '', module: '' }; state.shownVersion = null
  await openDiagram(EMPTY_DIAGRAM)
  state.dirty = false
  refreshContentChrome(); refreshView('explorer'); refreshView('property')
}

// 切换到某组织变体。
async function selectSubVariant (targetKey) {
  const sn = state.subNav; if (!sn) return
  if (state.dirty && typeof window !== 'undefined' && window.confirm && !window.confirm('当前子流程有未保存修改，切换将丢弃。确定？')) return
  await loadSubflowIntoContent(targetKey)
}

// 为某组织新建子流程变体（空模板，保存时自动绑定）。orgId=''表示默认兜底。
async function subNewVariantForOrg (orgId) {
  const sn = state.subNav; if (!sn) return
  if (state.dirty && typeof window !== 'undefined' && window.confirm && !window.confirm('当前子流程有未保存修改，新建将丢弃。确定？')) return
  await loadSubflowNew(orgId)
  toast(orgId ? '为该组织新建子流程，保存后自动绑定' : '新建默认兜底子流程，保存后自动绑定')
}

// 返回主流程：还原暂存的主 XML + 选中节点 + 上下文。
async function backToMain () {
  const sn = state.subNav; if (!sn) return
  if (state.dirty && typeof window !== 'undefined' && window.confirm && !window.confirm('子流程有未保存修改，返回将丢弃。确定返回主流程？')) return
  state.selectedKey = sn.mainKey; state.name = sn.mainName
  state.defDam = sn.mainDam || { domain: '', application: '', module: '' }
  state.shownVersion = sn.mainShownVersion
  const selId = sn.mainSelId; const mainDirty = sn.mainDirty; const mainXml = sn.mainXml
  state.subNav = null
  await openDiagram(mainXml)
  state.dirty = mainDirty
  if (selId && state.modeler) {
    try { const reg = state.modeler.get('elementRegistry'); const live = reg.get(selId); if (live) state.modeler.get('selection').select(live) } catch {}
  }
  refreshContentChrome(); refreshView('explorer'); refreshView('property')
}

// 拉某 calledKey 的组织变体（复用绑定列表）→ state.subNav.variants。
async function reloadSubVariants (calledKey) {
  if (!state.subNav) return
  try {
    const d = await apiJson('/api/flow/subflow-bindings/' + enc(calledKey))
    const binds = d.bindings || []
    state.subNav.variants = binds.map((bd) => ({
      id: bd.id, orgId: bd.orgId, orgName: bd.orgName, isDefault: bd.isDefault,
      targetKey: bd.targetKey, enabled: bd.enabled,
    }))
  } catch (e) { state.subNav.variants = []; state.subNav.error = '加载变体失败: ' + e.message }
}

// 新建子流程存草稿前，若 process id 仍是空模板默认 new_process（或撞已有定义），改写成有意义唯一 key。
function subXmlWithUniqueKey (xml, sn) {
  const m = xml.match(/<(?:bpmn:)?process\b[^>]*\bid="([^"]+)"/)
  const curId = m ? m[1] : ''
  if (sn.activeTargetKey && curId === sn.activeTargetKey) return xml   // 已是真子流程 → 迭代存版本
  const existing = new Set((state.definitions || []).map((d) => d.key))
  if (curId && curId !== 'new_process' && !existing.has(curId)) return xml   // 用户已给合法 id
  let base
  if (sn.calledKey) {
    const orgPart = (sn.pendingBindOrg !== undefined) ? (sn.pendingBindOrg ? sn.pendingBindOrg : 'default') : 'sub'
    base = `${sn.calledKey}_${orgPart}`.replace(/[^A-Za-z0-9_]/g, '_')
  } else { base = 'subflow' }
  let cand = base; let i = 1
  while (existing.has(cand)) { i += 1; cand = `${base}_${i}` }
  return rewriteProcessId(xml, cand)
}

// 子流程存草稿（工具栏 save 在 subNav 模式路由到此）：唯一 key + 新变体 auto-bind + 刷变体侧栏。
async function subSave () {
  const sn = state.subNav; if (!sn) return
  try {
    let xml = await getXml()
    xml = subXmlWithUniqueKey(xml, sn)
    const r = await apiJson('/api/flow/definitions/draft', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: state.name || '未命名子流程', bpmnXml: xml }),
    })
    const wasNew = !sn.activeTargetKey || sn.activeTargetKey !== r.key
    sn.activeTargetKey = r.key; state.selectedKey = r.key; sn.subName = state.name || r.key; state.dirty = false
    if (sn.calledKey && wasNew && sn.pendingBindOrg !== undefined) {
      await apiJson('/api/flow/subflow-bindings', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ calledKey: sn.calledKey, orgId: sn.pendingBindOrg || null, targetKey: r.key, enabled: true }),
      })
      sn.pendingBindOrg = undefined
    }
    if (sn.calledKey) await reloadSubVariants(sn.calledKey)
    await loadDefs()
    // 新建首存后：画布仍持模板 id → 重载已存版本，使后续保存迭代同一 key。
    if (wasNew) {
      try { const d = await apiJson('/api/flow/definitions/' + enc(r.key)); if (d.bpmnXml) { sn.subName = d.name || r.key; state.name = sn.subName; await openDiagram(d.bpmnXml); state.dirty = false } } catch {}
    }
    toast('子流程草稿已保存: ' + r.key)
    refreshContentChrome(); refreshView('explorer'); refreshView('property')
  } catch (e) { toast('保存子流程失败: ' + e.message) }
}

// 子流程发布（工具栏 publish 在 subNav 模式路由到此）。
async function subPublish () {
  const sn = state.subNav; if (!sn) return
  if (!sn.activeTargetKey) { await subSave() }
  const key = state.subNav && state.subNav.activeTargetKey
  if (!key) { toast('请先保存子流程草稿再发布'); return }
  try {
    let xml = await getXml(); xml = subXmlWithUniqueKey(xml, sn)
    await apiJson('/api/flow/definitions/draft', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: state.name || '未命名子流程', bpmnXml: xml }) })
    const r = await apiJson('/api/flow/definitions/' + enc(key) + '/publish', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ note: null }),
    })
    if (sn.calledKey) await reloadSubVariants(sn.calledKey)
    await loadDefs()
    toast('子流程已发布 ' + r.key + ' v' + r.version)
    refreshContentChrome(); refreshView('explorer'); refreshView('property')
  } catch (e) { toast('发布子流程失败: ' + e.message) }
}

// explorer 区（子流程模式）：面包屑返回 + 组织变体列表 + 新增变体。
function subflowExplorerHtml () {
  const sn = state.subNav; if (!sn) return ''
  const isOrg = !!sn.calledKey
  let list = ''
  if (isOrg) {
    const rows = sn.variants.length
      ? sn.variants.map((v) => {
        const on = v.targetKey === sn.activeTargetKey
        const label = v.isDefault ? '默认（兜底）' : esc(v.orgName || v.orgId)
        return `<button class="flow-subv ${on ? 'on' : ''}" data-sub-variant="${esc(v.targetKey)}">
          <span class="flow-subv-mark">${on ? '●' : '○'}</span>
          <span class="flow-subv-main"><b>${label}</b><small>→ ${esc(v.targetKey)}${!v.enabled ? ' · 停用' : ''}</small></span>
        </button>`
      }).join('')
      : `<div class="flow-hint" style="padding:8px">暂无组织变体，下方新增（建议先建「默认兜底」）。</div>`
    const boundOrgs = new Set(sn.variants.map((v) => v.orgId).filter(Boolean))
    const hasDefault = sn.variants.some((v) => v.isDefault)
    const freeOrgOpts = `<option value="">选组织新建变体…</option>` +
      (hasDefault ? '' : `<option value="__default__">默认（兜底）</option>`) +
      (state.orgs || []).filter((o) => !boundOrgs.has(o.id))
        .map((o) => `<option value="${esc(o.id)}">${esc('　'.repeat(((o.path || '').split('/').length - 2) || 0) + (o.name || o.id))}</option>`).join('')
    list = `<div class="flow-subv-list">${rows}</div>
      <div class="flow-subv-new"><select data-sub-newvar-org>${freeOrgOpts}</select></div>`
  } else {
    list = `<div class="flow-hint" style="padding:8px">固定子流程：<code>${esc(sn.fixedKey || sn.activeTargetKey || '')}</code>（所有组织同一个）。</div>`
  }
  return `<section class="flow flow-explorer">
    <div class="flow-head compact">
      <div><b>子流程</b><span>${esc(sn.calledKey ? '逻辑名 ' + sn.calledKey : '固定')}</span></div>
    </div>
    <button class="flow-btn block" data-sub-back><ui5-icon name="nav-back"></ui5-icon> 返回主流程（${esc(sn.mainName || sn.mainKey || '')}）</button>
    <div class="flow-subv-scroll">
      <div class="flow-subv-hd"><ui5-icon name="org-chart"></ui5-icon> 组织变体</div>
      ${list}
    </div>
    ${outlineHtml()}
  </section>`
}


// ————————————————————— ⑤ 变量声明编辑器：开/关/存 + 事件 —————————————————————

function openVarDialog () {
  state.varDialog = true
  state.varError = ''
  refreshView('property')
}
function closeVarDialog () {
  state.varDialog = false
  state.varError = ''
  refreshView('property')
}

// 保存声明：前端先 recompute + 后端 shape 校验，通过则写进 XML（getXml 注入）并存草稿。
async function saveVarSchema () {
  recomputeVarPaths()
  try {
    const r = await apiJson('/api/flow/definitions/variables/validate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ schema: state.varSchema }),
    })
    if (r && r.valid === false) {
      state.varError = '声明有误：' + (r.violations || []).map((v) => v.message).join('；')
      refreshView('property')
      return
    }
  } catch (e) { state.varError = '校验失败: ' + e.message; refreshView('property'); return }
  state.varDialog = false
  state.varError = ''
  // F6：变量声明存在模块外 state（不经 commandStack），故手动置脏，让 U1 未保存保护能拦住离开；
  // 声明随 getXml 注入 XML，「保存草稿/发布」时才真正落库——文案如实说明，避免误以为已存。
  state.dirty = true
  toast(`变量声明已更新（${state.varSchema.length} 个）。⚠ 尚未落库，请点工具栏「保存草稿」持久化`)
  refreshView('property')
}

// 变量编辑器事件（每次重渲对话框后重绑）。
function bindVarDialog (root) {
  root.querySelector('[data-var-mask]')?.addEventListener('click', (e) => { if (e.target === e.currentTarget) closeVarDialog() })
  root.querySelectorAll('[data-var-close]').forEach((b) => b.addEventListener('click', () => closeVarDialog()))
  root.querySelector('[data-var-save]')?.addEventListener('click', () => saveVarSchema())
  root.querySelector('[data-var-add]')?.addEventListener('click', () => {
    state.varSchema.push({ name: '', type: 'STRING', label: '' })
    refreshView('property')
  })
  root.querySelector('[data-var-policy]')?.addEventListener('change', (e) => { state.varValidation = e.target.value })
  // 字段级编辑（name/type/label/desc/req/enum/itemtype/add字段/删）。
  root.querySelectorAll('[data-var-name]').forEach((i) => i.addEventListener('change', (e) => { varLocate(i.dataset.varName).node.name = e.target.value.trim(); recomputeVarPaths() }))
  root.querySelectorAll('[data-var-label]').forEach((i) => i.addEventListener('change', (e) => { varLocate(i.dataset.varLabel).node.label = e.target.value }))
  root.querySelectorAll('[data-var-desc]').forEach((i) => i.addEventListener('change', (e) => { varLocate(i.dataset.varDesc).node.description = e.target.value }))
  root.querySelectorAll('[data-var-req]').forEach((i) => i.addEventListener('change', (e) => { varLocate(i.dataset.varReq).node.required = e.target.checked }))
  root.querySelectorAll('[data-var-type]').forEach((s) => s.addEventListener('change', (e) => {
    const n = varLocate(s.dataset.varType).node; n.type = e.target.value; normalizeDecl(n); recomputeVarPaths(); refreshView('property')
  }))
  root.querySelectorAll('[data-var-enum]').forEach((i) => i.addEventListener('change', (e) => {
    varLocate(i.dataset.varEnum).node.enumOptions = e.target.value.split(',').map((s) => s.trim()).filter(Boolean); recomputeVarPaths()
  }))
  root.querySelectorAll('[data-var-itemtype]').forEach((s) => s.addEventListener('change', (e) => {
    const arr = varLocate(s.dataset.varItemtype).node
    arr.item = arr.item || { name: 'item', type: 'OBJECT', fields: [] }
    arr.item.type = e.target.value; normalizeDecl(arr.item); recomputeVarPaths(); refreshView('property')
  }))
  root.querySelectorAll('[data-var-addfield]').forEach((b) => b.addEventListener('click', () => {
    const target = varLocate(b.dataset.varAddfield).node   // OBJECT decl 或 array.item
    target.fields = target.fields || []
    target.fields.push({ name: '', type: 'STRING', label: '' })
    refreshView('property')
  }))
  root.querySelectorAll('[data-var-del]').forEach((b) => b.addEventListener('click', () => {
    const { arr, idx } = varLocate(b.dataset.varDel)
    if (arr && idx >= 0) { arr.splice(idx, 1); recomputeVarPaths(); refreshView('property') }
  }))
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
  const dimKey = state.bindingDialog?.dimKey || 'org'
  const dimValue = root.querySelector('[data-bind-org]')?.value || ''
  const targetKey = root.querySelector('[data-bind-target]')?.value || ''
  if (!targetKey) { state.bindingError = '请选择目标子流程'; refreshView('property'); return }
  // U4：同取值已有绑定 → 保存会覆盖，先确认。dimValue 空 = 默认兜底绑定。
  const existing = (state.bindings || []).find((bd) => (bd.dimValue || '') === dimValue)
  if (existing && existing.targetKey !== targetKey && typeof window !== 'undefined' && window.confirm) {
    const who = dimValue ? (existing.dimValueName || dimValue) : '默认（兜底）'
    if (!window.confirm(`「${who}」已绑定到「${existing.targetKey}」，保存将覆盖为「${targetKey}」。确定？`)) return
  }
  try {
    await apiJson('/api/flow/subflow-bindings', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ calledKey: key, dimKey, dimValue: dimValue || null, targetKey, enabled: true }),
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
  // U2：删除组织绑定不可恢复，加确认（对齐 deleteVersion）。
  if (typeof window !== 'undefined' && window.confirm && !window.confirm('确认删除该组织绑定？删除后该组织将沿组织树继承或落默认绑定。')) return
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
    const r = await apiJson('/api/flow/definitions/' + enc(key) + '/versions/' + version + '/activate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}',
    })
    state.versionError = ''
    await loadDefs()                 // 刷新版本列表（activeVersion 变了）
    state.selectedVersion[key] = version
    await loadDef(key, version)       // 画布切到新当前版本
    refreshContentChrome()
    toast('v' + version + (r && r.hotLoaded ? ' 已设为当前版本并热装载，立即生效' : ' 已设为当前版本（热装载失败，重启后生效）'))
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
      mountMinimap(); scheduleMinimapViewport() // 复用画布：缩略图 DOM 可能随 content 重渲丢失，幂等补挂
      return
    }
    // 容器变了（首次或切走又回来）→ 新建 modeler 挂到新容器
    if (state.modeler) { try { state.modeler.destroy() } catch {} state.modeler = null }
    // 关键：bpmn-js 初始化/导入前容器必须有非零尺寸，否则 Canvas.getLayer 读 viewbox 报
    // "reading 'root-0'" + SVGMatrix non-finite。native-page tab 首帧容器可能 0 尺寸，等它布局好。
    await waitForSize(canvasEl)
    state.modeler = new window.BpmnJS({
      container: canvasEl,
      // 键盘监听绑本区 renderRoot（工具条+画布的 keydown 都能冒泡到），绝不能绑 document：
      // ① 绑 document 则监听常驻全局，切到门户其他 tab 后仍在拦截按键（Delete/Ctrl+C/V 被当
      //    画布快捷键 preventDefault，其他页面输入框复制粘贴删除全废）；
      // ② diagram-js 只放行 target 为 input/textarea 的事件，而门户输入框藏在多层 shadow
      //    root 里，事件冒泡到 document 时 target 已重定向为宿主元素 → 守卫失效。绑在本区
      //    shadow 树内无重定向，原生守卫恢复有效（画布 tabindex="0" 保证焦点落在树内）。
      keyboard: { bindTo: root },
      // 中文界面：经 bpmn-js 官方 translate 扩展点注入内置词典（零远程,词条对照 v17.11.1 bundle 提取）。
      additionalModules: [ZH_TRANSLATE_MODULE],
    })
    state.canvasEl = canvasEl
    state.modeler.on('selection.changed', (e) => {
      state.selectedElement = e.newSelection.length === 1 ? e.newSelection[0] : null
      if (!state.selectedElement || state.selectedElement.type !== 'bpmn:SequenceFlow') state.__condFor = null
      initConditionForSelection()
      refreshView('property')
      refreshOutline() // ★ 画布 → 大纲：选中变化即同步大纲高亮
      broadcastSelection() // 协同：去抖广播我选中的节点（远端高亮）
    })
    state.modeler.on('element.changed', (e) => {
      if (state.selectedElement && state.selectedElement.id === e.element.id) refreshView('property')
      // ① 徽章随节点属性变化即时更新（办理人/会签/子流程 key 改了立刻反映）。去抖到下一帧。
      if (state.__badgeRaf) cancelAnimationFrame(state.__badgeRaf)
      state.__badgeRaf = requestAnimationFrame(() => { state.__badgeRaf = null; renderNodeBadges() })
      refreshOutline() // 元素改名/属性变 → 大纲项文案随之更新
    })
    // 结构变化（增删节点/连线）→ 大纲重算 + 缩略图重建 + 协同 M3 广播。事件很密，去抖到下一帧。
    for (const ev of ['shape.added', 'shape.removed', 'connection.added', 'connection.removed', 'root.set']) {
      state.modeler.on(ev, (e) => { captureStructOp(ev, e && e.element); refreshOutline(); scheduleMinimapRender() })
    }
    // 元素移动/改尺寸 → 缩略图重建 + 协同 M3 广播位移（拖节点后位置变）。
    state.modeler.on('elements.changed', (e) => { captureMoveOps(e && e.elements); scheduleMinimapRender() })
    // 画布视口变化（平移/缩放）→ 只重定位缩略图里的视口小方框（轻量，高频）。
    state.modeler.on('canvas.viewbox.changed', () => scheduleMinimapViewport())
    // 导入完成（载入定义 / 钻入子流程 / 新建）→ 挂载并重建缩略图。
    state.modeler.on('import.done', () => { mountMinimap(); scheduleMinimapRender() })
    // U1：命令栈变化 = 有未保存编辑。用户操作（增删节点/连线/属性）都经 commandStack。
    //   编辑使上一次模拟高亮失效 → 清除画布 marker（addMarker 不经 commandStack，无递归）。
    state.modeler.on('commandStack.changed', () => {
      if (state.__loadingDiagram) return
      state.dirty = true
      if (state.sim.marked.length) { clearSimMarkers(); state.sim.result = null }
      if (state.diff && state.diff.marked.length) { clearDiffMarkers(); state.diff.result = null }
    })
    // ★ 快捷入口：双击 callActivity 节点 → 打开子流程编辑器（与属性面板「编辑子流程」等效）。
    state.modeler.on('element.dblclick', (e) => {
      const el = e.element
      if (el && el.type === 'bpmn:CallActivity' && !state.subNav) { openSubflow(el) }
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

async function openDiagram (xml, modeler, canvasEl) {
  const m = modeler || activeModeler()
  if (!m) return
  try {
    // 兼容旧图/导入图：可能没声明 xmlns:cmx 或 xmlns:flowable。缺声明时在其上配办理人/子流程键/
    // 会签集合，cmx:* 与 flowable:* 属性会在 saveXML 时被 bpmn-js moddle 静默丢弃（E1 静默数据丢失）。
    // 载入前补齐两个命名空间声明即可正常回写。幂等，已声明则不动；兼容 <definitions> 各前缀。
    const defTagRe = /<(?:\w+:)?definitions\b/
    if (!/xmlns:cmx\s*=/.test(xml)) {
      xml = xml.replace(defTagRe, '$& xmlns:cmx="http://cmx/flow"')
    }
    if (!/xmlns:flowable\s*=/.test(xml)) {
      xml = xml.replace(defTagRe, '$& xmlns:flowable="http://flowable.org/bpmn"')
    }
    const noDI = !/BPMNDiagram|BPMNPlane/i.test(xml)
    let toImport = xml
    if (noDI) { toImport = layoutXml(xml) }
    // ⑤ 从 XML 读出变量声明（模块外存 state.varSchema，避免 bpmn-js moddle 丢弃扩展元素）。
    readVarSchemaFromXml(xml)
    // U1：载入期间 commandStack 变化不算「用户编辑」，避免刚加载就标脏。
    state.__loadingDiagram = true
    await m.importXML(toImport)
    state.subMode = {}          // 换图清空子流程模式记忆（F5 的 per-el 记忆按当前图作用域）
    if (noDI) relayoutConnections(m)
    // 下一帧再 fit：importXML 后 bpmn-js 的图形 bbox 要等一帧才算准，同帧 fit 会读到空 bbox 只框住起点。
    requestAnimationFrame(() => { fitView(m, canvasEl); renderNodeBadges(); state.__loadingDiagram = false; setActiveDirty(false) })
  } catch (err) {
    state.__loadingDiagram = false
    // E5：导入/加载失败画布可能留白，给明确指引（非法 BPMN 常见）。
    toast('加载失败（画布可能空白）: ' + err.message + ' — 请检查 XML 合法性或点「新建」重来')
    console.error(err)
  }
}

// ① 业务化节点徽章：用 bpmn-js 官方 overlays 给节点叠加「一眼可辨」的类型徽标（会签/或签/
// 定时器/子流程/消息/服务/规则）。overlay 是官方 HTML 注解，随缩放平移自动跟随，且**绝不
// 会像自定义 renderer 那样因异常把画布画空**——安全增强。每次重画先清本类 overlay 再叠。
const BADGE_TYPE = 'cmx-node-badge'
function renderNodeBadges () {
  const m = activeModeler()
  if (!m) return
  let overlays; let registry
  try { overlays = m.get('overlays'); registry = m.get('elementRegistry') } catch { return }
  try { overlays.remove({ type: BADGE_TYPE }) } catch {}
  registry.getAll().forEach((el) => {
    const badges = badgesFor(el)
    if (!badges.length) return
    const html = `<div class="cmx-badges">${badges.map((b) => `<span class="cmx-badge ${b.cls}" title="${esc(b.title)}"><ui5-icon name="${b.icon}"></ui5-icon>${b.text ? `<i>${esc(b.text)}</i>` : ''}</span>`).join('')}</div>`
    try {
      overlays.add(el.id, BADGE_TYPE, { position: { top: -12, left: -6 }, html })
    } catch {}
  })
}

// 判断一个元素该挂哪些徽章（读 businessObject，纯只读）。
function badgesFor (el) {
  const b = el.businessObject
  if (!b) return []
  const out = []
  const t = el.type
  // 会签 / 或签（多实例）。
  if (t === 'bpmn:UserTask' && b.loopCharacteristics) {
    out.push(b.loopCharacteristics.isSequential
      ? { cls: 'seq', icon: 'sort', title: '顺序或签（逐个办理）', text: '或签' }
      : { cls: 'par', icon: 'multiselect-all', title: '并行会签（同时办理）', text: '会签' })
  }
  // 用户任务办理人类型（角色/岗位/关系型）小标，帮助一眼看清「谁办」。
  if (t === 'bpmn:UserTask') {
    const who = assigneeBadge(b)
    if (who) out.push(who)
  }
  // 服务任务。
  if (t === 'bpmn:ServiceTask') out.push({ cls: 'svc', icon: 'settings', title: '服务任务（调外部/delegate）', text: '服务' })
  // 业务规则任务（决策表）。
  if (t === 'bpmn:BusinessRuleTask') out.push({ cls: 'rule', icon: 'table-view', title: '业务规则任务（决策表）', text: '规则' })
  // 子流程调用。
  if (t === 'bpmn:CallActivity') {
    const key = getCalledKey(b)
    out.push({ cls: 'sub', icon: 'process', title: key ? `子流程（按组织路由 ${key}）` : '子流程调用', text: '子流程' })
  }
  // 消息中间捕获事件。
  if (t === 'bpmn:IntermediateCatchEvent' && hasEventDef(b, 'MessageEventDefinition')) {
    out.push({ cls: 'msg', icon: 'email', title: '消息捕获（等外部回调唤醒）', text: '消息' })
  }
  // 边界定时器事件。
  if (t === 'bpmn:BoundaryEvent' && hasEventDef(b, 'TimerEventDefinition')) {
    const interrupting = b.cancelActivity !== false
    out.push({ cls: 'timer', icon: 'history', title: interrupting ? '中断型边界定时器（超时中断升级）' : '非中断型边界定时器（超时旁路催办）', text: interrupting ? '限时' : '催办' })
  }
  // 终止结束事件。
  if (t === 'bpmn:EndEvent' && hasEventDef(b, 'TerminateEventDefinition')) {
    out.push({ cls: 'term', icon: 'sys-cancel', title: '终止事件（一票否决整个流程）', text: '终止' })
  }
  return out
}

// 从 userTask 反推办理人徽章（复用属性面板的 readAssignee 语义，只出「非静态人」的类型标）。
function assigneeBadge (b) {
  let cur
  try { cur = readAssignee(b) } catch { return null }
  if (!cur) return null
  const map = {
    role: { cls: 'who', icon: 'group', title: '按角色派单', text: '角色' },
    position: { cls: 'who', icon: 'employee', title: '按岗位派单', text: '岗位' },
    org: { cls: 'who', icon: 'org-chart', title: '按部门派单', text: '部门' },
    orgLeader: { cls: 'who', icon: 'manager', title: '部门领导审批', text: '领导' },
    initiator: { cls: 'who', icon: 'person-placeholder', title: '发起人本人', text: '发起人' },
    initiatorLeader: { cls: 'who', icon: 'manager', title: '发起人上级', text: '上级' },
  }
  return map[cur.kind] || null
}

// businessObject 是否含某类 eventDefinition（前缀无关，按类名尾匹配）。
function hasEventDef (b, defName) {
  const defs = b.eventDefinitions || []
  return defs.some((d) => (d.$type || '').endsWith(defName))
}

// 安全适应视口：容器尺寸非有限时 zoom fit-viewport 会抛 SVGMatrix non-finite。
// native-page tab 首帧/动画期间容器尺寸会变，故用 ResizeObserver 持续跟随，尺寸每变一次就重新 fit，
// 稳定后（连续几帧不变）停止。这样无论 tab 何时定尺寸，图都能正确缩放铺满。
function fitView (modeler, canvasEl) {
  const m = modeler || activeModeler()
  if (!m) return
  const canvas = m.get('canvas')
  const el = canvasEl || state.canvasEl
  const fit = () => {
    if (!m || !el || !el.isConnected) return false
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
function relayoutConnections (modeler) {
  try {
    const m = modeler || activeModeler()
    const er = m.get('elementRegistry')
    const modeling = m.get('modeling')
    er.getAll().forEach((el) => {
      if (el.type === 'bpmn:SequenceFlow' && el.source && el.target) modeling.layoutConnection(el, {})
    })
  } catch (e) { console.warn('精确布线失败:', e) }
}

function applyProp (prop, value) {
  const el = state.selectedElement
  if (!el || !activeModeler()) return
  applyPropTo(el, prop, value)
  // 协同 M2：本端属性编辑广播 op（远端就地合并）。远端回放期间 __applying 置真，不再回广播（防回声风暴）。
  if (state.collab && state.collab.on && !state.collab.__applying && !state.subNav) {
    broadcastOp(el.id, prop, value)
  }
}

// 把一次属性编辑落到指定元素（本端交互 + 远端 op 回放共用同一写回逻辑，保证两端语义一致）。
function applyPropTo (el, prop, value) {
  if (!el || !activeModeler()) return
  const modeling = activeModeler().get('modeling')
  const moddle = activeModeler().get('moddle')
  try {
    if (prop === 'name') modeling.updateProperties(el, { name: value })
    else if (prop === 'assignee') modeling.updateProperties(el, { 'flowable:assignee': value || undefined })
    else if (prop === 'candidateGroups') modeling.updateProperties(el, { 'flowable:candidateGroups': value || undefined })
    else if (prop === 'calledElement') modeling.updateProperties(el, { calledElement: value || undefined })
    else if (prop === 'calledKey') setCalledKey(el, value)
    else if (prop === 'dimKey') {
      // RD4：路由维度（cmx:dimKey）。org 为默认，写 undefined 省属性（向后兼容）。切维度后清空该 key
      // 的绑定缓存，下次开对话框按新维度重取条目。
      setFormAttr(el, 'dimKey', value === 'org' ? '' : value)
      state.bindingDialog = null; state.bindings = []
    }
    else if (prop === 'delegate') setAttrKey(el, 'flowable:delegateExpression', value, ['flowable:delegateExpression', 'delegateExpression', 'flowable:class', 'class', 'cmx:delegate', 'delegate', 'type'])
    else if (prop === 'decisionRef') setAttrKey(el, 'flowable:decisionRef', value, ['flowable:decisionRef', 'decisionRef', 'cmx:decision', 'decision'])
    else if (prop === 'messageName') setAttrKey(el, 'cmx:message', value, ['cmx:message', 'message'])
    else if (prop === 'correlationVar') setAttrKey(el, 'cmx:correlationVar', value, ['cmx:correlationVar', 'correlationVar'])
    else if (prop === 'timerDuration') setTimerDuration(el, value)
    else if (prop === 'formKey' || prop === 'formMode' || prop === 'formFields') setFormAttr(el, prop, value)
    else if (prop === 'cc') {
      // 抄送：走 cmx:cc 扩展属性（编译器按本地名 cc 解析）。直传前缀名增删，勿传 { $attrs }。
      modeling.updateProperties(el, { 'cmx:cc': value || undefined, cc: undefined })
    }
    else if (prop === 'documentation') {
      const arr = value ? [moddle.create('bpmn:Documentation', { text: value })] : undefined
      modeling.updateProperties(el, { documentation: arr })
    }
    else if (prop === 'condition') {
      if (value) modeling.updateProperties(el, { conditionExpression: moddle.create('bpmn:FormalExpression', { body: value }) })
      else modeling.updateProperties(el, { conditionExpression: undefined })
    }
  } catch (err) { toast('设置失败: ' + err.message) }
}

// ————————————————————— 条件构造器：绑定 + 变更 + 试算 —————————————————————

// 懒加载内置函数目录（一次，缓存）。加载后若当前正看条件面板则重渲以填充函数下拉。
async function ensureFnCatalog () {
  if (state.fnCatalog) return
  try {
    const d = await apiJson('/api/flow/conditions/functions')
    state.fnCatalog = d.functions || []
  } catch (e) { state.fnCatalog = [] }
}

// 从原始表达式串尽力解析回条件行（供已有流程加载时回填可视化）。解析不了就留空行 + 高级模式，
// 保证「任何合法表达式都不会丢失」——回填失败退化为高级模式直接显示原串。
function parseCondToRows (raw) {
  const src = String(raw || '').trim().replace(/^\$\{/, '').replace(/\}$/, '').trim()
  if (!src) return { rows: [], advanced: false }
  // 仅回填「无括号、纯 var op val 由 && / || 连接」的常见形态；复杂表达式转高级模式。
  if (/[()]/.test(src) || /[+\-*/]/.test(src) || /\b(IN|CONTAINS|IS_EMPTY|IF|COALESCE|MIN|MAX)\s*\(/i.test(src)) {
    return { rows: [], advanced: true }
  }
  const segs = src.split(/\s*(&&|\|\|)\s*/) // ['a > 1','&&','b == 2', ...]
  const rows = []
  for (let i = 0; i < segs.length; i += 2) {
    const m = segs[i].match(/^(\w+)\s*\(\s*([\w.]*)\s*\)\s*(>=|<=|==|!=|>|<)\s*(.+)$/) ||
              segs[i].match(/^([\w.]+)\s*(>=|<=|==|!=|>|<)\s*(.+)$/)
    if (!m) return { rows: [], advanced: true }
    const r = newCondRow()
    if (m.length === 5) { r.fn = m[1]; r.varName = m[2]; r.op = m[3]; var rhs = m[4] }
    else { r.varName = m[1]; r.op = m[2]; rhs = m[3] }
    rhs = rhs.trim()
    const qm = rhs.match(/^'(.*)'$/)
    if (qm) { r.val = qm[1]; r.valIsVar = false }
    else if (!isNaN(Number(rhs)) || rhs === 'true' || rhs === 'false') { r.val = rhs; r.valIsVar = false }
    else { r.val = rhs; r.valIsVar = true }
    r.conn = segs[i + 1] || '&&'
    rows.push(r)
  }
  return { rows, advanced: false }
}

// 选中一条 SequenceFlow 时初始化构造器状态（回填行 + 懒加载目录）。幂等：同元素不重复回填。
function initConditionForSelection () {
  const el = state.selectedElement
  if (!el || el.type !== 'bpmn:SequenceFlow') return
  if (state.__condFor === el.id) return
  state.__condFor = el.id
  const raw = (el.businessObject && el.businessObject.conditionExpression && el.businessObject.conditionExpression.body) || ''
  const parsed = parseCondToRows(raw)
  state.condRows = parsed.rows
  state.condAdvanced = parsed.advanced
  state.condTest = null
  ensureFnCatalog().then(() => { if (state.selectedElement && state.selectedElement.id === el.id) refreshProp() })
}

// 把当前 condRows 编译成表达式并写回 BPMN（走既有 applyProp('condition')，契约不变）。
function pushCondToModel () {
  const expr = compileCondRows(state.condRows)
  applyProp('condition', expr)
}

function bindConditionBuilder (root) {
  const el = state.selectedElement
  if (!el || el.type !== 'bpmn:SequenceFlow') return
  // 模式切换标签。
  root.querySelectorAll('[data-cond-mode]').forEach((btn) => btn.addEventListener('click', () => {
    state.condAdvanced = btn.dataset.condMode === 'adv'
    refreshProp()
  }))
  // 高级模式的原始框已由 [data-prop=condition] 通用绑定接管（change→applyProp），无需额外处理。
  // 加行。
  root.querySelector('[data-cond-add]')?.addEventListener('click', () => {
    state.condRows.push(newCondRow()); pushCondToModel(); refreshProp()
  })
  // 删行。
  root.querySelectorAll('[data-cond-del]').forEach((btn) => btn.addEventListener('click', () => {
    state.condRows.splice(Number(btn.dataset.condDel), 1); pushCondToModel(); refreshProp()
  }))
  // 行内字段变更。
  root.querySelectorAll('.flow-cond-row').forEach((rowEl) => {
    const i = Number(rowEl.dataset.condI)
    rowEl.querySelectorAll('[data-cond-f]').forEach((inp) => {
      const evt = (inp.type === 'checkbox' || inp.tagName === 'SELECT') ? 'change' : 'input'
      inp.addEventListener(evt, () => {
        const f = inp.dataset.condF
        const r = state.condRows[i]; if (!r) return
        r[f] = (inp.type === 'checkbox') ? inp.checked : inp.value
        pushCondToModel()
        // F7：勾「变量」切换值输入框占位（值↔变量名）——就地改 placeholder，不整体重渲以保留焦点。
        if (f === 'valIsVar') {
          const valInp = rowEl.querySelector('[data-cond-f="val"]')
          if (valInp) valInp.placeholder = r.valIsVar ? '变量名' : '值'
        }
        updateCondPreview(root)
      })
    })
  })
  // 试算变量输入。
  const tv = root.querySelector('[data-cond-testvars]')
  if (tv) tv.addEventListener('input', () => { state.condTestVars = tv.value })
  // 试算按钮。
  root.querySelector('[data-cond-test]')?.addEventListener('click', () => runCondTest())
}

// 就地更新预览串（避免每次按键全量重渲打断输入焦点）。
function updateCondPreview (root) {
  const code = root.querySelector('.flow-cond-preview code')
  if (code) code.textContent = compileCondRows(state.condRows) || '（空）'
}

// 试算：打后端 /conditions/eval，用当前编译表达式 + 用户填的样例变量。
async function runCondTest () {
  const expr = compileCondRows(state.condRows)
  let variables = {}
  if (state.condTestVars.trim()) {
    try { variables = JSON.parse(state.condTestVars) } catch (e) {
      state.condTest = { ok: false, error: '变量 JSON 非法' }; refreshProp(); return
    }
  }
  try {
    const d = await apiJson('/api/flow/conditions/eval', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ expr, variables })
    })
    state.condTest = { ok: true, result: !!d.result }
  } catch (e) {
    state.condTest = { ok: false, error: e.message }
  }
  refreshProp()
}

// 网关默认流：写 BPMN default 属性（选中的出边 id）。
function setDefaultFlow (flowId) {
  const el = state.selectedElement
  if (!el || !activeModeler()) return
  const modeling = activeModeler().get('modeling')
  try {
    const target = flowId ? (el.businessObject.outgoing || []).find((f) => f.id === flowId) : undefined
    modeling.updateProperties(el, { default: target || undefined })
  } catch (e) { toast('设置默认流失败: ' + e.message) }
}

// ————————————————————— UserTask 分页签绑定 + 写回（P3） —————————————————————

function bindUserTaskTabs (root) {
  const el = state.selectedElement
  if (!el || el.type !== 'bpmn:UserTask') return
  // 页签切换。
  root.querySelectorAll('[data-uttab]').forEach((btn) => btn.addEventListener('click', () => {
    state.utTab = btn.dataset.uttab
    ensureIdnForTab()
    refreshProp()
  }))
  // 办理人类型单选。
  root.querySelectorAll('[data-akind]').forEach((btn) => btn.addEventListener('click', () => {
    setAssigneeKind(btn.dataset.akind)
  }))
  // 身份选择器（下拉）→ 写回。
  root.querySelector('[data-idn-pick]')?.addEventListener('change', (e) => applyAssigneeValue(e.target.value))
  // 身份选择器（文本框，外接/未加载时）→ 写回。
  root.querySelector('[data-idn-text]')?.addEventListener('change', (e) => applyAssigneeValue(e.target.value))
  // 表达式办理人。
  root.querySelector('[data-akind-val]')?.addEventListener('change', (e) => applyAssigneeValue(e.target.value))
  // 审批方式单选。
  root.querySelectorAll('[data-approval]').forEach((btn) => btn.addEventListener('click', () => {
    setApprovalMode(btn.dataset.approval)
  }))
  // multiInstance 明细字段。
  root.querySelectorAll('[data-mi-f]').forEach((inp) => inp.addEventListener('change', () => {
    setMultiInstanceField(inp.dataset.miF, inp.value)
  }))
  // 首次进办理人/抄送页签时懒加载身份列表。
  ensureIdnForTab()
}

// 当前页签需要身份列表时懒加载（办理人/抄送）。加载完重渲让下拉出现。
async function ensureIdnForTab () {
  if (state.utTab !== 'assignee' && state.utTab !== 'cc') return
  // 已加载则不重复。
  if (state.idnCache.__loaded) return
  state.idnCache.__loaded = true
  try {
    // 先探模式（external 时列表可能空，退化文本框）。
    const m = await apiJson('/api/flow/identity/mode').catch(() => null)
    state.idnMode = m ? m.mode : 'external'
    for (const ent of ['orgs', 'roles', 'positions', 'users']) {
      try {
        const d = await apiJson('/api/flow/identity/' + ent)
        state.idnCache[ent] = d.items || []
      } catch { state.idnCache[ent] = [] }
    }
  } catch { /* 身份端点不可用：保持文本框退化 */ }
  refreshProp()
}

// 切办理人类型：清掉旧的三处办理人属性，按新类型的空值重写（值待选择器填）。
function setAssigneeKind (kind) {
  const el = state.selectedElement
  if (!el) return
  state.assigneeKind[el.id] = kind // 记住显式选择：切到需值类型时值暂空，避免重渲反推回落 user
  const def = ASSIGNEE_KINDS.find((k) => k.key === kind) || ASSIGNEE_KINDS[0]
  // 无参关系型 / 发起人类：直接写定值。
  if (kind === 'initiator' || kind === 'initiatorLeader') {
    writeAssigneeAttrs(el, 'candidates', def.expr(''))
  } else {
    // 需要值的类型：先清空，等选择器/文本框填值。保留 state.utTab。
    writeAssigneeAttrs(el, def.attr, '')
  }
  refreshProp()
}

// 选择器/文本框选定值 → 按当前类型的 expr 规则写回。
function applyAssigneeValue (value) {
  const el = state.selectedElement
  if (!el) return
  const inferred = readAssignee(el.businessObject)
  // 同 assigneeTabHtml：属性为空时以记忆的显式类型为准，否则选中角色/岗位的值会被当成 user 直派写入。
  const isEmpty = inferred.kind === 'user' && inferred.value === ''
  const kind = (isEmpty && state.assigneeKind[el.id]) ? state.assigneeKind[el.id] : inferred.kind
  const def = ASSIGNEE_KINDS.find((k) => k.key === kind) || ASSIGNEE_KINDS[0]
  const expr = def.expr(value)
  writeAssigneeAttrs(el, def.attr, expr)
  // 不整段重渲（避免打断选择），仅在需要时由用户切页签触发。
}

// 统一写办理人：清掉 assignee/candidateGroups/candidates 三处，只写目标属性。
// attr='assignee' → flowable:assignee；attr='candidates' → cmx:candidates（编译器按本地名 candidates 解析）。
// ⚠ 必须用带前缀的属性名逐项增删（undefined=删除），不能 updateProperties(el,{ $attrs:{...} })——
//    后者会被 bpmn-js 当成名为 "$attrs" 的普通属性塞进 businessObject.$attrs.$attrs，旧值清不掉
//    （实测根因：切「角色/岗位」后旧 flowable:assignee 残留 → readAssignee 反推回落「指定人员」）。
function writeAssigneeAttrs (el, attr, value) {
  const props = {
    'flowable:assignee': undefined, assignee: undefined,
    'flowable:candidateGroups': undefined, candidateGroups: undefined,
    'cmx:candidates': undefined, candidates: undefined,
  }
  if (value) {
    if (attr === 'assignee') props['flowable:assignee'] = value
    else props['cmx:candidates'] = value
  }
  try {
    activeModeler().get('modeling').updateProperties(el, props)
  } catch (e) { toast('设置办理人失败: ' + e.message) }
}

// 切审批方式：single = 删 loopCharacteristics；parallel/sequential = 建/改 multiInstanceLoopCharacteristics。
function setApprovalMode (mode) {
  const el = state.selectedElement
  if (!el || !activeModeler()) return
  const moddle = activeModeler().get('moddle')
  const modeling = activeModeler().get('modeling')
  try {
    if (mode === 'single') {
      modeling.updateProperties(el, { loopCharacteristics: undefined })
    } else {
      const b = el.businessObject
      let lc = b.loopCharacteristics
      if (!lc || lc.$type !== 'bpmn:MultiInstanceLoopCharacteristics') {
        lc = moddle.create('bpmn:MultiInstanceLoopCharacteristics', {})
      }
      lc.isSequential = (mode === 'sequential')
      modeling.updateProperties(el, { loopCharacteristics: lc })
    }
  } catch (e) { toast('设置审批方式失败: ' + e.message) }
  refreshProp()
}

// 写 multiInstance 明细字段（collection/elementVariable/completionCondition）。
function setMultiInstanceField (f, value) {
  const el = state.selectedElement
  if (!el || !activeModeler()) return
  const moddle = activeModeler().get('moddle')
  const modeling = activeModeler().get('modeling')
  const b = el.businessObject
  let lc = b.loopCharacteristics
  if (!lc) { lc = moddle.create('bpmn:MultiInstanceLoopCharacteristics', {}); }
  try {
    if (f === 'collection') {
      // 用 cmx/flowable 扩展属性存 collection（编译器按本地名 collection 读）。
      const attrs = { ...(lc.$attrs || {}) }
      if (value) attrs['flowable:collection'] = value; else { delete attrs['flowable:collection']; delete attrs.collection }
      lc.$attrs = attrs
    } else if (f === 'elementVariable') {
      lc.elementVariable = value || undefined
    } else if (f === 'completionCondition') {
      lc.completionCondition = value ? moddle.create('bpmn:FormalExpression', { body: value }) : undefined
    }
    modeling.updateProperties(el, { loopCharacteristics: lc })
  } catch (e) { toast('设置会签参数失败: ' + e.message) }
}

// ————————————————————— 数据/动作 —————————————————————

// 分组下拉数据源（20260902 重构：/definition-groups；失败静默降级为空列表）。
async function loadGroups () {
  try {
    const d = await apiJson('/api/flow/definition-groups')
    state.groups = (d.rows || []).map((g) => ({ id: Number(g.id), name: g.name || '', enabled: g.enabled !== false }))
    refreshContentChrome()
  } catch { state.groups = [] }
}

async function loadDefs () {
  state.loading = true; refreshView('explorer')
  try {
    // 设计器列表来源定义库（草稿+已发布全列），而非引擎运行态已装载定义。
    // 保留全量（含 isSubflow 标记）：explorer 渲染时按 isSubflow 过滤只显主流程，
    // 但绑定目标下拉 / 子流程编辑器变体侧栏仍需完整集，故不在此剔除子流程。
    const d = await apiJson('/api/flow/design/definitions')
    state.definitions = (d.definitions || []).filter((x) => x.startable !== false)
    state.defPage = 0    // 刷新/重载列表回首页
  } catch (e) { toast('加载定义失败: ' + e.message); state.definitions = [] }
  state.loading = false; refreshView('explorer')
}

// 主流程列表（explorer 展示用）：默认排除子流程（被 callActivity/组织绑定引用为目标者）。
// showSubflows 开关打开时全显（便于直接维护/排查）。
function mainDefs () {
  return state.showSubflows ? state.definitions : state.definitions.filter((d) => !d.isSubflow)
}
function subflowCount () {
  return state.definitions.filter((d) => d.isSubflow).length
}

// 加载定义到画布。version 传具体版本号则载入该历史版本（只读快照，可另存草稿覆盖）；
// 不传/undefined 用该定义当前应展示版本（见 defVersion），null 明确取草稿。
// U1：有未保存编辑时确认丢弃。返回 true=可继续（无脏或用户确认丢弃），false=取消。
function confirmDiscard (action) {
  if (!state.dirty) return true
  const msg = `当前流程有未保存的改动，${action || '继续'}将丢弃这些改动。确定继续吗？`
  try { return (typeof window !== 'undefined' && window.confirm) ? window.confirm(msg) : true }
  catch { return true }
}

async function loadDef (key, version) {
  if (!confirmDiscard('切换定义')) return
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
    state.groupId = detail.groupId ?? null
    state.shownVersion = detail.shownVersion ?? v ?? null
    if (v != null) state.selectedVersion[key] = v
    const restoreScroll = preserveExplorerScroll()   // 点选流程后保持列表滚动位置不变（不跳回第一行）
    refreshView('explorer')       // explorer 高亮当前选中 + 版本下拉同步
    restoreScroll()               // 重渲染会把内层列表滚动重置为 0，这里原样复位
    refreshContentChrome()        // content 工具栏版本徽标/下拉同步（不销毁画布：openDiagram 会就地导入）
    refreshView('property')       // property 区 DAM 归属同步
    await openDiagram(detail.bpmnXml)   // 就地把 XML 导入现有 modeler（内含 readVarSchemaFromXml）
    refreshView('property')             // ⑤ varSchema 读入后刷新属性区（变量摘要/下拉数据源就位）
    // 协同 M1：仅编辑「草稿」（shownVersion==null）时启用感知+防冲突；看已发布版本(只读)则停。
    if (state.shownVersion == null) startCollab(key, detail.updatedAt)
    else stopCollab()
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
    const gs = root?.querySelector?.('[data-group]')
    if (gs) gs.value = state.groupId == null ? '' : String(state.groupId)
  }
}

async function newDiagram () {
  if (!confirmDiscard('新建流程')) return
  state.selectedKey = null
  state.name = '新建流程'
  // 新建流程默认继承 explorer 当前 DAM 过滤（在哪个模块下看，就归到哪个模块）。
  state.defDam = { domain: state.fDomain || '', application: state.fApp || '', module: state.fModule || '' }
  refreshView('explorer'); refreshView('property'); syncNameInput()
  await openDiagram(EMPTY_DIAGRAM)
  toast('已新建空白流程')
}

async function getXml () {
  // E4：modeler 未就绪时（bootCanvas 异步未完成就点保存/发布/导出/校验）给明确报错，
  // 而非「Cannot read properties of null」这类困惑提示。子流程编辑器打开时取其 modeler。
  const m = activeModeler()
  if (!m) throw new Error('画布尚未就绪，请稍候再试')
  const { xml } = await m.saveXML({ format: true })
  // ⑤ 把模块外维护的变量声明注回 XML（bpmn-js 不认 <cmx:varSchema> 扩展元素，故 saveXML 后手工注入）。
  return injectVarSchemaIntoXml(xml)
}

// ————————————————————— P4：导出 / 导入 / 另存为 —————————————————————

// 导出当前画布为 BPMN XML 文件（浏览器下载）。文件名用流程名/ key。
async function doExport () {
  try {
    const xml = await getXml()
    const base = (state.name || state.selectedKey || 'process').replace(/[^\w.-]+/g, '_')
    const blob = new Blob([xml], { type: 'application/xml' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url; a.download = base + '.bpmn'
    document.body.appendChild(a); a.click(); a.remove()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
    toast('已导出: ' + base + '.bpmn')
  } catch (e) { toast('导出失败: ' + e.message) }
}

// 导入 BPMN XML 文件 → 载入画布为新草稿（不覆盖现有定义；导入后另存为新流程）。
async function doImport (file) {
  if (!file) return
  if (!confirmDiscard('导入')) return
  try {
    const xml = await file.text()
    if (!/<(bpmn:)?definitions/i.test(xml)) { toast('导入失败: 不是 BPMN XML'); return }
    // 先校验（真编译），非法则拒绝导入到画布。
    let vr = null
    try {
      vr = await apiJson('/api/flow/definitions/validate', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ bpmnXml: xml }),
      })
    } catch { /* 校验端点不可用则跳过，仍尝试载入 */ }
    if (vr && vr.valid === false) { toast('导入的流程编译不过: ' + (vr.error || '')); return }
    // 载入画布，视为新草稿（清 selectedKey，让下次保存/另存为铸新号）。
    state.selectedKey = null
    state.name = (file.name || '导入流程').replace(/\.(bpmn|xml)$/i, '')
    state.shownVersion = null
    refreshView('explorer'); refreshContentChrome()
    await openDiagram(xml)
    toast('已导入到画布（新草稿）：' + state.name + '，点「保存草稿」落库')
  } catch (e) { toast('导入失败: ' + e.message) }
}

// 另存为：把当前画布复制成一个新流程（新 key）。后端 key 取自 BPMN process id，故必须
// 先把 XML 里的 process id 改成一个新值，否则会覆盖原定义（同 id → 同 key）。
async function doSaveAs () {
  try {
    const newName = (typeof window !== 'undefined' && window.prompt)
      ? window.prompt('另存为新流程，输入名称：', (state.name || '流程') + ' 副本')
      : (state.name || '流程') + ' 副本'
    if (newName == null) return // 取消
    let xml = await getXml()
    // 铸新 process id（原 id + _copy + 短随机后缀，避免撞既有 key）。
    const newId = newProcessId(xml)
    xml = rewriteProcessId(xml, newId)
    state.selectedKey = null
    state.name = newName || (state.name + ' 副本')
    const r = await apiJson('/api/flow/definitions/draft', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        name: state.name,
        domain: state.defDam.domain || null,
        application: state.defDam.application || null,
        module: state.defDam.module || null,
        bpmnXml: xml,
        groupId: state.groupId ?? undefined,
      }),
    })
    state.selectedKey = r.key
    // 载入改了 id 的 XML，让后续编辑与新 key 一致。
    await openDiagram(xml)
    toast('已另存为: ' + r.key)
    await loadDefs()
    refreshContentChrome()
  } catch (e) { toast('另存为失败: ' + e.message) }
}

// 从 XML 提取当前 process id，生成一个新 id（原名 + _copy + 计数后缀，避开既有定义 key）。
function newProcessId (xml) {
  const m = xml.match(/<(?:bpmn:)?process\b[^>]*\bid="([^"]+)"/)
  const base = (m ? m[1] : 'process').replace(/_copy\d*$/, '')
  const existing = new Set((state.definitions || []).map((d) => d.key))
  let i = 1
  let cand = `${base}_copy`
  while (existing.has(cand)) { i += 1; cand = `${base}_copy${i}` }
  return cand
}

// 把 XML 里 process 的 id 改成 newId，并同步 BPMNPlane 的 bpmnElement 引用（DI 指向 process）。
function rewriteProcessId (xml, newId) {
  const m = xml.match(/<(?:bpmn:)?process\b[^>]*\bid="([^"]+)"/)
  if (!m) return xml
  const oldId = m[1]
  // 只替换 process 的 id 与 BPMNPlane bpmnElement=oldId（避免误伤同名节点 id）。
  let out = xml.replace(
    /(<(?:bpmn:)?process\b[^>]*\bid=")([^"]+)(")/,
    (_s, a, _id, c) => a + newId + c
  )
  out = out.replace(
    new RegExp('(bpmnElement=")' + oldId.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '(")', 'g'),
    '$1' + newId + '$2'
  )
  return out
}

async function doValidate () {
  try {
    const xml = await getXml()
    if (!/bpmn:process|<process/i.test(xml)) { toast('校验失败: 无 process'); return }
    // 真校验：打后端 /definitions/validate，跑真实 compile + check_topology（不落库）。
    const r = await apiJson('/api/flow/definitions/validate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ bpmnXml: xml }),
    })
    if (r && r.valid) toast('✓ 校验通过：编译 OK（' + (r.key || 'process') + '），可发布')
    else toast('✗ 校验未过：' + ((r && r.error) || '编译失败'))
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
        groupId: state.groupId ?? undefined,  // 分组仅新建首存生效（20260902 重构）
        baseUpdatedAt: state.collab.baseUpdatedAt || undefined,  // 协同乐观锁基线
        updatedBy: currentUser(),
      }),
    })
    // 协同：草稿在你 base 之后被他人改过 → 让用户选覆盖 / 载入最新。
    if (r && r.conflict) {
      const who = r.updatedBy || '他人'
      const when = (r.currentUpdatedAt || '').slice(0, 19).replace('T', ' ')
      const overwrite = window.confirm(`${who} 刚保存了草稿（${when}）。\n\n确定 = 覆盖保存（以你的为准）\n取消 = 放弃保存并载入最新`)
      if (overwrite) { state.collab.baseUpdatedAt = r.currentUpdatedAt; return doSave() }
      if (state.selectedKey) await loadDef(state.selectedKey)
      return
    }
    state.selectedKey = r.key
    state.collab.baseUpdatedAt = r.updatedAt || state.collab.baseUpdatedAt  // 保存成功推进基线
    state.dirty = false   // U1：保存成功清脏
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
      baseUpdatedAt: state.collab.baseUpdatedAt || undefined,
      updatedBy: currentUser(),
    }),
  })
  // 协同：发布前静默存草稿撞冲突 → 抛出让发布流程中止并提示（勿静默覆盖）。
  if (r && r.conflict) throw new Error(`草稿已被 ${r.updatedBy || '他人'} 更新，请先「载入最新」再发布`)
  state.selectedKey = r.key
  state.collab.baseUpdatedAt = r.updatedAt || state.collab.baseUpdatedAt
  return r
}

// ═══════════════════════════ 协同 M1 · 感知层 + 防冲突 ═══════════════════════════
// 仅在编辑「草稿」时启用（shownVersion==null）。SSE 收 presence/draft.saved（后端 collab.rs），
// presence 心跳/选中经 POST 发；远端选中用 canvas.addMarker 高亮（复用 sim/diff 技法）。
// 传输：/api/flow/v1/design/*（fetch 流式 SSE 带 Authorization 头；defKey 过滤）。

function selectedId () { return state.selectedElement ? state.selectedElement.id : null }

function collabContentRoot () {
  for (const host of Array.from(state.hosts || [])) {
    if (host && host.__flowView === 'content' && host.isConnected) { const r = hostRoot(host); if (r) return r }
  }
  return null
}
// 向后端 presence 端点发一条（fire-and-forget；apiJson 已注入 X-User 定 actor）。
function collabPost (path, extra) {
  const c = state.collab
  if (!c.defKey || !c.sessionId) return
  apiJson('/api/flow/v1/design/presence/' + path, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(Object.assign({ defKey: c.defKey, sessionId: c.sessionId, user: c.user }, extra || {})),
  }).catch(() => {})
}
// 页面卸载：sendBeacon 尽力离场（fetch 卸载期不保证送达；leave 仅按 sessionId 无需鉴权）。
function collabLeaveBeacon () {
  const c = state.collab
  if (!c.on || !c.defKey || !c.sessionId) return
  try {
    const url = (CFG.apiBase || '') + '/api/flow/v1/design/presence/leave'
    const body = JSON.stringify({ defKey: c.defKey, sessionId: c.sessionId })
    if (navigator.sendBeacon) navigator.sendBeacon(url, new Blob([body], { type: 'application/json' }))
  } catch { /* ignore */ }
}

function startCollab (defKey, baseUpdatedAt) {
  const c = state.collab
  if (c.on && c.defKey === defKey) { if (baseUpdatedAt) c.baseUpdatedAt = baseUpdatedAt; return }
  stopCollab()
  if (!defKey) return
  c.on = true; c.defKey = defKey; c.user = currentUser(); c.baseUpdatedAt = baseUpdatedAt || null; c.notice = null
  c.opSeen = {}  // 新会话/换草稿：清 LWW seq 记录（旧 defKey 的 seq 不可跨草稿比较）
  c.sessionId = c.sessionId || ('s-' + Math.random().toString(36).slice(2, 10))
  collabPost('join', { selection: selectedId() })
  try {
    // 协同 SSE：走共享 openSseStream（fetch 流式，Authorization 头由门户全局拦截器注入）。
    c.es = openSseStream('/api/flow/v1/design/collab?defKey=' + enc(defKey), {
      presence: (d) => onPresence(d),
      'draft.saved': (d) => onDraftSaved(d),
      op: (d) => onRemoteOp(d),
    }, {
      // 连接就绪后再 join 一次：首个 join 在流建立前发出会丢自己的初始 roster——onopen 补发。
      onopen: () => {
        if (state.collab.on && state.collab.defKey === defKey) collabPost('join', { selection: selectedId() })
      },
    }, CFG)
  } catch { /* 流不可用则降级为仅心跳 */ }
  c.hbTimer = setInterval(() => collabPost('heartbeat', { selection: selectedId() }), 10000)
  if (!window.__flowCollabUnload) {
    window.__flowCollabUnload = true
    window.addEventListener('beforeunload', collabLeaveBeacon)
    window.addEventListener('pagehide', collabLeaveBeacon)
  }
  renderPresenceBar()
}
function stopCollab () {
  const c = state.collab
  if (c.hbTimer) { clearInterval(c.hbTimer); c.hbTimer = null }
  if (c.selTimer) { clearTimeout(c.selTimer); c.selTimer = null }
  if (c.on) collabPost('leave', {})
  if (c.es) { try { c.es.close() } catch { /* ignore */ } c.es = null }
  clearCollabMarks()
  c.on = false; c.defKey = null; c.roster = []; c.notice = null
  renderPresenceBar(); renderCollabNotice()
}
// 选中变化 → 去抖广播（远端画布高亮我选的节点）。
function broadcastSelection () {
  const c = state.collab
  if (!c.on) return
  clearTimeout(c.selTimer)
  c.selTimer = setTimeout(() => collabPost('select', { selection: selectedId() }), 250)
}

// 协同 M2：本端属性编辑 → POST /design/op（服务端盖 seq 后广播；远端就地合并）。fire-and-forget。
function broadcastOp (elementId, prop, value) {
  const c = state.collab
  if (!c.on || !c.defKey || !c.sessionId || !elementId) return
  // value 统一成 JSON 可序列化：undefined/空串→null（远端据此删属性）。
  const v = (value === undefined || value === '') ? null : value
  apiJson('/api/flow/v1/design/op', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ defKey: c.defKey, sessionId: c.sessionId, user: c.user, op: 'updateProperties', elementId, props: { [prop]: v } }),
  }).catch(() => {})
}

// ————————————————————— 协同 M3：结构级增删/移动实时合并 —————————————————————
//
// 捕获本端 bpmn-js 结构事件（增删节点/连线、移动）→ 广播足以在对端重放的 op；对端 applyStructOp
// 经 modeling API 重建等价元素。导入/远端回放自触发的事件用 __applying/__loadingDiagram 守卫挡掉。
// 幂等：create 若已存在则跳、remove 若已无则跳；move 用 `${id}::pos`→seq 的 LWW。冲突取务实 last-writer。

// 结构 op 是否应广播（协同开、非回放中、非载图中、非子流程钻入）。
function structBroadcastable () {
  const c = state.collab
  return c.on && c.defKey && c.sessionId && !c.__applying && !state.__loadingDiagram && !state.subNav
}
// 发一条结构 op（fire-and-forget，服务端盖 seq 广播）。
function postStructOp (op, extra) {
  const c = state.collab
  apiJson('/api/flow/v1/design/op', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ defKey: c.defKey, sessionId: c.sessionId, user: c.user, op, elementId: extra.elementId || '', props: extra }),
  }).catch(() => {})
}
// 捕获 shape/connection 增删事件 → 广播。
function captureStructOp (evName, el) {
  if (!structBroadcastable() || !el || !el.id) return
  if (el.id === '__implicitroot' || (el.type && el.type.indexOf('Root') >= 0)) return  // 根不广播
  const bo = el.businessObject || {}
  if (evName === 'shape.added') {
    if (el.type === 'label') return  // 外部标签随宿主元素创建，不单独广播
    postStructOp('createShape', {
      elementId: el.id, bpmnType: el.type,
      x: el.x, y: el.y, w: el.width, h: el.height,
      parentId: (el.parent && el.parent.id) || null,
      name: bo.name || null,
    })
  } else if (evName === 'connection.added') {
    postStructOp('createConnection', {
      elementId: el.id, bpmnType: el.type || 'bpmn:SequenceFlow',
      sourceId: (el.source && el.source.id) || (bo.sourceRef && bo.sourceRef.id) || null,
      targetId: (el.target && el.target.id) || (bo.targetRef && bo.targetRef.id) || null,
      waypoints: (el.waypoints || []).map((w) => ({ x: w.x, y: w.y })),
    })
  } else if (evName === 'shape.removed' || evName === 'connection.removed') {
    postStructOp('removeElements', { elementId: el.id, elementIds: [el.id] })
  }
}
// 捕获移动/尺寸变化 → 广播每个元素的终位（连线由端点自动重算，不单独广播）。
function captureMoveOps (els) {
  if (!structBroadcastable() || !Array.isArray(els)) return
  for (const el of els) {
    if (!el || !el.id || el.waypoints) continue  // 连线跳过（跟随端点）
    if (el.type === 'label' || el.type === 'root' || (el.type && el.type.indexOf('Root') >= 0)) continue
    if (typeof el.x !== 'number' || typeof el.y !== 'number') continue
    postStructOp('moveShape', { elementId: el.id, x: el.x, y: el.y, w: el.width, h: el.height })
  }
}

// 对端回放一条结构 op。全程 __applying/__loadingDiagram 守卫（防回声 + 不标脏/不清 marker）。
function applyStructOp (p) {
  const c = state.collab
  const m = state.modeler; if (!m || state.subNav) return
  let modeling, reg, elementFactory, bpmnFactory, canvas
  try {
    modeling = m.get('modeling'); reg = m.get('elementRegistry')
    elementFactory = m.get('elementFactory'); bpmnFactory = m.get('bpmnFactory'); canvas = m.get('canvas')
  } catch { return }
  const prevLoad = state.__loadingDiagram
  c.__applying = true; state.__loadingDiagram = true
  try {
    if (p.op === 'createShape') {
      if (reg.get(p.elementId)) return  // 幂等：已存在
      const parent = (p.parentId && reg.get(p.parentId)) || canvas.getRootElement()
      const bo = bpmnFactory.create((p.bpmnType || 'bpmn:Task').replace(/^bpmn:/, 'bpmn:'))
      bo.id = p.elementId
      if (p.name != null) bo.name = p.name
      const shape = elementFactory.createShape({ type: p.bpmnType || 'bpmn:Task', businessObject: bo, id: p.elementId, width: p.w || 100, height: p.h || 80 })
      const pos = { x: (p.x || 0) + (p.w || 100) / 2, y: (p.y || 0) + (p.h || 80) / 2 }  // modeling.createShape 取中心点
      modeling.createShape(shape, pos, parent)
    } else if (p.op === 'createConnection') {
      if (reg.get(p.elementId)) return
      const src = p.sourceId && reg.get(p.sourceId)
      const tgt = p.targetId && reg.get(p.targetId)
      if (!src || !tgt) return  // 端点未就绪（乱序到达）→ 跳过，宽容
      const bo = bpmnFactory.create((p.bpmnType || 'bpmn:SequenceFlow').replace(/^bpmn:/, 'bpmn:'))
      bo.id = p.elementId
      const conn = modeling.createConnection(src, tgt, { type: p.bpmnType || 'bpmn:SequenceFlow', businessObject: bo, id: p.elementId }, src.parent || canvas.getRootElement())
      if (conn && p.waypoints && p.waypoints.length >= 2) { try { modeling.updateWaypoints(conn, p.waypoints.map((w) => ({ x: w.x, y: w.y }))) } catch {} }
    } else if (p.op === 'removeElements') {
      const ids = p.elementIds || [p.elementId]
      const els = ids.map((id) => reg.get(id)).filter(Boolean)
      if (els.length) modeling.removeElements(els)
    } else if (p.op === 'moveShape') {
      // 位移 LWW：同元素只应用更大 seq。
      const k = p.elementId + '::pos'
      if (typeof p.seq === 'number') { if ((c.opSeen[k] || 0) >= p.seq) return; c.opSeen[k] = p.seq }
      const el = reg.get(p.elementId)
      if (!el) return  // 已删元素上的移动 → 存在性守卫跳过
      const dx = (p.x || 0) - el.x, dy = (p.y || 0) - el.y
      if (dx !== 0 || dy !== 0) modeling.moveShape(el, { x: dx, y: dy })
    }
  } catch { /* 单 op 失败不阻断后续 */ }
  finally { c.__applying = false; state.__loadingDiagram = prevLoad }
  refreshOutline(); scheduleMinimapRender()
  if (state.__badgeRaf) cancelAnimationFrame(state.__badgeRaf)
  state.__badgeRaf = requestAnimationFrame(() => { state.__badgeRaf = null; renderNodeBadges() })
}


// SSE: 远端属性 op → 对象级 LWW 合并到本端画布。自己的回声按 origin 忽略；旧 seq 丢弃。
function onRemoteOp (ev) {
  const c = state.collab
  const p = ev && ev.payload
  if (!p || !p.elementId && !p.op) return
  if (p.origin === c.sessionId) return  // 自己的回声，忽略
  // 协同 M3：结构级 op（create/delete/move）走独立回放路径。字段在 props 里（后端信封 {op,elementId,props,seq,origin}）。
  if (p.op === 'createShape' || p.op === 'createConnection' || p.op === 'removeElements' || p.op === 'moveShape') {
    applyStructOp({ ...(p.props || {}), op: p.op, elementId: p.elementId || (p.props && p.props.elementId), seq: p.seq })
    return
  }
  if (p.op !== 'updateProperties' || !p.elementId) return
  const m = state.modeler; if (!m) return
  let reg; try { reg = m.get('elementRegistry') } catch { return }
  const el = reg.get(p.elementId)
  if (!el) return  // 元素本端不存在（结构级差异留 M3；此处宽容跳过）
  const props = p.props || {}
  for (const prop of Object.keys(props)) {
    // 对象级 LWW：同 元素+属性 只应用更大的 seq（防乱序/回环覆盖新值）。seq 非数字则跳过闸门直接应用。
    const k = p.elementId + '::' + prop
    if (typeof p.seq === 'number') {
      if ((c.opSeen[k] || 0) >= p.seq) continue
      c.opSeen[k] = p.seq
    }
    // 回放期间置 __applying：applyProp 不再回广播（防回声风暴）；置 __loadingDiagram：commandStack
    //   变化不标脏、不清模拟/diff marker（远端合并非本人「未保存编辑」，语义上是同步对端已存意图）。
    const prevLoad = state.__loadingDiagram
    c.__applying = true; state.__loadingDiagram = true
    try {
      const val = (props[prop] === null) ? '' : props[prop]
      applyPropTo(el, prop, val)
    } catch { /* 单属性失败不阻断其余 */ }
    finally { c.__applying = false; state.__loadingDiagram = prevLoad }
  }
  // 若远端改的正是本端选中元素 → 刷新属性面板 + 徽章反映最新值。
  if (state.selectedElement && state.selectedElement.id === p.elementId) refreshView('property')
  if (state.__badgeRaf) cancelAnimationFrame(state.__badgeRaf)
  state.__badgeRaf = requestAnimationFrame(() => { state.__badgeRaf = null; renderNodeBadges() })
}

// SSE: presence 全 roster 快照 → 更新在场条 + 远端选中高亮。
function onPresence (ev) {
  state.collab.roster = (ev && ev.payload && ev.payload.roster) || []
  applyCollabMarks()
  renderPresenceBar()
}
// SSE: 他人保存了草稿 → 通知（不脏可一键载入；脏则提示自决）。自己的保存忽略。
function onDraftSaved (ev) {
  const c = state.collab
  if (!ev || ev.user === c.user) return
  c.notice = { user: ev.user, updatedAt: ev.payload && ev.payload.updatedAt }
  renderCollabNotice()
}

// 远端选中高亮：所有他人选中的元素打一个统一「协同选中」描边（谁选的看在场条）。
function clearCollabMarks () {
  const m = state.modeler; if (!m) return
  let canvas; try { canvas = m.get('canvas') } catch { return }
  for (const id of state.collab.marked) { try { canvas.removeMarker(id, 'flow-collab-sel') } catch { /* 已删 */ } }
  state.collab.marked = []
}
function applyCollabMarks () {
  clearCollabMarks()
  const m = state.modeler; if (!m) return
  let canvas, reg; try { canvas = m.get('canvas'); reg = m.get('elementRegistry') } catch { return }
  const me = state.collab.sessionId
  for (const p of (state.collab.roster || [])) {
    if (p.sessionId === me || !p.selection) continue
    if (reg.get(p.selection)) { try { canvas.addMarker(p.selection, 'flow-collab-sel'); state.collab.marked.push(p.selection) } catch { /* ignore */ } }
  }
}

// 在场条（content 画布右上；单人不显示）。头像=user 首二字，色由后端派生。
function renderPresenceBar () {
  const root = collabContentRoot(); if (!root) return
  const bar = root.querySelector('[data-collab-bar]'); if (!bar) return
  const c = state.collab
  const all = c.roster || []
  if (!c.on || all.length <= 1) { bar.innerHTML = ''; return }
  const chips = all.map((p) => {
    const self = p.sessionId === c.sessionId
    const initial = esc((p.user || '?').slice(0, 2))
    const tip = `${esc(p.user || '?')}${self ? '（你）' : ''}${p.selection ? ' · 选中 ' + esc(p.selection) : ''}`
    return `<span class="flow-collab-avatar${self ? ' me' : ''}" style="--c:${esc(p.color || '#888')}" title="${tip}">${initial}</span>`
  }).join('')
  bar.innerHTML = `<span class="flow-collab-label">协作中</span>${chips}`
}
// 「草稿已被他人更新」通知条（content 画布顶部居中）。
function renderCollabNotice () {
  const root = collabContentRoot(); if (!root) return
  const el = root.querySelector('[data-collab-notice]'); if (!el) return
  const n = state.collab.notice
  if (!n) { el.innerHTML = ''; el.classList.remove('show'); return }
  el.classList.add('show')
  el.innerHTML = `<ui5-icon name="history"></ui5-icon><span>${esc(n.user || '他人')} 更新了草稿${state.dirty ? '（你有未保存改动，载入将丢弃）' : ''}</span>
    <button class="flow-btn slim" data-collab-reload><ui5-icon name="refresh"></ui5-icon> 载入最新</button>
    <button class="flow-icon-btn" data-collab-dismiss title="忽略"><ui5-icon name="decline"></ui5-icon></button>`
  el.querySelector('[data-collab-reload]')?.addEventListener('click', async () => { state.collab.notice = null; if (state.selectedKey) await loadDef(state.selectedKey) })
  el.querySelector('[data-collab-dismiss]')?.addEventListener('click', () => { state.collab.notice = null; renderCollabNotice() })
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  /* bpmn 图标字体的 @font-face 已注入主文档 head（shadow 内声明 font-face 无效，Chrome 限制）。 */
  /* ① 节点业务徽章（overlays）：叠在节点左上角，随缩放平移跟随。 */
  .cmx-badges{display:flex;gap:3px;flex-wrap:wrap;pointer-events:none;max-width:180px}
  .cmx-badge{display:inline-flex;align-items:center;gap:2px;height:17px;padding:0 5px;border-radius:9px;font-size:10px;font-weight:600;line-height:1;color:var(--sapGroup_ContentBorderColor, #ffffff);white-space:nowrap;box-shadow:0 1px 2px rgba(0,0,0,.18)}
  .cmx-badge ui5-icon{width:10px;height:10px;color: #fff;min-width:10px}
  .cmx-badge i{font-style:normal}
  .cmx-badge.par{background:#8250df} .cmx-badge.seq{background:#6639ba}
  .cmx-badge.who{background:var(--sapInformationElementColor, #0969da)} .cmx-badge.svc{background:#57606a}
  .cmx-badge.rule{background:var(--sapCriticalElementColor, #bf8700)} .cmx-badge.sub{background:var(--sapPositiveElementColor, #1a7f37)}
  .cmx-badge.msg{background:var(--sapCriticalElementColor, #bc4c00)} .cmx-badge.timer{background:var(--sapNegativeElementColor, #cf222e)}
  .cmx-badge.term{background:var(--sapNegativeElementColor, #82071e)}
  /* 设计令牌层：全部派生自门户 --sap* 主题令牌（light/dark 自动翻，零 JS），写死值仅作降级 fallback。 */
  .flow{
    --brand:var(--sapButton_Emphasized_Background,var(--sapContent_IconColor,#0969da));
    --brand-ink:var(--sapButton_Emphasized_TextColor,#fff);
    --brand-d:color-mix(in srgb,var(--brand) 62%,var(--ink));
    --ink:var(--sapTextColor,#1f2328);
    --muted:var(--sapContent_LabelColor,#656d76);
    --line:var(--sapGroup_TitleBorderColor,var(--sapField_BorderColor,#d0d7de));
    --line-soft:color-mix(in srgb,var(--line) 55%,transparent);
    --surface:var(--sapBackgroundColor,#f6f8fa);
    --tile:var(--sapTile_Background,var(--sapList_Background,#fff));
    --header:var(--sapList_HeaderBackground,var(--sapTile_Background,#fbfdff));
    --ok:var(--sapPositiveColor,var(--sapSuccessColor,#1a7f37));
    --warn:var(--sapCriticalColor,var(--sapWarningColor,#bf8700));
    --red:var(--sapNegativeColor,var(--sapErrorColor,#cf222e));
    --new:var(--sapInformativeColor,#bc4c00);
    --brand-soft:color-mix(in srgb,var(--brand) 14%,var(--tile));
    --brand-line:color-mix(in srgb,var(--brand) 40%,var(--line));
    --ok-soft:color-mix(in srgb,var(--ok) 15%,var(--tile));
    --warn-soft:color-mix(in srgb,var(--warn) 16%,var(--tile));
    --red-soft:color-mix(in srgb,var(--red) 14%,var(--tile));
    --hover:color-mix(in srgb,var(--brand) 8%,transparent);
    --glow:color-mix(in srgb,var(--brand) 30%,transparent);
    --shadow:color-mix(in srgb,var(--ink) 18%,transparent);
    --grid-dot:color-mix(in srgb,var(--ink) 9%,transparent);
    --code-bg:#0d1117;--code-fg:#7ee787;
    --flow-bar-h:47px;
    color-scheme:light dark;
    font:13px/1.5 var(--sapFontFamily,-apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif);color:var(--ink);background:var(--surface);height:100%;box-sizing:border-box;display:flex;flex-direction:column}
  .flow *{box-sizing:border-box}
  .flow ui5-icon{color:currentColor}
  /* 三区顶部标题/工具栏统一高度（explorer/property 标题区 = content 工具栏，--flow-bar-h）。 */
  .flow-head{height:var(--flow-bar-h);flex:0 0 auto;display:flex;align-items:center;justify-content:space-between;padding:0 12px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .flow-head b{font-size:13px;letter-spacing:.01em} .flow-head span{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-icon-btn{border:1px solid var(--line);background:var(--tile);color:var(--muted);border-radius:8px;width:28px;height:28px;cursor:pointer;display:grid;place-items:center;transition:all .13s}
  .flow-icon-btn:hover{color:var(--brand);border-color:var(--brand-line);background:var(--brand-soft)}
  .flow-def-list{flex:1;overflow:auto;padding:8px}
  /* —— 流程列表分页条（首页/上页/下页/末页） —— */
  .flow-pager{flex:0 0 auto;display:flex;align-items:center;justify-content:center;gap:6px;padding:6px 8px;border-top:1px solid var(--line-soft);background:var(--header)}
  .flow-pg-btn{border:1px solid var(--line);background:var(--tile);border-radius:6px;padding:3px 8px;font-size:12px;line-height:1.4;color:var(--ink);cursor:pointer;white-space:nowrap;transition:all .13s}
  .flow-pg-btn.icon{width:26px;height:26px;padding:0;display:inline-grid;place-items:center;font-size:17px;line-height:1;font-family:ui-monospace,Menlo,monospace}
  .flow-pg-btn:hover:not(:disabled){border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)}
  .flow-pg-btn:disabled{opacity:.4;cursor:not-allowed}
  .flow-pg-info{font-size:12px;color:var(--muted);padding:0 4px;display:flex;flex-direction:column;align-items:center;line-height:1.15;font-family:ui-monospace,Menlo,monospace}
  .flow-pg-info small{font-size:10px;opacity:.8}
  /* —— 大纲面板（explorer 下部，联动画布） —— */
  .flow-outline{flex:0 0 auto;border-top:1px solid var(--line-soft);background:var(--header);display:flex;flex-direction:column;position:relative;min-height:0}
  .flow-outline.open{overflow:hidden}
  .flow-outline.closed{height:auto}
  .flow-ol-resize{position:absolute;top:0;left:0;right:0;height:6px;margin-top:-3px;cursor:ns-resize;z-index:2}
  .flow-outline.closed .flow-ol-resize{display:none}
  .flow-ol-resize:hover{background:var(--brand-soft)}
  .flow-ol-head{display:flex;align-items:center;gap:6px;width:100%;border:0;background:transparent;cursor:pointer;
    padding:8px 10px;font:inherit;color:var(--ink);flex:0 0 auto;border-bottom:1px solid var(--line-soft)}
  .flow-ol-head b{font-size:12.5px}
  .flow-ol-caret{font-size:12px;color:var(--muted)}
  .flow-ol-total{font-size:11px;color:var(--brand-ink);background:var(--brand);border-radius:9px;padding:0 6px;line-height:16px;min-width:16px;text-align:center;font-weight:700}
  .flow-ol-sub{margin-left:auto;font-size:10.5px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-ol-body{flex:1;overflow:auto;padding:4px 0 6px}
  .flow-ol-hint{padding:10px;font-size:12px;color:var(--muted);text-align:center}
  .flow-ol-group{margin-bottom:2px}
  .flow-ol-ghead{display:flex;align-items:center;gap:5px;width:100%;border:0;background:transparent;cursor:pointer;
    padding:5px 10px;font:inherit;color:var(--muted);font-size:11.5px}
  .flow-ol-ghead b{font-size:11.5px;color:var(--ink);font-weight:600}
  .flow-ol-ghead .flow-ol-caret{font-size:10px}
  .flow-ol-count{font-size:10.5px;color:var(--muted);background:var(--line-soft);border-radius:8px;padding:0 5px;line-height:15px}
  .flow-ol-items{display:flex;flex-direction:column}
  .flow-ol-item{display:flex;align-items:center;gap:7px;width:100%;border:0;background:transparent;cursor:pointer;
    padding:5px 10px 5px 22px;font:inherit;font-size:12px;color:var(--ink);text-align:left;border-left:2px solid transparent;transition:background .12s}
  .flow-ol-item:hover{background:var(--hover)}
  .flow-ol-item.on{background:var(--brand-soft);border-left-color:var(--brand);font-weight:600}
  .flow-ol-ic{font-size:13px;color:var(--muted);flex:0 0 auto}
  .flow-ol-item.on .flow-ol-ic{color:var(--brand)}
  .flow-ol-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .flow-ol-empty{padding:3px 10px 3px 22px;font-size:11px;color:var(--muted)}
  .flow-dam{display:flex;flex-direction:column;gap:6px;padding:8px 10px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .flow-dam-row{display:grid;grid-template-columns:1fr 1fr;gap:6px}
  .flow-dam select,.flow-field select{width:100%;height:28px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:7px;padding:0 6px;background:var(--tile);color:var(--ink);transition:border-color .13s}
  .flow-dam select:focus,.flow-field select:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  /* explorer 定义卡 + 版本行 */
  .flow-def-wrap{position:relative;border:1px solid transparent;border-radius:10px;margin-bottom:3px;transition:background .13s,box-shadow .13s,border-color .13s}
  .flow-def-wrap.active{background:var(--brand-soft);border-color:var(--brand-line);box-shadow:0 1px 8px var(--glow)}
  .flow-def-wrap.active::before{content:"";position:absolute;left:0;top:9px;bottom:9px;width:3px;border-radius:0 3px 3px 0;background:var(--brand)}
  .flow-def{width:100%;display:flex;align-items:center;gap:9px;padding:8px 10px;border:0;border-radius:10px;background:transparent;cursor:pointer;text-align:left;color:inherit}
  .flow-def-wrap:not(.active) .flow-def:hover{background:var(--hover)}
  .flow-def-ic{width:26px;height:26px;border-radius:8px;display:grid;place-items:center;color:var(--brand);background:color-mix(in srgb,var(--brand) 12%,var(--tile));flex:0 0 auto;transition:all .13s}
  .flow-def-ic ui5-icon{width:15px;height:15px}
  .flow-def-wrap.active .flow-def-ic{background:var(--brand);color:var(--brand-ink);box-shadow:0 3px 10px var(--glow)}
  .flow-def-main{min-width:0} .flow-def-main b{display:block;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-def-main small{display:block;font-size:10.5px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-def-ver{display:flex;align-items:center;gap:6px;padding:5px 10px 7px 42px}
  .flow-def-ver-ic{color:var(--muted);width:14px;height:14px;flex:0 0 auto}
  .flow-def-vsel{flex:1;min-width:0;height:26px;font:inherit;font-size:11.5px;border:1px solid var(--line);border-radius:6px;padding:0 6px;background:var(--tile);color:var(--ink)}
  .flow-def-vcount{font-size:10px;color:var(--muted);white-space:nowrap;flex:0 0 auto}
  .flow-btn{font:inherit;font-size:12.5px;border:1px solid var(--line);background:var(--tile);color:var(--ink);border-radius:8px;padding:6px 12px;cursor:pointer;font-weight:600;transition:all .13s}
  .flow-btn:hover{border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)} .flow-btn.primary{background:var(--brand);border-color:var(--brand);color:var(--brand-ink);box-shadow:0 2px 8px var(--glow)}
  .flow-btn.primary:hover{background:color-mix(in srgb,var(--brand) 88%,#000);color:var(--brand-ink)}
  .flow-btn.ok{background:var(--ok);border-color:var(--ok);color:var(--brand-ink);box-shadow:0 2px 8px color-mix(in srgb,var(--ok) 28%,transparent)} .flow-btn.ok:hover{background:color-mix(in srgb,var(--ok) 88%,#000);color:var(--brand-ink)} .flow-btn.block{margin:8px 0 0;width:100%;justify-content:center;display:flex;align-items:center;gap:6px}
  .flow-btn[disabled]{opacity:.5;cursor:not-allowed}
  .flow-btn.slim{padding:4px 9px;font-size:11.5px} .flow-btn.slim.is-cur{background:var(--ok);border-color:var(--ok);color:var(--brand-ink)}
  .flow-btn.danger{color:var(--red);border-color:color-mix(in srgb,var(--red) 42%,var(--line));background:var(--tile)} .flow-btn.danger:hover{background:var(--red-soft);border-color:var(--red)}
  .flow-content{height:100%}
  .flow-toolbar{height:var(--flow-bar-h);flex:0 0 auto;display:flex;align-items:center;gap:6px;padding:0 10px;border-bottom:1px solid var(--line-soft);background:var(--header);flex-wrap:nowrap;overflow-x:auto;overflow-y:hidden}
  .flow-toolbar>.flow-btn,.flow-toolbar>.flow-name,.flow-toolbar>.flow-ver-badge,.flow-toolbar>.flow-ver-sel,.flow-toolbar>.flow-tb-div{flex:0 0 auto}
  .flow-name{font:inherit;font-size:12.5px;border:1px solid var(--line);border-radius:8px;padding:6px 10px;width:130px;background:var(--tile);color:var(--ink);transition:border-color .13s}
  .flow-name:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  .flow-tb-div{width:1px;height:20px;background:var(--line);margin:0 1px}
  .flow-group{height:28px;max-width:130px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:8px;padding:0 6px;background:var(--tile);color:var(--ink)}
  .flow-tb-ic{color:var(--muted);width:15px;height:15px}
  .flow-ver-badge{font-size:11.5px;font-weight:700;color:var(--brand-d);background:var(--brand-soft);border:1px solid var(--brand-line);border-radius:6px;padding:3px 8px;white-space:nowrap}
  .flow-ver-badge.cur{color:var(--ok);background:var(--ok-soft);border-color:color-mix(in srgb,var(--ok) 40%,var(--line))}
  .flow-ver-sel{height:28px;max-width:140px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:6px;padding:0 6px;background:var(--tile);color:var(--ink)}
  .flow-sp{flex:1}
  .flow-canvas-wrap{position:relative;flex:1;min-height:0;background:var(--surface) radial-gradient(circle,var(--grid-dot) 1px,transparent 1px) 0 0/22px 22px}
  .flow-canvas{position:absolute;inset:0}
  /* 去掉 bpmn-js 自带的右下角 bpmn.io 水印链接 */
  .flow-canvas .bjs-powered-by{display:none!important}
  /* 缩略图预览（画布右下角，可拖动视口方框）—— 半透明玻璃质感 */
  .flow-minimap{position:absolute;right:14px;bottom:14px;width:212px;background:color-mix(in srgb,var(--tile) 86%,transparent);backdrop-filter:blur(9px);-webkit-backdrop-filter:blur(9px);
    border:1px solid var(--line);border-radius:11px;box-shadow:0 8px 24px var(--shadow);overflow:hidden;z-index:14;user-select:none}
  .flow-mm-head{display:flex;align-items:center;gap:6px;padding:5px 9px;cursor:pointer;border-bottom:1px solid var(--line-soft);background:color-mix(in srgb,var(--header) 70%,transparent)}
  .flow-mm-head b{font-size:11.5px;font-weight:600}
  .flow-mm-ic{font-size:13px;color:var(--brand)}
  .flow-mm-caret{margin-left:auto;font-size:12px;color:var(--muted);line-height:1;width:14px;text-align:center}
  .flow-mm-stage{position:relative;width:100%;height:142px;background:var(--tile);cursor:crosshair;overflow:hidden}
  .flow-mm-diagram{position:absolute;inset:0}
  .flow-mm-diagram svg{width:100%;height:100%;display:block}
  .flow-mm-overlay{position:absolute;inset:0;width:100%;height:100%}
  .flow-mm-vp{fill:color-mix(in srgb,var(--brand) 16%,transparent);stroke:var(--brand);stroke-width:2px;vector-effect:non-scaling-stroke}
  .flow-minimap.collapsed .flow-mm-stage{display:none}
  .flow-minimap.collapsed{width:auto}
  /* 版本管理对话框（content 画布区内浮层） */
  .flow-dialog-mask{position:absolute;inset:0;z-index:30;display:flex;align-items:center;justify-content:center;background:color-mix(in srgb,var(--sapInformationElementColor, #04070c) 46%,transparent);backdrop-filter:blur(2px);-webkit-backdrop-filter:blur(2px);padding:18px}
  .flow-dialog{width:min(560px,100%);max-height:100%;overflow:hidden;display:flex;flex-direction:column;background:var(--tile);border:1px solid var(--brand-line);border-radius:12px;box-shadow:0 22px 54px var(--shadow)}
  .flow-dialog-head{display:flex;align-items:center;gap:10px;padding:12px 14px;border-bottom:1px solid var(--line-soft);background:linear-gradient(135deg,var(--brand-soft),var(--tile))}
  .flow-dialog-ic{width:32px;height:32px;border-radius:9px;background:var(--brand);color:var(--brand-ink);display:grid;place-items:center;flex:0 0 auto;box-shadow:0 3px 10px var(--glow)}
  .flow-dialog-head>div{flex:1;min-width:0} .flow-dialog-head b{display:block;font-size:14px} .flow-dialog-head em{display:block;font-style:normal;font-size:11px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-dialog-err{margin:10px 14px 0;border:1px solid color-mix(in srgb,var(--red) 40%,var(--line));background:var(--red-soft);color:var(--red);border-radius:7px;padding:7px 9px;font-size:12px}
  .flow-dialog-body{padding:12px 14px;overflow:auto}
  /* ⑤ 变量声明面板 + 全屏编辑器 */
  .flow-dialog-lg{width:min(760px,100%)}
  .flow-var-summary{display:flex;flex-wrap:wrap;gap:6px;margin:6px 0 10px}
  .flow-var-chip{display:inline-flex;align-items:baseline;gap:5px;border:1px solid var(--brand-line);background:var(--brand-soft);border-radius:20px;padding:3px 11px;font-size:12px}
  .flow-var-chip b{font-weight:600;color:var(--ink)} .flow-var-chip em{font-style:normal;font-size:10px;color:var(--brand-d);text-transform:uppercase;letter-spacing:.3px}
  .flow-var-body{max-height:min(64vh,540px)}
  .flow-var-list{display:flex;flex-direction:column;gap:9px;margin-bottom:12px}
  .flow-var-row{border:1px solid var(--line);border-radius:10px;padding:9px 11px;background:var(--tile);margin-left:calc(var(--d,0)*18px);box-shadow:0 1px 2px var(--shadow)}
  .flow-var-row:hover{border-color:var(--brand-line)}
  .flow-var-main{display:flex;align-items:center;gap:7px;flex-wrap:wrap}
  .flow-var-name{flex:1 1 130px;min-width:110px;font-family:ui-monospace,Menlo,monospace;font-size:13px;border:1px solid var(--line);border-radius:7px;padding:6px 9px;background:var(--tile);color:var(--ink)}
  .flow-var-type{flex:0 0 88px;border:1px solid var(--line);border-radius:7px;padding:6px 6px;font-size:12px;background:var(--tile);color:var(--ink)}
  .flow-var-label{flex:1 1 130px;min-width:100px;border:1px solid var(--line);border-radius:7px;padding:6px 9px;font-size:13px;background:var(--tile);color:var(--ink)}
  .flow-var-req{flex:0 0 auto;display:inline-flex;align-items:center;gap:3px;font-size:11px;color:var(--muted);cursor:pointer;white-space:nowrap}
  .flow-var-req input{margin:0}
  .flow-var-desc{width:100%;margin-top:6px;border:1px dashed var(--line);border-radius:7px;padding:5px 9px;font-size:12px;color:var(--muted);background:var(--tile)}
  .flow-var-sub{margin-top:8px;padding:8px 10px;border-left:2.5px solid var(--brand-line);background:var(--brand-soft);border-radius:0 8px 8px 0}
  .flow-var-subhead{display:flex;align-items:center;gap:6px;font-size:12px;font-weight:600;color:var(--brand-d);margin-bottom:6px}
  .flow-var-subhead .flow-btn.slim{margin-left:auto}
  .flow-var-foot{display:flex;align-items:center;gap:12px;padding:11px 14px;border-top:1px solid var(--line-soft);background:var(--header)}
  .flow-var-policy{display:flex;align-items:center;gap:6px;font-size:12px;color:var(--muted)}
  .flow-var-policy select{border:1px solid var(--line);border-radius:7px;padding:5px 7px;font-size:12px;background:var(--tile);color:var(--ink)}
  .flow-var-foot-act{margin-left:auto;display:flex;gap:8px}
  .flow-vempty{display:flex;flex-direction:column;align-items:center;gap:4px;padding:26px 12px;text-align:center;color:var(--muted)}
  .flow-vempty b{font-size:13px;color:var(--ink)} .flow-vempty span{font-size:12px}
  .flow-btn.slim{padding:3px 8px;font-size:11px}
  .flow-vlist-head{display:flex;align-items:baseline;justify-content:space-between;margin-bottom:8px} .flow-vlist-head b{font-size:13px} .flow-vlist-head span{font-size:11px;color:var(--muted)}
  .flow-vlist{display:flex;flex-direction:column;gap:7px;max-height:260px;overflow:auto}
  .flow-vrow{display:flex;align-items:center;gap:10px;border:1px solid var(--line);border-radius:8px;padding:8px 10px;background:var(--header)}
  .flow-vrow.cur{border-color:color-mix(in srgb,var(--ok) 40%,var(--line));background:var(--ok-soft)}
  .flow-vrow-main{flex:1;min-width:0} .flow-vrow-main b{font-size:13px;font-family:ui-monospace,Menlo,monospace} .flow-vtag{margin-left:6px;font-size:10px;font-weight:700;color:var(--ok);background:var(--ok-soft);border:1px solid color-mix(in srgb,var(--ok) 40%,var(--line));border-radius:5px;padding:1px 6px}
  .flow-vtag.off{color:var(--warn);background:var(--warn-soft);border-color:color-mix(in srgb,var(--warn) 40%,var(--line))}
  /* 子流程模式切换（按组织路由 / 固定） */
  .flow-mode{display:grid;grid-template-columns:1fr 1fr;gap:8px;margin-bottom:12px}
  .flow-mode-opt{text-align:left;border:1.5px solid var(--line);border-radius:9px;background:var(--tile);padding:8px 10px;cursor:pointer;transition:all .13s}
  .flow-mode-opt b{display:block;font-size:12.5px;color:var(--ink)} .flow-mode-opt small{display:block;font-size:10.5px;color:var(--muted);margin-top:2px}
  .flow-mode-opt.on{border-color:var(--brand);background:var(--brand-soft)} .flow-mode-opt.on b{color:var(--brand-d)}
  .flow-vrow-main em{display:block;font-style:normal;font-size:11.5px;color:var(--ink);margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-vrow-main small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .flow-vrow-act{display:flex;align-items:center;gap:6px;flex:0 0 auto}
  .flow-vempty{border:1px dashed var(--line);border-radius:8px;padding:20px;text-align:center;color:var(--muted)} .flow-vempty ui5-icon{color:var(--brand)} .flow-vempty b{display:block;margin-top:6px;color:var(--ink)}
  .flow-vnew{margin-top:14px;border-top:1px solid var(--line-soft);padding-top:12px}
  .flow-vnew-title{display:flex;align-items:center;gap:6px;font-weight:700;font-size:12.5px;color:var(--brand-d);margin-bottom:8px}
  .flow-prop-head{height:var(--flow-bar-h);flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 14px;border-bottom:1px solid var(--line-soft);background:var(--header)} .flow-prop-head b{font-size:14px} .flow-prop-head small{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .flow-prop-body{padding:12px 14px;overflow:auto}
  .flow-field{margin-bottom:12px} .flow-field label{display:block;font-size:11px;font-weight:700;color:var(--muted);text-transform:uppercase;margin-bottom:4px;letter-spacing:.03em}
  .flow-field input{width:100%;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:7px;padding:7px 10px;background:var(--tile);color:var(--ink);transition:border-color .13s}
  .flow-field input:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  .flow-hint{font-size:11px;color:var(--muted);margin-top:3px} .flow-sec{font-size:11px;font-weight:800;color:var(--brand-d);text-transform:uppercase;margin:14px 0 8px;padding-bottom:5px;border-bottom:1px solid var(--line-soft);letter-spacing:.04em}
  .flow-empty{display:flex;flex-direction:column;align-items:center;gap:8px;color:var(--muted);font-size:12.5px;padding:36px 16px;text-align:center}
  .flow-toast{position:absolute;left:50%;bottom:18px;transform:translateX(-50%);background:#0d1117;color: #fff;padding:9px 16px;border-radius:10px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s;z-index:20;max-width:min(88%,520px);white-space:normal;line-height:1.5;text-align:center;box-shadow:0 8px 24px rgba(0,0,0,.36);border:1px solid rgba(255,255,255,.08)}
  .flow-toast.show{opacity:1}
  /* 分支条件可视化构造器（P2-c） */
  .flow-cond{margin-bottom:12px}
  .flow-cond-mode{display:inline-flex;gap:0;margin-bottom:10px;border:1px solid var(--line);border-radius:8px;overflow:hidden}
  .flow-cond-tab{font:inherit;font-size:11.5px;font-weight:700;padding:5px 14px;background:var(--tile);color:var(--muted);border:none;cursor:pointer;transition:all .13s}
  .flow-cond-tab.on{background:var(--brand);color:var(--brand-ink)}
  .flow-cond-rows{display:flex;flex-direction:column;gap:7px}
  .flow-cond-row{display:flex;align-items:center;gap:5px;flex-wrap:wrap;background:var(--surface);border:1px solid var(--line-soft);border-radius:9px;padding:7px 8px}
  .flow-cond-row select,.flow-cond-row input[type=text],.flow-cond-row input:not([type]){font:inherit;font-size:12px;height:28px;border:1px solid var(--line);border-radius:6px;padding:0 6px;background:var(--tile);color:var(--ink)}
  .flow-cond-fn{max-width:96px} .flow-cond-op{max-width:104px} .flow-cond-conn{max-width:96px}
  .flow-cond-var{flex:1;min-width:110px} .flow-cond-val{width:88px}
  .flow-cond-val-wrap{display:flex;align-items:center;gap:4px}
  .flow-cond-isvar,.flow-cond-paren{display:inline-flex;align-items:center;gap:2px;font-size:11px;color:var(--muted);font-weight:700;cursor:pointer;white-space:nowrap}
  .flow-cond-isvar input,.flow-cond-paren input{margin:0}
  .flow-cond-empty{font-size:12px;color:var(--muted);padding:10px;background:var(--surface);border-radius:8px;border:1px dashed var(--line)}
  .flow-cond-preview{display:flex;align-items:center;gap:8px;margin-top:9px;font-size:11px}
  .flow-cond-preview span{font-weight:800;color:var(--muted);text-transform:uppercase}
  .flow-cond-preview code{flex:1;font-family:var(--mono,ui-monospace,Menlo,monospace);font-size:12px;background:var(--code-bg);color:var(--code-fg);padding:5px 9px;border-radius:6px;overflow-x:auto;white-space:nowrap;border:1px solid rgba(255,255,255,.06)}
  .flow-cond-test{display:flex;align-items:center;gap:6px;margin-top:8px;flex-wrap:wrap}
  .flow-cond-test input{flex:1;min-width:150px;font:inherit;font-size:12px;height:28px;border:1px solid var(--line);border-radius:6px;padding:0 8px;background:var(--tile);color:var(--ink)}
  .flow-cond-testres{font-size:11.5px;font-weight:700;padding:3px 9px;border-radius:20px}
  .flow-cond-testres.ok{background:var(--ok-soft);color:var(--ok)}
  .flow-cond-testres.off{background:var(--line-soft);color:var(--muted)}
  .flow-cond-testres.err{background:var(--red-soft);color:var(--red)}
  .flow-icon-btn.danger{color:var(--red)}
  /* UserTask 属性分页签（P3） */
  .flow-uttabs{display:flex;gap:2px;margin:0 0 12px;border-bottom:1px solid var(--line);flex-wrap:wrap}
  .flow-uttab{font:inherit;font-size:12px;font-weight:700;padding:7px 12px;background:transparent;color:var(--muted);border:none;border-bottom:2px solid transparent;cursor:pointer;margin-bottom:-1px;transition:color .13s}
  .flow-uttab:hover{color:var(--ink)}
  .flow-uttab.on{color:var(--brand-d);border-bottom-color:var(--brand)}
  .flow-utbody{}
  .flow-akinds{display:flex;flex-wrap:wrap;gap:5px;margin-top:2px}
  .flow-akind{font:inherit;font-size:12px;font-weight:600;padding:5px 11px;border:1px solid var(--line);border-radius:20px;background:var(--tile);color:var(--muted);cursor:pointer;transition:all .13s}
  .flow-akind:hover{border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)}
  .flow-akind.on{background:var(--brand);color:var(--brand-ink);border-color:var(--brand);box-shadow:0 2px 8px var(--glow)}
  .flow-utbody select[data-idn-pick]{width:100%;height:30px;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:7px;padding:0 8px;background:var(--tile);color:var(--ink)}
  .flow-utbody select[data-idn-pick]:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  .flow-utbody input[data-idn-text],.flow-utbody input[data-akind-val],.flow-utbody input[data-mi-f]{width:100%;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:7px;padding:7px 10px;background:var(--tile);color:var(--ink)}
  .flow-utbody input:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  /* 子流程变量映射（P5） */
  .flow-vm-head{display:flex;justify-content:space-between;align-items:center;font-size:12px;font-weight:700;color:var(--muted);margin-bottom:6px}
  .flow-vm-row{display:flex;align-items:center;gap:5px;margin-bottom:5px}
  .flow-vm-src,.flow-vm-tgt{flex:1;min-width:0;font:inherit;font-size:12px;height:28px;border:1px solid var(--line);border-radius:6px;padding:0 7px;background:var(--tile);color:var(--ink)}
  .flow-vm-src:focus,.flow-vm-tgt:focus{outline:none;border-color:var(--brand-line);box-shadow:0 0 0 3px var(--brand-soft)}
  .flow-vm-arr{color:var(--muted);font-weight:700;flex:none}
  .flow-vm-empty{font-size:11.5px;color:var(--muted);padding:6px 8px;background:var(--surface);border:1px dashed var(--line);border-radius:7px}
  /* explorer 「显示子流程」开关 */
  .flow-subfl-toggle{display:flex;align-items:center;gap:6px;padding:6px 10px 2px;font-size:12px;color:var(--muted);cursor:pointer;user-select:none}
  .flow-subfl-toggle input{margin:0}
  /* ═══ 子流程钻入式（无浮层）：工具栏面包屑 + 返回 / explorer 变体列表 / property 页签 ═══ */
  /* content 工具栏：常驻「← 返回主流程」+ 面包屑（主流程 › 子流程） */
  .flow-btn.back{background:var(--brand);color:var(--brand-ink);border-color:var(--brand);font-weight:700;box-shadow:0 2px 8px var(--glow)}
  .flow-btn.back:hover{filter:brightness(1.06)}
  .flow-crumb{display:inline-flex;align-items:center;gap:5px;min-width:0;max-width:46%;padding:3px 10px;background:var(--brand-soft);border:1px solid var(--brand-line);border-radius:16px;font-size:12.5px}
  .flow-crumb-ic{color:var(--muted)} .flow-crumb-ic.sub{color:var(--brand)}
  .flow-crumb-main{color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:40%}
  .flow-crumb-sep{color:var(--brand);font-weight:700}
  .flow-crumb-sub{color:var(--ink);font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0}
  /* explorer 子流程模式：组织变体列表 */
  .flow-subv-scroll{flex:1;overflow:auto;min-height:0}
  .flow-subv-hd{display:flex;align-items:center;gap:6px;padding:9px 11px 4px;font-size:12px;font-weight:700;color:var(--brand-d)}  .flow-subv-list{padding:2px 7px}
  .flow-subv{display:flex;align-items:center;gap:8px;width:100%;text-align:left;border:1px solid transparent;background:none;border-radius:8px;padding:8px 9px;cursor:pointer;margin-bottom:3px;transition:all .13s}
  .flow-subv:hover{background:var(--hover)}
  .flow-subv.on{background:var(--brand-soft);border-color:var(--brand-line)}
  .flow-subv-mark{flex:none;color:var(--brand);font-size:12px}
  .flow-subv-main{min-width:0}
  .flow-subv-main b{display:block;font-size:12.5px;color:var(--ink)}
  .flow-subv-main small{display:block;font-size:10.5px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .flow-subv-new{padding:8px 7px}
  .flow-subv-new select{width:100%;border:1px solid var(--line);border-radius:7px;padding:6px 7px;font-size:12px;background:var(--tile);color:var(--ink)}
  /* property 区页签：节点属性 | 数据模型 */
  .flow-prop-wrap{height:100%;display:flex;flex-direction:column;min-height:0}
  .flow-ptabs{display:flex;gap:2px;padding:6px 8px 0;border-bottom:1px solid var(--line);background:var(--header);flex:none}
  .flow-ptab{font:inherit;font-size:12.5px;font-weight:700;display:inline-flex;align-items:center;gap:5px;padding:8px 13px;background:transparent;color:var(--muted);border:none;border-bottom:2px solid transparent;cursor:pointer;margin-bottom:-1px;transition:color .13s}
  .flow-ptab:hover{color:var(--ink)}
  .flow-ptab.on{color:var(--brand-d);border-bottom-color:var(--brand)}
  .flow-prop-wrap>.flow-prop{flex:1;min-height:0;overflow:auto}
  /* ═══ 暗色画布（仅 data-theme=dark 生效；light 模式保持 bpmn 原生零改） ═══ */
  /* 渲染器把图形默认描边/填充烘焙成内联属性（不吃 CSS 变量），故用 !important 覆盖：
     亮描边 + 深纸面 + 亮标签/箭头；diagram-js 自身 chrome（palette/context-pad/popup/选中框）走活变量重定义。 */
  .flow[data-theme=dark] .djs-container{color:var(--ink)}
  .flow[data-theme=dark] .djs-visual > :is(rect,circle,ellipse,polygon,path){stroke:var(--ink) !important}
  .flow[data-theme=dark] .djs-shape .djs-visual > :is(rect,circle,ellipse,polygon,path):first-child{fill:var(--tile) !important}
  .flow[data-theme=dark] .djs-connection .djs-visual > path{fill:none !important}
  .flow[data-theme=dark] .djs-visual text,.flow[data-theme=dark] .djs-label,.flow[data-theme=dark] text.djs-label{fill:var(--ink) !important;stroke:none !important}
  .flow[data-theme=dark] marker :is(path,circle,polygon,polyline){fill:var(--ink) !important;stroke:var(--ink) !important}
  .flow[data-theme=dark] .djs-parent{
    --color-white:var(--tile);
    --color-black:var(--ink);
    --color-grey-225-10-15:var(--ink);
    --color-grey-225-10-35:var(--muted);
    --color-grey-225-10-55:var(--muted);
    --color-grey-225-10-75:var(--line);
    --color-grey-225-10-80:var(--line);
    --color-grey-225-10-85:color-mix(in srgb,var(--line) 65%,var(--surface));
    --color-grey-225-10-90:color-mix(in srgb,var(--surface) 55%,var(--tile));
    --color-grey-225-10-95:color-mix(in srgb,var(--surface) 78%,var(--tile));
    --color-grey-225-10-97:var(--surface);
    --canvas-fill-color:var(--tile);
    --context-pad-entry-background-color:var(--tile);
    --popup-background-color:var(--tile);
    --popup-shadow-color:color-mix(in srgb,#000 55%,transparent);
  }
  /* ── 模拟（simulate）画布高亮：复刻 ops-console，改 .djs-visual 首子元素描边（marker 由 canvas.addMarker 加类）。task 覆盖 hit ── */
  .flow-sim-flow .djs-visual > :nth-child(1){stroke:var(--brand)!important;stroke-width:3px!important}
  .flow-sim-hit .djs-visual > :nth-child(1){stroke:var(--brand)!important;stroke-width:3px!important;fill:color-mix(in srgb,var(--brand) 12%,var(--tile))!important}
  .flow-sim-task .djs-visual > :nth-child(1){stroke:var(--warn)!important;stroke-width:3.5px!important;fill:color-mix(in srgb,var(--warn) 14%,var(--tile))!important}
  /* ── 模拟面板 ── */
  .flow-sim-facts,.flow-sim-ctx{display:flex;flex-direction:column;gap:8px;margin-top:10px}
  .flow-sim-ctx{margin-top:8px;padding-top:8px;border-top:1px dashed var(--line-soft)}
  .flow-sim-f{display:flex;flex-direction:column;gap:3px;font-size:11.5px;color:var(--ink)}
  .flow-sim-f>span{color:var(--muted);display:flex;align-items:center;gap:5px}
  .flow-sim-f code{font-family:ui-monospace,Menlo,monospace;font-size:10.5px;color:var(--brand-d)}
  .flow-sim-f em{font-style:normal;font-size:9.5px;color:var(--muted);border:1px solid var(--line);border-radius:4px;padding:0 4px}
  .flow-sim-f input,.flow-sim-f select{height:28px;font:inherit;font-size:12px;border:1px solid var(--line);border-radius:7px;padding:0 8px;background:var(--tile);color:var(--ink)}
  .flow-sim-f input:focus,.flow-sim-f select:focus{outline:none;border-color:var(--brand)}
  .flow-sim-adv{margin-top:8px}
  .flow-sim-adv summary{font-size:11px;color:var(--muted);cursor:pointer}
  .flow-sim-adv textarea{width:100%;margin-top:6px;font-family:ui-monospace,Menlo,monospace;font-size:11px;border:1px solid var(--line);border-radius:7px;padding:6px 8px;background:var(--tile);color:var(--ink);resize:vertical;box-sizing:border-box}
  .flow-sim-res{margin-top:12px;border-top:1px solid var(--line-soft);padding-top:10px}
  .flow-sim-sum{display:flex;align-items:center;gap:8px;font-size:11.5px;color:var(--muted);margin-bottom:6px}
  .flow-sim-pill{font-size:11px;font-weight:700;padding:2px 8px;border-radius:9px}
  .flow-sim-pill.ok{color:var(--ok);background:color-mix(in srgb,var(--ok) 14%,var(--tile));border:1px solid color-mix(in srgb,var(--ok) 34%,transparent)}
  .flow-sim-pill.no{color:var(--warn);background:color-mix(in srgb,var(--warn) 14%,var(--tile));border:1px solid color-mix(in srgb,var(--warn) 34%,transparent)}
  .flow-sim-sec{font-size:10.5px;font-weight:800;color:var(--brand-d);text-transform:uppercase;letter-spacing:.04em;margin:10px 0 5px}
  .flow-sim-row{display:flex;align-items:center;gap:7px;font-size:11.5px;color:var(--ink);padding:4px 8px;border:1px solid var(--line-soft);border-radius:7px;margin-bottom:4px;background:var(--tile)}
  .flow-sim-row ui5-icon{color:var(--brand);width:14px;height:14px;flex:0 0 auto}
  .flow-sim-row b{font-family:ui-monospace,Menlo,monospace;font-size:10.5px;flex:0 0 auto}
  .flow-sim-row span{color:var(--muted);min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .flow-sim-row em{font-style:normal;color:var(--warn)}
  .flow-sim-row.bad{border-color:color-mix(in srgb,var(--warn) 40%,var(--line))} .flow-sim-row.bad ui5-icon{color:var(--warn)}
  .flow-sim-warn{display:flex;flex-direction:column;gap:4px;margin-bottom:8px}
  .flow-sim-warn>div{display:flex;align-items:center;gap:6px;font-size:11px;color:var(--warn);background:color-mix(in srgb,var(--warn) 10%,var(--tile));border:1px solid color-mix(in srgb,var(--warn) 28%,transparent);border-radius:7px;padding:5px 8px}
  .flow-sim-warn ui5-icon{width:13px;height:13px;flex:0 0 auto}
  /* ── 版本 diff 画布高亮：新增=绿(--ok) / 修改=琥珀(--warn)。删除项不在画布 ── */
  .flow-diff-add .djs-visual > :nth-child(1){stroke:var(--ok)!important;stroke-width:3px!important;fill:color-mix(in srgb,var(--ok) 12%,var(--tile))!important}
  .flow-diff-chg .djs-visual > :nth-child(1){stroke:var(--warn)!important;stroke-width:3px!important;fill:color-mix(in srgb,var(--warn) 12%,var(--tile))!important}
  /* ── 差异面板 ── */
  .flow-diff-pick{display:flex;align-items:flex-end;gap:8px;margin-top:10px}
  .flow-diff-pick .flow-sim-f{flex:1;min-width:0}
  .flow-diff-arrow{color:var(--muted);width:16px;height:16px;margin-bottom:6px;flex:0 0 auto}
  .flow-diff-btns{display:flex;gap:8px;margin-top:10px} .flow-diff-btns .flow-btn{flex:1;justify-content:center;display:flex;align-items:center;gap:6px}
  .flow-diff-row{display:flex;align-items:center;gap:7px;font-size:11.5px;color:var(--ink);padding:5px 8px;border:1px solid var(--line-soft);border-left-width:3px;border-radius:7px;margin-bottom:4px;background:var(--tile);cursor:pointer;transition:background .12s}
  .flow-diff-row:hover{background:var(--brand-soft)}
  .flow-diff-row ui5-icon{width:14px;height:14px;flex:0 0 auto;color:var(--muted)}
  .flow-diff-row b{font-weight:600;flex:0 0 auto} .flow-diff-row code{font-family:ui-monospace,Menlo,monospace;font-size:10px;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .flow-diff-row.add{border-left-color:var(--ok)} .flow-diff-row.add ui5-icon{color:var(--ok)}
  .flow-diff-row.chg{border-left-color:var(--warn)} .flow-diff-row.chg ui5-icon{color:var(--warn)}
  .flow-diff-row.del{border-left-color:var(--muted);opacity:.72} .flow-diff-row.del b{text-decoration:line-through}
  .flow-diff-fields{margin-left:auto;font-size:10px;color:var(--warn);background:color-mix(in srgb,var(--warn) 12%,var(--tile));border-radius:6px;padding:1px 6px;flex:0 0 auto}
  /* ── 协同 M1：在场条 + 远端选中高亮 + 草稿更新通知 ── */
  .flow-collab-bar{position:absolute;top:8px;right:12px;z-index:6;display:flex;align-items:center;gap:5px;pointer-events:none}
  .flow-collab-bar:empty{display:none}
  .flow-collab-label{font-size:10.5px;color:var(--muted);background:var(--tile);border:1px solid var(--line-soft);border-radius:9px;padding:1px 8px;box-shadow:0 1px 3px var(--glow)}
  .flow-collab-avatar{width:24px;height:24px;border-radius:50%;display:inline-flex;align-items:center;justify-content:center;font-size:10.5px;font-weight:700;color:#fff;background:var(--c,#888);border:2px solid var(--tile);box-shadow:0 1px 3px rgba(0,0,0,.2);margin-left:-8px}
  .flow-collab-avatar.me{outline:2px solid color-mix(in srgb,var(--c) 50%,transparent)}
  /* 远端选中：虚线描边（区别本端实线蓝选中）。改 .djs-visual 首子元素，复用 sim/diff marker 技法。 */
  .flow-collab-sel .djs-visual > :nth-child(1){stroke:var(--violet,#7c5cff)!important;stroke-width:2.5px!important;stroke-dasharray:5 3!important}
  .flow-collab-notice{position:absolute;top:8px;left:50%;transform:translateX(-50%);z-index:7;display:none;align-items:center;gap:8px;background:var(--tile);border:1px solid color-mix(in srgb,var(--brand) 34%,var(--line));border-radius:9px;padding:6px 10px;box-shadow:0 4px 16px var(--glow);font-size:12px;color:var(--ink);max-width:80%}
  .flow-collab-notice.show{display:flex}
  .flow-collab-notice ui5-icon{color:var(--brand);width:15px;height:15px;flex:0 0 auto}
  `
}

// 门户壳 export default（CFG 默认值=今天：同源 fetch + /portal/vendor/bpmn-js 资产）；
// S5 组件壳 import { configure, mount } 覆盖 apiBase/authHeaders/bpmnBase 后自挂 shadowRoot。
// __state 导出：调试/自动化测试可读模块级 state（如取 modeler 驱动结构编辑）。非门户契约，勿依赖。
export { configure, mount, state as __state }
export default {
  defaultView: 'content',
  views: {
    async explorer (ctx) { return mount(ctx, 'explorer') },
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
