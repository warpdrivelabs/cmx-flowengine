/*
 * @Describe: F1/F3 集成支撑：单据↔实例关联 + 任务意见留痕 + 任务节点反查。
 *
 * 都是「流程引擎之外」的薄持久化：直接走 cmx-database-pg，落在 FLOW_DB_ID(fico-db) 库。
 * 约定对齐 cmx-flow-def 系列：cmx_ 前缀、无外键（软关联 + 索引）、行 id 用 uuid 字符串、
 * 时间戳可空列用 DataValue::DateTime、DDL 幂等（ensure_schema 自举 + 迁移双写）。
 *
 * 分三块：
 *   - biz_link：cmx_flow_biz_link，发起时把业务单据(表+主键)关联到实例；双向查询。
 *   - comment ：cmx_flow_task_comment，办结时按环节留痕审批意见；供表单审批区展示历史。
 *   - 反查    ：按 (instance_id, task_id) 取任务的 node_bpmn_id（写意见时补节点）。
 *
 * 数据读取约定：本文件全部是**控制面小行读**（单行→DTO、待办分页列表手工投影成
 * serde_json::Value），故走 query_sql→DataSet 的按名取值，符合约定。若未来新增
 * 「对外返回业务行数据集」的端点（大结果集导出），改用 ZmcDataSet + msgpack（对标
 * DOC/DCT/RPT 的 data.bin），详见 ../../README.md「数据读取约定」。
 */

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::engine::{FlowRuntime, current_flow_db_id};

/// 本文件所有 DB 读写的目标库 = 当前请求租户的运行态库（db-per-tenant，S2）。
/// 读 task_local 租户 → db_id；无租户回退默认租户库（单租户零回归）。build_for 建表时经
/// tenant::scope 包裹，故这里同样解析到该租户库。
#[inline]
fn db() -> String {
    current_flow_db_id()
}

/// 建表 DDL（幂等）。engine build 时调一次自举；生产走 docs/sql 迁移。
pub const DDL_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_biz_link (
        id           VARCHAR(64)  PRIMARY KEY,
        instance_id  VARCHAR(64)  NOT NULL,
        biz_table    VARCHAR(128) NOT NULL,
        biz_id       VARCHAR(128) NOT NULL,
        biz_key      VARCHAR(128),
        role         VARCHAR(32)  NOT NULL DEFAULT 'primary',
        created_at   TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_biz_link_instance ON cmx_flow_biz_link (instance_id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_biz_link_biz ON cmx_flow_biz_link (biz_table, biz_id, instance_id)",
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_task_comment (
        id           VARCHAR(64)  PRIMARY KEY,
        instance_id  VARCHAR(64)  NOT NULL,
        task_id      VARCHAR(64)  NOT NULL,
        node_bpmn_id VARCHAR(128),
        user_id      VARCHAR(64),
        decision     VARCHAR(32),
        comment      TEXT,
        created_at   TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_comment_instance ON cmx_flow_task_comment (instance_id)",
    // —— F4：表单注册表（formKey → 表单页坐标；接新表单从写代码降为配一行） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_form_binding (
        form_key     VARCHAR(128) PRIMARY KEY,
        kind         VARCHAR(16)  NOT NULL DEFAULT 'native',
        native_page  VARCHAR(128),
        native_view  VARCHAR(64),
        html_page    VARCHAR(128),
        biz_table    VARCHAR(128),
        domain       VARCHAR(32),
        application  VARCHAR(32),
        module       VARCHAR(32),
        file         VARCHAR(128),
        pk_field     VARCHAR(64),
        title        VARCHAR(255),
        updated_at   TIMESTAMPTZ  NOT NULL
    )"#,
    // 已有表补列（幂等）。
    "ALTER TABLE cmx_flow_form_binding ADD COLUMN IF NOT EXISTS native_view VARCHAR(64)",
    "ALTER TABLE cmx_flow_form_binding ADD COLUMN IF NOT EXISTS file VARCHAR(128)",
    "ALTER TABLE cmx_flow_form_binding ADD COLUMN IF NOT EXISTS pk_field VARCHAR(64)",
    // kind='workspace' 时指向门户工作区节点库（data/node/nodes.json）的一个完整 workspace node id。
    "ALTER TABLE cmx_flow_form_binding ADD COLUMN IF NOT EXISTS workspace_node VARCHAR(128)",
    // property 区审批控制台归属：'platform'（默认，挂 task-form 通用控制台）/ 'none'
    // （表单自带审批操作，待办中心不再挂控制台——业务封装审批动作的模块用，如 MDM M7.1）。
    "ALTER TABLE cmx_flow_form_binding ADD COLUMN IF NOT EXISTS console VARCHAR(32) NOT NULL DEFAULT 'platform'",
];

/// 自举建表（engine build 后调一次）。失败仅告警，不阻断启动。
pub async fn ensure_schema() -> Result<(), String> {
    for stmt in DDL_STATEMENTS {
        execute_sql(&db(), None, stmt)
            .await
            .map_err(|e| format!("建表失败: {e}"))?;
    }
    Ok(())
}

// ————————————————————— 单据↔实例关联 —————————————————————

/// 发起时把业务单据关联到实例（幂等：同 (biz_table, biz_id, instance_id) 不重复插）。
pub async fn link_biz_to_instance(
    _rt: &FlowRuntime,
    instance_id: &str,
    biz_table: &str,
    biz_id: &str,
    biz_key: Option<String>,
    role: Option<String>,
) -> Result<(), String> {
    let sql = "INSERT INTO cmx_flow_biz_link \
        (id, instance_id, biz_table, biz_id, biz_key, role, created_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7) \
        ON CONFLICT (biz_table, biz_id, instance_id) DO NOTHING";
    let params = SqlParams::DataValues(vec![
        DataValue::String(Uuid::new_v4().to_string()),
        DataValue::String(instance_id.to_string()),
        DataValue::String(biz_table.to_string()),
        DataValue::String(biz_id.to_string()),
        opt_str(biz_key),
        DataValue::String(role.unwrap_or_else(|| "primary".to_string())),
        DataValue::DateTime(Utc::now()),
    ]);
    execute_sql_with_params(&db(), None, sql, params)
        .await
        .map_err(|e| format!("写关联失败: {e}"))?;
    Ok(())
}

/// 正向：实例 → 绑的单据列表。
pub async fn biz_of_instance(instance_id: &str) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT biz_table, biz_id, biz_key, role FROM cmx_flow_biz_link \
         WHERE instance_id = '{}' ORDER BY created_at",
        esc(instance_id)
    );
    let ds = query_sql(&db(), None, &sql, "flow_biz_of_inst")
        .await
        .map_err(|e| format!("查关联失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(json!({
            "bizTable": get_str(row, schema, "biz_table"),
            "bizId": get_str(row, schema, "biz_id"),
            "bizKey": get_opt(row, schema, "biz_key"),
            "role": get_str(row, schema, "role"),
        }));
    }
    Ok(out)
}

