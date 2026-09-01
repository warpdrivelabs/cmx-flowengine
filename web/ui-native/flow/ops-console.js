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
  bpmnBase: '/portal/vendor/bpmn-js',   // P4：bpmn-js 只读查看器静态资产根（同设计器）
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }
function bpmnBase () { return CFG.bpmnBase }

const state = {
  list: [],          // 实例列表
  filter: { keyword: '', state: '' },
  selectedId: null,  // 当前查看实例
  detail: null,      // 当前实例详情（含 incidents/tokens/tasks/variables）
  loading: false,
  hosts: new Set(),
  viewer: null,      // P4：bpmn-js 只读查看器实例（单例，跨渲染复用）
  viewerEl: null,    // 查看器当前挂载的容器
  viewerKey: null,   // 查看器当前载入的定义 key（换实例/定义才重导入）
  bpmnCache: {},     // definitionKey → bpmnXml（避免重复拉取）
  es: null,          // P4+：生命周期事件 SSE（EventSource），令牌位置实时刷新替轮询
  liveOn: false,     // SSE 是否已连接（图例上显示「实时/手动」）
  liveTimer: null,   // 事件到达后的去抖 reload 定时器（合并突发事件）
  varHistory: null,  // 变量历史（含引擎派生 decision/subflow）；随选中实例载入
  // 生命周期回放：把 /activities（已闭合的历史节点访问，时间序）+ 当前 activeNodes（此刻）
  // 组成「帧」序列，在只读画布上按帧步进高亮令牌走过的路径。frames[i] = { at, occupied:[node], entered:[node], live } 。
  replay: { on: false, frames: [], idx: 0, playing: false, timer: null, loading: false },
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装（cmx-data-comp/lib/cmx-page-helpers.js）；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

// SSE 连接（带 jwt 一次性票据）。原生 EventSource 不能带 header，故 jwt 模式下先用带 header 的 POST
// 换一张短期一次性票据再拼进 URL（?ticket=）；off 模式后端忽略票据。断线重连会用旧票 401 → onerror
// 关闭后重新铸票重连（节流+次数守卫）。listeners={name:fn(data)}；onopen/onclose 通知连接态（驱动实时 pill）。
function openSse (path, listeners, hooks = {}) {
  const wc = !(CFG.fetchInit && CFG.fetchInit.credentials === 'omit')
  const handle = { es: null, closed: false, retries: 0, timer: null }
  const MAX_RETRIES = 6
  const connect = async () => {
    if (handle.closed) return
    let ticket = ''
    try { const t = await apiJson('/api/flow/v1/sse/ticket', { method: 'POST' }); ticket = (t && t.ticket) || '' } catch { /* 无票裸连（off 仍可用） */ }
    if (handle.closed) return
    try {
      const sep = path.includes('?') ? '&' : '?'
      const es = new EventSource((CFG.apiBase || '') + path + (ticket ? sep + 'ticket=' + enc(ticket) : ''), { withCredentials: wc })
      handle.es = es
      for (const [name, fn] of Object.entries(listeners || {})) es.addEventListener(name, (m) => { try { fn(m) } catch { /* ignore */ } })
      es.onopen = () => { handle.retries = 0; if (!handle.closed && hooks.onopen) hooks.onopen() }
      es.onerror = () => {
        try { es.close() } catch { /* ignore */ }
        handle.es = null
        if (hooks.onclose) hooks.onclose()
        if (handle.closed || handle.retries >= MAX_RETRIES) return
        handle.retries++
        handle.timer = setTimeout(connect, Math.min(1000 * handle.retries, 5000))
      }
    } catch { /* EventSource 不可用则降级 */ }
  }
  connect()
  return {
    close () {
      handle.closed = true
      if (handle.timer) { clearTimeout(handle.timer); handle.timer = null }
      if (handle.es) { try { handle.es.close() } catch { /* ignore */ } handle.es = null }
    },
  }
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}
const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

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
  // 令牌状态图例（只列本实例出现的状态 + 恒常的活动/异常/结束，避免图例过长）。
  const presentStates = new Set((d.tokens || []).map((t) => t.state))
  const legendItems = TOK_LEGEND.filter(([cls]) => {
    if (cls === 'run' || cls === 'inc' || cls === 'end') return true
    const map = { wait: 'WAITING', subflow: 'WAITING_SUBFLOW', timer: 'WAITING_TIMER', async: 'WAITING_ASYNC', msg: 'WAITING_MESSAGE' }
    return presentStates.has(map[cls])
  }).map(([, color, name]) => `<span class="ops-lg"><i style="background:${color}"></i>${name}</span>`).join('')
  const liveDot = state.liveOn
    ? '<span class="ops-live" title="实时更新已连接（SSE）"><i></i>实时</span>'
    : '<span class="ops-live off" title="实时更新未连接，手动刷新"><i></i>手动</span>'
  return `<section class="ops ops-content">
    ${toolbar}
    <div class="ops-sec">流程图（令牌位置高亮）<span class="ops-sec-r">${liveDot}<button class="ops-mini ${state.replay.on ? 'on' : ''}" data-act="replay" title="生命周期回放：按时间步进令牌走过的路径">▶ 回放</button><button class="ops-mini" data-act="fit" title="适配视图">⤢ 适配</button></span></div>
    <div class="ops-diagram" data-ops-canvas><div class="ops-diagram-loading">加载流程图…</div></div>
    ${state.replay.on ? replayBarHtml() : ''}
    <div class="ops-legend">${legendItems}</div>
    <div class="ops-sec">令牌位置</div>
    <div class="ops-nodes">${tokens}</div>
    <div class="ops-sec">未办结任务</div>
    <div class="ops-tasks">${tasks}</div>
    <div class="ops-sec">实例变量</div>
    <table class="ops-vtab"><tbody>${varRows}</tbody></table>
    <div class="ops-toast"></div>
  </section>`
}

