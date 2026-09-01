//! Kafka / RabbitMQ 通道骨架（feature 门控：`channel-kafka` / `channel-rabbitmq`，001 方案 §4.4）。
//!
//! 对齐 cmx-mdm 分发订阅的骨架模式：配置结构校验可用，deliver 返回明确的「未实现」错误
//! （不产生错误投递）。启用期次（M4+）接入时补全：
//! - 连接凭据（brokers / SASL）走**环境级 toml** `[flow.webhook.channels]`（019 口径：凭据不进业务表），
//!   订阅 channel_config 只存路由目标（kafka：topic + partition_key；rabbitmq：exchange + routing_key）；
//! - kafka 引 rdkafka（需 librdkafka 编译环境）或纯 Rust 客户端，rocketmq 走 5.x gRPC——
//!   选型复用 mdm 方案结论；
//! - 签名适配：三契约头语义照搬、载体为 MQ 消息头（新契约形态，不在 webhook 通道
//!   「wire 零变化」承诺范围）；partition_key = instance_id（同实例事件同分区有序）；
//! - 保序策略可按通道配置：MQ 通道可退化为分区内有序或不保序（避免同订阅串行拖吞吐）。

use serde_json::Value;

use crate::channel::{DeliveryChannel, DeliveryOutcome, DeliveryTask};

/// 通用校验：要求非空字符串键。
fn require_str(config: &Value, key: &str) -> Result<(), String> {
    let v = config.get(key).and_then(Value::as_str).map(str::trim).unwrap_or("");
    if v.is_empty() {
        Err(format!("{key} 不能为空"))
    } else {
        Ok(())
    }
}

/// Kafka 通道骨架（`channel-kafka` feature 启用时进注册表）。
pub struct KafkaChannel;

#[async_trait::async_trait]
impl DeliveryChannel for KafkaChannel {
    fn channel_type(&self) -> &'static str {
        "kafka"
    }

    fn display_name(&self) -> &'static str {
        "Kafka 主题（未启用）"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({
            "topic": { "type": "string", "required": true, "desc": "目标主题（连接凭据走环境级 toml，不进订阅表）" },
            "partition_key": { "type": "string", "required": false, "desc": "分区键（缺省 instance_id，同实例同分区有序）" }
        })
    }

    async fn validate_config(&self, config: &Value) -> Result<(), String> {
        require_str(config, "topic")
    }

    async fn deliver(
        &self,
        _config: &Value,
        _task: &DeliveryTask,
        _timeout: Option<std::time::Duration>,
    ) -> DeliveryOutcome {
        // 骨架：M4+ 启用 channel-kafka feature 并引入客户端后实现（broker ack → Success）。
        DeliveryOutcome::retry("kafka 通道未启用（M4+ 接入 channel-kafka feature）")
    }
}

/// RabbitMQ 通道骨架（`channel-rabbitmq` feature 启用时进注册表）。
pub struct RabbitMqChannel;

#[async_trait::async_trait]
impl DeliveryChannel for RabbitMqChannel {
    fn channel_type(&self) -> &'static str {
        "rabbitmq"
    }

    fn display_name(&self) -> &'static str {
        "RabbitMQ（未启用）"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({
            "exchange": { "type": "string", "required": true, "desc": "目标交换机（连接凭据走环境级 toml）" },
            "routing_key": { "type": "string", "required": false, "desc": "路由键（缺省事件类型）" }
        })
    }

    async fn validate_config(&self, config: &Value) -> Result<(), String> {
        require_str(config, "exchange")
    }

    async fn deliver(
        &self,
        _config: &Value,
        _task: &DeliveryTask,
        _timeout: Option<std::time::Duration>,
    ) -> DeliveryOutcome {
        DeliveryOutcome::retry("rabbitmq 通道未启用（M4+ 接入 channel-rabbitmq feature）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skeletons_validate_route_keys_only() {
        assert!(KafkaChannel.validate_config(&serde_json::json!({ "topic": "t" })).await.is_ok());
        assert!(KafkaChannel.validate_config(&serde_json::json!({})).await.is_err());
        assert!(RabbitMqChannel
            .validate_config(&serde_json::json!({ "exchange": "e" }))
            .await
            .is_ok());
        assert!(RabbitMqChannel.validate_config(&serde_json::json!({})).await.is_err());
    }

    #[tokio::test]
    async fn skeleton_deliver_is_retryable_not_implemented() {
        let task = DeliveryTask {
            subscription_name: "s".into(),
            event_type: "instance.started".into(),
            definition_key: None,
            business_key: None,
            instance_id: "i".into(),
            delivery_id: "d".into(),
            payload: serde_json::json!({}),
        };
        assert!(matches!(
            KafkaChannel.deliver(&serde_json::json!({ "topic": "t" }), &task, None).await,
            DeliveryOutcome::Retryable { .. }
        ));
    }
}
