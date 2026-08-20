#!/usr/bin/env bash
# 维度路由测试 · 第2部分：路由矩阵验证 + 同实例双维度 + 端到端办结
# 前置：先跑 dimtest_setup.sh（部署+绑定）。本脚本起实例验证路由解析。
cd "$(dirname "$0")"
BASE="http://127.0.0.1:8091/api/flow/v1"
K="X-API-Key: cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6"
XU="X-User: u_applicant"
CT="Content-Type: application/json"
LOG="logs/dimtest-transcript.log"
RES="logs/dimtest-results.tsv"
PASS=0; TOTAL=0
[ -f logs/dimtest-phase012.txt ] && read P0 T0 < logs/dimtest-phase012.txt && PASS=$P0 && TOTAL=$T0

j() { local m="$1" p="$2" b="${3:-}"; local r
  if [ -n "$b" ]; then r=$(curl -s -X "$m" -H "$CT" -H "$XU" -d "$b" "$BASE$p")
  elif [ "$m" = GET ]; then r=$(curl -s -H "$XU" "$BASE$p")
  else r=$(curl -s -X "$m" -H "$XU" "$BASE$p"); fi
  { echo "### $m $p"; [ -n "$b" ] && echo "REQ  $b"; echo "RESP $r"; echo; } >> "$LOG"; echo "$r"; }
A() { TOTAL=$((TOTAL+1)); local id="$1" desc="$2" exp="$3" got="$4"
  if [ "$exp" = "$got" ]; then PASS=$((PASS+1)); printf '  \033[32m✓\033[0m %-40s %s\n' "$id" "$desc"; echo -e "$id\tPASS\t$desc" >> "$RES"
  else printf '  \033[31m✗\033[0m %-40s %s\n     期望[%s] 实得[%s]\n' "$id" "$desc" "$exp" "$got"; echo -e "$id\tFAIL\t$desc\texp=$exp got=$got" >> "$RES"; fi; }

start_dim() { j POST /instances "$(jq -n --arg k "$1" --argjson d "$2" --arg bk "$3" '{definitionKey:$k,variables:{initiator:"u_applicant"},dimensions:$d,businessKey:$bk}')"; }
iid() { echo "$1" | jq -r '.data.id // empty'; }
kids() { j GET "/instances/$1/children"; }
# children API 不暴露 parentNodeBpmnId，故按子流程 def 前缀区分挂载点：
#   callBudget 挂载 → budget_* 子流程；callComp 挂载 → comp_* 子流程。
child_budget() { kids "$1" | jq -r '[.data.children[]?|select(.definitionKey|test("^budget_"))][-1].definitionKey // "NONE"'; }
child_comp()   { kids "$1" | jq -r '[.data.children[]?|select(.definitionKey|test("^comp_"))][-1].definitionKey // "NONE"'; }
child_budget_id() { kids "$1" | jq -r '[.data.children[]?|select(.definitionKey|test("^budget_"))][-1].id // empty'; }
child_comp_id()   { kids "$1" | jq -r '[.data.children[]?|select(.definitionKey|test("^comp_"))][-1].id // empty'; }
inst() { j GET "/instances/$1"; }
istate() { inst "$1" | jq -r '.data.state'; }
opentask() { inst "$1" | jq -r '.data.openTasks[0].id // empty'; }
# 办结任务须以**该任务的 assignee** 身份（auth-off 认 X-User）；子流程各节点 assignee 各异。
opentask_who() { inst "$1" | jq -r '.data.openTasks[0].assignee // "u_applicant"'; }
donetask() { # donetask <taskId> <instanceId> <asUser>
  curl -s -X POST -H "$CT" -H "X-User: $3" -d "$(jq -n --arg i "$2" '{instanceId:$i,comment:"办理"}')" "$BASE/tasks/$1/complete" >> "$LOG" 2>&1; }
# 办结一个实例的所有 userTask 直到非 ACTIVE（每个任务按其 assignee 办）。带步数上限防死循环。
drain_child() { local c="$1" t who n=0
  while [ "$(istate "$c")" = "ACTIVE" ] && [ $n -lt 20 ]; do
    t=$(opentask "$c"); [ -z "$t" ] && break
    who=$(opentask_who "$c"); donetask "$t" "$c" "$who"; n=$((n+1))
  done; }
# 起实例并办结 apply(推进到 callBudget)，返回主实例 id。apply 的 assignee=u_applicant。
start_to_budget() { local r i t; r=$(start_dim reimburse_dual "$1" "$2"); i=$(iid "$r")
  t=$(opentask "$i"); [ -n "$t" ] && donetask "$t" "$i" "u_applicant"; echo "$i"; }

