/*
 * @Describe: cmx-flow-engine 错误定义（thiserror，独立 error 模块）。
 */

use cmx_flow_model::StoreError;

/// 引擎运行错误。
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// 未找到指定 key 的流程定义（未部署）。
    #[error("流程定义未部署: {0}")]
    DefinitionNotFound(String),

    /// 部署的定义用到了 M1 引擎尚不支持的拓扑形态（如网关无出边）。
    #[error("不支持的流程拓扑: {0}")]
    UnsupportedTopology(String),

    /// 指定的令牌不在预期状态（如 complete 一个非等待令牌）。
    #[error("令牌状态非法: {0}")]
    IllegalTokenState(String),

    /// 指定任务不存在或已办结。
    #[error("任务不可办理: {0}")]
    TaskNotActionable(String),

    /// serviceTask 引用的 delegate 未注册。
    #[error("delegate 未注册: {0}")]
    DelegateNotFound(String),

    /// delegate 执行体内部报错。
    #[error("delegate 执行失败: {0}")]
    DelegateFailed(String),

    /// 表达式求值错误（条件边）。
    #[error("条件表达式错误: {0}")]
    Expr(#[from] cmx_flow_model::Error),

    /// 排他网关无满足条件的出边且无 default。
    #[error("排他网关 {gateway} 无可走出边（无满足条件者且无 default）")]
    NoOutgoingFlow {
        /// 网关 bpmn_id。
        gateway: String,
    },

    /// 多实例（会签/或签）配置或运行错误（如集合变量非数组）。
    #[error("多实例错误: {0}")]
    MultiInstance(String),

    /// 候选人解析错误（M4.1：角色/岗位/部门解析失败）。
    #[error("候选人解析错误: {0}")]
    Resolve(#[from] cmx_flow_model::ResolveError),

    /// 子流程路由错误（M5.2：逻辑 key + 组织解析不到具体子流程）。
    #[error("子流程路由错误: {0}")]
    Route(#[from] cmx_flow_model::RouteError),

    /// 任务认领错误（M4.1：认领非候选任务 / 已被认领）。
    #[error("任务认领失败: {0}")]
    ClaimFailed(String),

    /// 推进步数超过安全上限（疑似定义中存在无等待态死循环）。
    #[error("推进步数超过上限 {0}，疑似存在无等待态环路")]
    StepLimitExceeded(usize),

    /// 持久化层错误。
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// 本 crate 统一 Result。
pub type Result<T> = core::result::Result<T, Error>;