// 回放控制条：进度滑块 + 播放/暂停 + 单步 + 帧信息（含该帧新进入节点 + 时间）。
function replayBarHtml () {
  const rp = state.replay
  if (rp.loading) return `<div class="ops-replay"><span class="ops-muted">回放数据加载中…</span></div>`
  const n = rp.frames.length
  if (!n) return `<div class="ops-replay"><span class="ops-muted">该实例暂无可回放的历史活动（节点尚未离开或无 DI 布局）。</span></div>`
  const f = rp.frames[Math.min(rp.idx, n - 1)]
  const entered = (f.entered || []).join(', ') || '—'
  const atStr = f.live ? '此刻（当前活动）' : (f.at || '').slice(0, 19).replace('T', ' ')
  return `<div class="ops-replay">
    <div class="ops-replay-ctl">
      <button class="ops-mini" data-rp="first" title="第一帧">⏮</button>
      <button class="ops-mini" data-rp="prev" title="上一步">◀</button>
      <button class="ops-mini play" data-rp="play" title="${rp.playing ? '暂停' : '播放'}">${rp.playing ? '⏸' : '▶'}</button>
      <button class="ops-mini" data-rp="next" title="下一步">▶</button>
      <button class="ops-mini" data-rp="last" title="最后一帧">⏭</button>
      <input type="range" class="ops-replay-range" min="0" max="${n - 1}" value="${Math.min(rp.idx, n - 1)}" data-rp-range>
      <span class="ops-replay-lbl">${Math.min(rp.idx, n - 1) + 1}/${n}</span>
    </div>
    <div class="ops-replay-info"><b>进入</b> ${esc(entered)} <span class="ops-replay-at">· ${esc(atStr)}</span></div>
  </div>`
}

function tokenLabel (s) {
  return {
    ACTIVE: '活动', WAITING: '等待', JOINING: '合流', WAITING_SUBFLOW: '子流程',
    WAITING_TIMER: '定时', WAITING_ASYNC: '异步', WAITING_MESSAGE: '消息',
    INCIDENT: '异常', ENDED: '结束',
  }[s] || s
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
  // 变量历史（含引擎派生 decision/subflow）：谁在何时把哪个变量从什么改成什么、经由哪条路径。
  const vh = state.varHistory
  const vhBody = vh == null
    ? '<div class="ops-muted">加载中…</div>'
    : (vh.length
      ? vh.map((h) => `<div class="ops-vh">
          <div class="ops-vh-head">
            <span class="ops-vh-src ${srcClass(h.source)}">${esc(srcLabel(h.source))}</span>
            <b class="ops-vh-name">${esc(h.varName)}</b>
            ${h.nodeBpmnId ? `<span class="ops-vh-node">@${esc(h.nodeBpmnId)}</span>` : ''}
            <em class="ops-vh-at">${esc((h.changedAt || '').slice(0, 19).replace('T', ' '))}</em>
          </div>
          <div class="ops-vh-diff"><span class="ops-vh-old">${esc(vhVal(h.oldValue))}</span><span class="ops-vh-arw">→</span><span class="ops-vh-new">${esc(vhVal(h.newValue))}</span></div>
          ${h.changedBy ? `<div class="ops-vh-by">${esc(h.changedBy)}</div>` : ''}
        </div>`).join('')
      : '<div class="ops-muted">无变量变更</div>')
  return `<section class="ops ops-property">
    <div class="ops-prop-head"><b>异常与台账</b></div>
    <div class="ops-prop-body">
      <div class="ops-sec">异常挂起 (incident)</div>
      ${incidents}
      <div class="ops-sec">流转台账</div>
      ${dels}
      <div class="ops-sec">变量历史 <span class="ops-sec-r">${vh && vh.length ? vh.length + ' 条' : ''}</span></div>
      ${vhBody}
    </div>
  </section>`
}
function delKind (k) {
  return { TRANSFER: '转办', DELEGATE: '委派', REJECT: '退回', ADDSIGN_BEFORE: '前加签', ADDSIGN_AFTER: '后加签', RESOLVE: '归还' }[k] || k
}
// 变量历史来源标签 + 配色。start/complete/set-variables = 调用方送入；decision/subflow = 引擎派生。
function srcLabel (s) {
  return { start: '发起', complete: '办理', 'set-variables': '改量', decision: '决策', subflow: '子流程' }[s] || s
}
function srcClass (s) {
  // decision/subflow=引擎派生(紫)；start=发起；complete=办理；其余(set-variables 等)=人工改量。
  return { decision: 'derived', subflow: 'derived', start: 'start', complete: 'complete' }[s] || 'manual'
}
function vhVal (v) {
  if (v == null) return '∅'
  const s = String(v)
  return s.length > 60 ? s.slice(0, 57) + '…' : s
}

