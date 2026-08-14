//! 流程引擎微服务自持的前端页只读投递（native + html；一芯双壳的前端页面壳）。
//!
//! 门户的 native/html page 是 **API-backed**（非静态文件）：源码存磁盘 + 索引，经
//! `GET /api/{native,html}-pages/{id}` 读出 `ApiResp<...>` 返回，shell 用 API 取页面内容。
//! 本模块让 flow-server 用**字节对齐门户的信封**自投递流程那 3 个 native 页 + 1 个 html 页——
//! 门户 F3 反代取页请求到本服务时，响应与门户内嵌路径逐字节一致，shell 零感知。
//!
//! 信封走 flow-app 自持的 [`crate::resp`]（不借 cmx-api-types，与引擎其余 handler 一致；
//! `{code,msg,data}` 及 404/business 码均对齐门户 cmx-api-types）。
//! rev = xxhash64(bytes, 0) → 16-hex（字节对齐门户 cmx-jsonstore::content_rev）。
//! 目录 env：native `FLOW_UI_DIR`（默认 `web/ui-native`）、html `FLOW_UI_HTML_DIR`（默认 `web/ui-html`）。

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path as AxPath, Query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::resp::{ApiResp, FlowError, Result};

// ============================================================================
// native-pages（扁平索引 index.json + 源文件 <ui_dir>/<relPath>）
// ============================================================================

/// 与门户 `NativePageFull` 同字段同序（serde 驼峰 sourceType），保证反代响应逐字节一致。
#[derive(Debug, Clone, Serialize)]
pub struct NativePageFull {
    pub id: String,
    pub name: String,
    pub details: String,
    #[serde(rename = "sourceType")]
    pub source_type: String,
    #[serde(rename = "relPath")]
    pub rel_path: String,
    pub rev: String,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
struct IndexEntry {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    details: String,
    #[serde(default, rename = "sourceType")]
    source_type: String,
    #[serde(rename = "relPath")]
    rel_path: String,
}

#[derive(Debug, Deserialize)]
struct IndexFile {
    #[serde(default)]
    pages: Vec<IndexEntry>,
}

/// native UI 目录（env `FLOW_UI_DIR`，默认 `web/ui-native`）。
fn ui_dir() -> PathBuf {
    let d = std::env::var("FLOW_UI_DIR").unwrap_or_else(|_| "web/ui-native".to_string());
    PathBuf::from(d)
}

/// 读页面索引（`<ui_dir>/index.json`）。失败 → 空集（降级，绝不 500 整个服务）。
fn read_index() -> Vec<IndexEntry> {
    let p = ui_dir().join("index.json");
    match std::fs::read_to_string(&p) {
        Ok(t) => serde_json::from_str::<IndexFile>(&t).map(|f| f.pages).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 安全拼接源文件路径（禁止 `..` 越界，relPath 只能落在 ui_dir 内）。
fn source_abs(rel: &str) -> Option<PathBuf> {
    if rel.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return None;
    }
    let mut p = ui_dir();
    for seg in rel.split('/') {
        p.push(seg);
    }
    Some(p)
}

/// rev = xxhash64(bytes, 0) → 16-hex（字节对齐门户 cmx-jsonstore::content_rev）。
fn content_rev(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh64::xxh64(bytes, 0))
}

fn source_type_from_rel(rel: &str) -> String {
    let l = rel.to_lowercase();
    if l.ends_with(".js") || l.ends_with(".mjs") { "js".into() }
    else if l.ends_with(".html") || l.ends_with(".htm") { "html".into() }
    else { String::new() }
}

/// 由索引项 + 源码组装 NativePageFull（源文件缺失 → NotFound）。
fn load_full(e: &IndexEntry) -> Result<NativePageFull> {
    let abs = source_abs(&e.rel_path)
        .ok_or_else(|| FlowError::business(format!("native page relPath 非法: {}", e.rel_path)))?;
    let source = std::fs::read_to_string(&abs)
        .map_err(|_| FlowError::not_found(format!("native page 源文件缺失: {}", e.rel_path)))?;
    let rev = content_rev(source.as_bytes());
    Ok(NativePageFull {
        id: e.id.clone(),
        name: e.name.clone(),
        details: e.details.clone(),
        source_type: if e.source_type.is_empty() { source_type_from_rel(&e.rel_path) } else { e.source_type.clone() },
        rel_path: e.rel_path.clone(),
        rev,
        source,
    })
}

/// `GET /native-pages/{id}` —— 单条含源码。
pub async fn get_native_page(AxPath(id): AxPath<String>) -> Result<Json<ApiResp<NativePageFull>>> {
    let idx = read_index();
    let e = idx.iter().find(|e| e.id == id)
        .ok_or_else(|| FlowError::not_found(format!("native page 不存在: {id}")))?;
    Ok(Json(ApiResp::ok(load_full(e)?)))
}

#[derive(Debug, Deserialize)]
pub struct BatchReq {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// `POST /native-pages/batch` —— 批量取源码（body:{ids:[]}）。返回 {items:[NativePageFull]}。
pub async fn batch_native_pages(Json(req): Json<BatchReq>) -> Result<Json<ApiResp<Value>>> {
    let idx = read_index();
    let mut items = Vec::new();
    for id in &req.ids {
        if let Some(e) = idx.iter().find(|e| &e.id == id) {
            if let Ok(full) = load_full(e) {
                items.push(serde_json::to_value(full).unwrap_or(Value::Null));
            }
        }
    }
    Ok(Json(ApiResp::ok(json!({ "items": items }))))
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default, rename = "pageSize")]
    pub page_size: Option<u32>,
}

