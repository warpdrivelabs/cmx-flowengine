#!/usr/bin/env bash
# 验证子流程 2 缺陷修复：#1 无绑定→Incident(可见可恢复,不留僵尸) ; #2 cancel/suspend 级联子流程
source "$(dirname "$0")/lib.sh"
export RESULTS="$PWD/logs/sub-fix-results.tsv"; : > "$RESULTS"
echo "════════ 子流程缺陷修复验证 ════════"
st() { inst "$1" | jq -r '.data.state'; }
FICO="postgres://postgres:postgres@127.0.0.1:5432/fico"

# ══ Fix #1: 无绑定 → Incident(不再僵尸) ══
echo "--- Fix#1 无绑定→Incident ---"
del_binding "$(binding_id_fnv ghost_key_no_binding)" >/dev/null 2>&1
r=$(start main_nobinding zongbu '{"initiator":"fixchk"}' "SUBFIX-NOBIND")
code=$(echo "$r"|jq -r '.code'); P=$(iid "$r")
echo "  start: code=$code data.id=$P"
# 新契约: start 成功(code=0)并返回实例 id
assert "F1-start-ok" "无绑定start成功返回(code=0)不再丢id" "0" "$code"
assert "F1-has-id" "start返回实例id(可排障)" "true" "$([ -n "$P" ] && [ "$P" != "null" ] && echo true || echo false)"
# 实例可见且带 incident
assert "F1-active" "实例保留Active" "ACTIVE" "$(st $P)"
assert "F1-incident" "hasIncident=true(可见)" "true" "$(inst $P|jq -r '.data.hasIncident')"
assert "F1-no-waiting-subflow" "无静默WaitingSubflow令牌" "false" "$(inst $P|jq -r '.data.waitingSubflow')"
assert "F1-no-child" "未产生子实例" "0" "$(children $P|jq -r '.data.children|length')"
inc=$(inst $P|jq -r '.data.incidents[0].reason // .data.incidents[0].node // "有"' 2>/dev/null)
echo "  incident: $(inst $P|jq -c '.data.incidents')"
assert "F1-incident-recorded" "incident记录了原因" "true" "$(inst $P|jq -r '(.data.incidents|length)>=1')"

# ── Fix #1 恢复性: 补绑定 + retry_incident → 继续推进 ──
echo "--- Fix#1 补绑定+retry恢复 ---"
upsert_binding ghost_key_no_binding "" sub_review >/dev/null
rr=$(j POST "/instances/$P/retry-incident" '{}')
echo "  retry: code=$(echo "$rr"|jq -r '.code')"
assert "F1-retry-ok" "retry-incident成功" "0" "$(echo "$rr"|jq -r '.code')"
# retry 后应解析成功 → 挂上 sub_review 子实例
child=$(children $P|jq -r '.data.children[0].definitionKey // "NONE"')
echo "  retry后子实例: $child, 父hasIncident=$(inst $P|jq -r '.data.hasIncident')"
assert "F1-recovered-child" "补绑定retry后挂上子流程sub_review" "sub_review" "$child"
assert "F1-incident-cleared" "恢复后不再有incident" "false" "$(inst $P|jq -r '.data.hasIncident')"
# 办结子 → 父办结
c=$(children $P|jq -r '.data.children[0].id')
t=$(inst $c|jq -r '.data.openTasks[0].id'); complete "$t" "$c" 办 >/dev/null
assert "F1-e2e-done" "恢复后端到端办结" "COMPLETED" "$(st $P)"
del_binding "$(binding_id_fnv ghost_key_no_binding)" >/dev/null 2>&1

# ══ Fix #2: cancel 级联子流程 ══
echo "--- Fix#2 取消级联 ---"
r=$(start main_org_routed fin_bj '{"initiator":"fix_cancel"}' "SUBFIX-CANCEL"); PC=$(iid "$r")
c=$(children $PC|jq -r '.data.children[0].id'); cdef=$(children $PC|jq -r '.data.children[0].definitionKey')
echo "  父=$PC 子=$c($cdef) 取消前子态=$(st $c)"
cancel "$PC" >/dev/null
echo "  取消后: 父=$(st $PC) 子=$(st $c)"
assert "F2-cancel-parent" "父取消→TERMINATED" "TERMINATED" "$(st $PC)"
assert "F2-cancel-child" "★子流程级联终止(不再孤立)" "TERMINATED" "$(st $c)"

# ══ Fix #2: 嵌套 cancel 级联到孙 ══
echo "--- Fix#2 嵌套取消级联到孙 ---"
r=$(start main_nested "" '{"initiator":"fix_nestcancel"}' "SUBFIX-NESTCANCEL"); PN=$(iid "$r")
mid=$(children $PN|jq -r '[.data.children[]?|select(.definitionKey=="sub_middle")][0].id')
# 推进 mid 到挂载孙
tm=$(inst $mid|jq -r '.data.openTasks[0].id'); complete "$tm" "$mid" 中级 >/dev/null
gc=$(children $mid|jq -r '[.data.children[]?|select(.definitionKey=="sub_grandchild")][0].id')
echo "  链: main=$PN mid=$mid gc=$gc; 取消前 mid=$(st $mid) gc=$(st $gc)"
cancel "$PN" >/dev/null
echo "  取消main后: main=$(st $PN) mid=$(st $mid) gc=$(st $gc)"
assert "F2-nest-main" "main取消→TERMINATED" "TERMINATED" "$(st $PN)"
assert "F2-nest-mid" "★中间子流程级联终止" "TERMINATED" "$(st $mid)"
assert "F2-nest-gc" "★孙子流程递归级联终止" "TERMINATED" "$(st $gc)"

# ══ Fix #2: suspend/resume 级联 ══
echo "--- Fix#2 挂起/恢复级联 ---"
r=$(start main_org_routed fin_bj '{"initiator":"fix_susp"}' "SUBFIX-SUSPEND"); PS=$(iid "$r")
cs=$(children $PS|jq -r '.data.children[0].id')
suspend "$PS" >/dev/null
echo "  挂起后: 父=$(st $PS) 子=$(st $cs)"
assert "F2-susp-parent" "父挂起→SUSPENDED" "SUSPENDED" "$(st $PS)"
assert "F2-susp-child" "★子流程级联挂起" "SUSPENDED" "$(st $cs)"
resume "$PS" >/dev/null
echo "  恢复后: 父=$(st $PS) 子=$(st $cs)"
assert "F2-resume-parent" "父恢复→ACTIVE" "ACTIVE" "$(st $PS)"
assert "F2-resume-child" "★子流程级联恢复ACTIVE" "ACTIVE" "$(st $cs)"
# 恢复后子能办结→父办结
while [ "$(st $cs)" = "ACTIVE" ]; do t=$(inst $cs|jq -r '.data.openTasks[0].id'); complete "$t" "$cs" 办 >/dev/null; done
assert "F2-resume-e2e" "级联恢复后端到端办结" "COMPLETED" "$(st $PS)"

summary; echo "PASS=$PASS TOTAL=$TOTAL"
