/*
 * @Describe: 流程变量设计态 Schema —— 把「运行时随手 set 的动态 KV」升级为「设计态先声明的
 * 类型化契约」。这是分支条件可视化构造器（变量列下拉）、表单字段映射、类型软校验三者的共同地基。
 *
 * 定位澄清（与 variables.rs 的分工）：
 * - `Variables`（variables.rs）：**运行态**的动态值容器（JSON KV），实例真正携带的数据，引擎读写。
 * - `VarSchema`（本模块）：**设计态**的变量声明元数据，随流程定义走（sidecar），引擎运行不依赖它。
 *   它回答「这个流程有哪些变量、什么类型、默认值、从哪来」——设计器据此渲染下拉与做联动。
 *
 * 哲学延续：本模块是纯数据 + 纯函数（声明 / 默认值物化 / 软校验），零 DB、零 IO、可 wasm。
 * 类型系统是**设计态软约束**：不声明也能跑（向后兼容），声明了则用于设计器联动与可选的落库前校验，
 * 绝不改变运行时 `Variables` 仍是 JSON 的事实——引擎侧零破坏。
 */

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::variables::Variables;

/// 变量数据类型（设计态）。对齐分支条件构造器「比较值」控件的输入形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VarType {
    /// 字符串。
    String,
    /// 数值（统一 f64/整型，对齐 JSON number 与 expr 数值语义）。
    Number,
    /// 布尔。
    Boolean,
    /// 日期（ISO 8601 字符串载体；类型标记用于设计器渲染日期选择器）。
    Date,
    /// 枚举（取值受 `enum_options` 约束；载体为字符串）。
    Enum,
    /// 对象（嵌套 JSON；配合 expr 的点号下钻 `order.amount`）。
    Object,
    /// 数组。
    Array,
}

impl VarType {
    /// 类型名（小写，用于错误信息与前端标签键）。
    pub fn as_str(self) -> &'static str {
        match self {
            VarType::String => "string",
            VarType::Number => "number",
            VarType::Boolean => "boolean",
            VarType::Date => "date",
            VarType::Enum => "enum",
            VarType::Object => "object",
            VarType::Array => "array",
        }
    }

    /// 一个 JSON 值是否与本类型相容（软校验用；Null 一律相容，代表「未赋值」）。
    pub fn accepts(self, v: &Value) -> bool {
        match (self, v) {
            (_, Value::Null) => true,
            (VarType::String, Value::String(_)) => true,
            (VarType::Number, Value::Number(_)) => true,
            (VarType::Boolean, Value::Bool(_)) => true,
            // Date/Enum 以字符串为载体。
            (VarType::Date, Value::String(_)) => true,
            (VarType::Enum, Value::String(_)) => true,
            (VarType::Object, Value::Object(_)) => true,
            (VarType::Array, Value::Array(_)) => true,
            _ => false,
        }
    }
}

/// 变量来源（设计态语义标记）：说明这个变量的值预期从哪来，驱动设计器的联动与提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VarSource {
    /// 手工声明（设计者直接定义）。
    Manual,
    /// 由绑定表单的字段提升而来（E1 表单字段映射）。
    FormField,
    /// serviceTask 执行返回写入。
    ServiceReturn,
    /// 流程发起参数（start 时注入）。
    StartParam,
    /// 子流程回传（callActivity out 映射）。
    SubflowReturn,
}

impl Default for VarSource {
    fn default() -> Self {
        VarSource::Manual
    }
}

/// 变量作用域。当前引擎变量仅实例级一层；本枚举为设计态预留任务级作用域的表达力，
/// 运行时暂统一按实例级处理（不声明作用域即实例级），未来引擎补局部作用域时无需改 schema 格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VarScope {
    /// 流程实例级（默认）——全流程可见。
    Instance,
    /// 任务局部级——预留，运行时暂等同实例级。
    Task,
}

impl Default for VarScope {
    fn default() -> Self {
        VarScope::Instance
    }
}

/// 单条变量声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarDecl {
    /// 变量名（表达式里的标识符；可被 `a.b` 点号路径引用其对象字段）。
    pub name: String,
    /// 数据类型。
    #[serde(rename = "type")]
    pub var_type: VarType,
    /// 展示标签（设计器/表单用；缺省回退 name）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 默认值（可空）。物化实例时若变量未提供则注入此值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// 来源标记。
    #[serde(default)]
    pub source: VarSource,
    /// 作用域。
    #[serde(default)]
    pub scope: VarScope,
    /// 是否必填（软校验：物化后仍为 Null 则违规）。
    #[serde(default)]
    pub required: bool,
    /// 枚举取值（仅 `VarType::Enum` 有意义）——设计器渲染下拉、校验取值合法性。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_options: Vec<String>,
}

