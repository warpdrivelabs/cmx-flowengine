/**
 * 任务表单宿主 —— native_pages（由待办中心 openWorkNode 动态打开，不进菜单）。F3。
 *
 * props（来自待办中心，见 openTaskForm）：
 *   mode:'task', formKey, formMode(approve|edit|readonly), taskId, instanceId,
 *   domain/application/module/file, bizTable, bizId, apiPath, title
 *
 * content 区：业务单据审阅区（native 表单：变量+单据只读视图；F4 接通用动态渲染）。
 *            html-pages 类表单不走本页 content——由待办中心把 content 视图直接指向 html_pages，
 *            业务表单由门户原生 hydrate 渲染；本页只提供 property 区的审批控制台。
 * property 区：审批控制台（历史意见 + 我的意见 + 同意/驳回/办结）+ 流程轨迹。两类表单共用。
 *
 * 办结走 F1 已通用化的 complete（塞 lastDecision/comment），成功后广播 cmx-flow-task-done
 * 让待办中心刷新，并尝试关闭本 tab。认领/转签由待办中心侧完成，此页专注办理。
 *
 * F3 首版：业务单据区用「变量 + 单据行的只读 JSON 视图」兜底（不硬耦合 doc-loader 的 meta 渲染）；
 * F4 再接通用动态字段渲染。formMode=edit 的可编辑收紧也留 F4。
 */