/// 反向：单据 → 关联的实例列表（含实例状态，供业务列表页显示「审批中」）。
pub async fn instances_of_biz(biz_table: &str, biz_id: &str) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT l.instance_id AS instance_id, i.definition_key AS definition_key, \
                i.state AS state, i.business_key AS business_key, l.role AS role \
         FROM cmx_flow_biz_link l \
         JOIN cmx_flow_instance i ON i.id = l.instance_id \
         WHERE l.biz_table = '{}' AND l.biz_id = '{}' \
         ORDER BY l.created_at DESC",
        esc(biz_table),
        esc(biz_id)
    );
    let ds = query_sql(&db(), None, &sql, "flow_inst_of_biz")
        .await
        .map_err(|e| format!("反查关联失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(json!({
            "instanceId": get_str(row, schema, "instance_id"),
            "definitionKey": get_str(row, schema, "definition_key"),
            "state": get_str(row, schema, "state"),
            "businessKey": get_opt(row, schema, "business_key"),
            "role": get_str(row, schema, "role"),
        }));
    }
    Ok(out)
}

// ————————————————————— 任务意见留痕 —————————————————————

/// 反查任务的 node_bpmn_id（写意见时补节点信息）。找不到返回 None。
pub async fn task_node_bpmn_id(
    _rt: &FlowRuntime,
    instance_id: &str,
    task_id: &str,
) -> Option<String> {
    let sql = format!(
        "SELECT node_bpmn_id FROM cmx_flow_task WHERE id = '{}' AND instance_id = '{}'",
        esc(task_id),
        esc(instance_id)
    );
    let ds = query_sql(&db(), None, &sql, "flow_task_node").await.ok()?;
    let schema = ds.schema.as_ref();
    ds.iter()
        .next()
        .and_then(|row| get_opt(row, schema, "node_bpmn_id"))
}

