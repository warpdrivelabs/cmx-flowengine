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

// formKey → 表单页坐标。F4：唯一真源 = 后端注册表 /api/flow/forms/{key}（接新表单只需配一行，
// 不改前端）。技术债 014：前端内置 FORM_MAP 兜底已删除——注册表外的硬编码路由会让「删除的
// 绑定离线复活」，掩盖配置错误；注册表不可用时明确报错而非静默兜底。缓存解析结果（会话级，
// 绑定热更新后刷新页面生效）。
const formCache = {}
async function resolveForm (formKey) {
  if (!formKey) return {}
  if (formCache[formKey]) return formCache[formKey]
  const b = await apiJson('/api/flow/forms/' + enc(formKey))
  if (!b || !b.formKey) throw new Error('表单绑定不存在: ' + formKey + '（请在表单绑定管理页注册）')
  const f = { kind: b.kind || 'native', nativePage: b.nativePage || '', nativeView: b.nativeView || b.view || 'content', htmlPage: b.htmlPage || '', workspaceNode: b.workspaceNode || '', bizTable: b.bizTable || '', domain: b.domain || '', application: b.application || '', module: b.module || '', file: b.file || '', apiPath: b.apiPath || '', title: b.title || '', console: b.console || 'platform' }
  formCache[formKey] = f
  return f
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
  error: '',
  selected: null,     // 选中的待办（property 展示其轨迹）
  trail: null,        // 选中实例的详情（令牌 + 任务链）
  trailDefinition: null, // 选中实例对应流程定义（当前实例接口无 nodes，轨迹从这里补节点）
  trailLoading: false,
  trailError: '',
  comments: [],       // 选中实例的意见历史
  cardMenu: '',       // 当前展开的卡片更多菜单 taskId
  dialog: null,       // 页内确认框 / 转签人员选择器
  hosts: new Set(),
  // 查找/过滤/分页（每次切分类重置）
  filter: { keyword: '', definitionKey: '', nodeBpmnId: '', state: '' },
  page: 1,
  pageSize: 20,
  total: 0,
  defOptions: [],     // 过滤下拉：定义列表 [{key,name,nodes:[{id,name}]}]
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const { deepClone } = globalThis.__cmxDataComp // 共享深拷贝（cmx-data-comp/lib/cmx-deep-clone.js；审查 B-04）
const enc = encodeURIComponent
const slug = (s) => String(s || '').replace(/[^A-Za-z0-9_-]+/g, '_') || 'x'

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装（cmx-data-comp/lib/cmx-page-helpers.js）；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

function rememberUserSnapshots (users) {
  try {
    globalThis.__cmxFlowUsers = globalThis.__cmxFlowUsers || {}
    for (const u of (users || [])) {
      const id = String(u?.id || u?.userId || u?.user_id || '')
      if (!id) continue
      globalThis.__cmxFlowUsers[id] = {
        nickName: u.nickName || u.nickname || '',
        userName: u.userName || u.username || '',
      }
    }
  } catch {}
}

async function loadUserSnapshots () {
  try {
    if (globalThis.__cmxFlowUsersLoaded) return
    globalThis.__cmxFlowUsersLoaded = true
    const rows = await apiJson('/api/iam/users/list', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ pageSize: 200 }),
    })
    rememberUserSnapshots(Array.isArray(rows) ? rows : rows?.items || [])
  } catch { /* 历史意见保留用户 ID */ }
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

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
      : (state.error
          ? `<div class="todo-empty"><ui5-icon name="error"></ui5-icon><span>${esc(state.error)}</span><button class="todo-btn" data-act="refresh">重试</button></div>`
          : (state.startables.length
              ? state.startables.map(startableCard).join('')
              : `<div class="todo-empty"><ui5-icon name="inbox"></ui5-icon><span>暂无可发起流程</span></div>`))
    return `<section class="todo todo-content">
      <div class="todo-bar"><b>发起流程</b><span class="todo-count">${state.startables.length}</span>
        <span class="todo-sp"></span>
        ${filterControlsHtml()}
        <button class="todo-btn" data-act="refresh"><ui5-icon name="refresh"></ui5-icon> 刷新</button></div>
      <div class="todo-list">${body}</div>
      ${dialogHtml()}
      <div class="todo-toast"></div>
    </section>`
  }
  const body = state.loading
    ? `<div class="todo-empty"><ui5-icon name="busy"></ui5-icon><span>加载中...</span></div>`
    : (state.error
        ? `<div class="todo-empty"><ui5-icon name="error"></ui5-icon><span>${esc(state.error)}</span><button class="todo-btn" data-act="refresh">重试</button></div>`
        : (state.todos.length
        ? state.todos.map(todoCard).join('')
        : `<div class="todo-empty"><ui5-icon name="inbox"></ui5-icon><span>${esc(cat?.label || '')}：暂无</span></div>`))
  return `<section class="todo todo-content">
    <div class="todo-bar"><b>${esc(cat?.label || '待办')}</b><span class="todo-count">${state.total}</span>
      <span class="todo-sp"></span>
      ${filterControlsHtml()}
      <button class="todo-btn" data-act="refresh"><ui5-icon name="refresh"></ui5-icon> 刷新</button></div>
    <div class="todo-list">${body}</div>
    ${pagerHtml()}
    ${dialogHtml()}
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
  // 主动作只保留一个，次要动作收入「更多」；只读类不超过两个动作。
  const menuOpen = state.cardMenu === t.taskId
  let acts
  if (cat === 'initiated') {
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>
      <button class="todo-btn icon" data-card-menu="${esc(t.taskId)}" aria-haspopup="menu" aria-expanded="${menuOpen ? 'true' : 'false'}" title="更多操作"><ui5-icon name="overflow"></ui5-icon></button>
      ${menuOpen ? `<div class="todo-card-menu" role="menu">
        ${t.state === 'ACTIVE' ? `<button role="menuitem" data-withdraw="${esc(t.instanceId)}">取回</button><button role="menuitem" class="danger" data-cancel="${esc(t.instanceId)}">撤销</button>` : '<span>流程已结束</span>'}
      </div>` : ''}`
  } else if (cat === 'cc') {
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>
            <button class="todo-btn" data-ccread="${esc(t.ccId || t.taskId)}">标记已读</button>`
  } else if (cat === 'done') {
    acts = `<button class="todo-btn" data-view="${esc(t.taskId)}">查看</button>`
  } else if (t.claimable) {
    acts = `<button class="todo-btn primary" data-claim="${esc(t.taskId)}">认领</button>`
  } else {
    acts = `<button class="todo-btn primary" data-open="${esc(t.taskId)}">办理</button>
      <button class="todo-btn icon" data-card-menu="${esc(t.taskId)}" aria-haspopup="menu" aria-expanded="${menuOpen ? 'true' : 'false'}" title="更多操作"><ui5-icon name="overflow"></ui5-icon></button>
      ${menuOpen ? `<div class="todo-card-menu" role="menu"><button role="menuitem" data-transfer="${esc(t.taskId)}">转签</button></div>` : ''}`
  }
  const icon = cat === 'initiated' ? 'journey-arrive' : (t.claimable ? 'inbox' : (cat === 'cc' ? 'email' : (cat === 'done' ? 'accept' : 'workflow-tasks')))
  return `<article class="todo-card ${active ? 'active' : ''} ${t.urgent ? 'urgent' : ''} ${menuOpen ? 'menu-open' : ''}" data-task="${esc(t.taskId)}">
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
  // 后端时间统一为 RFC3339 UTC；展示层转浏览器本地时区，避免把 05:56Z 直接当本地 05:56。
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return String(iso).replace('T', ' ').slice(0, 16)
  const p = (n) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}

