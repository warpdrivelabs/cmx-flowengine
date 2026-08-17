#!/usr/bin/env bash
# SUITE 11 —— 反常/边界猎bug：契约异常验证 + MI退回挡回 + 空集合 + 无效退回目标 + 审计留痕 + correlate
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 11: 反常 / 边界 / 猎bug ════════"

# ── 11A 【疑似bug】/todos/initiated 是否真按发起人过滤？──
echo "--- 11A todos/initiated 发起人过滤 ---"
ra=$(start approval_chain "" '{"initiator":"alice_init"}' "S11-INIT-ALICE"); IA=$(iid "$ra")
rb=$(start approval_chain "" '{"initiator":"bob_init"}'   "S11-INIT-BOB");   IB=$(iid "$rb")
alice_list=$(todos_initiated alice_init | jq -c '[.data.tasks[]?|.instanceId]')
bob_has_in_alice=$(echo "$alice_list" | jq -r 'index("'$IB'")!=null')
echo "alice's initiated list contains bob's instance? $bob_has_in_alice"
echo "  alice_init list size: $(echo "$alice_list"|jq 'length')"
# 期望：按发起人过滤时 alice 的列表不含 bob 的实例
assert "S11A-initiated-filter" "todos/initiated?user=alice 不应含 bob 的实例" "false" "$bob_has_in_alice"

# ── 11B MI/会签域任务退回应被挡回（rejectable:false）──
echo "--- 11B 会签任务不可退回 ---"
r=$(start cs_all "" '{"approvers":["u_fin1","u_fin2"]}' "S11-MI-REJECT"); MI=$(iid "$r")
t=$(taskof u_fin1 "$MI")
rt=$(rtargets "$t" "$MI")
echo "MI reject-targets: $(echo "$rt"|jq -c '.data')"
assert "S11B-mi-noreject" "会签子任务rejectable=false" "false" "$(echo "$rt"|jq -r '.data.rejectable')"

# ── 11C 空 MI 集合 → 跳过节点 ──
echo "--- 11C 空集合MI跳过 ---"
r=$(start cs_all "" '{"approvers":[]}' "S11-MI-EMPTY"); ME=$(iid "$r")
echo "empty-MI state: $(show $ME)"
assert "S11C-empty-mi" "空approvers→会签节点跳过→COMPLETED" "COMPLETED" "$(inst $ME|jq -r '.data.state')"

# ── 11D 退回到无效/非上游目标 → 应报错或忽略 ──
echo "--- 11D 无效退回目标 ---"
r=$(start approval_chain "" '{"initiator":"u_z"}' "S11-BADREJECT")
P=$(iid "$r"); t=$(taskof u_fin1 "$P")
# l1 是首节点，尝试退回到不存在的节点
rr=$(reject "$t" "$P" "no_such_node" "退回到不存在节点" u_fin1)
echo "reject to invalid node => $(echo "$rr"|jq -c '{code,msg}')"
# 记录行为（应 code=1 拒绝，不应把令牌送到虚空）
stafter=$(inst "$P"|jq -r '.data.state')
node_after=$(inst "$P"|jq -r '.data.openTasks[0].nodeBpmnId // "NONE"')
echo "after invalid reject: state=$stafter node=$node_after"
assert "S11D-invalid-reject" "无效退回目标被拒(code=1)或安全留在l1" "true" "$([ "$(echo "$rr"|jq -r '.code')" = "1" ] || [ "$node_after" = "l1" ] && echo true || echo false)"

# ── 11E 【审计】complete 是否记录办理人？──
echo "--- 11E complete 审计留痕 ---"
r=$(start approval_chain "" '{"initiator":"u_audit"}' "S11-AUDIT")
P=$(iid "$r"); t=$(taskof u_fin1 "$P")
complete "$t" "$P" "审计意见测试" >/dev/null
uid=$(icomments "$P" | jq -r '.data.comments[0].userId // "NULL"')
echo "complete comment userId = $uid (契约: complete 无办理人入参)"
# 记录为发现（非阻断）：complete 不带办理人 → userId 为空
assert "S11E-audit-note" "记录:complete意见留痕存在(userId=$uid)" "true" "$(icomments "$P"|jq -r '(.data.comments|length)>=1')"

# ── 11F kind=all 行为（契约异常：等同 todo）──
echo "--- 11F tasks/my kind=all ---"
r=$(start approval_chain "" '{"initiator":"u_k"}' "S11-KINDALL"); KA=$(iid "$r")
todo_n=$(mytasks u_fin1 | jq -r '.data.total')
all_n=$(mytasks u_fin1 "kind=all" | jq -r '.data.total')
echo "u_fin1 todo=$todo_n all=$all_n (契约称 kind=all 实际=todo)"
assert "S11F-kindall" "kind=all 有响应(不报错)" "true" "$([ -n "$all_n" ] && echo true || echo false)"

# ── 11G correlate_message（A4 外部消息唤醒）端点存活 ──
echo "--- 11G correlate_message ---"
cr=$(j POST /messages/correlate '{"messageName":"test_msg","correlationKey":"none","variables":{}}')
echo "correlate => $(echo "$cr"|jq -c '{code,msg}')"
assert "S11G-correlate" "correlate端点有结构化响应" "true" "$(echo "$cr"|jq -r 'has("code")')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
