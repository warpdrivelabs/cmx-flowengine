/**
 * Webhook 订阅管理工作台 —— native_pages 三区（001 方案 M2，方案见
 * documents/plans/20260901_流程引擎_出站webhook订阅入库与管理页面方案.md v2.3）。
 *
 * 骨架照 form-binding-admin（CFG 接缝 + 字段驱动渲染 + 共享 apiJson + UI5 组件 + Neo 主题），
 * 交互范式对标 mdm subscription-manager / dispatch-monitor（随机 secret、掩码沿用、测试投递、
 * 死信重发/处置）。
 *   explorer：启停筛选 tab（全部/启用/停用）+ 关键字过滤 + 订阅卡片列表 + 新增。
 *   content ：两个 tab——「订阅」编辑表单（通道下拉/服务键/回调路径/secret 只写掩码/
 *             definitionKeys 多选/eventTypes 复选默认推荐集/retry_max）+ 测试投递；
 *             「流水」投递记录（状态过滤 + 死信勾选重发/处置）。
 *   property：字段说明 + 投递口径（保序/退避/at-least-once/结果分类）+ 首启导入迁移说明。
 *
 * 数据源（前缀 /api/flow，经门户反代映射到引擎 /api/flow/v1）：/webhook-subscriptions/*（query/detail/
 * save/delete/set-active/test/channels）、/webhook-deliveries/*（query/retry/skip）。
 * 全部 POST + JSON body、无 Path Variable（GET 仅 detail/channels 简单只读）。
 */

const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

// 事件类型全集（与后端 webhook_admin::EVENT_TYPES 同源维护）；打星 = 管理页默认勾选推荐集。
const EVENT_TYPES = [
  { key: 'instance.started', label: '实例发起' },
  { key: 'instance.completed', label: '实例办结', rec: true },
  { key: 'instance.terminated', label: '实例终止' },
  { key: 'task.created', label: '待办产生', rec: true },
  { key: 'task.completed', label: '任务办结' },
  { key: 'task.reassigned', label: '任务转办' },
]
// 投递流水状态机（与后端 state 列值域同源）。
const DLV_STATES = ['PENDING', 'IN_FLIGHT', 'DONE', 'DEAD', 'SKIPPED']

const state = {
  tab: 'all',            // explorer 筛选：all | on | off
  keyword: '',
  items: [],             // 订阅列表（当前页）
  total: 0,
  channels: [],          // 注册表通道 [{type,name,configSchema}]
  definitions: [],       // 已装载定义 [{key,name}]（definitionKeys 多选数据源）
  selected: null,        // 当前编辑订阅行（null = 新增态）
  draft: null,           // 编辑中字段值
  contentTab: 'sub',     // content 区：sub | dlv
  dlv: { rows: [], total: 0, page: 1, pageSize: 20, stateFilter: '', checked: new Set() },
  hosts: new Set(),
}

const { escHtml: esc } = globalThis.__cmxDataComp // 共享转义（cmx-data-comp/lib/cmx-page-helpers.js）
const enc = encodeURIComponent

const { apiJson: _sharedApiJson } = globalThis.__cmxDataComp // 共享 fetch 封装；经 CFG 转发保留组件壳 configure() 契约
async function apiJson (url, options = {}) { return _sharedApiJson(url, options, CFG) }

const { showCmxToast: toast } = globalThis.__cmxDataComp // 共享 toast（cmx-data-comp/lib/cmx-toast.js）

