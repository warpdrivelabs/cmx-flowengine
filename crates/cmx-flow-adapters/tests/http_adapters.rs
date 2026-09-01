//! HTTP 适配器集成测试：起进程内 axum 桩服务，把桩地址登记进**独立基座句柄**（每测试
//! 自建 `ServiceRpcHandle`，不经全局单例——并行安全），验证三适配器经 cmx-service-rpc
//! 基座（目录键定位 → 传输 → raw 响应解析）的请求→响应→trait 返回/变量写回全链路 +
//! 错误路径（非 2xx→Backend、404/空→NoBinding/空链、4xx→InvalidRef）。
//!
//! 复用 workspace 已有 axum（dev-dep），桩服务 bind 127.0.0.1:0 拿随机端口，spawn 后即用。

use std::net::SocketAddr;

use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};

use cmx_flow_adapters::{
    HttpAssigneeResolver, HttpDelegate, HttpDimensionResolver, HttpSubflowRouter,
};
use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, DelegateContext, DimensionResolver,
    JavaDelegate, ResolveError, RouteError, SubflowRouter, Variables,
};

/// 测试统一使用的服务目录键。
const SVC_KEY: &str = "ext_svc";

/// 用桩地址构建独立基座句柄（目录：`ext_svc` → 静态 url；不经全局单例）。
fn svc_handle(url: impl Into<String>) -> cmx_service_rpc::ServiceRpcHandle {
    let mut cfg = cmx_service_rpc::ServiceRpcConfig::default();
    cfg.services.insert(
        SVC_KEY.to_string(),
        cmx_service_rpc::ServiceEntry {
            url: Some(url.into()),
            ..Default::default()
        },
    );
    cmx_service_rpc::ServiceRpcHandle::new(cfg)
}

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
                    // RD0：请求体 dimValue（原 orgId 的泛化）。
                    let dv = body.get("dimValue").and_then(|v| v.as_str()).unwrap_or("hq");
                    (StatusCode::OK, Json(json!({ "targetKey": format!("fin_review_{dv}") })))
                } else {
                    (StatusCode::NOT_FOUND, Json(json!({ "msg": "无绑定" })))
                }
            }),
        )
        // delegate：回读变量 amount，写回 riskLevel + 透传 node（query）；forceError→500。
        .route(
            "/delegate/run",
            post(|Query(q): Query<std::collections::HashMap<String, String>>, Json(body): Json<Value>| async move {
                if body.get("forceError").is_some() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"msg":"外部逻辑失败"})));
                }
                let amount = body.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let level = if amount > 10000.0 { "高" } else { "低" };
                let node = q.get("node").cloned().unwrap_or_default();
                (StatusCode::OK, Json(json!({ "variables": { "riskLevel": level, "seenNode": node } })))
            }),
        )
        // 维度层级（RD5）：org=bj_g1→两级祖先；未知取值→404（空链=无继承）。
        .route(
            "/dimensions/ancestors",
            get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                match q.get("dimValue").map(String::as_str) {
                    Some("bj_g1") => (
                        StatusCode::OK,
                        Json(json!({ "ancestors": ["bj", "hq"] })),
                    ),
                    _ => (StatusCode::NOT_FOUND, Json(json!({ "msg": "无层级" }))),
                }
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
    let r = HttpAssigneeResolver::with_handle(SVC_KEY, svc_handle(base));
    let ids = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
        .await
        .unwrap();
    assert_eq!(ids, vec!["u_a", "u_b"]);
}

#[tokio::test]
async fn http_resolver_user_shortcircuits() {
    // User kind 不打网络，直接返回自身（目录给个坏地址也应成功）。
    let r = HttpAssigneeResolver::with_handle(SVC_KEY, svc_handle("http://127.0.0.1:1"));
    let ids = r
        .resolve(&CandidateRef { kind: CandidateKind::User, value: "u_self".into() })
        .await
        .unwrap();
    assert_eq!(ids, vec!["u_self"]);
}

#[tokio::test]
async fn http_resolver_invalid_ref_is_client_error() {
    let base = spawn_stub().await;
    let r = HttpAssigneeResolver::with_handle(SVC_KEY, svc_handle(base));
    let err = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "nonexistent".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::InvalidRef(_)), "got {err:?}");
}

#[tokio::test]
async fn http_resolver_network_error_is_backend() {
    // 无人监听的端口 → 基座请求失败（Unavailable）→ Backend。
    let r = HttpAssigneeResolver::with_handle(SVC_KEY, svc_handle("http://127.0.0.1:2"));
    let err = r
        .resolve(&CandidateRef { kind: CandidateKind::Role, value: "finance".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::Backend(_)), "got {err:?}");
}

#[tokio::test]
async fn http_router_resolves_and_missing_is_nobinding() {
    let base = spawn_stub().await;
    let r = HttpSubflowRouter::with_handle(SVC_KEY, svc_handle(base));
    // 有解：带上 org 维度。
    assert_eq!(r.resolve("fin_review", "org", Some("bj")).await.unwrap(), "fin_review_bj");
    // 无解：404 → NoBinding。
    let err = r.resolve("unknown_key", "org", Some("bj")).await.unwrap_err();
    assert!(
        matches!(err, RouteError::NoBinding { ref called_key, .. } if called_key == "unknown_key"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn http_dimension_ancestors_and_404_is_empty() {
    let base = spawn_stub().await;
    let r = HttpDimensionResolver::with_handle(SVC_KEY, svc_handle(base));
    // 有层级：由近及远两级祖先。
    assert_eq!(
        r.ancestors("org", "bj_g1").await.unwrap(),
        vec!["bj".to_string(), "hq".to_string()]
    );
    // 404 → 空链（无继承，非错误）。
    assert!(r.ancestors("org", "unknown").await.unwrap().is_empty());
}

#[tokio::test]
async fn http_delegate_posts_vars_and_merges_back() {
    let base = spawn_stub().await;
    let d = HttpDelegate::with_handle(SVC_KEY, svc_handle(base));
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
    // 桩的 /delegate/run 在收到 forceError 变量时回 500 → 基座 Remote → Err。
    let base = spawn_stub().await;
    let d = HttpDelegate::with_handle(SVC_KEY, svc_handle(base));
    let mut vars = Variables::new();
    vars.set("forceError", json!(true));
    let mut ctx = DelegateContext {
        instance_id: "i_1",
        node_bpmn_id: "svc",
        variables: &mut vars,
    };
    assert!(d.execute(&mut ctx).await.is_err());
}
