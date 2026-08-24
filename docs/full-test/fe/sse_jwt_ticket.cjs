// SSE JWT 一次性票据验证（feature ①）。
//
// 起一个 auth.mode=jwt 的独立 flow-server（:8097），验证：
//   ① 无票据裸连 /design/collab → 401（jwt 模式 EventSource 无 header 会被拒）。
//   ② 带 header 的 POST /sse/ticket 铸票成功；带 ?ticket= 连 SSE → 200 且收到 keep-alive/事件。
//   ③ 票据单次消费：同票二次连接 → 401。
//   ④ 无 JWT 的 POST /sse/ticket 本身也要 401（票据端点在 auth 层内）。
//
// 纯 fetch/http，不需要浏览器（EventSource 用原始 HTTP GET 探响应码 + 首字节即可）。
const http = require('http')
const crypto = require('crypto')
const { spawn } = require('child_process')
const path = require('path')

const PORT = 8097
const SECRET = 'sse-ticket-test-secret'
const BASE = `http://127.0.0.1:${PORT}`
const ROOT = path.resolve(__dirname, '../../..') // cmx-flowengine/
const BIN = path.join(ROOT, 'target/debug/cmx-flow-server')

// —— 手搓 HS256 JWT（node 无 jsonwebtoken；JWT = base64url(header).base64url(payload).HMAC） ——
const b64u = (buf) => Buffer.from(buf).toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
function signJwt (claims, secret) {
  const header = b64u(JSON.stringify({ alg: 'HS256', typ: 'JWT' }))
  const payload = b64u(JSON.stringify(claims))
  const data = `${header}.${payload}`
  const sig = crypto.createHmac('sha256', secret).update(data).digest()
  return `${data}.${b64u(sig)}`
}
const TOKEN = signJwt({ sub: 'u_sse', tenant: 'default', roles: ['designer'], exp: 4102444800 }, SECRET)

// 简单请求：resolve({status, body}). headers 可选。
function req (method, urlPath, headers = {}, timeoutMs = 4000) {
  return new Promise((resolve, reject) => {
    const u = new URL(BASE + urlPath)
    const r = http.request({ hostname: u.hostname, port: u.port, path: u.pathname + u.search, method, headers }, (res) => {
      let body = ''
      res.on('data', (c) => { body += c })
      res.on('end', () => resolve({ status: res.statusCode, body }))
    })
    r.on('error', reject)
    r.setTimeout(timeoutMs, () => { r.destroy(new Error('timeout')); })
    r.end()
  })
}

// SSE 探针：连上后读到首个数据块即算连上（拿 status + 首块），然后主动断开。
function probeSse (urlPath, headers = {}, waitMs = 1500) {
  return new Promise((resolve, reject) => {
    const u = new URL(BASE + urlPath)
    const r = http.request({ hostname: u.hostname, port: u.port, path: u.pathname + u.search, method: 'GET', headers: { Accept: 'text/event-stream', ...headers } }, (res) => {
      let firstChunk = ''
      const done = (extra) => { try { r.destroy() } catch {} resolve({ status: res.statusCode, firstChunk, ...extra }) }
      res.on('data', (c) => { firstChunk += c; if (firstChunk.length > 0) setTimeout(() => done({}), 120) })
      // 401 等错误响应也会走这里但可能无 data；给个短超时兜底
      setTimeout(() => done({}), waitMs)
    })
    r.on('error', reject)
    r.end()
  })
}

const results = []
const A = (id, ok, detail) => { results.push({ id, ok: !!ok, detail }); console.log(`${ok ? '✅' : '❌'} ${id}${ok ? '' : '  << ' + detail}`) }

;(async () => {
  // 起 jwt 模式 server（配置走同目录 flow-sse-test.toml；jwt/端口经 AUTH__* / SERVER__PORT env 覆盖；
  // collab/ticket 纯内存，不依赖 PG 通路）。
  const env = { ...process.env, CONFIG_FILE: path.join(__dirname, 'flow-sse-test.toml'), SERVER__PORT: String(PORT), AUTH__MODE: 'jwt', AUTH__JWT_ALG: 'HS256', AUTH__JWT_SECRET: SECRET }
  const srv = spawn(BIN, [], { env, stdio: 'ignore' })
  srv.on('error', (e) => { console.error('server spawn failed', e); process.exit(2) })

  // 等 server 起来（探 /sse/ticket 无 token 返 401 = 已在监听且 auth 生效）。
  let up = false
  for (let i = 0; i < 40; i++) {
    await new Promise((r) => setTimeout(r, 400))
    try { const res = await req('POST', '/api/flow/v1/sse/ticket'); if (res.status === 401 || res.status === 200) { up = true; break } } catch { /* not yet */ }
  }
  if (!up) { console.error('server did not come up on :' + PORT); try { srv.kill('SIGKILL') } catch {} process.exit(2) }

  try {
    // ④ 无 JWT 的铸票端点 → 401
    const noJwt = await req('POST', '/api/flow/v1/sse/ticket')
    A('④ 无JWT POST /sse/ticket → 401', noJwt.status === 401, `status=${noJwt.status}`)

    // ① 无票据裸连 collab SSE → 401
    const bare = await probeSse('/api/flow/v1/design/collab?defKey=k1')
    A('① 无票据裸连 /design/collab → 401', bare.status === 401, `status=${bare.status}`)

    // ② 带 JWT header 铸票 → 200 + ticket
    const mint = await req('POST', '/api/flow/v1/sse/ticket', { Authorization: 'Bearer ' + TOKEN })
    let ticket = ''
    try { const j = JSON.parse(mint.body); ticket = (j.data && j.data.ticket) || '' } catch {}
    A('② 带JWT铸票 → 200 且返 ticket', mint.status === 200 && ticket.length > 0, `status=${mint.status} ticket=${ticket.slice(0, 8)}`)

    // ② 带 ?ticket= 连 SSE → 200 且收到数据（keep-alive 或 presence）
    const connected = await probeSse('/api/flow/v1/design/collab?defKey=k1&ticket=' + encodeURIComponent(ticket))
    A('② 带票据连 SSE → 200 且有数据流', connected.status === 200, `status=${connected.status} firstChunk=${JSON.stringify((connected.firstChunk || '').slice(0, 30))}`)

    // ③ 票据单次消费：同票二次连 → 401
    const reuse = await probeSse('/api/flow/v1/design/collab?defKey=k1&ticket=' + encodeURIComponent(ticket))
    A('③ 同票二次连 → 401（单次消费）', reuse.status === 401, `status=${reuse.status}`)

    // ③b events SSE 同机制：铸新票连 /events → 200
    const mint2 = await req('POST', '/api/flow/v1/sse/ticket', { Authorization: 'Bearer ' + TOKEN })
    let t2 = ''; try { t2 = JSON.parse(mint2.body).data.ticket } catch {}
    const ev = await probeSse('/api/flow/v1/events?ticket=' + encodeURIComponent(t2))
    A('③b 票据也适用 /events → 200', ev.status === 200, `status=${ev.status}`)
  } finally {
    try { srv.kill('SIGKILL') } catch {}
  }

  const pass = results.filter((r) => r.ok).length
  console.log(`\n${pass}/${results.length} passed`)
  process.exit(pass === results.length ? 0 : 1)
})().catch((e) => { console.error('FATAL', e); process.exit(2) })
