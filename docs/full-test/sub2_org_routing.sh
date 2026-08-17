#!/usr/bin/env bash
# SUB-SUITE 2 —— 按组织选择子流程：精确/沿path继承/深层继承/默认兜底/禁用跳过/多key独立
source "$(dirname "$0")/lib.sh"
echo "════════ SUB2: 按组织选择子流程（路由矩阵）════════"
# main_org_routed: 单 callActivity cmx:calledKey="dept_review"
# 绑定矩阵: zongbu→sub_review, fin_sh→sub_risk, fin_bj→fin_review_hq,
#           branch_gz→fin_review_branch, 默认→sub_review, fin_bj_g1(禁用)→sub_risk
routed_child() { children "$1" | jq -r '.data.children[0].definitionKey // "NONE"'; }

# 起 main_org_routed 于指定 org, 返回路由到的子流程 def
route_for() {  # route_for <org> <bizkey>
  local org="$1" bk="$2" r P
  r=$(start main_org_routed "$org" "{\"initiator\":\"u_route\"}" "$bk"); P=$(iid "$r")
  echo "$P"
}

echo "--- SUB2-A 精确匹配 ---"
P=$(route_for zongbu "SUB2-EXACT-ZB"); echo "$P">data/sub2_zongbu.txt
assert "SUB2-exact-zongbu" "zongbu精确→sub_review" "sub_review" "$(routed_child $P)"
P=$(route_for fin_sh "SUB2-EXACT-SH"); echo "$P">data/sub2_finsh.txt
assert "SUB2-exact-finsh" "fin_sh精确→sub_risk" "sub_risk" "$(routed_child $P)"
P=$(route_for fin_bj "SUB2-EXACT-BJ"); echo "$P">data/sub2_finbj.txt
assert "SUB2-exact-finbj" "fin_bj精确→fin_review_hq" "fin_review_hq" "$(routed_child $P)"

echo "--- SUB2-B 沿组织树 path 深层继承 ---"
# fin_bj_g1(孙, 自身绑定禁用) → 应继承父 fin_bj → fin_review_hq
P=$(route_for fin_bj_g1 "SUB2-INHERIT-G1"); echo "$P">data/sub2_g1.txt
echo "  fin_bj_g1路由到: $(routed_child $P)"
assert "SUB2-inherit-deep" "孙组织(自身禁用)→继承fin_bj→fin_review_hq" "fin_review_hq" "$(routed_child $P)"

echo "--- SUB2-C 独立根不误继承总部 ---"
# branch_gz 自身有绑定 → fin_review_branch(不受 zongbu 影响)
P=$(route_for branch_gz "SUB2-GZ"); echo "$P">data/sub2_gz.txt
assert "SUB2-own-branch" "广州(独立根)→自身fin_review_branch" "fin_review_branch" "$(routed_child $P)"

echo "--- SUB2-D 默认兜底（无匹配组织）---"
# 用一个没有精确/继承绑定的 org。zongbu 之外的独立 org? 用不存在于绑定的 org id
# 造一个临时 org 无绑定但存在? 直接用 org 不在树里 → 无 path 继承 → 默认兜底
P=$(route_for "org_unbound_xyz" "SUB2-DEFAULT"); echo "$P">data/sub2_default.txt
echo "  未绑定org路由到: $(routed_child $P)"
assert "SUB2-default" "无匹配组织→默认兜底sub_review" "sub_review" "$(routed_child $P)"

echo "--- SUB2-E 禁用绑定被跳过 ---"
# fin_bj_g1 的精确绑定 enabled=false → 不应命中 sub_risk, 而是继承
assert "SUB2-disabled-skip" "禁用的精确绑定不命中(→非sub_risk)" "true" "$([ "$(routed_child $(cat data/sub2_g1.txt))" != "sub_risk" ] && echo true || echo false)"

echo "--- SUB2-F 同 main 不同 org 走不同子流程（对照）---"
zb=$(routed_child $(cat data/sub2_zongbu.txt))
sh=$(routed_child $(cat data/sub2_finsh.txt))
bj=$(routed_child $(cat data/sub2_finbj.txt))
gz=$(routed_child $(cat data/sub2_gz.txt))
echo "  路由对照: zongbu=$zb fin_sh=$sh fin_bj=$bj branch_gz=$gz"
assert "SUB2-diverge" "同一主流程按org路由到4种不同子流程" "true" "$([ "$zb" != "$sh" ] && [ "$sh" != "$bj" ] && [ "$bj" != "$gz" ] && echo true || echo false)"

echo "--- SUB2-G 组织路由子流程可正常办结（端到端）---"
P=$(cat data/sub2_finsh.txt)
c=$(children $P | jq -r '.data.children[0].id')
while [ "$(inst $c|jq -r '.data.state')" = "ACTIVE" ]; do t=$(inst $c|jq -r '.data.openTasks[0].id'); complete "$t" "$c" 办 >/dev/null; done
assert "SUB2-e2e-child-done" "路由子流程办结" "COMPLETED" "$(inst $c|jq -r '.data.state')"
assert "SUB2-e2e-parent-done" "路由后父流程办结" "COMPLETED" "$(inst $P|jq -r '.data.state')"

echo "--- SUB2-H 多个逻辑key在同一主流程各自独立路由 ---"
# main_serial_multi 的 call3 用 fin_review, main_org_routed 用 dept_review —— 二者独立
# 验证 fin_review 与 dept_review 对同一 org(fin_sh) 路由到不同目标
r=$(start main_serial_multi fin_sh '{"initiator":"u_mk","amount":100}' "SUB2-MULTIKEY"); MK=$(iid "$r")
t=$(taskof u_fin1 "$MK"); complete "$t" "$MK" 申请 >/dev/null   # →call1 sub_review
advc() { local c="$1" t; t=$(inst "$c"|jq -r '.data.openTasks[0].id'); complete "$t" "$c" x >/dev/null; }
c1=$(children $MK|jq -r '[.data.children[]?|select(.definitionKey=="sub_review")][0].id'); advc "$c1"  # →call2 sub_risk
c2=$(children $MK|jq -r '[.data.children[]?|select(.definitionKey=="sub_risk")][0].id'); advc "$c2"    # →call3 fin_review@fin_sh→branch
c3def=$(children $MK|jq -r '[.data.children[]?|select(.definitionKey|test("fin_review"))][0].definitionKey')
echo "  fin_review@fin_sh → $c3def (dept_review@fin_sh → sub_risk, 独立)"
assert "SUB2-multikey-indep" "fin_review与dept_review对同org路由独立(fin_review@fin_sh=branch)" "fin_review_branch" "$c3def"
summary; echo "PASS=$PASS TOTAL=$TOTAL"
