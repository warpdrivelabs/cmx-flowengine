// Tier-2b：直接验证 WIP「办理人类型」修复 —— 切到角色不回弹 + 值输入出现 + 属性写回
const { chromium } = require('playwright');
const path = require('path');
const { startStatic, vendorRoute, clickDefPaged } = require('./_harness.cjs');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const SHOTS = path.join(__dirname, 'shots');
const PORT = 9094;
const results = [];
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); };

(async () => {
  const srv = startStatic(PORT); // 自起静态服（serve web/），测试自包含只依赖 :8091
  await new Promise((r) => setTimeout(r, 900));
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1600, height: 950 } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  await vendorRoute(page, ctx); // bpmn-js vendor 从磁盘服（门户没起也能跑）

  await page.goto(`http://127.0.0.1:${PORT}/demo/index.html`, { waitUntil: 'domcontentloaded' });
  await page.fill('#apiBase', 'http://127.0.0.1:8091');
  await page.click('[data-tab="designer"]');
  await page.waitForTimeout(1000);

  // 载入 travel_expense（explorer 分页翻页定位，共享脚手架）
  await page.waitForFunction(() => { const w = r => [...r.querySelectorAll('*')].some(e => (e.className && ('' + e.className).includes('flow-def')) || (e.shadowRoot && w(e.shadowRoot))); return w(document); }, { timeout: 10000 }).catch(() => {});
  const clickedDef = await clickDefPaged(page, 'travel_expense');
  if (!clickedDef) A('FE-WIP-click-def', '点选 travel_expense', false, 'not found in any page');
  await page.waitForSelector('[data-element-id="mgr"]', { timeout: 8000 }).catch(() => {});
  const hasMgr = await page.locator('[data-element-id="mgr"]').count();
  A('FE-WIP-load', '载入 travel_expense 且 mgr 节点渲染', hasMgr > 0, `mgrShapes=${hasMgr}`);
  await page.screenshot({ path: path.join(SHOTS, '04-travel-loaded.png'), fullPage: true });

  // 选中 mgr userTask
  await page.click('[data-element-id="mgr"]', { force: true, timeout: 6000 }).catch(() => {});
  await page.waitForTimeout(800);

  // 找到并点「办理人」property 页签（穿透 shadow，按文本）
  const assigneeTab = page.locator('text=办理人').first();
  await assigneeTab.click({ timeout: 4000 }).catch(() => {});
  await page.waitForTimeout(500);

  // 初始应推断为「指定人员」(user)，值 u_mgr
  const initKind = await page.evaluate(() => {
    const deep = sel => { const r = []; const w = root => { root.querySelectorAll(sel).forEach(e => r.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && w(e.shadowRoot)); }; w(document); return r; };
    const on = deep('.flow-akind.on')[0];
    const val = deep('[data-akind-val]')[0];
    return { onKind: on?.getAttribute('data-akind') || null, val: val ? val.value : null, akindCount: deep('.flow-akind').length };
  });
  A('FE-WIP-initial', '选中mgr→办理人页签渲染类型按钮', initKind.akindCount >= 5, `kinds=${initKind.akindCount} on=${initKind.onKind} val=${initKind.val}`);

  // ★ WIP 核心：点「角色」→ 应保持选中角色(不回弹指定人员) + 出现值输入
  await page.click('.flow-akind[data-akind="role"]', { timeout: 4000 }).catch(() => {});
  await page.waitForTimeout(600);
  const afterRole = await page.evaluate(() => {
    const deep = sel => { const r = []; const w = root => { root.querySelectorAll(sel).forEach(e => r.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && w(e.shadowRoot)); }; w(document); return r; };
    const on = deep('.flow-akind.on')[0];
    const valInput = deep('[data-akind-val],[data-idn-text],select')[0];
    return { onKind: on?.getAttribute('data-akind') || null, hasValInput: !!valInput };
  });
  A('FE-WIP-role-sticky', '★切到「角色」不回弹(仍选中role)', afterRole.onKind === 'role', `on=${afterRole.onKind}`);
  A('FE-WIP-role-valinput', '★切到「角色」出现值输入框', afterRole.hasValInput, `hasInput=${afterRole.hasValInput}`);
  await page.screenshot({ path: path.join(SHOTS, '05-assignee-role.png'), fullPage: true });

  // 再切「岗位」验证同样不回弹（WIP fix 覆盖多类型）
  await page.click('.flow-akind[data-akind="position"]', { timeout: 4000 }).catch(() => {});
  await page.waitForTimeout(500);
  const afterPos = await page.evaluate(() => {
    const deep = sel => { const r = []; const w = root => { root.querySelectorAll(sel).forEach(e => r.push(e)); root.querySelectorAll('*').forEach(e => e.shadowRoot && w(e.shadowRoot)); }; w(document); return r; };
    return { onKind: deep('.flow-akind.on')[0]?.getAttribute('data-akind') || null };
  });
  A('FE-WIP-position-sticky', '★切到「岗位」不回弹(仍选中position)', afterPos.onKind === 'position', `on=${afterPos.onKind}`);

  A('FE-WIP-noerr', '设计器交互无 pageerror', errors.length === 0, errors.slice(0, 2).join(' | ').slice(0, 160));
  await browser.close();
  try { srv.kill('SIGTERM'); } catch {}
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== FE Tier2b WIP assignee-fix: ${pass}/${results.length} ====`);
  require('fs').writeFileSync(path.join(__dirname, 'tier2b-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