// 确认框：cmxConfirm（组件库）优先，回退 window.confirm。
async function confirmBox (message, confirmText = '确认') {
  const C = (typeof globalThis !== 'undefined' && globalThis.__cmxDataComp) || {}
  return typeof C.cmxConfirm === 'function'
    ? await C.cmxConfirm({ message, intent: 'danger', confirmText })
    : window.confirm(message)
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const host = ctx.host
  state.hosts.add(host)
  if (host) host.__whaView = view
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(view)}`
    bind(root, view, host)
  }
  requestAnimationFrame(() => {
    render()
    if (view === 'explorer' || view === 'content') { loadChannels(); loadDefinitions(); loadList() }
  })
  return `<style>${styleCss()}</style>${viewHtml(view)}`
}

function refreshView (view) {
  for (const host of Array.from(state.hosts)) {
    if (!host || !host.isConnected) { state.hosts.delete(host); continue }
    if (host.__whaView !== view) continue
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

// ————————————————————— explorer 区 —————————————————————

function filteredItems () {
  const kw = state.keyword.trim().toLowerCase()
  return state.items.filter((it) => {
    if (state.tab === 'on' && !it.active) return false
    if (state.tab === 'off' && it.active) return false
    if (!kw) return true
    return String(it.name || '').toLowerCase().includes(kw)
      || String(it.channelConfig?.service_key || '').toLowerCase().includes(kw)
  })
}

function subCard (it) {
  const active = state.selected && state.selected.id === it.id
  const cfg = it.channelConfig || {}
  return `<button class="wha-row ${active ? 'active' : ''}" data-id="${esc(String(it.id))}">
    <b>${esc(it.name || '')}</b>
    <small>${esc(cfg.service_key || '')}${cfg.callback_path ? ' · ' + esc(cfg.callback_path) : ''}</small>
    <span class="wha-tags">
      <i class="wha-tag">${esc(it.channel || 'webhook')}</i>
      <i class="wha-tag ${it.active ? 'on' : 'off'}">${it.active ? '启用' : '停用'}</i>
      ${it.source === 'env' ? '<i class="wha-tag env">env 导入</i>' : ''}
    </span>
  </button>`
}

const TABS = [
  { key: 'all', label: '全部', icon: 'list' },
  { key: 'on', label: '启用', icon: 'play' },
  { key: 'off', label: '停用', icon: 'pause' },
]

function explorerHtml () {
  const tabs = TABS.map((t) => `<button class="wha-tab ${state.tab === t.key ? 'active' : ''}" data-tab="${t.key}">
    <ui5-icon name="${t.icon}"></ui5-icon><span>${esc(t.label)}</span>
  </button>`).join('')
  const rows = filteredItems().map(subCard).join('')
  return `<section class="wha wha-explorer">
    <div class="wha-fixed-head">
      <div class="wha-tabs">${tabs}</div>
      <div class="wha-search">
        <ui5-input data-kw value="${esc(state.keyword)}" placeholder="过滤名称 / 服务键…"></ui5-input>
        <ui5-button icon="add" design="Emphasized" data-act="new">新增</ui5-button>
      </div>
    </div>
    <div class="wha-list-head"><b>Webhook 订阅</b><span>${filteredItems().length} / ${state.total}</span></div>
    <div class="wha-list" data-list>${rows || '<div class="wha-empty"><ui5-icon name="sap-icons/s4hana/2"></ui5-icon><span>暂无订阅</span></div>'}</div>
  </section>`
}

function bindList (listEl) {
  listEl?.querySelectorAll('[data-id]').forEach((b) => b.addEventListener('click', () => selectRecord(Number(b.dataset.id))))
}

// ————————————————————— content 区 —————————————————————

function contentHtml () {
  const tabbar = `<div class="wha-ctabs">
    <button class="wha-ctab ${state.contentTab === 'sub' ? 'active' : ''}" data-ct="sub">订阅</button>
    <button class="wha-ctab ${state.contentTab === 'dlv' ? 'active' : ''}" data-ct="dlv">投递流水</button>
  </div>`
  return `<section class="wha wha-content">${tabbar}${state.contentTab === 'sub' ? subFormHtml() : deliveriesHtml()}</section>`
}

function fieldInput (f) {
  const d = state.draft || {}
  const val = d[f.k] ?? ''
  if (f.k === 'channel') {
    const opts = (state.channels.length ? state.channels : [{ type: 'webhook', name: 'Webhook 回调' }])
      .map((c) => `<ui5-option value="${esc(c.type)}"${val === c.type ? ' selected' : ''}>${esc(c.name || c.type)}${c.type !== 'webhook' ? '（未启用）' : ''}</ui5-option>`).join('')
    return `<ui5-select data-f="channel"${state.selected ? '' : ''}>${opts}</ui5-select>`
  }
  if (f.k === 'secret') {
    return `<div class="wha-secret">
      <ui5-input data-f="secret" type="Password" value="${esc(val)}" placeholder="留空 / 掩码 = 沿用旧密钥"></ui5-input>
      <ui5-button icon="key" data-act="gen-secret" design="Transparent">随机生成</ui5-button>
    </div>`
  }
  if (f.k === 'definitionKeys') {
    const defs = state.definitions
    const picked = new Set(d.definitionKeys || [])
    if (!defs.length) return `<div class="wha-hint">定义列表加载中 / 无已装载定义（空集 = 订阅全部流程）</div>`
    return `<div class="wha-checks">${defs.map((x) => `<label class="wha-check"><input type="checkbox" data-dk="${esc(x.key)}"${picked.has(x.key) ? ' checked' : ''}> ${esc(x.name ? `${x.name}（${x.key}）` : x.key)}</label>`).join('')}</div>`
  }
  if (f.k === 'eventTypes') {
    const picked = new Set(d.eventTypes || [])
    return `<div class="wha-checks">${EVENT_TYPES.map((t) => `<label class="wha-check"><input type="checkbox" data-et="${esc(t.key)}"${picked.has(t.key) ? ' checked' : ''}> ${esc(t.label)}<code>${esc(t.key)}</code></label>`).join('')}</div>`
  }
  if (f.k === 'active') {
    return `<ui5-switch data-f="active" checked="${!!d.active}"></ui5-switch>`
  }
  return `<ui5-input data-f="${esc(f.k)}" value="${esc(val)}" placeholder="${esc(f.ph || '')}"></ui5-input>`
}

const FIELDS = [
  { k: 'name', label: '订阅名', required: true, hint: '同租户唯一；投递流水以名称快照留档' },
  { k: 'channel', label: '通道', hint: 'v1 仅 webhook；kafka/rabbitmq 启用后出现在下拉（未启用通道后端拒绝保存）' },
  { k: 'service_key', label: '目标服务键', required: true, hint: '[service_rpc.services] 目录键——内部走注册发现，外部登记静态 url' },
  { k: 'callback_path', label: '回调路径', ph: '/api/mdm/flow/callback', hint: '以 / 开头；缺省兼容 mdm 回调端点' },
  { k: 'secret', label: '签名密钥（只写）', hint: 'HMAC-SHA256 共享密钥：只写不回显（列表掩码）；留空或保持掩码 = 沿用旧值。改密钥须同步接收端' },
  { k: 'definitionKeys', label: '订阅的流程（definitionKey）', hint: '不勾 = 订阅全部流程定义' },
  { k: 'eventTypes', label: '订阅的事件类型', hint: '不勾 = 订阅全部 6 种事件；推荐集 = 实例办结 + 待办产生' },
  { k: 'retry_max', label: '最大尝试次数（含首发）', hint: '默认 10 = 重试 9 次；1s 起指数退避封顶 5 分钟，首发到死信约 9~14 分钟' },
  { k: 'active', label: '启用', hint: '停用即不再生成新投递行（存量行保留可查可清）' },
]

function subFormHtml () {
  const d = state.draft || {}
  const isNew = !state.selected
  const fields = FIELDS.map((f) => `<div class="wha-field">
    <label>${esc(f.label)}${f.required ? ' <em>*</em>' : ''}</label>
    ${fieldInput(f)}
    <div class="wha-hint">${esc(f.hint || '')}</div>
  </div>`).join('')
  const toolbar = `<div class="wha-toolbar">
    <b class="wha-title">${isNew ? '新增订阅' : `编辑：${esc(state.selected.name || '')}`}</b>
    <span class="wha-tb-sp"></span>
    ${isNew ? '' : `<ui5-button icon="send" data-act="test">测试投递</ui5-button>
    <ui5-button icon="${d.active ? 'pause' : 'play'}" data-act="toggle">${d.active ? '停用' : '启用'}</ui5-button>`}
    <ui5-button icon="add" data-act="new">新增</ui5-button>
    <ui5-button icon="save" design="Emphasized" data-act="save">保存</ui5-button>
    <ui5-button icon="delete" design="Negative" data-act="delete"${isNew ? ' disabled' : ''}>删除</ui5-button>
  </div>`
  return `<div class="wha-subpane">
    ${toolbar}
    <div class="wha-form">${fields}</div>
  </div>`
}

function deliveriesHtml () {
  const st = state.dlv
  const canAct = st.checked.size > 0
  const opts = ['<option value="">全部状态</option>']
    .concat(DLV_STATES.map((s) => `<option value="${s}"${st.stateFilter === s ? ' selected' : ''}>${s}</option>`)).join('')
  const rows = st.rows.map((r) => {
    const dead = r.state === 'DEAD'
    return `<tr data-seq="${r.seq}">
      <td>${dead ? `<input type="checkbox" data-chk="${r.id}"${st.checked.has(r.id) ? ' checked' : ''}>` : ''}</td>
      <td><i class="wha-tag ${String(r.state).toLowerCase()}">${esc(r.state)}</i></td>
      <td>${esc(r.eventType)}</td>
      <td>${esc(r.definitionKey || '-')}</td>
      <td class="mono">${esc(r.subscriptionName)}</td>
      <td>${esc(String(r.attempts))}</td>
      <td>${r.lastHttpStatus ? esc(String(r.lastHttpStatus)) : '-'}</td>
      <td class="mono" title="${esc(r.lastError || '')}">${esc((r.lastError || '-').slice(0, 60))}</td>
      <td>${esc(r.source)}</td>
    </tr>`
  }).join('')
  const pages = Math.max(1, Math.ceil(st.total / st.pageSize))
  return `<div class="wha-subpane">
    <div class="wha-toolbar">
      <b class="wha-title">投递流水${state.selected ? ` · ${esc(state.selected.name)}` : '（全部订阅）'}</b>
      <span class="wha-tb-sp"></span>
      <ui5-button icon="refresh" data-act="dlv-refresh">刷新</ui5-button>
      <ui5-button icon="undo" data-act="dlv-retry"${canAct ? '' : ' disabled'}>重发勾选</ui5-button>
      <ui5-button icon="decline" design="Negative" data-act="dlv-skip"${canAct ? '' : ' disabled'}>处置勾选</ui5-button>
    </div>
    <div class="wha-dlvbar">
      <select class="wha-native-select" data-dlv-state-native>${opts}</select>
      <label class="wha-hint">勾选 DEAD 行后可「重发」（回队列重试）或「处置」（SKIPPED 留痕放弃）</label>
    </div>
    <div class="wha-table-wrap">
      <table class="wha-table">
        <thead><tr><th></th><th>状态</th><th>事件</th><th>流程</th><th>订阅</th><th>尝试</th><th>HTTP</th><th>最近错误</th><th>来源</th></tr></thead>
        <tbody>${rows || '<tr><td colspan="9" class="wha-empty-cell">暂无投递记录</td></tr>'}</tbody>
      </table>
    </div>
    <div class="wha-pager">
      <ui5-button icon="nav-back" data-dlv-page="prev"${st.page > 1 ? '' : ' disabled'}></ui5-button>
      <span>${st.page} / ${pages} · 共 ${st.total}</span>
      <ui5-button icon="nav-forward" data-dlv-page="next"${st.page < pages ? '' : ' disabled'}></ui5-button>
    </div>
  </div>`
}

// ————————————————————— property 区（说明） —————————————————————

function propertyHtml () {
  const fieldRows = FIELDS.map((f) => `<tr><td>${esc(f.label)}</td><td>${esc(f.hint || '')}</td></tr>`).join('')
  return `<section class="wha wha-property">
    <div class="wha-prop-head"><b>说明</b><span class="wha-mode">cmx_flow_webhook_subscription / _delivery</span></div>
    <div class="wha-prop-body">
      <div class="wha-sec">投递口径</div>
      <div class="wha-note">
        · <b>at-least-once</b>：投递行先落库再投递（重启不丢）；发送方长停顿的异常窗口可能重复投递，<b>接收方须按 delivery_id 或业务键幂等</b>。<br>
        · <b>同订阅严格保序</b>：按落库序投递；某行退避等待会压住同订阅后续（有序优先），终态（DONE/DEAD/SKIPPED）不阻塞。<br>
        · <b>结果分类</b>：408/429/5xx、超时、网络错误 → 退避重试（1s 起指数封顶 5 分钟）；其余 4xx（含 401/403）→ 直达死信（配置性错误重试无意义）。<br>
        · <b>死信</b>：重试耗尽进入 DEAD，可「重发」（回队列、尝试次数归零）或「处置」（SKIPPED 留痕放弃）；DONE/SKIPPED 默认保留 7 天。
      </div>
      <div class="wha-sec">测试投递</div>
      <div class="wha-note">
        向该订阅目标<b>真实投递</b>一条 <code>webhook.test</code> 伪事件（不走 definitionKeys/eventTypes 过滤），
        10s 短超时；结果同步返回并落审计行（失败也记 DONE + 错误留痕，不进死信队列）。同订阅 1 分钟至多 3 次。
      </div>
      <div class="wha-sec">首启导入（存量迁移）</div>
      <div class="wha-note">
        订阅表为空且配置了 <code>FLOW_WEBHOOK_TARGETS</code> 时，引擎首启按 <code>env-&lt;服务键&gt;</code> 确定性名导入订阅行
        （secret 沿用全局 <code>FLOW_WEBHOOK_SIGNING_KEY</code> 不断签，<span class="wha-tag env">env 导入</span>角标可辨）。
        建议尽快改配<b>每订阅独立密钥</b>并同步接收端；导入只发生一次，绝不覆盖手工改动。
      </div>
      <div class="wha-sec">多副本与主题</div>
      <div class="wha-note">
        投递子系统多副本安全（租约 + SKIP LOCKED）；订阅改动跨副本 ≤5s 收敛。
        <code>FLOW_WEBHOOK_MODE=legacy</code> 可回退内存链路（集群级配置，全量同配）。
      </div>
      <div class="wha-sec">字段</div>
      <table class="wha-ftab">${fieldRows}</table>
    </div>
  </section>`
}

// ————————————————————— 绑定 —————————————————————

function blankDraft () {
  return {
    name: '', channel: 'webhook', service_key: '', callback_path: '/api/mdm/flow/callback',
    secret: '', definitionKeys: [], eventTypes: EVENT_TYPES.filter((t) => t.rec).map((t) => t.key),
    retry_max: 10, active: true,
  }
}

function bind (root, view, host) {
  if (view === 'explorer') {
    root.querySelectorAll('[data-tab]').forEach((b) => b.addEventListener('click', () => { state.tab = b.dataset.tab; refreshView('explorer') }))
    root.querySelector('[data-kw]')?.addEventListener('input', (e) => {
      state.keyword = e.target.value || ''
      const list = root.querySelector('[data-list]')
      if (list) { list.innerHTML = filteredItems().map(subCard).join('') || '<div class="wha-empty"><span>无匹配订阅</span></div>'; bindList(list) }
      const head = root.querySelector('.wha-list-head span')
      if (head) head.textContent = `${filteredItems().length} / ${state.total}`
    })
    root.querySelector('[data-act="new"]')?.addEventListener('click', () => startNew())
    bindList(root.querySelector('[data-list]'))
  }
  if (view === 'content') {
    root.querySelectorAll('[data-ct]').forEach((b) => b.addEventListener('click', () => {
      state.contentTab = b.dataset.ct
      refreshView('content')
      if (state.contentTab === 'dlv') loadDeliveries()
    }))
    if (state.contentTab === 'sub') bindSubForm(root)
    else bindDeliveries(root)
  }
}

function bindSubForm (root) {
  root.querySelectorAll('[data-f]').forEach((inp) => {
    const key = inp.dataset.f
    if (key === 'active') {
      inp.addEventListener('change', () => { ensureDraft(); state.draft.active = !!inp.checked })
      return
    }
    inp.addEventListener('change', () => {
      ensureDraft()
      const val = inp.tagName === 'UI5-SELECT'
        ? (inp.selectedOption?.getAttribute('value') ?? '')
        : (inp.value ?? '')
      if (key === 'definitionKeys' || key === 'eventTypes') return // 复选组单独绑
      state.draft[key] = key === 'retry_max' ? (Number(val) || 10) : val
    })
  })
  root.querySelectorAll('[data-dk]').forEach((cb) => cb.addEventListener('change', () => {
    ensureDraft()
    const set = new Set(state.draft.definitionKeys || [])
    cb.checked ? set.add(cb.dataset.dk) : set.delete(cb.dataset.dk)
    state.draft.definitionKeys = Array.from(set)
  }))
  root.querySelectorAll('[data-et]').forEach((cb) => cb.addEventListener('change', () => {
    ensureDraft()
    const set = new Set(state.draft.eventTypes || [])
    cb.checked ? set.add(cb.dataset.et) : set.delete(cb.dataset.et)
    state.draft.eventTypes = Array.from(set)
  }))
  root.querySelector('[data-act="gen-secret"]')?.addEventListener('click', () => {
    const bytes = new Uint8Array(16)
    ;(globalThis.crypto || window.crypto).getRandomValues(bytes)
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
    ensureDraft(); state.draft.secret = hex
    const inp = root.querySelector('[data-f="secret"]')
    if (inp) { inp.value = hex; inp.type = 'Text'; toast('已生成 32 位随机密钥——保存后请同步到接收端') }
  })
  root.querySelector('[data-act="new"]')?.addEventListener('click', () => startNew())
  root.querySelector('[data-act="save"]')?.addEventListener('click', () => saveSubscription())
  root.querySelector('[data-act="delete"]')?.addEventListener('click', () => deleteSubscription())
  root.querySelector('[data-act="toggle"]')?.addEventListener('click', () => toggleActive())
  root.querySelector('[data-act="test"]')?.addEventListener('click', () => testDelivery())
}

function bindDeliveries (root) {
  root.querySelector('[data-dlv-state-native]')?.addEventListener('change', (e) => {
    state.dlv.stateFilter = e.target.value || ''
    state.dlv.page = 1; state.dlv.checked = new Set()
    loadDeliveries()
  })
  root.querySelector('[data-act="dlv-refresh"]')?.addEventListener('click', () => loadDeliveries())
  root.querySelectorAll('[data-chk]').forEach((cb) => cb.addEventListener('change', () => {
    const id = Number(cb.dataset.chk)
    cb.checked ? state.dlv.checked.add(id) : state.dlv.checked.delete(id)
    refreshView('content')
  }))
  root.querySelector('[data-act="dlv-retry"]')?.addEventListener('click', () => actDeliveries('retry'))
  root.querySelector('[data-act="dlv-skip"]')?.addEventListener('click', () => actDeliveries('skip'))
  root.querySelectorAll('[data-dlv-page]').forEach((b) => b.addEventListener('click', () => {
    if (b.dataset.dlvPage === 'prev' && state.dlv.page > 1) state.dlv.page--
    if (b.dataset.dlvPage === 'next') state.dlv.page++
    loadDeliveries()
  }))
}

function ensureDraft () { if (!state.draft) state.draft = blankDraft() }

function startNew () {
  state.selected = null
  state.draft = blankDraft()
  state.contentTab = 'sub'
  refreshAll()
}

function selectRecord (id) {
  const it = state.items.find((x) => Number(x.id) === id)
  if (!it) return
  state.selected = it
  state.draft = {
    name: it.name || '',
    channel: it.channel || 'webhook',
    service_key: it.channelConfig?.service_key || '',
    callback_path: it.channelConfig?.callback_path || '',
    secret: it.channelConfig?.secret || '', // 掩码值：原样回传 = 后端沿用旧值
    definitionKeys: it.definitionKeys || [],
    eventTypes: it.eventTypes || [],
    retry_max: it.retryMax ?? 10,
    active: !!it.active,
  }
  refreshAll()
}

// ————————————————————— 数据 / 动作 —————————————————————

async function loadChannels () {
  try {
    const d = await apiJson('/api/flow/webhook-subscriptions/channels')
    state.channels = d.channels || []
  } catch { state.channels = [] }
  refreshView('content')
}

async function loadDefinitions () {
  try {
    const d = await apiJson('/api/flow/definitions')
    state.definitions = (d.definitions || []).map((x) => ({ key: x.key, name: x.name || '' }))
  } catch { state.definitions = [] }
  refreshView('content')
}

async function loadList () {
  try {
    const d = await apiJson('/api/flow/webhook-subscriptions/query', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ page: 1, pageSize: 200 }),
    })
    state.items = d.rows || []
    state.total = d.total ?? state.items.length
  } catch (e) { toast('加载订阅失败: ' + e.message); state.items = [] }
  refreshView('explorer')
}

async function saveSubscription () {
  const d = state.draft || {}
  if (!String(d.name || '').trim()) { toast('请填写订阅名'); return }
  if (!String(d.service_key || '').trim()) { toast('请填写目标服务键'); return }
  const path = String(d.callback_path || '').trim()
  if (path && !path.startsWith('/')) { toast('回调路径须以 / 开头'); return }
  const body = {
    id: state.selected?.id,
    name: String(d.name).trim(),
    channel: d.channel || 'webhook',
    channelConfig: {
      service_key: String(d.service_key).trim(),
      callback_path: path || '/api/mdm/flow/callback',
      secret: String(d.secret || ''), // 掩码/空 = 后端沿用旧值；新密钥为明文
    },
    definitionKeys: d.definitionKeys || [],
    eventTypes: d.eventTypes || [],
    active: d.active !== false,
    retryMax: Number(d.retry_max) || 10,
  }
  try {
    const r = await apiJson('/api/flow/webhook-subscriptions/save', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    toast('已保存：' + (r?.subscription?.name || body.name))
    await loadList()
    if (r?.subscription?.id) selectRecord(r.subscription.id)
  } catch (e) { toast('保存失败: ' + e.message) }
}

async function deleteSubscription () {
  const it = state.selected
  if (!it) return
  if (!(await confirmBox(`确认删除订阅「${it.name}」？\n（仅停用态可删；历史投递流水以名称快照保留审计）`, '删除'))) return
  try {
    await apiJson('/api/flow/webhook-subscriptions/delete', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: it.id }),
    })
    toast('已删除')
    state.selected = null; state.draft = blankDraft()
    await loadList(); refreshAll()
  } catch (e) { toast('删除失败: ' + e.message) }
}

async function toggleActive () {
  const it = state.selected
  if (!it) return
  try {
    await apiJson('/api/flow/webhook-subscriptions/set-active', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: it.id, active: !it.active }),
    })
    toast(!it.active ? '已启用（≤5s 生效）' : '已停用（不再生成新投递行）')
    await loadList()
    selectRecord(it.id)
  } catch (e) { toast('操作失败: ' + e.message) }
}

async function testDelivery () {
  const it = state.selected
  if (!it) return
  toast('测试投递中（短超时 10s）…')
  try {
    const r = await apiJson('/api/flow/webhook-subscriptions/test', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: it.id }),
    })
    if (r?.success) toast(`✅ 测试投递成功（deliveryId=${r.deliveryId}）`)
    else toast(`❌ 测试失败：${r?.httpStatus ? 'HTTP ' + r.httpStatus + ' ' : ''}${r?.error || '未知错误'}（已留审计痕）`)
  } catch (e) { toast('测试请求失败: ' + e.message) }
}

async function loadDeliveries () {
  const st = state.dlv
  try {
    const body = { page: st.page, pageSize: st.pageSize }
    if (st.stateFilter) body.state = st.stateFilter
    if (state.selected) body.subscriptionId = state.selected.id
    const d = await apiJson('/api/flow/webhook-deliveries/query', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    st.rows = d.rows || []
    st.total = d.total ?? 0
    st.checked = new Set()
  } catch (e) { toast('加载流水失败: ' + e.message); st.rows = [] }
  refreshView('content')
}

async function actDeliveries (kind) {
  const ids = Array.from(state.dlv.checked)
  if (!ids.length) { toast('请先勾选 DEAD 行'); return }
  const verb = kind === 'retry' ? '重发' : '处置为 SKIPPED（确认放弃）'
  if (!(await confirmBox(`确认对勾选的 ${ids.length} 条死信执行「${verb}」？`, kind === 'retry' ? '重发' : '处置'))) return
  try {
    const r = await apiJson(`/api/flow/webhook-deliveries/${kind}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ids }),
    })
    toast(`${kind === 'retry' ? '已重发' : '已处置'} ${r?.reset ?? r?.skipped ?? 0} 条`)
    loadDeliveries()
  } catch (e) { toast('操作失败: ' + e.message) }
}

