/*
 * @Describe: cmx-flow-model —— 流程引擎的语义中立内核。
 *
 * 血缘与定位见各子模块头注。对外导出：
 * - IR：ProcessDefinition / FlowNode / NodeKind / SequenceFlow（编译产物，只读执行）
 * - 运行态：ProcessInstance / Token / Task / InstanceSnapshot（引擎↔存储流转单元）
 * - Variables：流程作用域动态变量容器
 * - expr::eval_condition：受控条件表达式求值（sequenceFlow 条件）
 * - RuntimeStore：驱动无关持久化契约
 *
 * 本 crate 不依赖任何数据库、任何 cmx-* infra，可被 wasm / 嵌入式复用。
 */

pub mod candidate;
pub mod decision;
pub mod duration;
pub mod error;
pub mod expr;
pub mod ir;
pub mod migration;
pub mod resolver;
pub mod runtime;
pub mod store;
pub mod subflow;
pub mod var_schema;
pub mod variables;

// —— 扁平化再导出，给消费方一个稳定的浅层 API —— //
pub use error::{Error, Result};
pub use expr::{BuiltinFn, builtin_catalog, eval_condition, eval_value, interpolate, validate_syntax};
pub use ir::{
    BoundaryError, BoundaryTimer, BusinessRule, CallActivity, CandidateKind, CandidateRef,
    ErrorStart, FlowNode, MessageCatch, MessageStart, MultiInstance, NodeId, NodeKind,
    ProcessDefinition, SequenceFlow, ServiceTask, TimerDuration, TimerSpec, UserTask, VarMapping,
};
pub use resolver::{AssigneeResolver, ResolveContext, ResolveError, ResolveResult};
pub use migration::{
    MigrationPlan, MigrationValidation, MigrationViolation, MigrationViolationCode,
};pub use decision::{
    DecisionResult, DecisionRule, DecisionTable, HitPolicy, decision_from_json,
    evaluate as evaluate_decision,
};
pub use runtime::{
    AsyncJob, ActivityRecord, CcRecord, CcSummary, DeadLetterJob, DueJob, InstanceSnapshot,
    InstanceState, InstanceSummary, MessageSubscription, MessageSubscriptionKind, MiScope,
    ProcessInstance, Task, TaskCandidate, TaskDelegation, TimerJob, TimerJobKind, Token, TokenState,
};
pub use store::{RuntimeStore, StoreError, StoreResult};
pub use subflow::{RouteError, RouteResult, SubflowRouter, DIM_ORG};
pub use var_schema::{
    VarDecl, VarPath, VarSchema, VarScope, VarSource, VarType, VarViolation, VarViolationCode,
};
pub use variables::Variables;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expr_numeric_comparison() {
        let mut vars = Variables::new();
        vars.set("amount", json!(1500));
        assert!(expr::eval_condition("${amount > 1000}", &vars).unwrap());
        assert!(!expr::eval_condition("${amount <= 1000}", &vars).unwrap());
    }

    #[test]
    fn expr_bool_and_logic() {
        let mut vars = Variables::new();
        vars.set("approved", json!(true));
        vars.set("amount", json!(500));
        assert!(expr::eval_condition("approved && amount < 1000", &vars).unwrap());
        assert!(!expr::eval_condition("approved && amount > 1000", &vars).unwrap());
        assert!(expr::eval_condition("approved || amount > 1000", &vars).unwrap());
    }

    #[test]
    fn expr_string_equality() {
        let mut vars = Variables::new();
        vars.set("level", json!("vip"));
        assert!(expr::eval_condition("level == 'vip'", &vars).unwrap());
        assert!(!expr::eval_condition("level == 'normal'", &vars).unwrap());
        assert!(expr::eval_condition("level != 'normal'", &vars).unwrap());
    }

    #[test]
    fn expr_empty_is_true() {
        let vars = Variables::new();
        assert!(expr::eval_condition("", &vars).unwrap());
        assert!(expr::eval_condition("   ", &vars).unwrap());
    }

    #[test]
    fn expr_missing_var_is_null_falsy() {
        let vars = Variables::new();
        // 缺失变量 → null → 比较为假，不 panic。
        assert!(!expr::eval_condition("approved", &vars).unwrap());
        assert!(expr::eval_condition("!approved", &vars).unwrap());
    }

    #[test]
    fn expr_null_numeric_comparison_is_false_not_error() {
        // 未赋值变量参与数值比较 → false（不报错）：审批「缺数据即条件不满足」+ 决策未写变量不误抛。
        let vars = Variables::new();
        assert!(!expr::eval_condition("approvalLevel >= 3", &vars).unwrap());
        assert!(!expr::eval_condition("amount < 100", &vars).unwrap());
        assert!(!expr::eval_condition("x > y", &vars).unwrap());
    }

    #[test]
    fn expr_arithmetic_precedence() {
        let mut vars = Variables::new();
        vars.set("base", json!(10));
        // 2 + 10 * 3 = 32，验证乘法优先级。
        assert!(expr::eval_condition("2 + base * 3 == 32", &vars).unwrap());
    }

    #[test]
    fn expr_paren_override() {
        let mut vars = Variables::new();
        vars.set("base", json!(10));
        // (2 + base) * 3 = 36
        assert!(expr::eval_condition("(2 + base) * 3 == 36", &vars).unwrap());
    }

    #[test]
    fn variables_roundtrip_json() {
        let mut vars = Variables::new();
        vars.set("a", json!(1));
        vars.set("b", json!("x"));
        let j = vars.to_json();
        let back = Variables::from_json(j);
        assert_eq!(vars, back);
    }

    // ————————————————— P2-a：嵌套路径 —————————————————

    #[test]
    fn expr_nested_path_object() {
        let mut vars = Variables::new();
        vars.set("order", json!({"amount": 12000, "buyer": {"vip": true}}));
        // order.amount 真正下钻到对象字段（历史 bug：曾被当平坦 key）。
        assert!(expr::eval_condition("order.amount > 10000", &vars).unwrap());
        assert!(!expr::eval_condition("order.amount < 10000", &vars).unwrap());
        // 多级下钻。
        assert!(expr::eval_condition("order.buyer.vip", &vars).unwrap());
    }

    #[test]
    fn expr_nested_path_array_index() {
        let mut vars = Variables::new();
        vars.set("items", json!([{"qty": 3}, {"qty": 9}]));
        assert!(expr::eval_condition("items.0.qty == 3", &vars).unwrap());
        assert!(expr::eval_condition("items.1.qty == 9", &vars).unwrap());
    }

    #[test]
    fn expr_flat_dotted_key_backward_compat() {
        // 历史流程若真存了名为 "order.amount" 的平坦 key，整名精确匹配须优先命中。
        let mut vars = Variables::new();
        vars.set("order.amount", json!(5));
        // 精确命中平坦 key = 5，而非下钻不存在的 order 对象（那会得 null）。
        assert!(expr::eval_condition("order.amount == 5", &vars).unwrap());
    }

    #[test]
    fn expr_nested_missing_is_null_falsy() {
        let mut vars = Variables::new();
        vars.set("order", json!({"amount": 1}));
        // 任一段缺失 → null → 比较为假，不报错。
        assert!(!expr::eval_condition("order.buyer.vip", &vars).unwrap());
        assert!(!expr::eval_condition("nope.deep.path == 1", &vars).unwrap());
    }

    // ————————————————— P2-a：内置函数库 —————————————————

    #[test]
    fn expr_fn_string() {
        let mut vars = Variables::new();
        vars.set("name", json!("  Alice  "));
        vars.set("code", json!("INV-2026"));
        assert!(expr::eval_condition("TRIM(name) == 'Alice'", &vars).unwrap());
        assert!(expr::eval_condition("UPPER('ab') == 'AB'", &vars).unwrap());
        assert!(expr::eval_condition("LOWER('AB') == 'ab'", &vars).unwrap());
        assert!(expr::eval_condition("LEN(TRIM(name)) == 5", &vars).unwrap());
        assert!(expr::eval_condition("STARTSWITH(code, 'INV')", &vars).unwrap());
        assert!(expr::eval_condition("ENDSWITH(code, '2026')", &vars).unwrap());
    }

    #[test]
    fn expr_fn_collection() {
        let mut vars = Variables::new();
        vars.set("tags", json!(["a", "b", "c"]));
        vars.set("role", json!("finance"));
        assert!(expr::eval_condition("CONTAINS(tags, 'b')", &vars).unwrap());
        assert!(!expr::eval_condition("CONTAINS(tags, 'z')", &vars).unwrap());
        assert!(expr::eval_condition("LEN(tags) == 3", &vars).unwrap());
        assert!(expr::eval_condition("IN(role, 'finance', 'legal')", &vars).unwrap());
        assert!(!expr::eval_condition("IN(role, 'hr', 'it')", &vars).unwrap());
        assert!(expr::eval_condition("IS_EMPTY(missing)", &vars).unwrap());
        assert!(!expr::eval_condition("IS_EMPTY(tags)", &vars).unwrap());
    }

    #[test]
    fn expr_fn_numeric_and_logic() {
        let mut vars = Variables::new();
        vars.set("x", json!(-7.4));
        assert!(expr::eval_condition("ABS(x) > 7", &vars).unwrap());
        assert!(expr::eval_condition("ROUND(x) == -7", &vars).unwrap());
        assert!(expr::eval_condition("FLOOR(x) == -8", &vars).unwrap());
        assert!(expr::eval_condition("CEIL(x) == -7", &vars).unwrap());
        assert!(expr::eval_condition("MAX(1, 5, 3) == 5", &vars).unwrap());
        assert!(expr::eval_condition("MIN(1, 5, 3) == 1", &vars).unwrap());
        // COALESCE 取第一个非 null；missing 缺失 → null。
        assert!(expr::eval_condition("COALESCE(missing, 42) == 42", &vars).unwrap());
        assert!(expr::eval_condition("IF(x < 0, 'neg', 'pos') == 'neg'", &vars).unwrap());
    }

    #[test]
    fn expr_fn_unknown_is_error() {
        let vars = Variables::new();
        // 未知函数应报错，不静默为真。
        assert!(expr::eval_condition("BOGUS(1)", &vars).is_err());
    }

    #[test]
    fn expr_fn_composed_with_nested_path() {
        // 函数 + 嵌套路径联合：贴近真实分支条件构造器产物。
        let mut vars = Variables::new();
        vars.set("order", json!({"amount": 30000, "region": "north"}));
        assert!(
            expr::eval_condition(
                "order.amount >= 10000 && IN(order.region, 'north', 'south')",
                &vars
            )
            .unwrap()
        );
    }

    #[test]
    fn expr_validate_syntax_ok_and_err() {
        // 语法校验不需变量环境。
        assert!(expr::validate_syntax("order.amount > 10000 && approved").is_ok());
        assert!(expr::validate_syntax("${IN(role, 'a', 'b')}").is_ok());
        assert!(expr::validate_syntax("").is_ok()); // 空 = 无条件边
        // 语法错：缺右括号 / 悬空运算符。
        assert!(expr::validate_syntax("(a > 1").is_err());
        assert!(expr::validate_syntax("a >").is_err());
        assert!(expr::validate_syntax("BOGUS(").is_err());
    }

    #[test]
    fn expr_builtin_catalog_matches_dispatch() {
        // 目录里每个函数都能被 call_builtin 识别（不返回「未知函数」）。
        // 用满足元数的哑参数调用，断言错误不是 ExprSyntax(未知函数)。
        for f in expr::builtin_catalog() {
            let n = f.arity.unwrap_or(1).max(1);
            let args: Vec<String> = (0..n).map(|_| "1".to_string()).collect();
            let call = format!("{}({})", f.name, args.join(", "));
            // 允许求值错（类型不符），但不允许「未知函数」语法错。
            if let Err(e) = expr::eval_condition(&call, &Variables::new()) {
                let msg = e.to_string();
                assert!(
                    !msg.contains("未知函数"),
                    "目录函数 {} 未被 call_builtin 识别: {msg}",
                    f.name
                );
            }
        }
    }

    #[test]
    fn expr_eval_value_returns_typed_value() {
        let mut vars = Variables::new();
        vars.set("n", serde_json::json!(42));
        vars.set("s", serde_json::json!("hi"));
        vars.set("obj", serde_json::json!({"k": "v"}));
        assert_eq!(expr::eval_value("n", &vars).unwrap(), serde_json::json!(42)); // 裸变量返回原值
        assert_eq!(expr::eval_value("s", &vars).unwrap(), serde_json::json!("hi"));
        assert_eq!(expr::eval_value("obj.k", &vars).unwrap(), serde_json::json!("v"));
        assert_eq!(expr::eval_value("", &vars).unwrap(), serde_json::Value::Null); // 空 → Null
        assert_eq!(expr::eval_value("missing", &vars).unwrap(), serde_json::Value::Null);
    }

    #[test]
    fn expr_interpolate_templates() {
        let mut vars = Variables::new();
        vars.set("approver", serde_json::json!("u1"));
        vars.set("product", serde_json::json!({"id": 42, "ownerUser": "u_a"}));
        // 整串即表达式。
        assert_eq!(expr::interpolate("${approver}", &vars).unwrap(), "u1");
        assert_eq!(expr::interpolate("${product.ownerUser}", &vars).unwrap(), "u_a");
        // 嵌入式（字面量 + 表达式）。
        assert_eq!(expr::interpolate("user_${product.id}", &vars).unwrap(), "user_42");
        // 无 ${} → 原样（静态字面量向后兼容）。
        assert_eq!(expr::interpolate("mgr", &vars).unwrap(), "mgr");
        // 求值 null → 空串。
        assert_eq!(expr::interpolate("${product.missing}", &vars).unwrap(), "");
        // 缺闭合 } → 语法错。
        assert!(expr::interpolate("${approver", &vars).is_err());
    }
}
