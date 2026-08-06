/*
 * @Describe: serviceTask 的 delegate（委托执行体）契约与注册表。
 *
 * 对标 Flowable 的 JavaDelegate：serviceTask 执行时调用一段用户代码。Java 靠类加载 +
 * EL 反射定位；Rust 侧改为**显式注册**——用户实现 `JavaDelegate` trait（名字致敬），
 * 按 delegate 键注册进 `DelegateRegistry`，引擎按键查找调用。无反射、无沙箱、类型安全。
 *
 * delegate 拿到 `DelegateContext`（只读实例上下文 + 可变变量），执行副作用（调用外部
 * 服务、算值），把结果写回变量。M1 同步执行、不落等待态。
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cmx_flow_model::Variables;

/// delegate 执行上下文。
///
/// M1 提供实例/节点标识与可变变量。日后可扩展注入 store、当前 token 等。
pub struct DelegateContext<'a> {
    /// 流程实例 id。
    pub instance_id: &'a str,
    /// 当前 serviceTask 的 bpmn_id。
    pub node_bpmn_id: &'a str,
    /// 实例变量（可读可写；delegate 的产出写回这里）。
    pub variables: &'a mut Variables,
}

/// 委托执行体契约（名字致敬 Flowable 的 JavaDelegate）。
#[async_trait]
pub trait JavaDelegate: Send + Sync {
    /// 执行业务逻辑。返回 Err 会中断当前运行段并向上抛（M1 不做补偿/重试）。
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String>;
}

/// delegate 注册表：delegate 键 → 执行体。
#[derive(Clone, Default)]
pub struct DelegateRegistry {
    map: HashMap<String, Arc<dyn JavaDelegate>>,
}

impl DelegateRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 delegate（同键覆盖）。
    pub fn register(&mut self, key: impl Into<String>, delegate: Arc<dyn JavaDelegate>) {
        self.map.insert(key.into(), delegate);
    }

    /// 便捷注册：直接从实现了 JavaDelegate 的具体类型注册。
    pub fn register_delegate<D: JavaDelegate + 'static>(
        &mut self,
        key: impl Into<String>,
        delegate: D,
    ) {
        self.register(key, Arc::new(delegate));
    }

    /// 查找 delegate。
    pub fn get(&self, key: &str) -> Option<Arc<dyn JavaDelegate>> {
        self.map.get(key).cloned()
    }
}
