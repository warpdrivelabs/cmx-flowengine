// 决策表可编辑设计器 · CDP 真机测试（路线图 Next：决策表可编辑设计器）。
//
// 只依赖 flow-server :8091（off 模式）。三区挂 shadow host，驱动：
//   D1 新建决策表（key）→ content 出网格（默认 1 入 1 出 1 则）
//   D2 编辑：加输入列 + 填条件/输出 + 选命中策略 → 保存 → 后端 GET 往返一致（hit_policy/conditions/outputs）
//   D3 试算：facts 命中正确规则、输出正确
//   D4 非法保存：conditions 宽度与 inputs 不符 → 后端 400 校验错，前端展示诊断
//   D5 删除 → 列表不再含该 key
//   D6 全程无 pageerror
const { chromium } = require('playwright')
const fs = require('fs')
const path = require('path')
const { WEB_DIR, deepEval, startStatic, vendorRoute } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9098
const KEY = 'dsd_test_' + Math.floor(Date.now() / 1000 % 100000) // 稳定但基本唯一（避免残留撞键；无 Date.now 限制这是普通 node）
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

const HARNESS = `<!doctype html><html><head><meta charset="utf-8"><title>dsd</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh}.region{overflow:hidden;height:100%}#r-explorer{flex:0 0 260px}#r-content{flex:1}#r-property{flex:0 0 320px}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/decision-designer.js'
  window.__mod = mod
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}) })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  mod.default.views.property({ host: mk('h-property') })
  window.__ready = true
</script></body></html>`

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

