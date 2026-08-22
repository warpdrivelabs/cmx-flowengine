//! HttpSubflowRouter —— 子流程路由的外部 HTTP 实现（方案 §4②）。
//!
//! 实现 `SubflowRouter`：把「逻辑子流程 key + 路由维度 + 维度取值」POST 给外部服务解析成具体子流程
//! 定义 key，替代 Pg 版沿维度字典物化路径继承的库内解析。维度字典/绑定的真相在外部服务。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cmx_flow_engine::{DimensionResolver, RouteError, RouteResult, SubflowRouter};

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
    #[serde(rename = "dimKey")]
    dim_key: &'a str,
    #[serde(rename = "dimValue", skip_serializing_if = "Option::is_none")]
    dim_value: Option<&'a str>,
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
    async fn resolve(
        &self,
        called_key: &str,
        dim_key: &str,
        dim_value: Option<&str>,
    ) -> RouteResult<String> {
        let url = format!("{}/subflow/resolve", self.base_url);
        let body = RouteReq { called_key, dim_key, dim_value };
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
                dim_key: dim_key.to_string(),
                dim_value: dim_value.map(|s| s.to_string()),
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
                dim_key: dim_key.to_string(),
                dim_value: dim_value.map(|s| s.to_string()),
            }),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RD5 · 维度层级的外部 HTTP 实现 —— 独立部署时经它读字典/组织层级做继承解析。
// 绑定表始终 flow 本地；本实现只解耦「维度祖先链」这一外部事实源（不直连字典表）。
// ─────────────────────────────────────────────────────────────────────────────

/// 期望响应体：祖先取值链（由近及远，不含自身）。
#[derive(Deserialize)]
struct AncestorsResp {
    #[serde(default)]
    ancestors: Vec<String>,
}

/// 维度层级的外部 HTTP 实现（RD5）。
///
/// `GET {base}/dimensions/ancestors?dimKey=..&dimValue=..` → `{"ancestors":["parent","grandparent",..]}`。
/// 404 视为「无层级」（返回空链 = 无继承，非错误）；非成功 5xx/4xx → Backend 错误。
#[derive(Debug, Clone)]
pub struct HttpDimensionResolver {
    base_url: String,
    http: reqwest::Client,
}

impl HttpDimensionResolver {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl DimensionResolver for HttpDimensionResolver {
    async fn ancestors(&self, dim_key: &str, dim_value: &str) -> RouteResult<Vec<String>> {
        let url = format!("{}/dimensions/ancestors", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("dimKey", dim_key), ("dimValue", dim_value)])
            .send()
            .await
            .map_err(|e| RouteError::Backend(format!("维度服务请求失败: {e}")))?;
        let status = resp.status();
        // 无层级/未知取值 → 空链（无继承，非错误；与 Pg 版平级维度天然跳继承一致）。
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            return Err(RouteError::Backend(format!("维度服务返回 {status}: {msg}")));
        }
        let parsed: AncestorsResp = resp
            .json()
            .await
            .map_err(|e| RouteError::Backend(format!("维度服务响应解析失败: {e}")))?;
        Ok(parsed.ancestors)
    }
}

/// 维度层级的 Mock 实现（测试/脱外部）：按 dim_value 固定祖先链。
#[derive(Debug, Clone, Default)]
pub struct MockDimensionResolver {
    map: std::collections::HashMap<String, Vec<String>>,
}

impl MockDimensionResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条 `dim_value → 祖先链（由近及远）` 映射。链式。
    pub fn with(mut self, dim_value: impl Into<String>, ancestors: Vec<String>) -> Self {
        self.map.insert(dim_value.into(), ancestors);
        self
    }
}

#[async_trait]
impl DimensionResolver for MockDimensionResolver {
    async fn ancestors(&self, _dim_key: &str, dim_value: &str) -> RouteResult<Vec<String>> {
        Ok(self.map.get(dim_value).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod rd5_tests {
    use super::*;

    #[tokio::test]
    async fn mock_dimension_resolver_returns_configured_ancestors() {
        let r = MockDimensionResolver::new()
            .with("fin_bj_g1", vec!["fin_bj".into(), "zongbu".into()]);
        assert_eq!(
            r.ancestors("org", "fin_bj_g1").await.unwrap(),
            vec!["fin_bj".to_string(), "zongbu".to_string()]
        );
        // 未知取值 → 空链（无继承）。
        assert!(r.ancestors("org", "unknown").await.unwrap().is_empty());
    }
}
