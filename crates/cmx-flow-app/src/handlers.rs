/*
 * @Describe: cmx-flow 的平台中立 axum handler：提取参数 → 取引擎单例 → 调引擎/定义服务 → ApiResp 信封。
 *
 * 抽核自 cmx-flow-api（原 cmx-flow-demo/main.rs 移植而来）。抽核差异：
 *   - 丢弃原来绑定不用的 State(_s)/CmxSvrContext(_ctx) 两提取器 → handler 与 AppState 类型无关，
 *     故路由 flow_routes::<S>() 对任意 state 泛型 S 成立，平台/独立两壳复用同一 handler。
 *   - 经 crate::engine::flow() 取 OnceCell 单例。
 *   - 返回 Result<Json<ApiResp<Value>>>，信封/错误来自 crate::resp（自持，不依赖 cmx-api）。
 * 响应 JSON 形状与抽核前完全一致（前端依赖 key/name/state/activeVersion/bpmnXml/instances/tasks…）。
 */

use axum::Json;
use axum::extract::{Path, Query};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::resp::{ApiResp, FlowError, Result};

use cmx_flow_engine::{
    AssigneeResolver, CandidateKind, CandidateRef, ResolveContext, RuntimeStore, Variables,
};
use cmx_flow_store_pg::{PgIamAssigneeResolver, PgSubflowRouter};

use crate::engine::{FlowRuntime, current_iam_db_id, flow};
use crate::views::{definition_view, instance_state_str, instance_view, summary_view};

use cmx_flow_adapters::{FlowEvent, FlowEventKind};

// ————————————————————— 出站 webhook emit 辅助 —————————————————————

/// 读实例 `variables.initiator`（T0b 授权基准：发起人判定一律以服务端变量为准）。
/// 实例不存在返回 Err；变量缺失返回 Ok(None)（老数据，调用方自行决定放行或收紧）。
async fn instance_initiator(rt: &FlowRuntime, id: &str) -> Result<Option<String>> {
    let snap = rt
        .engine
        .store()
        .load_snapshot(id)
        .await
        .map_err(|_| FlowError::not_found(format!("实例不存在: {id}")))?;
    Ok(snap
        .instance
        .variables
        .get("initiator")
        .and_then(|v| v.as_str())
        .map(String::from))
}

/// 当前时刻 RFC3339（webhook 事件 occurred_at）。
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 发一批生命周期事件：**双发**——出站 webhook（按 MODE 分流）+ 进程内 SSE 广播（S3，始终）。
/// 自动补上当前租户（SSE 按租户过滤）。同请求的事件在 emit 点收齐后单批写入（001 方案 §4.1）。
///
/// **SSE 半边不受 MODE 影响**：两种模式照常广播。webhook 半边：
/// - outbox（默认）→ 事件落投递行（订阅匹配 + uk 幂等），投递 poller 租约式异步投递；
/// - legacy → 现行内存链路（WebhookSender，保留至 M3）。
async fn publish_events(rt: &FlowRuntime, events: Vec<FlowEvent>) {
    if events.is_empty() {
        return;
    }
    let events: Vec<FlowEvent> = events
        .into_iter()
        .map(|e| e.tenant(Some(crate::tenant::current_tenant())))
        .collect();
    for e in &events {
        crate::events::publish(e.clone());
    }
    match crate::webhook_outbox::webhook_mode() {
        crate::webhook_outbox::WebhookMode::Outbox => {
            crate::webhook_outbox::emit_to_outbox(&events).await;
        }
        crate::webhook_outbox::WebhookMode::Legacy => {
            if rt.webhook.is_enabled() {
                for e in events {
                    rt.webhook.emit(e);
                }
            }
        }
    }
}

/// 发一条生命周期事件（[`publish_events`] 的单事件便捷形态）。
async fn publish_event(rt: &FlowRuntime, event: FlowEvent) {
    publish_events(rt, vec![event]).await;
}

/// 从实例快照投影出 state/definitionKey/businessKey（webhook 事件公共字段）。
async fn emit_instance_event(rt: &FlowRuntime, kind: FlowEventKind, instance_id: &str) {
    // 借快照补齐展示字段；取不到就只带 instance_id（事件是通知，不因取数失败阻断）。
    let (state, def_key, biz_key) = match rt.engine.store().load_snapshot(instance_id).await {
        Ok(snap) => (
            Some(instance_state_str(snap.instance.state).to_string()),
            Some(snap.instance.definition_key.clone()),
            snap.instance.business_key.clone(),
        ),
        Err(_) => (None, None, None),
    };
    publish_event(
        rt,
        FlowEvent::new(kind, instance_id, now_rfc3339())
            .state(state)
            .definition_key(def_key)
            .business_key(biz_key),
    )
    .await;
}

/// 为 ExecutionResult 的每个当前未办结任务 emit 一条 task 事件（task.created / task.reassigned）。
/// 同一次推进的多条事件收齐后**单批写入**（001 方案 §4.1）。
async fn emit_task_events(
    rt: &FlowRuntime,
    kind: FlowEventKind,
    result: &cmx_flow_engine::ExecutionResult,
) {
    if result.open_tasks.is_empty() {
        return;
    }
    // 补齐 definitionKey/businessKey（借快照一次）。
    let (def_key, biz_key) = match rt.engine.store().load_snapshot(&result.instance_id).await {
        Ok(snap) => (
            Some(snap.instance.definition_key.clone()),
            snap.instance.business_key.clone(),
        ),
        Err(_) => (None, None),
    };
    let ts = now_rfc3339();
    let events = result
        .open_tasks
        .iter()
        .map(|t| {
            FlowEvent::new(kind, &result.instance_id, ts.clone())
                .definition_key(def_key.clone())
                .business_key(biz_key.clone())
                .task(Some(t.id.clone()), Some(t.node_bpmn_id.clone()))
                .assignee(t.assignee.clone())
        })
        .collect();
    publish_events(rt, events).await;
}

/// emit 一条 task.reassigned（转办/委派/加签后新办理人 = to_user）。
///
/// 001 方案 §4.1 前置补齐：**补设 definition_key**（此前 2/6 事件载荷不带，订阅按
/// definitionKey 过滤的前提不成立——借快照一并取 node_bpmn_id）。
async fn emit_reassigned(rt: &FlowRuntime, instance_id: &str, task_id: &str, to_user: &str) {
    // 补 node_bpmn_id + definition_key（借快照找该任务；找不到就不带）。
    let (node, def_key) = rt
        .engine
        .store()
        .load_snapshot(instance_id)
        .await
        .ok()
        .map(|snap| {
            let node = snap
                .tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.node_bpmn_id.clone());
            (node, Some(snap.instance.definition_key.clone()))
        })
        .unwrap_or((None, None));
    publish_event(
        rt,
        FlowEvent::new(FlowEventKind::TaskReassigned, instance_id, now_rfc3339())
            .definition_key(def_key)
            .task(Some(task_id.to_string()), node)
            .assignee(Some(to_user.to_string())),
    )
    .await;
}

// ————————————————————— 错误桥 —————————————————————

fn engine_err(e: cmx_flow_engine::Error) -> FlowError {
    FlowError::business_error(e.to_string())
}
fn def_err(e: cmx_flow_def::DefError) -> FlowError {
    FlowError::business_error(e.to_string())
}
fn msg_err(msg: String) -> FlowError {
    FlowError::business_error(msg)
}

/// 载入实例并返回视图信封（多个 handler 共用）。
async fn load_view(rt: &FlowRuntime, instance_id: &str) -> Result<Json<ApiResp<Value>>> {
    let snap = rt
        .engine
        .store()
        .load_snapshot(instance_id)
        .await
        .map_err(|e| msg_err(format!("载入实例失败: {e}")))?;
    Ok(Json(ApiResp::ok(instance_view(&snap))))
}

// ————————————————————— 定义（设计器） —————————————————————

/// 全部流程定义 → 前端画图用的 JSON（每个含节点 + 边）。来源引擎已装载定义（运行态视角）。
pub async fn get_definitions(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let defs: Vec<Value> = rt
        .definitions
        .read()
        .await
        .iter()
        .map(definition_view)
        .collect();
    Ok(Json(ApiResp::ok(json!({ "definitions": defs }))))
}

/// 设计器用的定义列表 → 来源定义库（草稿 + 已发布全都列，设计态视角）。
/// 与 get_definitions 区别：那个是引擎运行态已装载的；这个是库里所有可编辑的定义。
/// 每条附带版本序列（versions[]，版本号降序，含变更说明），activeVersion=当前生效版本。
pub async fn list_design_definitions(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let recs = rt.def_svc.list().await.map_err(def_err)?;
    // 一次取全部版本，按 def_key 分组（省 N+1）。
    let all_vers = rt.def_svc.list_all_versions().await.map_err(def_err)?;
    // 「哪些定义是子流程」= 被引用为子流程目标的 key 集合（派生，零 schema）：
    //   ① 固定引用：任一 callActivity 的 calledElement（遍历运行态 IR）。
    //   ② 组织路由：任一 subflow_binding.target_definition_key（绑定表）。
    // 被引用者不在主流程列表展示（前端据 isSubflow 过滤）。
    let mut subflow_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        use cmx_flow_model::NodeKind;
        let defs_ir = rt.definitions.read().await;
        for d in defs_ir.iter() {
            for n in &d.nodes {
                if let NodeKind::CallActivity(ca) = &n.kind {
                    if !ca.called_element.is_empty() {
                        subflow_keys.insert(ca.called_element.clone());
                    }
                }
            }
        }
    }
    if let Ok(targets) = rt.binding_store.list_all_target_keys().await {
        subflow_keys.extend(targets);
    }
    let defs: Vec<Value> = recs
        .iter()
        .map(|r| {
            let versions: Vec<Value> = all_vers
                .iter()
                .filter(|v| v.def_key == r.key)
                .map(version_meta_view)
                .collect();
            json!({
                "key": r.key,
                "name": r.name,
                "domain": r.domain,
                "application": r.application,
                "module": r.module,
                "state": r.state.as_str(),
                "activeVersion": r.active_version,
                "versionCount": versions.len(),
                "versions": versions,
                "startable": true,
                "isSubflow": subflow_keys.contains(&r.key),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "definitions": defs }))))
}

/// 单条版本元信息 → 前端 JSON（对齐报表版本字段命名习惯）。
fn version_meta_view(v: &cmx_flow_def::VersionMeta) -> Value {
    json!({
        "version": v.version,
        "note": v.note,
        "publishedAt": v.published_at.to_rfc3339(),
        "publishedBy": v.published_by,
    })
}

/// 设计器：存草稿请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDraftReq {
    name: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    category: Option<String>,
    /// 设计器导出的 BPMN 2.0 XML。
    bpmn_xml: String,
    #[serde(default)]
    updated_by: Option<String>,
    /// 协同 M1 乐观锁：载入草稿时的 updatedAt（RFC3339）。当前草稿更新时间已推进则返回冲突不覆盖。
    #[serde(default)]
    base_updated_at: Option<String>,
}

