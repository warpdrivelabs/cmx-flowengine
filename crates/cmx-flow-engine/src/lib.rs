/*
 * @Describe: cmx-flow-engine —— 令牌执行内核。
 *
 * 对外导出：
 * - Engine<S: RuntimeStore>：deploy / start_process / complete_task / register_delegate
 * - JavaDelegate / DelegateContext / DelegateRegistry：serviceTask 委托执行
 * - InMemoryStore：进程内 RuntimeStore（测试/嵌入）
 * - ExecutionResult / TaskView：对外只读结果视图
 *
 * 不变量：令牌推进到 userTask 等等待态即停止并落库——「等待态即提交点」。引擎泛型于
 * RuntimeStore，与持久化实现解耦。
 */

pub mod clock;
pub mod delegate;
pub mod engine;
pub mod error;
pub mod memory_store;

pub use clock::{Clock, SystemClock, TestClock};
pub use delegate::{DelegateContext, DelegateRegistry, JavaDelegate};
pub use engine::{Engine, ExecutionResult, FiredTimer, TaskView};
pub use error::{Error, Result};
pub use memory_store::InMemoryStore;

// 便于消费方一处引入运行态/模型类型。
pub use cmx_flow_model::{
    AssigneeResolver, CandidateKind, CandidateRef, CcRecord, CcSummary, InstanceSnapshot,
    InstanceState, ProcessDefinition, ProcessInstance, ResolveError, ResolveResult, RouteError,
    RouteResult, RuntimeStore, SubflowRouter, Task, TaskCandidate, TaskDelegation, Token,
    TokenState, Variables,
};
