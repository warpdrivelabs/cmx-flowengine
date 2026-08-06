/*
 * @Describe: DefinitionService —— 流程定义的业务编排（试编译 + 版本推进）。
 *
 * 夹在 API handler 与 DefinitionStore 之间，落三条业务规则：
 *   - save_draft：先 validate_bpmn 试编译（非法 XML 挡回，带诊断）+ 校验 BPMN process id 与
 *     传入 key 一致 → upsert 草稿。
 *   - publish：取草稿 → 再试编译（双保险）→ max_version+1 追加版本行 → 标记主记录 PUBLISHED。
 *   - list / get / get_version：透传存储层。
 *   - load_published_definitions：引擎启动装载——取已发布 XML 逐个 compile 成 ProcessDefinition。
 *
 * 设计时服务（非运行态引擎），时间直接用 chrono::Utc::now()，不走引擎的可注入 Clock。
 */

use chrono::Utc;
use cmx_flow_model::ProcessDefinition;
use uuid::Uuid;

use crate::{
    DefError, DefResult, DefinitionRecord, DefinitionState, DefinitionStore, VersionMeta,
    VersionRecord, validate_bpmn,
};

/// 定义服务。持一个 DefinitionStore 实现（泛型，测试可注入内存假实现）。
pub struct DefinitionService<S: DefinitionStore> {
    store: S,
}

impl<S: DefinitionStore> DefinitionService<S> {
    /// 用指定存储构建。
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// 借用底层存储（handler 偶尔直接查）。
    pub fn store(&self) -> &S {
        &self.store
    }

    /// 建表（幂等）。
    pub async fn ensure_schema(&self) -> DefResult<()> {
        self.store.ensure_schema().await
    }

    /// 存草稿：试编译校验 + process id 一致性校验 → upsert。
    ///
    /// 返回编译出的定义 key（= BPMN process id）。key 由 XML 决定，不由调用方乱传——
    /// 若 process id 与传入 key 不一致，以 XML 里的为准并在此提示（避免存串）。
    // 8 个参数均为语义独立的定义坐标（name/domain/application/module/category）+ 载荷(bpmn_xml)
    // + 审计(updated_by)；聚成 struct 只是把同样的字段搬个位置，收益为负，故就地放行。
    #[allow(clippy::too_many_arguments)]
    pub async fn save_draft(
        &self,
        name: &str,
        domain: Option<String>,
        application: Option<String>,
        module: Option<String>,
        category: Option<String>,
        bpmn_xml: &str,
        updated_by: Option<String>,
    ) -> DefResult<DefinitionRecord> {
        // 试编译：非法 XML 直接挡回，带可读诊断。key 取自编译结果（BPMN process id）。
        let key = validate_bpmn(bpmn_xml)?;

        // 保留已有的 active_version（存草稿不影响已发布指针）。
        let existing = self.store.get(&key).await?;
        let (state, active_version) = match &existing {
            Some(r) => (r.state, r.active_version),
            None => (DefinitionState::Draft, None),
        };

        let rec = DefinitionRecord {
            key: key.clone(),
            name: name.to_string(),
            domain,
            application,
            module,
            category,
            state,
            active_version,
            draft_xml: Some(bpmn_xml.to_string()),
            updated_at: Utc::now(),
            updated_by,
        };
        self.store.upsert_draft(&rec).await?;
        Ok(rec)
    }

    /// 发布：草稿 → 版本 +1，标记已发布。返回新版本号。
    ///
    /// `note` 为本次发布的变更说明（对标报表版本的 change_summary，可空）。
    pub async fn publish(
        &self,
        key: &str,
        note: Option<String>,
        published_by: Option<String>,
    ) -> DefResult<i32> {
        let rec = self
            .store
            .get(key)
            .await?
            .ok_or_else(|| DefError::NotFound(key.to_string()))?;
        let xml = rec
            .draft_xml
            .ok_or_else(|| DefError::NoDraft(key.to_string()))?;

        // 双保险：发布前再试编译一次（草稿存进来后编译器可能已升级）。
        validate_bpmn(&xml)?;

        let next = self.store.max_version(key).await? + 1;
        let ver = VersionRecord {
            id: Uuid::new_v4().to_string(),
            def_key: key.to_string(),
            version: next,
            bpmn_xml: xml,
            note,
            published_at: Utc::now(),
            published_by,
        };
        self.store.insert_version(&ver).await?;
        self.store.mark_published(key, next).await?;
        Ok(next)
    }

