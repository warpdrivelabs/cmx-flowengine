/**
 * 身份管理工作台 —— native_pages 四区（P0-c，仅 FLOW_IDENTITY_MODE=local 时有意义）。
 *
 * 对标字典维护工作台：一个四区页管四类身份主数据（组织/角色/岗位/用户），落 fid_* 表。
 *   explorer：实体类型切换（组织机构 / 角色 / 岗位 / 用户）+ 该类记录列表。
 *   content ：选中记录的编辑表单（新增/更新）；顶部工具条「新增 / 保存 / 删除」。
 *   property：字段说明 + 当前身份模式（local/external）提示；external 模式只读横幅。
 *
 * 数据源：GET /api/flow/identity/mode、GET/POST /api/flow/identity/{entity}、
 *        DELETE /api/flow/identity/{entity}/{id}、POST /api/flow/identity/users/{id}/roles。
 *
 * S4 抽核纪律：与 todo-center/design-workbench 同款 CFG 接缝——门户默认值=同源+cookie，逐字节
 * 零回归；组件壳可 configure 覆盖 apiBase/authHeaders。核心只经 CFG 触达外部。
 */

const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

// 四类实体的元信息：URL 段、标签、图标、字段定义（驱动 content 表单渲染）。
const ENTITIES = [
  {
    key: 'orgs', label: '组织机构', icon: 'org-chart', single: '组织',
    fields: [
      { k: 'code', label: '编码', required: true, hint: '组织唯一编码，如 FIN' },
      { k: 'name', label: '名称', required: true, hint: '显示名，如 财务部' },
      { k: 'parentId', label: '上级组织 id', hint: '留空为顶级；填上级组织的 id' },
      { k: 'leaderUserId', label: '部门领导 (user id)', hint: '关系型审批「部门领导」解析到此人' },
      { k: 'sortOrder', label: '排序', type: 'number', hint: '同级排序，小在前' },
    ],
  },
  {
    key: 'roles', label: '角色', icon: 'role', single: '角色',
    fields: [
      { k: 'code', label: '编码', required: true, hint: '角色 code，BPMN 里 role(code) 引用它' },
      { k: 'name', label: '名称', required: true, hint: '如 财务审批角色' },
    ],
  },
  {
    key: 'positions', label: '岗位', icon: 'employee', single: '岗位',
    fields: [
      { k: 'code', label: '编码', required: true, hint: '岗位 code，position(code) 引用' },
      { k: 'name', label: '名称', required: true, hint: '如 财务经理' },
    ],
  },
  {
    key: 'users', label: '用户', icon: 'person-placeholder', single: '用户',
    fields: [
      { k: 'username', label: '用户名', required: true, hint: '登录名/工号' },
      { k: 'name', label: '姓名', hint: '显示姓名' },
      { k: 'orgId', label: '所属组织 id', hint: '决定「发起人上级」解析到哪个部门的领导' },
    ],
  },
]

const state = {
  entity: 'orgs',       // 当前实体类型
  items: [],            // 当前实体列表
  selected: null,       // 当前编辑的记录（null=未选，新增态用 draft）
  draft: null,          // 编辑中的字段值 { k: v }
  mode: 'external',     // 身份模式（探测 /identity/mode）
  editable: false,      // external 模式只读
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
    const t = hostRoot(host)?.querySelector?.('.idn-toast')
    if (t) { t.textContent = msg; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2600) }
  }
}

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__idnView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'explorer' || view === 'content') { loadMode(); loadList() } })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}

function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__idnView !== view) continue
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

function curEntity () { return ENTITIES.find((e) => e.key === state.entity) || ENTITIES[0] }

// ————————————————————— explorer 区 —————————————————————

