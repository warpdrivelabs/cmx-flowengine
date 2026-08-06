/*
 * @Describe: cmx-flow-model 错误定义。遵循项目规范：thiserror 派生，独立 error 模块，
 *            对外统一 `pub type Result<T> = core::result::Result<T, Error>`。
 */

/// 流程模型层错误。
///
/// 覆盖 IR 编译校验、表达式求值、变量类型转换三类。运行时/持久化的错误在
/// 各自 crate 定义，通过 `#[from]` 或字符串桥接。
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// IR 结构非法（如引用了不存在的节点、缺少 start event）。
    #[error("流程定义非法: {0}")]
    InvalidDefinition(String),

    /// 表达式语法错误。
    #[error("表达式语法错误: {0}")]
    ExprSyntax(String),

    /// 表达式求值错误（如类型不匹配、变量缺失且无默认）。
    #[error("表达式求值失败: {0}")]
    ExprEval(String),

    /// 变量类型转换失败。
    #[error("变量类型转换失败: 期望 {expected}, 实际 {actual}")]
    VarType {
        /// 期望类型名。
        expected: String,
        /// 实际类型名。
        actual: String,
    },

    /// 序列化 / 反序列化失败。
    #[error("序列化失败: {0}")]
    Serde(#[from] serde_json::Error),
}

/// 本 crate 统一 Result 别名。
pub type Result<T> = core::result::Result<T, Error>;