    /// 列某定义的全部版本元信息（版本号降序，不含 XML）。
    pub async fn list_versions(&self, key: &str) -> DefResult<Vec<VersionMeta>> {
        self.store.list_versions(key).await
    }

    /// 列全部定义的版本元信息（handler 用于把版本聚合进定义列表）。
    pub async fn list_all_versions(&self) -> DefResult<Vec<VersionMeta>> {
        self.store.list_all_versions().await
    }

    /// 激活某个历史版本为当前生效版本（对标报表「设为默认版本」）。
    ///
    /// 把主记录的 active_version 指向指定版本、状态置 PUBLISHED，并把该版本的 XML 回填草稿区
    /// （这样设计器打开时看到的就是当前生效版本，可继续编辑再发布新版）。新 active 版本下次
    /// 服务重启由引擎装载（Arc 引擎不能再 &mut deploy，同 publish 语义）。
    pub async fn activate_version(&self, key: &str, version: i32) -> DefResult<()> {
        let ver = self
            .store
            .get_version(key, version)
            .await?
            .ok_or_else(|| DefError::NotFound(format!("{key}@v{version}")))?;
        let mut rec = self
            .store
            .get(key)
            .await?
            .ok_or_else(|| DefError::NotFound(key.to_string()))?;
        // 回填草稿为该版本 XML，并标记发布指针。
        rec.draft_xml = Some(ver.bpmn_xml);
        rec.state = DefinitionState::Published;
        rec.active_version = Some(version);
        rec.updated_at = Utc::now();
        self.store.upsert_draft(&rec).await?;
        self.store.mark_published(key, version).await?;
        Ok(())
    }

    /// 删除一个历史版本（不能删当前 active 版本——那会让引擎装载失去目标）。
    pub async fn delete_version(&self, key: &str, version: i32) -> DefResult<()> {
        let rec = self
            .store
            .get(key)
            .await?
            .ok_or_else(|| DefError::NotFound(key.to_string()))?;
        if rec.active_version == Some(version) {
            return Err(DefError::Conflict(format!(
                "版本 v{version} 是当前生效版本，不能删除；请先切换到其他版本"
            )));
        }
        self.store.delete_version(key, version).await
    }

    /// 列表（不带 XML）。
    pub async fn list(&self) -> DefResult<Vec<DefinitionRecord>> {
        self.store.list().await
    }

    /// 幂等种入一份内置定义：库里已有该 key 则跳过（不覆盖用户改动），否则存草稿。
    ///
    /// 供 demo 启动时把 include_str! 的内置 BPMN 种进定义库，让设计器列表/详情与引擎同源
    /// （内置流程也能在设计器里打开、编辑、另存）。返回是否实际种入（true=新种，false=已存在）。
    pub async fn seed_if_absent(
        &self,
        name: &str,
        module: Option<String>,
        bpmn_xml: &str,
    ) -> DefResult<bool> {
        let key = validate_bpmn(bpmn_xml)?;
        if self.store.get(&key).await?.is_some() {
            return Ok(false); // 已存在，不覆盖
        }
        self.save_draft(
            name,
            None,
            None,
            module,
            None,
            bpmn_xml,
            Some("seed".into()),
        )
        .await?;
        Ok(true)
    }

    /// 取单个定义（含草稿 XML）。
    pub async fn get(&self, key: &str) -> DefResult<Option<DefinitionRecord>> {
        self.store.get(key).await
    }

    /// 取指定历史版本。
    pub async fn get_version(&self, key: &str, version: i32) -> DefResult<Option<VersionRecord>> {
        self.store.get_version(key, version).await
    }

    /// 引擎启动装载：取所有已发布定义并编译成 ProcessDefinition。
    ///
    /// 返回 (编译成功的定义, 编译失败的诊断)。失败项不阻断其余装载——某个历史定义因编译器
    /// 演进而不再兼容时，跳过并上报，而非整体起不来。
    pub async fn load_published_definitions(
        &self,
    ) -> DefResult<(Vec<ProcessDefinition>, Vec<(String, String)>)> {
        let entries = self.store.load_published().await?;
        let mut defs = Vec::new();
        let mut errors = Vec::new();
        for e in entries {
            match cmx_flow_bpmn::compile(&e.bpmn_xml) {
                Ok(def) => defs.push(def),
                Err(err) => errors.push((format!("{}@v{}", e.key, e.version), err.to_string())),
            }
        }
        Ok((defs, errors))
    }
}
