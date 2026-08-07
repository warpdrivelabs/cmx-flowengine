//! 出站 webhook 集成测试：起进程内 axum 接收桩，验证 WebhookSender 后台投递、HMAC 签名、
//! 事件字段、重试（前几次 500 后成功）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::mpsc;

use cmx_flow_adapters::{FlowEvent, FlowEventKind, WebhookSender};

type HmacSha256 = Hmac<Sha256>;

/// 桩收到的一条请求：body + 关键头。
#[derive(Debug, Clone)]
struct Received {
    body: Vec<u8>,
    event: String,
    signature: Option<String>,
}

#[derive(Clone)]
struct StubState {
    tx: mpsc::UnboundedSender<Received>,
    /// 前 N 次请求返回 500（验证重试），之后 200。
    fail_first: Arc<AtomicUsize>,
}

async fn spawn_receiver(fail_first: usize) -> (String, mpsc::UnboundedReceiver<Received>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = StubState {
        tx,
        fail_first: Arc::new(AtomicUsize::new(fail_first)),
    };
    let app = Router::new()
        .route(
            "/hook",
            post(|State(st): State<StubState>, headers: HeaderMap, body: axum::body::Bytes| async move {
                let event = headers
                    .get("x-cmx-flow-event")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let signature = headers
                    .get("x-cmx-flow-signature")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let _ = st.tx.send(Received {
                    body: body.to_vec(),
                    event,
                    signature,
                });
                // 前 fail_first 次返回 500，触发重试。
                let remaining = st.fail_first.load(Ordering::SeqCst);
                if remaining > 0 {
                    st.fail_first.store(remaining - 1, Ordering::SeqCst);
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::OK
                }
            }),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/hook"), rx)
}

#[tokio::test]
async fn delivers_event_with_all_fields() {
    let (url, mut rx) = spawn_receiver(0).await;
    let sender = WebhookSender::spawn_worker(vec![url], None, 0);
    assert!(sender.is_enabled());

    let ev = FlowEvent::new(FlowEventKind::InstanceStarted, "inst-1", "2026-08-06T00:00:00Z".into())
        .state(Some("ACTIVE".into()))
        .definition_key(Some("ss_demo".into()))
        .business_key(Some("BK-1".into()));
    sender.emit(ev);

    let got = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("超时未收到 webhook")
        .expect("通道关闭");
    assert_eq!(got.event, "instance.started");
    let json: Value = serde_json::from_slice(&got.body).unwrap();
    assert_eq!(json["event"], "instance.started");
    assert_eq!(json["instanceId"], "inst-1");
    assert_eq!(json["state"], "ACTIVE");
    assert_eq!(json["definitionKey"], "ss_demo");
    assert_eq!(json["businessKey"], "BK-1");
    assert_eq!(json["occurredAt"], "2026-08-06T00:00:00Z");
}

#[tokio::test]
async fn signature_is_valid_hmac() {
    let (url, mut rx) = spawn_receiver(0).await;
    let key = "topsecret";
    let sender = WebhookSender::spawn_worker(vec![url], Some(key.into()), 0);
    sender.emit(FlowEvent::new(FlowEventKind::TaskCreated, "inst-2", "t".into()));

    let got = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("超时")
        .expect("关闭");
    let sig = got.signature.expect("应带签名头");
    assert!(sig.starts_with("sha256="), "sig={sig}");
    // 用同密钥对收到的 body 重算 HMAC，应与头一致。
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
    mac.update(&got.body);
    let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert_eq!(sig, expected, "签名不匹配 → 验签会失败");
}

#[tokio::test]
async fn retries_until_success() {
    // 前 2 次 500，第 3 次 200：max_retries=3 应最终成功，桩共收到 3 次。
    let (url, mut rx) = spawn_receiver(2).await;
    let sender = WebhookSender::spawn_worker(vec![url], None, 3);
    sender.emit(FlowEvent::new(FlowEventKind::InstanceCompleted, "inst-3", "t".into()));

    // 收 3 次（含 2 次失败重试）。退避 1s+2s，给足 6s。
    let mut count = 0;
    let deadline = std::time::Duration::from_secs(8);
    let start = tokio::time::Instant::now();
    while count < 3 && start.elapsed() < deadline {
        if let Ok(Some(_)) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            count += 1;
        } else {
            break;
        }
    }
    assert_eq!(count, 3, "应重试至第 3 次成功（共收到 3 次投递）");
}

#[tokio::test]
async fn disabled_sender_sends_nothing() {
    let (url, mut rx) = spawn_receiver(0).await;
    // 空 URL 列表 → disabled。
    let sender = WebhookSender::spawn_worker(vec![], None, 0);
    assert!(!sender.is_enabled());
    sender.emit(FlowEvent::new(FlowEventKind::InstanceStarted, "x", "t".into()));
    // 也验证 url 桩根本没被调用（短等无消息）。
    let _ = url;
    let got = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
    assert!(got.is_err(), "disabled 不应发出任何请求");
}
