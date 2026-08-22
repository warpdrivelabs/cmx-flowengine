/*
 * @Describe: PgSubflowRouter —— 子流程组织路由的 PG 实现（M5.2）。
 *
 * 实现 cmx-flow-model::SubflowRouter：给定「逻辑子流程 key + 组织 id」解析出具体子流程定义 key。
 * 数据源 cmx_flow_subflow_binding（called_key + org_id → target_definition_key），三层解析：
 *   1. 精确：本组织 org_id 的启用绑定；
 *   2. 继承：沿 cmx_org.path 向上找最近祖先的绑定（path 最长 = 最近，优先）；
 *   3. 兜底：org_id IS NULL 的默认绑定。
 * 全无 → RouteError::NoBinding。
 *
 * 只读查询走 cmx-database-pg 的 query_sql。引擎经 SubflowRouter trait 依赖它，不直连表——中立。
 */

use async_trait::async_trait;
use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};
use cmx_flow_model::{RouteError, RouteResult, SubflowRouter};

/// 子流程组织路由器。持目标 db_id（绑定表 + cmx_org 所在库），所有查询走该库。
#[derive(Clone)]
pub struct PgSubflowRouter {
    db_id: String,
    /// 自分级维度字典规格注册表（RD1）：`dim_key → DimSpec`。用于「沿维度字典物化路径向上继承」。
    /// `org` 维度内建（不必注册）；其余自分级 cf_* 字典由 app 层按 DCT 元数据注册进来。
    /// 未注册的维度 = 平级字典，只走精确 + 兜底（无继承）。
    dim_specs: std::collections::HashMap<String, DimSpec>,
    /// RD5（可选）：维度层级解析器。注入后「继承」步改由它返回祖先链（独立部署经 HTTP 读字典层级），
    /// 逐祖先查本地绑定表，不再直连字典表 JOIN。None = 沿用 DimSpec 直连字典表继承（默认，零回归）。
    dim_resolver: Option<std::sync::Arc<dyn cmx_flow_model::DimensionResolver>>,
}

/// 一个自分级路由维度的物理规格（RD1）——把 M5.2 写死的 cmx_org/path/'/' 参数化。
#[derive(Clone, Debug)]
pub struct DimSpec {
    /// 维度字典物理表名（如 `cmx_org` / `cf_legal_entity`）。仅允许白名单形态（见 [`is_safe_table`]）。
    pub table: String,
    /// 条目主键列（org=`id`；cf_* 视 pk 为 `id` 或 `code`）。
    pub id_col: String,
    /// 物化路径列（org=`path`；cf_*=`full_path`）。
    pub path_col: String,
    /// 路径分隔符（org=`/`；cf_*=`.`）——继承前缀匹配追加它修边界（`/a` 不再错配 `/ab`）。
    pub delim: String,
}

impl DimSpec {
    /// 内建组织维度规格（cmx_org / id / path / '/'）——保住 M5.2 行为。
    pub fn org() -> Self {
        Self {
            table: "cmx_org".into(),
            id_col: "id".into(),
            path_col: "path".into(),
            delim: "/".into(),
        }
    }
}

