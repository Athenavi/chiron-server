# 部署指南

本仓库产出两样东西：Rust API 服务（`chiron-server`）与两个 Next.js 前端（`web/` 租户前台、
`console/` 平台控制台）。服务端 API-only，前端由反代同域提供。

## 1. 拓扑

```
租户 Host（<slug>.<base> 或自定义域名）── HTTPS ──► 反代（Caddy / Nginx）
                                                      │
                                    chiron-server（数据面，按 Host 选租户库）
                                      ├─ 控制库 chiron_control（租户/域名/套餐/审计/用量）
                                      ├─ 租户库 chiron_t_<slug>（每租户独立库与角色）
                                      ├─ Redis（会话版本、事件流、租约、限流；带租户前缀）
                                      ├─ sandbox 节点池：sysbox / gVisor / microVM
                                      └─ egress 正向代理（可选，proxy 模式）
```

- 平台 API 只在 `PLATFORM_HOST` 可用；平台控制台 `console/` 部署在该域名下。
- 租户子域名需要在 DNS 侧配置通配符记录；反代要把原始 Host 透传给服务端，否则租户解析失败。
- `deploy/Caddyfile` 现在是多租户 on-demand TLS 示例（含 SSE 关缓冲、70MB 上传、安全头）。
  它通过 `CHIRON_TLS_ASK_URL=/api/platform/v1/tls/allow` 向服务端确认域名是否可签发；
  服务端只放行平台 Host、活跃租户子域名与已验证自定义域名。DNS-01/通配符前置条件见
  `deploy/caddy/README.md`。

## 2. 前置

- Docker Engine（Linux 主机最佳；Windows 走 WSL2 后端，资源配置见 `deploy/wsl/.wslconfig.example`）。
- sysbox-runc（Standard 档）：`docker info --format '{{json .Runtimes}}'` 含 `sysbox-runc`，
  且 `docker run --runtime=sysbox-runc --rm alpine id` 退出 0。
- Pro/Enterprise 档另需宿主装好 `runsc`（gVisor）/ `kata-runtime`。
- Agent 镜像：`docker build -t chiron-agent:py314 deploy/agent`（Alpine + Python 3.14 + Git，非 root）。

## 3. 配置

```bash
cp .env.team.example .env.team && chmod 600 .env.team
```

必填/关键项（完整清单见模板与 `src/config.rs`）：

