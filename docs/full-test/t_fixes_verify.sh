#!/usr/bin/env bash
# 验证 4 处缺陷修复
source "$(dirname "$0")/lib.sh"
echo "════════ 缺陷修复验证 ════════"

# ── Fix #1: /todos/initiated 按发起人过滤 ──
echo "--- Fix#1 我发起的按发起人过滤 ---"
ra=$(start approval_chain "" '{"initiator":"vf_alice"}' "VFIX-ALICE"); IA=$(iid "$ra")
rb=$(start approval_chain "" '{"initiator":"vf_bob"}'   "VFIX-BOB");   IB=$(iid "$rb")
alice=$(todos_initiated vf_alice | jq -c '[.data.tasks[]?|.instanceId]')
has_ia=$(echo "$alice" | jq -r 'index("'$IA'")!=null')
has_ib=$(echo "$alice" | jq -r 'index("'$IB'")!=null')
n_alice=$(echo "$alice" | jq 'length')
echo "alice列表: 含自己实例=$has_ia 含bob实例=$has_ib 条数=$n_alice"
assert "FIX1-own" "我发起的含本人实例" "true" "$has_ia"
assert "FIX1-noother" "我发起的不含他人(bob)实例" "false" "$has_ib"
assert "FIX1-scoped" "仅本人发起(此处=1条)" "1" "$n_alice"
# 空 user 仍返回(不因过滤而全空——空发起人=不过滤，保持兼容)
assert "FIX1-empty-user" "空user不过滤(返回≥2)" "true" "$([ "$(todos_initiated '' | jq '.data.tasks|length')" -ge 2 ] && echo true || echo false)"

# ── Fix #2: Swagger /docs 跳转保留 /api 前缀 ──
echo "--- Fix#2 Swagger跳转 ---"
loc=$(curl -s -o /dev/null -w '%{redirect_url}' -H "$K" "http://127.0.0.1:8091/api/flow/v1/docs")
final=$(curl -sL -o /dev/null -w '%{http_code}' -H "$K" "http://127.0.0.1:8091/api/flow/v1/docs")
echo "无尾斜杠跳转到: $loc ; 跟随后最终码: $final"
assert "FIX2-redirect-keeps-api" "跳转URL含/api前缀" "true" "$(echo "$loc" | grep -q '/api/flow/v1/docs' && echo true || echo false)"
assert "FIX2-follow-200" "跟随跳转最终200(不再404)" "200" "$final"
assert "FIX2-openapi" "openapi.json仍200" "200" "$(curl -s -o /dev/null -w '%{http_code}' -H "$K" 'http://127.0.0.1:8091/api/flow/v1/openapi.json')"

# ── Fix #3: complete 记录办理人到审计留痕 ──
echo "--- Fix#3 complete办理人留痕 ---"
r=$(start approval_chain "" '{"initiator":"vf_c"}' "VFIX-OPERATOR"); P=$(iid "$r")
t=$(taskof u_fin1 "$P")
# 带 operator 办结
j POST "/tasks/$t/complete" "$(jq -n --arg i "$P" '{instanceId:$i, comment:"我审批的", operator:"u_fin1"}')" >/dev/null
uid=$(icomments "$P" | jq -r '[.data.comments[]?|select(.taskId=="'$t'")][0].userId // "NULL"')
echo "complete留痕 userId=$uid (修复前为NULL)"
assert "FIX3-audit-user" "办结留痕记录办理人=u_fin1" "u_fin1" "$uid"

# ── Fix #4: kind=all = 直派 ∪ 可认领 ──
echo "--- Fix#4 kind=all并集 ---"
# 确保 u_fin1 既有直派(approval_chain l1)又有可认领(probe_cand finance池)
start probe_cand fin_bj '{"initiator":"vf_pool"}' "VFIX-POOL" >/dev/null
todo_n=$(mytasks u_fin1 | jq -r '.data.total')
claim_n=$(mytasks u_fin1 "kind=claimable" | jq -r '.data.total')
all_n=$(mytasks u_fin1 "kind=all" | jq -r '.data.total')
all_has_pool=$(mytasks u_fin1 "kind=all" | jq -r '[.data.tasks[]?|select(.claimable==true)]|length>=1')
echo "u_fin1: todo=$todo_n claimable=$claim_n all=$all_n (all含可认领=$all_has_pool)"
assert "FIX4-all-union" "kind=all总数=直派+可认领($todo_n+$claim_n)" "$((todo_n+claim_n))" "$all_n"
assert "FIX4-all-haspool" "kind=all含可认领任务" "true" "$all_has_pool"
assert "FIX4-all-gt-todo" "kind=all>kind=todo(真的合并了)" "true" "$([ "$all_n" -gt "$todo_n" ] && echo true || echo false)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