function explorerHtml () {
  const tabs = ENTITIES.map((e) => `<button class="idn-tab ${state.entity === e.key ? 'active' : ''}" data-entity="${e.key}">
    <span class="idn-tab-ic"><ui5-icon name="${e.icon}"></ui5-icon></span>
    <span>${esc(e.label)}</span>
  </button>`).join('')
  const ent = curEntity()
  const rows = state.items.length
    ? state.items.map((it) => {
        const title = it.name || it.username || it.code || it.id
        const sub = it.code || it.username || it.id
        const active = state.selected && state.selected.id === it.id
        return `<button class="idn-row ${active ? 'active' : ''}" data-id="${esc(it.id)}">
          <b>${esc(title)}</b><small>${esc(sub)}</small>
        </button>`
      }).join('')
    : `<div class="idn-empty"><ui5-icon name="${ent.icon}"></ui5-icon><span>暂无${esc(ent.single)}</span></div>`
  return `<section class="idn idn-explorer">
    <div class="idn-tabs">${tabs}</div>
    <div class="idn-list-head"><b>${esc(ent.label)}列表</b><span>${state.items.length}</span></div>
    <div class="idn-list">${rows}</div>
  </section>`
}

// ————————————————————— content 区（编辑表单） —————————————————————

function contentHtml () {
  const ent = curEntity()
  const banner = state.editable ? '' :
    `<div class="idn-banner"><ui5-icon name="locked"></ui5-icon> 当前为<b>外接身份</b>模式（external），内建身份只读。如需在此维护，设 <code>FLOW_IDENTITY_MODE=local</code> 重启服务。</div>`
  const d = state.draft || {}
  const isNew = !state.selected
  const fields = ent.fields.map((f) => {
    const val = d[f.k] ?? ''
    const dis = state.editable ? '' : 'disabled'
    return `<div class="idn-field">
      <label>${esc(f.label)}${f.required ? ' <em>*</em>' : ''}</label>
      <input data-f="${esc(f.k)}" type="${f.type === 'number' ? 'number' : 'text'}" value="${esc(val)}" ${dis} placeholder="${esc(f.hint || '')}">
      ${f.hint ? `<div class="idn-hint">${esc(f.hint)}</div>` : ''}
    </div>`
  }).join('')
  const toolbar = `<div class="idn-toolbar">
    <b class="idn-title">${isNew ? '新增' : '编辑'}${esc(ent.single)}${state.selected ? `：${esc(state.selected.name || state.selected.username || state.selected.code || state.selected.id)}` : ''}</b>
    <span class="idn-tb-sp"></span>
    <button class="idn-btn" data-act="new" ${state.editable ? '' : 'disabled'}><ui5-icon name="add"></ui5-icon> 新增</button>
    <button class="idn-btn primary" data-act="save" ${state.editable ? '' : 'disabled'}><ui5-icon name="save"></ui5-icon> 保存</button>
    <button class="idn-btn danger" data-act="delete" ${state.editable && state.selected ? '' : 'disabled'}><ui5-icon name="delete"></ui5-icon> 删除</button>
  </div>`
  // 用户实体：多一个角色多选（逗号分隔 role id，简化版）。
  const rolesField = ent.key === 'users' ? `<div class="idn-field">
    <label>角色（role id，逗号分隔）</label>
    <input data-f="__roleIds" type="text" value="${esc(d.__roleIds ?? '')}" ${state.editable ? '' : 'disabled'} placeholder="如 role_fin,role_audit；保存用户后单独提交">
    <div class="idn-hint">保存用户后点「保存角色」提交到 fid_user_role。</div>
    <button class="idn-btn" data-act="save-roles" ${state.editable && state.selected ? '' : 'disabled'} style="margin-top:6px"><ui5-icon name="role"></ui5-icon> 保存角色</button>
  </div>` : ''
  return `<section class="idn idn-content">
    ${toolbar}
    ${banner}
    <div class="idn-form">${fields}${rolesField}</div>
    <div class="idn-toast"></div>
  </section>`
}

// ————————————————————— property 区（说明/模式） —————————————————————

