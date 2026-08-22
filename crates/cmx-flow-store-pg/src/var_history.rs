/*
 * @Describe: PgVarHistoryStore —— 流程实例变量变更历史 + TTL 归档（PostgreSQL）。
 *
 * 记录调用方对实例变量的每次变更（谁、何时、在哪个节点、把哪个变量从什么改成什么、经由哪条路径），
 * 供审批审计「某变量为何变成这个值」回溯。捕获点在 app 层「调用方送入变量」的三条路径：start(发起初值)
 * / complete(办理携带) / set-variables(显式改量)——引擎内部派生(决策输出/子流程回填)为后续增强。
 *
 * TTL 归档：sweep_older_than_days(n) 删除 n 天前的历史（可经端点手动触发，或后台定时清理）。
 * 与 PgDecisionStore 同构：flow 租户库、幂等 ensure_schema、走 cmx-database-pg。
 */

use cmx_core::model::cell::DataValue;
use cmx_database_pg::{SqlParams, execute_sql, execute_sql_with_params, query_sql};

/// 一条变量变更记录（app 层 diff 后构造）。
#[derive(Debug, Clone)]
pub struct VarChange {
    pub instance_id: String,
    pub var_name: String,
    /// 变更前值（JSON 文本；无则 None）。
    pub old_value: Option<String>,
    /// 变更后值（JSON 文本；删除则 None）。
    pub new_value: Option<String>,
    /// 来源路径：start | complete | set-variables。
    pub source: String,
    /// 发生时所在节点 bpmnId（可空，实例级改量可为 None）。
    pub node_bpmn_id: Option<String>,
    /// 变更人（认证用户/办理人；可空）。
    pub changed_by: Option<String>,
}

/// 一条历史记录（读出，含时间）。
#[derive(Debug, Clone)]
pub struct VarHistoryEntry {
    pub var_name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub source: String,
    pub node_bpmn_id: Option<String>,
    pub changed_by: Option<String>,
    pub changed_at: String,
}

/// 变量历史 PG 存储。持 flow（租户）库 db_id。
pub struct PgVarHistoryStore {
    db_id: String,
}

fn as_str(v: Option<&DataValue>) -> Option<String> {
    match v {
        Some(DataValue::String(s)) | Some(DataValue::Json(s)) => Some(s.clone()),
        Some(DataValue::ShortStr(s)) | Some(DataValue::LongStr(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn opt(v: &Option<String>) -> DataValue {
    match v {
        Some(s) => DataValue::String(s.clone()),
        None => DataValue::Null,
    }
}

impl PgVarHistoryStore {
    pub fn new(db_id: impl Into<String>) -> Self {
        Self {
            db_id: db_id.into(),
        }
    }

    /// 建表（幂等）。
    pub async fn ensure_schema(&self) -> Result<(), String> {
        let stmts = [
            "CREATE TABLE IF NOT EXISTS cmx_flow_var_history (\
                id BIGSERIAL PRIMARY KEY, \
                instance_id VARCHAR(64) NOT NULL, \
                var_name VARCHAR(128) NOT NULL, \
                old_value TEXT, new_value TEXT, \
                source VARCHAR(32) NOT NULL, \
                node_bpmn_id VARCHAR(128), \
                changed_by VARCHAR(128), \
                changed_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            "CREATE INDEX IF NOT EXISTS idx_cmx_flow_var_history_instance ON cmx_flow_var_history (instance_id, changed_at)",
            "CREATE INDEX IF NOT EXISTS idx_cmx_flow_var_history_changed_at ON cmx_flow_var_history (changed_at)",
        ];
        for ddl in stmts {
            execute_sql(&self.db_id, None, ddl)
                .await
                .map_err(|e| format!("建变量历史表失败: {e}"))?;
        }
        Ok(())
    }

    /// 批量记录变更（一次变量写入的若干 key）。空则跳过。
    pub async fn record(&self, changes: &[VarChange]) -> Result<(), String> {
        for c in changes {
            let sql = "INSERT INTO cmx_flow_var_history \
                (instance_id, var_name, old_value, new_value, source, node_bpmn_id, changed_by, changed_at) \
                VALUES ($1, $2, $3, $4, $5, $6, $7, now())";
            let params = SqlParams::DataValues(vec![
                DataValue::String(c.instance_id.clone()),
                DataValue::String(c.var_name.clone()),
                opt(&c.old_value),
                opt(&c.new_value),
                DataValue::String(c.source.clone()),
                opt(&c.node_bpmn_id),
                opt(&c.changed_by),
            ]);
            execute_sql_with_params(&self.db_id, None, sql, params)
                .await
                .map_err(|e| format!("写入变量历史失败: {e}"))?;
        }
        Ok(())
    }

    /// 列某实例的变量变更历史（时间正序；limit 截断）。
    pub async fn list_by_instance(
        &self,
        instance_id: &str,
        limit: usize,
    ) -> Result<Vec<VarHistoryEntry>, String> {
        // query_sql 无参数化版；instance_id 单引号转义防注入，limit 为整数内联安全。
        let sql = format!(
            "SELECT var_name, old_value, new_value, source, node_bpmn_id, changed_by, changed_at \
             FROM cmx_flow_var_history WHERE instance_id = '{}' ORDER BY changed_at, id LIMIT {}",
            instance_id.replace('\'', "''"),
            limit
        );
        let ds = query_sql(&self.db_id, None, &sql, "var_history_list")
            .await
            .map_err(|e| format!("查询变量历史失败: {e}"))?;
        let schema = ds.schema.as_ref();
        Ok(ds
            .iter()
            .map(|row| VarHistoryEntry {
                var_name: as_str(row.get_by_name(schema, "var_name")).unwrap_or_default(),
                old_value: as_str(row.get_by_name(schema, "old_value")),
                new_value: as_str(row.get_by_name(schema, "new_value")),
                source: as_str(row.get_by_name(schema, "source")).unwrap_or_default(),
                node_bpmn_id: as_str(row.get_by_name(schema, "node_bpmn_id")),
                changed_by: as_str(row.get_by_name(schema, "changed_by")),
                changed_at: match row.get_by_name(schema, "changed_at") {
                    Some(DataValue::DateTime(dt)) => dt.to_rfc3339(),
                    other => as_str(other).unwrap_or_default(),
                },
            })
            .collect())
    }

    /// TTL 归档：删除 `days` 天前的历史，返回删除行数。
    pub async fn sweep_older_than_days(&self, days: i64) -> Result<u64, String> {
        let sql = format!(
            "DELETE FROM cmx_flow_var_history WHERE changed_at < now() - make_interval(days => {})",
            days.max(0)
        );
        let n = execute_sql(&self.db_id, None, &sql)
            .await
            .map_err(|e| format!("清理变量历史失败: {e}"))?;
        Ok(n)
    }
}
