/**
 * 事件订阅管理（native-page，20260902 重构方案 §6.2）。
 *
 * 列表 + 编辑大弹框（对标 mdm subscription-manager 双形态）：
 *  - 列表：cmx-filter-bar（关键字/通道/启停）+ cmx-revo-grid（名称/描述/通道/规则数/状态/
 *    待投/24h死信/最近投递）+ cmx-pager；行操作 编辑/测试/启停/删除（仅停用态）/补发。
 *  - 编辑：cmx-floating-dialog 大尺寸——① 基本信息（名称/描述/通道/重试上限/启用）
 *    ② 通道配置（webhook：服务键/回调路径/secret 只写掩码+随机生成）
 *    ③ 订阅规则卡片区（每卡：规则名/启停/事件类型多选/流程分组多选/key glob 模式 chips；
 *    卡可增删上移下移；底部「预览命中定义」→ rules/preview 弹框：每规则命中数/样例/死组标注）。
 *  - 测试投递：不校验规则——文案澄清「验证回调连通与签名，不是规则命中」。
 *
 * 端点（前缀 /api/flow，门户反代 → 引擎 /flow）：
 *   POST /event-subscribers/{query,save,delete,set-active,test,rules/preview,rebuild}
 *   GET  /event-subscribers/{detail?id=,channels}
 *   GET  /definition-groups（分组多选数据源）
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

// 事件类型全集（与后端 event_admin::EVENT_TYPES 同源维护）。
const EVENT_TYPES = [
  { key: 'instance.started', label: '实例发起' },
  { key: 'instance.completed', label: '实例办结' },
  { key: 'instance.terminated', label: '实例终止' },
  { key: 'task.created', label: '待办产生' },
  { key: 'task.completed', label: '任务办结' },
  { key: 'task.reassigned', label: '任务转办' },
]

const _hostState = new WeakMap()
function initState () {
  return {
    rows: [], total: 0, page: 1, pageSize: 20,
    fKeyword: '', fChannel: '', fActive: '',
    channels: [], groups: [],
    grid: null,
  }
}
function getState (host) {
  if (host && !_hostState.has(host)) _hostState.set(host, initState())
  return host ? _hostState.get(host) : null
}
function hostRoot (host) { return (host && (host.renderRoot || host.shadowRoot)) || null }
function fmtTime (t) { if (!t) return ''; const s = String(t); return s.length > 19 ? s.slice(0, 19).replace('T', ' ') : s }
function randomSecret () {
  const bytes = new Uint8Array(24)
  crypto.getRandomValues(bytes)
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('')
}

function styleCss () {
  return `
  .pg { height:100%; overflow:hidden; display:flex; flex-direction:column; box-sizing:border-box; padding:12px 20px 16px;
    background:var(--sapBackgroundColor); color:var(--sapTextColor);
    font-family:var(--sapFontFamily,'72','Segoe UI',Arial,sans-serif); }
  .pg-head { margin-bottom:10px; }
  .pg-title { font-size:20px; font-weight:600; color:var(--sapTitleColor); }
  .pg-sub { font-size:12px; color:var(--sapContent_LabelColor); margin-top:2px; }
  .card { display:flex; flex-direction:column; flex:1; min-height:0;
    background:var(--sapList_Background); border:1px solid var(--sapList_BorderColor); border-radius:8px; padding:12px 14px; }
  .card-hd { display:flex; justify-content:space-between; align-items:center; gap:8px; margin-bottom:10px; }
  .card-title { font-size:15px; font-weight:600; color:var(--sapTitleColor); }
  cmx-toolbar, cmx-filter-bar { display:block; }
  .f-ipt { min-width:130px; }
  .tbl-wrap { flex:1; min-height:0; overflow:hidden; display:flex; flex-direction:column; margin-top:10px; }
  .tbl-wrap cmx-revo-grid { display:flex; width:100%; flex:1 1 0%; min-width:0; min-height:0; flex-direction:column; }
  `
}

function viewHtml (st) {
  const chOpts = ['<ui5-option value="">全部通道</ui5-option>']
    .concat(st.channels.map((c) => `<ui5-option value="${esc(c.type)}" ${st.fChannel === c.type ? 'selected' : ''}>${esc(c.label || c.type)}</ui5-option>`))
    .join('')
  return `<div class="pg">
    <div class="pg-head"><div class="pg-title">事件订阅管理</div>
      <div class="pg-sub">先注册订阅者（回调方式/地址/密钥），再配置多条订阅规则：事件类型 × 流程分组 × key glob 模式，规则内与、跨规则或、自上而下首个命中</div></div>
    <div class="card">
      <div class="card-hd"><div class="card-title" id="esTotal">订阅者（共 ${st.total} 条）</div>
        <cmx-toolbar><ui5-button design="Emphasized" icon="add" id="esAdd">注册订阅者</ui5-button>
          <ui5-button design="Transparent" icon="refresh" id="esReload">刷新</ui5-button></cmx-toolbar></div>
      <cmx-filter-bar show-search="false">
        <ui5-input id="esFKey" class="f-ipt" placeholder="名称搜索" value="${esc(st.fKeyword)}"></ui5-input>
        <ui5-select id="esFChannel">${chOpts}</ui5-select>
        <ui5-select id="esFActive">
          <ui5-option value="" ${st.fActive === '' ? 'selected' : ''}>全部状态</ui5-option>
          <ui5-option value="true" ${st.fActive === 'true' ? 'selected' : ''}>启用</ui5-option>
          <ui5-option value="false" ${st.fActive === 'false' ? 'selected' : ''}>停用</ui5-option>
        </ui5-select>
        <ui5-button slot="actions" design="Default" icon="search" id="esSearch">查询</ui5-button>
        <ui5-button slot="actions" design="Transparent" icon="reset" id="esReset">重置</ui5-button>
      </cmx-filter-bar>
      <div class="tbl-wrap"><cmx-revo-grid id="esGrid"></cmx-revo-grid></div>
      <cmx-pager id="esPager" page-size="20" page-sizes="10,20,50,100"></cmx-pager>
    </div></div>`
}

// ————————————————————— 数据 —————————————————————

async function loadLookups (st) {
  try {
    const d = (await apiGet('/api/flow/event-subscribers/channels')) || {}
    st.channels = (d.channels || []).map((c) => ({ type: c.type, label: c.name || c.type }))
  } catch { st.channels = [{ type: 'webhook', label: 'webhook' }] }
  try {
    const d = (await apiGet('/api/flow/definition-groups')) || {}
    st.groups = (d.rows || []).map((g) => ({ id: Number(g.id), name: g.name || '', enabled: g.enabled !== false }))
  } catch { st.groups = [] }
}

async function loadRows (st) {
  const body = { page: st.page, pageSize: st.pageSize }
  if (st.fKeyword.trim()) body.keyword = st.fKeyword.trim()
  if (st.fChannel) body.channel = st.fChannel
  if (st.fActive !== '') body.active = st.fActive === 'true'
  const d = (await apiPost('/api/flow/event-subscribers/query', body)) || {}
  st.rows = d.rows || []
  st.total = Number(d.total) || 0
}

// ————————————————————— grid —————————————————————

function buildGrid (host, st) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const grid = root.querySelector('#esGrid'); if (!grid) return
  grid.setAttribute('data-cmx-fill-height', '')
  grid.setAttribute('data-cmx-options', '{"editable":false,"showTotals":false,"showRequiredMark":false}')
  grid.classList.add('cmx-grid-neo')
  st.grid = grid
  if (!(C.CmxColumnModel && C.CmxColumn)) return
  const cm = new C.CmxColumnModel({ datasetId: 'es-list' })
  cm.setMembers([
    new C.CmxColumn({ id: 'name', caption: '名称', dataType: 'VARCHAR', width: '160px' }),
    new C.CmxColumn({ id: 'description', caption: '描述', dataType: 'VARCHAR', width: '170px' }),
    new C.CmxColumn({ id: 'channel', caption: '通道', dataType: 'VARCHAR', width: '90px' }),
    new C.CmxColumn({ id: 'ruleCount', caption: '规则数', dataType: 'VARCHAR', width: '80px' }),
    new C.CmxColumn({ id: 'active_text', caption: '状态', dataType: 'VARCHAR', width: '90px' }),
    new C.CmxColumn({ id: 'pendingCount', caption: '待投', dataType: 'VARCHAR', width: '70px' }),
    new C.CmxColumn({ id: 'deadCount24h', caption: '24h死信', dataType: 'VARCHAR', width: '90px' }),
    new C.CmxColumn({ id: 'lastDeliveredAt', caption: '最近投递', dataType: 'VARCHAR', width: '150px' }),
    new C.CmxColumn({ id: '_action', caption: '操作', dataType: 'VARCHAR', width: '330px', frozen: 'right', edit: { mode: 'readonly' },
      display: { mode: 'actions', actions: [
        { text: '编辑', actionRef: 'edit', icon: 'edit' },
        { text: '测试', actionRef: 'test', icon: 'paper-plane' },
        { text: '停用', actionRef: 'disable', icon: 'pause', visible: (m) => !!m.active },
        { text: '启用', actionRef: 'enable', icon: 'play', visible: (m) => !m.active },
        { text: '补发', actionRef: 'rebuild', icon: 'restart' },
        { text: '删除', actionRef: 'delete', icon: 'delete', variant: 'negative', visible: (m) => !m.active },
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
    doAction(host, st, d.actionRef, rec)
  })
}

function applyData (host, st, first) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const t = root.querySelector('#esTotal')
  if (t) t.textContent = `订阅者（共 ${st.total} 条）`
  const pager = root.querySelector('#esPager')
  if (pager) { pager.total = st.total; pager.page = st.page; pager.pageSize = st.pageSize }
  const grid = st.grid; if (!grid) return
  const rows = st.rows.map((r) => ({
    ...r,
    active: !!r.active,
    active_text: r.active ? '● 启用' : '○ 停用',
    lastDeliveredAt: fmtTime(r.lastDeliveredAt),
  }))
  const fill = () => {
    const g = st.grid
    if (!g) return
    if (C.CmxDataSet) { const ds = new C.CmxDataSet({ datasetId: 'es-list' }); ds.setRows(rows); g.setDataSet(ds) }
    else g.setDataSet?.(rows)
    g.refreshLayout?.()
  }
  // 一律双 rAF：重挂 grid 后立即 setDataSet 会被新元素升级/布局静默丢弃。
  requestAnimationFrame(() => requestAnimationFrame(fill))
}

async function reload (host, st) {
  try { await loadRows(st); applyData(host, st) } catch (e) { cmx().cmxError?.(`加载失败：${e.message}`) }
}

// ————————————————————— 行操作 —————————————————————

async function doAction (host, st, act, rec) {
  const M = cmx()
  const id = Number(rec.id)
  try {
    if (act === 'edit') {
      const sub = (await apiGet(`/api/flow/event-subscribers/detail?id=${encodeURIComponent(String(id))}`)) || {}
      openEditDialog(host, st, sub)
    } else if (act === 'test') {
      const t = (await apiPost('/api/flow/event-subscribers/test', { id })) || {}
      if (t.success) showCmxToast(`测试投递成功（HTTP ${t.httpStatus ?? 200}）——回调连通与签名验证通过`)
      else M.cmxError?.(`测试投递失败：${t.error || '未知原因'}（HTTP ${t.httpStatus ?? '-'}）`)
      reload(host, st)
    } else if (act === 'enable' || act === 'disable') {
      const active = act === 'enable'
      const r = (await apiPost('/api/flow/event-subscribers/set-active', { id, active })) || {}
      if (!active && Number(r.pendingCount) > 0) showCmxToast(`已停用；存量 ${r.pendingCount} 条待投行仍会投完`)
      else showCmxToast(active ? '已启用' : '已停用')
      reload(host, st)
    } else if (act === 'rebuild') {
      openRebuildDialog(host, st, rec)
    } else if (act === 'delete') {
      if (!await confirmBox(`确认删除订阅者「${rec.name}」？流水凭名称快照保留可查。`)) return
      await apiPost('/api/flow/event-subscribers/delete', { id })
      showCmxToast('已删除')
      reload(host, st)
    }
  } catch (e) { M.cmxError?.(e.message) }
}

// ————————————————————— 编辑弹框 —————————————————————

function blankRule () { return { name: '', enabled: true, eventTypes: [], groupIds: [], keyPatterns: [] } }

function openEditDialog (host, st, sub) {
  const C = cmx(); const M = C
  if (!customElements.get('cmx-floating-dialog')) { M.cmxError?.('弹框组件未就绪'); return }
  const isNew = !sub
  const cfg = (sub && sub.channelConfig) || {}
  const fm = {
    id: sub ? Number(sub.id) : null,
    name: (sub && sub.name) || '',
    description: (sub && sub.description) || '',
    channel: (sub && sub.channel) || 'webhook',
    active: sub ? !!sub.active : true,
    retryMax: (sub && sub.retryMax) != null ? Number(sub.retryMax) : 10,
    serviceKey: cfg.service_key || '',
    callbackPath: cfg.callback_path || '/api/mdm/flow/callback',
    secret: '',
    rules: ((sub && sub.rules) || []).map((r) => ({
      name: r.name || '',
      enabled: r.enabled !== false,
      eventTypes: Array.isArray(r.eventTypes) ? r.eventTypes.slice() : [],
      groupIds: Array.isArray(r.groupIds) ? r.groupIds.map(Number) : [],
      keyPatterns: Array.isArray(r.keyPatterns) ? r.keyPatterns.slice() : [],
    })),
  }
  if (!fm.rules.length) fm.rules = [blankRule()]

  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: isNew ? '注册订阅者' : `编辑订阅者 · ${fm.name}`, icon: 'settings',
    dialogWidth: '860px', dialogHeight: '86vh',
    showConfirm: false, showCancel: false,
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'flex:1;min-height:0;padding:6px 18px 14px;display:flex;flex-direction:column;font-size:13px;'
  wrap.innerHTML = `<style>
    .es-dlg { display:flex; flex-direction:column; flex:1 1 auto; min-height:0; }
    .es-scroll { flex:1; min-height:0; overflow-y:auto; display:flex; flex-direction:column; gap:10px; padding:2px 6px 8px 0; }
    .es-dlg label { font-size:12px; color:var(--sapContent_LabelColor); }
    .sec-title { font-size:13px; font-weight:600; color:var(--sapTitleColor); margin-top:8px; padding-bottom:4px;
      border-bottom:1px solid var(--sapList_BorderColor,#e5e5e5); }
    .grid2 { display:grid; grid-template-columns:1fr 1fr; gap:10px 14px; }
    .grid3 { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:10px 14px; }
    .f { display:flex; flex-direction:column; gap:4px; min-width:0; }
    .hint { font-size:12px; color:var(--sapContent_LabelColor); }
    .rule-card { border:1px solid var(--sapGroup_ContentBorderColor,#e9e9e9); border-radius:8px;
      background:var(--sapList_Background); padding:10px 12px; display:flex; flex-direction:column; gap:8px; }
    .rule-hd { display:flex; align-items:center; gap:8px; }
    .rule-hd ui5-input { flex:1; }
    .rule-idx { font-size:12px; color:var(--sapContent_LabelColor); width:52px; flex-shrink:0; }
    .chk-row { display:flex; flex-wrap:wrap; gap:6px 18px; }
    .rule-lbl { font-size:12px; color:var(--sapContent_LabelColor); margin:2px 0; }
    .chips { display:flex; flex-wrap:wrap; gap:6px; align-items:center; }
    .chip { display:inline-flex; align-items:center; gap:4px; padding:2px 8px; border-radius:10px; font-size:12px;
      border:1px solid var(--sapList_SelectionBorderColor,#8c8c8c); background:var(--sapList_Background); }
    .chip button { border:none; background:transparent; color:var(--sapContent_LabelColor); cursor:pointer; padding:0 2px; }
    .pat-add { display:flex; gap:6px; }
    .pat-add ui5-input { flex:1; }
    .dlg-foot { display:flex; justify-content:flex-end; gap:8px; padding:10px 6px 4px 0; flex-shrink:0;
      border-top:1px solid var(--sapList_BorderColor,#e5e5e5); }
  </style>
  <div class="es-dlg"><div class="es-scroll">
    <div class="sec-title">① 基本信息</div>
    <div class="grid2">
      <div class="f"><label>名称 *（同租户唯一，≤128 字符）</label><ui5-input id="esName" placeholder="如：MDM 主数据回调" value="${esc(fm.name)}"></ui5-input></div>
      <div class="f"><label>描述</label><ui5-input id="esDesc" placeholder="用途说明（可空）" value="${esc(fm.description)}"></ui5-input></div>
      <div class="f"><label>通道 *</label><ui5-select id="esChannel">${
        st.channels.map((c) => `<ui5-option value="${esc(c.type)}" ${fm.channel === c.type ? 'selected' : ''}>${esc(c.label || c.type)}</ui5-option>`).join('')
      }</ui5-select></div>
      <div class="f"><label>最大尝试次数（含首发）</label><ui5-input id="esRetry" type="Number" value="${fm.retryMax}"></ui5-input></div>
      <div class="f"><label>启用</label><ui5-switch id="esActive" ${fm.active ? 'checked' : ''}></ui5-switch></div>
    </div>
    <div class="sec-title">② 通道配置（webhook）</div>
    <div class="grid3">
      <div class="f"><label>服务键（service_key）</label><ui5-input id="esSvcKey" placeholder="如 mdm" value="${esc(fm.serviceKey)}"></ui5-input></div>
      <div class="f"><label>回调路径（callback_path）</label><ui5-input id="esCbPath" placeholder="/api/mdm/flow/callback" value="${esc(fm.callbackPath)}"></ui5-input></div>
      <div class="f"><label>签名密钥（secret；留空/掩码 = 沿用旧值）</label>
        <div style="display:flex;gap:6px"><ui5-input id="esSecret" type="Password" placeholder="${isNew ? '' : '******（回显掩码 = 沿用旧值）'}"></ui5-input>
        <ui5-button design="Transparent" icon="random" id="esGen" title="随机生成"></ui5-button></div></div>
    </div>
    <div class="hint">密钥明文只写不读：API 永远回显掩码；接收端按 HMAC-SHA256 验签（头 x-cmx-flow-signature）。</div>
    <div class="sec-title" style="display:flex;justify-content:space-between;align-items:center">
      <span>③ 订阅规则（自上而下首个命中；规则内三维全部满足）</span>
      <span><ui5-button design="Transparent" icon="play" id="esPreview">预览命中定义</ui5-button>
      <ui5-button design="Transparent" icon="add" id="esAddRule">添加规则</ui5-button></span></div>
    <div id="esRules" style="display:flex;flex-direction:column;gap:8px;"></div>
    <div class="hint">事件类型空 = 全部六种；分组空 = 不限分组；key 模式空 = 不限（仅支持 * 通配，如 mdm_* / *_审批）；三维全空 = 匹配全部（网关形态）。</div>
  </div>
  <div class="dlg-foot">
    <ui5-button design="Transparent" id="esCancel">取消</ui5-button>
    <ui5-button design="Emphasized" icon="save" id="esSave">保存</ui5-button>
  </div></div>`
  dlg.setContent(wrap)

  // ——— 规则卡片区渲染 ———
  const rulesBox = wrap.querySelector('#esRules')
  const drawRules = () => {
    rulesBox.innerHTML = fm.rules.map((r, i) => `<div class="rule-card" data-i="${i}">
      <div class="rule-hd">
        <span class="rule-idx">规则 ${i + 1}${i === 0 ? '（首选）' : ''}</span>
        <ui5-input class="r-name" placeholder="规则名（≤64，订阅者内唯一；命中后随投递行留痕）" value="${esc(r.name)}"></ui5-input>
        <ui5-switch class="r-en" ${r.enabled ? 'checked' : ''}></ui5-switch>
        <ui5-button design="Transparent" icon="navigation-up-arrow" data-act="up" title="上移" ${i === 0 ? 'disabled' : ''}></ui5-button>
        <ui5-button design="Transparent" icon="navigation-down-arrow" data-act="down" title="下移" ${i === fm.rules.length - 1 ? 'disabled' : ''}></ui5-button>
        <ui5-button design="Transparent" icon="delete" data-act="del" title="删除"></ui5-button>
      </div>
      <div><div class="rule-lbl">事件类型（空 = 全部）</div>
        <div class="chk-row">${EVENT_TYPES.map((t) => `<ui5-checkbox class="r-evt" data-v="${t.key}" text="${t.label}" ${r.eventTypes.includes(t.key) ? 'checked' : ''}></ui5-checkbox>`).join('')}</div></div>
      <div><div class="rule-lbl">流程分组（空 = 不限）</div>
        <div class="chk-row">${st.groups.map((g) => `<ui5-checkbox class="r-grp" data-v="${g.id}" text="${esc(g.name)}${g.enabled ? '' : '（停用）'}" ${r.groupIds.includes(g.id) ? 'checked' : ''}></ui5-checkbox>`).join('') || '<span class="hint">暂无分组（可在流程定义页维护）</span>'}</div></div>
      <div><div class="rule-lbl">定义 key 模式（glob，仅 * 通配；空 = 不限）</div>
        <div class="chips">${r.keyPatterns.map((p, j) => `<span class="chip"><code>${esc(p)}</code><button type="button" data-pat="${j}" title="移除">×</button></span>`).join('')}
          <span class="pat-add"><ui5-input class="r-pat-in" placeholder="如 mdm_* 回车添加"></ui5-input></span></div></div>
    </div>`).join('')
    // 事件绑定
    rulesBox.querySelectorAll('.rule-card')?.forEach((card, i) => {
      const r = fm.rules[i]
      card.querySelector('.r-name')?.addEventListener('input', (e) => { r.name = e.target.value })
      card.querySelector('.r-en')?.addEventListener('change', (e) => { r.enabled = !!e.target.checked })
      card.querySelectorAll('.r-evt')?.forEach((cb) => cb.addEventListener('change', (e) => {
        const k = cb.getAttribute('data-v')
        if (e.target.checked) { if (!r.eventTypes.includes(k)) r.eventTypes.push(k) }
        else r.eventTypes = r.eventTypes.filter((x) => x !== k)
      }))
      card.querySelectorAll('.r-grp')?.forEach((cb) => cb.addEventListener('change', (e) => {
        const k = Number(cb.getAttribute('data-v'))
        if (e.target.checked) { if (!r.groupIds.includes(k)) r.groupIds.push(k) }
        else r.groupIds = r.groupIds.filter((x) => x !== k)
      }))
      const patIn = card.querySelector('.r-pat-in')
      patIn?.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter') return
        e.preventDefault()
        const v = (patIn.value || '').trim()
        if (!v) return
        if (v.length > 128) { M.cmxWarn?.('单条模式 ≤128 字符'); return }
        if (/[?[\]]/.test(v)) { M.cmxWarn?.('仅支持 * 通配（不支持 ? / []）'); return }
        if (!r.keyPatterns.includes(v)) r.keyPatterns.push(v)
        patIn.value = ''
        drawRules()
      })
      card.querySelectorAll('.chip button[data-pat]')?.forEach((b) => b.addEventListener('click', () => {
        r.keyPatterns.splice(Number(b.getAttribute('data-pat')), 1)
        drawRules()
      }))
      card.querySelectorAll('.rule-hd ui5-button[data-act]')?.forEach((b) => b.addEventListener('click', () => {
        const act = b.getAttribute('data-act')
        if (act === 'up' && i > 0) fm.rules.splice(i - 1, 0, fm.rules.splice(i, 1)[0])
        else if (act === 'down' && i < fm.rules.length - 1) fm.rules.splice(i + 1, 0, fm.rules.splice(i, 1)[0])
        else if (act === 'del') fm.rules.splice(i, 1)
        drawRules()
      }))
    })
  }
  drawRules()
  wrap.querySelector('#esAddRule')?.addEventListener('click', () => {
    if (fm.rules.length >= 20) { M.cmxWarn?.('规则数上限 20 条'); return }
    fm.rules.push(blankRule())
    drawRules()
  })
  wrap.querySelector('#esGen')?.addEventListener('click', () => {
    const inp = wrap.querySelector('#esSecret')
    inp.value = randomSecret()
    showCmxToast('已生成随机密钥（保存后生效；请同步配置到接收端）')
  })
  wrap.querySelector('#esCancel')?.addEventListener('click', () => dlg.close?.())

  // 收集表单 → save body
  const collect = () => ({
    id: fm.id,
    name: (wrap.querySelector('#esName') || {}).value?.trim() || '',
    description: ((wrap.querySelector('#esDesc') || {}).value || '').trim() || null,
    channel: (wrap.querySelector('#esChannel') || {}).value || 'webhook',
    active: !!(wrap.querySelector('#esActive') || {}).checked,
    retryMax: Number((wrap.querySelector('#esRetry') || {}).value) || 10,
    channelConfig: {
      service_key: ((wrap.querySelector('#esSvcKey') || {}).value || '').trim(),
      callback_path: ((wrap.querySelector('#esCbPath') || {}).value || '').trim(),
      secret: ((wrap.querySelector('#esSecret') || {}).value || ''),
    },
    rules: fm.rules.map((r) => ({
      name: (r.name || '').trim(),
      enabled: !!r.enabled,
      eventTypes: r.eventTypes,
      groupIds: r.groupIds,
      keyPatterns: r.keyPatterns,
    })),
  })

  wrap.querySelector('#esSave')?.addEventListener('click', async () => {
    const body = collect()
    if (!body.name) { M.cmxWarn?.('订阅者名称必填'); return }
    try {
      await apiPost('/api/flow/event-subscribers/save', body)
      showCmxToast('已保存')
      dlg.close?.()
      reload(host, st)
    } catch (e) { M.cmxError?.(e.message) }
  })

  // 规则预览（后端权威演算）
  wrap.querySelector('#esPreview')?.addEventListener('click', () => {
    const body = collect()
    if (!body.rules.length) { M.cmxWarn?.('请先添加规则'); return }
    openPreviewDialog(st, body.rules)
  })

  dlg.openModal().then(() => dlg.remove())
}

// ————————————————————— 规则预览弹框 —————————————————————

function openPreviewDialog (st, rules) {
  const C = cmx(); const M = C
  if (!customElements.get('cmx-floating-dialog')) { M.cmxError?.('弹框组件未就绪'); return }
  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: '规则命中预览（按定义表实数据演算）', icon: 'hint', dialogWidth: '680px', dialogHeight: '70vh',
    showConfirm: false, showCancel: false,
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'flex:1;min-height:0;padding:6px 16px 12px;display:flex;flex-direction:column;font-size:13px;'
  wrap.innerHTML = '<div class="hint" style="color:var(--sapContent_LabelColor);padding:8px 0">演算中…</div>'
  dlg.setContent(wrap)
  dlg.openModal().then(() => dlg.remove())
  ;(async () => {
    try {
      const d = (await apiPost('/api/flow/event-subscribers/rules/preview', { rules })) || {}
      const rows = (d.rules || []).map((r) => {
        const grpName = (id) => { const g = (d.groups || []).find((x) => Number(x.id) === Number(id)); return g ? g.name : `#${id}` }
        const dead = (r.deadGroupIds || []).map(String)
        return `<div class="pv-row">
          <div class="pv-hd"><b>${esc(r.name)}</b>${r.enabled ? '' : '<span class="pv-off">（已停用）</span>'}
            <span style="flex:1"></span><span>命中定义 ${r.matchedCount} 个${dead.length ? ` · <span class="pv-dead">引用了不存在的分组：${dead.map(grpName).map(esc).join('、')}</span>` : ''}</span></div>
          <div class="pv-keys">${(r.sampleKeys || []).map((k) => `<code>${esc(k)}</code>`).join('') || '<span class="hint">（无命中）</span>'}${r.matchedCount > (r.sampleKeys || []).length ? `<span class="hint"> …等 ${r.matchedCount} 个</span>` : ''}</div>
        </div>`
      }).join('')
      wrap.innerHTML = `<style>
        .pv-row { border:1px solid var(--sapGroup_ContentBorderColor,#e9e9e9); border-radius:6px; padding:8px 10px; margin-bottom:8px; }
        .pv-hd { display:flex; align-items:center; gap:8px; margin-bottom:6px; }
        .pv-off { color:var(--sapContent_LabelColor); font-size:12px; }
        .pv-dead { color:var(--sapErrorColor,#bb0000); font-size:12px; }
        .pv-keys { display:flex; flex-wrap:wrap; gap:4px 10px; font-size:12px; }
        .pv-keys code { background:var(--sapList_Hover_Background,rgba(0,0,0,.05)); padding:1px 6px; border-radius:4px; }
        .hint { font-size:12px; color:var(--sapContent_LabelColor); }
        .dlg-foot { display:flex; justify-content:flex-end; padding-top:8px; border-top:1px solid var(--sapList_BorderColor,#e5e5e5); }
      </style>
      <div style="flex:1;min-height:0;overflow-y:auto">${rows || '<div class="hint">未配置规则</div>'}</div>
      <div class="dlg-foot"><ui5-button design="Transparent" id="pvClose">关闭</ui5-button></div>`
      wrap.querySelector('#pvClose')?.addEventListener('click', () => dlg.close?.())
    } catch (e) {
      // 错误分支同样要有关闭按钮（否则预演失败后弹框只能靠 ESC 逃出）。
      wrap.innerHTML = `<div class="hint" style="flex:1;padding:12px 0;color:var(--sapErrorColor,#bb0000)">预演失败：${esc(e.message)}</div>
      <div style="display:flex;justify-content:flex-end;padding-top:8px;border-top:1px solid var(--sapList_BorderColor,#e5e5e5);">
        <ui5-button design="Transparent" id="pvClose">关闭</ui5-button></div>`
      wrap.querySelector('#pvClose')?.addEventListener('click', () => dlg.close?.())
    }
  })()
}

// ————————————————————— 补发弹框 —————————————————————

function openRebuildDialog (host, st, rec) {
  const C = cmx(); const M = C
  if (!customElements.get('cmx-floating-dialog')) { M.cmxError?.('弹框组件未就绪'); return }
  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: `补发 · ${rec.name}`, icon: 'restart', dialogWidth: '480px',
    showConfirm: true, confirmText: '补发', cancelText: '取消',
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'padding:8px 4px;font-size:13px;display:flex;flex-direction:column;gap:8px;'
  wrap.innerHTML = `<div style="font-size:12px;color:var(--sapContent_LabelColor)">
      把时间窗内终态实例的「办结/终止」事件按该订阅者的规则重放进投递管线（确定性事件 id，重复补发幂等；任务级事件不在补发范围）。</div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px;">
      <div><label style="font-size:12px;color:var(--sapContent_LabelColor)">起始（RFC3339，可空）</label><ui5-input id="rbSince" placeholder="2026-09-01T00:00:00Z"></ui5-input></div>
      <div><label style="font-size:12px;color:var(--sapContent_LabelColor)">截止（可空 = 至今）</label><ui5-input id="rbUntil" placeholder=""></ui5-input></div></div>`
  dlg.setContent(wrap)
  // 组件无 onConfirm：openModal resolve { action } 后自行补发（mdm 同款）。
  dlg.openModal().then(async (r) => {
    dlg.remove()
    if (!r || r.action !== 'confirm') return
    const body = { name: rec.name }
    const s = ((wrap.querySelector('#rbSince') || {}).value || '').trim()
    const u = ((wrap.querySelector('#rbUntil') || {}).value || '').trim()
    if (s) body.since = s
    if (u) body.until = u
    try {
      const rr = (await apiPost('/api/flow/event-subscribers/rebuild', body)) || {}
      M.cmxConfirm?.({
        message: `补发完成：扫描终态实例 ${rr.scanned} 个，命中规则 ${rr.matched} 个，写入投递行 ${rr.inserted} 行（已进入投递管线，同订阅者保序）。`,
        intent: 'positive', confirmText: '知道了', hideCancel: true,
      })
    } catch (e) { M.cmxError?.(e.message) }
  })
}

// ————————————————————— 绑定与入口 —————————————————————

function bind (host, st) {
  const root = hostRoot(host); if (!root) return
  root.querySelector('#esAdd')?.addEventListener('click', () => openEditDialog(host, st, null))
  root.querySelector('#esReload')?.addEventListener('click', () => reload(host, st))
  root.querySelector('#esSearch')?.addEventListener('click', () => {
    st.fKeyword = ((root.querySelector('#esFKey') || {}).value) || ''
    st.fChannel = ((root.querySelector('#esFChannel') || {}).value) || ''
    st.fActive = ((root.querySelector('#esFActive') || {}).value) || ''
    st.page = 1
    reload(host, st)
  })
  root.querySelector('#esReset')?.addEventListener('click', () => {
    st.fKeyword = ''; st.fChannel = ''; st.fActive = ''; st.page = 1
    // 输入/下拉直接清值（本页无整页重绘；select 回选首项"全部…"）。
    const i = root.querySelector('#esFKey'); if (i) i.value = ''
    for (const id of ['#esFChannel', '#esFActive']) {
      const s = root.querySelector(id)
      try { if (s) s.selectedIndex = 0 } catch { /* 旧版无 selectedIndex 则忽略 */ }
    }
    reload(host, st)
  })
  root.querySelector('#esFKey')?.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return
    st.fKeyword = ((root.querySelector('#esFKey') || {}).value) || ''
    st.fChannel = ((root.querySelector('#esFChannel') || {}).value) || ''
    st.fActive = ((root.querySelector('#esFActive') || {}).value) || ''
    st.page = 1
    reload(host, st)
  })
  const pager = root.querySelector('#esPager')
  pager?.addEventListener('page-change', (e) => {
    const d = e.detail || {}
    if (d.pageSize && d.pageSize !== st.pageSize) { st.pageSize = d.pageSize; st.page = 1 }
    else st.page = d.page || 1
    loadRows(st).then(() => applyData(host, st)).catch((err) => cmx().cmxError?.(err.message))
  })
  buildGrid(host, st)
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
        await Promise.all([loadLookups(st), loadRows(st)])
      } catch (e) { console.error('[event-subscription-manager] init fail', e); cmx().cmxError?.(`初始化失败：${e.message}`) }
      if (host) whenRendered(host, '.pg', () => bind(host, st))
      return `<style>${styleCss()}</style>${viewHtml(st)}`
    },
  },
}
