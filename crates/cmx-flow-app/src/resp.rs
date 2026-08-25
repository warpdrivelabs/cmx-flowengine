//! 平台中立的响应信封 + 错误类型（字节对齐 cmx-api-types，故抽核后平台输出零变化）。
//!
//! 抽核前 handler 返回 `cmx_api::Result<Json<cmx_api::ApiResp<Value>>>`，其中
//! `ApiResp`/`Result`/`Error` 是 cmx-api-types 的再导出。本 crate 不依赖 cmx-api-types
//! （它拖 cmx-database/utoipa/validator），改为在此自持等价定义：
//!
//! - `ApiResp<T>`：`{code,msg,data}` camelCase，`data` 为 None 时不序列化——与
//!   cmx-api-types::ApiResp 序列化结果逐字节一致（省略 pagination 字段，本模块 handler 从不用它）。
//! - `FlowError`：对齐 cmx-api-types::Error 的 IntoResponse——BusinessError → HTTP 200 +
//!   `{code:1,msg}`；NotFound → 404；其余 → 500。本 crate 的 handler 只产 business/not_found 两类。
//! - `Result<T> = core::result::Result<T, FlowError>`。
//!
//! 这样 `Ok(Json(ApiResp::ok(x)))` 与 `Err(FlowError::business(m))` 在两壳（平台 cmx-flow-api /
//! 独立 cmx-flow-server）里表现完全一致，且都不碰 cmx-api。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;

/// 统一响应信封。字段/命名/省略规则对齐 cmx-api-types::ApiResp（去掉本模块用不到的 pagination）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResp<T> {
    /// 业务码：0 = 成功，非 0 = 业务错误码。
    pub code: u16,
    /// 消息：成功为 "success"，失败为错误描述。
    pub msg: String,
    /// 业务数据；None 时不出现在 JSON 里（与 cmx-api-types 一致）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T> ApiResp<T> {
    /// 成功响应：code=0 / msg="success" / data=Some(data)。
    pub fn ok(data: T) -> Self {
        Self {
            code: 0,
            msg: "success".to_string(),
            data: Some(data),
        }
    }
}

/// 平台中立错误。对齐 cmx-api-types::Error 中本模块实际会产生的两类 + 兜底。
#[derive(Debug, Clone)]
pub enum FlowError {
    /// 业务错误（HTTP 200 + code=1）——对齐 cmx-api-types 的 BusinessError。
    Business(String),
    /// 资源不存在（HTTP 404 + code=4）。
    NotFound(String),
    /// 兜底内部错误（HTTP 500 + code=5）。
    Internal(String),
}

impl FlowError {
    /// 业务错误（同 cmx-api-types 的 business_error → BizError 桥语义）。
    pub fn business(msg: impl Into<String>) -> Self {
        Self::Business(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    fn code(&self) -> u16 {
        match self {
            // 对齐 cmx-api-types::ErrCode：BusinessError=1 / NotFound=4 / InternalError=5。
            Self::Business(_) => 1,
            Self::NotFound(_) => 4,
            Self::Internal(_) => 5,
        }
    }
    fn status(&self) -> StatusCode {
        match self {
            // 对齐 cmx-api-types::Error::status_code：BusinessError → 200。
            Self::Business(_) => StatusCode::OK,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
    fn message(&self) -> &str {
        match self {
            Self::Business(m) | Self::NotFound(m) | Self::Internal(m) => m,
        }
    }
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}
impl std::error::Error for FlowError {}

impl IntoResponse for FlowError {
    fn into_response(self) -> Response {
        tracing::error!("{:<12} - FlowError {self:?}", "ERROR");
        let status = self.status();
        // 错误体 {code,msg}——与 cmx-api-types::Error::to_response_body 一致（无 data 键）。
        let body = Json(json!({ "code": self.code(), "msg": self.message() }));
        (status, body).into_response()
    }
}

/// 页面投递内部错误 → 自持信封（保持历史错误体字节：BadRequest→business code=1，
/// NotFound→404 code=4，与原 frontend_pages.rs 语义一致）。
impl From<cmx_form::serve::PageServeError> for FlowError {
    fn from(e: cmx_form::serve::PageServeError) -> Self {
        match e {
            cmx_form::serve::PageServeError::BadRequest(m) => Self::business(m),
            cmx_form::serve::PageServeError::NotFound(m) => Self::not_found(m),
            // F3-save 写路径落盘失败：按业务错误透出（code=1）
            cmx_form::serve::PageServeError::Io(m) => Self::business(m),
        }
    }
}

/// handler 结果别名（对齐抽核前 `cmx_api::Result`）。
pub type Result<T> = core::result::Result<T, FlowError>;
