//! 内建身份主数据 CRUD（P0-c）——仅 `FLOW_IDENTITY_MODE=local` 时有意义。
//!
//! 对标字典维护工作台的后端：组织/角色/岗位/用户四类实体的 list/upsert/delete，落 `fid_*` 表
//! （见 cmx-flow-identity）。external 模式下这些端点仍在，但操作的是内建库——若未启用 local，
//! 前端不会挂身份工作台菜单，端点闲置无害。
//!
//! 路由（挂 /flow 前缀下）：
//!   GET    /flow/identity/{entity}          —— 列表（entity ∈ orgs|roles|positions|users）
//!   POST   /flow/identity/{entity}          —— 新增/更新（body 为实体 JSON）
//!   DELETE /flow/identity/{entity}/{id}     —— 软删除
//!   POST   /flow/identity/users/{id}/roles  —— 设置用户角色集合（body: {roleIds:[]}）
//!   GET    /flow/identity/mode              —— 探测当前身份模式（前端据此决定是否挂工作台）

use axum::Json;
use axum::extract::Path;
use serde_json::{Value, json};

use cmx_flow_identity::{Entity, IdentityStore};

use crate::engine::{current_iam_db_id, identity_is_local};
use crate::resp::{ApiResp, FlowError, Result};

/// 解析 entity 段 → Entity，未知则业务错误。
fn parse_entity(seg: &str) -> Result<Entity> {
    Entity::from_str(seg).ok_or_else(|| FlowError::business_error(format!("未知身份实体: {seg}")))
}

/// 取内建身份 store（绑当前租户 IAM 库；local 模式 fid_* 表建在该库）。
fn store() -> IdentityStore {
    IdentityStore::new(current_iam_db_id())
}

/// GET /identity/mode —— 返回 {mode, editable}。前端据此决定是否挂身份管理工作台。
pub async fn get_mode() -> Result<Json<ApiResp<Value>>> {
    let local = identity_is_local();
    Ok(Json(ApiResp::ok(json!({
        "mode": if local { "local" } else { "external" },
        "editable": local,
    }))))
}

/// GET /identity/{entity} —— 列表。
pub async fn list(Path(entity): Path<String>) -> Result<Json<ApiResp<Value>>> {
    let e = parse_entity(&entity)?;
    let rows = store()
        .list(e)
        .await
        .map_err(|err| FlowError::business_error(format!("查询失败: {err}")))?;
    Ok(Json(ApiResp::ok(json!({ "items": rows }))))
}

/// POST /identity/{entity} —— 新增/更新。external 模式拒绝写（只 local 可编辑）。
pub async fn upsert(
    Path(entity): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<ApiResp<Value>>> {
    if !identity_is_local() {
        return Err(FlowError::business_error(
            "当前为外接身份模式（external），内建身份只读；如需内建请设 FLOW_IDENTITY_MODE=local",
        ));
    }
    let e = parse_entity(&entity)?;
    let id = store()
        .upsert(e, &body)
        .await
        .map_err(|err| FlowError::business_error(format!("保存失败: {err}")))?;
    Ok(Json(ApiResp::ok(json!({ "id": id }))))
}

/// DELETE /identity/{entity}/{id} —— 软删除。
pub async fn delete(Path((entity, id)): Path<(String, String)>) -> Result<Json<ApiResp<Value>>> {
    if !identity_is_local() {
        return Err(FlowError::business_error("当前为外接身份模式（external），内建身份只读"));
    }
    let e = parse_entity(&entity)?;
    store()
        .delete(e, &id)
        .await
        .map_err(|err| FlowError::business_error(format!("删除失败: {err}")))?;
    Ok(Json(ApiResp::ok(json!({ "deleted": id }))))
}

/// POST /identity/users/{id}/roles —— 设置用户角色集合（body: {roleIds:[]}）。
pub async fn set_user_roles(
    Path(user_id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<ApiResp<Value>>> {
    if !identity_is_local() {
        return Err(FlowError::business_error("当前为外接身份模式（external），内建身份只读"));
    }
    let role_ids: Vec<String> = body
        .get("roleIds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    store()
        .set_user_roles(&user_id, &role_ids)
        .await
        .map_err(|err| FlowError::business_error(format!("设置角色失败: {err}")))?;
    Ok(Json(ApiResp::ok(json!({ "userId": user_id, "roleCount": role_ids.len() }))))
}
