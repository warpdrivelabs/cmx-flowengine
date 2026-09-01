//! HttpDelegate —— serviceTask 逻辑外包的外部 HTTP 实现（方案 §4④）。
//!
//! 实现 `JavaDelegate`：把实例当前变量 POST 给外部服务，拿返回变量 merge 回实例，替代进程内
//! delegate（如 RiskDelegate）。「算风险/调外部逻辑」外包给第三方——引擎不认识业务逻辑。
//!
//! 传输走 cmx-service-rpc 基座：目标 = `[service_rpc.services]` 服务目录键（无注册中心时
//! 目录登记静态 url 直连）。ServiceTask IR 只带 delegate 键（无地址槽），故目标键由本实例
//! 持有（一个 delegate 键一个目标，注册在 delegate 注册表），路径固定 `/delegate/run`，
//! 请求带 `?node=&instance=` 供外部按节点/实例细分。对端协议保持自定义裸 JSON
//! （非 ApiResp 信封——外部逻辑服务不受 CMX 契约约束），故用 `execute` 取原始响应。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use cmx_flow_engine::{DelegateContext, JavaDelegate, Variables};
use cmx_service_rpc::ServiceRpcHandle;

/// 外部 serviceTask 逻辑委托。持服务目录键 + 基座句柄。
#[derive(Clone)]
pub struct HttpDelegate {
    key: String,
    rpc: Arc<ServiceRpcHandle>,
}

impl HttpDelegate {
    /// 生产构造：目标为服务目录键（`[service_rpc.services]` 登记）。基座未初始化时返回
    /// `None`，装配点据此不注册 httpDelegate。
    pub fn new(key: impl Into<String>) -> Option<Self> {
        cmx_service_rpc::global_arc().map(|rpc| Self {
            key: key.into(),
            rpc,
        })
    }

    /// 测试/定制构造：显式传入基座句柄（不经全局单例，测试并行安全）。
    pub fn with_handle(key: impl Into<String>, rpc: ServiceRpcHandle) -> Self {
        Self {
            key: key.into(),
            rpc: Arc::new(rpc),
        }
    }
}

#[async_trait]
impl JavaDelegate for HttpDelegate {
    async fn execute(&self, ctx: &mut DelegateContext<'_>) -> Result<(), cmx_flow_engine::DelegateError> {
        // 请求体 = 当前全部实例变量（对象）；query 带节点/实例供外部细分。
        // 外部逻辑可能改状态 → 非幂等，不标记重试。
        let body = ctx.variables.to_json();
        let req = cmx_service_rpc::RpcRequest::post(self.key.clone(), "/delegate/run")
            .query("node", ctx.node_bpmn_id)
            .query("instance", ctx.instance_id)
            .json_body(body);
        let resp = self
            .rpc
            .execute(req)
            .await
            .map_err(|e| format!("外部 delegate 调用失败: {e}"))?;

        let out = resp.body;
        // 基座对无法解析为 JSON 的响应置 Null——与原裸 reqwest 行为对齐，此时报错而非静默 no-op。
        if out.is_null() {
            return Err("外部 delegate 响应解析失败: 非 JSON".into());
        }
        // 兼容两种返回形态：{"variables":{...}} 包裹，或直接是变量对象。
        let vars_json = match &out {
            Value::Object(m) if m.contains_key("variables") => {
                out.get("variables").cloned().unwrap_or(Value::Null)
            }
            _ => out,
        };
        // 写回：merge 返回键（同名覆盖），非对象返回则忽略（no-op，不报错）。
        if vars_json.is_object() {
            ctx.variables.merge(Variables::from_json(vars_json));
        }
        Ok(())
    }
}
