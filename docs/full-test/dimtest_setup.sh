#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# 子流程路由「维度泛化」端到端全量测试（RD0-RD4）
#   两个维度：组织机构(org, 内建, cmx_org 斜杠路径) + 产品(product, cf_product 点路径)
#   主流程 reimburse_dual 两挂载点：callBudget 按 org、callComp 按 product
#   覆盖：精确/沿树继承/多级继承/默认兜底/禁用跳过/同实例双维度各路由/端到端办结/独立分公司
#   测试数据全部保留入库（不清理）。生成完整测试报告。
# ═══════════════════════════════════════════════════════════════════════════
cd "$(dirname "$0")"
BASE="http://127.0.0.1:8091/api/flow/v1"
K="X-API-Key: cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6"
CT="Content-Type: application/json"
DEFS="defs/dimtest"
LOG="logs/dimtest-transcript.log"; : > "$LOG"
RES="logs/dimtest-results.tsv"; : > "$RES"
PASS=0; TOTAL=0

j() { local m="$1" p="$2" b="${3:-}"; local r
  if [ -n "$b" ]; then r=$(curl -s -X "$m" -H "$K" -H "$CT" -d "$b" "$BASE$p")
  elif [ "$m" = GET ]; then r=$(curl -s -H "$K" "$BASE$p")
  else r=$(curl -s -X "$m" -H "$K" "$BASE$p"); fi
  { echo "### $m $p"; [ -n "$b" ] && echo "REQ  $b"; echo "RESP $r"; echo; } >> "$LOG"; echo "$r"; }
A() { TOTAL=$((TOTAL+1)); local id="$1" desc="$2" exp="$3" got="$4"
  if [ "$exp" = "$got" ]; then PASS=$((PASS+1)); printf '  \033[32m✓\033[0m %-42s %s\n' "$id" "$desc"; echo -e "$id\tPASS\t$desc" >> "$RES"
  else printf '  \033[31m✗\033[0m %-42s %s\n     期望[%s] 实得[%s]\n' "$id" "$desc" "$exp" "$got"; echo -e "$id\tFAIL\t$desc\texp=$exp got=$got" >> "$RES"; fi; }

# —— 定义部署 ——
deploy() { local name="$1" f="$2" key
  key=$(jq -Rs --arg n "$name" '{name:$n,bpmnXml:.}' < "$f" | curl -s -X POST -H "$K" -H "$CT" -d @- "$BASE/definitions/draft" | jq -r '.data.key // empty')
  [ -z "$key" ] && { echo "  DEPLOY-FAIL $f"; return 1; }
  j POST "/definitions/$key/publish" '{"note":"dimtest","publishedBy":"tester"}' >/dev/null; echo "$key"; }

# —— 绑定（维度泛化）——
bind() { # bind <calledKey> <dimKey> <dimValue('' =兜底)> <targetKey> [enabled]
  local ck="$1" dk="$2" dv="$3" tk="$4" en="${5:-true}"
  j POST /subflow-bindings "$(jq -n --arg ck "$ck" --arg dk "$dk" --arg dv "$dv" --arg tk "$tk" --argjson en "$en" \
    '{calledKey:$ck,dimKey:$dk,targetKey:$tk,enabled:$en}+(if $dv==""then{}else{dimValue:$dv}end)')" >/dev/null; }

# —— 实例 ——
start_dim() { # start_dim <defKey> <dimsJson> <bizKey>
  j POST /instances "$(jq -n --arg k "$1" --argjson d "$2" --arg bk "$3" '{definitionKey:$k,variables:{initiator:"u_applicant"},dimensions:$d,businessKey:$bk}')"; }
iid() { echo "$1" | jq -r '.data.id // empty'; }
kids() { j GET "/instances/$1/children"; }
childdef() { kids "$1" | jq -r '.data.children[-1].definitionKey // "NONE"'; } # 最新子实例的定义 key
inst() { j GET "/instances/$1"; }
istate() { inst "$1" | jq -r '.data.state'; }
# 办结某实例当前所有 userTask 直到非 ACTIVE
drain() { local i="$1" t
  while [ "$(istate "$i")" = "ACTIVE" ]; do
    t=$(inst "$i" | jq -r '.data.openTasks[0].id // empty'); [ -z "$t" ] && break
    j POST "/tasks/$t/complete" "$(jq -n --arg i "$i" '{instanceId:$i,comment:"办理"}')" >/dev/null
  done; }

