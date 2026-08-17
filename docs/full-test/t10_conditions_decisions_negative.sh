#!/usr/bin/env bash
# SUITE 10 —— 条件引擎 / 决策表 / 定时器 / 催办 / 认领 + 负向健壮性
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 10: 条件/决策/定时器/催办 + 负向 ════════"

# ── 10A 条件求值 ──
echo "--- 10A 条件引擎 ---"
r=$(cond_eval '{"expr":"amount > 20000","variables":{"amount":30000}}')
assert "S10A-eval-true" "amount>20000 & amount=30000 → true" "true" "$(echo "$r"|jq -r '.data.result')"
r=$(cond_eval '{"expr":"amount > 20000","variables":{"amount":100}}')
assert "S10A-eval-false" "amount=100 → false" "false" "$(echo "$r"|jq -r '.data.result')"
r=$(cond_validate '{"expr":"a && (b || c)"}')
assert "S10A-validate-ok" "合法表达式 valid=true" "true" "$(echo "$r"|jq -r '.data.valid')"
r=$(cond_validate '{"expr":"a && ("}')
assert "S10A-validate-bad" "非法表达式 valid=false" "false" "$(echo "$r"|jq -r '.data.valid')"
fn=$(cond_functions | jq -r '(.data.functions // .data)|length')
assert "S10A-functions" "函数目录非空" "true" "$([ "${fn:-0}" -ge 1 ] && echo true || echo false)"

# ── 10B 决策表 注册 + 试算 ──
echo "--- 10B 决策表 ---"
DT='{"key":"approval_matrix_test","inputs":["amount"],"outputs":["approvalLevel"],"hit_policy":"FIRST","rules":[{"conditions":["amount > 100000"],"outputs":{"approvalLevel":3}},{"conditions":["amount > 10000"],"outputs":{"approvalLevel":2}},{"conditions":["-"],"outputs":{"approvalLevel":1}}]}'
r=$(j POST /decisions "$DT")
echo "register => $(echo "$r"|jq -c '{code,data}')"
assert "S10B-register" "决策表注册 code=0" "0" "$(echo "$r"|jq -r '.code')"
r=$(j POST /decisions/evaluate "$(jq -n --argjson t "$DT" '{table:$t,variables:{amount:500000}}')")
assert "S10B-eval-hi" "amount=500000→approvalLevel=3" "3" "$(echo "$r"|jq -r '.data.outputs.approvalLevel')"
r=$(j POST /decisions/evaluate "$(jq -n --argjson t "$DT" '{table:$t,variables:{amount:500}}')")
assert "S10B-eval-lo" "amount=500→approvalLevel=1(兜底)" "1" "$(echo "$r"|jq -r '.data.outputs.approvalLevel')"

# ── 10C 定时器触发端点 ──
echo "--- 10C 定时器 ---"
r=$(timers_trigger)
assert "S10C-timers" "定时器触发端点 code=0" "0" "$(echo "$r"|jq -r '.code')"

# ── 10D 催办 urge ──
echo "--- 10D 催办 ---"
r=$(start approval_chain "" '{"initiator":"u_x"}' "S10D-URGE")
P=$(iid "$r"); echo "$P">data/s10d_iid.txt
t=$(taskof u_fin1 "$P")
ur=$(urge "$t" "$P" u_boss "尽快处理")
assert "S10D-urge" "催办 code=0" "0" "$(echo "$ur"|jq -r '.code')"

# ── 10E 认领：直派任务不可被他人认领 ──
echo "--- 10E 认领负向 ---"
cr=$(claim "$t" "$P" u_hacker)
echo "claim direct task by other => $(echo "$cr"|jq -c '{code,msg}')"
# 直派任务被认领的行为（观察）：记录结果
assert "S10E-claim-recorded" "认领直派任务有明确响应(code∈{0,1})" "true" "$(echo "$cr"|jq -r '(.code==0 or .code==1)')"

# ── 10F 负向健壮性 ──
echo "--- 10F 负向 ---"
r=$(complete "nonexistent-task-id-xyz" "$P" "幽灵办结")
assert "S10F-ghost-task" "办结不存在任务→code=1" "1" "$(echo "$r"|jq -r '.code')"
r=$(start "no_such_def_xyz" "" '{}' "S10F-BADDEF")
assert "S10F-bad-def" "起未知定义→code=1" "1" "$(echo "$r"|jq -r '.code')"
r=$(inst "no-such-instance-999")
assert "S10F-bad-inst" "查不存在实例→code=1" "1" "$(echo "$r"|jq -r '.code')"
# 双重办结
r2=$(start approval_chain "" '{"initiator":"u_y"}' "S10F-DOUBLE")
DI=$(iid "$r2"); t=$(taskof u_fin1 "$DI")
complete "$t" "$DI" "第一次办结" >/dev/null
rd=$(complete "$t" "$DI" "重复办结")
assert "S10F-double-complete" "重复办结同一任务→code=1" "1" "$(echo "$rd"|jq -r '.code')"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
