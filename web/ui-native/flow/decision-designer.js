/**
 * 决策表设计器 —— native_pages 三区可编辑设计器（路线图 Next：决策表可编辑设计器）。
 *
 * 对标只读的 decision-viewer（flow/decision-viewer.js），但**可编辑**：新建/改/删决策表并落库热注册。
 * 网格「机制」复用 cmx-rulesengine 的 rule/designer.js（增删行列 + 一次性委托监听 + dirty + fx 向导），
 * **数据层重绑**到 flow 的 DecisionTable（cmx-flow-model/src/decision.rs）——比 rules-engine 简：
 *   - 命中策略只两项 FIRST / COLLECT（非 DMN 11 项）。
 *   - 输入/输出列为**名字数组** inputs:[str] / outputs:[str]（doc 用）；规则行 =
 *     { conditions:[str]（每输入位一条 expr.rs DSL，空/"-"=不限）, outputs:{名:值} }。
 *   - 单元格：条件格是 expr.rs 条件（如 `amount > 10000`），fx 按钮查 /conditions/functions 目录、
 *     实时 /conditions/validate 校验；输出格填 JSON 值（数/字符串/布尔/对象）。
 *   - 增删输入列须同步每行 conditions 宽度（后端 validate 要求 conditions.len()==inputs.len()）。
 *
 * 数据源：GET /api/flow/decisions（列表）、GET /decisions/{key}（全表）、POST /decisions（保存=落库+热注册，
 *   400 带 validate 诊断）、DELETE /decisions/{key}、POST /decisions/evaluate（试算）、
 *   /api/flow/conditions/{functions,validate}（fx 向导）。**无版本/改名**（后端 upsert 覆盖式，记为后续）。
 */

const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

const state = {
  list: [],            // 决策表元数据列表
  filter: '',          // 列表关键字
  selectedKey: null,   // 当前编辑的表 key
  def: null,           // 当前表全量草稿 { key, hit_policy, inputs:[], outputs:[], rules:[] }（字段名对齐后端 DecisionTable 的 snake_case）
  dirty: false,
  loading: false,
  hosts: new Set(),
  fns: [],             // /conditions/functions 目录
  fx: null,            // fx 向导态 { r, c }
  lastEval: null,      // 试算结果
  factsDraft: '{}',
  validation: null,    // 保存前/失败的校验诊断（字符串数组）
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装（cmx-data-comp/lib/cmx-page-helpers.js）；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}
const { showCmxToast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

// ————————————————————— 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__dsdView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'explorer') { loadList(); ensureFns() } })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}
function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__dsdView !== view) continue
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

const HIT_POLICIES = ['FIRST', 'COLLECT']
const HP_LABEL = { FIRST: '首命中', COLLECT: '收集全部' }

// ————————————————————— explorer：列表 + 新建 —————————————————————

function explorerHtml () {
  const kw = state.filter.trim().toLowerCase()
  const items = kw ? state.list.filter((m) => (m.key || '').toLowerCase().includes(kw)) : state.list
  const rows = items.length
    ? items.map((m) => `<button class="dsd-row ${m.key === state.selectedKey ? 'active' : ''}" data-key="${esc(m.key)}">
        <div class="dsd-row-main">
          <b>${esc(m.key)}</b>
          <small>${esc(HP_LABEL[m.hitPolicy] || m.hitPolicy || 'FIRST')} · ${esc(m.ruleCount)} 规则 · ${esc(m.inputCount)} 输入</small>
        </div>
        <span class="dsd-badge">${esc((m.updatedAt || '').slice(0, 10))}</span>
      </button>`).join('')
    : `<div class="dsd-empty"><ui5-icon name="table-view"></ui5-icon><span>${state.loading ? '加载中…' : '无决策表 · 下方新建'}</span></div>`
  return `<section class="dsd">
    <div class="dsd-search">
      <input type="text" placeholder="搜索决策表 key…" data-search value="${esc(state.filter)}"/>
      <button class="dsd-btn" data-refresh title="刷新"><ui5-icon name="refresh"></ui5-icon></button>
    </div>
    <div class="dsd-list-head"><span>决策表 ${items.length}</span></div>
    <div class="dsd-list">${rows}</div>
    <div class="dsd-newbar">
      <input type="text" placeholder="新决策表 key（英数下划线）" data-newkey/>
      <button class="dsd-btn primary" data-new><ui5-icon name="add"></ui5-icon> 新建</button>
    </div>
    <div class="dsd-toast"></div>
  </section>`
}

// ————————————————————— content：可编辑网格 —————————————————————

