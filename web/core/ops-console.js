/**
 * 流程实例运维台 —— native_pages 四区（H4：单实例运维视图 + 干预）。
 *
 * 对标 Camunda Operate 的单实例视图：查实例卡在哪、为什么，并可干预。
 *   explorer：实例列表（可按定义/业务键/状态搜索）+ 异常(incident)高亮。
 *   content ：选中实例详情——状态徽标、活动节点(token 位置)、待办任务、变量表；
 *             干预工具条：重试异常 / 改变量 / 取消实例。
 *   property：异常(incident)明细（卡在哪节点、原因、重试次数）+ 流转台账时间线。
 *
 * 数据源：GET /api/flow/instances(列表)、GET /api/flow/instances/{id}(详情，含 incidents)、
 *        POST /api/flow/instances/{id}/{retry-incident,set-variables,cancel}(干预)。
 *
 * S4 抽核纪律：CFG 接缝同其它 native-page，门户默认同源+cookie 零回归。
 */

const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

const state = {
  list: [],          // 实例列表
  filter: { keyword: '', state: '' },
  selectedId: null,  // 当前查看实例
  detail: null,      // 当前实例详情（含 incidents/tokens/tasks/variables）
  loading: false,
  hosts: new Set(),
}

const esc = (s) => String(s ?? '')
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  .replace(/"/g, '&quot;').replace(/'/g, '&#39;')
const enc = encodeURIComponent

async function apiJson (url, options = {}) {
  const full = (CFG.apiBase && url.charAt(0) === '/') ? CFG.apiBase + url : url
  const res = await fetch(full, {
    ...CFG.fetchInit, ...options,
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
    const t = hostRoot(host)?.querySelector?.('.ops-toast')
    if (t) { t.textContent = msg; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2800) }
  }
}

// ————————————————————— 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__opsView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'explorer') loadList() })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}
function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__opsView !== view) continue
    const root = hostRoot(host)
    if (!root) continue
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
}
function refreshAll () { for (const v of ['explorer', 'content', 'property']) refreshView(v) }
function viewHtml (view) {
  if (view === 'explorer') return explorerHtml()
  if (view === 'property') return propertyHtml()
  return contentHtml()
}

// ————————————————————— explorer：实例列表 —————————————————————

const STATE_LABEL = { ACTIVE: '运行中', SUSPENDED: '已挂起', COMPLETED: '已完成', TERMINATED: '已终止' }
const STATE_CLS = { ACTIVE: 'run', SUSPENDED: 'susp', COMPLETED: 'done', TERMINATED: 'term' }

function explorerHtml () {
  const rows = state.list.length
    ? state.list.map((it) => {
        const active = state.selectedId === it.id
        const inc = it.hasIncident
        return `<button class="ops-row ${active ? 'active' : ''} ${inc ? 'incident' : ''}" data-id="${esc(it.id)}">
          <div class="ops-row-main">
            <b>${esc(it.businessKey || it.definitionKey || it.id.slice(0, 8))}</b>
            <small>${esc(it.definitionKey || '')} · ${esc(it.id.slice(0, 8))}</small>
          </div>
          <span class="ops-badge ${STATE_CLS[it.state] || ''}">${STATE_LABEL[it.state] || it.state}</span>
          ${inc ? '<span class="ops-badge inc">异常</span>' : ''}
        </button>`
      }).join('')
    : `<div class="ops-empty"><ui5-icon name="process"></ui5-icon><span>无实例</span></div>`
  return `<section class="ops ops-explorer">
    <div class="ops-search">
      <input data-search value="${esc(state.filter.keyword)}" placeholder="搜索业务键/定义/实例 id…">
      <select data-fstate>
        <option value="" ${!state.filter.state ? 'selected' : ''}>全部状态</option>
        <option value="ACTIVE" ${state.filter.state === 'ACTIVE' ? 'selected' : ''}>运行中</option>
        <option value="COMPLETED" ${state.filter.state === 'COMPLETED' ? 'selected' : ''}>已完成</option>
        <option value="TERMINATED" ${state.filter.state === 'TERMINATED' ? 'selected' : ''}>已终止</option>
      </select>
      <button class="ops-btn" data-refresh title="刷新"><ui5-icon name="refresh"></ui5-icon></button>
    </div>
    <div class="ops-list-head"><b>实例</b><span>${state.list.length}</span></div>
    <div class="ops-list">${rows}</div>
  </section>`
}

