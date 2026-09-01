//! HttpSubflowRouter —— 子流程路由的外部 HTTP 实现（方案 §4②）。
//!
//! 实现 `SubflowRouter`：把「逻辑子流程 key + 路由维度 + 维度取值」POST 给外部服务解析成具体子流程
//! 定义 key，替代 Pg 版沿维度字典物化路径继承的库内解析。维度字典/绑定的真相在外部服务。
//!
//! 传输走 cmx-service-rpc 基座：目标 = `[service_rpc.services]` 服务目录键（无注册中心时
//! 目录登记静态 url 直连）。对端协议保持自定义裸 JSON（非 ApiResp 信封——外部组织服务
//! 不受 CMX 契约约束），故用 `execute` 取原始响应。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cmx_flow_engine::{DimensionResolver, RouteError, RouteResult, SubflowRouter};
use cmx_service_rpc::{ServiceRpcError, ServiceRpcHandle};

/// 外部组织服务子流程路由器。持服务目录键 + 基座句柄。
#[derive(Clone)]
pub struct HttpSubflowRouter {
    key: String,
    rpc: Arc<ServiceRpcHandle>,
}

/// `POST {svc}/subflow/resolve` 请求体。
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

/// 期望响应体：祖先取值链（由近及远，不含自身）。
#[derive(Deserialize)]
struct AncestorsResp {
    #[serde(default)]
    ancestors: Vec<String>,
}

impl HttpSubflowRouter {
    /// 生产构造：目标为服务目录键（`[service_rpc.services]` 登记）。基座未初始化时返回
    /// `None`，装配点据此回退 mock。
    pub fn new(key: impl Into<String>) -> Option<Self> {
        cmx_service_rpc::global_arc().map(|rpc| Self {
            key: key.into(),
            rpc,
        })
    }

    /// 测试/定制构造：显式传入基座句柄（不经全局单例，测试并行安全）。
    pub fn with_handle(key: impl Into<String>, rpc: ServiceRpcHandle) -> Self {
        Self {
            key: key.into(),
            rpc: Arc::new(rpc),
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
        let body = RouteReq {
            called_key,
            dim_key,
            dim_value,
        };
        let req = cmx_service_rpc::RpcRequest::post(self.key.clone(), "/subflow/resolve")
            .json_body(serde_json::to_value(&body).map_err(|e| {
                RouteError::Backend(format!("组织服务请求序列化失败: {e}"))
            })?)
            // 查询语义（只读路由解析），允许连接级错误换实例重试。
            .idempotent();
        // 基座把非 2xx 归并为 Remote；404 语义为「无绑定」（与 Pg 版全无绑定一致），
        // 200 但 targetKey 空/缺省 → 同样无解。
        let no_binding = || RouteError::NoBinding {
            called_key: called_key.to_string(),
            dim_key: dim_key.to_string(),
            dim_value: dim_value.map(|s| s.to_string()),
        };
        let resp = match self.rpc.execute(req).await {
            Err(ServiceRpcError::Remote { http_status: 404, .. }) => return Err(no_binding()),
            Err(e) => return Err(RouteError::Backend(format!("组织服务调用失败: {e}"))),
            Ok(resp) => resp,
        };

        // 基座对无法解析为 JSON 的响应置 Null——与原裸 reqwest 行为对齐，此时报错而非解出 None。
        if resp.body.is_null() {
            return Err(RouteError::Backend(
                "组织服务响应解析失败: 非 JSON".to_string(),
            ));
        }
        let parsed: RouteResp = serde_json::from_value(resp.body)
            .map_err(|e| RouteError::Backend(format!("组织服务响应解析失败: {e}")))?;
        match parsed.target_key.filter(|s| !s.is_empty()) {
            Some(t) => Ok(t),
            None => Err(no_binding()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RD5 · 维度层级的外部 HTTP 实现 —— 独立部署时经它读字典/组织层级做继承解析。
// 绑定表始终 flow 本地；本实现只解耦「维度祖先链」这一外部事实源（不直连字典表）。
// ─────────────────────────────────────────────────────────────────────────────

/// 维度层级的外部 HTTP 实现（RD5）。
///
/// `GET {svc}/dimensions/ancestors?dimKey=..&dimValue=..` → `{"ancestors":["parent","grandparent",..]}`。
/// 404 视为「无层级」（返回空链 = 无继承，非错误）；非成功 5xx/4xx → Backend 错误。
#[derive(Clone)]
pub struct HttpDimensionResolver {
    key: String,
    rpc: Arc<ServiceRpcHandle>,
}

impl HttpDimensionResolver {
    /// 生产构造：目标为服务目录键。基座未初始化时返回 `None`（装配点据此退回库内继承）。
    pub fn new(key: impl Into<String>) -> Option<Self> {
        cmx_service_rpc::global_arc().map(|rpc| Self {
            key: key.into(),
            rpc,
        })
    }

    /// 测试/定制构造：显式传入基座句柄（不经全局单例，测试并行安全）。
    pub fn with_handle(key: impl Into<String>, rpc: ServiceRpcHandle) -> Self {
        Self {
            key: key.into(),
            rpc: Arc::new(rpc),
        }
    }
}

#[async_trait]
impl DimensionResolver for HttpDimensionResolver {
    async fn ancestors(&self, dim_key: &str, dim_value: &str) -> RouteResult<Vec<String>> {
        let req = cmx_service_rpc::RpcRequest::get(self.key.clone(), "/dimensions/ancestors")
            .query("dimKey", dim_key)
            .query("dimValue", dim_value);
        let resp = match self.rpc.execute(req).await {
            // 基座把非 2xx 归并为 Remote；404 在此语义为「无层级」= 空链（无继承，非错误；
            // 与 Pg 版平级维度天然跳继承一致）。
            Err(ServiceRpcError::Remote { http_status: 404, .. }) => return Ok(Vec::new()),
            Err(e) => return Err(RouteError::Backend(format!("维度服务调用失败: {e}"))),
            Ok(resp) => resp,
        };
        // 非 JSON（基座置 Null）→ 报错，与原裸 reqwest 行为对齐。
        if resp.body.is_null() {
            return Err(RouteError::Backend(
                "维度服务响应解析失败: 非 JSON".to_string(),
            ));
        }
        let parsed: AncestorsResp = serde_json::from_value(resp.body)
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
