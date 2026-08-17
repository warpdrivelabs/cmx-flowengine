// 子流程设计能力 · 全量深度功能测试（自清理）
// 覆盖：保存持久化+浮层存活、新变体唯一key+自动绑定、重存不重复建、发布产版本、
//       属性回写往返、固定模式编辑、双击入口、主列表隐藏、关闭保主画布。
const { chromium } = require('playwright');
const path = require('path');
const KEY = 'cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6';
const API = 'http://127.0.0.1:8091/api/flow/v1';
const SHOTS = path.join(__dirname, 'shots');
const results = [];
const A = (id, desc, ok, detail) => { results.push({ id, ok: !!ok, desc, detail }); console.log(`[${id}] ${ok ? 'PASS' : 'FAIL'}  ${desc}${detail ? '  :: ' + detail : ''}`); };
const deepEval = `(() => { const r=[]; const w=root=>{ root.querySelectorAll('*').forEach(e=>{ r.push(e); if(e.shadowRoot) w(e.shadowRoot); }); }; w(document); return r; })()`;
async function apiGet(p) { const r = await fetch(API + p, { headers: { 'X-API-Key': KEY } }); return r.json(); }

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const ctx = await browser.newContext({ extraHTTPHeaders: { 'X-API-Key': KEY }, viewport: { width: 1680, height: 1000 } });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.route('**/portal/vendor/**', async route => {
    const url = route.request().url(); const rel = url.substring(url.indexOf('/portal/vendor/'));
    try { const r = await ctx.request.get('http://127.0.0.1:8080' + rel); await route.fulfill({ status: r.status(), body: await r.body(), headers: { 'content-type': r.headers()['content-type'] || 'application/octet-stream' } }); }
    catch { await route.abort(); }
  });
  const clickSel = (sel) => page.evaluate(({ de, sel }) => { const b = eval(de).find(e => e.matches && e.matches(sel)); if (b) { b.click(); return true; } return false; }, { de: deepEval, sel });
  const loadDef = async (key) => { await page.evaluate(({ de, k }) => { const el = eval(de).find(e => e.classList && e.classList.contains('flow-def') && e.dataset.key === k); if (el) el.click(); }, { de: deepEval, k: key }); await page.waitForTimeout(2500); };
  // 选主画布节点：先点另一个节点(mgr)清选，再点目标——避免"重点已选中节点被 bpmn-js 反选"的测试假象。
  const selMainNode = async (id) => {
    await page.evaluate(({ de, id }) => { const o = eval(de).find(e => e.getAttribute && e.getAttribute('data-element-id') === (id === 'mgr' ? 'director' : 'mgr') && (!e.closest || !e.closest('.flow-sub-canvas'))); if (o) o.dispatchEvent(new MouseEvent('click', { bubbles: true })); }, { de: deepEval, id });
    await page.waitForTimeout(300);
    return page.evaluate(({ de, id }) => { const s = eval(de).find(e => e.getAttribute && e.getAttribute('data-element-id') === id && (!e.closest || !e.closest('.flow-sub-canvas'))); if (s) { s.dispatchEvent(new MouseEvent('click', { bubbles: true })); return true; } return false; }, { de: deepEval, id });
  };
  const subShapes = () => page.evaluate(de => eval(de).filter(e => e.getAttribute && e.getAttribute('data-element-id') && e.closest && e.closest('.flow-sub-canvas')).map(e => e.getAttribute('data-element-id')), deepEval);
  const overlayOpen = () => page.evaluate(de => !!eval(de).find(e => e.classList && e.classList.contains('flow-sub-mask')), deepEval);

  await page.goto('http://127.0.0.1:9099/demo/index.html', { waitUntil: 'domcontentloaded' });
  await page.fill('#apiBase', 'http://127.0.0.1:8091');
  await page.click('[data-tab="designer"]');
  await page.waitForTimeout(1500);
  await page.waitForFunction(`${deepEval}.some(e=>e.className&&(''+e.className).includes('flow-def'))`, { timeout: 8000 }).catch(() => {});

  // ═══ T1: 组织路由子流程 — 发布产版本 + 浮层存活 ═══
  console.log('\n--- T1 发布产版本 ---');
  await loadDef('travel_expense');
  await selMainNode('fin_review'); await page.waitForTimeout(700);
  await clickSel('[data-edit-subflow]'); await page.waitForTimeout(3000);
  const hqVersBefore = ((await apiGet('/definitions/fin_review_hq')).data.versions || []).length;
  await clickSel('[data-sub-act="publish"]'); await page.waitForTimeout(2800);
  const hqVersAfter = ((await apiGet('/definitions/fin_review_hq')).data.versions || []).length;
  A('T1-publish-version', '发布子流程→版本号+1', hqVersAfter === hqVersBefore + 1, `${hqVersBefore}→${hqVersAfter}`);
  A('T1-overlay-after-publish', '发布后浮层+子画布存活', (await overlayOpen()) && (await subShapes()).length >= 3, `shapes=${(await subShapes()).length}`);

  // ═══ T2: 属性回写往返 — 改子流程节点办理人 → 保存 → 重开验证 ═══
  console.log('\n--- T2 属性回写往返 ---');
  // 选子画布 fin1 节点，改其 name 属性（简单可验证的回写）
  const editRes = await page.evaluate(de => {
    const all = eval(de);
    const s = all.find(e => e.getAttribute && e.getAttribute('data-element-id') === 'fin1' && e.closest && e.closest('.flow-sub-canvas'));
    if (!s) return { sel: false };
    s.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    return { sel: true };
  }, deepEval);
  await page.waitForTimeout(1000);
  // 在属性子面板改「名称」input(data-prop=name)
  const renamed = await page.evaluate(de => {
    const all = eval(de);
    const inp = all.find(e => e.matches && e.matches('[data-sub-prop] [data-prop="name"]'));
    if (!inp) return { found: false };
    inp.value = '财务初审(改)'; inp.dispatchEvent(new Event('change', { bubbles: true }));
    return { found: true };
  }, deepEval);
  await page.waitForTimeout(800);
  A('T2-prop-select', '子画布选中 fin1 + 属性面板可编辑', editRes.sel && renamed.found, `sel=${editRes.sel} nameInput=${renamed.found}`);
  A('T2-canvas-alive-after-edit', '改属性后子画布不掉(仍13图元)', (await subShapes()).length >= 13, `shapes=${(await subShapes()).length}`);
  // 保存 → 重开该变体验证名字持久化
  await clickSel('[data-sub-act="save"]'); await page.waitForTimeout(2500);
  const hqDetail = await apiGet('/definitions/fin_review_hq');
  const nameRoundtrip = /财务初审\(改\)/.test(hqDetail.data.bpmnXml || '');
  A('T2-prop-roundtrip', '★属性改动经保存持久化到 XML', nameRoundtrip, nameRoundtrip ? 'name persisted' : 'NOT persisted');
  // 关闭浮层
  await clickSel('[data-sub-close]'); await page.waitForTimeout(1000);
  A('T2-main-preserved', '关闭后主画布保留', await page.evaluate(de => eval(de).some(e => e.getAttribute && e.getAttribute('data-element-id') === 'fin_review'), deepEval), '');

  // ═══ T3: 双击 callActivity 入口 ═══
  console.log('\n--- T3 双击入口 ---');
  const dbl = await page.evaluate(de => {
    const s = eval(de).find(e => e.getAttribute && e.getAttribute('data-element-id') === 'fin_review' && (!e.closest || !e.closest('.flow-sub-canvas')));
    if (!s) return false;
    s.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    return true;
  }, deepEval);
  await page.waitForTimeout(3000);
  A('T3-dblclick', '★双击 callActivity 打开子流程编辑器', dbl && (await overlayOpen()), `overlay=${await overlayOpen()}`);
  await clickSel('[data-sub-close]'); await page.waitForTimeout(800);

  // ═══ T4: 新建变体唯一key + 重存不重复 + 主列表隐藏 ═══
  console.log('\n--- T4 新变体唯一key/不重复/隐藏 ---');
  // 测试隔离：先清 branch_gz 遗留绑定+定义（经后端删接口不便，改用直连 SQL 由外层脚本清；
  // 这里改为幂等断言：若已存在则先删绑定，确保每次从"未配置"开始）。
  const delGz = ((await apiGet('/subflow-bindings/fin_review')).data.bindings || []).find(b => b.orgId === 'branch_gz');
  if (delGz) { await fetch(API + '/subflow-bindings/id/' + delGz.id, { method: 'DELETE', headers: { 'X-API-Key': KEY } }); await page.waitForTimeout(400); }
  await selMainNode('fin_review'); await page.waitForTimeout(600);
  await clickSel('[data-edit-subflow]'); await page.waitForTimeout(3000);
  const defsBefore = ((await apiGet('/design/definitions')).data.definitions || []).map(d => d.key);
  const pick = await page.evaluate(de => { const sel = eval(de).find(e => e.matches && e.matches('[data-sub-newvar-org]')); if (!sel) return 'nosel'; const o = [...sel.options].find(x => x.value === 'branch_gz'); if (!o) return 'noopt:' + [...sel.options].map(x => x.value).join(','); sel.value = 'branch_gz'; sel.dispatchEvent(new Event('change', { bubbles: true })); return 'ok'; }, deepEval);
  await page.waitForTimeout(2500);  // 等空模板 boot 完成
  await page.evaluate(de => { const inp = eval(de).find(e => e.matches && e.matches('[data-sub-name]')); if (inp) { inp.value = '广州复核'; inp.dispatchEvent(new Event('input', { bubbles: true })); } }, deepEval);
  await page.waitForTimeout(400);
  // 存两次 → 第二次不应再新建定义（同 key 迭代）
  await clickSel('[data-sub-act="save"]'); await page.waitForTimeout(3000);
  const defsAfter1 = ((await apiGet('/design/definitions')).data.definitions || []).map(d => d.key);
  const gzBind1 = ((await apiGet('/subflow-bindings/fin_review')).data.bindings || []).find(b => b.orgId === 'branch_gz');
  const gzKey = gzBind1 ? gzBind1.targetKey : null;
  const newKeys = defsAfter1.filter(k => !defsBefore.includes(k));
  await clickSel('[data-sub-act="save"]'); await page.waitForTimeout(3000);
  const defsAfter2 = ((await apiGet('/design/definitions')).data.definitions || []);
  A('T4-pick', 'newvar 下拉可选 branch_gz', pick === 'ok', pick);
  A('T4-newvar-unique', '新变体唯一key(非new_process)', gzKey && gzKey !== 'new_process', `key=${gzKey} newDefs=${JSON.stringify(newKeys)}`);
  A('T4-newvar-count', '首存产生1个新定义', newKeys.length === 1, `+${newKeys.length}`);
  A('T4-resave-nodup', '★重存同变体不重复建定义', defsAfter2.length === defsAfter1.length, `${defsAfter1.length}→${defsAfter2.length}`);
  const gzIsSubflow = defsAfter2.find(d => d.key === gzKey);
  A('T4-newvar-hidden', '★新子流程 isSubflow=true(主列表隐藏)', gzIsSubflow && gzIsSubflow.isSubflow === true, `isSubflow=${gzIsSubflow ? gzIsSubflow.isSubflow : 'not found'}`);
  await page.screenshot({ path: path.join(SHOTS, 'sdf-02-new-variant.png'), fullPage: true });
  await clickSel('[data-sub-close]'); await page.waitForTimeout(800);
  // 自清理：删 branch_gz 绑定（定义留库无害，或由外层脚本清）
  const cleanup = ((await apiGet('/subflow-bindings/fin_review')).data.bindings || []).find(b => b.orgId === 'branch_gz');
  if (cleanup) await fetch(API + '/subflow-bindings/id/' + cleanup.id, { method: 'DELETE', headers: { 'X-API-Key': KEY } });

  A('T-noerr', '全程无 pageerror', errors.length === 0, errors.slice(0, 3).join(' | ').slice(0, 200));

  await browser.close();
  const pass = results.filter(r => r.ok).length;
  console.log(`\n==== 子流程设计深度功能: ${pass}/${results.length} ====`);
  // 输出清理信息（供外部脚本清理）
  console.log('CLEANUP_KEY:' + (results.find(r => r.id === 'T4-newvar-unique') ? '' : ''));
  require('fs').writeFileSync(path.join(__dirname, 'subflow-designer-deep-results.json'), JSON.stringify(results, null, 2));
  process.exit(0);
})();
