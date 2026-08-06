//! DefinitionService 业务逻辑单测（内存假 store，始终可跑）。
//!
//! 验证：save_draft 试编译挡回非法 XML / 存草稿；publish 版本 +1、标记发布；
//! load_published 取已发布 XML。不碰 PG，纯逻辑。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use cmx_flow_def::{
    DefResult, DefinitionRecord, DefinitionService, DefinitionState, DefinitionStore,
    PublishedEntry, VersionMeta, VersionRecord,
};

/// 内存假 store：HashMap 存主记录 + Vec 存版本行。
#[derive(Default)]
struct MemStore {
    defs: Mutex<HashMap<String, DefinitionRecord>>,
    vers: Mutex<Vec<VersionRecord>>,
}

#[async_trait]
impl DefinitionStore for MemStore {
    async fn ensure_schema(&self) -> DefResult<()> {
        Ok(())
    }
    async fn upsert_draft(&self, rec: &DefinitionRecord) -> DefResult<()> {
        self.defs
            .lock()
            .unwrap()
            .insert(rec.key.clone(), rec.clone());
        Ok(())
    }
    async fn get(&self, key: &str) -> DefResult<Option<DefinitionRecord>> {
        Ok(self.defs.lock().unwrap().get(key).cloned())
    }
    async fn list(&self) -> DefResult<Vec<DefinitionRecord>> {
        Ok(self.defs.lock().unwrap().values().cloned().collect())
    }
    async fn insert_version(&self, ver: &VersionRecord) -> DefResult<()> {
        self.vers.lock().unwrap().push(ver.clone());
        Ok(())
    }
    async fn max_version(&self, def_key: &str) -> DefResult<i32> {
        Ok(self
            .vers
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.def_key == def_key)
            .map(|v| v.version)
            .max()
            .unwrap_or(0))
    }
    async fn get_version(&self, def_key: &str, version: i32) -> DefResult<Option<VersionRecord>> {
        Ok(self
            .vers
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.def_key == def_key && v.version == version)
            .cloned())
    }
    async fn list_versions(&self, def_key: &str) -> DefResult<Vec<VersionMeta>> {
        let mut v: Vec<VersionMeta> = self
            .vers
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.def_key == def_key)
            .map(|v| VersionMeta {
                def_key: v.def_key.clone(),
                version: v.version,
                note: v.note.clone(),
                published_at: v.published_at,
                published_by: v.published_by.clone(),
            })
            .collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.version));
        Ok(v)
    }
    async fn list_all_versions(&self) -> DefResult<Vec<VersionMeta>> {
        let mut v: Vec<VersionMeta> = self
            .vers
            .lock()
            .unwrap()
            .iter()
            .map(|v| VersionMeta {
                def_key: v.def_key.clone(),
                version: v.version,
                note: v.note.clone(),
                published_at: v.published_at,
                published_by: v.published_by.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.def_key.cmp(&b.def_key).then(b.version.cmp(&a.version)));
        Ok(v)
    }
    async fn delete_version(&self, def_key: &str, version: i32) -> DefResult<()> {
        self.vers
            .lock()
            .unwrap()
            .retain(|v| !(v.def_key == def_key && v.version == version));
        Ok(())
    }
    async fn mark_published(&self, key: &str, version: i32) -> DefResult<()> {
        if let Some(r) = self.defs.lock().unwrap().get_mut(key) {
            r.state = DefinitionState::Published;
            r.active_version = Some(version);
        }
        Ok(())
    }
    async fn load_published(&self) -> DefResult<Vec<PublishedEntry>> {
        let defs = self.defs.lock().unwrap();
        let vers = self.vers.lock().unwrap();
        let mut out = Vec::new();
        for r in defs.values() {
            if r.state == DefinitionState::Published
                && let Some(av) = r.active_version
                && let Some(v) = vers.iter().find(|v| v.def_key == r.key && v.version == av)
            {
                out.push(PublishedEntry {
                    key: r.key.clone(),
                    version: av,
                    bpmn_xml: v.bpmn_xml.clone(),
                });
            }
        }
        Ok(out)
    }
}

const VALID_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:flowable="http://flowable.org/bpmn">
  <process id="leave_request" name="请假申请" isExecutable="true">
    <startEvent id="start"/>
    <sequenceFlow id="s0" sourceRef="start" targetRef="approve"/>
    <userTask id="approve" name="经理审批" flowable:assignee="mgr"/>
    <sequenceFlow id="s1" sourceRef="approve" targetRef="done"/>
    <endEvent id="done"/>
  </process>
</definitions>"#;