/// 设计器：存草稿（先试编译挡回非法 BPMN）。
pub async fn save_definition_draft(
    Json(req): Json<SaveDraftReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let actor = req
        .updated_by
        .clone()
        .or_else(crate::tenant::current_display_user)
        .filter(|s| !s.is_empty());
    let outcome = rt
        .def_svc
        .save_draft_checked(
            &req.name,
            req.domain,
            req.application,
            req.module,
            req.category,
            &req.bpmn_xml,
            actor.clone(),
            req.base_updated_at,
        )
        .await
        .map_err(def_err)?;
    match outcome {
        cmx_flow_def::SaveDraftOutcome::Saved(rec) => {
            let updated_at = rec.updated_at.to_rfc3339();
            // 协同 M1：通知同草稿其他编辑者（谁存了 + 新时间戳，供「载入最新」）。
            crate::collab::publish_draft_saved(&rec.key, actor, &updated_at);
            Ok(Json(ApiResp::ok(json!({
                "key": rec.key,
                "name": rec.name,
                "state": rec.state.as_str(),
                "activeVersion": rec.active_version,
                "updatedAt": updated_at,
                "conflict": false,
            }))))
        }
        cmx_flow_def::SaveDraftOutcome::Conflict {
            current_updated_at,
            updated_by,
        } => Ok(Json(ApiResp::ok(json!({
            "conflict": true,
            "currentUpdatedAt": current_updated_at,
            "updatedBy": updated_by,
        })))),
    }
}

/// 设计器「校验」：只试编译 BPMN（真跑 compile + check_topology），**不落库**。
///
/// 与 save_draft 走同一道编译闸，但纯只读——设计器在保存前就能拿到真实诊断（无 start、
/// 死循环、网关无出口、引擎不支持的元素等），而非假的「有没有 process 字样」。
/// 语法/拓扑非法也回 HTTP 200 + `{valid:false,error}`，便于前端就地渲染，无需 catch。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateDefReq {
    /// 设计器导出的 BPMN 2.0 XML。
    bpmn_xml: String,
}

pub async fn validate_definition(
    Json(req): Json<ValidateDefReq>,
) -> Result<Json<ApiResp<Value>>> {
    match cmx_flow_def::validate_bpmn(&req.bpmn_xml) {
        Ok(key) => Ok(Json(ApiResp::ok(json!({ "valid": true, "key": key })))),
        Err(e) => Ok(Json(ApiResp::ok(json!({ "valid": false, "error": e.to_string() })))),
    }
}


/// 可选 ?version=N：取指定历史版本的 XML（版本切换用），不传则取当前草稿。
#[derive(Deserialize)]
pub struct DetailQuery {
    #[serde(default)]
    version: Option<i32>,
}

pub async fn get_definition_detail(
    Path(key): Path<String>,
    Query(q): Query<DetailQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let rec = rt
        .def_svc
        .get(&key)
        .await
        .map_err(def_err)?
        .ok_or_else(|| msg_err(format!("定义不存在: {key}")))?;
    // 版本列表（降序）。
    let versions: Vec<Value> = rt
        .def_svc
        .list_versions(&key)
        .await
        .map_err(def_err)?
        .iter()
        .map(version_meta_view)
        .collect();
    // ?version=N → 取该历史版本 XML；否则用当前草稿。
    let (xml, shown_version) = if let Some(vn) = q.version {
        let ver = rt
            .def_svc
            .get_version(&key, vn)
            .await
            .map_err(def_err)?
            .ok_or_else(|| msg_err(format!("版本不存在: {key}@v{vn}")))?;
        (Some(ver.bpmn_xml), Some(vn))
    } else {
        (rec.draft_xml.clone(), rec.active_version)
    };
    Ok(Json(ApiResp::ok(json!({
        "key": rec.key,
        "name": rec.name,
        "domain": rec.domain,
        "application": rec.application,
        "module": rec.module,
        "category": rec.category,
        "state": rec.state.as_str(),
        "activeVersion": rec.active_version,
        "shownVersion": shown_version,
        "versions": versions,
        "bpmnXml": xml,
        "updatedAt": rec.updated_at.to_rfc3339(),
    }))))
}

/// ⑤：取一个定义的变量声明 + 摊平路径列表（设计器下拉的唯一数据源）。`?version=N` 看指定版本。
///
/// `GET /definitions/{key}/variables?version=N`
pub async fn get_definition_variables(
    Path(key): Path<String>,
    Query(q): Query<DetailQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let rec = rt
        .def_svc
        .get(&key)
        .await
        .map_err(def_err)?
        .ok_or_else(|| msg_err(format!("定义不存在: {key}")))?;
    let xml = if let Some(vn) = q.version {
        rt.def_svc
            .get_version(&key, vn)
            .await
            .map_err(def_err)?
            .map(|v| v.bpmn_xml)
            .ok_or_else(|| msg_err(format!("版本不存在: {key}@v{vn}")))?
    } else {
        rec.draft_xml
            .clone()
            .ok_or_else(|| msg_err(format!("定义 {key} 无草稿 XML")))?
    };
    // 编译取 var_schema（坏 XML → 空 schema，不阻断下拉）。
    let schema = cmx_flow_bpmn::compile(&xml)
        .ok()
        .and_then(|d| d.var_schema)
        .unwrap_or_default();
    let paths: Vec<Value> = schema
        .flatten_paths()
        .iter()
        .map(|p| {
            json!({
                "path": p.path,
                "type": p.var_type.as_str(),
                "label": p.label,
                "description": p.description,
                "required": p.required,
                "isCollection": p.is_collection,
                "enumOptions": p.enum_options,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "key": key,
        "schema": schema,
        "paths": paths,
    }))))
}

/// ⑤：校验一份变量声明的 shape（设计器保存前用）。body = { schema: [VarDecl...] }。
#[derive(Deserialize)]
pub struct ValidateVarsReq {
    schema: cmx_flow_model::VarSchema,
}

pub async fn validate_definition_variables(
    Json(req): Json<ValidateVarsReq>,
) -> Result<Json<ApiResp<Value>>> {
    let violations = req.schema.validate_shape();
    let items: Vec<Value> = violations
        .iter()
        .map(|v| json!({ "var": v.var, "code": format!("{:?}", v.code), "message": v.message }))
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "valid": violations.is_empty(),
        "violations": items,
    }))))
}

/// 设计器：发布请求。note = 本次发布的变更说明（可空）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReq {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    published_by: Option<String>,
}

/// H1 热装载：取指定版本的 XML → 编译 → deploy 到运行引擎 + 刷新 rt.definitions。
/// 返回是否成功热装载。任一步失败仅告警不回滚（版本已落库）。publish/activate 共用。
async fn hot_load_version(rt: &FlowRuntime, key: &str, version: i32) -> bool {
    match rt.def_svc.get_version(key, version).await {
        Ok(Some(ver)) => match cmx_flow_bpmn::compile(&ver.bpmn_xml) {
            Ok(def) => {
                if let Err(e) = rt.engine.deploy(def.clone()) {
                    tracing::warn!(key = %key, error = %e, "热装载 deploy 失败");
                    false
                } else {
                    let mut defs = rt.definitions.write().await;
                    if let Some(slot) = defs.iter_mut().find(|d| d.key == def.key) {
                        *slot = def;
                    } else {
                        defs.push(def);
                    }
                    tracing::info!(key = %key, version, "已热装载定义版本");
                    true
                }
            }
            Err(e) => {
                tracing::warn!(key = %key, error = %e, "热装载编译失败");
                false
            }
        },
        Ok(None) => {
            tracing::warn!(key = %key, version, "热装载取版本 XML 为空");
            false
        }
        Err(e) => {
            tracing::warn!(key = %key, error = %e, "热装载取版本失败");
            false
        }
    }
}

/// 强校验 D2：对一段 BPMN XML 过发布闸（compile → required×策略检查）。
/// publish / activate 两入口共用；开关关闭时零影响直接放行。
fn ensure_publishable_xml(xml: &str) -> Result<()> {
    if !crate::publish_gate::gate_enabled() {
        return Ok(());
    }
    let def = cmx_flow_bpmn::compile(xml)
        .map_err(|e| FlowError::bad_request(format!("BPMN 编译失败: {e}")))?;
    crate::publish_gate::check_required_needs_strict(
        def.var_schema.as_ref(),
        def.var_validation.as_deref(),
    )
    .map_err(FlowError::bad_request)
}

/// 设计器：发布（草稿 → 版本 +1）。**H1：发布即热装载到运行引擎，无需重启。**
pub async fn publish_definition(
    Path(key): Path<String>,
    Json(req): Json<PublishReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    // 强校验 D2 发布闸：required×非 strict 拒绝发布（文案带出路）；在落库之前拦截，
    // 历史 lenient+required 版本不回溯、重新发布时受闸。
    {
        let xml = rt
            .def_svc
            .get(&key)
            .await
            .map_err(def_err)?
            .and_then(|r| r.draft_xml)
            .ok_or_else(|| FlowError::bad_request(format!("无可发布的草稿: {key}")))?;
        ensure_publishable_xml(&xml)?;
    }

    let version = rt
        .def_svc
        .publish(&key, req.note, req.published_by)
        .await
        .map_err(def_err)?;

    let hot_loaded = hot_load_version(&rt, &key, version).await;

    Ok(Json(ApiResp::ok(json!({
        "key": key,
        "version": version,
        "hotLoaded": hot_loaded,
        "note": if hot_loaded { "已发布并热装载，立即生效" } else { "已发布；热装载失败，重启服务后生效" },
    }))))
}

/// 设计器：列某定义的全部版本（版本号降序）。
pub async fn list_definition_versions(
    Path(key): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let rec = rt
        .def_svc
        .get(&key)
        .await
        .map_err(def_err)?
        .ok_or_else(|| msg_err(format!("定义不存在: {key}")))?;
    let versions: Vec<Value> = rt
        .def_svc
        .list_versions(&key)
        .await
        .map_err(def_err)?
        .iter()
        .map(version_meta_view)
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "key": rec.key,
        "activeVersion": rec.active_version,
        "versions": versions,
    }))))
}