// ————————————————————— content：详情 + 干预 —————————————————————

function contentHtml () {
  const d = state.detail
  if (!d) {
    return `<section class="ops ops-content">
      <div class="ops-blank"><ui5-icon name="detail-view"></ui5-icon><b>选择左侧实例查看运维详情</b>
      <span>可查看令牌位置、变量、异常，并执行重试/改变量/取消干预。</span></div>
      <div class="ops-toast"></div></section>`
  }
  const stTag = `<span class="ops-badge ${STATE_CLS[d.state] || ''}">${STATE_LABEL[d.state] || d.state}</span>`
  const incTag = d.hasIncident ? '<span class="ops-badge inc">异常挂起</span>' : ''
  const canAct = d.state === 'ACTIVE'
  const suspended = d.state === 'SUSPENDED'
  const toolbar = `<div class="ops-toolbar">
    <div class="ops-title"><b>${esc(d.businessKey || d.definitionKey || d.id.slice(0, 8))}</b> ${stTag} ${incTag}</div>
    <span class="ops-tb-sp"></span>
    <button class="ops-btn ${d.hasIncident ? 'warn' : ''}" data-act="retry" ${d.hasIncident ? '' : 'disabled'} title="重试所有异常挂起的令牌"><ui5-icon name="restart"></ui5-icon> 重试异常</button>
    <button class="ops-btn" data-act="setvar" ${canAct || suspended ? '' : 'disabled'} title="改实例变量（修数据）"><ui5-icon name="edit"></ui5-icon> 改变量</button>
    <button class="ops-btn" data-act="jump" ${canAct ? '' : 'disabled'} title="自由跳转到指定节点"><ui5-icon name="journey-arrive"></ui5-icon> 跳转</button>
    ${suspended
      ? `<button class="ops-btn" data-act="resume" title="恢复挂起的实例"><ui5-icon name="play"></ui5-icon> 恢复</button>`
      : `<button class="ops-btn" data-act="suspend" ${canAct ? '' : 'disabled'} title="挂起实例（暂停办理）"><ui5-icon name="pause"></ui5-icon> 挂起</button>`}
    <button class="ops-btn danger" data-act="cancel" ${canAct || suspended ? '' : 'disabled'} title="取消/终止实例"><ui5-icon name="stop"></ui5-icon> 取消实例</button>
  </div>`
  // 活动节点（token 位置）。
  const tokens = (d.tokens || []).map((t) => `<span class="ops-node ${t.state === 'INCIDENT' ? 'inc' : (t.state === 'ENDED' ? 'end' : 'run')}">${esc(t.nodeBpmnId)}<em>${esc(tokenLabel(t.state))}</em></span>`).join('') || '<span class="ops-muted">无令牌</span>'
  // 待办任务。
  const tasks = (d.openTasks || []).length
    ? (d.openTasks || []).map((t) => `<div class="ops-task">
        <div class="ops-task-main"><b>${esc(t.name || t.nodeBpmnId)}</b><small>办理人 ${esc(t.assignee || '—')}${t.candidates && t.candidates.length ? ' · 候选 ' + t.candidates.length : ''}</small></div>
        <button class="ops-btn slim" data-urge="${esc(t.id)}" ${t.assignee ? '' : 'disabled'} title="催办当前办理人"><ui5-icon name="bell"></ui5-icon> 催办</button>
      </div>`).join('')
    : '<div class="ops-muted">无未办结任务</div>'
  // 变量表。
  const vars = d.variables && Object.keys(d.variables).filter((k) => k !== '__incident')
  const varRows = (vars && vars.length)
    ? vars.map((k) => `<tr><td>${esc(k)}</td><td>${esc(typeof d.variables[k] === 'object' ? JSON.stringify(d.variables[k]) : d.variables[k])}</td></tr>`).join('')
    : '<tr><td colspan="2" class="ops-muted">无变量</td></tr>'
  return `<section class="ops ops-content">
    ${toolbar}
    <div class="ops-sec">令牌位置</div>
    <div class="ops-nodes">${tokens}</div>
    <div class="ops-sec">未办结任务</div>
    <div class="ops-tasks">${tasks}</div>
    <div class="ops-sec">实例变量</div>
    <table class="ops-vtab"><tbody>${varRows}</tbody></table>
    <div class="ops-toast"></div>
  </section>`
}

