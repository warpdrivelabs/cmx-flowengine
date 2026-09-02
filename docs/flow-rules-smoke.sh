#!/usr/bin/env bash
# flow × rules 决策集成 真机 E2E（方案 A · P1）。
#
# 前置（两服务均本机、auth off）：
#   rules :8094  → cd cmx-rulesengine && CONFIG_FILE=rules-server-e2e.toml ./rules.sh
#   flow  :8091  → cd cmx-flowengine  && CONFIG_FILE=flow-server-e2e.toml \
#                    FLOW_RULES_MODE=http FLOW_RULES_SERVICE=rules \
#                    FLOW_RULES_DECISIONS=creditScoring FLOW_RULES_TRACE_PERSIST=logid \
#                    FLOW_IDENTITY_MODE=pg FLOW_SUBFLOW_MODE=pg FLOW_DELEGATE_MODE=pg ./flow.sh
#
# 幂等：决策 upsert（save_draft+publish 覆盖）；流程 key 稳定（e2e_rules / e2e_rules_bad，重发布覆盖）。
# 用 python 驱动（避免 bash 对 ${..} BPMN 条件表达式的引用陷阱）。
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
python3 "$DIR/flow-rules-smoke.py"
