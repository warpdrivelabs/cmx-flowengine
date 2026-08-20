-- ═══════════════════════════════════════════════════════════════════════════
-- 维度路由测试 · 产品字典（cf_product，自分级，对标 DCT cf_* 物理表）—— cmx 库（IAM_DB_ID）
--
-- 该表模拟一个「任意字典」维度：full_path 点分 code 段、parent_id 自引用（与 cmx_org 同构，
-- 但字典表名/路径列/分隔符不同——证明路由维度可泛化到组织机构之外的任意字典）。
--
-- 产品树：
--   ALL 全部产品 (ALL)
--   ├─ FIN 金融产品 (ALL.FIN)
--   │   ├─ SAVE 储蓄卡 (ALL.FIN.SAVE)
--   │   └─ CREDIT 信用卡 (ALL.FIN.CREDIT)
--   └─ INS 保险产品 (ALL.INS)
--       └─ CAR 车险 (ALL.INS.CAR)
-- 幂等：先删后插。
-- ═══════════════════════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS cf_product (
  id         VARCHAR(64)  PRIMARY KEY,
  code       VARCHAR(64),
  name       VARCHAR(128) NOT NULL,
  parent_id  VARCHAR(64),
  full_path  VARCHAR(512) NOT NULL,
  sort_no    INTEGER      NOT NULL DEFAULT 0,
  status     INTEGER      NOT NULL DEFAULT 1,
  create_time TIMESTAMPTZ NOT NULL DEFAULT now()
);

DELETE FROM cf_product WHERE id IN ('ALL','FIN','SAVE','CREDIT','INS','CAR');

INSERT INTO cf_product (id, code, name, parent_id, full_path, sort_no) VALUES
 ('ALL',    'ALL',    '全部产品', NULL,  'ALL',            0),
 ('FIN',    'FIN',    '金融产品', 'ALL', 'ALL.FIN',        1),
 ('SAVE',   'SAVE',   '储蓄卡',   'FIN', 'ALL.FIN.SAVE',   1),
 ('CREDIT', 'CREDIT', '信用卡',   'FIN', 'ALL.FIN.CREDIT', 2),
 ('INS',    'INS',    '保险产品', 'ALL', 'ALL.INS',        2),
 ('CAR',    'CAR',    '车险',     'INS', 'ALL.INS.CAR',    1);
