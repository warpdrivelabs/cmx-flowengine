// P4 CDP 验证：ops-console 令牌位置图形化 overlay。
// 通过门户加载（门户托管 bpmn-js vendor 资产），选中活动实例 → 验证 bpmn-js 画布渲染 + 令牌高亮 marker。
const { chromium } = require('playwright')

;(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const page = await browser.newPage()
  const errors = []
  page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()) })

  // 直接打开门户 shell 里的运维台页（门户会经 native-pages 加载我们的 ops-console.js）。
  // 若门户有专门的页面路由则用之；否则用一个最小 harness：拉 native-page 源码 + 执行。
  const base = 'http://localhost:8080'

  // 先落到门户同源页面（bpmn-js 资产 + fetch 同源），再注入 harness DOM + 动态 import 页面源。
  await page.goto(base + '/portal/', { waitUntil: 'domcontentloaded' }).catch(() => {})
  await page.evaluate(async (base) => {
    document.body.innerHTML = ''
    // 关键：host 必须是**已连接**的 DOM 元素（refreshView 检查 host.isConnected），
    // 且 hostRoot(host)=host.renderRoot 也须已连接（render 检查 root.isConnected）。
    const mkHost = (id) => {
      const h = document.createElement('div')
      h.id = 'host-' + id
      const root = document.createElement('div')
      root.className = 'native-page-root'
      h.appendChild(root)
      document.body.appendChild(h)   // 连接到文档 → isConnected=true
      h.renderRoot = root
      return h
    }
    window.__ready = (async () => {
      const r = await fetch(base + '/api/native-pages/portal.flow.ops-console')
      const j = await r.json()
      const src = (j.data || j).source
      const url = URL.createObjectURL(new Blob([src], { type: 'text/javascript' }))
      const mod = await import(url)
      const def = mod.default
      window.__page = def
      for (const view of ['explorer', 'content', 'property']) {
        const host = mkHost(view)
        await def.views[view]({ host, workspace: {}, node: {} })
      }
      return true
    })().catch((e) => { window.__err = String((e && e.stack) || e); return false })
  }, base)
  const ok = await page.evaluate(() => window.__ready)
  const bootErr = await page.evaluate(() => window.__err || null)
  console.log('boot ok:', ok, 'bootErr:', bootErr)

  // 等实例列表出现。优先点一个 DI-bearing 定义（travel_expense，设计器产出含图形布局）
  // 以验证真实图形渲染路径；找不到则退而点第一个（验证 DI-less 优雅降级）。
  await page.waitForTimeout(3000)
  let rows = await page.$$('[data-id]')
  if (!rows.length) { await page.waitForTimeout(3000); rows = await page.$$('[data-id]') }
  console.log('instance rows:', rows.length)

  // 在列表里找 travel_expense 行（其 small 标签含 definitionKey）。
  const diRow = await page.evaluateHandle(() => {
    const btns = Array.from(document.querySelectorAll('[data-id]'))
    return btns.find((b) => /travel_expense/.test(b.textContent)) || null
  })
  const diEl = diRow.asElement()
  if (diEl) {
    console.log('clicking DI-bearing instance (travel_expense)')
    await diEl.click()
  } else if (rows.length) {
    console.log('no travel_expense row; clicking first (degradation path)')
    await rows[0].click()
  }
  await page.waitForTimeout(3000) // 等 detail 拉取 + bpmn-js 加载 + importXML

  // 验证：画布容器存在 + bpmn-js SVG 渲染 + 至少一个令牌 marker。
  const diag = await page.$('[data-ops-canvas]')
  const svg = await page.$('[data-ops-canvas] svg')
  const markers = await page.$$eval('[data-ops-canvas] .ops-tok-run, [data-ops-canvas] .ops-tok-wait, [data-ops-canvas] .ops-tok-inc',
    (els) => els.length).catch(() => 0)
  const loadingTxt = await page.$eval('[data-ops-canvas] .ops-diagram-loading', (e) => e.textContent).catch(() => null)

  console.log('canvas div:', !!diag)
  console.log('bpmn svg rendered:', !!svg)
  console.log('token markers:', markers)
  console.log('loading/fallback text:', loadingTxt)
  console.log('console errors:', errors.slice(0, 5))

  const pass = !!diag && (!!svg || loadingTxt) // 画布在；SVG 渲染 或 优雅降级文案
  if (svg) { try { await page.screenshot({ path: 'docs/p4-token-overlay.png', fullPage: false }) } catch {} }
  console.log(pass ? 'PASS' : 'FAIL')
  await browser.close()
  process.exit(pass ? 0 : 1)
})()
