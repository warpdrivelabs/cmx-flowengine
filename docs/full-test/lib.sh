#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# cmx-flowengine 全量功能测试 · REST 助手库
# 契约（经源码核对，含大小写陷阱）：
#   信封：成功 {code:0,msg,data}；业务失败 HTTP 200 + {code:1,msg}（须断言 code 非 HTTP 码）
#   鉴权：X-API-Key 仅决定租户；办理人一律显式传 body/query
#   驼峰 body：complete/reject/urge/start/jump/withdraw
#   蛇形 body：claim/transfer/delegate/addsign（instance_id/from_user/to_user/user_id）
# 用法：source lib.sh ；每个请求都追加进 logs/transcript.log（数据全留存）
# ─────────────────────────────────────────────────────────────────────────────
BASE="http://127.0.0.1:8091/api/flow/v1"
# 鉴权：走 off 模式头路径（X-Tenant 定租户；X-User 定调用者身份，满足 T0/T0b 任务端点授权）。
# 不用 X-API-Key——它在 auth 中间件里优先命中服务身份分支(current_user=None)，会短路掉读 X-User 的
# off 路径，导致 complete/reject 等因「缺少用户身份」被拒。故服务器须 auth.mode=off（默认）起。
K="X-Tenant: default"
CT="Content-Type: application/json"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# LOG/RESULTS 可由调用方 export 覆盖（统一入口用它汇总到独立账本）；否则用默认。
LOG="${LOG:-$HERE/logs/transcript.log}"
RESULTS="${RESULTS:-$HERE/logs/results.tsv}"
mkdir -p "$HERE/logs" "$HERE/data"
: > "${LOG}.tmp" 2>/dev/null || true

_ts() { date +"%H:%M:%S"; }   # display only; not used for logic

# 核心请求：j METHOD PATH [jsonBody] [actingUser] —— 回显响应 body，全程记账
# actingUser（可选）：off 模式下作为 X-User 头 → current_user，满足 T0/T0b 任务端点调用者授权。
j() {
  local m="$1" p="$2" body="${3:-}" user="${4:-}"
  local resp hdrs=(-H "$K")
  [ -n "$user" ] && hdrs+=(-H "X-User: $user")
  if [ -n "$body" ]; then
    resp=$(curl -s -X "$m" "${hdrs[@]}" -H "$CT" -d "$body" "$BASE$p")
  elif [ "$m" = "GET" ]; then
    resp=$(curl -s "${hdrs[@]}" "$BASE$p")
  else
    resp=$(curl -s -X "$m" "${hdrs[@]}" "$BASE$p")
  fi
  { echo "### $m $p"; [ -n "$user" ] && echo "USER $user"; [ -n "$body" ] && echo "REQ  $body"; echo "RESP $resp"; echo; } >> "$LOG"
  echo "$resp"
}
# 只取 HTTP 码
jcode() {
  local m="$1" p="$2" body="${3:-}"
  if [ -n "$body" ]; then curl -s -o /dev/null -w "%{http_code}" -X "$m" -H "$K" -H "$CT" -d "$body" "$BASE$p"
  else curl -s -o /dev/null -w "%{http_code}" -X "$m" -H "$K" "$BASE$p"; fi
}

# ── 定义生命周期 ──────────────────────────────────────────────────────────────
validate_bpmn() { local body; body=$(jq -Rs '{bpmnXml:.}' < "$1"); j POST /definitions/validate "$body"; }
save_draft() {  # save_draft <name> <bpmnFile>
  local name="$1" f="$2"
  jq -Rs --arg n "$name" '{name:$n, bpmnXml:.}' < "$f" | curl -s -X POST -H "$K" -H "$CT" -d @- "$BASE/definitions/draft" \
    | tee -a "$LOG" ; echo >> "$LOG"
}
publish() { j POST "/definitions/$1/publish" "$(jq -n --arg by "tester" '{note:"full-test",publishedBy:$by}')"; }
deploy() {  # deploy <name> <bpmnFile>  —— 存草稿+发布，回显 key
  local name="$1" f="$2" key
  key=$(save_draft "$name" "$f" | jq -r '.data.key // empty')
  [ -z "$key" ] && { echo "DEPLOY-FAIL: $f"; return 1; }
  publish "$key" >/dev/null
  echo "$key"
}
defs() { j GET /definitions; }
defdetail() { j GET "/definitions/$1"; }
versions() { j GET "/definitions/$1/versions"; }

