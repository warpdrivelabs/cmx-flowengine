#!/usr/bin/env bash
# SUITE 7 —— 抄送 CC：节点级 cmx:cc（固定人 + 角色）+ 抄送列表 + 已读 + todos/cc
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 7: 抄送 CC ════════"
r=$(start cc_flow "" '{"initiator":"u_boss"}' "S7-CC")
P=$(iid "$r"); echo "inst=$P"; echo "$P">data/s7_iid.txt

# ── 7A apply(cc=u_auditor1) 办结 → 抄送 u_auditor1 ──
echo "--- 7A 固定人抄送 ---"
t=$(taskof u_bjlead "$P"); complete "$t" "$P" "经理批准" >/dev/null
cc1=$(cc_list u_auditor1 | jq -c '[.data.cc[]?|select(.instanceId=="'$P'")]')
echo "u_auditor1 cc for inst: $cc1"
assert "S7A-cc-created" "apply办结→抄送u_auditor1(≥1条)" "true" "$(echo "$cc1"|jq -r 'length>=1')"
assert "S7A-cc-unread" "抄送初始未读read=false" "false" "$(echo "$cc1"|jq -r '.[0].read')"
# 记录 cc id
ccid=$(echo "$cc1"|jq -r '.[0].id')
# 在实例视图里也应有 ccRecords
assert "S7A-inst-cc" "实例视图ccRecords含该抄送" "true" "$(inst $P|jq -r '[.data.ccRecords[]?|select(.toUserId=="u_auditor1")]|length>=1')"

# ── 7B 标记已读 ──
echo "--- 7B 抄送已读 ---"
rr=$(cc_read "$ccid")
assert "S7B-read-ok" "标记已读 ok=true" "true" "$(echo "$rr"|jq -r '.data.ok')"
readnow=$(cc_list u_auditor1 | jq -r '[.data.cc[]?|select(.id=="'$ccid'")][0].read')
assert "S7B-read-true" "已读后read=true" "true" "$readnow"
# unread 过滤应排除已读
inunread=$(cc_list u_auditor1 true | jq -r '[.data.cc[]?|select(.id=="'$ccid'")]|length')
assert "S7B-unread-filter" "unread=true过滤掉已读" "0" "$inunread"

# ── 7C final(cc=role(auditor)) 办结 → 角色抄送解析到 u_auditor1 ──
echo "--- 7C 角色抄送 ---"
t=$(taskof u_cfo "$P"); complete "$t" "$P" "终审通过" >/dev/null
total_cc=$(cc_list u_auditor1 | jq -r '[.data.cc[]?|select(.instanceId=="'$P'")]|length')
echo "u_auditor1 total cc for inst now: $total_cc"
assert "S7C-role-cc" "role(auditor)抄送解析到u_auditor1(累计≥2)" "true" "$([ "${total_cc:-0}" -ge 2 ] && echo true || echo false)"
assert "S7C-done" "cc_flow办结COMPLETED" "COMPLETED" "$(inst $P|jq -r '.data.state')"

# ── 7D todos/cc 抄送待办中心 ──
echo "--- 7D 抄送待办中心 ---"
tc=$(todos_cc u_auditor1 | jq -r '.data.total // (.data.tasks|length)')
echo "todos/cc total for u_auditor1: $tc"
assert "S7D-todos-cc" "todos/cc?user=u_auditor1有记录" "true" "$([ "${tc:-0}" -ge 1 ] && echo true || echo false)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
