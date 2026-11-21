# chiron-server 生产镜像（API-only，不含前端）。
#
# 构建（在仓库根目录执行，构建上下文必须包含 migrations/ 与 src/）：
#   docker build -f deploy/server/Dockerfile -t chiron-server:latest .
#
# 三条关键约束，改动前先读：
# 1. migrations 已内嵌：`src/main.rs` 的 `sqlx::migrate!("./migrations")` 是**编译期**
#    宏，SQL 在构建阶段被嵌进二进制。所以这里只把 migrations/ 拷到 builder，运行镜像
#    不需要也没有任何 .sql 文件；启动时自动对 DATABASE_URL 应用未执行的迁移。
# 2. 运行时需要 docker CLI：sandbox 容器管理与 `/readyz` 都通过 `docker` 命令访问宿主
#    daemon（见 `src/sandbox.rs`、`src/main.rs`）。compose 里必须挂 /var/run/docker.sock，
#    否则 /readyz 恒为 503、工具调用全部 `sandbox.unavailable`。
# 3. 非 root：运行用户固定 UID/GID 1000（与 sandbox 容器内 `--user 1000:1000` 一致），
#    这样服务端直接读写的 /var/agent-data 与容器写出的文件属主相同。
# 4. 运行时带 pg_dump 16：租户备份功能依赖它（见下方 postgresql-client-16 安装段）。
#
# 版本说明：基础镜像用移动 tag（未固化）。正式发版应固定到具体 digest 并记录，
# 避免构建漂移；本仓库不写死未经核实的小版本号。

# ── 构建阶段 ────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /build

# 依赖缓存层：先用空 main 编依赖，源码改动时不用重下/重编 crate。
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# sqlx::migrate! 在编译期读取 migrations/，必须进构建上下文。
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release \
    && strip target/release/chiron-server

# ── 宿主 docker CLI ────────────────────────────────────────────────────
# 从官方 CLI 镜像取静态 docker 客户端，避免在运行时镜像里安装整个 docker.io。
FROM docker:cli AS dockercli

# ── 运行阶段 ────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# postgresql-client：租户备份（`POST /api/platform/v1/tenants/{id}/backup` 与
# CHIRON_BACKUP_SECS 定时任务）调用 `pg_dump`，缺了会 503 backup_unavailable。
#
# 必须装 **16**（bookworm 源只提供 15）：pg_dump 拒绝连更高版本的服务器
# （compose 用 postgres:16，见 docker-compose.team.yml），client 15 会直接
# "aborting because of server version mismatch"。所以加官方 PGDG 源取 v16。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && install -d /usr/share/postgresql-common/pgdg \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-client-16 \
    && rm -rf /var/lib/apt/lists/* \
    && pg_dump --version \
    && groupadd --gid 1000 chiron \
    && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin chiron \
    && mkdir -p /var/agent-data \
    && chown -R chiron:chiron /var/agent-data

COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=builder /build/target/release/chiron-server /usr/local/bin/chiron-server

# 容器内必须监听 0.0.0.0；对外只发布到宿主机回环，由 Caddy 反代。
ENV BIND_ADDR=0.0.0.0:8088 \
    RUST_LOG=info \
    HOME=/home/chiron

USER 1000:1000
WORKDIR /home/chiron
EXPOSE 8088

# liveness(/healthz) + readiness(/readyz：PG / Redis / docker)。就绪失败说明依赖不可用，
# 编排侧应据此摘流量；进程本身没崩，不要用重启来"修复"依赖。
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=4 \
    CMD curl -fsS http://127.0.0.1:8088/healthz >/dev/null \
     && curl -fsS http://127.0.0.1:8088/readyz >/dev/null

# tini 收尸 + 转发信号，避免 SIGTERM 丢失导致 SSE 连接悬挂。
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/chiron-server"]
