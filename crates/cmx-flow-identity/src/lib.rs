//! cmx-flow-identity —— 可选内建身份模块（方案决策①「身份双模」的内建那一半，P0-b）。
//!
//! **何时启用**：流程微服务独立部署且**无外部 IAM** 可接时。它自建 `fid_*` 命名空间表
//! （组织/角色/岗位/用户 + 用户-角色/用户-岗位），提供：
//!   - [`LocalAssigneeResolver`]：实现引擎 `AssigneeResolver`（同 Pg/Http 版契约，含 P0 关系型
//!     部门领导/发起人上级/本人，用 `fid_org.leader_user_id`）；
//!   - [`IdentityStore`]：`fid_*` 主数据 CRUD（供身份管理工作台四区落库）。
//!
//! **守约**：表名 `fid_` 前缀，绝不叫 `cmx_user`/`cmx_org`——是流程微服务自持的**可选插件**，
//! 不冒充平台 IAM、不违背「流程库永不建 user/role 表」判据（它是独立模块，非引擎核）。
//! external 模式下 app 不注入本 crate 的任何东西，行为**零回归**。

pub mod ddl;
pub mod error;
pub mod resolver;
pub mod store;

pub use error::{IdentityError, IdentityResult};
pub use resolver::LocalAssigneeResolver;
pub use store::{Entity, IdentityStore};
