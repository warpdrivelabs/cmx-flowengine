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
