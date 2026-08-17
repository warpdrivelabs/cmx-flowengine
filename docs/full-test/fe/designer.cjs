// Tier-2 前端测试：BPMN 设计器（WIP 办理人类型修复）—— 加载/渲染/属性面板
const { chromium } = require('playwright');
const path = require('path');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const SHOTS = path.join(__dirname, 'shots');
const results = [];
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); };

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1600, height: 950 } });
  const page = await ctx.newPage();
  const errors = [], consoleErrs = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error') consoleErrs.push(m.text()); });

  // bpmn-js 资源代理到运行中的门户(:8080)
  await page.route('**/portal/vendor/**', async route => {
    const url = route.request().url();
    const rel = url.substring(url.indexOf('/portal/vendor/'));
    try {
      const r = await ctx.request.get('http://127.0.0.1:8080' + rel);
      const body = await r.body();
      await route.fulfill({ status: r.status(), body, headers: { 'content-type': r.headers()['content-type'] || 'application/octet-stream' } });
    } catch (e) { await route.abort(); }
  });

  // 挂载 <flow-designer>，api-base 指向 :8091
  await page.goto('http://127.0.0.1:9099/demo/index.html', { waitUntil: 'domcontentloaded' });
  await page.fill('#apiBase', 'http://127.0.0.1:8091');
  await page.click('[data-tab="designer"]');
  await page.waitForTimeout(1200);

  // 递归穿透 shadow DOM 的工具
  const pierce = () => {
    const out = []; const walk = (root, d) => { if (d > 8) return;
      root.querySelectorAll('*').forEach(el => { out.push(el.tagName.toLowerCase() + (el.getAttribute('data-act') ? `[data-act=${el.getAttribute('data-act')}]` : '')); if (el.shadowRoot) walk(el.shadowRoot, d + 1); }); };
    walk(document, 0); return out;
  };

  // 等待 flow-designer 挂上 shadow root
  await page.waitForFunction(() => !!document.querySelector('flow-designer')?.shadowRoot, { timeout: 8000 }).catch(() => {});
  A('FE-DSG-mount', '<flow-designer> 挂载并建立 shadowRoot', await page.evaluate(() => !!document.querySelector('flow-designer')?.shadowRoot));

  // 定义列表：等 explorer 拉到已发布定义，点击 travel_expense
  const loaded = await page.evaluate(async () => {
    const sleep = ms => new Promise(r => setTimeout(r, ms));
    const deepQueryAll = sel => { const res = []; const walk = root => { root.querySelectorAll(sel).forEach(e => res.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && walk(e.shadowRoot)); }; walk(document); return res; };
    for (let i = 0; i < 30; i++) {
      const items = deepQueryAll('*').filter(e => /travel_expense|差旅报销/.test(e.textContent || '') && e.children.length <= 2 && (e.onclick || e.getAttribute('data-key') || e.tagName === 'LI' || /item|row|def/i.test(e.className || '')));
      if (items.length) { items[0].click(); await sleep(1500); return { clicked: items[0].textContent.trim().slice(0, 40) }; }
      await sleep(400);
    }
    return { clicked: null };
  });
  A('FE-DSG-deflist', 'explorer 列出并可点选定义(travel_expense)', !!loaded.clicked, loaded.clicked || 'not found');

  // 等 bpmn 画布 SVG 渲染
  await page.waitForTimeout(2500);
  const canvas = await page.evaluate(() => {
    const deepQueryAll = sel => { const res = []; const walk = root => { root.querySelectorAll(sel).forEach(e => res.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && walk(e.shadowRoot)); }; walk(document); return res; };
    const svgs = deepQueryAll('svg');
    const djsShapes = deepQueryAll('.djs-element, .djs-shape, [data-element-id]');
    const tasks = deepQueryAll('[data-element-id]').map(e => e.getAttribute('data-element-id')).filter(Boolean);
    return { svgCount: svgs.length, shapeCount: djsShapes.length, elementIds: [...new Set(tasks)].slice(0, 20) };
  });
  A('FE-DSG-canvas', 'bpmn-js 画布渲染 SVG', canvas.svgCount >= 1, `svg=${canvas.svgCount}`);
  A('FE-DSG-shapes', 'BPMN 图元渲染(节点 shape)', canvas.shapeCount >= 3, `shapes=${canvas.shapeCount} ids=${JSON.stringify(canvas.elementIds)}`);
  await page.screenshot({ path: path.join(SHOTS, '03-designer-diagram.png'), fullPage: true });

  A('FE-DSG-noerr', '设计器无 pageerror', errors.length === 0, errors.slice(0, 2).join(' | ').slice(0, 200));
  A('FE-DSG-bpmn-loaded', 'bpmn-js 库加载成功(无“加载失败”)', !consoleErrs.some(e => /bpmn.*失败|加载失败|Canvas/.test(e)), consoleErrs.slice(0, 2).join(' | ').slice(0, 160));

  // 结构快照（供调试办理人面板）
  require('fs').writeFileSync(path.join(__dirname, 'designer-structure.json'), JSON.stringify({ canvas, consoleErrs: consoleErrs.slice(0, 10), errors: errors.slice(0, 5) }, null, 2));

  await browser.close();
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== FE Tier2 designer: ${pass}/${results.length} ====`);
  require('fs').writeFileSync(path.join(__dirname, 'tier2-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