| 变量 | 说明 |
| :--- | :--- |
| `JWT_SECRET` | ≥32 随机字符；占位值会拒绝启动，轮换等于让全部 token 失效 |
| `CHIRON_METRICS_TOKEN` | 团队/服务模式必填；`/metrics` Bearer token |
| `CHIRON_BILLING_WEBHOOK_SECRET` | 外部支付回写 HMAC 密钥；空=关闭 `/api/platform/v1/billing/webhook` |
| `COOKIE_SECURE` | 团队/服务模式未设置时默认 `1`；纯 HTTP 回环调试显式设 `0` |
| `POSTGRES_USER/PASSWORD/DB` | compose 内 DSN 由这三个值拼装，不要另写 `DATABASE_URL` |
| `CONTROL_DATABASE_URL` | 控制库；compose 默认指向 `postgres:5432/chiron_control` |
| `PROVISION_DATABASE_URL` | 开通用管理连接，留空复用控制库；角色需 `CREATEDB`/`CREATEROLE` |
| `SECRETS_KEY` | `openssl rand -base64 32`；加密租户 DSN 与 BYOK/AAD，丢失即不可解密 |
| `PLATFORM_HOST` | 平台控制台域名；留空只允许回环访问平台 API |
| `TENANT_BASE_DOMAIN` | `<slug>.<base>` 基域；留空则开通时不自动建子域名记录 |
| `PLATFORM_BOOTSTRAP_EMAIL/PASSWORD` | 控制库无管理员时首次启动创建，登录后请改密 |
| `CHIRON_TENANT_POOL_MAX` / `_POOLS_MAX` / `_IDLE_SECS` | 每库连接数 / 注册表上限 / 空闲回收 |
| `CHIRON_USAGE_ROLLUP_SECS` | 定时用量汇总间隔，0 关闭；leader 租约，窗口 3 天 |
| `CHIRON_BACKUP_DIR` | `pg_dump` 输出目录（0700/0600）；空则备份接口禁用；物理删除默认要求先备份 |
| `CHIRON_BACKUP_SECS` | 定时备份间隔；0（默认）关闭，最小值按 300s 生效；leader 租约 |
| `CHIRON_BACKUP_KEEP_DAYS` | 自动删除早于 N 天的本租户 `.dump`；0=不删除 |
| `CHIRON_ALERT_WEBHOOK_URL` | 备份/TLS/模型探测失败与容量阈值的 JSON Webhook；空=关闭 |
| `CHIRON_REGION` | 部署区域标识；出现在 `/healthz`、`/metrics`，并作为新租户默认 region |
| `CHIRON_REGION_STRICT` | 多区域严格绑定；开启后 region 为空/default 的租户返回 `503 tenant_wrong_region` |
| `CHIRON_READ_ONLY` | 只读数据面；租户 API 仅允许 GET/HEAD/OPTIONS，写请求返回 `503 region_read_only` |
| `CHIRON_REDIS_SENTINEL_URLS` | 逗号分隔 Sentinel 地址；非空时优先，自动热切换主节点 |
| `CHIRON_REDIS_SENTINEL_MASTER` | Sentinel 主节点名（默认 `mymaster`） |
| `CHIRON_REDIS_PASSWORD` | Sentinel 发现主节点的密码（可选） |
| `CHIRON_ALERT_COOLDOWN_SECS` | 同类告警冷却秒数（默认 1800；0=不去重） |
| `CHIRON_ALERT_ACTIVE_RUNS` | 默认库活跃 run 告警阈值（0=关闭；跨阈值告警一次） |
| `CHIRON_ALERT_TENANT_POOLS` | 缓存租户池告警阈值（0=关闭；跨阈值告警一次） |
| `CHIRON_ALERT_EGRESS_DENIED` | 单窗口 egress 拒绝数告警阈值（0=关闭），用于发现异常外联 |
| `CHIRON_TLS_ASK_URL` | 反代 on-demand TLS 的 ask 地址（通常 `/api/platform/v1/tls/allow`）；建议用内网 HTTP 入口，避免平台域名证书自锁 |
| `CHIRON_TLS_PROBE_SECS` | 服务端探测域名 HTTPS 并回写 `tls_status=issued/failed`；0=关闭，最小 300 |
| `CHIRON_TLS_PROBE_TIMEOUT_SECS` | 单域名探测超时（默认 5s）；控制台也可手动「检测 TLS」 |
| `CHIRON_EGRESS_PROXY_BIND` | egress 代理监听（如 `0.0.0.0:3128`）；空则关闭 |
| `CHIRON_EGRESS_PROXY_ADDR` | 沙箱内可见地址（默认 `host.docker.internal:3128`），需容器可达；代理仅允许 HTTPS:443，明文 HTTP 默认拒绝 |
| `CHIRON_EGRESS_ALLOW_PLAINTEXT` | 显式设为 1 才放行明文 HTTP:80；即使放行，携带 Authorization/Cookie/API key 的请求仍会被拒绝 |
| `CHIRON_MEDIA_ROOT` | 媒体库根目录，默认 `<SANDBOX_DATA_ROOT>/media`，需持久化且可写 |
| `CHIRON_MEDIA_BACKEND` | `local`（默认）/`webdav`/`s3`；缺配置时明确失败 |
| `CHIRON_MEDIA_WEBDAV_URL` | WebDAV 基址（http/https，无 query/fragment）；`webdav` 后端必填 |
| `CHIRON_MEDIA_WEBDAV_USERNAME/PASSWORD` | WebDAV Basic Auth（可选，需同时提供） |
| `CHIRON_MEDIA_WEBDAV_PREFIX` | WebDAV 根下的可选路径前缀 |
| `CHIRON_MEDIA_S3_BUCKET/REGION/ENDPOINT` | S3 桶、区域（默认 us-east-1）、自定义端点（MinIO 等） |
| `CHIRON_MEDIA_S3_ACCESS_KEY/SECRET_KEY/SESSION_TOKEN` | SigV4 凭证；前两者必填，token 可选 |
| `CHIRON_MEDIA_S3_PATH_STYLE/PREFIX` | 路径风格（自定义端点默认 true）与对象前缀 |
| `SANDBOX_DOCKER_RUNTIME` / `SANDBOX_DATA_ROOT` / `SANDBOX_NETWORK` | 宿主 runtime、数据根（compose 同路径挂载）、默认网络；生产默认 `none`，需要出网用 `proxy` |
| `CHIRON_MCP_COMMANDS` | MCP stdio 宿主命令白名单；默认空 = 全部拒绝，勿加入解释器/`docker` |
| `DOCKER_GID` | `stat -c '%g' /var/run/docker.sock`；服务端以 UID 1000 运行，靠附加组访问 |
| Provider key | `OPENAI_API_KEY` 等；仅作平台回落，租户 key 池优先；两者都没有时该 provider 不可用 |
| `CHIRON_DEFAULT_MODEL` / `CHIRON_DEFAULT_PROVIDER` | 未指定模型时的默认值；租户路由命中时以路由的 `upstream_model` 为准 |

