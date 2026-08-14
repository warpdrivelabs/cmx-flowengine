/*
 * @Describe: 内建身份模块 `fid_*` 表结构 DDL（幂等）。
 *
 * 命名铁律：**fid_ 前缀**（flow-identity），绝不叫 cmx_user/cmx_org——本模块是流程微服务自持的
 * 可选身份插件，不冒充平台 IAM。表形态对齐外接 IAM 的语义（org 带 leader_user_id 支持关系型
 * 审批人解析），但独立命名空间，与外接 IAM 各存各的。
 *
 * 六张表（禁外键，索引替代；DDL 幂等 IF NOT EXISTS）：
 *   fid_org           —— 组织机构（树，parent_id + path，leader_user_id 支持部门领导）
 *   fid_role          —— 角色
 *   fid_position      —— 岗位
 *   fid_user          —— 用户（org_id 指向所属组织）
 *   fid_user_role     —— 用户-角色关联
 *   fid_user_position —— 用户-岗位关联
 */

/// 建表 DDL（幂等）。按顺序执行。
pub const DDL_STATEMENTS: &[&str] = &[
    // —— 组织机构 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_org (
        id             VARCHAR(64)  PRIMARY KEY,
        code           VARCHAR(100) NOT NULL,
        name           VARCHAR(200) NOT NULL,
        parent_id      VARCHAR(64),
        path           VARCHAR(500),
        leader_user_id VARCHAR(64),
        sort_order     INTEGER      NOT NULL DEFAULT 0,
        archived       INTEGER      NOT NULL DEFAULT 0,
        created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_org_parent ON fid_org (parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_fid_org_code ON fid_org (code)",
    // —— 角色 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_role (
        id         VARCHAR(64)  PRIMARY KEY,
        code       VARCHAR(100) NOT NULL,
        name       VARCHAR(200) NOT NULL,
        archived   INTEGER      NOT NULL DEFAULT 0,
        created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at TIMESTAMPTZ  NOT NULL DEFAULT now()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_role_code ON fid_role (code)",
    // —— 岗位 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_position (
        id         VARCHAR(64)  PRIMARY KEY,
        code       VARCHAR(100) NOT NULL,
        name       VARCHAR(200) NOT NULL,
        archived   INTEGER      NOT NULL DEFAULT 0,
        created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at TIMESTAMPTZ  NOT NULL DEFAULT now()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_position_code ON fid_position (code)",
    // —— 用户 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_user (
        id         VARCHAR(64)  PRIMARY KEY,
        username   VARCHAR(100) NOT NULL,
        name       VARCHAR(200),
        org_id     VARCHAR(64),
        archived   INTEGER      NOT NULL DEFAULT 0,
        created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
        updated_at TIMESTAMPTZ  NOT NULL DEFAULT now()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_user_org ON fid_user (org_id)",
    "CREATE INDEX IF NOT EXISTS idx_fid_user_username ON fid_user (username)",
    // —— 用户-角色 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_user_role (
        user_id  VARCHAR(64) NOT NULL,
        role_id  VARCHAR(64) NOT NULL,
        archived INTEGER     NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, role_id)
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_user_role_role ON fid_user_role (role_id)",
    // —— 用户-岗位 —— //
    r#"CREATE TABLE IF NOT EXISTS fid_user_position (
        user_id     VARCHAR(64) NOT NULL,
        position_id VARCHAR(64) NOT NULL,
        archived    INTEGER     NOT NULL DEFAULT 0,
        PRIMARY KEY (user_id, position_id)
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_fid_user_position_pos ON fid_user_position (position_id)",
];