function tokenLabel (s) {
  return { ACTIVE: '活动', WAITING: '等待', JOINING: '合流', WAITING_SUBFLOW: '子流程', INCIDENT: '异常', ENDED: '结束' }[s] || s
}

// ————————————————————— property：异常明细 + 台账时间线 —————————————————————

function propertyHtml () {
  const d = state.detail
  if (!d) return `<section class="ops ops-property"><div class="ops-blank sm"><span>选择实例查看异常与台账</span></div></section>`
  const incidents = (d.incidents || []).length
    ? (d.incidents || []).map((i) => `<div class="ops-inc">
        <div class="ops-inc-head"><ui5-icon name="alert"></ui5-icon><b>${esc(i.nodeBpmnId)}</b><span class="ops-inc-retries">重试 ${esc(i.retries)} 次</span></div>
        <div class="ops-inc-reason">${esc(i.reason || '（无原因记录）')}</div>
        <div class="ops-inc-since">自 ${esc((i.since || '').slice(0, 19).replace('T', ' '))}</div>
      </div>`).join('')
    : '<div class="ops-muted">无异常</div>'
  const dels = (d.delegations || []).length
    ? (d.delegations || []).map((x) => `<div class="ops-tl">
        <span class="ops-tl-kind">${esc(delKind(x.kind))}</span>
        <span class="ops-tl-who">${esc(x.fromUserId || '')} → ${esc(x.toUserId || '')}</span>
        <em>${esc((x.createdAt || '').slice(0, 19).replace('T', ' '))}</em>
        ${x.reason ? `<div class="ops-tl-reason">${esc(x.reason)}</div>` : ''}
      </div>`).join('')
    : '<div class="ops-muted">无流转记录</div>'
  return `<section class="ops ops-property">
    <div class="ops-prop-head"><b>异常与台账</b></div>
    <div class="ops-prop-body">
      <div class="ops-sec">异常挂起 (incident)</div>
      ${incidents}
      <div class="ops-sec">流转台账</div>
      ${dels}
    </div>
  </section>`
}
function delKind (k) {
  return { TRANSFER: '转办', DELEGATE: '委派', REJECT: '退回', ADDSIGN_BEFORE: '前加签', ADDSIGN_AFTER: '后加签', RESOLVE: '归还' }[k] || k
}

// ————————————————————— 绑定 —————————————————————

