/*
 * @Describe: 轻量决策表（A3，DMN 决策表的受控子集，自包含零外部依赖）。
 *
 * 为什么手写而非引 DMN 引擎：与 expr.rs「受控 DSL 覆盖 95% 审批条件」同一哲学——审批矩阵
 * （「什么金额/部门 → 几级审批」）用一张可业务维护的表表达，把决策逻辑从流程图里剥离出来。
 * 不引 FEEL/DMN 全套（那是 B 级标准化），先给一个引擎可控、可静态校验、复用 eval_condition
 * 的最小内核。
 *
 * 模型：一张表 = 若干输入(input) + 若干规则(rule)。每条规则对每个输入给一个**条件表达式**
 * （复用 expr.rs 的受控 DSL，对当前变量求值），全部输入条件都为真 → 规则命中 → 应用其输出
 * （output 名 → 值）写回变量。命中策略 First（首个命中即止）/ Collect（全部命中依次应用）。
 *
 * 与运行时的接口：引擎注册决策表（类似 delegate 注册表），businessRuleTask 按 key 调
 * `evaluate(table, vars)` 求值并把输出 merge 进实例变量。纯函数、无 IO、可 wasm。
 */

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::expr::eval_condition;
use crate::variables::Variables;

/// 命中策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HitPolicy {
    /// 首个命中的规则即止（对齐 DMN FIRST）——审批矩阵常用（规则按优先序排列）。
    First,
    /// 收集所有命中规则，依次应用输出（后者覆盖同名 output）（对齐 DMN COLLECT 的简化）。
    Collect,
}

impl Default for HitPolicy {
    fn default() -> Self {
        HitPolicy::First
    }
}

/// 一条规则：对每个输入的条件表达式 + 命中后写回的输出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRule {
    /// 每个输入位的条件表达式（与 `DecisionTable.inputs` 一一对应，个数须相等）。
    ///
    /// 表达式复用 expr.rs 的受控 DSL，对当前变量求值。空串/"-" 视为「不限」（恒真）——
    /// 对齐 DMN 决策表空单元格语义。可写 `amount > 10000`、`level == 'vip'` 等。
    pub conditions: Vec<String>,
    /// 命中后的输出赋值：output 名 → JSON 值。写回实例变量。
    pub outputs: std::collections::BTreeMap<String, Value>,
}

/// 一张决策表。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTable {
    /// 表 key（引擎注册键，businessRuleTask 按此引用）。
    pub key: String,
    /// 输入变量名（仅作文档/校验用；条件表达式自己引用变量，不强制只用这些）。
    #[serde(default)]
    pub inputs: Vec<String>,
    /// 输出变量名（仅作文档/校验用）。
    #[serde(default)]
    pub outputs: Vec<String>,
    /// 命中策略。
    #[serde(default)]
    pub hit_policy: HitPolicy,
    /// 规则集（按优先序；First 策略下靠前者优先）。
    pub rules: Vec<DecisionRule>,
}

/// 求值结果：应用了哪些输出（已 merge 到返回的 Variables）+ 命中规则下标。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionResult {
    /// 命中规则的下标（First 至多一个；Collect 可多个）。
    pub matched_rules: Vec<usize>,
    /// 汇总的输出变量（供引擎 merge 进实例）。
    pub outputs: Variables,
}

impl DecisionTable {
    /// 结构校验：每条规则的条件个数须等于 inputs 个数（inputs 非空时）；key 非空。
    /// 返回可读诊断（空 = 合法）。设计器「保存前校验」用。
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.key.trim().is_empty() {
            errs.push("决策表 key 不能为空".into());
        }
        if self.rules.is_empty() {
            errs.push("决策表至少需要一条规则".into());
        }
        if !self.inputs.is_empty() {
            for (i, r) in self.rules.iter().enumerate() {
                if r.conditions.len() != self.inputs.len() {
                    errs.push(format!(
                        "规则 {} 的条件数 {} 与输入数 {} 不符",
                        i,
                        r.conditions.len(),
                        self.inputs.len()
                    ));
                }
            }
        }
        errs
    }
}

/// 对一张决策表按当前变量求值。
///
/// 逐规则求值：一条规则命中 = 其所有条件表达式都为真（空/"-" 视为恒真）。First 命中即止；
/// Collect 收集所有命中并依次应用输出（后者覆盖同名）。任一条件表达式语法/求值错 → 报错。
pub fn evaluate(table: &DecisionTable, vars: &Variables) -> Result<DecisionResult> {
    let mut result = DecisionResult::default();
    for (ridx, rule) in table.rules.iter().enumerate() {
        let mut all_pass = true;
        for cond in &rule.conditions {
            let c = cond.trim();
            // 空单元格 / "-" = 不限（恒真），跳过求值。
            if c.is_empty() || c == "-" {
                continue;
            }
            if !eval_condition(c, vars)? {
                all_pass = false;
                break;
            }
        }
        if !all_pass {
            continue;
        }
        // 命中：应用输出。
        result.matched_rules.push(ridx);
        for (k, v) in &rule.outputs {
            result.outputs.set(k.clone(), v.clone());
        }
        if table.hit_policy == HitPolicy::First {
            break;
        }
    }
    if result.matched_rules.is_empty() {
        // 无命中：不报错，返回空输出（对齐「决策表未覆盖 → 不写变量」，由流程默认分支兜底）。
        // 若希望强制覆盖可在校验期加 default 规则。
        return Ok(result);
    }
    Ok(result)
}

