// 子流程设计能力 CDP 验证：explorer 只显主流程 + callActivity 全屏子流程编辑器 + 变体侧栏
const { chromium } = require('playwright');
const path = require('path');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const SHOTS = path.join(__dirname, 'shots');
const results = [];
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); };
const deepEval = `(() => { const r=[]; const w=root=>{ root.querySelectorAll('*').forEach(e=>{ r.push(e); if(e.shadowRoot) w(e.shadowRoot); }); }; w(document); return r; })()`;

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1680, height: 1000 } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  // 代理 bpmn-js 资源到门户
  await page.route('**/portal/vendor/**', async route => {
    const url = route.request().url(); const rel = url.substring(url.indexOf('/portal/vendor/'));
    try { const r = await ctx.request.get('http://127.0.0.1:8080' + rel); await route.fulfill({ status: r.status(), body: await r.body(), headers: { 'content-type': r.headers()['content-type'] || 'application/octet-stream' } }); }
    catch { await route.abort(); }
  });

  await page.goto('http://127.0.0.1:9099/demo/index.html', { waitUntil: 'domcontentloaded' });
  await page.fill('#apiBase', 'http://127.0.0.1:8091');
  await page.click('[data-tab="designer"]');
  await page.waitForTimeout(1500);
  await page.waitForFunction(() => !!document.querySelector('flow-designer')?.shadowRoot, { timeout: 8000 }).catch(() => {});

  // ── 1. explorer 只显主流程（子流程不出现）──
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 8000 }).catch(() => {});
  const explorer = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const defKeys = all.filter(e => e.classList && e.classList.contains('flow-def')).map(e => e.dataset.key);
    return { keys: defKeys };
  }, deepEval);
  console.log('explorer keys:', JSON.stringify(explorer.keys));
  const subflowKeys = ['sub_review', 'sub_risk', 'fin_review_hq', 'fin_review_branch', 'sub_middle', 'sub_grandchild', 'sub_varchild'];
  const mainKeys = ['travel_expense', 'main_serial_multi', 'main_org_routed', 'cc_flow'];
  A('SD-explorer-nomain', 'explorer 含主流程(travel_expense 等)', mainKeys.every(k => explorer.keys.includes(k)), `mains present`);
  A('SD-explorer-nosubflow', 'explorer 不含子流程(sub_*/fin_review_*)', subflowKeys.every(k => !explorer.keys.includes(k)), `subflows hidden`);
  await page.screenshot({ path: path.join(SHOTS, 'sd-01-explorer-main-only.png'), fullPage: true });

  // ── 2. 「显示子流程」开关 → 子流程临时出现 ──
  const toggled = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const cb = all.find(e => e.matches && e.matches('[data-show-subflows]'));
    if (!cb) return { found: false };
    cb.click();
    return { found: true };
  }, deepEval);
  await page.waitForTimeout(600);
  const afterToggle = await page.evaluate(deepEval => eval(deepEval).filter(e => e.classList && e.classList.contains('flow-def')).map(e => e.dataset.key), deepEval);
  A('SD-toggle-exists', '「显示子流程」开关存在', toggled.found, '');
  A('SD-toggle-shows', '开关打开后子流程出现', subflowKeys.some(k => afterToggle.includes(k)), `now ${afterToggle.length} defs`);
  // 关回去
  await page.evaluate(deepEval => { const cb = eval(deepEval).find(e => e.matches && e.matches('[data-show-subflows]')); if (cb && cb.checked) cb.click(); }, deepEval);
  await page.waitForTimeout(400);

  // ── 3. 载入 travel_expense → 选 callActivity(fin_review) → 属性面板「编辑子流程」按钮 ──
  await page.evaluate(deepEval => { const el = eval(deepEval).find(e => e.classList && e.classList.contains('flow-def') && e.dataset.key === 'travel_expense'); if (el) el.click(); }, deepEval);
  await page.waitForTimeout(2500);
  // 选中 fin_review callActivity（在子 shadow 里点其 SVG 节点）
  const selected = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const shape = all.find(e => e.getAttribute && e.getAttribute('data-element-id') === 'fin_review');
    if (!shape) return { found: false };
    shape.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    return { found: true };
  }, deepEval);
  await page.waitForTimeout(1200);
  const editBtn = await page.evaluate(deepEval => {
    const b = eval(deepEval).find(e => e.matches && e.matches('[data-edit-subflow]'));
    return { present: !!b, disabled: b ? b.hasAttribute('disabled') : null };
  }, deepEval);
  A('SD-callact-select', '选中 fin_review callActivity', selected.found, '');
  A('SD-editbtn', '属性面板「编辑子流程」按钮存在且可用', editBtn.present && !editBtn.disabled, `present=${editBtn.present} disabled=${editBtn.disabled}`);
  await page.screenshot({ path: path.join(SHOTS, 'sd-02-callactivity-prop.png'), fullPage: true });

  // ── 4. 点「编辑子流程」→ 全屏浮层 + 变体侧栏 ──
  await page.evaluate(deepEval => { const b = eval(deepEval).find(e => e.matches && e.matches('[data-edit-subflow]')); if (b) b.click(); }, deepEval);
  await page.waitForTimeout(3000);  // 等浮层 + 子画布 boot + 变体载入
  const overlay = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const mask = all.find(e => e.classList && e.classList.contains('flow-sub-mask'));
    const variants = all.filter(e => e.matches && e.matches('[data-sub-variant]')).map(e => (e.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 30));
    const subCanvasSvg = all.filter(e => e.tagName === 'svg' && e.closest && e.closest('.flow-sub-canvas')).length;
    const crumb = (all.find(e => e.matches && e.matches('[data-sub-crumb]')) || {}).textContent || '';
    return { open: !!mask, variants, subCanvasSvg, crumb: crumb.replace(/\s+/g, ' ').trim() };
  }, deepEval);
  console.log('overlay:', JSON.stringify(overlay));
  A('SD-overlay-open', '全屏子流程编辑器浮层打开', overlay.open, '');
  A('SD-overlay-crumb', '面包屑显示 主流程›子流程', /差旅|travel|主流程|子流程/.test(overlay.crumb), overlay.crumb.slice(0, 50));
  A('SD-variant-sidebar', '变体侧栏列出组织变体(≥2)', overlay.variants.length >= 2, `variants=${JSON.stringify(overlay.variants)}`);
  A('SD-subcanvas', '子流程画布渲染 SVG', overlay.subCanvasSvg >= 1, `svg=${overlay.subCanvasSvg}`);
  await page.screenshot({ path: path.join(SHOTS, 'sd-03-subflow-editor.png'), fullPage: true });

  // ── 5. 切换变体 → 右画布载入不同子流程 ──
  const variantSwitch = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const btns = all.filter(e => e.matches && e.matches('[data-sub-variant]'));
    // 点一个非当前选中的变体
    const target = btns.find(b => !b.classList.contains('on')) || btns[1] || btns[0];
    if (!target) return { clicked: false };
    const key = target.getAttribute('data-sub-variant');
    target.click();
    return { clicked: true, key };
  }, deepEval);
  await page.waitForTimeout(2500);
  const afterSwitch = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const on = all.find(e => e.matches && e.matches('[data-sub-variant].on'));
    const subCanvasSvg = all.filter(e => e.tagName === 'svg' && e.closest && e.closest('.flow-sub-canvas')).length;
    return { onKey: on ? on.getAttribute('data-sub-variant') : null, subCanvasSvg };
  }, deepEval);
  A('SD-variant-switch', '切换变体后画布仍渲染(载入了目标子流程)', variantSwitch.clicked && afterSwitch.subCanvasSvg >= 1, `switched→${variantSwitch.key} on=${afterSwitch.onKey} svg=${afterSwitch.subCanvasSvg}`);
  await page.screenshot({ path: path.join(SHOTS, 'sd-04-variant-switched.png'), fullPage: true });

  // ── 6. 关闭浮层 → 主画布仍在（零丢失）──
  await page.evaluate(deepEval => { const b = eval(deepEval).find(e => e.matches && e.matches('[data-sub-close]')); if (b) b.click(); }, deepEval);
  await page.waitForTimeout(1200);
  const afterClose = await page.evaluate(deepEval => {
    const all = eval(deepEval);
    const maskGone = !all.find(e => e.classList && e.classList.contains('flow-sub-mask'));
    const mainShapes = all.filter(e => e.getAttribute && e.getAttribute('data-element-id') === 'fin_review').length;
    return { maskGone, mainShapes };
  }, deepEval);
  A('SD-close', '关闭浮层后主画布原样保留', afterClose.maskGone && afterClose.mainShapes >= 1, `maskGone=${afterClose.maskGone} mainFinReview=${afterClose.mainShapes}`);
  A('SD-noerr', '全程无 pageerror', errors.length === 0, errors.slice(0, 3).join(' | ').slice(0, 200));

  await browser.close();
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== 子流程设计 CDP: ${pass}/${results.length} ====`);
  require('fs').writeFileSync(path.join(__dirname, 'subflow-designer-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
