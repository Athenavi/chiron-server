# Chiron 开发路线图（2026-11-22）

> 本文是**状态与排期的单一来源**：PRD 只描述产品边界，API/deploy 只描述契约与部署，
> CHANGELOG 只记录变更。任务以「可观察行为」验收，不以代码写完为准。

## 1. 当前基线

- **多租户核心 S0–S9 完成**：开通、Host 解析、迁移、配额、BYOK、沙箱分档、egress、控制台、导出/备份/删除。
- **租户模型服务 M1 完成**：key 池、模型目录、路由/失败转移、自动发现、健康探测、用户隐藏模型。
- **租户前台**：聊天（流式/审批/工具/推理/显示偏好/锚点/导出分享）、归档、记忆、集成、媒体、模型、用量、审计、成员、设置。
- **平台控制台**：租户、套餐、用量、迁移、域名/TLS、BYOK、平台管理员 TOTP。
- **运维与 HA**：工作区文件、存储用量、S3/WebDAV、`/metrics`、告警、定时备份、Redis Sentinel、多区域主备。
- **已验证**：438 项后端测试；本地主备切区（JWT/Host）、21/21 跨租户审计、10 并发 run/40 SSE 基线。
- **生产加固**：MCP/沙箱/egress 默认拒绝、配置占位值拒绝、local-session 代理绕过、
  TOTP 防重放、迁移锁、备份原子性与权限、容器/任务租户隔离、SSRF DNS 固定、删除前备份、
  媒体校验、SSE 上限、登录共享限流、集成对象写权限、project_id 校验、导出脱敏、
  租户池 DSN 失效、暂停静默、PG 租约续期、egress 限流。
- **未交付**：计费支付/发票、双活多区域、TLS 解密式凭据哨兵、目标环境容量/生产 RPO/RTO。

## 2. 已交付（按域）

| 域 | 关键能力 | 落点 |
| :--- | :--- | :--- |
| 平台核心 | 租户生命周期、Host/JWT 双校验、迁移、配额、BYOK、导出/删除/备份 | `src/control/`、`src/tenant/` |
| 模型服务 | key 池/目录/路由/失败转移/发现/探测；用户隐藏模型 | `src/models/`、`src/provider/`、`web/components/models/` |
| 租户前台 | 聊天、审批、运行面板、命令面板、导入导出/分享、媒体、记忆、集成 | `web/app/`、`web/components/` |
| 运维 | 工作区文件、存储用量、`/metrics`、告警、备份、TLS 探测、平台 TOTP | `src/sessions/files_api.rs`、`src/metrics.rs`、`src/alerts.rs`、`src/control/` |
| 存储 | local / WebDAV / S3（SigV4，兼容 MinIO） | `src/media/storage.rs` |
| HA / 多区域 | Redis Sentinel 热切换；region 绑定 fail-closed、主备 dump/restore 切流 | `src/redis_ha.rs`、`src/tenant/`、`scripts/region/` |
| 门禁与工具 | i18n AST 守卫、CI web job、k6/local-capacity、跨租户审计 | `web/scripts/`、`scripts/loadtest/`、`scripts/security/` |
| 生产加固 | 集成对象写权限、project_id 校验、MCP 重定向/SSE 端点、导出脱敏、备份权限、事件队列上限、租户池 DSN 失效、暂停静默、租约续期、egress 限流 | `src/integration/`、`src/control/`、`src/sessions/`、`src/tenant/` |
| 发布门禁 | Playwright 覆盖控制台登录/建租户/TLS/TOTP；CI 真实 `pg_dump`/`pg_restore` 备份演练 | `console/e2e/`、`.github/workflows/ci.yml`、`scripts/backup/` |

## 3. 下一批任务

### P0：发布前验收闭环

| # | 任务 | 验收 | 主要文件 |
| :--- | :--- | :--- | :--- |
| P0-1 | 目标环境容量与 SLA | 工具/阈值已就绪（`scripts/loadtest/`）；待目标环境记录并发 run、SSE、容器密度与 p95/p99 | `scripts/loadtest/`、`docs/deploy.md` |
| P0-2 | 生产主备切区演练 | 切流/回滚步骤已文档化；待真实 DNS/反代切流并记录 RPO/RTO | `scripts/region/failover.sh`、`docs/deploy.md §8` |
| P0-3 | ✅ 备份恢复演练 | CI 真实 dump/restore；本地 6 表 checksum PASS；脚本与值班步骤见 `scripts/backup/` | `src/control/backup.rs`、`scripts/` |