function displayActor (row) {
  const id = String(row?.userId || row?.user_id || row?.assignee || '')
  const cached = id ? globalThis.__cmxFlowUsers?.[id] : null
  return row?.nickName || row?.userName || row?.nickname || row?.username || cached?.nickName || cached?.userName || row?.userId || row?.user_id || row?.assignee || '—'
}

const APPROVAL_ACTION_LABELS = {
  approve: '同意', reject: '驳回', return: '退回', submit: '制单',
  complete: '办结', transfer: '转签', withdraw: '取回', cancel: '撤销',
}

// 与 task-form 保持同版本：待办中心轻量进度与办理页完整轨迹必须由同一套算法产出。
const APPROVAL_VIEW_MODEL_VERSION = '20260831-step-time-1'

function normalizeApprovalAction (row, node) {
  const raw = String(row?.decision ?? '').trim()
  const key = raw.toLowerCase()
  if (key && APPROVAL_ACTION_LABELS[key]) {
    return { key, label: APPROVAL_ACTION_LABELS[key], tone: key === 'reject' ? 'rej' : (key === 'return' || key === 'withdraw' ? 'warn' : 'ok') }
  }
  if (key) return { key, label: raw, tone: 'ok' }
  const id = String(node?.id || row?.nodeBpmnId || '').toLowerCase()
  const isCreation = id === 'apply' || id === 'start' || String(node?.name || '').includes('发起')
  return { key: isCreation ? 'submit' : 'complete', label: isCreation ? '制单' : '办理', tone: 'ok' }
}

function normalizeApprovalComment (row, node) {
  const action = normalizeApprovalAction(row, node)
  const text = String(row?.comment ?? '').trim()
  return {
    id: String(row?.id || row?.taskId || `${row?.nodeBpmnId || 'unknown'}-${row?.createdAt || row?.userId || Math.random()}`),
    nodeId: String(row?.nodeBpmnId || ''),
    actor: displayActor(row),
    action,
    text: text || (action.key === 'submit' ? '制单提交' : '（未填写意见）'),
    createdAt: row?.createdAt || '',
    time: fmtTime(row?.createdAt),
  }
}

function graphHasBranch (definition) {
  const count = new Map()
  for (const e of (definition?.edges || [])) count.set(e.from, (count.get(e.from) || 0) + 1)
  return Array.from(count.values()).some((n) => n > 1)
}

function buildApprovalSteps ({ instance, definition, comments, task }) {
  const inst = instance || {}
  const activeIds = new Set([
    ...((inst.tokens || []).map((x) => String(x.nodeBpmnId || ''))),
    ...((inst.activeNodes || []).map((x) => String(x || ''))),
  ].filter(Boolean))
  const tasks = inst.tasks || []
  const completedIds = new Set(tasks.filter((x) => x.completed).map((x) => String(x.nodeBpmnId || '')).filter(Boolean))
  // 只把真实执行证据（活动令牌 / 任务）补进节点轴；意见找不到节点时保留 unmatched，不伪造成步骤。
  const observedIds = new Set([...activeIds, ...completedIds])
  const branch = graphHasBranch(definition)
  let nodes = []
  if (Array.isArray(definition?.nodes) && definition.nodes.length) {
    nodes = definition.nodes
      .filter((n) => String(n?.kind || '') === 'userTask' || observedIds.has(String(n?.id || '')))
      .map((n) => ({ ...n, id: String(n.id || ''), source: 'definition' }))
  } else if (Array.isArray(inst.nodes) && inst.nodes.length) {
    nodes = inst.nodes.map((n) => ({ ...n, id: String(n.id || ''), source: 'instance' }))
  } else {
    const seen = new Set()
    nodes = tasks
      .map((x) => ({ id: String(x.nodeBpmnId || ''), name: x.name || x.nodeBpmnId, kind: 'userTask', source: 'observed' }))
      .filter((n) => n.id && !seen.has(n.id) && seen.add(n.id))
  }
  const knownIds = new Set(nodes.map((n) => n.id))
  for (const id of observedIds) {
    if (knownIds.has(id)) continue
    nodes.push({ id, name: tasks.find((x) => String(x.nodeBpmnId || '') === id)?.name || id, kind: 'userTask', source: 'observed' })
    knownIds.add(id)
  }
  const normalized = (comments || []).map((c) => normalizeApprovalComment(c, nodes.find((n) => n.id === String(c.nodeBpmnId || ''))))
    .sort((a, b) => String(a.createdAt).localeCompare(String(b.createdAt)))
  const mismatch = normalized.some((c) => c.nodeId && !knownIds.has(c.nodeId))
  const steps = nodes.map((node, index) => {
    const current = activeIds.has(node.id)
    const done = !current && completedIds.has(node.id)
    const uncertain = !current && !done && branch
    const nodeTasks = tasks.filter((x) => String(x.nodeBpmnId || '') === node.id)
    const nodeComments = normalized.filter((c) => c.nodeId === node.id)
    const isSelectedTaskNode = current && String(task?.nodeBpmnId || '') === node.id
    const latestComment = nodeComments[nodeComments.length - 1] || null
    const stepTime = current
      ? (isSelectedTaskNode ? (task?.createdAt || task?.taskCreatedAt || '') : (latestComment?.createdAt || ''))
      : (done ? (latestComment?.createdAt || '') : '')
    return {
      id: node.id, name: node.name || node.id, index,
      status: current ? 'current' : (done ? 'done' : (uncertain ? 'possible' : 'pending')),
      statusText: current ? '当前' : (done ? '已完成' : (uncertain ? '可能' : '待处理')),
      time: stepTime,
      timeText: fmtTime(stepTime),
      timeLabel: current ? '到达时间' : (done ? '完成时间' : ''),
      actors: Array.from(new Set(nodeTasks.map((x) => x.assignee || x.ownerUserId).filter(Boolean))),
      comments: nodeComments,
      source: node.source,
    }
  })
  return { steps, unmatchedActions: normalized.filter((c) => !c.nodeId || !knownIds.has(c.nodeId)), definitionMismatch: mismatch }
}