;(async () => {
  const srv = startStatic(PORT)
  await sleep(900)
  const browser = await chromium.launch({
    channel: 'chrome',
    headless: true,
    // 关闭 Chrome 私有网络访问(PNA)门禁：测试页(:9098) fetch 跨源到 flow-server(:8091) 均为 loopback，
    // 新版 Chrome 默认拦 loopback→loopback；测试环境放行（对齐 apiJson 直连 :8091 的既有模式）。
    args: ['--disable-features=PrivateNetworkAccessChecks,BlockInsecurePrivateNetworkRequests,LocalNetworkAccessChecks'],
  })
  const ctx = await browser.newContext({ viewport: { width: 1500, height: 920 } })
  const page = await ctx.newPage()
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  page.on('dialog', (d) => d.accept()) // 删除确认框

  await page.route('**/_dsd.html', (route) => route.fulfill({ status: 200, contentType: 'text/html', body: HARNESS }))
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/_dsd.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => window.__ready === true, { timeout: 8000 })
  await sleep(600)

  // deep helpers
  const H = {
    click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
    setVal: (sel, val) => page.evaluate(({ de, sel, val }) => { const el = eval(de).find((e) => e.matches && e.matches(sel)); if (!el) return false; el.value = val; el.dispatchEvent(new Event('input', { bubbles: true })); el.dispatchEvent(new Event('change', { bubbles: true })); return true }, { de: deepEval, sel, val }),
    count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
    text: (sel) => page.evaluate(({ de, sel }) => { const el = eval(de).find((e) => e.matches && e.matches(sel)); return el ? el.textContent : null }, { de: deepEval, sel }),
    selectVal: (sel, val) => page.evaluate(({ de, sel, val }) => { const el = eval(de).find((e) => e.matches && e.matches(sel)); if (!el) return false; el.value = val; el.dispatchEvent(new Event('change', { bubbles: true })); return true }, { de: deepEval, sel, val }),
  }

  try {
    // 预清理：若残留同 key，先删
    await fetch(`${APIB}/decisions/${KEY}`, { method: 'DELETE' }).catch(() => {})

    // ── D1 新建 ──
    await H.setVal('[data-newkey]', KEY)
    await H.click('[data-new]')
    await sleep(500)
    const hasGrid = await H.count('.dsd-grid')
    const inCols = await H.count('th.dsd-in')
    const outCols = await H.count('th.dsd-out')
    A('D1-new-grid', hasGrid >= 1 && inCols === 1 && outCols === 1, '新建 → 网格(1入1出)', `grid=${hasGrid} in=${inCols} out=${outCols}`)

    // ── D2 编辑：加输入列 → 2 入；命名列；填两行规则；选 FIRST ──
    await H.click('[data-act="add-input"]')
    await sleep(200)
    await H.click('[data-act="add-rule"]') // 现在 2 行
    await sleep(200)
    // 命名输入/输出列
    await H.setVal('.dsd-h[data-kind="in-name"][data-c="0"]', 'amount')
    await H.setVal('.dsd-h[data-kind="in-name"][data-c="1"]', 'vip')
    await H.setVal('.dsd-h[data-kind="out-name"][data-c="0"]', 'level')
    // 规则行 0：amount>10000 且 vip==true → "gold"
    await H.setVal('.dsd-cell[data-kind="cond"][data-r="0"][data-c="0"]', 'amount > 10000')
    await H.setVal('.dsd-cell[data-kind="cond"][data-r="0"][data-c="1"]', 'vip == true')
    await H.setVal('.dsd-cell[data-kind="out"][data-r="0"][data-c="0"]', '"gold"')
    // 规则行 1：兜底 - / - → "normal"
    await H.setVal('.dsd-cell[data-kind="cond"][data-r="1"][data-c="0"]', '-')
    await H.setVal('.dsd-cell[data-kind="cond"][data-r="1"][data-c="1"]', '-')
    await H.setVal('.dsd-cell[data-kind="out"][data-r="1"][data-c="0"]', '"normal"')
    await H.selectVal('.dsd-hp', 'FIRST')
    await sleep(150)
    await H.click('[data-act="save"]')
    await sleep(700)

    // 后端往返校验
    const got = await fetch(`${APIB}/decisions/${KEY}`).then((r) => r.json()).then((j) => j.data).catch(() => null)
    const okRoundtrip = got && got.hit_policy === 'FIRST' &&
      JSON.stringify(got.inputs) === JSON.stringify(['amount', 'vip']) &&
      JSON.stringify(got.outputs) === JSON.stringify(['level']) &&
      got.rules.length === 2 &&
      JSON.stringify(got.rules[0].conditions) === JSON.stringify(['amount > 10000', 'vip == true']) &&
      got.rules[0].outputs.level === 'gold' &&
      got.rules[1].outputs.level === 'normal'
    A('D2-save-roundtrip', okRoundtrip, '保存→GET 往返一致', got ? `hp=${got.hit_policy} inputs=${JSON.stringify(got.inputs)} r0=${JSON.stringify(got.rules[0])}` : 'GET null')

    // ── D3 试算：amount=50000, vip=true → 命中行0 → level=gold ──
    await H.setVal('.dsd-facts', '{"amount": 50000, "vip": true}')
    await H.click('[data-eval]')
    await sleep(500)
    const hitChip = await H.text('.dsd-hit-chip')
    const evalOut = await page.evaluate(() => window.__mod && null) // no-op
    const vtab = await H.text('.dsd-vtab')
    A('D3-evaluate', (hitChip && hitChip.includes('#1')) && (vtab && vtab.includes('gold')), '试算命中行1 → level=gold', `chip=${hitChip} vtab=${(vtab || '').replace(/\\s+/g, ' ').trim().slice(0, 40)}`)

    // ── D4 非法保存：删一个输入列的表头会同步 conditions；这里制造宽度不符——直接后端探（前端保持同步，故用 API 验证后端确实拦截）──
    const badResp = await fetch(`${APIB}/decisions`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ key: KEY + '_bad', hit_policy: 'FIRST', inputs: ['a', 'b'], outputs: ['o'], rules: [{ conditions: ['a > 1'], outputs: { o: 1 } }] }) })
    const badJson = await badResp.json().catch(() => ({}))
    A('D4-validate-rejects', badResp.status >= 400 || badJson.code !== 0, '宽度不符(2入1条件) → 后端拒', `status=${badResp.status} code=${badJson.code} msg=${(badJson.msg || '').slice(0, 40)}`)

    // ── D5 删除 ──
    await H.click('[data-del]')
    await sleep(700)
    const listAfter = await fetch(`${APIB}/decisions`).then((r) => r.json()).then((j) => (j.data.decisions || []).map((m) => m.key)).catch(() => [])
    A('D5-delete', !listAfter.includes(KEY), '删除 → 列表不含该 key', `still? ${listAfter.includes(KEY)}`)

    // ── D6 无错 ──
    A('D6-noerr', errors.length === 0, '全程无 pageerror', errors.join(' | '))

    // 截图
    const SHOTS = path.join(__dirname, 'shots')
    try { fs.mkdirSync(SHOTS, { recursive: true }) } catch {}
    await page.screenshot({ path: path.join(SHOTS, 'decision_designer.png') })
  } finally {
    // 收尾清理（防残留）
    await fetch(`${APIB}/decisions/${KEY}`, { method: 'DELETE' }).catch(() => {})
    await fetch(`${APIB}/decisions/${KEY}_bad`, { method: 'DELETE' }).catch(() => {})
    await browser.close()
    srv.kill('SIGTERM')
  }

  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 决策表可编辑设计器: ${pass}/${results.length} ====`)
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