/// 任务的当前办理人（assignee）。任务不存在或候选池未认领（assignee NULL）→ None。
/// T0b complete 授权校验用。
pub async fn task_assignee(
    _rt: &FlowRuntime,
    instance_id: &str,
    task_id: &str,
) -> Option<String> {
    let sql = format!(
        "SELECT assignee FROM cmx_flow_task WHERE id = '{}' AND instance_id = '{}'",
        esc(task_id),
        esc(instance_id)
    );
    let ds = query_sql(&db(), None, &sql, "flow_task_assignee").await.ok()?;
    let schema = ds.schema.as_ref();
    ds.iter().next().and_then(|row| get_opt(row, schema, "assignee"))
}

/// 用户是否为任务候选（发起时物化的 resolved_user_id）。T0b complete 授权校验用。
pub async fn task_has_candidate(
    _rt: &FlowRuntime,
    task_id: &str,
    user: &str,
) -> bool {
    let sql = format!(
        "SELECT 1 AS hit FROM cmx_flow_task_candidate \
         WHERE task_id = '{}' AND resolved_user_id = '{}' LIMIT 1",
        esc(task_id),
        esc(user)
    );
    query_sql(&db(), None, &sql, "flow_task_cand_hit")
        .await
        .map(|ds| ds.row_count() > 0)
        .unwrap_or(false)
}

/// 办结时插一行意见留痕。
pub async fn insert_task_comment(
    _rt: &FlowRuntime,
    instance_id: &str,
    task_id: &str,
    node_bpmn_id: &str,
    operator: Option<String>,
    decision: Option<String>,
    comment: Option<String>,
) -> Result<(), String> {
    let sql = "INSERT INTO cmx_flow_task_comment \
        (id, instance_id, task_id, node_bpmn_id, user_id, decision, comment, created_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";
    let params = SqlParams::DataValues(vec![
        DataValue::String(Uuid::new_v4().to_string()),
        DataValue::String(instance_id.to_string()),
        DataValue::String(task_id.to_string()),
        opt_str(if node_bpmn_id.is_empty() {
            None
        } else {
            Some(node_bpmn_id.to_string())
        }),
        opt_str(operator.filter(|s| !s.trim().is_empty())), // user_id：办理人（谁办结/审批的）
        opt_str(decision),
        opt_str(comment),
        DataValue::DateTime(Utc::now()),
    ]);
    execute_sql_with_params(&db(), None, sql, params)
        .await
        .map_err(|e| format!("写意见失败: {e}"))?;
    Ok(())
}

/// 列某实例的全部审批意见（按时间正序，供表单审批区展示历史）。
pub async fn comments_of_instance(instance_id: &str) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT task_id, node_bpmn_id, user_id, decision, comment, created_at \
         FROM cmx_flow_task_comment WHERE instance_id = '{}' ORDER BY created_at",
        esc(instance_id)
    );
    let ds = query_sql(&db(), None, &sql, "flow_comments")
        .await
        .map_err(|e| format!("查意见失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(json!({
            "taskId": get_str(row, schema, "task_id"),
            "nodeBpmnId": get_opt(row, schema, "node_bpmn_id"),
            "userId": get_opt(row, schema, "user_id"),
            "decision": get_opt(row, schema, "decision"),
            "comment": get_opt(row, schema, "comment"),
            "createdAt": get_ts_rfc3339(row, schema, "created_at"),
        }));
    }
    Ok(out)
}

// ————————————————————— F2：我的待办（跨实例） —————————————————————

/// 列表查找/过滤/分页参数（六列表共用）。
#[derive(Default, Clone)]
pub struct TodoFilter {
    /// 关键字（模糊匹配 单号/流程名，视列表也含申请人）。
    pub keyword: Option<String>,
    /// 按流程定义 key 过滤。
    pub definition_key: Option<String>,
    /// 按节点 bpmn_id 过滤（仅任务类列表）。
    pub node_bpmn_id: Option<String>,
    /// 按实例状态过滤（ACTIVE/COMPLETED/TERMINATED，仅实例类列表）。
    pub state: Option<String>,
    /// 按发起人过滤（仅「我发起的」实例列表用；匹配实例变量 `initiator`）。空 = 不过滤。
    pub initiator: Option<String>,
    /// 页码（1 起）。
    pub page: i64,
    /// 每页条数。
    pub page_size: i64,
}

