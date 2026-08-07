//! HttpDelegate —— serviceTask 逻辑外包的外部 HTTP 实现（方案 §4④）。
//!
//! 实现 `JavaDelegate`：把实例当前变量 POST 给外部 URL，拿返回变量 merge 回实例，替代进程内
//! delegate（如 RiskDelegate）。「算风险/调外部逻辑」外包给第三方——引擎不认识业务逻辑。
//!
//! URL 来源：ServiceTask IR 只带 delegate 键（无 URL 槽），故 URL 由本实例持有（一个 delegate
//! 键一个 URL，注册在 delegate 注册表）。请求带 `?node=&instance=` 供外部按节点/实例细分。

use async_trait::async_trait;
use serde_json::Value;

use cmx_flow_engine::{DelegateContext, JavaDelegate, Variables};

/// 外部 serviceTask 逻辑委托。
#[derive(Debug, Clone)]
pub struct HttpDelegate {
    url: String,
    http: reqwest::Client,
}

impl HttpDelegate {
    /// 用外部逻辑服务 URL 构建。
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl JavaDelegate for HttpDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String> {
        // 请求体 = 当前全部实例变量（对象）；query 带节点/实例供外部细分。
        let body = ctx.variables.to_json();
        let resp = self
            .http
            .post(&self.url)
            .query(&[("node", ctx.node_bpmn_id), ("instance", ctx.instance_id)])
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("外部 delegate 请求失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            return Err(format!("外部 delegate 返回 {status}: {msg}"));
        }

        let out: Value = resp
            .json()
            .await
            .map_err(|e| format!("外部 delegate 响应解析失败: {e}"))?;

        // 兼容两种返回形态：{"variables":{...}} 包裹，或直接是变量对象。
        let vars_json = match out {
            Value::Object(ref m) if m.contains_key("variables") => {
                out.get("variables").cloned().unwrap_or(Value::Null)
            }
            other => other,
        };
        // 写回：merge 返回键（同名覆盖），非对象返回则忽略（no-op，不报错）。
        if vars_json.is_object() {
            ctx.variables.merge(Variables::from_json(vars_json));
        }
        Ok(())
    }
}
