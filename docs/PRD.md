# PRD：Chiron 多租户 AI Agent SaaS

- 状态：按当前代码整理（2026-11-22）；状态与排期见 [ROADMAP.md](ROADMAP.md)
- 形态：Rust API-only 服务 + 租户前台 `web/` + 平台控制台 `console/`（两者均支持 zh/en）
- 一句话：服务商运营的多租户 Agent 服务，租户由后台开通，数据 **DB-per-tenant** 物理隔离，
  请求按 Host 解析租户，沙箱按套餐分档。

## 1. 定位与边界

- 面向运营方（服务商）而非自助注册：**没有公开注册路由**，租户、套餐、域名全由平台侧开通。
- 租户（客户组织）只访问自己的域名与数据；平台管理员独立账号，只在平台 Host 上工作。
- 本仓库不托管租户前端静态文件：`web/` 是独立 Next.js 站点，反代同域部署（见 `deploy/Caddyfile`）。
- 计费只做到计量与配额（token/成本/存储/沙箱数），账单对接未实现。

## 2. 角色与租户模型

| 角色 | 范围 | 能力 |
| :--- | :--- | :--- |
| 平台管理员 | 控制库 | 建/暂停/恢复/删除租户、绑域名、BYOK、导出/备份、迁移升级、用量与审计 |
| 租户 owner | 单租户 | 全部租户功能 + 成员管理 + 审计/用量；不能跨租户 |
| 租户 admin | 单租户 | 成员管理（不能操作 owner/admin）、审计/用量、模型服务 key 池/目录/路由 |
| 用户 | 单租户 | 会话、Agent、自己的记忆与媒体文件；只看到租户启用的模型（可再隐藏） |

- 租户必有 `<slug>.<TENANT_BASE_DOMAIN>`，可另绑 **1 个**自定义域名（DNS TXT 验证）。
- slug 3–32 位小写字母/数字/连字符；库名与角色名为 `chiron_t_<slug 下划线化>`。
- 租户状态：`pending → provisioning → active`，可 `suspended`；`failed` 可重试开通；
  `deleted` 只保留审计行，slug 与库名释放后可复用。

## 3. 数据隔离

| 维度 | 做法 |
| :--- | :--- |
| 业务数据 | 每租户独立 PostgreSQL 库与角色；控制库只存元数据与加密 DSN |
| 路由 | `Host` → `tenant_domains` → 租户；未知 Host 404，暂停/未开通 503，无默认租户回落 |
| 身份 | JWT 带控制库 `tenant`；与 Host 解析结果不一致 → 401 |
| 连接 | 每租户连接池注册表（`CHIRON_TENANT_POOL_MAX` / `_POOLS_MAX` / `_IDLE_SECS`） |
| Redis | 会话版本、登录锁、事件流、租约 key 带租户前缀 |
| 沙箱 | 容器名/工作区/标签带控制库 tenant uuid；套餐决定 runtime 与节点池 |
| 媒体 | 元数据在租户库 `media_assets`，文件落 `CHIRON_MEDIA_ROOT/<tenant-uuid>/<user-uuid>/<asset-id>/blob` |
| 密钥 | 租户 provider key 池与 DSN 均 AES-256-GCM 加密（AAD 区分租户/provider），接口只回 id/label/weight/enabled |
| 模型 | key 池、模型目录、路由存控制库并按租户隔离；用户隐藏模型存租户库 `user_hidden_models` |
| 审计 | 租户库 `audit_logs` + 控制库 `platform_audit` 双份（模型 key 变更记 `model.*`） |

## 4. 套餐与隔离档

控制库 `plans` 内置三档（数值为默认值，可由平台侧改）：

| 套餐 | 隔离档 | 用户 | 并发 run | 月 token | 存储 | 沙箱 |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: |
| standard | `sysbox` | 20 | 2 | 10M | 20 GB | 20 |
| pro | `gvisor` | 100 | 10 | 100M | 200 GB | 100 |
| enterprise | `microvm` | 1000 | 50 | 1B | 2000 GB | 1000 |

- `isolation_tier` → `sysbox`/`gvisor`/`microvm` 映射到 `sysbox-runc`/`runsc`/`kata-runtime`；
  目标 runtime 未安装时 `docker run` 明确失败，**不静默降级**。
