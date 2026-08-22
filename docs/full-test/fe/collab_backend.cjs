// 协同功能 · 后端/API 层完整测试（presence 生命周期 + SSE 过滤 + 乐观锁防冲突 + M2 op 中继 + 身份）。
//
// 用原生 http 打 SSE（EventSource 不能带 header，但服务端 off 模式 tenant=default 恒定，故裸连即可），
// 捕获 presence / draft.saved / op 三类事件；用 fetch 打 POST 端点。只依赖 flow-server :8091（off 模式）。
//
// 分组：A presence 生命周期 | B SSE 过滤/隔离 | C 乐观锁防冲突 | D M2 op 中继 | E 身份/actor(off)
const http = require('http')
const fs = require('fs')
const path = require('path')

const HOST = '127.0.0.1'
const PORT = 8091
const V1 = '/api/flow/v1'
const FLOW = '/api/flow'
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok, desc, detail: detail || '' }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ── HTTP POST（fetch，注入可选 X-User）──
function post (p, body, user) {
  const headers = { 'Content-Type': 'application/json' }
  if (user) headers['X-User'] = user
  return fetch(`http://${HOST}:${PORT}${p}`, { method: 'POST', headers, body: JSON.stringify(body) })
    .then(async (r) => ({ status: r.status, json: await r.json().catch(() => null) }))
}
function get (p, user) {
  const headers = {}
  if (user) headers['X-User'] = user
  return fetch(`http://${HOST}:${PORT}${p}`, { headers }).then(async (r) => ({ status: r.status, json: await r.json().catch(() => null) }))
}

// ── SSE 客户端（原生 http，解析 event/data 帧，收进数组）──
function sseClient (defKey) {
  const events = []
  const req = http.request({
    host: HOST, port: PORT,
    path: `${V1}/design/collab?defKey=${encodeURIComponent(defKey)}`,
    headers: { Accept: 'text/event-stream' },
  }, (res) => {
    res.setEncoding('utf8')
    let buf = ''
    res.on('data', (chunk) => {
      buf += chunk
      let idx
      while ((idx = buf.indexOf('\n\n')) >= 0) {
        const raw = buf.slice(0, idx); buf = buf.slice(idx + 2)
        let event = null; let data = ''
        for (const line of raw.split('\n')) {
          if (line.startsWith('event:')) event = line.slice(6).trim()
          else if (line.startsWith('data:')) data += line.slice(5).trim()
        }
        if (event && event !== 'keep-alive') {
          let parsed = null; try { parsed = JSON.parse(data) } catch {}
          events.push({ event, data: parsed, at: events.length })
        }
      }
    })
  })
  req.on('error', () => {})
  req.end()
  return { events, close: () => { try { req.destroy() } catch {} } }
}
// 从捕获事件里筛某类 + 谓词，带轮询等待。
async function waitFor (client, pred, ms = 3000) {
  const t0 = Date.now()
  while (Date.now() - t0 < ms) {
    const hit = client.events.find(pred)
    if (hit) return hit
    await sleep(80)
  }
  return null
}
const lastPresence = (client) => [...client.events].reverse().find((e) => e.event === 'presence')

// 存草稿（key 由 XML 的 process id 决定）。返回 {conflict, updatedAt|currentUpdatedAt, updatedBy}。
function draftXml (procId, taskName) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" id="Defs_${procId}" targetNamespace="http://cmx">
  <bpmn:process id="${procId}" name="${procId}" isExecutable="true">
    <bpmn:startEvent id="s1"><bpmn:outgoing>e1</bpmn:outgoing></bpmn:startEvent>
    <bpmn:userTask id="a1" name="${taskName || '任务A'}"><bpmn:incoming>e1</bpmn:incoming><bpmn:outgoing>e2</bpmn:outgoing></bpmn:userTask>
    <bpmn:endEvent id="en1"><bpmn:incoming>e2</bpmn:incoming></bpmn:endEvent>
    <bpmn:sequenceFlow id="e1" sourceRef="s1" targetRef="a1"/>
    <bpmn:sequenceFlow id="e2" sourceRef="a1" targetRef="en1"/>
  </bpmn:process>
  <bpmndi:BPMNDiagram id="di"><bpmndi:BPMNPlane id="pl" bpmnElement="${procId}">
    <bpmndi:BPMNShape id="s1_di" bpmnElement="s1"><dc:Bounds x="150" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="a1_di" bpmnElement="a1"><dc:Bounds x="240" y="78" width="100" height="80"/></bpmndi:BPMNShape>
    <bpmndi:BPMNShape id="en1_di" bpmnElement="en1"><dc:Bounds x="400" y="100" width="36" height="36"/></bpmndi:BPMNShape>
    <bpmndi:BPMNEdge id="e1_di" bpmnElement="e1"><di:waypoint x="186" y="118"/><di:waypoint x="240" y="118"/></bpmndi:BPMNEdge>
    <bpmndi:BPMNEdge id="e2_di" bpmnElement="e2"><di:waypoint x="340" y="118"/><di:waypoint x="400" y="118"/></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane></bpmndi:BPMNDiagram>