function propertyHtml () {
  const ent = curEntity()
  const modeTag = state.mode === 'local'
    ? '<span class="idn-mode local">内建身份 local</span>'
    : '<span class="idn-mode ext">外接身份 external</span>'
  const rel = ent.key === 'orgs'
    ? `<div class="idn-note"><b>关系型审批</b>：组织的「部门领导 (leaderUserId)」是 BPMN 里 <code>orgLeader</code>、<code>initiatorLeader</code> 的解析来源。给组织设好领导，流程才能路由到「部门领导」。</div>`
    : ent.key === 'roles'
      ? `<div class="idn-note"><b>用法</b>：角色 <code>code</code> 就是 BPMN 候选表达式 <code>role(code)</code> 里的 code。给用户挂角色后，该角色的任务会落到这些用户的候选池。</div>`
      : ent.key === 'users'
        ? `<div class="idn-note"><b>关系型审批</b>：用户的「所属组织 (orgId)」决定 <code>initiatorLeader</code>（发起人上级）解析到哪个部门的领导。</div>`
        : `<div class="idn-note"><b>用法</b>：岗位 <code>code</code> 对应 BPMN <code>position(code)</code>。</div>`
  return `<section class="idn idn-property">
    <div class="idn-prop-head"><b>身份模式</b>${modeTag}</div>
    <div class="idn-prop-body">
      <div class="idn-sec">${esc(ent.label)}说明</div>
      ${rel}
      <div class="idn-sec">字段</div>
      <table class="idn-ftab">
        ${ent.fields.map((f) => `<tr><td>${esc(f.label)}</td><td>${esc(f.hint || '')}</td></tr>`).join('')}
      </table>
    </div>
  </section>`
}

// ————————————————————— 绑定 —————————————————————

function bind (root, view, host) {
  if (view === 'explorer') {
    root.querySelectorAll('[data-entity]').forEach((b) => b.addEventListener('click', () => {
      state.entity = b.dataset.entity; state.selected = null; state.draft = blankDraft()
      loadList(); refreshAll()
    }))
    root.querySelectorAll('[data-id]').forEach((b) => b.addEventListener('click', () => selectRecord(b.dataset.id)))
  }
  if (view === 'content') {
    root.querySelectorAll('[data-f]').forEach((inp) => inp.addEventListener('input', () => {
      if (!state.draft) state.draft = blankDraft()
      state.draft[inp.dataset.f] = inp.value
    }))
    root.querySelector('[data-act="new"]')?.addEventListener('click', () => { state.selected = null; state.draft = blankDraft(); refreshAll() })
    root.querySelector('[data-act="save"]')?.addEventListener('click', () => saveRecord())
    root.querySelector('[data-act="delete"]')?.addEventListener('click', () => deleteRecord())
    root.querySelector('[data-act="save-roles"]')?.addEventListener('click', () => saveRoles())
  }
}

function blankDraft () { return {} }

// ————————————————————— 数据/动作 —————————————————————

async function loadMode () {
  try {
    const d = await apiJson('/api/flow/identity/mode')
    state.mode = d.mode || 'external'
    state.editable = !!d.editable
  } catch { state.mode = 'external'; state.editable = false }
  refreshView('content'); refreshView('property')
}

async function loadList () {
  state.loading = true
  try {
    const d = await apiJson('/api/flow/identity/' + enc(state.entity))
    state.items = d.items || []
  } catch (e) { toast('加载失败: ' + e.message); state.items = [] }
  state.loading = false
  refreshView('explorer')
}

function selectRecord (id) {
  const it = state.items.find((x) => x.id === id)
  if (!it) return
  state.selected = it
  // 用后端字段名回填 draft（后端返回下划线键，映射回表单 camelCase key）。
  state.draft = recordToDraft(it)
  refreshAll()
}

// 后端行（下划线键）→ 表单 draft（表单字段 key）。
function recordToDraft (it) {
  const d = { id: it.id }
  const ent = curEntity()
  for (const f of ent.fields) {
    // 后端列名：camelCase → snake_case 映射（parentId→parent_id 等）。
    const col = f.k.replace(/[A-Z]/g, (m) => '_' + m.toLowerCase())
    d[f.k] = it[col] ?? it[f.k] ?? ''
  }
  return d
}

