#!/usr/bin/env bash
# SUITE 2 —— 所有类型「指定人员」+ 提交(complete)。驱动 probe_cand 走完全部候选类型。
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 2: 所有类型指定人员 + 提交 ════════"
r=$(start probe_cand fin_bj '{"initiator":"u_applicant","applicant":"u_applicant"}' "S2-ALLCAND")
IID=$(iid "$r"); echo "instance = $IID"; echo "$IID" > data/s2_iid.txt

# ── t_role_pool: role(finance) → 候选池 u_fin1/2/3 ──
cand=$(inst "$IID" | jq -c '[.data.openTasks[]?|select(.nodeBpmnId=="t_role_pool")|.candidates[].userId]|sort')
assert "S2-rolepool-cand" "role(finance)→池[u_fin1,u_fin2,u_fin3]" '["u_fin1","u_fin2","u_fin3"]' "$cand"
# u_fin2 认领
tid=$(claimtaskof u_fin2 "$IID")
echo "claimable task for u_fin2 at role_pool: $tid"
claim "$tid" "$IID" u_fin2 >/dev/null
who=$(inst "$IID" | jq -r '.data.openTasks[]?|select(.nodeBpmnId=="t_role_pool")|.assignee')
assert "S2-rolepool-claim" "u_fin2认领后assignee=u_fin2" "u_fin2" "$who"
# u_fin1 认领后不再可见（已被认领）
still=$(claimtaskof u_fin1 "$IID"); assert "S2-rolepool-claimed-gone" "认领后他人不再可认领" "" "$still"
complete "$tid" "$IID" "财务会办通过" >/dev/null

# ── t_role_one: role(cashier) → 直派 u_cashier1 ──
who=$(inst "$IID" | jq -r '.data.openTasks[]?|select(.nodeBpmnId=="t_role_one")|.assignee')
assert "S2-roleone-direct" "role(cashier)单人→直派u_cashier1" "u_cashier1" "$who"
tid=$(taskof u_cashier1 "$IID"); complete "$tid" "$IID" "出纳确认" >/dev/null

# ── t_pos_one: position(cfo) → 直派 u_cfo ──
who=$(inst "$IID" | jq -r '.data.openTasks[]?|select(.nodeBpmnId=="t_pos_one")|.assignee')
assert "S2-pos-direct" "position(cfo)→直派u_cfo" "u_cfo" "$who"
tid=$(taskof u_cfo "$IID"); complete "$tid" "$IID" "CFO审批" >/dev/null

# ── t_org: org(fin_bj) → 候选池(4人) ──
cand=$(inst "$IID" | jq -c '[.data.openTasks[]?|select(.nodeBpmnId=="t_org")|.candidates[].userId]|sort')
assert "S2-org-pool" "org(fin_bj)→池5人(含子组fin_bj_g1的u_bjg1)" '["u_bjg1","u_bjlead","u_cashier1","u_fin1","u_fin2"]' "$cand"
tid=$(claimtaskof u_bjlead "$IID"); claim "$tid" "$IID" u_bjlead >/dev/null
complete "$tid" "$IID" "部门会办" >/dev/null

# ── t_leader: orgLeader(fin_bj) → 直派 u_bjlead ──
who=$(inst "$IID" | jq -r '.data.openTasks[]?|select(.nodeBpmnId=="t_leader")|.assignee')
assert "S2-leader-direct" "orgLeader(fin_bj)→直派u_bjlead" "u_bjlead" "$who"
tid=$(taskof u_bjlead "$IID"); complete "$tid" "$IID" "领导审批" >/dev/null

# ── t_init: initiator → 直派发起人 u_applicant ──
who=$(inst "$IID" | jq -r '.data.openTasks[]?|select(.nodeBpmnId=="t_init")|.assignee')
assert "S2-init-direct" "initiator→直派发起人u_applicant" "u_applicant" "$who"
tid=$(taskof u_applicant "$IID"); complete "$tid" "$IID" "发起人确认" >/dev/null

# ── 办结 ──
st=$(inst "$IID" | jq -r '.data.state')
assert "S2-completed" "全类型走完→实例COMPLETED" "COMPLETED" "$st"
summary
echo "PASS=$PASS TOTAL=$TOTAL"
