//! 请求级租户上下文——已收编至 `cmx-engine-kit::tenant`（唯一真源）。
//!
//! 本模块保留为 re-export shim：handlers / main 既有 `crate::tenant::*` 引用零改动。
//! 真源含完整行为契约文档、nickname 展示名管道与单测，见
//! `../cmx-container/crates/libs/cmx-engine-kit/src/tenant.rs`。

pub use cmx_engine_kit::tenant::*;
