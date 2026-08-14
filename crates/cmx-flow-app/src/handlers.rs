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

use cmx_flow_engine::{RuntimeStore, Variables};

use crate::engine::{FlowRuntime, current_iam_db_id, flow};
use crate::views::{definition_view, instance_state_str, instance_view, summary_view};

use cmx_flow_adapters::{FlowEvent, FlowEventKind};

// ————————————————————— 出站 webhook emit 辅助 —————————————————————

/// 当前时刻 RFC3339（webhook 事件 occurred_at）。
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 发一条生命周期事件：**双发**——出站 webhook（若启用）+ 进程内 SSE 广播（S3，始终）。
/// 自动补上当前租户（SSE 按租户过滤）。webhook 关也能 SSE，故不再以 webhook 启用为前提。
fn publish_event(rt: &FlowRuntime, event: FlowEvent) {
    let event = event.tenant(Some(crate::tenant::current_tenant()));
    if rt.webhook.is_enabled() {
        rt.webhook.emit(event.clone());
    }
    crate::events::publish(event);
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
    );
}

/// 为 ExecutionResult 的每个当前未办结任务 emit 一条 task 事件（task.created / task.reassigned）。
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
    for t in &result.open_tasks {
        publish_event(
            rt,
            FlowEvent::new(kind, &result.instance_id, ts.clone())
                .definition_key(def_key.clone())
                .business_key(biz_key.clone())
                .task(Some(t.id.clone()), Some(t.node_bpmn_id.clone()))
                .assignee(t.assignee.clone()),
        );
    }
}

/// emit 一条 task.reassigned（转办/委派/加签后新办理人 = to_user）。
async fn emit_reassigned(rt: &FlowRuntime, instance_id: &str, task_id: &str, to_user: &str) {
    // 补 node_bpmn_id（借快照找该任务；找不到就不带）。
    let node = rt
        .engine
        .store()
        .load_snapshot(instance_id)
        .await
        .ok()
        .and_then(|snap| {
            snap.tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.node_bpmn_id.clone())
        });
    publish_event(
        rt,
        FlowEvent::new(FlowEventKind::TaskReassigned, instance_id, now_rfc3339())
            .task(Some(task_id.to_string()), node)
            .assignee(Some(to_user.to_string())),
    );
}

// ————————————————————— 错误桥 —————————————————————

fn engine_err(e: cmx_flow_engine::Error) -> FlowError {
    FlowError::business(e.to_string())
}
fn def_err(e: cmx_flow_def::DefError) -> FlowError {
    FlowError::business(e.to_string())
}
fn msg_err(msg: String) -> FlowError {
    FlowError::business(msg)
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
}

/// 设计器：存草稿（先试编译挡回非法 BPMN）。
pub async fn save_definition_draft(
    Json(req): Json<SaveDraftReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let rec = rt
        .def_svc
        .save_draft(
            &req.name,
            req.domain,
            req.application,
            req.module,
            req.category,
            &req.bpmn_xml,
            req.updated_by,
        )
        .await
        .map_err(def_err)?;
    Ok(Json(ApiResp::ok(json!({
        "key": rec.key,
        "name": rec.name,
        "state": rec.state.as_str(),
        "activeVersion": rec.active_version,
    }))))
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

/// 设计器：发布请求。note = 本次发布的变更说明（可空）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReq {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    published_by: Option<String>,
}

/// 设计器：发布（草稿 → 版本 +1）。**H1：发布即热装载到运行引擎，无需重启。**
pub async fn publish_definition(
    Path(key): Path<String>,
    Json(req): Json<PublishReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let version = rt
        .def_svc
        .publish(&key, req.note, req.published_by)
        .await
        .map_err(def_err)?;

    // H1 热装载：取刚发布版本的 XML → 编译 → deploy 到运行引擎（deploy 取 &self，Arc 后仍可热更）。
    // 同步刷新 rt.definitions（前端画图列表）。任一步失败仅告警，不回滚发布（已落库）。
    let mut hot_loaded = false;
    match rt.def_svc.get_version(&key, version).await {
        Ok(Some(ver)) => match cmx_flow_bpmn::compile(&ver.bpmn_xml) {
            Ok(def) => {
                if let Err(e) = rt.engine.deploy(def.clone()) {
                    tracing::warn!(key = %key, error = %e, "热装载 deploy 失败");
                } else {
                    // 刷新前端定义列表：同 key 覆盖，否则追加（tokio RwLock）。
                    {
                        let mut defs = rt.definitions.write().await;
                        if let Some(slot) = defs.iter_mut().find(|d| d.key == def.key) {
                            *slot = def;
                        } else {
                            defs.push(def);
                        }
                    }
                    hot_loaded = true;
                    tracing::info!(key = %key, version, "已热装载新发布定义");
                }
            }
            Err(e) => tracing::warn!(key = %key, error = %e, "热装载编译失败"),
        },
        Ok(None) => tracing::warn!(key = %key, version, "热装载取版本 XML 为空"),
        Err(e) => tracing::warn!(key = %key, error = %e, "热装载取版本失败"),
    }

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
    rt.def_svc
        .activate_version(&key, version)
        .await
        .map_err(def_err)?;
    Ok(Json(ApiResp::ok(json!({
        "key": key,
        "activeVersion": version,
        "note": "已设为当前版本；重启服务后引擎装载生效",
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
    let rt = flow().await?;
    let def_key = req
        .definition_key
        .clone()
        .unwrap_or_else(|| "credit_approval".to_string());

    let vars = req.resolve_variables();
    // businessKey 优先显式入参；兼容垫片下从 applicant 拼一个。
    let biz_key = req
        .business_key
        .clone()
        .or_else(|| req.applicant.as_ref().map(|a| format!("CR-{a}")));

    let result = rt
        .engine
        .start_process_org(&def_key, vars, biz_key.clone(), req.org_id.clone())
        .await
        .map_err(engine_err)?;

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
pub async fn cancel_instance(
    Path(id): Path<String>,
    Json(req): Json<CancelReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    rt.engine
        .cancel_process(&id, req.reason)
        .await
        .map_err(engine_err)?;
    // 出站 webhook：实例已终止（撤单/取消）。
    emit_instance_event(&rt, FlowEventKind::InstanceTerminated, &id).await;
    load_view(&rt, &id).await
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
}
fn default_native() -> String {
    "native".to_string()
}

pub async fn save_form_binding(
    Json(req): Json<FormBindingReq>,
) -> Result<Json<ApiResp<Value>>> {
    let _rt = flow().await?;
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
    })
    .await
    .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "formKey": form_key }))))
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
}

