#!/usr/bin/env bash
# SUITE 5 —— 取回/撤回 withdraw：发起人未处理可取回 + 非发起人拒绝 + strict下游已办结拒绝 + 办结后拒绝 + 台账
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 5: 取回 withdraw ════════"
node0() { inst "$1" | jq -r '.data.openTasks[0].nodeBpmnId'; }
who0()  { inst "$1" | jq -r '.data.openTasks[0].assignee'; }

# ── 5A 发起人未处理→可取回，落回首节点并回派发起人 ──
echo "--- 5A 发起人取回 ---"
r=$(start approval_chain "" '{"initiator":"u_applicant"}' "S5A-WITHDRAW-OK")
P=$(iid "$r"); echo "inst=$P"; echo "$P">data/s5a_iid.txt
assert "S5A-can" "发起人可取回=true" "true" "$(withdrawable "$P" u_applicant | jq -r '.data.withdrawable')"
assert "S5A-noninit" "非发起人u_fin1不可取回" "false" "$(withdrawable "$P" u_fin1 | jq -r '.data.withdrawable')"
wr=$(withdraw "$P" u_applicant "取回改数据")
assert "S5A-do" "取回 code=0" "0" "$(echo "$wr"|jq -r '.code')"
assert "S5A-active" "取回后实例保持ACTIVE" "ACTIVE" "$(inst $P|jq -r '.data.state')"
assert "S5A-landing" "落回首节点l1" "l1" "$(node0 $P)"
assert "S5A-reassign" "首节点回派发起人u_applicant" "u_applicant" "$(who0 $P)"
led=$(inst "$P" | jq -r '[.data.delegations[]?|select(.kind=="WITHDRAW")]|length')
assert "S5A-ledger" "WITHDRAW台账留痕" "true" "$([ "${led:-0}" -ge 1 ] && echo true || echo false)"
# 取回后发起人改数据重新提交 → 继续前进
t=$(taskof u_applicant "$P"); complete "$t" "$P" "改后重交" >/dev/null
assert "S5A-resubmit" "取回后重新提交→推进到l2" "l2" "$(node0 $P)"

# ── 5B strict：下游已办结→不可取回 ──
echo "--- 5B strict 下游已办结拒绝 ---"
r=$(start approval_chain "" '{"initiator":"u_applicant"}' "S5B-WITHDRAW-STRICT")
Q=$(iid "$r"); echo "$Q">data/s5b_iid.txt
t=$(taskof u_fin1 "$Q"); complete "$t" "$Q" "L1办结" >/dev/null   # 下游 l1 办结
assert "S5B-can-false" "下游已办结→可取回=false" "false" "$(withdrawable "$Q" u_applicant | jq -r '.data.withdrawable')"
wr=$(withdraw "$Q" u_applicant "试图取回")
assert "S5B-deny" "strict取回被拒 code=1" "1" "$(echo "$wr"|jq -r '.code')"
echo "deny reason: $(echo "$wr"|jq -r '.msg')"

# ── 5C 办结后取回被拒 ──
echo "--- 5C 办结后取回拒绝 ---"
r=$(start approval_chain "" '{"initiator":"u_applicant2"}' "S5C-WITHDRAW-DONE")
Z=$(iid "$r"); echo "$Z">data/s5c_iid.txt
# 用 lenient? 不，直接驱动办结
for a in u_fin1 u_bjlead u_cfo; do t=$(taskof "$a" "$Z"); complete "$t" "$Z" ok >/dev/null; done
assert "S5C-done" "已办结COMPLETED" "COMPLETED" "$(inst $Z|jq -r '.data.state')"
wr=$(withdraw "$Z" u_applicant2 "办结后取回")
assert "S5C-deny-done" "办结后取回被拒 code=1" "1" "$(echo "$wr"|jq -r '.code')"

# ── 5D 非发起人取回被拒 ──
echo "--- 5D 非发起人取回拒绝 ---"
r=$(start approval_chain "" '{"initiator":"u_applicant"}' "S5D-WITHDRAW-NONINIT")
Y=$(iid "$r"); echo "$Y">data/s5d_iid.txt
wr=$(withdraw "$Y" u_hacker "冒充取回")
assert "S5D-deny-noninit" "非发起人取回被拒 code=1" "1" "$(echo "$wr"|jq -r '.code')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
