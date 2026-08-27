/*
 * @Describe: cmx-flow 引擎单例（web-server 集成用）。
 *
 * 与 RPT 无状态模块的关键差异：Engine::deploy / set_resolver / set_subflow_router 都要 &mut self，
 * 引擎必须在启动时建好、装载完已发布定义、注入 resolver/router 后才能包 Arc 共享。故用
 * tokio::sync::OnceCell 存一个 FlowRuntime 单例，首次访问（get_or_try_init）时构建一次。
 *
 * db_id 对齐 web-server（dev-local.toml 注册）：
 *   FLOW_DB_ID="fico-db" —— 运行态 store + 定义 store（cmx_flow_* 表所在库）
 *   IAM_DB_ID ="primary" —— 候选人 resolver + 子流程 router（cmx_user/cmx_role/cmx_org 所在库）
 * 两库均已由 web-server init_datasources 注册进 cmx-database-pg 全局 manager，本 crate 不再注册。
 *
 * 生产不 seed demo 的 include_str! BPMN 夹具；定义经设计器 save_draft/publish 落库，启动装载。
 */

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{OnceCell, RwLock};

use cmx_flow_adapters::{
    AdapterConfig, AdapterMode, HttpAssigneeResolver, HttpDelegate, HttpDimensionResolver,
    HttpSubflowRouter, MockAssigneeResolver, MockDelegate, MockSubflowRouter, WebhookSender,
};
use cmx_flow_def::{DefinitionService, PgDefinitionStore};
use cmx_flow_engine::{DelegateContext, Engine, JavaDelegate, ProcessDefinition};
use cmx_flow_store_pg::{
    PgDecisionStore, PgIamAssigneeResolver, PgRuntimeStore, PgSubflowBindingStore, PgSubflowRouter,
    PgVarHistoryStore,
};

use crate::tenant::{DEFAULT_TENANT, current_tenant};
use crate::tenancy::{TenancyConfig, register_tenant_datasources};

/// 默认租户的运行态 store + 定义所在库（cmx_flow_* 表）。多租户下各租户用各自库。
pub const FLOW_DB_ID: &str = "fico-db";
/// 默认租户的 IAM 所在库（候选人解析 + 子流程组织路由）。
pub const IAM_DB_ID: &str = "primary";

/// 当前请求租户的运行态库 db_id（读 task_local 租户 → db_id 映射；无租户回退默认 = FLOW_DB_ID）。
///
/// biz_link 等 free fn 用它替代硬编码 FLOW_DB_ID，从而随请求租户走对应库。
pub fn current_flow_db_id() -> String {
    TenancyConfig::global().flow_db_id(&current_tenant())
}

/// 当前请求租户的 IAM 库 db_id（供 handler 里直查 IAM 的少数 SQL 用，如 list_users）。
/// 无租户 / 无 IAM 模板时回退默认 = IAM_DB_ID。
pub fn current_iam_db_id() -> String {
    TenancyConfig::global().iam_db_id(&current_tenant())
}

/// 是否启用内建身份模块（`FLOW_IDENTITY_MODE=local`）。默认 false → 外接 IAM（零回归）。
///
/// local 模式：候选人解析走 `LocalAssigneeResolver`（fid_* 表），并开放身份管理 CRUD 端点 +
/// 四区工作台。external（默认）：继续用平台 IAM（Pg/Http/Mock），本模块完全不参与。
pub fn identity_is_local() -> bool {
    std::env::var("FLOW_IDENTITY_MODE")
        .map(|v| v.eq_ignore_ascii_case("local"))
        .unwrap_or(false)
}

