#!/usr/bin/env bash
# SUITE 9 —— 并行网关 fork/join + 实例生命周期(挂起/恢复/取消/跳转/改变量)
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 9: 并行网关 + 生命周期 ════════"
st() { inst "$1" | jq -r '.data.state'; }
active() { inst "$1" | jq -c '[.data.tokens[]?|select(.state!="ENDED")|.nodeBpmnId]|sort'; }
opencount() { inst "$1" | jq -r '[.data.openTasks[]?]|length'; }

# ── 9A 并行网关 fork/join ──
echo "--- 9A 并行网关 ---"
r=$(start par_gw "" '{}' "S9A-PARGW")
P=$(iid "$r"); echo "$P">data/s9a_iid.txt
assert "S9A-fork" "fork后2个并发任务" "2" "$(opencount $P)"
assert "S9A-branches" "两分支legal+fin并发" '["fin","legal"]' "$(active $P)"
t=$(taskof u_auditor1 "$P"); complete "$t" "$P" "法务通过" >/dev/null
assert "S9A-join-wait" "一支办结→join等待另一支(仍ACTIVE)" "ACTIVE" "$(st $P)"
t=$(taskof u_cfo "$P"); complete "$t" "$P" "财务通过" >/dev/null
assert "S9A-join-done" "两支齐→join合流→COMPLETED" "COMPLETED" "$(st $P)"

# ── 9B 挂起 / 恢复 ──
echo "--- 9B 挂起/恢复 ---"
r=$(start approval_chain "" '{"initiator":"u_a"}' "S9B-SUSPEND")
Q=$(iid "$r"); echo "$Q">data/s9b_iid.txt
suspend "$Q" >/dev/null
assert "S9B-suspended" "挂起→SUSPENDED" "SUSPENDED" "$(st $Q)"
# 挂起态尝试办理应被拒
t=$(inst "$Q"|jq -r '.data.tasks[0].id')
cr=$(complete "$t" "$Q" "挂起态办理" 2>/dev/null)
assert "S9B-suspended-noact" "挂起态办理被拒code=1" "1" "$(echo "$cr"|jq -r '.code')"
resume "$Q" >/dev/null
assert "S9B-resumed" "恢复→ACTIVE" "ACTIVE" "$(st $Q)"
# 恢复后可正常办理
t=$(taskof u_fin1 "$Q"); complete "$t" "$Q" "恢复后办理" >/dev/null
assert "S9B-after-resume" "恢复后办理→推进l2" "l2" "$(inst $Q|jq -r '.data.openTasks[0].nodeBpmnId')"

# ── 9C 取消 ──
echo "--- 9C 取消 ---"
r=$(start approval_chain "" '{"initiator":"u_b"}' "S9C-CANCEL")
Z=$(iid "$r"); echo "$Z">data/s9c_iid.txt
cancel "$Z" >/dev/null
assert "S9C-cancelled" "取消→TERMINATED" "TERMINATED" "$(st $Z)"

# ── 9D 跳转 jump ──
echo "--- 9D 跳转 ---"
r=$(start approval_chain "" '{"initiator":"u_c"}' "S9D-JUMP")
Y=$(iid "$r"); echo "$Y">data/s9d_iid.txt
jr=$(jump "$Y" l3 "管理员跳到三级")
assert "S9D-jump-code" "跳转 code=0" "0" "$(echo "$jr"|jq -r '.code')"
assert "S9D-jumped" "跳转后活动节点=l3" "l3" "$(inst $Y|jq -r '.data.openTasks[0].nodeBpmnId')"

# ── 9E 改变量 set-variables ──
echo "--- 9E 改实例变量 ---"
r=$(start approval_chain "" '{"initiator":"u_d","amount":100}' "S9E-SETVARS")
W=$(iid "$r"); echo "$W">data/s9e_iid.txt
setvars "$W" '{"amount":999,"extra":"added"}' >/dev/null
v=$(ivars "$W" | jq -c '.data.variables // .data')
echo "vars now: $v"
assert "S9E-var-updated" "amount更新为999" "999" "$(ivars $W|jq -r '.data.variables.amount // .data.amount')"
assert "S9E-var-added" "新增extra=added" "added" "$(ivars $W|jq -r '.data.variables.extra // .data.extra')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
