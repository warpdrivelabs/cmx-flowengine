/**
 * 待办中心 —— native_pages 四区（对标流程设计工作台骨架）。F3。
 *
 * explorer：待办分类（我的待办 / 待我认领 / 我发起的 / 抄送我的 / 我已办）。
 * content ：待办列表卡片；「办理」→ openWorkNode 打开任务表单页（portal.flow.task-form）。
 * property：选中待办的流程轨迹 + 审批意见历史。
 *
 * 数据源：F2 的 GET /api/flow/tasks/my（我的待办 / 待认领）、GET /api/flow/instances/{id}
 *        （轨迹）、GET /api/flow/instances/{id}/comments（意见历史）。办理/认领/转签走现成端点。
 *
 * 打开表单：复用报表设计器验证过的 openWorkNode 5 级兜底链（openTab→openWorkspaceNode→
 *          POST /api/workspace-nodes→postMessage→CustomEvent），把 {formKey,taskId,bizRef,mode}
 *          作为 workspace.params/props 传给 task-form 页。
 */

// —— S4 抽核：配置接缝 ——
// 门户壳（export default）用 CFG 默认值 = 今天行为，逐字节零回归；可嵌组件壳 / headless 壳（S5）
// 调 configure({...}) 覆盖：apiBase 指向远程 flow-server、authHeaders 注入 Bearer、getUser 取宿主
// 登录态、onOpenTask 派发 CustomEvent 交宿主决定怎么开。核心逻辑（loadTodos/openTaskForm/…）全部
// 只经 CFG 触达外部，不再直接摸 localStorage / 同源 fetch / 门户 Tab 链。
const CFG = {
  apiBase: '',                                // 前缀所有 /api/* 请求；门户空串 = 同源相对路径（今天）
  fetchInit: { credentials: 'same-origin' },  // 门户带同源 cookie；组件壳可换 { credentials:'omit' }
  authHeaders: () => ({}),                    // 附加请求头；组件壳返回 { Authorization:'Bearer …' }
  getUser: () => {                            // 当前用户；门户从 localStorage 兜底，组件壳由宿主注入
    try {
      return window.localStorage.getItem('cmx_user_id') ||
             window.localStorage.getItem('cmx_username') || 'admin'
    } catch { return 'admin' }
  },
  onOpenTask: null,                           // 打开任务工作台；null = 门户默认 openWorkNode 链，
}                                             //   组件壳设为回调（派发事件让宿主开 Tab/路由）
function configure (o) { Object.assign(CFG, o || {}); return CFG }

// 当前用户（走 CFG，门户默认从 localStorage 兜底）。
function currentUser () { return CFG.getUser() }

// formKey → 表单页坐标。F4：优先查后端注册表 /api/flow/forms/{key}（接新表单只需配一行，
// 不改前端）；查不到再退回内置 FORM_MAP 兜底（离线/未种子时仍可用）。缓存解析结果。
const FORM_MAP = {
  'pay.review': { kind: 'native', nativePage: 'portal.flow.task-form', domain: 'fi', application: 'cmxfico', module: 'gl', bizTable: 'cf_pay_request' },
  'pay.review.html': { kind: 'html', htmlPage: 'flow-pay-review-form', domain: 'fi', application: 'cmxfico', module: 'gl', bizTable: 'cf_pay_request' },
}
const formCache = {}
async function resolveForm (formKey) {
  if (!formKey) return {}
  if (formCache[formKey]) return formCache[formKey]
  try {
    const b = await apiJson('/api/flow/forms/' + enc(formKey))
    if (b && b.formKey) {
      const f = { kind: b.kind || 'native', nativePage: b.nativePage || '', nativeView: b.nativeView || b.view || 'content', htmlPage: b.htmlPage || '', workspaceNode: b.workspaceNode || '', bizTable: b.bizTable || '', domain: b.domain || '', application: b.application || '', module: b.module || '', file: b.file || '', apiPath: b.apiPath || '', title: b.title || '', console: b.console || 'platform' }
      formCache[formKey] = f
      return f
    }
  } catch { /* 注册表不可用则兜底 */ }
  const fb = FORM_MAP[formKey] || {}
  formCache[formKey] = fb
  return fb
}

const CATEGORIES = [
  { key: 'todo', label: '我的待办', icon: 'task', hint: '直派给我、待我办理' },
  { key: 'claimable', label: '待我认领', icon: 'inbox', hint: '候选人含我，认领后办理' },
  { key: 'initiate', label: '发起流程', icon: 'add-activity', hint: '新建单据、发起审批' },
  { key: 'initiated', label: '我发起的', icon: 'journey-arrive', hint: '我发起的流程实例' },
  { key: 'cc', label: '抄送我的', icon: 'email', hint: '知会给我，只读' },
  { key: 'done', label: '我已办', icon: 'accept', hint: '我办结过的任务' },
]

const state = {
  category: 'todo',
  todos: [],
  startables: [],     // 可发起流程列表（发起态）
  loading: false,
  selected: null,     // 选中的待办（property 展示其轨迹）
  trail: null,        // 选中实例的详情（令牌 + 任务链）
  comments: [],       // 选中实例的意见历史
  hosts: new Set(),
  // 查找/过滤/分页（每次切分类重置）
  filter: { keyword: '', definitionKey: '', nodeBpmnId: '', state: '' },
  page: 1,
  pageSize: 20,
  total: 0,
  defOptions: [],     // 过滤下拉：定义列表 [{key,name,nodes:[{id,name}]}]
}

const esc = (s) => String(s ?? '')
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  .replace(/"/g, '&quot;').replace(/'/g, '&#39;')
const enc = encodeURIComponent
const slug = (s) => String(s || '').replace(/[^A-Za-z0-9_-]+/g, '_') || 'x'

async function apiJson (url, options = {}) {
  // S4：apiBase 前缀（门户空串=同源）+ CFG.authHeaders/fetchInit（门户 same-origin cookie，组件壳 Bearer）。
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

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

function toast (msg) {
  for (const host of Array.from(state.hosts)) {
    const t = hostRoot(host)?.querySelector?.('.todo-toast')
    if (t) { t.textContent = msg; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2600) }
  }
}

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__todoView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'content' || view === 'explorer') loadTodos(); if (view === 'content') loadFilterOptions() })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}

function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__todoView !== view) continue
    const root = hostRoot(host)
    if (!root) continue
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
}

function viewHtml (view) {
  if (view === 'explorer') return explorerHtml()
  if (view === 'property') return propertyHtml()
  return contentHtml()
}

// ————————————————————— explorer 区 —————————————————————

function explorerHtml () {
  const items = CATEGORIES.map((c) => `<button class="todo-cat ${state.category === c.key ? 'active' : ''}" data-cat="${c.key}">
    <span class="todo-cat-ic"><ui5-icon name="${c.icon}"></ui5-icon></span>
    <span class="todo-cat-main"><b>${esc(c.label)}</b><small>${esc(c.hint)}</small></span>
  </button>`).join('')
  return `<section class="todo todo-explorer">
    <div class="todo-head"><div><b>待办中心</b><span>cmx-flow / my tasks</span></div>
      <button class="todo-icon-btn" data-act="refresh" title="刷新"><ui5-icon name="refresh"></ui5-icon></button></div>
    <div class="todo-cat-list">${items}</div>
  </section>`
}