/// 设计器：激活指定版本为当前生效版本（对标报表「设为默认版本」）。重启后引擎装载生效。
pub async fn activate_definition_version(
    Path((key, version)): Path<(String, i32)>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    // 强校验 D2：activate 与 publish 同闸——直热装载历史版本不经 publish 校验，
    // 必须在此过闸否则历史 lenient+required 版本可经版本管理回切绕过。
    let ver_xml = rt
        .def_svc
        .get_version(&key, version)
        .await
        .map_err(def_err)?
        .ok_or_else(|| FlowError::not_found(format!("版本不存在: {key}@{version}")))?
        .bpmn_xml;
    ensure_publishable_xml(&ver_xml)?;

    rt.def_svc
        .activate_version(&key, version)
        .await
        .map_err(def_err)?;
    // U3：激活即热装载该版本到运行引擎（与 publish 一致），无需重启。
    let hot_loaded = hot_load_version(&rt, &key, version).await;
    Ok(Json(ApiResp::ok(json!({
        "key": key,
        "activeVersion": version,
        "hotLoaded": hot_loaded,
        "note": if hot_loaded { "已设为当前版本并热装载，立即生效" } else { "已设为当前版本；热装载失败，重启后生效" },
    }))))
}

/// 设计器：删除某历史版本（不能删当前生效版本）。
pub async fn delete_definition_version(
    Path((key, version)): Path<(String, i32)>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.def_svc
        .delete_version(&key, version)
        .await
        .map_err(def_err)?;
    Ok(Json(ApiResp::ok(
        json!({ "key": key, "deletedVersion": version }),
    )))
}

// ————————————————————— 实例 —————————————————————

/// 单据引用（F1）：表单产出的业务单据坐标，发起时随实例关联落库。
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BizLinkReq {
    biz_table: String,
    biz_id: String,
    #[serde(default)]
    biz_key: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// 启动实例请求（F1 通用化）。
///
/// 新形态：`variables` 携带任意业务/决策变量对象；`businessKey`/`orgId` 显式入参；
/// 可选 `bizLink` 发起即绑单据。为不破坏 demo/存量调用，保留旧字段
/// `applicant/amount/approvers` 作兼容垫片——`variables` 为空时从旧字段拼一个。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartReq {
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    business_key: Option<String>,
    #[serde(default)]
    org_id: Option<String>,
    /// 路由维度上下文（RD3）：`dim_key → dim_value`，各挂载点按 cmx:dimKey 取对应维度值路由。
    /// 缺省空；只传 orgId 等价 `{"org": orgId}`（引擎在实例构造时补投影）。
    #[serde(default)]
    dimensions: std::collections::BTreeMap<String, String>,
    /// 通用变量对象（首选）。
    #[serde(default)]
    variables: Value,
    /// 发起即绑业务单据（可选）。
    #[serde(default)]
    biz_link: Option<BizLinkReq>,
    // —— 向后兼容垫片（旧 demo/工作台调用形态） ——
    #[serde(default)]
    applicant: Option<String>,
    #[serde(default)]
    amount: Option<f64>,
    #[serde(default)]
    approvers: Option<Vec<String>>,
}

impl StartReq {
    /// 归一变量：优先 `variables`，为空则从旧字段拼兼容垫片。
    fn resolve_variables(&self) -> Variables {
        if let Value::Object(m) = &self.variables {
            if !m.is_empty() {
                return Variables::from_json(self.variables.clone());
            }
        }
        // 兼容垫片：旧调用只传 applicant/amount/approvers。
        let mut vars = Variables::new();
        if let Some(a) = &self.applicant {
            vars.set("applicant", json!(a));
        }
        if let Some(amt) = self.amount {
            vars.set("amount", json!(amt));
        }
        if let Some(ap) = &self.approvers {
            vars.set("approvers", json!(ap));
        }
        vars
    }
}

/// 启动一个流程实例。
pub async fn start_instance(
    Json(req): Json<StartReq>,
) -> Result<Json<ApiResp<Value>>> {
    // 强校验 D1：definitionKey 结构必传——trim 判空一并拦截，不再静默回落 demo 流程
    // credit_approval；拒绝语义为 BadRequest → HTTP 400。放运行时初始化（flow()）
    // 之前，DB 不可达也不影响结构校验先行。
    let def_key = req
        .definition_key
        .clone()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            FlowError::bad_request("缺少 definitionKey（不再回落 demo 流程 credit_approval）")
        })?;
    let rt = flow().await?;

    let mut vars = req.resolve_variables();
    // T0：initiator 缺失时从认证上下文兜底注入——「我发起的」过滤、撤销/取回护栏均按
    // variables.initiator 判定，存量 UI 发起不带它会导致列表失联与授权误判。
    let has_initiator = vars
        .get("initiator")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    if !has_initiator
        && let Some(user) = crate::tenant::current_user()
    {
        vars.set("initiator", json!(user));
    }
    // businessKey 优先显式入参；兼容垫片下从 applicant 拼一个。
    let biz_key = req
        .business_key
        .clone()
        .or_else(|| req.applicant.as_ref().map(|a| format!("CR-{a}")));

    // 变量历史：发起初值（vars 随即被 start 消费，先留一份 JSON）。
    let init_vars_json = vars.to_json();
    let result = rt
        .engine
        .start_process_dims(&def_key, vars, biz_key.clone(), req.org_id.clone(), req.dimensions.clone())
        .await
        .map_err(engine_err)?;

    // 变量历史：发起初值全部记为新增（old 空；by=发起人；source=start）。失败非致命。
    {
        let by = init_vars_json
            .get("initiator")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(crate::tenant::current_display_user);
        let vh = diff_var_changes(
            &result.instance_id,
            &init_vars_json,
            &Variables::from_json(json!({})),
            "start",
            None,
            by.as_deref(),
        );
        record_var_history(&rt, &vh).await;
    }

    // F1：若带 bizLink，回写单据↔实例关联；失败即取消实例（无孤儿），对客户端表现为发起失败。
    if let Some(link) = &req.biz_link {
        if let Err(e) = crate::biz_link::link_biz_to_instance(
            &rt,
            &result.instance_id,
            &link.biz_table,
            &link.biz_id,
            link.biz_key.clone().or_else(|| biz_key.clone()),
            link.role.clone(),
        )
        .await
        {
            // 补偿：取消刚起的实例，避免孤儿实例。
            let _ = rt
                .engine
                .cancel_process(&result.instance_id, Some("绑定业务单据失败自动回滚".into()))
                .await;
            return Err(msg_err(format!("绑定业务单据失败，已取消实例: {e}")));
        }
    }

    // 出站 webhook：实例已发起 + 每个初始待办任务已创建。
    emit_instance_event(&rt, FlowEventKind::InstanceStarted, &result.instance_id).await;
    emit_task_events(&rt, FlowEventKind::TaskCreated, &result).await;

    load_view(&rt, &result.instance_id).await
}

/// 列全部实例。
pub async fn list_instances(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let summaries = rt
        .engine
        .store()
        .list_instances(100)
        .await
        .map_err(|e| msg_err(format!("查询实例列表失败: {e}")))?;
    let instances: Vec<Value> = summaries.iter().map(summary_view).collect();
    Ok(Json(ApiResp::ok(json!({ "instances": instances }))))
}

/// 单实例详情。
pub async fn get_instance(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let snap = rt
        .engine
        .store()
        .load_snapshot(&id)
        .await
        .map_err(|_| msg_err(format!("实例不存在: {id}")))?;
    Ok(Json(ApiResp::ok(instance_view(&snap))))
}

/// 列某实例的子实例（M5.1 子流程）。
pub async fn get_children(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let children = rt
        .engine
        .store()
        .find_child_instances(&id)
        .await
        .map_err(|e| msg_err(format!("查询子实例失败: {e}")))?;
    let mut items = Vec::new();
    for c in &children {
        if let Ok(snap) = rt.engine.store().load_snapshot(&c.id).await {
            items.push(instance_view(&snap));
        }
    }
    Ok(Json(ApiResp::ok(json!({ "children": items }))))
}

#[derive(Deserialize)]
pub struct CancelReq {
    #[serde(default)]
    reason: Option<String>,
}

/// 撤单 / 取消一个流程实例（M3）。
///
/// T0b 授权：有用户身份时须为发起人（variables.initiator，服务端判定）；无身份放行——
/// 覆盖两类合法通道：宿主未挂 auth 中间件（平台内嵌形态，平台 mw_auth 兜底）与纯服务调用
/// （X-API-Key 无委托令牌，运维/业务后端代理）。initiator 缺失的老数据放行 + warn。
pub async fn cancel_instance(
    Path(id): Path<String>,
    Json(req): Json<CancelReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    if let Some(user) = crate::tenant::current_user() {
        let initiator = instance_initiator(&rt, &id).await?;
        match initiator {
            Some(init) if init != user => {
                return Err(FlowError::business_error(format!(
                    "无权取消该实例（非发起人）"
                )));
            }
            Some(_) => {}
            None => {
                tracing::warn!(instance = %id, "实例缺少 initiator 变量，取消操作放行（老数据）");
            }
        }
    }
    rt.engine
        .cancel_process(&id, req.reason)
        .await
        .map_err(engine_err)?;
    // 出站 webhook：实例已终止（撤单/取消）。
    emit_instance_event(&rt, FlowEventKind::InstanceTerminated, &id).await;
    load_view(&rt, &id).await
}

/// 撤回 / 取回（④）请求体。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawReq {
    /// 取回发起人（须为流程发起人）。
    user: String,
    #[serde(default)]
    reason: Option<String>,
}

/// 撤回 / 取回一个流程实例（④）——发起人在下游未处理时拉回发起处，可改后重交。
///
/// T0b 授权：以服务端身份为准（`current_user`==`variables.initiator`），body.user 仅作
/// 留痕回退——其来自前端 localStorage，与 JWT sub 可能不同源，不能作为鉴权依据。
/// auth 中间件已生效却拿不到用户身份（纯服务调用/委托令牌验签失败）→ 拒绝（取回是敏感动作）。
pub async fn withdraw_instance(
    Path(id): Path<String>,
    Json(req): Json<WithdrawReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let user = match crate::tenant::current_user() {
        Some(user) => {
            let initiator = instance_initiator(&rt, &id).await?;
            if let Some(init) = initiator
                && init != user
            {
                return Err(FlowError::business_error("无权取回该实例（非发起人）"));
            }
            user
        }
        None => {
            if crate::auth::auth_middleware_active() {
                return Err(FlowError::business_error(
                    "缺少用户身份，不能取回实例（需登录态或有效委托令牌）",
                ));
            }
            // 宿主未挂 auth（内嵌形态）：维持旧口径，以 body.user 为准。
            req.user.clone()
        }
    };
    let result = rt
        .engine
        .withdraw_process(&id, &user, req.reason.as_deref())
        .await
        .map_err(engine_err)?;
    // 取回落点的新任务归发起人 → 发一条 task.reassigned 事件（供待办刷新）。
    emit_task_events(&rt, FlowEventKind::TaskReassigned, &result).await;
    load_view(&rt, &id).await
}

/// 是否可撤回查询（④）：`?user=<uid>`。
#[derive(Deserialize)]
pub struct WithdrawableQuery {
    user: String,
}

