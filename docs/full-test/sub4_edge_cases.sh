#!/usr/bin/env bash
# SUB-SUITE 4 —— 子流程边界/异常：无绑定 + 取消级联 + 挂起级联 + 未部署key
# 说明: 引擎语义(修复后): 无绑定→父挂载点令牌转 Incident(可见+可 retry 恢复,不留僵尸);
#       cancel/suspend/resume 级联到子实例(含递归到孙)。本套件断言修复后的正确行为。
source "$(dirname "$0")/lib.sh"
echo "════════ SUB4: 子流程边界/异常 ════════"
st() { inst "$1" | jq -r '.data.state'; }

# ── SUB4-A 无绑定+无默认 → start成功但挂载点转Incident(可见,不留僵尸WaitingSubflow) ──
echo "--- SUB4-A 无绑定→Incident ---"
# 测试隔离：先清掉 ghost_key_no_binding 的任何遗留绑定（SUB4-B 会补，故重跑前必清），
# 确保本例真的"无绑定"。绑定 id = binding_id(key, None) 的 FNV-1a（见 handlers.rs）。
del_binding "$(binding_id_fnv ghost_key_no_binding)" >/dev/null 2>&1
r=$(start main_nobinding zongbu '{"initiator":"u_nb"}' "SUB4A-NOBIND")
code=$(echo "$r" | jq -r '.code'); P=$(iid "$r")
echo "  起 main_nobinding => code=$code data.id=$P"
# 修复后契约: start 成功(code=0)且返回实例 id(不再丢);实例保留 Active 且 hasIncident
assert "SUB4A-start-ok" "无绑定start成功(code=0)不丢id" "0" "$code"
assert "SUB4A-has-id" "start返回实例id(可排障)" "true" "$([ -n "$P" ] && [ "$P" != "null" ] && echo true || echo false)"
assert "SUB4A-incident" "无绑定→hasIncident=true(可见,非僵尸)" "true" "$(inst $P|jq -r '.data.hasIncident')"
assert "SUB4A-no-waiting" "无静默WaitingSubflow令牌" "false" "$(inst $P|jq -r '.data.waitingSubflow')"
assert "SUB4A-no-child" "无绑定→未产生子实例" "0" "$(children $P|jq -r '.data.children|length')"
echo "$P">data/sub4a_iid.txt

# ── SUB4-B 补绑定 + retry-incident → 恢复推进(可恢复性)──
echo "--- SUB4-B 补绑定+retry恢复 ---"
upsert_binding ghost_key_no_binding "" sub_review >/dev/null   # 补默认兜底
rr=$(j POST "/instances/$P/retry-incident" '{}')
assert "SUB4B-retry-ok" "retry-incident成功(code=0)" "0" "$(echo "$rr"|jq -r '.code')"
assert "SUB4B-recovered" "补绑定retry后挂上sub_review" "sub_review" "$(children $P|jq -r '.data.children[0].definitionKey // "NONE"')"
assert "SUB4B-incident-cleared" "恢复后不再有incident" "false" "$(inst $P|jq -r '.data.hasIncident')"
del_binding "$(binding_id_fnv ghost_key_no_binding)" >/dev/null 2>&1  # 清理,保持测试隔离

# ── SUB4-C 父取消 → 子流程级联终止(不再孤立)──
echo "--- SUB4-C 父取消级联 ---"
r=$(start main_org_routed fin_bj '{"initiator":"u_cancel"}' "SUB4C-CANCEL"); PC=$(iid "$r"); echo "$PC">data/sub4c_iid.txt
c=$(children $PC | jq -r '.data.children[0].id')
cdef=$(children $PC | jq -r '.data.children[0].definitionKey')
echo "  父=$PC 子=$c($cdef) 取消前子态=$(st $c)"
cancel "$PC" >/dev/null
echo "  取消后: 父=$(st $PC) 子=$(st $c)"
assert "SUB4C-parent-terminated" "父取消→父TERMINATED" "TERMINATED" "$(st $PC)"
assert "SUB4C-child-cascade" "★子流程级联终止(修复:不再孤立)" "TERMINATED" "$(st $c)"

# ── SUB4-D 父挂起/恢复 → 子流程级联 ──
echo "--- SUB4-D 父挂起/恢复级联 ---"
r=$(start main_org_routed fin_bj '{"initiator":"u_susp"}' "SUB4D-SUSPEND"); PS=$(iid "$r"); echo "$PS">data/sub4d_iid.txt
cs=$(children $PS | jq -r '.data.children[0].id')
suspend "$PS" >/dev/null
echo "  父挂起后: 父=$(st $PS) 子=$(st $cs)"
assert "SUB4D-parent-suspended" "父挂起→父SUSPENDED" "SUSPENDED" "$(st $PS)"
assert "SUB4D-child-cascade" "★子流程级联挂起(修复)" "SUSPENDED" "$(st $cs)"
resume "$PS" >/dev/null
assert "SUB4D-child-resume" "★父恢复→子级联恢复ACTIVE(修复)" "ACTIVE" "$(st $cs)"
# 恢复父, 办结子, 看父能否正常唤醒
while [ "$(st $cs)" = "ACTIVE" ]; do t=$(inst $cs|jq -r '.data.openTasks[0].id'); complete "$t" "$cs" 办 >/dev/null; done
assert "SUB4D-resume-flow" "父恢复+子办结→父被唤醒办结" "COMPLETED" "$(st $PS)"

# ── SUB4-E 空/单节点子流程即时完成（launch 即 complete）──
echo "--- SUB4-E 组织路由到单节点子流程正常收口 ---"
# zongbu→sub_review(单节点), 已在 SUB2 验证; 此处验证launch后父正确进入等待而非直接跳过
r=$(start main_org_routed zongbu '{"initiator":"u_single"}' "SUB4E-SINGLE"); PE=$(iid "$r"); echo "$PE">data/sub4e_iid.txt
assert "SUB4E-waiting" "单节点子流程:父仍WaitingSubflow(非空,不跳过)" "true" "$(inst $PE|jq -r '.data.waitingSubflow')"
ce=$(children $PE|jq -r '.data.children[0].id')
t=$(inst $ce|jq -r '.data.openTasks[0].id'); complete "$t" "$ce" 办 >/dev/null
assert "SUB4E-done" "单节点子流程办结→父办结" "COMPLETED" "$(st $PE)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"

