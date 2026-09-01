//! 投递通道抽象（001 方案 §4.2，对齐 cmx-mdm `DistributionChannel` + `ChannelRegistry`）。
//!
//! 投递框架（队列 / poller / 死信 / 管理页）与「事件怎么送到目标」解耦：
//! v1 只注册 [`WebhookChannel`](crate::channel_webhook::WebhookChannel)；
//! kafka / rabbitmq 为 feature 门控骨架（[`channel_mq`]），启用 = 新 feature + 新实现 +
//! 注册表登记 + save 放开值域，**框架零改动**——这是通道抽象的验收标准。
//!
//! 注册表是基础设施（启动期代码装配、运行期只读），进程内单例，非业务数据缓存。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use serde_json::Value;

/// 一次投递任务（poller 从投递行投影；通道只认这个，不感知 DB 行结构）。
#[derive(Debug, Clone)]
pub struct DeliveryTask {
    /// 订阅名快照（日志/诊断）。
    pub subscription_name: String,
    /// 事件类型（instance.started 等 / webhook.test 伪事件）。
    pub event_type: String,
    /// 流程定义 key（可空）。
    pub definition_key: Option<String>,
    /// 业务键（可空）。
    pub business_key: Option<String>,
    /// 实例 id。
    pub instance_id: String,
    /// wire 幂等参考键（x-cmx-flow-delivery 头；接收方按此或业务键幂等）。
    pub delivery_id: String,
    /// 完整事件体（webhook 通道序列化为 body 并签名；MQ 通道作为消息负载）。
    pub payload: Value,
}

/// 单次投递结果分类（001 方案 §4.2 结果分类）。
#[derive(Debug, Clone)]
pub enum DeliveryOutcome {
    /// HTTP 2xx（webhook）/ broker ack（未来 MQ）——投递成功。
    Success,
    /// 可重试失败：408/429/5xx / 超时 / 传输错误——进指数退避重试。
    Retryable {
        /// HTTP 状态码（传输层错误无）。
        http_status: Option<u16>,
        /// 失败原因（人读，进 last_error）。
        error: String,
        /// 响应/错误摘要（截断由调用方落库时处理）。
        snippet: Option<String>,
    },
    /// 不可重试失败：其余 4xx（含 401/403）——契约/配置性错误，重试不可愈，直达 DEAD。
    Fatal {
        /// HTTP 状态码。
        http_status: Option<u16>,
        /// 失败原因。
        error: String,
        /// 响应/错误摘要。
        snippet: Option<String>,
    },
}

impl DeliveryOutcome {
    /// 便捷构造：可重试失败（无摘要）。
    pub fn retry(error: impl Into<String>) -> Self {
        Self::Retryable { http_status: None, error: error.into(), snippet: None }
    }
    /// 便捷构造：不可重试失败（无摘要）。
    pub fn fatal(error: impl Into<String>) -> Self {
        Self::Fatal { http_status: None, error: error.into(), snippet: None }
    }
}

/// 投递通道 trait：一种目标形态（webhook / kafka / …）一个实现。
#[async_trait::async_trait]
pub trait DeliveryChannel: Send + Sync {
    /// 通道类型标识（对应订阅表 channel 列，如 "webhook"）。
    fn channel_type(&self) -> &'static str;

    /// 展示名（channels 端点 / 管理页通道下拉）。
    fn display_name(&self) -> &'static str;

    /// channel_config 的结构说明（channels 端点返回；管理页按通道动态渲染表单的依据）。
    fn config_schema(&self) -> Value;

    /// 校验订阅的 channel_config（save 端点前置调用；必填键缺失/类型错误返回可读错误）。
    async fn validate_config(&self, config: &Value) -> Result<(), String>;

    /// 投递一条事件。
    ///
    /// `timeout` 为调用方指定的单次超时覆盖（None = 基座键级超时；test 端点传短超时 10s）。
    async fn deliver(
        &self,
        config: &Value,
        task: &DeliveryTask,
        timeout: Option<Duration>,
    ) -> DeliveryOutcome;
}

/// 通道注册表：channel_type → 实现（启动期装配、运行期只读；同 type 后注册覆盖）。
pub struct ChannelRegistry {
    channels: RwLock<HashMap<&'static str, std::sync::Arc<dyn DeliveryChannel>>>,
}

impl ChannelRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self { channels: RwLock::new(HashMap::new()) }
    }

    /// 登记通道实现（同 type 后注册覆盖）。
    pub fn register(&self, channel: std::sync::Arc<dyn DeliveryChannel>) {
        self.channels
            .write()
            .expect("通道注册表锁中毒")
            .insert(channel.channel_type(), channel);
    }

    /// 按类型取通道；未登记（含 feature 未启用）返回 None。
    pub fn get(&self, channel_type: &str) -> Option<std::sync::Arc<dyn DeliveryChannel>> {
        self.channels.read().expect("通道注册表锁中毒").get(channel_type).cloned()
    }

    /// 全部已注册通道的类型（排序；管理页通道下拉数据源）。
    pub fn types(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> =
            self.channels.read().expect("通道注册表锁中毒").keys().copied().collect();
        v.sort_unstable();
        v
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局注册表单例（首次访问时装入默认通道集：webhook 恒注册；kafka/rabbitmq 随 feature）。
pub fn global_registry() -> &'static ChannelRegistry {
    static REG: std::sync::OnceLock<ChannelRegistry> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        let reg = ChannelRegistry::new();
        reg.register(std::sync::Arc::new(crate::channel_webhook::WebhookChannel));
        #[cfg(feature = "channel-kafka")]
        reg.register(std::sync::Arc::new(crate::channel_mq::KafkaChannel));
        #[cfg(feature = "channel-rabbitmq")]
        reg.register(std::sync::Arc::new(crate::channel_mq::RabbitMqChannel));
        reg
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;

    #[async_trait::async_trait]
    impl DeliveryChannel for Stub {
        fn channel_type(&self) -> &'static str {
            "stub"
        }
        fn display_name(&self) -> &'static str {
            "桩通道"
        }
        fn config_schema(&self) -> Value {
            serde_json::json!({})
        }
        async fn validate_config(&self, _config: &Value) -> Result<(), String> {
            Ok(())
        }
        async fn deliver(
            &self,
            _config: &Value,
            _task: &DeliveryTask,
            _timeout: Option<Duration>,
        ) -> DeliveryOutcome {
            DeliveryOutcome::Success
        }
    }

    #[test]
    fn register_lookup_types() {
        let reg = ChannelRegistry::new();
        assert!(reg.get("stub").is_none());
        assert!(reg.types().is_empty());
        reg.register(std::sync::Arc::new(Stub));
        assert!(reg.get("stub").is_some());
        assert_eq!(reg.types(), vec!["stub"]);
    }
}