/// 查一个实例是否可被某用户撤回（④，只读）——供前端「取回」按钮点亮/置灰 + 原因提示。
pub async fn get_withdrawable(
    Path(id): Path<String>,
    Query(q): Query<WithdrawableQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let (ok, reason) = rt
        .engine
        .can_withdraw(&id, &q.user)
        .await
        .map_err(engine_err)?;
    Ok(Json(ApiResp::ok(json!({ "withdrawable": ok, "reason": reason }))))
}

// ————————————————————— F1：变量 / 单据关联 —————————————————————

/// 只读取实例变量（表单办理态拉流程上下文用）。
pub async fn get_instance_variables(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let snap = rt
        .engine
        .store()
        .load_snapshot(&id)
        .await
        .map_err(|e| msg_err(format!("载入实例失败: {e}")))?;
    Ok(Json(ApiResp::ok(snap.instance.variables.to_json())))
}

#[derive(Deserialize)]
pub struct VarHistoryQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /instances/{id}/variables/history —— 变量变更历史（时间正序）。
pub async fn get_instance_var_history(
    Path(id): Path<String>,
    Query(q): Query<VarHistoryQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let limit = q.limit.unwrap_or(500).min(5000);
    let entries = rt
        .var_history_store
        .list_by_instance(&id, limit)
        .await
        .map_err(msg_err)?;
    let items: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "varName": e.var_name,
                "oldValue": e.old_value,
                "newValue": e.new_value,
                "source": e.source,
                "nodeBpmnId": e.node_bpmn_id,
                "changedBy": e.changed_by,
                "changedAt": e.changed_at,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(
        json!({ "instanceId": id, "total": items.len(), "history": items }),
    )))
}

#[derive(Deserialize)]
pub struct SweepReq {
    #[serde(default)]
    days: Option<i64>,
}

/// POST /admin/var-history/sweep —— TTL 归档：删除 days 天前的变量历史（默认 90 天）。
pub async fn sweep_var_history(Json(req): Json<SweepReq>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let days = req.days.unwrap_or(90);
    let deleted = rt
        .var_history_store
        .sweep_older_than_days(days)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "days": days, "deleted": deleted }))))
}

// ───────────────────── 身份 / 维度回连端点（⑤ + RD5 服务端） ─────────────────────
//
// 独立部署的 flow-server（无 IAM 库访问）经 HttpAssigneeResolver / HttpDimensionResolver 回连一个
// **有 IAM 访问的提供方**做身份/维度解析。本组端点就是那个提供方：本 flow-server（连着 IAM 库）把
// 既有 PgIamAssigneeResolver / PgSubflowRouter.ancestors 的解析能力经 HTTP 暴露，契约与两个 Http*
// 客户端逐字节对齐（POST /identity/resolve → {userIds}；GET /dimensions/ancestors → {ancestors}）。
// 平台若要 `/api/iam/flow-identity/*` 前缀，反代到这两个端点即可（同一契约）。

#[derive(Deserialize)]
pub struct IdentityResolveReq {
    /// 候选类型（USER/ROLE/POSITION/ORG/ORG_LEADER/INITIATOR/INITIATOR_LEADER）。
    kind: String,
    /// 候选值（user_id / role code / position code / org id；关系型可空）。
    #[serde(default)]
    value: String,
    /// 解析上下文：发起人（orgLeader/initiator 类关系型解析用）。
    #[serde(default)]
    initiator: Option<String>,
    #[serde(default, rename = "orgId")]
    org_id: Option<String>,
}

/// POST /identity/resolve —— 回连身份解析（镜像 AssigneeResolver）。{kind,value,ctx} → {userIds}。
/// **返回裸 JSON**（`{userIds:[..]}`，无 {code,msg,data} 信封）——契约对齐 HttpAssigneeResolver 期望的
/// 外部身份服务响应体，故不能用 ApiResp 包裹。
pub async fn resolve_identity(Json(req): Json<IdentityResolveReq>) -> Result<Json<Value>> {
    let _rt = flow().await?;
    let kind: CandidateKind = serde_json::from_value(json!(req.kind))
        .map_err(|_| FlowError::business_error(format!("未知候选类型: {}", req.kind)))?;
    let candidate = CandidateRef {
        kind,
        value: req.value.clone(),
    };
    let ctx = ResolveContext {
        initiator: req.initiator.clone(),
        org_id: req.org_id.clone(),
    };
    let resolver = PgIamAssigneeResolver::new(current_iam_db_id());
    let user_ids = resolver
        .resolve_with(&candidate, &ctx)
        .await
        .map_err(|e| msg_err(format!("身份解析失败: {e}")))?;
    Ok(Json(json!({ "userIds": user_ids })))
}

#[derive(Deserialize)]
pub struct DimAncestorsQuery {
    #[serde(rename = "dimKey", default)]
    dim_key: Option<String>,
    #[serde(rename = "dimValue", default)]
    dim_value: String,
}

/// GET /dimensions/ancestors?dimKey&dimValue —— 回连维度层级（RD5）。返回祖先链（由近及远）。
/// **返回裸 JSON**（`{ancestors:[..]}`，无信封）——契约对齐 HttpDimensionResolver 期望。
pub async fn get_dimension_ancestors(Query(q): Query<DimAncestorsQuery>) -> Result<Json<Value>> {
    let rt = flow().await?;
    let dim_key = q.dim_key.clone().unwrap_or_else(|| "org".to_string());
    let mut router = PgSubflowRouter::new(current_iam_db_id());
    for d in rt.dimension_specs.iter() {
        router.register_dim(d.dim_key.clone(), d.spec.clone());
    }
    let anc = router
        .ancestors(&dim_key, &q.dim_value)
        .await
        .map_err(|e| msg_err(format!("维度祖先解析失败: {e}")))?;
    Ok(Json(json!({ "ancestors": anc })))
}

/// 正向：实例 → 绑的业务单据。
pub async fn get_instance_biz(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let links = crate::biz_link::biz_of_instance(&id).await.map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "links": links }))))
}

/// 反向：业务单据 → 关联的流程实例（业务列表页显示「审批中」用）。
pub async fn get_biz_instances(
    Path((biz_table, biz_id)): Path<(String, String)>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let instances = crate::biz_link::instances_of_biz(&biz_table, &biz_id)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "instances": instances }))))
}

/// F3：某实例的审批意见历史（表单审批区展示）。
pub async fn get_instance_comments(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let comments = crate::biz_link::comments_of_instance(&id)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "comments": comments }))))
}

// ————————————————————— F4：表单注册表 + 发起态 —————————————————————

/// 列全部表单绑定（设计器选 formKey、待办中心解析用）。
pub async fn list_form_bindings(
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let items = crate::biz_link::list_form_bindings().await.map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "bindings": items }))))
}

/// 取单条表单绑定（待办打开表单时解析 formKey → 页坐标）。
pub async fn get_form_binding(
    Path(form_key): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let b = crate::biz_link::get_form_binding(&form_key)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(b.unwrap_or(Value::Null))))
}

/// upsert 一条表单绑定（管理面/设计器保存用）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormBindingReq {
    form_key: String,
    #[serde(default = "default_native")]
    kind: String,
    #[serde(default)]
    native_page: Option<String>,
    #[serde(default)]
    native_view: Option<String>,
    #[serde(default)]
    html_page: Option<String>,
    #[serde(default)]
    biz_table: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    pk_field: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    workspace_node: Option<String>,
    /// property 区审批控制台归属：platform（默认）/ none（表单自带审批操作，不挂平台控制台）。
    #[serde(default)]
    console: Option<String>,
}
fn default_native() -> String {
    "native".to_string()
}

pub async fn save_form_binding(
    Json(req): Json<FormBindingReq>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    // kind 枚举校验：非法值在消费端被 `b.kind || 'native'` 兜底掩盖，写入层直接拒绝。
    if !matches!(req.kind.as_str(), "workspace" | "html" | "native") {
        return Err(msg_err(format!(
            "kind 非法: {}（仅 workspace/html/native）",
            req.kind
        )));
    }
    let form_key = req.form_key.clone();
    crate::biz_link::upsert_form_binding(crate::biz_link::FormBinding {
        form_key: req.form_key,
        kind: req.kind,
        native_page: req.native_page,
        native_view: req.native_view,
        html_page: req.html_page,
        biz_table: req.biz_table,
        domain: req.domain,
        application: req.application,
        module: req.module,
        file: req.file,
        pk_field: req.pk_field,
        title: req.title,
        workspace_node: req.workspace_node,
        console: req.console,
    })
    .await
    .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "formKey": form_key }))))
}

/// 删除一条表单绑定（管理页）。幂等：不存在也返回成功（deleted=0）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFormBindingReq {
    form_key: String,
}

pub async fn delete_form_binding(
    Json(req): Json<DeleteFormBindingReq>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
    let deleted = crate::biz_link::delete_form_binding(&req.form_key)
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "formKey": req.form_key, "deleted": deleted }))))
}

/// 可发起流程列表（发起态）：引擎已装载定义 + 其 startFormKey。只列可发起的。
pub async fn list_startable_definitions(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let defs = rt.definitions.read().await;
    let items: Vec<Value> = defs
        .iter()
        .map(|d| {
            json!({
                "key": d.key,
                "name": d.name,
                "startFormKey": d.start_form_key,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "definitions": items }))))
}

// ————————————————————— 任务 —————————————————————

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteReq {
    instance_id: String,
    /// 通用变量对象（F1 首选）：办结时 merge 进实例变量，驱动后续网关。
    #[serde(default)]
    variables: Value,
    /// 向后兼容：单独传 decision 时并入 variables.lastDecision。
    #[serde(default)]
    decision: Option<String>,
    /// 审批意见（F3）：非空则随办结落意见留痕表 + 并入变量 comment。
    #[serde(default)]
    comment: Option<String>,
    /// 办理人（谁办结的）：落审计留痕 `comment.user_id`。委托令牌模式下由平台注入登录用户；
    /// 独立直连时由调用方传入。留空则留痕 user_id 为空（历史行为）。
    #[serde(default)]
    operator: Option<String>,
}

