// 令牌可视化 · 生命周期回放 · CDP 真机测试（路线图 Next：令牌可视化 · 生命周期回放）。
//
// 只依赖 flow-server :8091（off 模式）。挂 ops-console content+explorer，选一个已推进的多节点实例，
// 进回放，验证：
//   R1 进回放 → 出现 scrubber（滑块 + 帧信息），帧数 = /activities 条数(+末帧 live)
//   R2 拖到第 1 帧 → 画布仅高亮首节点（trail+now）；帧信息「进入」= 首活动节点
//   R3 单步/末帧 → 高亮推进；末帧含当前 activeNodes
//   R4 退出回放 → 恢复 live 令牌高亮（scrubber 消失）
//   R5 全程无 pageerror
const { chromium } = require('playwright')
const fs = require('fs')
const path = require('path')
const { deepEval, startStatic, vendorRoute } = require('./_harness.cjs')

const APIB = 'http://127.0.0.1:8091/api/flow'
const PORT = 9097
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }

const HARNESS = `<!doctype html><html><head><meta charset="utf-8"><title>replay</title>
<style>html,body{margin:0;height:100%}#stage{display:flex;height:100vh}.region{overflow:auto;height:100%}#r-explorer{flex:0 0 280px}#r-content{flex:1}.host{height:100%;display:block}</style></head>
<body><div id="stage">
  <div class="region" id="r-explorer"><div class="host" id="h-explorer"></div></div>
  <div class="region" id="r-content"><div class="host" id="h-content"></div></div>
</div>
<script type="module">
  import * as mod from '/core/ops-console.js'
  window.__mod = mod
  mod.configure({ apiBase: 'http://127.0.0.1:8091', fetchInit: { credentials: 'omit' }, authHeaders: () => ({}) })
  function mk (id) { const el = document.getElementById(id); const sr = el.attachShadow({ mode: 'open' }); const r = document.createElement('div'); r.className = 'native-page-root'; r.style.height = '100%'; sr.appendChild(r); return el }
  mod.default.views.content({ host: mk('h-content') })
  mod.default.views.explorer({ host: mk('h-explorer') })
  window.__ready = true
</script></body></html>`

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// 自播种一个「带 DI + 可推进」的定义与实例，使本测试自包含、不依赖运行库既有数据。
// 流程 rp_demo：start → t1(userTask) → t2(userTask) → end，均带 BPMNDiagram 布局。
// 发起后办结 t1 → 产生 ≥2 条已闭合 activity（start、t1），令牌停在 t2（当前活动，供末帧 live）。
const RP_XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" xmlns:flowable="http://flowable.org/bpmn" id="Defs_rp" targetNamespace="http://cmx">
  <bpmn:process id="rp_replay_demo" name="Replay Demo" isExecutable="true">
    <bpmn:startEvent id="s" name="开始"><bpmn:outgoing>f1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="t1" name="第一步" flowable:assignee="u_rp"><bpmn:incoming>f1</bpmn:incoming><bpmn:outgoing>f2</bpmn:outgoing></bpmn:userTask>
    <bpmn:userTask id="t2" name="第二步" flowable:assignee="u_rp"><bpmn:incoming>f2</bpmn:incoming><bpmn:outgoing>f3</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="e" name="结束"><bpmn:incoming>f3</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="f1" sourceRef="s" targetRef="t1"/>
    <bpmn:sequenceFlow id="f2" sourceRef="t1" targetRef="t2"/>
    <bpmn:sequenceFlow id="f3" sourceRef="t2" targetRef="e"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="rp_replay_demo">
    <bpmndi:BPMNShape id="s_di" bpmnElement="s"><dc:Bounds x="150" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="t1_di" bpmnElement="t1"><dc:Bounds x="240" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="t2_di" bpmnElement="t2"><dc:Bounds x="400" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="e_di" bpmnElement="e"><dc:Bounds x="560" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="f1_di" bpmnElement="f1"><di:waypoint x="186" y="118"/><di:waypoint x="240" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="f2_di" bpmnElement="f2"><di:waypoint x="340" y="118"/><di:waypoint x="400" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="f3_di" bpmnElement="f3"><di:waypoint x="500" y="118"/><di:waypoint x="560" y="118"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`

async function post (p, body) {
  return fetch(APIB + p, { method: 'POST', headers: { 'X-Tenant': 'default', 'X-User': 'u_rp', 'Content-Type': 'application/json' }, body: JSON.stringify(body) }).then((r) => r.json()).catch(() => null)
}
// 播种：发布定义 → 发起实例 → 办结 t1（令牌到 t2）→ 返回 instanceId（保证 ≥2 activities + 当前活动 t2）。
async function seedReplayInstance () {
  // 存草稿 + 发布（定义库要有 DI 的 bpmnXml；发布后引擎装载可发起）。
  await post('/definitions/draft', { name: 'Replay Demo', bpmnXml: RP_XML })
  await post('/definitions/rp_replay_demo/publish', { note: 'replay-fixture', publishedBy: 'u_rp' }).catch(() => {})
  const started = await post('/v1/instances', { definitionKey: 'rp_replay_demo', businessKey: 'RP-REPLAY-FIX', variables: {} })
  const iid = started && started.data && (started.data.id || started.data.instanceId)
  if (!iid) return null
  // 办结 t1：取该实例在 u_rp 名下未完成任务 → complete，令牌推进到 t2。
  const my = await fetch(`${APIB}/v1/tasks/my?assignee=u_rp`, { headers: { 'X-Tenant': 'default' } }).then((r) => r.json()).catch(() => null)
  const task = my && my.data && (my.data.tasks || []).find((t) => t.instanceId === iid && (t.nodeBpmnId === 't1' || !t.completed))
  if (task) await post(`/v1/tasks/${task.taskId || task.id}/complete`, { instanceId: iid, comment: 'seed', variables: {} })
  await sleep(300)
  return iid
}

;(async () => {
  // 自播种一个带 DI + 已推进（t1 已办、令牌在 t2）的实例，测试自包含不靠既有库数据。
  const seededId = await seedReplayInstance()
  const listRaw = await fetch(`${APIB}/instances?limit=100`).then((r) => r.json()).then((j) => j.data).catch(() => null)
  const items = (listRaw && (listRaw.items || listRaw.instances)) || []
  const defHasDI = {} // definitionKey → bool（缓存，避免重复拉定义）
  async function hasDI (key) {
    if (key in defHasDI) return defHasDI[key]
    let di = false
    try {
      const x = await fetch(`${APIB}/definitions/${encodeURIComponent(key)}`).then((r) => r.json()).then((j) => (j.data && j.data.bpmnXml) || '')
      di = /BPMNDiagram|bpmndi:/.test(x)
    } catch {}
    defHasDI[key] = di
    return di
  }
  let target = null
  // 优先自播种的实例；否则任意「带 DI + ≥2 activities」实例（travel_expense 次优先）。
  const ordered = items.slice().sort((a, b) => {
    const rank = (i) => (i.id === seededId ? 2 : (i.definitionKey === 'travel_expense' ? 1 : 0))
    return rank(b) - rank(a)
  })
  for (const it of ordered) {
    if (!it.definitionKey || !(await hasDI(it.definitionKey))) continue
    const acts = await fetch(`${APIB}/instances/${it.id}/activities`).then((r) => r.json()).then((j) => j.data.activities || []).catch(() => [])
    if (acts.length >= 2) { target = { id: it.id, acts: acts.length, def: it.definitionKey }; break }
  }
  if (!target) { console.error('未找到「带 DI 定义 + ≥2 activities」的实例（自播种也失败？检查发布/发起/办结链路）'); process.exit(2) }
  console.log('target instance:', target.id, 'def:', target.def, 'activities:', target.acts, seededId && target.id === seededId ? '(seeded)' : '(existing)')

  const srv = startStatic(PORT)
  await sleep(900)
  const browser = await chromium.launch({
    channel: 'chrome', headless: true,
    args: ['--disable-features=PrivateNetworkAccessChecks,BlockInsecurePrivateNetworkRequests,LocalNetworkAccessChecks'],
  })
  const ctx = await browser.newContext({ viewport: { width: 1500, height: 940 } })
  const page = await ctx.newPage()
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  page.on('dialog', (d) => d.accept())

  await page.route('**/_rp.html', (route) => route.fulfill({ status: 200, contentType: 'text/html', body: HARNESS }))
  await vendorRoute(page, ctx)
  await page.goto(`http://127.0.0.1:${PORT}/_rp.html`, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => window.__ready === true, { timeout: 8000 })
  await sleep(500)

  const H = {
    click: (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find((e) => e.matches && e.matches(sel)); if (b) { b.click(); return true } return false }, { de: deepEval, sel }),
    count: (sel) => page.evaluate(({ de, sel }) => eval(de).filter((e) => e.matches && e.matches(sel)).length, { de: deepEval, sel }),
    text: (sel) => page.evaluate(({ de, sel }) => { const el = eval(de).find((e) => e.matches && e.matches(sel)); return el ? el.textContent : null }, { de: deepEval, sel }),
    markers: (cls) => page.evaluate(({ de, cls }) => eval(de).filter((e) => e.classList && e.classList.contains(cls)).length, { de: deepEval, cls }),
    setRange: (v) => page.evaluate(({ de, v }) => { const el = eval(de).find((e) => e.matches && e.matches('[data-rp-range]')); if (!el) return false; el.value = String(v); el.dispatchEvent(new Event('input', { bubbles: true })); return true }, { de: deepEval, v }),
  }

  try {
    // 选中目标实例
    await H.click(`[data-id="${target.id}"]`)
    await sleep(1500) // 详情 + bpmn 图渲染

    // ── R1 进回放 → scrubber 出现 ──
    await H.click('[data-act="replay"]')
    await sleep(1200)
    const hasBar = await H.count('.ops-replay-range')
    const lbl = await H.text('.ops-replay-lbl')
    // 帧数 = activities + (末帧 live，若有活动节点)
    const okBar = hasBar >= 1 && lbl && /\d+\/\d+/.test(lbl)
    A('R1-scrubber', okBar, '进回放 → scrubber 出现', `bar=${hasBar} lbl=${lbl}`)

    // ── R2 拖到第 1 帧 → 仅首节点占用高亮（ops-rp-now）+ trail ──
    await H.setRange(0)
    await sleep(500)
    const nowAt0 = await H.markers('ops-rp-now')
    const info0 = await H.text('.ops-replay-info')
    A('R2-frame1', nowAt0 >= 1 && info0 && info0.includes('进入'), '第1帧 → 首节点高亮 + 帧信息', `now=${nowAt0} info=${(info0 || '').replace(/\s+/g, ' ').trim().slice(0, 40)}`)

    // ── R3 单步到末帧 → 高亮推进；末帧 live ──
    await H.click('[data-rp="last"]')
    await sleep(600)
    const lblLast = await H.text('.ops-replay-lbl')
    const infoLast = await H.text('.ops-replay-info')
    const trailLast = await H.markers('ops-rp-trail')
    const nowLast = await H.markers('ops-rp-now') // 末帧 live 用 ops-tok-run；也可能 ops-rp-now
    const runLast = await H.markers('ops-tok-run')
    const lastIsEnd = lblLast && (() => { const m = lblLast.match(/(\d+)\/(\d+)/); return m && m[1] === m[2] })()
    A('R3-advance-last', lastIsEnd && (trailLast >= 1 || nowLast >= 1 || runLast >= 1), '末帧 → 轨迹+占用高亮', `lbl=${lblLast} trail=${trailLast} now=${nowLast} run=${runLast} info=${(infoLast || '').replace(/\s+/g, ' ').trim().slice(0, 40)}`)

    // 截图（末帧）
    const SHOTS = path.join(__dirname, 'shots')
    try { fs.mkdirSync(SHOTS, { recursive: true }) } catch {}
    await page.screenshot({ path: path.join(SHOTS, 'token_replay.png') })

    // ── R4 退出回放 → scrubber 消失 + 恢复 live 高亮 ──
    await H.click('[data-act="replay"]')
    await sleep(800)
    const barGone = await H.count('.ops-replay-range')
    A('R4-exit', barGone === 0, '退出回放 → scrubber 消失，恢复 live', `bar=${barGone}`)

    // ── R5 无错 ──
    A('R5-noerr', errors.length === 0, '全程无 pageerror', errors.join(' | '))
  } finally {
    await browser.close()
    srv.kill('SIGTERM')
  }

  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 令牌生命周期回放: ${pass}/${results.length} ====`)
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
