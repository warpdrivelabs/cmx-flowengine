/*
 * @Describe: SubflowRouter —— 子流程路由契约（M5.2 组织路由 → RD0 维度泛化）。
 *
 * 引擎向外的**第四个可注入扩展点**（前三个：JavaDelegate·M1、Clock·M2.5、AssigneeResolver·M4）。
 * 引擎定义 trait，宿主注入实现：生产按**路由维度**（某字典）+ 维度取值解析出具体子流程定义 key，
 * 测试用假实现（固定映射）。引擎因此不依赖任何 DB / 字典 / 组织体系，保持中立可测。
 *
 * 职责单一：给定 callActivity 的「逻辑 key」+「路由维度 dim_key」+「维度取值 dim_value」，解析出
 * **具体子流程定义 key**。解析策略由实现决定（精确绑定 → 沿维度字典树向上继承 → 默认兜底），
 * 引擎只认结果。
 *
 * RD0 泛化：M5.2 的路由维度写死为「组织 id」；现抽象成「任意维度」——dim_key 标识用哪个字典
 * （内建 "org" 映射组织表 cmx_org；其余映射 cf_* 字典），dim_value 是该维度上的取值（条目 id/code）。
 * dim_key 缺省 "org" 时行为与 M5.2 逐字节一致（向后兼容）。
 */

use async_trait::async_trait;

/// 路由错误（中立壳，实现方把自身错误转字符串塞入）。
#[derive(thiserror::Error, Debug)]
pub enum RouteError {
    /// 底层查询错误（DB 等）。
    #[error("子流程路由查询失败: {0}")]
    Backend(String),
    /// 该逻辑 key + 维度找不到任何绑定（含向上继承与默认兜底均无）。
    #[error("子流程路由无解: called_key={called_key} dim={dim_key} value={dim_value:?}")]
    NoBinding {
        /// 逻辑 key。
        called_key: String,
        /// 路由维度 key（如 "org" / "risk_level"）。
        dim_key: String,
        /// 维度取值（None = 无维度上下文）。
        dim_value: Option<String>,
    },
}

/// 路由结果别名。
pub type RouteResult<T> = core::result::Result<T, RouteError>;

/// 内建路由维度：组织机构（映射 cmx_org 表 + path 物化路径）。dim_key 缺省即此。
pub const DIM_ORG: &str = "org";

/// 子流程路由契约。
#[async_trait]
pub trait SubflowRouter: Send + Sync {
    /// 把「逻辑子流程 key + 路由维度 + 维度取值」解析成**具体子流程定义 key**。
    ///
    /// - `called_key`：主流程 callActivity 上写的逻辑名（如 `fin_review`）。
    /// - `dim_key`：路由维度（= 字典 dictCode 或内建 [`DIM_ORG`]；缺省由引擎填 `"org"`）。
    /// - `dim_value`：主实例在该维度上的取值（None = 无维度上下文，实现通常回退默认绑定）。
    ///
    /// 实现约定的解析优先序（PgSubflowRouter）：
    ///   1. 精确：`called_key` + `dim_key` + 本 `dim_value` 的启用绑定；
    ///   2. 继承：该维度字典自分级时，沿其物化路径向上找最近祖先的绑定（最长前缀优先）；
    ///   3. 兜底：`dim_value IS NULL` 的默认绑定。
    /// 全无 → `RouteError::NoBinding`。
    async fn resolve(
        &self,
        called_key: &str,
        dim_key: &str,
        dim_value: Option<&str>,
    ) -> RouteResult<String>;
}
