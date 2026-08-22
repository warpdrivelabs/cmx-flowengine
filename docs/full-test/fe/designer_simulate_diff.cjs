// 设计器「模拟(simulate) + 版本 diff」· 门户级多区真机测试（3 独立 shadow root：explorer/content/property）
//
// 复刻门户结构：explorer / content / property 各挂独立 shadow host（同 subflow_drilldown 保真），
// 驱动真实交互：载入定义 → 切「模拟」页签 → 给样例变量运行 → 断言画布高亮 + trace；
//            → 切「对比」→ 选两版本 → 断言结构差异列表 + 画布高亮 + 守卫 + 退出清理。
//
// 断言：
//   S1 模拟页签渲染（facts/JSON/运行按钮）
//   S2 大额(amount=30000) 运行 → 画布走过节点/边/任务高亮(flow-sim-*) + trace 含 director(总监分支)
//   S3 小额(amount=5000) 运行 → trace 无 director（排他网关走另一分支）+ 可达结束
//   S4 切走模拟页签 → 画布高亮清除
//   D1 「对比」按钮 → property 出差异面板 + 「差异」页签
//   D2 dd_diff_demo v1→v2 → 差异列表：新增≥1(任务B) + 修改≥1(任务A/连线)
//   D3 同版本守卫（va==vb → 报错「请选择两个不同的版本」）
//   D4 退出对比 → 「差异」页签消失 + diff 高亮清除
//   T-noerr 全程无 pageerror（favicon/DAM 注册表噪声已滤）
const { chromium } = require('playwright')
const path = require('path')
const fs = require('fs')
const { WEB_DIR, deepEval, startStatic, vendorRoute, loadDefAndWait } = require('./_harness.cjs')

const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6'
const API = 'http://127.0.0.1:8091/api/flow/v1'
const SHOTS = path.join(__dirname, 'shots')
const PORT = 9096
const results = []
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }


// ── 两版本差异 def（seed 用；key 由 process id 派生 = dd_diff_demo）──
const DIFF_KEY = 'dd_diff_demo'
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