/// 从 JSON 构造决策表（设计器/注册用）。非法结构 → InvalidDefinition。
pub fn decision_from_json(value: &Value) -> Result<DecisionTable> {
    serde_json::from_value(value.clone())
        .map_err(|e| Error::InvalidDefinition(format!("决策表 JSON 非法: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// 审批级别矩阵：金额 → 审批级别 + 是否需董事会。
    fn approval_matrix() -> DecisionTable {
        let rule = |cond: &str, level: i64, board: bool| DecisionRule {
            conditions: vec![cond.to_string()],
            outputs: {
                let mut m = BTreeMap::new();
                m.insert("approvalLevel".to_string(), json!(level));
                m.insert("needBoard".to_string(), json!(board));
                m
            },
        };
        DecisionTable {
            key: "approval_matrix".into(),
            inputs: vec!["amount".into()],
            outputs: vec!["approvalLevel".into(), "needBoard".into()],
            hit_policy: HitPolicy::First,
            rules: vec![
                rule("amount > 100000", 3, true),
                rule("amount > 10000", 2, false),
                rule("-", 1, false), // 兜底：其余走一级
            ],
        }
    }

    #[test]
    fn first_hit_picks_top_matching_rule() {
        let t = approval_matrix();
        let mut vars = Variables::new();
        vars.set("amount", json!(500000));
        let r = evaluate(&t, &vars).unwrap();
        assert_eq!(r.matched_rules, vec![0], "大额命中第一条");
        assert_eq!(r.outputs.get("approvalLevel"), Some(&json!(3)));
        assert_eq!(r.outputs.get("needBoard"), Some(&json!(true)));
    }

    #[test]
    fn first_hit_mid_and_fallback() {
        let t = approval_matrix();
        let mut vars = Variables::new();
        vars.set("amount", json!(50000));
        let r = evaluate(&t, &vars).unwrap();
        assert_eq!(r.matched_rules, vec![1], "中额命中第二条");
        assert_eq!(r.outputs.get("approvalLevel"), Some(&json!(2)));

        // 小额走兜底（"-" 恒真）。
        let mut v2 = Variables::new();
        v2.set("amount", json!(100));
        let r2 = evaluate(&t, &v2).unwrap();
        assert_eq!(r2.matched_rules, vec![2]);
        assert_eq!(r2.outputs.get("approvalLevel"), Some(&json!(1)));
    }

    #[test]
    fn multi_input_all_must_pass() {
        // 两输入：金额 + 部门。都满足才命中。
        let mut m = BTreeMap::new();
        m.insert("route".to_string(), json!("cfo"));
        let t = DecisionTable {
            key: "k".into(),
            inputs: vec!["amount".into(), "dept".into()],
            outputs: vec!["route".into()],
            hit_policy: HitPolicy::First,
            rules: vec![DecisionRule {
                conditions: vec!["amount > 10000".into(), "dept == 'fin'".into()],
                outputs: m,
            }],
        };
        let mut vars = Variables::new();
        vars.set("amount", json!(20000));
        vars.set("dept", json!("fin"));
        assert_eq!(evaluate(&t, &vars).unwrap().matched_rules, vec![0]);
        // 部门不符 → 不命中。
        let mut v2 = Variables::new();
        v2.set("amount", json!(20000));
        v2.set("dept", json!("hr"));
        assert!(evaluate(&t, &v2).unwrap().matched_rules.is_empty());
    }

    #[test]
    fn collect_applies_all_matching() {
        let mk = |cond: &str, key: &str, val: i64| DecisionRule {
            conditions: vec![cond.to_string()],
            outputs: {
                let mut m = BTreeMap::new();
                m.insert(key.to_string(), json!(val));
                m
            },
        };
        let t = DecisionTable {
            key: "c".into(),
            inputs: vec!["x".into()],
            outputs: vec![],
            hit_policy: HitPolicy::Collect,
            rules: vec![mk("x > 0", "a", 1), mk("x > 10", "b", 2), mk("x > 100", "c", 3)],
        };
        let mut vars = Variables::new();
        vars.set("x", json!(50));
        let r = evaluate(&t, &vars).unwrap();
        assert_eq!(r.matched_rules, vec![0, 1], "x=50 命中前两条");
        assert_eq!(r.outputs.get("a"), Some(&json!(1)));
        assert_eq!(r.outputs.get("b"), Some(&json!(2)));
        assert_eq!(r.outputs.get("c"), None);
    }

    #[test]
    fn no_match_is_empty_not_error() {
        let t = DecisionTable {
            key: "n".into(),
            inputs: vec!["x".into()],
            outputs: vec![],
            hit_policy: HitPolicy::First,
            rules: vec![DecisionRule {
                conditions: vec!["x > 1000".into()],
                outputs: BTreeMap::new(),
            }],
        };
        let mut vars = Variables::new();
        vars.set("x", json!(1));
        assert!(evaluate(&t, &vars).unwrap().matched_rules.is_empty());
    }

    #[test]
    fn validate_catches_shape_errors() {
        let t = DecisionTable {
            key: "".into(),
            inputs: vec!["a".into(), "b".into()],
            outputs: vec![],
            hit_policy: HitPolicy::First,
            rules: vec![DecisionRule {
                conditions: vec!["a > 1".into()], // 只 1 个条件，inputs 有 2 个
                outputs: BTreeMap::new(),
            }],
        };
        let errs = t.validate();
        assert!(errs.iter().any(|e| e.contains("key 不能为空")));
        assert!(errs.iter().any(|e| e.contains("条件数")));
    }

    #[test]
    fn json_roundtrip() {
        let t = approval_matrix();
        let j = serde_json::to_value(&t).unwrap();
        let back = decision_from_json(&j).unwrap();
        assert_eq!(t, back);
    }
}