/// RD5：读维度层级外部解析地址。`FLOW_DIMENSION_MODE=http` 且 `FLOW_DIMENSION_URL` 非空时返回 Some，
/// 否则 None（沿用 PgSubflowRouter 直连字典表继承，零回归）。
fn dimension_http_url() -> Option<String> {
    let mode = std::env::var("FLOW_DIMENSION_MODE").unwrap_or_default();
    if !mode.eq_ignore_ascii_case("http") {
        return None;
    }
    std::env::var("FLOW_DIMENSION_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// 流程运行态聚合：共享引擎 + 已装载定义（供前端画图）+ 定义服务（设计器草稿/发布）。
pub struct FlowRuntime {
    pub engine: Arc<Engine<PgRuntimeStore>>,
    /// 已装载定义列表。RwLock 便于发布后热追加（引擎本身不热部署，见下）。
    pub definitions: Arc<RwLock<Vec<ProcessDefinition>>>,
    pub def_svc: Arc<DefinitionService<PgDefinitionStore>>,
    /// 子流程组织绑定管理（设计态 CRUD + 组织树；与运行期 PgSubflowRouter 同表 IAM 库）。
    pub binding_store: Arc<PgSubflowBindingStore>,
    /// 决策表持久化（businessRuleTask 引用的表随注册落库，启动装载回引擎；flow 租户库）。
    pub decision_store: Arc<PgDecisionStore>,
    /// 变量变更历史 + TTL 归档（start/complete/set-variables 三路径捕获；flow 租户库）。
    pub var_history_store: Arc<PgVarHistoryStore>,
    /// 已注册的自分级路由维度字典（RD2）：供 /flow/dimensions 列举 + 维度条目读取。org 内建不在内。
    pub dimension_specs: Arc<Vec<DimRegistration>>,
    /// 出站生命周期 webhook 发送器（app 层 handler emit 事件，后台异步投递第三方）。
    pub webhook: WebhookSender,
    /// 本运行时所属租户（供后台任务/日志标识）。
    pub tenant: String,
}

/// 一个自分级路由维度字典的注册项（RD2）——把 DCT 字典元数据映射成路由所需的物理规格 + 展示信息。
#[derive(Clone, Debug)]
pub struct DimRegistration {
    /// 维度 key（= 字典 dictCode）。
    pub dim_key: String,
    /// 展示名（维度选择器显示）。
    pub name: String,
    /// 物理规格（表/pk/路径列/分隔符）——喂给 PgSubflowRouter 做继承解析。
    pub spec: cmx_flow_store_pg::DimSpec,
    /// 条目展示名列（如 name / label）。
    pub name_col: String,
    /// 父引用列（自分级有；None = 平级）。
    pub parent_col: Option<String>,
    /// 是否自分级（有物化路径可继承）。
    pub self_hierarchy: bool,
}

/// 从环境变量 `FLOW_ROUTING_DIMENSIONS`（JSON 数组）解析路由维度注册（RD2）。
/// 每项形如：`{"dimKey":"legal_entity","name":"法人公司","table":"cf_legal_entity",
///   "idCol":"id","pathCol":"full_path","delim":".","nameCol":"name","parentCol":"parent_id"}`。
/// 缺省/解析失败 → 空（仅内建 org 维度，向后兼容）。
fn parse_dim_registrations() -> Vec<DimRegistration> {
    let raw = match std::env::var("FLOW_ROUTING_DIMENSIONS") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Vec::new(),
    };
    let arr: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "FLOW_ROUTING_DIMENSIONS 解析失败，忽略（仅内建 org 维度）");
            return Vec::new();
        }
    };
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    arr.into_iter()
        .filter_map(|v| {
            let dim_key = s(&v, "dimKey")?;
            let table = s(&v, "table")?;
            let parent_col = s(&v, "parentCol");
            Some(DimRegistration {
                name: s(&v, "name").unwrap_or_else(|| dim_key.clone()),
                self_hierarchy: parent_col.is_some(),
                spec: cmx_flow_store_pg::DimSpec {
                    table,
                    id_col: s(&v, "idCol").unwrap_or_else(|| "id".into()),
                    path_col: s(&v, "pathCol").unwrap_or_else(|| "full_path".into()),
                    delim: s(&v, "delim").unwrap_or_else(|| ".".into()),
                },
                name_col: s(&v, "nameCol").unwrap_or_else(|| "name".into()),
                parent_col,
                dim_key,
            })
        })
        .collect()
}

