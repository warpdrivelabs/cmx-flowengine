// 协同 M1 · 感知层 + 防冲突 · 双用户 CDP 真机测试（两个 browser context = 两个用户，共享同一草稿）。
//
// 断言：
//   C1 两用户各自在场条见到对方（roster≥2 头像）
//   C2 A 选中节点 a1 → B 画布该节点现远端选中高亮(.flow-collab-sel)
//   C3 A 保存草稿 → B 收「草稿已被更新 · 载入最新」通知条
//   C4 B 用过期 base 保存 → 冲突确认框（防静默覆盖）
//   C5 全程无 pageerror
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, clickDefPaged } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9091
const DK = 'collab_demo'
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

// 仅草稿的定义（不发布）→ loadDef 默认 shownVersion==null → 协同启用。process id = 定义 key。
const XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_collab" targetNamespace="http://cmx">
  <bpmn:process id="collab_demo" name="Collab Demo" isExecutable="true">
    <bpmn:startEvent id="s1" name="开始"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="任务A"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1" name="结束"><bpmn:incoming>e2</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="collab_demo">
    <bpmndi:BPMNShape id="s1_di" bpmnElement="s1"><dc:Bounds x="150" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="a1_di" bpmnElement="a1"><dc:Bounds x="240" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="en1_di" bpmnElement="en1"><dc:Bounds x="400" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="e1_di" bpmnElement="e1"><di:waypoint x="186" y="118"/><di:waypoint x="240" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e2_di" bpmnElement="e2"><di:waypoint x="340" y="118"/><di:waypoint x="400" y="118"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`

async function seedDraft () {
  await fetch(APIB + '/definitions/draft', {
    method: 'POST', headers: { 'X-Tenant': 'default', 'X-User': 'seeder', 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: 'Collab Demo', bpmnXml: XML }),
  }).catch(() => {})
}

// 无 X-API-Key：走 off 模式 X-User（apiJson 注入）定 actor，否则服务身份会盖掉 per-user presence。
const HARNESS = `<!doctype html><html><head><meta charset="utf-8"><title>collab</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh}.region{overflow:hidden;transform:translateZ(0);height:100%}#r-explorer{flex:0 0 240px}#r-content{flex:1}#r-property{flex:0 0 320px}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/design-workbench.js'
  window.__mod = mod
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  mod.default.views.property({ host: mk('h-property') })
  window.__ready = true
</script></body></html>`

// 深穿透 helpers（每 page 一份）。
const mkHelpers = (page) => ({
  count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
  has: (sel) => page.evaluate(({ de, sel }) => !!eval(de).find((e) => e.matches && e.matches(sel)), { de: deepEval, sel }),
  shapes: () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval),
  click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
  clickEl: (id) => page.evaluate(({ de, id }) => { const el = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id); if (el) { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true } return false }, { de: deepEval, id }),
  marks: (cls) => page.evaluate(({ de, cls }) => eval(de).filter((e) => e.classList && e.classList.contains(cls)).length, { de: deepEval, cls }),
})

async function boot (browser, user) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } })
  await ctx.addInitScript((u) => { try { localStorage.setItem('cmx_user_id', u) } catch {} }, user)
  const page = await ctx.newPage()
  const errs = []
  page.on('pageerror', (e) => errs.push(e.message))
  page.on('console', (m) => { if (m.type() === 'error') errs.push('c:' + m.text()) })
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/__collab_harness.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(600)
  const h = mkHelpers(page)
  // 载入 collab_demo 草稿（分页找），等图元。
  await clickDefPaged(page, DK)
  for (let i = 0; i < 16; i++) { await page.waitForTimeout(300); if ((await h.shapes()).includes('a1')) break }
  return { ctx, page, h, errs }
}

;(async () => {
  await seedDraft()
  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const harnessFile = path.join(WEB_DIR, '__collab_harness.html')
  fs.writeFileSync(harnessFile, HARNESS)
  const browser = await chromium.launch({ channel: 'chrome', headless: true })

  const A1 = await boot(browser, 'u_alice')
  const B1 = await boot(browser, 'u_bob')
  // B 的冲突确认框：捕获文案后取消（→ 载入最新路径）。
  let bDialog = null
  B1.page.on('dialog', (d) => { bDialog = d.message(); d.dismiss().catch(() => {}) })

  // 让两端 join + 首次心跳/roster 传播。
  await A1.page.waitForTimeout(2500)

  const aLoaded = (await A1.h.shapes()).includes('a1')
  const bLoaded = (await B1.h.shapes()).includes('a1')
  A('C0-both-load-draft', aLoaded && bLoaded, '两用户各自载入 collab_demo 草稿', `A=${aLoaded} B=${bLoaded}`)

  const aAvatars = await A1.h.count('.flow-collab-avatar')
  const bAvatars = await B1.h.count('.flow-collab-avatar')
  A('C1-presence-roster', aAvatars >= 2 && bAvatars >= 2, '两用户在场条各见 ≥2 头像（含对方）', `A=${aAvatars} B=${bAvatars}`)

  // C2: A 选中 a1 → B 画布 a1 现远端选中高亮。
  await A1.h.clickEl('a1')
  await A1.page.waitForTimeout(2000)
  const bRemoteSel = await B1.h.marks('flow-collab-sel')
  A('C2-remote-selection', bRemoteSel >= 1, 'A 选中 a1 → B 画布现远端选中高亮', `B .flow-collab-sel=${bRemoteSel}`)

  // C3: A 保存草稿 → B 收「草稿已更新」通知条。
  await A1.h.click('[data-act="save"]')
  await A1.page.waitForTimeout(2500)
  const bNotice = await B1.h.has('.flow-collab-notice.show')
  const bReloadBtn = await B1.h.has('[data-collab-reload]')
  A('C3-draft-saved-notice', bNotice && bReloadBtn, 'A 保存 → B 收「草稿已更新·载入最新」通知', `notice=${bNotice} reloadBtn=${bReloadBtn}`)

  // C4: B 用过期 base 保存 → 冲突确认框（B 的 baseUpdatedAt 停留在 A 保存前）。
  await B1.h.click('[data-act="save"]')
  await B1.page.waitForTimeout(1500)
  A('C4-save-conflict', !!bDialog && /草稿/.test(bDialog), 'B 过期保存 → 冲突确认框（防静默覆盖）', bDialog ? bDialog.slice(0, 60).replace(/\n/g, ' ') : '(无弹窗)')

  const errs = [...A1.errs, ...B1.errs].filter((e) => !/favicon|registry\/dam|Failed to load resource/i.test(e))
  A('C5-noerr', errs.length === 0, '全程无 pageerror', errs.slice(0, 2).join(' | ').slice(0, 200))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(harnessFile) } catch {}
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 协同M1 感知+防冲突(双用户): ${pass}/${results.length} ====`)
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); try { fs.unlinkSync(path.join(WEB_DIR, '__collab_harness.html')) } catch {} process.exit(2) })