function contentHtml () {
  const d = state.def
  if (!d) return `<section class="dsd dsd-content"><div class="dsd-blank"><ui5-icon name="table-view"></ui5-icon><b>选择或新建一张决策表</b><span class="dsd-blank-sub">左侧列表点选 / 底部新建</span></div><div class="dsd-toast"></div></section>`
  const ins = d.inputs, outs = d.outputs, rules = d.rules
  const matched = new Set((state.lastEval && state.lastEval.matchedRules) || [])
  const hpSel = `<select class="dsd-hp" data-act="hitpolicy" title="命中策略">${HIT_POLICIES.map((h) => `<option value="${h}" ${h === d.hit_policy ? 'selected' : ''}>${h} · ${HP_LABEL[h]}</option>`).join('')}</select>`
  const toolbar = `<div class="dsd-toolbar">
    <b class="dsd-title">${esc(d.key)}</b>
    <span class="dsd-dirty ${state.dirty ? 'on' : ''}">${state.dirty ? '● 未保存' : '已保存'}</span>
    <span class="dsd-sp"></span>
    命中策略 ${hpSel}
    <button class="dsd-btn" data-act="add-rule">+ 规则行</button>
    <button class="dsd-btn" data-act="add-input">+ 输入列</button>
    <button class="dsd-btn" data-act="add-output">+ 输出列</button>
    <button class="dsd-btn primary" data-act="save">保存</button>
  </div>`
  const head = `<tr>
    <th class="dsd-idx">#</th>
    ${ins.map((name, ci) => `<th class="dsd-in">
      <input class="dsd-h" data-kind="in-name" data-c="${ci}" value="${esc(name)}" placeholder="输入名"/>
      <button class="dsd-x" data-act="del-input" data-c="${ci}" title="删除输入列">×</button>
    </th>`).join('')}
    ${outs.map((name, ci) => `<th class="dsd-out">
      <input class="dsd-h" data-kind="out-name" data-c="${ci}" value="${esc(name)}" placeholder="输出名"/>
      <button class="dsd-x" data-act="del-output" data-c="${ci}" title="删除输出列">×</button>
    </th>`).join('')}
    <th class="dsd-ops"></th>
  </tr>`
  const body = rules.map((rl, ri) => `<tr class="${matched.has(ri) ? 'hit' : ''}">
    <td class="dsd-idx">${ri + 1}${matched.has(ri) ? '<span class="dsd-hit-dot" title="本次试算命中"></span>' : ''}</td>
    ${ins.map((_, ci) => `<td class="dsd-in">
      <input class="dsd-cell" data-kind="cond" data-r="${ri}" data-c="${ci}" value="${esc((rl.conditions && rl.conditions[ci]) ?? '')}" placeholder="-"/>
      <button class="dsd-fx" data-act="fx" data-r="${ri}" data-c="${ci}" title="条件函数向导">fx</button>
    </td>`).join('')}
    ${outs.map((name, ci) => `<td class="dsd-out"><input class="dsd-cell" data-kind="out" data-r="${ri}" data-c="${ci}" value="${esc(outValStr(rl.outputs, name))}" placeholder="值 / &quot;字符串&quot;"/></td>`).join('')}
    <td class="dsd-ops"><button class="dsd-x" data-act="del-rule" data-r="${ri}" title="删除规则行">×</button></td>
  </tr>`).join('')
  const empty = (!ins.length && !outs.length) ? `<div class="dsd-ph">空决策表：先加输入列 / 输出列，再加规则行</div>` : ''
  const vErr = (state.validation && state.validation.length)
    ? `<div class="dsd-verr"><b>校验未通过：</b>${state.validation.map((m) => `<div>· ${esc(m)}</div>`).join('')}</div>` : ''
  return `<section class="dsd dsd-content">
    ${toolbar}
    ${vErr}
    <div class="dsd-gridwrap"><table class="dsd-grid"><thead>${head}</thead><tbody>${body}</tbody></table></div>
    ${empty}
    <div class="dsd-hint">输入格 = 条件表达式（如 <code>amount &gt; 10000</code>，空 / <code>-</code> = 不限）；输出格 = JSON 值（字符串加引号）。保存即落库并热注册到引擎。</div>
    ${state.fx ? fxHtml() : ''}
    <div class="dsd-toast"></div>
  </section>`
}

