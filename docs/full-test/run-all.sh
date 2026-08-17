#!/usr/bin/env bash
# 全量后端测试统一入口：清账本 → 顺序跑 12 个套件 → 汇总
cd "$(dirname "$0")"
source lib.sh
: > logs/results.tsv          # 清空账本，得权威计数
: > logs/transcript.log       # 清空请求流水（本轮全留存）
echo "########## cmx-flowengine 后端全量回归 $(date +%F' '%T) ##########"
for s in t1_design t2_assignees t3_mainflow_subflow t4_reject t5_withdraw \
         t6_transfer_delegate_addsign t7_cc t8_countersign_mi \
         t9_gateway_lifecycle t10_conditions_decisions_negative \
         t11_anomaly_edge t13_mi_styleB_subreject; do
  [ -f "$s.sh" ] && bash "$s.sh" 2>&1
  echo ""
done
echo "########## 权威汇总 ##########"
awk -F'\t' '{tot++; if($2=="PASS")p++} END{printf "总断言: %d/%d 通过 (%.1f%%)\n", p, tot, p*100/tot}' logs/results.tsv
echo "—— 未通过项（区分真实缺陷 vs 脚手架）——"
grep -P '\tFAIL\t' logs/results.tsv | cut -f1,3 || echo "（全部通过）"
