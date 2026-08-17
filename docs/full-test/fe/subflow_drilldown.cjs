// 子流程「四区钻入式」设计器 · 门户级多区真机测试（复现并防回归「浮层覆盖全页无法返回」失控）
//
// 关键保真：不再用 demo 那种「三区同一个 <flow-designer> 单 shadow root」蒙混——那正是上一轮
// position:fixed 全屏浮层「看起来正常」的假象来源。这里按真实门户结构：explorer / content / property
// 挂成 3 个【各自独立 shadow root】的 region 宿主，每个 region 祖先带 overflow:hidden + transform
// （transform 造 containing block → 任何 position:fixed 都会被裁进本区，正是门户里把浮层困在 320px
// 窄 property 区、关闭按钮够不到的机制）。若有人再引入 fixed 浮层，本测 T1/T2 必红。
//
// 断言：进子流程 无 position:fixed 遮罩 / content 工具栏「← 返回主流程」可见可点可返回 / 面包屑 /
//       content 画布非零尺寸且渲出子流程图元 / explorer 列组织变体+返回 / 切变体 / 新建变体空模板 /
//       property「数据模型」页签可见可编辑 / 返回后主图元还原。
const { chromium } = require('playwright')
const { spawn } = require('child_process')
const path = require('path')
const fs = require('fs')

const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6'
const API = 'http://127.0.0.1:8091/api/flow/v1'
const WEB_DIR = path.resolve(__dirname, '../../../web')
const SHOTS = path.join(__dirname, 'shots')
const PORT = 9097   // 独立端口，避免与其它跑着的静态服冲突
const results = []
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

// 深度穿透所有 shadow root 收集全部元素（3 个独立 region shadow 各自被 w() 递归进入）。
const deepEval = `(() => { const r=[]; const w=root=>{ root.querySelectorAll('*').forEach(e=>{ r.push(e); if(e.shadowRoot) w(e.shadowRoot); }); }; w(document); return r; })()`

async function apiGet (p) { const r = await fetch(API + p, { headers: { 'X-API-Key': KEY } }); return r.json() }

// 门户级多区宿主页：3 个独立 shadow host + overflow:hidden + transform 的 region 祖先。
const HARNESS_HTML = `<!doctype html><html><head><meta charset="utf-8"><title>flow multiregion harness</title>
<style>
  html,body{margin:0;height:100%}
  #stage{display:flex;height:100vh;width:100vw;overflow:hidden}
  /* 每个 region：独立盒子 + overflow:hidden + transform(造 containing block，复刻门户对 fixed 的裁剪) */
  .region{box-sizing:border-box;overflow:hidden;transform:translateZ(0);position:relative;height:100%}
  #r-explorer{flex:0 0 240px;border-right:1px solid #e2e6ec}
  #r-content{flex:1 1 auto;min-width:0}
  #r-property{flex:0 0 320px;border-left:1px solid #e2e6ec}
  .host{height:100%;display:block}
</style></head>
<body>
<div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/design-workbench.js'
  window.__mod = mod
  mod.configure({
    apiBase: 'http://127.0.0.1:8091',
    fetchInit: { credentials: 'omit' },
    authHeaders: () => ({ 'X-API-Key': ${JSON.stringify(KEY)} }),
    bpmnBase: '/portal/vendor/bpmn-js',
  })
  // 造门户 <cmx-native-pages-host> 契约：host.shadowRoot 内一个 .native-page-root（hostRoot 取它）。
  function makeHost (id) {
    const el = document.getElementById(id)
    const sr = el.attachShadow({ mode: 'open' })
    const root = document.createElement('div')
    root.className = 'native-page-root'
    root.style.height = '100%'
    sr.appendChild(root)
    return el   // 不设 renderRoot → 走 shadowRoot.querySelector('.native-page-root') 分支
  }
  const hc = makeHost('h-content')
  const he = makeHost('h-explorer')
  const hp = makeHost('h-property')
  mod.default.views.content({ host: hc })
  mod.default.views.explorer({ host: he })
  mod.default.views.property({ host: hp })
  window.__ready = true
</script>
</body></html>`