// ————————————————————— 绑定 —————————————————————

// ————————————————————— P4：bpmn-js 只读令牌位置图 —————————————————————

let _bpmnLoad = null
function ensureBpmnJs () {
  if (window.BpmnJS) return Promise.resolve()
  if (_bpmnLoad) return _bpmnLoad
  _bpmnLoad = new Promise((resolve, reject) => {
    const s = document.createElement('script')
    // NavigatedViewer 够用（只读 + 平移缩放），但门户 vendor 打包的是 modeler bundle，
    // 它同样导出 window.BpmnJS 且可只读用（不挂 palette）。直接复用设计器同一资产，零新增下载。
    s.src = `${bpmnBase()}/bpmn-modeler.production.min.js`
    s.onload = resolve
    s.onerror = () => reject(new Error('bpmn-js 加载失败'))
    document.head.appendChild(s)
  })
  return _bpmnLoad
}

function injectBpmnCss (root) {
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
    l.rel = 'stylesheet'; l.href = `${bpmnBase()}/${f}`; l.setAttribute('data-bpmn-css', '1')
    root.appendChild(l)
  }
  // 令牌高亮标记样式（注入 shadow root，diagram-js addMarker 加的 class 生效）。
  if (!root.querySelector('style[data-ops-marker]')) {
    const st = document.createElement('style')
    st.setAttribute('data-ops-marker', '1')
    st.textContent = `
      .ops-tok-run .djs-visual > :nth-child(1){stroke:#3b82f6 !important;stroke-width:3px !important;fill:#dbeafe !important}
      .ops-tok-inc .djs-visual > :nth-child(1){stroke:#dc2626 !important;stroke-width:3px !important;fill:#fee2e2 !important}
      .ops-tok-wait .djs-visual > :nth-child(1){stroke:#d97706 !important;stroke-width:3px !important;fill:#fef3c7 !important}
      .ops-tok-subflow .djs-visual > :nth-child(1){stroke:#7c3aed !important;stroke-width:3px !important;fill:#ede9fe !important}
      .ops-tok-timer .djs-visual > :nth-child(1){stroke:#0891b2 !important;stroke-width:3px !important;fill:#cffafe !important}
      .ops-tok-async .djs-visual > :nth-child(1){stroke:#c2410c !important;stroke-width:3px !important;fill:#ffedd5 !important}
      .ops-tok-msg .djs-visual > :nth-child(1){stroke:#be185d !important;stroke-width:3px !important;fill:#fce7f3 !important}
      .ops-tok-end .djs-visual > :nth-child(1){stroke:#16a34a !important;stroke-width:3px !important}
      /* 回放：轨迹（走过的节点/边，暗色）+ 当前帧占用节点（活动蓝，同 run） */
      .ops-rp-trail .djs-visual > :nth-child(1){stroke:#94a3b8 !important;stroke-width:2px !important}
      .ops-rp-trail.djs-connection .djs-visual > :nth-child(1){stroke:#94a3b8 !important}
      .ops-rp-now .djs-visual > :nth-child(1){stroke:#3b82f6 !important;stroke-width:3.5px !important;fill:#dbeafe !important}
      /* 令牌数徽标（并行/多实例：一个节点多个令牌）——右上角计数气泡 */
      .ops-tok-badge{background:#0d1117;color:#fff;font:700 10px/1 var(--mono,monospace);min-width:16px;height:16px;padding:0 4px;border-radius:9px;display:flex;align-items:center;justify-content:center;box-shadow:0 1px 3px rgba(0,0,0,.35);border:1.5px solid #fff}
      .ops-tok-badge.inc{background:#cf222e}`
    root.appendChild(st)
  }
}

// 令牌状态 → marker class（决定节点高亮色）。等待态细分色，一眼区分卡在何种等待。
function tokMarkerClass (s) {
  if (s === 'INCIDENT') return 'ops-tok-inc'
  if (s === 'ENDED') return 'ops-tok-end'
  if (s === 'ACTIVE' || s === 'JOINING') return 'ops-tok-run'
  if (s === 'WAITING_SUBFLOW') return 'ops-tok-subflow'
  if (s === 'WAITING_TIMER') return 'ops-tok-timer'
  if (s === 'WAITING_ASYNC') return 'ops-tok-async'
  if (s === 'WAITING_MESSAGE') return 'ops-tok-msg'
  return 'ops-tok-wait'  // WAITING / 其它挂起态
}

// 令牌状态图例（色 + 文字，与画布高亮同色；不靠色区分——每项带中文名）。
const TOK_LEGEND = [
  ['run', '#3b82f6', '活动'],
  ['wait', '#d97706', '等待'],
  ['subflow', '#7c3aed', '子流程'],
  ['timer', '#0891b2', '定时'],
  ['async', '#c2410c', '异步'],
  ['msg', '#be185d', '消息'],
  ['inc', '#dc2626', '异常'],
  ['end', '#16a34a', '结束'],
]