const esc = (s) => String(s ?? '')
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  .replace(/"/g, '&quot;').replace(/'/g, '&#39;')
const enc = encodeURIComponent

// —— S4 抽核：配置接缝 ——（详见 todo-center.js 同名 CFG 注释）
// 门户壳用默认值 = 今天行为；组件壳/headless 壳 configure({...}) 覆盖 apiBase/authHeaders/getUser +
// onTaskDone（办结后通知，门户=派发 cmx-flow-task-done 让待办中心刷新）+ onClose（关闭当前视图，
// 门户=探测 closeTab 关工作区 Tab，组件壳=派发事件让宿主收起面板）。
const CFG = {
  apiBase: '',
  fetchInit: { credentials: 'same-origin' },
  authHeaders: () => ({}),
  onTaskDone: (detail) => {                   // 办结/发起成功后广播；门户默认派发全局事件
    try {
      const ev = new CustomEvent('cmx-flow-task-done', { detail: detail || {} })
      window.dispatchEvent(ev); window.top?.dispatchEvent?.(ev)
    } catch {}
  },
  onClose: (nodeId) => {                       // 关闭当前任务视图；门户默认探测工作区 Tab 关闭链
    const targets = [window, window.parent, window.top, globalThis].filter(Boolean)
    for (const t of targets) {
      try {
        if (typeof t.closeTab === 'function') { t.closeTab(nodeId); return }
        if (typeof t.closeWorkspaceNode === 'function') { t.closeWorkspaceNode(nodeId); return }
      } catch {}
    }
    try {
      window.top?.postMessage({ type: 'closeTab', payload: { id: nodeId } }, '*')
      document.dispatchEvent(new CustomEvent('cmx-close-workspace-node', { detail: { id: nodeId }, bubbles: true, composed: true }))
    } catch {}
  },
}
function configure (o) { Object.assign(CFG, o || {}); return CFG }

async function apiJson (url, options = {}) {
  // S4：apiBase 前缀（门户空串=同源）+ CFG.authHeaders/fetchInit。
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

function propsOf (ctx) {
  const p = (ctx && ctx.props) || (ctx && ctx.host && ctx.host.__props) || {}
  return {
    mode: p.mode || 'task',
    formKey: p.formKey || '',
    formMode: p.formMode || 'approve',
    viewOnly: !!p.viewOnly,
    taskId: p.taskId || '',
    instanceId: p.instanceId || '',
    bizTable: p.bizTable || '',
    bizId: p.bizId || '',
    domain: p.domain || '', application: p.application || '', module: p.module || '', file: p.file || '',
    apiPath: p.apiPath || '',
    // 发起态（mode:'start'）字段
    definitionKey: p.definitionKey || '', definitionName: p.definitionName || '', startFormKey: p.startFormKey || '',
    title: p.title || '任务表单',
  }
}

function hostRoot (host) {
  return host?.renderRoot || host?.shadowRoot?.querySelector('.native-page-root') || null
}

// 每个 (instanceId,taskId) 一份独立状态（两条待办可并排打开）。
const instances = {}
function stOf (p) {
  const key = `${p.instanceId}@@${p.taskId}`
  if (!instances[key]) {
    instances[key] = { props: p, inst: null, biz: null, comments: [], busy: false, hosts: new Set() }
  }
  instances[key].props = p
  return instances[key]
}

const fmtTime = (iso) => String(iso || '').replace('T', ' ').slice(0, 16)

// ————————————————————— native-page 入口 —————————————————————

function mount (ctx, view) {
  const p = propsOf(ctx)
  const st = stOf(p)
  const host = ctx.host
  st.hosts.add(host)
  if (host) { host.__tfView = view; host.__tfKey = `${p.instanceId}@@${p.taskId}` }
  const render = () => {
    const root = hostRoot(host)
    if (!root || !root.isConnected) return
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(st, view)}`
    bind(root, st, view, host)
  }
  requestAnimationFrame(async () => { render(); await loadAll(st); refreshAll(st) })
  return `<style>${styleCss()}</style>${viewHtml(st, view)}`
}

function refreshAll (st) {
  for (const host of Array.from(st.hosts)) {
    if (!host || !host.isConnected) { st.hosts.delete(host); continue }
    const root = hostRoot(host)
    if (!root) continue
    const view = host.__tfView
    root.innerHTML = `<style>${styleCss()}</style>${viewHtml(st, view)}`
    bind(root, st, view, host)
  }
}

function viewHtml (st, view) {
  if (st.props.mode === 'start') {
    return view === 'property' ? startPropHtml(st) : startContentHtml(st)
  }
  if (view === 'property') return trailHtml(st)
  return contentHtml(st)
}

// ————————————————————— 数据装载 —————————————————————

async function loadAll (st) {
  const p = st.props
  if (p.mode === 'start') { st.inst = null; st.comments = []; return }
  try {
    if (p.instanceId) {
      st.inst = await apiJson(`/api/flow/instances/${enc(p.instanceId)}`)
      const c = await apiJson(`/api/flow/instances/${enc(p.instanceId)}/comments`)
      st.comments = c.comments || []
    }
  } catch { st.inst = null; st.comments = [] }
  // 业务单据：F3 首版用变量投影 + 关联单据坐标兜底展示。真正拉 cf_* 行留 F4 接通用渲染。
  st.biz = null
}

// ————————————————————— 发起态（mode:'start'）—————————————————————
// 起点表单：F4 首版用「通用字段录入」（单号/申请人/金额/摘要 + 自定义键值），提交=建单据引用 + 起流程。
// html-pages 起点表单则由门户原生渲染业务表单（content 走 html_pages），发起动作在 property。

function startContentHtml (st) {
  const p = st.props
  return `<section class="tf">
    <div class="tf-bar"><div class="tf-bar-main"><b>${esc(p.title)}</b><span>${esc(p.definitionKey)}${p.startFormKey ? ' · ' + esc(p.startFormKey) : ''}</span></div>
      <span class="tf-mode edit">发起</span></div>
    <div class="tf-body">
      <div class="tf-panel">
        <div class="tf-panel-head"><ui5-icon name="add-document"></ui5-icon> 新建单据</div>
        <div class="tf-kvs">
          ${startField('businessKey', '单号', '如 PAY-2026-0001')}
          ${startField('applicant', '申请人', '用户 id')}
          ${startField('amount', '金额', '数字', 'number')}
          ${startField('bizId', '单据主键', '业务单据 id')}
        </div>
        <label class="tf-label" style="margin-top:12px">摘要</label>
        <textarea class="tf-comment" data-sf="summary" placeholder="单据摘要（可空）"></textarea>
        <div class="tf-hint" style="margin-top:8px">提交后按此建立单据引用并发起流程；重数据在业务表，此处只填驱动流转的关键字段。</div>
      </div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}
function startField (key, label, ph, type) {
  return `<div class="tf-kv"><span>${esc(label)}</span><input data-sf="${esc(key)}" type="${type || 'text'}" placeholder="${esc(ph)}" style="width:100%;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:6px;padding:5px 8px;margin-top:3px"></div>`
}

function startPropHtml (st) {
  const p = st.props
  return `<section class="tf">
    <div class="tf-prop-head"><b>发起流程</b><small>${esc(p.definitionName || p.definitionKey)}</small></div>
    <div class="tf-prop-body">
      <div class="tf-psec">流程</div>
      <div class="tf-kv"><span>定义</span><b>${esc(p.definitionName || p.definitionKey)}</b></div>
      <div class="tf-kv" style="margin-top:8px"><span>起点表单</span><b>${esc(p.startFormKey || '（无）')}</b></div>
      <div class="tf-psec">操作</div>
      <div class="tf-actions"><button class="tf-btn ok" data-start-submit>提交并发起</button></div>
      <div class="tf-hint" style="margin-top:8px">从 content 表单收集字段作为流程变量，起实例后令牌进入首个环节。</div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}

// 收集 content 起点表单字段（跨所有 host 找 data-sf）。
function collectStartFields (st) {
  const out = {}
  for (const h of Array.from(st.hosts)) {
    const root = hostRoot(h)
    if (!root) continue
    root.querySelectorAll?.('[data-sf]').forEach((el) => {
      const k = el.getAttribute('data-sf')
      let v = el.value
      if (el.type === 'number' && v !== '') v = Number(v)
      if (v !== '' && v != null) out[k] = v
    })
  }
  return out
}

async function submitStart (st, host) {
  if (st.busy) return
  const p = st.props
  const fields = collectStartFields(st)
  st.busy = true
  try {
    const bizId = fields.bizId || fields.businessKey || ('BIZ-' + (fields.applicant || 'x'))
    const variables = { ...fields, bizTable: p.bizTable || '', bizId }
    delete variables.businessKey
    const body = {
      definitionKey: p.definitionKey,
      businessKey: fields.businessKey || null,
      variables,
    }
    if (p.bizTable) body.bizLink = { bizTable: p.bizTable, bizId }
    const r = await apiJson('/api/flow/instances', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    toast(st, '已发起流程实例 ' + (r.id ? String(r.id).slice(0, 8) : ''))
    CFG.onTaskDone({ started: true, definitionKey: p.definitionKey })
    setTimeout(() => closeSelf({ instanceId: 'start', taskId: p.definitionKey }), 600)
  } catch (e) {
    st.busy = false
    toast(st, '发起失败: ' + e.message)
  }
}

// ————————————————————— content 区 —————————————————————

function contentHtml (st) {
  const p = st.props
  const vars = st.inst?.variables || {}
  const readOnly = p.formMode === 'readonly'
  const noComment = p.formMode === 'readonly'
  // 业务单据审阅（F3：变量 + 单据坐标的只读视图；F4 换动态字段渲染）
  const bizRows = Object.entries(vars)
    .filter(([k]) => !['bizTable', 'bizId'].includes(k))
    .map(([k, v]) => `<div class="tf-kv"><span>${esc(k)}</span><b>${esc(typeof v === 'object' ? JSON.stringify(v) : v)}</b></div>`)
    .join('') || '<div class="tf-hint">无业务变量</div>'
  const bizRef = p.bizTable ? `<div class="tf-bizref"><ui5-icon name="document"></ui5-icon> ${esc(p.bizTable)} / ${esc(p.bizId)}</div>` : ''

  const history = commentHistoryHtml(st)

  const approveArea = approveAreaHtml(p, noComment)

  return `<section class="tf">
    <div class="tf-bar">
      <div class="tf-bar-main"><b>${esc(p.title)}</b><span>${esc(p.formKey || '无绑定表单')} · ${esc(modeLabel(p.formMode))}</span></div>
      <span class="tf-mode ${p.formMode}">${esc(modeLabel(p.formMode))}</span>
    </div>
    <div class="tf-body">
      <div class="tf-panel">
        <div class="tf-panel-head"><ui5-icon name="document-text"></ui5-icon> 业务单据${readOnly ? '（只读审阅）' : ''}</div>
        ${bizRef}
        <div class="tf-kvs">${bizRows}</div>
      </div>
      <div class="tf-panel">
        <div class="tf-panel-head"><ui5-icon name="comment"></ui5-icon> 审批</div>
        <div class="tf-history">${history}</div>
        ${approveArea}
      </div>
    </div>
    <div class="tf-toast"></div>
  </section>`
}

// 办理人显示名：优先服务端姓名快照（nickName 昵称优先 / userName username 口径，
// 20260827 起随意见落库），存量行无快照回退 userId。
function displayUserName (c) {
  return c.nickName || c.userName || c.userId || '—'
}

// 历程含制单节点留痕；decision/comment 为空时补业务动作，避免渲染成空卡片。
function commentHistoryHtml (st) {
  return st.comments.length
    ? st.comments.map((c) => `<div class="tf-cmt">
        <div class="tf-cmt-head"><b>${esc(displayUserName(c))}</b>
          <span class="tf-dec ${c.decision === 'reject' ? 'rej' : 'ok'}">${esc(commentDecisionText(c))}</span>
          <em>${esc(fmtTime(c.createdAt))}</em></div>
        <div class="tf-cmt-body">${esc(commentBodyText(c))}</div></div>`).join('')
    : '<div class="tf-hint">暂无审批意见</div>'
}

function isCreationComment (c) {
  return String(c.nodeBpmnId || '').trim().toLowerCase() === 'apply'
}

function commentDecisionText (c) {
  const decision = String(c.decision || '').trim()
  if (decision) {
    return ({ approve: '同意', reject: '驳回', return: '退回' })[decision.toLowerCase()] || decision
  }
  return isCreationComment(c) ? '制单' : '办理'
}

function commentBodyText (c) {
  const comment = String(c.comment || '').trim()
  if (comment) return comment
  return isCreationComment(c) ? '制单提交' : '（未填写意见）'
}

// 审批动作区（同意/驳回/退回上一步/退回到…），content 与 property 两处共用——改一处即可，防漂移。
function approveAreaHtml (p, noComment) {
  if (p.viewOnly) return ''  // 查看态：只看单据/轨迹 + 意见历史，不出任何办结动作
  if (noComment) return '<div class="tf-actions"><button class="tf-btn ok" data-confirm>确认知悉（办结）</button></div>'
  return `<label class="tf-label">我的意见</label>
       <textarea class="tf-comment" data-comment placeholder="填写审批意见..."></textarea>
       <div class="tf-actions">
         <button class="tf-btn ok" data-approve>同意</button>
         <button class="tf-btn danger" data-reject>驳回</button>
         <button class="tf-btn" data-return title="退回到上一审批节点重新办理">退回上一步</button>
         <button class="tf-btn" data-return-pick title="选择任意上游节点退回">退回到…</button>
       </div>
       <div class="tf-return-menu" data-return-menu hidden></div>`
}

function modeLabel (m) {
  return m === 'edit' ? '可编辑' : (m === 'readonly' ? '只读' : '审批')
}

// ————————————————————— property 区（审批控制台 + 轨迹） —————————————————————
// 审批控制台放 property，让 native 与 html-pages 两类表单共用：html 表单的 content 由门户
// 原生渲染业务表单，审批动作统一走这里的 property 控制台。

function trailHtml (st) {
  const p = st.props
  const inst = st.inst
  const noComment = p.formMode === 'readonly'
  const cur = new Set((inst?.tokens || []).map((k) => k.nodeBpmnId))
  const rows = (inst?.nodes || []).map((n) => {
    const on = cur.has(n.id)
    const done = (inst?.tasks || []).some((x) => x.nodeBpmnId === n.id && x.completed)
    return `<div class="tf-trail ${on ? 'cur' : (done ? 'done' : '')}"><span class="tf-dot"></span><b>${esc(n.name || n.id)}</b><em>${on ? '当前' : (done ? '已过' : '')}</em></div>`
  }).join('') || '<div class="tf-hint">（轨迹以令牌位置为准）</div>'
  const history = commentHistoryHtml(st)
  const approveArea = approveAreaHtml(p, noComment)
  return `<section class="tf">
    <div class="tf-prop-head"><b>${esc(inst?.businessKey || p.instanceId || '')}</b><small>${esc(p.formKey || '')} · ${esc(modeLabel(p.formMode))}</small></div>
    <div class="tf-prop-body">
      <div class="tf-psec">审批</div>
      <div class="tf-history">${history}</div>
      ${approveArea}
      <div class="tf-psec">流程轨迹</div>${rows}
    </div>
    <div class="tf-toast"></div>
  </section>`
}

// ————————————————————— 事件 + 办结 —————————————————————

function bind (root, st, view, host) {
  // 发起态：提交并发起。
  root.querySelector('[data-start-submit]')?.addEventListener('click', () => submitStart(st, host))
  // 审批动作在 content(native 表单) 和 property(审批控制台，html 表单也用) 两处都可能出现。
  root.querySelector('[data-approve]')?.addEventListener('click', () => complete(st, 'approve', host))
  root.querySelector('[data-reject]')?.addEventListener('click', () => complete(st, 'reject', host))
  root.querySelector('[data-return]')?.addEventListener('click', () => doReturn(st, host))
  root.querySelector('[data-return-pick]')?.addEventListener('click', () => openReturnPicker(st, host, root))
  root.querySelector('[data-confirm]')?.addEventListener('click', () => complete(st, 'approve', host))
}

function toast (st, msg) {
  for (const host of Array.from(st.hosts)) {
    const t = hostRoot(host)?.querySelector?.('.tf-toast')
    if (t) { t.textContent = msg; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2600) }
  }
}

async function complete (st, decision, host) {
  if (st.busy) return
  const p = st.props
  // 意见框可能在 content 或 property 任一 host —— 跨所有 host 找。
  let comment = ''
  for (const h of Array.from(st.hosts)) {
    const c = hostRoot(h)?.querySelector?.('[data-comment]')
    if (c && c.value) { comment = c.value; break }
  }
  st.busy = true
  try {
    await apiJson(`/api/flow/tasks/${enc(p.taskId)}/complete`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ instanceId: p.instanceId, decision, comment: comment || null }),
    })
    toast(st, decision === 'approve' ? '已同意办结' : '已驳回')
    // 广播 → 待办中心刷新（门户默认派发 cmx-flow-task-done；组件壳由 CFG.onTaskDone 接管）
    CFG.onTaskDone({ taskId: p.taskId, instanceId: p.instanceId })
    // 尝试关闭本视图
    setTimeout(() => closeSelf(p), 600)
  } catch (e) {
    st.busy = false
    toast(st, '办结失败: ' + e.message)
  }
}

// 退回上一步（P6）：调 reject_task 把任务打回前一审批节点重新办理（区别于「驳回」的决策位）。
// targetBpmnId 省略 = 引擎默认回退直接前驱；传入 = 退回到指定上游节点（③）。
async function doReturn (st, host, targetBpmnId) {
  if (st.busy) return
  const p = st.props
  let comment = ''
  for (const h of Array.from(st.hosts)) {
    const c = hostRoot(h)?.querySelector?.('[data-comment]')
    if (c && c.value) { comment = c.value; break }
  }
  st.busy = true
  try {
    await apiJson(`/api/flow/tasks/${enc(p.taskId)}/reject`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        instanceId: p.instanceId,
        reason: comment || (targetBpmnId ? '退回到指定节点' : '退回上一步'),
        ...(targetBpmnId ? { targetBpmnId } : {}),
      }),
    })
    toast(st, targetBpmnId ? '已退回到指定节点' : '已退回上一步')
    CFG.onTaskDone({ taskId: p.taskId, instanceId: p.instanceId })
    setTimeout(() => closeSelf(p), 600)
  } catch (e) {
    st.busy = false
    toast(st, '退回失败: ' + e.message)
  }
}

// 退回到任意上游节点（③）：拉 reject-targets 渲染可退节点菜单，选一个 → doReturn(target)。
// 再点「退回到…」收起菜单。会签任务/首环节 → 提示不可退。
async function openReturnPicker (st, host, root) {
  const p = st.props
  const menu = root.querySelector?.('[data-return-menu]')
  if (!menu) return
  if (!menu.hidden) { menu.hidden = true; menu.innerHTML = ''; return }
  menu.innerHTML = '<div class="tf-hint">加载可退节点…</div>'
  menu.hidden = false
  try {
    const r = await apiJson(`/api/flow/tasks/${enc(p.taskId)}/reject-targets?instanceId=${enc(p.instanceId)}`)
    const targets = (r && r.targets) || []
    if (!r || !r.rejectable || !targets.length) {
      menu.innerHTML = '<div class="tf-hint">无可退节点（会签任务或已是首环节）</div>'
      return
    }
    menu.innerHTML = targets.map((t) => {
      const star = t.isDirectPredecessor ? '★ ' : ''
      const dist = t.distance ? ` <em>上${t.distance}步</em>` : ''
      return `<button class="tf-btn tf-return-item" data-return-to="${esc(t.bpmnId)}">${star}${esc(t.name || t.bpmnId)}${dist}</button>`
    }).join('')
    menu.querySelectorAll('[data-return-to]').forEach((b) => {
      b.addEventListener('click', () => doReturn(st, host, b.dataset.returnTo))
    })
  } catch (e) {
    menu.innerHTML = `<div class="tf-hint">加载失败: ${esc(e.message)}</div>`
  }
}

// 关闭当前任务视图。nodeId 派生不变（门户工作区 Tab 命名规则），关闭动作委托 CFG.onClose：
// 门户壳默认探测 closeTab/closeWorkspaceNode/postMessage 关工作区 Tab；组件壳派发事件让宿主收面板。
function closeSelf (p) {
  const nodeId = `flow-task-${String(p.instanceId + '-' + p.taskId).replace(/[^A-Za-z0-9_-]+/g, '_')}`
  CFG.onClose(nodeId)
}

// ————————————————————— 样式 —————————————————————

function styleCss () {
  return `
  .tf{--brand:#0969da;--brand-d:#0a4d8c;--brand-soft:#ddf4ff;--ink:#1f2328;--muted:#656d76;--line:#d0d7de;--line-soft:#eaeef2;--ok:#1a7f37;--red:#cf222e;--warn:#bc4c00;
    font:13px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;color:var(--ink);height:100%;box-sizing:border-box;display:flex;flex-direction:column}
  .tf *{box-sizing:border-box}
  .tf-bar{display:flex;align-items:center;gap:10px;height:47px;flex:0 0 auto;padding:0 14px;border-bottom:1px solid var(--line);background:#fff}
  .tf-bar-main{min-width:0} .tf-bar-main b{font-size:14px} .tf-bar-main span{display:block;font-size:11px;color:var(--muted);font-family:ui-monospace,Menlo,monospace}
  .tf-mode{margin-left:auto;font-size:11px;font-weight:700;padding:3px 9px;border-radius:6px;background:#eef3f8;color:var(--brand-d)}
  .tf-mode.approve{background:#eef6ff;color:var(--brand-d)} .tf-mode.edit{background:#fff8ef;color:var(--warn)} .tf-mode.readonly{background:#f2f3f5;color:var(--muted)}
  .tf-body{flex:1;min-height:0;overflow:auto;padding:14px;display:flex;flex-direction:column;gap:12px;background:#f6f8fa}
  .tf-panel{border:1px solid var(--line);border-radius:10px;background:#fff;padding:12px 14px}
  .tf-panel-head{font-size:13px;font-weight:700;color:var(--brand-d);display:flex;align-items:center;gap:6px;margin-bottom:10px;padding-bottom:8px;border-bottom:1px solid var(--line-soft)}
  .tf-panel-head ui5-icon{width:1rem;height:1rem}
  .tf-bizref{display:inline-flex;align-items:center;gap:5px;font-size:11.5px;color:var(--muted);border:1px solid var(--line-soft);border-radius:6px;padding:3px 8px;margin-bottom:10px;font-family:ui-monospace,Menlo,monospace}
  .tf-bizref ui5-icon{width:.85rem;height:.85rem}
  .tf-kvs{display:grid;grid-template-columns:1fr 1fr;gap:8px}
  .tf-kv{border:1px solid var(--line-soft);border-radius:7px;padding:7px 10px;background:#fafbfc}
  .tf-kv span{display:block;font-size:11px;color:var(--muted)} .tf-kv b{font-size:13px}
  .tf-history{max-height:220px;overflow:auto;margin-bottom:12px}
  .tf-cmt{border:1px solid var(--line-soft);border-radius:8px;padding:8px 10px;margin-bottom:7px;background:#fafbfc}
  .tf-cmt-head{display:flex;flex-wrap:wrap;align-items:center;gap:5px 7px;font-size:12px} .tf-cmt-head b{flex:1 1 100%;min-width:0;overflow-wrap:anywhere;color:var(--ink)}
  .tf-dec{flex:0 0 auto;font-size:10px;font-weight:700;padding:1px 6px;border-radius:5px;white-space:nowrap} .tf-dec.ok{color:var(--ok);background:#eaffef;border:1px solid #ace0b9} .tf-dec.rej{color:var(--red);background:#ffebe9;border:1px solid #ff8182}
  .tf-cmt-head em{margin-left:auto;flex:0 0 auto;font-style:normal;font-size:11px;color:var(--muted);white-space:nowrap}
  .tf-cmt-body{font-size:12.5px;margin-top:4px}
  .tf-label{display:block;font-size:11px;font-weight:700;color:var(--muted);text-transform:uppercase;margin-bottom:5px}
  .tf-comment{width:100%;min-height:70px;font:inherit;font-size:13px;border:1px solid var(--line);border-radius:8px;padding:8px 10px;resize:vertical}
  .tf-comment:focus{outline:none;border-color:var(--brand);box-shadow:0 0 0 3px var(--brand-soft)}
  .tf-actions{display:flex;gap:9px;margin-top:12px;flex-wrap:wrap}
  .tf-return-menu{display:flex;flex-direction:column;gap:6px;margin-top:8px;padding:8px;border:1px solid var(--line-soft);border-radius:8px;background:#fafbfc}
  .tf-return-item{text-align:left;font-weight:600}
  .tf-return-item em{color:var(--muted);font-style:normal;font-size:11px}
  .tf-btn{font:inherit;font-size:13px;border:1px solid var(--line);background:#fff;color:var(--ink);border-radius:8px;padding:8px 18px;cursor:pointer;font-weight:700}
  .tf-btn.ok{background:var(--ok);border-color:var(--ok);color:#fff} .tf-btn.danger{background:#fff;border-color:#e6b3ae;color:var(--red)} .tf-btn.danger:hover{background:#fdeceb}
  .tf-prop-head{height:47px;flex:0 0 auto;display:flex;flex-direction:column;justify-content:center;padding:0 14px;border-bottom:1px solid var(--line-soft)}
  .tf-prop-head b{font-size:14px} .tf-prop-head small{display:block;font-size:11px;color:var(--muted)}
  .tf-prop-body{padding:12px 14px;overflow:auto}
  .tf-psec{font-size:11px;font-weight:800;color:var(--brand-d);text-transform:uppercase;margin:14px 0 8px;padding-bottom:5px;border-bottom:1px solid var(--line-soft)}
  .tf-psec:first-child{margin-top:0}
  .tf-trail{display:flex;align-items:center;gap:8px;padding:5px 0;font-size:12.5px;color:var(--muted)}
  .tf-trail.cur{color:var(--brand);font-weight:700} .tf-trail.done{color:var(--ok)}
  .tf-dot{width:9px;height:9px;border-radius:50%;background:currentColor;flex:0 0 auto} .tf-trail em{margin-left:auto;font-style:normal;font-size:11px}
  .tf-hint{font-size:12px;color:var(--muted);padding:6px 0}
  .tf-empty{padding:44px;text-align:center;color:var(--muted)}
  .tf-toast{position:absolute;left:50%;bottom:18px;transform:translateX(-50%);background:#0d1117;color:#fff;padding:9px 16px;border-radius:9px;font-size:12.5px;font-weight:600;opacity:0;pointer-events:none;transition:opacity .2s;z-index:20}
  .tf-toast.show{opacity:1}
  `
}

// 门户壳 export default（CFG 默认值=今天）；S5 组件壳 import { configure, mount } 覆盖后自挂。
export { configure, mount }
export default {
  defaultView: 'content',
  views: {
    async content (ctx) { return mount(ctx, 'content') },
    async property (ctx) { return mount(ctx, 'property') },
  },
}
