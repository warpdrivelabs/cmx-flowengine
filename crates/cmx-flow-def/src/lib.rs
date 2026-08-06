/*
 * @Describe: cmx-flow-def —— 流程定义持久化层（设计器 ⇄ 引擎之间缺失的一环）。
 *
 * 职责：存 BPMN XML + 版本 + 草稿/发布状态，让设计器画完能存、引擎能取、变更能版本化。
 *   - save_draft(key, xml)：先试编译（compile）挡回非法 XML → 存草稿（覆盖当前草稿版）。
 *   - publish(key)：草稿 → 发布，version+1，追加不可变版本行，标记为 active。
 *   - list / get / get_version：列表、取当前发布版 XML、取历史版本。
 *   - load_published()：引擎启动时取所有已发布定义（key + 最新 xml），逐个 compile+deploy。
 *
 * 为什么存 XML 而非编译后 IR：XML 是标准交换格式，可重新加载编辑、人工审阅、版本 diff、
 * 导出迁移；IR 随代码演进。存 XML + 装载时 compile，既保留可编辑性又与引擎同步。定义数量级
 * 极小（几十上百），每次装载编译一次可忽略。
 *
 * 分层：本 crate = model（类型 + Store trait + 校验）；PG 实现见 store 模块（同 crate，接
 * cmx-database-pg）。service 模块编排 save/publish/list 的业务规则（试编译、版本推进）。
 */

pub mod service;
pub mod store;

pub use service::DefinitionService;
pub use store::PgDefinitionStore;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 定义状态：草稿态 / 已发布态。
///
/// 一个定义（按 key）在库里始终有一行主记录，state 表示它当前是纯草稿还是已有发布版本。
/// 已发布后仍可再存草稿（改动积累在草稿区，不影响 active 版本），下次 publish 再推进版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DefinitionState {
    /// 草稿：从未发布过，或有未发布的改动。
    Draft,
    /// 已发布：至少有一个发布版本，active_version 指向它。
    Published,
}

impl DefinitionState {
    /// 存库用的字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            DefinitionState::Draft => "DRAFT",
            DefinitionState::Published => "PUBLISHED",
        }
    }

    /// 从库字符串解析（未知值兜底为 Draft，避免 load 报错）。
    pub fn parse_str(s: &str) -> Self {
        match s {
            "PUBLISHED" => DefinitionState::Published,
            _ => DefinitionState::Draft,
        }
    }
}

/// 定义主记录（当前指针）：一个流程定义在库里的当前状态。
///
/// key 是流程逻辑标识（同 BPMN process id / ProcessDefinition.key）。draft_xml 是当前草稿的
/// BPMN；active_version 指向已发布的版本号（未发布时为 None）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionRecord {
    /// 流程定义 key（主键；= BPMN process id）。
    pub key: String,
    /// 展示名。
    pub name: String,
    /// 所属域（DAM 三段之一，可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// 所属应用（DAM 三段之一，可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
    /// 所属模块（DAM 三段之一，可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// 分类（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// 状态。
    pub state: DefinitionState,
    /// 当前已发布的版本号（未发布为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<i32>,
    /// 当前草稿的 BPMN XML（列表查询时可不带，用 None 省流量）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_xml: Option<String>,
    /// 最近更新时间。
    pub updated_at: DateTime<Utc>,
    /// 最近更新人（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
}

/// 版本记录（不可变历史）：每次 publish 追加一行，存当时的 BPMN 快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionRecord {
    /// 版本行 id（UUID）。
    pub id: String,
    /// 所属定义 key。
    pub def_key: String,
    /// 版本号（同 def_key 下从 1 递增）。
    pub version: i32,
    /// 该版本的 BPMN XML（发布时的草稿快照）。
    pub bpmn_xml: String,
    /// 变更说明（发布时填写，可空；对标报表版本的 change_summary）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 发布时间。
    pub published_at: DateTime<Utc>,
    /// 发布人（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
}