function startStatic () {
  const srv = spawn('python3', ['-m', 'http.server', String(PORT)], { cwd: WEB_DIR, stdio: 'ignore' })
  return srv
}

;(async () => {
  if (!fs.existsSync(SHOTS)) fs.mkdirSync(SHOTS, { recursive: true })
  const srv = startStatic()
  await new Promise((r) => setTimeout(r, 900))
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1680, height: 1000 } })
  const page = await ctx.newPage()
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  page.on('console', (m) => { if (m.type() === 'error') errors.push('console:' + m.text()) })
  // 记录非 200 响应 URL（favicon 等浏览器自发请求属噪声，单独滤除）。
  const badResp = []
  page.on('response', (r) => { if (r.status() >= 400) badResp.push(`${r.status()} ${r.url()}`) })
  // 任何 confirm（切变体/返回时若有未保存修改）一律接受——测试走「丢弃」路径，不被弹窗拦截。
  page.on('dialog', (d) => d.accept())

  // 静态资源：核模块/资产走本地 :PORT；bpmn vendor 转门户 :8080。
  await page.route('**/portal/vendor/**', async (route) => {
    const url = route.request().url(); const rel = url.substring(url.indexOf('/portal/vendor/'))
    try { const r = await ctx.request.get('http://127.0.0.1:8080' + rel); await route.fulfill({ status: r.status(), body: await r.body(), headers: { 'content-type': r.headers()['content-type'] || 'application/octet-stream' } }) } catch { await route.abort() }
  })
  // 多区宿主页写成 web/ 下真实文件由静态服提供（loopback 文档 → 允许 fetch :8091；route.fulfill 的
  // 合成文档会被 Chrome PNA 判为非 loopback 地址空间而拦截跨端口请求）。测后删除。
  const harnessFile = path.join(WEB_DIR, '__mr_harness.html')
  fs.writeFileSync(harnessFile, HARNESS_HTML)

  // ── 深度穿透 helper ──
  const findOne = (sel) => page.evaluate(({ de, sel }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); return !!e }, { de: deepEval, sel })
  const clickSel = (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel })
  const countSel = (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel })
  const textSel = (sel) => page.evaluate(({ de, sel }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); return e ? (e.textContent || '').trim() : null }, { de: deepEval, sel })
  const subNav = () => page.evaluate('window.__mod && window.__mod.__state ? 1 : 0').catch(() => 0)
  // content 画布尺寸（bpmn 容器 = 模块 [data-flow-canvas] 宿主 div）。
  const canvasSize = () => page.evaluate((de) => { const c = eval(de).find((e) => e.matches && e.matches('[data-flow-canvas]')); if (!c) return { w: 0, h: 0 }; const r = c.getBoundingClientRect(); return { w: Math.round(r.width), h: Math.round(r.height) } }, deepEval)
  // 主/子画布上的 bpmn 图元 id（content 区）。
  const shapes = () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval)
  // 覆盖大半视口的 position:fixed 元素（复现「浮层覆盖整个页面」失控的精确特征）+ 旧遮罩类名。
  // bpmn-js 内部偶有小尺寸 fixed 元素（工具提示等），按面积阈值(>50% 视口)排除，只揪真·全屏遮罩。
  const bigFixedOverlays = () => page.evaluate((de) => {
    const VW = window.innerWidth, VH = window.innerHeight
    return eval(de).filter((e) => {
      try {
        const cs = getComputedStyle(e)
        if (cs.position !== 'fixed') return false
        const r = e.getBoundingClientRect()
        return (r.width * r.height) > (VW * VH * 0.5)
      } catch { return false }
    }).map((e) => `${e.tagName}.${('' + (e.className && e.className.baseVal !== undefined ? e.className.baseVal : e.className)) || ''}`)
  }, deepEval)
  const maskEls = () => page.evaluate((de) => eval(de).filter((e) => e.classList && (e.classList.contains('flow-sub-mask') || e.classList.contains('flow-sub-editor'))).length, deepEval)
  // 「返回主流程」按钮的可视矩形 + 是否落在视口内（未被裁到 0 / 不在屏外）。
  const backBtnRect = () => page.evaluate((de) => { const b = eval(de).find((e) => e.matches && e.matches('[data-act="back-to-main"]')); if (!b) return null; const r = b.getBoundingClientRect(); return { w: Math.round(r.width), h: Math.round(r.height), top: Math.round(r.top), left: Math.round(r.left), right: Math.round(r.right), bottom: Math.round(r.bottom) } }, deepEval)
  const vw = 1680; const vh = 1000
  const loadDef = async (key) => { await page.evaluate(({ de, k }) => { const el = eval(de).find((e) => e.classList && e.classList.contains('flow-def') && e.dataset.key === k); if (el) el.click() }, { de: deepEval, k: key }); await page.waitForTimeout(2600) }
  const selMainNode = async (id) => {
    await page.evaluate(({ de, id }) => { const o = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === (id === 'mgr' ? 'director' : 'mgr')); if (o) o.dispatchEvent(new MouseEvent('click', { bubbles: true })) }, { de: deepEval, id })
    await page.waitForTimeout(300)
    return page.evaluate(({ de, id }) => { const s = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id); if (s) { s.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true } return false }, { de: deepEval, id })
  }

  await page.goto(`http://127.0.0.1:${PORT}/__mr_harness.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(600)

  // ═══ T0: 门户级 3 独立 shadow root + 窄 property 区 ═══
  const t0 = await page.evaluate(() => {
    const ids = ['h-explorer', 'h-content', 'h-property']
    const roots = ids.map((i) => document.getElementById(i).shadowRoot).filter(Boolean)
    const distinct = new Set(roots).size
    const pr = document.getElementById('r-property').getBoundingClientRect()
    return { distinct, propW: Math.round(pr.width) }
  })
  A('T0-multiregion', '★门户级 3 个独立 shadow root（非 demo 单组件）+ property 窄区 ~320', t0.distinct === 3 && t0.propW >= 280 && t0.propW <= 360, `roots=${t0.distinct} propW=${t0.propW}`)

  // ═══ 载入主流程 travel_expense，选中 org 路由 callActivity fin_review ═══
  await loadDef('travel_expense')
  const mainShapesBefore = await shapes()
  A('T-load-main', '主流程载入且有图元', mainShapesBefore.includes('fin_review'), `shapes=${mainShapesBefore.length}`)
  const selOk = await selMainNode('fin_review')
  await page.waitForTimeout(600)
  A('T-sel-callactivity', '选中 callActivity fin_review', selOk, '')
  const hasEditBtn = await findOne('[data-edit-subflow]')
  A('T-edit-btn', 'property 出现「编辑子流程」按钮', hasEditBtn, '')

  // ═══ T1/T2/T3/T4: 钻入子流程（点「编辑子流程」）═══
  await clickSel('[data-edit-subflow]')
  await page.waitForTimeout(3200)
  await page.screenshot({ path: path.join(SHOTS, 'dd-01-drilled-in.png'), fullPage: true })

  const fixedAfter = await bigFixedOverlays()
  const maskAfter = await maskEls()
  A('T1-no-fixed-overlay', '★进子流程后无覆盖全屏的 position:fixed 遮罩（失控根因已除）', fixedAfter.length === 0 && maskAfter === 0, `bigFixed=${JSON.stringify(fixedAfter)} mask=${maskAfter}`)

  const rect = await backBtnRect()
  const reachable = rect && rect.w > 0 && rect.h > 0 && rect.top >= 0 && rect.left >= 0 && rect.right <= vw && rect.bottom <= vh
  A('T2-back-reachable', '★「← 返回主流程」在 content 工具栏内可见且落在视口内（未被裁/未失控）', reachable, JSON.stringify(rect))

  const crumb = await textSel('.flow-crumb')
  const mainName = ((await apiGet('/definitions/travel_expense')).data || {}).name || '差旅报销申请'
  A('T3-breadcrumb', '面包屑显示 主流程名 › 子流程名', !!crumb && crumb.includes(mainName) && crumb.includes('›'), (crumb || 'none').replace(/\s+/g, ' '))

  const cs = await canvasSize()
  const subShapes = await shapes()
  A('T4-canvas-sized', '★content 画布非零尺寸（钻入复用同一画布，非 0×0 被裁）', cs.w > 200 && cs.h > 150, `${cs.w}×${cs.h}`)
  A('T4-sub-rendered', '子流程图元已渲染（≥3）', subShapes.length >= 3, `shapes=${subShapes.length}`)

  // ═══ Phase 2 · 功能持久化（钻入工具栏 save/publish 路由到 subSave/subPublish）═══
  // P1: 改子流程名 → 工具栏「保存」→ 画布仍在(旧 Bug A 回归防线) + 名称落库 + 无遮罩 → 复原。
  const hqName0 = ((await apiGet('/definitions/fin_review_hq')).data || {}).name || '总部财务三级复核'
  await page.evaluate((de) => { const i = eval(de).find((e) => e.matches && e.matches('[data-name]')); if (i) { i.value = '复核DD-test'; i.dispatchEvent(new Event('change', { bubbles: true })) } }, deepEval)
  await page.waitForTimeout(300)
  await clickSel('[data-act="save"]')
  await page.waitForTimeout(2600)
  const afterSaveShapes = await shapes()
  const afterSaveFixed = await bigFixedOverlays()
  const hqName1 = ((await apiGet('/definitions/fin_review_hq')).data || {}).name || ''
  A('P1-save-canvas-alive', '★保存子流程后画布存活(≥3图元)+无遮罩(旧Bug A:保存毁画布 已防)', afterSaveShapes.length >= 3 && afterSaveFixed.length === 0, `shapes=${afterSaveShapes.length} fixed=${afterSaveFixed.length}`)
  A('P1-save-persist', '★工具栏「保存」→ subSave 真落库（名称往返）', hqName1 === '复核DD-test', `name="${hqName1}"`)
  // 复原名称（保持数据整洁，只增不乱）。
  await page.evaluate(({ de, n }) => { const i = eval(de).find((e) => e.matches && e.matches('[data-name]')); if (i) { i.value = n; i.dispatchEvent(new Event('change', { bubbles: true })) } }, { de: deepEval, n: hqName0 })
  await page.waitForTimeout(200)
  await clickSel('[data-act="save"]'); await page.waitForTimeout(2200)

  // P2: 工具栏「发布」→ subPublish → 版本 +1；画布仍存活。
  const hqVers0 = (((await apiGet('/definitions/fin_review_hq')).data || {}).versions || []).length
  await clickSel('[data-act="publish"]')
  await page.waitForTimeout(2800)
  const hqVers1 = (((await apiGet('/definitions/fin_review_hq')).data || {}).versions || []).length
  const afterPubShapes = await shapes()
  A('P2-publish-version', '★工具栏「发布」→ subPublish → 版本号 +1', hqVers1 === hqVers0 + 1, `${hqVers0}→${hqVers1}`)
  A('P2-publish-canvas-alive', '发布后画布存活', afterPubShapes.length >= 3, `shapes=${afterPubShapes.length}`)

  // ═══ T5: explorer 区列组织变体 + 返回按钮 ═══
  const varCount = await countSel('[data-sub-variant]')
  const hasBackInExplorer = await findOne('[data-sub-back]')
  const hasNewVarSel = await findOne('[data-sub-newvar-org]')
  A('T5-explorer-variants', 'explorer 列出组织变体（3）+ 返回 + 新增变体下拉', varCount === 3 && hasBackInExplorer && hasNewVarSel, `variants=${varCount} back=${hasBackInExplorer} newsel=${hasNewVarSel}`)

  // ═══ T6: 切换变体（点非当前变体 fin_review_branch）→ 画布仍有图元 ═══
  const switched = await page.evaluate((de) => { const bs = eval(de).filter((e) => e.matches && e.matches('[data-sub-variant]')); const t = bs.find((b) => (b.dataset.subVariant || '') === 'fin_review_branch'); if (t) { t.click(); return true } return false }, deepEval)
  await page.waitForTimeout(2600)
  const afterSwitchShapes = await shapes()
  const activeTgt = await page.evaluate((de) => { const on = eval(de).find((e) => e.matches && e.matches('[data-sub-variant].on')); return on ? on.dataset.subVariant : null }, deepEval)
  A('T6-variant-switch', '切换到 fin_review_branch 变体：画布重载有图元 + 高亮切换', switched && afterSwitchShapes.length >= 3 && activeTgt === 'fin_review_branch', `active=${activeTgt} shapes=${afterSwitchShapes.length}`)

  // ═══ T7: 新建组织变体（选一个未绑定组织）→ 空模板 ═══
  const newVarPick = await page.evaluate((de) => { const sel = eval(de).find((e) => e.matches && e.matches('[data-sub-newvar-org]')); if (!sel) return { ok: false }; const opt = [...sel.options].find((o) => o.value && o.value !== '__default__' && o.value !== ''); if (!opt) return { ok: false, opts: [...sel.options].map((o) => o.value) }; sel.value = opt.value; sel.dispatchEvent(new Event('change', { bubbles: true })); return { ok: true, org: opt.value } }, deepEval)
  await page.waitForTimeout(2400)
  const newTemplShapes = await shapes()
  // 空模板应远少于已加载变体（13/9 图元）；且面包屑子名变「新建」。
  const crumbNew = await textSel('.flow-crumb-sub')
  A('T7-new-variant', '为未绑定组织新建变体：载入空模板（图元远少于变体）+ 面包屑标「新建」', newVarPick.ok && newTemplShapes.length <= 6 && /新建/.test(crumbNew || ''), `org=${newVarPick.org} shapes=${newTemplShapes.length} crumbSub=${crumbNew}`)
  await page.screenshot({ path: path.join(SHOTS, 'dd-02-new-variant.png'), fullPage: true })

  // ═══ P3: 新变体命名+保存 → 唯一 key + 自动绑定 + isSubflow=true + 重存不重复（工具栏 save→subSave）═══
  const pickedOrg = newVarPick.org
  const defsBefore = ((await apiGet('/design/definitions')).data.definitions || []).map((d) => d.key)
  await page.evaluate((de) => { const i = eval(de).find((e) => e.matches && e.matches('[data-name]')); if (i) { i.value = '钻入新建变体DD'; i.dispatchEvent(new Event('input', { bubbles: true })); i.dispatchEvent(new Event('change', { bubbles: true })) } }, deepEval)
  await page.waitForTimeout(300)
  await clickSel('[data-act="save"]'); await page.waitForTimeout(3000)
  const defsAfter1 = ((await apiGet('/design/definitions')).data.definitions || [])
  const newKeys = defsAfter1.map((d) => d.key).filter((k) => !defsBefore.includes(k))
  const gzBind = ((await apiGet('/subflow-bindings/fin_review')).data.bindings || []).find((b) => (b.orgId || '') === pickedOrg)
  const gzKey = gzBind ? gzBind.targetKey : null
  A('P3-newvar-unique-key', '★新变体唯一 key（非 new_process）', !!gzKey && gzKey !== 'new_process', `key=${gzKey} newDefs=${JSON.stringify(newKeys)}`)
  A('P3-newvar-autobind', '★保存即自动绑定到该组织', !!gzBind, `org=${pickedOrg}→${gzKey}`)
  const gzDef = defsAfter1.find((d) => d.key === gzKey)
  A('P3-newvar-isSubflow', '★新子流程 isSubflow=true（主列表隐藏）', !!gzDef && gzDef.isSubflow === true, `isSubflow=${gzDef ? gzDef.isSubflow : 'not found'}`)
  await clickSel('[data-act="save"]'); await page.waitForTimeout(2800)
  const defsAfter2 = ((await apiGet('/design/definitions')).data.definitions || [])
  A('P3-resave-nodup', '★重存同变体不重复建定义', defsAfter2.length === defsAfter1.length, `${defsAfter1.length}→${defsAfter2.length}`)
  // 清理本测新建绑定（定义留库无害，isSubflow 已从主列表隐藏），恢复 fin_review 绑定基线。
  if (gzBind) { try { await fetch(API + '/subflow-bindings/id/' + gzBind.id, { method: 'DELETE', headers: { 'X-API-Key': KEY } }) } catch {} }

  // ═══ T8: property「数据模型」页签可见可编辑 ═══
  const hasModelTab = await findOne('[data-ptab="model"]')
  await clickSel('[data-ptab="model"]')
  await page.waitForTimeout(500)
  const modelVisible = await findOne('[data-var-add]')
  await clickSel('[data-var-add]')
  await page.waitForTimeout(500)
  const varRows = await countSel('[data-var-name]')
  A('T8-datamodel-tab', '★property「数据模型」页签可见 + 新增变量生效（内联编辑器，非遮罩）', hasModelTab && modelVisible && varRows >= 1, `tab=${hasModelTab} editor=${modelVisible} rows=${varRows}`)
  // 切回节点属性
  await clickSel('[data-ptab="node"]')
  await page.waitForTimeout(300)

  // ═══ T9: 返回主流程（点 content 工具栏返回按钮）→ 主图元还原、subNav 清空、无遮罩 ═══
  // 先切回一个已存变体避免「未保存」确认弹窗拦截（新模板 dirty=false，切变体会丢弃；这里直接返回）。
  page.on('dialog', (d) => d.accept())   // 兜底：任何 confirm 一律接受（丢弃未保存返回）
  await clickSel('[data-act="back-to-main"]')
  await page.waitForTimeout(3000)
  const backShapes = await shapes()
  const fixedFinal = await bigFixedOverlays()
  const stillCrumb = await findOne('.flow-crumb')
  A('T9-back-to-main', '★点返回：主流程图元还原（fin_review 在）+ 面包屑消失 + 无遗留全屏遮罩', backShapes.includes('fin_review') && !stillCrumb && fixedFinal.length === 0, `mainBack=${backShapes.includes('fin_review')} crumbGone=${!stillCrumb} bigFixed=${fixedFinal.length}`)
  await page.screenshot({ path: path.join(SHOTS, 'dd-03-back-to-main.png'), fullPage: true })

  // 噪声滤除：①favicon（浏览器自发）②/api/registry/dam（平台注册表，独立 flow-server 不提供，
  // 模块 loadDam() 显式 catch 优雅降级为「不显示 DAM 过滤项」，真实门户 :8080 才有——非 bug）。
  const noise = (s) => /favicon/i.test(s) || /\/api\/registry\/dam/i.test(s) || /Failed to load resource/i.test(s)
  const realErrors = errors.filter((e) => !noise(e))
  const realBad = badResp.filter((b) => !noise(b))
  A('T-noerr', '全程无 pageerror / 真实 4xx（favicon + 可选 DAM 注册表噪声已滤）', realErrors.length === 0 && realBad.length === 0, [...realErrors.slice(0, 3), ...realBad.slice(0, 3)].join(' | ').slice(0, 300))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(harnessFile) } catch {}
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 子流程钻入式(门户级多区): ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'subflow-drilldown-results.json'), JSON.stringify(results, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); try { fs.unlinkSync(path.join(WEB_DIR, '__mr_harness.html')) } catch {} process.exit(2) })
