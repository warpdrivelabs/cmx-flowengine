//! cmx-flow-worker-sdk —— 外部 job worker 客户端 SDK（A7 外部 Worker + P1 异步作业）。
//!
//! serviceTask 标 `flowable:type="external-worker"` + `flowable:topic="X"` 时，令牌停在 `WaitingAsync`
//! 并生成一条带 topic 的作业；平台**不**在进程内执行它，等外部 worker 按 topic 抢占、执行、回调。
//! 本 SDK 就是那个「外部 worker」的客户端：
//!
//! ```no_run
//! use cmx_flow_worker_sdk::{WorkerClient, HandlerResult};
//! # async fn demo() {
//! let client = WorkerClient::new("http://127.0.0.1:8091", "pay-worker-1")
//!     .with_api_key("cmx_sk_dev_...");
//! client.run("pay", std::time::Duration::from_secs(2), |job| async move {
//!     // 执行真实支付……成功写回变量，失败返回原因。
//!     let paid = job.variables.get("amount").cloned().unwrap_or_default();
//!     HandlerResult::Ok(serde_json::json!({ "paid": paid, "gateway": "alipay" }))
//! }).await;
//! # }
//! ```
//!
//! 纯 HTTP 客户端（reqwest），零引擎/DB 依赖——可被任意 Rust 服务/worker 引入。契约对齐平台端点：
//!   - `POST {base}/api/flow/v1/external-worker/jobs/acquire`  按 topic 抢占（SKIP LOCKED，集群安全）
//!   - `POST {base}/api/flow/v1/async-jobs/{id}/complete`      回调完成（写回变量，推进令牌）
//!   - `POST {base}/api/flow/v1/async-jobs/{id}/fail`          回调失败（重试-1；耗尽转 Incident）

use std::time::Duration;

use serde::Deserialize;

/// 一个待处理的外部作业（acquire 返回）。
#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    /// 作业 id（回调 complete/fail 用）。acquire 响应里字段名为 `id`。
    #[serde(rename = "id")]
    pub job_id: String,
    /// 所属流程实例 id。
    #[serde(rename = "instanceId")]
    pub instance_id: String,
    /// serviceTask 节点 bpmnId。
    #[serde(rename = "nodeBpmnId", default)]
    pub node_bpmn_id: Option<String>,
    /// 作业 topic。
    #[serde(default)]
    pub topic: Option<String>,
    /// 已重试次数。
    #[serde(default)]
    pub retries: i64,
    /// 最大重试次数。
    #[serde(rename = "maxRetries", default)]
    pub max_retries: i64,
    /// 实例当前变量快照（handler 据此执行）。
    #[serde(default)]
    pub variables: serde_json::Value,
}

/// handler 结果：`Ok(写回变量)` → complete；`Err(原因)` → fail。
pub type HandlerResult = Result<serde_json::Value, String>;

/// SDK 错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// HTTP 传输错误。
    #[error("worker HTTP 请求失败: {0}")]
    Http(String),
    /// 响应体解析错误。
    #[error("worker 响应解析失败: {0}")]
    Decode(String),
}

/// 外部 worker 客户端。
pub struct WorkerClient {
    /// 已归一到 `{root}/api/flow/v1` 的基址。
    base: String,
    api_key: Option<String>,
    worker_id: String,
    http: reqwest::Client,
    lock_secs: i64,
    limit: i64,
}

impl WorkerClient {
    /// 用 flow 服务根地址（如 `http://host:8091`，或已带 `/api/flow/v1`）+ worker 唯一标识构建。
    pub fn new(base_url: impl Into<String>, worker_id: impl Into<String>) -> Self {
        let mut b = base_url.into().trim_end_matches('/').to_string();
        // 容错：允许传服务根地址；缺 v1 前缀则补齐。
        if !b.ends_with("/api/flow/v1") {
            b = format!("{b}/api/flow/v1");
        }
        Self {
            base: b,
            api_key: None,
            worker_id: worker_id.into(),
            http: reqwest::Client::new(),
            lock_secs: 60,
            limit: 10,
        }
    }

    /// 设服务 API Key（外部 worker 是服务身份，经 `X-API-Key` 鉴权）。
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// 设锁定时长秒数（缺省 60；handler 应远快于此，否则锁到期作业被他人重抢）。
    pub fn with_lock_secs(mut self, secs: i64) -> Self {
        self.lock_secs = secs;
        self
    }

