//! Webhook 通道集成测试（001 方案 §4.3）：进程内 axum 接收桩（伪装外部订阅方），
//! 经 `cmx-service-rpc` 基座目录（键 "mdm" → 桩 URL）真实走 [`WebhookChannel::deliver`]，
//! 验证：wire 契约（三头 + HMAC 签名对拍）原样保持、2xx → Success、5xx → Retryable、
//! 4xx → Fatal（直达 DEAD 分类）、短超时覆盖生效（test 端点同款形态）。
//!
//! 基座全局句柄进程内单例：断言合并为顺序测试（共享桩 + 一次目录 install），
//! 避免并行测试争抢全局目录。

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use axum::{Router, extract::State, http::HeaderMap, routing::post};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use cmx_flow_adapters::{DeliveryChannel, DeliveryOutcome, DeliveryTask, WebhookChannel, global_registry};

type HmacSha256 = Hmac<Sha256>;

const SECRET: &str = "channel-test-secret";

#[derive(Clone)]
struct StubState {
    /// 前 N 次返回的状态码（0 = 恒 200）。
    fail_status: Arc<AtomicU16>,
    tx: tokio::sync::mpsc::UnboundedSender<Received>,
}

#[derive(Debug)]
struct Received {
    headers: HeaderMap,
    body: Vec<u8>,
}

fn task() -> DeliveryTask {
    DeliveryTask {
        subscription_name: "桩订阅".into(),
        event_type: "instance.started".into(),
        definition_key: Some("mdm_cr".into()),
        business_key: Some("BK-1".into()),
        instance_id: "i-1".into(),
        delivery_id: "i-1-t-9-2026-09-01T00:00:00Z".into(),
        payload: json!({ "event": "instance.started", "instanceId": "i-1" }),
    }
}

/// 起接收桩，返回 (base_url, 状态码开关, 接收队列)。
async fn spawn_stub() -> (String, Arc<AtomicU16>, tokio::sync::mpsc::UnboundedReceiver<Received>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let fail_status = Arc::new(AtomicU16::new(0));
    let state = StubState { fail_status: fail_status.clone(), tx };
    let app = Router::new()
        .route(
            "/api/mdm/flow/callback",
            post(
                |State(st): State<StubState>, headers: HeaderMap, body: axum::body::Bytes| async move {
                    st.tx.send(Received { headers, body: body.to_vec() }).ok();
                    let code = st.fail_status.load(Ordering::SeqCst);
                    if code == 0 {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::from_u16(code).unwrap_or(axum::http::StatusCode::OK)
                    }
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind 桩端口");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve 桩") });
    (format!("http://{addr}"), fail_status, rx)
}

/// 把基座全局目录装成「mdm → 桩 / slow → 不可达」（进程内一次 install，两键齐配）。
fn install_base(stub_url: &str) {
    let mut cfg = cmx_service_rpc::ServiceRpcConfig::default();
    cfg.services.insert(
        "mdm".to_string(),
        cmx_service_rpc::ServiceEntry {
            url: Some(stub_url.to_string()),
            ..Default::default()
        },
    );
    cfg.services.insert(
        "slow".to_string(),
        cmx_service_rpc::ServiceEntry {
            // 端口 1 不可达：连接级失败/超时 → Retryable 分类的传输路径。
            url: Some("http://127.0.0.1:1".to_string()),
            ..Default::default()
        },
    );
    let _ = cmx_service_rpc::install(cmx_service_rpc::ServiceRpcHandle::new(cfg));
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// 顺序主测试：成功 / 重试分类 / 死信分类 / 事件头与签名。
#[tokio::test]
async fn webhook_channel_full_chain() {
    let registry = global_registry();
    // 注册表含 webhook 通道（kafka/rabbitmq 未启用 feature，天然不出现）。
    assert_eq!(registry.types(), vec!["webhook"]);

    let (base, fail_status, mut rx) = spawn_stub().await;
    install_base(&base);
    let ch = WebhookChannel;
    let config = json!({
        "service_key": "mdm",
        "callback_path": "/api/mdm/flow/callback",
        "secret": SECRET,
    });

    // —— 1) 成功链路：wire 契约原样（三头 + 对 body 字节的 HMAC 签名 + 2xx → Success）。
    let outcome = ch.deliver(&config, &task(), None).await;
    assert!(matches!(outcome, DeliveryOutcome::Success), "2xx 应 Success");
    let rec = rx.recv().await.expect("桩未收到请求");
    assert_eq!(
        rec.headers.get("x-cmx-flow-event").and_then(|v| v.to_str().ok()),
        Some("instance.started")
    );
    assert_eq!(
        rec.headers.get("x-cmx-flow-delivery").and_then(|v| v.to_str().ok()),
        Some("i-1-t-9-2026-09-01T00:00:00Z")
    );
    assert_eq!(
        rec.headers.get("x-cmx-flow-signature").and_then(|v| v.to_str().ok()),
        Some(sign(SECRET, &rec.body).as_str()),
        "签名应对实际 body 字节可对拍"
    );

    // —— 2) 500 → Retryable（进退避重试，不是死信）。
    fail_status.store(500, Ordering::SeqCst);
    let outcome = ch.deliver(&config, &task(), None).await;
    assert!(matches!(outcome, DeliveryOutcome::Retryable { http_status: Some(500), .. }));
    let _ = rx.recv().await;

    // —— 3) 404（非重试类 4xx）→ Fatal：契约/配置性错误直达 DEAD。
    fail_status.store(404, Ordering::SeqCst);
    let outcome = ch.deliver(&config, &task(), None).await;
    assert!(matches!(outcome, DeliveryOutcome::Fatal { http_status: Some(404), .. }));

    // —— 4) 401 → Fatal（基座映射 AuthRejected：密钥/目录配置性错误，直达 DEAD）。
    fail_status.store(401, Ordering::SeqCst);
    let outcome = ch.deliver(&config, &task(), None).await;
    assert!(matches!(outcome, DeliveryOutcome::Fatal { .. }), "401 应 Fatal，实际 {outcome:?}");
    fail_status.store(0, Ordering::SeqCst);

    // —— 5) 短超时覆盖（test 端点同款形态）：不可达键 + 50ms 超时 → Retryable（传输级可重试）。
    let config_slow = json!({ "service_key": "slow", "callback_path": "/x", "secret": "s" });
    let outcome = ch.deliver(&config_slow, &task(), Some(Duration::from_millis(50))).await;
    assert!(
        matches!(outcome, DeliveryOutcome::Retryable { .. }),
        "超时/不可达应 Retryable，实际 {outcome:?}"
    );
}

/// 契约常量静态断言（防头名漂移——接收端按文档实现的锚点；承接自已删除的 legacy 集成测试）。
#[test]
fn webhook_contract_constants() {
    assert_eq!(cmx_flow_adapters::SIGNATURE_HEADER, "x-cmx-flow-signature");
    assert_eq!(cmx_flow_adapters::EVENT_HEADER, "x-cmx-flow-event");
    assert_eq!(cmx_flow_adapters::DELIVERY_HEADER, "x-cmx-flow-delivery");
}
