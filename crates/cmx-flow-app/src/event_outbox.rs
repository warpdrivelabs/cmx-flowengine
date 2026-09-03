//! 事件订阅的 outbox 半边：emit 侧规则匹配落库（重构方案 §四）+ 租约式投递 poller。
//!
//! **匹配模型**：订阅者 rules 数组——规则内三维 AND（eventTypes × groupIds × keyPatterns，
//! 空 = 该维不限）、跨规则 OR、**数组序 = 命中序**（首个命中即返回，规则名落投递行
//! matched_rule 快照）；全空且 enabled 的规则 = 匹配全部（网关形态）。null definitionKey
//! 的事件只被「相应维为空」的规则命中（无法证明命中的维度一律不命中——防泄露默认放行）。
//!
//! **投递语义 = at-least-once**：常态（逐行续租正常）下各 worker 行集互不相交；租约真
//! 过期（进程长停顿/网络分区）的异常窗口同一行可能双投——接收方须按 delivery_id 或
//! 业务键幂等（对接文档要求）。
//!
//! **emit 是通知路径**：写入失败只记 error 日志 + 计数，不因投递行写不进阻断业务。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Semaphore;

use cmx_flow_adapters::{DeliveryOutcome, DeliveryTask, global_registry};

use crate::engine::FlowRuntime;
use crate::event_store::{
    SubRule, SubscriberRow, claim_due_deliveries, finish_dead, finish_done, finish_retry_or_dead,
    get_subscribers_by_ids, insert_deliveries, renew_leases, route_snapshot_cached, DeliveryInsert,
};

// ============================================================
// 可观测计数（/event-deliveries/stats 附带返回）
// ============================================================

/// 事件零订阅命中计数（有活跃订阅者但无任何规则命中——规则配窄了/分组没人订的观测信号）。
pub static EMIT_NO_MATCH: AtomicU64 = AtomicU64::new(0);
/// 事件缺 definitionKey 计数（keyPatterns/ groupIds 非空的规则均不可命中；前置补齐后
/// 不应常态出现，持续增长 = 某 emit 链路漏带定义 key）。
pub static EMIT_NULL_DEFKEY: AtomicU64 = AtomicU64::new(0);
/// 非原子窗口计数（业务成功、投递行写入失败的理论窗口）。
pub static OUTBOX_INSERT_ERRORS: AtomicU64 = AtomicU64::new(0);

/// 三计数快照（stats 端点投影）。
pub fn emit_counters() -> serde_json::Value {
    json!({
        "emitNoMatch": EMIT_NO_MATCH.load(Ordering::Relaxed),
        "emitNullDefKey": EMIT_NULL_DEFKEY.load(Ordering::Relaxed),
        "outboxInsertErrors": OUTBOX_INSERT_ERRORS.load(Ordering::Relaxed),
    })
}

// ============================================================
// glob 匹配（仅 `*` 通配；迭代双指针，按字符推进防 UTF-8 边界 panic）
// ============================================================

/// glob 匹配：`*` 匹配任意字符序列（含空），其余字符逐字相等。
///
/// 实现约束（重构方案 §四/§八硬承诺）：**仅支持 `*`**——`?`/`[]`/`**`/转义一律按字面量
/// 处理；要扩展语法先换 globset，不做增量扩展。经典双指针 + 单星回溯点算法：典型
/// O(n+m)，最坏 O(n·m)（多星+失配回溯），订阅规则量级下足够。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // 最近一个 '*' 的下一位置 / 该星号匹配到的 text 位置（回溯锚点）。
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if star != usize::MAX {
            // 失配回溯：* 多吞一个字符重试。
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    // text 耗尽后 pattern 剩余必须全为 '*'。
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ============================================================
// 规则匹配（emit / rebuild / preview 共用同一实现——禁第二份过滤逻辑）
// ============================================================

/// 单规则命中判定（三维 AND；None group_id = 定义未分组/未知，只被 groupIds 空的规则命中）。
/// pub 供 rules/preview 按规则逐条演算（与 emit 同一判定路径——禁第二份过滤逻辑）。
pub fn rule_matches(rule: &SubRule, event_type: &str, def_key: Option<&str>, def_group: Option<i64>) -> bool {
    if !rule.enabled {
        return false;
    }
    if !rule.event_types.is_empty() && !rule.event_types.iter().any(|t| t == event_type) {
        return false;
    }
    if !rule.group_ids.is_empty()
        && (def_group.is_none() || !rule.group_ids.contains(&def_group.unwrap()))
    {
        return false;
    }
    if !rule.key_patterns.is_empty() {
        let Some(dk) = def_key else { return false };
        if !rule.key_patterns.iter().any(|p| glob_match(p, dk)) {
            return false;
        }
    }
    true
}

/// 订阅者规则集匹配：数组序遍历，首个命中规则的 name 返回（= 命中序 + matched_rule 快照源）。
pub fn match_rules<'a>(
    rules: &'a [SubRule],
    event_type: &str,
    def_key: Option<&str>,
    def_group: Option<i64>,
) -> Option<&'a str> {
    rules
        .iter()
        .find(|r| rule_matches(r, event_type, def_key, def_group))
        .map(|r| r.name.as_str())
}