// 挂载/更新只读流程图 + 令牌高亮（在每次 content 渲染后调用；容器随 innerHTML 重建，故 viewer 重挂）。
async function mountDiagram (root) {
  const el = root.querySelector('[data-ops-canvas]')
  const d = state.detail
  if (!el || !d) return
  try {
    injectBpmnCss(root)
    await ensureBpmnJs()
    // 取定义 bpmnXml（按 definitionKey 缓存，换实例同定义不重复拉）。
    const key = d.definitionKey
    let xml = state.bpmnCache[key]
    if (!xml) {
      try {
        const def = await apiJson('/api/flow/definitions/' + enc(key))
        xml = def && def.bpmnXml
        if (xml) state.bpmnCache[key] = xml
      } catch { /* 定义拉取失败：降级只显示文字令牌位置 */ }
    }
    if (!xml) { el.innerHTML = '<div class="ops-diagram-loading">（无流程图，见下方文字令牌位置）</div>'; return }
    // 语义-only 定义（无 BPMNDiagram 图形布局，如 seed/代码构建的流程）：bpmn-js 无法渲染，
    // 直接优雅降级到文字令牌位置，而非抛「加载失败」误导为错误。设计器产出的定义含 DI，走下方图形路径。
    if (!/BPMNDiagram|bpmndi:/.test(xml)) {
      el.innerHTML = '<div class="ops-diagram-loading">该流程无图形布局，见下方文字令牌位置</div>'; return
    }
    // content 每次重渲染 el 都是新节点 → 重建 viewer 挂到它。
    if (state.viewer) { try { state.viewer.destroy() } catch {} state.viewer = null }
    el.innerHTML = ''
    state.viewer = new window.BpmnJS({ container: el })
    state.viewerEl = el
    await state.viewer.importXML(xml)
    const canvas = state.viewer.get('canvas')
    canvas.zoom('fit-viewport', 'auto')
    // 回放模式：不画 live 令牌，改画当前回放帧（轨迹 + 占用节点）。
    if (state.replay.on) { applyReplayFrame(); return }
    // 令牌按节点分组：一节点可有多个活动令牌（并行网关分裂/多实例会签）——高亮该节点 +
    // 令牌数 ≥2 时右上角挂计数徽标（bpmn-js overlays），一眼看出并发度。
    const byNode = new Map()
    for (const t of (d.tokens || [])) {
      if (t.state === 'ENDED') continue  // 已结束令牌不高亮（避免整图铺满）
      const arr = byNode.get(t.nodeBpmnId) || []
      arr.push(t)
      byNode.set(t.nodeBpmnId, arr)
    }
    let overlays = null
    try { overlays = state.viewer.get('overlays') } catch { /* 老 bundle 无 overlays，仅高亮 */ }
    for (const [nodeId, toks] of byNode) {
      // 高亮色取该节点「最紧要」令牌状态（异常 > 活动 > 其它等待），避免多令牌时色彩抖动。
      const lead = toks.find((t) => t.state === 'INCIDENT')
        || toks.find((t) => t.state === 'ACTIVE' || t.state === 'JOINING')
        || toks[0]
      try { canvas.addMarker(nodeId, tokMarkerClass(lead.state)) } catch { /* 节点 id 不在图中，跳过 */ }
      if (overlays && toks.length >= 2) {
        const hasInc = toks.some((t) => t.state === 'INCIDENT')
        try {
          overlays.add(nodeId, {
            position: { top: -10, right: 10 },
            html: `<div class="ops-tok-badge${hasInc ? ' inc' : ''}" title="${toks.length} 个令牌">${toks.length}</div>`,
          })
        } catch { /* overlay 定位失败（节点不在图）跳过 */ }
      }
    }
  } catch (e) {
    el.innerHTML = `<div class="ops-diagram-loading">流程图加载失败：${esc(e.message)}</div>`
  }
}

// ————————————————————— 生命周期回放 —————————————————————
//
// 数据源：GET /instances/{id}/activities（已闭合节点访问，ORDER BY entered_at）+ 当前 activeNodes（此刻）。
// 时间戳可能同秒（快 E2E），故帧序以 activities 数组的既有时间序为准（逐条揭示），末帧叠加 live 活动节点。
async function toggleReplay () {
  const rp = state.replay
  if (rp.on) { exitReplay(); return }
  if (!state.selectedId) return
  rp.on = true; rp.loading = true; rp.frames = []; rp.idx = 0; rp.playing = false
  refreshView('content')
  try {
    const d = await apiJson('/api/flow/instances/' + enc(state.selectedId) + '/activities')
    const acts = (d.activities || d.items || []).slice()
    rp.frames = buildReplayFrames(acts, state.detail)
    rp.idx = rp.frames.length ? rp.frames.length - 1 : 0  // 默认停在末帧（= 当前态）
  } catch (e) { toast('回放数据加载失败: ' + e.message); rp.frames = [] }
  rp.loading = false
  refreshView('content')
}
function exitReplay () {
  const rp = state.replay
  rp.on = false; rp.playing = false
  if (rp.timer) { clearInterval(rp.timer); rp.timer = null }
  refreshView('content')  // 重渲染 → mountDiagram 恢复 live 令牌高亮
}

