#!/usr/bin/env bash
# SUITE 4 —— 回退(退回) reject：默认前驱 + reject-targets 枚举 + 退回到任意上游节点
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 4: 回退 reject / reject-targets / 退回任意节点 ════════"
node0() { inst "$1" | jq -r '.data.openTasks[0].nodeBpmnId'; }

# approval_chain: s→l1(u_fin1)→l2(u_bjlead)→l3(u_cfo)→e
# ── 4A 默认退回上一步：l3 退回 → 回到 l2 ──
echo "--- 4A 默认退回上一步 ---"
r=$(start approval_chain "" '{"initiator":"u_fin1"}' "S4A-REJECT-PREV")
P=$(iid "$r"); echo "inst=$P"; echo "$P">data/s4a_iid.txt
t=$(taskof u_fin1 "$P"); complete "$t" "$P" "L1同意" >/dev/null       # l1→l2
t=$(taskof u_bjlead "$P"); complete "$t" "$P" "L2同意" >/dev/null     # l2→l3
assert "S4A-at-l3" "推进到三级审批l3" "l3" "$(node0 $P)"
# l3 reject-targets 枚举
t=$(taskof u_cfo "$P")
rt=$(rtargets "$t" "$P")
echo "reject-targets: $(echo "$rt"|jq -c '.data')"
assert "S4A-rt-default" "l3默认退回目标=l2" "l2" "$(echo "$rt"|jq -r '.data.defaultTarget')"
tgts=$(echo "$rt"|jq -c '[.data.targets[]?.bpmnId]|sort')
assert "S4A-rt-list" "l3可退目标={l1,l2}" '["l1","l2"]' "$tgts"
# 默认退回（不带 targetBpmnId）→ 回到 l2
reject "$t" "$P" "" "退回上一步补充" u_cfo >/dev/null
assert "S4A-back-l2" "默认退回→回到l2" "l2" "$(node0 $P)"

# ── 4B 退回到任意指定节点：l3 直接退回到 l1（跨级） ──
echo "--- 4B 退回到指定上游节点(跨级) ---"
r=$(start approval_chain "" '{"initiator":"u_fin1"}' "S4B-REJECT-ANY")
Q=$(iid "$r"); echo "inst=$Q"; echo "$Q">data/s4b_iid.txt
t=$(taskof u_fin1 "$Q"); complete "$t" "$Q" ok >/dev/null
t=$(taskof u_bjlead "$Q"); complete "$t" "$Q" ok >/dev/null
t=$(taskof u_cfo "$Q")   # at l3
reject "$t" "$Q" "l1" "跨级退回到一级重办" u_cfo >/dev/null
assert "S4B-back-l1" "指定退回→跨级回到l1" "l1" "$(node0 $Q)"
# 回到 l1 后重新走，验证可继续推进
t=$(taskof u_fin1 "$Q"); complete "$t" "$Q" 重办 >/dev/null
assert "S4B-resume-l2" "退回后重新推进到l2" "l2" "$(node0 $Q)"

# ── 4C 首个节点无上游可退：l1 的 reject-targets 为空 ──
echo "--- 4C 首节点无处可退 ---"
r=$(start approval_chain "" '{"initiator":"u_fin1"}' "S4C-REJECT-FIRST")
W=$(iid "$r"); echo "$W">data/s4c_iid.txt
t=$(taskof u_fin1 "$W")
rt=$(rtargets "$t" "$W")
assert "S4C-first-empty" "首节点l1 rejectable=false" "false" "$(echo "$rt"|jq -r '.data.rejectable')"
assert "S4C-first-tgts" "首节点可退目标为空" "0" "$(echo "$rt"|jq -r '.data.targets|length')"

# ── 4D 退回台账留痕（cmx_flow_task_comment / hi_task）──
echo "--- 4D 退回留痕 ---"
cmts=$(icomments "$P" | jq -r '[.data.comments[]?|select(.action=="REJECT" or (.comment|test("退回")))]|length' 2>/dev/null)
echo "P comments: $(icomments "$P" | jq -c '.data.comments // .data' 2>/dev/null | head -c 300)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
