#!/usr/bin/env bash
# SUITE 6 —— 转签 transfer / 委托 delegate / 加签 addsign（含挂起-临时任务-父恢复）
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 6: 转签 / 委托 / 加签 ════════"
tinfo() { inst "$1" | jq -c --arg n "$2" '[.data.tasks[]?|select(.nodeBpmnId==$n and .completed==false)][0]|{who:.assignee,owner:.ownerUserId,del:.delegationState,par:.parentTaskId}'; }
node0() { inst "$1" | jq -r '.data.openTasks[0].nodeBpmnId'; }

r=$(start approval_chain "" '{"initiator":"u_applicant"}' "S6-TRANSFER-DELEGATE-ADDSIGN")
P=$(iid "$r"); echo "inst=$P"; echo "$P">data/s6_iid.txt

# ── 6A 转签(transfer)：l1 u_fin1 → u_fin2 ──
echo "--- 6A 转签 ---"
t=$(taskof u_fin1 "$P")
transfer "$t" "$P" u_fin1 u_fin2 "我出差转给你" >/dev/null
echo "l1 task after transfer: $(tinfo $P l1)"
assert "S6A-assignee" "转签后assignee=u_fin2" "u_fin2" "$(tinfo $P l1|jq -r '.who')"
assert "S6A-owner" "转签彻底换人owner=u_fin2" "u_fin2" "$(tinfo $P l1|jq -r '.owner')"
assert "S6A-ledger" "TRANSFER台账" "true" "$(inst $P|jq -r '[.data.delegations[]?|select(.kind=="TRANSFER")]|length>=1')"
t=$(taskof u_fin2 "$P"); complete "$t" "$P" "受让人办结" >/dev/null
assert "S6A-advance" "转签后受让人办结→l2" "l2" "$(node0 $P)"

# ── 6B 委托(delegate)：l2 u_bjlead → u_fin3（owner保持原主）──
echo "--- 6B 委托 ---"
t=$(taskof u_bjlead "$P")
delegate "$t" "$P" u_bjlead u_fin3 "临时委托代办" >/dev/null
echo "l2 task after delegate: $(tinfo $P l2)"
assert "S6B-assignee" "委托后assignee=u_fin3" "u_fin3" "$(tinfo $P l2|jq -r '.who')"
assert "S6B-owner-kept" "委托owner保持原主u_bjlead" "u_bjlead" "$(tinfo $P l2|jq -r '.owner')"
assert "S6B-state" "委托delegationState=DELEGATED" "DELEGATED" "$(tinfo $P l2|jq -r '.del')"
assert "S6B-ledger" "DELEGATE台账" "true" "$(inst $P|jq -r '[.data.delegations[]?|select(.kind=="DELEGATE")]|length>=1')"
t=$(taskof u_fin3 "$P"); complete "$t" "$P" "受托人办结" >/dev/null
assert "S6B-advance" "委托后受托人办结→l3" "l3" "$(node0 $P)"

# ── 6C 加签(addsign before)：l3 u_cfo 加签 u_auditor1 在前 ──
echo "--- 6C 加签(前) ---"
t=$(taskof u_cfo "$P")   # l3 original
addsign "$t" "$P" u_cfo u_auditor1 true "请审计先看" >/dev/null
echo "l3 tasks after addsign: $(inst $P|jq -c '[.data.tasks[]?|select(.nodeBpmnId=="l3" and .completed==false)|{who:.assignee,del:.delegationState,par:(.parentTaskId!=null)}]')"
susp=$(inst $P|jq -r '[.data.tasks[]?|select(.id=="'$t'")][0].delegationState')
assert "S6C-suspend" "原任务挂起SUSPENDED" "SUSPENDED" "$susp"
temp=$(inst $P|jq -r '[.data.tasks[]?|select(.parentTaskId=="'$t'" and .completed==false)][0]')
assert "S6C-temp-who" "临时任务派给被加签人u_auditor1" "u_auditor1" "$(echo "$temp"|jq -r '.assignee')"
assert "S6C-temp-state" "临时任务delegationState=ADDSIGN" "ADDSIGN" "$(echo "$temp"|jq -r '.delegationState')"
assert "S6C-ledger" "ADDSIGN_BEFORE台账" "true" "$(inst $P|jq -r '[.data.delegations[]?|select(.kind|test("ADDSIGN"))]|length>=1')"
# 被加签人办结临时任务 → 父任务恢复
tempid=$(echo "$temp"|jq -r '.id')
complete "$tempid" "$P" "审计已看" >/dev/null
resumed=$(inst $P|jq -r '[.data.tasks[]?|select(.id=="'$t'")][0]|{del:.delegationState,completed:.completed}')
echo "parent after temp done: $resumed"
assert "S6C-parent-resume" "临时办结后父任务恢复(不再SUSPENDED)" "false" "$(echo "$resumed"|jq -r '.del=="SUSPENDED"')"
# 原办理人办结父任务 → 流程办结
t2=$(taskof u_cfo "$P"); [ -z "$t2" ] && t2=$t
complete "$t2" "$P" "总监办结" >/dev/null
assert "S6C-done" "加签全办结→COMPLETED" "COMPLETED" "$(inst $P|jq -r '.data.state')"

# ── 6D 加签(after)在另一实例观察 ──
echo "--- 6D 加签(后) ---"
r=$(start approval_chain "" '{"initiator":"u_applicant"}' "S6D-ADDSIGN-AFTER")
Q=$(iid "$r"); echo "$Q">data/s6d_iid.txt
t=$(taskof u_fin1 "$Q")
ar=$(addsign "$t" "$Q" u_fin1 u_fin2 false "会办后加签")
assert "S6D-after-code" "加签(后) code=0" "0" "$(echo "$ar"|jq -r '.code')"
assert "S6D-after-ledger" "ADDSIGN_AFTER台账" "true" "$(inst $Q|jq -r '[.data.delegations[]?|select(.kind=="ADDSIGN_AFTER")]|length>=1')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
