//! 适配器选择配置：mode(mock|http|pg) + 目标服务键，从环境变量读。
//!
//! 一份代码三部署姿态（方案 §9）：
//!   - `mock`：脱一切外部单跑（开发/演示/CI）——默认，最安全。
//!   - `http`：接外部身份/组织/单据服务（纯独立微服务姿态）。
//!   - `pg`  ：回连平台库（平台内嵌姿态，用 cmx-flow-store-pg 的 Pg* 实现，本 crate 不含）。
//!
//! 选择逻辑本身在 `cmx-flow-app::engine`（它同时能看到 pg 实现）；本模块只负责「从环境读出
//! 每个适配器该用哪种 mode + 对应目标**服务键**」。地址不在此处——`http` 形态的目标是
//! `[service_rpc.services]` 目录键（无注册中心时目录里登记静态 url 直连，语义与 webhook 的
//! `FLOW_WEBHOOK_TARGETS` 一致），传输/鉴权/超时/重试/熔断由 cmx-service-rpc 基座统一承载。

/// 适配器模式。三姿态由此区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterMode {
    /// 脱外部内建实现（默认）。
    Mock,
    /// 接外部 HTTP 服务。
    Http,
    /// 回连平台库（Pg* 实现，在 cmx-flow-store-pg）。
    Pg,
}

impl AdapterMode {
    /// 从字符串解析（大小写不敏感）；未知/空 → 默认 `Mock`。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Self::Http,
            "pg" => Self::Pg,
            _ => Self::Mock,
        }
    }

    /// 从环境变量读；缺省 → `default`。
    pub fn from_env(var: &str, default: AdapterMode) -> Self {
        match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => Self::parse(&v),
            _ => default,
        }
    }
}

/// 三适配器的选择配置。
///
/// 环境变量（缺省见括号）：
///   - `FLOW_IDENTITY_MODE`(pg) / `FLOW_IDENTITY_TARGET`（服务目录键）
///   - `FLOW_SUBFLOW_MODE`(pg)  / `FLOW_SUBFLOW_TARGET`（服务目录键）
///   - `FLOW_DELEGATE_MODE`(pg) / `FLOW_DELEGATE_TARGET`（服务目录键）
///
/// **默认全 pg**：与抽核前 engine.rs 写死的三注入等价 → 平台内嵌与既有测试零回归。
/// 独立微服务显式设 `=http`（配 TARGET，指向 `[service_rpc.services]` 键）或 `=mock`（脱外部单跑）。
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub identity_mode: AdapterMode,
    pub identity_target: Option<String>,
    pub subflow_mode: AdapterMode,
    pub subflow_target: Option<String>,
    pub delegate_mode: AdapterMode,
    pub delegate_target: Option<String>,
    /// 出站 webhook 配置（生命周期事件通知第三方）。
    pub webhook: WebhookConfig,
}

/// 一个 webhook 投递目标：服务目录键 + 接收方回调路径。
///
/// 回调面向**外部/三方系统**（mdm 只是订阅者之一），各系统回调路径不同，故路径随目标
/// 配置（不走共享 SDK 契约——外部系统不会有本仓 crate，契约以文档为真源，
/// 见 `docs/usage/08-external-integration.md` §8.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookTarget {
    /// 服务目录键（`[service_rpc.services]` 定位传输地址）。
    pub key: String,
    /// 接收方回调路径（以 `/` 开头，如 `/api/mdm/flow/callback`）。
    pub path: String,
}

impl WebhookTarget {
    /// 解析一条目 `键:路径`；键空 / 缺路径 / 路径不以 `/` 开头 → `None`（装配点 warn 跳过）。
    fn parse(item: &str) -> Option<Self> {
        let (key, path) = item.split_once(':')?;
        let (key, path) = (key.trim(), path.trim());
        if key.is_empty() || !path.starts_with('/') {
            return None;
        }
        Some(Self {
            key: key.to_string(),
            path: path.to_string(),
        })
    }
}

