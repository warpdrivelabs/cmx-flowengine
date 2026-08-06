//! cmx-flow-tests —— 流程引擎 M1 端到端测试载体。
//!
//! 本 lib 仅承载共享的样例流程与测试工具；实际用例在 `tests/` 集成测试目录。

/// M1 样例流程：请假审批。
///
/// 拓扑：start → 经理审批(userTask) → 排他网关(金额判断)
///   ├─ amount > 1000 → 总监审批(userTask) → 结束
///   └─ 否则(default)  → 结束
///
/// 覆盖 M1 全部要素：start/end event、userTask 等待态、serviceTask（计算日数）、
/// exclusiveGateway + 条件边、变量驱动分支。
pub const LEAVE_REQUEST_BPMN: &str = include_str!("../resources/leave_request.bpmn20.xml");
