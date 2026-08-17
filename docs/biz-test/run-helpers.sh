#!/usr/bin/env bash
# 差旅报销业务流程 E2E 测试 helper（curl + jq）
B=http://127.0.0.1:8091/api/flow/v1
CT='Content-Type: application/json'

# 发起：start <defKey> <orgId> <amount> <initiator> <bizKey> [extraJson]
start() {
  jq -n --arg k "$1" --arg o "$2" --argjson a "$3" --arg init "$4" --arg bk "$5" \
    '{definitionKey:$k, orgId:$o, businessKey:$bk,
      variables:{amount:$a, initiator:$init, applicant:$init}}' \
    | curl -s -X POST "$B/instances" -H "$CT" -d @-
}
# 实例详情
inst() { curl -s "$B/instances/$1"; }
# 子实例
children() { curl -s "$B/instances/$1/children"; }
# 某人待办（可选按实例过滤）
mytasks() { curl -s "$B/tasks/my?assignee=$1"; }
# 取某实例在某人名下的首个未完成任务 id：taskof <assignee> <instanceId>
taskof() { curl -s "$B/tasks/my?assignee=$1" | jq -r --arg i "$2" '.data.tasks[]|select(.instanceId==$i)|.taskId' | head -1; }
# 办结：done <taskId> <instanceId> [comment]
done_() {
  jq -n --arg i "$2" --arg c "${3:-同意}" '{instanceId:$i, comment:$c, variables:{}}' \
    | curl -s -X POST "$B/tasks/$1/complete" -H "$CT" -d @-
}
# 退回目标：rtargets <taskId> <instanceId>
rtargets() { curl -s "$B/tasks/$1/reject-targets?instanceId=$2"; }
# 退回：reject <taskId> <instanceId> [targetBpmnId] [reason]
reject() {
  jq -n --arg i "$2" --arg t "$3" --arg r "${4:-退回修改}" \
    '{instanceId:$i, reason:$r} + (if $t=="" then {} else {targetBpmnId:$t} end)' \
    | curl -s -X POST "$B/tasks/$1/reject" -H "$CT" -d @-
}
# 可否取回：canwd <instanceId> <user>
canwd() { curl -s "$B/instances/$1/withdrawable?user=$2"; }
# 取回：withdraw <instanceId> <user> [reason]
withdraw() {
  jq -n --arg u "$2" --arg r "${3:-发起人取回修改}" '{user:$u, reason:$r}' \
    | curl -s -X POST "$B/instances/$1/withdraw" -H "$CT" -d @-
}
# 精简展示实例态：show <instanceId>
show() {
  inst "$1" | jq -c '{state:.data.state,
     active:[.data.tokens[]|select(.state!="ENDED")|.nodeBpmnId],
     openTasks:[.data.tasks[]|select(.completed==false)|{node:.nodeBpmnId,name:.name,who:.assignee}]}'
}
# 子实例精简：showchild <parentId>
showchild() {
  children "$1" | jq -c '.data.children[]|{id:.id, def:.definitionKey, state:.state,
     active:[.tokens[]|select(.state!="ENDED")|.nodeBpmnId],
     open:[.tasks[]|select(.completed==false)|{node:.nodeBpmnId,who:.assignee}]}'
}