function bind (root, view, host) {
  if (view === 'explorer') {
    const s = root.querySelector('[data-search]')
    if (s) s.addEventListener('input', () => { state.filter.keyword = s.value })
    if (s) s.addEventListener('keydown', (e) => { if (e.key === 'Enter') loadList() })
    root.querySelector('[data-fstate]')?.addEventListener('change', (e) => { state.filter.state = e.target.value; loadList() })
    root.querySelector('[data-refresh]')?.addEventListener('click', () => loadList())
    root.querySelectorAll('[data-id]').forEach((b) => b.addEventListener('click', () => selectInstance(b.dataset.id)))
  }
  if (view === 'content') {
    root.querySelector('[data-act="retry"]')?.addEventListener('click', () => doRetry())
    root.querySelector('[data-act="setvar"]')?.addEventListener('click', () => doSetVar())
    root.querySelector('[data-act="jump"]')?.addEventListener('click', () => doJump())
    root.querySelector('[data-act="suspend"]')?.addEventListener('click', () => doSuspendResume('suspend'))
    root.querySelector('[data-act="resume"]')?.addEventListener('click', () => doSuspendResume('resume'))
    root.querySelector('[data-act="cancel"]')?.addEventListener('click', () => doCancel())
    root.querySelectorAll('[data-urge]').forEach((b) => b.addEventListener('click', () => doUrge(b.dataset.urge)))
  }
}

// ————————————————————— 数据/动作 —————————————————————

async function loadList () {
  state.loading = true
  try {
    const d = await apiJson('/api/flow/instances?limit=100')
    let items = d.items || d.instances || (Array.isArray(d) ? d : [])
    const kw = state.filter.keyword.trim().toLowerCase()
    if (kw) items = items.filter((it) => JSON.stringify(it).toLowerCase().includes(kw))
    if (state.filter.state) items = items.filter((it) => it.state === state.filter.state)
    state.list = items
  } catch (e) { toast('加载失败: ' + e.message); state.list = [] }
  state.loading = false
  refreshView('explorer')
}

async function selectInstance (id) {
  state.selectedId = id
  try {
    state.detail = await apiJson('/api/flow/instances/' + enc(id))
  } catch (e) { toast('加载详情失败: ' + e.message); state.detail = null }
  refreshAll()
}

async function reloadDetail () {
  if (!state.selectedId) return
  try { state.detail = await apiJson('/api/flow/instances/' + enc(state.selectedId)) } catch {}
  refreshAll()
  loadList()
}

async function doRetry () {
  if (!state.selectedId) return
  try {
    await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/retry-incident', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({}),
    })
    toast('已触发重试')
    await reloadDetail()
  } catch (e) { toast('重试失败: ' + e.message) }
}

async function doSetVar () {
  if (!state.selectedId) return
  const raw = (typeof window !== 'undefined' && window.prompt)
    ? window.prompt('输入要 merge 的变量 JSON，如 {"amount": 5000}：', '{}')
    : '{}'
  if (raw == null) return
  let variables
  try { variables = JSON.parse(raw) } catch { toast('变量 JSON 非法'); return }
  try {
    await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/set-variables', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ variables }),
    })
    toast('变量已更新')
    await reloadDetail()
  } catch (e) { toast('改变量失败: ' + e.message) }
}

async function doCancel () {
  if (!state.selectedId) return
  if (typeof window !== 'undefined' && window.confirm && !window.confirm('确认取消/终止该实例？此操作不可逆。')) return
  try {
    await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/cancel', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({}),
    })
    toast('实例已取消')
    await reloadDetail()
  } catch (e) { toast('取消失败: ' + e.message) }
}

// 自由跳转（A7）：输入目标节点 bpmn id。
async function doJump () {
  if (!state.selectedId) return
  const target = (typeof window !== 'undefined' && window.prompt)
    ? window.prompt('跳转到哪个用户任务节点？输入节点 bpmn id：', '')
    : ''
  if (!target) return
  try {
    await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/jump', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ targetBpmnId: target, reason: '运维跳转' }),
    })
    toast('已跳转到 ' + target)
    await reloadDetail()
  } catch (e) { toast('跳转失败: ' + e.message) }
}

// 挂起 / 恢复（A7）。
async function doSuspendResume (action) {
  if (!state.selectedId) return
  try {
    await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/' + action, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({}),
    })
    toast(action === 'suspend' ? '实例已挂起' : '实例已恢复')
    await reloadDetail()
  } catch (e) { toast((action === 'suspend' ? '挂起' : '恢复') + '失败: ' + e.message) }
}

