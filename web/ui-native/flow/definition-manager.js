/**
 * 流程定义管理（native-page，20260902 重构方案 §6.1）。
 *
 * 布局：左侧流程分组面板（全部/未分组/各分组 + 编辑分组弹框[上移下移/启停/增改删]）
 *       + 右侧定义列表（cmx-filter-bar 搜索 + cmx-revo-grid + cmx-pager）。
 * 行操作：改分组 / 详情（元数据 + 版本表 + 工作台入口）/ 在设计工作台打开。
 * 「新建」按当前选中分组跳设计工作台（initialContext.groupId 预填；工作台侧兜底分组下拉）。
 *
 * 端点（前缀 /api/flow，门户反代 → 引擎 /flow）：
 *   POST /definitions/query  定义分页（keyword/state/groupId/ungrouped）
 *   POST /definitions/set-group 批量设置分组
 *   GET  /definition-groups  分组列表（含定义数）
 *   POST /definition-groups/{save,delete}
 *   GET  /definitions/{key}/versions 版本表（详情视图用）
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

// 按 host 隔离的 state（多实例安全）。
const _hostState = new WeakMap()
function initState () {
  return {
    groups: [],            // [{id,name,sortNo,enabled,remark,defCount}]
    grandTotal: null,      // 全库定义总数（不受列表筛选影响）——左侧角标专用
    selGroup: 'all',       // all | ungrouped | <groupId>
    keyword: '', state: '',
    rows: [], total: 0, page: 1, pageSize: 20,
    grid: null, vgrid: null,
    detail: null,          // 详情视图当前定义（null = 列表视图）
    detailVersions: [],
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
  .main { flex:1; min-height:0; display:flex; gap:12px; }
  .grp-panel { width:230px; flex-shrink:0; display:flex; flex-direction:column;
    background:var(--sapList_Background); border:1px solid var(--sapList_BorderColor); border-radius:8px; padding:10px; }
  .grp-hd { display:flex; justify-content:space-between; align-items:center; margin-bottom:8px; }
  .grp-title { font-size:14px; font-weight:600; color:var(--sapTitleColor); }
  .grp-list { flex:1; min-height:0; overflow-y:auto; display:flex; flex-direction:column; gap:2px; }
  .grp-item { display:flex; justify-content:space-between; align-items:center; gap:6px; padding:7px 10px; border-radius:6px;
    border:1px solid transparent; cursor:pointer; font-size:13px; color:var(--sapTextColor);
    background:transparent; text-align:left; width:100%; box-sizing:border-box; }
  .grp-item:hover { background:var(--sapList_Hover_Background, rgba(0,0,0,.04)); }
  .grp-item.on { background:var(--sapList_SelectionBackgroundColor, rgba(8,87,214,.08));
    border-color:var(--sapList_SelectionBorderColor, rgba(8,87,214,.35)); font-weight:600; }
  .grp-item.off-group .grp-name { color:var(--sapContent_LabelColor); text-decoration:line-through; }
  .grp-cnt { font-size:11px; color:var(--sapContent_LabelColor); flex-shrink:0; }
  .card { flex:1; min-width:0; display:flex; flex-direction:column;
    background:var(--sapList_Background); border:1px solid var(--sapList_BorderColor); border-radius:8px; padding:12px 14px; }
  .card-hd { display:flex; justify-content:space-between; align-items:center; gap:8px; margin-bottom:10px; }
  .card-title { font-size:15px; font-weight:600; color:var(--sapTitleColor); }
  cmx-toolbar, cmx-filter-bar { display:block; }
  .f-ipt { min-width:140px; }
  .tbl-wrap { flex:1; min-height:0; overflow:hidden; display:flex; flex-direction:column; margin-top:10px; }
  .tbl-wrap cmx-revo-grid { display:flex; width:100%; flex:1 1 0%; min-width:0; min-height:0; flex-direction:column; }
  .detail-meta { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:8px 24px; padding:6px 2px; }
  .meta-row { display:flex; gap:10px; font-size:13px; }
  .meta-k { width:92px; flex-shrink:0; color:var(--sapContent_LabelColor); }
  .meta-v { color:var(--sapTextColor); word-break:break-all; }
  .ver-wrap { flex:1; min-height:120px; display:flex; flex-direction:column; margin-top:8px; }
  .ver-wrap cmx-revo-grid { display:flex; width:100%; flex:1 1 0%; min-width:0; min-height:0; flex-direction:column; }
  .empty { flex:1; display:flex; align-items:center; justify-content:center;
    color:var(--sapContent_LabelColor); font-size:13px; }
  `
}

// ————————————————————— 数据加载 —————————————————————

async function loadGroups (st) {
  // 分组角标数据源必须与列表筛选解耦：defCount 来自分组接口（全库口径），
  // grandTotal 用 pageSize=1 的无筛选查询取 total——否则选中分组后 st.total 变成
  // 筛选值，「全部定义/未分组」角标跟着漂移。
  const [d, q] = await Promise.all([
    apiGet('/api/flow/definition-groups').then((x) => x || {}),
    apiPost('/api/flow/definitions/query', { page: 1, pageSize: 1 }).then((x) => x || {}).catch(() => null),
  ])
  st.groups = (d.rows || []).map((g) => ({
    id: Number(g.id), name: g.name || '', sortNo: Number(g.sortNo) || 0,
    enabled: g.enabled !== false, remark: g.remark || '', defCount: Number(g.defCount) || 0,
  }))
  if (q && q.total != null) st.grandTotal = Number(q.total) || 0
}

async function loadRows (st) {
  const body = { page: st.page, pageSize: st.pageSize }
  if (st.keyword.trim()) body.keyword = st.keyword.trim()
  if (st.state) body.state = st.state
  if (st.selGroup === 'ungrouped') body.ungrouped = true
  else if (st.selGroup !== 'all') body.groupId = Number(st.selGroup)
  const d = (await apiPost('/api/flow/definitions/query', body)) || {}
  st.rows = d.rows || []
  st.total = Number(d.total) || 0
}

async function loadVersions (st, key) {
  // 接口 data = { key, activeVersion, versions:[...] }——取 versions 数组。
  const d = (await apiGet(`/api/flow/definitions/${encodeURIComponent(key)}/versions`)) || {}
  st.detailVersions = Array.isArray(d) ? d : (d.versions || [])
}

// ————————————————————— 视图 —————————————————————

function grpItemHtml (st, key, name, count, extraCls) {
  const on = st.selGroup === key ? 'on' : ''
  return `<button class="grp-item ${on} ${extraCls || ''}" data-grp="${esc(key)}">
    <span class="grp-name">${esc(name)}</span><span class="grp-cnt">${count != null ? count : ''}</span></button>`
}

function viewHtml (st) {
  // 角标口径：全部/未分组用 grandTotal（全库），单组用 defCount——都不随右侧列表筛选变。
  const gt = st.grandTotal != null ? st.grandTotal : st.total
  const grpHtml = [
    grpItemHtml(st, 'all', '全部定义', gt),
    grpItemHtml(st, 'ungrouped', '未分组', gt != null ? gt - st.groups.reduce((a, g) => a + (g.defCount || 0), 0) : null),
  ]
    .concat(st.groups.map((g) => grpItemHtml(st, String(g.id), g.name, g.defCount, g.enabled ? '' : 'off-group')))
    .join('')
  const stOpts = ['<ui5-option value="">全部状态</ui5-option>']
    .concat(['DRAFT', 'PUBLISHED'].map((s) => `<ui5-option value="${s}" ${st.state === s ? 'selected' : ''}>${s === 'DRAFT' ? '草稿' : '已发布'}</ui5-option>`))
    .join('')
  const right = st.detail ? detailHtml(st) : listHtml(st, stOpts)
  return `<div class="pg">
    <div class="pg-head"><div class="pg-title">流程定义管理</div>
      <div class="pg-sub">左侧按流程分组归属浏览与维护；分组是事件订阅规则的匹配维度（方案 20260902）</div></div>
    <div class="main">
      <div class="grp-panel">
        <div class="grp-hd"><span class="grp-title">流程分组</span>
          <ui5-button design="Transparent" icon="settings" id="dmGrpEdit">编辑分组</ui5-button></div>
        <div class="grp-list" id="dmGrpList">${grpHtml}</div>
      </div>
      <div class="card">${right}</div>
    </div></div>`
}

function listHtml (st, stOpts) {
  // 仅选中具体分组时显示"新建"（全部定义/未分组没有落组语义）；新建即建到当前分组。
  const isNewVisible = st.selGroup !== 'all' && st.selGroup !== 'ungrouped'
  const newBtn = isNewVisible
    ? `<ui5-button design="Emphasized" icon="add" id="dmNew">新建流程定义</ui5-button>`
    : `<span class="new-hint" style="font-size:12px;color:var(--sapContent_LabelColor)">选中左侧分组后可新建</span>`
  return `<div class="card-hd"><div class="card-title" id="dmTotal">定义列表（共 ${st.total} 条）</div>
    <cmx-toolbar>${newBtn}<ui5-button design="Transparent" icon="refresh" slot="actions" id="dmReload">刷新</ui5-button></cmx-toolbar></div>
  <cmx-filter-bar show-search="false">
    <ui5-input id="dmFKey" class="f-ipt" placeholder="key / 名称搜索" value="${esc(st.keyword)}"></ui5-input>
    <ui5-select id="dmFState">${stOpts}</ui5-select>
    <ui5-button slot="actions" design="Default" icon="search" id="dmSearch">查询</ui5-button>
    <ui5-button slot="actions" design="Transparent" icon="reset" id="dmReset">重置</ui5-button>
  </cmx-filter-bar>
  <div class="tbl-wrap"><cmx-revo-grid id="dmGrid"></cmx-revo-grid></div>
  <cmx-pager id="dmPager" page-size="20" page-sizes="10,20,50,100"></cmx-pager>`
}

function detailHtml (st) {
  const d = st.detail || {}
  const meta = [
    ['定义 Key', d.key], ['名称', d.name], ['分组', d.groupName || '未分组'],
    ['状态', d.state === 'PUBLISHED' ? '已发布' : '草稿'], ['当前版本', d.activeVersion != null ? `v${d.activeVersion}` : '-'],
    ['域 / 应用 / 模块', [d.domain, d.application, d.module].filter(Boolean).join(' / ') || '-'],
    ['最近更新', fmtTime(d.updatedAt)], ['更新人', d.updatedBy || '-'],
  ]
    .map(([k, v]) => `<div class="meta-row"><span class="meta-k">${esc(k)}</span><span class="meta-v">${esc(v ?? '-')}</span></div>`)
    .join('')
  return `<div class="card-hd"><div class="card-title">定义详情 · ${esc(d.name || d.key || '')}</div>
    <cmx-toolbar>
      <ui5-button design="Emphasized" icon="edit" id="dmOpenWb">在设计工作台打开</ui5-button>
      <ui5-button design="Transparent" icon="nav-back" id="dmBack">返回列表</ui5-button></cmx-toolbar></div>
  <div class="detail-meta">${meta}</div>
  <div class="card-title" style="margin-top:10px">版本历史（不可变，发布追加）</div>
  <div class="ver-wrap"><cmx-revo-grid id="dmVGrid"></cmx-revo-grid></div>`
}

// ————————————————————— grid —————————————————————

function buildGrid (host, st) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const wrap = root.querySelector('.tbl-wrap')
  const grid = wrap && wrap.querySelector('cmx-revo-grid')
  if (!grid) return
  grid.setAttribute('data-cmx-fill-height', '')
  grid.setAttribute('data-cmx-options', '{"editable":false,"showTotals":false,"showRequiredMark":false}')
  grid.classList.add('cmx-grid-neo')
  st.grid = grid
  if (!(C.CmxColumnModel && C.CmxColumn)) return
  const cm = new C.CmxColumnModel({ datasetId: 'dm-list' })
  cm.setMembers([
    new C.CmxColumn({ id: 'key', caption: 'Key', dataType: 'VARCHAR', width: '200px' }),
    new C.CmxColumn({ id: 'name', caption: '名称', dataType: 'VARCHAR', width: '170px' }),
    new C.CmxColumn({ id: 'groupName', caption: '分组', dataType: 'VARCHAR', width: '120px' }),
    new C.CmxColumn({ id: 'state_text', caption: '状态', dataType: 'VARCHAR', width: '90px' }),
    new C.CmxColumn({ id: 'ver_text', caption: '当前版本', dataType: 'VARCHAR', width: '90px' }),
    new C.CmxColumn({ id: 'updatedAt', caption: '更新时间', dataType: 'VARCHAR', width: '150px' }),
    new C.CmxColumn({ id: '_action', caption: '操作', dataType: 'VARCHAR', width: '240px', frozen: 'right', edit: { mode: 'readonly' },
      display: { mode: 'actions', actions: [
        { text: '改分组', actionRef: 'setgroup', icon: 'folder' },
        { text: '详情', actionRef: 'detail', icon: 'detail-view' },
        { text: '工作台', actionRef: 'workbench', icon: 'edit' },
      ] } }),
  ])
  grid.setColumnModel(cm)
  grid.setOptions?.({ selectionMode: 'none', fillHeight: true, showRowIndex: true, showTotals: false, allowTextSelect: true, resize: true })
  grid.addEventListener('cmx-cell-link-click', (e) => {
    const dd = e.detail || {}
    const ds = grid._ds
    const row = (ds && ds.rows && !isNaN(parseInt(dd.rowId, 10))) ? ds.rows[parseInt(dd.rowId, 10)] : null
    const rec = row ? (row.toPlainObject ? row.toPlainObject() : row) : null
    if (!rec || !rec.key) return
    doRowAction(host, st, dd.actionRef, rec)
  })
}

function buildVersionGrid (host, st) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const grid = root.querySelector('#dmVGrid')
  if (!grid) return
  grid.setAttribute('data-cmx-fill-height', '')
  grid.classList.add('cmx-grid-neo')
  st.vgrid = grid
  if (!(C.CmxColumnModel && C.CmxColumn)) return
  const cm = new C.CmxColumnModel({ datasetId: 'dm-versions' })
  cm.setMembers([
    new C.CmxColumn({ id: 'version', caption: '版本', dataType: 'VARCHAR', width: '80px' }),
    new C.CmxColumn({ id: 'note', caption: '变更说明', dataType: 'VARCHAR', width: '300px' }),
    new C.CmxColumn({ id: 'publishedAt', caption: '发布时间', dataType: 'VARCHAR', width: '150px' }),
    new C.CmxColumn({ id: 'publishedBy', caption: '发布人', dataType: 'VARCHAR', width: '120px' }),
  ])
  grid.setColumnModel(cm)
  grid.setOptions?.({ selectionMode: 'none', fillHeight: true, showRowIndex: false, showTotals: false, allowTextSelect: true, resize: true })
}

function applyData (host, st, first) {
  const C = cmx()
  const root = hostRoot(host); if (!root) return
  const t = root.querySelector('#dmTotal')
  if (t) t.textContent = `定义列表（共 ${st.total} 条）`
  const pager = root.querySelector('#dmPager')
  if (pager) { pager.total = st.total; pager.page = st.page; pager.pageSize = st.pageSize }
  const grid = st.grid
  if (!grid) return
  const rows = st.rows.map((r) => ({
    ...r,
    groupName: r.groupName || '未分组',
    state_text: r.state === 'PUBLISHED' ? '已发布' : '草稿',
    ver_text: r.activeVersion != null ? `v${r.activeVersion}` : '-',
    updatedAt: fmtTime(r.updatedAt),
  }))
  const fill = () => {
    const g = st.grid
    if (!g) return
    if (C.CmxDataSet) { const ds = new C.CmxDataSet({ datasetId: 'dm-list' }); ds.setRows(rows); g.setDataSet(ds) }
    else g.setDataSet?.(rows)
    g.refreshLayout?.()
  }
  // 一律双 rAF：分组切换/翻页会重挂 grid（rerender→buildGrid），立即 setDataSet 会在
  // 新元素升级/布局完成前被静默丢弃（首屏数据可载入但后续刷新全部不生效）。
  requestAnimationFrame(() => requestAnimationFrame(fill))
}

function applyVersions (st) {
  const C = cmx()
  const grid = st.vgrid
  if (!grid) return
  const rows = st.detailVersions.map((v) => ({
    version: `v${v.version}`, note: v.note || '', publishedAt: fmtTime(v.publishedAt), publishedBy: v.publishedBy || '-',
  }))
  const fill = () => {
    const g = st.vgrid
    if (!g) return
    if (C.CmxDataSet) { const ds = new C.CmxDataSet({ datasetId: 'dm-versions' }); ds.setRows(rows); g.setDataSet(ds) }
    else g.setDataSet?.(rows)
    g.refreshLayout?.()
  }
  // 与 applyData 同款：详情重挂后立即 setDataSet 会被新元素升级/布局静默丢弃。
  requestAnimationFrame(() => requestAnimationFrame(fill))
}

// ————————————————————— 交互 —————————————————————

function rerender (host, st) {
  const root = hostRoot(host)
  if (!root) return
  const pg = root.querySelector('.pg')
  if (!pg) return
  const holder = document.createElement('div')
  holder.innerHTML = viewHtml(st)
  pg.replaceWith(holder.firstChild)
  bind(host, st)
}

function bind (host, st) {
  const root = hostRoot(host); if (!root) return
  root.querySelectorAll('.grp-item')?.forEach((btn) => {
    btn.addEventListener('click', () => {
      st.selGroup = btn.getAttribute('data-grp')
      st.page = 1
      rerender(host, st)
      loadRows(st).then(() => applyData(host, st)).catch((e) => cmx().cmxError?.(e.message))
    })
  })
  root.querySelector('#dmGrpEdit')?.addEventListener('click', () => openGroupDialog(host, st))
  root.querySelector('#dmNew')?.addEventListener('click', () => {
    const gid = st.selGroup !== 'all' && st.selGroup !== 'ungrouped' ? Number(st.selGroup) : null
    openWorkbench(host, st.detail ? st.detail.key : null, gid)
  })
  root.querySelector('#dmReload')?.addEventListener('click', () => reload(host, st))
  root.querySelector('#dmSearch')?.addEventListener('click', () => {
    st.keyword = ((root.querySelector('#dmFKey') || {}).value) || ''
    st.state = ((root.querySelector('#dmFState') || {}).value) || ''
    st.page = 1
    reload(host, st)
  })
  root.querySelector('#dmReset')?.addEventListener('click', () => {
    st.keyword = ''; st.state = ''; st.page = 1
    rerender(host, st)
    reload(host, st)
  })
  root.querySelector('#dmFKey')?.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return
    st.keyword = ((root.querySelector('#dmFKey') || {}).value) || ''
    st.state = ((root.querySelector('#dmFState') || {}).value) || ''
    st.page = 1
    reload(host, st)
  })
  const pager = root.querySelector('#dmPager')
  pager?.addEventListener('page-change', (e) => {
    const d = e.detail || {}
    if (d.pageSize && d.pageSize !== st.pageSize) { st.pageSize = d.pageSize; st.page = 1 }
    else st.page = d.page || 1
    loadRows(st).then(() => applyData(host, st)).catch((err) => cmx().cmxError?.(err.message))
  })
  // 详情视图
  root.querySelector('#dmBack')?.addEventListener('click', () => { st.detail = null; rerender(host, st); reload(host, st) })
  root.querySelector('#dmOpenWb')?.addEventListener('click', () => {
    if (st.detail) openWorkbench(host, st.detail.key, st.detail.groupId ?? null)
  })
  if (st.detail) { buildVersionGrid(host, st); applyVersions(st) }
  else { buildGrid(host, st); applyData(host, st, true) }
}

async function reload (host, st) {
  try {
    await Promise.all([loadGroups(st), loadRows(st)])
    // 分组面板计数随数据刷新（不整页重绘，重挂分组列表）。
    const root = hostRoot(host)
    const list = root && root.querySelector('#dmGrpList')
    if (list && !st.detail) {
      const holder = document.createElement('div')
      holder.innerHTML = viewHtml(st)
      const nl = holder.querySelector('#dmGrpList')
      if (nl) { list.replaceWith(nl); bindGroupList(host, st) }
    }
    applyData(host, st)
  } catch (e) { cmx().cmxError?.(`加载失败：${e.message}`) }
}

function bindGroupList (host, st) {
  const root = hostRoot(host); if (!root) return
  root.querySelectorAll('.grp-item')?.forEach((btn) => {
    btn.addEventListener('click', () => {
      st.selGroup = btn.getAttribute('data-grp'); st.page = 1
      rerender(host, st)
      loadRows(st).then(() => applyData(host, st)).catch((e) => cmx().cmxError?.(e.message))
    })
  })
}

async function doRowAction (host, st, act, rec) {
  try {
    if (act === 'setgroup') openSetGroupDialog(host, st, rec)
    else if (act === 'detail') {
      st.detail = rec
      rerender(host, st)
      try { await loadVersions(st, rec.key); applyVersions(st) } catch (e) { /* 版本空不阻断 */ }
    } else if (act === 'workbench') openWorkbench(host, rec.key, rec.groupId ?? null)
  } catch (e) { cmx().cmxError?.(e.message) }
}