function buildApprovalViewModel ({ instance, definition, comments, task }) {
  const shared = globalThis.__cmxFlowApprovalView?.buildApprovalViewModel
  // task-form 已注册同构实现时优先复用；本页自身注册则直接走本地实现。
  if (typeof shared === 'function' && globalThis.__cmxFlowApprovalView?.version === APPROVAL_VIEW_MODEL_VERSION && shared !== buildApprovalViewModel) return shared({ instance, definition, comments, task })
  const inst = instance || {}
  const built = buildApprovalSteps({ instance: inst, definition, comments, task })
  const actions = [...built.steps.flatMap((s) => s.comments), ...built.unmatchedActions]
    .sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)))
  const current = built.steps.find((s) => s.status === 'current')
  return {
    meta: {
      businessKey: inst.businessKey || task?.businessKey || inst.id || task?.instanceId || '',
      definitionName: definition?.name || task?.definitionName || inst.definitionKey || task?.definitionKey || '',
      instanceState: inst.state || task?.state || '',
      currentNode: current?.name || task?.nodeName || task?.currentNode || '',
    },
    ...built,
    latestAction: actions[0] || null,
  }
}
try {
  const existing = globalThis.__cmxFlowApprovalView
  globalThis.__cmxFlowApprovalView = existing?.version === APPROVAL_VIEW_MODEL_VERSION
    ? existing
    : { version: APPROVAL_VIEW_MODEL_VERSION, buildApprovalViewModel }
} catch {}

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

// ————————————————————— property 区（统一流转时间线） —————————————————————

function propertyHtml () {
  const t = state.selected
  if (!t) {
    return `<section class="todo todo-prop"><div class="todo-prop-head"><b>流程概览</b><small>未选中</small></div>
      <div class="todo-empty"><ui5-icon name="detail-view"></ui5-icon><span>点击一条待办<br>查看轻量流程进度</span></div></section>`
  }
  if (state.trailLoading) {
    return `<section class="todo todo-prop">
      <div class="todo-prop-head"><b>${esc(t.businessKey || t.instanceId)}</b><small>加载中…</small></div>
      <div class="todo-prop-body"><div class="todo-skeleton-list"><span></span><span></span><span></span></div></div>
    </section>`
  }
  if (state.trailError) {
    return `<section class="todo todo-prop">
      <div class="todo-prop-head"><b>${esc(t.businessKey || t.instanceId)}</b><small>加载失败</small></div>
      <div class="todo-prop-body"><div class="todo-inline-error">${esc(state.trailError)}</div>
        <div class="todo-actions"><button class="todo-btn" data-reload-trail>重试</button></div></div>
    </section>`
  }
  const vm = buildApprovalViewModel({
    instance: state.trail,
    definition: state.trailDefinition,
    comments: state.comments,
    task: {
      ...t,
      nodeBpmnId: t.nodeBpmnId || t.currentNode || '',
      createdAt: t.createdAt || '',
    },
  })
  const stateText = ({ ACTIVE: '进行中', COMPLETED: '已完成', TERMINATED: '已终止' })[vm.meta.instanceState] || vm.meta.instanceState || '—'
  return `<section class="todo todo-prop">
    <div class="todo-prop-head"><b>${esc(vm.meta.businessKey || t.instanceId)}</b><small>${esc(vm.meta.definitionName || '')} · ${esc(stateText)}</small></div>
    <div class="todo-prop-body">
      <div class="todo-summary">
        <div><span>当前节点</span><b>${esc(vm.meta.currentNode || '—')}</b></div>
        <div><span>单据状态</span><b>${esc(stateText)}</b></div>
      </div>
      <div class="todo-sec">流程进度</div>${compactTimelineHtml(vm)}
      <div class="todo-sec">最新流转</div>${vm.latestAction ? commentCardHtml(vm.latestAction) : '<div class="todo-hint">暂无流转记录</div>'}
    </div></section>`
}

