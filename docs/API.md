# API 契约

与当前代码一致的两套 API：租户 API `/api/v1`（按 Host 解析租户）与平台 API
`/api/platform/v1`（只在平台 Host）。未实现的能力不在此列出。

## 1. 租户边界与 Host 解析

- 每个 `/api/v1/*` 请求（含登录）先按 `Host` 查控制库 `tenant_domains`：
  未知 Host → `404 not_found`；`suspended` → `503 tenant_suspended`；
  未开通/未 `active` → `503 tenant_not_ready`；库连不上 → `503 tenant_database_unavailable`。
  不回落默认租户。
- JWT 内含控制库 `tenant`；与 Host 解析结果不一致 → `401 auth.required`。
- 会话版本、登录失败锁、事件流 key 都带租户前缀，跨租户同名标识互不影响。
- 平台 API 在租户 Host 上一律 404；租户 API 不提供任何跨租户列表。
- 自定义域名每租户最多 1 个，需 DNS TXT 验证（`chiron-verify=<tenant_id>`）。

## 2. 通用约定

- 认证：`Authorization: Bearer <jwt>`，或 httpOnly Cookie `chiron_token`。
- 成功：`{ "data": ... }`；失败：`{ "error": { "code", "message", "details"? } }`。
- `code` 是稳定字符串，客户端据此分支，不要依赖 message 文案。
- 分页：`?cursor=&limit=`，响应带 `next_cursor`（不透明游标）。
- 幂等：仅 `POST /sessions/{id}/messages` 识别 `Idempotency-Key`（≤200 字节，
  24 小时，同键不同请求体返回 `409 conflict`）。
- 时间 RFC3339 UTC。`must_change_password` 为真时只放行改密、登出、profile、session。

## 3. 错误码

| code | HTTP | 场景 |
| :--- | :--- | :--- |
| `bad_request` | 400 | 参数/游标/时间窗口非法 |
| `auth.required` | 401 | 未登录、令牌无效/吊销、Host 与 token 租户不一致 |
| `auth.totp_required` / `auth.totp_invalid` | 401 | 平台管理员已启用 TOTP：缺码 / 错误或过期 |
| `auth.password_change_required` | 403 | 首登或重置后未改密 |
| `forbidden` | 403 | 权限不足 |
| `model_not_allowed` | 403 | 模型不在套餐 `model_allowlist` 或租户启用目录内 |
| `mcp.blocked` | 403 | MCP 目标被网络策略拒绝 |
| `not_found` | 404 | 资源不存在、未知 Host、平台 API 出现在租户 Host |
| `conflict` | 409 | 唯一约束/状态冲突、幂等键冲突 |
| `run.busy` | 409 | 会话已有活跃 run |
| `mcp.cancelled` | 409 | MCP 调用被取消 |
| `payload_too_large` | 413 | 媒体上传超过 50 MiB |
| `auth.locked` | 429 | 登录失败锁定（`message` 含重试秒数） |
| `quota.exceeded` | 429 | 限流；带 `Retry-After` 与 `details.retry_after` |
| `quota_exceeded` | 429 | 套餐配额（用户数/并发 run/token/成本/沙箱/存储） |
| `internal_error` | 500 | 服务端错误（细节只进日志） |
| `provider.failed` | 502 | 上游模型调用失败 |
| `mcp.failed` | 502 | MCP 传输/协议失败 |
| `mcp.timeout` | 504 | MCP 调用超时 |
| `sandbox.unavailable` | 503 | 沙箱容器不可用/未就绪 |
| `tenant_not_ready` / `tenant_suspended` / `tenant_database_unavailable` | 503 | 租户路由失败 |
| `tenant_wrong_region` | 503 | 租户绑定区域与当前 `CHIRON_REGION` 不一致 |
| `control_unavailable` / `provision_unavailable` | 503 | 控制库未配置/开通连接不可用 |
| `egress_unavailable` / `dns_unavailable` | 503 | egress 代理或 DNS 解析不可用 |
| `media_unavailable` | 503 | 媒体存储后端不可用 |
| `backup_disabled` / `backup_unavailable` | 503 | 未配置 `CHIRON_BACKUP_DIR` / 备份执行失败 |

