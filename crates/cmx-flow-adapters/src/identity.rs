//! HttpAssigneeResolver —— 候选人解析的外部 HTTP 实现（方案 §4①）。
//!
//! 实现 `AssigneeResolver`：把 role/position/org 引用 POST 给外部身份服务解析成用户 id 列表，
//! 替代 Pg 版的直连 IAM 库。引擎因此不认识任何身份系统——换服务键即换后端。
//!
//! 传输走 cmx-service-rpc 基座：目标 = `[service_rpc.services]` 服务目录键（无注册中心时
//! 目录登记静态 url 直连），鉴权注入/超时/重试/熔断由基座统一承载。对端协议保持自定义
//! 裸 JSON（非 ApiResp 信封——外部身份服务不受 CMX 契约约束），故用 `execute` 取原始响应。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, ResolveContext, ResolveError, ResolveResult,
};
use cmx_service_rpc::{ServiceRpcError, ServiceRpcHandle};

/// 外部身份服务候选人解析器。持服务目录键 + 基座句柄。
#[derive(Clone)]
pub struct HttpAssigneeResolver {
    key: String,
    rpc: Arc<ServiceRpcHandle>,
}

/// `POST {svc}/identity/resolve` 请求体。
#[derive(Serialize)]
struct ResolveReq<'a> {
    /// 候选类型（USER/ROLE/POSITION/ORG/ORG_LEADER/INITIATOR/INITIATOR_LEADER，对齐
    /// CandidateKind 的 SCREAMING_SNAKE 序列化）。
    kind: &'a str,
    /// 候选值（user_id / role code / position code / org id；关系型可空）。
    value: &'a str,
    /// 解析上下文（P0 关系型解析用；非关系型可忽略）。外部服务据此解析部门领导/发起人上级。
    #[serde(skip_serializing_if = "Option::is_none")]
    initiator: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "orgId")]
    org_id: Option<&'a str>,
}

/// 期望响应体：解析出的用户 id 列表。
#[derive(Deserialize)]
struct ResolveResp {
    #[serde(default, rename = "userIds")]
    user_ids: Vec<String>,
}

/// 基座错误 → 身份解析错误：4xx 语义为「引用无效」（角色 code 不存在等），其余归后端故障。
fn map_err(e: ServiceRpcError) -> ResolveError {
    match &e {
        ServiceRpcError::Remote { http_status, .. } if (400..500).contains(http_status) => {
            ResolveError::InvalidRef(e.to_string())
        }
        _ => ResolveError::Backend(format!("身份服务调用失败: {e}")),
    }
}

impl HttpAssigneeResolver {
    /// 生产构造：目标为服务目录键（`[service_rpc.services]` 登记）。基座未初始化（未跑
    /// `init_infra`）时返回 `None`，装配点据此回退 mock。
    pub fn new(key: impl Into<String>) -> Option<Self> {
        cmx_service_rpc::global_arc().map(|rpc| Self {
            key: key.into(),
            rpc,
        })
    }

    /// 测试/定制构造：显式传入基座句柄（不经全局单例，测试并行安全）。
    pub fn with_handle(key: impl Into<String>, rpc: ServiceRpcHandle) -> Self {
        Self {
            key: key.into(),
            rpc: Arc::new(rpc),
        }
    }

    fn kind_str(kind: CandidateKind) -> &'static str {
        match kind {
            CandidateKind::User => "USER",
            CandidateKind::Role => "ROLE",
            CandidateKind::Position => "POSITION",
            CandidateKind::Org => "ORG",
            CandidateKind::OrgLeader => "ORG_LEADER",
            CandidateKind::Initiator => "INITIATOR",
            CandidateKind::InitiatorLeader => "INITIATOR_LEADER",
        }
    }

    /// 统一 POST → user_ids。ctx 为关系型解析提供 initiator/org（非关系型时两者为 None）。
    async fn post_resolve(
        &self,
        candidate: &CandidateRef,
        ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        // User 本地短路：就是该 id 本身（与 Pg 一致，省一次往返）。
        if candidate.kind == CandidateKind::User {
            return Ok(vec![candidate.value.clone()]);
        }
        // 发起人本人：本地解析（不必往返外部服务）。
        if candidate.kind == CandidateKind::Initiator {
            return Ok(ctx
                .initiator
                .clone()
                .filter(|s| !s.is_empty())
                .into_iter()
                .collect());
        }

        let body = ResolveReq {
            kind: Self::kind_str(candidate.kind),
            value: &candidate.value,
            initiator: ctx.initiator.as_deref(),
            org_id: ctx.org_id.as_deref(),
        };
        let req = cmx_service_rpc::RpcRequest::post(self.key.clone(), "/identity/resolve")
            .json_body(
                serde_json::to_value(&body)
                    .map_err(|e| ResolveError::Backend(format!("身份服务请求序列化失败: {e}")))?,
            )
            // 查询语义（只读解析），允许连接级错误换实例重试。
            .idempotent();
        let resp = self.rpc.execute(req).await.map_err(map_err)?;

        // 基座对无法解析为 JSON 的响应置 Null——与原裸 reqwest 行为对齐，此时报错而非解出空表。
        if resp.body.is_null() {
            return Err(ResolveError::Backend(
                "身份服务响应解析失败: 非 JSON".to_string(),
            ));
        }
        let parsed: ResolveResp = serde_json::from_value(resp.body)
            .map_err(|e| ResolveError::Backend(format!("身份服务响应解析失败: {e}")))?;
        Ok(parsed.user_ids)
    }
}

#[async_trait]
impl AssigneeResolver for HttpAssigneeResolver {
    async fn resolve(&self, candidate: &CandidateRef) -> ResolveResult<Vec<String>> {
        self.post_resolve(candidate, &ResolveContext::default())
            .await
    }

    async fn resolve_with(
        &self,
        candidate: &CandidateRef,
        ctx: &ResolveContext,
    ) -> ResolveResult<Vec<String>> {
        self.post_resolve(candidate, ctx).await
    }
}
