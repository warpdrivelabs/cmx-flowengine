/**
 * 任务表单宿主 —— native_pages（由待办中心 openWorkNode 动态打开，不进菜单）。F3。
 *
 * props（来自待办中心，见 openTaskForm）：
 *   mode:'task', formKey, formMode(approve|edit|readonly), taskId, instanceId,
 *   domain/application/module/file, bizTable, bizId, apiPath, title
 *
 * content 区：业务单据审阅区（native 表单：变量+单据只读视图；F4 接通用动态渲染）。
 *            html-pages 类表单不走本页 content——由待办中心把 content 视图直接指向 html_pages，
 *            业务表单由门户原生 hydrate 渲染；本页只提供 property 区的审批控制台。
 * property 区：审批控制台（历史意见 + 我的意见 + 同意/驳回/办结）+ 流程轨迹。两类表单共用。
 *
 * 办结走 F1 已通用化的 complete（塞 lastDecision/comment），成功后广播 cmx-flow-task-done
 * 让待办中心刷新，并尝试关闭本 tab。认领/转签由待办中心侧完成，此页专注办理。
 *
 * F3 首版：业务单据区用「变量 + 单据行的只读 JSON 视图」兜底（不硬耦合 doc-loader 的 meta 渲染）；
 * F4 再接通用动态字段渲染。formMode=edit 的可编辑收紧也留 F4。
 */

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

// —— S4 抽核：配置接缝 ——（详见 todo-center.js 同名 CFG 注释）
// 门户壳用默认值 = 今天行为；组件壳/headless 壳 configure({...}) 覆盖 apiBase/authHeaders/getUser +
// onTaskDone（办结后通知，门户=派发 cmx-flow-task-done 让待办中心刷新）+ onClose（关闭当前视图，
// 门户=探测 closeTab 关工作区 Tab，组件壳=派发事件让宿主收起面板）。
const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
  onTaskDone: (detail) => {                   // 办结/发起成功后广播；门户默认派发全局事件
    try {
      const ev = new CustomEvent('cmx-flow-task-done', { detail: detail || {} })
      window.dispatchEvent(ev); window.top?.dispatchEvent?.(ev)
    } catch {}
  },
  onClose: (nodeId) => {                       // 关闭当前任务视图；门户默认探测工作区 Tab 关闭链
    const targets = [window, window.parent, window.top, globalThis].filter(Boolean)
    for (const t of targets) {
      try {
        if (typeof t.closeTab === 'function') { t.closeTab(nodeId); return }
        if (typeof t.closeWorkspaceNode === 'function') { t.closeWorkspaceNode(nodeId); return }
      } catch {}
    }
    try {
      window.top?.postMessage({ type: 'closeTab', payload: { id: nodeId } }, '*')
      document.dispatchEvent(new CustomEvent('cmx-close-workspace-node', { detail: { id: nodeId }, bubbles: true, composed: true }))
    } catch {}
  },
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

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

