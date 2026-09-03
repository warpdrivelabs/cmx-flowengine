/*
 * @Describe: 流程引擎 PG 持久化的表结构 DDL（幂等）。
 *
 * 遵循项目硬约束：表名 `cmx_` 前缀；禁外键，用索引替代关联查询；DDL 幂等
 * （IF NOT EXISTS）。三张运行态表：
 *   cmx_flow_instance —— 流程实例（含 variables jsonb）
 *   cmx_flow_token    —— 令牌（当前所在节点 bpmn_id + 状态）
 *   cmx_flow_task     —— 用户任务（等待态外化）
 *
 * 正式接入时应改由 sql-guide/pg-table-generator 技能纳管到 docs/sql；此处内置 DDL 仅为
 * M1 自举与测试可独立跑起来。生产迁移请以 docs/sql 为准。
 */

/// 建表 DDL（幂等）。按顺序执行。
pub const DDL_STATEMENTS: &[&str] = &[
    // —— 实例表 —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_instance (
        id                 VARCHAR(64)  PRIMARY KEY,
        definition_key     VARCHAR(128) NOT NULL,
        business_key       VARCHAR(128),
        state              VARCHAR(16)  NOT NULL,
        variables          JSONB        NOT NULL DEFAULT '{}'::jsonb,
        created_at         TIMESTAMPTZ  NOT NULL,
        updated_at         TIMESTAMPTZ  NOT NULL,
        ended_at           TIMESTAMPTZ,
        org_id             VARCHAR(64),
        parent_instance_id VARCHAR(64),
        parent_token_id    VARCHAR(64),
        parent_node_bpmn_id VARCHAR(128)
    )"#,
    // 幂等补列：既有库升级到 M5 时补上子流程父子/组织列。
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS org_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS parent_instance_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS parent_token_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS parent_node_bpmn_id VARCHAR(128)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_instance_defkey ON cmx_flow_instance (definition_key)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_instance_bizkey ON cmx_flow_instance (business_key)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_instance_state ON cmx_flow_instance (state)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_instance_parent ON cmx_flow_instance (parent_instance_id)",
    // —— 令牌表 —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_token (
        id            VARCHAR(64)  PRIMARY KEY,
        instance_id   VARCHAR(64)  NOT NULL,
        node_bpmn_id  VARCHAR(128) NOT NULL,
        state         VARCHAR(16)  NOT NULL,
        parent_id     VARCHAR(64),
        created_at    TIMESTAMPTZ  NOT NULL,
        updated_at    TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_token_instance ON cmx_flow_token (instance_id)",
    // —— 任务表 —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_task (
        id               VARCHAR(64)  PRIMARY KEY,
        instance_id      VARCHAR(64)  NOT NULL,
        token_id         VARCHAR(64)  NOT NULL,
        node_bpmn_id     VARCHAR(128) NOT NULL,
        name             VARCHAR(255),
        assignee         VARCHAR(128),
        candidate_groups VARCHAR(512),
        element_value    JSONB,
        owner_user_id    VARCHAR(64),
        parent_task_id   VARCHAR(64),
        delegation_state VARCHAR(16),
        completed        BOOLEAN      NOT NULL DEFAULT FALSE,
        created_at       TIMESTAMPTZ  NOT NULL,
        completed_at     TIMESTAMPTZ
    )"#,
    // 幂等补列：既有库（M1/M2 建的表）升级时补上后加的列。
    "ALTER TABLE cmx_flow_task ADD COLUMN IF NOT EXISTS element_value JSONB",
    "ALTER TABLE cmx_flow_task ADD COLUMN IF NOT EXISTS owner_user_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_task ADD COLUMN IF NOT EXISTS parent_task_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_task ADD COLUMN IF NOT EXISTS delegation_state VARCHAR(16)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_instance ON cmx_flow_task (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_assignee ON cmx_flow_task (assignee)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_open ON cmx_flow_task (assignee, completed)",
    // —— 多实例执行域表（会签/或签账本；随快照全删重插，与令牌/任务同生命周期） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_mi_scope (
        id                   VARCHAR(64)  PRIMARY KEY,
        instance_id          VARCHAR(64)  NOT NULL,
        node_bpmn_id         VARCHAR(128) NOT NULL,
        sequential           BOOLEAN      NOT NULL DEFAULT FALSE,
        total                INTEGER      NOT NULL,
        completed            INTEGER      NOT NULL DEFAULT 0,
        next_index           INTEGER      NOT NULL DEFAULT 0,
        collection           JSONB        NOT NULL DEFAULT '[]'::jsonb,
        element_var          VARCHAR(128),
        completion_condition VARCHAR(512),
        finished             BOOLEAN      NOT NULL DEFAULT FALSE
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_mi_scope_instance ON cmx_flow_mi_scope (instance_id)",
    // —— 定时器作业表（M2.5：边界定时器到期表；随快照全删重插，与令牌同生命周期） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_job (
        id               VARCHAR(64)  PRIMARY KEY,
        instance_id      VARCHAR(64)  NOT NULL,
        token_id         VARCHAR(64)  NOT NULL,
        boundary_bpmn_id VARCHAR(128) NOT NULL,
        cancel_activity  BOOLEAN      NOT NULL DEFAULT TRUE,
        due_at           TIMESTAMPTZ  NOT NULL,
        created_at       TIMESTAMPTZ  NOT NULL,
        kind                   VARCHAR(24) NOT NULL DEFAULT 'BOUNDARY',
        cycle_interval_seconds BIGINT,
        cycle_remaining        INTEGER
    )"#,
    // 幂等补列：既有库（M2.5 建的表）升级到 A1/A5 时补上作业类型 + 周期列。
    "ALTER TABLE cmx_flow_job ADD COLUMN IF NOT EXISTS kind VARCHAR(24) NOT NULL DEFAULT 'BOUNDARY'",
    "ALTER TABLE cmx_flow_job ADD COLUMN IF NOT EXISTS cycle_interval_seconds BIGINT",
    "ALTER TABLE cmx_flow_job ADD COLUMN IF NOT EXISTS cycle_remaining INTEGER",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_job_instance ON cmx_flow_job (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_job_due ON cmx_flow_job (due_at)",
    // 技术债 008 定时器抢占：租约列（对齐 async_job 的 locked_by/lock_expires_at 模式）。
    "ALTER TABLE cmx_flow_job ADD COLUMN IF NOT EXISTS claimed_by VARCHAR(128)",
    "ALTER TABLE cmx_flow_job ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_job_acquire ON cmx_flow_job (due_at, claimed_by, lease_expires_at)",
    // —— 任务候选人池表（M4.1：多人候选待认领；随快照全删重插，与任务同生命周期） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_task_candidate (
        id               VARCHAR(64)  PRIMARY KEY,
        task_id          VARCHAR(64)  NOT NULL,
        instance_id      VARCHAR(64)  NOT NULL,
        candidate_type   VARCHAR(16)  NOT NULL,
        candidate_ref    VARCHAR(128) NOT NULL,
        resolved_user_id VARCHAR(64)  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_candidate_instance ON cmx_flow_task_candidate (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_candidate_user ON cmx_flow_task_candidate (resolved_user_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_candidate_task ON cmx_flow_task_candidate (task_id)",
    // —— 抄送记录表（M4.2：只读知会 + 已读追踪；007 旁路剥离——不随快照删重插，
    //     insert_cc 走 ON CONFLICT 幂等追加且不覆盖 read_at，实例终态随 010 retention 清理） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_cc (
        id            VARCHAR(64)  PRIMARY KEY,
        instance_id   VARCHAR(64)  NOT NULL,
        node_bpmn_id  VARCHAR(128),
        to_user_id    VARCHAR(64)  NOT NULL,
        from_user_id  VARCHAR(64),
        reason        VARCHAR(500),
        read_at       TIMESTAMPTZ,
        created_at    TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_cc_instance ON cmx_flow_cc (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_cc_to_user ON cmx_flow_cc (to_user_id, read_at)",
    // —— 转签台账表（M4.3：转办/加签/委派流转链；007 旁路剥离——不随快照删重插，
    //     insert_delegation 走 ON CONFLICT DO NOTHING 幂等追加） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_task_delegation (
        id            VARCHAR(64)  PRIMARY KEY,
        task_id       VARCHAR(64)  NOT NULL,
        instance_id   VARCHAR(64)  NOT NULL,
        kind          VARCHAR(20)  NOT NULL,
        from_user_id  VARCHAR(64)  NOT NULL,
        to_user_id    VARCHAR(64)  NOT NULL,
        temp_task_id  VARCHAR(64),
        reason        VARCHAR(500),
        created_at    TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_delegation_instance ON cmx_flow_task_delegation (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_task_delegation_task ON cmx_flow_task_delegation (task_id)",
    // —— 历史实例表（RU/HI 分离：实例终态时归档，供审计/查询，与热运行态解耦） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_hi_instance (
        id             VARCHAR(64)  PRIMARY KEY,
        definition_key VARCHAR(128) NOT NULL,
        business_key   VARCHAR(128),
        state          VARCHAR(16)  NOT NULL,
        variables      JSONB        NOT NULL DEFAULT '{}'::jsonb,
        created_at     TIMESTAMPTZ  NOT NULL,
        ended_at       TIMESTAMPTZ,
        duration_ms    BIGINT,
        archived_at    TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_instance_defkey ON cmx_flow_hi_instance (definition_key)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_instance_bizkey ON cmx_flow_hi_instance (business_key)",
    // —— 历史任务表（办结任务归档，含耗时，供工时分析/审计） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_hi_task (
        id            VARCHAR(64)  PRIMARY KEY,
        instance_id   VARCHAR(64)  NOT NULL,
        node_bpmn_id  VARCHAR(128) NOT NULL,
        name          VARCHAR(255),
        assignee      VARCHAR(128),
        created_at    TIMESTAMPTZ  NOT NULL,
        completed_at  TIMESTAMPTZ,
        duration_ms   BIGINT,
        archived_at   TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_task_instance ON cmx_flow_hi_task (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_task_assignee ON cmx_flow_hi_task (assignee)",
    // —— 子流程组织绑定表（M5.2：逻辑 key + 组织 → 具体子流程定义；定义态配置，非实例聚合） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_subflow_binding (
        id                    VARCHAR(64)  PRIMARY KEY,
        called_key            VARCHAR(128) NOT NULL,
        org_id                VARCHAR(64),
        target_definition_key VARCHAR(128) NOT NULL,
        enabled               BOOLEAN      NOT NULL DEFAULT TRUE,
        remark                VARCHAR(500),
        created_at            TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at            TIMESTAMPTZ  NOT NULL DEFAULT now()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_key ON cmx_flow_subflow_binding (called_key)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_org ON cmx_flow_subflow_binding (org_id)",
    // —— RD0/RD2 路由维度泛化：绑定维度从写死 org_id 泛化为 (dim_key, dim_value) ——
    // dim_key 默认 'org'（= 组织维度，向后兼容 M5.2）；dim_value 是原 org_id 的泛化（某维度字典的条目 id/code）。
    "ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_key   VARCHAR(64)  NOT NULL DEFAULT 'org'",
    "ALTER TABLE cmx_flow_subflow_binding ADD COLUMN IF NOT EXISTS dim_value VARCHAR(128)",
    // 老数据迁移：org_id → dim_value（dim_key 已由 DEFAULT 填 'org'）。幂等：仅迁尚未迁的行。
    "UPDATE cmx_flow_subflow_binding SET dim_value = org_id WHERE dim_value IS NULL AND org_id IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_subflow_binding_dim ON cmx_flow_subflow_binding (called_key, dim_key, dim_value)",
    // —— RD0/RD3 实例维度上下文：dim_key → dim_value 多维取值（org 维度仍可用 org_id 标量列兼容）——
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS dimensions JSONB",
    // —— 技术债 007：实例乐观锁版本列（JPA @Version / Flowable OPTLOCK 同款）——
    // save 以 WHERE id=$1 AND version=$2 CAS 提交并 +1；0 行 = 并发覆盖冲突。
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS version BIGINT NOT NULL DEFAULT 0",
    "ALTER TABLE cmx_flow_hi_instance ADD COLUMN IF NOT EXISTS version BIGINT NOT NULL DEFAULT 0",
    // —— 技术债 005：发起方业务系统归属列（结构化 key 声明 TenantCtx.system 落库）——
    // NULL = legacy 调用未声明系统；结构化 key 的实例带 system_id，供归属过滤与命名空间审计。
    "ALTER TABLE cmx_flow_instance ADD COLUMN IF NOT EXISTS system_id VARCHAR(64)",
    "ALTER TABLE cmx_flow_hi_instance ADD COLUMN IF NOT EXISTS system_id VARCHAR(64)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_instance_system ON cmx_flow_instance (system_id)",
    // —— 消息订阅表（P3 消息等待持久化 + A2 消息启动索引；重启后订阅不丢） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_message_subscription (
        id              VARCHAR(64)  PRIMARY KEY,
        kind            VARCHAR(16)  NOT NULL,
        message_name    VARCHAR(255) NOT NULL,
        instance_id     VARCHAR(64),
        token_id        VARCHAR(64),
        node_bpmn_id    VARCHAR(128) NOT NULL,
        correlation_var VARCHAR(128),
        definition_key  VARCHAR(128),
        tenant_id       VARCHAR(64)  NOT NULL DEFAULT 'default',
        created_at      TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_msg_sub_name_tenant ON cmx_flow_message_subscription (message_name, tenant_id, kind)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_msg_sub_instance ON cmx_flow_message_subscription (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_msg_sub_def ON cmx_flow_message_subscription (definition_key, kind)",
    // —— 异步服务任务作业表（P1：serviceTask flowable:async="true"；SKIP LOCKED 集群抢占） —— //
    // 独立侧表，不随快照全删重插：worker 写的 locked_by/lock_expires_at 只能由 acquire/fail 改，
    // 若混进 save_snapshot 的删+插循环，任一并发 save 都会抹掉锁 → 重复领取、delegate 二次执行。
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_async_job (
        id                    VARCHAR(64)  PRIMARY KEY,
        instance_id           VARCHAR(64)  NOT NULL,
        token_id              VARCHAR(64)  NOT NULL,
        node_bpmn_id          VARCHAR(128) NOT NULL,
        delegate_key          VARCHAR(255) NOT NULL,
        topic                 VARCHAR(128),
        max_retries           INTEGER      NOT NULL DEFAULT 3,
        retries               INTEGER      NOT NULL DEFAULT 3,
        retry_backoff_seconds BIGINT,
        locked_by             VARCHAR(128),
        lock_expires_at       TIMESTAMPTZ,
        created_at            TIMESTAMPTZ  NOT NULL
    )"#,
    // 幂等补列：既有库（P1 建的表）升级到 A7 时补上 topic 列（外部 worker 主题）。
    "ALTER TABLE cmx_flow_async_job ADD COLUMN IF NOT EXISTS topic VARCHAR(128)",
    // 抢占扫描索引：按可领取性（未锁/锁超期）+ 创建时序取队首。
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_async_job_acquire ON cmx_flow_async_job (locked_by, lock_expires_at, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_async_job_instance ON cmx_flow_async_job (instance_id)",
    // A7：外部 worker 按 topic 拉取的扫描索引。
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_async_job_topic ON cmx_flow_async_job (topic, locked_by, lock_expires_at)",
    // —— 死信作业表（P2：异步 Job 重试耗尽的托底；运维台可见可重投可删除） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_deadletter_job (
        id                  VARCHAR(64)  PRIMARY KEY,
        instance_id         VARCHAR(64)  NOT NULL,
        token_id            VARCHAR(64)  NOT NULL,
        node_bpmn_id        VARCHAR(128) NOT NULL,
        delegate_key        VARCHAR(255) NOT NULL,
        max_retries         INTEGER      NOT NULL DEFAULT 3,
        error               TEXT         NOT NULL DEFAULT '',
        original_created_at TIMESTAMPTZ  NOT NULL,
        dead_lettered_at    TIMESTAMPTZ  NOT NULL,
        tenant_id           VARCHAR(64)  NOT NULL DEFAULT 'default'
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_deadletter_instance ON cmx_flow_deadletter_job (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_deadletter_time ON cmx_flow_deadletter_job (dead_lettered_at)",
    // —— 活动历史表（A6：节点级进出时段，驱动 SLA 看板/审计回放/时效分析） —— //
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_hi_activity (
        id               VARCHAR(64)  PRIMARY KEY,
        instance_id      VARCHAR(64)  NOT NULL,
        token_id         VARCHAR(64)  NOT NULL,
        activity_bpmn_id VARCHAR(128) NOT NULL,
        activity_name    VARCHAR(255),
        activity_type    VARCHAR(48)  NOT NULL,
        entered_at       TIMESTAMPTZ  NOT NULL,
        exited_at        TIMESTAMPTZ  NOT NULL,
        duration_ms      BIGINT       NOT NULL DEFAULT 0,
        assignee         VARCHAR(64),
        tenant_id        VARCHAR(64)  NOT NULL DEFAULT 'default'
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_activity_instance ON cmx_flow_hi_activity (instance_id, entered_at)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_activity_type ON cmx_flow_hi_activity (activity_type)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_hi_activity_assignee ON cmx_flow_hi_activity (assignee)",
    // —— 故障清单表（技术债 011：跨实例 incident 台账，/incidents 清单端点 + 自动重试数据源） ——
    r#"CREATE TABLE IF NOT EXISTS cmx_flow_incident (
        id              VARCHAR(64)  PRIMARY KEY,
        instance_id     VARCHAR(64)  NOT NULL,
        token_id        VARCHAR(64),
        node_bpmn_id    VARCHAR(128) NOT NULL,
        definition_key  VARCHAR(128) NOT NULL,
        business_key    VARCHAR(128),
        reason          TEXT         NOT NULL DEFAULT '',
        retries         INTEGER      NOT NULL DEFAULT 0,
        state           VARCHAR(16)  NOT NULL DEFAULT 'OPEN',
        created_at      TIMESTAMPTZ  NOT NULL,
        updated_at      TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_cmx_flow_incident_inst_node ON cmx_flow_incident (instance_id, node_bpmn_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_incident_state ON cmx_flow_incident (state, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_incident_def ON cmx_flow_incident (definition_key)",
];