    /// 设单次抢占上限（缺省 10）。
    pub fn with_limit(mut self, n: i64) -> Self {
        self.limit = n;
        self
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(k) => rb.header("X-API-Key", k),
            None => rb,
        }
    }

    /// 按 topic 抢占一批作业（SKIP LOCKED，集群安全）。空 = 当前无待处理作业。
    pub async fn acquire(&self, topic: &str) -> Result<Vec<Job>, WorkerError> {
        let url = format!("{}/external-worker/jobs/acquire", self.base);
        let body = serde_json::json!({
            "worker_id": self.worker_id, "topic": topic,
            "lock_secs": self.lock_secs, "limit": self.limit,
        });
        let resp = self
            .auth(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| WorkerError::Http(e.to_string()))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| WorkerError::Decode(e.to_string()))?;
        let jobs = v
            .get("data")
            .and_then(|d| d.get("jobs"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));
        serde_json::from_value(jobs).map_err(|e| WorkerError::Decode(e.to_string()))
    }

    /// 回调完成：写回变量，令牌从 WaitingAsync 转 Active 沿出边推进。
    pub async fn complete(
        &self,
        job_id: &str,
        variables: serde_json::Value,
    ) -> Result<(), WorkerError> {
        let url = format!("{}/async-jobs/{}/complete", self.base, job_id);
        self.auth(self.http.post(&url))
            .json(&serde_json::json!({ "variables": variables }))
            .send()
            .await
            .map_err(|e| WorkerError::Http(e.to_string()))?;
        Ok(())
    }

    /// 回调失败：重试次数 -1 并释放锁；耗尽则令牌转 Incident。返回 `retryable`（是否仍可重试）。
    pub async fn fail(&self, job_id: &str, error: &str) -> Result<bool, WorkerError> {
        let url = format!("{}/async-jobs/{}/fail", self.base, job_id);
        let resp = self
            .auth(self.http.post(&url))
            .json(&serde_json::json!({ "error": error }))
            .send()
            .await
            .map_err(|e| WorkerError::Http(e.to_string()))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| WorkerError::Decode(e.to_string()))?;
        Ok(v.get("data")
            .and_then(|d| d.get("retryable"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false))
    }

    /// 拉一批 → 逐个跑 handler → 回调 complete/fail。返回本批处理数量。
    /// 供自定义调度 / 单元测试用（不含 sleep 循环）。
    pub async fn poll_once<F, Fut>(
        &self,
        topic: &str,
        handler: &F,
    ) -> Result<usize, WorkerError>
    where
        F: Fn(Job) -> Fut,
        Fut: std::future::Future<Output = HandlerResult>,
    {
        let jobs = self.acquire(topic).await?;
        let n = jobs.len();
        for job in jobs {
            let id = job.job_id.clone();
            match handler(job).await {
                Ok(vars) => {
                    if let Err(e) = self.complete(&id, vars).await {
                        tracing::warn!(job = %id, error = %e, "worker complete 回调失败");
                    }
                }
                Err(reason) => {
                    if let Err(e) = self.fail(&id, &reason).await {
                        tracing::warn!(job = %id, error = %e, "worker fail 回调失败");
                    }
                }
            }
        }
        Ok(n)
    }

    /// 长轮询主循环：有作业立即再拉，无作业 sleep `poll_interval`。永不返回（除非 acquire 持续错）。
    pub async fn run<F, Fut>(&self, topic: &str, poll_interval: Duration, handler: F)
    where
        F: Fn(Job) -> Fut,
        Fut: std::future::Future<Output = HandlerResult>,
    {
        loop {
            match self.poll_once(topic, &handler).await {
                Ok(0) => tokio::time::sleep(poll_interval).await,
                Ok(_) => { /* 有作业 → 立即再拉，吃尽积压 */ }
                Err(e) => {
                    tracing::warn!(error = %e, "worker acquire 失败，退避后重试");
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalizes_root_and_v1() {
        let a = WorkerClient::new("http://h:8091", "w");
        assert_eq!(a.base, "http://h:8091/api/flow/v1");
        let b = WorkerClient::new("http://h:8091/api/flow/v1/", "w");
        assert_eq!(b.base, "http://h:8091/api/flow/v1");
    }

    #[test]
    fn job_deserializes_from_acquire_shape() {
        let j: Job = serde_json::from_value(serde_json::json!({
            "id": "j1", "instanceId": "i1", "nodeBpmnId": "svc",
            "topic": "pay", "retries": 0, "maxRetries": 3,
            "variables": { "amount": 100 }
        }))
        .unwrap();
        assert_eq!(j.job_id, "j1");
        assert_eq!(j.topic.as_deref(), Some("pay"));
        assert_eq!(j.variables.get("amount").unwrap(), 100);
    }
}
