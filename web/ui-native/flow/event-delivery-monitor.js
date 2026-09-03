/**
 * 事件投递记录（native-page，20260902 重构方案 §6.3）。
 *
 * 布局：KPI 4 卡（24h 投递量/成功率/死信/待投）+ 双 Tab——
 *  - 流水：cmx-filter-bar（订阅者/状态/通道/定义key/命中规则/时间窗）+ cmx-revo-grid + cmx-pager；
 *    行操作：载荷（弹框看 payload/诊断三列）。
 *  - 死信：DEAD 行列表 + 行操作 重发/处置；工具条 全部重发。
 * 工具条：清理（purge DONE/SKIPPED）。流水 30s 轮询、死信 60s 轮询（页面可见时）。
 *
 * 端点（前缀 /api/flow，门户反代 → 引擎 /flow）：
 *   POST /event-deliveries/{query,stats,retry,skip,purge}
 *   POST /event-subscribers/query（订阅者过滤下拉数据源）
 */

const cmx = () => (typeof globalThis !== 'undefined' && globalThis.__cmxDataComp) || {}
const { escHtml: esc } = globalThis.__cmxDataComp
const { apiGet, apiPost } = globalThis.__cmxDataComp
const { showCmxToast } = globalThis.__cmxDataComp

async function confirmBox (message, confirmText = '确认') {
  const C = cmx()
  return typeof C.cmxConfirm === 'function'
    ? await C.cmxConfirm({ message, intent: 'danger', confirmText })
    : window.confirm(message)
}

const DLV_STATES = [
  { k: 'PENDING', label: '待投' },
  { k: 'IN_FLIGHT', label: '投递中' },
  { k: 'DONE', label: '成功' },
  { k: 'DEAD', label: '死信' },
  { k: 'SKIPPED', label: '已处置' },
]

const _hostState = new WeakMap()
function initState () {
  return {
    tab: 'flow',            // flow | dead
    stats: null,
    subs: [],               // 订阅者下拉 [{id,name}]
    hours: 24,
    // 流水 tab 过滤
    fSub: '', fState: '', fChannel: '', fDefKey: '', fRule: '',
    rows: [], total: 0, page: 1, pageSize: 20,
    // 死信 tab
    dRows: [], dTotal: 0, dPage: 1, dPageSize: 20,
    grid: null, dgrid: null,
    timer: null,
  }
}
function getState (host) {
  if (host && !_hostState.has(host)) _hostState.set(host, initState())
  return host ? _hostState.get(host) : null
}
function hostRoot (host) { return (host && (host.renderRoot || host.shadowRoot)) || null }
function fmtTime (t) { if (!t) return ''; const s = String(t); return s.length > 19 ? s.slice(0, 19).replace('T', ' ') : s }

function styleCss () {
  return `
  .pg { height:100%; overflow:hidden; display:flex; flex-direction:column; box-sizing:border-box; padding:12px 20px 16px;
    background:var(--sapBackgroundColor); color:var(--sapTextColor);
    font-family:var(--sapFontFamily,'72','Segoe UI',Arial,sans-serif); }
  .pg-head { margin-bottom:10px; }
  .pg-title { font-size:20px; font-weight:600; color:var(--sapTitleColor); }
  .pg-sub { font-size:12px; color:var(--sapContent_LabelColor); margin-top:2px; }
  .alarm { display:none; align-items:center; gap:8px; padding:8px 14px; margin-bottom:10px; border-radius:6px;
    background:var(--sapErrorBackground,#ffebeb); border:1px solid var(--sapErrorBorderColor,#f08080);
    color:var(--sapNegativeTextColor,#b00); font-size:13px; }
  .alarm.on { display:flex; }
  .kpi-row { display:grid; grid-template-columns:repeat(auto-fit,minmax(150px,1fr)); gap:12px; margin-bottom:12px; }
  .card { display:flex; flex-direction:column; flex:1; min-height:0;
    background:var(--sapList_Background); border:1px solid var(--sapList_BorderColor); border-radius:8px; padding:12px 14px; }
  .card-hd { display:flex; justify-content:space-between; align-items:center; gap:8px; margin-bottom:10px; }
  .card-title { font-size:15px; font-weight:600; color:var(--sapTitleColor); }
  .tabs { display:flex; gap:4px; flex-shrink:0; }
  .tab { border:1px solid transparent; border-bottom:none; border-radius:6px 6px 0 0; padding:6px 16px; cursor:pointer;
    font-size:13px; color:var(--sapContent_LabelColor); background:transparent; }
  .tab.on { background:var(--sapList_Background); border-color:var(--sapList_BorderColor);
    color:var(--sapTitleColor); font-weight:600; position:relative; top:1px; }
  cmx-toolbar, cmx-filter-bar { display:block; }
  .f-ipt { min-width:120px; }
  .tbl-wrap { flex:1; min-height:0; overflow:hidden; display:flex; flex-direction:column; margin-top:10px; }
  .tbl-wrap cmx-revo-grid { display:flex; width:100%; flex:1 1 0%; min-width:0; min-height:0; flex-direction:column; }
  .pv-box { max-height:48vh; overflow:auto; font-size:12px; white-space:pre-wrap; word-break:break-all;
    background:var(--sapList_Hover_Background,rgba(0,0,0,.05)); border-radius:6px; padding:10px; }
  .pv-foot { display:flex; justify-content:flex-end; padding-top:8px; border-top:1px solid var(--sapList_BorderColor,#e5e5e5); flex-shrink:0; }
  `
}

