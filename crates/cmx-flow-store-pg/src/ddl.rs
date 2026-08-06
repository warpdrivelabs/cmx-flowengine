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
        created_at       TIMESTAMPTZ  NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_job_instance ON cmx_flow_job (instance_id)",
    "CREATE INDEX IF NOT EXISTS idx_cmx_flow_job_due ON cmx_flow_job (due_at)",
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
    // —— 抄送记录表（M4.2：只读知会 + 已读追踪；随快照全删重插，实例终态随聚合归档） —— //
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
    // —— 转签台账表（M4.3：转办/加签/委派流转链；随快照全删重插） —— //
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
];
