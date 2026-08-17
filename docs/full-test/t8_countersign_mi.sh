#!/usr/bin/env bash
# SUITE 8 —— 会签/或签(多实例) + 动态逐元素派人
source "$(dirname "$0")/lib.sh"
echo "════════ SUITE 8: 会签(并行/过半/顺序) + 动态逐元素派人 ════════"
opencount() { inst "$1" | jq -r '[.data.openTasks[]?]|length'; }
openwho()  { inst "$1" | jq -c '[.data.openTasks[]?|.assignee]|sort'; }
st() { inst "$1" | jq -r '.data.state'; }

# ── 8A 并行会签·全票（3人全签才过）──
echo "--- 8A 并行会签(全票) ---"
r=$(start cs_all "" '{"approvers":["u_fin1","u_fin2","u_fin3"]}' "S8A-CS-ALL")
P=$(iid "$r"); echo "$P">data/s8a_iid.txt
assert "S8A-expand" "并行展开3个子任务" "3" "$(opencount $P)"
assert "S8A-assignees" "各子任务派给各approver" '["u_fin1","u_fin2","u_fin3"]' "$(openwho $P)"
t=$(taskof u_fin1 "$P"); complete "$t" "$P" "签1" >/dev/null
assert "S8A-partial" "签1人后仍ACTIVE(需全票)" "ACTIVE" "$(st $P)"
t=$(taskof u_fin2 "$P"); complete "$t" "$P" "签2" >/dev/null
t=$(taskof u_fin3 "$P"); complete "$t" "$P" "签3" >/dev/null
assert "S8A-all-done" "全票签完→COMPLETED" "COMPLETED" "$(st $P)"

# ── 8B 并行会签·过半（3人签2人即过，剩余作废）──
echo "--- 8B 并行会签(过半) ---"
r=$(start cs_majority "" '{"approvers":["u_fin1","u_fin2","u_fin3"]}' "S8B-CS-MAJ")
Q=$(iid "$r"); echo "$Q">data/s8b_iid.txt
assert "S8B-expand" "并行展开3子任务" "3" "$(opencount $Q)"
t=$(taskof u_fin1 "$Q"); complete "$t" "$Q" "签1" >/dev/null
assert "S8B-1of3" "1/3未过半仍ACTIVE" "ACTIVE" "$(st $Q)"
t=$(taskof u_fin2 "$Q"); complete "$t" "$Q" "签2" >/dev/null
assert "S8B-majority" "2/3过半→COMPLETED" "COMPLETED" "$(st $Q)"
assert "S8B-remainder-cancelled" "剩余子任务作废(u_fin3无待办)" "" "$(taskof u_fin3 "$Q")"

# ── 8C 顺序会签（或签逐个，全签）──
echo "--- 8C 顺序会签(逐个) ---"
r=$(start cs_seq "" '{"approvers":["u_fin1","u_fin2","u_fin3"]}' "S8C-CS-SEQ")
Z=$(iid "$r"); echo "$Z">data/s8c_iid.txt
assert "S8C-one-first" "顺序:初始仅1个子任务" "1" "$(opencount $Z)"
assert "S8C-first-who" "首个是u_fin1" '["u_fin1"]' "$(openwho $Z)"
t=$(taskof u_fin1 "$Z"); complete "$t" "$Z" "顺序1" >/dev/null
assert "S8C-second-who" "办结后下一个u_fin2出现" '["u_fin2"]' "$(openwho $Z)"
t=$(taskof u_fin2 "$Z"); complete "$t" "$Z" "顺序2" >/dev/null
assert "S8C-third-who" "再下一个u_fin3" '["u_fin3"]' "$(openwho $Z)"
t=$(taskof u_fin3 "$Z"); complete "$t" "$Z" "顺序3" >/dev/null
assert "S8C-seq-done" "顺序全签→COMPLETED" "COMPLETED" "$(st $Z)"

# ── 8D 动态逐元素派人（产品负责人）──
echo "--- 8D 动态逐元素派人 ---"
r=$(start mi_dyn "" '{"products":[{"sku":"A","owner":"u_fin1"},{"sku":"B","owner":"u_fin2"},{"sku":"C","owner":"u_cfo"}]}' "S8D-MI-DYN")
Y=$(iid "$r"); echo "$Y">data/s8d_iid.txt
assert "S8D-expand" "按产品数展开3子任务" "3" "$(opencount $Y)"
assert "S8D-assignees" "各子任务派给产品owner" '["u_cfo","u_fin1","u_fin2"]' "$(openwho $Y)"
# elementValue 携带对应产品
ev=$(inst $Y | jq -c '[.data.openTasks[]?|{who:.assignee,sku:.elementValue.sku}]|sort_by(.who)')
echo "element values: $ev"
assert "S8D-elemvalue" "u_fin1子任务elementValue.sku=A" "A" "$(inst $Y|jq -r '[.data.openTasks[]?|select(.assignee=="u_fin1")][0].elementValue.sku')"
assert "S8D-elemvalue2" "u_cfo子任务elementValue.sku=C" "C" "$(inst $Y|jq -r '[.data.openTasks[]?|select(.assignee=="u_cfo")][0].elementValue.sku')"
for a in u_fin1 u_fin2 u_cfo; do t=$(taskof "$a" "$Y"); complete "$t" "$Y" "产品审$a" >/dev/null; done
assert "S8D-done" "全产品审完→COMPLETED" "COMPLETED" "$(st $Y)"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
