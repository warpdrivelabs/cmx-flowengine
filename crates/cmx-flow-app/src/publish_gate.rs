//! 发布闸（强校验改造 D2）：声明了必填变量（`required`）的流程定义，发布 / 激活时
//! 必须配 `strict` 校验策略——否则 lenient 仅告警、off 直接跳过，required 形同虚设。
//!
//! 落点（两个入口共用本模块的纯逻辑，见 `handlers.rs`）：
//! - `definitions/{key}/publish`：在 `def_svc.publish` **落库之前**先编译草稿并过闸；
//! - `definitions/{key}/versions/{version}/activate`：热装载历史版本前 compile → 过闸。
//!
//! 逃生门：env `FLOW_PUBLISH_STRICT_REQUIRED=off` 显式关闭；否则 ConfigManager
//! `flow.enforce_strict_required_on_publish`（缺省 true = 常开）。

use cmx_flow_model::var_schema::VarSchema;

/// 纯逻辑：schema 含任一 `required = true` 声明时必须为 strict，否则报错（文案即出路）。
/// 无 schema / 空 schema / 无 required 声明的定义零影响。
pub fn check_required_needs_strict(
    schema: Option<&VarSchema>,
    var_validation: Option<&str>,
) -> Result<(), String> {
    let Some(schema) = schema else {
        return Ok(());
    };
    if schema.is_empty() || !schema.decls.iter().any(|d| d.required) {
        return Ok(());
    }
    // 与引擎发起边界同款默认口径：未配置按 lenient。
    let policy = var_validation
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("lenient");
    if policy.eq_ignore_ascii_case("strict") {
        return Ok(());
    }
    Err(format!(
        "流程声明了 required 变量但校验策略为 `{policy}`——漏传必填变量会被放行（lenient 仅告警）或跳过校验（off）。\
         请在该流程 BPMN 的 process 元素上设置 cmx:varValidation=\"strict\" 后重新发布；\
         确需宽松策略请先编辑草稿去掉对应变量的 required 声明"
    ))
}

/// 是否启用发布闸。env 显式关闭优先于配置文件；未初始化 ConfigManager（单测 / 独立组件）
/// 也回落 true——与引擎 lenient 默认不同向：闸管的是"契约自洽"，缺配置应从紧。
pub fn gate_enabled() -> bool {
    if let Ok(v) = std::env::var("FLOW_PUBLISH_STRICT_REQUIRED") {
        let v = v.trim();
        if v.eq_ignore_ascii_case("off")
            || v.eq_ignore_ascii_case("false")
            || v == "0"
        {
            return false;
        }
    }
    cmx_utils::ConfigManager::try_global()
        .and_then(|cm| cm.get_string("flow.enforce_strict_required_on_publish").ok())
        .map(|v| !matches!(v.trim(), "off" | "false" | "0"))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmx_flow_model::var_schema::{VarDecl, VarType};

    fn decl(name: &str, required: bool) -> VarDecl {
        let mut d = VarDecl::new(name, VarType::String);
        d.required = required;
        d
    }

    #[test]
    fn no_schema_passes() {
        assert!(check_required_needs_strict(None, None).is_ok());
    }

    #[test]
    fn empty_schema_and_no_required_pass() {
        let empty = VarSchema::new();
        assert!(check_required_needs_strict(Some(&empty), Some("lenient")).is_ok());

        let opt_schema = VarSchema::from_decls(vec![decl("docType", false)]);
        assert!(check_required_needs_strict(Some(&opt_schema), None).is_ok());
    }

    #[test]
    fn required_with_strict_passes() {
        let schema = VarSchema::from_decls(vec![decl("initiator", true)]);
        assert!(check_required_needs_strict(Some(&schema), Some("strict")).is_ok());
        assert!(check_required_needs_strict(Some(&schema), Some("STRICT")).is_ok());
    }

    #[test]
    fn required_without_validation_defaults_to_lenient_and_rejects_with_guidance() {
        let schema = VarSchema::from_decls(vec![decl("initiator", true)]);
        for policy in [None, Some(""), Some("lenient"), Some("off")] {
            let err = check_required_needs_strict(Some(&schema), policy)
                .expect_err("闸应拒绝");
            assert!(err.contains("cmx:varValidation=\"strict\""), "文案须带出路: {err}");
            assert!(err.contains(policy.unwrap_or("lenient")), "文案须含现行策略: {err}");
        }
    }

    #[test]
    fn gate_enabled_defaults_true_without_config() {
        // 未设置逃生门 env 且无全局 ConfigManager → 常开
        // （本 crate 不存在写 FLOW_PUBLISH_STRICT_REQUIRED 的路径，无串扰担忧）。
        assert!(gate_enabled());
    }
}
