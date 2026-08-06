/*
 * @Describe: 候选人表达式解析（M4）—— 把 BPMN 里的办理人/候选/抄送表达式拆成结构化引用。
 *
 * 表达式语法（受控、手写解析，延续 expr.rs/duration.rs「不引第三方」哲学）：
 *   逗号分隔的若干项，每项是 `kind(value)` 或裸 id：
 *     "user(u_1001), role(finance), position(cfo), org(d_fin)"
 *     裸 id（无括号）视为 user：  "u_1001, u_1002"  等价 "user(u_1001), user(u_1002)"
 *   兼容 Flowable：candidateUsers 逗号列表 → 一串 User 引用；candidateGroups → 一串 Role 引用。
 *
 * 解析在编译期做一次，产出 Vec<CandidateRef>，运行期交给 AssigneeResolver 解析成真实用户。
 */

use crate::ir::{CandidateKind, CandidateRef};

/// 解析一个候选人表达式为若干 CandidateRef。空串 / 全空白 → 空 Vec（不报错，宽容处理）。
///
/// `default_kind` 决定「裸值」（无 `kind(...)` 包裹）按什么类型解释：
/// - 通用 assignee/自定义表达式用 `CandidateKind::User`；
/// - `candidateGroups` 场景传 `CandidateKind::Role`（Flowable 里 group 即角色）。
pub fn parse_candidate_expr(expr: &str, default_kind: CandidateKind) -> Vec<CandidateRef> {
    let mut out = Vec::new();
    for raw in expr.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        out.push(parse_one(item, default_kind));
    }
    out
}

/// 解析单项：`kind(value)` 或裸值。无法识别的 kind 前缀退回按 default_kind 处理整个串。
fn parse_one(item: &str, default_kind: CandidateKind) -> CandidateRef {
    // 形如 name(value)：取前缀 name 与括号内 value。
    if let Some(open) = item.find('(')
        && item.ends_with(')')
        && open > 0
    {
        let name = item[..open].trim().to_ascii_lowercase();
        let value = item[open + 1..item.len() - 1].trim().to_string();
        if let Some(kind) = kind_from_name(&name) {
            return CandidateRef { kind, value };
        }
        // 未知前缀：整项按裸值 + default_kind（如把 "foo(bar)" 当字面 user 少见，保守落默认）。
        return CandidateRef {
            kind: default_kind,
            value: item.to_string(),
        };
    }
    // 裸值：按 default_kind。
    CandidateRef {
        kind: default_kind,
        value: item.to_string(),
    }
}

/// 把前缀名映射到 CandidateKind。支持若干同义写法。
fn kind_from_name(name: &str) -> Option<CandidateKind> {
    match name {
        "user" | "u" => Some(CandidateKind::User),
        "role" | "group" => Some(CandidateKind::Role),
        "position" | "pos" | "post" => Some(CandidateKind::Position),
        "org" | "dept" | "department" => Some(CandidateKind::Org),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_refs() {
        let refs = parse_candidate_expr(
            "user(u_1001), role(finance), position(cfo), org(d_fin)",
            CandidateKind::User,
        );
        assert_eq!(refs.len(), 4);
        assert_eq!(refs[0].kind, CandidateKind::User);
        assert_eq!(refs[0].value, "u_1001");
        assert_eq!(refs[1].kind, CandidateKind::Role);
        assert_eq!(refs[1].value, "finance");
        assert_eq!(refs[2].kind, CandidateKind::Position);
        assert_eq!(refs[2].value, "cfo");
        assert_eq!(refs[3].kind, CandidateKind::Org);
        assert_eq!(refs[3].value, "d_fin");
    }

    #[test]
    fn bare_values_take_default_kind() {
        // candidateUsers 场景：裸 id 列表当 User。
        let refs = parse_candidate_expr("u_1001, u_1002", CandidateKind::User);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|r| r.kind == CandidateKind::User));
        // candidateGroups 场景：裸 code 列表当 Role。
        let groups = parse_candidate_expr("finance, legal", CandidateKind::Role);
        assert!(groups.iter().all(|r| r.kind == CandidateKind::Role));
        assert_eq!(groups[1].value, "legal");
    }

    #[test]
    fn synonyms_and_empty() {
        assert_eq!(
            parse_candidate_expr("pos(cfo)", CandidateKind::User)[0].kind,
            CandidateKind::Position
        );
        assert_eq!(
            parse_candidate_expr("dept(d1)", CandidateKind::User)[0].kind,
            CandidateKind::Org
        );
        assert!(parse_candidate_expr("", CandidateKind::User).is_empty());
        assert!(parse_candidate_expr("  ,  ", CandidateKind::User).is_empty());
    }
}
