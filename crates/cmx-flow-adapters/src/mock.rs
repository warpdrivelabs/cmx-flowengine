//! 三适配器的 Mock 默认实现：脱一切外部可单跑（开发/演示/CI/单测锚点），恒成功、输出可预测。

use async_trait::async_trait;
use serde_json::json;

use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, DelegateContext, JavaDelegate, ResolveContext,
    ResolveResult, RouteResult, SubflowRouter,
};

/// Mock 候选人解析：User→自身；Role/Position/Org→可预测的合成用户 id。
///
/// 不连任何库，输出确定：`resolve(role(finance))` → `["role:finance:u1","role:finance:u2"]`，
/// 便于「脱外部起流程 → 候选池落 2 人 → 待认领」的端到端演示与断言。
#[derive(Debug, Clone, Default)]
pub struct MockAssigneeResolver;

#[async_trait]
impl AssigneeResolver for MockAssigneeResolver {
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>> {
        let v = &candidate.value;
        let out = match candidate.kind {
            // 指定用户：就是该 id 本身（与 Pg/Http 一致）。
            CandidateKind::User => vec![v.clone()],
            // 角色/岗位/部门：合成两个可预测用户 id（够触发候选池「待认领」路径）。
            CandidateKind::Role => vec![format!("role:{v}:u1"), format!("role:{v}:u2")],
            CandidateKind::Position => vec![format!("position:{v}:u1")],
            CandidateKind::Org => vec![format!("org:{v}:u1"), format!("org:{v}:u2")],
            // 关系型（无上下文）：orgLeader(orgId) 显式可合成；其余需上下文，返回空。
            CandidateKind::OrgLeader if !v.is_empty() => vec![format!("orgLeader:{v}")],
            CandidateKind::OrgLeader
            | CandidateKind::Initiator
            | CandidateKind::InitiatorLeader => vec![],
        };
        Ok(out)
    }

    /// 带上下文：关系型合成可预测 id（发起人本人=ctx.initiator；部门领导=orgLeader:<org>；
    /// 发起人上级=initiatorLeader:<initiator>）。便于端到端演示无需真实 IAM。
    async fn resolve_with(
        &self,
        candidate: &CandidateRef,
        ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        let out = match candidate.kind {
            CandidateKind::OrgLeader => {
                let org = if candidate.value.is_empty() {
                    ctx.org_id.clone().unwrap_or_default()
                } else {
                    candidate.value.clone()
                };
                if org.is_empty() {
                    vec![]
                } else {
                    vec![format!("orgLeader:{org}")]
                }
            }
            CandidateKind::Initiator => ctx
                .initiator
                .clone()
                .filter(|s| !s.is_empty())
                .into_iter()
                .collect(),
            CandidateKind::InitiatorLeader => match ctx.initiator.as_deref() {
                Some(u) if !u.is_empty() => vec![format!("initiatorLeader:{u}")],
                _ => vec![],
            },
            _ => return self.resolve(candidate).await,
        };
        Ok(out)
    }
}

/// Mock 子流程路由：`called_key` 原样当目标定义 key 返回，保证恒有解（不产生 NoBinding）。
#[derive(Debug, Clone, Default)]
pub struct MockSubflowRouter;

#[async_trait]
impl SubflowRouter for MockSubflowRouter {
    async fn resolve(&self, called_key: &str, _org_id: Option<&str>) -> RouteResult<String> {
        // 逻辑 key 直接作为目标定义 key（demo 里 callActivity 常直接写目标 key）。
        Ok(called_key.to_string())
    }
}

/// Mock serviceTask delegate：no-op，只写一个标记变量，恒成功。
#[derive(Debug, Clone, Default)]
pub struct MockDelegate;

#[async_trait]
impl JavaDelegate for MockDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String> {
        ctx.variables.set("mockDelegate", json!(true));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmx_flow_engine::Variables;

    #[tokio::test]
    async fn mock_resolver_predictable() {
        let r = MockAssigneeResolver;
        // User → 自身
        let u = r
            .resolve(&CandidateRef { kind: CandidateKind::User, value: "u_1".into() })
            .await
            .unwrap();
        assert_eq!(u, vec!["u_1"]);
        // Role → 两个合成 id（够落候选池）
        let role = r
            .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
            .await
            .unwrap();
        assert_eq!(role, vec!["role:finance:u1", "role:finance:u2"]);
    }

    #[tokio::test]
    async fn mock_resolves_relationship_kinds_with_context() {
        let r = MockAssigneeResolver;
        let ctx = ResolveContext::new(Some("u_boss".into()), Some("d_fin".into()));
        // 发起人本人 → ctx.initiator
        let init = r
            .resolve_with(&CandidateRef { kind: CandidateKind::Initiator, value: String::new() }, &ctx)
            .await
            .unwrap();
        assert_eq!(init, vec!["u_boss"]);
        // 部门领导（无显式 org）→ 用 ctx.org_id
        let ol = r
            .resolve_with(&CandidateRef { kind: CandidateKind::OrgLeader, value: String::new() }, &ctx)
            .await
            .unwrap();
        assert_eq!(ol, vec!["orgLeader:d_fin"]);
        // 部门领导（显式 org 覆盖）
        let ol2 = r
            .resolve_with(&CandidateRef { kind: CandidateKind::OrgLeader, value: "d_hr".into() }, &ctx)
            .await
            .unwrap();
        assert_eq!(ol2, vec!["orgLeader:d_hr"]);
        // 发起人上级
        let il = r
            .resolve_with(&CandidateRef { kind: CandidateKind::InitiatorLeader, value: String::new() }, &ctx)
            .await
            .unwrap();
        assert_eq!(il, vec!["initiatorLeader:u_boss"]);
    }

    #[tokio::test]
    async fn mock_relationship_without_context_is_empty() {
        let r = MockAssigneeResolver;
        // 无上下文（走无参 resolve）：发起人相关无从解析 → 空。
        let init = r
            .resolve(&CandidateRef { kind: CandidateKind::Initiator, value: String::new() })
            .await
            .unwrap();
        assert!(init.is_empty());
        // orgLeader 显式给 org 时无上下文也能合成。
        let ol = r
            .resolve(&CandidateRef { kind: CandidateKind::OrgLeader, value: "d_x".into() })
            .await
            .unwrap();
        assert_eq!(ol, vec!["orgLeader:d_x"]);
    }

    #[tokio::test]
    async fn mock_router_echoes_key() {
        let r = MockSubflowRouter;
        assert_eq!(r.resolve("fin_review", Some("d_bj")).await.unwrap(), "fin_review");
        assert_eq!(r.resolve("fin_review", None).await.unwrap(), "fin_review");
    }

    #[tokio::test]
    async fn mock_delegate_sets_marker() {
        let d = MockDelegate;
        let mut vars = Variables::new();
        let mut ctx = DelegateContext {
            instance_id: "i1",
            node_bpmn_id: "svc1",
            variables: &mut vars,
        };
        d.execute(&mut ctx).await.unwrap();
        assert_eq!(vars.get("mockDelegate"), Some(&json!(true)));
    }
}
