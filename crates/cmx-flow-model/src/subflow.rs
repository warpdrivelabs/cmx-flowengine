/*
 * @Describe: SubflowRouter —— 子流程组织路由契约（M5.2）。
 *
 * 引擎向外的**第四个可注入扩展点**（前三个：JavaDelegate·M1、Clock·M2.5、AssigneeResolver·M4）。
 * 引擎定义 trait，宿主注入实现：生产查 cmx_flow_subflow_binding + 沿 cmx_org.path 向上继承，
 * 测试用假实现（固定映射）。引擎因此不依赖任何 DB / 组织体系，保持中立可测。
 *
 * 职责单一：给定 callActivity 的「逻辑 key」+ 主实例的「组织 id」，解析出**具体子流程定义 key**。
 * 解析策略由实现决定（精确绑定 → 沿组织树向上继承 → 默认兜底），引擎只认结果。
 */

use async_trait::async_trait;

/// 路由错误（中立壳，实现方把自身错误转字符串塞入）。
#[derive(thiserror::Error, Debug)]
pub enum RouteError {
    /// 底层查询错误（DB 等）。
    #[error("子流程路由查询失败: {0}")]
    Backend(String),
    /// 该逻辑 key + 组织找不到任何绑定（含向上继承与默认兜底均无）。
    #[error("子流程路由无解: called_key={called_key} org={org:?}")]
    NoBinding {
        /// 逻辑 key。
        called_key: String,
        /// 组织 id（None = 无组织上下文）。
        org: Option<String>,
    },
}

/// 路由结果别名。
pub type RouteResult<T> = core::result::Result<T, RouteError>;

/// 子流程组织路由契约。
#[async_trait]
pub trait SubflowRouter: Send + Sync {
    /// 把「逻辑子流程 key + 组织 id」解析成**具体子流程定义 key**。
    ///
    /// - `called_key`：主流程 callActivity 上写的逻辑名（如 `fin_review`）。
    /// - `org_id`：主实例所属组织（None = 无组织上下文，实现通常回退默认绑定）。
    ///
    /// 实现约定的解析优先序（PgSubflowRouter）：
    ///   1. 精确：`called_key` + 本组织的启用绑定；
    ///   2. 继承：沿 `cmx_org.path` 向上找最近祖先的绑定（最长前缀优先）；
    ///   3. 兜底：`org_id IS NULL` 的默认绑定。
    /// 全无 → `RouteError::NoBinding`。
    async fn resolve(&self, called_key: &str, org_id: Option<&str>) -> RouteResult<String>;
}