- 团队/服务模式强制 Redis（会话吊销）；`/readyz` 在团队模式下同时要求 Docker 可达。
- 限流可选覆盖：`CHIRON_RL_HEAVY_BURST` / `CHIRON_RL_HEAVY_PER_MINUTE`（媒体上传/下载、MCP probe、市场安装、批量派生等重接口）。
- 控制库由 `deploy/postgres/init-control.sql` 在 PG 卷首次初始化时创建；租户库由开通流程建。
- 服务端启动时对内嵌迁移执行 `sqlx::migrate!`，不需要独立迁移容器。

### 模型服务

- 租户 owner/admin 在租户前台 `/models` 或 `/admin/models` API 维护 key 池、目录与路由；
  平台 API 的 BYOK 仍写 `label=default`，可作为同一张表的平台侧入口。
- key 明文只在写入请求与进程内存中出现，控制库存 AES-256-GCM 密文；接口只回
  id/label/weight/enabled。丢失 `SECRETS_KEY` 后已存 key 与 DSN 都无法解密。
- 运行时优先用租户路由与 key 池；无路由时退回 `<provider>:<model>` 解析，无租户 key
  时用平台 env。可用 provider = 租户池有启用 key，或平台 env 配了该 preset 的 key。
- 自动发现与健康探测是**手动接口**（`/admin/models/discover`、
  `/admin/models/providers/{provider}/probe`），没有后台定时任务；探测错误脱敏后写入
  路由的 `last_error`，不回显 key。
- key/路由在进程内缓存 30 秒，写操作会失效本节点缓存；多实例部署下其他节点最迟
  30 秒看到变更。平台级 `/admin/runtime` 的 provider 健康目前只看平台 env 是否配置 key。

## 4. 启动

```bash
docker compose --env-file .env.team -f docker-compose.team.yml config   # 静态校验
docker compose --env-file .env.team -f docker-compose.team.yml up -d --build
curl -fsS http://127.0.0.1:8088/healthz
curl -fsS http://127.0.0.1:8088/readyz
```

- compose 只含 `postgres` + `redis` + `server`；服务端发布到 `127.0.0.1:8088`，TLS 由反代终结。
- 必须挂 `/var/run/docker.sock`（沙箱与 `/readyz`）和 `/var/agent-data`（**同路径**，
  服务端自己读写同一目录，路径改一处就变成两份数据）；`CHIRON_MEDIA_ROOT` 与
  `CHIRON_BACKUP_DIR` 建议放在该持久卷下。