/// `GET /native-pages?page=&pageSize=` —— 分页列表（不含源码，对齐门户 list）。
pub async fn list_native_pages(Query(q): Query<PageQuery>) -> Result<Json<ApiResp<Value>>> {
    let idx = read_index();
    let total = idx.len();
    let page = q.page.unwrap_or(1).max(1) as usize;
    let size = q.page_size.unwrap_or(50).max(1) as usize;
    let start = (page - 1) * size;
    let items: Vec<Value> = idx.iter().skip(start).take(size).map(|e| json!({
        "id": e.id, "name": e.name, "details": e.details,
        "sourceType": if e.source_type.is_empty() { source_type_from_rel(&e.rel_path) } else { e.source_type.clone() },
        "relPath": e.rel_path,
    })).collect();
    Ok(Json(ApiResp::ok(json!({
        "items": items, "total": total, "page": page, "pageSize": size,
    }))))
}

// ============================================================================
// html-pages（v2：manifest index.json + 分片 index/<domain>.pages.json + 命名空间源）
// ----------------------------------------------------------------------------
// id 为 domain.app.module.page 命名空间；单页响应 = {id,name,details,domain,app,module,doc,relPath,
// rev,html}，rev=xxhash64(html)。流程拥有 fi.cmxfico.gl.flow-pay-review-form（待办办理表单）。
// 目录 env FLOW_UI_HTML_DIR 默认 web/ui-html。字节对齐门户 cmx-form/pages/html.rs::read_full_from_row。
// ============================================================================

fn html_dir() -> PathBuf {
    let d = std::env::var("FLOW_UI_HTML_DIR").unwrap_or_else(|_| "web/ui-html".to_string());
    PathBuf::from(d)
}

#[derive(Debug, Clone, Deserialize)]
struct HtmlRow {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    details: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    module: String,
    #[serde(default)]
    doc: String,
    #[serde(rename = "relPath")]
    rel_path: String,
}