# ── 实例 ──────────────────────────────────────────────────────────────────────
# start <defKey> <orgId> <varsJson> [businessKey]
start() {
  local k="$1" o="$2" v="$3" bk="${4:-}"; [ -z "$v" ] && v='{}'
  local body
  body=$(jq -n --arg k "$k" --arg o "$o" --argjson v "$v" --arg bk "$bk" \
    '{definitionKey:$k, variables:$v} + (if $o=="" then {} else {orgId:$o} end) + (if $bk=="" then {} else {businessKey:$bk} end)')
  j POST /instances "$body"
}
inst()     { j GET "/instances/$1"; }
children() { j GET "/instances/$1/children"; }
cancel()   { j POST "/instances/$1/cancel" '{"reason":"test-cancel"}'; }
suspend()  { j POST "/instances/$1/suspend"; }
resume()   { j POST "/instances/$1/resume"; }
jump()     { j POST "/instances/$1/jump" "$(jq -n --arg t "$2" --arg r "${3:-test-jump}" '{targetBpmnId:$t,reason:$r}')"; }
setvars()  { j POST "/instances/$1/set-variables" "$(jq -n --argjson v "$2" '{variables:$v}')"; }
ivars()    { j GET "/instances/$1/variables"; }
icomments(){ j GET "/instances/$1/comments"; }

# ── 待办查询 ──────────────────────────────────────────────────────────────────
mytasks()  { j GET "/tasks/my?assignee=$1${2:+&$2}"; }
# taskof <assignee> <instanceId> —— 该实例名下首个未完成(直派)任务 id
taskof()   { j GET "/tasks/my?assignee=$1" | jq -r --arg i "$2" '.data.tasks[]?|select(.instanceId==$i)|.taskId' | head -1; }
# claimtaskof <assignee> <instanceId> —— 该实例名下首个可认领(候选池)任务 id
claimtaskof() { j GET "/tasks/my?assignee=$1&kind=claimable" | jq -r --arg i "$2" '.data.tasks[]?|select(.instanceId==$i)|.taskId' | head -1; }
# nodeof <assignee> <instanceId> —— 对应节点
nodeof()   { j GET "/tasks/my?assignee=$1" | jq -r --arg i "$2" '.data.tasks[]?|select(.instanceId==$i)|.nodeBpmnId' | head -1; }
# iid <startResponse> —— 取实例 id（实例视图顶层字段是 .data.id）
iid() { echo "$1" | jq -r '.data.id // empty'; }

# ── 任务动作（注意大小写！）─────────────────────────────────────────────────
# T0/T0b：任务端点校验调用者身份（off 模式下经 X-User → current_user，须为办理人/候选/发起人）。
# 各助手把「实际办理人」作为 X-User 传给 j：complete/reject 缺省从任务当前 assignee 推断；
# claim/transfer/delegate/addsign/urge/withdraw 用其显式办理人参数。
# task_assignee <taskId> <instanceId> —— 取该任务当前 assignee（推断 complete/reject 的调用者）
# 注意：实例视图 openTasks 的任务 id 字段是 .id（非 /tasks/my 的 .taskId）。
task_assignee() { inst "$2" | jq -r --arg t "$1" '.data.openTasks[]?|select(.id==$t)|.assignee // empty' | head -1; }
# complete <taskId> <instanceId> [comment] [decision] [varsJson]
complete() {
  local t="$1" i="$2" c="${3:-同意}" d="${4:-}" v="$5"; [ -z "$v" ] && v='{}'
  local u; u=$(task_assignee "$t" "$i")
  local body
  body=$(jq -n --arg i "$i" --arg c "$c" --arg d "$d" --argjson v "$v" \
    '{instanceId:$i, comment:$c, variables:$v} + (if $d=="" then {} else {decision:$d} end)')
  j POST "/tasks/$t/complete" "$body" "$u"
}
# reject <taskId> <instanceId> [targetBpmnId] [reason] [fromUser]
reject() {
  local t="$1" i="$2" tgt="${3:-}" r="${4:-退回修改}" fu="${5:-}"
  local u="$fu"; [ -z "$u" ] && u=$(task_assignee "$t" "$i")
  local body
  body=$(jq -n --arg i "$i" --arg r "$r" --arg tgt "$tgt" --arg fu "$fu" \
    '{instanceId:$i, reason:$r} + (if $tgt=="" then {} else {targetBpmnId:$tgt} end) + (if $fu=="" then {} else {fromUser:$fu} end)')
  j POST "/tasks/$t/reject" "$body" "$u"
}
rtargets() { j GET "/tasks/$1/reject-targets?instanceId=$2"; }
# claim/transfer/delegate/addsign —— 蛇形！调用者=claimer/from（作 X-User）。
claim()    { j POST "/tasks/$1/claim" "$(jq -n --arg i "$2" --arg u "$3" '{instance_id:$i,user_id:$u}')" "$3"; }
transfer() { j POST "/tasks/$1/transfer" "$(jq -n --arg i "$2" --arg f "$3" --arg t "$4" --arg r "${5:-转办}" '{instance_id:$i,from_user:$f,to_user:$t,reason:$r}')" "$3"; }
delegate() { j POST "/tasks/$1/delegate" "$(jq -n --arg i "$2" --arg f "$3" --arg t "$4" --arg r "${5:-委托}" '{instance_id:$i,from_user:$f,to_user:$t,reason:$r}')" "$3"; }
# addsign <taskId> <instanceId> <fromUser> <toUser> [before(true/false)] [reason]
addsign()  { j POST "/tasks/$1/addsign" "$(jq -n --arg i "$2" --arg f "$3" --arg t "$4" --argjson b "${5:-true}" --arg r "${6:-加签}" '{instance_id:$i,from_user:$f,to_user:$t,before:$b,reason:$r}')" "$3"; }
urge()     { j POST "/tasks/$1/urge" "$(jq -n --arg i "$2" --arg f "${3:-}" --arg m "${4:-催办}" '{instanceId:$i,fromUser:$f,message:$m}')" "${3:-}"; }

