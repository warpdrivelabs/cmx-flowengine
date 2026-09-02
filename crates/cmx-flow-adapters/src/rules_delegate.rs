//! RulesEngineDelegate —— serviceTask 决策外包到 cmx-rulesengine 的实现（方案 A · P1）。
//!
//! 实现 `JavaDelegate`：把实例当前变量作为 facts POST 给规则引擎的评估端点，判信封 code
//! 后把决策 `output` merge 回实例变量，并把 `logId`（可选 trace）留痕到 `__decisions`。
//! 让流程把「真决策」（11 命中策略 / 决策图 / FEEL / gap-overlap / 归因）委托给专业的
//! cmx-rulesengine，而 flow 内置 `businessRuleTask` + `DecisionTable` 保留为简单内联兜底。
//!
//! 与 [`crate::delegate::HttpDelegate`] 的区别：
//!   - 目标路径固定为 rules 的 `POST /api/rules/v1/decisions/{key}/evaluate`（本实例持 decisionKey）；
//!   - 请求体是 `{ "input": {全部变量}, "options": {trace,log} }`（rules 契约，非 HttpDelegate 的裸变量）；
//!   - 附 `X-Tenant`（P1 租户契约：rules off 模式据此定位租户库）；
//!   - 对端是 **ApiResp 信封**（`{code,msg,data}`）——**业务失败是 HTTP 200 且 code≠0**，故用
//!     基座 `execute` 取原始响应后**自解信封**：code==0 → 取 data.output/logId/trace；
//!     code≠0 → `DelegateError::Bpmn{code:"decisionFailed"}`（可被节点错误边界优雅接住）。
//!
//! 每个可调用的 decisionKey 注册一个本实例（键 = `rules:<decisionKey>`，装配在 cmx-flow-app）。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use cmx_flow_engine::{DelegateContext, DelegateError, JavaDelegate, Variables};
use cmx_service_rpc::ServiceRpcHandle;

/// 决策 trace 的回写粒度（对应 `FLOW_RULES_TRACE_PERSIST`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TracePersist {
    /// 不回写决策留痕。
    Off,
    /// 只回写 logId + 元信息（默认，省快照空间；全量 trace 已在 rules 侧决策日志）。
    #[default]
    LogId,
    /// 回写全量 trace（内联进流程变量历史）。
    Full,
}

impl TracePersist {
    /// 从字符串解析（大小写不敏感）；未知/空 → 默认 `LogId`。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Self::Off,
            "full" => Self::Full,
            _ => Self::LogId,
        }
    }
}

/// 规则引擎决策委托。持服务目录键 + decisionKey + 租户/粒度配置 + 基座句柄。
#[derive(Clone)]
pub struct RulesEngineDelegate {
    /// cmx-service-rpc 服务目录键（`[service_rpc.services]` 登记，如 "rules"）。
    service_key: String,
    /// 目标决策键（rules 侧 `decision_key`）。
    decision_key: String,
    /// 出站租户名（附 `X-Tenant`；P1 rules off 模式据此选租户库）。
    tenant: Option<String>,
    /// trace 回写粒度。
    trace_persist: TracePersist,
    /// 是否请求 rules 返回 trace（`options.trace`）；Off 粒度下省流量置 false。
    request_trace: bool,
    /// 基座句柄。
    rpc: Arc<ServiceRpcHandle>,
}

impl RulesEngineDelegate {
    /// 生产构造：基座未初始化时返回 `None`，装配点据此不注册。
    pub fn new(
        service_key: impl Into<String>,
        decision_key: impl Into<String>,
        tenant: Option<String>,
        trace_persist: TracePersist,
    ) -> Option<Self> {
        cmx_service_rpc::global_arc().map(|rpc| Self {
            service_key: service_key.into(),
            decision_key: decision_key.into(),
            tenant,
            trace_persist,
            request_trace: trace_persist != TracePersist::Off,
            rpc,
        })
    }

    /// 测试/定制构造：显式传入基座句柄（不经全局单例，测试并行安全）。
    pub fn with_handle(
        service_key: impl Into<String>,
        decision_key: impl Into<String>,
        tenant: Option<String>,
        trace_persist: TracePersist,
        rpc: ServiceRpcHandle,
    ) -> Self {
        Self {
            service_key: service_key.into(),
            decision_key: decision_key.into(),
            tenant,
            trace_persist,
            request_trace: trace_persist != TracePersist::Off,
            rpc: Arc::new(rpc),
        }
    }

    /// 规则引擎评估路径：`/api/rules/v1/decisions/{key}/evaluate`。
    fn eval_path(&self) -> String {
        format!("/api/rules/v1/decisions/{}/evaluate", self.decision_key)
    }
}