// 构帧：逐条 activity 揭示一帧（累积走过的节点为轨迹，本条为「新进入」并作为占用高亮）；
// 末帧追加当前 activeNodes（此刻仍占用的节点，历史表尚未记录）。
function buildReplayFrames (acts, detail) {
  const frames = []
  const trail = []
  for (const a of acts) {
    const node = a.activityBpmnId
    trail.push(node)
    frames.push({ at: a.enteredAt, entered: [node], occupied: [node], trail: trail.slice(), live: false })
  }
  // 末帧：当前活动节点（未闭合、不在 hi_activity）。
  const activeNow = (detail && (detail.activeNodes || (detail.tokens || []).filter((t) => t.state !== 'ENDED').map((t) => t.nodeBpmnId))) || []
  const uniqActive = Array.from(new Set(activeNow))
  if (uniqActive.length) {
    const trail2 = trail.slice()
    for (const n of uniqActive) if (!trail2.includes(n)) trail2.push(n)
    frames.push({ at: null, entered: uniqActive, occupied: uniqActive, trail: trail2, live: true })
  }
  return frames
}

// 应用当前回放帧到画布：清所有令牌 marker → 轨迹节点打暗色、占用节点打活动色。
function applyReplayFrame () {
  const rp = state.replay
  if (!rp.on || !state.viewer || !rp.frames.length) return
  let canvas
  try { canvas = state.viewer.get('canvas') } catch { return }
  const reg = state.viewer.get('elementRegistry')
  // 清掉所有已知令牌/轨迹 marker（含 live 高亮）。
  const ALL = ['ops-tok-run', 'ops-tok-inc', 'ops-tok-wait', 'ops-tok-subflow', 'ops-tok-timer', 'ops-tok-async', 'ops-tok-msg', 'ops-tok-end', 'ops-rp-trail', 'ops-rp-now']
  reg.getAll().forEach((el) => { ALL.forEach((c) => { try { canvas.removeMarker(el.id, c) } catch {} }) })
  const f = rp.frames[Math.min(rp.idx, rp.frames.length - 1)]
  if (!f) return
  for (const n of (f.trail || [])) { if (reg.get(n)) { try { canvas.addMarker(n, 'ops-rp-trail') } catch {} } }
  for (const n of (f.occupied || [])) { if (reg.get(n)) { try { canvas.removeMarker(n, 'ops-rp-trail'); canvas.addMarker(n, f.live ? 'ops-tok-run' : 'ops-rp-now') } catch {} } }
}
function replayGoto (idx) {
  const rp = state.replay
  const n = rp.frames.length
  if (!n) return
  rp.idx = Math.max(0, Math.min(idx, n - 1))
  // 只更新滑块 label + 信息 + 画布，不整段重渲（避免 mountDiagram 重导入闪烁）。
  for (const host of Array.from(state.hosts)) {
    if (host.__opsView !== 'content') continue
    const root = hostRoot(host)
    if (!root) continue
    const rangeEl = root.querySelector('[data-rp-range]'); if (rangeEl) rangeEl.value = String(rp.idx)
    const lbl = root.querySelector('.ops-replay-lbl'); if (lbl) lbl.textContent = (rp.idx + 1) + '/' + n
    const info = root.querySelector('.ops-replay-info')
    if (info) {
      const f = rp.frames[rp.idx]
      const atStr = f.live ? '此刻（当前活动）' : (f.at || '').slice(0, 19).replace('T', ' ')
      info.innerHTML = `<b>进入</b> ${esc((f.entered || []).join(', ') || '—')} <span class="ops-replay-at">· ${esc(atStr)}</span>`
    }
    const play = root.querySelector('[data-rp="play"]'); if (play) play.textContent = rp.playing ? '⏸' : '▶'
  }
  applyReplayFrame()
}
function replayPlayPause () {
  const rp = state.replay
  if (rp.playing) { rp.playing = false; if (rp.timer) { clearInterval(rp.timer); rp.timer = null } replayGoto(rp.idx); return }
  // 从末帧按播放则回到起点重播。
  if (rp.idx >= rp.frames.length - 1) rp.idx = 0
  rp.playing = true
  rp.timer = setInterval(() => {
    if (rp.idx >= rp.frames.length - 1) { rp.playing = false; if (rp.timer) { clearInterval(rp.timer); rp.timer = null } replayGoto(rp.idx); return }
    replayGoto(rp.idx + 1)
  }, 900)
  replayGoto(rp.idx)
}


