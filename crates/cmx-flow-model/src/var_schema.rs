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
#[serde(rename_all = "camelCase")]
pub struct VarDecl {
    /// 变量名（表达式里的标识符；可被 `a.b` 点号路径引用其对象字段）。
    pub name: String,
    /// 数据类型。
    #[serde(rename = "type")]
    pub var_type: VarType,
    /// 展示标签（设计器/表单用；缺省回退 name）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 说明（设计器 tooltip / 文档；业务含义描述）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// 对象字段结构（仅 `VarType::Object` 有意义）：递归声明每个字段的形状。
    /// 空 = 未声明字段（退化为顶层 OBJECT 标记，向后兼容）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<VarDecl>,
    /// 数组元素结构（仅 `VarType::Array` 有意义）：描述数组每个元素的形状。
    /// 元素是对象 → `item.var_type==Object` + `item.fields`；标量 → `item.var_type==Number` 等。
    /// None = 未声明元素形状（退化为顶层 ARRAY 标记）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Box<VarDecl>>,
}

impl VarDecl {
    /// 便捷构造：一个最小声明（Manual/Instance/非必填/无默认）。
    pub fn new(name: impl Into<String>, var_type: VarType) -> Self {
        Self {
            name: name.into(),
            var_type,
            label: None,
            description: None,
            default: None,
            source: VarSource::default(),
            scope: VarScope::default(),
            required: false,
            enum_options: Vec::new(),
            fields: Vec::new(),
            item: None,
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

/// 摊平后的一条变量路径（设计器下拉的数据源）。对象字段用 `a.b` 点号，数组元素字段用 `a[].b`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarPath {
    /// 点路径（`amount` / `order.customer.level` / `products[].ownerUser`）。
    pub path: String,
    /// 叶子/节点类型。
    #[serde(rename = "type")]
    pub var_type: VarType,
    /// 展示标签（回退 name）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 是否必填（仅顶层声明有意义；子字段沿用其 required）。
    #[serde(default)]
    pub required: bool,
    /// 是否为数组（供多实例 collection 下拉过滤）。
    #[serde(default)]
    pub is_collection: bool,
    /// 枚举候选（Enum 类型才非空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_options: Vec<String>,
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
    /// - 变量名非空、无重复（同层）；
    /// - `Enum` 类型必须给出至少一个候选，非 Enum 类型不应带候选；
    /// - 默认值（若给）须与声明类型相容、且 Enum 默认须在候选内；
    /// - 递归校验对象字段（`fields`）与数组元素（`item`）。
    ///
    /// 返回结构化违规列表（空 = 合法）。这是设计器「保存前校验」变量页签的后端权威。
    pub fn validate_shape(&self) -> Vec<VarViolation> {
        let mut out = Vec::new();
        check_decls(&self.decls, "", &mut out);
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

    /// 摊平成点路径列表（设计器下拉的唯一数据源）：顶层变量 + 递归对象字段（`a.b`）+
    /// 数组元素字段（`a[].b`）。深度上限防御异常深嵌套（正常 schema 远达不到）。
    pub fn flatten_paths(&self) -> Vec<VarPath> {
        let mut out = Vec::new();
        for d in &self.decls {
            flatten_decl(d, &d.name, d.required, 0, &mut out);
        }
        out
    }
}

/// 递归摊平一条声明到 `out`。`prefix` 是累积路径，`top_required` 记顶层必填。
fn flatten_decl(d: &VarDecl, prefix: &str, top_required: bool, depth: usize, out: &mut Vec<VarPath>) {
    if depth > 16 {
        return; // 防御：异常深嵌套截断。
    }
    out.push(VarPath {
        path: prefix.to_string(),
        var_type: d.var_type,
        label: d.label.clone(),
        description: d.description.clone(),
        required: top_required,
        is_collection: d.var_type == VarType::Array,
        enum_options: d.enum_options.clone(),
    });
    // 对象 → 递归字段（prefix.field）。
    if d.var_type == VarType::Object {
        for f in &d.fields {
            let child = format!("{prefix}.{}", f.name);
            flatten_decl(f, &child, f.required, depth + 1, out);
        }
    }
    // 数组 → 递归元素结构（prefix[].xxx）。元素是对象则展开其字段。
    if d.var_type == VarType::Array {
        if let Some(item) = &d.item {
            if item.var_type == VarType::Object {
                for f in &item.fields {
                    let child = format!("{prefix}[].{}", f.name);
                    flatten_decl(f, &child, f.required, depth + 1, out);
                }
            }
            // 标量元素：不额外产路径（数组本身 path 已在，多实例元素变量另经 elementVariable 引用）。
        }
    }
}

/// 递归校验一层声明列表（同层名唯一）。`path_prefix` 用于违规定位（子字段带父路径）。
fn check_decls(decls: &[VarDecl], path_prefix: &str, out: &mut Vec<VarViolation>) {
    let mut seen = std::collections::BTreeSet::new();
    for d in decls {
        let qname = if path_prefix.is_empty() {
            d.name.clone()
        } else {
            format!("{path_prefix}.{}", d.name)
        };
        if d.name.trim().is_empty() {
            out.push(VarViolation {
                var: qname.clone(),
                code: VarViolationCode::Type,
                message: "变量名不能为空".into(),
            });
            continue;
        }
        if !seen.insert(d.name.clone()) {
            out.push(VarViolation {
                var: qname.clone(),
                code: VarViolationCode::Type,
                message: format!("变量名 '{}' 重复声明", qname),
            });
        }
        check_decl(d, &qname, out);
    }
}

/// 校验单条声明（枚举候选 / 默认值类型 / 递归子结构）。
fn check_decl(d: &VarDecl, qname: &str, out: &mut Vec<VarViolation>) {
    match d.var_type {
        VarType::Enum if d.enum_options.is_empty() => out.push(VarViolation {
            var: qname.to_string(),
            code: VarViolationCode::Enum,
            message: format!("枚举变量 '{qname}' 未给出候选值"),
        }),
        VarType::Enum => {}
        _ if !d.enum_options.is_empty() => out.push(VarViolation {
            var: qname.to_string(),
            code: VarViolationCode::Type,
            message: format!(
                "非枚举变量 '{qname}' 不应带候选值（type={}）",
                d.var_type.as_str()
            ),
        }),
        _ => {}
    }
    if let Some(def) = &d.default {
        if !d.var_type.accepts(def) {
            out.push(VarViolation {
                var: qname.to_string(),
                code: VarViolationCode::Type,
                message: format!("变量 '{qname}' 默认值类型与声明 {} 不符", d.var_type.as_str()),
            });
        }
        if d.var_type == VarType::Enum {
            if let Value::String(s) = def {
                if !d.enum_options.contains(s) {
                    out.push(VarViolation {
                        var: qname.to_string(),
                        code: VarViolationCode::Enum,
                        message: format!("变量 '{qname}' 默认值 '{s}' 不在候选内"),
                    });
                }
            }
        }
    }
    // 递归：对象字段 / 数组元素。
    if d.var_type == VarType::Object && !d.fields.is_empty() {
        check_decls(&d.fields, qname, out);
    }
    if d.var_type == VarType::Array {
        if let Some(item) = &d.item {
            if item.var_type == VarType::Object && !item.fields.is_empty() {
                check_decls(&item.fields, &format!("{qname}[]"), out);
            } else {
                check_decl(item, &format!("{qname}[]"), out);
            }
        }
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

    // —— ⑤：对象/数组字段结构 + 摊平路径 —— //

    /// 含对象字段 + 对象数组元素的 schema。
    fn nested_schema() -> VarSchema {
        let level = {
            let mut d = VarDecl::new("level", VarType::Enum);
            d.enum_options = vec!["VIP".into(), "normal".into()];
            d
        };
        let customer = {
            let mut d = VarDecl::new("customer", VarType::Object);
            d.fields = vec![level];
            d
        };
        let order = {
            let mut d = VarDecl::new("order", VarType::Object);
            d.label = Some("订单".into());
            d.fields = vec![VarDecl::new("total", VarType::Number), customer];
            d
        };
        let product = {
            let mut d = VarDecl::new("product", VarType::Object);
            d.fields = vec![
                VarDecl::new("sku", VarType::String),
                VarDecl::new("ownerUser", VarType::String),
            ];
            d
        };
        let products = {
            let mut d = VarDecl::new("products", VarType::Array);
            d.item = Some(Box::new(product));
            d
        };
        VarSchema::from_decls(vec![order, products])
    }

    #[test]
    fn flatten_paths_covers_object_and_array_fields() {
        let paths: Vec<String> = nested_schema().flatten_paths().into_iter().map(|p| p.path).collect();
        assert!(paths.contains(&"order".to_string()));
        assert!(paths.contains(&"order.total".to_string()));
        assert!(paths.contains(&"order.customer".to_string()));
        assert!(paths.contains(&"order.customer.level".to_string()));
        assert!(paths.contains(&"products".to_string()));
        assert!(paths.contains(&"products[].sku".to_string()));
        assert!(paths.contains(&"products[].ownerUser".to_string()));
    }

    #[test]
    fn flatten_marks_collection_and_enum() {
        let paths = nested_schema().flatten_paths();
        let products = paths.iter().find(|p| p.path == "products").unwrap();
        assert!(products.is_collection, "数组标记 is_collection");
        let level = paths.iter().find(|p| p.path == "order.customer.level").unwrap();
        assert_eq!(level.var_type, VarType::Enum);
        assert_eq!(level.enum_options, vec!["VIP", "normal"]);
    }

    #[test]
    fn validate_shape_recurses_into_fields_and_item() {
        // 对象字段里放一个「枚举无候选」，数组元素字段里放一个「非枚举带候选」。
        let bad_field = VarDecl::new("bad", VarType::Enum); // 枚举无候选
        let obj = {
            let mut d = VarDecl::new("o", VarType::Object);
            d.fields = vec![bad_field];
            d
        };
        let bad_item_field = {
            let mut d = VarDecl::new("x", VarType::String);
            d.enum_options = vec!["a".into()]; // 非枚举带候选
            d
        };
        let arr = {
            let mut d = VarDecl::new("a", VarType::Array);
            let mut item = VarDecl::new("el", VarType::Object);
            item.fields = vec![bad_item_field];
            d.item = Some(Box::new(item));
            d
        };
        let v = VarSchema::from_decls(vec![obj, arr]).validate_shape();
        assert!(v.iter().any(|x| x.var == "o.bad" && x.code == VarViolationCode::Enum), "对象子字段枚举违规定位到 o.bad");
        assert!(v.iter().any(|x| x.var == "a[].x" && x.code == VarViolationCode::Type), "数组元素子字段违规定位到 a[].x");
    }

    #[test]
    fn nested_schema_json_roundtrip() {
        let s = nested_schema();
        let j = serde_json::to_value(&s).unwrap();
        let back: VarSchema = serde_json::from_value(j).unwrap();
        assert_eq!(s, back, "含 fields/item 的 schema JSON 往返一致");
    }
}
