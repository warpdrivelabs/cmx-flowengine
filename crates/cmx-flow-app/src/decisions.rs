//! 决策表管理 + 试算（A3）——businessRuleTask 引用的决策表注册与调试。
//!
//! 端点：
//!   POST /flow/decisions          —— 注册/更新一张决策表（body = DecisionTable JSON），热注册到引擎。
//!   POST /flow/decisions/evaluate —— 试算：给决策表 + 变量 → 命中规则 + 输出（设计器调试用，不落库）。
//!
//! 决策表当前存引擎内存注册表（热注册，Arc 后仍可注册）。持久化（随定义发布落库）是后续增强；
//! 本轮先打通「注册 → businessRuleTask 求值 → 写回变量」闭环，纯函数试算便于设计器调试审批矩阵。

use axum::Json;
use serde_json::{Value, json};

use cmx_flow_model::decision::{decision_from_json, evaluate as eval_decision};
use cmx_flow_model::Variables;

use crate::engine::flow;
use crate::resp::{ApiResp, FlowError, Result};

/// POST /decisions —— 注册/更新一张决策表（热注册到运行引擎）。
pub async fn register_decision(Json(body): Json<Value>) -> Result<Json<ApiResp<Value>>> {
    let table = decision_from_json(&body).map_err(|e| FlowError::business(e.to_string()))?;
    let errs = table.validate();
    if !errs.is_empty() {
        return Err(FlowError::business(format!("决策表校验未过: {}", errs.join("; "))));
    }
    let key = table.key.clone();
    let rules = table.rules.len();
    let rt = flow().await?;
    rt.engine.register_decision(table);
    Ok(Json(ApiResp::ok(json!({ "key": key, "rules": rules }))))
}

/// POST /decisions/evaluate —— 试算：{table, variables} → {matchedRules, outputs}。
///
/// table 为内联决策表 JSON（设计器调试），variables 为样例变量。纯函数，不注册、不落库。
pub async fn evaluate_decision(Json(body): Json<Value>) -> Result<Json<ApiResp<Value>>> {
    let table_json = body.get("table").ok_or_else(|| FlowError::business("缺少 table"))?;
    let table = decision_from_json(table_json).map_err(|e| FlowError::business(e.to_string()))?;
    let vars = Variables::from_json(body.get("variables").cloned().unwrap_or(json!({})));
    match eval_decision(&table, &vars) {
        Ok(res) => Ok(Json(ApiResp::ok(json!({
            "matchedRules": res.matched_rules,
            "outputs": res.outputs.to_json(),
        })))),
        Err(e) => Err(FlowError::business(format!("决策求值失败: {e}"))),
    }
}