function propsOf (ctx) {
  const p = (ctx && ctx.props) || (ctx && ctx.host && ctx.host.__props) || {}
  return {
    mode: p.mode || 'task',
    formKey: p.formKey || '',
    formMode: p.formMode || 'approve',
    viewOnly: !!p.viewOnly,
    taskId: p.taskId || '',
    instanceId: p.instanceId || '',
    businessKey: p.businessKey || '',
    nodeBpmnId: p.nodeBpmnId || '',
    nodeName: p.nodeName || '',
    taskCreatedAt: p.taskCreatedAt || p.createdAt || '',
    bizTable: p.bizTable || '',
    bizId: p.bizId || '',
    domain: p.domain || '', application: p.application || '', module: p.module || '', file: p.file || '',
    apiPath: p.apiPath || '',
    // 发起态（mode:'start'）字段
    definitionKey: p.definitionKey || '', definitionName: p.definitionName || '', startFormKey: p.startFormKey || '',
    consoleMode: p.consoleMode || 'platform',
    title: p.title || '任务表单',
  }
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

// 每个 (instanceId,taskId) 一份独立状态（两条待办可并排打开）。
const instances = {}
function stOf (p) {
  const key = `${p.instanceId}@@${p.taskId}`
  if (!instances[key]) {
    instances[key] = {
      props: p,
      inst: null,
      definition: null,
      biz: null,
      comments: [],
      loading: false,
      loadError: '',
      busy: false,
      busyAction: '',
      submitted: false,
      actionError: '',
      draft: { comment: '' },
      startDraft: {},
      activeTab: 'handle',
      menuOpen: false,
      pendingAction: '',
      returnTargets: null,
      returnLoading: false,
      returnError: '',
      returnTarget: '',
      returnTargetName: '',
      hosts: new Set(),
    }
  }
  instances[key].props = p
  return instances[key]
}

function formatLocalDateTime (iso) {
  if (!iso) return ''
  // 后端时间统一为 RFC3339 UTC；展示层转浏览器本地时区，避免把 05:56Z 直接当本地 05:56。
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return String(iso).replace('T', ' ').slice(0, 16)
  const p = (n) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}
const fmtTime = formatLocalDateTime

function displayActor (row) {
  const id = String(row?.userId || row?.user_id || row?.assignee || '')
  const cached = id ? globalThis.__cmxFlowUsers?.[id] : null
  return row?.nickName || row?.userName || row?.nickname || row?.username || cached?.nickName || cached?.userName || row?.userId || row?.user_id || row?.assignee || '—'
}

const APPROVAL_ACTION_LABELS = {
  approve: '同意',
  reject: '驳回',
  return: '退回',
  submit: '制单',
  complete: '办结',
  transfer: '转签',
  withdraw: '取回',
  cancel: '撤销',
}

// 两个 native page 会分别加载一份模块；版本一致时复用先加载的纯视图模型，避免待办中心 / 办理页算法漂移。
const APPROVAL_VIEW_MODEL_VERSION = '20260902-allnodes-4'

// 轨迹轴收录的节点种类：业务语义节点全收（发起/审批/子流程/自动任务）——
// 定义接口返回全量 nodes，但轴上只呈现这些；网关与边界/消息/定时事件是路由管道，不进时间轴。
// 结束节点也不进：不少定义把审批结果建成名为「已通过/已驳回」的 endEvent，当步骤显示会误读成待办，
// 流程是否走完由单据状态表达。
const TRAIL_NODE_KINDS = new Set([
  'startEvent', 'userTask', 'serviceTask', 'businessRuleTask',
  'callActivity', 'subProcess',
])
// 非 userTask 节点的种类徽标文案（渲染在步骤名旁，说明该步为何无办理人/意见）。
const TRAIL_KIND_LABELS = {
  startEvent: '发起', serviceTask: '自动', businessRuleTask: '规则',
  callActivity: '子流程', subProcess: '子流程',
}

function normalizeApprovalAction (row, node) {
  const raw = String(row?.decision ?? '').trim()
  const key = raw.toLowerCase()
  const id = String(node?.id || row?.nodeBpmnId || '').toLowerCase()
  if (key && APPROVAL_ACTION_LABELS[key]) {
    return { key, label: APPROVAL_ACTION_LABELS[key], tone: key === 'reject' ? 'rej' : (key === 'return' || key === 'withdraw' ? 'warn' : 'ok') }
  }
  if (key) return { key, label: raw, tone: 'ok' }
  const isCreation = id === 'apply' || id === 'start' || String(node?.name || '').includes('发起')
  return {
    key: isCreation ? 'submit' : 'complete',
    label: isCreation ? '制单' : '办理',
    tone: isCreation ? 'ok' : 'ok',
  }
}

function normalizeApprovalComment (row, node) {
  const action = normalizeApprovalAction(row, node)
  const text = String(row?.comment ?? '').trim()
  return {
    id: String(row?.id || row?.taskId || `${row?.nodeBpmnId || 'unknown'}-${row?.createdAt || row?.userId || Math.random()}`),
    nodeId: String(row?.nodeBpmnId || ''),
    taskId: String(row?.taskId || ''),
    actor: displayActor(row),
    action,
    text: text || (action.key === 'submit' ? '制单提交' : '（未填写意见）'),
    createdAt: row?.createdAt || '',
    time: formatLocalDateTime(row?.createdAt),
    raw: row,
  }
}

function normalizeDefinition (definition) {
  if (!definition || Array.isArray(definition)) return definition || null
  return definition
}

function approvalGraphHasBranch (definition) {
  const edges = Array.isArray(definition?.edges) ? definition.edges : []
  const out = new Map()
  for (const e of edges) {
    const k = String(e?.from || '')
    out.set(k, (out.get(k) || 0) + 1)
  }
  return Array.from(out.values()).some((n) => n > 1)
}

function buildApprovalSteps ({ instance, definition, comments, task }) {
  const inst = instance || {}
  const def = normalizeDefinition(definition)
  // 令牌里 ENDED 是历史终点（如停在结束节点的令牌），不算「当前」——否则已完实例的结束节点会显示成当前。
  const activeIds = new Set([
    ...((inst.tokens || []).filter((x) => String(x?.state || '') !== 'ENDED').map((x) => String(x.nodeBpmnId || ''))),
    ...((inst.activeNodes || []).map((x) => String(x || ''))),
  ].filter(Boolean))
  const tasks = inst.tasks || []
  const completedIds = new Set(tasks.filter((x) => x.completed).map((x) => String(x.nodeBpmnId || '')).filter(Boolean))
  // 只把真实执行证据（活动令牌 / 任务）补进节点轴；意见找不到节点时保留 unmatched，不伪造成步骤。
  const observedIds = new Set([...activeIds, ...completedIds])
  const hasBranch = approvalGraphHasBranch(def)

  let nodes = []
  if (Array.isArray(def?.nodes) && def.nodes.length) {
    nodes = def.nodes
      .filter((n) => TRAIL_NODE_KINDS.has(String(n?.kind || '')) || observedIds.has(String(n?.id || '')))
      .map((n) => {
        const kind = String(n?.kind || '')
        return { ...n, id: String(n.id || ''), kind, name: n.name || (kind === 'startEvent' ? '发起' : '') || n.id, source: 'definition' }
      })
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

  const normalizedComments = (comments || []).map((c) => normalizeApprovalComment(c, nodes.find((n) => n.id === String(c.nodeBpmnId || ''))))
    .sort((a, b) => String(a.createdAt).localeCompare(String(b.createdAt)))
  const mismatch = normalizedComments.some((c) => c.nodeId && !knownIds.has(c.nodeId))

  // 证据集合：当前有令牌或留有已完成任务的节点。子流程/自动任务执行后不在父实例留任务痕迹，
  // 需要靠「下游已有证据」推断其已流转。
  const adj = new Map()
  for (const e of (Array.isArray(def?.edges) ? def.edges : [])) {
    const from = String(e?.from || ''); const to = String(e?.to || '')
    if (!from || !to) continue
    if (!adj.has(from)) adj.set(from, [])
    adj.get(from).push(to)
  }
  const reachCache = new Map()
  const reachFrom = (start) => {
    if (!reachCache.has(start)) {
      const seen = new Set(); const queue = (adj.get(start) || []).slice()
      while (queue.length) {
        const cur = queue.pop()
        if (seen.has(cur)) continue
        seen.add(cur)
        for (const nx of (adj.get(cur) || [])) queue.push(nx)
      }
      reachCache.set(start, seen)
    }
    return reachCache.get(start)
  }
  // 退回判定：节点留有旧完成记录，但开放令牌在它上游（能顺边走到它）——流程已打回重办，
  // 旧记录不算「已完成」，节点回到待处理（将重走）。正向推进时上游令牌够不到下游节点，不会误伤。
  const hasActiveUpstream = (id) => nodes.some((n) => n.id !== id && activeIds.has(n.id) && reachFrom(n.id).has(id))
  const returnedIds = new Set(nodes.filter((n) => completedIds.has(n.id) && !activeIds.has(n.id) && hasActiveUpstream(n.id)).map((n) => n.id))
  // 证据集合：当前有令牌、或留有未被打回的完成任务记录的节点。子流程/自动任务执行后不在父实例
  // 留任务痕迹，需要靠「下游已有证据」推断其已流转（退回产生的旧记录不算证据）。
  const evidenceIds = new Set(nodes.filter((n) => activeIds.has(n.id) || (completedIds.has(n.id) && !returnedIds.has(n.id))).map((n) => n.id))
  const downstreamHasEvidence = (start) => Array.from(reachFrom(start)).some((x) => evidenceIds.has(x))
  const instanceTerminal = ['COMPLETED', 'TERMINATED'].includes(String(inst.state || ''))
  const firstCommentAt = normalizedComments[0]?.createdAt || ''

  const steps = nodes.map((node, index) => {
    const isStart = node.kind === 'startEvent'
    const current = activeIds.has(node.id)
    // 无任务痕迹节点的状态推定：起点随实例必然已过；
    // 子流程/自动任务仅线性流按下游证据推「已完成」——分支流里旁支会被下游汇合点误照亮，不冒认已完成。
    // 未到达节点：实例还在跑 → 待处理；实例已完结/终止 → 未经过（分支跳过/退回未重走的节点如实呈现）。
    const done = !current && (
      (completedIds.has(node.id) && !returnedIds.has(node.id))
      || isStart
      || (!isStart && !hasBranch && downstreamHasEvidence(node.id))
    )
    const nodeComments = normalizedComments.filter((c) => c.nodeId === node.id)
    const nodeTasks = tasks.filter((x) => String(x.nodeBpmnId || '') === node.id)
    const actors = Array.from(new Set(nodeTasks.map((x) => x.assignee || x.ownerUserId).filter(Boolean)))
    const isSelectedTaskNode = current && String(task?.nodeBpmnId || '') === node.id
    const latestComment = nodeComments[nodeComments.length - 1] || null
    const stepTime = current
      ? (isSelectedTaskNode ? (task?.createdAt || task?.taskCreatedAt || '') : (latestComment?.createdAt || ''))
      : (done ? (latestComment?.createdAt || (isStart ? firstCommentAt : '')) : '')
    return {
      id: node.id,
      name: node.name || node.id,
      index,
      status: current ? 'current' : (done ? 'done' : 'pending'),
      statusText: current ? '当前' : (done ? (isStart ? '已发起' : '已完成') : (instanceTerminal ? '未经过' : '待处理')),
      time: stepTime,
      timeText: formatLocalDateTime(stepTime),
      timeLabel: current ? '到达时间' : (done ? '完成时间' : ''),
      kindLabel: TRAIL_KIND_LABELS[node.kind] || '',
      calledElement: String(node.calledElement || ''),
      actors,
      comments: nodeComments,
      source: node.source,
    }
  })
  const unmatchedActions = normalizedComments.filter((c) => !c.nodeId || !knownIds.has(c.nodeId))
  return { steps, unmatchedActions, definitionMismatch: mismatch }
}

function buildApprovalViewModel ({ instance, definition, comments, task }) {
  const inst = instance || {}
  const def = normalizeDefinition(definition)
  const built = buildApprovalSteps({ instance: inst, definition: def, comments, task })
  const allActions = [
    ...built.steps.flatMap((s) => s.comments),
    ...built.unmatchedActions,
  ].sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)))
  const current = built.steps.find((s) => s.status === 'current')
  return {
    meta: {
      businessKey: inst.businessKey || task?.businessKey || inst.id || task?.instanceId || '',
      definitionName: def?.name || task?.definitionName || inst.definitionKey || task?.definitionKey || '',
      instanceState: inst.state || task?.state || '',
      currentNode: current?.name || task?.nodeName || task?.currentNode || '',
    },
    ...built,
    latestAction: allActions[0] || null,
  }
}