/// 出站 webhook 选择配置。
///
/// 环境变量：
///   - `FLOW_WEBHOOK_TARGETS`：逗号分隔的目标条目列表，每条 `服务键:回调路径`（如
///     `mdm:/api/mdm/flow/callback`）——键经 `[service_rpc.services]` 定位，路径归接收方
///     定义。格式不合法的条目 warn 跳过；列表空 = 关闭 webhook）。
///   - `FLOW_WEBHOOK_SIGNING_KEY`：HMAC-SHA256 签名密钥（首启 env 导入行的过渡共享密钥；
///     独立 secret 由订阅 `channel_config` 承载——001 订阅入库）。
#[derive(Debug, Clone, Default)]
pub struct WebhookConfig {
    pub targets: Vec<WebhookTarget>,
    pub signing_key: Option<String>,
}

impl WebhookConfig {
    /// 从环境变量装配。targets 空即视为关闭。
    pub fn from_env() -> Self {
        let targets = env_opt("FLOW_WEBHOOK_TARGETS")
            .map(|s| {
                s.split(',')
                    .filter_map(|item| match WebhookTarget::parse(item) {
                        Some(t) => Some(t),
                        None => {
                            tracing::warn!(
                                item,
                                "FLOW_WEBHOOK_TARGETS 条目格式应为 键:/路径（如 mdm:/api/mdm/flow/callback），已跳过"
                            );
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            targets,
            signing_key: env_opt("FLOW_WEBHOOK_SIGNING_KEY"),
        }
    }
}

impl AdapterConfig {
    /// 从环境变量装配。默认全 `Pg`（零回归）。
    pub fn from_env() -> Self {
        Self {
            identity_mode: AdapterMode::from_env("FLOW_IDENTITY_MODE", AdapterMode::Pg),
            identity_target: env_opt("FLOW_IDENTITY_TARGET"),
            subflow_mode: AdapterMode::from_env("FLOW_SUBFLOW_MODE", AdapterMode::Pg),
            subflow_target: env_opt("FLOW_SUBFLOW_TARGET"),
            delegate_mode: AdapterMode::from_env("FLOW_DELEGATE_MODE", AdapterMode::Pg),
            delegate_target: env_opt("FLOW_DELEGATE_TARGET"),
            webhook: WebhookConfig::from_env(),
        }
    }
}

/// 读一个非空环境变量为 `Option<String>`。
fn env_opt(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes() {
        assert_eq!(AdapterMode::parse("http"), AdapterMode::Http);
        assert_eq!(AdapterMode::parse("HTTP"), AdapterMode::Http);
        assert_eq!(AdapterMode::parse("pg"), AdapterMode::Pg);
        assert_eq!(AdapterMode::parse("mock"), AdapterMode::Mock);
        assert_eq!(AdapterMode::parse("garbage"), AdapterMode::Mock);
        assert_eq!(AdapterMode::parse(""), AdapterMode::Mock);
    }

    #[test]
    fn parse_webhook_targets() {
        let ok = WebhookTarget::parse("mdm:/api/mdm/flow/callback").unwrap();
        assert_eq!(ok.key, "mdm");
        assert_eq!(ok.path, "/api/mdm/flow/callback");
        // 键/路径两侧空白容忍。
        let padded = WebhookTarget::parse(" mdm : /api/x ").unwrap();
        assert_eq!(padded.key, "mdm");
        assert_eq!(padded.path, "/api/x");
        // 旧式纯键 / 缺路径 / 路径不以 / 开头 / 空键 → 拒绝。
        assert!(WebhookTarget::parse("mdm").is_none());
        assert!(WebhookTarget::parse("mdm:").is_none());
        assert!(WebhookTarget::parse("mdm:api/x").is_none());
        assert!(WebhookTarget::parse(":/api/x").is_none());
    }
}