// KPI 行：cmx-kpi-card 带 tone 配色（对标 mdm dispatch-monitor）；死信 >0 另有页顶告警条。
function kpiRowHtml (st) {
  const s = st.stats || {}
  const e = s.emit || {}
  const ready = !!st.stats
  const dead = e.dead ?? 0
  const pending = (e.pending ?? 0) + (e.inFlight ?? 0)
  const rate = e.successRate
  return `<div class="kpi-row">
    <cmx-kpi-card variant="card" id="edKpiTotal" label="近 ${st.hours}h 投递量" value="${ready ? (s.total ?? 0) : '…'}" tone="info"></cmx-kpi-card>
    <cmx-kpi-card variant="card" id="edKpiRate" label="投递成功率" value="${ready && rate != null ? rate : '…'}" unit="${ready && rate != null ? '%' : ''}" tone="${ready && rate != null ? 'success' : 'neutral'}"></cmx-kpi-card>
    <cmx-kpi-card variant="card" id="edKpiDead" label="死信" value="${ready ? dead : '…'}" tone="${ready && dead > 0 ? 'danger' : 'neutral'}"></cmx-kpi-card>
    <cmx-kpi-card variant="card" id="edKpiPending" label="待投 / 投递中" value="${ready ? pending : '…'}" tone="${ready && pending > 0 ? 'warning' : 'neutral'}"></cmx-kpi-card>
  </div>`
}

function viewHtml (st) {
  const subOpts = ['<ui5-option value="">全部订阅者</ui5-option>']
    .concat(st.subs.map((s) => `<ui5-option value="${s.id}" ${String(st.fSub) === String(s.id) ? 'selected' : ''}>${esc(s.name)}</ui5-option>`))
    .join('')
  const stOpts = ['<ui5-option value="">全部状态</ui5-option>']
    .concat(DLV_STATES.map((s) => `<ui5-option value="${s.k}" ${st.fState === s.k ? 'selected' : ''}>${s.label}（${s.k}）</ui5-option>`))
    .join('')
  const dead = ((st.stats || {}).emit || {}).dead ?? 0
  const body = st.tab === 'dead' ? deadHtml(st) : flowHtml(st, subOpts, stOpts)
  return `<div class="pg">
    <div class="pg-head"><div class="pg-title">事件投递记录</div>
      <div class="pg-sub">事件持久化投递队列：租约抢占 + 同订阅者保序 + 指数退避 + 死信处置；接收方按投递行幂等</div></div>
    <div class="alarm ${dead > 0 ? 'on' : ''}" id="edAlarm">⚠ 当前死信 <b id="edAlarmN">${dead}</b> 条：重试超限的投递进入死信队列，可批量重发或处置。</div>
    ${kpiRowHtml(st)}
    <div class="tabs">
      <button class="tab ${st.tab === 'flow' ? 'on' : ''}" data-tab="flow">投递流水</button>
      <button class="tab ${st.tab === 'dead' ? 'on' : ''}" data-tab="dead">死信队列</button>
    </div>
    <div class="card">${body}</div>
  </div>`
}

