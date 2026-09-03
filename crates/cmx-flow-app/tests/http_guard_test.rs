//! 安全闸 HTTP 层断言（X2-T，018 gate 随批落）：oneshot 挂 `flow_routes` + `tenant::scope`
//! 注入身份，不起服务器、不依赖 DB——验证闸的**拒绝路径**在运行时初始化之前生效
//!（与 http_start_validation.rs 同一前提：闸函数置于 `flow()` 之前）。
//!
//! 覆盖：
//!   - 事件订阅管理写端点角色闸三态：无角色 → 403；service/flow-admin/admin → 过闸
//!     （过闸后因 DB 不可达会得到别的错误，但**不得**是 403 EVENT_ADMIN_REQUIRED）；
//!   - 干预端点闸（X2-7）：无角色 JWT 用户 suspend → 403 FLOW_ADMIN_REQUIRED；service 放行；
//!   - reject 无身份拒绝（S-11）：auth 语境下 current_user 为 None + 缺 from_user → 400。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cmx_engine_kit::tenant::{self, TenantCtx};
use cmx_flow_app::flow_routes;
use serde_json::json;
use tower::ServiceExt;

fn ctx(user: Option<&str>, roles: &[&str]) -> TenantCtx {
    let mut c = TenantCtx::new("default").with_roles(roles.iter().map(|s| s.to_string()).collect());
    c.user = user.map(str::to_string);
    c
}

async fn call(method: &str, uri: &str, body: Option<serde_json::Value>) -> (StatusCode, String) {
    let app = flow_routes::<()>();
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let req = b
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let text = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, text)
}

/// 无角色 JWT 用户调事件订阅管理写端点 → 403（EVENT_ADMIN_REQUIRED）。
#[tokio::test]
async fn event_write_gate_rejects_roleless_user() {
    let (status, body) = tenant::scope(ctx(Some("u1"), &[]), async {
        call("POST", "/flow/event-subscribers/set-active", Some(json!({"id": 1, "active": false}))).await
    }).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("EVENT_ADMIN_REQUIRED"), "{body}");
}

/// service 身份（M2M key）过闸（不再 403；后续 DB 不可达属预期非闸拒绝）。
#[tokio::test]
async fn event_write_gate_allows_service_identity() {
    for roles in [["service"], ["flow-admin"], ["flow-event-admin"], ["admin"]] {
        let (status, body) = tenant::scope(ctx(None, &roles), async {
            call("POST", "/flow/event-subscribers/set-active", Some(json!({"id": 1, "active": false}))).await
        }).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "roles={roles:?} 应过闸: {body}");
        assert!(!body.contains("EVENT_ADMIN_REQUIRED"), "roles={roles:?}: {body}");
    }
}

/// X2-7：无角色 JWT 用户 suspend → 403 FLOW_ADMIN_REQUIRED（干预闸拒绝路径）。
#[tokio::test]
async fn ops_gate_rejects_roleless_user() {
    let (status, body) = tenant::scope(ctx(Some("u1"), &[]), async {
        call("POST", "/flow/instances/i-1/suspend", None).await
    }).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("FLOW_ADMIN_REQUIRED"), "{body}");
}

/// X2-7：flow-admin 用户过闸；service（无用户身份）过闸（两阶段：key 通道维持）。
#[tokio::test]
async fn ops_gate_allows_admin_and_service() {
    for (user, roles) in [(Some("u1"), ["flow-admin"]), (None, ["service"])] {
        let (status, body) = tenant::scope(ctx(user, &roles), async {
            call("POST", "/flow/instances/i-1/suspend", None).await
        }).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains("FLOW_ADMIN_REQUIRED"), "{body}");
    }
}

/// X2-4：无角色 JWT 用户保存子流程绑定 → 403（写闸挂于 upsert）。
#[tokio::test]
async fn subflow_binding_write_gate_rejects_roleless_user() {
    let (status, body) = tenant::scope(ctx(Some("u1"), &[]), async {
        call("POST", "/flow/subflow-bindings", Some(json!({
            "calledKey": "mdm_cr", "targetKey": "mdm_cr_v2"
        }))).await
    }).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("FLOW_ADMIN_REQUIRED") || body.contains("NAMESPACE_MISMATCH"), "{body}");
}

/// S-11：reject 的操作者校验——服务端身份 u1 与自报 from_user=u2 不一致 → 403
/// IDENTITY_MISMATCH（原「from_user 可选、不传即跳过校验」的绕过面由兜底逻辑修复，
/// 此处锁定「传了但冒名」的拒绝路径；「无身份拒绝」依赖 auth_middleware_active 的
/// 进程级状态，单测无法注入，由 E2E 覆盖）。
#[tokio::test]
async fn reject_identity_mismatch_is_403() {
    let (status, body) = tenant::scope(ctx(Some("u1"), &[]), async {
        call("POST", "/flow/tasks/t-1/reject", Some(json!({
            "instanceId": "i-1", "fromUser": "u2"
        }))).await
    }).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("IDENTITY_MISMATCH"), "{body}");
}