Agent 运行期失败写在 `run.failed` 事件的 `data.code` 里，取值包括 `provider.failed`、
`tool.failed`（含审批超时）、`sandbox.unavailable`、`quota.exceeded`、`run.step_limit`；
检查点接管相关的终态另有 `run.abandoned` / `run.recovery_exhausted`。

## 4. 租户 API

### Auth

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| POST | `/auth/login` | `{identifier, password}`；identifier 为邮箱或用户名；单独限流 |
| POST | `/auth/local-session` | 仅 `deploy_mode=desktop`、回环直连、无 `X-Forwarded-*`；引导本机 owner |
| GET | `/auth/session` | 当前会话（含用户与套餐） |
| POST | `/auth/logout` | 吊销当前 token |
| POST | `/auth/password` | `{old_password, new_password}` |
| GET / PATCH | `/auth/profile` | 当前用户；PATCH 改昵称/显示名 |

### 成员管理（owner/admin）

`GET|POST /admin/users`、`DELETE /admin/users/{id}`、
`POST /admin/users/{id}/password/reset|disable|enable|force-logout`。

- admin 不能操作 owner 或其他 admin，不能操作自己，不能移除最后一个 owner。
- 禁用/删除/重置密码会推进 Redis 会话纪元，旧 token 立即失效。

### 模型（用户侧）

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| GET | `/models` | 租户启用目录 + 平台 preset；目录为空时 `models` 为空数组（回落平台列表） |
| GET | `/models/hidden` | 当前用户隐藏的模型名 |
| PUT | `/models/hidden` | `{models: string[]}` 覆盖写；去重、≤200 条，空数组为清空 |

- `GET /models` 响应：`{ data: { models: [{model, display_name, providers: [...]}], providers: [...] } }`。
  `providers` 是平台 preset 清单（27 个，字段同旧契约），供管理界面做下拉；`models` 的
  `providers` 是该模型启用路由可用的 provider。
- 目录为空（未配置/单库形态）时只返回 `providers`，`models` 为空，行为与升级前一致。
- 隐藏模型只影响前端选择，不阻止 API 显式调用；是否可调用由套餐白名单与租户目录决定。

### 模型服务管理（owner/admin）

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| GET | `/admin/models` | 概览：`{providers, keys, models, routes}`，key 不含明文 |
| POST | `/admin/models/keys` | `{provider, api_key, label?, weight?}`；按 `(provider,label)` upsert |
| PATCH / DELETE | `/admin/models/keys/{id}` | PATCH `{enabled?, weight?}`；DELETE 删单条 key |
| PATCH | `/admin/models/providers/{provider}` | `{enabled}` 启停该 provider 的全部 key（无 key 时 404） |
| POST | `/admin/models/catalog` | `{model, display_name?}` 写入并启用目录项 |
| PATCH / DELETE | `/admin/models/catalog/{model}` | PATCH `{enabled?, display_name?}` |
| POST | `/admin/models/routes` | `{model, provider, upstream_model?, priority?, weight?}`；按 `(model,provider)` upsert |
| PATCH / DELETE | `/admin/models/routes/{id}` | PATCH `{upstream_model?, priority?, weight?, enabled?}` |
| POST | `/admin/models/discover` | `{provider?, persist?}`；拉取可用 provider 的远端模型 |
| POST | `/admin/models/providers/{provider}/probe` | 拉一次 `/models` 并更新该 provider 路由健康 |

- 名称限制：`model` ≤200 字符、不得含空白/控制字符；URL 路径里的 `model` 需编码
  （模型名常含 `:`）。`weight` 1–1000，`priority` 0–100000；默认 `priority=100`、`weight=1`。
- key 写入后只返回 `id/provider/label/weight/enabled/updated_at`；审计动作形如
  `model.key_added`、`model.route_updated`，不含明文。
- `discover` 的 `persist=true` 只新增缺失目录项，并为新模型补一条默认路由
  （`priority=100`），不覆盖已有配置；单个 provider 失败不影响其他 provider。
- `probe` 成功清空该 provider 全部路由的 `last_error`，失败写入脱敏文本（≤300 字符）；
  返回 `{ok, models, error?}`。探测目前是手动触发，没有后台定时任务。
- 写入会立即失效进程内 key/路由缓存（30 秒兜底 TTL），多实例下其他节点最迟 30 秒生效。
- 运行时：路由按 priority 升序、weight 降序排候选；每条路由再按 key 池 weight 加权
  展开 key。候选逐个失败转移。无路由时退回 `<provider>:<model>` 解析结果。

