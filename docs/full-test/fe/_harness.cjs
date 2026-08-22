// 共享 FE 测试脚手架（cmx-flowengine 门户级/demo 级 CDP 测试通用）。
//
// 目标：每个 FE 测试**只依赖 flow-server(:8091)**，不再依赖门户(:8080)或外部 demo 静态服(:9099)。
//   ① startStatic  —— 自起 python 静态服（serve web/），测试自包含。
//   ② vendorRoute  —— /portal/vendor/** 路由：bpmn-js 资产从磁盘服（门户没起也能跑），缺失才回退 :8080。
//   ③ clickDefPaged —— explorer 分页（每页固定条、无搜索框）时翻页定位 .flow-def[data-key] 再点，
//                      避免定义列表增长后目标被挤出首页导致点不到（历史脆弱点）。
//   ④ deepEval     —— 递归穿透所有 shadow root 收集全部元素的 IIFE 源码串（page.evaluate 用）。
const { spawn } = require('child_process')
const path = require('path')
const fs = require('fs')

const WEB_DIR = path.resolve(__dirname, '../../../web')                       // cmx-flowengine/web
const VENDOR_ROOT = path.resolve(__dirname, '../../../../CMXPortalManager/public') // bpmn-js vendor 磁盘根
const MIME = { '.js': 'text/javascript', '.css': 'text/css', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.eot': 'application/vnd.ms-fontobject', '.svg': 'image/svg+xml', '.json': 'application/json', '.html': 'text/html', '.png': 'image/png' }

// 递归穿透所有 shadow root（3 独立 region shadow / demo 单组件 shadow 皆适用）。
const deepEval = `(() => { const r=[]; const w=root=>{ root.querySelectorAll('*').forEach(e=>{ r.push(e); if(e.shadowRoot) w(e.shadowRoot); }); }; w(document); return r; })()`

// 起 python 静态服（serve web/）。返回 child——测试收尾 srv.kill('SIGTERM')。
function startStatic (port) { return spawn('python3', ['-m', 'http.server', String(port)], { cwd: WEB_DIR, stdio: 'ignore' }) }

// 注册 bpmn-js vendor 路由：磁盘优先（CMXPortalManager/public/vendor），缺失回退门户 :8080（需传 ctx）。
// 同时匹配门户前缀 /portal/vendor/bpmn-js/** 与可嵌组件默认的裸 /vendor/bpmn-js/**（两种资产基路径）。
async function vendorRoute (page, ctx) {
  await page.route('**/vendor/bpmn-js/**', async (route) => {
    const url = route.request().url()
    const rel = url.substring(url.indexOf('/vendor/bpmn-js/')).split('?')[0] // → /vendor/bpmn-js/...
    try {
      const buf = fs.readFileSync(path.join(VENDOR_ROOT, rel))
      await route.fulfill({ status: 200, body: buf, headers: { 'content-type': MIME[path.extname(rel).toLowerCase()] || 'application/octet-stream' } })
    } catch {
      if (!ctx) return route.abort()
      try { const r = await ctx.request.get('http://127.0.0.1:8080/portal' + rel); await route.fulfill({ status: r.status(), body: await r.body(), headers: { 'content-type': r.headers()['content-type'] || 'application/octet-stream' } }) } catch { await route.abort() }
    }
  })
}

// explorer 分页找 .flow-def[data-key] 再点。返回是否点到（false=遍历完所有页仍无此 def）。
async function clickDefPaged (page, key) {
  const present = () => page.evaluate(({ de, k }) => !!eval(de).find((e) => e.classList && e.classList.contains('flow-def') && e.dataset.key === k), { de: deepEval, k: key })
  await page.evaluate((de) => { const b = eval(de).find((e) => e.matches && e.matches('[data-page="first"]')); if (b && !b.disabled) b.click() }, deepEval).catch(() => {})
  await page.waitForTimeout(250)
  for (let p = 0; p < 15; p++) {
    if (await present()) break
    const moved = await page.evaluate((de) => { const b = eval(de).find((e) => e.matches && e.matches('[data-page="next"]')); if (b && !b.disabled) { b.click(); return true } return false }, deepEval)
    await page.waitForTimeout(300)
    if (!moved) break
  }
  return page.evaluate(({ de, k }) => { const el = eval(de).find((e) => e.classList && e.classList.contains('flow-def') && e.dataset.key === k); if (el) { el.click(); return true } return false }, { de: deepEval, k: key })
}

// 载入 def 并等待期望图元出现在画布（多轮重试，容忍 bpmn-js 初始化竞态）。返回 true=已载。
async function loadDefAndWait (page, key, expectId) {
  const shapes = () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval)
  for (let round = 0; round < 3; round++) {
    await clickDefPaged(page, key)
    for (let i = 0; i < 12; i++) { await page.waitForTimeout(300); if ((await shapes()).includes(expectId)) return true }
  }
  return false
}

module.exports = { WEB_DIR, VENDOR_ROOT, MIME, deepEval, startStatic, vendorRoute, clickDefPaged, loadDefAndWait }
