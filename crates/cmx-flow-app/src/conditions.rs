//! 分支条件求值 / 校验 / 函数目录（P2-b：给前端可视化条件构造器的真实后端）。
//!
//! 三个端点，全部是**纯函数**（不取引擎单例、不碰租户库、无 IO）——因此在两壳（平台
//! cmx-flow-api / 独立 cmx-flow-server）里表现完全一致，且可直接 curl 验证：
//!
//! - `POST /flow/conditions/eval`      —— 给表达式 + 变量 → 求布尔值（设计器「试算」按钮）
//! - `POST /flow/conditions/validate`  —— 只校验语法（设计器「保存前校验」）
//! - `GET  /flow/conditions/functions` —— 内置函数目录（设计器「函数列」下拉的权威来源）
//!
//! 契约驱动：函数下拉读 `/functions` 而非前端另维护一份清单——引擎加函数，前端自动可见。

use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use cmx_flow_model::{Variables, builtin_catalog, eval_condition, validate_syntax};

use crate::resp::{ApiResp, FlowError, Result};

/// 求值请求：一条条件表达式 + 变量环境（JSON 对象，缺省空）。
#[derive(Debug, Deserialize)]
pub struct EvalReq {
    /// 条件表达式源文本（可含或不含 `${...}`）。
    pub expr: String,
    /// 变量环境（JSON 对象）；非对象或缺省 → 空变量集。
    #[serde(default)]
    pub variables: Value,
}

/// 求值一条分支条件：返回 `{expr, result, truthy}`。
///
/// 语法错 → 业务错误（HTTP 200 + code=1，前端弹校验提示）；求值错（类型不符/除零）同理。
/// 设计器「试算」：填一组样例变量，即时看这条边走不走。
pub async fn eval(Json(req): Json<EvalReq>) -> Result<Json<ApiResp<Value>>> {
    let vars = Variables::from_json(req.variables.clone());
    match eval_condition(&req.expr, &vars) {
        Ok(b) => Ok(Json(ApiResp::ok(json!({
            "expr": req.expr,
            "result": b,
            "truthy": b,
        })))),
        Err(e) => Err(FlowError::business(format!("条件求值失败: {e}"))),
    }
}

/// 语法校验请求：只需表达式本身。
#[derive(Debug, Deserialize)]
pub struct ValidateReq {
    /// 待校验的条件表达式源文本。
    pub expr: String,
}

/// 只校验条件表达式**语法**（不需变量）：返回 `{valid, error?}`。
///
/// 与 eval 不同：这里语法非法**也回 200 + 结构化 `{valid:false,error}`**（而非业务错误），
/// 便于设计器把校验结果就地渲染在条件行旁，无需 catch。空表达式 = 合法（无条件边）。
pub async fn validate(Json(req): Json<ValidateReq>) -> Result<Json<ApiResp<Value>>> {
    match validate_syntax(&req.expr) {
        Ok(()) => Ok(Json(ApiResp::ok(json!({ "valid": true })))),
        Err(e) => Ok(Json(ApiResp::ok(json!({
            "valid": false,
            "error": e.to_string(),
        })))),
    }
}

/// 内置函数目录：返回 `[{name,category,arity,desc}]`。
///
/// 前端「函数列」下拉直接消费本表——契约驱动，勿在前端另维护清单。`arity=null` 表示可变参数。
pub async fn functions() -> Result<Json<ApiResp<Value>>> {
    let items: Vec<Value> = builtin_catalog()
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "category": f.category,
                "arity": f.arity,
                "desc": f.desc,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "functions": items }))))
}
