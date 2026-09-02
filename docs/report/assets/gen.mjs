// gen.mjs — 生成「cmx-flowengine 实现方案与开源全景对比」报告的全部图。
// 自包含浅色卡片 SVG，复用阶段总结验证过的 CVD-安全调色板与 helper。
// 用法: node docs/report/assets/gen.mjs  → 写出 fig-*.svg
import { writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const DIR = dirname(fileURLToPath(import.meta.url))

// ── 调色板（dataviz CVD-安全，浅色卡面）──
const P = {
  surface: '#fcfcfb', plane: '#f4f4f1', ink: '#0b0b0b', ink2: '#52514e', muted: '#898781',
  grid: '#e1e0d9', base: '#c3c2b7', border: 'rgba(11,11,11,0.12)',
  blue: '#2a78d6', orange: '#eb6834', aqua: '#1baf7a', yellow: '#eda100',
  magenta: '#e87ba4', green: '#008300', violet: '#4a3aa7', red: '#e34948',
  good: '#0ca30c', warning: '#fab219', serious: '#ec835a', critical: '#d03b3b',
  blue100: '#cde2fb', blue550: '#1c5cab',
}
const FONT = "system-ui,-apple-system,'Segoe UI','PingFang SC','Hiragino Sans GB','Microsoft YaHei',sans-serif"
const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
const wOf = (s, per = 11) => [...String(s)].reduce((a, c) => a + (/[\x00-\xff]/.test(c) ? per * 0.58 : per), 0)

const T = (x, y, s, o = {}) => {
  const { size = 13, w = 400, fill = P.ink, anchor = 'start', op = 1, mono = false } = o
  return `<text x="${x}" y="${y}" font-family="${FONT}" font-size="${size}" font-weight="${w}" fill="${fill}" text-anchor="${anchor}" opacity="${op}"${mono ? ' font-variant-numeric="tabular-nums"' : ''}>${esc(s)}</text>`
}
const R = (x, y, w, h, o = {}) => {
  const { rx = 10, fill = 'none', stroke = 'none', sw = 1, fop = 1, sop = 1 } = o
  return `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${rx}" fill="${fill}" fill-opacity="${fop}" stroke="${stroke}" stroke-opacity="${sop}" stroke-width="${sw}"/>`
}
const LINE = (x1, y1, x2, y2, o = {}) => {
  const { stroke = P.muted, sw = 1.5, dash = '', marker = true } = o
  return `<line x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}" stroke="${stroke}" stroke-width="${sw}"${dash ? ` stroke-dasharray="${dash}"` : ''}${marker ? ' marker-end="url(#arr)"' : ''}/>`
}
const card = (w, h) => R(0, 0, w, h, { rx: 16, fill: P.surface, stroke: P.border, sw: 1 })
const defs = `<defs>
  <marker id="arr" markerWidth="9" markerHeight="9" refX="6.5" refY="3" orient="auto"><path d="M0,0 L6.5,3 L0,6 Z" fill="${P.muted}"/></marker>
</defs>`
const doc = (w, h, body) => `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img">${defs}${card(w, h)}${body}</svg>`

const band = (x, y, w, h, hue, title, sub, loc) => {
  let s = R(x, y, w, h, { rx: 10, fill: hue, fop: 0.10, stroke: hue, sop: 0.32, sw: 1 })
  s += R(x, y, 4, h, { rx: 2, fill: hue })
  s += T(x + 18, y + (sub ? h / 2 - 4 : h / 2 + 5), title, { size: 14.5, w: 700 })
  if (sub) s += T(x + 18, y + h / 2 + 15, sub, { size: 11.5, fill: P.ink2 })
  if (loc) s += T(x + w - 14, y + h / 2 + 5, loc, { size: 12, fill: P.muted, anchor: 'end', mono: true })
  return s
}
const cell = (x, y, w, h, hue, title, sub) => {
  let s = R(x, y, w, h, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
  s += R(x, y, 4, h, { rx: 2, fill: hue })
  s += T(x + w / 2 + 2, y + (sub ? h / 2 - 2 : h / 2 + 4), title, { size: 12.5, w: 700, anchor: 'middle' })
  if (sub) s += T(x + w / 2 + 2, y + h / 2 + 13, sub, { size: 10, fill: P.ink2, anchor: 'middle' })
  return s
}
const chip = (x, y, label, hue, o = {}) => {
  const { size = 11, pad = 11, h = 22 } = o
  const w = Math.round(wOf(label, size) + pad * 2)
  let s = R(x, y, w, h, { rx: h / 2, fill: hue, fop: 0.13, stroke: hue, sop: 0.34, sw: 1 })
  s += `<circle cx="${x + pad - 2}" cy="${y + h / 2}" r="3" fill="${hue}"/>`
  s += T(x + pad + 5, y + h / 2 + 4, label, { size, fill: P.ink })
  return { svg: s, w }
}
const chipFlow = (x0, y0, maxX, items, hue, o = {}) => {
  const { gap = 7, lh = 28 } = o
  let x = x0, y = y0, out = ''
  for (const it of items) {
    const c = chip(x, y, it, hue, o)
    if (x + c.w > maxX && x > x0) { x = x0; y += lh; }
    const c2 = chip(x, y, it, hue, o)
    out += c2.svg; x += c2.w + gap
  }
  return { svg: out, height: y + lh - y0 }
}
const titleBlk = (w, s, sub) => T(w / 2, 34, s, { size: 19, w: 800, anchor: 'middle' }) +
  (sub ? T(w / 2, 54, sub, { size: 12.5, fill: P.ink2, anchor: 'middle' }) : '')

// 状态格：✓ 已交付 / ! 部分 / – 暂无。图标+文字，绝不靠色区分。
const ST = { ok: P.good, warn: P.warning, no: P.muted }
const GLY = { ok: '✓', warn: '!', no: '–' }
const statusCell = (x, y, label, st, wFixed) => {
  const hue = ST[st], h = 24, w = wFixed || Math.round(wOf(label, 11.5) + 40)
  let s = R(x, y, w, h, { rx: 7, fill: hue, fop: st === 'no' ? 0.07 : 0.13, stroke: hue, sop: st === 'no' ? 0.3 : 0.36, sw: 1 })
  s += `<circle cx="${x + 13}" cy="${y + h / 2}" r="7.5" fill="${hue}"/>`
  s += T(x + 13, y + h / 2 + 4, GLY[st], { size: 11, w: 800, anchor: 'middle', fill: '#fff' })
  s += T(x + 27, y + h / 2 + 4.5, label, { size: 11.5, fill: st === 'no' ? P.ink2 : P.ink })
  return { svg: s, w }
}
const statusFlow = (x0, y0, maxX, items) => {
  const gap = 7, lh = 30
  let x = x0, y = y0, out = ''
  for (const [label, st] of items) {
    const probe = statusCell(x, y, label, st)
    if (x + probe.w > maxX && x > x0) { x = x0; y += lh }
    const c = statusCell(x, y, label, st)
    out += c.svg; x += c.w + gap
  }
  return { svg: out, height: y + lh - y0 }
}
// 迷你图标格（矩阵单元）：仅图标，居中，配 tooltip 文案由表格承担
const markCell = (cx, cy, st, r = 8.5) => {
  const hue = ST[st]
  let s = `<circle cx="${cx}" cy="${cy}" r="${r}" fill="${hue}" fill-opacity="${st === 'no' ? 0.16 : 1}" stroke="${hue}" stroke-opacity="0.5" stroke-width="1"/>`
  s += T(cx, cy + 4, GLY[st], { size: 11, w: 800, anchor: 'middle', fill: st === 'no' ? P.muted : '#fff' })
  return s
}

// ══════════════ 图·架构总览「一芯多壳」 ══════════════
function figArch () {
  const W = 940, H = 566
  const x = 40, w = W - 80
  let b = titleBlk(W, '总体架构 · 一芯多壳（One Core, Multi-Shell）',
    '12 crates · 25,012 域 LOC + 9,351 测试 LOC · edition 2024 · 工具链 1.97.1 · Apache-2.0 · 语义中立内核零框架/零平台依赖')
  const shW = (w - 2 * 16) / 3
  b += cell(x, 74, shW, 46, P.violet, '① 独立 server bin', 'cmx-flow-server · :8091')
  b += cell(x + shW + 16, 74, shW, 46, P.violet, '② 平台反代壳', 'cmx-flow-api · 门户内嵌(HTTP)')
  b += cell(x + 2 * (shW + 16), 74, shW, 46, P.violet, '③ 可嵌 Web Component', 'web/elements · 框架无关')
  b += LINE(W / 2, 120, W / 2, 140)
  b += band(x, 142, w, 50, P.blue, 'cmx-flow-app · 平台中立应用核',
    'flow_routes::<S>() 泛型路由 + 引擎单例 + 全 handler + 模拟/协同/决策端点 + 身份/维度回连', '6,926')
  b += LINE(W / 2, 192, W / 2, 210)
  const iW = (w - 3 * 12) / 4
  const inj = [['cmx-flow-store-pg', 'PG 持久化 · 3,081', P.aqua], ['cmx-flow-adapters', 'HTTP/Mock 注入 · 1,004', P.aqua],
    ['cmx-flow-identity', '内置身份 fid_* · 533', P.aqua], ['cmx-flow-def', '定义持久化 · 887', P.aqua]]
  inj.forEach((it, i) => { b += cell(x + i * (iW + 12), 212, iW, 44, it[2], it[0], it[1]) })
  b += LINE(W / 2, 256, W / 2, 274)
  b += band(x, 276, w, 48, P.blue, 'cmx-flow-engine · 令牌执行内核 Engine<S: RuntimeStore>',
    '等待态即提交点（pause+persist）· run_to_wait 步进 · JavaDelegate/DelegateRegistry · SKIP-LOCKED · InMemoryStore', '5,016')
  b += LINE(W / 2, 324, W / 2, 340)
  b += band(x, 342, w, 44, P.orange, 'cmx-flow-bpmn · BPMN 2.0 XML → 中立 IR 编译器',
    'compile(xml) roxmltree 前缀无关 · 不支持元素编译期显式报错（绝不静默降级）', '1,551')
  b += LINE(W / 2, 386, W / 2, 402)
  b += band(x, 404, w, 48, P.violet, 'cmx-flow-model · 语义中立内核（IR + 运行态 + 7 trait 契约）',
    'ProcessDefinition/FlowNode/Token/19 NodeKind · 自研 ${} 表达式求值器 · VarChangeRecord · 可 wasm/嵌入', '4,474')
  b += R(x, 466, w, 62, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x + 16, 488, '单向借用 cmx-container 基础库（仅编译期 path 依赖，无反向引用；cargo tree 已验证门户不依赖引擎源码）：', { size: 11.5, w: 700, fill: P.ink2 })
  b += T(x + 16, 508, 'cmx-database-pg · cmx-core · cmx-web-chassis · cmx-web-monitor · cmx-service-base', { size: 11.5, fill: P.muted, mono: true })
  b += T(x + 16, 524, '7 可注入 trait：RuntimeStore · JavaDelegate · Clock · AssigneeResolver · SubflowRouter · DimensionResolver · DefinitionStore（mock/http/pg 三模）', { size: 10.5, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图·BPMN 2.0 能力覆盖矩阵 ══════════════
function figBpmn () {
  const W = 980
  const sections = [
    ['事件 Events', P.blue, [
      ['开始·空', 'ok'], ['开始·消息', 'ok'], ['开始·错误(事件子过程)', 'ok'], ['开始·定时/信号/条件', 'no'],
      ['结束·空', 'ok'], ['结束·终止(一票否决)', 'ok'], ['抛结束·错误/升级/消息', 'no'],
      ['中间捕获·定时', 'ok'], ['中间捕获·消息', 'ok'], ['中间捕获·信号/条件', 'no'], ['中间抛出(全部)', 'no'],
      ['边界定时·中断', 'ok'], ['边界定时·非中断(催办)', 'ok'], ['边界错误·中断', 'ok'], ['边界·消息/信号/升级/补偿', 'no']]],
    ['网关 Gateways', P.violet, [
      ['排他 XOR', 'ok'], ['并行 AND', 'ok'], ['包容 OR', 'ok'], ['事件网关(竞速)', 'ok'], ['复杂网关', 'no']]],
    ['任务 Tasks', P.aqua, [
      ['用户任务', 'ok'], ['服务任务·同步', 'ok'], ['服务任务·异步 Job', 'ok'], ['外部 Worker', 'ok'],
      ['业务规则任务(决策表)', 'ok'], ['调用活动(子流程)', 'ok'], ['脚本/发送/接收/手工', 'no']]],
    ['子过程 · 多实例', P.green, [
      ['嵌入子过程(扁平化)', 'warn'], ['事件子过程(错误·中断)', 'warn'], ['事务子过程', 'no'],
      ['并行多实例(会签)', 'ok'], ['串行多实例(或签)', 'ok'], ['完成条件(提前结束)', 'ok'], ['动态办理人', 'ok']]],
    ['数据 · 补偿 · 关联', P.orange, [
      ['过程变量 + 历史', 'ok'], ['变量 schema 声明', 'ok'], ['顺序流条件 ${..}', 'ok'], ['默认流', 'ok'],
      ['消息关联', 'ok'], ['补偿(边界/抛出/活动)', 'no'], ['信号事件 + 关联', 'no'], ['升级事件', 'no']]],
  ]
  const x0 = 152, maxX = W - 30
  let rows = '', y = 78
  for (const [name, hue, items] of sections) {
    const f = statusFlow(x0, y, maxX, items)
    const rowH = f.height
    rows += R(28, y - 4, 116, rowH - 4, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
    rows += R(28, y - 4, 4, rowH - 4, { rx: 2, fill: hue })
    rows += T(44, y + 15, name, { size: 12.5, w: 800 })
    rows += f.svg
    y += rowH + 8
  }
  const H = y + 58
  let b = titleBlk(W, '领域模型 · BPMN 2.0 能力覆盖矩阵',
    '19 个 NodeKind 变体 · 约 28 项能力「已建模 + 执行 + 集成测试」（每变体一份 tests/*.rs）')
  b += rows
  b += R(28, y + 2, W - 56, 44, { rx: 9, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(44, y + 22, '✓ 已交付并测试　·　! 部分（嵌入/事件子过程编译期扁平化、事件子过程仅错误·中断）　·　– 暂不支持', { size: 11, w: 700, fill: P.ink2 })
  b += T(44, y + 38, '主要缺口：补偿 / 信号事件 / 升级事件 / 事务子过程 / 复杂网关 / 脚本·收发任务 / 中间抛出事件（不支持元素编译期显式报错，绝不静默降级）', { size: 10.5, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图·执行引擎令牌语义 ══════════════
function figToken () {
  const W = 940, H = 596
  const x = 40, w = W - 80
  const panel = (px, py, pw, ph, hue, tt, lines) => {
    let s = R(px, py, pw, ph, { rx: 10, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
    s += R(px, py, 4, ph, { rx: 2, fill: hue })
    s += T(px + 16, py + 22, tt, { size: 12.5, w: 800 })
    lines.forEach((ln, i) => { s += T(px + 16, py + 42 + i * 16, ln, { size: 10.5, fill: P.ink2 }) })
    return s
  }
  let b = titleBlk(W, '执行引擎 · 令牌持久化语义（Token-Based Persistent Execution）',
    'run_to_wait 单线程步进 · 等待态即一次 PG 事务提交点 · SKIP LOCKED 集群安全 · Incident→重试→死信')
  // Row1 编译流水线
  let y = 74
  b += cell(x, y, 250, 52, P.violet, 'BPMN 2.0 XML', '标准交换格式（存 XML 非 IR）')
  b += LINE(x + 250, y + 26, x + 303, y + 26); b += T(x + 277, y + 16, 'compile', { size: 9.5, fill: P.muted, anchor: 'middle' }); b += T(x + 277, y + 42, 'roxmltree', { size: 9, fill: P.muted, anchor: 'middle', mono: true })
  b += cell(x + 305, y, 270, 52, P.blue, 'ProcessDefinition IR', 'arena + 19 NodeKind enum（不可变）')
  b += LINE(x + 575, y + 26, x + 608, y + 26); b += T(x + 592, y + 16, '发起', { size: 9.5, fill: P.muted, anchor: 'middle' })
  b += cell(x + 610, y, 250, 52, P.aqua, '运行实例', 'InstanceSnapshot 原子聚合')
  // Row2 步进循环
  y = 150
  b += R(x, y, w, 58, { rx: 10, fill: P.blue, fop: 0.12, stroke: P.blue, sop: 0.34, sw: 1 })
  b += R(x, y, 4, 58, { rx: 2, fill: P.blue })
  b += T(x + 18, y + 24, 'run_to_wait · 令牌步进内核（单线程循环 · STEP_LIMIT 防跑飞）', { size: 14, w: 800 })
  b += T(x + 18, y + 44, '取首个 Active 令牌 → clone kind/outgoing 断借用 → match NodeKind 前进一步 → 无 Active 即停 → 全 Ended 则实例 Completed', { size: 11, fill: P.ink2 })
  // Row3 分叉/合并语义
  y = 222
  const cw = (w - 2 * 14) / 3
  b += panel(x, y, cw, 76, P.aqua, '并行 AND', ['fork = 克隆令牌到全部出边', 'join = Joining 计数达 incoming_count', '　　 → 保留单 survivor 再前进'])
  b += panel(x + cw + 14, y, cw, 76, P.green, '包容 OR', ['fork = 全部条件真出边(否则默认)', 'join = can_reach BFS 可达性探测', '　　 → 无其它令牌可达才释放'])
  b += panel(x + 2 * (cw + 14), y, cw, 76, P.magenta, '事件网关 · 竞速', ['武装 定时 + 消息 后继', '首个到达胜出', '　　 → 取消其余分支'])
  // Row4 等待态
  y = 314
  b += R(x, y, w, 66, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x + 16, y + 22, '等待态 = 提交点（每次持久化一次 PG 事务 · InstanceSnapshot 差量落库）：', { size: 12, w: 700, fill: P.ink2 })
  const waits = ['UserTask', 'CallActivity', 'WaitingMessage', 'WaitingTimer', 'WaitingAsync', 'WaitingEventGateway', 'Joining', 'Incident']
  b += chipFlow(x + 16, y + 33, x + w - 16, waits, P.blue, { size: 10.5 }).svg
  // Row5 双护栏
  y = 392
  const rw = (w - 14) / 2
  b += panel(x, y, rw, 72, P.aqua, 'SKIP-LOCKED 轮询器 · 集群安全(HA)', ['异步 Job / 定时器 / 外部 Worker', 'UPDATE … WHERE id IN (SELECT … FOR UPDATE SKIP LOCKED)', 'N worker 互斥取件 · 侧表隔离锁不被快照覆盖'])
  b += panel(x + rw + 14, y, rw, 72, P.red, '失败隔离 · Incident / 死信', ['DelegateError::Bpmn → 错误边界 / 事件子过程', 'Generic → Incident（令牌留存）→ retry_incident', '异步重试耗尽 → 死信队列 → 运维 retry / discard'])
  // Row6 持久化表
  y = 476
  b += R(x, y, w, 88, { rx: 10, fill: P.orange, fop: 0.08, stroke: P.orange, sop: 0.26, sw: 1 })
  b += R(x, y, 4, 88, { rx: 2, fill: P.orange })
  b += T(x + 16, y + 22, '持久化 · cmx_flow_* 表（运行态 RU 与 历史 HI 分离 · 幂等 DDL · 无外键）', { size: 12, w: 800 })
  const tbls = ['instance', 'token', 'task', 'candidate', 'cc', 'delegation', 'async_job', 'deadletter_job', 'job·timer', 'message_subscription', 'mi_scope', 'var_history', 'decision', 'subflow_binding', 'hi_instance', 'hi_task', 'hi_activity', 'biz_link', 'form_binding']
  b += chipFlow(x + 16, y + 32, x + w - 14, tbls, P.orange, { size: 10 }).svg
  return doc(W, H, b)
}

// ══════════════ 图·企业运行时能力全景 ══════════════
function figRuntime () {
  const W = 940
  const x = 40, w = W - 80
  const cols = [
    ['可靠性 · 伸缩性', P.aqua, ['SKIP-LOCKED 异步执行器', '死信队列 DLQ', '外部 Worker + SDK', '注入式时钟定时器', '实例迁移(dry-run 校验)', '活动历史 / SLA', 'Incident 重试', '定义热加载']],
    ['多租户 · 安全 · API', P.blue, ['db-per-tenant 懒注册', 'JWT / API-Key / off', 'SSE 一次性票据', 'Headless /flow/v1', 'SSE 事件流(按租户)', 'HMAC-SHA256 Webhook', 'OpenAPI + Swagger', 'Nacos 自注册', '门户反代 FlowProxy']],
    ['设计态 · 前端 · 运维', P.violet, ['bpmn-js 四区设计器', '模拟试跑 + 追踪', '版本结构级 diff', '决策表 设计/查看', '实时协同 M1/M2/M3', '运维台·令牌活标记', '令牌回放(history)', 'Web Component 可嵌', '监控大盘 + /_mon', '表单绑定 + 待办中心']],
  ]
  const cw = (w - 2 * 16) / 3
  let maxH = 0, body = ''
  cols.forEach(([name, hue, items], ci) => {
    const cx = x + ci * (cw + 16)
    body += R(cx, 74, cw, 40, { rx: 9, fill: hue, fop: 0.14, stroke: hue, sop: 0.34, sw: 1 })
    body += T(cx + cw / 2, 74 + 25, name, { size: 13, w: 800, anchor: 'middle' })
    const f = chipFlow(cx + 8, 126, cx + cw - 4, items, hue, { size: 10.5, lh: 27 })
    body += f.svg
    maxH = Math.max(maxH, 126 + f.height)
  })
  const H = maxH + 58
  let b = titleBlk(W, '企业运行时能力全景', '可靠性/伸缩 · 多租户/安全/API · 设计态/前端/运维 —— 均已实现，多数含单测 + E2E/CDP 验证')
  b += body
  b += R(x, maxH + 8, w, 34, { rx: 9, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x + 16, maxH + 30, '一芯多壳：同一 cmx-flow-app 核，既可独立 flow-server 部署，也可门户进程内嵌反代，一行配置切换。', { size: 11, fill: P.ink2 })
  return doc(W, H, b)
}

// ══════════════ 图·与主流开源引擎全维度对比矩阵（中心图）══════════════
function figCompare () {
  const engines = [
    ['cmx-flowengine', 'Rust · Apache-2.0', true],
    ['Camunda 7', 'Java · Apache*', false],
    ['Camunda 8 · Zeebe', 'Go/Java · 源码可得', false],
    ['Flowable', 'Java · Apache-2.0', false],
    ['Temporal', 'Go · MIT', false],
  ]
  const rows = [
    ['语言 / 运行时', [['Rust', 'neu'], ['Java/JVM', 'neu'], ['Go+Java', 'neu'], ['Java 17', 'neu'], ['Go', 'neu']]],
    ['执行模型', [['令牌·持久化', 'neu'], ['令牌·关系库', 'neu'], ['事件溯源·分区', 'neu'], ['令牌·关系库', 'neu'], ['溯源·确定重放', 'neu']]],
    ['BPMN 元素广度', [['子集·28 能力', 'warn'], ['近乎完整', 'ok'], ['子集', 'warn'], ['近乎完整', 'ok'], ['无 BPMN', 'no']]],
    ['DMN / 决策', [['规则引擎+决策表', 'warn'], ['完整 DMN', 'ok'], ['DMN', 'ok'], ['完整 DMN', 'ok'], ['代码内', 'no']]],
    ['人工审批', [['完整·7 类办理人', 'ok'], ['完整', 'ok'], ['基础任务', 'warn'], ['完整', 'ok'], ['无', 'no']]],
    ['异步作业执行器', [['SKIP LOCKED', 'ok'], ['线程池+锁', 'ok'], ['分区并行', 'ok'], ['SKIP LOCKED', 'ok'], ['分片队列', 'ok']]],
    ['外部 Worker', [['✓ + Rust SDK', 'ok'], ['✓ 外部任务', 'ok'], ['✓ Job Worker', 'ok'], ['✓', 'ok'], ['Activity Worker', 'ok']]],
    ['死信 / 重试', [['DLQ + Incident', 'ok'], ['死信 + Incident', 'ok'], ['Incident', 'ok'], ['死信队列', 'ok'], ['重试策略', 'ok']]],
    ['实例迁移', [['✓ dry-run 校验', 'ok'], ['✓ Migration API', 'ok'], ['✓ 8.5+', 'ok'], ['✓', 'ok'], ['Reset / Patch', 'warn']]],
    ['多租户', [['db-per-tenant', 'ok'], ['库/schema/表', 'ok'], ['✓ 8.3+', 'warn'], ['✓', 'ok'], ['Namespace', 'ok']]],
    ['设计器', [['bpmn-js 四区·内置', 'ok'], ['桌面 Modeler', 'ok'], ['Web Modeler 云', 'ok'], ['网页 Modeler', 'ok'], ['无', 'no']]],
    ['实时协同建模 ★', [['✓ SSE M1/M2/M3', 'ok'], ['–', 'no'], ['云 SaaS 版', 'warn'], ['–', 'no'], ['N/A', 'no']]],
    ['可嵌入形态', [['库/服务/Web组件', 'ok'], ['库/服务', 'ok'], ['仅集群', 'no'], ['库/服务', 'ok'], ['仅集群', 'no']]],
    ['水平扩展 / 吞吐', [['HA worker·ERP 级', 'warn'], ['集群作业器', 'warn'], ['互联网级', 'ok'], ['集群', 'warn'], ['互联网级', 'ok']]],
    ['许可 / 开放性 ★', [['Apache-2.0 无限制', 'ok'], ['CE 已 EOL(25.10)', 'warn'], ['源码可得·生产收费', 'no'], ['Apache-2.0', 'ok'], ['MIT', 'ok']]],
  ]
  const W = 1060, x0 = 30, dimW = 140, ew = (W - 2 * x0 - dimW) / 5
  const headY = 74, headH = 52, rowH = 34, y0 = headY + headH + 5
  const H = y0 + rows.length * rowH + 50
  let b = titleBlk(W, '与主流开源流程引擎 · 全维度对比',
    'cmx-flowengine 当前能力（非陈旧 gap 文档口径）vs Camunda 7 / Camunda 8·Zeebe / Flowable / Temporal　·　★ = cmx 差异化项')
  engines.forEach((e, i) => {
    const cx = x0 + dimW + i * ew
    const acc = e[2] ? P.blue : P.base
    b += R(cx + 2, headY, ew - 4, headH, { rx: 8, fill: acc, fop: e[2] ? 0.16 : 0.08, stroke: acc, sop: e[2] ? 0.44 : 0.22, sw: e[2] ? 1.5 : 1 })
    b += T(cx + ew / 2, headY + 22, e[0], { size: 12.5, w: 800, anchor: 'middle', fill: e[2] ? P.blue550 : P.ink })
    b += T(cx + ew / 2, headY + 40, e[1], { size: 9.5, anchor: 'middle', fill: P.ink2 })
  })
  b += T(x0 + dimW / 2, headY + 30, '对比维度', { size: 11.5, w: 800, anchor: 'middle', fill: P.ink2 })
  const gTint = { ok: P.good, warn: P.warning, no: P.muted }
  rows.forEach((r, ri) => {
    const ry = y0 + ri * rowH
    b += R(x0, ry, dimW, rowH - 3, { rx: 6, fill: P.plane, stroke: P.border, sw: 1 })
    b += T(x0 + 10, ry + rowH / 2 + 2, r[0], { size: 11, w: 700, fill: P.ink })
    r[1].forEach(([txt, g], ci) => {
      const cx = x0 + dimW + ci * ew
      const isCmx = ci === 0
      if (g === 'neu') {
        b += R(cx + 2, ry, ew - 4, rowH - 3, { rx: 6, fill: isCmx ? P.blue : P.plane, fop: isCmx ? 0.05 : 1, stroke: P.border, sw: 1 })
        b += T(cx + ew / 2, ry + rowH / 2 + 2, txt, { size: 10.5, anchor: 'middle', fill: P.ink2 })
      } else {
        const hue = gTint[g]
        b += R(cx + 2, ry, ew - 4, rowH - 3, { rx: 6, fill: hue, fop: g === 'no' ? 0.07 : 0.14, stroke: hue, sop: g === 'no' ? 0.28 : 0.36, sw: 1 })
        b += R(cx + 2, ry, 3, rowH - 3, { rx: 1.5, fill: hue })
        b += T(cx + ew / 2 + 2, ry + rowH / 2 + 2, txt, { size: 10.5, w: g === 'no' ? 400 : 600, anchor: 'middle', fill: g === 'no' ? P.ink2 : P.ink })
      }
    })
  })
  const ly = y0 + rows.length * rowH + 10
  b += R(x0, ly, W - 2 * x0, 30, { rx: 8, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x0 + 14, ly + 19, '图例：', { size: 10.5, w: 700, fill: P.ink2 })
  let lx = x0 + 58
  ;[['强 / 完整', 'ok'], ['部分 / 受限', 'warn'], ['弱 / 无', 'no'], ['中性事实', 'neu']].forEach(([lab, g]) => {
    const hue = g === 'neu' ? P.base : gTint[g]
    b += R(lx, ly + 7, 15, 15, { rx: 4, fill: hue, fop: g === 'no' ? 0.1 : 0.16, stroke: hue, sop: 0.4, sw: 1 })
    b += T(lx + 21, ly + 19, lab, { size: 10.5, fill: P.ink2 }); lx += 34 + wOf(lab, 10.5)
  })
  return doc(W, H, b)
}

// ══════════════ 图·许可与开放性格局 ══════════════
function figOpenness () {
  const W = 940, H = 402
  const x = 40, w = W - 80
  const pw = (w - 16) / 2
  let b = titleBlk(W, '许可与开放性格局（Licensing & Openness）',
    '开源流程引擎的可持续性差异 —— OSI 真开源 vs 源码可得(受限) vs 社区版已 EOL')
  const litem = (px, py, name, note, star) => {
    let s = T(px, py, (star ? '★ ' : '') + name, { size: 12.5, w: 800, fill: star ? P.green : P.ink })
    s += T(px, py + 16, note, { size: 10, fill: P.ink2 })
    return s
  }
  // 左：自由开源
  let py = 74
  b += R(x, py, pw, 300, { rx: 12, fill: P.green, fop: 0.08, stroke: P.green, sop: 0.30, sw: 1.2 })
  b += R(x, py, pw, 4, { rx: 2, fill: P.green })
  b += T(x + 18, py + 28, '① 自由开源 · 自托管生产免费', { size: 13.5, w: 800, fill: P.green })
  b += T(x + 18, py + 46, 'OSI 批准许可 · 可商用 · 可魔改 · 无生产许可门槛', { size: 10.5, fill: P.ink2 })
  const L = [
    ['cmx-flowengine', 'Apache-2.0 · 无使用限制 · 可自托管/商用/二开', true],
    ['Flowable', 'Apache-2.0 · BPMN/CMMN/DMN 引擎全开源', false],
    ['Activiti / jBPM', 'Apache-2.0 · 传统 Java BPMN 引擎', false],
    ['Temporal', 'MIT · 代码优先持久执行（非 BPMN 建模）', false],
    ['Conductor OSS', 'Apache-2.0 · JSON 编排（非 BPMN）', false],
  ]
  L.forEach((it, i) => { b += litem(x + 20, py + 82 + i * 42, it[0], it[1], it[2]) })
  // 右：受限/生命周期风险
  const rx = x + pw + 16
  b += R(rx, py, pw, 300, { rx: 12, fill: P.warning, fop: 0.09, stroke: P.warning, sop: 0.34, sw: 1.2 })
  b += R(rx, py, pw, 4, { rx: 2, fill: P.warning })
  b += T(rx + 18, py + 28, '② 受限 / 生命周期风险', { size: 13.5, w: 800, fill: P.serious })
  b += T(rx + 18, py + 46, '源码可得≠开源；社区版可能停更 —— 自托管前须评估', { size: 10.5, fill: P.ink2 })
  b += R(rx + 16, py + 66, pw - 32, 96, { rx: 9, fill: P.critical, fop: 0.06, stroke: P.critical, sop: 0.26, sw: 1 })
  b += T(rx + 30, py + 90, 'Camunda 8 / Zeebe', { size: 12.5, w: 800, fill: P.critical })
  b += T(rx + 30, py + 110, '源码可得（Camunda License v1.0，非 OSI 开源）', { size: 10, fill: P.ink2 })
  b += T(rx + 30, py + 126, '自 8.6 起：核心引擎「生产使用」需付费许可', { size: 10, fill: P.ink2 })
  b += T(rx + 30, py + 142, '仅连接器 SDK / 客户端 / Exporter 仍 Apache-2.0', { size: 10, fill: P.muted })
  b += R(rx + 16, py + 172, pw - 32, 96, { rx: 9, fill: P.warning, fop: 0.10, stroke: P.warning, sop: 0.30, sw: 1 })
  b += T(rx + 30, py + 196, 'Camunda 7 社区版 (CE)', { size: 12.5, w: 800, fill: P.serious })
  b += T(rx + 30, py + 216, 'Apache-2.0，但 2025-10-14 已 EOL（v7.24 终版）', { size: 10, fill: P.ink2 })
  b += T(rx + 30, py + 232, '仓库归档、停止更新与安全补丁', { size: 10, fill: P.ink2 })
  b += T(rx + 30, py + 248, '企业版延至 2030（付费）', { size: 10, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图·封面定位 ══════════════
function figOverview () {
  const W = 940, H = 342
  const x = 40, w = W - 80
  let b = titleBlk(W, 'cmx-flowengine · Rust 原生 BPMN 2.0 流程引擎',
    '一芯多壳 · 令牌持久化执行 · 独立微服务 / 可嵌 Web Component / Headless · 对标 Camunda · Flowable · Zeebe · Temporal')
  const tiles = [
    ['Rust', '语言 · edition 2024', P.violet],
    ['Apache-2.0', '许可 · 无使用限制', P.green],
    ['25,012', '域 LOC（+9,351 测试）', P.blue],
    ['令牌持久化', '等待态 = 提交点', P.aqua],
    ['28 项', 'BPMN 2.0 能力', P.orange],
    ['7 trait', '可注入扩展点', P.magenta],
  ]
  const cols = 3, gap = 16, tw = (w - (cols - 1) * gap) / cols, th = 74, y0 = 74
  tiles.forEach((t, i) => {
    const cx = x + (i % cols) * (tw + gap), cy = y0 + Math.floor(i / cols) * (th + 14)
    b += R(cx, cy, tw, th, { rx: 12, fill: P.surface, stroke: P.border, sw: 1 })
    b += R(cx, cy, tw, 4, { rx: 2, fill: t[2] })
    b += T(cx + tw / 2, cy + 42, t[0], { size: 25, w: 800, anchor: 'middle', fill: t[2], mono: true })
    b += T(cx + tw / 2, cy + 62, t[1], { size: 11, w: 600, anchor: 'middle', fill: P.ink2 })
  })
  const chy = 252
  const diff = ['单静态二进制·无 GC', 'Web Component 可嵌', '设计器实时协同 M1–M3', 'db-per-tenant 多租户', 'SKIP-LOCKED 异步执行器', '死信队列', '外部 Worker + SDK', '实例迁移(dry-run)', '令牌可视化+回放', '279 测试真机绿']
  b += T(x, chy + 16, '差异化亮点：', { size: 11.5, w: 800, fill: P.ink2 })
  b += chipFlow(x + 84, chy, x + w, diff, P.blue, { size: 10.5 }).svg
  return doc(W, H, b)
}

// ══════════════ 图·部署姿态 ══════════════
function figDeploy () {
  const W = 920, H = 428
  let b = titleBlk(W, '部署姿态 · 同一引擎核', '门户与引擎编译期解耦：门户不依赖引擎源码，纯 HTTP 反代（cargo tree 已验证切断）')
  const box = (x, y, w, h, hue, t, s) => cell(x, y, w, h, hue, t, s)
  const lane = (y, tag, tagHue) => { b += R(30, y, 96, 58, { rx: 9, fill: tagHue, fop: 0.14, stroke: tagHue, sop: 0.34, sw: 1 }); b += T(78, y + 26, tag.split('|')[0], { size: 12.5, w: 800, anchor: 'middle' }); b += T(78, y + 43, tag.split('|')[1], { size: 10.5, fill: P.ink2, anchor: 'middle' }) }
  let y = 78; lane(y, '① 独立|微服务', P.violet)
  b += box(150, y, 176, 58, P.blue, '门户 Portal', ':8080')
  b += LINE(326, y + 29, 396, y + 29); b += T(361, y + 20, 'FlowProxy', { size: 10, fill: P.muted, anchor: 'middle' }); b += T(361, y + 46, '/api/flow/*', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true })
  b += box(398, y, 176, 58, P.aqua, 'flow-server', 'cmx-flow-server · :8091')
  b += LINE(574, y + 29, 644, y + 29); b += T(609, y + 20, 'db-per-tenant', { size: 10, fill: P.muted, anchor: 'middle' })
  b += box(646, y, 234, 58, P.orange, 'PostgreSQL', 'fico + cmx · flow_<tenant>')
  y = 168; lane(y, '② 可嵌|组件', P.magenta)
  b += box(150, y, 210, 58, P.green, '第三方 App', 'React / Vue / 原生')
  b += LINE(360, y + 29, 452, y + 29); b += T(406, y + 20, '<flow-designer>', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true }); b += T(406, y + 46, '<flow-todo> …', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true })
  b += box(454, y, 426, 58, P.aqua, 'flow-server v1 API', '自定义元素直连 /api/flow/v1/*')
  y = 258; lane(y, '③ Headless|无界面', P.blue)
  b += box(150, y, 210, 58, P.violet, '自建前端 / 系统', '完全自绘 UI')
  b += LINE(360, y + 29, 452, y + 29); b += T(406, y + 21, 'REST + SSE', { size: 9.5, fill: P.muted, anchor: 'middle' }); b += T(406, y + 46, '+ OpenAPI', { size: 9.5, fill: P.muted, anchor: 'middle' })
  b += box(454, y, 426, 58, P.aqua, '/api/flow/v1/* · /events(SSE) · /docs', 'Swagger + 事件流 + API Key/JWT')
  b += R(30, 336, W - 60, 64, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(46, 358, '后端一芯双壳：同一 cmx-flow-app 核，既可门户进程内嵌反代、也可独立 flow-server 部署；第四姿态 cmx-flow-demo(:8090) 自包含 SPA 供本地评估。', { size: 11, fill: P.ink2 })
  b += T(46, 378, '三层出站鉴权：X-API-Key（服务身份）+ X-Delegated-User-Token（真实办理人）+ X-Request-Id；多租户 db-per-tenant 物理隔离。', { size: 11, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图·测试与质量 ══════════════
function figTests () {
  const W = 940, H = 428
  let b = titleBlk(W, '测试与质量 · 真机验证', 'Rust 单测/集成 + 后端 curl 回归 + Playwright/CDP 前端；只增不删、数据留库（口径为当前实测）')
  const tiles = [
    ['279', 'Rust 测试函数 · 0 失败', 'cargo test --workspace'],
    ['25', 'PG 活库集成 · 默认 ignore', 'TEST_PG_URL 门控'],
    ['159/159', '后端全量回归', 'run-all.sh · 12 套件'],
    ['60/60', '子流程专项', 'run-subflow.sh'],
    ['25/25', '维度路由 RD0–4', 'dimtest_routing.sh'],
    ['18/18', '新功能真机 E2E', 'A7/A9/P1/A8/A3'],
    ['22/22', '差旅报销业务 E2E', 'biz-test'],
    ['23/23', '子流程钻入(门户级)', 'subflow_drilldown.cjs'],
    ['12/12', '设计器 模拟+diff', 'designer_simulate_diff.cjs'],
    ['32/32', '协同 M1+M2 断言', 'collab CDP'],
    ['10/10', '设计器新功能验收', 'designer_features.cjs'],
    ['11/11', '缺陷回归专项', 't_fixes_verify.sh'],
  ]
  const cols = 4, gap = 16, x0 = 40, tw = (W - 80 - (cols - 1) * gap) / cols, th = 96, y0 = 78
  tiles.forEach((t, i) => {
    const cx = x0 + (i % cols) * (tw + gap), cy = y0 + Math.floor(i / cols) * (th + 16)
    b += R(cx, cy, tw, th, { rx: 12, fill: P.surface, stroke: P.border, sw: 1 })
    b += R(cx, cy, tw, 4, { rx: 2, fill: P.good })
    b += `<circle cx="${cx + 16}" cy="${cy + 26}" r="7" fill="${P.good}" fill-opacity="0.15"/><path d="M${cx + 12.5},${cy + 26} l2.5,2.5 l5,-5.5" stroke="${P.good}" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>`
    b += T(cx + tw / 2 + 8, cy + 48, t[0], { size: 26, w: 800, anchor: 'middle', fill: P.good, mono: true })
    b += T(cx + tw / 2, cy + 68, t[1], { size: 11, w: 700, anchor: 'middle', fill: P.ink })
    b += T(cx + tw / 2, cy + 85, t[2], { size: 9.5, anchor: 'middle', fill: P.muted, mono: true })
  })
  return doc(W, H, b)
}

// ══════════════ 图·能力演进时间线 ══════════════
function figTimeline () {
  const W = 980
  const tracks = [
    ['引擎核心', 'M1–M5.3', P.blue, ['M1 定义/发起', 'M2 网关', 'M2.5 边界定时器', 'M3 多实例', 'M4 抄送/转签', 'M5.1 子流程', 'M5.2 组织路由', 'M5.3 多挂载']],
    ['BPMN 补齐', 'A1–A10', P.orange, ['A1 包容网关', 'A2 终止', 'A3 事件子流程', 'A5 扁平子过程', 'A6 活动历史', 'A7 外部 Worker', 'A8 错误边界', 'A9 实例迁移', 'A10 事件网关']],
    ['可靠性', 'H1–H4 · P1–P4', P.aqua, ['H1 热加载', 'H2 incident', 'H3 异步 Job', 'H4 运维视图', 'P1 SKIP-LOCKED 执行器', 'P2 死信队列', 'P4 令牌可视化']],
    ['人工审批', '', P.green, ['会签/或签', '转签/加签/委派', '抄送', '退回任意节点', '取回', '7 类办理人', '身份双模', '表单绑定/单据关联', '审批意见留痕']],
    ['维度路由', 'RD0–RD5', P.magenta, ['RD0 契约', 'RD1 注入', 'RD2 精确/继承/兜底', 'RD3 多挂载', 'RD4 设计器', 'RD5 HTTP 维度解析']],
    ['设计器 · 大前端', '', P.yellow, ['四区画布', '属性面板', '变量声明', '子流程钻入', '模拟试跑', '版本 diff', '协同 M1/M2/M3', '决策表设计/查看', '令牌回放', '科技感 light/dark']],
    ['微服务化', 'S0–S6', P.violet, ['S0 迁移', 'S1 适配器', 'S2 多租户', 'S3 headless', 'S4 前端抽核', 'S5 组件', 'S6 平台反代']],
  ]
  const x0 = 156, maxX = W - 34
  let rows = '', y = 78
  for (const [name, code, hue, items] of tracks) {
    const f = chipFlow(x0, y, maxX, items, hue, { size: 11, lh: 28 })
    const rowH = f.height
    rows += R(28, y - 4, 116, rowH - 4, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
    rows += R(28, y - 4, 4, rowH - 4, { rx: 2, fill: hue })
    rows += T(44, y + 14, name, { size: 12.5, w: 800 })
    if (code) rows += T(44, y + 31, code, { size: 10, fill: P.muted, mono: true })
    rows += f.svg
    y += rowH + 8
  }
  const H = y + 42
  let b = titleBlk(W, '能力演进时间线 · 七条能力轨', '均已交付并回归通过（里程碑代号取自源码/测试文件命名）')
  b += rows
  b += R(28, y + 2, W - 56, 30, { rx: 9, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(44, y + 22, 'M 引擎核心 · A BPMN 补齐 · H/P 可靠性 · RD 维度路由 · S 微服务化 —— 全部已交付并纳入回归（279 Rust 测试 + 后端/前端 E2E 全绿）。', { size: 11, fill: P.ink2 })
  return doc(W, H, b)
}

// ══════════════ 图·路线图 Now/Next/Later ══════════════
function figRoadmap () {
  const W = 940
  const x = 40, w = W - 80
  const cols = [
    ['Now · 已交付并测试', P.green, ['令牌引擎 + 持久化', 'A1–A10 BPMN 补齐', 'P1–P4 生产可靠性', '会签/或签多实例', '子流程 + 维度路由 RD0–5', '多租户 db-per-tenant', 'Headless v1 + SSE + Webhook', '设计器 + 模拟 + diff + 协同', '运维台 + 令牌回放', '实例迁移 · 外部 Worker · 死信']],
    ['Next · 审批完整度', P.blue, ['信号 / 升级事件', '补偿(边界/抛出/活动)', 'FEEL / DMN 子集深化', '更多 delegate / 连接器', '变量历史归档增强', '事件子过程非错误触发']],
    ['Later · 世界级 · 择机', P.violet, ['事务子过程', 'CMMN 案例管理', '事件注册中心 Kafka/RabbitMQ', '水平扩展(分区执行)', '增量快照持久化', '多语言 Worker SDK', '复杂网关(不建议追)']],
  ]
  const cw = (w - 2 * 16) / 3
  let maxH = 0, body = ''
  cols.forEach(([name, hue, items], ci) => {
    const cx = x + ci * (cw + 16)
    body += R(cx, 74, cw, 40, { rx: 9, fill: hue, fop: 0.14, stroke: hue, sop: 0.34, sw: 1 })
    body += T(cx + cw / 2, 74 + 25, name, { size: 12.5, w: 800, anchor: 'middle' })
    const f = chipFlow(cx + 8, 126, cx + cw - 4, items, hue, { size: 10.5, lh: 27 })
    body += f.svg
    maxH = Math.max(maxH, 126 + f.height)
  })
  const H = maxH + 64
  let b = titleBlk(W, '路线图 · Now / Next / Later', '三赛道策略：深耕人工审批(A) · 择机借鉴云原生可靠性(B) · 不追代码优先(C)')
  b += body
  b += R(x, maxH + 8, w, 40, { rx: 9, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x + 16, maxH + 26, '审批赛道够用即止；世界级完整度按业务诉求择机推进 —— 优先「把审批引擎做透 + 做到生产可靠」，而非盲目追求通用引擎广度（更优 ROI）。', { size: 11, fill: P.ink2 })
  b += T(x + 16, maxH + 41, 'Now 全部已交付并经真机测试；Next / Later 为规划项，非承诺排期。', { size: 10, fill: P.muted })
  return doc(W, H, b)
}

const FIGS = {
  'fig-overview': figOverview, 'fig-arch': figArch, 'fig-bpmn': figBpmn, 'fig-token': figToken,
  'fig-runtime': figRuntime, 'fig-deploy': figDeploy, 'fig-compare': figCompare, 'fig-openness': figOpenness,
  'fig-tests': figTests, 'fig-timeline': figTimeline, 'fig-roadmap': figRoadmap,
}

function main () {
  for (const [k, fn] of Object.entries(FIGS)) {
    const svg = fn()
    writeFileSync(join(DIR, `${k}.svg`), svg)
    console.log(`wrote ${k}.svg (${(svg.length / 1024).toFixed(1)} KiB)`)
  }
}
main()