/// 办结一个任务。
///
/// T0：`operator` 留空时兜底取认证用户（`tenant::current_user()`），审批留痕不缺办理人。
/// T0b 授权：有用户身份时须为该任务的 assignee 或候选（物化候选表）；auth 中间件已生效却
/// 拿不到身份（纯服务调用/委托令牌验签失败）→ 拒绝——审批动作不允许无身份代办。
/// 宿主未挂 auth（平台内嵌形态）→ 放行（平台 mw_auth 兜底，现状兼容）。
pub async fn complete_task(
    Path(task_id): Path<String>,
    Json(req): Json<CompleteReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    match crate::tenant::current_user() {
        Some(user) => {
            let allowed = crate::biz_link::task_assignee(&rt, &req.instance_id, &task_id)
                .await
                .as_deref()
                    == Some(user.as_str())
                || crate::biz_link::task_has_candidate(&rt, &task_id, &user).await;
            if !allowed {
                return Err(FlowError::business_error(
                    "无权办理该任务：既非办理人也非候选",
                ));
            }
        }
        None => {
            if crate::auth::auth_middleware_active() {
                return Err(FlowError::business_error(
                    "缺少用户身份，不能办理任务（需登录态或有效委托令牌）",
                ));
            }
        }
    }
    // 兜底用展示名（username 优先、id 兜底）——审批留痕是给人看的人名列。
    let operator = req.operator.clone().or_else(crate::tenant::current_display_user);
    let mut vars = Variables::from_json(req.variables.clone());
    if let Some(d) = &req.decision {
        vars.set("lastDecision", json!(d));
    }
    if let Some(c) = &req.comment {
        vars.set("comment", json!(c));
    }
    // 办结前取该任务的节点 bpmn_id（写意见留痕用），失败不阻断办结。
    let node_bpmn_id = crate::biz_link::task_node_bpmn_id(&rt, &req.instance_id, &task_id)
        .await
        .unwrap_or_default();
    // 变量历史：办结前取旧值（办理携带的变量 req.variables 相对旧值算变更）。
    let old_vars = rt
        .engine
        .store()
        .load_snapshot(&req.instance_id)
        .await
        .map(|s| s.instance.variables)
        .unwrap_or_else(|_| Variables::from_json(json!({})));
    rt.engine
        .complete_task(&req.instance_id, &task_id, vars)
        .await
        .map_err(engine_err)?;
    // 变量历史：办理携带的变量变更（不含 comment/decision 留痕，那走意见表）。
    let vh = diff_var_changes(
        &req.instance_id,
        &req.variables,
        &old_vars,
        "complete",
        if node_bpmn_id.is_empty() {
            None
        } else {
            Some(node_bpmn_id.as_str())
        },
        operator.as_deref(),
    );
    record_var_history(&rt, &vh).await;
    // F3：意见留痕（有意见/决策/办理人才记）。失败仅告警，不影响办结结果。
    // user_name/nick_name 为办理人姓名快照（写入时点定版，人员改名不影响历史展示）。
    if req.comment.is_some() || req.decision.is_some() || operator.is_some() {
        let _ = crate::biz_link::insert_task_comment(
            &rt,
            &req.instance_id,
            &task_id,
            &node_bpmn_id,
            operator,
            crate::tenant::current_display_user(),
            crate::tenant::current_display_nickname(),
            req.decision.clone(),
            req.comment.clone(),
        )
        .await;
    }
    // 生命周期事件（webhook + SSE 双发）：该任务已办结；若实例随之完成发 instance.completed，
    // 否则为新产生的待办发 task.created。
    {
        // 借快照补 definitionKey（001 前置修复：TaskCompleted 此前不带，订阅过滤前提不成立）
        // 并筛出新待办（009-3 修复：快照 tasks 含已办结历史行，只对本轮**新开**任务 emit）。
        let snap = rt.engine.store().load_snapshot(&req.instance_id).await.ok();
        let def_key = snap.as_ref().map(|s| s.instance.definition_key.clone());
        let biz_key = snap.as_ref().and_then(|s| s.instance.business_key.clone());
        publish_event(
            &rt,
            FlowEvent::new(FlowEventKind::TaskCompleted, &req.instance_id, now_rfc3339())
                .definition_key(def_key.clone())
                .business_key(biz_key.clone())
                .task(Some(task_id.clone()), Some(node_bpmn_id.clone())),
        )
        .await;
        if let Some(snap) = snap {
            use cmx_flow_model::InstanceState;
            if snap.instance.state == InstanceState::Completed {
                emit_instance_event(&rt, FlowEventKind::InstanceCompleted, &req.instance_id).await;
            } else {
                // 新开的待办（办结推进后可能产生下一环节任务）。009-3：只 emit **未办结**且
                // 非刚办结本身的任务——历史已办结行随快照返回，旧代码会把它们误当"新建"重复 emit。
                let ts = now_rfc3339();
                let events = snap
                    .tasks
                    .iter()
                    .filter(|t| !t.completed && t.id != task_id)
                    .map(|t| {
                        FlowEvent::new(FlowEventKind::TaskCreated, &req.instance_id, ts.clone())
                            .definition_key(def_key.clone())
                            .business_key(biz_key.clone())
                            .task(Some(t.id.clone()), Some(t.node_bpmn_id.clone()))
                            .assignee(t.assignee.clone())
                    })
                    .collect();
                publish_events(&rt, events).await;
            }
        }
    }
    load_view(&rt, &req.instance_id).await
}

#[derive(Deserialize)]
pub struct ClaimReq {
    instance_id: String,
    user_id: String,
}

/// 认领一个候选任务（M4.1）。
pub async fn claim_task(
    Path(task_id): Path<String>,
    Json(req): Json<ClaimReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .claim_task(&req.instance_id, &task_id, &req.user_id)
        .await
        .map_err(engine_err)?;
    load_view(&rt, &req.instance_id).await
}

#[derive(Deserialize)]
pub struct TransferReq {
    instance_id: String,
    from_user: String,
    to_user: String,
    #[serde(default)]
    reason: Option<String>,
}

/// 转办（M4.3）。
pub async fn transfer_task(
    Path(task_id): Path<String>,
    Json(req): Json<TransferReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .transfer_task(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.reason.as_deref(),
        )
        .await
        .map_err(engine_err)?;
    emit_reassigned(&rt, &req.instance_id, &task_id, &req.to_user).await;
    load_view(&rt, &req.instance_id).await
}

/// 退回 / 驳回（P6）请求体。camelCase（对齐前端 instanceId 等）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectReq {
    instance_id: String,
    #[serde(default)]
    from_user: Option<String>,
    /// 退回目标节点 bpmn id（可空 = 回退到直接前驱用户任务）。
    #[serde(default)]
    target_bpmn_id: Option<String>,
    /// 驳回意见。
    #[serde(default)]
    reason: Option<String>,
    /// 附带变量（如驳回原因码），办结前 merge 进实例。
    #[serde(default)]
    variables: Value,
}

/// 退回 / 驳回（P6）——把待办打回之前的节点重新办理。
pub async fn reject_task(
    Path(task_id): Path<String>,
    Json(req): Json<RejectReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = Variables::from_json(req.variables.clone());
    // 退回前取节点 bpmn_id（意见留痕用），失败不阻断。
    let node_bpmn_id = crate::biz_link::task_node_bpmn_id(&rt, &req.instance_id, &task_id)
        .await
        .unwrap_or_default();
    rt.engine
        .reject_task(
            &req.instance_id,
            &task_id,
            req.from_user.as_deref().unwrap_or(""),
            req.target_bpmn_id.as_deref(),
            req.reason.as_deref(),
            vars,
        )
        .await
        .map_err(engine_err)?;
    // 意见留痕（与 complete 对称）：退回意见/办理人也进 cmx_flow_task_comment，
    // 否则审批历史缺退回环节（业务封装端点 return 的 reason 在此落库）。
    if req.reason.is_some() || req.from_user.is_some() {
        let _ = crate::biz_link::insert_task_comment(
            &rt,
            &req.instance_id,
            &task_id,
            &node_bpmn_id,
            req.from_user.clone(),
            crate::tenant::current_display_user(),
            crate::tenant::current_display_nickname(),
            Some("return".to_string()),
            req.reason.clone(),
        )
        .await;
    }
    load_view(&rt, &req.instance_id).await
}

/// 退回可选目标查询（③）：`?instanceId=<iid>`。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectTargetsQuery {
    instance_id: String,
}

/// 列举一个任务的全部合法退回目标（③，只读）——供前端「退回到…」选择器。
///
/// `GET /tasks/{taskId}/reject-targets?instanceId=<iid>`
pub async fn get_reject_targets(
    Path(task_id): Path<String>,
    Query(q): Query<RejectTargetsQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let info = rt
        .engine
        .reject_targets(&q.instance_id, &task_id)
        .await
        .map_err(engine_err)?;
    let targets: Vec<Value> = info
        .targets
        .iter()
        .map(|t| {
            json!({
                "bpmnId": t.bpmn_id,
                "name": t.name,
                "isDirectPredecessor": t.is_direct_predecessor,
                "distance": t.distance,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({
        "taskId": info.task_id,
        "currentNode": info.current_node,
        "rejectable": info.rejectable,
        "defaultTarget": info.default_target,
        "targets": targets,
    }))))
}

/// 重试 incident（H2）请求体。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryIncidentReq {
    /// 重试前 merge 的修正变量（可空）。
    #[serde(default)]
    variables: Value,
}

/// 运维改变量（H4）请求体。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetVarsReq {
    /// 要 merge 进实例的变量对象。
    variables: Value,
}

/// 运维干预：改实例变量（H4）——修数据后可再 retry-incident。
/// 变量历史：从「送入的变量对象」相对旧值算出变更条目（未变的 key 跳过，避免噪声）。
fn diff_var_changes(
    instance_id: &str,
    incoming: &Value,
    old: &Variables,
    source: &str,
    node: Option<&str>,
    by: Option<&str>,
) -> Vec<cmx_flow_store_pg::VarChange> {
    let mut out = Vec::new();
    if let Value::Object(m) = incoming {
        for (k, v) in m {
            let old_v = old.get(k).map(|x| x.to_string());
            let new_v = Some(v.to_string());
            if old_v == new_v {
                continue;
            }
            out.push(cmx_flow_store_pg::VarChange {
                instance_id: instance_id.to_string(),
                var_name: k.clone(),
                old_value: old_v,
                new_value: new_v,
                source: source.to_string(),
                node_bpmn_id: node.map(str::to_string),
                changed_by: by.map(str::to_string),
            });
        }
    }
    out
}

/// 记录变量历史（fire-and-forget，非致命：失败仅告警，不阻断主流程）。
async fn record_var_history(rt: &FlowRuntime, changes: &[cmx_flow_store_pg::VarChange]) {
    if changes.is_empty() {
        return;
    }
    if let Err(e) = rt.var_history_store.record(changes).await {
        tracing::warn!(error = %e, "记录变量历史失败");
    }
}

pub async fn set_instance_variables(
    Path(instance_id): Path<String>,
    Json(req): Json<SetVarsReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = Variables::from_json(req.variables.clone());
    // 变量历史：改动前取旧值 + 当前节点上下文。
    let (old, node) = match rt.engine.store().load_snapshot(&instance_id).await {
        Ok(s) => (
            s.instance.variables.clone(),
            s.tokens.first().map(|t| t.node_bpmn_id.clone()),
        ),
        Err(_) => (Variables::from_json(json!({})), None),
    };
    rt.engine
        .set_variables(&instance_id, vars)
        .await
        .map_err(engine_err)?;
    let by = crate::tenant::current_user();
    let changes = diff_var_changes(
        &instance_id,
        &req.variables,
        &old,
        "set-variables",
        node.as_deref(),
        by.as_deref(),
    );
    record_var_history(&rt, &changes).await;
    load_view(&rt, &instance_id).await
}