// ————————————————————— 样式（Neo 主题：色值全部 --sap*/--neo* 变量派生，light/dark 自动跟随） —————————————————————

function styleCss () {
  return `
  :host, .wha {
    --bg: var(--sapBackgroundColor, #f6f8fa);
    --panel: var(--sapList_Background, #fff);
    --ink: var(--sapTextColor, #1d2d3e);
    --muted: var(--sapContent_LabelColor, #6a6d70);
    --line: var(--sapGroup_ContentBorderColor, #d9d9d9);
    --line-soft: var(--neo-border-subtle, var(--sapTile_SeparatorColor, #e9e9e9));
    --brand: var(--neo-accent, #00b4d8);
    --brand-soft: color-mix(in srgb, var(--neo-accent, #00b4d8) 12%, transparent);
    --ok: var(--sapPositiveColor, #107e3e);
    --ok-soft: color-mix(in srgb, var(--sapPositiveColor, #107e3e) 12%, transparent);
    --warn: var(--neo-warn, #f59e0b);
    --warn-soft: color-mix(in srgb, var(--neo-warn, #f59e0b) 12%, transparent);
    --danger: var(--sapNegativeColor, #bb0000);
    --danger-soft: color-mix(in srgb, var(--sapNegativeColor, #bb0000) 12%, transparent);
    --mono: ui-monospace, Menlo, Consolas, monospace;
  }
  .wha { font: 13px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif; color: var(--ink); height: 100%; box-sizing: border-box; display: flex; flex-direction: column; background: var(--bg); }
  .wha * { box-sizing: border-box; }
  /* explorer */
  .wha-fixed-head { position: sticky; top: 0; z-index: 6; background: var(--bg); }
  .wha-tabs { display: flex; gap: 4px; padding: 8px; flex-wrap: wrap; border-bottom: 1px solid var(--line-soft); }
  .wha-tab { display: flex; align-items: center; gap: 5px; font: inherit; font-size: 12px; font-weight: 700; padding: 6px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--panel); color: var(--muted); cursor: pointer; }
  .wha-tab:hover { border-color: var(--brand); }
  .wha-tab.active { background: var(--brand); color: #fff; border-color: var(--brand); }
  .wha-tab ui5-icon { width: 14px; height: 14px; }
  .wha-search { display: flex; gap: 6px; padding: 8px; border-bottom: 1px solid var(--line-soft); align-items: center; }
  .wha-search ui5-input { flex: 1; min-width: 0; }
  .wha-list-head { display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; font-size: 11px; color: var(--muted); text-transform: uppercase; font-weight: 800; }
  .wha-list-head span { background: var(--brand-soft); color: var(--brand); border-radius: 20px; padding: 1px 8px; }
  .wha-list { flex: 1; overflow-y: auto; padding: 0 8px 8px; }
  .wha-row { display: flex; flex-direction: column; align-items: flex-start; width: 100%; text-align: left; padding: 8px 10px; border: 1px solid var(--line-soft); border-radius: 8px; margin-bottom: 5px; background: var(--panel); cursor: pointer; gap: 2px; }
  .wha-row:hover { border-color: var(--brand); }
  .wha-row.active { background: var(--brand-soft); border-color: var(--brand); }
  .wha-row b { font-size: 13px; color: var(--ink); }
  .wha-row small { color: var(--muted); font-size: 11px; font-family: var(--mono); }
  .wha-tags { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 2px; }
  .wha-tag { font-style: normal; font-size: 10px; font-weight: 700; padding: 1px 7px; border-radius: 10px; background: var(--brand-soft); color: var(--brand); }
  .wha-tag.on { background: var(--ok-soft); color: var(--ok); }
  .wha-tag.off { background: color-mix(in srgb, var(--muted) 14%, transparent); color: var(--muted); }
  .wha-tag.env { background: var(--warn-soft); color: var(--warn); }
  .wha-tag.pending { background: var(--warn-soft); color: var(--warn); }
  .wha-tag.in_flight { background: var(--brand-soft); color: var(--brand); }
  .wha-tag.done { background: var(--ok-soft); color: var(--ok); }
  .wha-tag.dead { background: var(--danger-soft); color: var(--danger); }
  .wha-tag.skipped { background: color-mix(in srgb, var(--muted) 14%, transparent); color: var(--muted); }
  .wha-empty { display: flex; flex-direction: column; align-items: center; gap: 8px; color: var(--muted); padding: 36px 16px; font-size: 12.5px; }
  /* content */
  .wha-ctabs { display: flex; gap: 4px; padding: 8px 12px 0; border-bottom: 1px solid var(--line-soft); background: var(--panel); }
  .wha-ctab { font: inherit; font-size: 12.5px; font-weight: 700; padding: 7px 14px; border: none; border-bottom: 2px solid transparent; background: transparent; color: var(--muted); cursor: pointer; }
  .wha-ctab.active { color: var(--brand); border-bottom-color: var(--brand); }
  .wha-subpane { flex: 1; display: flex; flex-direction: column; min-height: 0; }
  .wha-toolbar { display: flex; align-items: center; gap: 6px; padding: 8px 12px; border-bottom: 1px solid var(--line-soft); background: var(--panel); flex-wrap: wrap; position: sticky; top: 0; z-index: 5; }
  .wha-title { font-size: 14px; color: var(--ink); } .wha-tb-sp { flex: 1; }
  .wha-form { padding: 16px 14px; max-width: 640px; overflow-y: auto; }
  .wha-field { margin-bottom: 14px; }
  .wha-field label { display: block; font-size: 11px; font-weight: 700; color: var(--muted); text-transform: uppercase; margin-bottom: 4px; }
  .wha-field label em { color: var(--danger); font-style: normal; }
  .wha-field ui5-input, .wha-field ui5-select { width: 100%; }
  .wha-secret { display: flex; gap: 6px; align-items: center; }
  .wha-secret ui5-input { flex: 1; }
  .wha-checks { display: flex; flex-direction: column; gap: 4px; border: 1px solid var(--line-soft); border-radius: 8px; padding: 8px 10px; background: var(--panel); }
  .wha-check { display: flex; align-items: center; gap: 6px; font-size: 12.5px; cursor: pointer; flex-wrap: wrap; }
  .wha-check code { font-family: var(--mono); font-size: 10.5px; color: var(--muted); background: var(--bg); border-radius: 4px; padding: 0 5px; }
  .wha-hint { font-size: 11px; color: var(--muted); margin-top: 3px; }
  /* 流水 */
  .wha-dlvbar { display: flex; align-items: center; gap: 10px; padding: 8px 12px; }
  .wha-native-select { font: inherit; font-size: 12.5px; padding: 4px 8px; border: 1px solid var(--line); border-radius: 8px; background: var(--panel); color: var(--ink); }
  .wha-table-wrap { flex: 1; overflow: auto; padding: 0 12px; }
  .wha-table { width: 100%; border-collapse: collapse; font-size: 12px; background: var(--panel); }
  .wha-table th { position: sticky; top: 0; background: var(--panel); text-align: left; font-size: 10.5px; text-transform: uppercase; color: var(--muted); padding: 7px 8px; border-bottom: 1px solid var(--line-soft); }
  .wha-table td { padding: 6px 8px; border-bottom: 1px solid var(--line-soft); vertical-align: top; }
  .wha-table td.mono { font-family: var(--mono); font-size: 11px; }
  .wha-empty-cell { text-align: center; color: var(--muted); padding: 28px 0; }
  .wha-pager { display: flex; align-items: center; gap: 10px; padding: 8px 12px; font-size: 12px; color: var(--muted); }
  /* property */
  .wha-prop-head { display: flex; align-items: center; gap: 8px; padding: 12px 14px; border-bottom: 1px solid var(--line-soft); font-size: 14px; font-weight: 750; }
  .wha-mode { font-size: 11px; font-weight: 700; padding: 2px 9px; border-radius: 20px; background: var(--brand-soft); color: var(--brand); font-family: var(--mono); }
  .wha-prop-body { padding: 14px; overflow-y: auto; }
  .wha-sec { font-size: 11px; font-weight: 800; color: var(--brand); text-transform: uppercase; margin: 14px 0 8px; padding-bottom: 5px; border-bottom: 1px solid var(--line-soft); }
  .wha-sec:first-child { margin-top: 0; }
  .wha-note { background: var(--brand-soft); border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent); border-radius: 8px; padding: 10px 12px; font-size: 12.5px; color: var(--ink); }
  .wha-note code { font-family: var(--mono); background: var(--panel); padding: 1px 5px; border-radius: 4px; }
  .wha-ftab { width: 100%; border-collapse: collapse; font-size: 12px; }
  .wha-ftab td { padding: 6px 8px; border-bottom: 1px solid var(--line-soft); vertical-align: top; }
  .wha-ftab td:first-child { font-weight: 700; white-space: nowrap; color: var(--muted); }
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