- 服务端容器非 root（UID/GID 1000），新建的备份/媒体目录需要属主可写。
- 镜像内置 `docker` CLI 与 `pg_dump` 16（PGDG）；备份目录对 UID 1000 可写即可启用。
- `/metrics` 未认证，只应在可信网络暴露；建议在反代层做来源白名单。
- 首次登录：平台管理员用 `PLATFORM_BOOTSTRAP_*`；租户 owner 用一次性密码，首登强制改密。
- 平台管理员可在控制台首页启用 TOTP；启用后登录需 6 位动态码或一次性恢复码。密钥与
  恢复码只在启用响应出现一次，请离线保存；同邮箱连续 5 次失败锁 5 分钟。

## 5. 本地开发

```bash
docker compose -f docker-compose.dev.yml up -d     # PG + Redis，端口只在 127.0.0.1
cp .env.example .env
cargo run                                          # 迁移自动执行
cd web && npm install && npm run dev               # 前台，默认 mock 数据
```

- `.env.example` 默认团队模式但未配控制库 → 单库形态，租户中间件直接放行。
- 要本地验证多租户：在 PG 建 `chiron_control`，配 `CONTROL_DATABASE_URL`、`SECRETS_KEY`、
  `PLATFORM_HOST`/`TENANT_BASE_DOMAIN`，并给 `/etc/hosts` 或 `Host` 头加租户域名。
- 前端访真后端：`NEXT_PUBLIC_USE_MOCK=0`，`app/api/v1/[...path]/route.ts` 会补
  `Host: CHIRON_DEV_TENANT_HOST` 后反代到 `CHIRON_API_ORIGIN`（默认 `127.0.0.1:8088`）。

## 6. 沙箱参数

- 运行时由套餐 tier 决定（`sysbox-runc` / `runsc` / `kata-runtime`）；未知 tier 明确失败。
- `--cap-drop=ALL`、`--security-opt no-new-privileges`、`--read-only`、`--tmpfs /tmp`。
- `--user 1000:1000`、`--memory 512m`（`SANDBOX_MEMORY`）、`--cpus 1.0`、`--pids-limit 256`。
- 标签：`chiron.managed-by`、`chiron.tenant`、`chiron.tenant-id`（控制库 uuid）、
  `chiron.user`、`chiron.scope(-id)`、`chiron.tier`；容器名带 tier 后缀。
- 工作区：`/var/agent-data/<tenant>/<user>` → `/workspace`；空闲 30 分钟停机，数据保留。
- egress：`none` 断网；`proxy` 注入 `HTTP(S)_PROXY=http://<tenant-uuid>:<hmac>@<addr>`，
  按套餐白名单放行，连接前拒私网/回环/链路本地/metadata。

## 7. 验收

1. `POST /api/platform/v1/auth/login`（平台 Host）取 token；`POST /tenants` 建租户，
   轮询到 `active`，`POST /tenants/{id}/owner-credentials` 只能成功一次。
2. `GET /migrations` 看目标版本；`POST /migrations/upgrade {"dry_run":true}` 检查后正式执行。
   正式升级默认先做 `pg_dump`；未配置 `CHIRON_BACKUP_DIR` 会拒绝升级。
3. `POST /tenants/{id}/export` 下载 JSON；`POST /tenants/{id}/backup` 生成 dump（需备份目录）；
   配置 `CHIRON_BACKUP_SECS` 后可在日志看到定时备份的 ok/failed 统计。
4. 租户 Host 登录（owner 临时密码）→ 首登改密 → 邀请成员；把 `max_users` 调到当前人数再建号应 429。
5. 提交对话，`curl -N` 应看到 `run.started`、`message.delta`、`tool.*`、`run.completed`；
   `POST /usage/rollup` 后 `GET /usage` 能看到当日 token/成本。
6. 跨租户：A 的 token 访问 B 的 Host 应 401；未知 Host 404；suspend 后租户 Host 立刻 503。
7. 容器内 `id` 非 root、限额生效；`proxy` 档访问白名单外域名应被代理拒绝。
8. 禁用用户后旧 token 立即 401；断线 10s 重连带 `Last-Event-ID`，事件不丢。
9. 租户 owner 在 `/models` 配 key → 发现模型 → 建路由；普通用户 `GET /models` 只看到
   启用目录，目录外模型提交应 403；`probe` 失败原因不含明文 key。

