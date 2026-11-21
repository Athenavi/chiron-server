# 压测（P2）

k6 脚本，覆盖租户 API 的关键路径：

| 脚本 | 覆盖 |
| :--- | :--- |
| `chiron-loadtest.js` | 会话列表、创建会话、提交消息、SSE 连接 + 带 `Last-Event-ID` 重连、文件上传 |
| `sse-reconnect.js` | 真实 SSE 流解析：首事件延迟、心跳、`run.gap`、断线续传（`k6/x/sse`） |

两个脚本都只使用 `/api/v1/*` 租户 API，**不包含**删除、`/admin/*`、平台 API、
暂停/恢复等任何破坏性或跨租户操作。

## 1. 准备

- k6 ≥ 1.0。`sse-reconnect.js` 依赖 `k6/x/sse`；新版 k6 会自动解析扩展，
  旧版需要 `xk6 build --with github.com/phymbert/xk6-sse@latest`。
- 一个**专用压测租户**和专用账号（owner 或普通成员均可，普通成员更贴近真实用户）。
- 目标地址必须是租户 Host（`https://<slug>.<base>` 或自定义域名）。Caddy 按
  `tenant_domains` 解析租户，直接打 IP 而不带 Host 头只会得到 404。
- 本地/自签证书：
  - `k6 run --insecure-skip-tls-verify ...`；或
  - 用 `LT_HOST_MAP="acme.apps.example.com=127.0.0.1:8443"` 把域名映射到本机端口，
    Host 与 SNI 仍是租户域名。

## 2. 环境变量

| 变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `LT_BASE_URL` | 必填 | 租户 origin，如 `https://acme.apps.example.com` |
| `LT_TOKEN` | 空 | 预取 JWT；提供后不再登录 |
| `LT_IDENTIFIER` / `LT_PASSWORD` | 空 | 没有 token 时在 `setup()` 里登录一次 |
| `LT_SESSION_ID` | 空 | 已存在会话；只读模式下测 SSE 必需 |
| `LT_MODEL` | 空 | 提交消息时指定的模型；留空用服务端默认 |
| `LT_ALLOW_WRITES` | `no` | 允许创建会话/上传文件 |
| `LT_ALLOW_RUNS` | `no` | 允许提交消息（会真的启动 agent run） |
| `LT_INTERRUPT` | `yes` | 提交后立刻中断 run，限制 token/沙箱消耗 |
| `LT_ENABLE_SSE` / `LT_ENABLE_UPLOAD` | `yes` | 关闭对应步骤 |
| `LT_SSE_TIMEOUT` | `8s` | 探针保持连接的时长 |
| `LT_LAST_EVENT_ID` | `0` | 重连时回放的起点 seq |
| `LT_SCENARIO` | `smoke` | `smoke` / `ramp` / `soak` |
| `LT_VUS` / `LT_RAMP` / `LT_HOLD` / `LT_DURATION` | 5 / 30s / 2m / 10m | 场景参数 |
| `LT_HOST_MAP` | 空 | `host=addr,host2=addr2`，写入 `options.hosts` |
| `LT_ALLOW_PROD` | `no` | 目标命中 `prod/production/live` 时的显式放行 |
| `LT_SSE_HOLD` / `LT_SSE_EVENTS` / `LT_SSE_RECONNECTS` | 20s / 3 / 2 | 仅 `sse-reconnect.js` |
| `LT_SSE_EXPECT_ACTIVE_RUN` | `no` | 为 `yes` 才校验首事件延迟阈值 |

## 3. 运行

```sh
# 冒烟：只读，1 个 VU，验证鉴权/Host/SSE 通路
LT_BASE_URL=https://acme.apps.example.com LT_TOKEN=... \
  k6 run scripts/loadtest/chiron-loadtest.js

# 阶梯：只读并发，观察反代与限流表现
LT_BASE_URL=... LT_TOKEN=... LT_SCENARIO=ramp LT_VUS=50 LT_RAMP=1m LT_HOLD=5m \
  k6 run scripts/loadtest/chiron-loadtest.js

# 写路径：会话 + 上传（不含模型调用）
LT_BASE_URL=... LT_TOKEN=... LT_ALLOW_WRITES=yes \
  k6 run scripts/loadtest/chiron-loadtest.js

# 含模型：会真实产生 token 成本，默认提交后立即中断
LT_BASE_URL=... LT_TOKEN=... LT_ALLOW_WRITES=yes LT_ALLOW_RUNS=yes LT_INTERRUPT=yes \
  LT_MODEL=<租户目录内模型> k6 run scripts/loadtest/chiron-loadtest.js

# 长稳：4 小时 soak
LT_BASE_URL=... LT_TOKEN=... LT_SCENARIO=soak LT_VUS=20 LT_DURATION=4h \
  k6 run scripts/loadtest/chiron-loadtest.js

# SSE 深度验证（真实解析事件流 + 续传）
LT_BASE_URL=... LT_TOKEN=... LT_SESSION_ID=<uuid> \
  k6 run scripts/loadtest/sse-reconnect.js
```

