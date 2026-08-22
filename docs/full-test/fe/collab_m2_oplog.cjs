// 协同 M2 · 对象级属性合并 · 双用户 CDP 真机测试（两 browser context = 两用户，共享同一草稿）。
//
// M2 = 一端改节点属性（bpmn-js updateProperties）→ 经 /design/op 广播 → 另一端就地合并（对象级 LWW）。
// 断言：
//   M0 两用户各自载入 collab_m2_demo 草稿（含节点 a1）
//   M1 A 改 a1 的名称 → B 画布 a1 的显示名同步为新值（远端对象级合并）
//   M2 B 改 a1 的名称 → A 画布 a1 同步（双向）
//   M3 B 未点保存但已合并 A 的改动（实时性：不经保存/刷新）
//   M4 全程无 pageerror
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, clickDefPaged } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9093
const DK = 'collab_m2_demo'
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

const XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_m2" targetNamespace="http://cmx">
  <bpmn:process id="collab_m2_demo" name="Collab M2 Demo" isExecutable="true">
    <bpmn:startEvent id="s1" name="开始"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="原始任务名"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1" name="结束"><bpmn:incoming>e2</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="collab_m2_demo">
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
    body: JSON.stringify({ name: 'Collab M2 Demo', bpmnXml: XML }),
  }).catch(() => {})
}

const HARNESS = `<!doctype html><html><head><meta charset="utf-8"><title>collab-m2</title>
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

const mkHelpers = (page) => ({
  count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
  has: (sel) => page.evaluate(({ de, sel }) => !!eval(de).find((e) => e.matches && e.matches(sel)), { de: deepEval, sel }),
  shapes: () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval),
  clickEl: (id) => page.evaluate(({ de, id }) => { const el = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id); if (el) { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true } return false }, { de: deepEval, id }),
  // 取节点在画布上的显示名（bpmn label 文本，穿透 shadow 找 data-element-id 元素内的文本）。
  nodeLabel: (id) => page.evaluate(({ de, id }) => {
    const el = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id)
    if (!el) return null
    // bpmn-js 把标签渲染在节点 g 内的 <text>/<tspan>。取全部文本拼接。
    const texts = el.querySelectorAll ? [...el.querySelectorAll('text, tspan')].map((t) => t.textContent).join('') : ''
    return texts || (el.textContent || '').trim()
  }, { de: deepEval, id }),
  // 设置属性面板里 data-prop=name 的输入值并派发 change（触发 applyProp → broadcastOp）。
  setName: (val) => page.evaluate(({ de, val }) => {
    const inp = eval(de).find((e) => e.matches && e.matches('input[data-prop="name"]'))
    if (!inp) return false
    inp.value = val
    inp.dispatchEvent(new Event('change', { bubbles: true }))
    return true
  }, { de: deepEval, val }),
})

async function boot (browser, user) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } })
  await ctx.addInitScript((u) => { try { localStorage.setItem('cmx_user_id', u) } catch {} }, user)
  const page = await ctx.newPage()
  const errs = []
  page.on('pageerror', (e) => errs.push(e.message))
  page.on('console', (m) => { if (m.type() === 'error') errs.push('c:' + m.text()) })
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/__collab_m2_harness.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(600)
  const h = mkHelpers(page)
  await clickDefPaged(page, DK)
  for (let i = 0; i < 16; i++) { await page.waitForTimeout(300); if ((await h.shapes()).includes('a1')) break }
  return { ctx, page, h, errs }
}

;(async () => {
  await seedDraft()
  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const harnessFile = path.join(WEB_DIR, '__collab_m2_harness.html')
  fs.writeFileSync(harnessFile, HARNESS)
  const browser = await chromium.launch({ channel: 'chrome', headless: true })

  const A1 = await boot(browser, 'u_alice')
  const B1 = await boot(browser, 'u_bob')
  await A1.page.waitForTimeout(2500)  // join + roster 传播

  const aLoaded = (await A1.h.shapes()).includes('a1')
  const bLoaded = (await B1.h.shapes()).includes('a1')
  A('M0-both-load', aLoaded && bLoaded, '两用户各自载入 collab_m2_demo 草稿', `A=${aLoaded} B=${bLoaded}`)

  // M1: A 选中 a1 → 改名称 → B 画布 a1 名称同步。
  const NEW_A = 'Alice改名_' + Math.floor(Date.now ? 0 : 0) + 'X'  // 固定串（Date 在脚本禁用，用固定标识）
  await A1.h.clickEl('a1')
  await A1.page.waitForTimeout(500)
  await A1.h.setName(NEW_A)
  await A1.page.waitForTimeout(2200)
  const bLabel1 = await B1.h.nodeLabel('a1')
  A('M1-a-to-b-merge', bLabel1 && bLabel1.includes('Alice改名'), 'A 改 a1 名称 → B 画布同步（对象级合并）', `B label="${bLabel1}"`)

  // M2: B 选中 a1 → 改名称 → A 画布 a1 名称同步（双向）。
  const NEW_B = 'Bob改名YY'
  await B1.h.clickEl('a1')
  await B1.page.waitForTimeout(500)
  await B1.h.setName(NEW_B)
  await B1.page.waitForTimeout(2200)
  const aLabel2 = await A1.h.nodeLabel('a1')
  A('M2-b-to-a-merge', aLabel2 && aLabel2.includes('Bob改名'), 'B 改 a1 名称 → A 画布同步（双向合并）', `A label="${aLabel2}"`)

  // M3: 合并是实时的（B 从未点保存，A 从未刷新）——由 M1/M2 已隐含；此处再确认 B 未出现「草稿已更新」通知
  //     （M2 是 op 合并，不是 draft.saved 保存通知）。
  const bHasSaveNotice = await B1.h.has('.flow-collab-notice.show')
  A('M3-realtime-no-save', !bHasSaveNotice, 'M2 合并为实时 op（非草稿保存通知）', `B saveNotice=${bHasSaveNotice}`)

  const errs = [...A1.errs, ...B1.errs].filter((e) => !/favicon|registry\/dam|Failed to load resource/i.test(e))
  A('M4-noerr', errs.length === 0, '全程无 pageerror', errs.slice(0, 2).join(' | ').slice(0, 200))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(harnessFile) } catch {}
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 协同M2 对象级合并(双用户): ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'collab-m2-results.json'), JSON.stringify(results, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); try { fs.unlinkSync(path.join(WEB_DIR, '__collab_m2_harness.html')) } catch {} process.exit(2) })