// 打开流程定义工作台（portal.flow.definition-workbench 副本；initialContext 带 key/groupId 预填）。
function openWorkbench (host, key, groupId) {
  let app = null
  try { app = document.querySelector('cmx-portal-app') } catch { app = null }
  if (!app || typeof app.openNode !== 'function') {
    let n = host
    for (let i = 0; i < 6 && n; i++) {
      if (typeof n.openNode === 'function') { app = n; break }
      const r = n.getRootNode && n.getRootNode(); n = r && r.host
    }
  }
  if (!app || typeof app.openNode !== 'function') { cmx().cmxWarn?.('未找到门户 openNode，无法打开工作台'); return }
  const caption = key ? `流程定义工作台·${key}` : '流程定义工作台'
  const ctx = {}
  if (key) ctx.definitionKey = key
  if (groupId != null) ctx.groupId = groupId
  // 打开"流程定义工作台"副本（portal.flow.definition-workbench）：explorer/content/property
  // 三区同开（结构照设计工作台菜单节点）；explorer 不带定义列表，仅画布结构大纲。
  app.openNode({
    id: `portal-flow-def-wb-${key || 'new'}`, name: 'portal.flow.definition-workbench', caption, type: 'workspace-node',
    workspace: {
      id: 'flow_definition_workbench',
      explorer: {
        caption: '流程大纲', icon: 'tree',
        views: [{ id: 'flow-def-wb-explorer', tabLabel: '大纲', icon: 'tree', type: 'native_pages', native_page: 'portal.flow.definition-workbench', view: 'explorer' }],
      },
      content: {
        caption, icon: 'workflow-tasks',
        views: [{ id: 'flow-def-wb-content', tabLabel: '流程图', icon: 'workflow-tasks', type: 'native_pages', native_page: 'portal.flow.definition-workbench', view: 'content' }],
      },
      property: {
        caption: '节点属性', icon: 'detail-view',
        views: [{ id: 'flow-def-wb-prop', tabLabel: '属性', icon: 'detail-view', type: 'native_pages', native_page: 'portal.flow.definition-workbench', view: 'property' }],
      },
    },
  }, { initialContext: ctx })
}

