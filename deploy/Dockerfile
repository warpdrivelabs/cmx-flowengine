# cmx-flow-server 独立流程微服务镜像（S6 交付）。
#
# 多阶段：builder 编译静态二进制 → distroless 运行（无 shell、体积小、攻击面小）。
# 工具链锁 1.97.1（与 rust-toolchain.toml / cmx-container 对齐，避免"本机过 CI 不过"）。
#
# 构建（在 cmx-flowengine 目录）：
#   docker build -f deploy/Dockerfile -t cmx-flow-server:latest .
# 运行（配置统一走 toml；env 只做覆盖，框架键前缀 CMX_*，业务键 AUTH__*）：
#   docker run -p 8091:8091 \
#     -v $PWD/flow-server.toml:/app/flow-server.toml:ro \
#     -e CONFIG_FILE=/app/flow-server.toml \
#     cmx-flow-server:latest

# ---- builder ----
FROM rust:1.97.1-slim-bookworm AS builder
WORKDIR /build

# 系统依赖（rustls 用 ring，需要 cc/perl；postgres 客户端库非必需，tokio-postgres 纯 Rust）
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# 先拷 manifest 预热依赖缓存（源码未变时复用 layer）。
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo ./.cargo
COPY crates ./crates

# 跨 workspace path 依赖 cmx-container 的 infra crate（见 .cargo/config.toml 的 nora registry +
# aliyun 镜像）。CI 环境需同时挂载 cmx-container（或用其发布的 crate）。本地构建时把 cmx-container
# 作为同级目录挂进来：docker build 的 context 需含两者，或改用 workspace 顶层构建。
# —— 为简明起见，这里假设已能解析（生产 CI 按实际仓库布局调整 context）。

RUN cargo build --release --bin cmx-flow-server

# ---- runtime (distroless) ----
FROM gcr.io/distroless/cc-debian12 AS runtime
WORKDIR /app

# 静态资产：headless 契约不需要前端；可嵌组件（S5 web/）按需另行托管（CDN / nginx）。
COPY --from=builder /build/target/release/cmx-flow-server /app/cmx-flow-server

# 默认端口（可用 CMX_PORT 覆盖）。
EXPOSE 8091

# distroless 无 shell，直接 exec 二进制。配置统一走 toml（见 flow-server.toml.example），
# env 只做覆盖。
ENTRYPOINT ["/app/cmx-flow-server"]
