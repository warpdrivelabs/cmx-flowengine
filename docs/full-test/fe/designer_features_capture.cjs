// 流程设计器「新增前端功能」完整可视化验收 —— 每项功能断言 + 独立截图确认。
//
// 覆盖三组新增能力：
//   A. 模拟(simulate)  —— property「模拟」页签：facts/JSON → 运行 → 画布高亮 + trace
//   B. 版本 diff        —— 「对比」→ 选两版本 → 结构差异列表 + 画布 add/chg 高亮
//   C. 协同 M1          —— 双用户：在场头像条 / 远端选中高亮 / 草稿已更新通知 / 保存冲突框
//
// 每个功能点落一张带名截图到 docs/full-test/fe/shots/features/，供人工/视觉复核。
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, clickDefPaged, loadDefAndWait } = require('./_harness.cjs')

const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6'
const APIV1 = 'http://127.0.0.1:8091/api/flow/v1'
const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9097
const SHOTS = path.join(__dirname, 'shots', 'features')
const DIFF_KEY = 'dd_diff_demo'
const DK = 'collab_demo'
const results = []
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }
const shot = async (page, name) => { const f = path.join(SHOTS, name); await page.screenshot({ path: f, fullPage: true }); console.log('  📷 ' + name) }

// ── seed: 两版本 diff def（幂等，≥2 版跳过）──
const XML_V1 = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_dddiff" targetNamespace="http://cmx">
  <bpmn:process id="dd_diff_demo" name="Diff Demo" isExecutable="true">
    <bpmn:startEvent id="s1" name="开始"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="任务A"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1" name="结束"><bpmn:incoming>e2</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="dd_diff_demo">
    <bpmndi:BPMNShape id="s1_di" bpmnElement="s1"><dc:Bounds x="150" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="a1_di" bpmnElement="a1"><dc:Bounds x="240" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="en1_di" bpmnElement="en1"><dc:Bounds x="400" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="e1_di" bpmnElement="e1"><di:waypoint x="186" y="118"/><di:waypoint x="240" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e2_di" bpmnElement="e2"><di:waypoint x="340" y="118"/><di:waypoint x="400" y="118"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`
const XML_V2 = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_dddiff" targetNamespace="http://cmx">
  <bpmn:process id="dd_diff_demo" name="Diff Demo" isExecutable="true">
    <bpmn:startEvent id="s1" name="开始"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="任务A改"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:userTask id="b1" name="任务B"><bpmn:incoming>e2</bpmn:incoming><bpmn:outgoing>e3</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1" name="结束"><bpmn:incoming>e3</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="b1"/>
    <bpmn:sequenceFlow id="e3" sourceRef="b1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="dd_diff_demo">
    <bpmndi:BPMNShape id="s1_di" bpmnElement="s1"><dc:Bounds x="150" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="a1_di" bpmnElement="a1"><dc:Bounds x="240" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="b1_di" bpmnElement="b1"><dc:Bounds x="390" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="en1_di" bpmnElement="en1"><dc:Bounds x="550" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="e1_di" bpmnElement="e1"><di:waypoint x="186" y="118"/><di:waypoint x="240" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e2_di" bpmnElement="e2"><di:waypoint x="340" y="118"/><di:waypoint x="390" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e3_di" bpmnElement="e3"><di:waypoint x="490" y="118"/><di:waypoint x="550" y="118"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`
// 协同：仅草稿（不发布）→ loadDef 默认 shownVersion==null → 协同启用。process id = key。
const XML_COLLAB = `<?xml version="1.0" encoding="UTF-8"?>
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

