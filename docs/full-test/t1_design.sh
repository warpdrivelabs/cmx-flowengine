#!/usr/bin/env bash
# SUITE 1 —— 流程设计生命周期：validate(正/负) + draft + publish(热装载) + 版本(bump/list/activate/delete)
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 1: 流程设计生命周期 ════════"
DEFS="probe_cand cs_all cs_majority cs_seq mi_dyn cc_flow par_gw approval_chain mi_dyn_role"
for key in $DEFS; do
  f="defs/$key.bpmn"; [ -f "$f" ] || continue
  v=$(validate_bpmn "$f" | jq -r '.data.valid')
  assert "DSN-$key-valid" "validate($key)通过编译+拓扑" "true" "$v"
  dk=$(deploy "$key-测试" "$f")
  assert "DSN-$key-key" "deploy key=$key" "$key" "$dk"
  hot=$(publish "$dk" | jq -r '.data.hotLoaded')
  assert "DSN-$key-hot" "发布热装载生效" "true" "$hot"
done
# 负向
cat > defs/bad_nostart.bpmn <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" id="d_bad"><bpmn:process id="bad_nostart" isExecutable="true"><bpmn:userTask id="only"/></bpmn:process></bpmn:definitions>
XML
assert "DSN-neg-nostart" "无start事件→valid=false" "false" "$(validate_bpmn defs/bad_nostart.bpmn|jq -r '.data.valid')"
assert "DSN-neg-xml" "非法XML→valid=false" "false" "$(j POST /definitions/validate '{"bpmnXml":"<oops"}'|jq -r '.data.valid')"
# 版本管理
v1=$(publish probe_cand|jq -r '.data.version'); v2=$(publish probe_cand|jq -r '.data.version')
assert "DSN-ver-bump" "重复发布版本递增($v1→$v2)" "true" "$([ "$v2" -gt "$v1" ]&&echo true||echo false)"
assert "DSN-ver-activate" "激活历史版本v1 code=0" "0" "$(j POST /definitions/probe_cand/versions/1/activate '{}'|jq -r '.code')"
j POST "/definitions/probe_cand/versions/$v2/activate" '{}' >/dev/null   # 复位到最新
summary; echo "PASS=$PASS TOTAL=$TOTAL"
