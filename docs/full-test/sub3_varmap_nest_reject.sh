#!/usr/bin/env bash
# SUB-SUITE 3 —— 变量映射(in/out) + 三级嵌套 + 子流程内退回
source "$(dirname "$0")/lib.sh"
echo "════════ SUB3: 变量映射 + 嵌套 + 子流程退回 ════════"
advc() { local c="$1" t; t=$(inst "$c"|jq -r '.data.openTasks[0].id'); complete "$t" "$c" "${2:-办}" "" "${3:-{}}" >/dev/null; }
cvars() { inst "$1" | jq -c '.data.variables'; }

# ── SUB3-A 变量映射 in: amount→subAmount, applicant(同名); out: subResult→reviewResult ──
echo "--- SUB3-A 父子变量映射 ---"
r=$(start main_varmap "" '{"initiator":"u_vm","amount":8888,"applicant":"vm_user"}' "SUB3A-VARMAP"); P=$(iid "$r"); echo "$P">data/sub3a_iid.txt
c=$(children $P | jq -r '.data.children[0].id')
echo "  子实例变量: $(cvars $c)"
# in 映射: 子流程应有 subAmount=8888(from amount) + applicant=vm_user(同名), 不应有 amount(未映射)
assert "SUB3A-in-mapped"   "in映射:子流程subAmount=8888" "8888" "$(cvars $c | jq -r '.subAmount')"
assert "SUB3A-in-samename" "in映射:applicant同名透传=vm_user" "vm_user" "$(cvars $c | jq -r '.applicant')"
assert "SUB3A-in-notall"   "in映射:未列的amount不透传(null)" "null" "$(cvars $c | jq -r '.amount')"
# 子流程办结并写 subResult
t=$(inst $c|jq -r '.data.openTasks[0].id')
complete "$t" "$c" "核对完成" "" '{"subResult":"APPROVED"}' >/dev/null
# out 映射: 父流程应得 reviewResult=APPROVED(from subResult), 不应有 subResult
echo "  回写后父变量: $(cvars $P)"
assert "SUB3A-out-mapped" "out映射:父流程reviewResult=APPROVED" "APPROVED" "$(cvars $P | jq -r '.reviewResult')"
assert "SUB3A-out-notall"  "out映射:未列的subResult不回写(null)" "null" "$(cvars $P | jq -r '.subResult')"
assert "SUB3A-done" "变量映射主流程办结" "COMPLETED" "$(inst $P|jq -r '.data.state')"

# ── SUB3-B 三级嵌套: main_nested→sub_middle→sub_grandchild ──
echo "--- SUB3-B 三级嵌套 ---"
r=$(start main_nested "" '{"initiator":"u_nest"}' "SUB3B-NEST"); N=$(iid "$r"); echo "$N">data/sub3b_iid.txt
# main 挂 sub_middle
mid=$(children $N | jq -r '[.data.children[]?|select(.definitionKey=="sub_middle")][0].id')
assert "SUB3B-mid" "main→挂载sub_middle" "true" "$([ -n "$mid" ] && echo true || echo false)"
assert "SUB3B-main-wait" "main在callActivity等待" "true" "$(inst $N|jq -r '.data.waitingSubflow')"
# sub_middle 第一个节点(中级审批)办结 → 内部 callActivity 挂 sub_grandchild
advc "$mid" 中级审批
gc=$(children $mid | jq -r '[.data.children[]?|select(.definitionKey=="sub_grandchild")][0].id')
echo "  嵌套链: main($N) → mid($mid) → gc($gc)"
assert "SUB3B-gc" "sub_middle→挂载sub_grandchild(孙)" "true" "$([ -n "$gc" ] && echo true || echo false)"
assert "SUB3B-mid-wait" "sub_middle在其callActivity等待" "true" "$(inst $mid|jq -r '.data.waitingSubflow')"
# 办结孙 → 逐级唤醒: gc完成→mid的call唤醒→mid完成→main的call唤醒→main完成
advc "$gc" 孙级审批
assert "SUB3B-gc-done" "孙子流程办结" "COMPLETED" "$(inst $gc|jq -r '.data.state')"
assert "SUB3B-mid-done" "孙完成逐级唤醒→中间子流程办结" "COMPLETED" "$(inst $mid|jq -r '.data.state')"
assert "SUB3B-main-done" "逐级唤醒到顶→主流程办结" "COMPLETED" "$(inst $N|jq -r '.data.state')"

# ── SUB3-C 子流程内退回(reject) 不影响父等待 ──
echo "--- SUB3-C 子流程内退回 ---"
# 用 fin_review_hq(三级) 作为组织路由子流程(zongbu), 在子流程内 fin3 退回 fin1
r=$(start main_org_routed zongbu '{"initiator":"u_subrej"}' "SUB3C-SUBREJ"); S=$(iid "$r"); echo "$S">data/sub3c_iid.txt
# zongbu→sub_review(单节点), 换个多节点子流程测退回: 直接用 travel_expense 的思路不便
# 改用 main_serial_multi call3(fin_review@zongbu→fin_review_hq 三级) 更适合退回
r=$(start main_serial_multi zongbu '{"initiator":"u_subrej2","amount":1}' "SUB3C-SUBREJ2"); S=$(iid "$r"); echo "$S">data/sub3c_iid.txt
t=$(taskof u_fin1 "$S"); complete "$t" "$S" 申请 >/dev/null
advc "$(children $S|jq -r '[.data.children[]?|select(.definitionKey=="sub_review")][0].id')"
advc "$(children $S|jq -r '[.data.children[]?|select(.definitionKey=="sub_risk")][0].id')"
hq=$(children $S|jq -r '[.data.children[]?|select(.definitionKey=="fin_review_hq")][0].id')
echo "  子流程(三级)=$hq"
# 推进 hq 到 fin3
advc "$hq"; advc "$hq"   # fin1→fin2→fin3
assert "SUB3C-at-fin3" "子流程推进到fin3" "fin3" "$(inst $hq|jq -r '.data.openTasks[0].nodeBpmnId')"
# fin3 退回 fin1
tj=$(inst $hq|jq -r '.data.openTasks[0].id')
reject "$tj" "$hq" "fin1" "子流程内退回" u_fin3 >/dev/null
assert "SUB3C-sub-back" "子流程内fin3退回→回到fin1" "fin1" "$(inst $hq|jq -r '.data.openTasks[0].nodeBpmnId')"
assert "SUB3C-parent-wait" "退回期间父在call3等待不受影响" "true" "$(inst $S|jq -r '.data.waitingSubflow')"
assert "SUB3C-sub-active" "退回后子流程仍ACTIVE" "ACTIVE" "$(inst $hq|jq -r '.data.state')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