# ── 取回/撤回 ──────────────────────────────────────────────────────────────────
withdrawable() { j GET "/instances/$1/withdrawable?user=$2"; }
withdraw()     { j POST "/instances/$1/withdraw" "$(jq -n --arg u "$2" --arg r "${3:-发起人取回}" '{user:$u,reason:$r}')" "$2"; }

# ── 抄送 ──────────────────────────────────────────────────────────────────────
cc_list()  { j GET "/cc?user=$1${2:+&unread=$2}"; }
cc_read()  { j POST "/cc/$1/read"; }
todos_cc() { j GET "/todos/cc?user=$1"; }
todos_done(){ j GET "/todos/done?user=$1"; }
todos_initiated(){ j GET "/todos/initiated?user=$1"; }
todos_filters(){ j GET /todos/filters; }

# ── 子流程绑定 / 组织 / 用户 ───────────────────────────────────────────────────
orgs() { j GET /orgs; }
users() { j GET /users; }
bind() { j POST /subflow-bindings "$1"; }
binds() { j GET "/subflow-bindings/$1"; }
# upsert_binding <calledKey> <orgId('' =默认兜底)> <targetKey> [enabled(true)]
upsert_binding() {
  local ck="$1" org="$2" tk="$3" en="${4:-true}"
  j POST /subflow-bindings "$(jq -n --arg ck "$ck" --arg o "$org" --arg tk "$tk" --argjson en "$en" \
    '{calledKey:$ck, targetKey:$tk, enabled:$en} + (if $o=="" then {} else {orgId:$o} end)')"
}
del_binding() { j DELETE "/subflow-bindings/id/$1"; }
# binding_id_fnv <calledKey> [orgId] —— 复刻 handlers.rs binding_id() 的 FNV-1a，用于精确删除
binding_id_fnv() {
  local raw="$1|${2:-__default__}"
  python3 - "$raw" <<'PY'
import sys
h=0xcbf29ce484222325
for b in sys.argv[1].encode():
    h^=b; h=(h*0x100000001b3)&0xFFFFFFFFFFFFFFFF
print("sb_%016x"%h)
PY
}

# ── 条件 / 决策 / 定时器 / 身份 ─────────────────────────────────────────────────
cond_eval() { j POST /conditions/eval "$1"; }
cond_validate() { j POST /conditions/validate "$1"; }
cond_functions() { j GET /conditions/functions; }
timers_trigger() { j POST /timers/trigger '{}'; }
identity_mode() { j GET /identity/mode; }

# ── 精简展示（实例视图：id/state/tokens/openTasks/candidates）──────────────────
show() { inst "$1" | jq -c '{id:.data.id, state:.data.state, active:[.data.tokens[]?|select(.state!="ENDED")|.nodeBpmnId], open:[.data.openTasks[]?|{node:.nodeBpmnId,who:.assignee,cand:[.candidates[]?.userId],elem:.elementValue,owner:.ownerUserId,par:.parentTaskId,del:.delegationState}], cc:[.data.ccRecords[]?|{to:.toUserId,read:.readAt}], deleg:.data.delegations}'; }
showchild() { children "$1" | jq -c '.data.children[]?|{id:.id,def:.definitionKey,state:.state,open:[.openTasks[]?|{node:.nodeBpmnId,who:.assignee}]}'; }

# ── 断言账本 ──────────────────────────────────────────────────────────────────
# assert <id> <desc> <expected> <actual>
TOTAL=0; PASS=0
assert() {
  TOTAL=$((TOTAL+1))
  local id="$1" desc="$2" exp="$3" act="$4" ok="FAIL"
  [ "$exp" = "$act" ] && { ok="PASS"; PASS=$((PASS+1)); }
  printf "%s\t%s\t%s\texp=%s\tact=%s\n" "$id" "$ok" "$desc" "$exp" "$act" >> "$RESULTS"
  printf "[%s] %-6s %s  (exp=%s act=%s)\n" "$id" "$ok" "$desc" "$exp" "$act"
}
# assert_contains <id> <desc> <needle> <haystack>
assert_contains() {
  TOTAL=$((TOTAL+1))
  local id="$1" desc="$2" needle="$3" hay="$4" ok="FAIL"
  case "$hay" in *"$needle"*) ok="PASS"; PASS=$((PASS+1));; esac
  printf "%s\t%s\t%s\tneedle=%s\n" "$id" "$ok" "$desc" "$needle" >> "$RESULTS"
  printf "[%s] %-6s %s  (needle=%s)\n" "$id" "$ok" "$desc" "$needle"
}
summary() { echo "======== $PASS / $TOTAL assertions passed ========"; }