async function apiPost (p, body) {
  const r = await fetch(API + p, { method: 'POST', headers: { 'X-API-Key': KEY, 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
  return r.json()
}
async function apiGet (p) { const r = await fetch(API + p, { headers: { 'X-API-Key': KEY } }); return r.json() }

// 幂等 seed：dd_diff_demo 已有 ≥2 版本则跳过（避免重复跑无限增版本）。
async function seedDiffDef () {
  const cur = (((await apiGet('/definitions/' + DIFF_KEY)).data) || {}).versions || []
  if (cur.length >= 2) return { seeded: false, versions: cur.length }
  await apiPost('/definitions/draft', { name: 'Diff Demo', bpmnXml: XML_V1 })
  await apiPost('/definitions/' + DIFF_KEY + '/publish', { note: 'v1' })
  await apiPost('/definitions/draft', { name: 'Diff Demo', bpmnXml: XML_V2 })
  await apiPost('/definitions/' + DIFF_KEY + '/publish', { note: 'v2' })
  const now = (((await apiGet('/definitions/' + DIFF_KEY)).data) || {}).versions || []
  return { seeded: true, versions: now.length }
}

const HARNESS_HTML = `<!doctype html><html><head><meta charset="utf-8"><title>flow simulate/diff harness</title>
<style>
  html,body{margin:0;height:100%}
  #stage{display:flex;height:100vh;width:100vw;overflow:hidden}
  .region{box-sizing:border-box;overflow:hidden;transform:translateZ(0);position:relative;height:100%}
  #r-explorer{flex:0 0 240px;border-right:1px solid #e2e6ec}
  #r-content{flex:1 1 auto;min-width:0}
  #r-property{flex:0 0 340px;border-left:1px solid #e2e6ec}
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
  function makeHost (id) {
    const el = document.getElementById(id)
    const sr = el.attachShadow({ mode: 'open' })
    const root = document.createElement('div')
    root.className = 'native-page-root'
    root.style.height = '100%'
    sr.appendChild(root)
    return el
  }
  mod.default.views.content({ host: makeHost('h-content') })
  mod.default.views.explorer({ host: makeHost('h-explorer') })
  mod.default.views.property({ host: makeHost('h-property') })
  window.__ready = true
</script>
</body></html>`

;(async () => {
  if (!fs.existsSync(SHOTS)) fs.mkdirSync(SHOTS, { recursive: true })
  const seed = await seedDiffDef()
  console.log('seed dd_diff_demo:', JSON.stringify(seed))

  const srv = startStatic(PORT)
  await new Promise((r) => setTimeout(r, 900))
  const browser = await chromium.launch({ channel: 'chrome', headless: true })
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1680, height: 1000 } })
  const page = await ctx.newPage()
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  page.on('console', (m) => { if (m.type() === 'error') errors.push('console:' + m.text()) })
  page.on('dialog', (d) => d.accept())
  await vendorRoute(page, ctx) // bpmn-js vendor 从磁盘服（门户没起也能跑）
  const harnessFile = path.join(WEB_DIR, '__sd_harness.html')
  fs.writeFileSync(harnessFile, HARNESS_HTML)

  // ── deep helpers ──
  const findOne = (sel) => page.evaluate(({ de, sel }) => !!eval(de).find((x) => x.matches && x.matches(sel)), { de: deepEval, sel })
  const clickSel = (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel })
  const countSel = (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel })
  const textSel = (sel) => page.evaluate(({ de, sel }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); return e ? (e.textContent || '').trim() : null }, { de: deepEval, sel })
  const setVal = (sel, val) => page.evaluate(({ de, sel, val }) => { const e = eval(de).find((x) => x.matches && x.matches(sel)); if (!e) return false; e.value = val; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); return true }, { de: deepEval, sel, val })
  const shapes = () => page.evaluate((de) => eval(de).filter((e) => e.getAttribute && e.getAttribute('data-element-id')).map((e) => e.getAttribute('data-element-id')), deepEval)
  // 等 bpmn-js 初始化 + 初始（空模板）导入完成——否则首个 loadDef 的 import 会被初始 mount 覆盖（竞态）。
  const waitModeler = async () => { for (let i = 0; i < 34; i++) { await page.waitForTimeout(300); if ((await shapes()).length > 0) return true } return false }
  const loadDef = (key, expectId) => loadDefAndWait(page, key, expectId) // 分页翻页 + 竞态重试（共享脚手架）


  await page.goto(`http://127.0.0.1:${PORT}/__sd_harness.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction('window.__ready === true', { timeout: 8000 }).catch(() => {})
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 9000 }).catch(() => {})
  await page.waitForTimeout(700)
  await waitModeler() // 等初始图导入，避免首个 loadDef 竞态被覆盖

  // ════════════════ 模拟(simulate) ════════════════
  const loadedTE = await loadDef('travel_expense', 'mgr')
  A('S0-load', '载入 travel_expense（画布出 mgr 图元）', loadedTE, `shapes=${(await shapes()).length}`)

  // 切「模拟」页签
  await clickSel('[data-ptab="sim"]')
  await page.waitForTimeout(500)
  const simUi = await findOne('[data-sim-run]') && await findOne('[data-sim-raw]')
  A('S1-sim-tab', '「模拟」页签渲染（运行按钮 + JSON 变量框）', simUi, '')

  // S2 大额 → f_big 分支，director(总监) 在办理人；画布高亮
  await setVal('[data-sim-raw]', '{"amount":30000}')
  await clickSel('[data-sim-run]')
  for (let i = 0; i < 20; i++) { await page.waitForTimeout(300); if (await findOne('.flow-sim-res')) break }
  await page.waitForTimeout(400)
  const hitBig = await countSel('.flow-sim-hit')
  const flowBig = await countSel('.flow-sim-flow')
  const taskBig = await countSel('.flow-sim-task')
  const resBig = (await textSel('.flow-sim-res')) || ''
  await page.screenshot({ path: path.join(SHOTS, 'sd-01-sim-big.png'), fullPage: true })
  A('S2-sim-big-markers', '大额运行 → 画布高亮 走过节点≥5 + 边≥1 + 任务≥1', hitBig >= 5 && flowBig >= 1 && taskBig >= 1, `hit=${hitBig} flow=${flowBig} task=${taskBig}`)
  A('S2-sim-big-trace', '大额 trace 含 director(总监分支) + 可达结束', /director/.test(resBig) && /可达结束/.test(resBig), resBig.replace(/\s+/g, ' ').slice(0, 120))

  // S3 小额 → 排他网关走另一分支，director 不在路径
  await setVal('[data-sim-raw]', '{"amount":5000}')
  await clickSel('[data-sim-run]')
  await page.waitForTimeout(1800)
  const hitSmall = await countSel('.flow-sim-hit')
  const resSmall = (await textSel('.flow-sim-res')) || ''
  A('S3-sim-small-branch', '小额运行 → trace 无 director（排他网关另一分支）+ 仍可达结束', !/director/.test(resSmall) && /可达结束/.test(resSmall) && hitSmall >= 4, `hit=${hitSmall} res="${resSmall.replace(/\s+/g, ' ').slice(0, 90)}"`)

  // S4 切走模拟页签 → 高亮清除
  await clickSel('[data-ptab="node"]')
  await page.waitForTimeout(500)
  const hitAfterLeave = await countSel('.flow-sim-hit')
  A('S4-markers-cleared', '切走「模拟」页签 → 画布 sim 高亮清除', hitAfterLeave === 0, `hit=${hitAfterLeave}`)

  // ════════════════ 版本 diff ════════════════
  const loadedDD = await loadDef(DIFF_KEY, 's1')
  A('D0-load-diffdef', `载入 ${DIFF_KEY}（画布出 s1 图元）`, loadedDD, `shapes=${(await shapes()).length}`)

  // D1 「对比」按钮 → property 出差异面板 + 「差异」页签
  await clickSel('[data-act="diff"]')
  await page.waitForTimeout(700)
  const diffUi = await findOne('[data-diff-run]') && await findOne('[data-ptab="diff"]')
  A('D1-diff-open', '「对比」→ property 差异面板 + 「差异」页签出现', diffUi, '')

  // D2 选 v1 → v2，对比 → 差异列表：新增≥1 + 修改≥1
  await setVal('[data-diff-va]', '1')
  await setVal('[data-diff-vb]', '2')
  await clickSel('[data-diff-run]')
  for (let i = 0; i < 20; i++) { await page.waitForTimeout(300); if (await findOne('.flow-diff-row')) break }
  await page.waitForTimeout(300)
  const rowsAll = await countSel('.flow-diff-row')
  const rowsAdd = await countSel('.flow-diff-row.add')
  const rowsChg = await countSel('.flow-diff-row.chg')
  const sumTxt = (await textSel('.flow-sim-sum')) || ''
  await page.screenshot({ path: path.join(SHOTS, 'sd-02-diff.png'), fullPage: true })
  A('D2-diff-detect', 'v1→v2 结构差异：新增≥1(任务B/连线) + 修改≥1(任务A改/连线)', rowsAdd >= 1 && rowsChg >= 1, `rows=${rowsAll} add=${rowsAdd} chg=${rowsChg} sum="${sumTxt.replace(/\s+/g, ' ')}"`)

  // D3 同版本守卫
  await setVal('[data-diff-va]', '2')
  await setVal('[data-diff-vb]', '2')
  await clickSel('[data-diff-run]')
  await page.waitForTimeout(600)
  const errTxt = (await textSel('.flow-dialog-err')) || ''
  A('D3-diff-guard', '同版本守卫（va==vb → 报错）', /不同的版本/.test(errTxt), `err="${errTxt}"`)

  // D4 退出对比 → 页签消失 + 高亮清除
  await clickSel('[data-diff-exit]')
  await page.waitForTimeout(500)
  const tabGone = !(await findOne('[data-ptab="diff"]'))
  const diffMarks = await countSel('.flow-diff-add') + await countSel('.flow-diff-chg')
  A('D4-diff-exit', '退出对比 → 「差异」页签消失 + diff 高亮清除', tabGone && diffMarks === 0, `tabGone=${tabGone} marks=${diffMarks}`)

  // 噪声滤除（同 subflow_drilldown）
  const noise = (s) => /favicon/i.test(s) || /\/api\/registry\/dam/i.test(s) || /Failed to load resource/i.test(s)
  const realErrors = errors.filter((e) => !noise(e))
  A('T-noerr', '全程无 pageerror（favicon + 可选 DAM 注册表噪声已滤）', realErrors.length === 0, realErrors.slice(0, 3).join(' | ').slice(0, 300))

  await browser.close()
  try { srv.kill('SIGTERM') } catch {}
  try { fs.unlinkSync(harnessFile) } catch {}
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 设计器模拟+diff(门户级多区): ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'designer-simulate-diff-results.json'), JSON.stringify(results, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); try { fs.unlinkSync(path.join(WEB_DIR, '__sd_harness.html')) } catch {} process.exit(2) })
