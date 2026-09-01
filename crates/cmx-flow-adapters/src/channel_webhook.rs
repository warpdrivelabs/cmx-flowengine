//! Webhook 通道实现（001 方案 §4.3）：复用 [`crate::webhook`] 的自包含契约——
//! 三契约头 + `sign_body` HMAC-SHA256 + HTTP 2xx 成功判定，原样组装，**wire 契约零变化**；
//! 差异仅在 secret 来源（订阅 `channel_config.secret`，替代 env 全局密钥）与
//! 结果分类（408/429/5xx/超时/传输错误可重试，其余 4xx 直达 DEAD）。

use std::time::Duration;

use serde_json::Value;

use crate::channel::{DeliveryChannel, DeliveryOutcome, DeliveryTask};
use crate::webhook::{DELIVERY_HEADER, EVENT_HEADER, SIGNATURE_HEADER, sign_body};
use cmx_service_rpc::{RpcRequest, ServiceRpcError};

/// webhook 通道（无状态）。
pub struct WebhookChannel;

/// 从 channel_config 取非空字符串键。
fn cfg_str<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// 截断到指定字节上限（防御 last_response_snippet 列宽）。
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }
}

impl WebhookChannel {
    /// channel_config 键名常量（schema 与校验共用）。
    pub const SERVICE_KEY: &'static str = "service_key";
    pub const CALLBACK_PATH: &'static str = "callback_path";
    pub const SECRET: &'static str = "secret";

    /// 缺省回调路径（兼容现状：mdm 的 flow 回调端点）。
    pub const DEFAULT_CALLBACK_PATH: &'static str = "/api/mdm/flow/callback";