/// 每租户运行时缓存：tenant → 该租户运行时的 OnceCell。懒建（首访某租户时建一次）。
///
/// db-per-tenant：每租户一套 store/引擎/定义/webhook，物理隔离。默认租户 = 原单库形态（零回归）。
/// **map 锁只护「取/插空 cell」（廉价、瞬时释放）**，昂贵的 build（连池+建表+装载定义，多次网络往返）
/// 在 per-tenant 的 `OnceCell::get_or_try_init` 内单飞——不持全局锁，故一租户冷启不阻塞其它租户/poller。
/// 这也让「该租户数据源注册 + build」严格执行一次（OnceCell 保证），避免重复 register 把连接池 drop 重建。
type TenantCell = Arc<OnceCell<Arc<FlowRuntime>>>;

static RUNTIMES: std::sync::OnceLock<RwLock<HashMap<String, TenantCell>>> =
    std::sync::OnceLock::new();

fn runtimes() -> &'static RwLock<HashMap<String, TenantCell>> {
    RUNTIMES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 取当前请求租户的流程运行时（handler 用；读 task_local 租户，缓存命中即返，否则懒建）。
pub async fn flow() -> crate::resp::Result<Arc<FlowRuntime>> {
    flow_for_tenant(&current_tenant()).await
}

