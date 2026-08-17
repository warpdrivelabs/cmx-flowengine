#!/usr/bin/env bash
# SUITE 13 —— MI 候选表达式(styleB 逐元素 0/1/≥2) + 子流程内跨级退回
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 13: MI候选表达式(styleB) + 子流程内退回 ════════"
[ -f defs/mi_dyn_role.bpmn ] && deploy "逐元素候选角色会签" defs/mi_dyn_role.bpmn >/dev/null

# 13A element A→role(finance)候选池；element B→role(cashier)直派
r=$(start mi_dyn_role "" '{"products":[{"sku":"A","role":"finance"},{"sku":"B","role":"cashier"}]}' "S13A-MI-ROLE")
P=$(iid "$r"); echo "$P">data/s13a_iid.txt
bwho=$(inst $P|jq -r '[.data.openTasks[]?|select(.elementValue.sku=="B")][0].assignee')
assert "S13A-B-direct" "产品B role(cashier)→直派u_cashier1" "u_cashier1" "$bwho"
acand=$(inst $P|jq -c '[.data.openTasks[]?|select(.elementValue.sku=="A")][0].candidates|map(.userId)|sort')
assert "S13A-A-pool" "产品A role(finance)→候选池3人" '["u_fin1","u_fin2","u_fin3"]' "$acand"
ta=$(claimtaskof u_fin2 "$P"); claim "$ta" "$P" u_fin2 >/dev/null; complete "$ta" "$P" A审 >/dev/null
tb=$(taskof u_cashier1 "$P"); complete "$tb" "$P" B审 >/dev/null
assert "S13A-done" "逐元素候选会签办结→COMPLETED" "COMPLETED" "$(inst $P|jq -r '.data.state')"

# 13B 子流程内跨级退回
r=$(start travel_expense zongbu '{"amount":30000,"applicant":"subrej","initiator":"subrej"}' "S13B-SUBREJECT")
PA=$(iid "$r"); echo "$PA">data/s13b_iid.txt
t=$(inst $PA|jq -r '.data.openTasks[0].id'); complete "$t" "$PA" mgr过 >/dev/null
t=$(inst $PA|jq -r '.data.openTasks[0].id'); complete "$t" "$PA" director过 >/dev/null
SUB=$(children "$PA"|jq -r '.data.children[0].id')
t=$(inst $SUB|jq -r '.data.openTasks[0].id'); complete "$t" "$SUB" fin1过 >/dev/null
t=$(inst $SUB|jq -r '.data.openTasks[0].id'); complete "$t" "$SUB" fin2过 >/dev/null
assert "S13B-at-fin3" "子流程推进到fin3" "fin3" "$(inst $SUB|jq -r '.data.openTasks[0].nodeBpmnId')"
t=$(inst $SUB|jq -r '.data.openTasks[0].id')
reject "$t" "$SUB" "fin1" "子流程跨级退回" u_fin3 >/dev/null
assert "S13B-sub-back" "子流程内退回→回到fin1" "fin1" "$(inst $SUB|jq -r '.data.openTasks[0].nodeBpmnId')"
assert "S13B-parent-wait" "退回期间父仍在callActivity等待" "true" "$(inst $PA|jq -r '[.data.tokens[]?|select(.nodeBpmnId=="fin_review" and .state!="ENDED")]|length>=1')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
