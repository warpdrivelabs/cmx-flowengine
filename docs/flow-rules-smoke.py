#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""flow × rules 决策集成 真机 E2E（方案 A · P1）。见同名 .sh 的前置说明。"""
import json, sys, urllib.request, urllib.error

R = "http://127.0.0.1:8094/api/rules/v1"
F = "http://127.0.0.1:8091/api/flow"
HDR = {"Content-Type": "application/json", "X-Tenant": "default"}
PASS = FAIL = 0


def _req(url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=HDR, method="POST" if body is not None else "GET")
    try:
        return json.load(urllib.request.urlopen(req)), None
    except urllib.error.HTTPError as e:
        try:
            return json.load(e), e.code
        except Exception:
            return {"code": -1, "msg": e.read().decode()[:200]}, e.code


def rules(path, body=None): return _req(R + path, body)
def flow(path, body=None): return _req(F + path, body)


def ok(m):
    global PASS; PASS += 1; print("  ✓", m)


def no(m):
    global FAIL; FAIL += 1; print("  ✗", m)


def say(m): print("\n===", m, "===")


DEC = {
    "key": "creditScoring", "name": "信用评分(E2E)", "version": 1, "kind": "decisionTable", "hitPolicy": "F",
    "inputs": [{"id": "amount", "label": "金额", "expression": "amount"},
               {"id": "level", "label": "等级", "expression": "level"}],
    "outputs": [{"id": "tier", "label": "级别", "name": "credit_tier"},
                {"id": "disc", "label": "折扣", "name": "discount"}],
    "rules": [
        {"id": "r1", "inputEntries": ["> 3000", '"gold"'], "outputEntries": ['"A"', "0.2"]},
        {"id": "r2", "inputEntries": ["> 3000", "-"], "outputEntries": ['"B"', "0.1"]},
        {"id": "r3", "inputEntries": ["-", "-"], "outputEntries": ['"C"', "0"]},
    ],
}

PROC = '''<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_rules" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="score"/>
    <serviceTask id="score" name="信用评分" flowable:delegateExpression="rules:creditScoring"/>
    <sequenceFlow id="s1" sourceRef="score" targetRef="gw"/>
    <exclusiveGateway id="gw"/>
    <sequenceFlow id="sA" sourceRef="gw" targetRef="vipTask"><conditionExpression>${credit_tier == "A"}</conditionExpression></sequenceFlow>
    <sequenceFlow id="sB" sourceRef="gw" targetRef="normalTask"/>
    <userTask id="vipTask" name="VIP审批" flowable:assignee="u1"/>
    <userTask id="normalTask" name="常规审批" flowable:assignee="u2"/>
    <sequenceFlow id="s2" sourceRef="vipTask" targetRef="end"/>
    <sequenceFlow id="s3" sourceRef="normalTask" targetRef="end"/>
    <endEvent id="end"/>
  </process></definitions>'''

BAD = '''<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn">
  <process id="e2e_rules_bad" isExecutable="true">
    <startEvent id="start"/><sequenceFlow id="s0" sourceRef="start" targetRef="bad"/>
    <serviceTask id="bad" name="未注册决策" flowable:delegateExpression="rules:notInAllowlist"/>
    <sequenceFlow id="s1" sourceRef="bad" targetRef="end"/><endEvent id="end"/>
  </process></definitions>'''


def deploy(xml, name):
    d, _ = flow("/definitions/draft", {"name": name, "bpmnXml": xml})
    k = d["data"]["key"]
    flow("/definitions/%s/publish" % k, {})
    return k


def inst_vars(k, vars):
    r, _ = flow("/instances", {"definitionKey": k, "variables": vars})
    if not r or "data" not in r:
        return None, r
    d, _ = flow("/instances/%s" % r["data"]["id"])
    return d["data"], r


# 0) rules 种决策
say("0 rules 侧种决策 creditScoring（FIRST）")
d, _ = rules("/definitions/draft", DEC)
(ok if d.get("code") == 0 else no)("决策草稿保存 code=%s" % d.get("code"))
d, _ = rules("/definitions/creditScoring/publish", {})
(ok if d.get("code") == 0 else no)("决策发布 code=%s（激活版就绪）" % d.get("code"))

# 1) rules 直连自测
say("1 rules /evaluate 直连自测")
d, _ = rules("/decisions/creditScoring/evaluate", {"input": {"amount": 5000, "level": "gold"}, "options": {"trace": True, "log": True}})
out = (d.get("data") or {}).get("output", {})
print("    output:", out)
(ok if d.get("code") == 0 else no)("直连 code=0")
(ok if out.get("credit_tier") == "A" else no)("直连 credit_tier=A（5000+gold 命中 r1）")
(ok if (d.get("data") or {}).get("logId") else no)("直连有 logId（可回写）")

# 2) flow serviceTask → 决策驱动网关
say("2 flow serviceTask delegate=rules:creditScoring → 网关按 credit_tier 分支")
k = deploy(PROC, "E2E-规则决策")
ok("部署+发布 key=%s" % k)
dA, _ = inst_vars(k, {"amount": 5000, "level": "gold"})
tasks = [t["nodeBpmnId"] for t in dA.get("openTasks", [])]
(ok if dA["variables"].get("credit_tier") == "A" else no)("output merge 回变量 credit_tier=A")
(ok if dA["variables"].get("discount") == 0.2 else no)("discount=0.2 一并回写")
(ok if "vipTask" in tasks else no)("网关按 credit_tier==A 走 VIP 分支（决策驱动流程）：%s" % tasks)
dec = dA["variables"].get("__decisions")
print("    __decisions:", json.dumps(dec, ensure_ascii=False))
(ok if dec and dec[0].get("logId") else no)("__decisions 留痕 logId（决策在流程内可审计）")

# 2b) 反向数据 → 兜底 C → 常规分支
say("2b 反向数据 amount=1000/silver → credit_tier=C → 常规分支")
dC, _ = inst_vars(k, {"amount": 1000, "level": "silver"})
tasksC = [t["nodeBpmnId"] for t in dC.get("openTasks", [])]
(ok if dC["variables"].get("credit_tier") == "C" else no)("credit_tier=C（命中兜底 r3）")
(ok if "normalTask" in tasksC else no)("走常规分支（不同数据→不同路径）：%s" % tasksC)

# 3) 负例：未注册 decisionKey
say("3 负例 delegate=rules:notInAllowlist（未在 allowlist）")
kb = deploy(BAD, "E2E-未注册决策")
r, _ = flow("/instances", {"definitionKey": kb, "variables": {}})
if r and r.get("code") == 0 and r.get("data"):
    d, _ = flow("/instances/%s" % r["data"]["id"])
    states = [t["state"] for t in d["data"]["tokens"]]
    (ok if any("INCIDENT" in s.upper() for s in states) else no)("未注册键 → 令牌停 Incident：%s" % states)
else:
    (ok if r and r.get("code") != 0 else no)("未注册键发起即报错（fail-fast）：code=%s msg=%s" % (r.get("code"), r.get("msg", "")))

print("\n========== flow×rules E2E: %d 通过 / %d 失败 ==========" % (PASS, FAIL))
sys.exit(1 if FAIL else 0)
