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

/// 候选人解析契约。
#[async_trait]
pub trait AssigneeResolver: Send + Sync {
    /// 把一条候选引用解析成用户 id 列表。
    ///
    /// - User → 单元素（就是该 id 本身，实现可校验存在性）
    /// - Role → cmx_user_role 反查该角色下的用户
    /// - Position → cmx_user_position 反查该岗位下的用户
    /// - Org → cmx_org 该部门（及子树）下的用户
    ///
    /// 返回空 Vec 表示「该引用当前无对应用户」（不视为错误，由引擎决定如何处理空候选）。
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>>;

    /// 便捷：解析一组引用并并集去重（默认实现，实现方通常无需覆盖）。
    async fn resolve_all(&self, candidates: &[CandidateRef]) -> ResolveResult<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for c in candidates {
            for uid in self.resolve(c).await? {
                if seen.insert(uid.clone()) {
                    out.push(uid);
                }
            }
        }
        Ok(out)
    }
}
