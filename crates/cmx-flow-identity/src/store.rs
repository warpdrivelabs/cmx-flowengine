/*
 * @Describe: 内建身份 CRUD 存储（fid_* 表的增删改查）。
 *
 * 供 app 层身份管理工作台（组织/角色/用户/岗位四区）落库。只做最小可用 CRUD：list/upsert/
 * delete(软删 archived=1)。载体统一 serde_json::Value（对齐前端 cmx-data-comp 数据集），
 * 写入用 cmx-database-pg 的参数化 execute，读用 query_sql → JSON 行。
 *
 * 与 resolver.rs 分工：resolver 只读、解析候选；store 是维护态的写+读。二者共用 fid_* 表。
 */

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};
use serde_json::{Value, json};

use crate::error::{IdentityError, IdentityResult};

/// 身份主数据 CRUD 门面。持 db_id（存 fid_* 表的库）。
#[derive(Clone)]
pub struct IdentityStore {
    db_id: String,
}

/// 四类身份实体（决定操作哪张表 + 字段集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Org,
    Role,
    Position,
    User,
}

impl Entity {
    /// 从 URL 段解析实体类型。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "org" | "orgs" => Some(Entity::Org),
            "role" | "roles" => Some(Entity::Role),
            "position" | "positions" => Some(Entity::Position),
            "user" | "users" => Some(Entity::User),
            _ => None,
        }
    }

    fn table(self) -> &'static str {
        match self {
            Entity::Org => "fid_org",
            Entity::Role => "fid_role",
            Entity::Position => "fid_position",
            Entity::User => "fid_user",
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

impl IdentityStore {
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。app 在 local 模式启动时调用一次。
    pub async fn ensure_schema(&self) -> IdentityResult<()> {
        for stmt in crate::ddl::DDL_STATEMENTS {
            execute_sql(&self.db_id, None, stmt)
                .await
                .map_err(|e| IdentityError::Backend(format!("建表失败: {e}")))?;
        }
        Ok(())
    }

    /// 列出某类实体（未归档），按 code/username 排序，返回 JSON 数组。
    pub async fn list(&self, entity: Entity) -> IdentityResult<Vec<Value>> {
        let sql = match entity {
            Entity::Org => "SELECT id, code, name, parent_id, path, leader_user_id, sort_order \
                            FROM fid_org WHERE archived = 0 ORDER BY sort_order, code"
                .to_string(),
            Entity::Role => {
                "SELECT id, code, name FROM fid_role WHERE archived = 0 ORDER BY code".to_string()
            }
            Entity::Position => "SELECT id, code, name FROM fid_position WHERE archived = 0 ORDER BY code"
                .to_string(),
            Entity::User => "SELECT id, username, name, org_id FROM fid_user WHERE archived = 0 ORDER BY username"
                .to_string(),
        };
        let ds = query_sql(&self.db_id, None, &sql, "fid_list")
            .await
            .map_err(|e| IdentityError::Backend(format!("查询失败: {e}")))?;
        Ok(dataset_to_json_rows(&ds))
    }

    /// upsert 一条实体（有 id 且已存在 → 更新；否则插入）。返回该记录 id。
    ///
    /// 字段从 JSON body 取，缺失用空/默认。**参数化写入**（值走 SqlParams，防注入）。
    pub async fn upsert(&self, entity: Entity, body: &Value) -> IdentityResult<String> {
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| gen_id(entity));

        let s = |k: &str| -> String {
            body.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        let (sql, params) = match entity {
            Entity::Org => {
                let parent = s("parentId");
                // path：有 parent 时 parent.path/id，否则 /id（简化：直接 /id 段，app 可后续重算）。
                let path = format!("/{id}");
                (
                    "INSERT INTO fid_org (id, code, name, parent_id, path, leader_user_id, sort_order, updated_at) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7, now()) \
                     ON CONFLICT (id) DO UPDATE SET code=$2, name=$3, parent_id=$4, path=$5, leader_user_id=$6, sort_order=$7, updated_at=now()"
                        .to_string(),
                    SqlParams::DataValues(vec![
                        DataValue::String(id.clone()),
                        DataValue::String(s("code")),
                        DataValue::String(s("name")),
                        opt_str(&parent),
                        DataValue::String(path),
                        opt_str(&s("leaderUserId")),
                        DataValue::Int(body.get("sortOrder").and_then(|v| v.as_i64()).unwrap_or(0)),
                    ]),
                )
            }
            Entity::Role => (
                "INSERT INTO fid_role (id, code, name, updated_at) VALUES ($1,$2,$3, now()) \
                 ON CONFLICT (id) DO UPDATE SET code=$2, name=$3, updated_at=now()"
                    .to_string(),
                SqlParams::DataValues(vec![
                    DataValue::String(id.clone()),
                    DataValue::String(s("code")),
                    DataValue::String(s("name")),
                ]),
            ),
            Entity::Position => (
                "INSERT INTO fid_position (id, code, name, updated_at) VALUES ($1,$2,$3, now()) \
                 ON CONFLICT (id) DO UPDATE SET code=$2, name=$3, updated_at=now()"
                    .to_string(),
                SqlParams::DataValues(vec![
                    DataValue::String(id.clone()),
                    DataValue::String(s("code")),
                    DataValue::String(s("name")),
                ]),
            ),
            Entity::User => (
                "INSERT INTO fid_user (id, username, name, org_id, updated_at) VALUES ($1,$2,$3,$4, now()) \
                 ON CONFLICT (id) DO UPDATE SET username=$2, name=$3, org_id=$4, updated_at=now()"
                    .to_string(),
                SqlParams::DataValues(vec![
                    DataValue::String(id.clone()),
                    DataValue::String(s("username")),
                    opt_str(&s("name")),
                    opt_str(&s("orgId")),
                ]),
            ),
        };

        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| IdentityError::Backend(format!("写入失败: {e}")))?;
        Ok(id)
    }

    /// 软删除一条实体（archived=1）。
    pub async fn delete(&self, entity: Entity, id: &str) -> IdentityResult<()> {
        let sql = format!(
            "UPDATE {} SET archived = 1, updated_at = now() WHERE id = '{}'",
            entity.table(),
            esc(id)
        );
        execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| IdentityError::Backend(format!("删除失败: {e}")))?;
        Ok(())
    }

    /// 设置用户的角色集合（先清后插；用于用户编辑页的角色多选）。
    pub async fn set_user_roles(&self, user_id: &str, role_ids: &[String]) -> IdentityResult<()> {
        let uid = esc(user_id);
        let del = format!("DELETE FROM fid_user_role WHERE user_id = '{uid}'");
        execute_sql(&self.db_id, None, &del)
            .await
            .map_err(|e| IdentityError::Backend(format!("清角色失败: {e}")))?;
        for rid in role_ids {
            let sql = format!(
                "INSERT INTO fid_user_role (user_id, role_id, archived) VALUES ('{}','{}',0) \
                 ON CONFLICT (user_id, role_id) DO UPDATE SET archived=0",
                uid,
                esc(rid)
            );
            execute_sql(&self.db_id, None, &sql)
                .await
                .map_err(|e| IdentityError::Backend(format!("加角色失败: {e}")))?;
        }
        Ok(())
    }
}