// 待办中心只做轻量预览：展示节点状态与最新一条记录；完整意见与办理动作进入办理页。
function compactTimelineHtml (vm) {
  if (!vm.steps.length && !vm.unmatchedActions.length) return '<div class="todo-hint">暂无流程节点</div>'
  const rows = vm.steps.map((step, idx) => `<li class="todo-flow-node ${step.status}"${step.status === 'current' ? ' aria-current="step"' : ''}>
      <div class="todo-flow-rail"><span class="todo-flow-step">${step.status === 'done' ? '<ui5-icon name="accept"></ui5-icon>' : String(idx + 1)}</span></div>
      <div class="todo-flow-content">
        <div class="todo-flow-title"><b>${esc(step.name)}</b><span class="todo-flow-state">${esc(step.statusText)}</span></div>
        <div class="todo-flow-meta">
          <span class="todo-flow-time" title="${esc(step.timeLabel || '时间')}${step.timeText ? '：' + esc(step.timeText) : ''}"><ui5-icon name="history"></ui5-icon>${esc(step.timeText || '—')}</span>
          ${step.actors.length ? `<span class="todo-flow-actors">办理人：${step.actors.map((x) => esc(displayActor({ userId: x }))).join('、')}</span>` : ''}
        </div>
      </div>
    </li>`).join('')
  const otherRows = vm.unmatchedActions.map((c) => `<li class="todo-flow-node other">
      <div class="todo-flow-rail"><span class="todo-flow-step record"></span></div>
      <div class="todo-flow-content">${commentCardHtml(c)}</div>
    </li>`).join('')
  const mismatch = vm.definitionMismatch ? '<div class="todo-inline-warn">当前定义与实例轨迹存在版本差异，未识别节点已按实际记录追加。</div>' : ''
  return `<ol class="todo-flow compact">${rows}${otherRows}</ol>${mismatch}`
}

function commentCardHtml (c) {
  return `<div class="todo-cmt">
    <div class="todo-cmt-head"><b>${esc(c.actor)}</b>
      <span class="todo-cmt-dec ${c.action.tone}">${esc(c.action.label)}</span>
      <em>${esc(c.time)}</em></div>
    <div class="todo-cmt-body">${esc(c.text)}</div></div>`
}

function dialogHtml () {
  const d = state.dialog
  if (!d) return ''
  if (d.kind === 'confirm') {
    return `<div class="todo-dialog-mask" data-dialog>
    <div class="todo-dialog ${d.intent || ''}" role="dialog" aria-modal="true" aria-labelledby="todo-dialog-title" tabindex="-1">
        <div class="todo-dialog-head"><b id="todo-dialog-title">${esc(d.title)}</b></div>
        <p>${esc(d.message)}</p>
        ${d.error ? `<div class="todo-inline-error">${esc(d.error)}</div>` : ''}
        <div class="todo-dialog-actions">
          <button class="todo-btn" data-dialog-cancel>取消</button>
          <button class="todo-btn ${d.intent === 'danger' ? 'danger' : 'primary'}" data-dialog-confirm ${d.submitting ? 'disabled' : ''}>${d.submitting ? '提交中…' : esc(d.confirmText || '确认')}</button>
        </div>
      </div>
    </div>`
  }
  const users = (d.users || []).filter((u) => {
    const kw = (d.keyword || '').trim().toLowerCase()
    if (!kw) return true
    return [u.id, u.username, u.nickname].some((v) => String(v || '').toLowerCase().includes(kw))
  })
  return `<div class="todo-dialog-mask" data-dialog>
    <div class="todo-dialog" role="dialog" aria-modal="true" aria-labelledby="todo-user-title" tabindex="-1">
      <div class="todo-dialog-head"><b id="todo-user-title">转签给</b><small>${esc(d.businessKey || '')}</small></div>
      <div class="todo-search dialog-search"><ui5-icon name="search"></ui5-icon>
        <input data-user-search value="${esc(d.keyword || '')}" placeholder="搜索姓名 / 账号 / 用户 ID"></div>
      ${d.loading ? '<div class="todo-hint">加载用户…</div>' : ''}
      ${d.error ? `<div class="todo-inline-error">${esc(d.error)}</div>` : ''}
      <div class="todo-user-list">
        ${users.length ? users.map((u) => `<button class="todo-user ${d.selectedUserId === u.id ? 'active' : ''}" data-user-id="${esc(u.id)}" data-user-name="${esc(u.nickname || u.username || u.id)}">
            <b>${esc(u.nickname || u.username || u.id)}</b><small>${esc(u.username || '')} · ${esc(u.id)}</small>
          </button>`).join('') : (d.loading ? '' : '<div class="todo-hint">没有匹配用户</div>')}
      </div>
      <div class="todo-dialog-actions">
        <button class="todo-btn" data-dialog-cancel>取消</button>
        <button class="todo-btn primary" data-dialog-confirm ${(d.selectedUserId && !d.submitting) ? '' : 'disabled'}>${d.submitting ? '提交中…' : '确认转签'}</button>
      </div>
    </div>
  </div>`
}

// ————————————————————— 事件绑定 —————————————————————

