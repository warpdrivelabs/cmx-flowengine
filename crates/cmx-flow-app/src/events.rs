//! 进程内事件总线 + SSE 事件流（S3 headless）。
//!
//! S1 的生命周期事件（FlowEvent）除了出站 webhook（POST 第三方），也广播到进程内 tokio broadcast，
//! 供 SSE 端点 `GET /api/flow/v1/events` 实时推给订阅者——第三方 UI 增量刷新替轮询。
//!
//! 隔离：SSE 按 `current_tenant()` 过滤，只推本租户事件（对齐 S2 多租户隔离，跨租户不泄漏）。
//! 解耦：与 webhook 独立——webhook 关也能 SSE；无订阅者时 broadcast 静默丢，不阻塞 emit。

use std::convert::Infallible;
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::Query;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use cmx_flow_adapters::FlowEvent;

use crate::tenant::current_tenant;

/// 事件总线容量（缓冲未及消费的事件；满则最老的被丢，SSE 侧忽略 Lagged 继续）。
const BUS_CAPACITY: usize = 512;

static EVENT_BUS: OnceLock<broadcast::Sender<FlowEvent>> = OnceLock::new();

fn bus() -> &'static broadcast::Sender<FlowEvent> {
    EVENT_BUS.get_or_init(|| broadcast::channel(BUS_CAPACITY).0)
}

/// 广播一条事件到进程内总线（emit 时与 webhook 并发调）。无订阅者则静默丢。
pub fn publish(event: FlowEvent) {
    // send 只在无接收者时 Err——正常（没人订阅 SSE），忽略。
    let _ = bus().send(event);
}

/// 订阅事件总线（SSE handler 用）。
pub fn subscribe() -> broadcast::Receiver<FlowEvent> {
    bus().subscribe()
}

/// SSE 查询参数：可选按 user 再筛（tenant 由认证上下文定，不从 query 取——防跨租户越权）。
#[derive(Debug, Deserialize)]
pub struct SseQuery {
    /// 只推 assignee = 此 user 的 task 事件（可选）。
    #[serde(default)]
    user: Option<String>,
}

/// `GET /api/flow/v1/events` —— SSE 实时事件流。
///
/// 按当前租户过滤（认证中间件建的 scope）；可选 `?user=` 再按办理人筛 task 事件。
/// 每条事件：`event: <名>`（instance.started 等）+ `data: <FlowEvent JSON>`。
pub async fn sse_events(
    Query(q): Query<SseQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let tenant = current_tenant();
    let want_user = q.user;

    // 活跃连接计数：建连 +1；guard 随流一起被 Drop（客户端断开）时 -1。
    crate::observe::sse_connect();
    let guard = SseConnGuard;

    let stream = BroadcastStream::new(subscribe())
        // 忽略 Lagged（订阅者落后丢老事件）等错误，只取成功事件。
        .filter_map(|res| res.ok())
        // 租户隔离：只推本租户事件（事件无 tenant 字段的视为 default 兼容）。
        .filter(move |ev: &FlowEvent| {
            let ev_tenant = ev.tenant.as_deref().unwrap_or("default");
            ev_tenant == tenant
        })
        // 可选按 user 筛（只影响带 assignee 的 task 事件；不带 assignee 的实例事件仍推）。
        .filter(move |ev: &FlowEvent| match &want_user {
            Some(u) => ev.assignee.as_deref().map(|a| a == u).unwrap_or(true),
            None => true,
        })
        .map(move |ev: FlowEvent| {
            let _ = &guard; // 把 guard 移进流闭包，流 Drop 时 guard 一并 Drop → 连接 -1。
            let name = ev.event.clone();
            // json_data 失败几乎不可能（FlowEvent 可序列化）；失败则发个 comment 占位。
            Ok(Event::default()
                .event(name)
                .json_data(&ev)
                .unwrap_or_else(|_| Event::default().comment("serialize error")))
        });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// SSE 活跃连接 RAII 计数守卫：Drop（客户端断开 / 流结束）时递减活跃连接数。
struct SseConnGuard;
impl Drop for SseConnGuard {
    fn drop(&mut self) {
        crate::observe::sse_disconnect();
    }
}