async function apiPost (p, body, hdr) { const r = await fetch(APIV1 + p, { method: 'POST', headers: { 'Content-Type': 'application/json', ...(hdr || { 'X-API-Key': KEY }) }, body: JSON.stringify(body) }); return r.json() }
async function apiGet (p) { const r = await fetch(APIV1 + p, { headers: { 'X-API-Key': KEY } }); return r.json() }
async function seedDiff () {
  const cur = (((await apiGet('/definitions/' + DIFF_KEY)).data) || {}).versions || []
  if (cur.length >= 2) return 'skip(' + cur.length + ')'
  await apiPost('/definitions/draft', { name: 'Diff Demo', bpmnXml: XML_V1 }); await apiPost('/definitions/' + DIFF_KEY + '/publish', { note: 'v1' })
  await apiPost('/definitions/draft', { name: 'Diff Demo', bpmnXml: XML_V2 }); await apiPost('/definitions/' + DIFF_KEY + '/publish', { note: 'v2' })
  return 'seeded'
}
async function seedCollab () { await fetch(APIB + '/definitions/draft', { method: 'POST', headers: { 'X-Tenant': 'default', 'X-User': 'seeder', 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'Collab Demo', bpmnXml: XML_COLLAB }) }).catch(() => {}) }

// 单区 harness（sim/diff，带 X-API-Key）。
const HARNESS_SD = `<!doctype html><html><head><meta charset="utf-8"><title>features</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh;width:100vw;overflow:hidden}.region{box-sizing:border-box;overflow:hidden;transform:translateZ(0);position:relative;height:100%}#r-explorer{flex:0 0 240px;border-right:1px solid #e2e6ec}#r-content{flex:1 1 auto;min-width:0}#r-property{flex:0 0 360px;border-left:1px solid #e2e6ec}.host{height:100%;display:block}</style></head>
<body><div id="stage"><div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div><div class="region" id="r-content"><div class="host" id="h-content"></div></div><div class="region" id="r-property"><div class="host" id="h-property"></div></div></div>
<script type="module">
  import * as mod from '/core/design-workbench.js'
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({ 'X-API-Key': ${JSON.stringify(KEY)} }), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk(id){const el=document.getElementById(id);const sr=el.attachShadow({mode:'open'});const r=document.createElement('div');r.className='native-page-root';r.style.height='100%';sr.appendChild(r);return el}
  mod.default.views.content({host:mk('h-content')});mod.default.views.explorer({host:mk('h-explorer')});mod.default.views.property({host:mk('h-property')})
  window.__ready=true
</script></body></html>`
// 协同 harness（无 X-API-Key → off 模式 X-User 定 actor）。
const HARNESS_COLLAB = `<!doctype html><html><head><meta charset="utf-8"><title>collab</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh}.region{overflow:hidden;transform:translateZ(0);height:100%}#r-explorer{flex:0 0 240px}#r-content{flex:1}#r-property{flex:0 0 320px}.host{height:100%;display:block}</style></head>
<body><div id="stage"><div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div><div class="region" id="r-content"><div class="host" id="h-content"></div></div><div class="region" id="r-property"><div class="host" id="h-property"></div></div></div>
<script type="module">
  import * as mod from '/core/design-workbench.js'
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}), bpmnBase: '/portal/vendor/bpmn-js' })
  function mk(id){const el=document.getElementById(id);const sr=el.attachShadow({mode:'open'});const r=document.createElement('div');r.className='native-page-root';r.style.height='100%';sr.appendChild(r);return el}
  mod.default.views.content({host:mk('h-content')});mod.default.views.explorer({host:mk('h-explorer')});mod.default.views.property({host:mk('h-property')})
  window.__ready=true
</script></body></html>`

const mkH = (page) => ({
  findOne: (sel) => page.evaluate(({ de, sel }) => !!eval(de).find((x) => x.matches && x.matches(sel)), { de: deepEval, sel }),
  click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
  count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
  text: (sel) => page.evaluate(({ de, sel }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); return e ? (e.textContent || '').trim() : null }, { de: deepEval, sel }),
  setVal: (sel, val) => page.evaluate(({ de, sel, val }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); if (!e) return false; e.value = val; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); return true }, { de: deepEval, sel, val }),
  shapes: () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval),
  clickEl: (id) => page.evaluate(({ de, id }) => { const el = eval(de).find((e) => e.getAttribute && e.getAttribute('data-element-id') === id); if (el) { el.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true } return false }, { de: deepEval, id }),
  marks: (cls) => page.evaluate(({ de, cls }) => eval(de).filter((e) => e.classList && e.classList.contains(cls)).length, { de: deepEval, cls }),
})

