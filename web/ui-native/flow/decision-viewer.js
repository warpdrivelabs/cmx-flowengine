/**
 * 决策表查看器 —— native_pages 三区（Next ② 决策表前端可视化）。
 *
 * 只读查看 + 试算，不做编辑（决策表编辑归 cmx-rulesengine 的决策表设计器；这里是流程引擎侧
 * 已注册决策表的运维/调试视图，对标 ops-console 的骨架）。
 *   explorer：已落库决策表列表（key / 命中策略 / 规则数 / 更新时间）+ 搜索 + 刷新。
 *   content ：选中决策表渲染为网格——输入列 | 输出列，逐行规则（条件单元格 + 输出单元格），
 *             命中策略徽标；空条件（"-"/""）显示为「不限」。
 *   property：内联试算——填 facts JSON → POST /decisions/evaluate → 命中规则高亮 + 输出结果。
 *
 * 数据源：GET /api/flow/decisions（列表元数据）、GET /api/flow/decisions/{key}（全表）、
 *        POST /api/flow/decisions/evaluate（试算，纯函数不落库）。
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
  list: [],           // 决策表元数据列表
  filter: '',         // 列表关键字
  selectedKey: null,  // 当前查看的表 key
  table: null,        // 当前表全量（含 rules）
  loading: false,
  hosts: new Set(),
  lastEval: null,     // 最近一次试算结果 { matchedRules, outputs }
  factsDraft: '{}',   // property 区 facts 输入草稿（跨渲染保留）
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装（cmx-data-comp/lib/cmx-page-helpers.js）；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}
const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

// ————————————————————— 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__dcvView = view
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
    if (host.__dcvView !== view) continue
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

// ————————————————————— explorer：决策表列表 —————————————————————

function explorerHtml () {
  const kw = state.filter.trim().toLowerCase()
  const items = kw ? state.list.filter((m) => (m.key || '').toLowerCase().includes(kw)) : state.list
  const rows = items.length
    ? items.map((m) => `<button class="dcv-row ${m.key === state.selectedKey ? 'active' : ''}" data-key="${esc(m.key)}">
        <div class="dcv-row-main">
          <b>${esc(m.key)}</b>
          <small>${esc(hitLabel(m.hitPolicy))} · ${esc(m.ruleCount)} 规则 · ${esc(m.inputCount)} 输入</small>
        </div>
        <span class="dcv-badge">${esc((m.updatedAt || '').slice(0, 10))}</span>
      </button>`).join('')
    : `<div class="dcv-empty"><ui5-icon name="table-view"></ui5-icon><span>${state.loading ? '加载中…' : '无已注册决策表'}</span></div>`
  return `<section class="dcv">
    <div class="dcv-search">
      <input type="text" placeholder="搜索决策表 key…" data-search value="${esc(state.filter)}"/>
      <button class="dcv-btn" data-refresh title="刷新"><ui5-icon name="refresh"></ui5-icon></button>
    </div>
    <div class="dcv-list-head"><span>决策表 ${items.length}</span></div>
    <div class="dcv-list">${rows}</div>
    <div class="dcv-toast"></div>
  </section>`
}

// ————————————————————— content：决策表网格 —————————————————————

function contentHtml () {
  const t = state.table
  if (!t) return `<section class="dcv dcv-content"><div class="dcv-blank"><ui5-icon name="table-view"></ui5-icon><b>选择一张决策表查看规则</b><span class="dcv-blank-sub">左侧列表点选</span></div><div class="dcv-toast"></div></section>`
  const inputs = t.inputs && t.inputs.length ? t.inputs : deriveInputCols(t)
  const outputs = t.outputs && t.outputs.length ? t.outputs : deriveOutputCols(t)
  const matched = new Set((state.lastEval && state.lastEval.matchedRules) || [])
  const headCells = [
    `<th class="dcv-th-idx">#</th>`,
    ...inputs.map((n) => `<th class="dcv-th-in">${esc(n)}</th>`),
    ...outputs.map((n) => `<th class="dcv-th-out">${esc(n)}</th>`),
  ].join('')
  const bodyRows = (t.rules || []).map((r, i) => {
    const conds = (r.conditions || [])
    const inCells = inputs.map((_, ci) => {
      const c = conds[ci]
      const blank = c == null || c === '' || c === '-'
      return `<td class="dcv-td-in${blank ? ' any' : ''}">${blank ? '<span class="dcv-any">不限</span>' : esc(c)}</td>`
    }).join('')
    const outCells = outputs.map((name) => {
      const v = r.outputs ? r.outputs[name] : undefined
      return `<td class="dcv-td-out">${v === undefined ? '<span class="dcv-any">—</span>' : esc(fmtVal(v))}</td>`
    }).join('')
    return `<tr class="${matched.has(i) ? 'hit' : ''}"><td class="dcv-td-idx">${i + 1}${matched.has(i) ? '<span class="dcv-hit-dot" title="本次试算命中"></span>' : ''}</td>${inCells}${outCells}</tr>`
  }).join('')
  return `<section class="dcv dcv-content">
    <div class="dcv-toolbar">
      <div class="dcv-title"><b>${esc(t.key)}</b><span class="dcv-hp ${hitClass(t.hitPolicy)}">${esc(hitLabel(t.hitPolicy))}</span></div>
      <span class="dcv-tb-sp"></span>
      <span class="dcv-meta">${(t.rules || []).length} 条规则 · ${inputs.length} 入 / ${outputs.length} 出</span>
    </div>
    <div class="dcv-grid-wrap">
      <table class="dcv-grid">
        <thead><tr>${headCells}</tr></thead>
        <tbody>${bodyRows}</tbody>
      </table>
    </div>
    <div class="dcv-hint">输入列为命中条件（「不限」= 空/恒真），输出列为命中后写回的变量。到 <b>属性</b> 页填 facts 可试算命中。</div>
    <div class="dcv-toast"></div>
  </section>`
}

// 从规则集推导输入/输出列名（表未显式声明 inputs/outputs 时兜底）。
function deriveInputCols (t) {
  const max = (t.rules || []).reduce((m, r) => Math.max(m, (r.conditions || []).length), 0)
  return Array.from({ length: max }, (_, i) => `条件${i + 1}`)
}
function deriveOutputCols (t) {
  const set = new Set()
  for (const r of (t.rules || [])) for (const k of Object.keys(r.outputs || {})) set.add(k)
  return Array.from(set)
}

// ————————————————————— property：试算 —————————————————————

function propertyHtml () {
  const t = state.table
  if (!t) return `<section class="dcv dcv-property"><div class="dcv-blank sm"><span>选择决策表后可试算</span></div></section>`
  const ev = state.lastEval
  let resultBody = '<div class="dcv-muted">填入 facts 后点「试算」查看命中与输出</div>'
  if (ev) {
    const hits = (ev.matchedRules || [])
    const outs = ev.outputs || {}
    const outRows = Object.keys(outs).length
      ? Object.keys(outs).map((k) => `<tr><td>${esc(k)}</td><td>${esc(fmtVal(outs[k]))}</td></tr>`).join('')
      : '<tr><td colspan="2" class="dcv-muted">无输出（无规则命中）</td></tr>'
    resultBody = `
      <div class="dcv-eval-hits">${hits.length
        ? '命中规则：' + hits.map((i) => `<span class="dcv-hit-chip">#${i + 1}</span>`).join('')
        : '<span class="dcv-muted">无规则命中</span>'}</div>
      <div class="dcv-sec">输出变量</div>
      <table class="dcv-vtab"><tbody>${outRows}</tbody></table>`
  }
  return `<section class="dcv dcv-property">
    <div class="dcv-prop-head"><b>试算</b><small>${esc(t.key)}</small></div>
    <div class="dcv-prop-body">
      <div class="dcv-sec">输入 facts (JSON)</div>
      <textarea class="dcv-facts" data-facts spellcheck="false" placeholder='{"amount": 500000}'>${esc(state.factsDraft)}</textarea>
      <div class="dcv-eval-bar"><button class="dcv-btn primary" data-eval><ui5-icon name="play"></ui5-icon> 试算</button></div>
      <div class="dcv-sec">结果</div>
      ${resultBody}
    </div>
    <div class="dcv-toast"></div>
  </section>`
}

// ————————————————————— 绑定 —————————————————————

function bind (root, view, host) {
  if (view === 'explorer') {
    const s = root.querySelector('[data-search]')
    if (s) {
      s.addEventListener('input', () => { state.filter = s.value; refreshView('explorer') })
    }
    root.querySelector('[data-refresh]')?.addEventListener('click', () => loadList())
    root.querySelectorAll('[data-key]').forEach((b) => b.addEventListener('click', () => selectTable(b.dataset.key)))
    // 保持搜索框焦点（输入即重渲染，光标复位到末尾）。
    const sf = root.querySelector('[data-search]')
    if (sf && state.filter) { sf.focus(); sf.setSelectionRange(sf.value.length, sf.value.length) }
  }
  if (view === 'property') {
    const ta = root.querySelector('[data-facts]')
    if (ta) ta.addEventListener('input', () => { state.factsDraft = ta.value })
    root.querySelector('[data-eval]')?.addEventListener('click', () => doEvaluate())
  }
}

// ————————————————————— 数据/动作 —————————————————————

async function loadList () {
  state.loading = true
  refreshView('explorer')
  try {
    const d = await apiJson('/api/flow/decisions')
    state.list = d.decisions || d.items || (Array.isArray(d) ? d : [])
  } catch (e) { toast('加载失败: ' + e.message); state.list = [] }
  state.loading = false
  refreshView('explorer')
}

async function selectTable (key) {
  state.selectedKey = key
  state.table = null
  state.lastEval = null
  refreshAll()
  try {
    state.table = await apiJson('/api/flow/decisions/' + enc(key))
  } catch (e) { toast('加载决策表失败: ' + e.message); state.table = null }
  refreshAll()
}

async function doEvaluate () {
  const t = state.table
  if (!t) return
  let facts
  try { facts = JSON.parse(state.factsDraft || '{}') } catch { toast('facts JSON 非法'); return }
  try {
    const d = await apiJson('/api/flow/decisions/evaluate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ table: t, variables: facts }),
    })
    state.lastEval = { matchedRules: d.matchedRules || [], outputs: d.outputs || {} }
    toast((state.lastEval.matchedRules.length ? '命中 ' + state.lastEval.matchedRules.length + ' 条' : '无命中'))
  } catch (e) { toast('试算失败: ' + e.message); state.lastEval = null }
  refreshAll()  // content 高亮命中行 + property 显示结果
}

// ————————————————————— 工具 —————————————————————

function hitLabel (p) { return { FIRST: '首命中', COLLECT: '收集全部' }[p] || p || 'FIRST' }
function hitClass (p) { return (p === 'COLLECT') ? 'collect' : 'first' }
function fmtVal (v) {
  if (v == null) return '∅'
  return typeof v === 'object' ? JSON.stringify(v) : String(v)
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  :host, .dcv { --bg:#f6f8fa; --panel:#fff; --ink:#1f2328; --muted:#656d76; --line:#d0d7de; --line-soft:#eaeef2; --brand:#0969da; --brand-d:#0a4d8c; --brand-soft:#ddf4ff; --ok:#1a7f37; --ok-soft:#e6f6eb; --violet:#6d28d9; --violet-soft:#efe9fd; --warn:#9a6700; --warn-soft:#fff8e6; --mono:ui-monospace,Menlo,Consolas,monospace; }
  .dcv { font:13px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC",sans-serif; color:var(--ink); height:100%; box-sizing:border-box; display:flex; flex-direction:column; }
  .dcv * { box-sizing:border-box; }
  .dcv-search { display:flex; gap:5px; padding:8px; border-bottom:1px solid var(--line-soft); }
  .dcv-search input { flex:1; min-width:0; font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; padding:0 8px; }
  .dcv-btn { display:inline-flex; align-items:center; gap:4px; font:inherit; font-size:12px; font-weight:700; padding:6px 12px; border:1px solid var(--line); border-radius:8px; background:#fff; color:var(--ink); cursor:pointer; }
  .dcv-btn ui5-icon { width:14px; height:14px; }
  .dcv-btn.primary { background:var(--brand); color:#fff; border-color:var(--brand); }
  .dcv-btn:disabled { opacity:.4; cursor:not-allowed; }
  .dcv-list-head { display:flex; justify-content:space-between; padding:8px 12px; font-size:11px; color:var(--muted); text-transform:uppercase; font-weight:800; }
  .dcv-list-head span { background:var(--brand-soft); color:var(--brand-d); border-radius:20px; padding:1px 8px; }
  .dcv-list { flex:1; overflow-y:auto; padding:0 8px 8px; }
  .dcv-row { display:flex; align-items:center; gap:6px; width:100%; text-align:left; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; background:#fff; cursor:pointer; }
  .dcv-row:hover { border-color:var(--brand); }
  .dcv-row.active { background:var(--brand-soft); border-color:var(--brand); }
  .dcv-row-main { flex:1; min-width:0; } .dcv-row-main b { display:block; font-size:13px; font-family:var(--mono); } .dcv-row-main small { color:var(--muted); font-size:11px; }
  .dcv-badge { font-size:10.5px; font-weight:700; padding:2px 8px; border-radius:20px; white-space:nowrap; background:#eee; color:#666; font-family:var(--mono); }
  .dcv-empty, .dcv-blank { display:flex; flex-direction:column; align-items:center; gap:8px; color:var(--muted); padding:40px 16px; text-align:center; }
  .dcv-empty ui5-icon, .dcv-blank ui5-icon { width:30px; height:30px; opacity:.5; }
  .dcv-blank b { font-size:14px; color:var(--ink); } .dcv-blank-sub { font-size:12px; } .dcv-blank.sm { padding:20px; font-size:12px; }
  .dcv-toolbar { display:flex; align-items:center; gap:6px; padding:10px 14px; border-bottom:1px solid var(--line-soft); flex-wrap:wrap; }
  .dcv-title { font-size:14px; display:flex; align-items:center; gap:8px; } .dcv-title b { font-family:var(--mono); } .dcv-tb-sp { flex:1; }
  .dcv-hp { font-size:10.5px; font-weight:800; padding:2px 9px; border-radius:20px; }
  .dcv-hp.first { background:var(--brand-soft); color:var(--brand-d); }
  .dcv-hp.collect { background:var(--violet-soft); color:var(--violet); }
  .dcv-meta { font-size:11.5px; color:var(--muted); }
  .dcv-grid-wrap { flex:1; overflow:auto; padding:12px 14px; }
  .dcv-grid { border-collapse:collapse; font-size:12px; width:100%; }
  .dcv-grid th, .dcv-grid td { border:1px solid var(--line); padding:6px 10px; text-align:left; vertical-align:top; }
  .dcv-grid thead th { position:sticky; top:0; background:var(--bg); font-weight:800; z-index:1; }
  .dcv-th-idx { width:44px; text-align:center; color:var(--muted); }
  .dcv-th-in { background:var(--brand-soft) !important; color:var(--brand-d); }
  .dcv-th-out { background:var(--ok-soft) !important; color:var(--ok); }
  .dcv-td-idx { text-align:center; color:var(--muted); font-family:var(--mono); position:relative; }
  .dcv-td-in { font-family:var(--mono); }
  .dcv-td-in.any, .dcv-td-out .dcv-any { color:var(--muted); }
  .dcv-td-out { font-family:var(--mono); background:#fafdfb; }
  .dcv-any { font-style:italic; font-size:11px; }
  .dcv-grid tr.hit td { background:var(--warn-soft); }
  .dcv-grid tr.hit .dcv-td-out { background:#fdf6df; }
  .dcv-hit-dot { display:inline-block; width:7px; height:7px; border-radius:50%; background:var(--warn); margin-left:4px; vertical-align:middle; }
  .dcv-hint { padding:6px 14px 12px; font-size:11px; color:var(--muted); }
  .dcv-prop-head { padding:12px 14px; border-bottom:1px solid var(--line-soft); font-size:14px; font-weight:750; display:flex; align-items:baseline; gap:8px; } .dcv-prop-head small { color:var(--muted); font-size:12px; font-family:var(--mono); font-weight:400; }
  .dcv-prop-body { padding:0 0 14px; }
  .dcv-sec { font-size:11px; font-weight:800; color:var(--brand-d); text-transform:uppercase; margin:14px 14px 8px; padding-bottom:5px; border-bottom:1px solid var(--line-soft); }
  .dcv-facts { width:calc(100% - 28px); margin:0 14px; height:110px; font:12px/1.5 var(--mono); border:1px solid var(--line); border-radius:8px; padding:8px; resize:vertical; }
  .dcv-eval-bar { padding:8px 14px 0; }
  .dcv-eval-hits { padding:0 14px; font-size:12px; }
  .dcv-hit-chip { display:inline-block; font-family:var(--mono); font-size:11px; font-weight:700; background:var(--warn-soft); color:var(--warn); padding:1px 7px; border-radius:6px; margin:0 3px; }
  .dcv-vtab { width:calc(100% - 28px); margin:0 14px; border-collapse:collapse; font-size:12px; }
  .dcv-vtab td { padding:5px 8px; border-bottom:1px solid var(--line-soft); vertical-align:top; word-break:break-all; font-family:var(--mono); }
  .dcv-vtab td:first-child { font-weight:700; color:var(--muted); white-space:nowrap; width:40%; }
  .dcv-muted { color:var(--muted); font-size:12px; padding:0 14px; }
  .dcv-toast { position:fixed; left:50%; bottom:20px; transform:translateX(-50%); background:#0d1117; color:#fff; padding:9px 16px; border-radius:9px; font-size:12.5px; font-weight:600; opacity:0; pointer-events:none; transition:opacity .2s; z-index:30; }
  .dcv-toast.show { opacity:1; }
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