impl TodoFilter {
    /// 归一：页码≥1、每页 [1,200] 默认 20。
    pub fn norm(&self) -> (i64, i64) {
        let p = self.page.max(1);
        let s = self.page_size.clamp(1, 200);
        (p, s)
    }
    fn like(&self) -> Option<String> {
        self.keyword.as_ref().and_then(|k| {
            let k = k.trim();
            if k.is_empty() {
                None
            } else {
                Some(format!("%{}%", esc(&k.to_lowercase())))
            }
        })
    }
}

/// 一页待办结果（含总数，供前端算总页数）。
pub struct TodoPage {
    pub rows: Vec<RawTodo>,
    pub total: i64,
}

/// 一条原始待办行（DB 查出的裸数据；formKey 由 handler 反查定义补齐）。
pub struct RawTodo {
    pub task_id: String,
    pub instance_id: String,
    pub node_bpmn_id: String,
    pub name: Option<String>,
    pub definition_key: String,
    pub business_key: Option<String>,
    /// 实例变量 JSON 字符串（jsonb 读回）。
    pub variables_json: Option<String>,
    pub created_at: Option<String>,
    /// true = 候选未认领（需先 claim），false = 直派给我。
    pub claimable: bool,
    /// 实例列表用：当前活动节点 bpmn id（node_bpmn_id 位被状态占用，故单列）。cc/done 留空。
    pub current_node: Option<String>,
    /// 多实例子任务携带的当前元素（jsonb 字符串；②：办理人看「审的是哪个产品」）。单实例为 None。
    pub element_value: Option<String>,
}

/// 直派待办：走 idx_cmx_flow_task_open (assignee, completed)，只活跃实例。
pub async fn open_tasks_by_assignee(user_id: &str, f: &TodoFilter) -> Result<TodoPage, String> {
    let mut cond = format!(
        "t.assignee = '{}' AND t.completed = false AND i.state = 'ACTIVE'",
        esc(user_id)
    );
    append_task_conds(&mut cond, f);
    let from = "FROM cmx_flow_task t JOIN cmx_flow_instance i ON i.id = t.instance_id";
    paged_todos(from, &cond, "t.created_at DESC", f, "flow_my_open", false).await
}

/// 候选未认领：走 idx…_candidate_user (resolved_user_id)，任务未被 claim（assignee IS NULL）。
pub async fn claimable_tasks_by_user(user_id: &str, f: &TodoFilter) -> Result<TodoPage, String> {
    let mut cond = format!(
        "c.resolved_user_id = '{}' AND t.assignee IS NULL",
        esc(user_id)
    );
    append_task_conds(&mut cond, f);
    let from = "FROM cmx_flow_task_candidate c \
         JOIN cmx_flow_task t ON t.id = c.task_id AND t.completed = false \
         JOIN cmx_flow_instance i ON i.id = t.instance_id AND i.state = 'ACTIVE'";
    paged_todos(from, &cond, "t.created_at DESC", f, "flow_my_claimable", true).await
}

/// 追加任务类列表的过滤条件（关键字/流程/节点；状态对任务类固定 ACTIVE 不再叠加）。
fn append_task_conds(cond: &mut String, f: &TodoFilter) {
    if let Some(dk) = f.definition_key.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND i.definition_key = '{}'", esc(dk)));
    }
    if let Some(nb) = f.node_bpmn_id.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND t.node_bpmn_id = '{}'", esc(nb)));
    }
    if let Some(like) = f.like() {
        // 关键字：单号 / 流程名(key) / 节点名 模糊匹配（大小写不敏感）。
        cond.push_str(&format!(
            " AND (LOWER(COALESCE(i.business_key,'')) LIKE '{like}' \
                OR LOWER(i.definition_key) LIKE '{like}' \
                OR LOWER(COALESCE(t.name,'')) LIKE '{like}')"
        ));
    }
}

/// 任务类分页查询：先 COUNT 再取一页（共用 SELECT 列）。
async fn paged_todos(
    from: &str,
    cond: &str,
    order: &str,
    f: &TodoFilter,
    tag: &str,
    claimable: bool,
) -> Result<TodoPage, String> {
    let (page, size) = f.norm();
    let offset = (page - 1) * size;
    let count_sql = format!("SELECT COUNT(*) AS n {from} WHERE {cond}");
    let total = query_one_i64(&count_sql, tag).await?;
    let sql = format!(
        "SELECT t.id AS task_id, t.instance_id AS instance_id, t.node_bpmn_id AS node_bpmn_id, \
                t.name AS name, t.created_at AS created_at, t.element_value AS element_value, \
                i.definition_key AS definition_key, i.business_key AS business_key, \
                i.variables AS variables \
         {from} WHERE {cond} ORDER BY {order} LIMIT {size} OFFSET {offset}"
    );
    let rows = rows_to_todos(&sql, tag, claimable).await?;
    Ok(TodoPage { rows, total })
}