    /// 结果分类：408/429/5xx / 超时 / 传输错误 → 可重试；其余 4xx（含 401/403）→ 直达 DEAD。
    fn classify(err: ServiceRpcError) -> DeliveryOutcome {
        match err {
            ServiceRpcError::Remote { http_status, msg, .. } => {
                let retryable = http_status == 408
                    || http_status == 429
                    || (500..600).contains(&http_status);
                let snippet = truncate(&msg, 512);
                if retryable {
                    DeliveryOutcome::Retryable {
                        http_status: Some(http_status),
                        error: format!("HTTP {http_status}: {msg}"),
                        snippet: Some(snippet),
                    }
                } else {
                    DeliveryOutcome::Fatal {
                        http_status: Some(http_status),
                        error: format!("HTTP {http_status}（非重试类 4xx）: {msg}"),
                        snippet: Some(snippet),
                    }
                }
            }
            // 401/403：密钥/目录配置性错误，重试不可愈。
            ServiceRpcError::AuthRejected { cause, .. } => DeliveryOutcome::Fatal {
                http_status: None,
                error: format!("鉴权被拒: {cause}"),
                snippet: Some(truncate(&cause, 512)),
            },
            ServiceRpcError::Timeout { timeout_ms, .. } => {
                DeliveryOutcome::retry(format!("调用超时（{timeout_ms}ms）"))
            }
            // 网络不可达 / 熔断 / 键缺失：传输级，可重试。
            other => DeliveryOutcome::retry(other.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl DeliveryChannel for WebhookChannel {
    fn channel_type(&self) -> &'static str {
        "webhook"
    }

    fn display_name(&self) -> &'static str {
        "Webhook 回调"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({
            "service_key": {
                "type": "string", "required": true,
                "desc": "目标服务目录键（[service_rpc.services] 登记；内部走注册发现，外部登记静态 url）"
            },
            "callback_path": {
                "type": "string", "required": false,
                "desc": "接收方回调路径（以 / 开头）", "default": Self::DEFAULT_CALLBACK_PATH
            },
            "secret": {
                "type": "string", "required": true, "writeOnly": true,
                "desc": "HMAC-SHA256 共享密钥（每订阅独立；API 掩码回显，编辑留空沿用旧值）"
            }
        })
    }

    async fn validate_config(&self, config: &Value) -> Result<(), String> {
        let service_key = cfg_str(config, Self::SERVICE_KEY).unwrap_or("");
        if service_key.is_empty() {
            return Err("webhook 通道缺 service_key（目标服务目录键）".into());
        }
        if let Some(path) = cfg_str(config, Self::CALLBACK_PATH)
            && !path.starts_with('/')
        {
            return Err("callback_path 须以 / 开头".into());
        }
        if cfg_str(config, Self::SECRET).is_none() {
            return Err("webhook 通道缺 secret（HMAC-SHA256 共享密钥）".into());
        }
        Ok(())
    }

    async fn deliver(
        &self,
        config: &Value,
        task: &DeliveryTask,
        timeout: Option<Duration>,
    ) -> DeliveryOutcome {
        let Some(rpc) = cmx_service_rpc::global_arc() else {
            return DeliveryOutcome::retry("service_rpc 基座未初始化");
        };
        let Some(service_key) = cfg_str(config, Self::SERVICE_KEY) else {
            return DeliveryOutcome::fatal("channel_config 缺 service_key");
        };
        let path = cfg_str(config, Self::CALLBACK_PATH).unwrap_or(Self::DEFAULT_CALLBACK_PATH);
        let secret = cfg_str(config, Self::SECRET).unwrap_or("");

        // body 用紧凑 JSON；签名对实际发送字节（Raw body 保证），与 legacy 链路一致。
        let body = match serde_json::to_vec(&task.payload) {
            Ok(b) => b,
            Err(e) => return DeliveryOutcome::fatal(format!("事件序列化失败: {e}")),
        };
        let mut req = RpcRequest::post(service_key, path)
            .raw_body(body.clone(), "application/json")
            .header(EVENT_HEADER, task.event_type.clone())
            .header(DELIVERY_HEADER, task.delivery_id.clone())
            .header(SIGNATURE_HEADER, sign_body(secret, &body));
        if let Some(d) = timeout {
            req = req.timeout(d);
        }
        match rpc.execute(req).await {
            // execute 已做 2xx 判定：Ok = HTTP 2xx，响应体不解析（接收方不受 CMX 信封约束）。
            Ok(_) => DeliveryOutcome::Success,
            Err(e) => Self::classify(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn validate_config_requires_keys() {
        let ch = WebhookChannel;
        // 缺 service_key / secret 均拒。
        assert!(ch.validate_config(&serde_json::json!({})).await.is_err());
        assert!(ch
            .validate_config(&serde_json::json!({ "service_key": "mdm" }))
            .await
            .is_err());
        // 齐备即过；callback_path 缺省合法。
        assert!(ch
            .validate_config(&serde_json::json!({
                "service_key": "mdm", "secret": "s3cret"
            }))
            .await
            .is_ok());
        // callback_path 不以 / 开头拒绝；额外键不拒（开放对象 forward-compat）。
        assert!(ch
            .validate_config(&serde_json::json!({
                "service_key": "mdm", "secret": "s", "callback_path": "api/x"
            }))
            .await
            .is_err());
        assert!(ch
            .validate_config(&serde_json::json!({
                "service_key": "mdm", "secret": "s", "future_key": 1
            }))
            .await
            .is_ok());
    }

    #[test]
    fn classify_maps_status_bands() {
        let retry = |s: u16| matches!(
            WebhookChannel::classify(ServiceRpcError::Remote {
                key: "k".into(),
                http_status: s,
                code: -1,
                msg: "x".into(),
            }),
            DeliveryOutcome::Retryable { .. }
        );
        let fatal = |s: u16| matches!(
            WebhookChannel::classify(ServiceRpcError::Remote {
                key: "k".into(),
                http_status: s,
                code: -1,
                msg: "x".into(),
            }),
            DeliveryOutcome::Fatal { .. }
        );
        // 408/429/5xx 可重试；其余 4xx（400/401/403/404/422）直达 DEAD。
        for s in [408, 429, 500, 502, 503, 504] {
            assert!(retry(s), "{s} 应可重试");
        }
        for s in [400, 401, 403, 404, 422] {
            assert!(fatal(s), "{s} 应直达 DEAD");
        }
        // 超时 / 传输不可达 → 可重试；鉴权拒 → DEAD。
        assert!(matches!(
            WebhookChannel::classify(ServiceRpcError::Timeout { key: "k".into(), timeout_ms: 1 }),
            DeliveryOutcome::Retryable { .. }
        ));
        assert!(matches!(
            WebhookChannel::classify(ServiceRpcError::Unavailable {
                key: "k".into(),
                cause: "down".into(),
            }),
            DeliveryOutcome::Retryable { .. }
        ));
        assert!(matches!(
            WebhookChannel::classify(ServiceRpcError::AuthRejected {
                key: "k".into(),
                cause: "bad key".into(),
            }),
            DeliveryOutcome::Fatal { .. }
        ));
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("abcdef", 3), "abc");
        let cn = "流程引擎投递失败";
        let t = truncate(cn, 7);
        assert!(cn.starts_with(&t));
        assert!(t.len() <= 7);
    }
}