/// 表/列名白名单：仅允许 `cmx_org` 或 `cf_*`/一般标识符（防注入面扩大——虽来自受信 dictMeta）。
fn is_safe_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl PgSubflowRouter {
    /// 用指定 db_id 构建（须已在 cmx-database-pg 注册数据源）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
            dim_specs: std::collections::HashMap::new(),
            dim_resolver: None,
        }
    }

    /// RD5：注入维度层级解析器（继承步改走它，不再直连字典表）。返回 self 便于链式构建。
    pub fn with_dim_resolver(
        mut self,
        resolver: std::sync::Arc<dyn cmx_flow_model::DimensionResolver>,
    ) -> Self {
        self.dim_resolver = Some(resolver);
        self
    }

    /// 注册一个自分级维度字典的物理规格（RD1，app 层按 DCT 元数据调用）。
    /// `org` 内建，无需注册；重复注册以最后一次为准。规格里表/列名非法则忽略（保守跳继承）。
    pub fn register_dim(&mut self, dim_key: impl Into<String>, spec: DimSpec) -> &mut Self {
        self.dim_specs.insert(dim_key.into(), spec);
        self
    }

    /// 取某维度的规格：org 内建；其余查注册表。None = 平级/未注册（无继承层）。
    fn dim_spec(&self, dim_key: &str) -> Option<DimSpec> {
        if dim_key == cmx_flow_model::DIM_ORG {
            return Some(DimSpec::org());
        }
        self.dim_specs.get(dim_key).cloned()
    }

    /// 执行一条只取首行 target_definition_key 的查询；无行 → None。
    async fn query_one_target(&self, sql: &str, tag: &str) -> RouteResult<Option<String>> {
        let ds = query_sql(&self.db_id, None, sql, tag)
            .await
            .map_err(|e| RouteError::Backend(format!("查询子流程绑定失败: {e}")))?;
        let schema = ds.schema.as_ref();
        for row in ds.iter() {
            match row.get_by_name(schema, "target_definition_key") {
                Some(DataValue::String(s)) => return Ok(Some(s.clone())),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => {
                    return Ok(Some(s.to_string()));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// 取某维度取值的祖先取值链（由近及远，不含自身）——供平台侧「维度层级回连端点」(⑤/RD5) 复用。
    /// 沿维度字典物化路径找所有祖先（前缀匹配 + 分隔符修边界），按路径长度 DESC = 最近在前。
    /// 平级/未注册维度或非法规格 → 空链。
    pub async fn ancestors(&self, dim_key: &str, dim_value: &str) -> RouteResult<Vec<String>> {
        let spec = match self.dim_spec(dim_key) {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };
        if !(is_safe_ident(&spec.table)
            && is_safe_ident(&spec.id_col)
            && is_safe_ident(&spec.path_col))
        {
            return Ok(Vec::new());
        }
        let (tbl, idc, pc) = (&spec.table, &spec.id_col, &spec.path_col);
        let d = esc(&spec.delim);
        let v = esc(dim_value);
        let sql = format!(
            "SELECT a.\"{idc}\" AS anc_id \
             FROM \"{tbl}\" a JOIN \"{tbl}\" s ON s.\"{idc}\" = '{v}' \
             WHERE a.\"{idc}\" <> s.\"{idc}\" \
               AND a.\"{pc}\" IS NOT NULL AND s.\"{pc}\" IS NOT NULL \
               AND (s.\"{pc}\" = a.\"{pc}\" OR s.\"{pc}\" LIKE a.\"{pc}\" || '{d}' || '%') \
             ORDER BY length(a.\"{pc}\") DESC"
        );
        let ds = query_sql(&self.db_id, None, &sql, "dim_ancestors")
            .await
            .map_err(|e| RouteError::Backend(format!("查询维度祖先失败: {e}")))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            match row.get_by_name(schema, "anc_id") {
                Some(DataValue::String(s)) => out.push(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => {
                    out.push(s.to_string())
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// 单引号转义（值来自 BPMN 定义 / 实例组织，无强注入面，仍防御）。
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

#[async_trait]
impl SubflowRouter for PgSubflowRouter {
    async fn resolve(
        &self,
        called_key: &str,
        dim_key: &str,
        dim_value: Option<&str>,
    ) -> RouteResult<String> {
        let k = esc(called_key);
        let dk = esc(dim_key);

        // 有维度取值：先精确，再（若该维度是自分级字典）沿其物化路径向上继承。
        if let Some(dv) = dim_value {
            let v = esc(dv);
            // 1) 精确：本维度取值的启用绑定。
            let exact = format!(
                "SELECT target_definition_key FROM cmx_flow_subflow_binding \
                 WHERE called_key = '{k}' AND dim_key = '{dk}' AND dim_value = '{v}' AND enabled = TRUE LIMIT 1"
            );
            if let Some(t) = self.query_one_target(&exact, "subflow_exact").await? {
                return Ok(t);
            }
            // 2) 继承（RD1）：该维度是自分级字典时，沿其物化路径向上找最近祖先的绑定（path 最长=最近）。
            //    把 M5.2 写死的 cmx_org/path 参数化为 DimSpec{table,id_col,path_col,delim}；
            //    平级/未注册维度无 spec → 天然跳过继承（无「上级」概念，非缺陷）。
            // RD5：若注入了维度解析器 → 优先经它取祖先链（由近及远），逐祖先查**本地**绑定表
            //      （独立部署经 HTTP 读字典层级，不直连字典表 JOIN）。
            if let Some(resolver) = &self.dim_resolver {
                let ancestors = resolver.ancestors(dim_key, dv).await?;
                for anc in &ancestors {
                    let av = esc(anc);
                    let sql = format!(
                        "SELECT target_definition_key FROM cmx_flow_subflow_binding \
                         WHERE called_key = '{k}' AND dim_key = '{dk}' AND dim_value = '{av}' AND enabled = TRUE LIMIT 1"
                    );
                    if let Some(t) = self.query_one_target(&sql, "subflow_inherit_dimresolver").await? {
                        return Ok(t);
                    }
                }
            } else if let Some(spec) = self.dim_spec(dim_key) {
                // 白名单校验表/列名（受信 dictMeta，仍防御注入面扩大）；非法则跳继承。
                if is_safe_ident(&spec.table)
                    && is_safe_ident(&spec.id_col)
                    && is_safe_ident(&spec.path_col)
                {
                    let (tbl, idc, pc) = (&spec.table, &spec.id_col, &spec.path_col);
                    let d = esc(&spec.delim);
                    // 追加分隔符修边界 bug：`self.path LIKE anc.path || delim || '%'`，
                    // 使 `/a` 不再错配 `/ab`（祖先自身用 self=anc 的等值覆盖）。
                    let inherited = format!(
                        "SELECT b.target_definition_key \
                         FROM cmx_flow_subflow_binding b \
                         JOIN \"{tbl}\" anc  ON anc.\"{idc}\" = b.dim_value \
                         JOIN \"{tbl}\" self_e ON self_e.\"{idc}\" = '{v}' \
                         WHERE b.called_key = '{k}' AND b.dim_key = '{dk}' AND b.enabled = TRUE \
                           AND self_e.\"{pc}\" IS NOT NULL AND anc.\"{pc}\" IS NOT NULL \
                           AND (self_e.\"{pc}\" = anc.\"{pc}\" \
                                OR self_e.\"{pc}\" LIKE anc.\"{pc}\" || '{d}' || '%') \
                         ORDER BY length(anc.\"{pc}\") DESC LIMIT 1"
                    );
                    if let Some(t) = self.query_one_target(&inherited, "subflow_inherit").await? {
                        return Ok(t);
                    }
                }
            }
        }

        // 3) 兜底：dim_value IS NULL 的默认绑定。
        let default = format!(
            "SELECT target_definition_key FROM cmx_flow_subflow_binding \
             WHERE called_key = '{k}' AND dim_key = '{dk}' AND dim_value IS NULL AND enabled = TRUE LIMIT 1"
        );
        if let Some(t) = self.query_one_target(&default, "subflow_default").await? {
            return Ok(t);
        }

        Err(RouteError::NoBinding {
            called_key: called_key.to_string(),
            dim_key: dim_key.to_string(),
            dim_value: dim_value.map(|s| s.to_string()),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 绑定管理（设计态 CRUD）
//
// PgSubflowRouter 只读解析（运行期）；设计器要能配置绑定，故补一个管理面。同库同表
// （cmx_flow_subflow_binding + cmx_org，IAM_DB_ID）。列绑定时 LEFT JOIN cmx_org 带出
// 组织名，前端不必再查一次。
// ─────────────────────────────────────────────────────────────────────────────

/// 一条子流程路由绑定（设计态视图，含维度取值展示名便于展示）。
#[derive(Debug, Clone)]
pub struct SubflowBinding {
    /// 绑定行 id。
    pub id: String,
    /// 逻辑子流程 key（= callActivity 的 cmx:calledKey）。
    pub called_key: String,
    /// 路由维度 key（"org" / 某字典 dictCode）。
    pub dim_key: String,
    /// 维度取值（None = 默认兜底绑定）。RD0：org 维度即组织 id。
    pub dim_value: Option<String>,
    /// 维度取值展示名（org 维度 JOIN cmx_org 得组织名；兜底绑定为 None）。
    pub dim_value_name: Option<String>,
    /// 目标子流程定义 key。
    pub target_definition_key: String,
    /// 是否启用。
    pub enabled: bool,
    /// 备注。
    pub remark: Option<String>,
}

/// 一个组织节点（设计器组织选择器用）。
#[derive(Debug, Clone)]
pub struct OrgNode {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub path: Option<String>,
}

/// 子流程绑定管理器（设计态 CRUD + 组织树读取）。持绑定表/cmx_org 所在库 db_id。
#[derive(Clone)]
pub struct PgSubflowBindingStore {
    db_id: String,
}

impl PgSubflowBindingStore {
    /// 用指定 db_id 构建（须已注册；同 PgSubflowRouter 的库）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。生产库（primary/IAM）不由引擎 ensure_schema 覆盖，故管理面自带 DDL 兜底。
    /// RD2：补 dim_key/dim_value 维度列（与主 DDL ddl.rs 一致），老 org_id 数据迁移到 dim_value。
    pub async fn ensure_schema(&self) -> Result<(), String> {
        let stmts = [
            "CREATE TABLE IF NOT EXISTS cmx_flow_subflow_binding (\
                id VARCHAR(64) PRIMARY KEY, called_key VARCHAR(128) NOT NULL, org_id VARCHAR(64), \
                target_definition_key VARCHAR(128) NOT NULL, enabled BOOLEAN NOT NULL DEFAULT TRUE, \
                remark VARCHAR(500), created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_key ON cmx_flow_subflow_binding (called_key)",
            "ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_key VARCHAR(64) NOT NULL DEFAULT 'org'",
            "ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_value VARCHAR(128)",
            "UPDATE cmx_flow_subflow_binding SET dim_value = org_id WHERE dim_value IS NULL AND org_id IS NOT NULL",
            "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_dim ON cmx_flow_subflow_binding (called_key, dim_key, dim_value)",
        ];
        for ddl in stmts {
            execute_sql(&self.db_id, None, ddl)
                .await
                .map_err(|e| format!("建绑定表失败: {e}"))?;
        }
        Ok(())
    }

    /// 列某逻辑 key 的全部绑定（org 维度带组织名，兜底绑定排最后）。
    /// RD2：返回 dim_key/dim_value；org 维度 LEFT JOIN cmx_org 带出组织名，其余维度展示名先等于 dim_value
    /// （app 层可用 DimensionCatalog 二次补名）。
    pub async fn list_by_key(&self, called_key: &str) -> Result<Vec<SubflowBinding>, String> {
        let sql = format!(
            "SELECT b.id, b.called_key, b.dim_key, b.dim_value, o.name AS dim_value_name, \
                    b.target_definition_key, b.enabled, b.remark \
             FROM cmx_flow_subflow_binding b \
             LEFT JOIN cmx_org o ON b.dim_key = 'org' AND o.id = b.dim_value \
             WHERE b.called_key = '{}' \
             ORDER BY b.dim_key, (b.dim_value IS NULL), b.dim_value NULLS FIRST",
            esc(called_key)
        );
        let ds = query_sql(&self.db_id, None, &sql, "subflow_binding_list")
            .await
            .map_err(|e| format!("查询绑定失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let gs = |row: &cmx_core::model::data::dataset::Row, c: &str| -> Option<String> {
            match row.get_by_name(schema, c) {
                Some(DataValue::String(s)) => Some(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
                _ => None,
            }
        };
        let gb = |row: &cmx_core::model::data::dataset::Row, c: &str| -> bool {
            matches!(row.get_by_name(schema, c), Some(DataValue::Bool(true)))
        };
        Ok(ds
            .iter()
            .map(|row| {
                let dim_value = gs(row, "dim_value");
                // 展示名：org 维度用 JOIN 出的组织名；其余维度退化为取值本身（app 层可再补）。
                let dim_value_name = gs(row, "dim_value_name").or_else(|| dim_value.clone());
                SubflowBinding {
                    id: gs(row, "id").unwrap_or_default(),
                    called_key: gs(row, "called_key").unwrap_or_default(),
                    dim_key: gs(row, "dim_key").unwrap_or_else(|| "org".into()),
                    dim_value,
                    dim_value_name,
                    target_definition_key: gs(row, "target_definition_key").unwrap_or_default(),
                    enabled: gb(row, "enabled"),
                    remark: gs(row, "remark"),
                }
            })
            .collect())
    }

    /// 列出所有被绑定为目标的子流程定义 key（去重）。用于「哪些定义是子流程」的判定：
    /// 一个定义若被任一组织绑定引用为 target，即视为子流程（不在主流程列表展示）。
    pub async fn list_all_target_keys(&self) -> Result<Vec<String>, String> {
        let sql = "SELECT DISTINCT target_definition_key FROM cmx_flow_subflow_binding \
                   WHERE target_definition_key IS NOT NULL AND target_definition_key <> ''";
        let ds = query_sql(&self.db_id, None, sql, "subflow_target_keys")
            .await
            .map_err(|e| format!("查询子流程目标 key 失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let mut out = Vec::with_capacity(ds.row_count());
        for row in ds.iter() {
            match row.get_by_name(schema, "target_definition_key") {
                Some(DataValue::String(s)) => out.push(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => {
                    out.push(s.to_string())
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// upsert 一条绑定：同 (called_key, dim_key, dim_value) 视为同一绑定（改目标/启用/备注）。
    /// dim_value 为 None 表示该维度的默认兜底绑定。org 维度同时镜像写 org_id 列（兼容）。
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert(
        &self,
        id: &str,
        called_key: &str,
        dim_key: &str,
        dim_value: Option<&str>,
        target_definition_key: &str,
        enabled: bool,
        remark: Option<&str>,
    ) -> Result<(), String> {
        // 先删同 (called_key, dim_key, dim_value) 的旧绑定（dim_value NULL 要特判），再插——避免同维度取值多条。
        let del = match dim_value {
            Some(v) => format!(
                "DELETE FROM cmx_flow_subflow_binding WHERE called_key = '{}' AND dim_key = '{}' AND dim_value = '{}'",
                esc(called_key),
                esc(dim_key),
                esc(v)
            ),
            None => format!(
                "DELETE FROM cmx_flow_subflow_binding WHERE called_key = '{}' AND dim_key = '{}' AND dim_value IS NULL",
                esc(called_key),
                esc(dim_key)
            ),
        };
        execute_sql(&self.db_id, None, &del)
            .await
            .map_err(|e| format!("清理旧绑定失败: {e}"))?;

        // org 维度镜像写 org_id 列（兼容老读路径 / 回滚）；其余维度 org_id 为 NULL。
        let org_mirror = if dim_key == cmx_flow_model::DIM_ORG {
            dim_value
        } else {
            None
        };
        let sql = "INSERT INTO cmx_flow_subflow_binding \
            (id, called_key, dim_key, dim_value, org_id, target_definition_key, enabled, remark, created_at, updated_at) \
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), now())";
        let opt = |o: Option<&str>| match o {
            Some(s) => DataValue::String(s.to_string()),
            None => DataValue::Null,
        };
        let params = SqlParams::DataValues(vec![
            DataValue::String(id.to_string()),
            DataValue::String(called_key.to_string()),
            DataValue::String(dim_key.to_string()),
            opt(dim_value),
            opt(org_mirror),
            DataValue::String(target_definition_key.to_string()),
            DataValue::Bool(enabled),
            opt(remark),
        ]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| format!("写入绑定失败: {e}"))?;
        Ok(())
    }

    /// 删除一条绑定（按 id）。
    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let sql = "DELETE FROM cmx_flow_subflow_binding WHERE id = $1";
        let params = SqlParams::DataValues(vec![DataValue::String(id.to_string())]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| format!("删除绑定失败: {e}"))?;
        Ok(())
    }

    /// 读组织树（全部启用组织，按 path 排序，前端自行建树）。
    pub async fn list_orgs(&self) -> Result<Vec<OrgNode>, String> {
        let sql = "SELECT id, name, parent_id, path FROM cmx_org \
             WHERE archived = 0 ORDER BY path";
        let ds = query_sql(&self.db_id, None, sql, "subflow_org_list")
            .await
            .map_err(|e| format!("查询组织失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let gs = |row: &cmx_core::model::data::dataset::Row, c: &str| -> Option<String> {
            match row.get_by_name(schema, c) {
                Some(DataValue::String(s)) => Some(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
                _ => None,
            }
        };
        Ok(ds
            .iter()
            .map(|row| OrgNode {
                id: gs(row, "id").unwrap_or_default(),
                name: gs(row, "name").unwrap_or_default(),
                parent_id: gs(row, "parent_id"),
                path: gs(row, "path"),
            })
            .collect())
    }

    /// 列某维度字典的条目（RD2，设计器维度条目选择器用）。org 维度走 [`Self::list_orgs`]；
    /// 其余自分级 cf_* 字典按 [`DimSpec`] 直读（接法①：同库 SQL）。表/列名白名单校验防注入。
    /// 返回 [`OrgNode`] 复用其 {id,name,parent_id,path} 结构（name 取 label/name/code 首个可得列）。
    pub async fn list_dim_entries(
        &self,
        spec: &DimSpec,
        name_col: &str,
        parent_col: Option<&str>,
    ) -> Result<Vec<OrgNode>, String> {
        if !is_safe_ident(&spec.table)
            || !is_safe_ident(&spec.id_col)
            || !is_safe_ident(&spec.path_col)
            || !is_safe_ident(name_col)
            || parent_col.map(|p| !is_safe_ident(p)).unwrap_or(false)
        {
            return Err(format!("维度字典 {} 表/列名非法（白名单拒绝）", spec.table));
        }
        let (tbl, idc, pc) = (&spec.table, &spec.id_col, &spec.path_col);
        let parent_sel = match parent_col {
            Some(p) => format!("\"{p}\" AS parent_id"),
            None => "NULL AS parent_id".to_string(),
        };
        let sql = format!(
            "SELECT \"{idc}\" AS id, \"{name_col}\" AS name, {parent_sel}, \"{pc}\" AS path \
             FROM \"{tbl}\" ORDER BY \"{pc}\""
        );
        let ds = query_sql(&self.db_id, None, &sql, "subflow_dim_entries")
            .await
            .map_err(|e| format!("查询维度字典条目失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let gs = |row: &cmx_core::model::data::dataset::Row, c: &str| -> Option<String> {
            match row.get_by_name(schema, c) {
                Some(DataValue::String(s)) => Some(s.clone()),
                Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
                Some(DataValue::Int(n)) => Some(n.to_string()),
                _ => None,
            }
        };
        Ok(ds
            .iter()
            .map(|row| OrgNode {
                id: gs(row, "id").unwrap_or_default(),
                name: gs(row, "name").unwrap_or_default(),
                parent_id: gs(row, "parent_id"),
                path: gs(row, "path"),
            })
            .collect())
    }
}