async fn query_one_i64(sql: &str, tag: &str) -> Result<i64, String> {
    let ds = query_sql(&db(), None, sql, tag)
        .await
        .map_err(|e| format!("计数失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .next()
        .map(|row| match row.get_by_name(schema, "n") {
            Some(DataValue::Int(v)) => *v,
            _ => 0,
        })
        .unwrap_or(0))
}

async fn rows_to_todos(sql: &str, tag: &str, claimable: bool) -> Result<Vec<RawTodo>, String> {
    let ds = query_sql(&db(), None, sql, tag)
        .await
        .map_err(|e| format!("查我的待办失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(RawTodo {
            task_id: get_str(row, schema, "task_id"),
            instance_id: get_str(row, schema, "instance_id"),
            node_bpmn_id: get_str(row, schema, "node_bpmn_id"),
            name: get_opt(row, schema, "name"),
            definition_key: get_str(row, schema, "definition_key"),
            business_key: get_opt(row, schema, "business_key"),
            variables_json: get_opt(row, schema, "variables"),
            created_at: get_ts_rfc3339(row, schema, "created_at"),
            claimable,
            current_node: get_opt(row, schema, "node_bpmn_id"),
            element_value: get_opt(row, schema, "element_value"),
        });
    }
    Ok(out)
}

// ————————————————————— 我发起的 / 抄送 / 已办（分页过滤） —————————————————————

/// 我发起的：实例列表（暂以全部实例为源；发起人字段未落库，前端仍展示 applicant 变量）。
/// 过滤：关键字(单号/流程名)、流程、状态；分页 LIMIT/OFFSET。
pub async fn list_instances_paged(f: &TodoFilter) -> Result<TodoPage, String> {
    let (page, size) = f.norm();
    let offset = (page - 1) * size;
    let mut cond = "1=1".to_string();
    if let Some(dk) = f.definition_key.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND definition_key = '{}'", esc(dk)));
    }
    if let Some(st) = f.state.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND state = '{}'", esc(st)));
    }
    // 「我发起的」按发起人过滤：发起人按约定存实例变量 `initiator`（与 withdraw 护栏同源）。
    if let Some(init) = f.initiator.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(
            " AND (variables ->> 'initiator') = '{}'",
            esc(init)
        ));
    }
    if let Some(like) = f.like() {
        cond.push_str(&format!(
            " AND (LOWER(COALESCE(business_key,'')) LIKE '{like}' OR LOWER(definition_key) LIKE '{like}')"
        ));
    }
    let total = query_one_i64(
        &format!("SELECT COUNT(*) AS n FROM cmx_flow_instance WHERE {cond}"),
        "flow_inst_count",
    )
    .await?;
    let sql = format!(
        "SELECT i.id AS instance_id, i.definition_key, i.business_key, i.state, i.variables, i.created_at, \
         (SELECT tk.node_bpmn_id FROM cmx_flow_token tk \
            WHERE tk.instance_id = i.id AND tk.state <> 'ENDED' \
            ORDER BY tk.created_at DESC LIMIT 1) AS current_node \
         FROM cmx_flow_instance i WHERE {cond} ORDER BY i.created_at DESC LIMIT {size} OFFSET {offset}"
    );
    let ds = query_sql(&db(), None, &sql, "flow_inst_paged")
        .await
        .map_err(|e| format!("查实例失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let rows = ds
        .iter()
        .map(|row| RawTodo {
            task_id: format!("inst-{}", get_str(row, schema, "instance_id")),
            instance_id: get_str(row, schema, "instance_id"),
            node_bpmn_id: get_str(row, schema, "state"), // 复用 node 位展示状态（前端 instToTodo 会转中文）
            name: get_opt(row, schema, "state"),
            definition_key: get_str(row, schema, "definition_key"),
            business_key: get_opt(row, schema, "business_key"),
            variables_json: get_opt(row, schema, "variables"),
            created_at: get_ts_rfc3339(row, schema, "created_at"),
            claimable: false,
            current_node: get_opt(row, schema, "current_node"),
            element_value: None,
        })
        .collect();
    Ok(TodoPage { rows, total })
}

/// 已办：历史任务 hi_task（按 assignee）。过滤：关键字(流程名/节点名)、流程、节点。
pub async fn list_done_paged(user_id: &str, f: &TodoFilter) -> Result<TodoPage, String> {
    let (page, size) = f.norm();
    let offset = (page - 1) * size;
    let mut cond = format!("h.assignee = '{}'", esc(user_id));
    if let Some(dk) = f.definition_key.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND i.definition_key = '{}'", esc(dk)));
    }
    if let Some(nb) = f.node_bpmn_id.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND h.node_bpmn_id = '{}'", esc(nb)));
    }
    if let Some(like) = f.like() {
        cond.push_str(&format!(
            " AND (LOWER(COALESCE(i.business_key,'')) LIKE '{like}' \
                OR LOWER(i.definition_key) LIKE '{like}' OR LOWER(COALESCE(h.name,'')) LIKE '{like}')"
        ));
    }
    let from = "FROM cmx_flow_hi_task h JOIN cmx_flow_instance i ON i.id = h.instance_id";
    let total = query_one_i64(
        &format!("SELECT COUNT(*) AS n {from} WHERE {cond}"),
        "flow_done_count",
    )
    .await?;
    let sql = format!(
        "SELECT h.id AS task_id, h.instance_id AS instance_id, h.node_bpmn_id AS node_bpmn_id, \
                h.name AS name, h.completed_at AS created_at, \
                i.definition_key AS definition_key, i.business_key AS business_key, i.variables AS variables \
         {from} WHERE {cond} ORDER BY h.completed_at DESC LIMIT {size} OFFSET {offset}"
    );
    let rows = rows_to_todos(&sql, "flow_done_paged", false).await?;
    Ok(TodoPage { rows, total })
}

