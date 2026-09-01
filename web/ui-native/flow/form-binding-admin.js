/**
 * 表单注册表管理工作台 —— native_pages 四区（002 号债务落地，方案见
 * documents/20260819_cmx-flowengine_表单注册表维护页面方案.md）。
 *
 * 对标 identity-workbench：一个四区页管 cmx_flow_form_binding 全部绑定（workspace/html/native）。
 *   explorer：kind 筛选 tab（全部/workspace/html/native）+ 关键字过滤 + 绑定卡片列表 + 新增。
 *   content ：选中绑定的编辑表单；字段按 kind 联动显隐；formKey 编辑态禁改（主键，改 key=删旧建新）。
 *   property：字段说明 + kind 联动规则 + 消费方/seed 复位警示。
 *
 * 数据源：GET /api/flow/forms（列表）、GET /api/flow/forms/{key}（存在性检查）、
 *        POST /api/flow/forms（upsert 整行）、POST /api/flow/forms/delete（幂等删除，返回 deleted 行数）。
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

// kind 元数据：标签 + 图标 + 各自的目标坐标字段（content 表单联动显隐 + 必填校验）。
const KINDS = [
  { key: 'all', label: '全部', icon: 'list' },
  { key: 'workspace', label: 'workspace', icon: 'workspace', fields: ['workspaceNode'] },
  { key: 'html', label: 'html', icon: 'html-source', fields: ['htmlPage', 'file'] },
  { key: 'native', label: 'native', icon: 'javascript', fields: ['nativePage', 'nativeView'] },
]

// 表单字段定义（驱动 content 渲染 + property 说明；k=draft 键，与 POST body 驼峰对齐）。
const FIELDS = [
  { k: 'formKey', label: 'formKey', required: true, hint: '主键；BPMN 节点 cmx:formKey 引用它。编辑态禁改，变更 key = 删旧建新。格式 [A-Za-z0-9._-]' },
  { k: 'kind', label: 'kind', type: 'select', options: ['workspace', 'html', 'native'], required: true, hint: '表单类型，决定待办中心打开任务的方式（三种方式的区别见右侧「kind 与打开方式」）' },
  { k: 'title', label: '标题', required: true, hint: '列表 / 待办 Tab 显示名' },
  { k: 'workspaceNode', label: '工作区节点 id', kind: 'workspace', required: true, hint: 'kind=workspace 必填；门户工作区节点库的完整 node id（如 flow-form-expense）——指向一套预先配置好的多区域工作台布局，见右侧说明' },
  { k: 'htmlPage', label: 'html 页面 id', kind: 'html', required: true, hint: 'kind=html 必填；html_pages 的页面 id（如 flow-pay-review-form）' },
  { k: 'file', label: 'html 文件名', kind: 'html', hint: 'doc-loader 类表单按 DAM+file 定位单据定义时的文件名' },
  { k: 'nativePage', label: 'native 页 id', kind: 'native', required: true, hint: 'kind=native 必填；native_pages 的页面 id（哪张页）' },
  { k: 'nativeView', label: 'native view', kind: 'native', hint: '用该页的哪个视图渲染（native 页可导出多个视图，即 views 映射的键；如 content/explorer/property），缺省 content（页面主视图惯例名）' },
  { k: 'bizTable', label: '业务表名', hint: '单据所在业务表（待办投影 / 反查用），建议填写' },
  { k: 'pkField', label: '单据主键字段', hint: 'bizTable 的主键字段名' },
  { k: 'domain', label: 'domain', hint: 'DAM 域（如 fi/wf）。只用于表单页定位业务数据/元数据，不影响流程流转——配不配的区别见右侧「DAM 配置说明」' },
  { k: 'application', label: 'application', hint: 'DAM 应用（如 cmxfico），与 domain 配套' },
  { k: 'module', label: 'module', hint: 'DAM 模块（如 gl/mdm），与 domain 配套' },
  { k: 'console', label: '审批控制台', type: 'select', options: ['platform', 'none'], hint: 'platform=property 区挂平台审批台（默认）；none=表单自带审批操作，property 只显只读轨迹（如 MDM cr-form）' },
]

const state = {
  kind: 'all',        // explorer 筛选 tab
  keyword: '',        // 关键字过滤（formKey/标题）
  items: [],          // 全量绑定（过滤在前端做，量级小）
  selected: null,     // 当前编辑的绑定行（null = 新增态）
  draft: null,        // 编辑中的字段值 { k: v }
  hosts: new Set(),
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js；最严格五字符集合，文本/属性上下文皆安全）
const enc = encodeURIComponent

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装（cmx-data-comp/lib/cmx-page-helpers.js）；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

// 确认框：cmxConfirm（组件库）优先，回退 window.confirm（design-workbench deleteVersion 同款判据）。
async function confirmBox (message, confirmText = '确认') {
  const C = (typeof globalThis !== 'undefined' && globalThis.__cmxDataComp) || {}
  return typeof C.cmxConfirm === 'function'
    ? await C.cmxConfirm({ message, intent: 'danger', confirmText })
    : window.confirm(message)
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js；治理清单 B-05）

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__fbaView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => { render(); if (view === 'explorer' || view === 'content') loadList() })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}

function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__fbaView !== view) continue
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

// 当前 kind 的元信息（all 兜底第一个）。
function curKind () { return KINDS.find((k) => k.key === state.kind) || KINDS[0] }

// ————————————————————— explorer 区 —————————————————————

// 前端过滤：kind tab + 关键字（formKey/标题，大小写不敏感）。
function filteredItems () {
  const kw = state.keyword.trim().toLowerCase()
  return state.items.filter((it) => {
    if (state.kind !== 'all' && it.kind !== state.kind) return false
    if (!kw) return true
    return String(it.formKey || '').toLowerCase().includes(kw)
      || String(it.title || '').toLowerCase().includes(kw)
  })
}

// 绑定卡片（explorerHtml 与关键字过滤的局部刷新共用）。
function rowHtml (it) {
  const active = state.selected && state.selected.formKey === it.formKey
  return `<button class="fba-row ${active ? 'active' : ''}" data-key="${esc(it.formKey)}">
    <b>${esc(it.formKey)}</b>
    <small>${esc(it.title || '')}</small>
    <span class="fba-tags">
      <i class="fba-tag kind-${esc(it.kind || '')}">${esc(it.kind || '')}</i>
      ${it.console === 'none' ? '<i class="fba-tag none">console=none</i>' : ''}
      ${it.seeded ? '<i class="fba-tag seeded">内置</i>' : ''}
    </span>
  </button>`
}

// 列表区内的行点击绑定（整页渲染与局部刷新共用，防局部刷新后行失去监听）。
function bindRows (listEl) {
  listEl?.querySelectorAll('[data-key]').forEach((b) => b.addEventListener('click', () => selectRecord(b.dataset.key)))
}

function explorerHtml () {
  const tabs = KINDS.map((k) => `<button class="fba-tab ${state.kind === k.key ? 'active' : ''}" data-kind="${k.key}">
    <span class="fba-tab-ic"><ui5-icon name="${k.icon}"></ui5-icon></span>
    <span>${esc(k.label)}</span>
  </button>`).join('')
  const rows = filteredItems().map(rowHtml).join('')
  const total = filteredItems().length
  return `<section class="fba fba-explorer">
    <div class="fba-fixed-head">
      <div class="fba-tabs">${tabs}</div>
      <div class="fba-search">
        <ui5-input data-kw value="${esc(state.keyword)}" placeholder="过滤 formKey / 标题…"></ui5-input>
        <ui5-button icon="add" design="Emphasized" data-act="new">新增</ui5-button>
      </div>
    </div>
    <div class="fba-list-head"><b>表单绑定</b><span>${total} / ${state.items.length}</span></div>
    <div class="fba-list">${rows || '<div class="fba-empty"><ui5-icon name="form"></ui5-icon><span>暂无绑定</span></div>'}</div>
  </section>`
}

// ————————————————————— content 区（编辑表单） —————————————————————

// kind 联动可见的字段。
function fieldVisible (f) {
  if (!f.kind) return true
  return state.draft?.kind === f.kind
}

function contentHtml () {
  const d = state.draft || {}
  const isNew = !state.selected
  const seededBanner = state.selected?.seeded
    ? `<div class="fba-banner"><ui5-icon name="information"></ui5-icon> <b>内置示例条目</b>：引擎每次启动会由 seed 幂等重写——删除后重启复活，编辑的字段重启后<b>复位</b>。建议新增自己的绑定而非改内置条目。</div>`
    : ''
  const fields = FIELDS.map((f) => {
    if (!fieldVisible(f)) return ''
    const val = d[f.k] ?? ''
    const dis = (f.k === 'formKey' && !isNew) ? ' disabled' : ''   // formKey 主键，编辑态禁改
    let input
    if (f.type === 'select') {
      input = `<ui5-select data-f="${esc(f.k)}"${dis}>${f.options.map((o) => `<ui5-option value="${esc(o)}"${val === o ? ' selected' : ''}>${esc(o)}</ui5-option>`).join('')}</ui5-select>`
    } else {
      input = `<ui5-input data-f="${esc(f.k)}" value="${esc(val)}" placeholder="${esc(f.ph || '')}"${dis}></ui5-input>`
    }
    return `<div class="fba-field">
      <label>${esc(f.label)}${f.required ? ' <em>*</em>' : ''}</label>
      ${input}
      ${f.hint ? `<div class="fba-hint">${esc(f.hint)}</div>` : ''}
    </div>`
  }).join('')
  const toolbar = `<div class="fba-toolbar">
    <b class="fba-title">${isNew ? '新增绑定' : `编辑：${esc(state.selected.formKey)}`}</b>
    <span class="fba-tb-sp"></span>
    <ui5-button icon="add" data-act="new">新增</ui5-button>
    <ui5-button icon="save" design="Emphasized" data-act="save">保存</ui5-button>
    <ui5-button icon="delete" design="Negative" data-act="delete"${state.selected ? '' : ' disabled'}>删除</ui5-button>
  </div>`
  return `<section class="fba fba-content">
    ${toolbar}
    ${seededBanner}
    <div class="fba-form">${fields}</div>
    <div class="fba-toast"></div>
  </section>`
}

// ————————————————————— property 区（说明）—————————————————————

function propertyHtml () {
  const fieldRows = FIELDS.map((f) => {
    const scope = f.kind ? `（仅 kind=${f.kind}）` : ''
    return `<tr><td>${esc(f.label)}${scope}</td><td>${esc(f.hint || '')}</td></tr>`
  }).join('')
  return `<section class="fba fba-property">
    <div class="fba-prop-head"><b>字段说明</b><span class="fba-mode">cmx_flow_form_binding</span></div>
    <div class="fba-prop-body">
      <div class="fba-sec">kind 与打开方式</div>
      <div class="fba-note">
        待办中心点「办理」时按 kind 分派三种打开形态：<br>
        · <b>workspace</b>：不是"一张表单页"，而是打开一个<b>预先配置好的完整工作台布局</b>（门户工作区节点）——explorer / content / property 等每个区域都可挂多张页（多 tab），content 可同时有主表单、明细、附件多张页。平台向各区注入任务参数（taskId/bizTable/bizId 等），并在 property 区<b>叠加</b>审批视图（工作台自带视图保留在前）。适合主从单据等复杂办理界面；流程设计工作台的「编辑表单工作台」按钮即自动创建此类节点并注册绑定。<br>
        · <b>html</b>：content 指向<b>一张</b> html_pages 设计器页面，平台套"表单 + property 审批栏"的标准壳。<br>
        · <b>native</b>：content 指向<b>一张</b> native_pages 页的指定 view。一个 native 页可导出多个视图（views 映射，如 content/explorer/property），<b>nativeView 指定用哪个视图渲染</b>，缺省 content。视图名须与目标页实际导出的 views 键一致，否则渲染不出内容。<br>
        切换 kind 会清空坐标字段，防脏值写入。
      </div>
      <div class="fba-sec">DAM 配置说明（domain / application / module）</div>
      <div class="fba-note">
        DAM <b>只用于表单页定位业务数据/元数据</b>（随任务 props 传给目标页），<b>不影响流程流转与回调</b>。配不配取决于目标页怎么取数：<br>
        · <b>doc-loader 类单据页（配 file）</b>：按 <b>DAM + file</b> 定位单据定义与数据——必配，不配找不到单据；<br>
        · <b>cr-form 类按 DAM 构造数据坐标的页面</b>：建议配（此类页优先读工作区上下文，绑定值为兜底）；<br>
        · <b>自包含页面</b>（数据写死或按 bizTable/bizId 取数）：可不配，配了仅作展示标记。
      </div>
      <div class="fba-sec">字段</div>
      <table class="fba-ftab">${fieldRows}</table>
      <div class="fba-sec">消费方</div>
      <div class="fba-note">
        · <b>待办中心</b>：办理任务时 GET /forms/{key} 解析页坐标（有会话缓存，管理页改动后已打开的待办中心需刷新）；<br>
        · <b>流程设计工作台</b>：「编辑表单工作台」保存后自动注册 kind=workspace 绑定——已存在时仅更新 workspaceNode/title，其余字段保留；<br>
        · <b>部署脚本</b>：如 MDM deploy-mdm-flow.sh 经 POST /forms 注册（mdm.cr.review）。
      </div>
      <div class="fba-sec">注意</div>
      <div class="fba-note warn">「内置」角标条目由引擎 seed 每次启动幂等重写（删除复活、编辑复位）；删除任何绑定不影响流程流转，仅影响待办打开表单（退回兜底视图）。</div>
    </div>
  </section>`
}

// ————————————————————— 绑定 —————————————————————

function blankDraft () { return { kind: 'native', console: 'platform', nativeView: 'content' } }

function bind (root, view, host) {
  if (view === 'explorer') {
    root.querySelectorAll('[data-kind]').forEach((b) => b.addEventListener('click', () => {
      state.kind = b.dataset.kind
      refreshView('explorer')
    }))
    root.querySelector('[data-kw]')?.addEventListener('input', (e) => {
      state.keyword = e.target.value || ''
      // 只重渲列表区，防输入框失焦；行点击监听随之重绑。
      const listHead = root.querySelector('.fba-list-head span')
      const list = root.querySelector('.fba-list')
      if (list) { list.innerHTML = explorerListHtml(); bindRows(list) }
      if (listHead) listHead.textContent = `${filteredItems().length} / ${state.items.length}`
    })
    root.querySelector('[data-act="new"]')?.addEventListener('click', () => startNew())
    bindRows(root.querySelector('.fba-list'))
  }
  if (view === 'content') {
    root.querySelectorAll('[data-f]').forEach((inp) => inp.addEventListener('change', () => {
      if (!state.draft) state.draft = blankDraft()
      // ui5-input 取 .value property；ui5-select 取选中 option 的 value 属性。
      const key = inp.dataset.f
      const val = (inp.tagName === 'UI5-SELECT')
        ? (inp.selectedOption?.getAttribute('value') ?? '')
        : (inp.value ?? '')
      if (key === 'kind') {
        // 切 kind：清空全部坐标字段，防互斥残留脏值随保存写入。
        state.draft.kind = val
        for (const f of FIELDS) if (f.kind) delete state.draft[f.k]
        state.draft.kind = val
        if (val === 'native') state.draft.nativeView = 'content'
        refreshView('content')
        return
      }
      state.draft[key] = val
    }))
    root.querySelector('[data-act="new"]')?.addEventListener('click', () => startNew())
    root.querySelector('[data-act="save"]')?.addEventListener('click', () => saveBinding())
    root.querySelector('[data-act="delete"]')?.addEventListener('click', () => deleteBinding())
  }
}

// 列表区行 HTML（关键字过滤时局部刷新用，防 input 失焦）。
function explorerListHtml () {
  const rows = filteredItems().map(rowHtml).join('')
  return rows || '<div class="fba-empty"><ui5-icon name="form"></ui5-icon><span>暂无绑定</span></div>'
}

function startNew () {
  state.selected = null
  state.draft = blankDraft()
  refreshAll()
}

function selectRecord (formKey) {
  const it = state.items.find((x) => x.formKey === formKey)
  if (!it) return
  state.selected = it
  // 回填 draft（剔除 seeded 展示标记，仅取可编辑字段）。
  state.draft = {}
  for (const f of FIELDS) state.draft[f.k] = it[f.k] ?? ''
  refreshAll()
}

// ————————————————————— 数据/动作 —————————————————————

async function loadList () {
  try {
    const d = await apiJson('/api/flow/forms')
    state.items = d.bindings || []
  } catch (e) { toast('加载失败: ' + e.message); state.items = [] }
  refreshView('explorer')
}

async function saveBinding () {
  const d = state.draft || {}
  // 必填 + formKey 格式校验。
  if (!String(d.formKey || '').trim()) { toast('请填写 formKey'); return }
  if (!/^[A-Za-z0-9._-]+$/.test(d.formKey)) { toast('formKey 仅允许 [A-Za-z0-9._-]'); return }
  if (!String(d.title || '').trim()) { toast('请填写标题'); return }
  for (const f of FIELDS) {
    if (f.required && fieldVisible(f) && !String(d[f.k] ?? '').trim()) { toast(`请填写「${f.label}」`); return }
  }
  // 新建时存在性检查：upsert 整行覆盖语义，提示后放行。
  if (!state.selected) {
    try {
      const b = await apiJson('/api/flow/forms/' + enc(d.formKey))
      if (b && b.formKey) {
        const go = await confirmBox(`formKey「${d.formKey}」已存在，保存将整行覆盖。确认继续？`, '覆盖')
        if (!go) return
      }
    } catch { /* 查询失败不阻断保存 */ }
  }
  const body = {}
  for (const f of FIELDS) {
    const v = String(d[f.k] ?? '').trim()
    body[f.k] = v === '' ? (f.k === 'kind' || f.k === 'console' ? d[f.k] : null) : v
  }
  try {
    await apiJson('/api/flow/forms', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    toast('已保存: ' + d.formKey)
    await loadList()
    selectRecord(d.formKey)
  } catch (e) { toast('保存失败: ' + e.message) }
}