echo "════════════════════════════════════════════════════════════════════"
echo "  子流程路由维度泛化 · 端到端全量测试（组织机构 + 产品 双维度）"
echo "════════════════════════════════════════════════════════════════════"

# ── 阶段 0：健康检查 ──
echo; echo "── 阶段0 · 环境 ──"
H=$(curl -s -o /dev/null -w "%{http_code}" -H "$K" "$BASE/definitions"); A "P0-health" "flow-server :8091 可达" "200" "$H"
DIMS=$(j GET /dimensions | jq -r '[.data.dimensions[].dimKey]|sort|join(",")')
A "P0-dims" "维度端点含 org+product" "org,product" "$DIMS"
PENT=$(j GET /dimension/product/entries | jq -r '.data.entries|length')
A "P0-prod-entries" "产品维度条目 6 个" "6" "$PENT"
OENT=$(j GET /dimension/org/entries | jq -r '.data.entries|length')
A "P0-org-entries" "组织维度条目 5 个" "5" "$OENT"

# ── 阶段 1：部署 9 个定义 ──
echo; echo "── 阶段1 · 部署定义 ──"
MAIN=$(deploy "报销双维度主流程" "$DEFS/main_reimburse_dual.bpmn")
for s in budget_hq budget_bj budget_sh budget_branch comp_generic comp_finance comp_credit comp_insurance; do
  deploy "$s" "$DEFS/$s.bpmn" >/dev/null
done
DN=$(j GET /definitions | jq -r '[.data.definitions[]?|select(.key|test("reimburse_dual|budget_|comp_"))]|length')
A "P1-deploy" "主流程+8子流程共9个已部署" "9" "$DN"
A "P1-mainkey" "主流程 key=reimburse_dual" "reimburse_dual" "$MAIN"

# ── 阶段 2：配置双维度绑定矩阵 ──
echo; echo "── 阶段2 · 绑定矩阵 ──"
# org 维度（budget_appr）：总部→hq，北京→bj，上海→sh，广州(独立)→branch，默认兜底→hq
#   北京一组(fin_bj_g1) 不绑 → 应沿 path 继承北京(bj)
bind budget_appr org zongbu    budget_hq
bind budget_appr org fin_bj    budget_bj
bind budget_appr org fin_sh    budget_sh
bind budget_appr org branch_gz budget_branch
bind budget_appr org ""        budget_hq          # 兜底
# product 维度（prod_compliance）：金融→finance，信用卡→credit，保险→insurance，默认兜底→generic
#   储蓄卡(SAVE) 不绑 → 应沿 full_path 继承金融(FIN)→finance
#   车险(CAR) 不绑 → 应沿 full_path 继承保险(INS)→insurance
bind prod_compliance product FIN    comp_finance
bind prod_compliance product CREDIT comp_credit
bind prod_compliance product INS    comp_insurance
bind prod_compliance product ""     comp_generic       # 兜底
# 一条禁用绑定（储蓄卡显式绑但停用 → 应被跳过，继承金融）
bind prod_compliance product SAVE   comp_credit false
NB_ORG=$(binds() { j GET "/subflow-bindings/$1"; }; binds budget_appr | jq -r '.data.bindings|length')
NB_PROD=$(j GET /subflow-bindings/prod_compliance | jq -r '.data.bindings|length')
A "P2-bind-org"  "org 维度绑定 5 条" "5" "$NB_ORG"
A "P2-bind-prod" "product 维度绑定 5 条(含1禁用)" "5" "$NB_PROD"

echo "  测试报告数据已生成 → 见 report 阶段"
# 阶段 3+ 在 part2 里跑（路由矩阵 + 端到端），此脚本先出数据与绑定
echo; echo "════ 阶段0-2 小结：PASS=$PASS/$TOTAL ════"
echo "$PASS $TOTAL" > logs/dimtest-phase012.txt
