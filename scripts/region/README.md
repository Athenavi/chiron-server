# 区域切流（R-3，主备方案）

`healthcheck.sh` 是只读探测：对主/备区 `/readyz` 计时并输出 `keep` / `failover` /
`unavailable`（退出码 0/10/20），不修改任何状态。

`auto-failover.sh` 连续 `ROUNDS` 次判定 failover 后才行动；默认只打印计划（退出 10）。
真正执行必须 `--apply` + `AUTO_FAILOVER=1` + `CONFIRM=<slug>`，避免单次抖动触发切流。

`failover.sh` 是**人工确认**的主备切流脚本：先 suspend，`pg_dump`/`pg_restore` 到目标区，
再 PATCH 控制库 `region` 并 resume，最后校验目标区 `/healthz`。设置 `DNS_SWITCH_HOOK`
时会在验证目标区后、resume 前 POST 一个不含密钥的 JSON（tenant/host/region），用于
对接外部 DNS/反代自动化；未设置则仍需人工切 DNS。

## 健康探测

```bash
PRIMARY_BASE=https://app.cn-east.example.com \
STANDBY_BASE=https://app.cn-east-2.example.com \
TENANT_HOST=acme.apps.example.com \
./scripts/region/healthcheck.sh
```

主区 200 且低于 `MAX_PRIMARY_MS` 时保持；否则备区健康则建议切流。探测结果只用于决策，
不会自动执行 `failover.sh --apply`。

```bash
ROUNDS=3 SLEEP_SECS=30 TENANT_SLUG=acme \
PRIMARY_BASE=... STANDBY_BASE=... TENANT_HOST=acme.apps.example.com \
./scripts/region/auto-failover.sh          # 演练：连续 3 次 failover 只打印计划
# 受控自动化：加 --apply、AUTO_FAILOVER=1、CONFIRM=acme 及 failover.sh 所需变量
```

## 前置

- 两个区域各有一套完整数据面（server + tenant PG + Redis + sandbox），目标区
  `CHIRON_REGION` 与脚本的 `TARGET_REGION` 一致。
- 目标租户库已创建且可写；`TARGET_DSN` 具有建表/写入权限。
- 两区使用相同的 `JWT_SECRET`、`SECRETS_KEY`、`TENANT_BASE_DOMAIN`；否则切区后 token
  和加密 DSN 都会失效。
- 先 `pg_dump` 验证过恢复流程；目标区已跑完当前迁移版本。

## 使用

```bash
PLATFORM_BASE=https://platform.example.com \
PLATFORM_TOKEN=... TENANT_ID=... TENANT_SLUG=acme TENANT_HOST=acme.apps.example.com \
TARGET_REGION=cn-east-2 SOURCE_DSN='postgresql://...' TARGET_DSN='postgresql://...' \
./scripts/region/failover.sh              # 只打印计划

CONFIRM=acme ... ./scripts/region/failover.sh --apply
```

脚本只打印开始/结束时间，不打印 DSN 或 token；dump 默认在退出时删除。

## 验收与 RPO/RTO

1. 记录 `started`/`finished` 与备份时间点（RPO = 最后一次成功备份到故障点）。
2. 切流后验证：租户登录、`GET /sessions`、SSE 断线重连、媒体下载、owner 改密。
3. 旧区保持只读/暂停，确认无写入后再下线；回滚只能用切流前的 dump 恢复。
4. 每次演练把 RPO/RTO 写入值班记录；脚本本身不保证强一致，也不做双活。

## 回滚

1. **切换前**保存源区最后一次 dump 与 `SOURCE_DSN`；暂停后源库不再接受业务写入。
2. **目标区验证失败、DNS 尚未切换**：用相同的 `failover.sh` 反向执行（源/备参数互换，
   `TARGET_REGION` 改回原区），或直接 PATCH 回原 `region`/`dsn` 后 resume；旧库仍是切换点状态。
3. **DNS 已切换但业务异常**：先把 TTL 降到 60s 并切回旧区，再按需恢复步骤 1 的 dump；
   恢复前保持租户 suspended，避免双写。
4. **验证**：登录、会话列表、SSE 重连、媒体下载、审批恢复、`/regions` 区域归属。
5. 每次回滚记录时间线与 RPO/RTO；不要删除切换前后的 dump，直到观察窗口结束。

## 实测记录（2026-11-22，本机临时集群）

`drill.sh`：50,000 行、约 1.2 MB custom dump

| 指标 | 结果 |
| :--- | :--- |
| dump | 525 ms |
| restore | 573 ms |
| 校验 | `count=50000, sum(hashtext(payload))` 一致 |
| RPO | 以最后一次 dump 时间点为准（本演练 dump 时刻即恢复点） |
| RTO | 约 0.57 s（仅数据库恢复；不含 DNS/反代切流） |

另用两个 server 实例（`CHIRON_REGION=east/west`，共享控制库与 JWT）验证：

- 同一租户 JWT 在 east 返回 200、west 返回 `503 tenant_wrong_region`；
- `PATCH /tenants/{id} {"region":"west"}` 后 east 503、west 200，JWT 不失效；
- 说明区域切换只改控制库绑定，Host/JWT 校验与区域判断解耦。

未覆盖：真实 DNS 切流、跨机网络延迟、生产数据量、双活并发写。

## 限制

- 没有增量复制；`pg_restore --clean` 会覆盖目标库，必须确认 `TARGET_DSN` 指向备区空库。
- 不做 DNS 切换、不校验两区 schema 版本；切流前用平台 `/migrations` 对齐。
- Redis 不跨区共享，SSE 事件流在切换后从头/从快照重建。