/// 版本元信息（不含 BPMN XML，列表/管理用，省流量）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMeta {
    /// 所属定义 key。
    pub def_key: String,
    /// 版本号。
    pub version: i32,
    /// 变更说明（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 发布时间。
    pub published_at: DateTime<Utc>,
    /// 发布人（可空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
}

/// 已发布定义的装载条目（引擎启动装载用）：key + 最新发布版 XML。
#[derive(Debug, Clone)]
pub struct PublishedEntry {
    /// 定义 key。
    pub key: String,
    /// 最新发布版本号。
    pub version: i32,
    /// 该版本 BPMN XML。
    pub bpmn_xml: String,
}

/// 定义层错误。
#[derive(thiserror::Error, Debug)]
pub enum DefError {
    /// 目标定义不存在。
    #[error("流程定义不存在: {0}")]
    NotFound(String),

    /// 没有可发布的草稿（未存过草稿就 publish）。
    #[error("无草稿可发布: {0}")]
    NoDraft(String),

    /// BPMN 校验失败（save/publish 前试编译不过）。携带可读诊断。
    #[error("BPMN 校验失败: {0}")]
    Invalid(String),

    /// 操作与当前状态冲突（如删除当前生效版本）。携带可读原因。
    #[error("{0}")]
    Conflict(String),

    /// 底层存储错误（DB/IO）。
    #[error("存储层错误: {0}")]
    Backend(String),
}

/// 本 crate 统一 Result。
pub type DefResult<T> = core::result::Result<T, DefError>;

/// 定义存储契约（model 定义，PG 实现见 store 模块）。
///
/// 只管持久化的原子读写；业务规则（试编译、版本推进）在 service 层。
#[async_trait::async_trait]
pub trait DefinitionStore: Send + Sync {
    /// 建表（幂等）。
    async fn ensure_schema(&self) -> DefResult<()>;

    /// upsert 主记录的草稿（存在则更新 draft_xml/name/updated_*，不存在则插入 DRAFT 行）。
    async fn upsert_draft(&self, rec: &DefinitionRecord) -> DefResult<()>;

    /// 取主记录（含 draft_xml）；不存在返回 None。
    async fn get(&self, key: &str) -> DefResult<Option<DefinitionRecord>>;

    /// 列表（不带 draft_xml，省流量）。
    async fn list(&self) -> DefResult<Vec<DefinitionRecord>>;

    /// 追加一个版本行。
    async fn insert_version(&self, ver: &VersionRecord) -> DefResult<()>;

    /// 取某定义的当前最大版本号（无版本返回 0）。
    async fn max_version(&self, def_key: &str) -> DefResult<i32>;

    /// 取指定版本的 XML；不存在返回 None。
    async fn get_version(&self, def_key: &str, version: i32) -> DefResult<Option<VersionRecord>>;

    /// 列某定义的全部版本元信息（不含 XML，版本号降序）。
    async fn list_versions(&self, def_key: &str) -> DefResult<Vec<VersionMeta>>;

    /// 列全部定义的版本元信息（一次查询，handler 按 def_key 分组，省 N+1）。
    async fn list_all_versions(&self) -> DefResult<Vec<VersionMeta>>;

    /// 删除某个历史版本行（调用方须保证不是当前 active 版本）。
    async fn delete_version(&self, def_key: &str, version: i32) -> DefResult<()>;

    /// 把主记录标记为已发布，active_version 指向新版本。
    async fn mark_published(&self, key: &str, version: i32) -> DefResult<()>;

    /// 取所有已发布定义的最新版本（引擎启动装载用）。
    async fn load_published(&self) -> DefResult<Vec<PublishedEntry>>;
}

/// 试编译校验：把 BPMN XML 跑一遍编译器，成功返回编译出的定义 key，失败返回可读诊断。
///
/// service 的 save_draft/publish 都先过这道闸——非法 XML 根本存不进库。返回的 key 用于
/// 校验「BPMN process id 与传入的 key 一致」。
pub fn validate_bpmn(xml: &str) -> DefResult<String> {
    match cmx_flow_bpmn::compile(xml) {
        Ok(def) => Ok(def.key),
        Err(e) => Err(DefError::Invalid(e.to_string())),
    }
}