function flowHtml (st, subOpts, stOpts) {
  return `<div class="card-hd"><div class="card-title" id="edTotal">流水（共 ${st.total} 条）</div>
    <cmx-toolbar>
      <ui5-button design="Transparent" icon="refresh" id="edReload">刷新</ui5-button>
      <ui5-button design="Transparent" icon="wrench" id="edPurge">清理终态行</ui5-button></cmx-toolbar></div>
  <cmx-filter-bar show-search="false">
    <ui5-select id="edFSub">${subOpts}</ui5-select>
    <ui5-select id="edFState">${stOpts}</ui5-select>
    <ui5-input id="edFDefKey" class="f-ipt" placeholder="定义 key" value="${esc(st.fDefKey)}"></ui5-input>
    <ui5-input id="edFRule" class="f-ipt" placeholder="命中规则名" value="${esc(st.fRule)}"></ui5-input>
    <ui5-select id="edFHours">
      <ui5-option value="24" ${st.hours === 24 ? 'selected' : ''}>近 24h（统计窗）</ui5-option>
      <ui5-option value="1" ${st.hours === 1 ? 'selected' : ''}>近 1h</ui5-option>
      <ui5-option value="168" ${st.hours === 168 ? 'selected' : ''}>近 7 天</ui5-option>
      <ui5-option value="720" ${st.hours === 720 ? 'selected' : ''}>近 30 天</ui5-option>
    </ui5-select>
    <ui5-button slot="actions" design="Default" icon="search" id="edSearch">查询</ui5-button>
    <ui5-button slot="actions" design="Transparent" icon="reset" id="edReset">重置</ui5-button>
  </cmx-filter-bar>
  <div class="tbl-wrap"><cmx-revo-grid id="edGrid"></cmx-revo-grid></div>
  <cmx-pager id="edPager" page-size="20" page-sizes="10,20,50,100"></cmx-pager>`
}

function deadHtml (st) {
  return `<div class="card-hd"><div class="card-title" id="edDTotal">死信（共 ${st.dTotal} 条）</div>
    <cmx-toolbar>
      <ui5-button design="Emphasized" icon="restart" id="edRetryAll">全部重发</ui5-button>
      <ui5-button design="Transparent" icon="refresh" id="edDReload">刷新</ui5-button></cmx-toolbar></div>
  <div class="hint" style="font-size:12px;color:var(--sapContent_LabelColor);margin-bottom:6px">
    重试耗尽（含首发口径）的死信行；重发 = 重置完整重试预算（attempts 归零），处置 = 人工确认放弃（SKIPPED 留痕）。</div>
  <div class="tbl-wrap"><cmx-revo-grid id="edDGrid"></cmx-revo-grid></div>
  <cmx-pager id="edDPager" page-size="20" page-sizes="10,20,50,100"></cmx-pager>`
}

// ————————————————————— 数据 —————————————————————

async function loadStats (st) {
  const body = { hours: st.hours }
  if (st.fSub) body.subscriberId = Number(st.fSub)
  st.stats = (await apiPost('/api/flow/event-deliveries/stats', body)) || {}
}

async function loadSubs (st) {
  const d = (await apiPost('/api/flow/event-subscribers/query', { page: 1, pageSize: 200 })) || {}
  st.subs = (d.rows || []).map((r) => ({ id: Number(r.id), name: r.name || `#${r.id}` }))
}

async function loadRows (st) {
  const body = { page: st.page, pageSize: st.pageSize }
  if (st.fSub) body.subscriberId = Number(st.fSub)
  if (st.fState) body.state = st.fState
  if (st.fDefKey.trim()) body.definitionKey = st.fDefKey.trim()
  if (st.fRule.trim()) body.matchedRule = st.fRule.trim()
  const d = (await apiPost('/api/flow/event-deliveries/query', body)) || {}
  st.rows = d.rows || []
  st.total = Number(d.total) || 0
}

async function loadDead (st) {
  const body = { page: st.dPage, pageSize: st.dPageSize, state: 'DEAD' }
  if (st.fSub) body.subscriberId = Number(st.fSub)
  const d = (await apiPost('/api/flow/event-deliveries/query', body)) || {}
  st.dRows = d.rows || []
  st.dTotal = Number(d.total) || 0
}

// ————————————————————— grid —————————————————————