// 催办（A7）：对某待办任务的当前办理人发催办。
async function doUrge (taskId) {
  if (!state.selectedId || !taskId) return
  const msg = (typeof window !== 'undefined' && window.prompt)
    ? window.prompt('催办留言（可空）：', '请尽快处理')
    : '请尽快处理'
  if (msg == null) return
  try {
    await apiJson('/api/flow/tasks/' + enc(taskId) + '/urge', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ instanceId: state.selectedId, message: msg || null }),
    })
    toast('已催办')
    await reloadDetail()
  } catch (e) { toast('催办失败: ' + e.message) }
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  :host, .ops { --bg:#f6f8fa; --panel:#fff; --ink:#1f2328; --muted:#656d76; --line:#d0d7de; --line-soft:#eaeef2; --brand:#0969da; --brand-d:#0a4d8c; --brand-soft:#ddf4ff; --ok:#1a7f37; --ok-soft:#e6f6eb; --danger:#cf222e; --danger-soft:#ffebe9; --warn:#9a6700; --warn-soft:#fff8e6; --mono:ui-monospace,Menlo,Consolas,monospace; }
  .ops { font:13px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC",sans-serif; color:var(--ink); height:100%; box-sizing:border-box; display:flex; flex-direction:column; }
  .ops * { box-sizing:border-box; }
  .ops-search { display:flex; gap:5px; padding:8px; border-bottom:1px solid var(--line-soft); }
  .ops-search input { flex:1; min-width:0; font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; padding:0 8px; }
  .ops-search select { font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; background:#fff; }
  .ops-list-head { display:flex; justify-content:space-between; padding:8px 12px; font-size:11px; color:var(--muted); text-transform:uppercase; font-weight:800; }
  .ops-list-head span { background:var(--brand-soft); color:var(--brand-d); border-radius:20px; padding:1px 8px; }
  .ops-list { flex:1; overflow-y:auto; padding:0 8px 8px; }
  .ops-row { display:flex; align-items:center; gap:6px; width:100%; text-align:left; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; background:#fff; cursor:pointer; }
  .ops-row:hover { border-color:var(--brand); }
  .ops-row.active { background:var(--brand-soft); border-color:var(--brand); }
  .ops-row.incident { border-left:3px solid var(--danger); }
  .ops-row-main { flex:1; min-width:0; } .ops-row-main b { display:block; font-size:13px; } .ops-row-main small { color:var(--muted); font-size:11px; font-family:var(--mono); }
  .ops-badge { font-size:10.5px; font-weight:700; padding:2px 8px; border-radius:20px; white-space:nowrap; background:#eee; color:#666; }
  .ops-badge.run { background:var(--brand-soft); color:var(--brand-d); }
  .ops-badge.susp { background:var(--warn-soft); color:var(--warn); }
  .ops-badge.done { background:var(--ok-soft); color:var(--ok); }
  .ops-badge.term { background:#eee; color:#888; }
  .ops-badge.inc { background:var(--danger-soft); color:var(--danger); }
  .ops-empty, .ops-blank { display:flex; flex-direction:column; align-items:center; gap:8px; color:var(--muted); padding:40px 16px; text-align:center; }
  .ops-empty ui5-icon, .ops-blank ui5-icon { width:30px; height:30px; opacity:.5; }
  .ops-blank b { font-size:14px; color:var(--ink); }
  .ops-blank.sm { padding:20px; font-size:12px; }
  .ops-toolbar { display:flex; align-items:center; gap:6px; padding:10px 14px; border-bottom:1px solid var(--line-soft); flex-wrap:wrap; }
  .ops-title { font-size:14px; } .ops-title b { margin-right:6px; } .ops-tb-sp { flex:1; }
  .ops-btn { display:inline-flex; align-items:center; gap:4px; font:inherit; font-size:12px; font-weight:700; padding:6px 12px; border:1px solid var(--line); border-radius:8px; background:#fff; color:var(--ink); cursor:pointer; }
  .ops-btn ui5-icon { width:14px; height:14px; }
  .ops-btn.warn { background:var(--warn-soft); color:var(--warn); border-color:#f0e0b0; }
  .ops-btn.danger { color:var(--danger); border-color:#ffc9c9; }
  .ops-btn:disabled { opacity:.4; cursor:not-allowed; }
  .ops-sec { font-size:11px; font-weight:800; color:var(--brand-d); text-transform:uppercase; margin:14px 14px 8px; padding-bottom:5px; border-bottom:1px solid var(--line-soft); }
  .ops-nodes { display:flex; flex-wrap:wrap; gap:6px; padding:0 14px; }
  .ops-node { display:inline-flex; flex-direction:column; align-items:center; font-family:var(--mono); font-size:12px; padding:6px 12px; border:1.5px solid var(--brand); border-radius:8px; color:var(--brand-d); }
  .ops-node em { font-family:inherit; font-size:10px; font-style:normal; color:var(--muted); }
  .ops-node.inc { border-color:var(--danger); color:var(--danger); }
  .ops-node.end { border-color:var(--line); color:var(--muted); }
  .ops-tasks { padding:0 14px; } .ops-task { display:flex; align-items:center; gap:8px; padding:7px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; } .ops-task-main { flex:1; min-width:0; } .ops-task b { font-size:13px; } .ops-task small { display:block; color:var(--muted); font-size:11px; }
  .ops-btn.slim { padding:4px 9px; font-size:11px; flex:none; }
  .ops-muted { color:var(--muted); font-size:12px; padding:0 14px; }
  .ops-vtab { width:calc(100% - 28px); margin:0 14px; border-collapse:collapse; font-size:12px; }
  .ops-vtab td { padding:5px 8px; border-bottom:1px solid var(--line-soft); vertical-align:top; word-break:break-all; }
  .ops-vtab td:first-child { font-weight:700; color:var(--muted); white-space:nowrap; font-family:var(--mono); width:34%; }
  .ops-prop-head { padding:12px 14px; border-bottom:1px solid var(--line-soft); font-size:14px; font-weight:750; }
  .ops-prop-body { padding:0 0 14px; }
  .ops-inc { margin:0 14px 8px; padding:10px 12px; background:var(--danger-soft); border:1px solid #ffc9c9; border-radius:8px; }
  .ops-inc-head { display:flex; align-items:center; gap:6px; font-size:13px; } .ops-inc-head ui5-icon { width:15px; height:15px; color:var(--danger); }
  .ops-inc-retries { margin-left:auto; font-size:11px; font-weight:700; color:var(--danger); }
  .ops-inc-reason { font-size:12px; color:#8a1f1f; margin-top:5px; }
  .ops-inc-since { font-size:10.5px; color:var(--muted); margin-top:3px; }
  .ops-tl { margin:0 14px 6px; padding:7px 10px; border-left:2px solid var(--line); font-size:12px; }
  .ops-tl-kind { font-weight:700; color:var(--brand-d); } .ops-tl-who { margin-left:6px; } .ops-tl em { color:var(--muted); font-size:10.5px; margin-left:6px; }
  .ops-tl-reason { color:var(--muted); font-size:11.5px; margin-top:2px; }
  .ops-toast { position:fixed; left:50%; bottom:20px; transform:translateX(-50%); background:#0d1117; color:#fff; padding:9px 16px; border-radius:9px; font-size:12.5px; font-weight:600; opacity:0; pointer-events:none; transition:opacity .2s; z-index:30; }
  .ops-toast.show { opacity:1; }
  `
}

export { configure, mount }
export default {
  defaultView: 'content',
  views: {
    async explorer (ctx) { return mount(ctx, 'explorer') },
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
