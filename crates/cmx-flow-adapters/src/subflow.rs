//! HttpSubflowRouter —— 子流程组织路由的外部 HTTP 实现（方案 §4②）。
//!
//! 实现 `SubflowRouter`：把「逻辑子流程 key + 组织 id」POST 给外部组织服务解析成具体子流程
//! 定义 key，替代 Pg 版沿 cmx_org.path 继承的库内解析。组织树/绑定的真相在外部组织服务。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cmx_flow_engine::{RouteError, RouteResult, SubflowRouter};

/// 外部组织服务子流程路由器。
#[derive(Debug, Clone)]
pub struct HttpSubflowRouter {
    base_url: String,
    http: reqwest::Client,
}

/// `POST {base}/subflow/resolve` 请求体。
#[derive(Serialize)]
struct RouteReq<'a> {
    #[serde(rename = "calledKey")]
    called_key: &'a str,
    #[serde(rename = "orgId", skip_serializing_if = "Option::is_none")]
    org_id: Option<&'a str>,
}

/// 期望响应体：解析出的目标子流程定义 key（无解时 targetKey 为 null/缺省）。
#[derive(Deserialize)]
struct RouteResp {
    #[serde(default, rename = "targetKey")]
    target_key: Option<String>,
}

impl HttpSubflowRouter {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl SubflowRouter for HttpSubflowRouter {
    async fn resolve(&self, called_key: &str, org_id: Option<&str>) -> RouteResult<String> {
        let url = format!("{}/subflow/resolve", self.base_url);
        let body = RouteReq { called_key, org_id };
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RouteError::Backend(format!("组织服务请求失败: {e}")))?;

        let status = resp.status();
        // 404 或非成功但非 5xx：视为「无绑定」（与 Pg 版全无绑定的语义一致）。
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(RouteError::NoBinding {
                called_key: called_key.to_string(),
                org: org_id.map(|s| s.to_string()),
            });
        }
        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            return Err(RouteError::Backend(format!("组织服务返回 {status}: {msg}")));
        }

        let parsed: RouteResp = resp
            .json()
            .await
            .map_err(|e| RouteError::Backend(format!("组织服务响应解析失败: {e}")))?;
        match parsed.target_key.filter(|s| !s.is_empty()) {
            Some(t) => Ok(t),
            // 200 但无 targetKey → 同样无解。
            None => Err(RouteError::NoBinding {
                called_key: called_key.to_string(),
                org: org_id.map(|s| s.to_string()),
            }),
        }
    }
}