function stateText (s) { const f = DLV_STATES.find((x) => x.k === s); return f ? `${f.label}（${s}）` : s }

function buildFlowGrid (host, st) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const grid = root.querySelector('#edGrid'); if (!grid) return
  grid.setAttribute('data-cmx-fill-height', '')
  grid.setAttribute('data-cmx-options', '{"editable":false,"showTotals":false,"showRequiredMark":false}')
  grid.classList.add('cmx-grid-neo')
  st.grid = grid
  if (!(C.CmxColumnModel && C.CmxColumn)) return
  const cm = new C.CmxColumnModel({ datasetId: 'ed-list' })
  cm.setMembers([
    new C.CmxColumn({ id: 'seq', caption: '#', dataType: 'VARCHAR', width: '80px' }),
    new C.CmxColumn({ id: 'subscriberName', caption: '订阅者', dataType: 'VARCHAR', width: '130px' }),
    new C.CmxColumn({ id: 'eventType', caption: '事件', dataType: 'VARCHAR', width: '140px' }),
    new C.CmxColumn({ id: 'definitionKey', caption: '定义 key', dataType: 'VARCHAR', width: '160px' }),
    new C.CmxColumn({ id: 'matchedRule', caption: '命中规则', dataType: 'VARCHAR', width: '110px' }),
    new C.CmxColumn({ id: 'state_text', caption: '状态', dataType: 'VARCHAR', width: '120px' }),
    new C.CmxColumn({ id: 'attempts', caption: '尝试', dataType: 'VARCHAR', width: '60px' }),
    new C.CmxColumn({ id: 'createdAt', caption: '创建时间', dataType: 'VARCHAR', width: '140px' }),
    new C.CmxColumn({ id: 'deliveredAt', caption: '投递成功', dataType: 'VARCHAR', width: '140px' }),
    new C.CmxColumn({ id: '_action', caption: '操作', dataType: 'VARCHAR', width: '150px', frozen: 'right', edit: { mode: 'readonly' },
      display: { mode: 'actions', actions: [
        { text: '载荷', actionRef: 'payload', icon: 'text' },
        { text: '诊断', actionRef: 'diag', icon: 'message-error', visible: (m) => m.state !== 'DONE' },
      ] } }),
  ])
  grid.setColumnModel(cm)
  grid.setOptions?.({ selectionMode: 'none', fillHeight: true, showRowIndex: true, showTotals: false, allowTextSelect: true, resize: true })
  grid.addEventListener('cmx-cell-link-click', (e) => {
    const d = e.detail || {}
    const ds = grid._ds
    const row = (ds && ds.rows && !isNaN(parseInt(d.rowId, 10))) ? ds.rows[parseInt(d.rowId, 10)] : null
    const rec = row ? (row.toPlainObject ? row.toPlainObject() : row) : null
    if (!rec || rec.id == null) return
    if (d.actionRef === 'payload' || d.actionRef === 'diag') openPayloadDialog(rec, d.actionRef === 'diag')
  })
}

function buildDeadGrid (host, st) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const grid = root.querySelector('#edDGrid'); if (!grid) return
  grid.setAttribute('data-cmx-fill-height', '')
  grid.setAttribute('data-cmx-options', '{"editable":false,"showTotals":false,"showRequiredMark":false}')
  grid.classList.add('cmx-grid-neo')
  st.dgrid = grid
  if (!(C.CmxColumnModel && C.CmxColumn)) return
  const cm = new C.CmxColumnModel({ datasetId: 'ed-dead' })
  cm.setMembers([
    new C.CmxColumn({ id: 'seq', caption: '#', dataType: 'VARCHAR', width: '80px' }),
    new C.CmxColumn({ id: 'subscriberName', caption: '订阅者', dataType: 'VARCHAR', width: '130px' }),
    new C.CmxColumn({ id: 'eventType', caption: '事件', dataType: 'VARCHAR', width: '140px' }),
    new C.CmxColumn({ id: 'definitionKey', caption: '定义 key', dataType: 'VARCHAR', width: '160px' }),
    new C.CmxColumn({ id: 'attempts', caption: '尝试', dataType: 'VARCHAR', width: '60px' }),
    new C.CmxColumn({ id: 'lastHttpStatus', caption: 'HTTP', dataType: 'VARCHAR', width: '70px' }),
    new C.CmxColumn({ id: 'lastError', caption: '失败原因', dataType: 'VARCHAR', width: '220px' }),
    new C.CmxColumn({ id: 'createdAt', caption: '创建时间', dataType: 'VARCHAR', width: '140px' }),
    new C.CmxColumn({ id: '_action', caption: '操作', dataType: 'VARCHAR', width: '180px', frozen: 'right', edit: { mode: 'readonly' },
      display: { mode: 'actions', actions: [
        { text: '重发', actionRef: 'retry', icon: 'restart' },
        { text: '处置', actionRef: 'skip', icon: 'decline' },
        { text: '载荷', actionRef: 'payload', icon: 'text' },
      ] } }),
  ])
  grid.setColumnModel(cm)
  grid.setOptions?.({ selectionMode: 'none', fillHeight: true, showRowIndex: true, showTotals: false, allowTextSelect: true, resize: true })
  grid.addEventListener('cmx-cell-link-click', (e) => {
    const d = e.detail || {}
    const ds = grid._ds
    const row = (ds && ds.rows && !isNaN(parseInt(d.rowId, 10))) ? ds.rows[parseInt(d.rowId, 10)] : null
    const rec = row ? (row.toPlainObject ? row.toPlainObject() : row) : null
    if (!rec || rec.id == null) return
    doDeadAction(host, st, d.actionRef, rec)
  })
}