// ————————————————————— 分组维护弹框 —————————————————————

function openGroupDialog (host, st) {
  const C = cmx()
  if (!customElements.get('cmx-floating-dialog')) { C.cmxError?.('弹框组件未就绪'); return }
  // 本地编辑副本（保存时整批提交：逐行 upsert；排序 = sortNo 重排）。
  const items = st.groups.map((g) => ({ ...g }))
  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: '维护流程分组', icon: 'folder', dialogWidth: '560px', dialogHeight: '70vh',
    showConfirm: false, showCancel: false,
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'flex:1;min-height:0;padding:6px 16px 12px;display:flex;flex-direction:column;font-size:13px;'
  wrap.innerHTML = `<style>
    .gl-list { flex:1; min-height:0; overflow-y:auto; display:flex; flex-direction:column; gap:6px; }
    .gl-row { display:flex; align-items:center; gap:6px; padding:6px 8px; border-radius:6px;
      border:1px solid var(--sapGroup_ContentBorderColor,#e9e9e9); background:var(--sapList_Background); }
    .gl-name { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
    .gl-name.off { color:var(--sapContent_LabelColor); text-decoration:line-through; }
    .gl-cnt { font-size:11px; color:var(--sapContent_LabelColor); flex-shrink:0; }
    .gl-add { display:flex; gap:6px; margin-top:10px; }
    .gl-add ui5-input { flex:1; }
    .dlg-foot { display:flex; justify-content:flex-end; gap:8px; padding:10px 0 0; border-top:1px solid var(--sapList_BorderColor,#e5e5e5); }
    .hint { font-size:12px; color:var(--sapContent_LabelColor); margin:2px 0 8px; }
  </style>
  <div class="hint">分组是一级扁平列表（非树）；停用只影响本页展示位（折叠置灰），不参与订阅匹配路由。</div>
  <div class="gl-list" id="glList"></div>
  <div class="gl-add">
    <ui5-input id="glNewName" placeholder="新分组名称（≤64 字符）"></ui5-input>
    <ui5-button design="Emphasized" icon="add" id="glAdd">新增</ui5-button>
  </div>
  <div class="dlg-foot">
    <ui5-button design="Transparent" id="glCancel">关闭</ui5-button>
  </div>`
  dlg.setContent(wrap)
  const drawRows = () => {
    const list = wrap.querySelector('#glList')
    list.innerHTML = items.map((g, i) => `<div class="gl-row" data-i="${i}">
      <span class="gl-name ${g.enabled ? '' : 'off'}" title="${esc(g.remark || g.name)}">${esc(g.name)}</span>
      <span class="gl-cnt">${g.defCount} 定义</span>
      <ui5-button design="Transparent" icon="navigation-up-arrow" data-act="up" title="上移"></ui5-button>
      <ui5-button design="Transparent" icon="navigation-down-arrow" data-act="down" title="下移"></ui5-button>
      <ui5-button design="Transparent" icon="${g.enabled ? 'pause' : 'play'}" data-act="toggle" title="${g.enabled ? '停用' : '启用'}"></ui5-button>
      <ui5-button design="Transparent" icon="delete" data-act="del" title="删除"></ui5-button>
    </div>`).join('') || '<div class="gl-cnt" style="padding:10px">暂无分组</div>'
    list.querySelectorAll('.gl-row')?.forEach((row) => {
      const i = Number(row.getAttribute('data-i'))
      row.querySelectorAll('ui5-button[data-act]')?.forEach((b) => {
        b.addEventListener('click', async () => {
          const act = b.getAttribute('data-act')
          const g = items[i]
          try {
            if (act === 'up' && i > 0) { items.splice(i - 1, 0, items.splice(i, 1)[0]); await persistOrder() }
            else if (act === 'down' && i < items.length - 1) { items.splice(i + 1, 0, items.splice(i, 1)[0]); await persistOrder() }
            else if (act === 'toggle') {
              await apiPost('/api/flow/definition-groups/save', { id: g.id, name: g.name, sortNo: g.sortNo, enabled: !g.enabled, remark: g.remark })
              g.enabled = !g.enabled
            } else if (act === 'del') {
              if (!await confirmBox(`确认删除分组「${g.name}」？`)) return
              await apiPost('/api/flow/definition-groups/delete', { id: g.id })
              items.splice(i, 1)
              showCmxToast('分组已删除')
            }
            drawRows()
            await refreshGroupsOnly(st)
          } catch (e) { cmx().cmxError?.(e.message) }
        })
      })
    })
  }
  const persistOrder = async () => {
    // 顺序变化即按当前顺序重写 sortNo（0..n-1，逐行 upsert）。
    for (let i = 0; i < items.length; i++) {
      const g = items[i]
      if (g.sortNo !== i) {
        await apiPost('/api/flow/definition-groups/save', { id: g.id, name: g.name, sortNo: i, enabled: g.enabled, remark: g.remark })
        g.sortNo = i
      }
    }
  }
  wrap.querySelector('#glAdd')?.addEventListener('click', async () => {
    const inp = wrap.querySelector('#glNewName')
    const name = (inp.value || '').trim()
    if (!name) return
    try {
      const r = (await apiPost('/api/flow/definition-groups/save', { name, sortNo: items.length, enabled: true })) || {}
      items.push({ id: Number(r.id) || Date.now(), name, sortNo: items.length, enabled: true, remark: '', defCount: 0 })
      inp.value = ''
      drawRows()
      await refreshGroupsOnly(st)
      showCmxToast('分组已新增')
    } catch (e) { cmx().cmxError?.(e.message) }
  })
  wrap.querySelector('#glCancel')?.addEventListener('click', () => dlg.close('cancel'))
  drawRows()
  // 组件无 open()/onClose：openModal 自动 append，resolve 即关闭——移除节点并刷新（mdm 同款）。
  dlg.openModal().then(() => { dlg.remove(); reload(host, st) })
}