### 会话 / Run

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| GET / POST | `/sessions` | 列表 / 新建（`title`、`model`、`project_id`、`parent_session_id`）；`project_id` 仅 `[A-Za-z0-9_-]{1,64}` |
| GET / PATCH / DELETE | `/sessions/{id}` | 详情 / 改名与归档 / 删除 |
| GET / POST | `/sessions/{id}/messages` | 历史 / 提交消息（`content`、`model`、`client_id`）→ 返回 `run_id` |
| GET | `/sessions/{id}/events` | SSE（见 §6） |
| GET | `/sessions/{id}/container` | 当前会话沙箱容器状态 |
| GET | `/sessions/{id}/files?path=&limit=` | 工作区目录列表（`{path, entries, truncated}`，limit 1..500） |
| GET | `/sessions/{id}/files/download?path=` | 下载工作区常规文件（≤50 MiB，流式，附件） |
| POST | `/sessions/{id}/interrupt` | 中断当前 run |
| POST | `/sessions/{id}/steer` | 运行中追加指令 |
| POST | `/sessions/{id}/approvals/{approval_id}` | 审批决定 |
| GET | `/runs/{id}` | run 状态、用量、错误 |

同一会话同一时刻最多一个活跃 run（`queued/running/waiting_approval/waiting_user`）。

`GET /sessions` 的每个会话附带 `active_run`（没有则为 `null`）与 `pending_approvals`；
响应顶层另有 `pending_approvals_total`（当前用户可见范围内全部待审批数），供侧栏徽标使用。

工作区文件接口与会话沙箱同源路径（`<SANDBOX_DATA_ROOT>/<tenant>/<user>[/<project>]`）：
绝对路径、`..`、NUL、越界符号链接一律拒绝；只读常规文件，GET 不会创建工作区目录。

### 子 Agent / 检查点

`GET /runs/{id}/subagents`、`GET|POST /sessions/{id}/subagents`、
`POST /subagents/{id}/cancel`；
`GET|POST /runs/{id}/checkpoints`、`POST /runs/{id}/recover`、
`GET /sessions/{id}/checkpoints`、`POST /checkpoints/{id}/restore`。

### 记忆

`GET|POST|DELETE /memories`（DELETE 为清空）、`PATCH|DELETE /memories/{id}`。

### 媒体（网盘式文件库）

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| GET | `/media` | 列表（`cursor`/`limit`）；普通用户只见自己的，owner/admin 见全租户 |
| POST | `/media?filename=` | 上传 raw body；`Content-Type` 仅作参考，落库类型按魔数判定 |
| GET | `/media/{id}` | 下载；`?download=1` 强制附件 |
| DELETE | `/media/{id}` | 删除元数据与存储对象 |
| POST | `/admin/media/gc?dry_run=1` | owner/admin 本地媒体孤儿清理；默认 dry-run，S3/WebDAV 返回 409 |
| POST | `/admin/media/migrate?dry_run=1&delete_source=0` | 当前租户 local→WebDAV/S3 历史对象迁移；默认 dry-run；删源前校验源/目标 sha256 |

- 任意文件可存，单文件 50 MiB；只有魔数校验通过的 PNG/JPEG/WebP/GIF 允许 inline，
  其余一律 `application/octet-stream` + attachment（阻断 HTML/SVG XSS）。
- 存储：`CHIRON_MEDIA_BACKEND=local`（默认）落
  `CHIRON_MEDIA_ROOT/<tenant-uuid>/<user-uuid>/<asset-id>/blob`；
  `webdav` 与 `s3`（SigV4，兼容 MinIO）后端可用；缺配置时明确失败，
  不静默退回本地。

### MCP / 插件 / 技能 / 市场

`/mcp/servers`（GET/POST）、`/mcp/servers/{id}`（PATCH/DELETE）、
`/mcp/servers/{id}/probe`（POST）、`/mcp/tools`（GET）、
`/plugins`、`/plugins/{id}`、`/skills`、`/skills/{id}`、
`/market/items`（GET/POST）、`/market/items/{id}/install|uninstall`（POST）。

### 运维与只读视图（owner/admin）

`GET /admin/audit`（租户/用户/动作/时间过滤 + 游标）、`GET /admin/usage`
（`days` 默认 7，1..90，或 `from`/`to`）、`GET /admin/runtime`、
`GET /admin/sandbox/containers`。

