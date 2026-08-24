#!/usr/bin/env bash
# 新功能真机 E2E（对 PG 后端 :8091），保留测试数据供运维台查看。
# 覆盖 A7 外部 worker、A9 迁移、P1 异步、A8 错误边界、A3 事件子流程。
#
# 前置：起服务须带 FLOW_ENABLE_E2E_DELEGATES=1 才会注册 e2eOkDelegate/e2eBpmnErr/e2eAlwaysFail
# （P1/A8/A3 用），生产默认不注册。配置统一走 flow-server.toml（[server]/[[databases]]/[auth]），
# env 只做覆盖（框架键 SERVER__* / 业务键 AUTH__*）。示例：
#   FLOW_ENABLE_E2E_DELEGATES=1 AUTH__MODE=off ./target/debug/cmx-flow-server &
set -u
BASE="http://localhost:8091/api/flow"
PASS=0; FAIL=0
say(){ printf '\n=== %s ===\n' "$1"; }
ok(){ printf '  \xE2\x9C\x93 %s\n' "$1"; PASS=$((PASS+1)); }
no(){ printf '  \xE2\x9C\x97 %s\n' "$1"; FAIL=$((FAIL+1)); }
# 把 name + bpmnXml(来自环境变量 XML) 组装成 draft JSON body
mkbody(){ NAME="$1" python3 -c 'import json,os;print(json.dumps({"name":os.environ["NAME"],"bpmnXml":os.environ["XML"]}))'; }
get(){ python3 -c "import sys,json;d=json.load(sys.stdin);print(eval(sys.argv[1]))" "$1" 2>/dev/null; }

# ---------- A7 外部 Worker Task ----------
say "A7 外部 Worker Task（部署→发起→按 topic 拉取→完成推进）"
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_a7" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="pay"/>
    <serviceTask id="pay" name="外部支付" flowable:type="external-worker" flowable:topic="e2e-pay"/>
    <sequenceFlow id="s1" sourceRef="pay" targetRef="ok"/>
    <userTask id="ok" name="确认" flowable:assignee="u1"/>
    <sequenceFlow id="s2" sourceRef="ok" targetRef="end"/><endEvent id="end"/>
  </process></definitions>'
K=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-A7-外部Worker')" | get "d['data']['key']")
[ -n "$K" ] && ok "部署草稿 key=$K" || no "部署失败"
curl -sS "$BASE/definitions/$K/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null && ok "发布" || no "发布失败"
IID=$(curl -sS "$BASE/instances" -H 'Content-Type: application/json' -d '{"definitionKey":"'$K'","businessKey":"E2E-A7-001","variables":{"initiator":"e2e"}}' | get "d['data']['id']")
[ -n "$IID" ] && ok "发起实例 $IID" || no "发起失败"
ST=$(curl -sS "$BASE/instances/$IID" | get "[t['state'] for t in d['data']['tokens']]")
echo "    令牌状态: $ST"
echo "$ST" | grep -q "WAITING_ASYNC" && ok "令牌停 WaitingAsync（外部作业已建）" || no "令牌未停 WaitingAsync"
ACQ=$(curl -sS "$BASE/external-worker/jobs/acquire" -H 'Content-Type: application/json' -d '{"worker_id":"e2e-worker","topic":"e2e-pay","lock_secs":60,"limit":5}')
JOB=$(echo "$ACQ" | get "d['data']['jobs'][0]['id']")
[ -n "$JOB" ] && ok "外部 worker 按 topic 拉到作业 $JOB" || no "拉取失败: $ACQ"
EMPTY=$(curl -sS "$BASE/external-worker/jobs/acquire" -H 'Content-Type: application/json' -d '{"worker_id":"e2e-worker","topic":"wrong-topic"}' | get "d['data']['acquiredCount']")
[ "$EMPTY" = "0" ] && ok "错误 topic 拉取为空（隔离生效）" || no "topic 隔离失效: $EMPTY"
curl -sS "$BASE/async-jobs/$JOB/complete" -H 'Content-Type: application/json' -d '{"variables":{"payResult":"ok"}}' >/dev/null
NODE=$(curl -sS "$BASE/instances/$IID" | get "[t['nodeBpmnId'] for t in d['data']['openTasks']]")
echo "    开放任务节点: $NODE"
echo "$NODE" | grep -q "ok" && ok "完成后推进到确认任务" || no "未推进: $NODE"