impl VarDecl {
    /// 便捷构造：一个最小声明（Manual/Instance/非必填/无默认）。
    pub fn new(name: impl Into<String>, var_type: VarType) -> Self {
        Self {
            name: name.into(),
            var_type,
            label: None,
            default: None,
            source: VarSource::default(),
            scope: VarScope::default(),
            required: false,
            enum_options: Vec::new(),
        }
    }

    /// 展示标签（回退到 name）。
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

/// 一条变量违规（软校验产出，结构化便于前端定位到具体变量）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarViolation {
    /// 违规变量名。
    pub var: String,
    /// 违规类型码（机器可读）。
    pub code: VarViolationCode,
    /// 人类可读说明。
    pub message: String,
}

/// 变量违规类型码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VarViolationCode {
    /// 必填但缺失（物化后仍 Null）。
    Required,
    /// 类型不匹配。
    Type,
    /// 枚举取值越界。
    Enum,
}

/// 流程变量 Schema：一个流程定义的全部变量声明。
///
/// 用 `Vec<VarDecl>` 保留声明顺序（设计器列表按声明序展示）；名称唯一性由 `validate_shape` 校验。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VarSchema {
    /// 变量声明列表（有序）。
    pub decls: Vec<VarDecl>,
}

impl VarSchema {
    /// 空 schema。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从声明列表构建。
    pub fn from_decls(decls: Vec<VarDecl>) -> Self {
        Self { decls }
    }

    /// 是否无任何声明。
    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    /// 按名查声明。
    pub fn get(&self, name: &str) -> Option<&VarDecl> {
        self.decls.iter().find(|d| d.name == name)
    }