function bind (root, view, host) {
  root.querySelector('[data-act="refresh"]')?.addEventListener('click', () => loadTodos())
  root.querySelector('[data-reload-trail]')?.addEventListener('click', () => selectTodo(state.selected?.taskId, true))
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
      if (card.dataset.task) {
        const menuChanged = state.cardMenu !== ''
        state.cardMenu = ''
        if (menuChanged) refreshView('content')
        selectTodo(card.dataset.task)
      }
    }))
    root.querySelectorAll('[data-open]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); openTaskForm(b.dataset.open, b) }))
    root.querySelectorAll('[data-card-menu]').forEach((b) => b.addEventListener('click', (e) => {
      e.stopPropagation()
      const id = b.dataset.cardMenu
      state.cardMenu = state.cardMenu === id ? '' : id
      refreshView('content')
    }))
    // 点页面任意空白关闭「更多操作」菜单：document 级委托（shadow DOM 的 click 是 composed 事件，
    // 冒泡到 document；composedPath()[0] 是真实目标，closest 同树判定是否落在菜单/开关/卡片内）。
    // 侧栏分类、属性面板、页面外壳等 root 之外的点击由此兜住——挂在 content root 上收不到这些。
    // 菜单项与「…」开关自身 stopPropagation 不会到这；卡片点击先清 cardMenu 再选中，
    // 这里的空值检查会直接退出，不与其冲突。模块单例，document 上守护绑定一次防堆叠。
    if (!document.__todoMenuOutside) {
      document.__todoMenuOutside = true
      document.addEventListener('click', (e) => {
        if (!state.cardMenu) return
        const t = e.composedPath()[0]
        if (t instanceof Element && t.closest('.todo-card-menu, [data-card-menu], .todo-card')) return
        state.cardMenu = ''
        refreshView('content')
      })
      document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && state.cardMenu) { state.cardMenu = ''; refreshView('content') }
      })
    }
    root.querySelectorAll('[data-claim]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); claimTodo(b.dataset.claim) }))
    root.querySelectorAll('[data-transfer]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); openTransferDialog(b.dataset.transfer) }))
    root.querySelectorAll('[data-start]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); openStartForm(b.dataset.start, b) }))
    root.querySelectorAll('[data-view]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); viewTodo(b.dataset.view, b) }))
    root.querySelectorAll('[data-cancel]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); cancelInstance(b.dataset.cancel) }))
    root.querySelectorAll('[data-withdraw]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); withdrawInstance(b.dataset.withdraw) }))
    root.querySelectorAll('[data-ccread]').forEach((b) => b.addEventListener('click', (e) => { e.stopPropagation(); markCcRead(b.dataset.ccread) }))
    bindDialog(root)
  }
}

function bindDialog (root) {
  const mask = root.querySelector('[data-dialog]')
  if (!mask) return
  mask.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeDialog() })
  root.querySelector('[data-dialog-cancel]')?.addEventListener('click', closeDialog)
  root.querySelector('[data-user-search]')?.addEventListener('input', (e) => {
    state.dialog.keyword = e.target.value || ''
    state.dialog.focusSearch = true
    refreshView('content')
    requestAnimationFrame(() => root.querySelector('[data-user-search]')?.focus())
  })
  root.querySelectorAll('[data-user-id]').forEach((b) => b.addEventListener('click', () => {
    state.dialog.selectedUserId = b.dataset.userId
    state.dialog.selectedUserName = b.dataset.userName
    refreshView('content')
  }))
  root.querySelector('[data-dialog-confirm]')?.addEventListener('click', () => confirmDialogAction())
}

// 切换待办分类：重置分页与过滤，避免跨类残留。
function switchCategory (key) {
  if (state.category === key) return
  state.category = key
  state.selected = null
  state.trail = null
  state.trailDefinition = null
  state.comments = []
  state.trailLoading = false
  state.trailError = ''
  state.cardMenu = ''
  if (state.dialog && !state.dialog.submitting) state.dialog = null
  state.page = 1
  state.filter = { keyword: '', definitionKey: '', nodeBpmnId: '', state: '' }
  refreshView('explorer'); refreshView('property'); loadTodos()
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
  state.cardMenu = ''
  state.loading = true; refreshView('content')
  state.error = ''
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
  } catch (e) {
    state.error = `加载失败：${e.message}`
    state.todos = []; state.startables = []; state.total = 0
    toast(state.error)
  }
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

const fullDefinitionCache = {}
async function loadFullDefinition (key) {
  if (!key) return null
  if (fullDefinitionCache[key]) return fullDefinitionCache[key]
  try {
    const d = await apiJson('/api/flow/definitions')
    for (const item of (d?.definitions || [])) fullDefinitionCache[item.key] = item
  } catch { /* 定义失败时保留实例已发生路径 */ }
  return fullDefinitionCache[key] || null
}

async function selectTodo (taskId, force) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  if (state.selected?.taskId === taskId && !force) return
  state.selected = t
  state.trailLoading = true
  state.trailError = ''
  state.trail = null
  state.trailDefinition = null
  state.comments = []
  refreshView('content')
  refreshView('property')
  // 实例接口不返回定义 nodes；并行拉实例、意见、定义，统一交给视图模型生成轨迹。
  try {
    if (t.instanceId) {
      const [inst, commentEnvelope] = await Promise.all([
        apiJson(`/api/flow/instances/${enc(t.instanceId)}`),
        apiJson(`/api/flow/instances/${enc(t.instanceId)}/comments`).catch(() => null),
        loadUserSnapshots(),
      ])
      state.trail = inst
      state.comments = Array.isArray(commentEnvelope?.comments) ? commentEnvelope.comments : []
      state.trailDefinition = await loadFullDefinition(inst.definitionKey || t.definitionKey)
    } else {
      state.trailError = '该记录没有流程实例标识'
    }
  } catch (e) {
    state.trail = null; state.comments = []; state.trailDefinition = null
    state.trailError = `流程轨迹加载失败：${e.message}`
  }
  state.trailLoading = false
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
  }).catch((e) => {
    // O-05：发起入口同补——绑定缺失/注册表不可达时可见反馈（原 unhandled rejection）。
    toast('打开发起表单失败: ' + ((e && e.message) || e))
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
  }).catch((e) => {
    // O-05：表单绑定缺失/注册表不可达时给出可见反馈（原 unhandled rejection 点按无反应）。
    toast('打开办理表单失败: ' + ((e && e.message) || e))
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
    definitionKey: t.definitionKey || '', definitionName: t.definitionName || '',
    bizTable: t.bizTable || f.bizTable || '', bizId: t.bizId || '',
    businessKey: t.businessKey || '', nodeBpmnId: t.nodeBpmnId || '', nodeName: t.nodeName || '',
    taskCreatedAt: t.createdAt || '',
    domain: f.domain || '', application: f.application || '', module: f.module || '', file: f.file || '', apiPath: f.apiPath || '',
    consoleMode: f.console || 'platform',
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
                views: [Object.assign({}, propView, { tabLabel: '流转', props: { ...taskCtx, viewOnly: true } })],
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
    definitionKey: t.definitionKey || '', definitionName: t.definitionName || '',
    bizTable: t.bizTable || (f && f.bizTable) || '', bizId: t.bizId || '',
    businessKey: t.businessKey || '', nodeBpmnId: t.nodeBpmnId || '', nodeName: t.nodeName || '',
    taskCreatedAt: t.createdAt || '',
    domain: (f && f.domain) || '', application: (f && f.application) || '', module: (f && f.module) || '',
    file: (f && f.file) || '', apiPath: (f && f.apiPath) || '',
    consoleMode: (f && f.console) || 'platform',
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
    tabLabel: readonly || f.console === 'none' ? '轨迹' : '审批', icon: 'detail-view',
    type: 'native_pages', native_page: 'portal.flow.task-form', view: 'property',
    props: {
      ...taskCtx,
      formMode: readonly ? 'readonly' : (t.formMode || 'approve'),
      viewOnly: readonly || f.console === 'none',
    },
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

function normalizeUser (u) {
  if (!u) return null
  return {
    id: String(u.id || u.userId || u.user_id || ''),
    username: String(u.username || u.userName || ''),
    nickname: String(u.nickname || u.nickName || ''),
  }
}

async function openTransferDialog (taskId) {
  const t = state.todos.find((x) => x.taskId === taskId)
  if (!t) return
  state.cardMenu = ''
  state.dialog = {
    kind: 'transfer', taskId, task: t, businessKey: t.businessKey || t.instanceId,
    users: [], keyword: '', selectedUserId: '', selectedUserName: '',
    loading: true, error: '', submitting: false,
  }
  refreshView('content')
  focusDialog()
  try {
    let users = []
    try {
      const portalUsers = await apiJson('/api/iam/users/list', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ pageSize: 100 }),
      })
      users = (Array.isArray(portalUsers) ? portalUsers : portalUsers?.items || []).map(normalizeUser).filter(Boolean)
    } catch { /* 门户用户目录不可用时退回 flow identity */ }
    if (!users.length) {
      const flowUsers = await apiJson('/api/flow/identity/users').catch(() => null)
      users = (flowUsers?.items || []).map(normalizeUser).filter(Boolean)
    }
    if (state.dialog?.kind !== 'transfer' || state.dialog.taskId !== taskId) return
    state.dialog.users = users.filter((u) => u.id)
    rememberUserSnapshots(users)
    state.dialog.loading = false
  } catch (e) {
    if (state.dialog?.kind !== 'transfer' || state.dialog.taskId !== taskId) return
    state.dialog.loading = false
    state.dialog.error = `用户列表加载失败：${e.message}`
  }
  refreshView('content')
  focusDialog()
}