`/admin/runtime` 的 `providers` 在旧字段之外增加 `label`、`available`、`key_source`、
`tenant_keys`（总数/启用数/最近更新）与 `routes`（启用数/最近成功/最近探测/脱敏错误）；
控制库不可用时降级为 `null`，不会把面板打成 500。

### 健康检查与指标（无认证）

`GET /healthz`（进程存活）、`GET /readyz`（PG、Redis、Docker；团队模式下 Redis 与
Docker 为必需，返回 `503` 表示依赖不可用；`/readyz` 同时返回 `region`、
`region_strict`、`read_only`）。

`GET /metrics` 输出 Prometheus 文本（连接池、SSE、egress 允许/拒绝、默认库活跃 run、控制库路由失败等）；
不含租户名或密钥。默认未认证；配置 `CHIRON_METRICS_TOKEN` 后要求
`Authorization: Bearer <token>`，仍建议限制在可信网络。

## 5. 平台 API

仅在平台 Host 可用（`PLATFORM_HOST`；留空只允许回环）。认证：
`POST /api/platform/v1/auth/login` 取平台 token（Bearer）。启用 TOTP 后登录体需带
`totp_code`（6 位动态码或一次性恢复码）；缺失返回 `401 auth.totp_required`，错误返回
`401 auth.totp_invalid`。同一 TOTP step 只能使用一次，重放返回 `401 auth.totp_invalid`。
同邮箱连续 5 次失败锁定 5 分钟（配置 Redis 时跨实例共享），返回 `429 auth.locked`；
登录入口同时按来源 IP 做滑动窗口限流。

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| GET | `/me` | 当前平台管理员（含 `totp_enabled`） |
| POST | `/me/totp/setup` | 生成 TOTP secret 与 `otpauth_url`（暂存，未启用） |
| POST | `/me/totp/enable` | `{code}` 校验后启用；响应含一次性 `recovery_codes` |
| POST | `/me/totp/disable` | `{code}` 校验后关闭二次验证 |
| GET | `/plans` | 套餐列表（含 `model_allowlist`、`egress_*`、`monthly_cost_cents`、`overage_policy`） |
| GET / POST | `/tenants` | 列表 / 创建（`slug`、`name`、`plan_id`、`owner_email`、`region`；strict 模式拒绝空/default） |
| GET / PATCH | `/tenants/{id}` | 详情 / 改名、套餐、`region`，以及 suspend 后更新加密 `dsn`（同时同步 `db_name`） |
| POST | `/tenants/{id}/suspend` `/resume` `/provision` | 状态流转与开通重试 |
| POST | `/tenants/{id}/owner-credentials` | 一次性返回 owner 临时密码 |
| GET / POST | `/tenants/{id}/domains` | 列表 / 绑定自定义域名（`{host}`，每租户最多 1 个；子域名在开通时自动建） |
| POST | `/tenants/{id}/domains/{domain_id}/verify` | 自定义域名 TXT 验证 |
| GET / PUT | `/tenants/{id}/provider-keys` | BYOK 列表 / 写入（不回显明文） |
| DELETE | `/tenants/{id}/provider-keys/{provider}` | 删除 BYOK |
| POST | `/tenants/{id}/export` | 19 张业务表 JSON（5 万行/表），附件下载；剔除密码哈希与 MCP env/headers |
| POST | `/tenants/{id}/delete` | `{confirm: slug, backup_before?: true}`，需先 suspend；默认先备份，备份失败则拒绝删除 |
| POST | `/tenants/{id}/backup` | `pg_dump` custom 格式，需 `CHIRON_BACKUP_DIR` |
| GET | `/migrations` | 目标版本与各租户版本 |
| POST | `/migrations/upgrade` | `{dry_run?, tenant_ids?, backup_before?}`；默认先备份再串行升级 |
| GET | `/tenants/{id}/bill` | 月度账单汇总（缺省当月，含 totals/days） |
| GET | `/tenants/{id}/bill.csv` | 月度账单 CSV 下载 |
| GET | `/tenants/{id}/invoice?month=YYYY-MM` | 发票数据（套餐费+用量费；读取外部支付状态） |
| POST | `/tenants/{id}/invoice/status` | 外部支付回写：`{month,status,provider?,external_id?,amount_cents?,paid_at?}` |
| GET | `/tenants/{id}/bill/reconcile?month=YYYY-MM` | 控制库 rollup 与租户库原始用量/runs 对账，返回 `drift` |
| GET | `/usage` | 按租户/日汇总（`tenant_usage_daily`） |
| GET | `/usage/export.csv?days=` | 用量 CSV 下载（1..366 天，默认 90），用于账单对账 |
| POST | `/usage/rollup` | `{days?}` 重算窗口（覆盖写，可重复执行） |
| POST | `/billing/webhook` | 外部支付回写（`X-Chiron-Signature: sha256=<hmac>`，无需管理员会话） |
| GET | `/regions` | 本地区域、控制库租户分布、未绑定区域租户数与 Redis 健康 |
| POST | `/regions/backfill` | `{region,dry_run?}`：把空/default region 的租户绑到指定区域（默认 dry-run） |
| GET | `/audit` | 平台审计 |
| GET | `/tls/allow?domain=` | Caddy on-demand TLS ask 端点：2xx 允许签发/续期，不返回租户信息 |
| POST | `/tenants/{id}/domains/{domain_id}/probe-tls` | 立即探测域名 HTTPS，回写 `tls_status` 并返回脱敏失败原因 |