/// 抄送我的：cc 表（按 to_user）。过滤：关键字(流程名)、流程；分页。
pub async fn list_cc_paged(user_id: &str, f: &TodoFilter) -> Result<TodoPage, String> {
    let (page, size) = f.norm();
    let offset = (page - 1) * size;
    let mut cond = format!("cc.to_user_id = '{}'", esc(user_id));
    if let Some(dk) = f.definition_key.as_ref().filter(|s| !s.trim().is_empty()) {
        cond.push_str(&format!(" AND i.definition_key = '{}'", esc(dk)));
    }
    if let Some(like) = f.like() {
        cond.push_str(&format!(
            " AND (LOWER(COALESCE(i.business_key,'')) LIKE '{like}' OR LOWER(i.definition_key) LIKE '{like}')"
        ));
    }
    let from = "FROM cmx_flow_cc cc JOIN cmx_flow_instance i ON i.id = cc.instance_id";
    let total = query_one_i64(
        &format!("SELECT COUNT(*) AS n {from} WHERE {cond}"),
        "flow_cc_count",
    )
    .await?;
    let sql = format!(
        "SELECT cc.id AS cc_id, cc.instance_id AS instance_id, cc.node_bpmn_id AS node_bpmn_id, \
                cc.read_at AS read_at, cc.created_at AS created_at, \
                i.definition_key AS definition_key, i.business_key AS business_key, i.variables AS variables \
         {from} WHERE {cond} ORDER BY cc.created_at DESC LIMIT {size} OFFSET {offset}"
    );
    let ds = query_sql(&db(), None, &sql, "flow_cc_paged")
        .await
        .map_err(|e| format!("查抄送失败: {e}"))?;
    let schema = ds.schema.as_ref();
    let rows = ds
        .iter()
        .map(|row| RawTodo {
            task_id: format!("cc-{}", get_str(row, schema, "cc_id")),
            instance_id: get_str(row, schema, "instance_id"),
            node_bpmn_id: get_str(row, schema, "node_bpmn_id"),
            name: get_opt(row, schema, "node_bpmn_id"),
            definition_key: get_str(row, schema, "definition_key"),
            business_key: get_opt(row, schema, "business_key"),
            variables_json: get_opt(row, schema, "variables"),
            created_at: get_ts_rfc3339(row, schema, "created_at"),
            claimable: false,
            current_node: get_opt(row, schema, "node_bpmn_id"),
            element_value: None,
        })
        .collect();
    Ok(TodoPage { rows, total })
}

