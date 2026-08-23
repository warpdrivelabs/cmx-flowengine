// 生成 cmx-flowengine 阶段性总结的全部图（自包含浅色卡片 SVG，共用 dataviz 验证过的 CVD-安全调色板）。
// 每张图自绘 #fcfcfb 卡面 → 在任意浅/深 markdown 渲染器上都清晰可读。
// 用法: node docs/summary/assets/gen-svgs.mjs  → 写出 fig-*.svg
import { writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const DIR = dirname(fileURLToPath(import.meta.url))

// ── 调色板（dataviz references/palette.md，浅色卡面）──
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
// CJK-aware 粗略宽度估算（chars * per-char + padding）
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
// 图元卡：浅色卡面 + 细边
const card = (w, h) => R(0, 0, w, h, { rx: 16, fill: P.surface, stroke: P.border, sw: 1 })
const defs = `<defs>
  <marker id="arr" markerWidth="9" markerHeight="9" refX="6.5" refY="3" orient="auto"><path d="M0,0 L6.5,3 L0,6 Z" fill="${P.muted}"/></marker>
</defs>`
const doc = (w, h, body) => `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img">${defs}${card(w, h)}${body}</svg>`

// 层带（左侧色条 + 标题 + 副标 + 右侧 LOC）
const band = (x, y, w, h, hue, title, sub, loc) => {
  let s = R(x, y, w, h, { rx: 10, fill: hue, fop: 0.10, stroke: hue, sop: 0.32, sw: 1 })
  s += R(x, y, 4, h, { rx: 2, fill: hue })
  s += T(x + 18, y + (sub ? h / 2 - 4 : h / 2 + 5), title, { size: 14.5, w: 700 })
  if (sub) s += T(x + 18, y + h / 2 + 15, sub, { size: 11.5, fill: P.ink2 })
  if (loc) s += T(x + w - 14, y + h / 2 + 5, loc, { size: 12, fill: P.muted, anchor: 'end', mono: true })
  return s
}
// 小格
const cell = (x, y, w, h, hue, title, sub) => {
  let s = R(x, y, w, h, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
  s += R(x, y, 4, h, { rx: 2, fill: hue })
  s += T(x + w / 2 + 2, y + (sub ? h / 2 - 2 : h / 2 + 4), title, { size: 12.5, w: 700, anchor: 'middle' })
  if (sub) s += T(x + w / 2 + 2, y + h / 2 + 13, sub, { size: 10, fill: P.ink2, anchor: 'middle' })
  return s
}
// chip（药丸）：色底 + 色边 + 墨字（标识来自色底，非字色，保证可读）
const chip = (x, y, label, hue, o = {}) => {
  const { size = 11, pad = 11, h = 22 } = o
  const w = Math.round(wOf(label, size) + pad * 2)
  let s = R(x, y, w, h, { rx: h / 2, fill: hue, fop: 0.13, stroke: hue, sop: 0.34, sw: 1 })
  s += `<circle cx="${x + pad - 2}" cy="${y + h / 2}" r="3" fill="${hue}"/>`
  s += T(x + pad + 5, y + h / 2 + 4, label, { size, fill: P.ink })
  return { svg: s, w }
}
// chip 流式排布（自动换行）
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
const title = (w, s, sub) => T(w / 2, 34, s, { size: 19, w: 800, anchor: 'middle' }) +
  (sub ? T(w / 2, 54, sub, { size: 12.5, fill: P.ink2, anchor: 'middle' }) : '')

// 状态格：✓ 已交付(good) / ! 部分(warning) / – 暂不支持(muted)。图标+文字，绝不靠色区分。
const ST = { ok: P.good, warn: P.warning, no: P.muted }
const GLY = { ok: '✓', warn: '!', no: '–' }
const statusCell = (x, y, label, st) => {
  const hue = ST[st], h = 24, w = Math.round(wOf(label, 11.5) + 40)
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

// ══════════════ 图1 · 架构总览「一芯多壳」 ══════════════
function fig1 () {
  const W = 920, H = 548
  const x = 40, w = W - 80
  let b = title(W, '架构总览 · 一芯多壳（One Core, Multi-Shell）', '12 crates（+worker-sdk）· ~25k 域 LOC · edition 2024 · 工具链 1.97.1 · 语义中立内核零框架/零平台依赖')
  // 三壳
  const shW = (w - 2 * 16) / 3
  b += cell(x, 74, shW, 46, P.violet, '① 独立 server bin', 'cmx-flow-server · :8091')
  b += cell(x + shW + 16, 74, shW, 46, P.violet, '② 平台反代壳', 'cmx-flow-api · 门户内嵌(HTTP)')
  b += cell(x + 2 * (shW + 16), 74, shW, 46, P.violet, '③ 可嵌 Web Component', 'web/elements · 框架无关')
  b += LINE(W / 2, 120, W / 2, 140)
  // app 核
  b += band(x, 142, w, 50, P.blue, 'cmx-flow-app · 平台中立应用核', 'flow_routes::<S>() 泛型路由 + 引擎单例 + 全 handler + 模拟/协同/决策查看端点 + 身份/维度回连', '7,508')
  b += LINE(W / 2, 192, W / 2, 210)
  // 注入层 4 格
  const iW = (w - 3 * 12) / 4
  const inj = [['cmx-flow-store-pg', 'PG 持久化', P.aqua], ['cmx-flow-adapters', 'HTTP/Mock 注入', P.aqua], ['cmx-flow-identity', '内置身份 fid_*', P.aqua], ['cmx-flow-def', '定义持久化', P.aqua]]
  inj.forEach((it, i) => { b += cell(x + i * (iW + 12), 212, iW, 44, it[2], it[0], it[1]) })
  b += LINE(W / 2, 256, W / 2, 274)
  // engine
  b += band(x, 276, w, 48, P.blue, 'cmx-flow-engine · 令牌执行内核 Engine<S: RuntimeStore>', '等待态即提交点（pause+persist）· JavaDelegate/DelegateRegistry · 变量历史派生捕获 · InMemoryStore', '5,016')
  b += LINE(W / 2, 324, W / 2, 340)
  // bpmn
  b += band(x, 342, w, 44, P.orange, 'cmx-flow-bpmn · BPMN 2.0 XML → 中立 IR 编译器', 'compile(xml) → ProcessDefinition · 不支持元素编译期显式报错', '1,507')
  b += LINE(W / 2, 386, W / 2, 402)
  // model
  b += band(x, 404, w, 48, P.violet, 'cmx-flow-model · 语义中立内核（IR + 运行态 + RuntimeStore trait）', 'ProcessDefinition/FlowNode/Token/Task/Variables · 条件求值 · VarChangeRecord · 可 wasm/嵌入', '4,474')
  // footer
  b += R(x, 466, w, 52, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(x + 16, 486, '单向借用 cmx-container 基础库（仅编译期 path 依赖，无反向引用）：', { size: 11.5, w: 700, fill: P.ink2 })
  b += T(x + 16, 505, 'cmx-database-pg · cmx-core · cmx-web-chassis · cmx-web-monitor · cmx-service-base', { size: 11.5, fill: P.muted, mono: true })
  return doc(W, H, b)
}

// ══════════════ 图2 · 三种部署姿态 ══════════════
function fig2 () {
  const W = 920, H = 430
  let b = title(W, '三种部署姿态 · 同一引擎核', '门户与引擎编译期解耦：门户不依赖引擎源码，纯 HTTP 反代（cargo tree 已验证切断）')
  const box = (x, y, w, h, hue, t, s) => cell(x, y, w, h, hue, t, s)
  const lane = (y, tag, tagHue) => { b += R(30, y, 96, 58, { rx: 9, fill: tagHue, fop: 0.14, stroke: tagHue, sop: 0.34, sw: 1 }); b += T(78, y + 26, tag.split('|')[0], { size: 12.5, w: 800, anchor: 'middle' }); b += T(78, y + 43, tag.split('|')[1], { size: 10.5, fill: P.ink2, anchor: 'middle' }) }
  // Row A 独立微服务
  let y = 78; lane(y, '① 独立|微服务', P.violet)
  b += box(150, y, 176, 58, P.blue, '门户 Portal', ':8080')
  b += LINE(326, y + 29, 396, y + 29); b += T(361, y + 20, 'FlowProxy', { size: 10, fill: P.muted, anchor: 'middle' }); b += T(361, y + 46, '/api/flow/*', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true })
  b += box(398, y, 176, 58, P.aqua, 'flow-server', 'cmx-flow-server · :8091')
  b += LINE(574, y + 29, 644, y + 29); b += T(609, y + 20, 'db-per-tenant', { size: 10, fill: P.muted, anchor: 'middle' })
  b += box(646, y, 234, 58, P.orange, 'PostgreSQL', 'fico + cmx · flow_<tenant>')
  // Row B 可嵌组件
  y = 168; lane(y, '② 可嵌|组件', P.magenta)
  b += box(150, y, 210, 58, P.green, '第三方 App', 'React / Vue / 原生')
  b += LINE(360, y + 29, 452, y + 29); b += T(406, y + 20, '<flow-designer>', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true }); b += T(406, y + 46, '<flow-todo> …', { size: 9.5, fill: P.muted, anchor: 'middle', mono: true })
  b += box(454, y, 426, 58, P.aqua, 'flow-server v1 API', '自定义元素直连 /api/flow/v1/*')
  // Row C headless
  y = 258; lane(y, '③ Headless|无界面', P.blue)
  b += box(150, y, 210, 58, P.violet, '自建前端 / 系统', '完全自绘 UI')
  b += LINE(360, y + 29, 452, y + 29); b += T(406, y + 21, 'REST + SSE', { size: 9.5, fill: P.muted, anchor: 'middle' }); b += T(406, y + 46, '+ OpenAPI', { size: 9.5, fill: P.muted, anchor: 'middle' })
  b += box(454, y, 426, 58, P.aqua, '/api/flow/v1/* · /events(SSE) · /docs', 'Swagger + 事件流 + API Key/JWT')
  // footer
  b += R(30, 338, W - 60, 58, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(46, 360, '后端一芯双壳：同一 cmx-flow-app 核，既可门户进程内嵌反代、也可独立 flow-server 部署；', { size: 11.5, fill: P.ink2, w: 600 })
  b += T(46, 380, '三层出站鉴权：X-API-Key（服务身份）+ X-Delegated-User-Token（真实办理人）+ X-Request-Id；多租户 db-per-tenant 物理隔离。', { size: 11.5, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图3 · 能力演进时间线（7 轨） ══════════════
function fig3 () {
  const W = 980
  const tracks = [
    ['引擎核心', 'M1–M5.3', P.blue, ['M1 定义/发起', 'M2 网关', 'M2.5 边界定时器', 'M3 多实例', 'M4 抄送/转签', 'M5.1 子流程', 'M5.2 组织路由', 'M5.3 多挂载']],
    ['BPMN 补齐', 'A1–A10', P.orange, ['A1 包容网关', 'A2 终止', 'A3 事件子流程', 'A5 扁平子过程', 'A6 活动历史', 'A7 外部 Worker', 'A8 错误边界', 'A9 实例迁移', 'A10 事件网关']],
    ['可靠性', 'H1–H4 · P1–P4', P.aqua, ['H1 热加载', 'H2 incident', 'H3 异步 Job', 'H4 运维视图', 'P1 SKIP-LOCKED 执行器', 'P2 死信队列', 'P4 令牌可视化']],
    ['人工审批', '', P.green, ['会签/或签', '转签/加签/委派', '抄送', '退回任意节点', '取回', '7 类办理人', '身份双模', '表单绑定/单据关联', '审批意见留痕']],
    ['维度路由', 'RD0–RD4', P.magenta, ['RD0 契约', 'RD1 注入', 'RD2 精确/继承/兜底', 'RD3 多挂载', 'RD4 设计器']],
    ['设计器', '', P.yellow, ['定义持久化', '四区画布', '属性面板', '变量声明', 'Excel 公式栏', '子流程钻入式', '模拟试跑', '版本 diff', '协同 M1（感知+防冲突）']],
    ['微服务', 'S0–S6', P.violet, ['S0 迁移', 'S1 适配器', 'S2 多租户', 'S3 headless', 'S4 前端抽核', 'S5 组件', 'S6 平台反代']],
    ['后端补齐', '0821', P.red, ['决策表落库持久化', '变量历史 + TTL/归档', 'RD5 HTTP 维度解析', '外部 worker SDK', '身份·维度回连端点']],
    ['大前端', '0822', P.orange, ['模拟：facts→trace+画布高亮', '版本 diff：结构级 XML 对比', '协同 M1：感知+防冲突', '远端选中高亮', '草稿保存乐观锁']],
    ['本轮增量', '0823', P.aqua, ['变量历史·引擎派生捕获（决策/子流程回填）', '决策表只读查看器', '变量历史时间线', '协同 M2：op-log 对象级合并', '令牌可视化：SSE实时+计数徽标', '等待态细分色', '设计器分页+大纲+缩略图', '办理人类型修复', '科技感 light/dark 主题（跟随门户 SAP 令牌）']],
  ]
  const x0 = 156, maxX = W - 34
  let rows = '', y = 78
  for (const [name, code, hue, items] of tracks) {
    const f = chipFlow(x0, y, maxX, items, hue, { size: 11, lh: 28 })
    const rowH = f.height
    // 轨标签
    rows += R(28, y - 4, 116, rowH - 4, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
    rows += R(28, y - 4, 4, rowH - 4, { rx: 2, fill: hue })
    rows += T(44, y + 14, name, { size: 13, w: 800 })
    if (code) rows += T(44, y + 31, code, { size: 10, fill: P.muted, mono: true })
    rows += f.svg
    y += rowH + 8
  }
  const H = y + 46
  let b = title(W, '能力演进时间线 · 全功能梳理', '十条能力轨，均已交付并回归通过（里程碑代号取自源码/测试文件命名）')
  b += rows
  b += R(28, y + 2, W - 56, 34, { rx: 9, fill: P.plane, stroke: P.border, sw: 1 })
  b += T(44, y + 24, '状态：M / A / P / H / RD / S 各轨 + 0821 后端补齐 + 0822 设计器大前端 + 0823 变量历史派生·决策/变量可视化·协同 M2·令牌可视化·主题化 —— 全部已交付并纳入回归。', { size: 11, fill: P.ink2 })
  return doc(W, H, b)
}

// ══════════════ 图5 · 测试覆盖 stat tiles ══════════════
function fig5 () {
  const W = 920, H = 476
  let b = title(W, '测试覆盖 · 真机验证', 'Rust 单测/集成 + 后端 curl 回归 + Playwright/CDP 前端；含 0823 变量历史派生 / 协同 M2 / 决策·变量可视化 + 0822 设计器大前端；均只增不删、数据留库')
  const tiles = [
    ['276', 'Rust 测试函数 · 0 失败', 'cargo test --workspace'],
    ['159/159', '后端全量回归', 'run-all.sh'],
    ['60/60', '子流程专项', 'run-subflow.sh'],
    ['25/25', '维度路由 RD0–4', '维度路由测试报告'],
    ['23/23', '子流程钻入(门户级)', 'subflow_drilldown.cjs'],
    ['22/22', '差旅业务 E2E', 'biz-test'],
    ['3/3', '变量历史·引擎派生 · 0823', 'var_history_derived.rs'],
    ['8/8', '决策/变量历史可视化 · 0823', 'viz_decision_varhistory.cjs'],
    ['5/5', '协同 M2 对象级合并 · 0823', 'collab_m2_oplog.cjs'],
    ['12/12', '设计器 模拟+diff · 0822', 'designer_simulate_diff.cjs'],
    ['10/10', '设计器新功能验收 · 0822', 'designer_features_capture.cjs'],
    ['6/6', '协同 M1 双用户 · 0822', 'collab_presence.cjs'],
  ]
  const cols = 4, gap = 16, x0 = 40, tw = (W - 80 - (cols - 1) * gap) / cols, th = 96, y0 = 78
  tiles.forEach((t, i) => {
    const cx = x0 + (i % cols) * (tw + gap), cy = y0 + Math.floor(i / cols) * (th + 16)
    b += R(cx, cy, tw, th, { rx: 12, fill: P.surface, stroke: P.border, sw: 1 })
    b += R(cx, cy, tw, 4, { rx: 2, fill: P.good })
    b += `<circle cx="${cx + 16}" cy="${cy + 26}" r="7" fill="${P.good}" fill-opacity="0.15"/><path d="M${cx + 12.5},${cy + 26} l2.5,2.5 l5,-5.5" stroke="${P.good}" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>`
    b += T(cx + tw / 2 + 8, cy + 48, t[0], { size: 27, w: 800, anchor: 'middle', fill: P.good, mono: true })
    b += T(cx + tw / 2, cy + 68, t[1], { size: 11, w: 700, anchor: 'middle', fill: P.ink })
    b += T(cx + tw / 2, cy + 85, t[2], { size: 9.5, anchor: 'middle', fill: P.muted, mono: true })
  })
  return doc(W, H, b)
}

// ══════════════ 图6 · 未来计划路线图 ══════════════
function fig6 () {
  const W = 940, H = 540
  let b = title(W, '未来计划 · Now / Next / Later', '审批赛道够用即止，世界级完整度按业务诉求择机推进')
  const colW = (W - 80 - 2 * 18) / 3, x0 = 40, y0 = 74
  const cols = [
    ['Now · 已巩固', P.good, ['审批全链路（会签/转签/抄送/退回/取回）', '子流程维度路由 RD0–5', '异步可靠性 incident/死信/SKIP-LOCKED', 'S0–S6 独立微服务 + 平台反代', '四区设计器 + 子流程钻入 + light/dark 主题化', '★ 0822：设计器模拟 / diff / 协同 M1（感知+防冲突）', '★ 0823：变量历史派生 · 决策/变量可视化 · 协同 M2 · 令牌可视化优化']],
    ['Next · 近期择机', P.blue, ['协同 M3（结构级增删/移动合并）', '决策表可编辑设计器（现为只读查看）', '令牌可视化 · 生命周期回放', 'SSE JWT 鉴权（协同非 off 模式）']],
    ['Later · B 级长期', P.violet, ['补偿事件（B2）', '全事件体系 信号/升级/条件/link（B4）', '完整 FEEL/DMN + DRD 序列化（B6）', '水平扩展执行（B8 · 暂不追）', '附件（接文档服务）', 'ZmcDataSet 数据集导出端点']],
  ]
  const CH = 410 // 卡片高度（容 Now 列 7 项）
  cols.forEach(([hd, hue, items], ci) => {
    const cx = x0 + ci * (colW + 18)
    b += R(cx, y0, colW, CH, { rx: 12, fill: P.surface, stroke: P.border, sw: 1 })
    b += R(cx, y0, colW, 34, { rx: 12, fill: hue, fop: 0.14 })
    b += R(cx, y0 + 22, colW, 12, { fill: hue, fop: 0.14 })
    b += R(cx, y0, 4, CH, { rx: 2, fill: hue })
    b += T(cx + 16, y0 + 22, hd, { size: 13.5, w: 800 })
    items.forEach((it, i) => {
      const iy = y0 + 52 + i * 47
      b += `<circle cx="${cx + 18}" cy="${iy}" r="3" fill="${hue}"/>`
      // 换行长文本
      const words = it, max = colW - 40
      if (wOf(words, 12) <= max) { b += T(cx + 30, iy + 4, words, { size: 12, fill: P.ink }) }
      else {
        // 简单二分断行
        let cut = words.length
        while (cut > 1 && wOf(words.slice(0, cut), 12) > max) cut--
        // 尽量在括号/空格断
        let br = words.slice(0, cut)
        const sp = Math.max(br.lastIndexOf('（'), br.lastIndexOf(' '), br.lastIndexOf('/'))
        if (sp > cut * 0.5) cut = sp
        b += T(cx + 30, iy - 3, words.slice(0, cut), { size: 12, fill: P.ink })
        b += T(cx + 30, iy + 13, words.slice(cut), { size: 12, fill: P.ink })
      }
    })
  })
  // 正交/坚决不做
  b += R(x0, y0 + CH + 8, W - 80, 46, { rx: 10, fill: P.plane, stroke: P.border, sw: 1 })
  b += R(x0, y0 + CH + 8, 4, 46, { rx: 2, fill: P.serious })
  b += T(x0 + 18, y0 + CH + 30, '正交 / 坚决不做', { size: 12, w: 800, fill: P.ink })
  b += T(x0 + 18, y0 + CH + 46, '引擎不认字典/组织/DB（维度经注入 resolver，唯一事实源）· 完整规则能力配 cmx-rulesengine · 水平扩展投产比低暂不追', { size: 11, fill: P.muted })
  return doc(W, H, b)
}

// ══════════════ 图4 · BPMN 2.0 能力地图（覆盖矩阵） ══════════════
function fig4 () {
  const W = 980
  const cats = [
    ['任务', P.blue, [['userTask', 'ok'], ['serviceTask 同步', 'ok'], ['serviceTask 异步', 'ok'], ['外部 Worker', 'ok'], ['businessRuleTask', 'ok'], ['callActivity', 'ok'], ['scriptTask', 'no'], ['send/receive/manualTask', 'no']]],
    ['网关', P.orange, [['排他 XOR', 'ok'], ['并行 AND', 'ok'], ['包容 OR', 'ok'], ['事件网关', 'ok'], ['复杂网关', 'no']]],
    ['事件', P.aqua, [['none 起/止', 'ok'], ['消息启动', 'ok'], ['终止结束', 'ok'], ['边界定时·中断', 'ok'], ['边界定时·非中断', 'ok'], ['边界错误', 'ok'], ['中间消息捕获', 'ok'], ['中间定时捕获', 'ok'], ['事件子流程·错误中断', 'warn'], ['定时启动', 'no'], ['错误结束', 'no'], ['信号/补偿/升级/条件/link', 'no']]],
    ['多实例', P.green, [['会签·并行', 'ok'], ['或签·串行', 'ok'], ['完成条件', 'ok'], ['实例取消', 'ok'], ['逐元素动态派人', 'ok'], ['loopCardinality', 'no']]],
    ['子流程', P.magenta, [['同步子流程', 'ok'], ['内嵌子过程', 'ok'], ['组织/维度路由', 'ok'], ['三级解析', 'ok'], ['多挂载', 'ok'], ['变量映射 in/out', 'ok'], ['绑定表', 'ok']]],
    ['可靠性·运维', P.blue, [['异步 Job·SKIP-LOCKED', 'ok'], ['死信队列', 'ok'], ['incident+重试', 'ok'], ['错误边界', 'ok'], ['活动历史', 'ok'], ['实例迁移', 'ok'], ['运行时干预', 'ok'], ['令牌可视化·前端', 'warn']]],
    ['人工·租户·表单', P.violet, [['7 类办理人', 'ok'], ['候选池+认领', 'ok'], ['转签/加签/委派', 'ok'], ['抄送', 'ok'], ['退回任意/取回', 'ok'], ['db-per-tenant', 'ok'], ['JWT/APIKey/委托令牌', 'ok'], ['表单绑定/biz_link', 'ok']]],
  ]
  const x0 = 152, maxX = W - 30
  let rows = '', y = 100
  for (const [name, hue, items] of cats) {
    const f = statusFlow(x0, y, maxX, items)
    const rowH = f.height
    rows += R(28, y - 4, 116, rowH - 4, { rx: 9, fill: hue, fop: 0.10, stroke: hue, sop: 0.30, sw: 1 })
    rows += R(28, y - 4, 4, rowH - 4, { rx: 2, fill: hue })
    rows += T(44, y + 13, name, { size: 12, w: 800 })
    rows += f.svg
    y += rowH + 8
  }
  const H = y + 16
  let b = title(W, 'BPMN 2.0 能力地图 · 覆盖矩阵', '审批赛道为先：核心构件全覆盖；未支持项多为审批场景少用或按设计外置（如完整规则配 cmx-rulesengine）')
  // 图例
  let lx = W / 2 - 220
  for (const [lab, st] of [['已交付', 'ok'], ['部分/进行', 'warn'], ['暂不支持（按需/择机）', 'no']]) {
    const c = statusCell(lx, 64, lab, st); b += c.svg; lx += c.w + 12
  }
  b += rows
  return doc(W, H, b)
}

const figs = { 'fig-1-architecture': fig1(), 'fig-2-deployment': fig2(), 'fig-3-timeline': fig3(), 'fig-4-bpmn-map': fig4(), 'fig-5-tests': fig5(), 'fig-6-roadmap': fig6() }
for (const [k, v] of Object.entries(figs)) { writeFileSync(join(DIR, k + '.svg'), v); console.log(`${k}.svg: ${v.length}B`) }
console.log(`\n${Object.keys(figs).length} figs written`)