- `provider-keys` 是租户 key 池的平台侧入口：GET 按 provider 聚合全部 label 并单列
  `default_present`；PUT 只写 `label=default`；DELETE 也只删 `label=default`，
  租户管理员维护的其它 label 不受影响（单条删除走 `/admin/models/keys/{id}`）。
- 创建租户要求合法 `slug`（3–32 位小写字母/数字/连字符）与 `owner_email`；
  配置 `SECRETS_KEY` 与开通 DSN 后异步开通，失败进 `failed` 可再次 `provision`。
- 删除与导出都需要控制库与 `SECRETS_KEY`；删除默认先 `pg_dump`（`backup_before=false`
  可显式跳过），随后终止连接、DROP DATABASE/ROLE、释放域名与 BYOK，保留 `deleted`
  行用于审计。
- 迁移、单租户备份与物理删除共用租户级 advisory lock；并发时返回 `409`（迁移批次中
  该租户标记为 `skipped`）。
- 迁移升级按租户串行、单库失败不影响其他租户；SQLx 无 down 迁移，回滚只能按库备份恢复。
  `backup_before` 默认 `true`：未配置 `CHIRON_BACKUP_DIR` 时拒绝升级；单租户备份失败也不会
  继续迁移该库。显式传 `false` 可跳过（不建议生产使用）。

## 6. SSE

```http
GET /api/v1/sessions/{id}/events
Accept: text/event-stream
Last-Event-ID: <seq>      # 可选，断线续传；非数字忽略
```

- 每条事件只有 `id:`（会话内单调递增的 `seq`）与 `data:`，负载是完整 envelope：

```json
{ "id": "evt_...", "seq": 42, "run_id": "...", "type": "message.delta", "data": {}, "ts": "..." }
```

- 事件 `type`：`run.started|completed|failed|cancelled`、`message.delta|completed`、
  `reasoning.delta`、`tool.call|result`、`approval.required|resolved`、
  `usage.updated`、`heartbeat`、`run.gap`。
- `heartbeat` 每 15s 一次且占用 `seq` 但不进回放窗口；响应带
  `X-Accel-Buffering: no` 与 `Cache-Control: no-cache`，反代仍应关闭缓冲
  （Caddy `flush_interval -1`、Nginx `proxy_buffering off`）。
- 单进程同一用户最多 16 条并发 SSE（多标签页/重连风暴保护）；超过返回
  `429 quota.exceeded` 与 `Retry-After`。
- `Last-Event-ID=seq` 从 `seq+1` 回放；超出保留窗口先发 `run.gap`
  （`{from_seq,to_seq}`），客户端据此重拉 `/sessions/{id}/messages` 与 `/runs/{id}`。
- 客户端按 `id` 去重；同一 run 内事件有序。

## 7. 授权摘要

- `/admin/*` 与平台 API 需要 owner/admin 或平台管理员；用户管理需 owner/admin 角色。
- 普通用户只能访问自己的会话、记忆与媒体文件。
- 所有租户查询只走当前租户连接池，客户端无法通过参数指定租户。