function applyData (host, st, first) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const t = root.querySelector('#edTotal')
  if (t) t.textContent = `流水（共 ${st.total} 条）`
  const dt = root.querySelector('#edDTotal')
  if (dt) dt.textContent = `死信（共 ${st.dTotal} 条）`
  const pager = root.querySelector('#edPager')
  if (pager) { pager.total = st.total; pager.page = st.page; pager.pageSize = st.pageSize }
  const dpager = root.querySelector('#edDPager')
  if (dpager) { dpager.total = st.dTotal; dpager.page = st.dPage; dpager.pageSize = st.dPageSize }
  const fill = (grid, rows, dsId) => {
    if (!grid) return
    const put = () => {
      if (C.CmxDataSet) { const ds = new C.CmxDataSet({ datasetId: dsId }); ds.setRows(rows); grid.setDataSet(ds) }
      else grid.setDataSet?.(rows)
      grid.refreshLayout?.()
    }
    // 一律双 rAF：重挂 grid 后立即 setDataSet 会被新元素升级/布局静默丢弃。
    requestAnimationFrame(() => requestAnimationFrame(put))
  }
  if (st.tab === 'flow') {
    fill(st.grid, st.rows.map((r) => ({
      ...r, state_text: stateText(r.state),
      createdAt: fmtTime(r.createdAt), deliveredAt: fmtTime(r.deliveredAt),
      matchedRule: r.matchedRule || '-', definitionKey: r.definitionKey || '-',
      lastHttpStatus: r.lastHttpStatus != null ? String(r.lastHttpStatus) : '-',
    })), 'ed-list')
  } else {
    fill(st.dgrid, st.dRows.map((r) => ({
      ...r,
      createdAt: fmtTime(r.createdAt), definitionKey: r.definitionKey || '-',
      lastError: r.lastError || '-',
      lastHttpStatus: r.lastHttpStatus != null ? String(r.lastHttpStatus) : '-',
    })), 'ed-dead')
  }
}

// ————————————————————— 操作 —————————————————————

async function doDeadAction (host, st, act, rec) {
  try {
    if (act === 'retry') {
      const r = (await apiPost('/api/flow/event-deliveries/retry', { ids: [Number(rec.id)] })) || {}
      showCmxToast(`已重发 ${r.reset ?? 1} 行（重试预算已重置）`)
    } else if (act === 'skip') {
      if (!await confirmBox('确认处置该死信行（放弃投递，SKIPPED 留痕）？')) return
      const r = (await apiPost('/api/flow/event-deliveries/skip', { ids: [Number(rec.id)] })) || {}
      showCmxToast(`已处置 ${r.skipped ?? 1} 行`)
    } else if (act === 'payload') { openPayloadDialog(rec, true); return }
    await refresh(host, st)
  } catch (e) { cmx().cmxError?.(e.message) }
}

