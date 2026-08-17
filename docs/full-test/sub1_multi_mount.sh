#!/usr/bin/env bash
# SUB-SUITE 1 —— 一主流程挂多个子流程：串行多挂载 + 并行多挂载
source "$(dirname "$0")/lib.sh"
echo "════════ SUB1: 主流程上多个子流程（串行/并行多挂载）════════"
st() { inst "$1" | jq -r '.data.state'; }
node0() { inst "$1" | jq -r '.data.openTasks[0].nodeBpmnId // "NONE"'; }
childdefs() { children "$1" | jq -c '[.data.children[]?|{def:.definitionKey,state:.state}]|sort_by(.def)'; }
childof() { children "$1" | jq -r --arg d "$2" '[.data.children[]?|select(.definitionKey==$d)][0].id // empty'; }
advchild() { local c t; c="$1"; t=$(inst "$c"|jq -r '.data.openTasks[0].id'); complete "$t" "$c" "子流程办结" >/dev/null; }
waitsub() { inst "$1" | jq -r '.data.waitingSubflow'; }

# ── SUB1-A 串行多子流程：apply→call1(sub_review)→call2(sub_risk)→call3(fin_review组织路由)→pay ──
echo "--- SUB1-A 串行 3 子流程（依次执行）---"
r=$(start main_serial_multi zongbu '{"initiator":"u_ser","amount":5000}' "SUB1A-SERIAL"); P=$(iid "$r"); echo "$P">data/sub1a_iid.txt
echo "起始: $(show $P | jq -c '{state,active}')"
# 推进 apply
t=$(taskof u_fin1 "$P"); complete "$t" "$P" 申请 >/dev/null
# 此刻应挂载 call1 = sub_review, 且只有 1 个子实例(串行)
assert "SUB1A-c1-only" "串行:此刻仅1个子实例" "1" "$(children $P|jq -r '.data.children|length')"
assert "SUB1A-c1-def" "①挂载=sub_review" "sub_review" "$(children $P|jq -r '.data.children[0].definitionKey')"
assert "SUB1A-waiting" "父在call1等待(waitingSubflow=true)" "true" "$(waitsub $P)"
# 办结 sub_review → call2 = sub_risk 出现
advchild "$(childof $P sub_review)"
assert "SUB1A-c2-def" "①完成后②挂载=sub_risk出现" "true" "$(children $P|jq -r '[.data.children[]?|select(.definitionKey=="sub_risk")]|length>=1')"
assert "SUB1A-c1-done" "①sub_review已COMPLETED" "COMPLETED" "$(children $P|jq -r '[.data.children[]?|select(.definitionKey=="sub_review")][0].state')"
# 办结 sub_risk → call3 = fin_review 组织路由(zongbu→fin_review_hq)
advchild "$(childof $P sub_risk)"
c3=$(childof $P fin_review_hq)
assert "SUB1A-c3-routed" "③fin_review按zongbu路由→fin_review_hq" "true" "$([ -n "$c3" ] && echo true || echo false)"
# 办结 fin_review_hq(三级)
while [ "$(inst $c3|jq -r '.data.state')" = "ACTIVE" ]; do advchild "$c3"; done
assert "SUB1A-c3-done" "③子流程办结" "COMPLETED" "$(inst $c3|jq -r '.data.state')"
# 父被唤醒 → pay
assert "SUB1A-parent-pay" "全部子流程完→父到打款节点" "pay" "$(node0 $P)"
t=$(taskof u_cashier1 "$P"); complete "$t" "$P" 打款 >/dev/null
assert "SUB1A-done" "串行多子流程主流程办结" "COMPLETED" "$(st $P)"
# 累计 3 个子实例
assert "SUB1A-3children" "共产生3个子实例" "3" "$(children $P|jq -r '.data.children|length')"

# ── SUB1-B 并行多子流程：fork→callFin(fin_review路由)+callRisk(sub_risk)并存→join ──
echo "--- SUB1-B 并行 2 子流程（同时并存）---"
r=$(start main_parallel_multi fin_sh '{"initiator":"u_par"}' "SUB1B-PARALLEL"); Q=$(iid "$r"); echo "$Q">data/sub1b_iid.txt
# fork 同时挂 2 个子流程
assert "SUB1B-2children" "并行:2子实例同时并存" "2" "$(children $Q|jq -r '.data.children|length')"
defs=$(childdefs $Q)
echo "并存子流程: $defs"
# fin_sh 路由 fin_review → fin_review_branch; 另一支 sub_risk
assert "SUB1B-fin-routed" "callFin按fin_sh路由→fin_review_branch" "true" "$(children $Q|jq -r '[.data.children[]?|select(.definitionKey=="fin_review_branch")]|length>=1')"
assert "SUB1B-risk" "callRisk→sub_risk并存" "true" "$(children $Q|jq -r '[.data.children[]?|select(.definitionKey=="sub_risk")]|length>=1')"
assert "SUB1B-both-active" "两子流程都ACTIVE" "true" "$(children $Q|jq -r '[.data.children[]?|select(.state=="ACTIVE")]|length==2')"
# 先办结 fin_review_branch → 主流程仍 ACTIVE(风控未完)
cb=$(childof $Q fin_review_branch)
while [ "$(inst $cb|jq -r '.data.state')" = "ACTIVE" ]; do advchild "$cb"; done
assert "SUB1B-still-active" "一支完另一支未完→主流程仍ACTIVE" "ACTIVE" "$(st $Q)"
# 办结 sub_risk → join 合流 → 办结
cr=$(childof $Q sub_risk); advchild "$cr"
assert "SUB1B-done" "两子流程齐→join合流→主流程办结" "COMPLETED" "$(st $Q)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
