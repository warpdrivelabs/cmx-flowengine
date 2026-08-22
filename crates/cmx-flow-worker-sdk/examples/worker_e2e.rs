//! 真机 E2E：部署 external-worker BPMN → 发起实例（作业挂 topic）→ SDK 抢占+完成 → 验证令牌推进。
//! 需 flow-server 在 127.0.0.1:8091（off 模式）。用法：cargo run -p cmx-flow-worker-sdk --example worker_e2e
use cmx_flow_worker_sdk::{HandlerResult, WorkerClient};

const ROOT: &str = "http://127.0.0.1:8091";
const V1: &str = "http://127.0.0.1:8091/api/flow/v1";
const BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:flowable="http://flowable.org/bpmn" id="d_sdk_ext" targetNamespace="http://cmx.io/flow/test">
 <bpmn:process id="sdk_ext_worker" name="SDK外部Worker探针" isExecutable="true">
  <bpmn:startEvent id="s"><bpmn:outgoing>f1</bpmn:outgoing></bpmn:startEvent>
  <bpmn:serviceTask id="pay" name="外部支付" flowable:type="external-worker" flowable:topic="sdk-pay"><bpmn:incoming>f1</bpmn:incoming><bpmn:outgoing>f2</bpmn:outgoing></bpmn:serviceTask>
  <bpmn:userTask id="approve" name="确认"><bpmn:incoming>f2</bpmn:incoming><bpmn:outgoing>f3</bpmn:outgoing></bpmn:userTask>
  <bpmn:endEvent id="e"><bpmn:incoming>f3</bpmn:incoming></bpmn:endEvent>
  <bpmn:sequenceFlow id="f1" sourceRef="s" targetRef="pay"/>
  <bpmn:sequenceFlow id="f2" sourceRef="pay" targetRef="approve"/>
  <bpmn:sequenceFlow id="f3" sourceRef="approve" targetRef="e"/>
 </bpmn:process>
</bpmn:definitions>"#;

async fn post(http: &reqwest::Client, path: &str, body: serde_json::Value) -> serde_json::Value {
    http.post(format!("{V1}{path}")).header("X-Tenant", "default").json(&body).send().await.unwrap().json().await.unwrap()
}

#[tokio::main]
async fn main() {
    let http = reqwest::Client::new();
    // 1) 部署（草稿+发布）。
    let draft = post(&http, "/definitions/draft", serde_json::json!({"name":"SDK外部Worker探针","bpmnXml":BPMN})).await;
    let key = draft["data"]["key"].as_str().unwrap().to_string();
    post(&http, &format!("/definitions/{key}/publish"), serde_json::json!({"note":"sdk-e2e"})).await;
    // 2) 发起 → 令牌停在 external-worker 作业。
    let inst = post(&http, "/instances", serde_json::json!({"definitionKey":key,"variables":{"initiator":"boss","amount":8888}})).await;
    let iid = inst["data"]["id"].as_str().unwrap().to_string();
    println!("instance={iid}  active before worker = {:?}", inst["data"]["activeNodes"]);
    // 3) SDK 抢占 + 完成。
    let client = WorkerClient::new(ROOT, "sdk-e2e-worker");
    let handled = client.poll_once("sdk-pay", &|job| async move {
        println!("  worker 领到作业 job={} node={:?} vars={}", job.job_id, job.node_bpmn_id, job.variables);
        HandlerResult::Ok(serde_json::json!({ "paid": true, "gateway": "alipay" }))
    }).await.unwrap();
    println!("worker 处理作业数={handled}");
    // 4) 验证令牌已推进到 userTask（svc 已完成）。
    let after: serde_json::Value = http.get(format!("{V1}/instances/{iid}")).header("X-Tenant","default").send().await.unwrap().json().await.unwrap();
    let active = &after["data"]["activeNodes"];
    let paid = &after["data"]["variables"]["paid"];
    println!("active after worker = {active}   variables.paid = {paid}");
    assert!(handled >= 1, "应至少领到 1 个作业");
    assert!(active.to_string().contains("approve"), "svc 完成后令牌应推进到 approve");
    assert_eq!(paid, &serde_json::json!(true), "worker 写回变量 paid 应合并进实例");
    println!("\n✅ SDK E2E 通过：抢占 → 执行 → complete 回写变量 → 令牌推进到 approve");
}