// ============================================================
// emit 侧：事件 → 投递行（事务提交后、单批写入）
// ============================================================

/// 把一批生命周期事件写入投递行（每事件 × 每命中订阅者一行）。
///
/// - 路由数据来自 [`route_snapshot_cached`]（TTL 5s 指纹对账；分组改名/定义换组/
///   订阅者变更跨副本 ≤5s 收敛）；
/// - payload **additive** 注入 `groupId?`/`groupName?`（定义所属分组；未分组不上线）；
///   `systemId?` 由 handler 侧从实例快照带上（legacy 调用不上线）；
/// - uk(subscriber_id, event_id) 不约束 emit（event_id 每行 uuid）——幂等责任在接收方
///   按 delivery_id/业务键（at-least-once 契约）。
pub async fn emit_to_outbox(events: &[cmx_flow_adapters::FlowEvent]) {
    if events.is_empty() {
        return;
    }
    let tenant = crate::tenant::current_tenant();
    let db_id = crate::engine::current_flow_db_id();
    let snap = route_snapshot_cached(&db_id, &tenant).await;
    let mut rows = Vec::new();
    for event in events {
        if event.definition_key.is_none() {
            EMIT_NULL_DEFKEY.fetch_add(1, Ordering::Relaxed);
        }
        let def_key = event.definition_key.as_deref();
        let def_group = def_key.and_then(|k| snap.def_group.get(k).copied());
        let mut matched_any = false;
        for sub in snap.subscribers.iter() {
            let Some(rule_name) = match_rules(&sub.rules, &event.event, def_key, def_group) else {
                continue;
            };
            matched_any = true;
            // payload 注入分组维度（additive；原字段逐字节不变）。
            let mut payload = json!(event);
            if let Some(gid) = def_group {
                payload["groupId"] = json!(gid);
                if let Some(gname) = snap.group_name.get(&gid) {
                    payload["groupName"] = json!(gname);
                }
            }
            rows.push(DeliveryInsert {
                subscriber_id: sub.id,
                subscriber_name: sub.name.clone(),
                channel: sub.channel.clone(),
                // 事件唯一键：emit 时生成 uuid（uk 不去重 emit——幂等在接收方）。
                event_id: uuid::Uuid::new_v4().to_string(),
                delivery_id: event.delivery_id(),
                source: "emit",
                event_type: event.event.clone(),
                definition_key: event.definition_key.clone(),
                business_key: event.business_key.clone(),
                instance_id: event.instance_id.clone(),
                payload,
                initial_state: "PENDING",
                last_error: None,
                last_http_status: None,
                last_response_snippet: None,
                delivered: false,
                matched_rule: Some(rule_name.to_string()),
            });
        }
        if !matched_any {
            EMIT_NO_MATCH.fetch_add(1, Ordering::Relaxed);
        }
    }
    if rows.is_empty() {
        return;
    }
    if let Err(e) = insert_deliveries(&db_id, &rows).await {
        OUTBOX_INSERT_ERRORS.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            tenant,
            rows = rows.len(),
            error = %e,
            "投递行写入失败（事件可能缺行，走对账/补发口径）"
        );
    }
}

// ============================================================
// worker 侧：投递执行（租约 + 逐行续租 + 持有者守卫 + 同订阅者保序）
// ============================================================