// ————————————————————— content 区 —————————————————————

// 过滤控件（内联进 content 顶部工具条，与刷新按钮同排；不单起一条 bar）。
// 关键字 + 按流程 + 按环节(依选中流程) + 按状态。按分类裁剪可用维度。
function filterControlsHtml () {
  const cat = state.category
  const f = state.filter
  const showState = cat === 'initiated'          // 状态仅实例类
  const showNode = cat === 'todo' || cat === 'claimable' || cat === 'done'  // 环节仅任务类
  const defOpts = ['<option value="">全部流程</option>'].concat(
    state.defOptions.map((d) => `<option value="${esc(d.key)}" ${d.key === f.definitionKey ? 'selected' : ''}>${esc(d.name || d.key)}</option>`)
  ).join('')
  const curDef = state.defOptions.find((d) => d.key === f.definitionKey)
  const nodeOpts = ['<option value="">全部环节</option>'].concat(
    (curDef?.nodes || []).map((n) => `<option value="${esc(n.id)}" ${n.id === f.nodeBpmnId ? 'selected' : ''}>${esc(n.name || n.id)}</option>`)
  ).join('')
  const stateOpts = [['', '全部状态'], ['ACTIVE', '进行中'], ['COMPLETED', '已完成'], ['TERMINATED', '已终止']]
    .map(([v, l]) => `<option value="${v}" ${v === f.state ? 'selected' : ''}>${l}</option>`).join('')
  return `<div class="todo-search"><ui5-icon name="search"></ui5-icon>
      <input data-f-keyword value="${esc(f.keyword)}" placeholder="搜索单号 / 流程名..."></div>
    <select data-f-def title="按流程">${defOpts}</select>
    ${showNode ? `<select data-f-node title="按环节" ${curDef ? '' : 'disabled'}>${nodeOpts}</select>` : ''}
    ${showState ? `<select data-f-state title="按状态">${stateOpts}</select>` : ''}
    <button class="todo-btn" data-f-reset title="重置过滤条件">重置</button>`
}

// 分页控件：首/上/下/末页 + 页码信息。
function pagerHtml () {
  const total = state.total || 0
  const size = state.pageSize
  const pages = Math.max(1, Math.ceil(total / size))
  const p = Math.min(state.page, pages)
  const from = total === 0 ? 0 : (p - 1) * size + 1
  const to = Math.min(p * size, total)
  return `<div class="todo-pager">
    <span class="todo-pager-info">共 ${total} 条 · 第 ${from}-${to} 条 · ${p}/${pages} 页</span>
    <span class="todo-sp"></span>
    <select data-pg-size title="每页">
      ${[10, 20, 50, 100].map((s) => `<option value="${s}" ${s === size ? 'selected' : ''}>${s}/页</option>`).join('')}
    </select>
    <button class="todo-pg" data-pg="first" ${p <= 1 ? 'disabled' : ''} title="首页">⏮</button>
    <button class="todo-pg" data-pg="prev" ${p <= 1 ? 'disabled' : ''} title="上页">‹</button>
    <button class="todo-pg" data-pg="next" ${p >= pages ? 'disabled' : ''} title="下页">›</button>
    <button class="todo-pg" data-pg="last" ${p >= pages ? 'disabled' : ''} title="末页">⏭</button>
  </div>`
}

function contentHtml () {
  const cat = CATEGORIES.find((c) => c.key === state.category)
  if (state.category === 'initiate') {
    const body = state.loading
      ? `<div class="todo-empty"><ui5-icon name="busy"></ui5-icon><span>加载中...</span></div>`
      : (state.startables.length
          ? state.startables.map(startableCard).join('')
          : `<div class="todo-empty"><ui5-icon name="tray"></ui5-icon><span>暂无可发起流程</span></div>`)
    return `<section class="todo todo-content">
      <div class="todo-bar"><b>发起流程</b><span class="todo-count">${state.startables.length}</span>
        <span class="todo-sp"></span>
        ${filterControlsHtml()}
        <button class="todo-btn" data-act="refresh"><ui5-icon name="refresh"></ui5-icon> 刷新</button></div>
      <div class="todo-list">${body}</div>
      <div class="todo-toast"></div>
    </section>`
  }
  const body = state.loading
    ? `<div class="todo-empty"><ui5-icon name="busy"></ui5-icon><span>加载中...</span></div>`
    : (state.todos.length
        ? state.todos.map(todoCard).join('')
        : `<div class="todo-empty"><ui5-icon name="tray"></ui5-icon><span>${esc(cat?.label || '')}：暂无</span></div>`)
  return `<section class="todo todo-content">
    <div class="todo-bar"><b>${esc(cat?.label || '待办')}</b><span class="todo-count">${state.total}</span>
      <span class="todo-sp"></span>
      ${filterControlsHtml()}
      <button class="todo-btn" data-act="refresh"><ui5-icon name="refresh"></ui5-icon> 刷新</button></div>
    <div class="todo-list">${body}</div>
    ${pagerHtml()}
    <div class="todo-toast"></div>
  </section>`
}

function startableCard (d) {
  return `<article class="todo-card startable" data-startable="${esc(d.key)}">
    <div class="todo-card-ava"><ui5-icon name="add-activity"></ui5-icon></div>
    <div class="todo-card-main">
      <div class="todo-card-title"><b>${esc(d.name || d.key)}</b><span>${esc(d.key)}</span></div>
      <div class="todo-card-meta">
        ${d.startFormKey ? `<span><ui5-icon name="form"></ui5-icon>${esc(d.startFormKey)}</span>` : '<span class="warn">无起点表单</span>'}
      </div>
    </div>
    <div class="todo-card-act"><button class="todo-btn primary" data-start="${esc(d.key)}">发起</button></div>
  </article>`
}