# ---------- A9 实例迁移 ----------
say "A9 实例迁移（部署 v1/v2→发起 v1→迁移到 v2→验证）"
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_mig_v1" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="review"/>
    <userTask id="review" name="审核v1" flowable:assignee="u1"/>
    <sequenceFlow id="s1" sourceRef="review" targetRef="end"/><endEvent id="end"/>
  </process></definitions>'
K1=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-A9-迁移v1')" | get "d['data']['key']")
curl -sS "$BASE/definitions/$K1/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_mig_v2" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="审批v2" flowable:assignee="u1"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="extra"/>
    <userTask id="extra" name="附加环节" flowable:assignee="u2"/>
    <sequenceFlow id="s2" sourceRef="extra" targetRef="end"/><endEvent id="end"/>
  </process></definitions>'
K2=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-A9-迁移v2')" | get "d['data']['key']")
curl -sS "$BASE/definitions/$K2/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null
[ -n "$K1" ] && [ -n "$K2" ] && ok "部署 v1=$K1 v2=$K2" || no "部署失败"
MIID=$(curl -sS "$BASE/instances" -H 'Content-Type: application/json' -d '{"definitionKey":"'$K1'","businessKey":"E2E-A9-001","variables":{"initiator":"e2e"}}' | get "d['data']['id']")
[ -n "$MIID" ] && ok "发起 v1 实例 ${MIID} （停在 review）" || no "发起失败"
DOK=$(curl -sS "$BASE/instances/$MIID/migrate/validate" -H 'Content-Type: application/json' -d "{\"target_definition_key\":\"$K2\",\"activity_mappings\":{}}" | get "d['data']['ok']")
[ "$DOK" = "False" ] && ok "干运行校验挡住未映射节点" || no "校验未挡: $DOK"
curl -sS "$BASE/instances/$MIID/migrate" -H 'Content-Type: application/json' -d "{\"target_definition_key\":\"$K2\",\"activity_mappings\":{\"review\":\"approve\"}}" >/dev/null
MDEF=$(curl -sS "$BASE/instances/$MIID" | get "d['data']['definitionKey']")
MTOK=$(curl -sS "$BASE/instances/$MIID" | get "[t['nodeBpmnId'] for t in d['data']['openTasks']]")
echo "    迁移后 definitionKey=$MDEF, 任务节点=$MTOK"
{ [ "$MDEF" = "$K2" ] && echo "$MTOK" | grep -q "approve"; } && ok "迁移：令牌重定位 approve + 定义指向 v2" || no "迁移结果异常"

printf '\n========== E2E 汇总: PASS=%d FAIL=%d ==========\n' "$PASS" "$FAIL"
printf '保留数据: A7实例=%s(def %s), A9实例=%s(def %s→%s)\n' "$IID" "$K" "$MIID" "$K1" "$K2"

# ---------- P1 异步 Job 执行器（async serviceTask + in-process poller）----------
say "P1 异步 Job（async serviceTask → WaitingAsync → poller 执行 → 推进）"
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_p1" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="异步调用" flowable:class="e2eOkDelegate" flowable:async="true"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="ok"/>
    <userTask id="ok" name="确认" flowable:assignee="u1"/>
    <sequenceFlow id="s2" sourceRef="ok" targetRef="end"/><endEvent id="end"/>
  </process></definitions>'
KP=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-P1-异步Job')" | get "d['data']['key']")
curl -sS "$BASE/definitions/$KP/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null
PIID=$(curl -sS "$BASE/instances" -H 'Content-Type: application/json' -d "{\"definitionKey\":\"$KP\",\"businessKey\":\"E2E-P1-001\",\"variables\":{\"initiator\":\"e2e\"}}" | get "d['data']['id']")
[ -n "$PIID" ] && ok "发起 $PIID" || no "发起失败"
PST=$(curl -sS "$BASE/instances/$PIID" | get "[t['state'] for t in d['data']['tokens']]")
echo "$PST" | grep -q "WAITING_ASYNC" && ok "令牌停 WaitingAsync（进程内异步作业）" || no "未停 WaitingAsync: $PST"
echo "    等 in-process poller（3s tick）执行..."; sleep 5
PNODE=$(curl -sS "$BASE/instances/$PIID" | get "[t['nodeBpmnId'] for t in d['data']['openTasks']]")
echo "    poller 后开放任务: $PNODE"
echo "$PNODE" | grep -q "ok" && ok "poller 执行 delegate 后推进到确认" || no "poller 未推进: $PNODE"