/// 取指定租户的运行时：per-tenant OnceCell 单飞懒建（首访注册数据源 + build_for 一次）。
pub async fn flow_for_tenant(tenant: &str) -> crate::resp::Result<Arc<FlowRuntime>> {
    // 1) 取该租户的 cell（map 锁只护这一步，瞬时释放；不在锁内做 async build）。
    let cell: TenantCell = {
        // 快路径：读锁命中已存在的 cell。
        if let Some(c) = runtimes().read().await.get(tenant).cloned() {
            c
        } else {
            // 慢路径：写锁下 entry-or-insert 空 cell（廉价）。
            runtimes()
                .write()
                .await
                .entry(tenant.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        }
    };
    // 2) 在 per-tenant cell 上单飞建（不持 map 锁）。已建则直接返缓存。
    cell.get_or_try_init(|| build_tenant_runtime(tenant))
        .await
        .cloned()
}

/// 注册该租户数据源 + build 一次（供 OnceCell::get_or_try_init 调用，保证单飞执行一次）。
async fn build_tenant_runtime(tenant: &str) -> crate::resp::Result<Arc<FlowRuntime>> {
    let cfg = TenancyConfig::global();
    let flow_db = cfg.flow_db_id(tenant);
    let iam_db = cfg.iam_db_id(tenant);
    register_tenant_datasources(tenant, &flow_db, &iam_db)
        .await
        .map_err(|e| bridge(format!("租户[{tenant}]数据源注册失败: {e}")))?;
    Ok(Arc::new(build_for(tenant, &flow_db, &iam_db).await?))
}


/// 构建某租户的流程运行时。所有 &mut 调用在包 Arc 之前完成，规避 &mut/单例竞态。
async fn build_for(
    tenant: &str,
    flow_db_id: &str,
    iam_db_id: &str,
) -> crate::resp::Result<FlowRuntime> {
    // 1) 运行态 store + 定义 store（该租户库），建表。
    let store = PgRuntimeStore::new(flow_db_id);
    store
        .ensure_schema()
        .await
        .map_err(|e| bridge(format!("流程运行态建表失败: {e}")))?;

    let def_svc = DefinitionService::new(PgDefinitionStore::new(flow_db_id));
    def_svc
        .ensure_schema()
        .await
        .map_err(|e| bridge(format!("流程定义建表失败: {e}")))?;

    // 2) 引擎：按适配器配置注入候选人 resolver + 子流程 router + serviceTask delegate。
    //    mode(mock|http|pg) 从环境变量选（AdapterConfig::from_env），**默认全 pg**——与抽核前
    //    写死 Pg* 注入等价，故平台内嵌与既有测试零回归。独立微服务显式设 =http/=mock。
    let cfg = AdapterConfig::from_env();
    let mut engine = Engine::new(store);

    // 候选人解析（身份）：**先看 local 模式**（可选内建身份模块，fid_* 表）——独立部署且无外部
    // IAM 时用。未设 local 则退回适配器 mode(pg 默认/http/mock)，行为与 P0 前完全一致（零回归）。
    if identity_is_local() {
        let fid_db = iam_db_id.to_string();
        // 建 fid_* 表（幂等）。建表失败不致命：记日志后仍注入 resolver（表可能已由迁移建好）。
        let store = cmx_flow_identity::IdentityStore::new(&fid_db);
        if let Err(e) = store.ensure_schema().await {
            tracing::warn!(error = %e, "内建身份 fid_* 建表失败（继续，表可能已存在）");
        }
        tracing::info!(db = %fid_db, "候选人解析走内建身份模块(local, fid_* 表)");
        engine.set_resolver(Arc::new(cmx_flow_identity::LocalAssigneeResolver::new(fid_db)));
    } else {
        match cfg.identity_mode {
            AdapterMode::Pg => {
                engine.set_resolver(Arc::new(PgIamAssigneeResolver::new(iam_db_id)));
            }
            AdapterMode::Http => match &cfg.identity_url {
                Some(url) => {
                    tracing::info!(url, "候选人解析走外部身份服务(http)");
                    engine.set_resolver(Arc::new(HttpAssigneeResolver::new(url.clone())));
                }
                None => {
                    tracing::warn!("FLOW_IDENTITY_MODE=http 但缺 FLOW_IDENTITY_URL，回退 mock");
                    engine.set_resolver(Arc::new(MockAssigneeResolver));
                }
            },
            AdapterMode::Mock => {
                tracing::info!("候选人解析走 mock（脱外部）");
                engine.set_resolver(Arc::new(MockAssigneeResolver));
            }
        }
    }

    // 子流程路由：pg 沿维度字典物化路径继承 / http 接外部服务 / mock 逻辑 key 直返。
    // RD2：pg 模式下把已注册的自分级维度字典规格喂给 router（org 内建，其余从 FLOW_ROUTING_DIMENSIONS）。
    let dim_regs = parse_dim_registrations();
    match cfg.subflow_mode {
        AdapterMode::Pg => {
            let mut router = PgSubflowRouter::new(iam_db_id);
            for d in &dim_regs {
                router.register_dim(d.dim_key.clone(), d.spec.clone());
            }
            // RD5：配了 FLOW_DIMENSION_MODE=http + FLOW_DIMENSION_URL → 继承步经外部 HTTP 读维度层级
            //      （独立部署不共享字典库；绑定表仍本地）。默认 None = 直连字典表继承（零回归）。
            let router = match dimension_http_url() {
                Some(url) => {
                    tracing::info!(url, "子流程继承：维度层级经外部 HTTP 解析（RD5）");
                    router.with_dim_resolver(Arc::new(HttpDimensionResolver::new(url)))
                }
                None => router,
            };
            engine.set_subflow_router(Arc::new(router));
        }
        AdapterMode::Http => match &cfg.subflow_url {
            Some(url) => {
                tracing::info!(url, "子流程路由走外部服务(http)");
                engine.set_subflow_router(Arc::new(HttpSubflowRouter::new(url.clone())));
            }
            None => {
                tracing::warn!("FLOW_SUBFLOW_MODE=http 但缺 FLOW_SUBFLOW_URL，回退 mock");
                engine.set_subflow_router(Arc::new(MockSubflowRouter));
            }
        },
        AdapterMode::Mock => {
            tracing::info!("子流程路由走 mock（脱外部）");
            engine.set_subflow_router(Arc::new(MockSubflowRouter));
        }
    }

    // serviceTask delegate：内置 riskDelegate 始终注册（进程内，零外部）；另按配置注册
    // httpDelegate（外包外部 URL）或 mockDelegate（no-op）——BPMN 用哪个由节点 delegate 键决定。
    engine.register_delegate("riskDelegate", RiskDelegate);
    // E2E/测试用 delegate：仅当 `FLOW_ENABLE_E2E_DELEGATES=1`（或 true）时注册——生产二进制默认不含。
    // 用于真机验证 P1（异步）/A8·A3（抛 BPMN 错误）/P2（恒失败死信）的服务任务路径。
    if std::env::var("FLOW_ENABLE_E2E_DELEGATES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        engine.register_delegate("e2eOkDelegate", E2eOkDelegate); // 成功（配 async 验 P1）
        engine.register_delegate("e2eBpmnErr", E2eBpmnErrDelegate); // 抛 BPMN 错误 E_RISK（验 A8/A3）
        engine.register_delegate("e2eAlwaysFail", E2eAlwaysFailDelegate); // 恒失败（验 P2 死信）
        tracing::warn!("已注册 E2E 测试 delegate（FLOW_ENABLE_E2E_DELEGATES 开启）——生产环境勿开");
    }
    match cfg.delegate_mode {
        AdapterMode::Http => match &cfg.delegate_url {
            Some(url) => {
                tracing::info!(url, "serviceTask 外包外部 delegate(http)，键=httpDelegate");
                engine.register_delegate("httpDelegate", HttpDelegate::new(url.clone()));
            }
            None => tracing::warn!("FLOW_DELEGATE_MODE=http 但缺 FLOW_DELEGATE_URL，不注册 httpDelegate"),
        },
        AdapterMode::Mock => {
            engine.register_delegate("mockDelegate", MockDelegate);
        }
        // pg：无独立 delegate 概念，仅内置 riskDelegate（保持现状）。
        AdapterMode::Pg => {}
    }

    // 2b) 子流程绑定管理面（IAM 库）。生产库不由引擎 ensure_schema 覆盖，故此处兜底建表，
    //     补上历史缺口（原来绑定表只靠 demo 的 CREATE TABLE 种入，生产从未建）。
    let binding_store = PgSubflowBindingStore::new(iam_db_id);
    if let Err(e) = binding_store.ensure_schema().await {
        tracing::warn!(error = %e, "子流程绑定表建表失败（组织路由配置将不可用）");
    }

    // 2c) F1/F3 集成支撑表（该租户库）：单据↔实例关联 + 任务意见留痕。幂等自举，失败仅告警。
    //     build_for 在请求 scope 外运行，故用 tenant::scope 包一层，让 biz_link 里的
    //     current_flow_db_id() 解析到本租户库（否则回退默认租户库，multi 下会串库）。
    crate::tenant::scope(crate::tenant::TenantCtx::new(tenant), async {
        if let Err(e) = crate::biz_link::ensure_schema().await {
            tracing::warn!(error = %e, "F1/F3 集成表建表失败（单据关联/意见留痕将不可用）");
        }
        // 2d) F4 表单注册表种入内置示例绑定（幂等）。失败仅告警。
        if let Err(e) = crate::biz_link::seed_form_bindings().await {
            tracing::warn!(error = %e, "F4 表单绑定种子失败（待办打开表单将退回硬编码兜底）");
        }
    })
    .await;

    // 3) 装载库里已发布的定义（设计器产物）。编译失败项跳过不阻断整体启动。
    let mut definitions: Vec<ProcessDefinition> = Vec::new();
    match def_svc.load_published_definitions().await {
        Ok((loaded, errors)) => {
            for (k, e) in &errors {
                tracing::warn!(def = %k, error = %e, "已发布定义编译失败，跳过装载");
            }
            for def in loaded {
                tracing::info!(key = %def.key, "装载已发布流程定义");
                definitions.push(def.clone());
                if let Err(e) = engine.deploy(def) {
                    tracing::warn!(error = %e, "装载流程定义失败");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "读取已发布定义失败"),
    }

    // 3b) 决策表持久化：建表兜底 + 装载库里已落库的决策表回引擎注册表（businessRuleTask 依赖）。
    //     决策表是设计态产物，与定义同库同租户（flow 库）。失败仅告警，不阻断启动。
    let decision_store = PgDecisionStore::new(flow_db_id);
    if let Err(e) = decision_store.ensure_schema().await {
        tracing::warn!(error = %e, "决策表建表失败（决策表持久化不可用，仅内存热注册）");
    }
    match decision_store.load_all().await {
        Ok((tables, errors)) => {
            for (k, e) in &errors {
                tracing::warn!(decision = %k, error = %e, "决策表解析失败，跳过装载");
            }
            let n = tables.len();
            for t in tables {
                engine.register_decision(t);
            }
            if n > 0 {
                tracing::info!(count = n, "装载已落库决策表");
            }
        }
        Err(e) => tracing::warn!(error = %e, "读取已落库决策表失败"),
    }

    // 3c) 变量历史表兜底建表（flow 库；start/complete/set-variables 捕获变更）。失败仅告警。
    let var_history_store = PgVarHistoryStore::new(flow_db_id);
    if let Err(e) = var_history_store.ensure_schema().await {
        tracing::warn!(error = %e, "变量历史表建表失败（变量历史不可用）");
    }

    // 4) 出站 webhook 发送器：配了 FLOW_WEBHOOK_URLS 则起后台投递 worker，否则 disabled（emit no-op）。
    let webhook = if cfg.webhook.is_enabled() {
        tracing::info!(
            urls = cfg.webhook.urls.len(),
            signed = cfg.webhook.signing_key.is_some(),
            "出站 webhook 已启用（生命周期事件通知第三方）"
        );
        WebhookSender::spawn_worker(
            cfg.webhook.urls.clone(),
            cfg.webhook.signing_key.clone(),
            cfg.webhook.max_retries,
        )
    } else {
        WebhookSender::disabled()
    };

    Ok(FlowRuntime {
        engine: Arc::new(engine),
        definitions: Arc::new(RwLock::new(definitions)),
        def_svc: Arc::new(def_svc),
        binding_store: Arc::new(binding_store),
        decision_store: Arc::new(decision_store),
        var_history_store: Arc::new(var_history_store),
        dimension_specs: Arc::new(dim_regs),
        webhook,
        tenant: tenant.to_string(),
    })
}

/// 启动后台定时器 poller：每 5 秒推进一次**所有已建租户运行时**的到期边界定时器。
///
/// db-per-tenant：task_local 不跨 spawn，故 poller 不靠请求租户，而是遍历运行时缓存
/// （`RUNTIMES`）逐个 `trigger_due_timers`。宿主先调 [`flow`]/[`flow_for_tenant`] 让默认租户
/// 运行时就绪（fail-fast 暴露 DB/schema 问题），后续新租户首访时进缓存、poller 下一 tick 自动纳入。
pub async fn spawn_timer_poller() -> crate::resp::Result<()> {
    // fail-fast：先建默认租户运行时（暴露 DB/schema 问题；与单租户旧行为一致）。
    let _ = flow_for_tenant(DEFAULT_TENANT).await?;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            ticker.tick().await;
            // 快照当前所有**已建成**租户运行时（读锁短持有；引擎调用在锁外做）。
            // cell 未初始化（租户已登记但 build 尚未完成）的跳过。
            let rts: Vec<Arc<FlowRuntime>> = runtimes()
                .read()
                .await
                .values()
                .filter_map(|cell| cell.get().cloned())
                .collect();
            for rt in rts {
                match rt.engine.trigger_due_timers(100).await {
                    Ok(fired) if !fired.is_empty() => {
                        for f in &fired {
                            tracing::info!(
                                tenant = %rt.tenant,
                                instance = %f.instance_id,
                                boundary = %f.boundary_bpmn_id,
                                interrupting = f.cancel_activity,
                                "⏰ 流程定时器触发"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(tenant = %rt.tenant, error = %e, "流程定时器推进出错"),
                }
            }
        }
    });
    tracing::info!("✅ 流程引擎已就绪（多租户定时器 poller 已启动）");
    Ok(())
}

/// 启动后台异步 Job 执行器：每 3 秒抢占并执行一批**所有已建租户**的异步服务任务作业（P1）。
///
/// 与定时器 poller 同构：遍历运行时缓存逐个 `run_async_jobs`。集群安全靠 store 侧
/// `acquire_async_jobs` 的 SKIP LOCKED——多进程各自跑本 poller，拿到互不相交的作业集，
/// 不会重复执行同一作业。`worker_id` 进程内唯一（pid + 启动纳秒），作为锁持有者标识。
pub async fn spawn_async_job_poller() -> crate::resp::Result<()> {
    let _ = flow_for_tenant(DEFAULT_TENANT).await?;
    // 进程唯一 worker 标识：同一 pid 内两次启动（理论上不会）也因纳秒不同而区分。
    let worker_id = format!(
        "async-worker-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    tracing::info!("✅ 流程引擎异步 Job 执行器已启动（SKIP LOCKED 集群安全，worker={worker_id}）");
    tokio::spawn(async move {
        // 锁定时长 60s：单个 delegate 应远快于此；崩溃/超时后锁自然过期，作业可被重抢（至少一次语义）。
        const LOCK_SECS: i64 = 60;
        const BATCH: usize = 50;
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3));
        loop {
            ticker.tick().await;
            let rts: Vec<Arc<FlowRuntime>> = runtimes()
                .read()
                .await
                .values()
                .filter_map(|cell| cell.get().cloned())
                .collect();
            for rt in rts {
                match rt.engine.run_async_jobs(&worker_id, LOCK_SECS, BATCH).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(tenant = %rt.tenant, completed = n, "⚙️ 异步服务任务已执行")
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(tenant = %rt.tenant, error = %e, "异步 Job 执行出错")
                    }
                }
            }
        }
    });
    Ok(())
}