/// 挂起实例（A7）。
pub async fn suspend_instance(
    Path(instance_id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine.suspend_process(&instance_id).await.map_err(engine_err)?;
    load_view(&rt, &instance_id).await
}

/// 恢复实例（A7）。
pub async fn resume_instance(
    Path(instance_id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine.resume_process(&instance_id).await.map_err(engine_err)?;
    load_view(&rt, &instance_id).await
}

/// 自由跳转（A7）请求体。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpReq {
    /// 目标用户任务节点 bpmn id。
    target_bpmn_id: String,
    #[serde(default)]
    reason: Option<String>,
}

/// 自由跳转（A7）——把实例令牌强制移到指定用户任务节点。
pub async fn jump_instance(
    Path(instance_id): Path<String>,
    Json(req): Json<JumpReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .jump_to(&instance_id, &req.target_bpmn_id, req.reason.as_deref())
        .await
        .map_err(engine_err)?;
    load_view(&rt, &instance_id).await
}

/// 催办（A7）请求体。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrgeReq {
    instance_id: String,
    #[serde(default)]
    from_user: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// 催办（A7）——对待办任务当前办理人发催办知会。
pub async fn urge_task(
    Path(task_id): Path<String>,
    Json(req): Json<UrgeReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .urge_task(
            &req.instance_id,
            &task_id,
            req.from_user.as_deref().unwrap_or(""),
            req.message.as_deref(),
        )
        .await
        .map_err(engine_err)?;
    load_view(&rt, &req.instance_id).await
}

/// 相关消息（A4）请求体——外部系统投递消息唤醒等待中的流程。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelateReq {
    /// 消息名（对齐 BPMN message name / messageRef）。
    message_name: String,
    /// 目标实例 id（点对点回调；给了则只在该实例内找）。
    #[serde(default)]
    instance_id: Option<String>,
    /// 相关键（跨实例路由：匹配实例 correlation_var 变量值）。
    #[serde(default)]
    correlation_key: Option<String>,
    /// 随消息带入的变量（merge 进实例）。
    #[serde(default)]
    variables: Value,
}

/// 相关消息（A4）——外部系统回调唤醒停在消息中间捕获事件的流程。
///
/// `POST /flow/messages/correlate`：按消息名 + (实例 id | 相关键) 定位等待令牌，merge 变量后继续。
pub async fn correlate_message(
    Json(req): Json<CorrelateReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = Variables::from_json(req.variables.clone());
    let result = rt
        .engine
        .correlate_message(
            req.instance_id.as_deref(),
            &req.message_name,
            req.correlation_key.as_deref(),
            vars,
        )
        .await
        .map_err(engine_err)?;
    // 用返回的实例视图。
    load_view(&rt, &result.instance_id).await
}

/// 重试 incident（H2）——把实例内所有异常挂起(Incident)令牌重新激活重跑。
pub async fn retry_incident(
    Path(instance_id): Path<String>,
    Json(req): Json<RetryIncidentReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = Variables::from_json(req.variables.clone());
    rt.engine
        .retry_incident(&instance_id, vars)
        .await
        .map_err(engine_err)?;
    load_view(&rt, &instance_id).await
}

// ————————————————————— 实例迁移（A9）—————————————————————

#[derive(Deserialize)]
pub struct MigrateReq {
    /// 目标流程定义 key。
    target_definition_key: String,
    /// 活动节点映射：源节点 bpmn_id → 目标节点 bpmn_id。
    #[serde(default)]
    activity_mappings: std::collections::BTreeMap<String, String>,
}

fn migrate_plan(req: MigrateReq) -> cmx_flow_engine::MigrationPlan {
    cmx_flow_engine::MigrationPlan {
        target_definition_key: req.target_definition_key,
        activity_mappings: req.activity_mappings,
    }
}

fn migration_validation_json(v: &cmx_flow_engine::MigrationValidation) -> Value {
    let vios: Vec<Value> = v
        .violations
        .iter()
        .map(|vi| {
            json!({
                "code": format!("{:?}", vi.code),
                "nodeBpmnId": vi.node_bpmn_id,
                "message": vi.message,
            })
        })
        .collect();
    json!({ "ok": v.ok, "violations": vios })
}

/// 校验迁移计划（A9 干运行）：`POST /instances/{id}/migrate/validate`。返回违规明细，不改数据。
pub async fn validate_migration(
    Path(instance_id): Path<String>,
    Json(req): Json<MigrateReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let plan = migrate_plan(req);
    let v = rt
        .engine
        .validate_migration(&instance_id, &plan)
        .await
        .map_err(engine_err)?;
    Ok(Json(ApiResp::ok(migration_validation_json(&v))))
}

/// 执行实例迁移（A9）：`POST /instances/{id}/migrate`。校验通过则重写令牌位置 + 改定义指向。
pub async fn migrate_instance(
    Path(instance_id): Path<String>,
    Json(req): Json<MigrateReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let plan = migrate_plan(req);
    rt.engine
        .migrate_instance(&instance_id, &plan)
        .await
        .map_err(engine_err)?;
    load_view(&rt, &instance_id).await
}


pub async fn delegate_task(
    Path(task_id): Path<String>,
    Json(req): Json<TransferReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .delegate_task(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.reason.as_deref(),
        )
        .await
        .map_err(engine_err)?;
    emit_reassigned(&rt, &req.instance_id, &task_id, &req.to_user).await;
    load_view(&rt, &req.instance_id).await
}

#[derive(Deserialize)]
pub struct AddSignReq {
    instance_id: String,
    from_user: String,
    to_user: String,
    #[serde(default = "default_true")]
    before: bool,
    #[serde(default)]
    reason: Option<String>,
}
fn default_true() -> bool {
    true
}

/// 加签（M4.3）。
pub async fn add_sign_task(
    Path(task_id): Path<String>,
    Json(req): Json<AddSignReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .add_sign(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.before,
            req.reason.as_deref(),
        )
        .await
        .map_err(engine_err)?;
    emit_reassigned(&rt, &req.instance_id, &task_id, &req.to_user).await;
    load_view(&rt, &req.instance_id).await
}

// ————————————————————— F2：我的待办（跨实例） —————————————————————

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyTasksQuery {
    /// 办理人 user_id（F3 应来自登录态；F2 允许 query 传值便于 curl 验证）。
    assignee: String,
    /// todo（待我办，直派）| claimable（待我认领）| all（两者）。
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    node_bpmn_id: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
}

