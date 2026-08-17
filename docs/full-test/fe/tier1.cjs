// Tier-1 前端测试：监控大盘 + Swagger + 微前端页模块服务完整性
const { chromium } = require('playwright');
const path = require('path');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const SHOTS = path.join(__dirname, 'shots');
const results = [];
function assert(id, desc, ok, detail) { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); }

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));

  // ── 1. 监控大盘 ──
  await page.goto('http://127.0.0.1:8091/', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1500); // 等一轮 /stats 轮询
  const dash = await page.evaluate(() => {
    const text = document.body.innerText;
    const canvases = document.querySelectorAll('canvas').length;
    // 抓大盘 KPI 数字（总实例/进行中/已完成等）
    const nums = (text.match(/\d[\d,]*/g) || []).slice(0, 30);
    return { len: text.length, canvases, hasFlowWord: /流程|实例|大盘|Flow|instance/i.test(text), sampleNums: nums.slice(0, 12), title: document.title };
  });
  await page.screenshot({ path: path.join(SHOTS, '01-dashboard.png'), fullPage: true });
  assert('FE-DASH-load', '大盘HTML加载(title含监控/流程)', /监控|流程|flow/i.test(dash.title), dash.title);
  assert('FE-DASH-canvas', '大盘含canvas图表(≥1)', dash.canvases >= 1, `canvas=${dash.canvases}`);
  assert('FE-DASH-content', '大盘渲染业务文案', dash.hasFlowWord, `len=${dash.len}`);
  assert('FE-DASH-noerr', '大盘无JS运行时错误', errors.length === 0, errors.join('|').slice(0, 160));

  // ── 2. Swagger UI （注意：无尾斜杠的 /docs 会 303 跳到丢失 /api 前缀的坏URL，须用 /docs/）──
  errors.length = 0;
  const docsNoSlash = await ctx.request.get('http://127.0.0.1:8091/api/flow/v1/docs', { maxRedirects: 0 }).catch(() => null);
  assert('FE-SWAGGER-redirect-bug', '【bug】/docs(无尾斜杠)303跳转丢失/api前缀→坏URL', docsNoSlash && docsNoSlash.status() === 303, docsNoSlash ? `303->${docsNoSlash.headers()['location']}` : 'n/a');
  await page.goto('http://127.0.0.1:8091/api/flow/v1/docs/', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1800);
  const sw = await page.evaluate(() => {
    const ops = document.querySelectorAll('.opblock, .opblock-summary-path, [data-path]').length;
    return { ops, hasSwagger: /swagger|openapi/i.test(document.body.innerHTML), title: document.title, len: document.body.innerText.length };
  });
  await page.screenshot({ path: path.join(SHOTS, '02-swagger.png'), fullPage: true });
  assert('FE-SWAGGER-load', 'Swagger UI 加载', sw.hasSwagger || sw.ops > 0, `ops=${sw.ops} title=${sw.title}`);
  assert('FE-SWAGGER-ops', 'Swagger 列出端点(≥1 opblock)', sw.ops >= 1, `opblocks=${sw.ops}`);

  // ── 3. 微前端页模块服务完整性（5 个 native page 都返回合法 JS + rev）──
  const pages = ['portal.flow.design-workbench','portal.flow.todo-center','portal.flow.task-form','portal.flow.identity-workbench','portal.flow.ops-console'];
  for (const id of pages) {
    const resp = await ctx.request.get(`http://127.0.0.1:8091/api/native-pages/${id}`);
    let ok = false, detail = `http ${resp.status()}`;
    if (resp.ok()) {
      const j = await resp.json();
      const src = j?.data?.source || '';
      const rev = j?.data?.rev;
      ok = src.length > 500 && !!rev;
      detail = `srcLen=${src.length} rev=${rev}`;
    }
    assert(`FE-NP-${id.split('.').pop()}`, `native-page ${id} 服务合法JS+rev`, ok, detail);
  }

  await browser.close();
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== FE Tier1: ${pass}/${results.length} ====`);
  require('fs').writeFileSync(path.join(__dirname, 'tier1-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