/// serviceTask delegate：按金额算风险等级写回变量（从 demo 移植）。
struct RiskDelegate;

#[async_trait]
impl JavaDelegate for RiskDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
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

/// 把任意错误消息桥成 FlowError（同抽核前 BizError 桥语义：业务错误）。
fn bridge(msg: String) -> crate::resp::FlowError {
    crate::resp::FlowError::business_error(msg)
}

// ———————— E2E/测试用 delegate（真机验证服务任务三条路径） ————————

/// 成功 delegate：写标记变量（配 flowable:async="true" 验 P1 异步执行器）。
struct E2eOkDelegate;
#[async_trait]
impl JavaDelegate for E2eOkDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        ctx.variables.set("e2eDelegateRan", json!(true));
        Ok(())
    }
}

/// 抛类型化 BPMN 错误 E_RISK 的 delegate（验 A8 错误边界 / A3 错误事件子流程）。
struct E2eBpmnErrDelegate;
#[async_trait]
impl JavaDelegate for E2eBpmnErrDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        Err(cmx_flow_engine::DelegateError::bpmn("E_RISK", "E2E 业务异常"))
    }
}

/// 恒失败 delegate（验 P2 死信队列：配 async 后重试耗尽转死信 + Incident）。
struct E2eAlwaysFailDelegate;
#[async_trait]
impl JavaDelegate for E2eAlwaysFailDelegate {
    async fn execute(&self, _ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        Err("E2E 外部服务持续不可达".into())
    }
}
