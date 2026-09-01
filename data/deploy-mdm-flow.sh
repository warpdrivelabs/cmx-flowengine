#!/usr/bin/env bash
#
# MDM 主数据审批流程部署脚本（M7）——幂等，可重复执行。
#
# 步骤：
#   0) 自检：表单绑定已注册则提示（--force 重跑）
#   1) 自检：mdm_approver 角色成员 > 0（候选在 start 时物化，无成员 = 任务对所有人不可见）
#   2) POST /definitions/draft（key 由 BPMN <process id> 派生，body 无 key 字段）
#   3) POST /definitions/validate：断言编译 key 与两个防退化属性
#   4) POST /definitions/{key}/publish（须合法 JSON body，空 body 会 400）
#   5) POST /api/flow/forms：注册表单绑定（nativeView 留空 → 待办中心回退 content 视图）
#
# 用法：
#   ./deploy-mdm-flow.sh            # 经门户 :8080（反代/内嵌双模式透明）
#   ./deploy-mdm-flow.sh --force    # 忽略已部署提示强制重跑
#
# 前置：门户已启动（默认 http://127.0.0.1:8080）；流程引擎可用（内嵌或反代均可）。
set -euo pipefail
cd "$(dirname "$0")"

PORTAL="${PORTAL_BASE:-http://127.0.0.1:8080}"
API_KEY="${X_API_KEY:-cmx_sk_dev_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6}"
BPMN_FILE="definitions/mdm_cr_approval.bpmn"
DEF_KEY="mdm_cr_approval"
FORM_KEY="mdm.cr.review"

AUTH=(-H "X-API-Key: ${API_KEY}" -H "Content-Type: application/json")

echo "== MDM 审批流程部署（${PORTAL}）=="

# 0) 已部署自检
EXISTING=$(curl -s "${AUTH[@]}" "${PORTAL}/api/flow/forms/${FORM_KEY}")
if echo "${EXISTING}" | grep -q '"code":0' && ! echo "${EXISTING}" | grep -q '"data":null'; then
  if [[ "${1:-}" != "--force" ]]; then
    echo "表单绑定 ${FORM_KEY} 已注册（${EXISTING}）。如需重跑：$0 --force"
    exit 0
  fi
  echo "--force：继续重新部署…"
fi

# 1) 角色成员自检（候选 start 时物化——无成员则任务无人可见，必须先补角色分配）
echo "== 步骤1：检查 mdm_approver 角色成员（经 startable 校验流程可用性）=="
if ! curl -s "${AUTH[@]}" "${PORTAL}/api/flow/startable" | grep -q "${DEF_KEY}"; then
  echo "提示：${DEF_KEY} 尚未发布（首次部署正常）；若为重跑失败，请先给 mdm_approver 角色分配成员"
fi
echo "（角色成员请经 IAM 管理界面核对：cmx_role.code='mdm_approver' 的用户数须 > 0）"

# 2) 草稿（key 由 BPMN process id 派生）
echo "== 步骤2：保存流程定义草稿 =="
BPMN_JSON=$(python3 -c "
import json,sys
xml = open('${BPMN_FILE}', encoding='utf-8').read()
print(json.dumps({ 'name': '主数据变更审批', 'module': 'mdm', 'bpmnXml': xml }, ensure_ascii=False))
")
curl -s "${AUTH[@]}" -X POST "${PORTAL}/api/flow/definitions/draft" -d "${BPMN_JSON}" | head -c 400; echo

# 3) 校验（编译 key + 防退化属性断言）
echo "== 步骤3：校验 BPMN =="
VALIDATE=$(curl -s "${AUTH[@]}" -X POST "${PORTAL}/api/flow/definitions/validate" \
  -d "$(python3 -c "
import json
xml = open('${BPMN_FILE}', encoding='utf-8').read()
print(json.dumps({ 'bpmnXml': xml }, ensure_ascii=False))
")")
echo "${VALIDATE}" | head -c 400; echo
echo "${VALIDATE}" | grep -q "\"code\":0" || { echo "校验失败，中止"; exit 1; }
# 防退化断言：lenient 取回策略 + initiator 关系直派（缺失则四眼防线退化，见 BPMN 头注释）
grep -q 'cmx:withdrawPolicy="lenient"' "${BPMN_FILE}" || { echo "BPMN 缺少 cmx:withdrawPolicy=\"lenient\"（防退化断言失败）"; exit 1; }
grep -q 'cmx:candidates="initiator"' "${BPMN_FILE}" || { echo "BPMN 的 apply 节点缺少 cmx:candidates=\"initiator\"（防退化断言失败）"; exit 1; }

# 4) 发布（既有接口含 Path Variable，属存量约定不受新接口规范约束）
echo "== 步骤4：发布 ${DEF_KEY} =="
curl -s "${AUTH[@]}" -X POST "${PORTAL}/api/flow/definitions/${DEF_KEY}/publish" \
  -d '{ "note": "MDM M7 首版部署" }' | head -c 400; echo

# 5) 表单绑定（nativeView 留空 → content 视图；console='none' → 待办中心不挂平台审批
#    控制台、全屏打开 cr-form——M7.1 决议：审批动作全收口 MDM 业务端点）。
#    apply/review 两节点共用。
echo "== 步骤5：注册表单绑定 ${FORM_KEY} =="
curl -s "${AUTH[@]}" -X POST "${PORTAL}/api/flow/forms" -d "$(cat <<'EOF'
{
  "formKey": "mdm.cr.review",
  "kind": "native",
  "nativePage": "portal.mdm.cr-form",
  "bizTable": "cv_mdm_apply",
  "domain": "basic",
  "application": "dataplatform",
  "module": "mdm",
  "pkField": "id",
  "title": "主数据变更审批",
  "console": "none"
}
EOF
)" | head -c 400; echo

echo "== 完成 =="
echo "验收：${PORTAL}/api/flow/startable 应含 ${DEF_KEY}；新建 CR 提交后待办中心应出现 review 任务。"