function todoCard (t) {
  const active = state.selected?.taskId === t.taskId
  const amount = t.amount != null && t.amount !== '' ? `¥${t.amount}` : ''
  const cat = state.category
  // 按分类给发起人/办理人/知会人各自的正确动作。
  let acts
  if (cat === 'initiated') {
    // 我发起的（是实例，非任务）：查看轨迹 + 撤销（仅进行中）。不是办理人，无办理/转签。
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>` +
      (t.state === 'ACTIVE' ? `<button class="todo-btn" data-withdraw="${esc(t.instanceId)}" title="下游未处理时取回，可改后重交">取回</button><button class="todo-btn danger" data-cancel="${esc(t.instanceId)}">撤销</button>` : '')
  } else if (cat === 'cc') {
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>
            <button class="todo-btn" data-ccread="${esc(t.ccId || t.taskId)}">标记已读</button>`
  } else if (cat === 'done') {
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>`
  } else if (t.claimable) {
    acts = `<button class="todo-btn primary" data-claim="${esc(t.taskId)}">认领</button>`
  } else {
    // 我的待办（办理人）
    acts = `<button class="todo-btn primary" data-open="${esc(t.taskId)}">办理</button>
            <button class="todo-btn" data-transfer="${esc(t.taskId)}">转签</button>`
  }
  const icon = cat === 'initiated' ? 'journey-arrive' : (t.claimable ? 'inbox' : (cat === 'cc' ? 'email' : (cat === 'done' ? 'accept' : 'workflow-tasks')))
  return `<article class="todo-card ${active ? 'active' : ''} ${t.urgent ? 'urgent' : ''}" data-task="${esc(t.taskId)}">
    <div class="todo-card-ava"><ui5-icon name="${icon}"></ui5-icon></div>
    <div class="todo-card-main">
      <div class="todo-card-title"><b>${esc(t.businessKey || t.instanceId)}</b><span>${esc(t.definitionName || t.definitionKey || '')}</span></div>
      <div class="todo-card-meta">
        <span class="node"><ui5-icon name="workflow-tasks"></ui5-icon>${esc(t.nodeName || t.nodeBpmnId || '')}</span>
        ${t.applicant ? `<span><ui5-icon name="employee"></ui5-icon>${esc(t.applicant)}</span>` : ''}
        ${amount ? `<span><ui5-icon name="money-bills"></ui5-icon>${esc(amount)}</span>` : ''}
        ${elementLabel(t.elementValue) ? `<span class="mi" title="本待办处理的明细项"><ui5-icon name="product"></ui5-icon>${esc(elementLabel(t.elementValue))}</span>` : ''}
        ${t.createdAt ? `<span><ui5-icon name="history"></ui5-icon>${esc(fmtTime(t.createdAt))}</span>` : ''}
      </div>
    </div>
    <div class="todo-card-act">${acts}</div>
  </article>`
}

function fmtTime (iso) {
  if (!iso) return ''
  return String(iso).replace('T', ' ').slice(0, 16)
}

// 多实例子任务的元素（②）→ 卡片上的紧凑标签：对象取代表字段(name/title/sku/label/code/id)，
// 否则 JSON 缩略；标量原样。null/空 → 空串（卡片不渲染该 chip）。
function elementLabel (v) {
  if (v == null) return ''
  if (typeof v === 'object') {
    if (Array.isArray(v)) return v.length ? `${v.length} 项` : ''
    for (const k of ['name', 'title', 'sku', 'label', 'code', 'id']) {
      if (v[k] != null && v[k] !== '') return String(v[k])
    }
    const s = JSON.stringify(v)
    return s.length > 24 ? s.slice(0, 23) + '…' : s
  }
  return String(v)
}

// ————————————————————— property 区（轨迹 + 意见） —————————————————————

function propertyHtml () {
  const t = state.selected
  if (!t) {
    return `<section class="todo todo-prop"><div class="todo-prop-head"><b>流程轨迹</b><small>未选中</small></div>
      <div class="todo-empty"><ui5-icon name="detail-view"></ui5-icon><span>点击一条待办<br>查看流程轨迹与意见</span></div></section>`
  }
  const tokens = state.trail?.tokens || []
  const tasks = state.trail?.tasks || []
  const cur = new Set(tokens.map((k) => k.nodeBpmnId))
  const trailRows = (state.trail?.nodes || []).map((n) => {
    const on = cur.has(n.id)
    const done = tasks.some((x) => x.nodeBpmnId === n.id && x.completed)
    return `<div class="todo-trail-row ${on ? 'cur' : (done ? 'done' : '')}">
      <span class="todo-trail-dot"></span><b>${esc(n.name || n.id)}</b>
      <em>${on ? '当前' : (done ? '已过' : '')}</em></div>`
  }).join('') || '<div class="todo-hint">（轨迹以令牌位置为准）</div>'
  const commentRows = state.comments.length
    ? state.comments.map((c) => `<div class="todo-cmt">
        <div class="todo-cmt-head"><b>${esc(c.userId || '—')}</b>
          <span class="todo-cmt-dec ${c.decision === 'reject' ? 'rej' : 'ok'}">${esc(c.decision || '')}</span>
          <em>${esc(fmtTime(c.createdAt))}</em></div>
        <div class="todo-cmt-body">${esc(c.comment || '')}</div></div>`).join('')
    : '<div class="todo-hint">暂无审批意见</div>'
  return `<section class="todo todo-prop">
    <div class="todo-prop-head"><b>${esc(t.businessKey || t.instanceId)}</b><small>${esc(t.nodeName || '')}</small></div>
    <div class="todo-prop-body">
      <div class="todo-sec">流程轨迹</div>${trailRows}
      <div class="todo-sec">审批意见</div>${commentRows}
    </div></section>`
}

// ————————————————————— 事件绑定 —————————————————————

function bind (root, view, host) {
  root.querySelector('[data-act="refresh"]')?.addEventListener('click', () => loadTodos())
  if (view === 'explorer') {
    root.querySelectorAll('[data-cat]').forEach((b) => b.addEventListener('click', () => {
      switchCategory(b.dataset.cat)
    }))
  }
  if (view === 'content') {
    bindFilterBar(root)
    bindPager(root)
    root.querySelectorAll('.todo-card').forEach((card) => card.addEventListener('click', (e) => {
      if (e.target.closest('button')) return
      if (card.dataset.task) selectTodo(card.dataset.task)
    }))
    root.querySelectorAll('[data-open]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); openTaskForm(b.dataset.open, b) }))
    root.querySelectorAll('[data-claim]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); claimTodo(b.dataset.claim) }))
    root.querySelectorAll('[data-transfer]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); transferTodo(b.dataset.transfer) }))
    root.querySelectorAll('[data-start]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); openStartForm(b.dataset.start, b) }))
    root.querySelectorAll('[data-view]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); viewTodo(b.dataset.view, b) }))
    root.querySelectorAll('[data-cancel]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); cancelInstance(b.dataset.cancel) }))
    root.querySelectorAll('[data-withdraw]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); withdrawInstance(b.dataset.withdraw) }))
    root.querySelectorAll('[data-ccread]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); markCcRead(b.dataset.ccread) }))
  }
}

// 切换待办分类：重置分页与过滤，避免跨类残留。
function switchCategory (key) {
  if (state.category === key) return
  state.category = key
  state.selected = null
  state.page = 1
  state.filter = { keyword: '', definitionKey: '', nodeBpmnId: '', state: '' }
  refreshView('explorer'); loadTodos()
}

// 工具条：关键字（回车/失焦即查）+ 三个下拉 change 即查。改任一过滤都回到第 1 页。
function bindFilterBar (root) {
  const kw = root.querySelector('[data-f-keyword]')
  if (kw) {
    const apply = () => { const v = kw.value.trim(); if (v === state.filter.keyword) return; state.filter.keyword = v; state.page = 1; loadTodos() }
    kw.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); apply() } })
    kw.addEventListener('change', apply)
  }
  root.querySelector('[data-f-def]')?.addEventListener('change', (e) => {
    state.filter.definitionKey = e.target.value; state.filter.nodeBpmnId = ''; state.page = 1; loadTodos()
  })
  root.querySelector('[data-f-node]')?.addEventListener('change', (e) => {
    state.filter.nodeBpmnId = e.target.value; state.page = 1; loadTodos()
  })
  root.querySelector('[data-f-state]')?.addEventListener('change', (e) => {
    state.filter.state = e.target.value; state.page = 1; loadTodos()
  })
  root.querySelector('[data-f-reset]')?.addEventListener('click', () => {
    state.filter = { keyword: '', definitionKey: '', nodeBpmnId: '', state: '' }; state.page = 1; loadTodos()
  })
}

// 分页控件：首/上/下/末页 + 每页条数。
function bindPager (root) {
  const total = state.total || 0
  const pages = Math.max(1, Math.ceil(total / state.pageSize))
  const go = (p) => { const np = Math.min(Math.max(1, p), pages); if (np === state.page) return; state.page = np; loadTodos() }
  root.querySelectorAll('[data-pg]').forEach((b) => b.addEventListener('click', () => {
    const k = b.dataset.pg
    if (k === 'first') go(1)
    else if (k === 'prev') go(state.page - 1)
    else if (k === 'next') go(state.page + 1)
    else if (k === 'last') go(pages)
  }))
  root.querySelector('[data-pg-size]')?.addEventListener('change', (e) => {
    state.pageSize = parseInt(e.target.value, 10) || 20; state.page = 1; loadTodos()
  })
}

// ————————————————————— 数据/动作 —————————————————————

// 组装查找/过滤/分页 query 串。
function filterQs (extra) {
  const f = state.filter
  const p = new URLSearchParams()
  if (f.keyword) p.set('keyword', f.keyword)
  if (f.definitionKey) p.set('definitionKey', f.definitionKey)
  if (f.nodeBpmnId) p.set('nodeBpmnId', f.nodeBpmnId)
  if (f.state) p.set('state', f.state)
  p.set('page', String(state.page))
  p.set('pageSize', String(state.pageSize))
  for (const k in (extra || {})) p.set(k, extra[k])
  return p.toString()
}

async function loadTodos () {
  state.loading = true; refreshView('content')
  const user = currentUser()
  try {
    if (state.category === 'initiate') {
      // 发起流程：可发起定义（量小，前端过滤，无分页）。
      const d = await apiJson('/api/flow/startable')
      let list = d.definitions || []
      const kw = (state.filter.keyword || '').trim().toLowerCase()
      if (kw) list = list.filter((x) => (x.name || x.key || '').toLowerCase().includes(kw) || (x.key || '').toLowerCase().includes(kw))
      if (state.filter.definitionKey) list = list.filter((x) => x.key === state.filter.definitionKey)
      state.startables = list; state.total = list.length
    } else if (state.category === 'todo' || state.category === 'claimable') {
      const d = await apiJson(`/api/flow/tasks/my?${filterQs({ assignee: user, kind: state.category })}`)
      state.todos = d.tasks || []; state.total = d.total || 0
    } else if (state.category === 'cc') {
      const d = await apiJson(`/api/flow/todos/cc?${filterQs({ user })}`)
      state.todos = (d.tasks || []).map((x) => ({ ...x, ccId: String(x.taskId || '').replace(/^cc-/, ''), definitionName: x.definitionKey }))
      state.total = d.total || 0
    } else if (state.category === 'initiated') {
      const d = await apiJson(`/api/flow/todos/initiated?${filterQs({})}`)
      state.todos = (d.tasks || []).map(instToTodo); state.total = d.total || 0
    } else if (state.category === 'done') {
      const d = await apiJson(`/api/flow/todos/done?${filterQs({ user })}`)
      state.todos = (d.tasks || []).map((x) => ({ ...x, definitionName: x.definitionKey }))
      state.total = d.total || 0
    }
  } catch (e) { toast('加载失败: ' + e.message); state.todos = []; state.startables = []; state.total = 0 }
  state.loading = false; refreshView('content'); refreshView('explorer')
}

// 加载过滤下拉选项（定义 + 节点），一次即可。
async function loadFilterOptions () {
  if (state.defOptions.length) return
  try {
    const d = await apiJson('/api/flow/todos/filters')
    state.defOptions = d.definitions || []
    refreshView('content')
  } catch { /* 忽略 */ }
}

function ccToTodo (c) {
  return { taskId: 'cc-' + (c.id || ''), ccId: c.id, instanceId: c.instanceId || c.instance_id,
    businessKey: c.businessKey || c.business_key, definitionName: c.definitionName,
    nodeName: c.nodeBpmnId || c.node_bpmn_id, formKey: null, createdAt: c.createdAt || c.created_at }
}
function instToTodo (i) {
  // /api/flow/todos/initiated 返回 RawTodo 投影：taskId(已 inst- 前缀)/instanceId/definitionKey/
  // businessKey/state/applicant/amount/createdAt/currentNode/formKey（当前活动环节反查）。
  return {
    taskId: i.taskId || ('inst-' + i.instanceId), instanceId: i.instanceId,
    businessKey: i.businessKey,
    definitionName: i.definitionKey, definitionKey: i.definitionKey,
    nodeName: stateLabel(i.state), state: i.state,
    currentNode: i.currentNode, nodeBpmnId: i.currentNode,
    applicant: i.applicant, amount: i.amount,
    formKey: i.formKey || null, formMode: i.formMode || 'approve',
    createdAt: i.createdAt || null,
  }
}
function stateLabel (s) {
  return ({ ACTIVE: '进行中', COMPLETED: '已完成', TERMINATED: '已终止' })[s] || s || ''
}

async function selectTodo (taskId) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  state.selected = t
  refreshView('content')
  // 拉轨迹 + 意见
  try {
    if (t.instanceId) {
      state.trail = await apiJson(`/api/flow/instances/${enc(t.instanceId)}`)
      const c = await apiJson(`/api/flow/instances/${enc(t.instanceId)}/comments`)
      state.comments = c.comments || []
    }
  } catch { state.trail = null; state.comments = [] }
  refreshView('property')
}

// 发起流程：解析 startFormKey → 打开发起表单（task-form 的 mode:'start'）。
function openStartForm (defKey, sourceEl) {
  const d = state.startables.find((x) => x.key === defKey)
  if (!d) return
  resolveForm(d.startFormKey).then((f) => {
    const startCtx = {
      mode: 'start', definitionKey: d.key, definitionName: d.name || d.key,
      startFormKey: d.startFormKey || '', formMode: 'edit',
      bizTable: f.bizTable || '', title: `发起 · ${d.name || d.key}`,
      domain: f.domain || '', application: f.application || '', module: f.module || '', file: f.file || '', apiPath: f.apiPath || '',
    }
    const sid = slug('start-' + d.key)
    const propView = {
      id: `flow-start-${sid}-prop`, tabLabel: '发起', icon: 'add-activity',
      type: 'native_pages', native_page: 'portal.flow.task-form', view: 'property', props: startCtx,
    }
    // content = 起点表单页（html_page / 真实 native_page / task-form 兜底），同办理态三分派。
    let contentRegion
    if (f.kind === 'html' && f.htmlPage) {
      contentRegion = { caption: '发起表单', icon: 'form',
        views: [{ id: `flow-start-${sid}-html`, tabLabel: '表单', icon: 'form', type: 'html_pages', html_page: f.htmlPage, props: { ...startCtx } }] }
    } else if (f.nativePage && f.nativePage !== 'portal.flow.task-form') {
      contentRegion = { caption: '发起表单', icon: 'form',
        views: [{ id: `flow-start-${sid}-page`, tabLabel: '表单', icon: 'form', type: 'native_pages', native_page: f.nativePage, view: f.nativeView || 'content', props: { ...startCtx } }] }
    } else {
      contentRegion = { caption: '发起', icon: 'form',
        views: [{ id: `flow-start-${sid}-content`, tabLabel: '表单', icon: 'form', type: 'native_pages', native_page: 'portal.flow.task-form', view: 'content', props: startCtx }] }
    }
    const workNode = {
      id: `flow-start-${sid}`, name: `flow-start-${sid}`, type: 'workspace-node',
      caption: startCtx.title, menuName: startCtx.title, icon: 'add-activity', openType: 0, status: 1,
      workspace: { id: `flow_start_${sid}`, params: startCtx, content: contentRegion, property: { caption: '发起', icon: 'add-activity', views: [propView] } },
    }
    openWorkNode(workNode, sourceEl)
  })
}

// 打开任务表单页（走 portal-help-action / inlineNode，与报表设计器同一条真实链路）。
// 两类分派：html 表单 → content 视图指向 html_pages 业务表单，审批控制台在 property(task-form)；
//          native 表单/兜底 → content+property 都是 task-form。
function openTaskForm (taskId, sourceEl) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  resolveForm(t.formKey).then((f) => {
    // kind='workspace'：打开节点定义的完整工作台（content=业务表单/菜单），property 叠加审批视图。
    if (f && f.kind === 'workspace' && f.workspaceNode) return buildWorkspaceWorknode(t, f, sourceEl, { readonly: false })
    return buildAndOpenTaskForm(t, f, sourceEl)
  })
}

// 查看（我发起的 / 抄送我的 / 我已办）：同样开完整工作台，但 property 叠加**只读**任务视图
// （只看轨迹+意见，无同意/驳回）。无 workspace 绑定则退回页内 property 轨迹（selectTodo）。
function viewTodo (taskId, sourceEl) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  resolveForm(t.formKey).then((f) => {
    if (f && f.kind === 'workspace' && f.workspaceNode) return buildWorkspaceWorknode(t, f, sourceEl, { readonly: true })
    return selectTodo(taskId)
  }).catch(() => selectTodo(taskId))
}

function buildAndOpenTaskForm (t, f, sourceEl) {
  // 任务上下文：传给节点表单页（native_page/html_pages）的 props，也作 html_pages 的 initialContext。
  const taskCtx = {
    mode: 'task', formKey: t.formKey || '', formMode: t.formMode || 'approve',
    taskId: t.taskId, instanceId: t.instanceId,
    bizTable: t.bizTable || f.bizTable || '', bizId: t.bizId || '',
    businessKey: t.businessKey || '', nodeName: t.nodeName || '',
    domain: f.domain || '', application: f.application || '', module: f.module || '', file: f.file || '', apiPath: f.apiPath || '',
  }
  const sid = slug(`${t.instanceId}-${t.taskId}`)
  const title = `${t.businessKey || t.instanceId} · ${t.nodeName || ''}`

  // property 区默认挂 task-form 通用审批控制台（同意/驳回/意见/轨迹）；表单绑定声明
  // console='none' 时省略——该表单自带审批操作（业务模块封装审批动作，如 MDM M7.1），
  // content 全屏承载，避免同一动作出现「平台控制台直调引擎 + 业务按钮走业务端点」双口径。
  const propView = {
    id: `flow-task-${sid}-prop`, tabLabel: '审批', icon: 'detail-view',
    type: 'native_pages', native_page: 'portal.flow.task-form', view: 'property', props: { ...taskCtx },
  }
  const usePlatformConsole = f.console !== 'none' 

  // content 区 = 节点 formKey 定义的真实表单页。三种来源：
  //   ① html_page → 门户原生 hydrate 的 html-pages 业务表单
  //   ② 真实 native_page（非 task-form 壳）→ 直接打开那张 native 表单页的指定 view
  //   ③ 都没配 → 退回 task-form 自带的通用只读展示（兜底，保证待办永远能打开）
  let contentRegion
  let initialContext = null
  if (f.kind === 'html' && f.htmlPage) {
    initialContext = { taskId: t.taskId, instanceId: t.instanceId, bizTable: taskCtx.bizTable, bizId: t.bizId || '', formMode: taskCtx.formMode, id: t.bizId || '' }
    contentRegion = {
      caption: '业务表单', icon: 'form',
      views: [{ id: `flow-task-${sid}-html`, tabLabel: '表单', icon: 'form', type: 'html_pages', html_page: f.htmlPage, props: { ...taskCtx, ...initialContext } }],
    }
  } else if (f.nativePage && f.nativePage !== 'portal.flow.task-form') {
    // 真实 native 表单页：直接以 worknode 打开节点定义的那张页
    contentRegion = {
      caption: '业务表单', icon: 'form',
      views: [{ id: `flow-task-${sid}-page`, tabLabel: '表单', icon: 'form', type: 'native_pages', native_page: f.nativePage, view: f.nativeView || 'content', props: { ...taskCtx } }],
    }
  } else {
    // 兜底：无绑定真实表单页 → task-form 通用展示
    contentRegion = {
      caption: '办理', icon: 'form',
      views: [{ id: `flow-task-${sid}-content`, tabLabel: '表单', icon: 'form', type: 'native_pages', native_page: 'portal.flow.task-form', view: 'content', props: { ...taskCtx } }],
    }
  }

  const workNode = {
    id: `flow-task-${sid}`, name: `flow-task-${sid}`, type: 'workspace-node',
    caption: title, menuName: title, icon: 'workflow-tasks', openType: 0, status: 1,
    workspace: {
      id: `flow_task_${sid}`, params: taskCtx,
      content: contentRegion,
      // console='none'：property 挂 task-form 的只读轨迹视图（viewOnly——意见历史+轨迹，
      // 无办结动作）；审批操作在 content 表单内（业务封装端点）。门户 workspace 对无
      // property/空 views 的节点渲染异常，故保留结构挂只读视图而非省略。
      ...(usePlatformConsole
          ? { property: { caption: '审批', icon: 'detail-view', views: [propView] } }
          : {
              property: {
                caption: '流程轨迹', icon: 'detail-view',
                views: [Object.assign({}, propView, { props: { ...taskCtx, viewOnly: true } })],
              },
            }),
    },
  }
  openWorkNode(workNode, sourceEl, initialContext)
}

// 任务上下文（传给节点表单页 / 叠加的审批视图作 props）。
function taskCtxOf (t, f) {
  return {
    mode: 'task', formKey: t.formKey || '', formMode: t.formMode || 'approve',
    taskId: t.taskId, instanceId: t.instanceId,
    bizTable: t.bizTable || (f && f.bizTable) || '', bizId: t.bizId || '',
    businessKey: t.businessKey || '', nodeName: t.nodeName || '',
    domain: (f && f.domain) || '', application: (f && f.application) || '', module: (f && f.module) || '',
    file: (f && f.file) || '', apiPath: (f && f.apiPath) || '',
  }
}

// kind='workspace'：取节点定义的完整 workspace node（门户文件库），把任务上下文注入各区视图 props，
// 并在 **property 区叠加**一个任务处理视图（办理=审批控制台；查看=只读轨迹）。节点原有 property 视图保留在前。
// 取不到 node / 无 workspace 时兜底回退旧 task-form 路径，保证不回归。
async function buildWorkspaceWorknode (t, f, sourceEl, opts) {
  const readonly = !!(opts && opts.readonly)
  let node = null
  try {
    node = await apiJson('/api/workspace-nodes/' + enc(f.workspaceNode))
  } catch { /* 取不到 → 兜底 */ }
  const ws = node && node.workspace
  if (!ws || typeof ws !== 'object') return buildAndOpenTaskForm(t, f, sourceEl)

  const taskCtx = taskCtxOf(t, f)
  // 深拷贝，避免污染缓存/复用；注入 props。
  const workspace = deepClone(ws)
  injectTaskProps(workspace, taskCtx)

  // property 叠加：追加审批/只读任务视图（保留节点自带 property 视图在前）。
  if (!workspace.property || typeof workspace.property !== 'object') {
    workspace.property = { caption: '处理', icon: 'detail-view', views: [] }
  }
  if (!Array.isArray(workspace.property.views)) workspace.property.views = []
  workspace.property.views.push({
    id: 'flow-task-approval',
    tabLabel: readonly ? '轨迹' : '审批', icon: 'detail-view',
    type: 'native_pages', native_page: 'portal.flow.task-form', view: 'property',
    props: { ...taskCtx, formMode: readonly ? 'readonly' : (t.formMode || 'approve'), viewOnly: readonly },
  })

  const sid = slug(`${t.instanceId}-${t.taskId}`)
  const title = `${t.businessKey || t.instanceId} · ${t.nodeName || ''}`
  const workNode = {
    id: `flow-wsform-${sid}`, name: `flow-wsform-${sid}`, type: 'workspace-node',
    caption: title, menuName: title, icon: node.icon || 'workflow-tasks', openType: 0, status: 1,
    workspace: { ...workspace, id: `flow_wsform_${sid}`, params: taskCtx },
  }
  return openWorkNode(workNode, sourceEl)
}

// 结构化深拷贝（无 structuredClone 时退回 JSON）。
function deepClone (o) {
  try { return (typeof structuredClone === 'function') ? structuredClone(o) : JSON.parse(JSON.stringify(o)) }
  catch { return JSON.parse(JSON.stringify(o)) }
}

// 给 workspace 各区（content/explorer/property/bottom/...）的 native_pages / html_pages 视图 props
// 注入任务上下文（浅合并，节点已配的 props 优先保留）。空区/空 views/非对象 props 全容错。
function injectTaskProps (workspace, taskCtx) {
  if (!workspace || typeof workspace !== 'object') return
  for (const key of Object.keys(workspace)) {
    const region = workspace[key]
    const views = region && Array.isArray(region.views) ? region.views
      : (Array.isArray(region) ? region : null)
    if (!views) continue
    for (const v of views) {
      if (!v || typeof v !== 'object') continue
      if (v.type === 'native_pages' || v.type === 'html_pages') {
        const p = (v.props && typeof v.props === 'object') ? v.props : {}
        v.props = { ...taskCtx, ...p }   // 节点已配的键优先（覆盖 taskCtx 默认）
      }
    }
  }
}
// 最后 POST /api/workspace-nodes 兜底。sourceEl 必须是门户 DOM 内的元素，事件才能 bubble 到 portal-app。
// initialContext（可选）：html_pages 表单的动态跳转传参（portal 在 hydrate 前 ws.context.set 逐键写入）。
async function openWorkNode (workNode, sourceEl, initialContext) {
  // S4：抽核接缝。组件壳/headless 壳设 CFG.onOpenTask → 把门户特有的开 Tab 链塌缩成一个回调
  // （宿主自决怎么开：派发 CustomEvent / 路由跳转 / 渲染到某容器）。门户壳 onOpenTask=null → 走下方
  // 原有 portal-help-action/inlineNode 真实链路，逐字节等价今天。
  if (typeof CFG.onOpenTask === 'function') {
    try { CFG.onOpenTask({ workNode, initialContext: initialContext || null }); return true }
    catch (e) { console.error('onOpenTask 回调失败', e); return false }
  }
  const candidates = [window, window.parent, window.top, globalThis].filter(Boolean)
  for (const target of candidates) {
    try {
      if (typeof target.openTab === 'function') { target.openTab(workNode, initialContext ? { initialContext } : undefined); return true }
      if (typeof target.openWorkspaceNode === 'function') { target.openWorkspaceNode(workNode, initialContext ? { initialContext } : undefined); return true }
    } catch {}
  }
  // 真实生效路径：portal-help-action + kind:inlineNode（portal-app.js 监听并 seed 打开）。
  const detail = { kind: 'inlineNode', node: workNode, icon: workNode.icon || 'workflow-tasks', title: workNode.caption || workNode.id }
  if (initialContext) detail.extras = { initialContext }
  const ev = () => new CustomEvent('portal-help-action', { detail, bubbles: true, composed: true })
  try {
    if (sourceEl?.dispatchEvent) { sourceEl.dispatchEvent(ev()); return true }
  } catch {}
  try { document.dispatchEvent(ev()); return true } catch {}
  try {
    await apiJson('/api/workspace-nodes', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: workNode.id, name: workNode.caption, icon: workNode.icon || 'workflow-tasks', details: `待办表单：${workNode.id}`, workspace: workNode.workspace }),
    })
    return true
  } catch {}
  return false
}

async function claimTodo (taskId) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  try {
    await apiJson(`/api/flow/tasks/${enc(taskId)}/claim`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ instanceId: t.instanceId, userId: currentUser() }),
    })
    toast('已认领')
    loadTodos()
  } catch (e) { toast('认领失败: ' + e.message) }
}

async function transferTodo (taskId) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  const to = window.prompt('转签给（用户 id）：')
  if (!to) return
  try {
    await apiJson(`/api/flow/tasks/${enc(taskId)}/transfer`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ instanceId: t.instanceId, fromUser: currentUser(), toUser: to, reason: '待办中心转签' }),
    })
    toast('已转签给 ' + to)
    loadTodos()
  } catch (e) { toast('转签失败: ' + e.message) }
}

// 我发起的：撤销一个进行中的实例（发起人视角动作）。
async function cancelInstance (instanceId) {
  if (!instanceId) return
  if (!window.confirm('确认撤销该流程实例？撤销后不可恢复。')) return
  try {
    await apiJson(`/api/flow/instances/${enc(instanceId)}/cancel`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ reason: '发起人在待办中心撤销' }),
    })
    toast('已撤销')
    loadTodos()
  } catch (e) { toast('撤销失败: ' + e.message) }
}

// 我发起的：取回 / 撤回（④）——下游未处理时拉回发起处（可改后重交）。后端护栏校验发起人 + 策略，
// 不满足则返回原因，这里 toast 提示（区别于「撤销」的整单终止）。
async function withdrawInstance (instanceId) {
  if (!instanceId) return
  if (!window.confirm('确认取回该流程？取回后流程回到你手中，可修改后重新提交。')) return
  try {
    await apiJson(`/api/flow/instances/${enc(instanceId)}/withdraw`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user: currentUser(), reason: '发起人在待办中心取回' }),
    })
    toast('已取回，可修改后重新提交')
    loadTodos()
  } catch (e) { toast('取回失败: ' + e.message) }
}

// 抄送我的：标记一条抄送为已读。
async function markCcRead (ccId) {
  if (!ccId) return
  try {
    await apiJson(`/api/flow/cc/${enc(ccId)}/read`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}',
    })
    toast('已标记已读')
    loadTodos()
  } catch (e) { toast('标记失败: ' + e.message) }
}

// 表单页办结后广播，待办中心收到即刷新。
try {
  window.addEventListener('cmx-flow-task-done', () => loadTodos())
} catch {}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  /* 主题跟随门户：全部走 --sap* 令牌（light/dark 自动翻），语义色用 color-mix 派生；写死值仅作降级 fallback。 */
  .todo{
    --brand:var(--sapButton_Emphasized_Background,var(--sapContent_IconColor,#0a6ed1));
    --ink:var(--sapTextColor,#1d2d3e);
    --muted:var(--sapContent_LabelColor,#6a6d70);
    --line:var(--sapGroup_TitleBorderColor,var(--sapField_BorderColor,#d9e2ec));
    --line-soft:color-mix(in srgb,var(--line) 55%,transparent);
    --surface:var(--sapBackgroundColor,#f5f6f7);
    --tile:var(--sapTile_Background,var(--sapList_Background,#fff));
    --header:var(--sapList_HeaderBackground,var(--sapTile_Background,#f7f9fc));
    --ok:var(--sapPositiveColor,var(--sapSuccessColor,#107e3e));
    --warn:var(--sapCriticalColor,var(--sapWarningColor,#e9730c));
    --red:var(--sapNegativeColor,var(--sapErrorColor,#e5484d));
    --brand-soft:color-mix(in srgb,var(--brand) 14%,var(--tile));
    --brand-line:color-mix(in srgb,var(--brand) 40%,var(--line));
    color-scheme:light dark;
    font:13px/1.5 var(--sapFontFamily,-apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif);
    color:var(--ink);background:var(--surface);height:100%;box-sizing:border-box;display:flex;flex-direction:column}
  .todo *{box-sizing:border-box}
  .todo ui5-icon{color:currentColor}

  /* explorer 头 + 分类导轨 */
  .todo-head{display:flex;align-items:center;justify-content:space-between;height:48px;flex:0 0 auto;padding:0 14px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .todo-head b{font-size:13.5px;font-weight:700;letter-spacing:.01em} .todo-head span{display:block;font-size:10.5px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .todo-icon-btn{border:1px solid var(--line);background:var(--tile);color:var(--muted);border-radius:8px;width:30px;height:30px;cursor:pointer;display:grid;place-items:center;transition:all .13s}
  .todo-icon-btn:hover{color:var(--brand);border-color:var(--brand-line);background:var(--brand-soft)}
  .todo-cat-list{flex:1;overflow:auto;padding:10px 8px;display:flex;flex-direction:column;gap:3px}
  .todo-cat{position:relative;display:flex;align-items:center;gap:10px;padding:9px 11px;border:1px solid transparent;border-radius:10px;background:transparent;cursor:pointer;text-align:left;transition:background .13s,box-shadow .13s;color:inherit}
  .todo-cat:hover{background:color-mix(in srgb,var(--brand) 7%,transparent)}
  .todo-cat.active{background:var(--brand-soft);box-shadow:inset 0 0 0 1px var(--brand-line)}
  .todo-cat.active::before{content:"";position:absolute;left:0;top:50%;transform:translateY(-50%);width:3px;height:20px;border-radius:0 3px 3px 0;background:var(--brand)}
  .todo-cat-ic{width:32px;height:32px;border-radius:9px;display:grid;place-items:center;background:color-mix(in srgb,var(--brand) 10%,var(--tile));color:var(--brand);flex:0 0 auto;transition:all .13s}
  .todo-cat-ic ui5-icon{width:1rem;height:1rem}
  .todo-cat.active .todo-cat-ic{background:var(--brand);color:var(--sapButton_Emphasized_TextColor,#fff);box-shadow:0 4px 12px color-mix(in srgb,var(--brand) 34%,transparent)}
  .todo-cat-main{min-width:0} .todo-cat-main b{display:block;font-size:13px;font-weight:600} .todo-cat-main small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .todo-cat.active .todo-cat-main b{color:var(--brand)}

  /* content 头 + 列表 */
  .todo-content{height:100%}
  .todo-bar{display:flex;align-items:center;gap:8px;min-height:48px;flex:0 0 auto;padding:7px 14px;border-bottom:1px solid var(--line-soft);background:var(--header);flex-wrap:wrap}
  .todo-bar b{font-size:14px;font-weight:700;white-space:nowrap}
  .todo-count{min-width:22px;height:20px;padding:0 7px;border-radius:999px;background:var(--brand);color:var(--sapButton_Emphasized_TextColor,#fff);font-size:11px;font-weight:800;display:inline-flex;align-items:center;justify-content:center;flex:0 0 auto}
  .todo-sp{flex:1 1 12px}
  .todo-btn{font:inherit;font-size:12px;border:1px solid var(--line);background:var(--tile);color:var(--ink);border-radius:8px;padding:6px 12px;cursor:pointer;font-weight:600;display:inline-flex;align-items:center;gap:5px;transition:all .13s;white-space:nowrap}
  .todo-btn:hover{border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)}
  .todo-btn.primary{background:var(--brand);border-color:var(--brand);color:var(--sapButton_Emphasized_TextColor,#fff);box-shadow:0 2px 8px color-mix(in srgb,var(--brand) 26%,transparent)}
  .todo-btn.primary:hover{background:color-mix(in srgb,var(--brand) 86%,#000);color:var(--sapButton_Emphasized_TextColor,#fff)}
  .todo-btn ui5-icon{width:.85rem;height:.85rem}
  .todo-list{flex:1;overflow:auto;padding:14px;display:flex;flex-direction:column;gap:10px;background:var(--surface)}

  /* content 过滤控件（内联在 .todo-bar 内，与刷新同排） */
  .todo-search{display:flex;align-items:center;gap:6px;flex:0 1 240px;min-width:150px;max-width:300px;height:30px;padding:0 10px;border:1px solid var(--line);border-radius:8px;background:var(--tile);transition:border-color .13s}
  .todo-search:focus-within{border-color:var(--brand-line);box-shadow:0 0 0 2px var(--brand-soft)}
  .todo-search ui5-icon{width:.9rem;height:.9rem;color:var(--muted);flex:0 0 auto}
  .todo-search input{flex:1;min-width:0;border:0;outline:0;background:transparent;font:inherit;font-size:12.5px;color:var(--ink)}
  .todo-search input::placeholder{color:var(--muted)}
  .todo-bar select{font:inherit;font-size:12.5px;height:30px;padding:0 26px 0 10px;border:1px solid var(--line);border-radius:8px;background:var(--tile) url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'><path fill='%236a6d70' d='M0 0l5 6 5-6z'/></svg>") no-repeat right 9px center;color:var(--ink);cursor:pointer;max-width:160px;appearance:none;-webkit-appearance:none;transition:border-color .13s}
  .todo-bar select:hover{border-color:var(--brand-line)}
  .todo-bar select:focus{outline:0;border-color:var(--brand-line);box-shadow:0 0 0 2px var(--brand-soft)}
  .todo-bar select:disabled{opacity:.5;cursor:not-allowed}

  /* content 分页页脚 */
  .todo-pager{display:flex;align-items:center;gap:6px;flex:0 0 auto;padding:8px 14px;border-top:1px solid var(--line-soft);background:var(--header)}
  .todo-pager-info{font-size:12px;color:var(--muted);white-space:nowrap}
  .todo-pager select{font:inherit;font-size:12px;height:28px;padding:0 8px;border:1px solid var(--line);border-radius:7px;background:var(--tile);color:var(--ink);cursor:pointer}
  .todo-pg{font:inherit;font-size:13px;min-width:30px;height:28px;padding:0 8px;border:1px solid var(--line);border-radius:7px;background:var(--tile);color:var(--ink);cursor:pointer;display:inline-flex;align-items:center;justify-content:center;transition:all .13s}
  .todo-pg:hover:not(:disabled){border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)}
  .todo-pg:disabled{opacity:.4;cursor:not-allowed}

  /* 待办卡片 —— 图标头像 + 主体 + 动作，悬浮抬升 */
  .todo-card{position:relative;display:flex;gap:12px;align-items:center;min-height:60px;border:1px solid var(--line-soft);border-radius:12px;background:var(--tile);padding:13px 15px 13px 16px;cursor:pointer;transition:border-color .14s,box-shadow .14s,transform .14s}
  .todo-card::before{content:"";position:absolute;left:0;top:8px;bottom:8px;width:3px;border-radius:3px;background:var(--brand);opacity:0;transition:opacity .14s}
  .todo-card:hover{border-color:var(--brand-line);box-shadow:0 6px 20px color-mix(in srgb,var(--brand) 12%,transparent);transform:translateY(-1px)}
  .todo-card:hover::before{opacity:.5}
  .todo-card.active{border-color:var(--brand);box-shadow:0 8px 24px color-mix(in srgb,var(--brand) 18%,transparent)}
  .todo-card.active::before{opacity:1}
  .todo-card.urgent{border-color:color-mix(in srgb,var(--warn) 40%,var(--line))}
  .todo-card.urgent::before{background:var(--warn);opacity:.7}
  .todo-card-ava{width:38px;height:38px;border-radius:10px;flex:0 0 auto;display:grid;place-items:center;background:color-mix(in srgb,var(--brand) 12%,var(--tile));color:var(--brand)}
  .todo-card-ava ui5-icon{width:1.15rem;height:1.15rem}
  .todo-card.urgent .todo-card-ava{background:color-mix(in srgb,var(--warn) 15%,var(--tile));color:var(--warn)}
  .todo-card-main{flex:1;min-width:0}
  .todo-card-title{display:flex;align-items:baseline;gap:8px;min-width:0}
  .todo-card-title b{font-size:14px;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis} .todo-card-title span{font-size:10.5px;color:var(--muted);font-family:ui-monospace,Menlo,monospace;flex:0 0 auto}
  /* 第二行：环节 + meta 标签，单行不换行、超出省略（保证卡片严格两行不撑破） */
  .todo-card-meta{display:flex;flex-wrap:nowrap;gap:6px;margin-top:6px;overflow:hidden}
  .todo-card-meta span{display:inline-flex;align-items:center;gap:4px;font-size:11px;border:1px solid var(--line-soft);border-radius:6px;padding:2px 7px;background:color-mix(in srgb,var(--ink) 3%,var(--tile));color:var(--muted);white-space:nowrap;flex:0 0 auto}
  .todo-card-meta span.node{color:var(--brand);border-color:color-mix(in srgb,var(--brand) 34%,var(--line));background:color-mix(in srgb,var(--brand) 9%,var(--tile));font-weight:600;min-width:0}
  .todo-card-meta span.node ui5-icon{flex:0 0 auto}
  .todo-card-meta span.warn{color:var(--warn);border-color:color-mix(in srgb,var(--warn) 38%,var(--line));background:color-mix(in srgb,var(--warn) 10%,var(--tile))}
  .todo-card-meta ui5-icon{width:.72rem;height:.72rem}
  .todo-card-act{display:flex;flex-direction:row;align-items:center;gap:6px;flex:0 0 auto}
  .todo-card-act .todo-btn{white-space:nowrap}
  .todo-empty{display:flex;flex-direction:column;align-items:center;gap:10px;color:var(--muted);font-size:12.5px;padding:56px 16px;text-align:center}
  .todo-empty ui5-icon{width:2rem;height:2rem;color:color-mix(in srgb,var(--muted) 60%,transparent)}

  /* property 轨迹 + 意见 */
  .todo-prop-head{height:48px;flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 15px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .todo-prop-head b{font-size:14px;font-weight:700} .todo-prop-head small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .todo-prop-body{padding:14px 15px;overflow:auto}
  .todo-sec{font-size:11px;font-weight:800;color:var(--brand);letter-spacing:.04em;text-transform:uppercase;margin:16px 0 9px;padding-bottom:6px;border-bottom:1px solid var(--line-soft);display:flex;align-items:center;gap:6px}
  .todo-sec:first-child{margin-top:0}
  .todo-trail-row{display:flex;align-items:center;gap:9px;padding:6px 0;font-size:12.5px;color:var(--muted)}
  .todo-trail-row.cur{color:var(--brand);font-weight:700} .todo-trail-row.done{color:var(--ok)}
  .todo-trail-dot{width:9px;height:9px;border-radius:50%;background:currentColor;flex:0 0 auto;box-shadow:0 0 0 3px color-mix(in srgb,currentColor 20%,transparent)} .todo-trail-row em{margin-left:auto;font-style:normal;font-size:10.5px;font-weight:600}
  .todo-cmt{border:1px solid var(--line-soft);border-radius:10px;padding:9px 11px;margin-bottom:8px;background:color-mix(in srgb,var(--ink) 2.5%,var(--tile))}
  .todo-cmt-head{display:flex;align-items:center;gap:7px;font-size:12px} .todo-cmt-head b{color:var(--ink);font-weight:700}
  .todo-cmt-dec{font-size:10px;font-weight:700;padding:1px 7px;border-radius:6px} .todo-cmt-dec.ok{color:var(--ok);background:color-mix(in srgb,var(--ok) 14%,var(--tile));border:1px solid color-mix(in srgb,var(--ok) 40%,transparent)} .todo-cmt-dec.rej{color:var(--red);background:color-mix(in srgb,var(--red) 14%,var(--tile));border:1px solid color-mix(in srgb,var(--red) 40%,transparent)}
  .todo-cmt-head em{margin-left:auto;font-style:normal;font-size:10.5px;color:var(--muted)}
  .todo-cmt-body{font-size:12.5px;margin-top:5px;color:var(--ink)}
  .todo-hint{font-size:12px;color:var(--muted);padding:8px 0}

  /* toast */
  .todo-toast{position:absolute;left:50%;bottom:20px;transform:translateX(-50%) translateY(8px);background:color-mix(in srgb,var(--ink) 92%,#000);color:var(--surface);padding:10px 18px;border-radius:10px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s,transform .2s;z-index:20;box-shadow:0 8px 28px rgba(0,0,0,.3)}
  .todo-toast.show{opacity:1;transform:translateX(-50%) translateY(0)}
  `
}

// 门户壳：export default 的 views 用 CFG 默认值（同源 fetch + localStorage 用户 + openWorkNode Tab 链），
// 等价今天。S5 可嵌组件壳 import { configure, mount } 后先 configure({apiBase,authHeaders,getUser,onOpenTask})
// 再自行 mount 到 shadowRoot——同一份核，两种壳。
export { configure, mount }
export default {
  defaultView: 'content',
  views: {
    async explorer (ctx) { return mount(ctx, 'explorer') },
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