/// 办结一个任务。
pub async fn complete_task(
    Path(task_id): Path<String>,
    Json(req): Json<CompleteReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
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
    rt.engine
        .complete_task(&req.instance_id, &task_id, vars)
        .await
        .map_err(engine_err)?;
    // F3：意见留痕（有意见/决策才记）。失败仅告警，不影响办结结果。
    if req.comment.is_some() || req.decision.is_some() {
        let _ = crate::biz_link::insert_task_comment(
            &rt,
            &req.instance_id,
            &task_id,
            &node_bpmn_id,
            req.decision.clone(),
            req.comment.clone(),
        )
        .await;
    }
    // 生命周期事件（webhook + SSE 双发）：该任务已办结；若实例随之完成发 instance.completed，
    // 否则为新产生的待办发 task.created。
    {
        publish_event(
            &rt,
            FlowEvent::new(FlowEventKind::TaskCompleted, &req.instance_id, now_rfc3339())
                .task(Some(task_id.clone()), Some(node_bpmn_id.clone())),
        );
        if let Ok(snap) = rt.engine.store().load_snapshot(&req.instance_id).await {
            use cmx_flow_model::InstanceState;
            if snap.instance.state == InstanceState::Completed {
                emit_instance_event(&rt, FlowEventKind::InstanceCompleted, &req.instance_id).await;
            } else {
                // 新开的待办（办结推进后可能产生下一环节任务）。跳过刚办结的任务本身
                // ——M1 简化把已办结任务仍保留在 tasks 表里（见 InstanceSnapshot.tasks 注释），
                // 不排除会把它误当"新建"重复 emit。
                for t in &snap.tasks {
                    if t.id == task_id {
                        continue;
                    }
                    publish_event(
                        &rt,
                        FlowEvent::new(FlowEventKind::TaskCreated, &req.instance_id, now_rfc3339())
                            .definition_key(Some(snap.instance.definition_key.clone()))
                            .business_key(snap.instance.business_key.clone())
                            .task(Some(t.id.clone()), Some(t.node_bpmn_id.clone()))
                            .assignee(t.assignee.clone()),
                    );
                }
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
    load_view(&rt, &req.instance_id).await
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
pub async fn set_instance_variables(
    Path(instance_id): Path<String>,
    Json(req): Json<SetVarsReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let vars = Variables::from_json(req.variables.clone());
    rt.engine
        .set_variables(&instance_id, vars)
        .await
        .map_err(engine_err)?;
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

    // 分页在 DB 层做（每类独立分页；UI 不用 all 组合）。
    let page = if kind == "claimable" {
        crate::biz_link::claimable_tasks_by_user(&q.assignee, &filter)
            .await
            .map_err(msg_err)?
    } else {
        crate::biz_link::open_tasks_by_assignee(&q.assignee, &filter)
            .await
            .map_err(msg_err)?
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
    let f = q.to_filter();
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

// ————————————————————— 子流程组织路由（绑定管理） —————————————————————

/// 组织树（设计器「按组织配置子流程」的组织选择器）。扁平表 + path，前端建树。
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

/// 列某逻辑子流程 key 的全部组织绑定（含默认兜底），带组织名。
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
        "orgId": b.org_id,
        "orgName": b.org_name,
        "targetKey": b.target_definition_key,
        "enabled": b.enabled,
        "remark": b.remark,
        "isDefault": b.org_id.is_none(),
    })
}

/// upsert 绑定请求。orgId 为空/缺省 = 默认兜底绑定。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertBindingReq {
    /// 逻辑子流程 key（= callActivity cmx:calledKey）。
    called_key: String,
    /// 组织 id（None/空 → 默认兜底绑定）。
    #[serde(default)]
    org_id: Option<String>,
    /// 目标子流程定义 key。
    target_key: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    remark: Option<String>,
}

/// upsert 一条组织绑定（同 called_key+org 视为一条）。id 由 called_key+org 派生（幂等）。
pub async fn upsert_subflow_binding(
    Json(req): Json<UpsertBindingReq>,
) -> Result<Json<ApiResp<Value>>> {
    let rt = flow().await?;
    let org = req.org_id.as_deref().filter(|s| !s.is_empty());
    // 派生稳定 id：便于同 (key,org) 反复保存不产生多行（upsert 内也会先删同键旧行）。
    let id = binding_id(&req.called_key, org);
    rt.binding_store
        .upsert(
            &id,
            &req.called_key,
            org,
            &req.target_key,
            req.enabled,
            req.remark.as_deref(),
        )
        .await
        .map_err(msg_err)?;
    Ok(Json(ApiResp::ok(json!({ "id": id }))))
}

/// 从 called_key + org 派生稳定绑定 id（非加密，仅去重定位用）。
fn binding_id(called_key: &str, org: Option<&str>) -> String {
    let raw = format!("{called_key}|{}", org.unwrap_or("__default__"));
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