//
// 订阅 GET /api/flow/v1/events（EventSource，按当前租户过滤）。任一生命周期事件到达时，
// 若它属于**当前查看的实例**，去抖后 reload 详情并重绘令牌高亮——无需手动刷新即见令牌流动。
// 经 openSse 走一次性票据：jwt 模式亦可用（EventSource 不能带 header）；off 模式后端忽略票据。
function startLiveEvents () {
  if (state.es || typeof window === 'undefined' || !window.EventSource) return
  const onAny = (m) => {
    let ev = null
    try { ev = JSON.parse(m.data) } catch { /* keep-alive/非 JSON，忽略 */ }
    if (!ev || !state.selectedId) return
    // 只对当前查看实例的事件反应（其它实例的事件仅刷新列表计数）。
    if (ev.instanceId === state.selectedId) scheduleLiveReload()
    else scheduleListRefresh()
  }
  // 事件名与 FlowEventKind.as_str 对齐；逐一监听（EventSource 无通配，onmessage 只收无 event 名的）。
  const listeners = { message: onAny }
  for (const name of ['instance.started', 'instance.completed', 'instance.terminated', 'task.created', 'task.completed', 'task.reassigned']) {
    listeners[name] = onAny
  }
  state.es = openSse('/api/flow/v1/events', listeners, {
    onopen: () => { state.liveOn = true; updateLivePill() },
    onclose: () => { if (state.liveOn) { state.liveOn = false; updateLivePill() } },
  })
}
// 只更新「实时/手动」指示点，不重渲染整个 content（避免为翻个状态点而重建 bpmn-js 查看器）。
function updateLivePill () {
  const pill = state.liveOn
    ? '<span class="ops-live" title="实时更新已连接（SSE）"><i></i>实时</span>'
    : '<span class="ops-live off" title="实时更新未连接，手动刷新"><i></i>手动</span>'
  for (const host of Array.from(state.hosts)) {
    if (host.__opsView !== 'content') continue
    const el = hostRoot(host)?.querySelector?.('.ops-sec-r .ops-live')
    if (el) el.outerHTML = pill
  }
}
let _listTimer = null
function scheduleListRefresh () {
  if (_listTimer) return
  _listTimer = setTimeout(() => { _listTimer = null; loadList() }, 800)
}
// 去抖：突发事件（如并行分裂一次推进多条 task.created）合并成一次 reload，避免抖动。
function scheduleLiveReload () {
  if (state.replay.on) return  // 回放态：不被 live 事件打断（帧是历史快照）。
  if (state.liveTimer) clearTimeout(state.liveTimer)
  state.liveTimer = setTimeout(() => { state.liveTimer = null; reloadDetail() }, 350)
}

// 适配视图：把当前查看器缩放到 fit-viewport（用户平移缩放后一键还原）。
function fitDiagram () {
  try { state.viewer?.get('canvas')?.zoom('fit-viewport', 'auto') } catch {}
}

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
    root.querySelector('[data-act="fit"]')?.addEventListener('click', () => fitDiagram())
    root.querySelector('[data-act="replay"]')?.addEventListener('click', () => toggleReplay())
    // 回放控制条（仅回放态存在）。
    root.querySelector('[data-rp="first"]')?.addEventListener('click', () => replayGoto(0))
    root.querySelector('[data-rp="prev"]')?.addEventListener('click', () => replayGoto(state.replay.idx - 1))
    root.querySelector('[data-rp="next"]')?.addEventListener('click', () => replayGoto(state.replay.idx + 1))
    root.querySelector('[data-rp="last"]')?.addEventListener('click', () => replayGoto(state.replay.frames.length - 1))
    root.querySelector('[data-rp="play"]')?.addEventListener('click', () => replayPlayPause())
    root.querySelector('[data-rp-range]')?.addEventListener('input', (e) => replayGoto(+e.target.value))
    // P4：渲染只读流程图 + 令牌位置高亮（异步，不阻塞事件绑定）。
    mountDiagram(root)
    // P4+：首次进入 content 即建立生命周期事件 SSE（令牌位置实时刷新）。幂等，已连不重建。
    startLiveEvents()
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
  state.varHistory = null  // 清旧，避免串实例
  // 切实例：退出上个实例的回放态（帧属于旧实例）。
  if (state.replay.on) { state.replay.on = false; state.replay.playing = false; if (state.replay.timer) { clearInterval(state.replay.timer); state.replay.timer = null } }
  try {
    state.detail = await apiJson('/api/flow/instances/' + enc(id))
  } catch (e) { toast('加载详情失败: ' + e.message); state.detail = null }
  refreshAll()
  loadVarHistory()  // 异步补载变量历史（不阻塞详情/图渲染）
}

async function reloadDetail () {
  if (!state.selectedId) return
  try { state.detail = await apiJson('/api/flow/instances/' + enc(state.selectedId)) } catch {}
  refreshAll()
  loadList()
  loadVarHistory()
}

