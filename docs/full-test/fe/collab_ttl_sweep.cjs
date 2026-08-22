// 协同 · presence TTL sweep 专项（慢测 ~28s）。验证：停心跳超 TTL(25s) → 下次 roster 构建时被 sweep 掉。
// 与 collab_backend.cjs 同机制（原生 http SSE），单独跑因需真实等待 >25s。
const http = require('http')
const fs = require('fs')
const path = require('path')
const HOST = '127.0.0.1'; const PORT = 8091; const V1 = '/api/flow/v1'; const FLOW = '/api/flow'
const results = []
const A = (id, ok, desc, detail) => { results.push({ id, ok: !!ok, desc, detail: detail || '' }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`) }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
function post (p, body, user) { const h = { 'Content-Type': 'application/json' }; if (user) h['X-User'] = user; return fetch(`http://${HOST}:${PORT}${p}`, { method: 'POST', headers: h, body: JSON.stringify(body) }).then(async (r) => ({ status: r.status, json: await r.json().catch(() => null) })) }
function sseClient (defKey) {
  const events = []
  const req = http.request({ host: HOST, port: PORT, path: `${V1}/design/collab?defKey=${encodeURIComponent(defKey)}`, headers: { Accept: 'text/event-stream' } }, (res) => {
    res.setEncoding('utf8'); let buf = ''
    res.on('data', (chunk) => { buf += chunk; let idx; while ((idx = buf.indexOf('\n\n')) >= 0) { const raw = buf.slice(0, idx); buf = buf.slice(idx + 2); let event = null; let data = ''; for (const line of raw.split('\n')) { if (line.startsWith('event:')) event = line.slice(6).trim(); else if (line.startsWith('data:')) data += line.slice(5).trim() } if (event && event !== 'keep-alive') { let p = null; try { p = JSON.parse(data) } catch {} events.push({ event, data: p }) } } })
  })
  req.on('error', () => {}); req.end(); return { events, close: () => { try { req.destroy() } catch {} } }
}
const lastRoster = (cli) => { const e = [...cli.events].reverse().find((x) => x.event === 'presence'); return e ? (e.data.payload.roster || []) : [] }
function xml (id) { return `<?xml version="1.0" encoding="UTF-8"?><bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" id="D_${id}"><bpmn:process id="${id}" isExecutable="true"><bpmn:startEvent id="s1"/></bpmn:process></bpmn:definitions>` }

;(async () => {
  const DK = 'collab_ttl_' + Math.floor(process.pid % 9000 + 1000)
  await post(`${FLOW}/definitions/draft`, { name: DK, bpmnXml: xml(DK) }, 'seeder')
  await sleep(300)
  const cli = sseClient(DK); await sleep(500)

  // 两人 join。
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 'alive', selection: null }, 'u_alive')
  await post(`${V1}/design/presence/join`, { defKey: DK, sessionId: 'ghost', selection: null }, 'u_ghost')
  await sleep(600)
  const r0 = lastRoster(cli)
  A('T1-both-present', r0.length === 2, '两 session join → roster=2', `[${r0.map((p) => p.sessionId).join(',')}]`)

  // alive 每 8s 心跳保活；ghost 停更。等待 > TTL(25s)。
  console.log('  … 等待 28s（TTL=25s）期间 alive 保活、ghost 静默 …')
  await post(`${V1}/design/presence/heartbeat`, { defKey: DK, sessionId: 'alive' }, 'u_alive'); await sleep(8000)
  await post(`${V1}/design/presence/heartbeat`, { defKey: DK, sessionId: 'alive' }, 'u_alive'); await sleep(8000)
  await post(`${V1}/design/presence/heartbeat`, { defKey: DK, sessionId: 'alive' }, 'u_alive'); await sleep(8000)
  await sleep(4000)  // 累计 ~28s，ghost 最后心跳已 >25s 前
  // 触发一次 roster 重建（alive 心跳）→ sweep-on-build 剔 ghost。
  await post(`${V1}/design/presence/heartbeat`, { defKey: DK, sessionId: 'alive' }, 'u_alive')
  await sleep(1000)
  const rFinal = lastRoster(cli)
  const ghostGone = !rFinal.some((p) => p.sessionId === 'ghost')
  const aliveKept = rFinal.some((p) => p.sessionId === 'alive')
  A('T2-ghost-swept', ghostGone && aliveKept, '停心跳超 TTL → ghost 被 sweep，alive 保留', `[${rFinal.map((p) => p.sessionId).join(',')}]`)

  cli.close(); await sleep(200)
  const pass = results.filter((r) => r.ok).length
  console.log(`\n==== presence TTL sweep: ${pass}/${results.length} ====`)
  fs.writeFileSync(path.join(__dirname, 'collab-ttl-results.json'), JSON.stringify({ total: results.length, pass, results }, null, 2))
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