    /// 校验 schema 自身结构合法性（设计态、与具体实例无关）：
    /// - 变量名非空、无重复；
    /// - `Enum` 类型必须给出至少一个候选，非 Enum 类型不应带候选；
    /// - 默认值（若给）须与声明类型相容、且 Enum 默认须在候选内。
    ///
    /// 返回结构化违规列表（空 = 合法）。这是设计器「保存前校验」变量页签的后端权威。
    pub fn validate_shape(&self) -> Vec<VarViolation> {
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for d in &self.decls {
            if d.name.trim().is_empty() {
                out.push(VarViolation {
                    var: d.name.clone(),
                    code: VarViolationCode::Type,
                    message: "变量名不能为空".into(),
                });
                continue;
            }
            if !seen.insert(d.name.clone()) {
                out.push(VarViolation {
                    var: d.name.clone(),
                    code: VarViolationCode::Type,
                    message: format!("变量名 '{}' 重复声明", d.name),
                });
            }
            match d.var_type {
                VarType::Enum if d.enum_options.is_empty() => out.push(VarViolation {
                    var: d.name.clone(),
                    code: VarViolationCode::Enum,
                    message: format!("枚举变量 '{}' 未给出候选值", d.name),
                }),
                VarType::Enum => {}
                _ if !d.enum_options.is_empty() => out.push(VarViolation {
                    var: d.name.clone(),
                    code: VarViolationCode::Type,
                    message: format!(
                        "非枚举变量 '{}' 不应带候选值（type={}）",
                        d.name,
                        d.var_type.as_str()
                    ),
                }),
                _ => {}
            }
            if let Some(def) = &d.default {
                if !d.var_type.accepts(def) {
                    out.push(VarViolation {
                        var: d.name.clone(),
                        code: VarViolationCode::Type,
                        message: format!(
                            "变量 '{}' 默认值类型与声明 {} 不符",
                            d.name,
                            d.var_type.as_str()
                        ),
                    });
                }
                if d.var_type == VarType::Enum {
                    if let Value::String(s) = def {
                        if !d.enum_options.contains(s) {
                            out.push(VarViolation {
                                var: d.name.clone(),
                                code: VarViolationCode::Enum,
                                message: format!("变量 '{}' 默认值 '{s}' 不在候选内", d.name),
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// 用声明的默认值物化一组实例变量：对每条有 `default` 且实例中缺失（未设或为 Null）的声明，
    /// 注入默认值。已有值不覆盖。返回补齐后的新 `Variables`（不改入参）。
    ///
    /// 用于 start_process 时按 schema 补默认——但**保持可选**：引擎可调可不调，不调即旧行为。
    pub fn materialize_defaults(&self, vars: &Variables) -> Variables {
        let mut out = vars.clone();
        for d in &self.decls {
            if let Some(def) = &d.default {
                let missing = matches!(out.get(&d.name), None | Some(Value::Null));
                if missing {
                    out.set(d.name.clone(), def.clone());
                }
            }
        }
        out
    }

    /// 对一组实例变量做软校验（必填 / 类型 / 枚举）。只校验**已声明**的变量；
    /// 未声明的变量一律放行（向后兼容，schema 不是白名单）。返回结构化违规列表。
    pub fn validate_values(&self, vars: &Variables) -> Vec<VarViolation> {
        let mut out = Vec::new();
        for d in &self.decls {
            let val = vars.get(&d.name);
            match val {
                None | Some(Value::Null) => {
                    if d.required {
                        out.push(VarViolation {
                            var: d.name.clone(),
                            code: VarViolationCode::Required,
                            message: format!("必填变量 '{}' 缺失", d.name),
                        });
                    }
                }
                Some(v) => {
                    if !d.var_type.accepts(v) {
                        out.push(VarViolation {
                            var: d.name.clone(),
                            code: VarViolationCode::Type,
                            message: format!(
                                "变量 '{}' 期望 {}，实际不符",
                                d.name,
                                d.var_type.as_str()
                            ),
                        });
                    }
                    if d.var_type == VarType::Enum {
                        if let Value::String(s) = v {
                            if !d.enum_options.contains(s) {
                                out.push(VarViolation {
                                    var: d.name.clone(),
                                    code: VarViolationCode::Enum,
                                    message: format!("变量 '{}' 取值 '{s}' 不在候选内", d.name),
                                });
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> VarSchema {
        VarSchema::from_decls(vec![
            {
                let mut d = VarDecl::new("amount", VarType::Number);
                d.required = true;
                d
            },
            {
                let mut d = VarDecl::new("region", VarType::Enum);
                d.enum_options = vec!["north".into(), "south".into()];
                d.default = Some(json!("north"));
                d
            },
            {
                let mut d = VarDecl::new("remark", VarType::String);
                d.default = Some(json!("n/a"));
                d
            },
        ])
    }

    #[test]
    fn shape_ok() {
        assert!(schema().validate_shape().is_empty());
    }

    #[test]
    fn shape_rejects_duplicate_and_bad_enum() {
        let s = VarSchema::from_decls(vec![
            VarDecl::new("x", VarType::Number),
            VarDecl::new("x", VarType::String), // 重复
            VarDecl::new("e", VarType::Enum),   // 枚举无候选
        ]);
        let v = s.validate_shape();
        assert!(v.iter().any(|x| x.message.contains("重复")));
        assert!(v.iter().any(|x| x.code == VarViolationCode::Enum));
    }

    #[test]
    fn shape_rejects_default_type_mismatch() {
        let mut d = VarDecl::new("n", VarType::Number);
        d.default = Some(json!("not-a-number"));
        let s = VarSchema::from_decls(vec![d]);
        let v = s.validate_shape();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, VarViolationCode::Type);
    }

    #[test]
    fn materialize_fills_missing_only() {
        let mut vars = Variables::new();
        vars.set("region", json!("south")); // 已有值不应被默认覆盖
        let out = schema().materialize_defaults(&vars);
        assert_eq!(out.get("region"), Some(&json!("south"))); // 保留
        assert_eq!(out.get("remark"), Some(&json!("n/a"))); // 补默认
        assert_eq!(out.get("amount"), None); // 无默认，不补
    }

    #[test]
    fn validate_values_required_and_enum_and_type() {
        let s = schema();
        // amount 必填缺失 + region 取值越界 + remark 类型错。
        let mut vars = Variables::new();
        vars.set("region", json!("west"));
        vars.set("remark", json!(123));
        let v = s.validate_values(&vars);
        assert!(v.iter().any(|x| x.var == "amount" && x.code == VarViolationCode::Required));
        assert!(v.iter().any(|x| x.var == "region" && x.code == VarViolationCode::Enum));
        assert!(v.iter().any(|x| x.var == "remark" && x.code == VarViolationCode::Type));
    }

    #[test]
    fn validate_values_passes_clean_and_ignores_undeclared() {
        let s = schema();
        let mut vars = Variables::new();
        vars.set("amount", json!(1000));
        vars.set("region", json!("north"));
        vars.set("undeclared_extra", json!("free")); // 未声明 → 放行
        assert!(s.validate_values(&vars).is_empty());
    }

    #[test]
    fn schema_json_roundtrip() {
        let s = schema();
        let j = serde_json::to_value(&s).unwrap();
        let back: VarSchema = serde_json::from_value(j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn type_accepts_null_always() {
        // Null 代表未赋值，任何类型相容——软校验不因未赋值误报类型错。
        assert!(VarType::Number.accepts(&Value::Null));
        assert!(VarType::Object.accepts(&Value::Null));
    }
}