## 8. 多区域主备（R-3）

当前支持**单区域 + 人工主备切流**：`tenants.region` 绑定租户，`CHIRON_REGION` 不匹配时
fail-closed；跨区调度、自动 DNS 切流与双活未实现。跨区部署时以下约束由运维层满足：

- 切流脚本会在 suspend 后同时 PATCH `region` 与加密后的目标 `dsn`；只改 region 不会
  让应用连接到备区库。
- 每个区域一套完整数据面（server + tenant PG + Redis + sandbox/egress），租户库留在
  本区；控制库要么跨区强一致，要么每区独立并明确“租户属于哪个控制库”。
- `JWT_SECRET`、`SECRETS_KEY`、`TENANT_BASE_DOMAIN` 与套餐/域名记录必须在切换前后一致；
  丢失 `SECRETS_KEY` 等于丢失租户 DSN/BYOK。
- 区域故障切换以 DNS/反代切流 + 目标区已有备份恢复为主；没有增量迁移，RPO 取决于
  备份频率，RTO 取决于恢复耗时，必须实测并写进值班手册。人工切流脚本与前置条件见
  `scripts/region/README.md`（默认 dry-run，`--apply` 需租户 slug 二次确认）。
- Redis 不能跨区共享；SSE 事件流、租约和会话吊销在同一区内保持一致，跨区不保证。
- `CHIRON_REGION` 与租户 `region` 不一致时服务端返回 `503 tenant_wrong_region`；
  切流必须把请求送到租户绑定的区域，不能靠本区回源。
- 切换前先 `suspend` 或停写，避免两个区同时写同一租户库；恢复后核对
  `/readyz`、`/migrations`、租户 Host 与 owner 登录。

## 9. 排障

| 现象 | 检查 |
| :--- | :--- |
| `/readyz` 503 | PG/Redis 连通；团队模式下 Docker 必须可达（docker.sock 挂载与 `DOCKER_GID`） |
| 所有租户请求 404 | Host 未匹配 `tenant_domains`；反代是否改写了 Host；`TENANT_BASE_DOMAIN` 配置 |
| 租户 503 | 状态非 `active`（暂停/开通失败/未开通），看控制台租户详情与 `provision_error` |
| 对话卡住后一次性输出 | 反代 SSE 缓冲未关闭 |
| 容器起不来 | `docker info` 里有没有该 tier 的 runtime；镜像是否已构建；`/var/agent-data` 权限 |
| 登录后立刻掉线 | `COOKIE_SECURE` 与实际访问协议不一致（HTTPS 需为 1，纯回环调试为 0） |
| 备份 503 | `CHIRON_BACKUP_DIR` 未配；目录对 UID 1000 不可写；`pg_dump` 版本与 PG 服务端不匹配 |
| 媒体上传后 503 | `s3`/`webdav` 配置缺失或凭证错误，或 local 根目录不可写 |
| WebDAV 503 | URL 缺失/非法、认证失败或远端非 2xx；看服务日志中的脱敏错误 |
| 提交消息 403 `model_not_allowed` | 模型不在套餐 `model_allowlist` 或租户启用目录；在 `/models` 加入目录或调整套餐 |
| 模型不可用 / 发现为空 | 租户 key 池与平台 env 都没配该 provider 的 key；看 `/admin/models` 与探测的 `last_error` |
| key/路由变更 30 秒后才生效 | 多实例下进程内缓存 TTL；确认写操作成功，或到对应节点触发一次写/重启 |
| egress 全部被拒 | 套餐 `egress_mode` 不是 `proxy`；白名单为空；代理地址容器不可达 |
| 事件断档 | 重连带 `Last-Event-ID`；超保留窗口会发 `run.gap`，客户端应重拉快照 |
