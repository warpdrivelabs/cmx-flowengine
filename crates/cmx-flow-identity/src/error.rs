/*
 * @Describe: cmx-flow-identity 错误定义（thiserror，对齐项目规范）。
 */

/// 内建身份模块错误。
#[derive(thiserror::Error, Debug)]
pub enum IdentityError {
    /// 底层 DB 错误。
    #[error("身份存储错误: {0}")]
    Backend(String),
    /// 未知实体类型（URL 段解析失败）。
    #[error("未知身份实体: {0}")]
    UnknownEntity(String),
}

/// 本 crate 统一 Result 别名。
pub type IdentityResult<T> = core::result::Result<T, IdentityError>;