async function deleteBinding () {
  const it = state.selected
  if (!it) return
  const msg = `确认删除表单绑定「${it.formKey}」？\n在途待办的该表单将退回兜底视图（已打开的待办中心需刷新后生效）${it.seeded ? '；内置条目引擎重启后会重新写入' : ''}。`
  if (!(await confirmBox(msg, '删除'))) return
  try {
    const r = await apiJson('/api/flow/forms/delete', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ formKey: it.formKey }),
    })
    toast(r && r.deleted ? '已删除: ' + it.formKey : '条目本不存在，视为成功: ' + it.formKey)
    state.selected = null; state.draft = blankDraft()
    await loadList(); refreshAll()
  } catch (e) { toast('删除失败: ' + e.message) }
}

// ————————————————————— 样式（Neo 主题：色值全部 --sap*/--neo* 变量派生，light/dark 自动跟随） —————————————————————

function styleCss () {
  return `
  :host, .fba {
    --bg: var(--sapBackgroundColor, #f6f8fa);
    --panel: var(--sapList_Background, #fff);
    --panel-head: var(--sapList_HeaderBackground, #f5f6f7);
    --ink: var(--sapTextColor, #1d2d3e);
    --muted: var(--sapContent_LabelColor, #6a6d70);
    --line: var(--sapGroup_ContentBorderColor, #d9d9d9);
    --line-soft: var(--neo-border-subtle, var(--sapTile_SeparatorColor, #e9e9e9));
    --brand: var(--neo-accent, #00b4d8);
    --brand-soft: color-mix(in srgb, var(--neo-accent, #00b4d8) 12%, transparent);
    --ok: var(--sapPositiveColor, #107e3e);
    --warn: var(--neo-warn, #f59e0b);
    --warn-soft: color-mix(in srgb, var(--neo-warn, #f59e0b) 12%, transparent);
    --danger: var(--sapNegativeColor, #bb0000);
    --mono: ui-monospace, Menlo, Consolas, monospace;
  }
  .fba { font: 13px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif; color: var(--ink); height: 100%; box-sizing: border-box; display: flex; flex-direction: column; background: var(--bg); }
  .fba * { box-sizing: border-box; }
  /* explorer */
  .fba-fixed-head { position: sticky; top: 0; z-index: 6; background: var(--bg); }
  .fba-tabs { display: flex; gap: 4px; padding: 8px; flex-wrap: wrap; border-bottom: 1px solid var(--line-soft); }
  .fba-tab { display: flex; align-items: center; gap: 5px; font: inherit; font-size: 12px; font-weight: 700; padding: 6px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--panel); color: var(--muted); cursor: pointer; }
  .fba-tab:hover { border-color: var(--brand); }
  .fba-tab.active { background: var(--brand); color: #fff; border-color: var(--brand); }
  .fba-tab-ic ui5-icon { width: 14px; height: 14px; }
  .fba-search { display: flex; gap: 6px; padding: 8px; border-bottom: 1px solid var(--line-soft); align-items: center; }
  .fba-search ui5-input { flex: 1; min-width: 0; }
  .fba-list-head { display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; font-size: 11px; color: var(--muted); text-transform: uppercase; font-weight: 800; }
  .fba-list-head span { background: var(--brand-soft); color: var(--brand); border-radius: 20px; padding: 1px 8px; }
  .fba-list { flex: 1; overflow-y: auto; padding: 0 8px 8px; }
  .fba-row { display: flex; flex-direction: column; align-items: flex-start; width: 100%; text-align: left; padding: 8px 10px; border: 1px solid var(--line-soft); border-radius: 8px; margin-bottom: 5px; background: var(--panel); cursor: pointer; gap: 2px; }
  .fba-row:hover { border-color: var(--brand); }
  .fba-row.active { background: var(--brand-soft); border-color: var(--brand); }
  .fba-row b { font-size: 13px; font-family: var(--mono); color: var(--ink); }
  .fba-row small { color: var(--muted); font-size: 11px; }
  .fba-tags { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 2px; }
  .fba-tag { font-style: normal; font-size: 10px; font-weight: 700; padding: 1px 7px; border-radius: 10px; background: var(--brand-soft); color: var(--brand); }
  .fba-tag.kind-workspace { background: color-mix(in srgb, var(--neo-violet, var(--neo-violet, #7c3aed)) 14%, transparent); color: var(--neo-violet, var(--neo-violet, #7c3aed)); }
  .fba-tag.kind-html { background: color-mix(in srgb, var(--neo-mint, #10b981) 14%, transparent); color: var(--neo-mint, #10b981); }
  .fba-tag.kind-native { background: var(--brand-soft); color: var(--brand); }
  .fba-tag.none { background: var(--warn-soft); color: var(--warn); }
  .fba-tag.seeded { background: color-mix(in srgb, var(--muted) 14%, transparent); color: var(--muted); }
  .fba-empty { display: flex; flex-direction: column; align-items: center; gap: 8px; color: var(--muted); padding: 36px 16px; font-size: 12.5px; }
  .fba-empty ui5-icon { width: 28px; height: 28px; opacity: .5; }
  /* content */
  .fba-toolbar { display: flex; align-items: center; gap: 6px; padding: 8px 12px; border-bottom: 1px solid var(--line-soft); background: var(--panel); flex-wrap: wrap; position: sticky; top: 0; z-index: 5; }
  .fba-title { font-size: 14px; font-family: var(--mono); color: var(--ink); } .fba-tb-sp { flex: 1; }
  .fba-banner { margin: 12px 14px 0; padding: 10px 14px; background: var(--warn-soft); border: 1px solid color-mix(in srgb, var(--warn) 30%, transparent); border-left: 3px solid var(--warn); border-radius: 0 8px 8px 0; font-size: 12.5px; color: var(--ink); }
  .fba-banner b { color: var(--warn); }
  .fba-form { padding: 16px 14px; max-width: 600px; }
  .fba-field { margin-bottom: 14px; }
  .fba-field label { display: block; font-size: 11px; font-weight: 700; color: var(--muted); text-transform: uppercase; margin-bottom: 4px; }
  .fba-field label em { color: var(--danger); font-style: normal; }
  .fba-field ui5-input, .fba-field ui5-select { width: 100%; }
  .fba-hint { font-size: 11px; color: var(--muted); margin-top: 3px; }
  /* property */
  .fba-prop-head { display: flex; align-items: center; gap: 8px; padding: 12px 14px; border-bottom: 1px solid var(--line-soft); font-size: 14px; font-weight: 750; }
  .fba-mode { font-size: 11px; font-weight: 700; padding: 2px 9px; border-radius: 20px; background: var(--brand-soft); color: var(--brand); font-family: var(--mono); }
  .fba-prop-body { padding: 14px; }
  .fba-sec { font-size: 11px; font-weight: 800; color: var(--brand); text-transform: uppercase; margin: 14px 0 8px; padding-bottom: 5px; border-bottom: 1px solid var(--line-soft); }
  .fba-sec:first-child { margin-top: 0; }
  .fba-note { background: var(--brand-soft); border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent); border-radius: 8px; padding: 10px 12px; font-size: 12.5px; color: var(--ink); }
  .fba-note.warn { background: var(--warn-soft); border-color: color-mix(in srgb, var(--warn) 30%, transparent); }
  .fba-note code { font-family: var(--mono); background: var(--panel); padding: 1px 5px; border-radius: 4px; }
  .fba-ftab { width: 100%; border-collapse: collapse; font-size: 12px; }
  .fba-ftab td { padding: 6px 8px; border-bottom: 1px solid var(--line-soft); vertical-align: top; }
  .fba-ftab td:first-child { font-weight: 700; white-space: nowrap; color: var(--muted); }
  /* toast（反色对调：两主题都成立） */
  .fba-toast { position: fixed; left: 50%; bottom: 20px; transform: translateX(-50%); background: color-mix(in srgb, var(--ink) 90%, transparent); color: var(--bg); padding: 9px 16px; border-radius: 9px; font-size: 12.5px; font-weight: 600; opacity: 0; pointer-events: none; transition: opacity .2s; z-index: 30; white-space: pre-line; max-width: 80vw; }
  .fba-toast.show { opacity: 1; }
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