// 阶段一先在两个 native 页之间共享纯视图模型；后续迁入门户运行时时替换此注册点。
try {
  const existing = globalThis.__cmxFlowApprovalView
  globalThis.__cmxFlowApprovalView = existing?.version === APPROVAL_VIEW_MODEL_VERSION
    ? existing
    : { version: APPROVAL_VIEW_MODEL_VERSION, buildApprovalViewModel }
} catch {}

// ── 流程轨迹组件（已上收 cmx-data-comp）────────────────────────────────────
// 20260903 上收：组件定义移至 packages/cmx-data-comp/src/components/cmx-flow-trail.js，
// barrel 全局注册一次（native 页 Blob 模块不能 import 共享代码，故归库而非页面内联副本）；
// 本页只写 <cmx-flow-trail> 标签 + bind() 里回填 el.trail，办理人用户快照由组件内部兜底拉取。

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const p = propsOf(ctx)
  const st = stOf(p)
  const host = ctx.host
  st.hosts.add(host)
  if (host) { host.__tfView = view; host.__tfKey = `${p.instanceId}@@${p.taskId}` }
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(st, view)}`
    bind(root, st, view, host)
    restoreDraft(st, root)
  }
  // 首帧引导双路竞速：rAF 在被遮挡/后台的窗口里会被浏览器无限暂停（嵌入面板实测永不触发），
  // 数据加载不能单押帧回调——rAF 与 setTimeout(0) 谁先跑谁置位，另一方跳过，保证 loadAll 必达。
  let booted = false
  const boot = async () => {
    if (booted) return
    booted = true
    render(); await loadAll(st); refreshAll(st)
  }
  requestAnimationFrame(() => { boot() })
  setTimeout(() => { boot() }, 0)
  return `<style>${styleCss()}</style>${viewHtml(st, view)}`
}

function refreshAll (st) {
  captureDraft(st)
  for (const host of Array.from(st.hosts)) {
    if (!host || !host.isConnected) { st.hosts.delete(host); continue }
    const root = hostRoot(host)
    if (!root) continue
    const view = host.__tfView
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(st, view)}`
    bind(root, st, view, host)
    restoreDraft(st, root)
  }
}

function viewHtml (st, view) {
  if (st.props.mode === 'start') {
    return view === 'property' ? startPropHtml(st) : startContentHtml(st)
  }
  if (view === 'property') return trailHtml(st)
  return contentHtml(st)
}

// ————————————————————— 数据装载 —————————————————————

const definitionCache = {}
async function loadDefinition (definitionKey) {
  if (!definitionKey) return null
  if (definitionCache[definitionKey]) return definitionCache[definitionKey]
  try {
    const d = await apiJson('/api/flow/definitions')
    for (const item of (d?.definitions || [])) {
      definitionCache[item.key] = item
    }
  } catch { /* 定义不可用时按实例已发生路径降级 */ }
  return definitionCache[definitionKey] || null
}

async function loadAll (st) {
  const p = st.props
  if (p.mode === 'start') { st.inst = null; st.definition = null; st.comments = []; return }
  st.loading = true
  st.loadError = ''
  refreshAll(st)
  const requests = []
  const instanceIndex = p.instanceId ? requests.push(apiJson(`/api/flow/instances/${enc(p.instanceId)}`).then((x) => x).catch(() => null)) - 1 : requests.push(Promise.resolve(null)) - 1
  const commentsIndex = p.instanceId ? requests.push(apiJson(`/api/flow/instances/${enc(p.instanceId)}/comments`).catch(() => null)) - 1 : requests.push(Promise.resolve(null)) - 1
  const values = await Promise.all([...requests, loadUserSnapshots()])
  const inst = values[instanceIndex]
  const commentEnvelope = values[commentsIndex]
  if (inst && inst.id) {
    st.inst = inst
    st.comments = Array.isArray(commentEnvelope?.comments) ? commentEnvelope.comments : []
    st.definition = await loadDefinition(inst.definitionKey || p.definitionKey)
  } else {
    st.inst = null
    st.comments = []
    st.definition = null
    st.loadError = '流程实例或审批意见加载失败，请刷新重试'
  }
  st.loading = false
  refreshAll(st)
  // 业务单据：F3 首版用变量投影 + 关联单据坐标兜底展示。真正拉 cf_* 行留 F4 接通用渲染。
  st.biz = null
}

function captureDraft (st) {
  for (const host of Array.from(st.hosts)) {
    const root = hostRoot(host)
    if (!root) continue
    const comment = root.querySelector?.('[data-comment]')
    if (comment) st.draft.comment = comment.value || ''
    const target = root.querySelector?.('[data-return-target]:checked')
    if (target) st.returnTarget = target.value || ''
    const startDraft = { ...(st.startDraft || {}) }
    root.querySelectorAll?.('[data-sf]').forEach((el) => {
      const key = el.getAttribute('data-sf')
      if (key) startDraft[key] = el.value
    })
    st.startDraft = startDraft
}
}

function restoreDraft (st, root) {
  const comment = root.querySelector?.('[data-comment]')
  if (comment) comment.value = st.draft.comment || ''
}

// ————————————————————— 发起态（mode:'start'）—————————————————————
// 起点表单：F4 首版用「通用字段录入」（单号/申请人/金额/摘要 + 自定义键值），提交=建单据引用 + 起流程。
// html-pages 起点表单则由门户原生渲染业务表单（content 走 html_pages），发起动作在 property。

