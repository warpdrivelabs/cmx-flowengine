/*
 * @Describe: AssigneeResolver —— 候选人解析契约（M4）。
 *
 * 引擎向外的**第三个可注入扩展点**（前两个：JavaDelegate·M1、Clock·M2.5）。引擎定义
 * trait，宿主注入实现：生产接 cmx-iam（按 role/position/org 查真实用户），测试用假实现
 * （固定映射）。引擎因此不依赖任何 IAM / DB，保持中立可测。
 *
 * 职责单一：把一条 CandidateRef（user/role/position/org 引用）解析成用户 id 集合。
 * 引擎拿到多条引用后各自解析、并集去重，得到该任务的候选用户全集。
 */

use async_trait::async_trait;

use crate::ir::CandidateRef;

/// 候选人解析错误（中立壳，实现方把自身错误转字符串塞入）。
#[derive(thiserror::Error, Debug)]
pub enum ResolveError {
    /// 底层查询错误（DB/IAM 等）。
    #[error("候选人解析失败: {0}")]
    Backend(String),
    /// 引用指向的实体不存在（如角色 code 无对应角色）。
    #[error("候选人引用无效: {0}")]
    InvalidRef(String),
}

/// 解析结果别名。
pub type ResolveResult<T> = core::result::Result<T, ResolveError>;

/// 解析上下文（P0）——关系型候选（部门领导 / 发起人上级 / 发起人本人）解析所需的实例侧信息。
///
/// 非关系型候选（User/Role/Position/Org）不需要它，故 `resolve` 旧签名保留；关系型候选走
/// `resolve_with(ref, ctx)`。默认实现忽略 ctx 委托回 `resolve`，因此**已有实现零改动、零回归**。
#[derive(Debug, Clone, Default)]
pub struct ResolveContext {
    /// 流程发起人 user_id（发起人本人 / 发起人上级策略的锚点）。约定从实例变量 `initiator` 取。
    pub initiator: Option<String>,
    /// 实例所属组织 id（部门领导策略的默认锚点；关系型引用可覆盖为显式 org）。
    pub org_id: Option<String>,
}

impl ResolveContext {
    /// 便捷构造。
    pub fn new(initiator: Option<String>, org_id: Option<String>) -> Self {
        Self { initiator, org_id }
    }
}

/// 候选人解析契约。
#[async_trait]
pub trait AssigneeResolver: Send + Sync {
    /// 把一条候选引用解析成用户 id 列表（**无上下文**版本，向后兼容）。
    ///
    /// - User → 单元素（就是该 id 本身，实现可校验存在性）
    /// - Role → cmx_user_role 反查该角色下的用户
    /// - Position → cmx_user_position 反查该岗位下的用户
    /// - Org → cmx_org 该部门（及子树）下的用户
    ///
    /// 返回空 Vec 表示「该引用当前无对应用户」（不视为错误，由引擎决定如何处理空候选）。
    ///
    /// 关系型引用（OrgLeader/Initiator/InitiatorLeader）在**无上下文**时无法解析，实现可返回空
    /// Vec 或 InvalidRef；正常路径引擎总是走 `resolve_with` 提供上下文。
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>>;

    /// 带上下文解析（P0）。默认实现忽略 ctx 委托回 [`resolve`]——**故已有实现无需改动即可编译**，
    /// 只有需要关系型解析（部门领导 / 发起人上级 / 本人）的实现才覆盖它。
    ///
    /// [`resolve`]: AssigneeResolver::resolve
    async fn resolve_with(
        &self,
        candidate: &CandidateRef,
        _ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        self.resolve(candidate).await
    }

    /// 便捷：解析一组引用并并集去重（默认实现，实现方通常无需覆盖）。
    async fn resolve_all(&self, candidates: &[CandidateRef]) -> ResolveResult<Vec<String>> {
        self.resolve_all_with(candidates, &ResolveContext::default())
            .await
    }

    /// 便捷：带上下文解析一组引用并并集去重（默认实现，逐条走 `resolve_with`）。
    async fn resolve_all_with(
        &self,
        candidates: &[CandidateRef],
        ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for c in candidates {
            for uid in self.resolve_with(c, ctx).await? {
                if seen.insert(uid.clone()) {
                    out.push(uid);
                }
            }
        }
        Ok(out)
    }
}