function openConfirmDialog ({ title, message, intent, confirmText, action, payload }) {
  state.cardMenu = ''
  state.dialog = { kind: 'confirm', title, message, intent, confirmText, action, payload }
  refreshView('content')
  focusDialog()
}

function focusDialog () {
  requestAnimationFrame(() => {
    for (const host of state.hosts) {
      const el = hostRoot(host)?.querySelector?.('.todo-dialog')
      if (el) { el.focus(); return }
    }
  })
}

function closeDialog () {
  if (state.dialog?.submitting) return
  state.dialog = null
  refreshView('content')
}

async function confirmDialogAction () {
  const d = state.dialog
  if (!d || d.submitting) return
  if (d.kind === 'confirm') {
    if (d.action === 'cancel') await performCancelInstance(d.payload)
    if (d.action === 'withdraw') await performWithdrawInstance(d.payload)
    return
  }
  if (!d.selectedUserId) return
  d.submitting = true
  refreshView('content')
  try {
    await apiJson(`/api/flow/tasks/${enc(d.taskId)}/transfer`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        instanceId: d.task.instanceId,
        fromUser: currentUser(),
        toUser: d.selectedUserId,
        reason: `待办中心转签给 ${d.selectedUserName || d.selectedUserId}`,
      }),
    })
    state.dialog = null
    toast(`已转签给 ${d.selectedUserName || d.selectedUserId}`)
    loadTodos()
  } catch (e) {
    d.submitting = false
    d.error = `转签失败：${e.message}`
    refreshView('content')
  }
}

// 我发起的：撤销一个进行中的实例（发起人视角动作）。
async function cancelInstance (instanceId) {
  if (!instanceId) return
  openConfirmDialog({
    title: '撤销流程',
    message: '确认撤销该流程实例？撤销后不可恢复。',
    intent: 'danger',
    confirmText: '撤销',
    action: 'cancel',
    payload: instanceId,
  })
}

async function performCancelInstance (instanceId) {
  const d = state.dialog
  if (!d) return
  d.submitting = true
  refreshView('content')
  try {
    await apiJson(`/api/flow/instances/${enc(instanceId)}/cancel`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ reason: '发起人在待办中心撤销' }),
    })
    state.dialog = null
    toast('已撤销')
    loadTodos()
  } catch (e) {
    d.submitting = false
    d.error = `撤销失败：${e.message}`
    refreshView('content')
  }
}

// 我发起的：取回 / 撤回——下游未处理时拉回发起处，可修改后重新提交。
async function withdrawInstance (instanceId) {
  if (!instanceId) return
  openConfirmDialog({
    title: '取回流程',
    message: '确认取回该流程？取回后流程回到你手中，可修改后重新提交。',
    confirmText: '取回',
    action: 'withdraw',
    payload: instanceId,
  })
}