// 输出值 → 编辑框字符串：字符串加引号显示，其它 JSON 序列化。
function outValStr (outputs, name) {
  if (!outputs || !(name in outputs)) return ''
  const v = outputs[name]
  return typeof v === 'string' ? JSON.stringify(v) : (v == null ? '' : JSON.stringify(v))
}
// 编辑框字符串 → 输出值：尝试 JSON.parse（数/布尔/对象/带引号字符串），失败当裸字符串。
function parseOutVal (s) {
  const t = (s || '').trim()
  if (t === '') return undefined
  try { return JSON.parse(t) } catch { return t }
}

function fxHtml () {
  const { r, c } = state.fx
  const cur = (state.def.rules[r] && state.def.rules[r].conditions[c]) ?? ''
  const cats = {}
  for (const f of state.fns) { (cats[f.category] = cats[f.category] || []).push(f) }
  const chips = Object.entries(cats).map(([cat, fns]) => `<div class="fx-cat"><span class="fx-catn">${esc(cat)}</span>${fns.map((f) => `<button class="fx-chip" data-fxins="${esc(f.name)}" title="${esc(f.desc || '')}">${esc(f.name)}<small>${esc(f.arity != null ? '/' + f.arity : '')}</small></button>`).join('')}</div>`).join('')
  return `<div class="dsd-fxpanel">
    <div class="fx-hd">条件向导 · 规则行 ${r + 1} · <span class="fx-valid" id="fxv"></span> <button class="dsd-x" data-act="fx-close">×</button></div>
    <div class="fx-editrow"><input class="fx-input" id="fxinput" value="${esc(cur)}" placeholder="条件表达式，如 amount > 10000 / level == 'vip' / -"/>
      <button class="dsd-btn primary" data-act="fx-commit">确定</button></div>
    <div class="fx-cats">${chips || '<span class="dsd-ph">函数目录加载中…</span>'}</div>
  </div>`
}

// ————————————————————— property：试算 —————————————————————