/// 租约时长：只须大于单行最长投递（基座 30s 键级超时）+ 续租余量；批长度经逐行续租
/// 不进入租约算术。
pub const LEASE_SECS: i64 = 120;
/// 单轮抢占上限（批内按订阅者分组、组内按 seq 串行投递）。
pub const BATCH: usize = 50;
/// 跨订阅者投递并发上限（semaphore）。
pub const CONCURRENCY: usize = 8;

/// 退避曲线：1s 起指数、封顶 5min（第 n 次尝试失败后等待 2^(n-1) 秒；
/// retry_max = 最大尝试次数含首发，默认 10 → 首发到 DEAD 约 9~14 分钟）。
pub fn backoff_after(attempts: i64) -> chrono::Duration {
    let exp = (attempts.max(1) - 1).min(9) as u32;
    let secs = 1u64.checked_shl(exp).unwrap_or(300).min(300);
    chrono::Duration::seconds(secs as i64)
}

/// 经通道注册表投递一条任务（poller 与 test 端点共用；timeout 仅 test 传短超时 10s）。
pub(crate) async fn dispatch_via_channel(
    channel_type: &str,
    config: &serde_json::Value,
    task: DeliveryTask,
    timeout: Option<Duration>,
) -> DeliveryOutcome {
    let Some(channel) = global_registry().get(channel_type) else {
        return DeliveryOutcome::retry(format!("通道 {channel_type} 未注册（feature 未启用或已下线）"));
    };
    channel.deliver(config, &task, timeout).await
}

/// 对一个租户运行态跑一轮投递（engine.rs 的 poller tick 每 2s 调一次）。
///
/// 循环抢占直到空批/不足批；每行投递 → **持有者守卫**落结果 → **续租**本 worker 剩余
/// 在途行。守卫 0 行命中 = 租约丢失（进程长停顿/分区），立即中止本租户本轮——剩余行
/// 留待租约过期后重抢（防双投扩大）。
pub async fn poll_once(rt: &FlowRuntime, worker: &str) {
    let db_id = crate::tenancy::TenancyConfig::global().flow_db_id(&rt.tenant);
    loop {
        let claimed = match claim_due_deliveries(&db_id, worker, LEASE_SECS, BATCH).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(tenant = %rt.tenant, error = %e, "事件投递行抢占失败");
                return;
            }
        };
        if claimed.is_empty() {
            return;
        }
        let full = claimed.len() >= BATCH;
        // 投递前取本批涉及的订阅者配置（不筛 active——停用订阅者的存量行仍投）。
        let mut sub_ids: Vec<i64> = claimed.iter().map(|c| c.subscriber_id).collect();
        sub_ids.sort_unstable();
        sub_ids.dedup();
        let subs = get_subscribers_by_ids(&db_id, &rt.tenant, &sub_ids)
            .await
            .unwrap_or_default();
        if !deliver_batch(&db_id, worker, claimed, &subs).await {
            tracing::warn!(tenant = %rt.tenant, "投递租约丢失（持有者守卫命中），中止本轮，剩余行待租约过期重抢");
            return;
        }
        if !full {
            return;
        }
    }
}

/// 投递一批：按订阅者分组（组内按 seq 串行——保序）、跨组 semaphore 并发。
/// 返回 false = 任一租约丢失（调用方应中止本轮）。
async fn deliver_batch(
    db_id: &str,
    worker: &str,
    claimed: Vec<crate::event_store::ClaimedDelivery>,
    subs: &HashMap<i64, SubscriberRow>,
) -> bool {
    let mut groups: HashMap<i64, Vec<crate::event_store::ClaimedDelivery>> = HashMap::new();
    for row in claimed {
        groups.entry(row.subscriber_id).or_default().push(row);
    }
    let sem = Arc::new(Semaphore::new(CONCURRENCY));
    let mut handles = Vec::with_capacity(groups.len());
    for (_, mut rows) in groups {
        rows.sort_by_key(|r| r.seq);
        let permit = sem.clone().acquire_owned().await;
        let db = db_id.to_string();
        let w = worker.to_string();
        let subs = subs.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            deliver_group(&db, &w, rows, &subs).await
        }));
    }
    for h in handles {
        match h.await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                tracing::error!(error = %e, "投递组任务 panic/取消");
                return false;
            }
        }
    }
    true
}