function startContentHtml (st) {
  const p = st.props
  return `<section class="tf">
    <div class="tf-bar"><div class="tf-bar-main"><b>${esc(p.title)}</b><span>${esc(p.definitionKey)}${p.startFormKey ? ' · ' + esc(p.startFormKey) : ''}</span></div>
      <span class="tf-mode edit">发起</span></div>
    <div class="tf-body">
      <div class="tf-panel">
        <div class="tf-panel-head"><ui5-icon name="add-document"></ui5-icon> 新建单据</div>
        <div class="tf-kvs">
          ${startField(st, 'businessKey', '单号', '如 PAY-2026-0001')}
          ${startField(st, 'applicant', '申请人', '用户 id')}
          ${startField(st, 'amount', '金额', '数字', 'number')}
          ${startField(st, 'bizId', '单据主键', '业务单据 id')}
        </div>
        <label class="tf-label" style="margin-top:12px">摘要</label>
        <textarea class="tf-comment" data-sf="summary" placeholder="单据摘要（可空）">${esc(st.startDraft?.summary || '')}</textarea>
        <div class="tf-hint" style="margin-top:8px">提交后按此建立单据引用并发起流程；重数据在业务表，此处只填驱动流转的关键字段。</div>
      </div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}
function startField (st, key, label, ph, type) {
  return `<div class="tf-kv"><span>${esc(label)}</span><input data-sf="${esc(key)}" type="${type || 'text'}" value="${esc(st.startDraft?.[key] || '')}" placeholder="${esc(ph)}" style="width:100%;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:6px;padding:5px 8px;margin-top:3px"></div>`
}

function startPropHtml (st) {
  const p = st.props
  return `<section class="tf">
    <div class="tf-prop-head"><b>发起流程</b><small>${esc(p.definitionName || p.definitionKey)}</small></div>
    <div class="tf-prop-body">
      <div class="tf-psec">流程</div>
      <div class="tf-kv"><span>定义</span><b>${esc(p.definitionName || p.definitionKey)}</b></div>
      <div class="tf-kv" style="margin-top:8px"><span>起点表单</span><b>${esc(p.startFormKey || '（无）')}</b></div>
      <div class="tf-psec">操作</div>
      <div class="tf-actions"><button class="tf-btn ok" data-start-submit ${st.busy ? 'disabled' : ''}>${st.busy ? '提交中…' : '提交并发起'}</button></div>
      <div class="tf-hint" style="margin-top:8px">从 content 表单收集字段作为流程变量，起实例后令牌进入首个环节。</div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}

// 收集 content 起点表单字段（跨所有 host 找 data-sf）。
function collectStartFields (st) {
  const out = {}
  for (const h of Array.from(st.hosts)) {
    const root = hostRoot(h)
    if (!root) continue
    root.querySelectorAll?.('[data-sf]').forEach((el) => {
      const k = el.getAttribute('data-sf')
      let v = el.value
      if (el.type === 'number' && v !== '') v = Number(v)
      if (v !== '' && v != null) out[k] = v
    })
  }
  return out
}