/// 过滤下拉选项源：已装载定义（key+name）；节点由前端按选中定义从定义详情取。
/// 这里只返回定义列表用于「按流程」下拉。
pub async fn distinct_definition_keys() -> Result<Vec<(String, Option<String>)>, String> {
    let sql = "SELECT DISTINCT definition_key FROM cmx_flow_instance ORDER BY definition_key";
    let ds = query_sql(&db(), None, sql, "flow_distinct_def")
        .await
        .map_err(|e| format!("查流程列表失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds
        .iter()
        .map(|row| (get_str(row, schema, "definition_key"), None))
        .collect())
}

// ————————————————————— F4：表单注册表 —————————————————————

/// 一条表单绑定入参（避免长参列表）。
#[derive(Default)]
pub struct FormBinding {
    pub form_key: String,
    pub kind: String,
    pub native_page: Option<String>,
    pub native_view: Option<String>,
    pub html_page: Option<String>,
    pub biz_table: Option<String>,
    pub domain: Option<String>,
    pub application: Option<String>,
    pub module: Option<String>,
    pub file: Option<String>,
    pub pk_field: Option<String>,
    pub title: Option<String>,
    /// kind='workspace' 时指向门户工作区节点库的完整 workspace node id。
    pub workspace_node: Option<String>,
    /// property 区审批控制台归属：platform（默认）/ none（表单自带审批操作）。
    pub console: Option<String>,
}

/// upsert 一条表单绑定（form_key 主键）。
pub async fn upsert_form_binding(b: FormBinding) -> Result<(), String> {
    let sql = "INSERT INTO cmx_flow_form_binding \
        (form_key, kind, native_page, native_view, html_page, biz_table, domain, application, module, file, pk_field, title, workspace_node, console, updated_at) \
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
        ON CONFLICT (form_key) DO UPDATE SET \
          kind=EXCLUDED.kind, native_page=EXCLUDED.native_page, native_view=EXCLUDED.native_view, \
          html_page=EXCLUDED.html_page, biz_table=EXCLUDED.biz_table, domain=EXCLUDED.domain, \
          application=EXCLUDED.application, module=EXCLUDED.module, file=EXCLUDED.file, \
          pk_field=EXCLUDED.pk_field, title=EXCLUDED.title, workspace_node=EXCLUDED.workspace_node, \
          console=EXCLUDED.console, updated_at=EXCLUDED.updated_at";
    let params = SqlParams::DataValues(vec![
        DataValue::String(b.form_key),
        DataValue::String(b.kind),
        opt_str(b.native_page),
        opt_str(b.native_view),
        opt_str(b.html_page),
        opt_str(b.biz_table),
        opt_str(b.domain),
        opt_str(b.application),
        opt_str(b.module),
        opt_str(b.file),
        opt_str(b.pk_field),
        opt_str(b.title),
        opt_str(b.workspace_node),
        // 缺省 platform（与列默认一致；显式传入以便覆盖旧值）。
        DataValue::String(b.console.unwrap_or_else(|| "platform".to_string())),
        DataValue::DateTime(Utc::now()),
    ]);
    execute_sql_with_params(&db(), None, sql, params)
        .await
        .map_err(|e| format!("写表单绑定失败: {e}"))?;
    Ok(())
}

fn form_binding_json(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
) -> Value {
    let form_key = get_str(row, schema, "form_key");
    // 内置条目标记：seed 每次引擎构建幂等重写这 3 条（删除复活、编辑复位），管理页据此打角标提示。
    let seeded = SEEDED_FORM_KEYS.contains(&form_key.as_str());
    json!({
        "formKey": form_key,
        "kind": get_str(row, schema, "kind"),
        "console": get_opt(row, schema, "console").unwrap_or_else(|| "platform".to_string()),
        "nativePage": get_opt(row, schema, "native_page"),
        "nativeView": get_opt(row, schema, "native_view"),
        "htmlPage": get_opt(row, schema, "html_page"),
        "bizTable": get_opt(row, schema, "biz_table"),
        "domain": get_opt(row, schema, "domain"),
        "application": get_opt(row, schema, "application"),
        "module": get_opt(row, schema, "module"),
        "file": get_opt(row, schema, "file"),
        "pkField": get_opt(row, schema, "pk_field"),
        "title": get_opt(row, schema, "title"),
        "workspaceNode": get_opt(row, schema, "workspace_node"),
        "seeded": seeded,
    })
}

const FORM_BINDING_COLS: &str = "form_key, kind, native_page, native_view, html_page, biz_table, domain, application, module, file, pk_field, title, workspace_node, console";

/// 取一条表单绑定；不存在返回 None。
pub async fn get_form_binding(form_key: &str) -> Result<Option<Value>, String> {
    let sql = format!(
        "SELECT {FORM_BINDING_COLS} FROM cmx_flow_form_binding WHERE form_key = '{}'",
        esc(form_key)
    );
    let ds = query_sql(&db(), None, &sql, "flow_form_get")
        .await
        .map_err(|e| format!("查表单绑定失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds.iter().next().map(|row| form_binding_json(row, schema)))
}

/// 列全部表单绑定。
pub async fn list_form_bindings() -> Result<Vec<Value>, String> {
    let sql = format!("SELECT {FORM_BINDING_COLS} FROM cmx_flow_form_binding ORDER BY form_key");
    let ds = query_sql(&db(), None, &sql, "flow_form_list")
        .await
        .map_err(|e| format!("查表单绑定列表失败: {e}"))?;
    let schema = ds.schema.as_ref();
    Ok(ds.iter().map(|row| form_binding_json(row, schema)).collect())
}

/// 内置示例绑定的 form_key 清单（与 seed_form_bindings 同步维护）。
/// 管理页「内置」角标与复位提示用；seed 每次引擎构建幂等重写这些行。
pub const SEEDED_FORM_KEYS: &[&str] = &["pay.review", "pay.review.html", "expense.form"];

/// 删一条表单绑定（管理页删除按钮）。
///
/// 返回受影响行数：0 = 本就不存在（幂等语义，调用方据此区分「真删了」与「本来就没有」）。
pub async fn delete_form_binding(form_key: &str) -> Result<u64, String> {
    let sql = "DELETE FROM cmx_flow_form_binding WHERE form_key = $1";
    let params = SqlParams::DataValues(vec![DataValue::String(form_key.to_string())]);
    execute_sql_with_params(&db(), None, sql, params)
        .await
        .map_err(|e| format!("删表单绑定失败: {e}"))
}

/// 种入内置示例绑定（幂等 upsert）。engine build 时调一次。
///
/// pay.review = 真实 html 单据表单示例（复用 F3 的 flow-pay-review-form html 页）；
/// pay.review.doc = doc-loader 真单据示例（会计凭证坐标，供 F5 验证）。task-form 壳只作兜底，
/// 不再作为默认绑定——节点应绑真实表单页。
pub async fn seed_form_bindings() -> Result<(), String> {
    upsert_form_binding(FormBinding {
        form_key: "pay.review".into(),
        kind: "html".into(),
        html_page: Some("flow-pay-review-form".into()),
        biz_table: Some("cf_pay_request".into()),
        domain: Some("fi".into()),
        application: Some("cmxfico".into()),
        module: Some("gl".into()),
        title: Some("请款单复核（HTML 表单）".into()),
        ..Default::default()
    })
    .await?;
    upsert_form_binding(FormBinding {
        form_key: "pay.review.html".into(),
        kind: "html".into(),
        html_page: Some("flow-pay-review-form".into()),
        biz_table: Some("cf_pay_request".into()),
        domain: Some("fi".into()),
        application: Some("cmxfico".into()),
        module: Some("gl".into()),
        title: Some("请款单复核（HTML 表单）".into()),
        ..Default::default()
    })
    .await?;
    // 完整测试流程（test_expense）的表单：kind='workspace' → 门户工作区节点 flow-form-expense
    // （explorer+content+property 三区都有；见 data/node/nodes.json）。办理/查看打开完整工作台，
    // property 区叠加审批/只读任务视图。所有 userTask 都绑此 formKey。
    upsert_form_binding(FormBinding {
        form_key: "expense.form".into(),
        kind: "workspace".into(),
        workspace_node: Some("flow-form-expense".into()),
        biz_table: Some("cf_expense".into()),
        domain: Some("fi".into()),
        application: Some("cmxfico".into()),
        module: Some("gl".into()),
        title: Some("差旅报销工作台".into()),
        ..Default::default()
    })
    .await?;
    Ok(())
}

// ————————————————————— helper —————————————————————

fn opt_str(v: Option<String>) -> DataValue {
    match v {
        Some(s) => DataValue::String(s),
        None => DataValue::Null,
    }
}

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

fn get_str(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> String {
    get_opt(row, schema, col).unwrap_or_default()
}

fn get_opt(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        // variables jsonb 列读回是 Json(String)——原样取字符串（handler 再 from_str 解析）。
        Some(DataValue::Json(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_ts_rfc3339(
    row: &cmx_core::model::data::dataset::Row,
    schema: &cmx_core::model::data::dataset::Schema,
    col: &str,
) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::DateTime(dt)) => Some(dt.to_rfc3339()),
        _ => None,
    }
}
