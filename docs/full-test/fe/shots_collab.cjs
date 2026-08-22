// 协同功能确认截图（双用户）：M1 在场条+远端选中、M1 草稿通知、M2 对象级合并。→ shots/collab/
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, clickDefPaged } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9097
const DK = 'collab_shot_demo'
const SHOTS = path.join(__dirname, 'shots', 'collab')

const XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_shot" targetNamespace="http://cmx">
  <bpmn:process id="collab_shot_demo" name="Collab Shot Demo" isExecutable="true">
    <bpmn:startEvent id="s1" name="开始"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="审批任务"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1" name="结束"><bpmn:incoming>e2</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="collab_shot_demo">
    <bpmndi:BPMNShape id="s1_di" bpmnElement="s1"><dc:Bounds x="160" y="120" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="a1_di" bpmnElement="a1"><dc:Bounds x="260" y="98" width="110" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="en1_di" bpmnElement="en1"><dc:Bounds x="440" y="120" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="e1_di" bpmnElement="e1"><di:waypoint x="196" y="138"/><di:waypoint x="260" y="138"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e2_di" bpmnElement="e2"><di:waypoint x="370" y="138"/><di:waypoint x="440" y="138"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`

const HARNESS = `<!doctype html><html><head><meta charset="utf-8"><title>collab</title>
<style>html,body{margin:0;height:100%;font-family:-apple-system,"PingFang SC",sans-serif}#stage{display:flex;height:100vh}.region{overflow:hidden;height:100%;border-right:1px solid #e1e4e8}#r-explorer{flex:0 0 220px}#r-content{flex:1}#r-property{flex:0 0 320px}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
  <div class="region" id="r-property"><div class="host" id="h-property"></div></div>
</div>
<script type="module">
  import * as mod from '/core/design-workbench.js'
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  mod.default.views.property({ host: mk('h-property') })
  window.__ready = true
</script></body></html>`

const H = (page) => ({
  clickEl: (id) => page.evaluate(({ de, id }) => { const el = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id); if (el) { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true } return false }, { de: deepEval, id }),
  shapes: () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval),
  setName: (val) => page.evaluate(({ de, val }) => { const inp = eval(de).find((e) => e.matches && e.matches('input[data-prop="name"]')); if (!inp) return false; inp.value = val; inp.dispatchEvent(new Event('change', { bubbles: true })); return true }, { de: deepEval, val }),
  click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
})

async function boot (browser, user) {
  const ctx = await browser.newContext({ viewport: { width: 1180, height: 760 }, deviceScaleFactor: 2 })
  await ctx.addInitScript((u) => { try { localStorage.setItem('cmx_user_id', u) } catch {} }, user)
  const page = await ctx.newPage()
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/__collab_shot.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(600)
  const h = H(page)
  await clickDefPaged(page, DK)
  for (let i = 0; i < 16; i++) { await page.waitForTimeout(300); if ((await h.shapes()).includes('a1')) break }
  return { ctx, page, h }
}

;(async () => {
  await fetch(APIB + '/definitions/draft', { method: 'POST', headers: { 'X-User': 'seeder', 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'Collab Shot Demo', bpmnXml: XML }) }).catch(() => {})
  const srv = startStatic(PORT); await new Promise((r) => setTimeout(r, 900))
  const file = path.join(WEB_DIR, '__collab_shot.html'); fs.writeFileSync(file, HARNESS)
  const browser = await chromium.launch({ channel: 'chrome', headless: true })

  const A1 = await boot(browser, 'u_alice')
  const B1 = await boot(browser, 'u_bob')
  await A1.page.waitForTimeout(2500)

  // 1) M1 在场条：两头像
  await A1.page.screenshot({ path: path.join(SHOTS, 'collab-01-presence-two-users.png') })

  // 2) M1 远端选中：A 选 a1 → B 画布远端高亮
  await A1.h.clickEl('a1')
  await A1.page.waitForTimeout(1800)
  await B1.page.screenshot({ path: path.join(SHOTS, 'collab-02-remote-selection.png') })

  // 3) M2 对象级合并：A 改 a1 名称 → B 画布同步
  await A1.h.setName('会签·财务复核')
  await A1.page.waitForTimeout(2000)
  await B1.page.screenshot({ path: path.join(SHOTS, 'collab-03-m2-property-merged.png') })

  // 4) M1 草稿保存通知：A 保存 → B 通知条
  await A1.h.click('[data-act="save"]')
  await A1.page.waitForTimeout(2200)
  await B1.page.screenshot({ path: path.join(SHOTS, 'collab-04-draft-saved-notice.png') })

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(file) } catch {}
  console.log('collab shots →', SHOTS)
  process.exit(0)
})().catch((e) => { console.error('FATAL', e); try { fs.unlinkSync(path.join(WEB_DIR, '__collab_shot.html')) } catch {} process.exit(2) })
