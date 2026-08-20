/*
 * @Describe: 实例迁移（A9）—— 把运行中实例迁到另一个流程定义版本。
 *
 * 最小版：节点映射迁移。给定 (源实例, 目标定义, 活动节点映射)，把实例每个活动/等待令牌的
 * 当前节点按映射表重定位到目标定义的对应节点，并把实例的 definition_key 指向目标。
 * 校验先行：所有活动令牌所在节点须有映射，映射目标须在目标定义中存在，目标定义须已部署。
 *
 * 不改核心推进循环——迁移只重写令牌的 node_bpmn_id（稳定锚点）+ 实例定义指向，
 * 迁移后实例照常经 run_to_wait 在新定义上推进。
 */

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 迁移计划：把源实例迁到目标定义，按节点映射重定位令牌。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// 目标流程定义 key（须已部署）。
    pub target_definition_key: String,
    /// 活动节点映射：`源节点 bpmn_id → 目标节点 bpmn_id`。
    /// 只需覆盖实例当前所有活动/等待令牌所在的节点；已结束令牌无需映射。
    pub activity_mappings: BTreeMap<String, String>,
}

/// 一条迁移违规（校验产出，结构化便于前端定位）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationViolation {
    /// 违规类型码。
    pub code: MigrationViolationCode,
    /// 涉及的节点 bpmn_id（若适用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_bpmn_id: Option<String>,
    /// 人类可读说明。
    pub message: String,
}

/// 迁移违规类型码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MigrationViolationCode {
    /// 目标定义未部署。
    TargetNotDeployed,
    /// 某活动令牌所在节点缺映射。
    UnmappedActivity,
    /// 映射的目标节点在目标定义中不存在。
    TargetNodeMissing,
    /// 实例状态不允许迁移（非 Active）。
    InstanceNotActive,
}

/// 迁移校验结果：违规为空即可迁移（干运行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationValidation {
    /// 是否可迁移（violations 为空）。
    pub ok: bool,
    /// 违规明细。
    pub violations: Vec<MigrationViolation>,
}
