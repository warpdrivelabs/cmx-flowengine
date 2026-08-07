//! HTTP 适配器集成测试：起进程内 axum 桩服务，验证三适配器的 请求→响应→trait 返回/变量写回
//! 全链路 + 错误路径（非 2xx→Backend、404/空→NoBinding、4xx→InvalidRef）。
//!
//! 复用 workspace 已有 axum（dev-dep），桩服务 bind 127.0.0.1:0 拿随机端口，spawn 后即用。

use std::net::SocketAddr;

use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    routing::post,
};
use serde_json::{Value, json};

use cmx_flow_adapters::{HttpAssigneeResolver, HttpDelegate, HttpSubflowRouter};
use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, DelegateContext, JavaDelegate, ResolveError,
    RouteError, SubflowRouter, Variables,
};

/// 起桩服务，返回 base_url（http://127.0.0.1:PORT）。
async fn spawn_stub() -> String {
    let app = Router::new()
        // 身份：ROLE(finance)→两人；未知 value→400（InvalidRef）；其它→空。
        .route(
            "/identity/resolve",
            post(|Json(body): Json<Value>| async move {
                let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let value = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
                if value == "nonexistent" {
                    return (StatusCode::BAD_REQUEST, Json(json!({"msg":"无此引用"})));
                }
                let ids = if kind == "ROLE" && value == "finance" {
                    vec!["u_a", "u_b"]
                } else {
                    vec![]
                };
                (StatusCode::OK, Json(json!({ "userIds": ids })))
            }),
        )
        // 子流程：fin_review→有解；unknown→404（NoBinding）。
        .route(
            "/subflow/resolve",
            post(|Json(body): Json<Value>| async move {
                let called = body.get("calledKey").and_then(|v| v.as_str()).unwrap_or("");
                if called == "fin_review" {
                    let org = body.get("orgId").and_then(|v| v.as_str()).unwrap_or("hq");
                    (StatusCode::OK, Json(json!({ "targetKey": format!("fin_review_{org}") })))
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({ "msg": "无绑定" })))
                }
            }),
        )
        // delegate：回读变量 amount，写回 riskLevel + 透传 node（query）。
        .route(
            "/delegate/run",
            post(|Query(q): Query<std::collections::HashMap<String, String>>, Json(body): Json<Value>| async move {
                let amount = body.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let level = if amount > 10000.0 { "高" } else { "低" };
                let node = q.get("node").cloned().unwrap_or_default();
                (StatusCode::OK, Json(json!({ "variables": { "riskLevel": level, "seenNode": node } })))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn http_resolver_role_returns_users() {
    let base = spawn_stub().await;
    let r = HttpAssigneeResolver::new(&base);
    let ids = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
        .await
        .unwrap();
    assert_eq!(ids, vec!["u_a", "u_b"]);
}

#[tokio::test]
async fn http_resolver_user_shortcircuits() {
    // User kind 不打网络，直接返回自身（base 给个坏地址也应成功）。
    let r = HttpAssigneeResolver::new("http://127.0.0.1:1");
    let ids = r
        .resolve(&CandidateRef { kind: CandidateKind::User, value: "u_self".into() })
        .await
        .unwrap();
    assert_eq!(ids, vec!["u_self"]);
}

#[tokio::test]
async fn http_resolver_invalid_ref_is_client_error() {
    let base = spawn_stub().await;
    let r = HttpAssigneeResolver::new(&base);
    let err = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "nonexistent".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::InvalidRef(_)), "got {err:?}");
}

#[tokio::test]
async fn http_resolver_network_error_is_backend() {
    // 无人监听的端口 → 网络错 → Backend。
    let r = HttpAssigneeResolver::new("http://127.0.0.1:2");
    let err = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::Backend(_)), "got {err:?}");
}

#[tokio::test]
async fn http_router_resolves_and_missing_is_nobinding() {
    let base = spawn_stub().await;
    let r = HttpSubflowRouter::new(&base);
    // 有解：带上 org。
    assert_eq!(r.resolve("fin_review", Some("bj")).await.unwrap(), "fin_review_bj");
    // 无解：404 → NoBinding。
    let err = r.resolve("unknown_key", Some("bj")).await.unwrap_err();
    assert!(
        matches!(err, RouteError::NoBinding { ref called_key, .. } if called_key == "unknown_key"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn http_delegate_posts_vars_and_merges_back() {
    let base = spawn_stub().await;
    let d = HttpDelegate::new(format!("{base}/delegate/run"));
    let mut vars = Variables::new();
    vars.set("amount", json!(50000));
    let mut ctx = DelegateContext {
        instance_id: "i_1",
        node_bpmn_id: "svc_risk",
        variables: &mut vars,
    };
    d.execute(&mut ctx).await.unwrap();
    // 外部算出 riskLevel=高 并 merge 回；node 经 query 透传回。
    assert_eq!(vars.get("riskLevel"), Some(&json!("高")));
    assert_eq!(vars.get("seenNode"), Some(&json!("svc_risk")));
    // 原变量保留。
    assert_eq!(vars.get("amount"), Some(&json!(50000)));
}

#[tokio::test]
async fn http_delegate_non_2xx_is_err() {
    // 打到一个存在的桩但错误路径（用不存在的 delegate 端点 → 404）。
    let base = spawn_stub().await;
    let d = HttpDelegate::new(format!("{base}/no/such/delegate"));
    let mut vars = Variables::new();
    let mut ctx = DelegateContext {
        instance_id: "i_1",
        node_bpmn_id: "svc",
        variables: &mut vars,
    };
    assert!(d.execute(&mut ctx).await.is_err());
}