;(async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  console.log('seed diff:', await seedDiff()); await seedCollab()
  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const browser = await chromium.launch({ channel: 'chrome', headless: true })

  // ════════════ A + B：模拟 + 版本 diff（单区，X-API-Key）════════════
  const fSD = path.join(WEB_DIR, '__feat_sd.html'); fs.writeFileSync(fSD, HARNESS_SD)
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1680, height: 1000 } })
  const page = await ctx.newPage()
  const errs = []
  page.on('pageerror', (e) => errs.push(e.message)); page.on('console', (m) => { if (m.type() === 'error') errs.push('c:' + m.text()) })
  page.on('dialog', (d) => d.accept())
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/__feat_sd.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready===true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(700)
  const h = mkH(page)
  for (let i = 0; i < 30; i++) { await page.waitForTimeout(300); if ((await h.shapes()).length > 0) break } // 等初始图

  // A. 模拟
  const teLoaded = await loadDefAndWait(page, 'travel_expense', 'mgr')
  await h.click('[data-ptab="sim"]'); await page.waitForTimeout(600)
  const simTab = await h.findOne('[data-sim-run]') && await h.findOne('[data-sim-raw]')
  A('A1-sim-tab', '模拟页签渲染（运行按钮+变量框）', teLoaded && simTab, `loaded=${teLoaded}`)
  await shot(page, 'feat-01-sim-tab.png')

  await h.setVal('[data-sim-raw]', '{"amount":30000}'); await h.click('[data-sim-run]')
  for (let i = 0; i < 20; i++) { await page.waitForTimeout(300); if (await h.findOne('.flow-sim-res')) break }
  await page.waitForTimeout(400)
  const hitBig = await h.count('.flow-sim-hit'); const resBig = (await h.text('.flow-sim-res')) || ''
  A('A2-sim-big', '大额(30000)运行→画布高亮≥5+trace含director', hitBig >= 5 && /director/.test(resBig) && /可达结束/.test(resBig), `hit=${hitBig}`)
  await shot(page, 'feat-02-sim-big-director.png')

  await h.setVal('[data-sim-raw]', '{"amount":5000}'); await h.click('[data-sim-run]'); await page.waitForTimeout(1800)
  const resSmall = (await h.text('.flow-sim-res')) || ''; const hitSmall = await h.count('.flow-sim-hit')
  A('A3-sim-small', '小额(5000)运行→trace无director（走另一分支）', !/director/.test(resSmall) && /可达结束/.test(resSmall) && hitSmall >= 4, `hit=${hitSmall}`)
  await shot(page, 'feat-03-sim-small-branch.png')

  // B. 版本 diff
  await loadDefAndWait(page, DIFF_KEY, 's1')
  await h.click('[data-act="diff"]'); await page.waitForTimeout(700)
  const diffPanel = await h.findOne('[data-diff-run]') && await h.findOne('[data-ptab="diff"]')
  A('B1-diff-panel', '「对比」→差异面板+「差异」页签', diffPanel, '')
  await shot(page, 'feat-04-diff-panel.png')

  await h.setVal('[data-diff-va]', '1'); await h.setVal('[data-diff-vb]', '2'); await h.click('[data-diff-run]')
  for (let i = 0; i < 20; i++) { await page.waitForTimeout(300); if (await h.findOne('.flow-diff-row')) break }
  await page.waitForTimeout(300)
  const rAdd = await h.count('.flow-diff-row.add'); const rChg = await h.count('.flow-diff-row.chg')
  A('B2-diff-result', 'v1→v2 差异：新增≥1+修改≥1（列表+画布高亮）', rAdd >= 1 && rChg >= 1, `add=${rAdd} chg=${rChg}`)
  await shot(page, 'feat-05-diff-result.png')
  await ctx.close()

  // ════════════ C：协同 M1（双 context = 两用户，共享 collab_demo 草稿）════════════
  const fC = path.join(WEB_DIR, '__feat_collab.html'); fs.writeFileSync(fC, HARNESS_COLLAB)
  const boot = async (user) => {
    const c = await browser.newContext({ viewport: { width: 1180, height: 820 } })
    await c.addInitScript((u) => { try { localStorage.setItem('cmx_user_id', u) } catch {} }, user)
    const p = await c.newPage(); const e = []
    p.on('pageerror', (x) => e.push(x.message)); p.on('console', (m) => { if (m.type() === 'error') e.push('c:' + m.text()) })
    await vendorRoute(p, c)
    await p.goto(`http://127.0.0.1:${PORT}/__feat_collab.html`, { waitUntil: 'domcontentloaded' })
    await p.waitForFunction('window.__ready===true', { timeout: 8000 }).catch(() => {})
    await p.waitForFunction(`${deepEval}.some(el=>el.className&&(''+el.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
    await p.waitForTimeout(600)
    const hh = mkH(p); await clickDefPaged(p, DK)
    for (let i = 0; i < 16; i++) { await p.waitForTimeout(300); if ((await hh.shapes()).includes('a1')) break }
    return { c, p, h: hh, e }
  }
  const Alice = await boot('u_alice'); const Bob = await boot('u_bob')
  let bDialog = null
  Bob.p.on('dialog', (d) => { bDialog = d.message(); d.dismiss().catch(() => {}) })
  await Alice.p.waitForTimeout(2500) // join + roster 传播

  // C1. 在场头像条（各见 ≥2）
  const aAv = await Alice.h.count('.flow-collab-avatar'); const bAv = await Bob.h.count('.flow-collab-avatar')
  A('C1-presence', '两用户在场条各见≥2头像（含对方）', aAv >= 2 && bAv >= 2, `A=${aAv} B=${bAv}`)
  await shot(Bob.p, 'feat-06-collab-presence.png')

  // C2. A 选中 a1 → B 画布远端高亮
  await Alice.h.clickEl('a1'); await Alice.p.waitForTimeout(2000)
  const bSel = await Bob.h.marks('flow-collab-sel')
  A('C2-remote-sel', 'A 选中 a1 → B 画布现远端选中高亮', bSel >= 1, `B .flow-collab-sel=${bSel}`)
  await shot(Bob.p, 'feat-07-collab-remote-selection.png')

  // C3. A 保存 → B 收「草稿已更新」通知条
  await Alice.h.click('[data-act="save"]'); await Alice.p.waitForTimeout(2500)
  const bNotice = await Bob.h.findOne('.flow-collab-notice.show'); const bReload = await Bob.h.findOne('[data-collab-reload]')
  A('C3-notice', 'A 保存→B 收「草稿已更新·载入最新」通知', bNotice && bReload, `notice=${bNotice} reload=${bReload}`)
  await shot(Bob.p, 'feat-08-collab-draft-notice.png')

  // C4. B 用过期 base 保存 → 冲突确认框（native confirm 无法截图，截保存前状态 + 记录文案）
  await shot(Bob.p, 'feat-09-collab-conflict-precondition.png')
  await Bob.h.click('[data-act="save"]'); await Bob.p.waitForTimeout(1500)
  A('C4-conflict', 'B 过期保存→冲突确认框（防静默覆盖）', !!bDialog && /草稿/.test(bDialog), bDialog ? bDialog.replace(/\n/g, ' ').slice(0, 80) : '(无弹窗)')

  const allErr = [...errs, ...Alice.e, ...Bob.e].filter((x) => !/favicon|registry\/dam|Failed to load resource/i.test(x))
  A('Z-noerr', '全程无 pageerror', allErr.length === 0, allErr.slice(0, 2).join(' | ').slice(0, 180))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(fSD); fs.unlinkSync(fC) } catch {}
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 新增前端功能可视化验收: ${pass}/${results.length} ====`)
  console.log('截图目录:', SHOTS)
  fs.writeFileSync(path.join(__dirname, 'designer-features-results.json'), JSON.stringify({ results, conflictDialog: bDialog }, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