async function refreshGroupsOnly (st) { try { await loadGroups(st) } catch { /* 弹框内静默 */ } }

// ————————————————————— 改分组弹框 —————————————————————

function openSetGroupDialog (host, st, rec) {
  const C = cmx()
  if (!customElements.get('cmx-floating-dialog')) { C.cmxError?.('弹框组件未就绪'); return }
  const dlg = document.createElement('cmx-floating-dialog')
  dlg.configure({
    title: `设置分组 · ${rec.name || rec.key}`, icon: 'folder', dialogWidth: '420px',
    showConfirm: true, confirmText: '保存', cancelText: '取消',
  })
  const wrap = document.createElement('div')
  wrap.style.cssText = 'padding:8px 4px;font-size:13px;'
  const opts = ['<ui5-option value="">未分组</ui5-option>']
    .concat(st.groups.map((g) => `<ui5-option value="${g.id}" ${rec.groupId === g.id ? 'selected' : ''}>${esc(g.name)}</ui5-option>`))
    .join('')
  wrap.innerHTML = `<div style="display:flex;flex-direction:column;gap:6px;margin-bottom:6px;">
    <label style="font-size:12px;color:var(--sapContent_LabelColor)">目标分组（未分组 = 不挂任何分组）</label>
    <ui5-select id="sgSel" style="width:100%">${opts}</ui5-select></div>`
  dlg.setContent(wrap)
  // 组件无 onConfirm：openModal resolve { action } 后自行保存（mdm 同款）。
  dlg.openModal().then(async (r) => {
    dlg.remove()
    if (!r || r.action !== 'confirm') return
    const v = (wrap.querySelector('#sgSel') || {}).value || ''
    const groupId = v ? Number(v) : null
    try {
      await apiPost('/api/flow/definitions/set-group', { keys: [rec.key], groupId })
      showCmxToast(groupId ? '分组已设置' : '已移出分组')
      reload(host, st)
    } catch (e) { cmx().cmxError?.(e.message) }
  })
}

// ————————————————————— native-page 入口 —————————————————————

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
        await Promise.all([loadGroups(st), loadRows(st)])
      } catch (e) { console.error('[definition-manager] init fail', e); cmx().cmxError?.(`初始化失败：${e.message}`) }
      if (host) whenRendered(host, '.pg', () => bind(host, st))
      return `<style>${styleCss()}</style>${viewHtml(st)}`
    },
  },
}