impl MyTasksQuery {
    fn to_filter(&self) -> crate::biz_link::TodoFilter {
        crate::biz_link::TodoFilter {
            keyword: self.keyword.clone(),
            definition_key: self.definition_key.clone(),
            node_bpmn_id: self.node_bpmn_id.clone(),
            state: None,
            initiator: None,
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

/// 我的待办：跨所有实例聚合当前用户的待办，每条带 formKey + 业务引用。
///
/// formKey/formMode 靠 (definition_key, node_bpmn_id) 反查内存定义得来（不冗余进任务表）；
/// bizTable/bizId 从实例变量投影（F1 塞入）。数据源走已有复合索引。
pub async fn get_my_tasks(
    Query(q): Query<MyTasksQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let kind = q.kind.as_deref().unwrap_or("todo");
    let filter = q.to_filter();

    // 分页在 DB 层做（每类独立分页）。all = 直派 + 可认领 的并集。
    let page = match kind {
        "claimable" => crate::biz_link::claimable_tasks_by_user(&q.assignee, &filter)
            .await
            .map_err(msg_err)?,
        "all" => {
            // 「两者」：直派待办 ∪ 可认领（候选池）。二者天然不相交（池任务 assignee=None），
            // 拼接 + 合计总数；仍按 task_id 轻量去重防御。
            let mut direct = crate::biz_link::open_tasks_by_assignee(&q.assignee, &filter)
                .await
                .map_err(msg_err)?;
            let claim = crate::biz_link::claimable_tasks_by_user(&q.assignee, &filter)
                .await
                .map_err(msg_err)?;
            let seen: std::collections::HashSet<String> =
                direct.rows.iter().map(|r| r.task_id.clone()).collect();
            for r in claim.rows {
                if !seen.contains(&r.task_id) {
                    direct.rows.push(r);
                }
            }
            direct.total += claim.total;
            direct
        }
        _ => crate::biz_link::open_tasks_by_assignee(&q.assignee, &filter)
            .await
            .map_err(msg_err)?,
    };
    let raws = page.rows;

    // 反查表单绑定 + 投影业务引用/展示字段（一次性借读定义快照）。
    let defs = rt.definitions.read().await;
    let tasks: Vec<Value> = raws
        .iter()
        .map(|t| {
            let (form_key, form_mode, form_fields, def_name) = defs
                .iter()
                .find(|d| d.key == t.definition_key)
                .map(|d| {
                    let (fk, fm, ff) = form_of_task(d, &t.node_bpmn_id);
                    (fk, fm, ff, Some(d.name.clone()))
                })
                .unwrap_or((None, None, vec![], None));
            let vars: Value = t
                .variables_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            let vget = |k: &str| vars.get(k).cloned().unwrap_or(Value::Null);
            json!({
                "taskId": t.task_id,
                "instanceId": t.instance_id,
                "nodeBpmnId": t.node_bpmn_id,
                "nodeName": t.name,
                "definitionKey": t.definition_key,
                "definitionName": def_name,
                "businessKey": t.business_key,
                "formKey": form_key,
                "formMode": form_mode.unwrap_or_else(|| "approve".to_string()),
                "formFields": form_fields,
                "bizTable": vget("bizTable"),
                "bizId": vget("bizId"),
                "applicant": vget("applicant"),
                "amount": vget("amount"),
                "elementValue": t
                    .element_value
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(Value::Null),
                "claimable": t.claimable,
                "createdAt": t.created_at,
            })
        })
        .collect();

    let (pno, psize) = filter.norm();
    Ok(Json(ApiResp::ok(json!({
        "tasks": tasks,
        "total": page.total,
        "page": pno,
        "pageSize": psize,
    }))))
}

/// 按 (definition, node_bpmn_id) 反查该 userTask 的表单绑定（F2）。
fn form_of_task(
    def: &cmx_flow_model::ProcessDefinition,
    node_bpmn_id: &str,
) -> (Option<String>, Option<String>, Vec<String>) {
    use cmx_flow_model::NodeKind;
    match def.node_by_bpmn(node_bpmn_id).map(|n| &n.kind) {
        Some(NodeKind::UserTask(ut)) => (
            ut.form_key.clone(),
            ut.form_mode.clone(),
            ut.form_fields.clone(),
        ),
        _ => (None, None, vec![]),
    }
}

/// 分页列表通用查询参数（我发起的/抄送/已办共用）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    definition_key: Option<String>,
    #[serde(default)]
    node_bpmn_id: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
}
impl ListQuery {
    fn to_filter(&self) -> crate::biz_link::TodoFilter {
        crate::biz_link::TodoFilter {
            keyword: self.keyword.clone(),
            definition_key: self.definition_key.clone(),
            node_bpmn_id: self.node_bpmn_id.clone(),
            state: self.state.clone(),
            initiator: None,
            page: self.page.unwrap_or(1),
            page_size: self.page_size.unwrap_or(20),
        }
    }
}

/// 把 RawTodo 投影成前端待办 JSON（含变量投影 + 状态/申请人）。用于实例/抄送/已办列表。
/// `defs`：已装载定义，用于按 (definitionKey, currentNode) 反查该环节 formKey/formMode
/// （查看时据此打开节点表单工作台）。
fn raw_todo_json(t: &crate::biz_link::RawTodo, defs: &[cmx_flow_model::ProcessDefinition]) -> Value {
    let vars: Value = t
        .variables_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let vget = |k: &str| vars.get(k).cloned().unwrap_or(Value::Null);
    // 反查当前环节的表单绑定（cc/done 的 current_node = 真实节点；initiated = 当前活动令牌节点）。
    let (form_key, form_mode) = t
        .current_node
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|node| {
            defs.iter()
                .find(|d| d.key == t.definition_key)
                .map(|d| form_of_task(d, node))
        })
        .map(|(fk, fm, _)| (fk, fm))
        .unwrap_or((None, None));
    json!({
        "taskId": t.task_id,
        "instanceId": t.instance_id,
        "nodeBpmnId": t.node_bpmn_id,
        "nodeName": t.name,
        "definitionKey": t.definition_key,
        "businessKey": t.business_key,
        "state": t.node_bpmn_id, // 实例列表复用 node 位存状态
        "currentNode": t.current_node,
        "formKey": form_key,
        "formMode": form_mode.unwrap_or_else(|| "approve".to_string()),
        "bizTable": vget("bizTable"),
        "bizId": vget("bizId"),
        "applicant": vget("applicant"),
        "amount": vget("amount"),
        "createdAt": t.created_at,
    })
}

fn page_resp(
    page: crate::biz_link::TodoPage,
    f: &crate::biz_link::TodoFilter,
    defs: &[cmx_flow_model::ProcessDefinition],
) -> Json<ApiResp<Value>> {
    let items: Vec<Value> = page.rows.iter().map(|t| raw_todo_json(t, defs)).collect();
    let (pno, psize) = f.norm();
    Json(ApiResp::ok(json!({
        "tasks": items, "total": page.total, "page": pno, "pageSize": psize,
    })))
}

/// 我发起的（分页过滤）。
pub async fn get_initiated(
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let mut f = q.to_filter();
    // 「我发起的」= 按发起人过滤（缺陷修复：此前 user 入参被忽略，返回全部实例）。
    f.initiator = q.user.clone().filter(|s| !s.trim().is_empty());
    let page = crate::biz_link::list_instances_paged(&f)
        .await
        .map_err(msg_err)?;
    let defs = rt.definitions.read().await;
    Ok(page_resp(page, &f, &defs))
}

/// 抄送我的（分页过滤）。
pub async fn get_cc_todos(
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let f = q.to_filter();
    let user = q.user.clone().unwrap_or_default();
    let page = crate::biz_link::list_cc_paged(&user, &f)
        .await
        .map_err(msg_err)?;
    let defs = rt.definitions.read().await;
    Ok(page_resp(page, &f, &defs))
}

/// 我已办（历史任务，分页过滤）。
pub async fn get_done_todos(
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let f = q.to_filter();
    let user = q.user.clone().unwrap_or_default();
    let page = crate::biz_link::list_done_paged(&user, &f)
        .await
        .map_err(msg_err)?;
    let defs = rt.definitions.read().await;
    Ok(page_resp(page, &f, &defs))
}

/// 过滤选项源：流程下拉（已有实例的定义）+ 已装载定义（含名称/节点）。
pub async fn get_todo_filters(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let defs = rt.definitions.read().await;
    // 每个定义 → { key, name, nodes:[{id,name}] }（userTask 节点，供「按环节」下拉）。
    let definitions: Vec<Value> = defs
        .iter()
        .map(|d| {
            use cmx_flow_model::NodeKind;
            let nodes: Vec<Value> = d
                .nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::UserTask(_)))
                .map(|n| json!({ "id": n.bpmn_id, "name": n.name }))
                .collect();
            json!({ "key": d.key, "name": d.name, "nodes": nodes })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "definitions": definitions }))))
}

// ————————————————————— 抄送 / 定时器 / 用户 —————————————————————

/// 手动「立即检查到期定时器」（M2.5）。
pub async fn trigger_timers(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let fired = rt
        .engine
        .trigger_due_timers(100)
        .await
        .map_err(engine_err)?;
    let items: Vec<Value> = fired
        .iter()
        .map(|f| {
            json!({
                "instanceId": f.instance_id,
                "boundaryBpmnId": f.boundary_bpmn_id,
                "cancelActivity": f.cancel_activity,
                "instanceState": instance_state_str(f.instance_state),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(
        json!({ "firedCount": fired.len(), "fired": items }),
    )))
}

// ————————————————————— 外部 worker：异步 Job 执行器（P1）—————————————————————

#[derive(Deserialize)]
pub struct AcquireAsyncReq {
    /// worker 唯一标识（锁持有者；崩溃后锁到期可被他人重抢）。
    worker_id: String,
    /// A7 外部 Worker 主题过滤：省略/null = 取进程内作业（topic 为空）；给定 = 取该 topic 外部作业。
    #[serde(default)]
    topic: Option<String>,
    /// 锁定时长秒数（缺省 60）。delegate 应远快于此；超时未回调则作业可被重抢。
    #[serde(default = "default_lock_secs")]
    lock_secs: i64,
    /// 单次最多抢占多少个（缺省 10）。
    #[serde(default = "default_acquire_limit")]
    limit: usize,
}
fn default_lock_secs() -> i64 {
    60
}
fn default_acquire_limit() -> usize {
    10
}

/// 外部 worker 抢占一批异步作业（P1/A7）：`POST /async-jobs/acquire`。
///
/// SKIP LOCKED 集群安全：多 worker 并发抢占拿到互不相交的作业集。`topic` 省略 = 进程内作业，
/// 给定 = 该 topic 外部作业。返回每个作业的令牌坐标 + delegate/topic + 实例变量快照。
pub async fn acquire_async_jobs(
    Json(req): Json<AcquireAsyncReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let jobs = rt
        .engine
        .acquire_async_jobs(&req.worker_id, req.topic.as_deref(), req.lock_secs, req.limit)
        .await
        .map_err(engine_err)?;
    // 附上每个作业宿主实例的当前变量快照，省 worker 再拉一次实例。
    let mut items: Vec<Value> = Vec::with_capacity(jobs.len());
    for j in &jobs {
        let variables = rt
            .engine
            .store()
            .load_snapshot(&j.instance_id)
            .await
            .ok()
            .map(|s| s.instance.variables.to_json())
            .unwrap_or(Value::Null);
        items.push(json!({
            "id": j.id,
            "instanceId": j.instance_id,
            "tokenId": j.token_id,
            "nodeBpmnId": j.node_bpmn_id,
            "delegateKey": j.delegate_key,
            "topic": j.topic,
            "retries": j.retries,
            "maxRetries": j.max_retries,
            "lockExpiresAt": j.lock_expires_at.map(|t| t.to_rfc3339()),
            "variables": variables,
        }));
    }
    Ok(Json(ApiResp::ok(
        json!({ "acquiredCount": jobs.len(), "jobs": items }),
    )))
}

/// A7 外部 Worker 按 topic 拉取作业：`POST /external-worker/jobs/acquire`。
///
/// 与 `/async-jobs/acquire` 同实现，但**要求 topic 非空**（外部 worker 语义必须指定主题）。
/// 语义清晰的专用端点——外部集成方按 `flowable:type="external-worker"` 的 topic 订阅拉取。
pub async fn acquire_external_worker_jobs(
    Json(req): Json<AcquireAsyncReq>,
) -> Result<Json<ApiResp<Value>>> {
    let topic = match req.topic.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => return Err(FlowError::business_error("external-worker 拉取必须指定 topic")),
    };
    let rt = flow().await?;
    let jobs = rt
        .engine
        .acquire_async_jobs(&req.worker_id, Some(&topic), req.lock_secs, req.limit)
        .await
        .map_err(engine_err)?;
    let mut items: Vec<Value> = Vec::with_capacity(jobs.len());
    for j in &jobs {
        let variables = rt
            .engine
            .store()
            .load_snapshot(&j.instance_id)
            .await
            .ok()
            .map(|s| s.instance.variables.to_json())
            .unwrap_or(Value::Null);
        items.push(json!({
            "id": j.id,
            "instanceId": j.instance_id,
            "tokenId": j.token_id,
            "nodeBpmnId": j.node_bpmn_id,
            "topic": j.topic,
            "retries": j.retries,
            "maxRetries": j.max_retries,
            "lockExpiresAt": j.lock_expires_at.map(|t| t.to_rfc3339()),
            "variables": variables,
        }));
    }
    Ok(Json(ApiResp::ok(
        json!({ "topic": topic, "acquiredCount": jobs.len(), "jobs": items }),
    )))
}

#[derive(Deserialize)]
pub struct CompleteAsyncReq {
    /// worker 执行 delegate 后写回的变量（合并进实例变量）。缺省不写回。
    #[serde(default)]
    variables: Value,
}

/// 外部 worker 完成一个异步作业（P1）：`POST /async-jobs/{id}/complete`。
/// 引擎删作业、合并变量、把令牌从 WaitingAsync 转 Active 沿出边推进。
pub async fn complete_async_job(
    Path(job_id): Path<String>,
    Json(req): Json<CompleteAsyncReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = if req.variables.is_null() {
        Variables::new()
    } else {
        Variables::from_json(req.variables.clone())
    };
    let result = rt
        .engine
        .complete_async_job(&job_id, vars)
        .await
        .map_err(engine_err)?;
    match result {
        // 作业存在并已推进 → 回宿主实例最新视图（与其它办理端点一致）。
        Some(exec) => load_view(&rt, &exec.instance_id).await,
        // 作业不存在（已完成/已死信）→ 幂等成功。
        None => Ok(Json(ApiResp::ok(
            json!({ "completed": false, "reason": "作业不存在或已处理" }),
        ))),
    }
}

#[derive(Deserialize)]
pub struct FailAsyncReq {
    /// 失败原因（记入 incident；供运维台展示）。
    #[serde(default)]
    error: Option<String>,
}

/// 外部 worker 标记一个异步作业失败（P1）：`POST /async-jobs/{id}/fail`。
/// 重试次数 -1 并释放锁（可被重抢）；耗尽则令牌转 Incident（可经 retry-incident 重发）。
/// 返回 `retryable`：true = 仍可重试，false = 已耗尽死信。
pub async fn fail_async_job(
    Path(job_id): Path<String>,
    Json(req): Json<FailAsyncReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let error = req.error.as_deref().unwrap_or("外部 worker 未提供失败原因");
    let retryable = rt
        .engine
        .fail_async_job(&job_id, error)
        .await
        .map_err(engine_err)?;
    Ok(Json(ApiResp::ok(json!({ "retryable": retryable }))))
}

// ————————————————————— 死信队列（P2）—————————————————————

/// 列出死信作业（P2 运维台）：`GET /dead-letter-jobs`。
pub async fn list_dead_letter_jobs() -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let jobs = rt
        .engine
        .list_dead_letter_jobs(200)
        .await
        .map_err(engine_err)?;
    let items: Vec<Value> = jobs
        .iter()
        .map(|j| {
            json!({
                "id": j.id,
                "instanceId": j.instance_id,
                "tokenId": j.token_id,
                "nodeBpmnId": j.node_bpmn_id,
                "delegateKey": j.delegate_key,
                "maxRetries": j.max_retries,
                "error": j.error,
                "originalCreatedAt": j.original_created_at.to_rfc3339(),
                "deadLetteredAt": j.dead_lettered_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(
        json!({ "count": jobs.len(), "jobs": items }),
    )))
}

/// 重投一条死信作业（P2）：`POST /dead-letter-jobs/{id}/retry`。
/// 重建 AsyncJob + 令牌回 WaitingAsync + 删死信行；worker 下一轮重抢执行。
pub async fn retry_dead_letter_job(Path(job_id): Path<String>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let retried = rt
        .engine
        .retry_dead_letter_job(&job_id)
        .await
        .map_err(engine_err)?;
    Ok(Json(ApiResp::ok(json!({ "retried": retried }))))
}

/// 丢弃一条死信作业（P2）：`DELETE /dead-letter-jobs/{id}`。令牌保持 Incident。
pub async fn discard_dead_letter_job(Path(job_id): Path<String>) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .discard_dead_letter_job(&job_id)
        .await
        .map_err(engine_err)?;
    Ok(Json(ApiResp::ok(json!({ "discarded": true }))))
}

// ————————————————————— 活动历史（A6）—————————————————————

/// 某实例的活动历史（A6 运维台/SLA）：`GET /instances/{id}/activities`。
/// 每条 = 令牌在一个节点的停留时段（enter/exit/duration + 类型/办理人），按进入时刻升序。
pub async fn get_instance_activities(
    Path(instance_id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let acts = rt
        .engine
        .store()
        .list_activities_by_instance(&instance_id)
        .await
        .map_err(|e| FlowError::business_error(format!("查询活动历史失败: {e}")))?;
    let items: Vec<Value> = acts
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "tokenId": a.token_id,
                "activityBpmnId": a.activity_bpmn_id,
                "activityName": a.activity_name,
                "activityType": a.activity_type,
                "enteredAt": a.entered_at.to_rfc3339(),
                "exitedAt": a.exited_at.to_rfc3339(),
                "durationMs": a.duration_ms,
                "assignee": a.assignee,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(
        json!({ "count": acts.len(), "activities": items }),
    )))
}

