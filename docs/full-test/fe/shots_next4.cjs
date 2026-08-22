// Next四项功能确认截图（决策查看器 / 变量历史 / 令牌可视化 / 协同M2）。
// 输出到 docs/full-test/fe/shots/next4/。
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, clickDefPaged } = require('./_harness.cjs')

const PORT = 9096
const SHOTS = path.join(__dirname, 'shots', 'next4')
const APIB = 'http://127.0.0.1:8091/api/flow'

const harnessFor = (mod) => `<!doctype html><html><head><meta charset="utf-8"><title>${mod}</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh;font-family:-apple-system,"PingFang SC",sans-serif}.region{overflow:hidden;height:100%;border-right:1px solid #e1e4e8}#r-explorer{flex:0 0 260px}#r-content{flex:1}#r-property{flex:0 0 340px}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/${mod}.js'
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  mod.default.views.property({ host: mk('h-property') })
  window.__ready = true
</script></body></html>`

const clickData = (page, attr, val) => page.evaluate(({ de, attr, val }) => { const b = eval(de).find((e) => e.getAttribute && e.getAttribute(attr) === val); if (b) { b.click(); return true } return false }, { de: deepEval, attr, val })
const click = (page, sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel })
const setVal = (page, sel, val) => page.evaluate(({ de, sel, val }) => { const inp = eval(de).find((e) => e.matches && e.matches(sel)); if (!inp) return false; inp.value = val; inp.dispatchEvent(new Event('input', { bubbles: true })); return true }, { de: deepEval, sel, val })

async function mountPage (browser, mod) {
  const ctx = await browser.newContext({ viewport: { width: 1360, height: 860 }, deviceScaleFactor: 2 })
  const page = await ctx.newPage()
  await vendorRoute(page, ctx)
  const file = path.join(WEB_DIR, `__shot_${mod}.html`)
  fs.writeFileSync(file, harnessFor(mod))
  await page.goto(`http://127.0.0.1:${PORT}/__shot_${mod}.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForTimeout(900)
  return { ctx, page, file }
}

;(async () => {
  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const files = []

  // ① 决策表查看器：网格 + 试算命中
  const DV = await mountPage(browser, 'decision-viewer'); files.push(DV.file)
  await DV.page.waitForTimeout(1000)
  await clickData(DV.page, 'data-key', 'e2e_matrix')
  await DV.page.waitForTimeout(900)
  await DV.page.screenshot({ path: path.join(SHOTS, 'n4-01-decision-grid.png') })
  await setVal(DV.page, 'textarea[data-facts]', '{"amount": 500000}')
  await click(DV.page, '[data-eval]')
  await DV.page.waitForTimeout(1000)
  await DV.page.screenshot({ path: path.join(SHOTS, 'n4-02-decision-simulate.png') })
  console.log('shot: decision viewer (grid + simulate)')

  // ②③ 运维台：令牌可视化(图例) + 变量历史(引擎派生徽标)
  const OC = await mountPage(browser, 'ops-console'); files.push(OC.file)
  await OC.page.waitForTimeout(1400)
  const targetId = await OC.page.evaluate((de) => {
    const rows = eval(de).filter((e) => e.classList && e.classList.contains('ops-row') && e.getAttribute('data-id'))
    const pref = rows.find((r) => /e2e_parent|e2e_rule_flow/i.test(r.textContent || ''))
    return (pref || rows[0]) ? (pref || rows[0]).getAttribute('data-id') : null
  }, deepEval)
  if (targetId) await clickData(OC.page, 'data-id', targetId)
  await OC.page.waitForTimeout(1800)
  await OC.page.screenshot({ path: path.join(SHOTS, 'n4-03-ops-token-viz.png') })
  console.log('shot: ops-console token viz + legend')
  // property 区（变量历史）单独截：切到 property host 已在同页右列，整页已含；再截右列聚焦。
  await OC.page.screenshot({ path: path.join(SHOTS, 'n4-04-var-history.png'), clip: { x: 1020, y: 0, width: 340, height: 860 } })
  console.log('shot: var-history timeline')

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  for (const f of files) { try { fs.unlinkSync(f) } catch {} }
  console.log('\nAll shots →', SHOTS)
  process.exit(0)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