# ---------- A8 错误边界事件（serviceTask 抛 BPMN 错误 → 边界分支）----------
say "A8 错误边界（serviceTask 抛 E_RISK → 边界处理分支）"
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_a8" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="风控" flowable:class="e2eBpmnErr"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="ok"/>
    <userTask id="ok" name="正常审批" flowable:assignee="u1"/>
    <sequenceFlow id="s2" sourceRef="ok" targetRef="end"/><endEvent id="end"/>
    <boundaryEvent id="onErr" attachedToRef="svc"><errorEventDefinition errorRef="E_RISK"/></boundaryEvent>
    <sequenceFlow id="s3" sourceRef="onErr" targetRef="handle"/>
    <userTask id="handle" name="异常处理" flowable:assignee="ops"/>
    <sequenceFlow id="s4" sourceRef="handle" targetRef="endE"/><endEvent id="endE"/>
  </process></definitions>'
KE=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-A8-错误边界')" | get "d['data']['key']")
curl -sS "$BASE/definitions/$KE/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null
EIID=$(curl -sS "$BASE/instances" -H 'Content-Type: application/json' -d "{\"definitionKey\":\"$KE\",\"businessKey\":\"E2E-A8-001\",\"variables\":{\"initiator\":\"e2e\"}}" | get "d['data']['id']")
[ -n "$EIID" ] && ok "发起 $EIID" || no "发起失败"
ENODE=$(curl -sS "$BASE/instances/$EIID" | get "[t['nodeBpmnId'] for t in d['data']['openTasks']]")
echo "    开放任务: $ENODE"
echo "$ENODE" | grep -q "handle" && ok "serviceTask 抛错 → 走错误边界处理分支" || no "未走边界: $ENODE"

# ---------- A3 事件子流程（错误触发中断型）----------
say "A3 事件子流程（serviceTask 抛错、无边界 → 事件子流程中断处理）"
export XML='<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_a3" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="svc"/>
    <serviceTask id="svc" name="风控" flowable:class="e2eBpmnErr"/>
    <sequenceFlow id="s1" sourceRef="svc" targetRef="ok"/>
    <userTask id="ok" name="正常审批" flowable:assignee="u1"/>
    <sequenceFlow id="s2" sourceRef="ok" targetRef="end"/><endEvent id="end"/>
    <subProcess id="evsub" triggeredByEvent="true">
      <startEvent id="estart"><errorEventDefinition errorRef="E_RISK"/></startEvent>
      <sequenceFlow id="es0" sourceRef="estart" targetRef="ehandle"/>
      <userTask id="ehandle" name="事件子流程处理" flowable:assignee="ops"/>
      <sequenceFlow id="es1" sourceRef="ehandle" targetRef="endH"/><endEvent id="endH"/>
    </subProcess>
  </process></definitions>'
K3=$(curl -sS "$BASE/definitions/draft" -H 'Content-Type: application/json' -d "$(mkbody 'E2E-A3-事件子流程')" | get "d['data']['key']")
curl -sS "$BASE/definitions/$K3/publish" -H 'Content-Type: application/json' -d '{}' >/dev/null
E3=$(curl -sS "$BASE/instances" -H 'Content-Type: application/json' -d "{\"definitionKey\":\"$K3\",\"businessKey\":\"E2E-A3-001\",\"variables\":{\"initiator\":\"e2e\"}}" | get "d['data']['id']")
[ -n "$E3" ] && ok "发起 $E3" || no "发起失败"
N3=$(curl -sS "$BASE/instances/$E3" | get "[t['nodeBpmnId'] for t in d['data']['openTasks']]")
echo "    开放任务: $N3"
echo "$N3" | grep -q "ehandle" && ok "无边界 → 中断主流程走事件子流程处理" || no "未走事件子流程: $N3"

printf '\n===== 追加功能 E2E: A7/A9/P1/A8/A3 =====\n'
printf '保留数据: P1=%s(def %s) A8=%s(def %s) A3=%s(def %s)\n' "$PIID" "$KP" "$EIID" "$KE" "$E3" "$K3"
