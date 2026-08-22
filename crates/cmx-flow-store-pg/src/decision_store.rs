/*
 * @Describe: PgDecisionStore —— 决策表（businessRuleTask 引用）的 PostgreSQL 持久化。
 *
 * 决策表原只存引擎内存注册表（POST /decisions 热注册，进程重启即丢、集群实例间不一致）。本模块把
 * 决策表随注册**落库**（cmx_flow_decision，flow 租户库），引擎启动时 load_all → register_decision
 * 逐张装载，使决策表跨重启/多实例一致——即路线图「决策表发布落库持久化」。
 *
 * 存储 = 整表 JSON（TEXT 列，读回 serde 反序列化，键序无关）+ 冗余元数据列（hit_policy/规则数/输入数/
 * 更新时间/更新人）供列表端点免解析 JSON。与 PgSubflowBindingStore 同构：自带幂等 ensure_schema，
 * 走 cmx-database-pg 的 execute_sql/query_sql；生产库不由引擎 ensure_schema 覆盖，故管理面自带 DDL 兜底。
 */

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};
use cmx_flow_model::DecisionTable;

/// 决策表元数据（列表端点用，不含整表规则明细）。
#[derive(Debug, Clone)]
pub struct DecisionMeta {
    pub key: String,
    pub hit_policy: String,
    pub rule_count: i64,
    pub input_count: i64,
    pub updated_at: String,
    pub updated_by: Option<String>,
}

/// 决策表 PG 存储。持 flow（租户）库 db_id（须已在 cmx-database-pg 注册数据源）。
pub struct PgDecisionStore {
    db_id: String,
}

/// 从字符串类 DataValue 取出 String（String/Json/ShortStr/LongStr 皆可）。
fn as_str(v: Option<&DataValue>) -> Option<String> {
    match v {
        Some(DataValue::String(s)) | Some(DataValue::Json(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn as_i64(v: Option<&DataValue>) -> i64 {
    match v {
        Some(DataValue::Int(i)) => *i,
        _ => 0,
    }
}

impl PgDecisionStore {
    /// 用指定 db_id 构建（同 PgDefinitionStore 用 flow 库——决策表是设计态产物，与定义同库同租户）。
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。
    pub async fn ensure_schema(&self) -> Result<(), String> {
        let ddl = "CREATE TABLE IF NOT EXISTS cmx_flow_decision (\
            key VARCHAR(128) PRIMARY KEY, \
            table_json TEXT NOT NULL, \
            hit_policy VARCHAR(32), \
            rule_count INT NOT NULL DEFAULT 0, \
            input_count INT NOT NULL DEFAULT 0, \
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
            updated_by VARCHAR(128))";
        execute_sql(&self.db_id, None, ddl)
            .await
            .map_err(|e| format!("建决策表失败: {e}"))?;
        Ok(())
    }

    /// upsert 一张决策表（随注册落库；同 key 覆盖）。整表序列化进 table_json，元数据冗余进列。
    pub async fn upsert(
        &self,
        table: &DecisionTable,
        updated_by: Option<&str>,
    ) -> Result<(), String> {
        let json = serde_json::to_string(table).map_err(|e| format!("决策表序列化失败: {e}"))?;
        // hit_policy 序列化为 "FIRST"/"COLLECT"（SCREAMING_SNAKE，见 HitPolicy serde）。
        let hit = serde_json::to_value(table.hit_policy)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "FIRST".into());
        // rule_count/input_count 为内部计算的整数，直接内联（非用户串，无注入面）。
        let sql = format!(
            "INSERT INTO cmx_flow_decision \
               (key, table_json, hit_policy, rule_count, input_count, updated_at, updated_by) \
             VALUES ($1, $2, $3, {}, {}, now(), $4) \
             ON CONFLICT (key) DO UPDATE SET \
               table_json = EXCLUDED.table_json, hit_policy = EXCLUDED.hit_policy, \
               rule_count = EXCLUDED.rule_count, input_count = EXCLUDED.input_count, \
               updated_at = now(), updated_by = EXCLUDED.updated_by",
            table.rules.len(),
            table.inputs.len()
        );
        let params = SqlParams::DataValues(vec![
            DataValue::String(table.key.clone()),
            DataValue::String(json),
            DataValue::String(hit),
            match updated_by {
                Some(u) => DataValue::String(u.to_string()),
                None => DataValue::Null,
            },
        ]);
        execute_sql_with_params(&self.db_id, None, &sql, params)
            .await
            .map_err(|e| format!("写入决策表失败: {e}"))?;
        Ok(())
    }

    /// 装载全部决策表（引擎启动时逐张 register_decision）。返回 (成功表, 解析失败[(key,err)])。
    pub async fn load_all(&self) -> Result<(Vec<DecisionTable>, Vec<(String, String)>), String> {
        let sql = "SELECT key, table_json FROM cmx_flow_decision ORDER BY key";
        let ds = query_sql(&self.db_id, None, sql, "decision_load_all")
            .await
            .map_err(|e| format!("查询决策表失败: {e}"))?;
        let schema = ds.schema.as_ref();
        let mut ok = Vec::with_capacity(ds.row_count());
        let mut errs = Vec::new();
        for row in ds.iter() {
            let key = as_str(row.get_by_name(schema, "key")).unwrap_or_default();
            let js = as_str(row.get_by_name(schema, "table_json")).unwrap_or_default();
            match serde_json::from_str::<DecisionTable>(&js) {
                Ok(t) => ok.push(t),
                Err(e) => errs.push((key, e.to_string())),
            }
        }
        Ok((ok, errs))
    }

    /// 列决策表元数据（设计器/运维列表用；不返回整表规则）。
    pub async fn list_meta(&self) -> Result<Vec<DecisionMeta>, String> {
        let sql = "SELECT key, hit_policy, rule_count, input_count, updated_at, updated_by \
                   FROM cmx_flow_decision ORDER BY key";
        let ds = query_sql(&self.db_id, None, sql, "decision_list_meta")
            .await
            .map_err(|e| format!("查询决策表列表失败: {e}"))?;
        let schema = ds.schema.as_ref();
        Ok(ds
            .iter()
            .map(|row| DecisionMeta {
                key: as_str(row.get_by_name(schema, "key")).unwrap_or_default(),
                hit_policy: as_str(row.get_by_name(schema, "hit_policy"))
                    .unwrap_or_else(|| "FIRST".into()),
                rule_count: as_i64(row.get_by_name(schema, "rule_count")),
                input_count: as_i64(row.get_by_name(schema, "input_count")),
                updated_at: match row.get_by_name(schema, "updated_at") {
                    Some(DataValue::DateTime(dt)) => dt.to_rfc3339(),
                    other => as_str(other).unwrap_or_default(),
                },
                updated_by: as_str(row.get_by_name(schema, "updated_by")),
            })
            .collect())
    }

    /// 删除一张决策表（按 key）。
    pub async fn delete(&self, key: &str) -> Result<(), String> {
        let sql = "DELETE FROM cmx_flow_decision WHERE key = $1";
        let params = SqlParams::DataValues(vec![DataValue::String(key.to_string())]);
        execute_sql_with_params(&self.db_id, None, sql, params)
            .await
            .map_err(|e| format!("删除决策表失败: {e}"))?;
        Ok(())
    }
}
