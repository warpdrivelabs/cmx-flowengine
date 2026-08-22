// Next ②/④ 前端可视化 · CDP 真机测试（决策表查看器 + 运维台令牌可视化/变量历史）。
//
// 决策表查看器（②）：
//   D0 列表加载（≥1 张已注册决策表）
//   D1 选中 e2e_matrix → content 渲染网格（表头含输入 amount / 输出 approvalLevel，≥2 行规则）
//   D2 property 试算 {amount:500000} → 命中规则高亮（.hit）+ 输出 approvalLevel=3
// 运维台（④）：
//   O0 实例列表加载
//   O1 选中一个实例 → content 渲染 + 图例(.ops-legend)出现
//   O2 property 变量历史时间线渲染（≥1 条），且含引擎派生来源徽标（decision/subflow → .ops-vh-src.derived）
//   Z  全程无 pageerror
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9094
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

// 页面壳：三区各挂一个 shadow host，直接 import native page ESM 并 mount。
const harnessFor = (mod) => `<!doctype html><html><head><meta charset="utf-8"><title>viz</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh}.region{overflow:hidden;height:100%}#r-explorer{flex:0 0 260px}#r-content{flex:1}#r-property{flex:0 0 340px}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/${mod}.js'
  window.__mod = mod
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  mod.default.views.property({ host: mk('h-property') })
  window.__ready = true
</script></body></html>`

const mkHelpers = (page) => ({
  count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
  has: (sel) => page.evaluate(({ de, sel }) => !!eval(de).find((e) => e.matches && e.matches(sel)), { de: deepEval, sel }),
  text: (sel) => page.evaluate(({ de, sel }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); return e ? (e.textContent || '').trim() : null }, { de: deepEval, sel }),
  allText: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).map((e) => (e.textContent || '').trim()), { de: deepEval, sel }),
  click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
  clickData: (attr, val) => page.evaluate(({ de, attr, val }) => { const b = eval(de).find((e) => e.getAttribute && e.getAttribute(attr) === val); if (b) { b.click(); return true } return false }, { de: deepEval, attr, val }),
  setVal: (sel, val) => page.evaluate(({ de, sel, val }) => { const inp = eval(de).find((e) => e.matches && e.matches(sel)); if (!inp) return false; inp.value = val; inp.dispatchEvent(new Event('input', { bubbles: true })); return true }, { de: deepEval, sel, val }),
})

async function mountPage (browser, mod) {
  const ctx = await browser.newContext({ viewport: { width: 1320, height: 900 } })
  const page = await ctx.newPage()
  const errs = []
  page.on('pageerror', (e) => errs.push(e.message))
  page.on('console', (m) => { if (m.type() === 'error') errs.push('c:' + m.text()) })
  await vendorRoute(page, ctx)
  const file = path.join(WEB_DIR, `__viz_${mod}.html`)
  fs.writeFileSync(file, harnessFor(mod))
  await page.goto(`http://127.0.0.1:${PORT}/__viz_${mod}.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForTimeout(700)
  return { ctx, page, h: mkHelpers(page), errs, file }
}

;(async () => {
  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const cleanup = []

  // ─────────── ② 决策表查看器 ───────────
  const DV = await mountPage(browser, 'decision-viewer')
  cleanup.push(DV.file)
  await DV.page.waitForTimeout(1200)  // loadList

  const dvRows = await DV.h.count('.dcv-row')
  A('D0-list-load', dvRows >= 1, '决策表列表加载（≥1 张）', `rows=${dvRows}`)

  // 选中 e2e_matrix（种子已注册）。
  const picked = await DV.h.clickData('data-key', 'e2e_matrix')
  await DV.page.waitForTimeout(1000)
  const hasGrid = await DV.h.has('.dcv-grid')
  const headTexts = await DV.h.allText('.dcv-grid thead th')
  const ruleRows = await DV.h.count('.dcv-grid tbody tr')
  const headOk = headTexts.some((t) => t.includes('amount')) && headTexts.some((t) => t.includes('approvalLevel'))
  A('D1-grid-render', picked && hasGrid && headOk && ruleRows >= 2, '选中 e2e_matrix → 网格渲染（输入 amount / 输出 approvalLevel，≥2 规则）', `head=[${headTexts.join(',')}] rows=${ruleRows}`)

  // 试算 {amount:500000} → 命中 + approvalLevel=3。
  await DV.h.setVal('textarea[data-facts]', '{"amount": 500000}')
  await DV.page.waitForTimeout(200)
  await DV.h.click('[data-eval]')
  await DV.page.waitForTimeout(1200)
  const hitRows = await DV.h.count('.dcv-grid tbody tr.hit')
  const outText = (await DV.h.allText('.dcv-vtab td')).join(' ')
  A('D2-simulate', hitRows >= 1 && /approvalLevel/.test(outText) && /\b3\b/.test(outText), '试算 amount=500000 → 命中高亮 + 输出 approvalLevel=3', `hitRows=${hitRows} out="${outText}"`)

  // ─────────── ④ 运维台：令牌可视化 + 变量历史 ───────────
  const OC = await mountPage(browser, 'ops-console')
  cleanup.push(OC.file)
  await OC.page.waitForTimeout(1400)  // loadList

  const ocRows = await OC.h.count('.ops-row')
  A('O0-inst-list', ocRows >= 1, '运维台实例列表加载', `rows=${ocRows}`)

  // 选中含引擎派生变量历史的实例：优先 e2e_parent（subflow 回填）/ e2e_rule_flow（decision 输出）。
  const targetId = await OC.page.evaluate((de) => {
    const rows = eval(de).filter((e) => e.classList && e.classList.contains('ops-row') && e.getAttribute('data-id'))
    const pref = rows.find((r) => /e2e_parent|e2e_rule_flow|E2E父|E2E决策/i.test(r.textContent || ''))
    return (pref || rows[0]) ? (pref || rows[0]).getAttribute('data-id') : null
  }, deepEval)
  if (targetId) await OC.h.clickData('data-id', targetId)
  await OC.page.waitForTimeout(1800)  // detail + var-history load (async)

  const hasLegend = await OC.h.has('.ops-legend')
  const hasDiagramSec = await OC.h.has('.ops-diagram')
  A('O1-token-viz', hasDiagramSec && hasLegend, '选中实例 → 令牌图 + 状态图例渲染', `diagram=${hasDiagramSec} legend=${hasLegend}`)

  // 变量历史时间线：≥1 条，且理想含 derived 徽标（decision/subflow）。
  const vhCount = await OC.h.count('.ops-vh')
  const hasDerived = await OC.h.has('.ops-vh-src.derived')
  A('O2-var-history', vhCount >= 1, '变量历史时间线渲染（≥1 条）', `entries=${vhCount} derivedBadge=${hasDerived}`)
  A('O2b-derived-badge', hasDerived, '变量历史含引擎派生来源徽标（decision/subflow）', `derived=${hasDerived}`)

  const errs = [...DV.errs, ...OC.errs].filter((e) => !/favicon|registry\/dam|Failed to load resource|bpmn-js 加载失败|net::ERR/i.test(e))
  A('Z-noerr', errs.length === 0, '全程无 pageerror', errs.slice(0, 3).join(' | ').slice(0, 240))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  for (const f of cleanup) { try { fs.unlinkSync(f) } catch {} }
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== Next②④ 前端可视化: ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'viz-decision-varhistory-results.json'), JSON.stringify(results, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
