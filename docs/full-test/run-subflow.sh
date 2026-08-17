#!/usr/bin/env bash
# 子流程全量测试统一入口：清账本 → 顺序跑 SUB1-4 → 汇总
cd "$(dirname "$0")"
source lib.sh
export RESULTS="$PWD/logs/sub-results.tsv"
: > "$RESULTS"
: > logs/sub-transcript.log; export LOG="$PWD/logs/sub-transcript.log"
echo "########## cmx-flowengine 子流程全量测试 $(date +%F' '%T) ##########"
echo "前置：确保 seed-iam.sql + seed-org-deep.sql 已灌、子流程定义已部署、dept_review 绑定已建"
for s in sub1_multi_mount sub2_org_routing sub3_varmap_nest_reject sub4_edge_cases; do
  [ -f "$s.sh" ] && bash "$s.sh" 2>&1
  echo ""
done
echo "########## 子流程测试权威汇总 ##########"
awk -F'\t' '{tot++; if($2=="PASS")p++} END{printf "子流程总断言: %d/%d 通过 (%.1f%%)\n", p, tot, p*100/tot}' "$RESULTS"
LC_ALL=C grep 'FAIL' "$RESULTS" | LC_ALL=C cut -f1,3 || echo "（全部通过）"