/// 列出 IAM 库用户（id → 昵称/用户名），供前端把候选人 id 显示成友好名字。
pub async fn list_users(
) -> Result<Json<ApiResp<Value>>> {
    let ds = cmx_database_pg::query_sql(
        &current_iam_db_id(),
        None,
        "SELECT id, username, nickname FROM cmx_user WHERE archived = 0 ORDER BY create_time LIMIT 200",
        "flow_list_users",
    )
    .await
    .map_err(|e| msg_err(format!("查询用户失败: {e}")))?;
    let schema = ds.schema.as_ref();
    let get = |row: &cmx_core::model::data::dataset::Row, col: &str| -> Option<String> {
        match row.get_by_name(schema, col) {
            Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    let users: Vec<Value> = ds
        .iter()
        .map(|row| {
            let id = get(row, "id").unwrap_or_default();
            let name = get(row, "nickname")
                .filter(|s| !s.is_empty())
                .or_else(|| get(row, "username"))
                .unwrap_or_else(|| id.clone());
            json!({ "id": id, "name": name })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "users": users }))))
}

#[derive(Deserialize)]
pub struct CcQuery {
    user: String,
    #[serde(default)]
    unread: bool,
}

/// 「抄送我的」列表（M4.2）。
pub async fn list_cc(
    Query(q): Query<CcQuery>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let items = rt
        .engine
        .cc_for_user(&q.user, q.unread, 100)
        .await
        .map_err(engine_err)?;
    let cc: Vec<Value> = items
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "instanceId": c.instance_id,
                "businessKey": c.business_key,
                "definitionKey": c.definition_key,
                "nodeBpmnId": c.node_bpmn_id,
                "reason": c.reason,
                "read": c.read,
                "createdAt": c.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "cc": cc }))))
}

/// 标记一条抄送已读（M4.2）。
pub async fn mark_cc_read(
    Path(cc_id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let ok = rt.engine.mark_cc_read(&cc_id).await.map_err(engine_err)?;
    Ok(Json(ApiResp::ok(json!({ "ok": ok }))))
}

// ————————————————————— 子流程路由（绑定管理 + 维度） —————————————————————

/// 组织树（设计器组织维度选择器）。扁平表 + path，前端建树。保留（org 维度的快捷端点）。
pub async fn list_orgs(
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let orgs = rt.binding_store.list_orgs().await.map_err(msg_err)?;
    let items: Vec<Value> = orgs
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "name": o.name,
                "parentId": o.parent_id,
                "path": o.path,
            })
        })
        .collect();
    Ok(Json(ApiResp::ok(json!({ "orgs": items }))))
}

/// 列可选路由维度（RD2）：内建「组织机构(org)」+ 运行时已注册的自分级维度字典。
/// 前端维度选择器数据源；org 置顶（推荐默认，向后兼容）。
pub async fn list_dimensions() -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let mut dims = vec![json!({
        "dimKey": "org", "name": "组织机构", "selfHierarchy": true, "builtin": true,
    })];
    for d in rt.dimension_specs.iter() {
        dims.push(json!({
            "dimKey": d.dim_key, "name": d.name,
            "selfHierarchy": d.self_hierarchy, "builtin": false,
        }));
    }
    Ok(Json(ApiResp::ok(json!({ "dimensions": dims }))))
}

/// 列某维度字典的条目（RD2，维度条目选择器）。org → 组织树；其余 → 按注册的 DimSpec 直读。
pub async fn list_dimension_entries(
    Path(dim_key): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let entries = if dim_key == "org" {
        rt.binding_store.list_orgs().await.map_err(msg_err)?
    } else {
        let reg = rt
            .dimension_specs
            .iter()
            .find(|d| d.dim_key == dim_key)
            .ok_or_else(|| FlowError::business_error(format!("未注册的路由维度: {dim_key}")))?;
        rt.binding_store
            .list_dim_entries(&reg.spec, &reg.name_col, reg.parent_col.as_deref())
            .await
            .map_err(msg_err)?
    };
    let items: Vec<Value> = entries
        .iter()
        .map(|o| json!({ "id": o.id, "name": o.name, "parentId": o.parent_id, "path": o.path }))
        .collect();
    Ok(Json(ApiResp::ok(json!({ "dimKey": dim_key, "entries": items }))))
}

/// 列某逻辑子流程 key 的全部维度绑定（含默认兜底），带维度取值展示名。
pub async fn list_subflow_bindings(
    Path(called_key): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let list = rt
        .binding_store
        .list_by_key(&called_key)
        .await
        .map_err(msg_err)?;
    let items: Vec<Value> = list.iter().map(binding_view).collect();
    Ok(Json(ApiResp::ok(
        json!({ "calledKey": called_key, "bindings": items }),
    )))
}

fn binding_view(b: &cmx_flow_store_pg::SubflowBinding) -> Value {
    json!({
        "id": b.id,
        "calledKey": b.called_key,
        "dimKey": b.dim_key,
        "dimValue": b.dim_value,
        "dimValueName": b.dim_value_name,
        // 兼容旧前端：org 维度仍暴露 orgId/orgName 别名。
        "orgId": if b.dim_key == "org" { b.dim_value.clone() } else { None },
        "orgName": if b.dim_key == "org" { b.dim_value_name.clone() } else { None },
        "targetKey": b.target_definition_key,
        "enabled": b.enabled,
        "remark": b.remark,
        "isDefault": b.dim_value.is_none(),
    })
}

/// upsert 绑定请求。dimValue 为空/缺省 = 该维度默认兜底绑定。兼容旧 orgId 字段。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertBindingReq {
    /// 逻辑子流程 key（= callActivity cmx:calledKey）。
    called_key: String,
    /// 路由维度 key（缺省 "org"，向后兼容）。
    #[serde(default)]
    dim_key: Option<String>,
    /// 维度取值（None/空 → 该维度默认兜底绑定）。
    #[serde(default)]
    dim_value: Option<String>,
    /// 【兼容】旧字段：等价 dim_key="org" 时的 dim_value。
    #[serde(default)]
    org_id: Option<String>,
    /// 目标子流程定义 key。
    target_key: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    remark: Option<String>,
}

/// upsert 一条维度绑定（同 called_key+dim_key+dim_value 视为一条）。id 派生自三元组（幂等）。
pub async fn upsert_subflow_binding(
    Json(req): Json<UpsertBindingReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let dim_key = req.dim_key.as_deref().filter(|s| !s.is_empty()).unwrap_or("org");
    // 维度取值：优先 dimValue；缺省回退旧 orgId（仅 org 维度语义）。空串归一为 None（兜底）。
    let dim_value = req
        .dim_value
        .as_deref()
        .or(req.org_id.as_deref())
        .filter(|s| !s.is_empty());
    let id = binding_id(&req.called_key, dim_key, dim_value);
    rt.binding_store
        .upsert(
            &id,
            &req.called_key,
            dim_key,
            dim_value,
            &req.target_key,
            req.enabled,
            req.remark.as_deref(),
        )
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "id": id }))))
}

/// 从 called_key + dim_key + dim_value 派生稳定绑定 id（非加密，仅去重定位用）。
fn binding_id(called_key: &str, dim_key: &str, dim_value: Option<&str>) -> String {
    let raw = format!("{called_key}|{dim_key}|{}", dim_value.unwrap_or("__default__"));
    // 简单 FNV-1a，避免引 uuid/sha 依赖；碰撞面为同库同 key，可忽略。
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in raw.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("sb_{h:016x}")
}

/// 删除一条绑定（按 id）。
pub async fn delete_subflow_binding(
    Path(id): Path<String>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.binding_store.delete(&id).await.map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "deleted": id }))))
}
