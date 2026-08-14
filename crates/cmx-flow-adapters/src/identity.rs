//! HttpAssigneeResolver —— 候选人解析的外部 HTTP 实现（方案 §4①）。
//!
//! 实现 `AssigneeResolver`：把 role/position/org 引用 POST 给外部身份服务解析成用户 id 列表，
//! 替代 Pg 版的直连 IAM 库。引擎因此不认识任何身份系统——换 URL 即换后端。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, ResolveContext, ResolveError, ResolveResult,
};

/// 外部身份服务候选人解析器。持 base_url + 复用连接的 reqwest client。
#[derive(Debug, Clone)]
pub struct HttpAssigneeResolver {
    base_url: String,
    http: reqwest::Client,
}

/// `POST {base}/identity/resolve` 请求体。
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

impl HttpAssigneeResolver {
    /// 用外部身份服务 base_url 构建（末尾斜杠会被规整）。
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
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

        let url = format!("{}/identity/resolve", self.base_url);
        let body = ResolveReq {
            kind: Self::kind_str(candidate.kind),
            value: &candidate.value,
            initiator: ctx.initiator.as_deref(),
            org_id: ctx.org_id.as_deref(),
        };
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ResolveError::Backend(format!("身份服务请求失败: {e}")))?;

        let status = resp.status();
        if status.is_client_error() {
            // 4xx：语义为「引用无效」（如角色 code 不存在）。
            let msg = resp.text().await.unwrap_or_default();
            return Err(ResolveError::InvalidRef(format!(
                "身份服务拒绝 {}({}): {status} {msg}",
                Self::kind_str(candidate.kind),
                candidate.value
            )));
        }
        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            return Err(ResolveError::Backend(format!(
                "身份服务返回 {status}: {msg}"
            )));
        }

        let parsed: ResolveResp = resp
            .json()
            .await
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