/// 生成实体 id（前缀 + 计数无关的随机替代——这里用表前缀 + 简单时间无关标记由调用方保证唯一；
/// 为避免依赖时钟/随机，用调用方传入 id 为主，未传时用一个基于内容的占位由 DB 唯一约束兜底）。
///
/// 说明：本模块不引入随机/时钟（对齐引擎中立哲学）。前端新增记录时应带一个临时 id 或 code，
/// upsert 用 code 作 id 的场景可由 app 决定。这里兜底返回 `<entity>_<code-less>` 需调用方避免。
fn gen_id(entity: Entity) -> String {
    // 兜底：用实体名前缀 + 固定占位；实际由 app 层保证传入 id（对齐 bigint-id-backend-generation
    // 的「新增时前端带临时 id」约定）。此处仅防 panic，不承诺唯一。
    format!("{}_new", entity.table())
}

/// 空串 → Null，否则 String（可空列写入）。
fn opt_str(s: &str) -> DataValue {
    if s.is_empty() {
        DataValue::Null
    } else {
        DataValue::String(s.to_string())
    }
}

/// ZmcDataSet → JSON 行数组（列名 → 值），供 API 直接吐给前端。
fn dataset_to_json_rows(ds: &cmx_core::model::data::dataset::DataSet) -> Vec<Value> {
    let schema = ds.schema.as_ref();
    let mut rows = Vec::with_capacity(ds.row_count());
    for row in ds.iter() {
        let mut obj = serde_json::Map::new();
        for field in schema.fields.iter() {
            let name = &field.name;
            let v = row.get_by_name(schema, name);
            obj.insert(name.clone(), datavalue_to_json(v));
        }
        rows.push(Value::Object(obj));
    }
    rows
}

/// DataValue → serde_json::Value（覆盖身份表用到的标量类型）。
fn datavalue_to_json(v: Option<&DataValue>) -> Value {
    match v {
        None | Some(DataValue::Null) => Value::Null,
        Some(DataValue::String(s)) => json!(s),
        Some(DataValue::ShortStr(s)) => json!(s.to_string()),
        Some(DataValue::LongStr(s)) => json!(s.to_string()),
        Some(DataValue::Int(i)) => json!(i),
        Some(DataValue::Bool(b)) => json!(b),
        Some(other) => json!(format!("{other:?}")),
    }
}