function propertyHtml () {
  const d = state.def
  if (!d) return `<section class="dsd dsd-property"><div class="dsd-blank sm"><span>选择决策表后可试算</span></div></section>`
  const ev = state.lastEval
  let resultBody = '<div class="dsd-muted">填入 facts 后点「试算」查看命中与输出（试算用当前编辑中的表，不必先保存）</div>'
  if (ev) {
    const hits = (ev.matchedRules || [])
    const outs = ev.outputs || {}
    const outRows = Object.keys(outs).length
      ? Object.keys(outs).map((k) => `<tr><td>${esc(k)}</td><td>${esc(fmtVal(outs[k]))}</td></tr>`).join('')
      : '<tr><td colspan="2" class="dsd-muted">无输出（无规则命中）</td></tr>'
    resultBody = `
      <div class="dsd-eval-hits">${hits.length
        ? '命中规则：' + hits.map((i) => `<span class="dsd-hit-chip">#${i + 1}</span>`).join('')
        : '<span class="dsd-muted">无规则命中</span>'}</div>
      <div class="dsd-sec">输出变量</div>
      <table class="dsd-vtab"><tbody>${outRows}</tbody></table>`
  }
  return `<section class="dsd dsd-property">
    <div class="dsd-prop-head"><b>试算</b><small>${esc(d.key)}</small></div>
    <div class="dsd-prop-body">
      <div class="dsd-kv"><span>命中策略</span><b>${esc(d.hit_policy)} · ${esc(HP_LABEL[d.hit_policy] || '')}</b></div>
      <div class="dsd-kv"><span>规模</span><b>${d.inputs.length} 入 / ${d.outputs.length} 出 / ${d.rules.length} 则</b></div>
      <div class="dsd-sec">输入 facts (JSON)</div>
      <textarea class="dsd-facts" data-facts spellcheck="false" placeholder='{"amount": 50000}'>${esc(state.factsDraft)}</textarea>
      <div class="dsd-eval-bar"><button class="dsd-btn primary" data-eval><ui5-icon name="play"></ui5-icon> 试算</button>
        <button class="dsd-btn danger" data-del title="删除此决策表"><ui5-icon name="delete"></ui5-icon> 删除</button></div>
      <div class="dsd-sec">结果</div>
      ${resultBody}
    </div>
    <div class="dsd-toast"></div>
  </section>`
}

// ————————————————————— 结构编辑 —————————————————————

function addRule () { const d = state.def; d.rules.push({ conditions: d.inputs.map(() => '-'), outputs: {} }); state.dirty = true; state.validation = null; refreshView('content') }
function delRule (r) { state.def.rules.splice(r, 1); state.dirty = true; state.validation = null; refreshView('content') }
function addInput () { const d = state.def; d.inputs.push('input' + (d.inputs.length + 1)); d.rules.forEach((rr) => { rr.conditions = rr.conditions || []; rr.conditions.push('-') }); state.dirty = true; state.validation = null; refreshView('content') }
function addOutput () { const d = state.def; d.outputs.push('out' + (d.outputs.length + 1)); state.dirty = true; state.validation = null; refreshView('content') }
function delInput (c) { const d = state.def; d.inputs.splice(c, 1); d.rules.forEach((rr) => { if (rr.conditions) rr.conditions.splice(c, 1) }); state.dirty = true; state.validation = null; refreshView('content') }
function delOutput (c) { const d = state.def; const name = d.outputs[c]; d.outputs.splice(c, 1); d.rules.forEach((rr) => { if (rr.outputs) delete rr.outputs[name] }); state.dirty = true; state.validation = null; refreshView('content') }

// 重命名输出列：把每行 outputs 里的旧键迁到新键（保值）。
function renameOutput (c, newName) {
  const d = state.def; const old = d.outputs[c]
  if (old === newName) return
  d.outputs[c] = newName
  d.rules.forEach((rr) => { if (rr.outputs && old in rr.outputs) { rr.outputs[newName] = rr.outputs[old]; delete rr.outputs[old] } })
  state.dirty = true
}

// ————————————————————— 绑定（委托监听只绑一次） —————————————————————

function bind (root, view, host) {
  if (view === 'explorer') {
    const s = root.querySelector('[data-search]')
    if (s) s.addEventListener('input', () => { state.filter = s.value; refreshView('explorer') })
    root.querySelector('[data-refresh]')?.addEventListener('click', () => loadList())
    root.querySelectorAll('[data-key]').forEach((b) => b.addEventListener('click', () => selectTable(b.dataset.key)))
    root.querySelector('[data-new]')?.addEventListener('click', () => {
      const inp = root.querySelector('[data-newkey]')
      newTable((inp && inp.value || '').trim())
    })
    const sf = root.querySelector('[data-search]')
    if (sf && state.filter) { sf.focus(); sf.setSelectionRange(sf.value.length, sf.value.length) }
    return
  }
  if (view === 'property') {
    const ta = root.querySelector('[data-facts]')
    if (ta) ta.addEventListener('input', () => { state.factsDraft = ta.value })
    root.querySelector('[data-eval]')?.addEventListener('click', () => doEvaluate())
    root.querySelector('[data-del]')?.addEventListener('click', () => deleteTable())
    return
  }
  // content：委托监听绑在持久的 root 上（innerHTML 只换子节点、root 不变），故只绑一次，
  // 否则每次 refreshView 叠加一层 → 一次点击多次触发（如 add-input 加多列）。
  if (root.__dsdContentBound) return
  root.__dsdContentBound = true
  root.addEventListener('change', (ev) => {
    const el = ev.target; const kind = el.getAttribute?.('data-kind')
    if (kind) {
      const c = +el.getAttribute('data-c'); const r = el.getAttribute('data-r')
      const d = state.def; state.dirty = true; state.validation = null
      if (kind === 'cond') { d.rules[+r].conditions[c] = el.value }
      else if (kind === 'out') { const name = d.outputs[c]; const v = parseOutVal(el.value); if (v === undefined) delete d.rules[+r].outputs[name]; else d.rules[+r].outputs[name] = v }
      else if (kind === 'in-name') { d.inputs[c] = el.value }
      else if (kind === 'out-name') { renameOutput(c, el.value) }
      markDirty(root)
      return
    }
    if (el.getAttribute?.('data-act') === 'hitpolicy') { state.def.hit_policy = el.value; state.dirty = true; markDirty(root) }
  })
  root.addEventListener('input', (ev) => { if (ev.target.id === 'fxinput') validateFx(root, ev.target.value) })
  root.addEventListener('click', (ev) => {
    const ins = ev.target.closest('[data-fxins]')?.getAttribute('data-fxins')
    if (ins != null) { const inp = root.querySelector('#fxinput'); if (inp) { inp.value = (inp.value && inp.value !== '-' ? inp.value + ' ' : '') + ins; validateFx(root, inp.value); inp.focus() } return }
    const act = ev.target.closest('[data-act]')?.getAttribute('data-act')
    if (!act) return
    const r = ev.target.closest('[data-r]')?.getAttribute('data-r')
    const c = ev.target.closest('[data-c]')?.getAttribute('data-c')
    if (act === 'add-rule') addRule()
    else if (act === 'del-rule') delRule(+r)
    else if (act === 'add-input') addInput()
    else if (act === 'add-output') addOutput()
    else if (act === 'del-input') delInput(+c)
    else if (act === 'del-output') delOutput(+c)
    else if (act === 'save') saveTable()
    else if (act === 'fx') { state.fx = { r: +r, c: +c }; refreshView('content') }
    else if (act === 'fx-close') { state.fx = null; refreshView('content') }
    else if (act === 'fx-commit') {
      const inp = root.querySelector('#fxinput')
      if (inp && state.fx) { state.def.rules[state.fx.r].conditions[state.fx.c] = inp.value; state.dirty = true; state.validation = null }
      state.fx = null; refreshView('content')
    }
  })
}
function markDirty (root) { const b = root.querySelector('.dsd-dirty'); if (b) { b.classList.toggle('on', state.dirty); b.textContent = state.dirty ? '● 未保存' : '已保存' } }
async function validateFx (root, expr) {
  const el = root.querySelector('#fxv'); if (!el) return
  const t = (expr || '').trim()
  if (t === '' || t === '-') { el.textContent = '不限（恒真）'; el.className = 'fx-valid ok'; return }
  try {
    const r = await apiJson('/api/flow/conditions/validate', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ expr: t }) })
    el.textContent = r.valid ? '✓ 语法正确' : '✗ ' + (r.error || '语法错误'); el.className = 'fx-valid ' + (r.valid ? 'ok' : 'bad')
  } catch { el.textContent = '' }
}

// ————————————————————— 数据/动作 —————————————————————

async function loadList () {
  state.loading = true; refreshView('explorer')
  try {
    const d = await apiJson('/api/flow/decisions')
    state.list = d.decisions || d.items || (Array.isArray(d) ? d : [])
  } catch (e) { showCmxToast('加载失败: ' + e.message, { level: 'error' }); state.list = [] }
  state.loading = false; refreshView('explorer')
}
async function ensureFns () {
  if (state.fns.length) return
  try { const d = await apiJson('/api/flow/conditions/functions'); state.fns = (d && d.functions) || [] } catch { state.fns = [] }
}
async function selectTable (key) {
  state.selectedKey = key; state.def = null; state.lastEval = null; state.dirty = false; state.validation = null; state.fx = null
  refreshAll()
  try {
    const t = await apiJson('/api/flow/decisions/' + enc(key))
    t.inputs = t.inputs || []; t.outputs = t.outputs || []; t.rules = t.rules || []; t.hit_policy = t.hit_policy || 'FIRST'
    t.rules.forEach((r) => { r.conditions = r.conditions || []; r.outputs = r.outputs || {} })
    state.def = t
  } catch (e) { showCmxToast('加载决策表失败: ' + e.message, { level: 'error' }) }
  refreshAll()
}
function newTable (key) {
  if (!key) { showCmxToast('请输入新决策表 key', { level: 'error' }); return }
  if (!/^[A-Za-z][A-Za-z0-9_]*$/.test(key)) { showCmxToast('key 需字母开头，仅字母数字下划线', { level: 'error' }); return }
  if (state.list.some((m) => m.key === key)) { showCmxToast('key 已存在，改为选中编辑', { level: 'error' }); selectTable(key); return }
  state.selectedKey = key
  state.def = { key, hit_policy: 'FIRST', inputs: ['input1'], outputs: ['out1'], rules: [{ conditions: ['-'], outputs: {} }] }
  state.dirty = true; state.lastEval = null; state.validation = null; state.fx = null
  refreshAll()
  showCmxToast('新建 ' + key + '（编辑后点保存落库）')
}
async function saveTable () {
  const d = state.def; if (!d) return
  state.validation = null
  try {
    await apiJson('/api/flow/decisions', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(d),
    })
    state.dirty = false
    showCmxToast('已保存并热注册：' + d.key)
    await loadList()
    refreshView('content')
  } catch (e) {
    // 后端 validate 400：body 可能带 violations/诊断；提取展示。
    const body = e.body || {}
    const diags = body.violations || body.errors || (body.data && (body.data.violations || body.data.errors)) || null
    if (Array.isArray(diags) && diags.length) state.validation = diags.map((x) => (typeof x === 'string' ? x : (x.message || JSON.stringify(x))))
    else state.validation = [e.message]
    showCmxToast('保存失败：' + e.message, { level: 'error' })
    refreshView('content')
  }
}
async function deleteTable () {
  const d = state.def; if (!d) return
  if (typeof window !== 'undefined' && window.confirm && !window.confirm(`删除决策表「${d.key}」？此操作从库中移除并从引擎注销。`)) return
  try {
    await apiJson('/api/flow/decisions/' + enc(d.key), { method: 'DELETE' })
    showCmxToast('已删除：' + d.key)
    state.selectedKey = null; state.def = null; state.lastEval = null; state.dirty = false
    await loadList(); refreshAll()
  } catch (e) { showCmxToast('删除失败: ' + e.message, { level: 'error' }) }
}
async function doEvaluate () {
  const d = state.def; if (!d) return
  let facts
  try { facts = JSON.parse(state.factsDraft || '{}') } catch { showCmxToast('facts JSON 非法', { level: 'error' }); return }
  try {
    const r = await apiJson('/api/flow/decisions/evaluate', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ table: d, variables: facts }),
    })
    state.lastEval = { matchedRules: r.matchedRules || [], outputs: r.outputs || {} }
    showCmxToast(state.lastEval.matchedRules.length ? '命中 ' + state.lastEval.matchedRules.length + ' 条' : '无命中')
  } catch (e) { showCmxToast('试算失败: ' + e.message, { level: 'error' }); state.lastEval = null }
  refreshAll()
}

// ————————————————————— 工具 —————————————————————

function fmtVal (v) {
  if (v == null) return '∅'
  return typeof v === 'object' ? JSON.stringify(v) : String(v)
}

// ————————————————————— 样式（令牌锚 --sap*，light/dark 自动翻；hex 兜底） —————————————————————

function styleCss () {
  return `
  .dsd{
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
    --warn:var(--sapCriticalColor,var(--sapWarningColor,#9a6700));
    --red:var(--sapNegativeColor,var(--sapErrorColor,#cf222e));
    --brand-soft:color-mix(in srgb,var(--brand) 14%,var(--tile));
    --brand-line:color-mix(in srgb,var(--brand) 40%,var(--line));
    --ok-soft:color-mix(in srgb,var(--ok) 15%,var(--tile));
    --warn-soft:color-mix(in srgb,var(--warn) 18%,var(--tile));
    --red-soft:color-mix(in srgb,var(--red) 14%,var(--tile));
    --glow:color-mix(in srgb,var(--brand) 30%,transparent);
    --mono:ui-monospace,Menlo,Consolas,monospace;
    color-scheme:light dark;
    font:13px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC",sans-serif; color:var(--ink); background:var(--surface); height:100%; box-sizing:border-box; display:flex; flex-direction:column; }
  .dsd *{ box-sizing:border-box; }
  .dsd ui5-icon{ color:currentColor; }
  .dsd-ph{ color:var(--muted); padding:22px 10px; text-align:center; font-size:12px; }
  /* explorer */
  .dsd-search{ display:flex; gap:5px; padding:8px; border-bottom:1px solid var(--line-soft); background:var(--header); }
  .dsd-search input{ flex:1; min-width:0; font:inherit; font-size:12px; height:28px; border:1px solid var(--line); border-radius:7px; padding:0 8px; background:var(--tile); color:var(--ink); }
  .dsd-btn{ display:inline-flex; align-items:center; gap:4px; font:inherit; font-size:12px; font-weight:600; padding:6px 12px; border:1px solid var(--line); border-radius:8px; background:var(--tile); color:var(--ink); cursor:pointer; transition:all .13s; white-space:nowrap; }
  .dsd-btn:hover{ border-color:var(--brand-line); color:var(--brand); background:var(--brand-soft); }
  .dsd-btn ui5-icon{ width:14px; height:14px; }
  .dsd-btn.primary{ background:var(--brand); color:var(--brand-ink); border-color:var(--brand); box-shadow:0 2px 8px var(--glow); }
  .dsd-btn.primary:hover{ background:color-mix(in srgb,var(--brand) 88%,#000); color:var(--brand-ink); }
  .dsd-btn.danger{ color:var(--red); border-color:color-mix(in srgb,var(--red) 42%,var(--line)); }
  .dsd-btn.danger:hover{ background:var(--red-soft); border-color:var(--red); }
  .dsd-list-head{ display:flex; justify-content:space-between; padding:8px 12px; font-size:11px; color:var(--muted); text-transform:uppercase; font-weight:800; }
  .dsd-list-head span{ background:var(--brand-soft); color:var(--brand-d); border-radius:20px; padding:1px 8px; }
  .dsd-list{ flex:1; overflow-y:auto; padding:0 8px 8px; }
  .dsd-row{ display:flex; align-items:center; gap:6px; width:100%; text-align:left; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; background:var(--tile); cursor:pointer; transition:all .13s; }
  .dsd-row:hover{ border-color:var(--brand-line); }
  .dsd-row.active{ background:var(--brand-soft); border-color:var(--brand); }
  .dsd-row-main{ flex:1; min-width:0; } .dsd-row-main b{ display:block; font-size:13px; font-family:var(--mono); } .dsd-row-main small{ color:var(--muted); font-size:11px; }
  .dsd-badge{ font-size:10.5px; font-weight:700; padding:2px 8px; border-radius:20px; white-space:nowrap; background:var(--line-soft); color:var(--muted); font-family:var(--mono); }
  .dsd-newbar{ display:flex; gap:5px; padding:8px; border-top:1px solid var(--line-soft); background:var(--header); }
  .dsd-newbar input{ flex:1; min-width:0; font:inherit; font-size:12px; height:30px; border:1px solid var(--line); border-radius:7px; padding:0 8px; background:var(--tile); color:var(--ink); }
  .dsd-empty,.dsd-blank{ display:flex; flex-direction:column; align-items:center; gap:8px; color:var(--muted); padding:40px 16px; text-align:center; }
  .dsd-empty ui5-icon,.dsd-blank ui5-icon{ width:30px; height:30px; opacity:.5; }
  .dsd-blank b{ font-size:14px; color:var(--ink); } .dsd-blank-sub{ font-size:12px; } .dsd-blank.sm{ padding:20px; font-size:12px; }
  /* content 工具栏 + 网格 */
  .dsd-toolbar{ display:flex; align-items:center; gap:8px; flex-wrap:wrap; padding:10px 14px; border-bottom:1px solid var(--line-soft); background:var(--header); }
  .dsd-title{ font-size:14px; font-weight:700; font-family:var(--mono); } .dsd-sp{ flex:1; }
  .dsd-dirty{ font-size:11px; color:var(--muted); } .dsd-dirty.on{ color:var(--warn); font-weight:700; }
  .dsd-hp{ border:1px solid var(--line); border-radius:8px; padding:5px 8px; font-size:12px; background:var(--tile); color:var(--ink); }
  .dsd-hp:focus{ outline:none; border-color:var(--brand-line); box-shadow:0 0 0 3px var(--brand-soft); }
  .dsd-verr{ margin:10px 14px 0; border:1px solid color-mix(in srgb,var(--red) 40%,var(--line)); background:var(--red-soft); color:var(--red); border-radius:8px; padding:8px 11px; font-size:12px; }
  .dsd-verr b{ display:block; margin-bottom:3px; }
  .dsd-gridwrap{ flex:1; overflow:auto; margin:12px 14px 0; border:1px solid var(--line); border-radius:11px; }
  .dsd-grid{ border-collapse:collapse; width:100%; font-size:12px; }
  .dsd-grid th,.dsd-grid td{ border:1px solid var(--line); padding:3px 5px; vertical-align:top; }
  .dsd-grid th{ background:color-mix(in srgb,var(--brand) 5%,var(--header)); position:relative; }
  .dsd-grid th.dsd-in,.dsd-grid td.dsd-in{ background:color-mix(in srgb,var(--brand) 6%,transparent); position:relative; }
  .dsd-grid th.dsd-out,.dsd-grid td.dsd-out{ background:color-mix(in srgb,var(--ok) 7%,transparent); }
  .dsd-idx{ color:var(--muted); text-align:center; width:30px; font-family:var(--mono); position:relative; }
  .dsd-h{ width:calc(100% - 16px); border:none; background:transparent; font-weight:600; font-size:12px; color:inherit; padding:2px 3px; }
  .dsd-h:focus,.dsd-cell:focus{ outline:none; box-shadow:0 0 0 2px var(--brand); border-radius:4px; background:var(--tile); }
  .dsd-cell{ width:100%; min-width:90px; border:1px solid transparent; background:transparent; font:12px var(--mono); color:inherit; padding:3px 4px; border-radius:4px; }
  .dsd-cell:hover{ border-color:var(--line); background:var(--tile); }
  .dsd-fx{ position:absolute; right:2px; top:2px; font-size:9px; padding:0 4px; border:1px solid var(--line); border-radius:4px; background:var(--tile); color:var(--brand); cursor:pointer; line-height:15px; }
  .dsd-fx:hover{ border-color:var(--brand); box-shadow:0 0 0 2px var(--brand-soft); }
  .dsd-x{ border:none; background:transparent; color:var(--red); cursor:pointer; font-size:14px; line-height:1; padding:0 4px; border-radius:5px; }
  .dsd-x:hover{ background:var(--red-soft); }
  th.dsd-in .dsd-x,th.dsd-out .dsd-x{ position:absolute; right:2px; top:2px; }
  .dsd-ops{ width:30px; text-align:center; }
  .dsd-grid tr.hit td{ background:var(--warn-soft); }
  .dsd-hit-dot{ display:inline-block; width:7px; height:7px; border-radius:50%; background:var(--warn); margin-left:4px; vertical-align:middle; }
  .dsd-hint{ padding:8px 14px 12px; font-size:11px; color:var(--muted); } .dsd-hint code{ font-family:var(--mono); background:var(--brand-soft); padding:0 4px; border-radius:4px; }
  /* fx 面板 */
  .dsd-fxpanel{ position:sticky; bottom:0; margin:10px 14px 0; border:1px solid var(--brand-line); border-radius:12px; background:linear-gradient(180deg,var(--brand-soft),transparent),var(--tile); box-shadow:0 -6px 22px -10px var(--glow); padding:11px 13px; }
  .fx-hd{ font-weight:600; font-size:12px; display:flex; align-items:center; gap:8px; margin-bottom:8px; }
  .fx-valid{ font-size:11px; color:var(--muted); } .fx-valid.ok{ color:var(--ok); } .fx-valid.bad{ color:var(--red); }
  .fx-editrow{ display:flex; gap:8px; margin-bottom:8px; } .fx-input{ flex:1; border:1px solid var(--line); border-radius:8px; padding:7px 10px; font:13px var(--mono); color:var(--ink); background:var(--tile); }
  .fx-input:focus{ outline:none; border-color:var(--brand-line); box-shadow:0 0 0 3px var(--brand-soft); }
  .fx-cats{ max-height:140px; overflow:auto; } .fx-cat{ margin-bottom:5px; } .fx-catn{ font-size:11px; color:var(--muted); margin-right:6px; }
  .fx-chip{ border:1px solid var(--line); border-radius:12px; background:var(--tile); padding:2px 8px; margin:2px; font-size:11px; cursor:pointer; color:inherit; font-family:var(--mono); transition:all .13s; }
  .fx-chip:hover{ border-color:var(--brand); box-shadow:0 0 0 2px var(--brand-soft); } .fx-chip small{ color:var(--muted); margin-left:3px; }
  /* property */
  .dsd-prop-head{ padding:12px 14px; border-bottom:1px solid var(--line-soft); background:var(--header); font-size:14px; font-weight:750; display:flex; align-items:baseline; gap:8px; } .dsd-prop-head small{ color:var(--muted); font-size:12px; font-family:var(--mono); font-weight:400; }
  .dsd-prop-body{ padding:0 0 14px; }
  .dsd-kv{ display:flex; gap:8px; padding:4px 14px; align-items:baseline; } .dsd-kv span{ color:var(--muted); width:64px; font-size:11px; flex:0 0 auto; }
  .dsd-sec{ font-size:11px; font-weight:800; color:var(--brand-d); text-transform:uppercase; margin:14px 14px 8px; padding-bottom:5px; border-bottom:1px solid var(--line-soft); }
  .dsd-facts{ width:calc(100% - 28px); margin:0 14px; height:110px; font:12px/1.5 var(--mono); border:1px solid var(--line); border-radius:8px; padding:8px; resize:vertical; background:var(--tile); color:var(--ink); }
  .dsd-eval-bar{ padding:8px 14px 0; display:flex; gap:8px; }
  .dsd-eval-hits{ padding:0 14px; font-size:12px; }
  .dsd-hit-chip{ display:inline-block; font-family:var(--mono); font-size:11px; font-weight:700; background:var(--warn-soft); color:var(--warn); padding:1px 7px; border-radius:6px; margin:0 3px; }
  .dsd-vtab{ width:calc(100% - 28px); margin:0 14px; border-collapse:collapse; font-size:12px; }
  .dsd-vtab td{ padding:5px 8px; border-bottom:1px solid var(--line-soft); vertical-align:top; word-break:break-all; font-family:var(--mono); }
  .dsd-vtab td:first-child{ font-weight:700; color:var(--muted); white-space:nowrap; width:40%; }
  .dsd-muted{ color:var(--muted); font-size:12px; padding:0 14px; }
  .dsd-toast{ position:fixed; left:50%; bottom:20px; transform:translateX(-50%); background:#0d1117; color:#fff; padding:9px 16px; border-radius:9px; font-size:12.5px; font-weight:600; opacity:0; pointer-events:none; transition:opacity .2s; z-index:30; border:1px solid rgba(255,255,255,.08); }
  .dsd-toast.show{ opacity:1; } .dsd-toast.err{ background:var(--red); }
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