function openPayloadDialog (rec, withDiag) {
  const C = cmx(); const M = C
  if (!customElements.get('cmx-floating-dialog')) { M.cmxError?.('弹框组件未就绪'); return }
  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: `投递行 #${rec.seq} · ${rec.subscriberName || ''} · ${rec.eventType || ''}`,
    icon: 'text', dialogWidth: '640px', dialogHeight: '72vh',
    showConfirm: false, showCancel: false,
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'flex:1;min-height:0;padding:6px 12px 10px;display:flex;flex-direction:column;gap:8px;font-size:13px;'
  const diag = withDiag ? `<div style="display:flex;flex-direction:column;gap:4px;">
      <div><b>状态</b>：${esc(stateText(rec.state))} · 尝试 ${rec.attempts ?? '-'} 次 · HTTP ${rec.lastHttpStatus ?? '-'}</div>
      <div><b>失败原因</b>：${esc(rec.lastError || '-')}</div>
      <div><b>响应摘要</b>：${esc(rec.lastResponseSnippet || '-')}</div></div>` : ''
  // 弹框内容 teleport 到 body，页面 shadow 内的样式表够不到——样式必须内联在内容里（预览弹框同款）。
  wrap.innerHTML = `<style>
    .pv-box { flex:1 1 auto; min-height:0; overflow:auto; font-size:12px; white-space:pre-wrap; word-break:break-all;
      background:var(--sapList_Hover_Background,rgba(0,0,0,.05)); border-radius:6px; padding:10px; }
    .pv-foot { display:flex; justify-content:flex-end; padding-top:8px; border-top:1px solid var(--sapList_BorderColor,#e5e5e5); flex-shrink:0; }
  </style>${diag}<div class="pv-box">${esc(JSON.stringify(rec.payload, null, 2))}</div>
    <div class="pv-foot"><ui5-button design="Transparent" id="pvClose">关闭</ui5-button></div>`
  wrap.querySelector('#pvClose')?.addEventListener('click', () => dlg.close?.())
  dlg.setContent(wrap)
  dlg.openModal().then(() => dlg.remove())
}

async function refresh (host, st) {
  try {
    if (st.tab === 'flow') { await Promise.all([loadRows(st), loadStats(st)]) }
    else { await Promise.all([loadDead(st), loadStats(st)]) }
    applyData(host, st)
    applyStats(host, st)
  } catch (e) { cmx().cmxError?.(`加载失败：${e.message}`) }
}

// KPI 局部更新（不整页重绘，对标 mdm dispatch-monitor 的 applyStats）。
function applyStats (host, st) {
  const root = hostRoot(host)
  if (!root || !st.stats) return
  const s = st.stats
  const e = s.emit || {}
  const set = (id, v, extra = {}) => {
    const el = root.querySelector(`#${id}`)
    if (!el) return
    el.setAttribute('value', String(v))
    for (const [k, val] of Object.entries(extra)) el.setAttribute(k, String(val))
  }
  const rate = e.successRate
  set('edKpiTotal', s.total ?? 0)
  set('edKpiRate', rate != null ? rate : '-', rate != null ? { unit: '%' } : {})
  const dead = e.dead ?? 0
  set('edKpiDead', dead, { tone: dead > 0 ? 'danger' : 'neutral' })
  const pending = (e.pending ?? 0) + (e.inFlight ?? 0)
  set('edKpiPending', pending, { tone: pending > 0 ? 'warning' : 'neutral' })
  const alarm = root.querySelector('#edAlarm')
  if (alarm) {
    alarm.classList.toggle('on', dead > 0)
    const n = alarm.querySelector('#edAlarmN')
    if (n) n.textContent = String(dead)
  }
}

// ————————————————————— 绑定与入口 —————————————————————

function rerender (host, st) {
  const root = hostRoot(host); if (!root) return
  const pg = root.querySelector('.pg'); if (!pg) return
  const holder = document.createElement('div')
  holder.innerHTML = viewHtml(st)
  pg.replaceWith(holder.firstChild)
  bind(host, st)
}