async function performWithdrawInstance (instanceId) {
  const d = state.dialog
  if (!d) return
  d.submitting = true
  refreshView('content')
  try {
    await apiJson(`/api/flow/instances/${enc(instanceId)}/withdraw`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user: currentUser(), reason: '发起人在待办中心取回' }),
    })
    state.dialog = null
    toast('已取回，可修改后重新提交')
    loadTodos()
  } catch (e) {
    d.submitting = false
    d.error = `取回失败：${e.message}`
    refreshView('content')
  }
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
  .todo-cat.active .todo-cat-ic{background:var(--brand);color:var(--sapButton_Emphasized_TextColor,var(--sapBaseColor));box-shadow:0 4px 12px color-mix(in srgb,var(--brand) 34%,transparent)}
  .todo-cat-main{min-width:0} .todo-cat-main b{display:block;font-size:13px;font-weight:600} .todo-cat-main small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .todo-cat.active .todo-cat-main b{color:var(--brand)}

  /* content 头 + 列表 */
  .todo-content{height:100%}
  .todo-bar{display:flex;align-items:center;gap:8px;min-height:48px;flex:0 0 auto;padding:7px 14px;border-bottom:1px solid var(--line-soft);background:var(--header);flex-wrap:wrap}
  .todo-bar b{font-size:14px;font-weight:700;white-space:nowrap}
  .todo-count{min-width:22px;height:20px;padding:0 7px;border-radius:999px;background:var(--brand);color:var(--sapButton_Emphasized_TextColor,var(--sapBaseColor));font-size:11px;font-weight:800;display:inline-flex;align-items:center;justify-content:center;flex:0 0 auto}
  .todo-sp{flex:1 1 12px}
  .todo-btn{font:inherit;font-size:12px;border:1px solid var(--line);background:var(--tile);color:var(--ink);border-radius:8px;padding:6px 12px;cursor:pointer;font-weight:600;display:inline-flex;align-items:center;gap:5px;transition:all .13s;white-space:nowrap}
  .todo-btn:hover{border-color:var(--brand-line);color:var(--brand);background:var(--brand-soft)}
  .todo-btn.primary{background:var(--brand);border-color:var(--brand);color:var(--sapButton_Emphasized_TextColor,var(--sapBaseColor));box-shadow:0 2px 8px color-mix(in srgb,var(--brand) 26%,transparent)}
  .todo-btn.primary:hover{background:color-mix(in srgb,var(--brand) 86%,var(--sapBaseColor));color:var(--sapButton_Emphasized_TextColor,var(--sapBaseColor))}
  .todo-btn.danger{border-color:color-mix(in srgb,var(--red) 40%,var(--line));color:var(--red)}
  .todo-btn.danger:hover{background:color-mix(in srgb,var(--red) 10%,var(--tile));color:var(--red)}
  .todo-btn:disabled{opacity:.55;cursor:not-allowed}
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
  .todo-card-act{position:relative;display:flex;flex-direction:row;align-items:center;gap:6px;flex:0 0 auto}
  .todo-card-act .todo-btn{white-space:nowrap}
  .todo-btn.icon{width:32px;padding:0;display:inline-flex;align-items:center;justify-content:center}
  .todo-btn.icon ui5-icon{width:.95rem;height:.95rem}
  /* 菜单打开时抬升整卡：hover 的 transform 会把卡片变成堆叠上下文，菜单 z-index 被困在卡内，
     会被 DOM 靠后的兄弟卡片盖住（症状：悬停时菜单被下一行遮挡）——整卡置顶后菜单随卡浮在上层。 */
  .todo-card.menu-open{z-index:30}
  .todo-card-menu{position:absolute;right:0;top:38px;z-index:12;min-width:112px;padding:5px;border:1px solid var(--line-soft);border-radius:9px;background:var(--tile);box-shadow:0 12px 32px color-mix(in srgb,var(--ink) 18%,transparent);display:flex;flex-direction:column;gap:3px}
  .todo-card-menu button,.todo-card-menu span{border:0;background:transparent;color:var(--ink);font:inherit;font-size:12.5px;text-align:left;padding:7px 9px;border-radius:7px;cursor:pointer}
  .todo-card-menu button:hover,.todo-card-menu button:focus-visible{background:color-mix(in srgb,var(--brand) 10%,var(--tile));outline:none}
  .todo-card-menu span{color:var(--muted);cursor:default}
  .todo-card-menu .danger{color:var(--red)}
  .todo-empty{display:flex;flex-direction:column;align-items:center;gap:10px;color:var(--muted);font-size:12.5px;padding:56px 16px;text-align:center}
  .todo-empty ui5-icon{width:2rem;height:2rem;color:color-mix(in srgb,var(--muted) 60%,transparent)}

  /* property：AntD Steps 语义的竖向只读步骤条 */
  .todo-prop-head{height:48px;flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 15px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .todo-prop-head b{font-size:14px;font-weight:700} .todo-prop-head small{display:block;font-size:10.5px;color:var(--muted);margin-top:1px}
  .todo-prop-body{padding:14px 15px;overflow:auto}
  .todo-summary{display:grid;grid-template-columns:1fr 1fr;gap:7px;margin-bottom:4px}
  .todo-summary div{min-width:0;border:1px solid var(--line-soft);border-radius:8px;padding:7px 9px;background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .todo-summary span{display:block;font-size:10.5px;color:var(--muted)}
  .todo-summary b{display:block;margin-top:2px;font-size:12px;overflow-wrap:anywhere}
  .todo-inline-error{font-size:12px;color:var(--red);background:color-mix(in srgb,var(--red) 10%,var(--tile));border-radius:8px;padding:8px 10px;box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--red) 26%,transparent)}
  .todo-inline-warn{margin-top:8px;font-size:12px;color:var(--warn);background:color-mix(in srgb,var(--warn) 10%,var(--tile));border-radius:8px;padding:8px 10px;box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--warn) 26%,transparent)}
  .todo-actions{display:flex;gap:8px;margin-top:9px}
  .todo-skeleton-list{display:flex;flex-direction:column;gap:10px}
  .todo-skeleton-list span{height:36px;border-radius:8px;background:color-mix(in srgb,var(--ink) 8%,transparent);animation:todo-skeleton 1.2s ease-in-out infinite}
  .todo-skeleton-list span:nth-child(2){animation-delay:.16s}.todo-skeleton-list span:nth-child(3){animation-delay:.32s}
  @keyframes todo-skeleton{0%,100%{opacity:.55}50%{opacity:1}}
  .todo-sec{font-size:11px;font-weight:800;color:var(--brand);letter-spacing:.04em;text-transform:uppercase;margin:16px 0 9px;padding-bottom:6px;border-bottom:1px solid var(--line-soft);display:flex;align-items:center;gap:6px}
  .todo-sec:first-child{margin-top:0}
  .todo-flow{display:block;margin:0;padding:0 0 4px;list-style:none}
  .todo-flow-node{display:grid;grid-template-columns:32px minmax(0,1fr);gap:0 12px;padding-bottom:16px}
  .todo-flow-node:last-of-type{padding-bottom:0}
  .todo-flow-rail{position:relative;display:flex;justify-content:center;z-index:1}
  .todo-flow-step{width:28px;height:28px;display:grid;place-items:center;border-radius:50%;border:1px solid var(--line);background:var(--tile);color:var(--muted);font-size:11.5px;font-weight:700;flex:0 0 auto}
  .todo-flow-step ui5-icon{width:.85rem;height:.85rem}
  .todo-flow-node:not(:last-of-type) .todo-flow-rail::after{content:"";position:absolute;top:32px;bottom:0;width:2px;background:color-mix(in srgb,var(--ink) 32%,transparent);z-index:-1}
  .todo-flow-node.done:not(:last-of-type) .todo-flow-rail::after{background:color-mix(in srgb,var(--ok) 58%,transparent)}
  .todo-flow-node.done .todo-flow-step{color:var(--ok);border-color:color-mix(in srgb,var(--ok) 48%,var(--line));background:var(--tile)}
  .todo-flow-node.current .todo-flow-step{color:var(--sapButton_Emphasized_TextColor,var(--sapContent_ContrastTextColor,var(--sapBaseColor)));border-color:var(--brand);background:var(--brand)}
  .todo-flow-node.pending .todo-flow-step{background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .todo-flow-node.possible .todo-flow-step{border-style:dashed}
  .todo-flow-node.other .todo-flow-step{width:10px;height:10px;margin:9px;border-style:dashed;background:transparent}
  .todo-flow-content{min-width:0;padding-top:4px}
  .todo-flow-title{display:flex;align-items:baseline;gap:8px;min-width:0}
  .todo-flow-title b{flex:1 1 auto;min-width:0;font-size:13px;font-weight:600;color:var(--ink);overflow-wrap:anywhere}
  .todo-flow-node.pending .todo-flow-title b{color:var(--muted)}
  .todo-flow-node.current .todo-flow-title b{color:var(--brand);font-weight:700}
  .todo-flow-state{flex:0 0 auto;font-size:10.5px;font-weight:600;color:var(--muted);white-space:nowrap}
  .todo-flow-node.done .todo-flow-state{color:var(--ok)}
  .todo-flow-node.current .todo-flow-state{color:var(--brand)}
  .todo-flow.compact .todo-flow-node{padding-bottom:12px}
  .todo-flow.compact .todo-flow-step{width:24px;height:24px;font-size:10.5px}
  .todo-flow.compact .todo-flow-node:not(:last-of-type) .todo-flow-rail::after{top:28px}
  .todo-flow.compact .todo-flow-title b{font-size:12.5px}
  .todo-flow-meta{display:flex;flex-wrap:wrap;align-items:center;gap:4px 8px;margin-top:3px}
  .todo-flow-time,.todo-flow-actors{display:inline-flex;align-items:center;gap:4px;min-width:0;font-size:10.5px;color:var(--muted)}
  .todo-flow-time ui5-icon{width:.7rem;height:.7rem;flex:0 0 auto}
  .todo-flow-actors{overflow-wrap:anywhere}
  .todo-flow-comments{display:flex;flex-direction:column;gap:7px;margin-top:8px}
  .todo-cmt{border-radius:8px;padding:8px 10px;background:color-mix(in srgb,var(--ink) 3%,var(--tile));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--ink) 7%,transparent)}
  .todo-cmt-head{display:flex;align-items:center;gap:6px;font-size:11.5px;flex-wrap:wrap} .todo-cmt-head b{min-width:0;max-width:100%;overflow-wrap:anywhere;color:var(--ink);font-weight:650}
  .todo-cmt-dec{flex:0 0 auto;font-size:10px;font-weight:700;padding:1px 6px;border-radius:999px;white-space:nowrap} .todo-cmt-dec.ok{color:var(--ok);background:color-mix(in srgb,var(--ok) 12%,transparent)} .todo-cmt-dec.warn{color:var(--warn);background:color-mix(in srgb,var(--warn) 12%,transparent)} .todo-cmt-dec.rej{color:var(--red);background:color-mix(in srgb,var(--red) 12%,transparent)}
  .todo-cmt-head em{margin-left:auto;flex:0 0 auto;font-style:normal;font-size:10px;color:var(--muted);white-space:nowrap}
  .todo-cmt-body{font-size:12px;line-height:1.45;margin-top:4px;color:var(--ink)}
  .todo-hint{font-size:12px;color:var(--muted);padding:8px 0}
  .todo-dialog-mask{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;padding:18px;background:color-mix(in srgb,var(--sapBaseColor,transparent) 58%,transparent);backdrop-filter:blur(2px)}
  .todo-dialog{width:min(420px,96vw);max-height:min(520px,92vh);display:flex;flex-direction:column;border:1px solid var(--line);border-radius:13px;background:var(--tile);color:var(--ink);box-shadow:0 22px 60px color-mix(in srgb,var(--ink) 30%,transparent)}
  .todo-dialog.danger{border-color:color-mix(in srgb,var(--red) 34%,var(--line))}
  .todo-dialog:focus-visible{outline:2px solid var(--brand);outline-offset:3px}
  .todo-dialog-head{padding:15px 17px 9px}
  .todo-dialog-head b{font-size:14.5px}
  .todo-dialog-head small{display:block;margin-top:3px;color:var(--muted);font-size:11.5px;overflow-wrap:anywhere}
  .todo-dialog p{margin:0;padding:0 17px;font-size:12.5px;line-height:1.6;color:var(--ink)}
  .todo-dialog .dialog-search{flex:0 0 auto;margin:10px 17px 8px;max-width:none}
  .todo-user-list{flex:1 1 auto;min-height:0;overflow:auto;display:flex;flex-direction:column;gap:6px;padding:0 17px}
  .todo-user{border:1px solid var(--line-soft);border-radius:9px;background:color-mix(in srgb,var(--ink) 3%,var(--tile));color:var(--ink);font:inherit;text-align:left;padding:9px 11px;cursor:pointer}
  .todo-user:hover,.todo-user:focus-visible,.todo-user.active{border-color:var(--brand);background:color-mix(in srgb,var(--brand) 10%,var(--tile));outline:none}
  .todo-user b{display:block;font-size:12.5px}
  .todo-user small{display:block;margin-top:2px;color:var(--muted);font-size:11px;overflow-wrap:anywhere}
  .todo-dialog-actions{display:flex;justify-content:flex-end;gap:8px;padding:14px 17px 16px}

  /* toast */
  .todo-toast{position:absolute;left:50%;bottom:20px;transform:translateX(-50%) translateY(8px);background:color-mix(in srgb,var(--ink) 92%,var(--sapBaseColor));color:var(--surface);padding:10px 18px;border-radius:10px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s,transform .2s;z-index:20;box-shadow:0 8px 28px color-mix(in srgb,var(--ink) 28%,transparent)}
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