- `egress_mode`：`bridge`（默认直连）、`proxy`（服务端正向代理按 `egress_allowlist` 放行）、
  `none`（断网）；未知值明确失败。默认白名单覆盖 GitHub、PyPI、npm、crates.io、Debian/Alpine 源。
- 配额预检：用户数、并发 run、当月 token、当月成本（0=不限）、沙箱数、工作区存储、
  模型白名单（空=不限）。除模型白名单返回 403 外，超限统一 429。
- `monthly_cost_cents`、`model_allowlist`、`egress_allowlist` 都是套餐字段，不是租户字段：
  同一套餐的租户共享策略。

**模型策略分三层**：

1. 套餐 `model_allowlist`（空=不限）是硬边界，提交消息时命中失败返回 `403 model_not_allowed`。
2. 租户模型目录决定普通用户可见/可调用的模型；目录非空时，目录外模型同样 403。
   目录、key 池与路由由租户 owner/admin 在 `/models` 页或 `/admin/models` API 维护。
3. 用户隐藏模型（`user_hidden_models`）只影响前端展示，不阻止 API 显式调用。

运行时按 `tenant_model_routes` 的 priority 升序、weight 降序排候选，再在 provider 的
key 池内按 weight 加权展开；候选逐个失败转移（同一请求只建立一次上游流）。租户无路由
时退回 `<provider>:<model>` 解析结果，保持升级前行为。

## 5. 交付批次

以下为已交付的产品范围（历史批次，均已完成）：

| 批次 | 内容 |
| :--- | :--- |
| S0–S3 | 控制库/平台 API/控制台骨架；租户开通；Host + 连接池 + JWT 双校验；迁移升级 |
| S4–S6 | 配额与用量汇总；BYOK 与模型白名单；套餐 tier → 沙箱 runtime 与沙箱数配额 |
| S7–S9 | egress 代理；平台控制台；导出/物理删除/`pg_dump` 备份 |
| M1 | 租户模型服务：key 池、目录、路由、失败转移、自动发现、健康探测、隐藏模型 |
| P1 | 运行体验与运维：审批徽标、运行面板、命令面板、工作区文件、存储用量、TLS 授权、定时备份、S3/WebDAV、指标、告警、平台 TOTP、Redis Sentinel、多区域主备、压测/审计工具 |

下一批（双活多区域、目标环境容量/生产 RPO-RTO、计费账单等）及当前状态见
[ROADMAP.md](ROADMAP.md)。

租户前台（`web/`）页面：登录、强制改密、会话列表、聊天（流式事件、审批、工具卡片、
推理面板、显示偏好、消息锚点、导出/分享）、归档、记忆、集成（MCP/插件/技能/市场）、
模型（用户目录 + owner/admin 的 key 池/Provider/路由管理）、用量、审计、成员、设置。
界面文案 zh/en 基本覆盖
（无 URL 前缀，localStorage 记忆），含日/数/货币的 locale 格式。

媒体库：网盘式文件页已实现（上传/搜索/预览/下载/删除、任意文件 ≤50 MiB）；存储后端
local、WebDAV 与 S3 均已落地。

## 6. 非目标与已知缺口（摘要）

- **不做账单**：只有用量、配额与超额拒绝；计费/发票对接未实现。
- **多区域仅主备**：已支持 region 绑定、fail-closed、人工主备切流与本地 RTO 演练；
  双活/自动调度未做。目标环境的连接数/并发 run/容器密度仍无实测上限。
- **迁移不可回滚**：SQLx 无 down 迁移，回滚依赖按库备份。
- **自定义域名 TLS**：TXT 验证、Caddy on-demand ask 与状态探测已内置；证书签发/续期
  仍由反代（Caddy）完成，跨区/多实例共享存储需运维配置。
- **凭证哨兵缺失**：egress 为 CONNECT 透传不解密；provider key 只在服务端使用。
- **平台管理员认证**：支持可选 TOTP；未启用时仍为单密码，靠独立 Host、审计与最小权限缓解。

更细的影响与排期见 [ROADMAP.md](ROADMAP.md) 的「已知缺口与风险」。