#[async_trait]
impl JavaDelegate for RulesEngineDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), DelegateError> {
        // 请求体 = { input: 全部实例变量, options: { trace, log } }（rules 契约）。
        // P1 传全量变量作 facts——决策只取自己声明的输入列，多余变量无害；节点级裁剪留后续。
        let body = json!({
            "input": ctx.variables.to_json(),
            "options": { "trace": self.request_trace, "log": true },
        });
        let mut req = cmx_service_rpc::RpcRequest::post(self.service_key.clone(), self.eval_path())
            .query("node", ctx.node_bpmn_id)
            .query("instance", ctx.instance_id)
            .json_body(body);
        // P1 租户契约：附 X-Tenant（rules off 模式据此定位租户库）。X-API-Key / 用户委托令牌
        // 由基座传输层自动注入，无需在此处理。
        if let Some(t) = &self.tenant {
            req = req.header("X-Tenant", t.clone());
        }

        // 用 execute 取传输层原始响应（HTTP 状态 + 已解析 JSON）——不用 call_api，因为业务失败是
        // HTTP 200 且 code≠0，需要自解信封才能区分「业务失败(→Bpmn)」与「传输/鉴权失败(→Generic)」。
        // 基座已把 401/403→AuthRejected、非 2xx→Remote 映射为 Err，落 Generic → Incident。
        let resp = self.rpc.execute(req).await.map_err(|e| {
            DelegateError::Generic(format!(
                "规则决策 [{}] 调用失败: {e}",
                self.decision_key
            ))
        })?;

        // 解 ApiResp 信封（{code, msg, data}）。
        let env = &resp.body;
        if env.is_null() {
            return Err(DelegateError::Generic(format!(
                "规则决策 [{}] 响应解析失败: 非 JSON",
                self.decision_key
            )));
        }
        let code = env.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            // 业务失败（HTTP 200, code≠0）→ 类型化 BPMN 异常，可被节点错误边界（errorRef="decisionFailed"）
            // 优雅接住走补偿/人工路径；无匹配边界则引擎回退 Incident。
            let msg = env
                .get("msg")
                .or_else(|| env.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("规则决策业务失败")
                .to_string();
            return Err(DelegateError::bpmn(
                "decisionFailed",
                format!("规则决策 [{}] code={code}: {msg}", self.decision_key),
            ));
        }

        // code==0：取 data.output 写回变量；data.logId / trace 按粒度留痕。
        let data = env.get("data").cloned().unwrap_or(Value::Null);
        if let Some(output) = data.get("output") {
            if output.is_object() {
                ctx.variables.merge(Variables::from_json(output.clone()));
            }
            // 非对象 output（如 Collect 的数组、聚合的数值）：包成命名变量写回，避免丢弃。
            else if !output.is_null() {
                let var_name = format!("{}_result", self.decision_key);
                ctx.variables.set(var_name, output.clone());
            }
        }
        self.record_decision(ctx, &data);
        Ok(())
    }
}