### P1：产品与运维补齐

| # | 任务 | 验收 | 主要文件 |
| :--- | :--- | :--- | :--- |
| P1-1 | 计费账单 | 账单汇总/CSV、发票 JSON+外部支付状态回写、套餐 `overage_policy`、rollup 对账已交付；具体支付渠道仍由外部系统接入 | `src/control/usage_rollup.rs`、平台 API |
| P1-2 | ✅ TOTP 恢复与锁定防护 | 一次性恢复码、平台管理员重置流程、失败限速 | `src/control/mod.rs`、`console/` |
| P1-3 | ✅ 指标与告警收敛 | `/metrics` Bearer/反代白名单文档化；告警冷却窗口 | `src/metrics.rs`、`src/alerts.rs` |
| P1-4 | ✅ 存储配额准确性 | 媒体+工作区统一计数；S3/WebDAV 迁移与孤儿清理 | `src/control/usage_rollup.rs`、`src/media/` |
| P1-5 | ✅ 控制台 i18n 与 E2E | zh/en + Playwright 覆盖登录、建租户、TLS、TOTP、发票/对账（4/4 通过） | `console/` |
| P1-6 | ✅ 迁移安全 | 升级前备份门禁；CI 真实 dump/restore 校验 | `src/control/tenant_migrations.rs`、CI |

### P2：规模化

- **区域自动化（部分）**：`healthcheck.sh` 输出主/备区健康与切流建议；`auto-failover.sh`
  连续失败后才触发，默认演练，需 `AUTO_FAILOVER=1` + `CONFIRM`；`failover.sh` 支持
  `DNS_SWITCH_HOOK` 调用外部 DNS/反代自动化；平台 `/regions` 暴露租户分布与未绑定区域告警，
  `/regions/backfill`（默认 dry-run）用于严格绑定前的区域回填；`CHIRON_REGION_STRICT=1`
  拒绝未绑定区域租户被任意区域服务，`CHIRON_READ_ONLY=1` 可让只读副本承接读流量。
  真正的跨区强一致与无人值守仲裁仍单独立项。
- **双活多区域**：跨区强一致控制库/Redis、并发写仲裁；当前只有人工主备。
- **凭据哨兵（部分）**：egress 仅允许 HTTPS CONNECT:443，明文 HTTP 需显式
  `CHIRON_EGRESS_ALLOW_PLAINTEXT=1`，且明文请求携带 Authorization/Cookie/API key 仍被拒绝；
  `/metrics` 暴露 egress 允许/拒绝计数，`CHIRON_ALERT_EGRESS_DENIED` 可对拒绝峰值告警。
  TLS 解密与内容级凭据检测仍单独立项。

**建议顺序**：P0-1（容量基线）→ P0-2（生产切流）→ P1-1（仅剩外部支付/发票）→
P2 区域自动化接入真实 DNS → 凭据哨兵/双活单独立项。

## 4. 已知缺口与风险

| 缺口 | 影响 | 下一步 |
| :--- | :--- | :--- |
| 目标环境未压测 | SLA/容器密度/连接数无实测上限 | P0-1 |
| 双活未实现 | 主备切流需人工/DNS 配合 | P2 |
| 计费未接入 | 账单/发票/对账/超额策略已实现；具体支付渠道未接 | P1-1 |
| 凭据哨兵仅基线 | egress 默认只允许 HTTPS，但不做 TLS 解密内容检测 | P2 |
| 迁移无 down | 回滚依赖按库备份 | P1-6 |
| 工作区字节可能为 0 | 容器内未挂载 data_root 时配额失真 | P1-4 |
| 工作区无磁盘配额 | 容器可写满宿主卷，需文件系统/project quota 兜底 | 基础设施 |
| 双活/凭据哨兵未交付 | 跨区强一致与 TLS 解密检测仍单独立项 | P2 |

## 5. 验收门禁

- 后端：`cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all`
  （Windows 用 `scripts\check.cmd`；Linux/macOS 用 `make check`；本机缺 fmt/clippy 时以 CI 为准）。
- 前端：`cd web && npm run typecheck && npm run check:i18n`；`console && npm run build`。
- 文档：改接口/错误码/环境变量/里程碑时同步 `docs/API.md`、`docs/deploy.md`、`docs/PRD.md`、`CHANGELOG.md`；
  状态与排期只改本文。