#[tokio::test]
async fn save_draft_rejects_invalid_bpmn() {
    let svc = DefinitionService::new(MemStore::default());
    // 非良构 XML → 试编译挡回。
    let r = svc
        .save_draft("坏流程", None, None, None, None, "<not-bpmn/>", None)
        .await;
    assert!(r.is_err(), "非法 BPMN 应被 save_draft 挡回");
}

#[tokio::test]
async fn save_draft_stores_and_derives_key_from_process_id() {
    let svc = DefinitionService::new(MemStore::default());
    let rec = svc
        .save_draft(
            "请假申请",
            Some("fi".into()),
            Some("cmxfico".into()),
            Some("hr".into()),
            None,
            VALID_BPMN,
            Some("alice".into()),
        )
        .await
        .expect("合法 BPMN 应存草稿成功");
    // key 取自 BPMN process id，不由调用方乱传。
    assert_eq!(rec.key, "leave_request");
    assert_eq!(rec.state, DefinitionState::Draft);
    assert_eq!(rec.active_version, None, "刚存草稿未发布");
    // 库里可取回，含草稿 XML。
    let got = svc.get("leave_request").await.unwrap().unwrap();
    assert!(got.draft_xml.is_some());
}

#[tokio::test]
async fn publish_bumps_version_and_marks_published() {
    let svc = DefinitionService::new(MemStore::default());
    svc.save_draft("请假申请", None, None, None, None, VALID_BPMN, None)
        .await
        .unwrap();

    // 首次发布 → v1。
    let v1 = svc
        .publish("leave_request", None, Some("bob".into()))
        .await
        .unwrap();
    assert_eq!(v1, 1);
    let rec = svc.get("leave_request").await.unwrap().unwrap();
    assert_eq!(rec.state, DefinitionState::Published);
    assert_eq!(rec.active_version, Some(1));

    // 再存草稿 + 再发布 → v2（版本递增，旧版保留）。
    svc.save_draft("请假申请v2", None, None, None, None, VALID_BPMN, None)
        .await
        .unwrap();
    let v2 = svc.publish("leave_request", None, None).await.unwrap();
    assert_eq!(v2, 2);
    assert!(
        svc.get_version("leave_request", 1).await.unwrap().is_some(),
        "旧版本保留"
    );
    assert!(svc.get_version("leave_request", 2).await.unwrap().is_some());
}

#[tokio::test]
async fn publish_without_draft_errors() {
    let svc = DefinitionService::new(MemStore::default());
    let r = svc.publish("nonexistent", None, None).await;
    assert!(r.is_err(), "无草稿发布应报错");
}

#[tokio::test]
async fn load_published_returns_compilable_definitions() {
    let svc = DefinitionService::new(MemStore::default());
    svc.save_draft("请假申请", None, None, None, None, VALID_BPMN, None)
        .await
        .unwrap();
    svc.publish("leave_request", None, None).await.unwrap();

    // 引擎装载路径：取已发布 → 逐个 compile。
    let (defs, errors) = svc.load_published_definitions().await.unwrap();
    assert_eq!(defs.len(), 1, "一个已发布定义");
    assert_eq!(defs[0].key, "leave_request");
    assert!(errors.is_empty(), "编译无错");
}

#[tokio::test]
async fn version_list_activate_and_delete() {
    let svc = DefinitionService::new(MemStore::default());
    svc.save_draft("请假申请", None, None, None, None, VALID_BPMN, None)
        .await
        .unwrap();
    svc.publish("leave_request", Some("初版".into()), None)
        .await
        .unwrap(); // v1
    svc.save_draft("请假申请", None, None, None, None, VALID_BPMN, None)
        .await
        .unwrap();
    svc.publish("leave_request", Some("改办理人".into()), None)
        .await
        .unwrap(); // v2

    // 列表：两个版本，降序，带 note。
    let vers = svc.list_versions("leave_request").await.unwrap();
    assert_eq!(vers.len(), 2);
    assert_eq!(vers[0].version, 2);
    assert_eq!(vers[0].note.as_deref(), Some("改办理人"));
    assert_eq!(vers[1].version, 1);

    // 当前 active = v2；删 v2 应被挡回。
    let rec = svc.get("leave_request").await.unwrap().unwrap();
    assert_eq!(rec.active_version, Some(2));
    assert!(
        svc.delete_version("leave_request", 2).await.is_err(),
        "当前版本不能删"
    );

    // 激活 v1 → active_version 回到 1，草稿回填 v1 XML。
    svc.activate_version("leave_request", 1).await.unwrap();
    let rec = svc.get("leave_request").await.unwrap().unwrap();
    assert_eq!(rec.active_version, Some(1));
    assert!(rec.draft_xml.is_some());

    // 现在 v2 非当前，可删。
    svc.delete_version("leave_request", 2).await.unwrap();
    assert_eq!(svc.list_versions("leave_request").await.unwrap().len(), 1);
}
