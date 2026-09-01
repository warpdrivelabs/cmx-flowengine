//! 出站 webhook 集成测试：起进程内 axum 接收桩（伪装 mdm 的 `/api/mdm/flow/callback`
//! 端点），经 `cmx-service-rpc` 基座目录（键 "mdm" → 桩 URL）真实走投递链路，验证
//! WebhookSender 后台投递、事件字段 / 幂等头、HMAC 签名（两端同源）、重试（前 2 次 500
//! 后成功）。
//!
//! 基座全局句柄进程内单例：接收桩相关断言合并为一条顺序测试（共享桩 + 一次 install），
//! 避免并行测试争抢全局目录。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{
    Router,
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
    delivery: String,
    signature: Option<String>,
}

#[derive(Clone)]
struct StubState {
    tx: mpsc::UnboundedSender<Received>,
    /// 前 N 次请求返回 500（验证重试），之后 200（标准信封）。
    fail_first: Arc<AtomicUsize>,
}

/// 起接收桩（伪装 mdm 回调端点）。
async fn spawn_receiver(fail_first: usize) -> (String, mpsc::UnboundedReceiver<Received>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = StubState {
        tx,
        fail_first: Arc::new(AtomicUsize::new(fail_first)),
    };
    let app = Router::new()
        .route(
            "/api/mdm/flow/callback",
            post(
                |State(st): State<StubState>, headers: HeaderMap, body: axum::body::Bytes| async move {
                    let event = headers
                        .get("x-cmx-flow-event")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let delivery = headers
                        .get("x-cmx-flow-delivery")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let signature = headers
                        .get("x-cmx-flow-signature")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    st.tx
                        .send(Received {
                            body: body.to_vec(),
                            event,
                            delivery,
                            signature,
                        })
                        .ok();
                    let remaining = st.fail_first.load(Ordering::SeqCst);
                    if remaining > 0 {
                        st.fail_first.store(remaining - 1, Ordering::SeqCst);
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                },
            ),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), rx)
}

/// 把基座全局目录装成「mdm 键 → 桩 URL」（已装则忽略——进程内单例）。
fn install_base(stub_url: &str) {
    let mut cfg = cmx_service_rpc::ServiceRpcConfig::default();
    cfg.services.insert(
        "mdm".to_string(),
        cmx_service_rpc::ServiceEntry {
            url: Some(stub_url.to_string()),
            ..Default::default()
        },
    );
    let _ = cmx_service_rpc::install(cmx_service_rpc::ServiceRpcHandle::new(cfg));
}

/// 投递全链路（顺序覆盖）：字段/幂等头 → 签名（两端同源）→ 重试至成功（前 2 次 500）。
#[tokio::test]
async fn webhook_delivery_contract() {
    let (url, mut rx) = spawn_receiver(2).await;
    install_base(&url);
    let key = "topsecret";
    let sender = WebhookSender::spawn_worker(vec!["mdm".to_string()], Some(key.to_string()), 3);
    assert!(sender.is_enabled());

    let ev = FlowEvent::new(
        FlowEventKind::InstanceStarted,
        "inst-1",
        "2026-08-06T00:00:00Z".to_string(),
    )
    .state(Some("ACTIVE".into()))
    .definition_key(Some("ss_demo".into()))
    .business_key(Some("BK-1".into()));
    sender.emit(ev);

    // —— 收 3 次（2 次失败重试 + 1 次成功；退避 1s+2s，给足窗口）——
    let mut received: Vec<Received> = Vec::new();
    let deadline = std::time::Duration::from_secs(10);
    let start = tokio::time::Instant::now();
    while received.len() < 3 && start.elapsed() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Some(got)) => received.push(got),
            _ => break,
        }
    }
    assert_eq!(received.len(), 3, "应重试至第 3 次成功（共收到 3 次投递）");

    // —— 字段 / 幂等头 / 签名（每次投递同体，任取一条断言）——
    let got = &received[0];
    assert_eq!(got.event, "instance.started");
    assert_eq!(got.delivery, "inst-1-2026-08-06T00:00:00Z", "幂等投递头");
    let json: Value = serde_json::from_slice(&got.body).unwrap();
    assert_eq!(json["event"], "instance.started");
    assert_eq!(json["instanceId"], "inst-1");
    assert_eq!(json["state"], "ACTIVE");
    assert_eq!(json["definitionKey"], "ss_demo");
    assert_eq!(json["businessKey"], "BK-1");
    assert_eq!(json["occurredAt"], "2026-08-06T00:00:00Z");

    // 签名：同密钥对收到的 body 重算 HMAC，应与头一致（接收端验签视角）。
    let sig = got.signature.as_deref().expect("应带签名头");
    assert!(sig.starts_with("sha256="), "sig={sig}");
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
    mac.update(&got.body);
    assert_eq!(
        sig,
        format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        "签名不匹配 → 验签会失败"
    );
    // 契约验签函数（接收端同源）认可该签名。
    assert!(cmx_mdm_sdk::verify_signature(key, &got.body, Some(sig)));
}

#[tokio::test]
async fn disabled_sender_sends_nothing() {
    let (url, mut rx) = spawn_receiver(0).await;
    // 空目标列表 → disabled。
    let sender = WebhookSender::spawn_worker(vec![], None, 0);
    assert!(!sender.is_enabled());
    sender.emit(FlowEvent::new(
        FlowEventKind::InstanceStarted,
        "x",
        "t".to_string(),
    ));
    let _ = url;
    let got = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
    assert!(got.is_err(), "disabled 不应发出任何请求");
}

/// 契约常量静态断言（防路径 / 键漂移）。
#[test]
fn sdk_contract_constants() {
    assert_eq!(cmx_mdm_sdk::paths::FLOW_CALLBACK, "/api/mdm/flow/callback");
    assert_eq!(cmx_mdm_sdk::SERVICE_KEY, "mdm");
}
