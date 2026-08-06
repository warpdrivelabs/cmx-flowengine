/*
 * @Describe: 运行态 DTO ↔ SQL 行/参数 的映射。
 *
 * 写路径：DTO → (sql, SqlParams::DataValues)，参数化绑定。可空 TIMESTAMPTZ 列的 None 值
 * 用 DataValue::NullTyped(Timestamp)（tokio-postgres 严格类型，裸 Null 绑 TIMESTAMPTZ 会
 * WrongType）；其余可空文本列可用裸 Null。
 * 读路径：DataSet 首行 → DTO，按列名取值，jsonb 列以 DataValue::Json(String) 回来。
 */

use chrono::{DateTime, Utc};
use cmx_core::model::cell::{DataValue, SqlTypeMarker};
use cmx_core::model::data::dataset::DataSet;
use cmx_database_pg::SqlParams;
use cmx_flow_model::{
    CandidateKind, CcRecord, CcSummary, DueJob, InstanceState, InstanceSummary, MiScope,
    ProcessInstance, StoreError, StoreResult, Task, TaskCandidate, TaskDelegation, TimerJob, Token,
    TokenState, Variables,
};
use serde_json::Value as JsonValue;

// ============================ 状态 <-> 文本 ============================

/// InstanceState → 存储文本。
pub fn instance_state_str(s: InstanceState) -> &'static str {
    match s {
        InstanceState::Active => "ACTIVE",
        InstanceState::Completed => "COMPLETED",
        InstanceState::Terminated => "TERMINATED",
    }
}

/// 存储文本 → InstanceState。
fn parse_instance_state(s: &str) -> StoreResult<InstanceState> {
    match s {
        "ACTIVE" => Ok(InstanceState::Active),
        "COMPLETED" => Ok(InstanceState::Completed),
        "TERMINATED" => Ok(InstanceState::Terminated),
        other => Err(StoreError::Backend(format!("未知实例状态: {other}"))),
    }
}

/// TokenState → 存储文本。
pub fn token_state_str(s: TokenState) -> &'static str {
    match s {
        TokenState::Active => "ACTIVE",
        TokenState::Waiting => "WAITING",
        TokenState::Joining => "JOINING",
        TokenState::WaitingSubflow => "WAITING_SUBFLOW",
        TokenState::Ended => "ENDED",
    }
}

/// 存储文本 → TokenState。
fn parse_token_state(s: &str) -> StoreResult<TokenState> {
    match s {
        "ACTIVE" => Ok(TokenState::Active),
        "WAITING" => Ok(TokenState::Waiting),
        "JOINING" => Ok(TokenState::Joining),
        "WAITING_SUBFLOW" => Ok(TokenState::WaitingSubflow),
        "ENDED" => Ok(TokenState::Ended),
        other => Err(StoreError::Backend(format!("未知令牌状态: {other}"))),
    }
}

/// CandidateKind → 存储文本（M4.1）。
fn candidate_kind_str(k: CandidateKind) -> &'static str {
    match k {
        CandidateKind::User => "USER",
        CandidateKind::Role => "ROLE",
        CandidateKind::Position => "POSITION",
        CandidateKind::Org => "ORG",
    }
}

/// 存储文本 → CandidateKind。
fn parse_candidate_kind(s: &str) -> StoreResult<CandidateKind> {
    match s {
        "USER" => Ok(CandidateKind::User),
        "ROLE" => Ok(CandidateKind::Role),
        "POSITION" => Ok(CandidateKind::Position),
        "ORG" => Ok(CandidateKind::Org),
        other => Err(StoreError::Backend(format!("未知候选类型: {other}"))),
    }
}

// ============================ 可空值助手 ============================

/// Option<String> → DataValue（可空文本列，None 用裸 Null 即可）。
fn opt_text(v: &Option<String>) -> DataValue {
    match v {
        Some(s) => DataValue::String(s.clone()),
        None => DataValue::Null,
    }
}

/// Option<DateTime<Utc>> → DataValue（可空 TIMESTAMPTZ 列，None 必须带类型标记）。
fn opt_ts(v: &Option<DateTime<Utc>>) -> DataValue {
    match v {
        Some(t) => DataValue::DateTime(*t),
        None => DataValue::NullTyped(SqlTypeMarker::Timestamp),
    }
}