async function submitStart (st, host) {
  if (st.busy) return
  const p = st.props
  const fields = collectStartFields(st)
  st.startDraft = { ...fields }
  st.busy = true
  refreshAll(st)
  try {
    const bizId = fields.bizId || fields.businessKey || ('BIZ-' + (fields.applicant || 'x'))
    const variables = { ...fields, bizTable: p.bizTable || '', bizId }
    delete variables.businessKey
    const body = {
      definitionKey: p.definitionKey,
      businessKey: fields.businessKey || null,
      variables,
    }
    if (p.bizTable) body.bizLink = { bizTable: p.bizTable, bizId }
    const r = await apiJson('/api/flow/instances', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    showCmxToast('已发起流程实例 ' + (r.id ? String(r.id).slice(0, 8) : ''))
    CFG.onTaskDone({ started: true, definitionKey: p.definitionKey })
    setTimeout(() => closeSelf({ instanceId: 'start', taskId: p.definitionKey }), 600)
  } catch (e) {
    st.busy = false
    refreshAll(st)
    showCmxToast('发起失败: ' + e.message)
  }
}

// ————————————————————— content 区 —————————————————————

function contentHtml (st) {
  const p = st.props
  const vars = st.inst?.variables || {}
  const readOnly = p.formMode === 'readonly'
  const bizRows = Object.entries(vars)
    .filter(([k]) => !['bizTable', 'bizId'].includes(k))
    .map(([k, v]) => `<div class="tf-kv"><span>${esc(k)}</span><b>${esc(typeof v === 'object' ? JSON.stringify(v) : v)}</b></div>`)
    .join('') || (st.loading ? '' : '<div class="tf-hint">无业务变量</div>')
  const bizRef = p.bizTable ? `<div class="tf-bizref"><ui5-icon name="document"></ui5-icon> ${esc(p.bizTable)} / ${esc(p.bizId)}</div>` : ''
  const consoleNote = p.consoleMode === 'none'
    ? '本表单绑定声明由业务表单自带审批动作，右侧仅提供只读流程轨迹。'
    : '审批意见与办结动作统一在右侧「审批」区处理，避免同一动作出现两个入口。'
  return `<section class="tf">
    <div class="tf-bar">
      <div class="tf-bar-main"><b>${esc(p.title)}</b><span>${esc(p.formKey || '无绑定表单')} · ${esc(modeLabel(p.formMode))}</span></div>
      <span class="tf-mode ${p.formMode}">${esc(modeLabel(p.formMode))}</span>
    </div>
    <div class="tf-body">
      <div class="tf-panel">
        <div class="tf-panel-head"><ui5-icon name="document-text"></ui5-icon> 业务单据${readOnly ? '（只读审阅）' : ''}</div>
        ${bizRef}
        <div class="tf-kvs">${st.loading ? '<div class="tf-hint">加载单据变量…</div>' : bizRows}</div>
        ${st.loadError ? `<div class="tf-inline-error">${esc(st.loadError)}</div>` : ''}
      </div>
      <div class="tf-panel-note"><ui5-icon name="information"></ui5-icon> ${esc(consoleNote)}</div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}

function approvalViewModelOf (st) {
  const shared = globalThis.__cmxFlowApprovalView?.buildApprovalViewModel
  const input = {
    instance: st.inst,
    definition: st.definition,
    comments: st.comments,
    task: {
      businessKey: st.props.businessKey,
      definitionName: st.props.definitionName,
      definitionKey: st.props.definitionKey,
      instanceId: st.props.instanceId,
      nodeBpmnId: st.props.nodeBpmnId,
      nodeName: st.props.nodeName,
      currentNode: st.props.nodeName,
      createdAt: st.props.taskCreatedAt || '',
      state: st.inst?.state,
    },
  }
  if (typeof shared === 'function' && globalThis.__cmxFlowApprovalView?.version === APPROVAL_VIEW_MODEL_VERSION && shared !== buildApprovalViewModel) return shared(input)
  return buildApprovalViewModel(input)
}

function instanceStateText (state) {
  return ({ ACTIVE: '进行中', COMPLETED: '已完成', TERMINATED: '已终止' })[state] || state || '—'
}

function approvalSummaryHtml (vm) {
  const rows = [
    ['流程', vm.meta.definitionName || '—'],
    ['单据', vm.meta.businessKey || '—'],
    ['当前节点', vm.meta.currentNode || '—'],
    ['实例状态', instanceStateText(vm.meta.instanceState)],
  ]
  return `<div class="tf-summary">${rows.map(([k, v]) => `<div class="tf-summary-item"><span>${esc(k)}</span><b>${esc(v)}</b></div>`).join('')}</div>`
}

// 轨迹渲染用 cmx-data-comp 组件库的 <cmx-flow-trail>（事件流口径，组件真源见库）。

function approvalActionLabel (st) {
  if (st.busy) return '提交中…'
  if (st.props.formMode === 'readonly') return '确认知悉（办结）'
  if (st.pendingAction === 'reject') return '确认驳回'
  if (st.pendingAction === 'return') return '确认退回'
  if (st.pendingAction === 'return-pick') return `退回至：${returnTargetName(st) || '未选择'}`
  return '同意'
}

function returnTargetName (st) {
  return (st.returnTargets || []).find((x) => x.bpmnId === st.returnTarget)?.name || ''
}

function approveAreaHtml (st) {
  const p = st.props
  if (p.viewOnly || st.submitted) return ''
  if (p.formMode === 'readonly') {
    return `<div class="tf-actions"><button class="tf-btn ok" data-submit data-action="approve" ${st.busy ? 'disabled' : ''}>${esc(approvalActionLabel(st))}</button></div>
      ${st.actionError ? `<div class="tf-inline-error">${esc(st.actionError)}</div>` : ''}`
  }
  const danger = st.pendingAction === 'reject' || st.pendingAction === 'return' || st.pendingAction === 'return-pick'
  const actionHint = st.pendingAction === 'reject'
    ? '驳回会记录本环节否定意见，请确认后提交。'
    : (st.pendingAction ? '退回后任务会回到目标节点重新办理。' : '同意后流程进入下一节点；意见可留空。')
  const required = st.pendingAction === 'reject' || st.pendingAction === 'return' || st.pendingAction === 'return-pick'
  return `<label class="tf-label" for="tf-comment">${required ? '<i class="tf-required" aria-hidden="true">*</i>' : ''}我的意见${required ? '<span>（必填）</span>' : ''}</label>
    <textarea id="tf-comment" class="tf-comment" data-comment placeholder="${required ? '填写审批意见（必填）...' : '填写审批意见...'}">${esc(st.draft.comment || '')}</textarea>
    <div class="tf-action-hint">${esc(actionHint)}</div>
    <div class="tf-actions">
      <button class="tf-btn ${danger ? 'danger solid' : 'ok'}" data-submit data-action="${esc(st.pendingAction || 'approve')}" ${st.busy ? 'disabled' : ''}>${esc(approvalActionLabel(st))}</button>
      ${st.pendingAction ? `<button class="tf-btn" data-cancel-action ${st.busy ? 'disabled' : ''}>取消</button>` : `<button class="tf-btn more" data-action-menu aria-haspopup="menu" aria-expanded="${st.menuOpen ? 'true' : 'false'}" ${st.busy ? 'disabled' : ''}>更多 <ui5-icon name="slim-arrow-down"></ui5-icon></button>`}
    </div>
    ${st.menuOpen ? `<div class="tf-action-menu" role="menu">
      <button role="menuitem" data-choose-action="reject">驳回</button>
      <button role="menuitem" data-choose-action="return">退回上一步</button>
      <button role="menuitem" data-choose-action="return-pick">退回到指定节点</button>
    </div>` : ''}
    ${st.pendingAction === 'return-pick' ? returnPickerHtml(st) : ''}
    ${st.actionError ? `<div class="tf-inline-error">${esc(st.actionError)}</div>` : ''}`
}

function returnPickerHtml (st) {
  if (st.returnLoading) return '<div class="tf-return-menu"><div class="tf-hint">加载可退节点…</div></div>'
  if (st.returnError) return `<div class="tf-return-menu"><div class="tf-inline-error">${esc(st.returnError)}</div><button class="tf-btn" data-reload-return>重试</button></div>`
  const targets = st.returnTargets || []
  if (!targets.length) return '<div class="tf-return-menu"><div class="tf-hint">无可退节点（会签任务或已是首环节）</div></div>'
  return `<div class="tf-return-menu" role="radiogroup" aria-label="可退节点">
    <div class="tf-return-head"><b>退回目标</b><small>任务将回到所选节点重新办理</small></div>
    ${targets.map((t) => `<label class="tf-return-option"><input type="radio" name="tf-return-target" data-return-target value="${esc(t.bpmnId)}" ${st.returnTarget === t.bpmnId ? 'checked' : ''}>
      <span><b>${esc(t.name || t.bpmnId)}</b><i>${t.isDirectPredecessor ? '直接前驱' : (t.distance ? `上${t.distance}步` : '上游节点')}</i></span></label>`).join('')}
  </div>`
}

function modeLabel (m) {
  return m === 'edit' ? '可编辑' : (m === 'readonly' ? '只读' : '审批')
}

// ————————————————————— property 区（审批控制台 + 轨迹） —————————————————————
// 审批控制台放 property，让 native 与 html-pages 两类表单共用：html 表单的 content 由门户
// 原生渲染业务表单，审批动作统一走这里的 property 控制台。

function trailHtml (st) {
  const p = st.props
  const vm = approvalViewModelOf(st)
  const canHandle = !p.viewOnly && !st.submitted
  const activeTab = canHandle ? st.activeTab : 'trail'
  const tabs = canHandle
    ? `<div class="tf-tabs" role="tablist" aria-label="审批面板">
        <button role="tab" data-tab="handle" aria-selected="${activeTab === 'handle' ? 'true' : 'false'}" class="${activeTab === 'handle' ? 'active' : ''}">办理</button>
        <button role="tab" data-tab="trail" aria-selected="${activeTab === 'trail' ? 'true' : 'false'}" class="${activeTab === 'trail' ? 'active' : ''}">流转</button>
      </div>`
    : ''
  let body = ''
  if (st.loading) {
    body = '<div class="tf-skeleton-list"><span></span><span></span><span></span></div>'
  } else if (st.loadError && !vm.steps.length) {
    body = `<div class="tf-inline-error">${esc(st.loadError)}</div><div class="tf-actions"><button class="tf-btn" data-reload>重新加载</button></div>`
  } else if (activeTab === 'handle') {
    body = `<div class="tf-current"><span>当前节点</span><b>${esc(vm.meta.currentNode || '—')}</b>${vm.latestAction ? `<small>最新动作：${esc(vm.latestAction.actor)} · ${esc(vm.latestAction.action.label)} · ${esc(vm.latestAction.time)}</small>` : ''}</div>${approveAreaHtml(st)}`
  } else {
    body = '<cmx-flow-trail></cmx-flow-trail>'
  }
  const result = st.submitted
    ? `<div class="tf-result ok"><ui5-icon name="status-positive"></ui5-icon><span>${esc(st.resultMessage || '操作已提交')}</span><button class="tf-btn" data-close-task>关闭</button></div>`
    : ''
  return `<section class="tf">
    <div class="tf-prop-head"><b>${esc(vm.meta.businessKey || p.instanceId)}</b><small>${esc(vm.meta.definitionName || p.formKey || '')} · ${esc(instanceStateText(vm.meta.instanceState))}</small></div>
    <div class="tf-prop-body">
      ${approvalSummaryHtml(vm)}
      ${tabs}
      ${body}
      ${result}
    </div>
    <div class="tf-toast"></div>
  </section>`
}

// ————————————————————— 事件 + 办结 —————————————————————

function bind (root, st, view, host) {
  // 流转页签轨迹组件数据回填（cmx-data-comp 的 <cmx-flow-trail>，事件流口径）。
  root.querySelectorAll('cmx-flow-trail').forEach((el) => {
    el.trail = { instance: st.inst, definition: st.definition, comments: st.comments }
  })
  root.querySelector('[data-start-submit]')?.addEventListener('click', () => submitStart(st, host))
  root.querySelectorAll('[data-tab]').forEach((b) => b.addEventListener('click', () => {
    captureDraft(st)
    st.activeTab = b.dataset.tab
    refreshAll(st)
  }))
  root.querySelector('[data-action-menu]')?.addEventListener('click', () => {
    st.menuOpen = !st.menuOpen
    refreshAll(st)
  })
  root.querySelectorAll('[data-choose-action]').forEach((b) => b.addEventListener('click', () => {
    captureDraft(st)
    st.menuOpen = false
    st.pendingAction = b.dataset.chooseAction
    st.actionError = ''
    if (st.pendingAction === 'return-pick') openReturnPicker(st, false)
    else refreshAll(st)
  }))
  root.querySelector('[data-cancel-action]')?.addEventListener('click', () => {
    captureDraft(st)
    st.pendingAction = ''
    st.actionError = ''
    st.returnTarget = ''
    refreshAll(st)
  })
  root.querySelector('[data-reload-return]')?.addEventListener('click', () => openReturnPicker(st, true))
  root.querySelector('[data-reload]')?.addEventListener('click', () => loadAll(st))
  root.querySelector('[data-close-task]')?.addEventListener('click', () => closeSelf(st.props))
  root.querySelector('[data-submit]')?.addEventListener('click', (e) => submitApproval(st, e.currentTarget.dataset.action))
  root.querySelector('[data-comment]')?.addEventListener('input', (e) => { st.draft.comment = e.target.value || ''; st.actionError = '' })
  root.querySelectorAll('[data-return-target]').forEach((r) => r.addEventListener('change', () => {
    st.returnTarget = r.value || ''
    st.returnTargetName = returnTargetName(st)
    st.actionError = ''
  }))
}

const { showCmxToast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

function validateApproval (st, action) {
  const comment = String(st.draft.comment || '').trim()
  if ((action === 'reject' || action === 'return') && !comment) return '驳回 / 退回必须填写审批意见'
  if (action === 'return-pick' && !st.returnTarget) return '请选择退回目标节点'
  return ''
}

async function submitApproval (st, action) {
  if (st.busy || st.submitted) return
  captureDraft(st)
  const kind = action || st.pendingAction || 'approve'
  const error = validateApproval(st, kind)
  if (error) {
    st.actionError = error
    refreshAll(st)
    return
  }
  const p = st.props
  const comment = String(st.draft.comment || '').trim()
  st.busy = true
  st.busyAction = kind
  st.actionError = ''
  refreshAll(st)
  try {
    if (kind === 'approve' || kind === 'reject') {
      await apiJson(`/api/flow/tasks/${enc(p.taskId)}/complete`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ instanceId: p.instanceId, decision: kind, comment: comment || null }),
      })
      st.resultMessage = kind === 'approve' ? '已同意办结' : '已驳回'
    } else {
      const target = kind === 'return-pick' ? st.returnTarget : ''
      await apiJson(`/api/flow/tasks/${enc(p.taskId)}/reject`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          instanceId: p.instanceId,
          reason: comment || (target ? '退回到指定节点' : '退回上一步'),
          ...(target ? { targetBpmnId: target } : {}),
        }),
      })
      st.resultMessage = target ? '已退回到指定节点' : '已退回上一步'
    }
    st.busy = false
    st.busyAction = ''
    st.submitted = true
    refreshAll(st)
    showCmxToast(st.resultMessage)
    CFG.onTaskDone({ taskId: p.taskId, instanceId: p.instanceId })
    setTimeout(() => closeSelf(p), 1200)
  } catch (e) {
    st.busy = false
    st.busyAction = ''
    st.actionError = `${kind === 'approve' || kind === 'reject' ? '办结失败' : '退回失败'}：${e.message}`
    refreshAll(st)
    showCmxToast(st.actionError)
  }
}

async function openReturnPicker (st, reload) {
  const p = st.props
  if (!reload && Array.isArray(st.returnTargets)) { refreshAll(st); return }
  st.returnLoading = true
  st.returnError = ''
  refreshAll(st)
  try {
    const r = await apiJson(`/api/flow/tasks/${enc(p.taskId)}/reject-targets?instanceId=${enc(p.instanceId)}`)
    const targets = (r && r.targets) || []
    st.returnTargets = r?.rejectable ? targets : []
    st.returnTarget = st.returnTargets.find((x) => x.isDirectPredecessor)?.bpmnId || st.returnTargets[0]?.bpmnId || ''
    st.returnTargetName = returnTargetName(st)
    if (!r?.rejectable) st.returnError = '无可退节点（会签任务或已是首环节）'
  } catch (e) {
    st.returnTargets = []
    st.returnTarget = ''
    st.returnError = `加载失败：${e.message}`
  }
  st.returnLoading = false
  refreshAll(st)
}

// 关闭当前任务视图。nodeId 派生不变（门户工作区 Tab 命名规则），关闭动作委托 CFG.onClose：
// 门户壳默认探测 closeTab/closeWorkspaceNode/postMessage 关工作区 Tab；组件壳派发事件让宿主收面板。
function closeSelf (p) {
  const nodeId = `flow-task-${String(p.instanceId + '-' + p.taskId).replace(/[^A-Za-z0-9_-]+/g, '_')}`
  CFG.onClose(nodeId)
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  .tf{
    --brand:var(--sapButton_Emphasized_Background,var(--sapContent_IconColor,#0a6ed1));
    --brand-d:var(--sapSelectedColor,var(--sapContent_IconColor,var(--brand)));
    --brand-soft:color-mix(in srgb,var(--brand) 14%,transparent);
    --ink:var(--sapTextColor,#1d2d3e);
    --muted:var(--sapContent_LabelColor,#6a6d70);
    --line:var(--sapGroup_TitleBorderColor,var(--sapField_BorderColor,#d9e2ec));
    --line-soft:color-mix(in srgb,var(--line) 55%,transparent);
    --surface:var(--sapBackgroundColor,#f5f6f7);
    --tile:var(--sapTile_Background,var(--sapList_Background,#fff));
    --header:var(--sapList_HeaderBackground,var(--sapTile_Background,#f7f9fc));
    --ok:var(--sapPositiveColor,var(--sapSuccessColor,#107e3e));
    --red:var(--sapNegativeColor,var(--sapErrorColor,#e5484d));
    --warn:var(--sapCriticalColor,var(--sapWarningColor,#e9730c));
    color-scheme:light dark;
    font:13px/1.5 var(--sapFontFamily,-apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHe",sans-serif);
    color:var(--ink);background:var(--surface);height:100%;box-sizing:border-box;display:flex;flex-direction:column}
  .tf *{box-sizing:border-box}
  .tf-bar{display:flex;align-items:center;gap:10px;height:47px;flex:0 0 auto;padding:0 14px;border-bottom:1px solid var(--line);background:var(--header)}
  .tf-bar-main{min-width:0} .tf-bar-main b{font-size:14px} .tf-bar-main span{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .tf-mode{margin-left:auto;font-size:11px;font-weight:700;padding:3px 9px;border-radius:6px;background:color-mix(in srgb,var(--ink) 7%,transparent);color:var(--brand-d)}
  .tf-mode.approve{background:color-mix(in srgb,var(--brand) 12%,transparent);color:var(--brand)}
  .tf-mode.edit{background:color-mix(in srgb,var(--warn) 12%,transparent);color:var(--warn)}
  .tf-mode.readonly{background:color-mix(in srgb,var(--ink) 7%,transparent);color:var(--muted)}
  .tf-body{flex:1;min-height:0;overflow:auto;padding:14px;display:flex;flex-direction:column;gap:12px;background:var(--surface)}
  .tf-panel{border:1px solid var(--line);border-radius:10px;background:var(--tile);padding:12px 14px}
  .tf-panel-head{font-size:13px;font-weight:700;color:var(--brand-d);display:flex;align-items:center;gap:6px;margin-bottom:10px;padding-bottom:8px;border-bottom:1px solid var(--line-soft)}
  .tf-panel-head ui5-icon{width:1rem;height:1rem}
  .tf-bizref{display:inline-flex;align-items:center;gap:5px;font-size:11.5px;color:var(--muted);border:1px solid var(--line-soft);border-radius:6px;padding:3px 8px;margin-bottom:10px;font-family:ui-monospace,Menlo,monospace}
  .tf-bizref ui5-icon{width:.85rem;height:.85rem}
  .tf-kvs{display:grid;grid-template-columns:1fr 1fr;gap:8px}
  .tf-kv{border:1px solid var(--line-soft);border-radius:7px;padding:7px 10px;background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .tf-kv span{display:block;font-size:11px;color:var(--muted)} .tf-kv b{font-size:13px}
  .tf-history{max-height:220px;overflow:auto;margin-bottom:12px}
  .tf-cmt{border-radius:8px;padding:8px 10px;margin-bottom:7px;background:color-mix(in srgb,var(--ink) 3%,var(--tile));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--ink) 7%,transparent)}
  .tf-cmt-head{display:flex;flex-wrap:wrap;align-items:center;gap:5px 7px;font-size:11.5px}
  .tf-cmt-head b{min-width:0;max-width:100%;overflow-wrap:anywhere;color:var(--ink);font-weight:650}
  .tf-dec{flex:0 0 auto;font-size:10px;font-weight:700;padding:1px 6px;border-radius:999px;white-space:nowrap}
  .tf-dec.ok{color:var(--ok);background:color-mix(in srgb,var(--ok) 12%,transparent)}
  .tf-dec.warn{color:var(--warn);background:color-mix(in srgb,var(--warn) 12%,transparent)}
  .tf-dec.rej{color:var(--red);background:color-mix(in srgb,var(--red) 12%,transparent)}
  .tf-cmt-head em{margin-left:auto;flex:0 0 auto;font-style:normal;font-size:10px;color:var(--muted);white-space:nowrap}
  .tf-cmt-body{font-size:12px;line-height:1.45;margin-top:4px;color:var(--ink)}
  .tf-label{display:block;font-size:11px;font-weight:700;color:var(--muted);text-transform:uppercase;margin-bottom:5px}
  .tf-label .tf-required{color:var(--red);font-style:normal;margin-right:2px}
  .tf-label span{margin-left:4px;color:var(--muted);font-weight:600;text-transform:none}
  .tf-comment{width:100%;min-height:70px;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:8px;padding:8px 10px;resize:vertical;background:var(--tile);color:var(--ink)}
  .tf-comment:focus{outline:none;border-color:var(--brand);box-shadow:0 0 0 3px var(--brand-soft)}
  .tf-actions{display:flex;gap:9px;margin-top:12px;flex-wrap:wrap}
  .tf-return-menu{display:flex;flex-direction:column;gap:6px;margin-top:8px;padding:8px;border:1px solid var(--line-soft);border-radius:8px;background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .tf-return-item{text-align:left;font-weight:600}
  .tf-return-item em{color:var(--muted);font-style:normal;font-size:11px}
  .tf-btn{font:inherit;font-size:13px;border:1px solid var(--line);background:var(--tile);color:var(--ink);border-radius:8px;padding:8px 18px;cursor:pointer;font-weight:700}
  .tf-btn.ok{background:var(--ok);border-color:var(--ok);color:var(--sapButton_Emphasized_TextColor,var(--sapContent_ContrastTextColor,var(--sapBaseColor)))}
  .tf-btn.danger{background:var(--tile);border-color:color-mix(in srgb,var(--red) 40%,var(--line));color:var(--red)}
  .tf-btn.danger:hover{background:color-mix(in srgb,var(--red) 10%,var(--tile))}
  .tf-btn.danger.solid{background:var(--red);border-color:var(--red);color:var(--sapButton_Emphasized_TextColor,var(--sapContent_ContrastTextColor,var(--sapBaseColor)))}
  .tf-btn.danger.solid:hover{background:color-mix(in srgb,var(--red) 86%,var(--sapBaseColor))}
  .tf-btn:disabled{opacity:.58;cursor:not-allowed}
  .tf-btn.more{display:inline-flex;align-items:center;gap:4px}
  .tf-btn.more ui5-icon{width:.75rem;height:.75rem}
  .tf-action-menu{display:flex;flex-direction:column;gap:4px;min-width:172px;margin-top:8px;padding:6px;border:1px solid var(--line-soft);border-radius:9px;background:var(--tile);box-shadow:0 10px 28px color-mix(in srgb,var(--ink) 18%,transparent)}
  .tf-action-menu button{border:0;background:transparent;color:var(--ink);font:inherit;font-size:12.5px;text-align:left;padding:7px 9px;border-radius:7px;cursor:pointer}
  .tf-action-menu button:hover,.tf-action-menu button:focus-visible{background:color-mix(in srgb,var(--brand) 10%,var(--tile));outline:none}
  .tf-action-hint{margin-top:6px;font-size:11.5px;color:var(--muted)}
  .tf-inline-error,.tf-inline-warn{margin-top:9px;font-size:12px;border-radius:8px;padding:8px 10px}
  .tf-inline-error{color:var(--red);background:color-mix(in srgb,var(--red) 10%,var(--tile));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--red) 26%,transparent)}
  .tf-inline-warn{color:var(--warn);background:color-mix(in srgb,var(--warn) 10%,var(--tile));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--warn) 26%,transparent)}
  .tf-panel-note{display:flex;align-items:center;gap:7px;border:1px dashed var(--line);border-radius:9px;padding:9px 11px;color:var(--muted);font-size:12px;background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .tf-panel-note ui5-icon{width:1rem;height:1rem;flex:0 0 auto;color:var(--brand)}
  .tf-summary{display:grid;grid-template-columns:1fr 1fr;gap:7px;margin-bottom:12px}
  .tf-summary-item{min-width:0;border:1px solid var(--line-soft);border-radius:8px;padding:7px 9px;background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .tf-summary-item span{display:block;font-size:10.5px;color:var(--muted)}
  .tf-summary-item b{display:block;margin-top:2px;font-size:12px;overflow-wrap:anywhere}
  .tf-tabs{display:flex;gap:4px;margin:2px 0 12px;padding:3px;border:1px solid var(--line-soft);border-radius:9px;background:color-mix(in srgb,var(--ink) 4%,var(--tile))}
  .tf-tabs button{flex:1;border:0;background:transparent;color:var(--muted);font:inherit;font-size:12.5px;font-weight:700;padding:7px 10px;border-radius:7px;cursor:pointer}
  .tf-tabs button.active{color:var(--brand);background:var(--tile);box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--brand) 28%,transparent)}
  .tf-current{border:1px solid color-mix(in srgb,var(--brand) 24%,var(--line));border-radius:9px;padding:9px 11px;background:color-mix(in srgb,var(--brand) 7%,var(--tile))}
  .tf-current span{display:block;font-size:10.5px;color:var(--muted)}
  .tf-current b{display:block;margin-top:2px;font-size:14px;color:var(--brand)}
  .tf-current small{display:block;margin-top:4px;font-size:11px;color:var(--muted);overflow-wrap:anywhere}
  .tf-result{display:flex;align-items:center;gap:8px;margin-top:12px;border-radius:9px;padding:9px 11px;color:var(--ok);background:color-mix(in srgb,var(--ok) 10%,var(--tile));box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--ok) 26%,transparent)}
  .tf-result ui5-icon{width:1rem;height:1rem;flex:0 0 auto}
  .tf-result span{flex:1 1 auto;font-size:12.5px;font-weight:700}
  .tf-skeleton-list{display:flex;flex-direction:column;gap:10px}
  .tf-skeleton-list span{height:38px;border-radius:8px;background:color-mix(in srgb,var(--ink) 8%,transparent);animation:tf-skeleton 1.2s ease-in-out infinite}
  .tf-skeleton-list span:nth-child(2){animation-delay:.16s}.tf-skeleton-list span:nth-child(3){animation-delay:.32s}
  @keyframes tf-skeleton{0%,100%{opacity:.55}50%{opacity:1}}
  .tf-return-option{display:flex;align-items:center;gap:8px;padding:6px 7px;border-radius:7px;cursor:pointer}
  .tf-return-option:hover{background:color-mix(in srgb,var(--ink) 5%,transparent)}
  .tf-return-head{display:flex;flex-direction:column;gap:2px;padding:2px 7px 6px;border-bottom:1px solid var(--line-soft);margin-bottom:5px}
  .tf-return-head b{font-size:12px;color:var(--ink)}
  .tf-return-head small{font-size:11px;color:var(--muted)}
  .tf-return-option span{min-width:0;flex:1;display:flex;align-items:center;gap:7px}
  .tf-return-option b{min-width:0;overflow-wrap:anywhere}
  .tf-return-option i{flex:0 0 auto;color:var(--muted);font-style:normal;font-size:10.5px;padding:1px 5px;border-radius:999px;background:color-mix(in srgb,var(--ink) 7%,transparent)}
  .tf-prop-head{height:47px;flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 14px;border-bottom:1px solid var(--line-soft);background:var(--header)}
  .tf-prop-head b{font-size:14px} .tf-prop-head small{display:block;font-size:11px;color:var(--muted)}
  .tf-prop-body{padding:12px 14px;overflow:auto}
  .tf-psec{font-size:11px;font-weight:800;color:var(--brand-d);text-transform:uppercase;margin:14px 0 8px;padding-bottom:5px;border-bottom:1px solid var(--line-soft)}
  .tf-psec:first-child{margin-top:0}
  .tf-flow{display:block;margin:0;padding:0 0 4px;list-style:none}
  .tf-flow-node{display:grid;grid-template-columns:32px minmax(0,1fr);gap:0 12px;padding-bottom:16px}
  .tf-flow-node:last-of-type{padding-bottom:0}
  .tf-flow-rail{position:relative;display:flex;justify-content:center;z-index:1}
  .tf-flow-step{width:28px;height:28px;display:grid;place-items:center;border-radius:50%;border:1px solid var(--line);background:var(--tile);color:var(--muted);font-size:11.5px;font-weight:700;flex:0 0 auto}
  .tf-flow-step ui5-icon{width:.85rem;height:.85rem}
  .tf-flow-node:not(:last-of-type) .tf-flow-rail::after{content:"";position:absolute;top:32px;bottom:0;width:2px;background:color-mix(in srgb,var(--ink) 32%,transparent);z-index:-1}
  .tf-flow-node.done:not(:last-of-type) .tf-flow-rail::after{background:color-mix(in srgb,var(--ok) 58%,transparent)}
  .tf-flow-node.done .tf-flow-step{color:var(--ok);border-color:color-mix(in srgb,var(--ok) 48%,var(--line));background:var(--tile)}
  .tf-flow-node.current .tf-flow-step{color:var(--sapButton_Emphasized_TextColor,var(--sapContent_ContrastTextColor,var(--sapBaseColor)));border-color:var(--brand);background:var(--brand)}
  .tf-flow-node.pending .tf-flow-step{background:color-mix(in srgb,var(--ink) 3%,var(--tile))}
  .tf-flow-node.other .tf-flow-step{width:10px;height:10px;margin:9px;border-style:dashed;background:transparent}
  .tf-flow-content{min-width:0;padding-top:4px}
  .tf-flow-title{display:flex;align-items:baseline;gap:8px;min-width:0}
  .tf-flow-title b{flex:1 1 auto;min-width:0;font-size:13px;font-weight:600;color:var(--ink);overflow-wrap:anywhere}
  .tf-flow-node.pending .tf-flow-title b{color:var(--muted)}
  .tf-flow-node.current .tf-flow-title b{color:var(--brand);font-weight:700}
  .tf-flow-state{flex:0 0 auto;font-size:10.5px;font-weight:600;color:var(--muted);white-space:nowrap}
  .tf-flow-kind{flex:0 0 auto;align-self:center;font-size:10px;font-weight:600;color:var(--muted);border:1px solid color-mix(in srgb,var(--muted) 45%,transparent);border-radius:999px;padding:1px 7px;white-space:nowrap}
  .tf-flow-node.done .tf-flow-state{color:var(--ok)}
  .tf-flow-node.current .tf-flow-state{color:var(--brand)}
  .tf-flow-meta{display:flex;flex-wrap:wrap;align-items:center;gap:4px 8px;margin-top:4px}
  .tf-flow-time,.tf-flow-actors{display:inline-flex;align-items:center;gap:4px;min-width:0;font-size:10.5px;color:var(--muted)}
  .tf-flow-time ui5-icon{width:.7rem;height:.7rem;flex:0 0 auto}
  .tf-flow-actors{overflow-wrap:anywhere}
  .tf-flow-comments{display:flex;flex-direction:column;gap:7px;margin-top:8px}
  .tf-hint{font-size:12px;color:var(--muted);padding:6px 0}
  .tf-empty{padding:44px;text-align:center;color:var(--muted)}
  .tf-toast{position:absolute;left:50%;bottom:18px;transform:translateX(-50%);background:color-mix(in srgb,var(--ink) 92%,transparent);color:var(--tile);padding:9px 16px;border-radius:9px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s;z-index:20;box-shadow:0 8px 28px color-mix(in srgb,var(--ink) 28%,transparent)}
  .tf-toast.show{opacity:1}
  `
}
// 门户壳 export default（CFG 默认值=今天）；S5 组件壳 import { configure, mount } 覆盖后自挂。
export { configure, mount }
export default {
  defaultView: 'content',
  views: {
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