function bind (host, st) {
  const root = hostRoot(host); if (!root) return
  root.querySelectorAll('.tab')?.forEach((t) => t.addEventListener('click', () => {
    const tab = t.getAttribute('data-tab')
    if (st.tab === tab) return
    st.tab = tab
    rerender(host, st)
    refresh(host, st)
  }))
  root.querySelector('#edReload')?.addEventListener('click', () => refresh(host, st))
  root.querySelector('#edDReload')?.addEventListener('click', () => refresh(host, st))
  root.querySelector('#edSearch')?.addEventListener('click', () => {
    st.fSub = ((root.querySelector('#edFSub') || {}).value) || ''
    st.fState = ((root.querySelector('#edFState') || {}).value) || ''
    st.fDefKey = ((root.querySelector('#edFDefKey') || {}).value) || ''
    st.fRule = ((root.querySelector('#edFRule') || {}).value) || ''
    st.hours = Number(((root.querySelector('#edFHours') || {}).value)) || 24
    st.page = 1; st.dPage = 1
    refresh(host, st)
  })
  root.querySelector('#edReset')?.addEventListener('click', () => {
    st.fSub = ''; st.fState = ''; st.fDefKey = ''; st.fRule = ''; st.page = 1
    rerender(host, st)
    refresh(host, st)
  })
  root.querySelector('#edPurge')?.addEventListener('click', async () => {
    if (!await confirmBox('清理 7 天前的 DONE / SKIPPED 终态行？（近 7 天内与死信不受影响）', '清理')) return
    try {
      const r = (await apiPost('/api/flow/event-deliveries/purge', { beforeDays: 7 })) || {}
      showCmxToast(`已清理 ${r.purged ?? 0} 行`)
      refresh(host, st)
    } catch (e) { cmx().cmxError?.(e.message) }
  })
  root.querySelector('#edRetryAll')?.addEventListener('click', async () => {
    if (!st.dTotal) { cmx().cmxWarn?.('当前无死信'); return }
    if (!await confirmBox(`重发全部死信（当前过滤下共 ${st.dTotal} 行，含租约过期卡死的投递中行）？`)) return
    try {
      const body = { state: 'DEAD' }
      if (st.fSub) body.subscriberId = Number(st.fSub)
      const r = (await apiPost('/api/flow/event-deliveries/retry', body)) || {}
      showCmxToast(`已重发 ${r.reset ?? 0} 行`)
      refresh(host, st)
    } catch (e) { cmx().cmxError?.(e.message) }
  })
  const pager = root.querySelector('#edPager')
  pager?.addEventListener('page-change', (e) => {
    const d = e.detail || {}
    if (d.pageSize && d.pageSize !== st.pageSize) { st.pageSize = d.pageSize; st.page = 1 }
    else st.page = d.page || 1
    loadRows(st).then(() => applyData(host, st)).catch((err) => cmx().cmxError?.(err.message))
  })
  const dpager = root.querySelector('#edDPager')
  dpager?.addEventListener('page-change', (e) => {
    const d = e.detail || {}
    if (d.pageSize && d.pageSize !== st.dPageSize) { st.dPageSize = d.pageSize; st.dPage = 1 }
    else st.dPage = d.page || 1
    loadDead(st).then(() => applyData(host, st)).catch((err) => cmx().cmxError?.(err.message))
  })
  if (st.tab === 'flow') buildFlowGrid(host, st)
  else buildDeadGrid(host, st)
  applyData(host, st, true)
}

function whenRendered (host, sel, cb, t) {
  const n = t == null ? 60 : t
  const root = hostRoot(host)
  if (root && root.querySelector(sel)) { cb(root); return }
  if (n <= 0) return
  requestAnimationFrame(() => whenRendered(host, sel, cb, n - 1))
}

export default {
  defaultView: 'content',
  views: {
    async content (ctx) {
      const host = ctx && ctx.host
      const st = getState(host)
      try {
        await Promise.all([loadSubs(st), loadStats(st), loadRows(st)])
      } catch (e) { console.error('[event-delivery-monitor] init fail', e); cmx().cmxError?.(`初始化失败：${e.message}`) }
      if (host) whenRendered(host, '.pg', () => bind(host, st))
      // 轮询：流水 30s / 死信 60s（host 仍连接时才刷）。
      if (host) {
        if (st.timer) clearInterval(st.timer)
        st.timer = setInterval(() => {
          if (!host.isConnected) { clearInterval(st.timer); st.timer = null; return }
          if (document.hidden) return
          const root = hostRoot(host)
          if (!root || !root.querySelector('.pg')) return
          refresh(host, st)
        }, 30000)
      }
      return `<style>${styleCss()}</style>${viewHtml(st)}`
    },
  },
}
