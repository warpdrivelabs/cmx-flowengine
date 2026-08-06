/*
 * @Describe: cmx-flow-store-pg —— 流程引擎的 PostgreSQL 持久化实现。
 *
 * 对外导出 PgRuntimeStore（实现 cmx-flow-model::RuntimeStore）与内置幂等 DDL。接入
 * cmx-database-pg 的全局 manager；save_snapshot 单事务重写实例聚合，一次提交对应一个
 * BPMN 等待态提交点。
 */

pub mod ddl;
pub mod mapping;
pub mod resolver;
pub mod store;
pub mod subflow_router;

pub use ddl::DDL_STATEMENTS;
pub use resolver::PgIamAssigneeResolver;
pub use store::PgRuntimeStore;
pub use subflow_router::{OrgNode, PgSubflowBindingStore, PgSubflowRouter, SubflowBinding};