</bpmn:definitions>`
}
function saveDraft (procId, xml, baseUpdatedAt, user) {
  const body = { name: procId, bpmnXml: xml }
  if (baseUpdatedAt !== undefined) body.baseUpdatedAt = baseUpdatedAt
  return post(`${FLOW}/definitions/draft`, body, user).then((r) => r.json && r.json.data)
}

;(async () => {
  const DK = 'collab_be_' + Math.floor(process.pid % 9000 + 1000)  // 唯一 key，避免串测试
  const DK2 = DK + '_other'
  // 预置两个草稿。
  await saveDraft(DK, draftXml(DK), undefined, 'seeder')
  await saveDraft(DK2, draftXml(DK2), undefined, 'seeder')
  await sleep(300)

  // ═══════════ A. presence 生命周期 ═══════════
  const cli = sseClient(DK)
  await sleep(500)  // SSE 建流

  // A1: session s1 join → SSE presence roster 含 s1（user=u_alice）
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 's1', selection: 'a1' }, 'u_alice')
  let ev = await waitFor(cli, (e) => e.event === 'presence' && (e.data.payload.roster || []).some((p) => p.sessionId === 's1'))
  const s1 = ev && ev.data.payload.roster.find((p) => p.sessionId === 's1')
  A('A1-join-roster', !!s1 && s1.user === 'u_alice', 'join s1 → SSE presence roster 含 s1(user=u_alice)', s1 ? `user=${s1.user} sel=${s1.selection} color=${s1.color}` : '(未收到)')

  // A2: 第二 session s2 join → roster 含两人
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 's2', selection: null }, 'u_bob')
  ev = await waitFor(cli, (e) => e.event === 'presence' && (e.data.payload.roster || []).length >= 2)
  const roster2 = ev ? ev.data.payload.roster : []
  A('A2-two-sessions', roster2.length >= 2, '第二 session join → roster 含两人', `roster=[${roster2.map((p) => p.sessionId + ':' + p.user).join(', ')}]`)

  // A3: s1 select a1→en1 → roster 里 s1.selection 更新
  await post(`${V1}/design/presence/select`, { defKey: DK, sessionId: 's1', selection: 'en1' }, 'u_alice')
  ev = await waitFor(cli, (e) => e.event === 'presence' && (e.data.payload.roster || []).some((p) => p.sessionId === 's1' && p.selection === 'en1'))
  A('A3-select-update', !!ev, 's1 改选中 en1 → roster 反映新 selection', ev ? 'selection=en1' : '(未更新)')

  // A4: 心跳刷新（不改 selection 也应保活；改 selection 应广播）
  const before = cli.events.length
  await post(`${V1}/design/presence/heartbeat`, { defKey: DK, sessionId: 's1', selection: 's1' }, 'u_alice')
  ev = await waitFor(cli, (e) => e.at >= before && e.event === 'presence' && (e.data.payload.roster || []).some((p) => p.sessionId === 's1' && p.selection === 's1'))
  A('A4-heartbeat-selupdate', !!ev, '心跳携新 selection → 广播更新（保活+选中同步）', ev ? 'ok' : '(未广播)')

  // A5: s2 leave → roster 只剩 s1
  await post(`${V1}/design/presence/leave`, { defKey: DK, sessionId: 's2' }, 'u_bob')
  ev = await waitFor(cli, (e) => e.event === 'presence' && !(e.data.payload.roster || []).some((p) => p.sessionId === 's2'))
  const rosterAfter = ev ? ev.data.payload.roster : []
  A('A5-leave', !!ev && rosterAfter.length === 1 && rosterAfter[0].sessionId === 's1', 's2 leave → roster 只剩 s1', `roster=[${rosterAfter.map((p) => p.sessionId).join(', ')}]`)

  // ═══════════ B. SSE 过滤 / 隔离 ═══════════
  // B1: 另开 SSE 订阅 DK2；对 DK 的 join 不应被 DK2 订阅者收到（按 defKey 过滤）
  const cliOther = sseClient(DK2)
  await sleep(500)
  const otherBefore = cliOther.events.length
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 's3', selection: null }, 'u_carol')
  await sleep(1000)
  const leakedToOther = cliOther.events.slice(otherBefore).some((e) => e.event === 'presence' && (e.data.payload.roster || []).some((p) => p.sessionId === 's3'))
  A('B1-defkey-isolation', !leakedToOther, 'DK 的 presence 不泄漏给 DK2 订阅者（按 defKey 过滤）', leakedToOther ? '泄漏!' : '隔离正确')

  // B2: presence 事件字段完整（sessionId/user/color；tenant/defKey 顶层）
  const anyPres = lastPresence(cli)
  const p0 = anyPres && (anyPres.data.payload.roster || [])[0]
  const fieldsOk = anyPres && anyPres.data.tenant === 'default' && anyPres.data.defKey === DK && p0 && p0.sessionId && p0.user && p0.color
  A('B2-event-shape', !!fieldsOk, 'presence 事件字段完整（tenant/defKey 顶层 + roster 项 sessionId/user/color）', anyPres ? `tenant=${anyPres.data.tenant} defKey=${anyPres.data.defKey} p0.color=${p0 && p0.color}` : '(无)')

  // ═══════════ C. 乐观锁防冲突 ═══════════
  // 取当前 updatedAt 作 base
  const det0 = await get(`${FLOW}/definitions/${DK}`)
  const base0 = det0.json && det0.json.data && det0.json.data.updatedAt
  // C1: 用正确 base 存 → Saved（conflict:false），推进 updatedAt
  const c1 = await saveDraft(DK, draftXml(DK, '改动1'), base0, 'u_alice')
  A('C1-save-ok', c1 && c1.conflict === false && c1.updatedAt && c1.updatedAt !== base0, '正确 base 保存 → Saved(conflict:false) + updatedAt 推进', c1 ? `conflict=${c1.conflict} new=${(c1.updatedAt || '').slice(11, 19)}` : '(无)')
  const base1 = c1 && c1.updatedAt

  // C2: 用过期 base（base0）再存 → Conflict(conflict:true) + currentUpdatedAt + updatedBy
  const c2 = await saveDraft(DK, draftXml(DK, '改动2'), base0, 'u_bob')
  A('C2-stale-conflict', c2 && c2.conflict === true && c2.currentUpdatedAt, '过期 base 保存 → Conflict(conflict:true) + currentUpdatedAt', c2 ? `conflict=${c2.conflict} current=${(c2.currentUpdatedAt || '').slice(11, 19)} by=${c2.updatedBy}` : '(无)')

  // C3: base=null（不传）→ 无条件 Saved（向后兼容/单人）
  const c3 = await saveDraft(DK, draftXml(DK, '改动3'), undefined, 'u_alice')
  A('C3-null-base-force', c3 && c3.conflict === false, 'base=null → 无条件 Saved（向后兼容单人路径）', c3 ? `conflict=${c3.conflict}` : '(无)')

  // C4: 成功保存广播 draft.saved SSE 事件（含 updatedAt；user）
  const beforeSave = cli.events.length
  const base3 = c3 && c3.updatedAt
  const c4 = await saveDraft(DK, draftXml(DK, '改动4'), base3, 'u_alice')
  ev = await waitFor(cli, (e) => e.at >= beforeSave && e.event === 'draft.saved')
  A('C4-draft-saved-sse', !!ev && ev.data.payload && ev.data.payload.updatedAt, '成功保存 → SSE draft.saved 事件(含 updatedAt+user)', ev ? `user=${ev.data.user} at=${(ev.data.payload.updatedAt || '').slice(11, 19)}` : '(未收到)')

  // ═══════════ D. M2 op 中继（对象级合并） ═══════════
  // D1: op 返回单调 seq（连发三次 → 1,2,3）
  const o1 = await post(`${V1}/design/op`, { defKey: DK, sessionId: 's1', op: 'updateProperties', elementId: 'a1', props: { name: 'x1' } }, 'u_alice')
  const o2 = await post(`${V1}/design/op`, { defKey: DK, sessionId: 's1', op: 'updateProperties', elementId: 'a1', props: { name: 'x2' } }, 'u_alice')
  const o3 = await post(`${V1}/design/op`, { defKey: DK, sessionId: 's1', op: 'updateProperties', elementId: 'a1', props: { name: 'x3' } }, 'u_alice')
  const seqs = [o1, o2, o3].map((o) => o.json && o.json.data && o.json.data.seq)
  const mono = seqs[0] < seqs[1] && seqs[1] < seqs[2]
  A('D1-monotonic-seq', mono, 'op 连发 → 服务端盖单调递增 seq', `seqs=[${seqs.join(',')}]`)

  // D2: seq 按 defKey 隔离（DK2 的 op 从更小/独立序列起）
  const oOther = await post(`${V1}/design/op`, { defKey: DK2, sessionId: 'sx', op: 'updateProperties', elementId: 'a1', props: { name: 'y' } }, 'u_alice')
  const seqOther = oOther.json && oOther.json.data && oOther.json.data.seq
  A('D2-seq-isolation', typeof seqOther === 'number' && seqOther < seqs[2], 'op seq 按 defKey 独立计数（DK2 独立序列）', `DK末=${seqs[2]} DK2=${seqOther}`)

  // D3: op 广播 SSE op 事件（payload 含 seq/origin/op/elementId/props）
  const beforeOp = cli.events.length
  await post(`${V1}/design/op`, { defKey: DK, sessionId: 's1', op: 'updateProperties', elementId: 'a1', props: { name: '最终名', 'flowable:assignee': 'mgr' } }, 'u_alice')
  ev = await waitFor(cli, (e) => e.at >= beforeOp && e.event === 'op')
  const opl = ev && ev.data.payload
  const opOk = opl && typeof opl.seq === 'number' && opl.origin === 's1' && opl.op === 'updateProperties' && opl.elementId === 'a1' && opl.props && opl.props.name === '最终名'
  A('D3-op-sse-broadcast', !!opOk, 'op 广播 SSE op 事件(payload seq/origin/op/elementId/props 完整)', opl ? `seq=${opl.seq} origin=${opl.origin} el=${opl.elementId} name=${opl.props && opl.props.name}` : '(未收到)')

  // D4: op 事件 origin = 发送方 sessionId（供发送端 echo-guard 忽略自己）
  A('D4-origin-echo-guard', opl && opl.origin === 's1', 'op 事件带 origin=发送方 sessionId（echo-guard 依据）', opl ? `origin=${opl.origin}` : '(无)')

  // D5: props 支持删除语义（null 值）
  const beforeOp2 = cli.events.length
  await post(`${V1}/design/op`, { defKey: DK, sessionId: 's1', op: 'updateProperties', elementId: 'a1', props: { 'flowable:assignee': null } }, 'u_alice')
  ev = await waitFor(cli, (e) => e.at >= beforeOp2 && e.event === 'op')
  const delOk = ev && ev.data.payload.props && Object.prototype.hasOwnProperty.call(ev.data.payload.props, 'flowable:assignee') && ev.data.payload.props['flowable:assignee'] === null
  A('D5-prop-delete-null', !!delOk, 'op props 支持 null（远端据此删属性）', delOk ? 'null 透传' : '(未透传 null)')

  // ═══════════ E. 身份 / actor（off 模式） ═══════════
  // E1: X-User header 定 actor（A1 已证 user=u_alice）——此处证 body.user 兜底
  const cliE = sseClient(DK)
  await sleep(400)
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 'sE', selection: null, user: 'body_user' })  // 无 X-User，靠 body.user
  ev = await waitFor(cliE, (e) => e.event === 'presence' && (e.data.payload.roster || []).some((p) => p.sessionId === 'sE'))
  const sE = ev && ev.data.payload.roster.find((p) => p.sessionId === 'sE')
  A('E1-body-user-fallback', sE && sE.user === 'body_user', '无 X-User → body.user 兜底定 actor', sE ? `user=${sE.user}` : '(无)')

  // E2: 同 user 派生同色（跨会话稳定）
  const aliceColors = new Set()
  for (const e of cli.events) {
    if (e.event === 'presence') for (const p of (e.data.payload.roster || [])) if (p.user === 'u_alice') aliceColors.add(p.color)
  }
  A('E2-stable-color', aliceColors.size === 1, '同 user 跨事件派生同一稳定色', `u_alice 色集=${[...aliceColors].join(',')}`)

  // E3: color 取自 CVD-安全调色板（8 色之一）
  const PALETTE = ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#4a3aa7', '#008300', '#e34948']
  const colorInPalette = [...aliceColors].every((c) => PALETTE.includes(c))
  A('E3-cvd-palette', colorInPalette && aliceColors.size >= 1, 'presence 色取自 CVD-安全 8 色板', `色∈板=${colorInPalette}`)

  cli.close(); cliOther.close(); cliE.close()
  await sleep(200)

  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== 协同后端/API 层: ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'collab-backend-results.json'), JSON.stringify({ defKey: DK, total: results.length, pass, results }, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