## 4. 阈值（起点，需按环境校准）

`chiron-loadtest.js`：

| 指标 | 阈值 | 含义 |
| :--- | :--- | :--- |
| `checks` | `rate>0.99` | 业务断言通过率 |
| `http_req_duration{expected_response:true}` | `p(95)<800ms`、`p(99)<2500ms` | 排除 SSE 探针（超时不算响应） |
| `http_req_failed{expected_response:true}` | `rate<0.01` | 5xx/连接错误 |
| `session_list_duration` | `p(95)<500ms` | 首屏列表 |
| `session_create_duration` | `p(95)<1200ms` | 仅写模式 |
| `message_submit_duration` | `p(95)<2000ms` | 仅含模型模式；不含 run 执行时间 |
| `upload_duration` | `p(95)<3000ms` | 1KB 文件，主要测鉴权/落库/审计 |
| `sse_stream_alive` | `rate>0.98` | 探针在超时窗口内保持连接的比例 |

`sse-reconnect.js`：`sse_stream_failures`/`sse_reconnect_failures` `rate<0.01`，
`sse_events_total` `count>0`；`sse_first_event_ms` 仅在
`LT_SSE_EXPECT_ACTIVE_RUN=yes` 时启用（空闲会话只有 15s 心跳，不能当 TTFT）。

阈值失败会以非零退出码结束，可直接接 CI。首次在任何环境跑之前先做只读冒烟；
把实测 p95/p99 填进 ROADMAP/SLA 文档，而不是反过来用这套默认值对外承诺。

## 5. 如何避免破坏性影响

1. **写操作默认关闭**。仅只读（列表 + SSE 探针）不会创建任何数据。
2. **提交消息单独开关**。`POST /sessions/{id}/messages` 会创建 queued run 并真的
   调用模型（token/成本/沙箱），因此必须同时给出 `LT_ALLOW_RUNS=yes` 与
   `LT_ALLOW_WRITES=yes`；默认 `LT_INTERRUPT=yes` 会在成功后立即中断，避免跑满。
3. **不做删除/清理**。脚本不调用 `DELETE`、不建用户、不动套餐与域名。压测产生的
   会话与媒体文件会留在专用租户里：用一次性租户，或按库备份/平台删除租户收尾。
4. **不压登录**。没有 token 时只在 `setup()` 登录一次；失败直接中止，避免触发
   `auth.locked` 并把压测账号锁死。
5. **生产拦截**。`LT_BASE_URL` 命中 `prod/production/live` 时拒绝启动，除非
   `LT_ALLOW_PROD=yes`。即便如此，也不要对生产租户开 `LT_ALLOW_RUNS`。
6. **配额影响**。并发 run、存储、token 套餐都会计入真实用量；压测前确认租户套餐
   足够，压测后核对 `/admin/usage`。限流返回 `429` 属于预期行为，脚本会把它算作
   `http_req_failed`，需要单独看 `quota.exceeded` 比例。

## 6. 已知限制

- stock k6 不能边收边解析 SSE，主脚本用"短超时探针"判断流是否保持打开：
  它能验证鉴权、Host、反代不缓冲/不提前关闭，但测不到真实 TTFT 与心跳间隔。
  需要这些数据时用 `sse-reconnect.js`。
- 探针会让 `http_req_failed` 增加超时计数，因此阈值只针对
  `{expected_response:true}`，不要改成全局 `http_req_failed`。
- 反向代理若把 `flush_interval` 配错，探针同样会"通过"（连接保持打开但数据被攒住）；
  真实缓冲问题只能在 `sse-reconnect.js` 的首事件延迟/心跳间隔里暴露。

## 7. 本地容量基线（2026-11-22，mock Provider）

没有 Docker/真实模型时，可用 `mock-provider.mjs` + `local-capacity.mjs` 跑控制面与
run 调度基线（不包含沙箱执行、真实模型延迟）：

```bash
MOCK_PORT=19090 node scripts/loadtest/mock-provider.mjs &
# server: CHIRON_PROVIDER_OPENAI_BASE_URL=http://127.0.0.1:19090/v1 OPENAI_API_KEY=mock
BASE=http://127.0.0.1:18084 HOST=bench.apps.local TOKEN=... \
SESSIONS=20 CONCURRENCY=10 SSE=40 node scripts/loadtest/local-capacity.mjs
```

本机结果（一次性 temp PG + 单 server，标准套餐调高并发到 10）：

| 指标 | 结果 |
| :--- | :--- |
| 创建 20 会话 | 64 ms |
| 20 个 run / 10 并发 | 全部 completed，wall 809 ms |
| run 延迟 | p50 379 ms / p95 413 ms / max 471 ms |
| SSE | 40/40 建立，31 ms |
| 失败 | 0 |

该基线只覆盖认证、租户路由、run 状态机、事件流与 mock 上游；**不覆盖**真实模型
TTFT、沙箱启动、容器密度与磁盘/网络上限，这些必须在目标环境用 k6 场景测。
