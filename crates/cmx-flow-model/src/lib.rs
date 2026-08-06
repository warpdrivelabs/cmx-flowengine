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
pub mod duration;
pub mod error;
pub mod expr;
pub mod ir;
pub mod resolver;
pub mod runtime;
pub mod store;
pub mod subflow;
pub mod variables;

// —— 扁平化再导出，给消费方一个稳定的浅层 API —— //
pub use error::{Error, Result};
pub use ir::{
    BoundaryTimer, CallActivity, CandidateKind, CandidateRef, FlowNode, MultiInstance, NodeId,
    NodeKind, ProcessDefinition, SequenceFlow, ServiceTask, TimerDuration, UserTask, VarMapping,
};
pub use resolver::{AssigneeResolver, ResolveError, ResolveResult};
pub use runtime::{
    CcRecord, CcSummary, DueJob, InstanceSnapshot, InstanceState, InstanceSummary, MiScope,
    ProcessInstance, Task, TaskCandidate, TaskDelegation, TimerJob, Token, TokenState,
};
pub use store::{RuntimeStore, StoreError, StoreResult};
pub use subflow::{RouteError, RouteResult, SubflowRouter};
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
}
