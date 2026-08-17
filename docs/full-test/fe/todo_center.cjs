// Tier-2c：待办中心 <flow-todo> 加载 + 列出 u_fin1 待办（提交/退回/转签等操作入口）
const { chromium } = require('playwright');
const path = require('path');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const SHOTS = path.join(__dirname, 'shots');
const results = [];
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); };

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1500, height: 950 } });
  await ctx.addInitScript(() => { try { localStorage.setItem('cmx_user_id', 'u_fin1'); localStorage.setItem('cmx_username', 'u_fin1'); } catch {} });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));

  await page.goto('http://127.0.0.1:9099/demo/index.html', { waitUntil: 'domcontentloaded' });
  await page.fill('#apiBase', 'http://127.0.0.1:8091');
  await page.click('[data-tab="todo"]');
  await page.waitForTimeout(2500);

  await page.waitForFunction(() => !!document.querySelector('flow-todo')?.shadowRoot, { timeout: 8000 }).catch(() => {});
  A('FE-TODO-mount', '<flow-todo> 挂载并建立 shadowRoot', await page.evaluate(() => !!document.querySelector('flow-todo')?.shadowRoot));

  const info = await page.evaluate(() => {
    const deep = sel => { const r = []; const w = root => { root.querySelectorAll(sel).forEach(e => r.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && w(e.shadowRoot)); }; w(document); return r; };
    const text = (document.querySelector('flow-todo')?.shadowRoot?.textContent) || '';
    // 待办行/卡片：常见 li / [data-task-id] / .todo-item / row
    const rows = deep('[data-task-id], .todo-item, .flow-todo-item, li').filter(e => /审批|审核|会签|一级|二级|三级|财务|出纳|经理|总监|产品/.test(e.textContent || ''));
    const tabs = deep('*').filter(e => /待办|已办|我发起|抄送|可认领/.test(e.textContent || '') && (e.tagName === 'BUTTON' || /tab/i.test(e.className || ''))).map(e => e.textContent.trim().slice(0, 8));
    return { rowCount: rows.length, hasTodoWord: /待办|审批|流程|任务/.test(text), tabs: [...new Set(tabs)].slice(0, 8), len: text.length };
  });
  await page.screenshot({ path: path.join(SHOTS, '06-todo-center.png'), fullPage: true });
  A('FE-TODO-render', '待办中心渲染业务内容', info.hasTodoWord, `len=${info.len}`);
  A('FE-TODO-tabs', '待办中心含分类页签(待办/已办/我发起/抄送)', info.tabs.length >= 2, `tabs=${JSON.stringify(info.tabs)}`);
  A('FE-TODO-rows', 'u_fin1 待办列表有任务行', info.rowCount >= 1, `rows=${info.rowCount}`);
  A('FE-TODO-noerr', '待办中心无 pageerror', errors.length === 0, errors.slice(0, 2).join(' | ').slice(0, 160));

  await browser.close();
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== FE Tier2c todo-center: ${pass}/${results.length} ====`);
  require('fs').writeFileSync(path.join(__dirname, 'tier2c-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
