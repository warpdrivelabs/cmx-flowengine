-- ═══════════════════════════════════════════════════════════════════════════
-- 子流程测试 · 组织树扩展（cmx 库）—— 增加深层组织以测「沿 path 多级继承」
-- 现有：zongbu(/zongbu) ├ fin_bj(/zongbu/fin_bj) └ fin_sh(/zongbu/fin_sh)
-- 新增：fin_bj 下的小组 fin_bj_g1(/zongbu/fin_bj/fin_bj_g1)  —— 用于「孙组织继承祖父绑定」
--       独立分公司 branch_gz(/branch_gz) —— 用于「有自身绑定，不继承总部」
-- 幂等：先删后插。不动既有 zongbu/fin_bj/fin_sh。
-- ═══════════════════════════════════════════════════════════════════════════
DELETE FROM cmx_user WHERE id IN ('u_bjg1','u_gz1');
DELETE FROM cmx_org  WHERE id IN ('fin_bj_g1','branch_gz');

INSERT INTO cmx_org(id,code,name,parent_id,path,leader_user_id,sort_order,status,archived,create_time) VALUES
 ('fin_bj_g1','FBJG1','北京财务一组','fin_bj','/zongbu/fin_bj/fin_bj_g1','u_bjg1',0,1,0,now()),
 ('branch_gz','BGZ','广州分公司','zongbu','/branch_gz',null,0,1,0,now());  -- path 不在 /zongbu 下，独立根

INSERT INTO cmx_user(id,username,nickname,org_id,status,archived,create_time,update_time) VALUES
 ('u_bjg1','u_bjg1','北京一组员','fin_bj_g1',1,0,now(),now()),
 ('u_gz1','u_gz1','广州员工','branch_gz',1,0,now(),now());
