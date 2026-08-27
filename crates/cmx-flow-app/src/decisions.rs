//! 决策表管理 + 试算（A3）——businessRuleTask 引用的决策表注册、持久化与调试。
//!
//! 端点：
//!   POST   /flow/decisions          —— 注册/更新一张决策表（body = DecisionTable JSON），**落库 + 热注册到引擎**。
//!   GET    /flow/decisions          —— 列出已落库决策表元数据（key/命中策略/规则数/更新时间）。
//!   DELETE /flow/decisions/{key}    —— 删除一张决策表（删库 + 注销内存注册）。
//!   POST   /flow/decisions/evaluate —— 试算：给决策表 + 变量 → 命中规则 + 输出（设计器调试用，不落库）。
//!
//! 决策表随注册**落库**（cmx_flow_decision，flow 租户库），引擎启动时装载回内存注册表——跨重启/
//! 多实例一致。求值仍走内存注册表（get_decision）。落库失败即报错，不做「只进内存」的静默降级。

use axum::Json;
use axum::extract::Path;
use serde_json::{Value, json};

use cmx_flow_model::Variables;
use cmx_flow_model::decision::{decision_from_json, evaluate as eval_decision};

use crate::engine::flow;
use crate::resp::{ApiResp, FlowError, Result};

/// POST /decisions —— 注册/更新一张决策表（先落库，再热注册到运行引擎）。
pub async fn register_decision(Json(body): Json<Value>) -> Result<Json<ApiResp<Value>>> {
    let table = decision_from_json(&body).map_err(|e| FlowError::business_error(e.to_string()))?;
    let errs = table.validate();
    if !errs.is_empty() {
        return Err(FlowError::business_error(format!("决策表校验未过: {}", errs.join("; "))));
    }
    let updated_by = body
        .get("updatedBy")
        .and_then(Value::as_str)
        .map(str::to_string);
    let key = table.key.clone();
    let rules = table.rules.len();
    let rt = flow().await?;
    // 先落库（发布持久化）——失败即报错，不静默只进内存。
    rt.decision_store
        .upsert(&table, updated_by.as_deref())
        .await
        .map_err(FlowError::business_error)?;
    // 再热注册到运行引擎（求值即时生效）。
    rt.engine.register_decision(table);
    Ok(Json(ApiResp::ok(
        json!({ "key": key, "rules": rules, "persisted": true }),
    )))
}

/// GET /decisions —— 列出已落库决策表元数据。
pub async fn list_decisions() -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let metas = rt
        .decision_store
        .list_meta()
        .await
        .map_err(FlowError::business_error)?;
    let items: Vec<Value> = metas
        .into_iter()
        .map(|m| {
            json!({
                "key": m.key,
                "hitPolicy": m.hit_policy,
                "ruleCount": m.rule_count,
                "inputCount": m.input_count,
                "updatedAt": m.updated_at,
                "updatedBy": m.updated_by,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(
        json!({ "total": items.len(), "decisions": items }),
    )))
}

/// DELETE /decisions/{key} —— 删除一张决策表（删库 + 注销内存注册）。
pub async fn delete_decision(Path(key): Path<String>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.decision_store
        .delete(&key)
        .await
        .map_err(FlowError::business_error)?;
    rt.engine.unregister_decision(&key);
    Ok(Json(ApiResp::ok(json!({ "key": key, "deleted": true }))))
}

/// GET /decisions/{key} —— 取单张决策表全表（含 inputs/outputs/hitPolicy/rules），供可视化查看器渲染网格。
pub async fn get_decision(Path(key): Path<String>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let table = rt
        .decision_store
        .get(&key)
        .await
        .map_err(FlowError::business_error)?
        .ok_or_else(|| FlowError::not_found(format!("决策表不存在: {key}")))?;
    // DecisionTable 派生 Serialize（rules/hit_policy 等），直接转 JSON 返回。
    let body = serde_json::to_value(&table)
        .map_err(|e| FlowError::business_error(format!("决策表序列化失败: {e}")))?;
    Ok(Json(ApiResp::ok(body)))
}

/// POST /decisions/evaluate —— 试算：{table, variables} → {matchedRules, outputs}。
///
/// table 为内联决策表 JSON（设计器调试），variables 为样例变量。纯函数，不注册、不落库。
pub async fn evaluate_decision(Json(body): Json<Value>) -> Result<Json<ApiResp<Value>>> {
    let table_json = body.get("table").ok_or_else(|| FlowError::business_error("缺少 table"))?;
    let table = decision_from_json(table_json).map_err(|e| FlowError::business_error(e.to_string()))?;
    let vars = Variables::from_json(body.get("variables").cloned().unwrap_or(json!({})));
    match eval_decision(&table, &vars) {
        Ok(res) => Ok(Json(ApiResp::ok(json!({
            "matchedRules": res.matched_rules,
            "outputs": res.outputs.to_json(),
        })))),
        Err(e) => Err(FlowError::business_error(format!("决策求值失败: {e}"))),
    }
}