#[derive(Debug, Deserialize)]
struct HtmlManifest {
    #[serde(default)]
    domains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HtmlShard {
    #[serde(default)]
    pages: Vec<HtmlRow>,
}

/// 读全部 html 行（遍历 manifest 声明的每个域分片）。失败 → 空集（降级）。
fn read_html_rows() -> Vec<HtmlRow> {
    let base = html_dir();
    let manifest: HtmlManifest = std::fs::read_to_string(base.join("index.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(HtmlManifest { domains: Vec::new() });
    let mut rows = Vec::new();
    for dom in &manifest.domains {
        let shard = base.join("index").join(format!("{dom}.pages.json"));
        if let Ok(t) = std::fs::read_to_string(&shard) {
            if let Ok(s) = serde_json::from_str::<HtmlShard>(&t) {
                rows.extend(s.pages);
            }
        }
    }
    rows
}

fn html_source_abs(rel: &str) -> Option<PathBuf> {
    if rel.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return None;
    }
    let mut p = html_dir();
    for seg in rel.split('/') {
        p.push(seg);
    }
    Some(p)
}

/// 由 html 行 + 源码组装门户同构 JSON（字段/序对齐 read_full_from_row）。
fn load_html_full(r: &HtmlRow) -> Result<Value> {
    let abs = html_source_abs(&r.rel_path)
        .ok_or_else(|| FlowError::business(format!("html page relPath 非法: {}", r.rel_path)))?;
    let html = std::fs::read_to_string(&abs)
        .map_err(|_| FlowError::not_found(format!("HTML 源码文件缺失或损坏: {}", r.rel_path)))?;
    let rev = content_rev(html.as_bytes());
    Ok(json!({
        "id": r.id, "name": r.name, "details": r.details,
        "domain": r.domain, "app": r.app, "module": r.module, "doc": r.doc,
        "relPath": r.rel_path, "rev": rev, "html": html,
    }))
}

/// `GET /html-pages/{id}` —— 单页含 html。
pub async fn get_html_page(AxPath(id): AxPath<String>) -> Result<Json<ApiResp<Value>>> {
    let rows = read_html_rows();
    let r = rows.iter().find(|r| r.id == id)
        .ok_or_else(|| FlowError::not_found(format!("html page 不存在: {id}")))?;
    Ok(Json(ApiResp::ok(load_html_full(r)?)))
}

/// `POST /html-pages/batch` —— 批量取页面。返回 {pages,revs,errors}（对齐门户）。
pub async fn batch_html_pages(Json(req): Json<BatchReq>) -> Result<Json<ApiResp<Value>>> {
    let rows = read_html_rows();
    let mut pages = Vec::new();
    let mut revs = serde_json::Map::new();
    let mut errors = Vec::new();
    for id in &req.ids {
        match rows.iter().find(|r| &r.id == id) {
            Some(r) => match load_html_full(r) {
                Ok(full) => {
                    if let Some(rev) = full.get("rev").and_then(|v| v.as_str()) {
                        revs.insert(id.clone(), Value::String(rev.to_string()));
                    }
                    pages.push(full);
                }
                Err(_) => errors.push(json!({ "id": id, "error": "源码缺失" })),
            },
            None => errors.push(json!({ "id": id, "error": "不存在" })),
        }
    }
    Ok(Json(ApiResp::ok(json!({ "pages": pages, "revs": revs, "errors": errors }))))
}

#[derive(Debug, Deserialize)]
pub struct HtmlListQuery {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default, rename = "pageSize")]
    pub page_size: Option<u32>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
}

/// `GET /html-pages?page=&pageSize=&domain=&app=&module=&keyword=` —— 分页列表（不含 html）。
pub async fn list_html_pages(Query(q): Query<HtmlListQuery>) -> Result<Json<ApiResp<Value>>> {
    let rows = read_html_rows();
    let filtered: Vec<&HtmlRow> = rows.iter().filter(|r| {
        q.domain.as_ref().map(|d| &r.domain == d).unwrap_or(true)
            && q.app.as_ref().map(|a| &r.app == a).unwrap_or(true)
            && q.module.as_ref().map(|m| &r.module == m).unwrap_or(true)
            && q.keyword.as_ref().map(|k| r.id.contains(k) || r.name.contains(k)).unwrap_or(true)
    }).collect();
    let total = filtered.len();
    let page = q.page.unwrap_or(1).max(1) as usize;
    let size = q.page_size.unwrap_or(50).max(1) as usize;
    let start = (page - 1) * size;
    let items: Vec<Value> = filtered.iter().skip(start).take(size).map(|r| json!({
        "id": r.id, "name": r.name, "details": r.details,
        "domain": r.domain, "app": r.app, "module": r.module, "relPath": r.rel_path,
    })).collect();
    Ok(Json(ApiResp::ok(json!({
        "items": items, "total": total, "page": page, "pageSize": size,
    }))))
}

/// 前端页面只读路由（native + html 并列，挂 flow-server /api 下，前缀与门户一致）。
/// 门户 F3 反代 flow 拥有的 native/html 页取页请求到本服务；独立运行时也自投递自己的界面。
pub fn frontend_pages_routes<S>() -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/native-pages", get(list_native_pages))
        .route("/native-pages/batch", post(batch_native_pages))
        .route("/native-pages/{id}", get(get_native_page))
        .route("/html-pages", get(list_html_pages))
        .route("/html-pages/batch", post(batch_html_pages))
        .route("/html-pages/{id}", get(get_html_page))
}
