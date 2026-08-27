//! 强校验 D1 的 HTTP 层断言（oneshot 挂 `flow_routes`，不起服务器、不依赖 DB）。
//!
//! 关键前提：`start_instance` 把 definitionKey 结构校验放在 `flow()` 运行时初始化
//! **之前**，所以 DB 不可达时这些用例依然成立。bizLink 反序列化失败（缺 bizTable /
//! bizId）走 axum Json rejection，同为 400——一并断言「结构必填 = HTTP 400」口径对齐。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cmx_flow_app::flow_routes;
use serde_json::json;
use tower::ServiceExt;

async fn post_start(body: serde_json::Value) -> (StatusCode, String) {
    let app = flow_routes::<()>();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/flow/instances")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
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

#[tokio::test]
async fn missing_definition_key_is_http_400() {
    let (status, body) = post_start(json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("\"code\":2"), "信封 code=2: {body}");
    assert!(body.contains("缺少 definitionKey"), "{body}");
}

#[tokio::test]
async fn blank_definition_key_is_http_400_too() {
    // serde 下空串是 Some("")，不走 None 分支——trim 判空须同样拦下。
    for key in ["", "   "] {
        let (status, body) = post_start(json!({ "definitionKey": key })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "key={key:?}");
        assert!(body.contains("\"code\":2"), "key={key:?}: {body}");
    }
}

#[tokio::test]
async fn biz_link_missing_required_fields_is_422_rejection() {
    // 实测口径：BizLinkReq.bizTable/bizId 无 serde default → 缺失走 axum 0.8 Json
    // rejection 的 **422 Unprocessable Entity**（非 400）。与 D1 的 BadRequest 400
    // 并列构成「结构必填 ≠ 200 成功」的两条路径：字节级缺失 → 422 自动拒，
    // 语义必填（definitionKey）→ 本改造自控的 400。
    let (status, _body) = post_start(json!({ "definitionKey": "x", "bizLink": {} })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