echo "── 阶段3 · org 维度路由矩阵（挂载点 callBudget）──"
# 精确
I=$(start_to_budget '{"org":"zongbu","product":"FIN"}' "DT-ORG-ZB"); echo "$I" > data/dimtest/i_org_zb.txt
A "P3-org-exact-hq"     "总部→budget_hq(精确)"        "budget_hq"     "$(child_budget "$I")"
I=$(start_to_budget '{"org":"fin_sh","product":"FIN"}' "DT-ORG-SH"); echo "$I" > data/dimtest/i_org_sh.txt
A "P3-org-exact-sh"     "上海→budget_sh(精确)"        "budget_sh"     "$(child_budget "$I")"
I=$(start_to_budget '{"org":"fin_bj","product":"FIN"}' "DT-ORG-BJ"); echo "$I" > data/dimtest/i_org_bj.txt
A "P3-org-exact-bj"     "北京→budget_bj(精确)"        "budget_bj"     "$(child_budget "$I")"
# 沿 path 继承：北京一组(fin_bj_g1) 未绑 → 继承北京
I=$(start_to_budget '{"org":"fin_bj_g1","product":"FIN"}' "DT-ORG-G1"); echo "$I" > data/dimtest/i_org_g1.txt
A "P3-org-inherit"      "北京一组→继承北京budget_bj"   "budget_bj"     "$(child_budget "$I")"
# 独立分公司(广州) 有自身绑定 → 不误继承总部
I=$(start_to_budget '{"org":"branch_gz","product":"FIN"}' "DT-ORG-GZ"); echo "$I" > data/dimtest/i_org_gz.txt
A "P3-org-own-branch"   "广州(独立根)→自身budget_branch" "budget_branch" "$(child_budget "$I")"
# 默认兜底：不在树的 org → 兜底
I=$(start_to_budget '{"org":"org_unknown","product":"FIN"}' "DT-ORG-DEF"); echo "$I" > data/dimtest/i_org_def.txt
A "P3-org-fallback"     "未知组织→兜底budget_hq"      "budget_hq"     "$(child_budget "$I")"

echo; echo "── 阶段4 · product 维度路由矩阵（挂载点 callComp，需先过 callBudget）──"
# 为隔离 product 维度，org 都用 zongbu(→budget_hq)，办结 budget 子实例推进到 callComp
route_product() { # route_product <productId> <bizkey> → 回显 callComp 子流程 def
  local i r bc; i=$(start_to_budget "{\"org\":\"zongbu\",\"product\":\"$1\"}" "$2")
  bc=$(child_budget "$i")
  # 找到 budget 子实例 id 并办结 → 主推进到 callComp
  local bcid; bcid=$(child_budget_id "$i")
  drain_child "$bcid"
  echo "$i" > "data/dimtest/i_prod_$2.txt"
  child_comp "$i"; }
A "P4-prod-exact-fin"   "金融产品→comp_finance(精确)"     "comp_finance"   "$(route_product FIN    DT-PROD-FIN)"
A "P4-prod-exact-credit" "信用卡→comp_credit(精确)"       "comp_credit"    "$(route_product CREDIT DT-PROD-CREDIT)"
A "P4-prod-exact-ins"   "保险产品→comp_insurance(精确)"   "comp_insurance" "$(route_product INS    DT-PROD-INS)"
# 沿 full_path 继承：储蓄卡(SAVE,禁用自身绑定) → 继承金融
A "P4-prod-inherit-save" "储蓄卡(自绑禁用)→继承金融comp_finance" "comp_finance" "$(route_product SAVE DT-PROD-SAVE)"
# 多级继承：车险(CAR) 未绑 → 继承保险(INS)
A "P4-prod-inherit-car" "车险→继承保险comp_insurance"     "comp_insurance" "$(route_product CAR  DT-PROD-CAR)"
# 默认兜底：全部产品(ALL) 未绑且无上级 → 兜底
A "P4-prod-fallback"    "全部产品(无绑无上级)→兜底comp_generic" "comp_generic" "$(route_product ALL DT-PROD-ALL)"

echo; echo "── 阶段5 · ★同实例双维度：挂载A按org、挂载B按product（核心诉求）──"
# 一个实例带 org=fin_sh + product=CREDIT：callBudget→budget_sh(按org)、callComp→comp_credit(按product)
R=$(start_dim reimburse_dual '{"org":"fin_sh","product":"CREDIT"}' "DT-DUAL-1"); DI=$(iid "$R"); echo "$DI" > data/dimtest/i_dual.txt
t=$(opentask "$DI"); donetask "$t" "$DI" "$(opentask_who "$DI")"   # 办 apply → callBudget 起
A "P5-dual-mountA-org"  "挂载A按org(fin_sh)→budget_sh"    "budget_sh"   "$(child_budget "$DI")"
bcid=$(child_budget_id "$DI"); drain_child "$bcid"  # 办结budget→callComp 起
A "P5-dual-mountB-prod" "挂载B按product(CREDIT)→comp_credit" "comp_credit" "$(child_comp "$DI")"
A "P5-dual-diverge"     "同实例两挂载走不同维度解析出不同子流程" "true" \
  "$([ "$(child_budget "$DI")" = budget_sh ] && [ "$(child_comp "$DI")" = comp_credit ] && echo true || echo false)"

echo; echo "── 阶段6 · 端到端办结（双维度实例跑到 COMPLETED）──"
ccid=$(child_comp_id "$DI"); drain_child "$ccid"  # 办结 comp
# 回主流程 pay 任务
t=$(opentask "$DI"); [ -n "$t" ] && donetask "$t" "$DI" "$(opentask_who "$DI")"
A "P6-e2e-complete"     "双维度实例端到端→COMPLETED"      "COMPLETED"   "$(istate "$DI")"

echo; echo "── 阶段7 · 维度隔离（同一 called_key 不跨维度串味）──"
# budget_appr 只在 org 维度有绑定；用 product 维度值起（不存在的绑定）→ 应无解走兜底?
# 实际 callBudget 恒用 dimKey=org，故 product 值不影响它。验证：改变 product 不改 budget 路由
I1=$(start_to_budget '{"org":"fin_sh","product":"CAR"}' "DT-ISO-1")
I2=$(start_to_budget '{"org":"fin_sh","product":"CREDIT"}' "DT-ISO-2")
A "P7-dim-isolation"    "callBudget 只认org维度(product变budget不变)" "true" \
  "$([ "$(child_budget "$I1")" = "$(child_budget "$I2")" ] && echo true || echo false)"

echo; echo "════ 阶段3-7 小结：累计 PASS=$PASS/$TOTAL ════"
echo "$PASS $TOTAL" > logs/dimtest-total.txt