impl RulesEngineDelegate {
    /// 把本次决策留痕追加到 `__decisions` 变量（数组）。粒度由 `trace_persist` 控制。
    fn record_decision(&self, ctx: &mut DelegateContext<'_>, data: &Value) {
        if self.trace_persist == TracePersist::Off {
            return;
        }
        let mut entry = json!({
            "key": self.decision_key,
            "node": ctx.node_bpmn_id,
            "logId": data.get("logId").cloned().unwrap_or(Value::Null),
            "timingUs": data.get("timingUs").cloned().unwrap_or(Value::Null),
        });
        if self.trace_persist == TracePersist::Full {
            if let Some(trace) = data.get("trace") {
                entry["trace"] = trace.clone();
            }
        }
        // 追加进 __decisions 数组（保留同实例多次决策的顺序）。
        let mut arr = match ctx.variables.get("__decisions") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        arr.push(entry);
        ctx.variables.set("__decisions", Value::Array(arr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use cmx_service_rpc::{
        OutgoingHeaders, RpcRequest, RpcResponse, ServiceEntry, ServiceRpcConfig, ServiceRpcError,
        ServiceRpcHandle, Transport,
    };

    /// 桩传输：捕获最后一次请求（供断言路径/头/body），返回预设响应。
    struct StubTransport {
        resp: RpcResponse,
        seen: Mutex<Option<(String, RpcRequest, OutgoingHeaders)>>,
    }
    impl StubTransport {
        fn new(resp: RpcResponse) -> Self {
            Self { resp, seen: Mutex::new(None) }
        }
    }
    #[async_trait]
    impl Transport for StubTransport {
        async fn execute(
            &self,
            base: &str,
            req: &RpcRequest,
            _timeout: Duration,
            headers: &OutgoingHeaders,
        ) -> Result<RpcResponse, ServiceRpcError> {
            *self.seen.lock().unwrap() = Some((base.to_string(), req.clone(), headers.clone()));
            Ok(self.resp.clone())
        }
    }

    fn handle_with(resp: RpcResponse) -> (ServiceRpcHandle, Arc<StubTransport>) {
        let mut cfg = ServiceRpcConfig::default();
        cfg.services.insert(
            "rules".to_string(),
            ServiceEntry { url: Some("http://127.0.0.1:8094".to_string()), ..Default::default() },
        );
        let stub = Arc::new(StubTransport::new(resp));
        (ServiceRpcHandle::with_transport(cfg, stub.clone()), stub)
    }

    fn resp(status: u16, body: Value) -> RpcResponse {
        RpcResponse { status, body }
    }

    async fn run(delegate: &RulesEngineDelegate, vars: &mut Variables) -> Result<(), DelegateError> {
        let mut ctx = DelegateContext {
            instance_id: "i-1",
            node_bpmn_id: "scoreNode",
            variables: vars,
        };
        delegate.execute(&mut ctx).await
    }

    #[tokio::test]
    async fn success_merges_output_and_records_logid() {
        let (h, stub) = handle_with(resp(
            200,
            json!({ "code": 0, "msg": "ok", "data": {
                "output": { "credit_tier": "A", "discount": 0.1 },
                "logId": "log-123", "timingUs": 42
            }}),
        ));
        let d = RulesEngineDelegate::with_handle(
            "rules", "creditScoring", Some("acme".into()), TracePersist::LogId, h,
        );
        let mut vars = Variables::from_json(json!({ "amount": 5000, "level": "gold" }));
        run(&d, &mut vars).await.expect("应成功");

        // 输出 merge 回变量。
        assert_eq!(vars.get("credit_tier"), Some(&json!("A")));
        assert_eq!(vars.get("discount"), Some(&json!(0.1)));
        // 决策留痕（默认 LogId 粒度）：logId 存、trace 不存。
        let dec = vars.get("__decisions").unwrap().as_array().unwrap();
        assert_eq!(dec.len(), 1);
        assert_eq!(dec[0].get("logId"), Some(&json!("log-123")));
        assert_eq!(dec[0].get("key"), Some(&json!("creditScoring")));
        assert!(dec[0].get("trace").is_none());

        // 请求契约：路径 + body.input + X-Tenant。
        let seen = stub.seen.lock().unwrap();
        let (base, req, headers) = seen.as_ref().unwrap();
        assert_eq!(base, "http://127.0.0.1:8094");
        assert_eq!(req.path, "/api/rules/v1/decisions/creditScoring/evaluate");
        if let cmx_service_rpc::Body::Json(b) = &req.body {
            assert_eq!(b.get("input").unwrap().get("amount"), Some(&json!(5000)));
            assert_eq!(b["options"]["log"], json!(true));
        } else {
            panic!("body 应为 JSON");
        }
        assert!(headers.extra.iter().any(|(k, v)| k == "X-Tenant" && v == "acme"));
    }

    #[tokio::test]
    async fn business_failure_maps_to_bpmn_decision_failed() {
        // 业务失败 = HTTP 200 且 code≠0。
        let (h, _) = handle_with(resp(200, json!({ "code": 1, "msg": "命中黑名单" })));
        let d = RulesEngineDelegate::with_handle(
            "rules", "amlScreening", None, TracePersist::LogId, h,
        );
        let mut vars = Variables::new();
        let err = run(&d, &mut vars).await.unwrap_err();
        match err {
            DelegateError::Bpmn { code, message } => {
                assert_eq!(code, "decisionFailed");
                assert!(message.contains("amlScreening") && message.contains("命中黑名单"));
            }
            other => panic!("应为 Bpmn，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_trace_persist_inlines_trace() {
        let (h, _) = handle_with(resp(
            200,
            json!({ "code": 0, "data": {
                "output": { "x": 1 }, "logId": "l1",
                "trace": [{ "nodeId": "n1", "matchedRules": [0] }]
            }}),
        ));
        let d = RulesEngineDelegate::with_handle(
            "rules", "k", None, TracePersist::Full, h,
        );
        let mut vars = Variables::new();
        run(&d, &mut vars).await.expect("应成功");
        let dec = vars.get("__decisions").unwrap().as_array().unwrap();
        assert!(dec[0].get("trace").is_some(), "Full 粒度应内联 trace");
    }

    #[tokio::test]
    async fn non_object_output_wrapped_as_named_var() {
        // Collect 命中策略返回数组 / 聚合返回数值 → 包成 <key>_result，避免丢弃。
        let (h, _) = handle_with(resp(
            200,
            json!({ "code": 0, "data": { "output": ["a", "b"], "logId": "l" }}),
        ));
        let d = RulesEngineDelegate::with_handle(
            "rules", "collectDecision", None, TracePersist::Off, h,
        );
        let mut vars = Variables::new();
        run(&d, &mut vars).await.expect("应成功");
        assert_eq!(vars.get("collectDecision_result"), Some(&json!(["a", "b"])));
        // Off 粒度：不写 __decisions。
        assert!(vars.get("__decisions").is_none());
    }

    #[test]
    fn trace_persist_parse() {
        assert_eq!(TracePersist::parse("full"), TracePersist::Full);
        assert_eq!(TracePersist::parse("OFF"), TracePersist::Off);
        assert_eq!(TracePersist::parse("logid"), TracePersist::LogId);
        assert_eq!(TracePersist::parse(""), TracePersist::LogId);
        assert_eq!(TracePersist::parse("garbage"), TracePersist::LogId);
    }
}