// 载入变量历史（含引擎派生 decision/subflow）；失败静默留空，仅刷新 property 区。
async function loadVarHistory () {
  const id = state.selectedId
  if (!id) return
  try {
    const d = await apiJson('/api/flow/instances/' + enc(id) + '/variables/history?limit=200')
    if (state.selectedId !== id) return  // 期间已切实例，丢弃过期结果
    state.varHistory = d.history || d.items || (Array.isArray(d) ? d : [])
  } catch { state.varHistory = [] }
  refreshView('property')
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

// ————————————————————— 样式（Neo 主题：色值全部 --sap*/--neo* 变量派生，light/dark 自动跟随。
// 画布内令牌高亮 marker / 图例色除外——它们画在 bpmn-js 恒亮画布上且须与 marker stroke 严格同色，属功能色不随主题） —————————————————————

function styleCss () {
  return `
  :host, .ops {
    --bg: var(--sapBackgroundColor, #f6f8fa);
    --panel: var(--sapList_Background, #fff);
    --ink: var(--sapTextColor, #1d2d3e);
    --muted: var(--sapContent_LabelColor, #6a6d70);
    --line: var(--sapGroup_ContentBorderColor, #d9d9d9);
    --line-soft: var(--neo-border-subtle, var(--sapTile_SeparatorColor, #e9e9e9));
    --brand: var(--neo-accent, #00b4d8);
    --brand-soft: color-mix(in srgb, var(--neo-accent, #00b4d8) 12%, transparent);
    --ok: var(--sapPositiveColor, #107e3e);
    --ok-soft: color-mix(in srgb, var(--ok, #107e3e) 12%, transparent);
    --warn: var(--neo-warn, #f59e0b);
    --warn-soft: color-mix(in srgb, var(--neo-warn, #f59e0b) 12%, transparent);
    --danger: var(--sapNegativeColor, #bb0000);
    --danger-soft: color-mix(in srgb, var(--danger, #bb0000) 12%, transparent);
    --field-bg: var(--sapField_Background, var(--panel));
    --field-ink: var(--sapField_TextColor, var(--ink));
    --bg-soft: var(--sapGroup_ContentBackground, #fafafa);
    --violet: var(--neo-violet, #7c3aed);
    --mono: ui-monospace, Menlo, Consolas, monospace;
  }
  .ops { font:13px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC",sans-serif; color:var(--ink); height:100%; box-sizing:border-box; display:flex; flex-direction:column; }
  .ops * { box-sizing:border-box; }
  .ops-search { display:flex; gap:5px; padding:8px; border-bottom:1px solid var(--line-soft); }
  .ops-search input { flex:1; min-width:0; font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; padding:0 8px; background:var(--field-bg); color:var(--field-ink); }
  .ops-search select { font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; background:var(--field-bg); color:var(--field-ink); }
  .ops-list-head { display:flex; justify-content:space-between; padding:8px 12px; font-size:11px; color:var(--muted); text-transform:uppercase; font-weight:800; }
  .ops-list-head span { background:var(--brand-soft); color:var(--brand); border-radius:20px; padding:1px 8px; }
  .ops-list { flex:1; overflow-y:auto; padding:0 8px 8px; }
  .ops-row { display:flex; align-items:center; gap:6px; width:100%; text-align:left; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; background:var(--panel); cursor:pointer; }
  .ops-row:hover { border-color:var(--brand); }
  .ops-row.active { background:var(--brand-soft); border-color:var(--brand); }
  .ops-row.incident { border-left:3px solid var(--danger); }
  .ops-row-main { flex:1; min-width:0; } .ops-row-main b { display:block; font-size:13px; } .ops-row-main small { color:var(--muted); font-size:11px; font-family:var(--mono); }
  .ops-badge { font-size:10.5px; font-weight:700; padding:2px 8px; border-radius:20px; white-space:nowrap; background:color-mix(in srgb, var(--muted) 14%, transparent); color:var(--muted); }
  .ops-badge.run { background:var(--brand-soft); color:var(--brand); }
  .ops-badge.susp { background:var(--warn-soft); color:var(--warn); }
  .ops-badge.done { background:var(--ok-soft); color:var(--ok); }
  .ops-badge.term { background:color-mix(in srgb, var(--muted) 14%, transparent); color:var(--muted); }
  .ops-badge.inc { background:var(--danger-soft); color:var(--danger); }
  .ops-empty, .ops-blank { display:flex; flex-direction:column; align-items:center; gap:8px; color:var(--muted); padding:40px 16px; text-align:center; }
  .ops-empty ui5-icon, .ops-blank ui5-icon { width:30px; height:30px; opacity:.5; }
  .ops-blank b { font-size:14px; color:var(--ink); }
  .ops-blank.sm { padding:20px; font-size:12px; }
  .ops-toolbar { display:flex; align-items:center; gap:6px; padding:10px 14px; border-bottom:1px solid var(--line-soft); flex-wrap:wrap; }
  .ops-title { font-size:14px; } .ops-title b { margin-right:6px; } .ops-tb-sp { flex:1; }
  .ops-btn { display:inline-flex; align-items:center; gap:4px; font:inherit; font-size:12px; font-weight:700; padding:6px 12px; border:1px solid var(--line); border-radius:8px; background:var(--panel); color:var(--ink); cursor:pointer; }
  .ops-btn ui5-icon { width:14px; height:14px; }
  .ops-btn.warn { background:var(--warn-soft); color:var(--warn); border-color:color-mix(in srgb, var(--warn) 30%, transparent); }
  .ops-btn.danger { color:var(--danger); border-color:color-mix(in srgb, var(--danger) 35%, transparent); }
  .ops-btn:disabled { opacity:.4; cursor:not-allowed; }
  .ops-sec { font-size:11px; font-weight:800; color:var(--brand); text-transform:uppercase; margin:14px 14px 8px; padding-bottom:5px; border-bottom:1px solid var(--line-soft); display:flex; align-items:center; justify-content:space-between; }
  .ops-sec-r { display:inline-flex; align-items:center; gap:8px; text-transform:none; }
  .ops-mini { font:inherit; font-size:11px; font-weight:700; padding:2px 8px; border:1px solid var(--line); border-radius:6px; background:var(--panel); color:var(--muted); cursor:pointer; }
  .ops-mini:hover { border-color:var(--brand); color:var(--brand); }
  .ops-mini.on { background:var(--brand); color:#fff; border-color:var(--brand); }
  /* 回放控制条 */
  .ops-replay { margin:8px 14px 0; padding:8px 10px; border:1px solid var(--line-soft); border-radius:9px; background:var(--bg); }
  .ops-replay-ctl { display:flex; align-items:center; gap:6px; }
  .ops-replay-ctl .ops-mini.play { min-width:30px; }
  .ops-replay-range { flex:1; min-width:80px; accent-color:var(--brand); cursor:pointer; }
  .ops-replay-lbl { font:700 11px/1 var(--mono,monospace); color:var(--muted); min-width:40px; text-align:right; }
  .ops-replay-info { margin-top:6px; font-size:11.5px; color:var(--ink); }
  .ops-replay-info b { color:var(--brand); font-size:11px; }
  .ops-replay-at { color:var(--muted); font-family:var(--mono,monospace); font-size:10.5px; }
  .ops-live { display:inline-flex; align-items:center; gap:4px; font-size:10.5px; font-weight:700; color:var(--ok); }
  .ops-live i { width:7px; height:7px; border-radius:50%; background:var(--ok); box-shadow:0 0 0 0 color-mix(in srgb, var(--ok) 50%, transparent); animation:ops-pulse 1.8s infinite; }
  .ops-live.off { color:var(--muted); } .ops-live.off i { background:var(--muted); animation:none; }
  @keyframes ops-pulse { 0%{box-shadow:0 0 0 0 color-mix(in srgb, var(--ok) 50%, transparent)} 70%{box-shadow:0 0 0 5px transparent} 100%{box-shadow:0 0 0 0 transparent} }
  .ops-legend { display:flex; flex-wrap:wrap; gap:10px; padding:8px 14px 0; }
  .ops-legend .ops-lg { display:inline-flex; align-items:center; gap:5px; font-size:11px; color:var(--muted); }
  .ops-legend .ops-lg i { width:11px; height:11px; border-radius:3px; }
  .ops-diagram { position:relative; margin:0 14px; height:300px; border:1px solid var(--line-soft); border-radius:8px; overflow:hidden; background:var(--bg-soft); }
  .ops-diagram .djs-container { background:transparent; }
  .ops-diagram-loading { position:absolute; inset:0; display:flex; align-items:center; justify-content:center; font-size:12px; color:var(--muted); }
  .ops-nodes { display:flex; flex-wrap:wrap; gap:6px; padding:0 14px; }
  .ops-node { display:inline-flex; flex-direction:column; align-items:center; font-family:var(--mono); font-size:12px; padding:6px 12px; border:1.5px solid var(--brand); border-radius:8px; color:var(--brand); }
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
  .ops-inc { margin:0 14px 8px; padding:10px 12px; background:var(--danger-soft); border:1px solid color-mix(in srgb, var(--danger) 35%, transparent); border-radius:8px; }
  .ops-inc-head { display:flex; align-items:center; gap:6px; font-size:13px; } .ops-inc-head ui5-icon { width:15px; height:15px; color:var(--danger); }
  .ops-inc-retries { margin-left:auto; font-size:11px; font-weight:700; color:var(--danger); }
  .ops-inc-reason { font-size:12px; color:var(--danger); margin-top:5px; }
  .ops-inc-since { font-size:10.5px; color:var(--muted); margin-top:3px; }
  .ops-tl { margin:0 14px 6px; padding:7px 10px; border-left:2px solid var(--line); font-size:12px; }
  .ops-tl-kind { font-weight:700; color:var(--brand); } .ops-tl-who { margin-left:6px; } .ops-tl em { color:var(--muted); font-size:10.5px; margin-left:6px; }
  .ops-tl-reason { color:var(--muted); font-size:11.5px; margin-top:2px; }
  .ops-vh { margin:0 14px 6px; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; }
  .ops-vh-head { display:flex; align-items:center; gap:6px; flex-wrap:wrap; }
  .ops-vh-src { font-size:10px; font-weight:800; padding:1px 7px; border-radius:20px; background:color-mix(in srgb, var(--muted) 14%, transparent); color:var(--muted); }
  .ops-vh-src.start { background:var(--brand-soft); color:var(--brand); }
  .ops-vh-src.complete { background:var(--ok-soft); color:var(--ok); }
  .ops-vh-src.manual { background:var(--warn-soft); color:var(--warn); }
  .ops-vh-src.derived { background:color-mix(in srgb, var(--violet) 14%, transparent); color:var(--violet); }
  .ops-vh-name { font-size:12.5px; font-family:var(--mono); }
  .ops-vh-node { font-size:10.5px; color:var(--muted); font-family:var(--mono); }
  .ops-vh-at { margin-left:auto; color:var(--muted); font-size:10.5px; }
  .ops-vh-diff { margin-top:5px; display:flex; align-items:center; gap:8px; font-family:var(--mono); font-size:11.5px; }
  .ops-vh-old { color:var(--muted); text-decoration:line-through; opacity:.75; word-break:break-all; }
  .ops-vh-arw { color:var(--brand); font-weight:800; flex:none; }
  .ops-vh-new { color:var(--ink); font-weight:600; word-break:break-all; }
  .ops-vh-by { margin-top:3px; font-size:10.5px; color:var(--muted); }
  /* toast（反色对调：两主题都成立） */
  .ops-toast { position:fixed; left:50%; bottom:20px; transform:translateX(-50%); background:color-mix(in srgb, var(--ink) 90%, transparent); color:var(--bg); padding:9px 16px; border-radius:9px; font-size:12.5px; font-weight:600; opacity:0; pointer-events:none; transition:opacity .2s; z-index:30; }
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