/// 一组（同订阅者）投递：组内按 seq 串行，逐行「结果落库（守卫）→ 续租」。
async fn deliver_group(
    db_id: &str,
    worker: &str,
    rows: Vec<crate::event_store::ClaimedDelivery>,
    subs: &HashMap<i64, SubscriberRow>,
) -> bool {
    for row in rows {
        let sub = subs.get(&row.subscriber_id);
        let outcome = match sub {
            // 订阅者已物理删（守卫下不应再有存量行）：直达死信，不空转重试。
            None => DeliveryOutcome::fatal("订阅者已删除，通道配置不可得（可 retry 重发或 skip 处置）"),
            Some(s) => {
                let task = DeliveryTask {
                    subscription_name: row.subscriber_name.clone(),
                    event_type: row.event_type.clone(),
                    definition_key: row.definition_key.clone(),
                    business_key: row.business_key.clone(),
                    instance_id: row.instance_id.clone(),
                    delivery_id: row.delivery_id.clone(),
                    payload: row.payload.clone(),
                };
                dispatch_via_channel(&row.channel, &s.channel_config, task, None).await
            }
        };
        // 结果落库（持有者守卫；0 行命中 = 租约已丢，返回 false 中止本组/批）。
        let (retry_max, attempts) = (
            sub.map(|s| s.retry_max).unwrap_or(10),
            row.attempts,
        );
        let diag = |error: &String, http_status: &Option<u16>, snippet: &Option<String>| {
            crate::event_store::Diagnostics {
                error: error.clone(),
                http_status: http_status.map(i64::from),
                snippet: snippet.clone(),
            }
        };
        let guarded = match &outcome {
            DeliveryOutcome::Success => finish_done(db_id, row.id, worker).await,
            DeliveryOutcome::Retryable { http_status, error, snippet } => {
                finish_retry_or_dead(
                    db_id,
                    row.id,
                    worker,
                    attempts,
                    retry_max,
                    backoff_after(attempts),
                    &diag(error, http_status, snippet),
                )
                .await
            }
            DeliveryOutcome::Fatal { http_status, error, snippet } => {
                finish_dead(db_id, row.id, worker, &diag(error, http_status, snippet)).await
            }
        };
        match guarded {
            Ok(true) => {}
            Ok(false) => return false, // 守卫命中：租约丢失。
            Err(e) => {
                tracing::error!(delivery = row.id, error = %e, "落投递结果失败（行留 IN_FLIGHT，租约过期后自愈重投）");
                return false;
            }
        }
        // 逐行续租：本 worker 名下全部在途行延长一窗——批长度不进入租约算术。
        if let Err(e) = renew_leases(db_id, worker, LEASE_SECS).await {
            tracing::warn!(error = %e, "投递租约续租失败（窗口内租约可能过期，行会被重抢）");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::SubRule;

    fn rule(
        name: &str,
        event_types: &[&str],
        group_ids: &[i64],
        key_patterns: &[&str],
        enabled: bool,
    ) -> SubRule {
        SubRule {
            name: name.into(),
            enabled,
            event_types: event_types.iter().map(|s| s.to_string()).collect(),
            group_ids: group_ids.to_vec(),
            key_patterns: key_patterns.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn evt(def_key: Option<&str>, event: &str) -> cmx_flow_adapters::FlowEvent {
        let mut ev = cmx_flow_adapters::FlowEvent::new(
            cmx_flow_adapters::FlowEventKind::InstanceStarted,
            "i-1",
            "t".into(),
        )
        .definition_key(def_key.map(String::from))
        .task(None, None)
        .tenant(None)
        .business_key(None)
        .assignee(None)
        .state(None);
        ev.event = event.to_string(); // 匹配语义测试需要任意事件名，直接覆盖。
        ev
    }

    // ————— glob：仅 * 通配；穷举边界 —————

    #[test]
    fn glob_basics() {
        // 精确。
        assert!(glob_match("mdm_cr", "mdm_cr"));
        assert!(!glob_match("mdm_cr", "mdm_cx"));
        // 前缀/中缀/后缀/多段。
        assert!(glob_match("mdm_*", "mdm_cr"));
        assert!(glob_match("mdm_*", "mdm_"));
        assert!(glob_match("*_cr", "mdm_cr"));
        assert!(glob_match("*cr*", "xx_cryy"));
        assert!(glob_match("m*m*c*", "mdm_cr"));
        assert!(!glob_match("m*m*c*", "mdm_dr"));
        // 全星/空串。
        assert!(glob_match("*", "anything"));
        assert!(glob_match("***", ""));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
        // 多星回溯（最坏路径）。
        assert!(glob_match("*c*r", "mdm_cr"));
        assert!(!glob_match("*c*r", "mdm_rr"));
        assert!(glob_match("a*b*c", "a_x_b_y_c"));
        assert!(!glob_match("a*b*c", "a_x_b_y"));
    }

    #[test]
    fn glob_utf8_safety() {
        // 多字节字符（中文 key）：按字符推进不 panic、语义正确。
        assert!(glob_match("报销*", "报销单-001"));
        assert!(!glob_match("报销*", "审批单-001"));
        assert!(glob_match("*单*", "报销单-001"));
        assert!(glob_match("报销*单", "报销审批单"));
    }

    // ————— 规则语义：三维 AND、跨规则 OR、数组序、null defKey —————

    #[test]
    fn rule_and_or_order_semantics() {
        let r1 = rule("审批事件", &["instance.completed"], &[7], &["mdm_*"], true);
        let r2 = rule("全量", &[], &[], &[], true);
        let rules = vec![r1.clone(), r2];
        let ev = evt(Some("mdm_cr"), "instance.completed");
        // 命中 r1（数组序：首个命中即返回）。
        assert_eq!(
            match_rules(&rules, &ev.event, ev.definition_key.as_deref(), Some(7)),
            Some("审批事件")
        );
        // 分组不符 → 跨到 r2（OR）。
        assert_eq!(
            match_rules(&rules, &ev.event, ev.definition_key.as_deref(), Some(8)),
            Some("全量")
        );
        // key 不符 → r2。
        assert_eq!(
            match_rules(&rules, &ev.event, Some("rpt_x"), Some(7)),
            Some("全量")
        );
        // 事件类型不符 → r2。
        assert_eq!(
            match_rules(&rules, "task.created", ev.definition_key.as_deref(), Some(7)),
            Some("全量")
        );
        // 全空规则 = 网关形态（全匹配）。
        assert_eq!(
            match_rules(&[rule("gw", &[], &[], &[], true)], "task.created", None, None),
            Some("gw")
        );
        // 停用规则被跳过。
        assert_eq!(
            match_rules(&[rule("off", &[], &[], &[], false)], "task.created", None, None),
            None
        );
        // r1 单独存在、条件不符 → 不命中。
        assert_eq!(
            match_rules(&[r1], "task.created", Some("mdm_cr"), Some(7)),
            None
        );
    }

    #[test]
    fn rule_null_defkey_semantics() {
        // null definitionKey：keyPatterns/groupIds 非空的规则不可命中（无法证明命中），
        // 只被相应维为空的规则命中——防泄露默认放行。
        let by_key = rule("按key", &[], &[], &["mdm_*"], true);
        let by_group = rule("按组", &[], &[7], &[], true);
        let free = rule("不限", &[], &[], &[], true);
        assert_eq!(match_rules(&[by_key], "instance.started", None, None), None);
        assert_eq!(match_rules(&[by_group], "instance.started", None, None), None);
        // 有 key 但定义未知分组（不在 map）：groupIds 非空不可命中。
        assert_eq!(
            match_rules(&[rule("按组", &[], &[7], &[], true)], "instance.started", Some("x"), None),
            None
        );
        assert_eq!(
            match_rules(&[free], "instance.started", None, None),
            Some("不限")
        );
    }

    /// 退避曲线：1s 起指数、封顶 300s。
    #[test]
    fn backoff_curve() {
        assert_eq!(backoff_after(1), chrono::Duration::seconds(1));
        assert_eq!(backoff_after(2), chrono::Duration::seconds(2));
        assert_eq!(backoff_after(3), chrono::Duration::seconds(4));
        assert_eq!(backoff_after(10), chrono::Duration::seconds(300));
        assert_eq!(backoff_after(20), chrono::Duration::seconds(300));
    }
}
