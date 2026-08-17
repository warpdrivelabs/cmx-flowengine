-- ═══════════════════════════════════════════════════════════════════════════
-- cmx-flowengine 全量测试 · IAM 种子数据（cmx 库，PgIamAssigneeResolver 读 cmx_* 表）
-- 目标：让 role/position/org/orgLeader/initiator 各类「指定人员」都能解析到真人。
-- 幂等：仅清理本测试引入的 u_* 用户与 finance/manager/... 角色岗位，绝不动 admin。
-- 解析口径（源码核对）：
--   role(code)     : cmx_user_role ⨝ cmx_role(code) ，archived=0
--   position(code) : cmx_user_position ⨝ cmx_position(code)，archived=0
--   org(id)        : cmx_user.org_id ∈ 子树(cmx_org.path LIKE root.path||'%')
--   orgLeader(id)  : cmx_org.leader_user_id
-- ═══════════════════════════════════════════════════════════════════════════

-- 幂等清理（只清测试行）
DELETE FROM cmx_user_role     WHERE user_id LIKE 'u\_%';
DELETE FROM cmx_user_position WHERE user_id LIKE 'u\_%';
DELETE FROM cmx_user          WHERE id      LIKE 'u\_%';
DELETE FROM cmx_role          WHERE id IN ('finance','manager','director','cashier','auditor');
DELETE FROM cmx_position      WHERE id IN ('cfo','fin_mgr','clerk');

-- 组织领导（复用已有 zongbu/fin_bj/fin_sh，补 leader）
UPDATE cmx_org SET leader_user_id='u_ceo'    WHERE id='zongbu';
UPDATE cmx_org SET leader_user_id='u_bjlead' WHERE id='fin_bj';
UPDATE cmx_org SET leader_user_id='u_shlead' WHERE id='fin_sh';

-- 角色（id=code 便于对照）
INSERT INTO cmx_role(id,code,name,status,archived,create_time) VALUES
 ('finance','finance','财务角色',1,0,now()),
 ('manager','manager','经理角色',1,0,now()),
 ('director','director','总监角色',1,0,now()),
 ('cashier','cashier','出纳角色',1,0,now()),
 ('auditor','auditor','审计角色',1,0,now());

-- 岗位
INSERT INTO cmx_position(id,code,name,org_id,status,archived,create_time) VALUES
 ('cfo','cfo','首席财务官','zongbu',1,0,now()),
 ('fin_mgr','fin_mgr','财务经理','fin_bj',1,0,now()),
 ('clerk','clerk','财务专员','fin_bj',1,0,now());

-- 用户（friendly id 直接用于 /tasks/my?assignee=<id>）
INSERT INTO cmx_user(id,username,nickname,org_id,status,archived,create_time,update_time) VALUES
 ('u_ceo','u_ceo','首席执行官','zongbu',1,0,now(),now()),
 ('u_bjlead','u_bjlead','北京财务负责人','fin_bj',1,0,now(),now()),
 ('u_shlead','u_shlead','上海财务负责人','fin_sh',1,0,now(),now()),
 ('u_fin1','u_fin1','财务专员1','fin_bj',1,0,now(),now()),
 ('u_fin2','u_fin2','财务专员2','fin_bj',1,0,now(),now()),
 ('u_fin3','u_fin3','财务专员3','fin_sh',1,0,now(),now()),
 ('u_cashier1','u_cashier1','出纳1','fin_bj',1,0,now(),now()),
 ('u_auditor1','u_auditor1','审计1','zongbu',1,0,now(),now()),
 ('u_cfo','u_cfo','财务总监CFO','zongbu',1,0,now(),now());

-- 用户-角色
INSERT INTO cmx_user_role(id,user_id,role_id,archived,create_time) VALUES
 ('ur1','u_ceo','manager',0,now()),
 ('ur2','u_ceo','director',0,now()),
 ('ur3','u_bjlead','manager',0,now()),
 ('ur4','u_shlead','manager',0,now()),
 ('ur5','u_fin1','finance',0,now()),
 ('ur6','u_fin2','finance',0,now()),
 ('ur7','u_fin3','finance',0,now()),
 ('ur8','u_cashier1','cashier',0,now()),
 ('ur9','u_auditor1','auditor',0,now()),
 ('ur10','u_cfo','director',0,now());

-- 用户-岗位
INSERT INTO cmx_user_position(id,user_id,position_id,is_primary,archived,create_time) VALUES
 ('up1','u_cfo','cfo',true,0,now()),
 ('up2','u_bjlead','fin_mgr',true,0,now()),
 ('up3','u_fin1','clerk',true,0,now());