async function saveRecord () {
  if (!state.editable) { toast('外接模式只读'); return }
  const ent = curEntity()
  const d = state.draft || {}
  // 必填校验。
  for (const f of ent.fields) {
    if (f.required && !String(d[f.k] ?? '').trim()) { toast(`请填写「${f.label}」`); return }
  }
  // 组装 body（带上 id 便于 upsert 更新；新增时前端给个基于 code/username 的 id）。
  const body = { ...d }
  if (!body.id) body.id = genId(ent, d)
  try {
    const r = await apiJson('/api/flow/identity/' + enc(state.entity), {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    toast('已保存: ' + (r.id || body.id))
    await loadList()
    // 重新选中刚保存的记录。
    selectRecord(r.id || body.id)
  } catch (e) { toast('保存失败: ' + e.message) }
}

// 新增时生成 id：实体前缀 + code/username（对齐后端「前端带 id」约定，避免后端兜底占位）。
function genId (ent, d) {
  const base = (d.code || d.username || 'new').toString().trim().replace(/[^A-Za-z0-9_-]+/g, '_').toLowerCase()
  const prefix = ent.key === 'orgs' ? 'org' : ent.key === 'roles' ? 'role' : ent.key === 'positions' ? 'pos' : 'usr'
  return `${prefix}_${base}`
}

async function deleteRecord () {
  if (!state.editable || !state.selected) return
  if (!confirm(`确认删除「${state.selected.name || state.selected.username || state.selected.code || state.selected.id}」？`)) return
  try {
    await apiJson('/api/flow/identity/' + enc(state.entity) + '/' + enc(state.selected.id), { method: 'DELETE' })
    toast('已删除')
    state.selected = null; state.draft = blankDraft()
    await loadList(); refreshAll()
  } catch (e) { toast('删除失败: ' + e.message) }
}

async function saveRoles () {
  if (!state.editable || !state.selected) return
  const ids = String((state.draft || {}).__roleIds || '').split(',').map((s) => s.trim()).filter(Boolean)
  try {
    await apiJson('/api/flow/identity/users/' + enc(state.selected.id) + '/roles', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ roleIds: ids }),
    })
    toast(`已设置 ${ids.length} 个角色`)
  } catch (e) { toast('设置角色失败: ' + e.message) }
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  :host, .idn { --bg:#f6f8fa; --panel:#fff; --ink:#1f2328; --muted:#656d76; --line:#d0d7de; --line-soft:#eaeef2; --brand:#0969da; --brand-d:#0a4d8c; --brand-soft:#ddf4ff; --ok:#1a7f37; --ok-soft:#e6f6eb; --danger:#cf222e; --mono:ui-monospace,Menlo,Consolas,monospace; }
  .idn { font: 13px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC",sans-serif; color: var(--ink); height:100%; box-sizing:border-box; display:flex; flex-direction:column; }
  .idn * { box-sizing: border-box; }
  /* explorer */
  .idn-tabs { display:flex; gap:4px; padding:8px; flex-wrap:wrap; border-bottom:1px solid var(--line-soft); }
  .idn-tab { display:flex; align-items:center; gap:5px; font:inherit; font-size:12px; font-weight:700; padding:6px 10px; border:1px solid var(--line); border-radius:8px; background:#fff; color:var(--muted); cursor:pointer; }
  .idn-tab.active { background:var(--brand); color:#fff; border-color:var(--brand); }
  .idn-tab-ic ui5-icon { width:14px; height:14px; }
  .idn-list-head { display:flex; justify-content:space-between; align-items:center; padding:8px 12px; font-size:11px; color:var(--muted); text-transform:uppercase; font-weight:800; }
  .idn-list-head span { background:var(--brand-soft); color:var(--brand-d); border-radius:20px; padding:1px 8px; }
  .idn-list { flex:1; overflow-y:auto; padding:0 8px 8px; }
  .idn-row { display:flex; flex-direction:column; align-items:flex-start; width:100%; text-align:left; padding:8px 10px; border:1px solid var(--line-soft); border-radius:8px; margin-bottom:5px; background:#fff; cursor:pointer; }
  .idn-row:hover { border-color:var(--brand); }
  .idn-row.active { background:var(--brand-soft); border-color:var(--brand); }
  .idn-row b { font-size:13px; } .idn-row small { color:var(--muted); font-size:11px; font-family:var(--mono); }
  .idn-empty { display:flex; flex-direction:column; align-items:center; gap:8px; color:var(--muted); padding:36px 16px; font-size:12.5px; }
  .idn-empty ui5-icon { width:28px; height:28px; opacity:.5; }
  /* content */
  .idn-content { padding:0; }
  .idn-toolbar { display:flex; align-items:center; gap:6px; padding:10px 14px; border-bottom:1px solid var(--line-soft); background:var(--panel); flex-wrap:wrap; }
  .idn-title { font-size:14px; } .idn-tb-sp { flex:1; }
  .idn-btn { display:inline-flex; align-items:center; gap:4px; font:inherit; font-size:12px; font-weight:700; padding:6px 12px; border:1px solid var(--line); border-radius:8px; background:#fff; color:var(--ink); cursor:pointer; }
  .idn-btn ui5-icon { width:14px; height:14px; }
  .idn-btn.primary { background:var(--brand); color:#fff; border-color:var(--brand); }
  .idn-btn.danger { color:var(--danger); border-color:#ffc9c9; }
  .idn-btn:disabled { opacity:.45; cursor:not-allowed; }
  .idn-banner { margin:12px 14px 0; padding:10px 14px; background:#fff8e6; border:1px solid #f0e0b0; border-left:3px solid #9a6700; border-radius:0 8px 8px 0; font-size:12.5px; color:#6b5200; }
  .idn-banner code { font-family:var(--mono); background:#fff; padding:1px 5px; border-radius:4px; }
  .idn-form { padding:16px 14px; max-width:560px; }
  .idn-field { margin-bottom:14px; }
  .idn-field label { display:block; font-size:11px; font-weight:700; color:var(--muted); text-transform:uppercase; margin-bottom:4px; }
  .idn-field label em { color:var(--danger); font-style:normal; }
  .idn-field input { width:100%; font:inherit; font-size:13px; border:1px solid var(--line); border-radius:7px; padding:7px 10px; }
  .idn-field input:focus { outline:none; border-color:var(--brand); box-shadow:0 0 0 3px var(--brand-soft); }
  .idn-field input:disabled { background:#f6f8fa; color:var(--muted); }
  .idn-hint { font-size:11px; color:var(--muted); margin-top:3px; }
  /* property */
  .idn-prop-head { display:flex; align-items:center; gap:8px; padding:12px 14px; border-bottom:1px solid var(--line-soft); font-size:14px; font-weight:750; }
  .idn-mode { font-size:11px; font-weight:700; padding:2px 9px; border-radius:20px; }
  .idn-mode.local { background:var(--ok-soft); color:var(--ok); } .idn-mode.ext { background:#eee; color:#666; }
  .idn-prop-body { padding:14px; }
  .idn-sec { font-size:11px; font-weight:800; color:var(--brand-d); text-transform:uppercase; margin:14px 0 8px; padding-bottom:5px; border-bottom:1px solid var(--line-soft); }
  .idn-note { background:var(--brand-soft); border:1px solid #cfe4ff; border-radius:8px; padding:10px 12px; font-size:12.5px; color:var(--brand-d); }
  .idn-note code { font-family:var(--mono); background:#fff; padding:1px 5px; border-radius:4px; }
  .idn-ftab { width:100%; border-collapse:collapse; font-size:12px; }
  .idn-ftab td { padding:6px 8px; border-bottom:1px solid var(--line-soft); vertical-align:top; }
  .idn-ftab td:first-child { font-weight:700; white-space:nowrap; color:var(--muted); }
  /* toast */
  .idn-toast { position:fixed; left:50%; bottom:20px; transform:translateX(-50%); background:#0d1117; color:#fff; padding:9px 16px; border-radius:9px; font-size:12.5px; font-weight:600; opacity:0; pointer-events:none; transition:opacity .2s; z-index:30; }
  .idn-toast.show { opacity:1; }
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
