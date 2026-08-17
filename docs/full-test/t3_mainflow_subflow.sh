#!/usr/bin/env bash
# SUITE 3 —— 主流程 happy path + 排他网关双分支 + 子流程组织路由 + 父子唤醒 + 提交
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 3: 主流程 + 网关 + 子流程路由 ════════"

# 完成实例首个开放任务（complete 无需办理人）
adv() { local i="$1" t; t=$(inst "$i" | jq -r '.data.openTasks[0].id'); complete "$t" "$i" "advance" >/dev/null; }
node_open() { inst "$1" | jq -r --arg n "$2" '[.data.openTasks[]?|select(.nodeBpmnId==$n)]|length>0'; }
active() { inst "$1" | jq -c '[.data.tokens[]?|select(.state!="ENDED")|.nodeBpmnId]'; }

# ── 3A 总部·大额：mgr→网关(大额)→director→子流程(hq三级)→cashier→办结 ──
echo "--- 3A 总部·大额 30000 ---"
r=$(start travel_expense zongbu '{"amount":30000,"applicant":"boss","initiator":"boss"}' "S3A-HQ-BIG")
A=$(iid "$r"); echo "parent=$A"; echo "$A" > data/s3a_iid.txt
assert "S3A-start-mgr" "起后停在经理审批" "mgr" "$(inst $A|jq -r '.data.openTasks[0].nodeBpmnId')"
adv "$A"   # 经理审批
assert "S3A-gw-big" "大额>20000→走总监分支" "true" "$(node_open $A director)"
adv "$A"   # 总监审批 → 进入 callActivity
# 子流程已建
C=$(children "$A" | jq -r '.data.children[0].id')
CK=$(children "$A" | jq -r '.data.children[0].definitionKey')
assert "S3A-subflow-hq" "总部路由→子流程fin_review_hq" "fin_review_hq" "$CK"
assert "S3A-parent-wait" "父在callActivity等待" "fin_review" "$(active $A|jq -r '.[0]')"
# 驱动子流程三级
adv "$C"; assert "S3A-sub-l2" "子流程推进到fin2" "true" "$(node_open $C fin2)"
adv "$C"; adv "$C"   # fin2, fin3
assert "S3A-sub-done" "子流程办结COMPLETED" "COMPLETED" "$(inst $C|jq -r '.data.state')"
# 父被唤醒 → cashier
assert "S3A-parent-wake" "子完成唤醒父→出纳打款" "cashier" "$(inst $A|jq -r '.data.openTasks[0].nodeBpmnId')"
adv "$A"
assert "S3A-done" "主流程办结COMPLETED" "COMPLETED" "$(inst $A|jq -r '.data.state')"

# ── 3B 上海·小额：mgr→网关(小额跳过总监)→子流程(branch单签)→cashier→办结 ──
echo "--- 3B 上海·小额 5000 ---"
r=$(start travel_expense fin_sh '{"amount":5000,"applicant":"xiaoli","initiator":"xiaoli"}' "S3B-SH-SMALL")
B=$(iid "$r"); echo "parent=$B"; echo "$B" > data/s3b_iid.txt
adv "$B"   # 经理审批 → 小额默认分支直接进 callActivity（跳过 director）
assert "S3B-skip-director" "小额→跳过总监(director不开放)" "false" "$(node_open $B director)"
CK=$(children "$B" | jq -r '.data.children[0].definitionKey')
assert "S3B-subflow-branch" "上海路由→子流程fin_review_branch" "fin_review_branch" "$CK"
C=$(children "$B" | jq -r '.data.children[0].id')
# 驱动 branch 子流程到完
while [ "$(inst $C|jq -r '.data.state')" = "ACTIVE" ]; do adv "$C"; done
assert "S3B-sub-done" "分公司子流程办结" "COMPLETED" "$(inst $C|jq -r '.data.state')"
adv "$B"   # cashier
assert "S3B-done" "上海主流程办结" "COMPLETED" "$(inst $B|jq -r '.data.state')"

# ── 3C 北京(fin_bj)·沿org树path继承 → 无精确绑定→继承zongbu→hq ──
echo "--- 3C 北京·path继承 ---"
r=$(start travel_expense fin_bj '{"amount":8000,"applicant":"bjuser","initiator":"bjuser"}' "S3C-BJ-INHERIT")
D=$(iid "$r"); echo "parent=$D"; echo "$D" > data/s3c_iid.txt
adv "$D"   # 经理审批 → 小额进 callActivity
CK=$(children "$D" | jq -r '.data.children[0].definitionKey')
assert "S3C-inherit" "北京无绑定→沿path继承总部→hq" "fin_review_hq" "$CK"
summary
echo "PASS=$PASS TOTAL=$TOTAL"
