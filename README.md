# cmx-flowengine —— 独立流程引擎微服务

从 cmx-container 抽出的**独立 Cargo workspace**，承载 cmx-flow 流程引擎与框架无关的全部 crate。目标：可独立部署、独立运行、独立升级、支持多租户的纯流程引擎微服务（身份/组织/表单外部化，前端一芯三壳）。

> 完整架构方案见 [`docs/standalone-microservice-design.html`](docs/standalone-microservice-design.html)（v2：身份/组织/表单外部化 + 一芯三壳 + headless 契约 + S0–S6 路线）。

## 现状：S0（迁移骨架 + 能跑）

本库当前完成 **S0**——引擎 crate 已从 cmx-container 物理迁入本 workspace，编译独立成立，自带 demo 可独立运行；平台（cmx-container）经保留的 `cmx-flow-api` 适配层跨 workspace 路径引用本库，功能零回归。

后续里程碑（S1 四外部适配器 / S2 多租户认证 / S3 headless API / S4 前端抽核 / S5 可嵌组件 / S6 center_client 对接）见方案文档。

## crate 布局

```
crates/
  cmx-flow-model     引擎语义中立内核：ProcessDefinition IR + 运行态 DTO + RuntimeStore 契约（零 cmx 依赖）
  cmx-flow-bpmn      BPMN 2.0 XML → IR 编译器
  cmx-flow-engine    令牌执行内核（等待态即提交点）+ delegate 注册表 + InMemoryStore
  cmx-flow-def       流程定义持久化（草稿/发布/版本/装载）
  cmx-flow-store-pg  RuntimeStore 的 PostgreSQL 实现 + IAM 候选人 resolver + 子流程组织 router
  cmx-flow-demo      自包含 axum 演示服务（独立可执行，内联 SPA，落 PG）
  cmx-flow-tests     引擎端到端测试（默认内存态，无外部依赖）
```

## 依赖策略

- **域内 flow crate**：纯 `path`。
- **基础设施**（`cmx-database-pg` / `cmx-core`）：以跨 workspace `path` 复用 cmx-container 的成员 crate——它们仍属 cmx-container workspace（对其根解析 `workspace=true` 与 `[patch.nora]`），故本库**无需 nora 私仓**、**不把 infra 纳入 members**。
- **外部 crate**：走 aliyun 镜像（见 `.cargo/config.toml`），版本与 cmx-container 根对齐以便 Cargo 合一。

> 因此：本库构建依赖 `../cmx-container/` 就在旁边（sibling 目录）。

## 快速开始

```bash
# 编译全部 crate
cargo build

# 引擎测试（内存态，无需 PG）
cargo test -p cmx-flow-tests

# 独立运行 demo（需本地 PG；默认 fico + cmx 两库）
FICO_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/fico \
IAM_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/cmx \
  cargo run -p cmx-flow-demo
# 浏览器打开 http://127.0.0.1:8090
```

## 数据读取约定（ZmcDataSet vs DataSet）

flow 读库时按「结果集的形态与去向」二选一，与 cmx-container 平台其它模块（DOC/DCT/RPT）保持一致——**不为统一而统一**：

- ✅ **用 `ZmcDataSet`**：当需要**对外返回一个业务行数据集**——把某表/视图的 N 行（可能很大）整体交给消费方。例：报表取数、单据列表二进制出口、给第三方的批量行导出。
  走 cmx-database-pg 的 `manager.query_sql_zmc(db_id, sql, dataset_id)`（或 `_with_datavalues` / `_stream_chunks` 流式），再 `encode_columnar_binary` 出列式 msgpack。这是驱动无关、零拷贝、可流式的出口（省 `DataValue` 拷贝 + msgpack + 峰值内存 O(单行)）。范例见 `cmx-doc` 的 `/api/doc/data.bin` 家族（`cmx-doc-api/src/handlers.rs` 的 `encode_columnar_binary`）与 `cmx-doc-store-pg/src/zmc_loader.rs`。背景见备忘 `cmx-rowsource-zmc`。

- ⬜ **保留 `DataSet`（`query_sql`）**：控制面的**小行读**——单行 → DTO、聚合快照装载、元数据/绑定查询、候选人解析。`ZmcDataSet` 是位置索引访问（`col_str(row,col)` / `get_i64(col)`），对「首行 → DTO」的小读反而更繁；这类读全平台（91 处）仍用 `query_sql` → `DataSet` 的按名取值（`row.get_by_name(schema, "col")`）。

> 现状：cmx-flowengine 当前**没有**「对外返回业务行数据集」的端点（待办/实例列表是手工投影成 `serde_json::Value` 的小结果），故全部读走 `DataSet`。**待真正新增数据集导出类端点时，按上面第一条用 ZmcDataSet 落地。**

