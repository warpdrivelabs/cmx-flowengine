-- docker-compose 初始化：建 IAM 候选人库 cmx（流程运行库 fico 已由 POSTGRES_DB 建）。
-- flow-server 的 IAM_PG_URL 指向它（pg 身份适配器模式用；mock 模式不访问）。
SELECT 'CREATE DATABASE cmx' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'cmx')\gexec
