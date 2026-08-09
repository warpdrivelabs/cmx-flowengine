//! 流程引擎监控大盘（根路径页面）。
//!
//! `GET /` —— 返回自包含单页 HTML 大盘（内联 CSS/JS，零外部依赖，canvas 手绘图表，
//! light/dark 双主题，每 5s 轮询 `/api/flow/v1/stats`）。HTML 经 `include_str!` 编进二进制，
//! distroless 运行也无需外部文件。替代原先根路径 404 白屏。

use axum::response::Html;

/// 大盘 HTML（编译期内嵌）。
const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

/// 根路径大盘页。
pub async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}
