/*
 * @Describe: cmx-flow-bpmn 错误定义（thiserror，独立 error 模块）。
 */

/// BPMN 编译错误。
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// XML 解析失败（非良构）。
    #[error("BPMN XML 解析失败: {0}")]
    Xml(#[from] roxmltree::Error),

    /// 缺少必需的结构（如没有 process、没有 startEvent）。
    #[error("BPMN 结构缺失: {0}")]
    MissingElement(String),

    /// 引用了不存在的节点（sequenceFlow 的 sourceRef/targetRef 悬空）。
    #[error("BPMN 引用非法: {0}")]
    DanglingReference(String),

    /// 出现了 M1 尚不支持的元素类型。
    #[error("暂不支持的 BPMN 元素: {0}")]
    Unsupported(String),

    /// 编译出的 IR 未通过模型层自检。
    #[error("流程定义校验失败: {0}")]
    Model(#[from] cmx_flow_model::Error),
}

/// 本 crate 统一 Result。
pub type Result<T> = core::result::Result<T, Error>;