/// Option<serde_json::Value> → DataValue（可空 jsonb 列，None 带 Json 类型标记）。
fn opt_json(v: &Option<JsonValue>) -> DataValue {
    match v {
        Some(j) => DataValue::Json(j.to_string()),
        None => DataValue::NullTyped(SqlTypeMarker::Json),
    }
}

// ============================ 写：INSERT/UPDATE ============================

/// 实例 INSERT。variables 落 jsonb（DataValue::Json 承载 JSON 字符串）。
pub fn insert_instance(inst: &ProcessInstance) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_instance \
        (id, definition_key, business_key, state, variables, created_at, updated_at, ended_at, \
         org_id, parent_instance_id, parent_token_id, parent_node_bpmn_id) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
        .to_string();
    let params = vec![
        DataValue::String(inst.id.clone()),
        DataValue::String(inst.definition_key.clone()),
        opt_text(&inst.business_key),
        DataValue::String(instance_state_str(inst.state).to_string()),
        DataValue::Json(inst.variables.to_json().to_string()),
        DataValue::DateTime(inst.created_at),
        DataValue::DateTime(inst.updated_at),
        opt_ts(&inst.ended_at),
        opt_text(&inst.org_id),
        opt_text(&inst.parent_instance_id),
        opt_text(&inst.parent_token_id),
        opt_text(&inst.parent_node_bpmn_id),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 实例 UPDATE（按 id）。父子/组织列在创建后不变，此处不重复更新，仅更新易变列。
pub fn update_instance(inst: &ProcessInstance) -> (String, SqlParams) {
    let sql = "UPDATE cmx_flow_instance SET \
        definition_key = $2, business_key = $3, state = $4, variables = $5, \
        updated_at = $6, ended_at = $7 WHERE id = $1"
        .to_string();
    let params = vec![
        DataValue::String(inst.id.clone()),
        DataValue::String(inst.definition_key.clone()),
        opt_text(&inst.business_key),
        DataValue::String(instance_state_str(inst.state).to_string()),
        DataValue::Json(inst.variables.to_json().to_string()),
        DataValue::DateTime(inst.updated_at),
        opt_ts(&inst.ended_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 令牌 INSERT。
pub fn insert_token(token: &Token) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_token \
        (id, instance_id, node_bpmn_id, state, parent_id, created_at, updated_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7)"
        .to_string();
    let params = vec![
        DataValue::String(token.id.clone()),
        DataValue::String(token.instance_id.clone()),
        DataValue::String(token.node_bpmn_id.clone()),
        DataValue::String(token_state_str(token.state).to_string()),
        opt_text(&token.parent_id),
        DataValue::DateTime(token.created_at),
        DataValue::DateTime(token.updated_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 任务 INSERT。
pub fn insert_task(task: &Task) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_task \
        (id, instance_id, token_id, node_bpmn_id, name, assignee, candidate_groups, \
         element_value, owner_user_id, parent_task_id, delegation_state, \
         completed, created_at, completed_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"
        .to_string();
    let params = vec![
        DataValue::String(task.id.clone()),
        DataValue::String(task.instance_id.clone()),
        DataValue::String(task.token_id.clone()),
        DataValue::String(task.node_bpmn_id.clone()),
        opt_text(&task.name),
        opt_text(&task.assignee),
        opt_text(&task.candidate_groups),
        opt_json(&task.element_value),
        opt_text(&task.owner_user_id),
        opt_text(&task.parent_task_id),
        opt_text(&task.delegation_state),
        DataValue::Bool(task.completed),
        DataValue::DateTime(task.created_at),
        opt_ts(&task.completed_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 多实例执行域 INSERT。collection 落 jsonb 数组。
pub fn insert_mi_scope(scope: &MiScope) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_mi_scope \
        (id, instance_id, node_bpmn_id, sequential, total, completed, next_index, \
         collection, element_var, completion_condition, finished) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        .to_string();
    let collection_json = JsonValue::Array(scope.collection.clone()).to_string();
    let params = vec![
        DataValue::String(scope.id.clone()),
        DataValue::String(scope.instance_id.clone()),
        DataValue::String(scope.node_bpmn_id.clone()),
        DataValue::Bool(scope.sequential),
        DataValue::Int(scope.total as i64),
        DataValue::Int(scope.completed as i64),
        DataValue::Int(scope.next_index as i64),
        DataValue::Json(collection_json),
        opt_text(&scope.element_var),
        opt_text(&scope.completion_condition),
        DataValue::Bool(scope.finished),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 定时器作业 INSERT（M2.5）。due_at 是 NOT NULL TIMESTAMPTZ，直接绑 DateTime。
pub fn insert_job(job: &TimerJob) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_job \
        (id, instance_id, token_id, boundary_bpmn_id, cancel_activity, due_at, created_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7)"
        .to_string();
    let params = vec![
        DataValue::String(job.id.clone()),
        DataValue::String(job.instance_id.clone()),
        DataValue::String(job.token_id.clone()),
        DataValue::String(job.boundary_bpmn_id.clone()),
        DataValue::Bool(job.cancel_activity),
        DataValue::DateTime(job.due_at),
        DataValue::DateTime(job.created_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 任务候选人 INSERT（M4.1）。
pub fn insert_candidate(c: &TaskCandidate) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_task_candidate \
        (id, task_id, instance_id, candidate_type, candidate_ref, resolved_user_id) \
        VALUES ($1, $2, $3, $4, $5, $6)"
        .to_string();
    let params = vec![
        DataValue::String(c.id.clone()),
        DataValue::String(c.task_id.clone()),
        DataValue::String(c.instance_id.clone()),
        DataValue::String(candidate_kind_str(c.candidate_type).to_string()),
        DataValue::String(c.candidate_ref.clone()),
        DataValue::String(c.resolved_user_id.clone()),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 抄送记录 INSERT（M4.2）。node_bpmn_id/from_user/reason 可空用裸 Null；read_at 可空 TIMESTAMPTZ。
pub fn insert_cc(cc: &CcRecord) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_cc \
        (id, instance_id, node_bpmn_id, to_user_id, from_user_id, reason, read_at, created_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
        .to_string();
    let params = vec![
        DataValue::String(cc.id.clone()),
        DataValue::String(cc.instance_id.clone()),
        opt_text(&cc.node_bpmn_id),
        DataValue::String(cc.to_user_id.clone()),
        opt_text(&cc.from_user_id),
        opt_text(&cc.reason),
        opt_ts(&cc.read_at),
        DataValue::DateTime(cc.created_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 转签台账 INSERT（M4.3）。
pub fn insert_delegation(d: &TaskDelegation) -> (String, SqlParams) {
    let sql = "INSERT INTO cmx_flow_task_delegation \
        (id, task_id, instance_id, kind, from_user_id, to_user_id, temp_task_id, reason, created_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
        .to_string();
    let params = vec![
        DataValue::String(d.id.clone()),
        DataValue::String(d.task_id.clone()),
        DataValue::String(d.instance_id.clone()),
        DataValue::String(d.kind.clone()),
        DataValue::String(d.from_user_id.clone()),
        DataValue::String(d.to_user_id.clone()),
        opt_text(&d.temp_task_id),
        opt_text(&d.reason),
        DataValue::DateTime(d.created_at),
    ];
    (sql, SqlParams::DataValues(params))
}

// ============================ 写：历史归档（RU → HI） ============================

/// 历史实例 upsert（幂等：同 id 再归档则更新）。archived_at 由调用方传入统一时刻。
pub fn upsert_hi_instance(
    inst: &ProcessInstance,
    archived_at: DateTime<Utc>,
) -> (String, SqlParams) {
    let duration_ms: DataValue = match inst.ended_at {
        Some(end) => DataValue::Int((end - inst.created_at).num_milliseconds()),
        None => DataValue::NullTyped(SqlTypeMarker::Int),
    };
    let sql = "INSERT INTO cmx_flow_hi_instance \
        (id, definition_key, business_key, state, variables, created_at, ended_at, duration_ms, archived_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
        ON CONFLICT (id) DO UPDATE SET \
          state = EXCLUDED.state, variables = EXCLUDED.variables, \
          ended_at = EXCLUDED.ended_at, duration_ms = EXCLUDED.duration_ms, \
          archived_at = EXCLUDED.archived_at"
        .to_string();
    let params = vec![
        DataValue::String(inst.id.clone()),
        DataValue::String(inst.definition_key.clone()),
        opt_text(&inst.business_key),
        DataValue::String(instance_state_str(inst.state).to_string()),
        DataValue::Json(inst.variables.to_json().to_string()),
        DataValue::DateTime(inst.created_at),
        opt_ts(&inst.ended_at),
        duration_ms,
        DataValue::DateTime(archived_at),
    ];
    (sql, SqlParams::DataValues(params))
}

/// 历史任务 upsert（幂等）。仅归档已办结任务。
pub fn upsert_hi_task(task: &Task, archived_at: DateTime<Utc>) -> (String, SqlParams) {
    let duration_ms: DataValue = match task.completed_at {
        Some(done) => DataValue::Int((done - task.created_at).num_milliseconds()),
        None => DataValue::NullTyped(SqlTypeMarker::Int),
    };
    let sql = "INSERT INTO cmx_flow_hi_task \
        (id, instance_id, node_bpmn_id, name, assignee, created_at, completed_at, duration_ms, archived_at) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
        ON CONFLICT (id) DO UPDATE SET \
          completed_at = EXCLUDED.completed_at, duration_ms = EXCLUDED.duration_ms, \
          archived_at = EXCLUDED.archived_at"
        .to_string();
    let params = vec![
        DataValue::String(task.id.clone()),
        DataValue::String(task.instance_id.clone()),
        DataValue::String(task.node_bpmn_id.clone()),
        opt_text(&task.name),
        opt_text(&task.assignee),
        DataValue::DateTime(task.created_at),
        opt_ts(&task.completed_at),
        duration_ms,
        DataValue::DateTime(archived_at),
    ];
    (sql, SqlParams::DataValues(params))
}

// ============================ 读：DataSet → DTO ============================

/// 从 DataSet 首行还原实例；无行则 None。
pub fn row_to_instance(ds: &DataSet) -> StoreResult<Option<ProcessInstance>> {
    let Some(row) = ds.iter().next() else {
        return Ok(None);
    };
    let schema = ds.schema.as_ref();

    let id = get_string(row, schema, "id")?;
    let definition_key = get_string(row, schema, "definition_key")?;
    let business_key = get_opt_string(row, schema, "business_key");
    let state = parse_instance_state(&get_string(row, schema, "state")?)?;
    let variables = get_variables(row, schema, "variables")?;
    let created_at = get_ts(row, schema, "created_at")?;
    let updated_at = get_ts(row, schema, "updated_at")?;
    let ended_at = get_opt_ts(row, schema, "ended_at");

    Ok(Some(ProcessInstance {
        id,
        definition_key,
        business_key,
        state,
        variables,
        created_at,
        updated_at,
        ended_at,
        org_id: get_opt_string(row, schema, "org_id"),
        parent_instance_id: get_opt_string(row, schema, "parent_instance_id"),
        parent_token_id: get_opt_string(row, schema, "parent_token_id"),
        parent_node_bpmn_id: get_opt_string(row, schema, "parent_node_bpmn_id"),
    }))
}

/// DataSet → 实例头列表（find_child_instances 用；含父子/组织列）。
pub fn rows_to_instances(ds: &DataSet) -> StoreResult<Vec<ProcessInstance>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(ProcessInstance {
            id: get_string(row, schema, "id")?,
            definition_key: get_string(row, schema, "definition_key")?,
            business_key: get_opt_string(row, schema, "business_key"),
            state: parse_instance_state(&get_string(row, schema, "state")?)?,
            variables: get_variables(row, schema, "variables")?,
            created_at: get_ts(row, schema, "created_at")?,
            updated_at: get_ts(row, schema, "updated_at")?,
            ended_at: get_opt_ts(row, schema, "ended_at"),
            org_id: get_opt_string(row, schema, "org_id"),
            parent_instance_id: get_opt_string(row, schema, "parent_instance_id"),
            parent_token_id: get_opt_string(row, schema, "parent_token_id"),
            parent_node_bpmn_id: get_opt_string(row, schema, "parent_node_bpmn_id"),
        });
    }
    Ok(out)
}

/// DataSet → 令牌列表。
pub fn rows_to_tokens(ds: &DataSet) -> StoreResult<Vec<Token>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(Token {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            node_bpmn_id: get_string(row, schema, "node_bpmn_id")?,
            state: parse_token_state(&get_string(row, schema, "state")?)?,
            parent_id: get_opt_string(row, schema, "parent_id"),
            created_at: get_ts(row, schema, "created_at")?,
            updated_at: get_ts(row, schema, "updated_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 任务列表。
pub fn rows_to_tasks(ds: &DataSet) -> StoreResult<Vec<Task>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(Task {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            token_id: get_string(row, schema, "token_id")?,
            node_bpmn_id: get_string(row, schema, "node_bpmn_id")?,
            name: get_opt_string(row, schema, "name"),
            assignee: get_opt_string(row, schema, "assignee"),
            candidate_groups: get_opt_string(row, schema, "candidate_groups"),
            element_value: get_opt_json(row, schema, "element_value"),
            owner_user_id: get_opt_string(row, schema, "owner_user_id"),
            parent_task_id: get_opt_string(row, schema, "parent_task_id"),
            delegation_state: get_opt_string(row, schema, "delegation_state"),
            completed: get_bool(row, schema, "completed"),
            created_at: get_ts(row, schema, "created_at")?,
            completed_at: get_opt_ts(row, schema, "completed_at"),
        });
    }
    Ok(out)
}

/// DataSet → 多实例执行域列表。
pub fn rows_to_mi_scopes(ds: &DataSet) -> StoreResult<Vec<MiScope>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        let collection = match get_opt_json(row, schema, "collection") {
            Some(JsonValue::Array(arr)) => arr,
            _ => Vec::new(),
        };
        out.push(MiScope {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            node_bpmn_id: get_string(row, schema, "node_bpmn_id")?,
            sequential: get_bool(row, schema, "sequential"),
            total: get_i64(row, schema, "total").max(0) as usize,
            completed: get_i64(row, schema, "completed").max(0) as usize,
            next_index: get_i64(row, schema, "next_index").max(0) as usize,
            collection,
            element_var: get_opt_string(row, schema, "element_var"),
            completion_condition: get_opt_string(row, schema, "completion_condition"),
            finished: get_bool(row, schema, "finished"),
        });
    }
    Ok(out)
}

/// DataSet → 定时器作业列表（M2.5）。
pub fn rows_to_jobs(ds: &DataSet) -> StoreResult<Vec<TimerJob>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(TimerJob {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            token_id: get_string(row, schema, "token_id")?,
            boundary_bpmn_id: get_string(row, schema, "boundary_bpmn_id")?,
            cancel_activity: get_bool(row, schema, "cancel_activity"),
            due_at: get_ts(row, schema, "due_at")?,
            created_at: get_ts(row, schema, "created_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 任务候选人列表（M4.1）。
pub fn rows_to_candidates(ds: &DataSet) -> StoreResult<Vec<TaskCandidate>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(TaskCandidate {
            id: get_string(row, schema, "id")?,
            task_id: get_string(row, schema, "task_id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            candidate_type: parse_candidate_kind(&get_string(row, schema, "candidate_type")?)?,
            candidate_ref: get_string(row, schema, "candidate_ref")?,
            resolved_user_id: get_string(row, schema, "resolved_user_id")?,
        });
    }
    Ok(out)
}

/// DataSet → 抄送记录列表（M4.2，load_snapshot 用）。
pub fn rows_to_cc(ds: &DataSet) -> StoreResult<Vec<CcRecord>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(CcRecord {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            node_bpmn_id: get_opt_string(row, schema, "node_bpmn_id"),
            to_user_id: get_string(row, schema, "to_user_id")?,
            from_user_id: get_opt_string(row, schema, "from_user_id"),
            reason: get_opt_string(row, schema, "reason"),
            read_at: get_opt_ts(row, schema, "read_at"),
            created_at: get_ts(row, schema, "created_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 转签台账列表（M4.3，load_snapshot 用）。
pub fn rows_to_delegations(ds: &DataSet) -> StoreResult<Vec<TaskDelegation>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(TaskDelegation {
            id: get_string(row, schema, "id")?,
            task_id: get_string(row, schema, "task_id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            kind: get_string(row, schema, "kind")?,
            from_user_id: get_string(row, schema, "from_user_id")?,
            to_user_id: get_string(row, schema, "to_user_id")?,
            temp_task_id: get_opt_string(row, schema, "temp_task_id"),
            reason: get_opt_string(row, schema, "reason"),
            created_at: get_ts(row, schema, "created_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 抄送摘要列表（find_cc_for_user 用；JOIN 出实例 def_key/biz_key）。
pub fn rows_to_cc_summaries(ds: &DataSet) -> StoreResult<Vec<CcSummary>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(CcSummary {
            id: get_string(row, schema, "id")?,
            instance_id: get_string(row, schema, "instance_id")?,
            business_key: get_opt_string(row, schema, "business_key"),
            definition_key: get_string(row, schema, "definition_key")?,
            node_bpmn_id: get_opt_string(row, schema, "node_bpmn_id"),
            reason: get_opt_string(row, schema, "reason"),
            read: get_opt_ts(row, schema, "read_at").is_some(),
            created_at: get_ts(row, schema, "created_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 到期作业轻量视图列表（find_due_jobs）。
pub fn rows_to_due_jobs(ds: &DataSet) -> StoreResult<Vec<DueJob>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(DueJob {
            instance_id: get_string(row, schema, "instance_id")?,
            job_id: get_string(row, schema, "id")?,
            due_at: get_ts(row, schema, "due_at")?,
        });
    }
    Ok(out)
}

/// DataSet → 实例摘要列表（list_instances 用）。
pub fn rows_to_summaries(ds: &DataSet) -> StoreResult<Vec<InstanceSummary>> {
    let schema = ds.schema.as_ref();
    let mut out = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        out.push(InstanceSummary {
            id: get_string(row, schema, "id")?,
            definition_key: get_string(row, schema, "definition_key")?,
            business_key: get_opt_string(row, schema, "business_key"),
            state: parse_instance_state(&get_string(row, schema, "state")?)?,
            variables: get_variables(row, schema, "variables")?,
            open_task_count: get_i64(row, schema, "open_task_count").max(0) as usize,
            created_at: get_ts(row, schema, "created_at")?,
            updated_at: get_ts(row, schema, "updated_at")?,
        });
    }
    Ok(out)
}

// ============================ 取值助手（DataValue → Rust） ============================

use cmx_core::model::data::dataset::{Row, Schema};

fn get_string(row: &Row, schema: &Schema, col: &str) -> StoreResult<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Ok(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Ok(s.to_string()),
        other => Err(StoreError::Backend(format!(
            "列 {col} 期望文本，实际 {other:?}"
        ))),
    }
}

fn get_opt_string(row: &Row, schema: &Schema, col: &str) -> Option<String> {
    match row.get_by_name(schema, col) {
        Some(DataValue::String(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn get_bool(row: &Row, schema: &Schema, col: &str) -> bool {
    matches!(row.get_by_name(schema, col), Some(DataValue::Bool(true)))
}

/// 取整数列（count(*) 回 int8 → DataValue::Int）。缺失/非整数回退 0。
fn get_i64(row: &Row, schema: &Schema, col: &str) -> i64 {
    match row.get_by_name(schema, col) {
        Some(DataValue::Int(v)) => *v,
        _ => 0,
    }
}

fn get_ts(row: &Row, schema: &Schema, col: &str) -> StoreResult<DateTime<Utc>> {
    match row.get_by_name(schema, col) {
        Some(DataValue::DateTime(dt)) => Ok(*dt),
        other => Err(StoreError::Backend(format!(
            "列 {col} 期望时间戳，实际 {other:?}"
        ))),
    }
}

fn get_opt_ts(row: &Row, schema: &Schema, col: &str) -> Option<DateTime<Utc>> {
    match row.get_by_name(schema, col) {
        Some(DataValue::DateTime(dt)) => Some(*dt),
        _ => None,
    }
}

/// 可空 jsonb 列 → Option<serde_json::Value>。NULL / 解析失败回退 None。
fn get_opt_json(row: &Row, schema: &Schema, col: &str) -> Option<JsonValue> {
    match row.get_by_name(schema, col) {
        Some(DataValue::Json(s)) => serde_json::from_str(s).ok(),
        Some(DataValue::String(s)) => serde_json::from_str(s).ok(),
        _ => None,
    }
}

/// jsonb 列 → Variables。DataValue::Json 承载 JSON 字符串；空/异常回退空变量集。
fn get_variables(row: &Row, schema: &Schema, col: &str) -> StoreResult<Variables> {
    match row.get_by_name(schema, col) {
        Some(DataValue::Json(s)) => {
            let value: JsonValue = serde_json::from_str(s)
                .map_err(|e| StoreError::Backend(format!("解析 variables jsonb 失败: {e}")))?;
            Ok(Variables::from_json(value))
        }
        Some(DataValue::String(s)) => {
            // 兜底：某些路径 jsonb 可能以字符串回来。
            let value: JsonValue = serde_json::from_str(s).unwrap_or(JsonValue::Null);
            Ok(Variables::from_json(value))
        }
        _ => Ok(Variables::new()),
    }
}
