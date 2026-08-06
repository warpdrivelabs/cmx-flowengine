/*
 * @Describe: cmx-flow 流程引擎自包含演示 —— 内嵌 axum + 前端可视化。
 *
 * 启动流程：注册 fico 数据源 → 建表 → 部署两个流程定义（信用审批 + 报销会签）+ 注册
 * delegate → 起 HTTP 服务。引擎生命周期：deploy/register_delegate 是 &mut self（启动期
 * 一次性完成），之后 Arc 共享，start_process/complete_task/cancel_process 是 &self，
 * 天然适合多请求并发。
 *
 * REST API（前端消费）：
 *   GET  /api/definitions             全部已部署流程定义（节点 + 边，供前端画流程图）
 *   POST /api/instances               启动实例 {definitionKey, applicant, amount, approvers?}
 *   GET  /api/instances               列全部实例（从 store 载入，重启可恢复）
 *   GET  /api/instances/:id           单实例详情（状态 + 令牌位置 + 待办任务 + 会签元素）
 *   POST /api/tasks/:id/complete       办结任务 {instance_id, decision?} → 最新实例视图
 *   POST /api/instances/:id/cancel     撤单/取消实例 {reason?} → 最新实例视图（M3）
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cmx_database_pg::{DbConfig, DbType, execute_sql, get_default_pg_db_manager};
use cmx_flow_bpmn::compile;
use cmx_flow_def::{DefinitionService, PgDefinitionStore};
use cmx_flow_engine::{
    DelegateContext, Engine, JavaDelegate, ProcessDefinition, RuntimeStore, Variables,
};
use cmx_flow_model::{InstanceSnapshot, NodeKind};
use cmx_flow_store_pg::{PgIamAssigneeResolver, PgRuntimeStore, PgSubflowRouter};

const DB_ID: &str = "fico";
/// IAM 所在库（候选人解析：cmx_role/cmx_user_role/cmx_position/cmx_user_position 在此库）。
const IAM_DB_ID: &str = "cmx";
const BPMN_CREDIT: &str = include_str!("../resources/credit_approval.bpmn20.xml");
const BPMN_COUNTERSIGN: &str = include_str!("../resources/expense_countersign.bpmn20.xml");
const BPMN_TIMED: &str = include_str!("../resources/timed_approval.bpmn20.xml");
const BPMN_CANDIDATE: &str = include_str!("../resources/candidate_approval.bpmn20.xml");
const BPMN_SUBFLOW_MAIN: &str = include_str!("../resources/subflow_main.bpmn20.xml");
const BPMN_FIN_HQ: &str = include_str!("../resources/fin_review_hq.bpmn20.xml");
const BPMN_FIN_BRANCH: &str = include_str!("../resources/fin_review_branch.bpmn20.xml");
const INDEX_HTML: &str = include_str!("../web/index.html");
const DESIGNER_HTML: &str = include_str!("../web/designer.html");

/// 共享应用状态：引擎（含 PG store）+ 全部已编译定义（供前端画图）+ 定义服务（设计器草稿/发布）。
#[derive(Clone)]
struct AppState {
    engine: Arc<Engine<PgRuntimeStore>>,
    definitions: Arc<Vec<ProcessDefinition>>,
    def_svc: Arc<DefinitionService<PgDefinitionStore>>,
}

/// serviceTask delegate：按金额算风险等级写回变量（供前端展示，也可驱动后续判断）。
struct RiskDelegate;

#[async_trait::async_trait]
impl JavaDelegate for RiskDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), String> {
        let amount = ctx
            .variables
            .get("amount")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let level = if amount > 50000.0 {
            "高"
        } else if amount > 10000.0 {
            "中"
        } else {
            "低"
        };
        ctx.variables.set("riskLevel", json!(level));
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,cmx_flow_demo=debug")
        .init();

    // 1) 注册 fico（flow 运行态，默认源）+ cmx（IAM 候选人解析源）两个数据源。
    let url = std::env::var("FICO_PG_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/fico".to_string());
    register_datasource(DB_ID, &url, true).await;
    let iam_url = std::env::var("IAM_PG_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:5432/cmx".to_string());
    register_datasource(IAM_DB_ID, &iam_url, false).await;

    // 2) 建表（幂等）。
    let store = PgRuntimeStore::new(DB_ID);
    store
        .ensure_schema()
        .await
        .expect("建表失败，请检查 fico 库连接");
    tracing::info!("✅ cmx_flow_* 表已就绪（fico 库）");

    // 2.5) 幂等播种演示 IAM 数据（角色/岗位/用户关联，df_ 前缀，供候选人解析演示）。
    seed_demo_iam().await;

    // 3) 编译 + 部署流程定义（含 M5.1 主/子流程），注册 delegate + 候选人解析器（连 cmx 库 IAM）。
    let credit = compile(BPMN_CREDIT).expect("信用审批 BPMN 编译失败");
    let countersign = compile(BPMN_COUNTERSIGN).expect("报销会签 BPMN 编译失败");
    let timed = compile(BPMN_TIMED).expect("限时审批 BPMN 编译失败");
    let candidate = compile(BPMN_CANDIDATE).expect("候选人审批 BPMN 编译失败");
    let subflow_main = compile(BPMN_SUBFLOW_MAIN).expect("子流程主流程 BPMN 编译失败");
    let fin_hq = compile(BPMN_FIN_HQ).expect("总部财务复核 BPMN 编译失败");
    let fin_branch = compile(BPMN_FIN_BRANCH).expect("分公司财务复核 BPMN 编译失败");
    // 全部定义都给前端（供画图/画子流程内嵌图）；子流程由 startable 标记区分，
    // 前端选择器只列可发起的顶层流程，子流程仅用于在主流程 callActivity 节点内展示路径图。
    let defs_for_state = vec![
        credit.clone(),
        countersign.clone(),
        timed.clone(),
        candidate.clone(),
        subflow_main.clone(),
        fin_hq.clone(),
        fin_branch.clone(),
    ];
    let mut engine = Engine::new(store);
    engine.deploy(credit).expect("部署信用审批失败");
    engine.deploy(countersign).expect("部署报销会签失败");
    engine.deploy(timed).expect("部署限时审批失败");
    engine.deploy(candidate).expect("部署候选人审批失败");
    engine.deploy(subflow_main).expect("部署子流程主流程失败");
    engine.deploy(fin_hq).expect("部署总部财务复核失败");
    engine.deploy(fin_branch).expect("部署分公司财务复核失败");
    engine.register_delegate("riskDelegate", RiskDelegate);
    engine.set_resolver(Arc::new(PgIamAssigneeResolver::new(IAM_DB_ID)));
    engine.set_subflow_router(Arc::new(PgSubflowRouter::new(IAM_DB_ID)));
    tracing::info!(
        "✅ 流程定义已部署：credit / countersign / timed / candidate / subflow_main(+hq/branch 子流程)"
    );

    // 3.6) 定义服务（设计器草稿/发布）。建表 + 装载库里已发布的定义（设计器产物）。
    //      内置 include_str! 定义是 seed；设计器发布的定义从库读，二者都进引擎。
    let def_svc = DefinitionService::new(PgDefinitionStore::new(DB_ID));
    def_svc.ensure_schema().await.expect("建定义表失败");

    // 幂等种入内置定义到定义库，让设计器列表/详情与引擎同源（内置流程也能在设计器里打开编辑）。
    // 已存在则跳过，不覆盖用户在设计器里的改动。
    for (xml, name) in [
        (BPMN_CREDIT, "信用审批"),
        (BPMN_COUNTERSIGN, "报销会签"),
        (BPMN_TIMED, "限时审批"),
        (BPMN_CANDIDATE, "候选人审批"),
        (BPMN_SUBFLOW_MAIN, "子流程主流程"),
        (BPMN_FIN_HQ, "总部财务复核"),
        (BPMN_FIN_BRANCH, "分公司财务复核"),
    ] {
        match def_svc.seed_if_absent(name, Some("demo".into()), xml).await {
            Ok(true) => tracing::info!(name, "种入内置定义到设计器库"),
            Ok(false) => {}
            Err(e) => tracing::warn!(name, error = %e, "种入内置定义失败"),
        }
    }

    let mut defs_for_state = defs_for_state;
    match def_svc.load_published_definitions().await {
        Ok((loaded, errors)) => {
            for (k, e) in &errors {
                tracing::warn!(def = %k, error = %e, "已发布定义编译失败，跳过装载");
            }
            for def in loaded {
                tracing::info!(key = %def.key, "装载设计器发布的定义");
                defs_for_state.push(def.clone());
                if let Err(e) = engine.deploy(def) {
                    tracing::warn!(error = %e, "装载设计器定义失败");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "读取已发布定义失败"),
    }

    let state = AppState {
        engine: Arc::new(engine),
        definitions: Arc::new(defs_for_state),
        def_svc: Arc::new(def_svc),
    };

    // 3.5) 后台定时器 poller：每 5 秒推进一次到期定时器（引擎不自带后台线程，宿主驱动）。
    {
        let engine = state.engine.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                match engine.trigger_due_timers(100).await {
                    Ok(fired) if !fired.is_empty() => {
                        for f in &fired {
                            tracing::info!(
                                instance = %f.instance_id,
                                boundary = %f.boundary_bpmn_id,
                                interrupting = f.cancel_activity,
                                "⏰ 定时器触发"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "定时器推进出错"),
                }
            }
        });
    }

    // 4) 路由。
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/designer", get(|| async { Html(DESIGNER_HTML) }))
        .route("/api/definitions", get(get_definitions))
        .route("/api/definitions/draft", post(save_definition_draft))
        .route("/api/definitions/{key}", get(get_definition_detail))
        .route("/api/definitions/{key}/publish", post(publish_definition))
        .route("/api/instances", get(list_instances).post(start_instance))
        .route("/api/instances/{id}", get(get_instance))
        .route("/api/instances/{id}/children", get(get_children))
        .route("/api/instances/{id}/cancel", post(cancel_instance))
        .route("/api/tasks/{id}/complete", post(complete_task))
        .route("/api/tasks/{id}/claim", post(claim_task))
        .route("/api/tasks/{id}/transfer", post(transfer_task))
        .route("/api/tasks/{id}/delegate", post(delegate_task))
        .route("/api/tasks/{id}/addsign", post(add_sign_task))
        .route("/api/users", get(list_users))
        .route("/api/cc", get(list_cc))
        .route("/api/cc/{id}/read", post(mark_cc_read))
        .route("/api/timers/trigger", post(trigger_timers))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    // 端口默认 8090（避开生产 web-server 常用的 8080），可用 DEMO_PORT 覆盖。
    let port = std::env::var("DEMO_PORT").unwrap_or_else(|_| "8090".to_string());
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("端口占用");
    tracing::info!("🚀 演示已启动 → http://{addr}");
    axum::serve(listener, app).await.expect("服务异常退出");
}

/// 注册一个数据源（default 决定是否为默认源）。
async fn register_datasource(db_id: &str, url: &str, default: bool) {
    let cfg = DbConfig {
        db_type: DbType::Postgres,
        db_url: url.to_string(),
        db_id: db_id.to_string(),
        db_name: None,
        db_schema: Some("public".to_string()),
        default,
        pool_config: Default::default(),
        health_check_interval: 60,
        health_check_timeout: 5,
        domain_code: None,
        application_code: None,
        module_code: None,
        source_type: Some("default".to_string()),
    };
    get_default_pg_db_manager()
        .register_data_source(cfg)
        .await
        .unwrap_or_else(|e| panic!("注册数据源 {db_id} 失败: {e}"));
}

/// 幂等播种演示 IAM 数据（角色 df_finance 三人、岗位 df_mgr 一人）到 cmx 库。
///
/// 用 df_ 前缀标识演示数据，不与真实数据冲突；`ON CONFLICT DO NOTHING` 幂等，重启无害。
/// 用户直接借用真实 cmx_user（取前几个 id），只补角色/岗位与关联行。
async fn seed_demo_iam() {
    // 取 cmx 库里真实存在的几个用户 id（借来做演示办理人；不新建用户）。
    let users = match cmx_database_pg::query_sql(
        IAM_DB_ID,
        None,
        "SELECT id FROM cmx_user WHERE archived = 0 ORDER BY create_time LIMIT 4",
        "seed_users",
    )
    .await
    {
        Ok(ds) => {
            let schema = ds.schema.as_ref();
            ds.iter()
                .filter_map(|row| match row.get_by_name(schema, "id") {
                    Some(cmx_core::model::cell::DataValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        }
        Err(e) => {
            tracing::warn!(error = %e, "播种 IAM 取用户失败，跳过（候选人 demo 可能无数据）");
            return;
        }
    };
    if users.len() < 3 {
        tracing::warn!("cmx 库用户不足 3 个，候选人 demo 数据不完整");
        return;
    }

    // 角色 df_finance + 岗位 df_mgr。
    let stmts = vec![
        "INSERT INTO cmx_role (id, code, name, archived) VALUES ('df_role_fin','df_finance','财务组(演示)',0) ON CONFLICT (id) DO NOTHING".to_string(),
        "INSERT INTO cmx_position (id, code, name, archived) VALUES ('df_pos_mgr','df_mgr','部门经理(演示)',0) ON CONFLICT (id) DO NOTHING".to_string(),
        // 财务组三人（借前 3 个真实用户）。
        format!("INSERT INTO cmx_user_role (id, user_id, role_id, archived) VALUES ('df_ur_1','{}','df_role_fin',0) ON CONFLICT (id) DO NOTHING", users[0]),
        format!("INSERT INTO cmx_user_role (id, user_id, role_id, archived) VALUES ('df_ur_2','{}','df_role_fin',0) ON CONFLICT (id) DO NOTHING", users[1]),
        format!("INSERT INTO cmx_user_role (id, user_id, role_id, archived) VALUES ('df_ur_3','{}','df_role_fin',0) ON CONFLICT (id) DO NOTHING", users[2]),
        // 部门经理岗位一人（第 4 个，或复用第 1 个）。
        format!("INSERT INTO cmx_user_position (id, user_id, position_id, archived) VALUES ('df_up_1','{}','df_pos_mgr',0) ON CONFLICT (id) DO NOTHING", users.get(3).unwrap_or(&users[0])),
        // —— M5.2 组织树 + 子流程绑定 —— //
        // 绑定表建在 cmx 库（与 cmx_org 同库，PgSubflowRouter 指向 IAM_DB_ID）。
        "CREATE TABLE IF NOT EXISTS cmx_flow_subflow_binding (id VARCHAR(64) PRIMARY KEY, called_key VARCHAR(128) NOT NULL, org_id VARCHAR(64), target_definition_key VARCHAR(128) NOT NULL, enabled BOOLEAN NOT NULL DEFAULT TRUE, remark VARCHAR(500), created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now())".to_string(),
        "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_key ON cmx_flow_subflow_binding (called_key)".to_string(),
        // 组织树：df_root 总部 → df_bj 北京(挂总部下) / df_sh 上海(挂总部下)。path 物化路径。
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_root','df_root','演示总部',NULL,'/df_root',0) ON CONFLICT (id) DO UPDATE SET path='/df_root', archived=0".to_string(),
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_bj','df_bj','北京分公司','df_root','/df_root/df_bj',0) ON CONFLICT (id) DO UPDATE SET path='/df_root/df_bj', archived=0".to_string(),
        "INSERT INTO cmx_org (id, code, name, parent_id, path, archived) VALUES ('df_sh','df_sh','上海分公司','df_root','/df_root/df_sh',0) ON CONFLICT (id) DO UPDATE SET path='/df_root/df_sh', archived=0".to_string(),
        // 子流程绑定：总部 fin_review→总部三级(fin_review_hq)；上海精确绑→分公司单签(fin_review_branch)；
        // 北京不绑（跑时应沿 path 继承总部）；默认兜底→总部三级。
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_sb_hq','fin_review','df_root','fin_review_hq',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_hq', enabled=TRUE".to_string(),
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_sb_sh','fin_review','df_sh','fin_review_branch',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_branch', enabled=TRUE".to_string(),
        "INSERT INTO cmx_flow_subflow_binding (id, called_key, org_id, target_definition_key, enabled, created_at, updated_at) VALUES ('df_sb_def','fin_review',NULL,'fin_review_hq',TRUE,now(),now()) ON CONFLICT (id) DO UPDATE SET target_definition_key='fin_review_hq', enabled=TRUE".to_string(),
    ];
    for sql in stmts {
        if let Err(e) = execute_sql(IAM_DB_ID, None, &sql).await {
            tracing::warn!(error = %e, "播种 IAM 语句失败");
        }
    }
    tracing::info!(
        "✅ 演示数据已播种（角色/岗位 + 组织树 df_root/df_bj/df_sh + 子流程绑定 fin_review）"
    );
}

// ============================ handlers ============================

/// 全部流程定义 → 前端画图用的 JSON（每个含节点 + 边）。
async fn get_definitions(State(st): State<AppState>) -> Json<Value> {
    let defs: Vec<Value> = st.definitions.iter().map(definition_view).collect();
    Json(json!({ "definitions": defs }))
}

/// 设计器：存草稿（先试编译挡回非法 BPMN）。body = { name, module?, category?, bpmnXml }。
async fn save_definition_draft(
    State(st): State<AppState>,
    Json(req): Json<SaveDraftReq>,
) -> Result<Json<Value>, ApiError> {
    let rec = st
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
        .map_err(ApiError::from_def)?;
    Ok(Json(json!({
        "key": rec.key,
        "name": rec.name,
        "state": rec.state.as_str(),
        "activeVersion": rec.active_version,
    })))
}

/// 设计器：取单个定义详情（含草稿 XML，供重新加载编辑）。
async fn get_definition_detail(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rec = st
        .def_svc
        .get(&key)
        .await
        .map_err(ApiError::from_def)?
        .ok_or_else(|| ApiError(format!("定义不存在: {key}")))?;
    Ok(Json(json!({
        "key": rec.key,
        "name": rec.name,
        "module": rec.module,
        "category": rec.category,
        "state": rec.state.as_str(),
        "activeVersion": rec.active_version,
        "bpmnXml": rec.draft_xml,
        "updatedAt": rec.updated_at.to_rfc3339(),
    })))
}

/// 设计器：发布（草稿 → 版本 +1）。注意：新版在下次服务重启装载生效（Phase 5 做热更）。
async fn publish_definition(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Json(req): Json<PublishReq>,
) -> Result<Json<Value>, ApiError> {
    let version = st
        .def_svc
        .publish(&key, req.note, req.published_by)
        .await
        .map_err(ApiError::from_def)?;
    Ok(Json(json!({
        "key": key,
        "version": version,
        "note": "已发布；重启服务后引擎装载新版（热更列入后续阶段）",
    })))
}

/// 单份流程定义 → JSON（key/名/节点/边/是否可发起）。
fn definition_view(def: &ProcessDefinition) -> Value {
    let nodes: Vec<Value> = def
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.bpmn_id,
                "name": n.name,
                "kind": node_kind_str(&n.kind),
                "multiInstance": node_multi_instance(&n.kind),
                "boundaryTimer": node_boundary_timer(&n.kind),
                "calledElement": node_called_element(&n.kind),
            })
        })
        .collect();
    let edges: Vec<Value> = def
        .nodes
        .iter()
        .flat_map(|n| {
            n.outgoing.iter().map(move |f| {
                json!({
                    "from": n.bpmn_id,
                    "to": f.target_bpmn_id,
                    "condition": f.condition,
                    "isDefault": f.is_default,
                })
            })
        })
        .collect();
    // 子流程（被 callActivity 调用的）不在发起列表；其余为可发起顶层流程。
    let startable = !matches!(def.key.as_str(), "fin_review_hq" | "fin_review_branch");
    json!({ "key": def.key, "name": def.name, "nodes": nodes, "edges": edges, "startable": startable })
}

/// 节点若为 callActivity，返回其调用的子流程定义 key（供前端画子流程内嵌路径图）。
fn node_called_element(kind: &NodeKind) -> Value {
    if let NodeKind::CallActivity(ca) = kind {
        // M5.1 用 called_element 写死；M5.2 若走 called_key 逻辑名，也一并给前端。
        let target = if !ca.called_element.is_empty() {
            ca.called_element.clone()
        } else {
            ca.called_key.clone().unwrap_or_default()
        };
        return json!(target);
    }
    Value::Null
}

/// 节点若为多实例 userTask，返回其会签/或签摘要（供前端标注）。
fn node_multi_instance(kind: &NodeKind) -> Value {
    if let NodeKind::UserTask(ut) = kind
        && let Some(mi) = &ut.multi_instance
    {
        return json!({
            "sequential": mi.sequential,
            "collectionVar": mi.collection_var,
            "completionCondition": mi.completion_condition,
        });
    }
    Value::Null
}

/// 节点若为边界定时器事件，返回其宿主/时长/中断性摘要（供前端画徽章）。
fn node_boundary_timer(kind: &NodeKind) -> Value {
    if let NodeKind::BoundaryTimerEvent(bt) = kind {
        return json!({
            "attachedTo": bt.attached_to_bpmn_id,
            "seconds": bt.duration.seconds,
            "cancelActivity": bt.cancel_activity,
        });
    }
    Value::Null
}

/// 设计器：存草稿请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDraftReq {
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

/// 设计器：发布请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishReq {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    published_by: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartReq {
    /// 流程定义 key（credit_approval / expense_countersign）。
    #[serde(default)]
    definition_key: Option<String>,
    applicant: String,
    amount: f64,
    /// 会签审批人列表（报销会签用；credit_approval 忽略）。
    #[serde(default)]
    approvers: Option<Vec<String>>,
    /// 发起组织（M5.2 子流程路由用；子流程主流程按此选具体子流程）。
    #[serde(default)]
    org_id: Option<String>,
}

/// 启动一个流程实例（信用审批或报销会签）。
async fn start_instance(
    State(st): State<AppState>,
    Json(req): Json<StartReq>,
) -> Result<Json<Value>, ApiError> {
    let def_key = req
        .definition_key
        .clone()
        .unwrap_or_else(|| "credit_approval".to_string());

    let mut vars = Variables::new();
    vars.set("applicant", json!(req.applicant));
    vars.set("amount", json!(req.amount));
    if let Some(approvers) = &req.approvers {
        vars.set("approvers", json!(approvers));
    }

    let biz_key = format!("CR-{}", req.applicant);
    let result = st
        .engine
        .start_process_org(&def_key, vars, Some(biz_key), req.org_id.clone())
        .await
        .map_err(ApiError::from_engine)?;

    let snap = st
        .engine
        .store()
        .load_snapshot(&result.instance_id)
        .await
        .map_err(|e| ApiError(format!("载入实例失败: {e}")))?;
    Ok(Json(instance_view(&snap)))
}

/// 列全部实例（直接从 store 查，重启后仍可从 fico 库恢复）。
async fn list_instances(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let summaries = st
        .engine
        .store()
        .list_instances(100)
        .await
        .map_err(|e| ApiError(format!("查询实例列表失败: {e}")))?;
    let instances: Vec<Value> = summaries.iter().map(summary_view).collect();
    Ok(Json(json!({ "instances": instances })))
}

/// 单实例详情。
async fn get_instance(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let snap = st
        .engine
        .store()
        .load_snapshot(&id)
        .await
        .map_err(|_| ApiError(format!("实例不存在: {id}")))?;
    Ok(Json(instance_view(&snap)))
}

/// 列某实例的子实例（M5.1 子流程）——供前端展开「子流程实例」。
async fn get_children(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let children = st
        .engine
        .store()
        .find_child_instances(&id)
        .await
        .map_err(|e| ApiError(format!("查询子实例失败: {e}")))?;
    // 每个子实例载入其快照取待办任务，便于前端直接办理。
    let mut items = Vec::new();
    for c in &children {
        if let Ok(snap) = st.engine.store().load_snapshot(&c.id).await {
            items.push(instance_view(&snap));
        }
    }
    Ok(Json(json!({ "children": items })))
}

#[derive(Deserialize)]
struct CompleteReq {
    instance_id: String,
    /// 审批意见（可选，仅记录到变量，供演示展示）。
    #[serde(default)]
    decision: Option<String>,
}

/// 办结一个任务。
async fn complete_task(
    State(st): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<CompleteReq>,
) -> Result<Json<Value>, ApiError> {
    let mut vars = Variables::new();
    if let Some(d) = &req.decision {
        // 用 taskDecision 记录最近一次意见（demo 简化）。
        vars.set("lastDecision", json!(d));
    }
    st.engine
        .complete_task(&req.instance_id, &task_id, vars)
        .await
        .map_err(ApiError::from_engine)?;

    let snap = st
        .engine
        .store()
        .load_snapshot(&req.instance_id)
        .await
        .map_err(|e| ApiError(format!("载入实例失败: {e}")))?;
    Ok(Json(instance_view(&snap)))
}

#[derive(Deserialize)]
struct ClaimReq {
    instance_id: String,
    /// 认领人 user id。
    user_id: String,
}

/// 认领一个候选任务（M4.1）——候选池里的用户据为己有。
async fn claim_task(
    State(st): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<ClaimReq>,
) -> Result<Json<Value>, ApiError> {
    st.engine
        .claim_task(&req.instance_id, &task_id, &req.user_id)
        .await
        .map_err(ApiError::from_engine)?;
    let snap = st
        .engine
        .store()
        .load_snapshot(&req.instance_id)
        .await
        .map_err(|e| ApiError(format!("载入实例失败: {e}")))?;
    Ok(Json(instance_view(&snap)))
}

#[derive(Deserialize)]
struct TransferReq {
    instance_id: String,
    from_user: String,
    to_user: String,
    #[serde(default)]
    reason: Option<String>,
}

/// 转办（M4.3）。
async fn transfer_task(
    State(st): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<TransferReq>,
) -> Result<Json<Value>, ApiError> {
    st.engine
        .transfer_task(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.reason.as_deref(),
        )
        .await
        .map_err(ApiError::from_engine)?;
    load_view(&st, &req.instance_id).await
}

/// 委派（M4.3）。
async fn delegate_task(
    State(st): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<TransferReq>,
) -> Result<Json<Value>, ApiError> {
    st.engine
        .delegate_task(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.reason.as_deref(),
        )
        .await
        .map_err(ApiError::from_engine)?;
    load_view(&st, &req.instance_id).await
}

#[derive(Deserialize)]
struct AddSignReq {
    instance_id: String,
    from_user: String,
    to_user: String,
    /// true=向前加签，false=向后加签。
    #[serde(default = "default_true")]
    before: bool,
    #[serde(default)]
    reason: Option<String>,
}
fn default_true() -> bool {
    true
}

/// 加签（M4.3）。
async fn add_sign_task(
    State(st): State<AppState>,
    Path(task_id): Path<String>,
    Json(req): Json<AddSignReq>,
) -> Result<Json<Value>, ApiError> {
    st.engine
        .add_sign(
            &req.instance_id,
            &task_id,
            &req.from_user,
            &req.to_user,
            req.before,
            req.reason.as_deref(),
        )
        .await
        .map_err(ApiError::from_engine)?;
    load_view(&st, &req.instance_id).await
}

/// 载入实例并返回视图（转签家族共用）。
async fn load_view(st: &AppState, instance_id: &str) -> Result<Json<Value>, ApiError> {
    let snap = st
        .engine
        .store()
        .load_snapshot(instance_id)
        .await
        .map_err(|e| ApiError(format!("载入实例失败: {e}")))?;
    Ok(Json(instance_view(&snap)))
}

#[derive(Deserialize)]
struct CancelReq {
    /// 撤单原因（可选）。
    #[serde(default)]
    reason: Option<String>,
}

/// 撤单 / 取消一个流程实例（M3）。
async fn cancel_instance(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CancelReq>,
) -> Result<Json<Value>, ApiError> {
    st.engine
        .cancel_process(&id, req.reason)
        .await
        .map_err(ApiError::from_engine)?;

    let snap = st
        .engine
        .store()
        .load_snapshot(&id)
        .await
        .map_err(|e| ApiError(format!("载入实例失败: {e}")))?;
    Ok(Json(instance_view(&snap)))
}

/// 手动「立即检查到期定时器」（M2.5）——前端「立即触发」按钮用，免等后台 5s 轮询。
async fn trigger_timers(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let fired = st
        .engine
        .trigger_due_timers(100)
        .await
        .map_err(ApiError::from_engine)?;
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
    Ok(Json(json!({ "firedCount": fired.len(), "fired": items })))
}

/// 列出 cmx 库用户（id → 昵称/用户名），供前端把候选人 id 显示成友好名字。
async fn list_users() -> Result<Json<Value>, ApiError> {
    let ds = cmx_database_pg::query_sql(
        IAM_DB_ID,
        None,
        "SELECT id, username, nickname FROM cmx_user WHERE archived = 0 ORDER BY create_time LIMIT 200",
        "list_users",
    )
    .await
    .map_err(|e| ApiError(format!("查询用户失败: {e}")))?;
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
    Ok(Json(json!({ "users": users })))
}

#[derive(Deserialize)]
struct CcQuery {
    /// 查谁的抄送。
    user: String,
    /// 只看未读。
    #[serde(default)]
    unread: bool,
}

/// 「抄送我的」列表（M4.2）。
async fn list_cc(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<CcQuery>,
) -> Result<Json<Value>, ApiError> {
    let items = st
        .engine
        .cc_for_user(&q.user, q.unread, 100)
        .await
        .map_err(ApiError::from_engine)?;
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
    Ok(Json(json!({ "cc": cc })))
}

/// 标记一条抄送已读（M4.2）。
async fn mark_cc_read(
    State(st): State<AppState>,
    Path(cc_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let ok = st
        .engine
        .mark_cc_read(&cc_id)
        .await
        .map_err(ApiError::from_engine)?;
    Ok(Json(json!({ "ok": ok })))
}

// ============================ 视图组装 ============================

/// 详细实例视图：状态 + 变量 + 令牌当前位置 + 待办任务。
fn instance_view(snap: &InstanceSnapshot) -> Value {
    let tokens: Vec<Value> = snap
        .tokens
        .iter()
        .map(|t| json!({ "nodeBpmnId": t.node_bpmn_id, "state": token_state_str(t.state) }))
        .collect();
    let tasks: Vec<Value> = snap
        .tasks
        .iter()
        .map(|t| {
            // 该任务的候选人（M4.1）：从候选池筛出，供前端展示"谁可认领"。
            let cands: Vec<Value> = snap
                .candidates
                .iter()
                .filter(|c| c.task_id == t.id)
                .map(|c| {
                    json!({
                        "userId": c.resolved_user_id,
                        "type": candidate_kind_str(c.candidate_type),
                        "ref": c.candidate_ref,
                    })
                })
                .collect();
            json!({
                "id": t.id,
                "nodeBpmnId": t.node_bpmn_id,
                "name": t.name,
                "assignee": t.assignee,
                "ownerUserId": t.owner_user_id,
                "parentTaskId": t.parent_task_id,
                "delegationState": t.delegation_state,
                "elementValue": t.element_value,
                "candidates": cands,
                "completed": t.completed,
            })
        })
        .collect();
    // 活动节点（供前端高亮）：未办结任务所在节点 + 非 Ended 令牌所在节点。
    let mut active: Vec<String> = snap
        .tokens
        .iter()
        .filter(|t| !matches!(t.state, cmx_flow_model::TokenState::Ended))
        .map(|t| t.node_bpmn_id.clone())
        .collect();
    active.sort();
    active.dedup();

    // 定时器作业（供前端画倒计时）。
    let jobs: Vec<Value> = snap
        .jobs
        .iter()
        .map(|j| {
            json!({
                "boundaryBpmnId": j.boundary_bpmn_id,
                "cancelActivity": j.cancel_activity,
                "dueAt": j.due_at.to_rfc3339(),
            })
        })
        .collect();

    // 抄送记录（供实例详情展示"已知会谁"）。
    let cc_records: Vec<Value> = snap
        .cc_records
        .iter()
        .map(|c| {
            json!({
                "toUserId": c.to_user_id,
                "nodeBpmnId": c.node_bpmn_id,
                "read": c.read_at.is_some(),
            })
        })
        .collect();

    // 转签台账（供实例详情画流转链）。
    let delegations: Vec<Value> = snap
        .delegations
        .iter()
        .map(|d| {
            json!({
                "kind": d.kind,
                "fromUserId": d.from_user_id,
                "toUserId": d.to_user_id,
                "reason": d.reason,
                "createdAt": d.created_at.to_rfc3339(),
            })
        })
        .collect();

    json!({
        "id": snap.instance.id,
        "definitionKey": snap.instance.definition_key,
        "businessKey": snap.instance.business_key,
        "state": instance_state_str(snap.instance.state),
        "variables": snap.instance.variables.to_json(),
        "parentInstanceId": snap.instance.parent_instance_id,
        "waitingSubflow": snap.tokens.iter().any(|t| matches!(t.state, cmx_flow_model::TokenState::WaitingSubflow)),
        "tokens": tokens,
        "tasks": tasks,
        "activeNodes": active,
        "jobs": jobs,
        "ccRecords": cc_records,
        "delegations": delegations,
        "openTasks": tasks.iter().filter(|t| t["completed"] == json!(false)).cloned().collect::<Vec<_>>(),
    })
}

/// 摘要视图（列表用）——直接映射 store 返回的 InstanceSummary。
fn summary_view(s: &cmx_flow_model::InstanceSummary) -> Value {
    let vars = s.variables.to_json();
    json!({
        "id": s.id,
        "definitionKey": s.definition_key,
        "businessKey": s.business_key,
        "applicant": vars.get("applicant").cloned().unwrap_or(Value::Null),
        "amount": vars.get("amount").cloned().unwrap_or(Value::Null),
        "riskLevel": vars.get("riskLevel").cloned().unwrap_or(Value::Null),
        "state": instance_state_str(s.state),
        "openTaskCount": s.open_task_count,
    })
}

fn node_kind_str(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::StartEvent => "startEvent",
        NodeKind::EndEvent => "endEvent",
        NodeKind::UserTask(_) => "userTask",
        NodeKind::ServiceTask(_) => "serviceTask",
        NodeKind::ExclusiveGateway => "exclusiveGateway",
        NodeKind::ParallelGateway => "parallelGateway",
        NodeKind::BoundaryTimerEvent(_) => "boundaryTimerEvent",
        NodeKind::CallActivity(_) => "callActivity",
    }
}

fn instance_state_str(s: cmx_flow_model::InstanceState) -> &'static str {
    use cmx_flow_model::InstanceState::*;
    match s {
        Active => "ACTIVE",
        Completed => "COMPLETED",
        Terminated => "TERMINATED",
    }
}

fn candidate_kind_str(k: cmx_flow_model::CandidateKind) -> &'static str {
    use cmx_flow_model::CandidateKind::*;
    match k {
        User => "USER",
        Role => "ROLE",
        Position => "POSITION",
        Org => "ORG",
    }
}

fn token_state_str(s: cmx_flow_model::TokenState) -> &'static str {
    use cmx_flow_model::TokenState::*;
    match s {
        Active => "ACTIVE",
        Waiting => "WAITING",
        Joining => "JOINING",
        WaitingSubflow => "WAITING_SUBFLOW",
        Ended => "ENDED",
    }
}

// ============================ 错误 ============================

/// 简易 API 错误（demo 用；生产应对齐 cmx-api 的 ApiResp 规范）。
#[derive(Serialize)]
struct ApiError(String);

impl ApiError {
    fn from_engine(e: cmx_flow_engine::Error) -> Self {
        ApiError(e.to_string())
    }
    fn from_def(e: cmx_flow_def::DefError) -> Self {
        ApiError(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": self.0 }))).into_response()
    }
}
