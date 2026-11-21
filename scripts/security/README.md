# 跨租户安全审计（P2）

`cross-tenant-audit.sh` 用真实 HTTP 请求验证租户边界：把 A 租户的令牌打到 B、未知、
暂停租户的 Host，以及反向组合，输出 pass/fail 矩阵。只发 `GET`（登录可选），
不创建、不修改、不删除任何数据。

## 1. 前置条件

准备以下对象（由平台管理员在控制台/API 侧提前建好，脚本不负责创建）：

- 租户 A：正常 `active`，有一个可用账号，能拿到 token（或提供账号密码）。
- 租户 B：另一个 `active` 租户（跨租户对照组），最好有独立的账号与会话/媒体数据。
- 一个**未在 `tenant_domains` 注册**的域名（未知 Host 用例），例如
  `audit-unknown.<base>`。
- 一个 `suspended` 状态的租户域名（可选）。暂停/恢复属于平台写操作，脚本不执行。
- 一个 A 租户里不存在的 B 租户对象 id（会话、媒体资产，可选）。

网络与证书：脚本默认连接 `AUDIT_BASE_URL` 的地址，只替换 `Host` 请求头，
因此入口必须是"通配 Host"式的反代（`deploy/Caddyfile` 就是这样）。如果你的
负载均衡只认固定站点名，需要 `AUDIT_CURL_EXTRA="--resolve <host>:443:<ip>"`，
并配上通配/多域名证书或 `AUDIT_INSECURE=1`。

## 2. 环境变量

| 变量 | 必填 | 说明 |
| :--- | :--- | :--- |
| `AUDIT_BASE_URL` | 是 | 租户 A 的 origin，如 `https://acme.apps.example.com`；连接地址与 SNI 都用它 |
| `AUDIT_HOST_A` | 否 | 租户 A 的 Host 头；默认取 `AUDIT_BASE_URL` 的 host |
| `AUDIT_HOST_B` | 是* | 租户 B 的 Host；跨租户用例必需 |
| `AUDIT_HOST_UNKNOWN` | 是* | 未注册域名；未知名 Host 用例必需 |
| `AUDIT_HOST_SUSPENDED` | 否 | 已暂停租户的 Host |
| `AUDIT_TOKEN_A` | 是* | 租户 A 的 JWT（推荐，避免登录） |
| `AUDIT_IDENTIFIER_A` / `AUDIT_PASSWORD_A` | 否 | 无 token 时用于登录；需 `AUDIT_ALLOW_LOGIN=1` |
| `AUDIT_TOKEN_B` | 否 | 租户 B 的 JWT；缺失则 B 侧基线用例跳过 |
| `AUDIT_IDENTIFIER_B` / `AUDIT_PASSWORD_B` | 否 | 同上，需 `AUDIT_ALLOW_LOGIN=1` |
| `AUDIT_PLATFORM_HOST` | 否 | `PLATFORM_HOST`；设置后启用平台 Host 用例 |
| `AUDIT_PLATFORM_TOKEN` | 否 | 平台管理员 token；用于"平台 token 打租户 Host"用例 |
| `AUDIT_SESSION_B_ID` | 否 | B 租户的会话 UUID；对象级越权用例 |
| `AUDIT_MEDIA_B_ID` | 否 | B 租户的媒体 UUID；对象级越权用例 |
| `AUDIT_INSECURE` | 否 | `1` = curl `-k`（自签证书） |
| `AUDIT_CURL_EXTRA` | 否 | 追加给 curl 的参数，如 `--resolve a.example.com:443:10.0.0.5` |
| `AUDIT_TIMEOUT` | 否 | 单请求超时秒数，默认 10 |
| `AUDIT_ALLOW_LOGIN` | 否 | `1` 才允许 `POST /auth/login` |
| `AUDIT_DRY_RUN` | 否 | `1` = 只打印矩阵，不发任何请求 |
| `AUDIT_VERBOSE` | 否 | `1` = 失败时额外打印期望值与排查提示（仍不打印 token/body） |
| `AUDIT_EXPECT_SUSPENDED_STATUS/CODE` | 否 | 默认 `503` / `tenant_suspended` |
| `AUDIT_EXPECT_UNKNOWN_STATUS/CODE` | 否 | 默认 `404` / `not_found` |

\* 见"前置条件"。令牌只在内存与请求头里使用，输出只显示 `<set:len=N>`。

## 3. 运行

```sh
export AUDIT_BASE_URL=https://acme.apps.example.com
export AUDIT_HOST_A=acme.apps.example.com
export AUDIT_HOST_B=beta.apps.example.com
export AUDIT_HOST_UNKNOWN=audit-unknown.apps.example.com
export AUDIT_TOKEN_A=...            # 或 AUDIT_IDENTIFIER_A/AUDIT_PASSWORD_A + AUDIT_ALLOW_LOGIN=1
export AUDIT_TOKEN_B=...
export AUDIT_SESSION_B_ID=<uuid>
export AUDIT_MEDIA_B_ID=<uuid>
export AUDIT_PLATFORM_HOST=platform.example.com

bash scripts/security/cross-tenant-audit.sh

# 先看用例清单，不触网：
AUDIT_DRY_RUN=1 bash scripts/security/cross-tenant-audit.sh
```

退出码：`0` 全部通过（或仅跳过）；`1` 有 FAIL；`2` 配置缺失。可直接接 CI。

## 4. 用例矩阵

| # | 用例 | 期望 |
| :--- | :--- | :--- |
| 1 | A token + A Host，`/auth/session` | 200（基线） |
| 2 | 无 token + A Host，`/sessions` | 401 `auth.required` |
| 3 | Host 大写 + 尾点（归一化） | 200（不得因大小写/尾点绕过或误拒） |
| 4 | Host 带端口 | 200（端口必须被剥离） |
| 5 | `X-Forwarded-Host` 伪造 B | 200（服务端只信 `Host`，不信 XFH） |
| 6 | A token + B Host | 401 `auth.required` |
| 7 | 无 token + B Host | 401 `auth.required` |
| 8 | B token + A Host | 401 `auth.required` |
| 9 | B token + B Host | 200（B 自身基线） |
| 10 | A token + 未知 Host | 404 `not_found` |
| 11 | 无 token + 未知 Host | 404 `not_found`（不能回落默认租户） |
| 12 | A token + 暂停租户 Host | 503 `tenant_suspended` |
| 13 | 无 token + 暂停租户 Host | 503 `tenant_suspended` |
| 14 | 平台 API + 租户 Host（A token） | 404 `not_found` |
| 15 | 平台 API + 未知 Host | 404 `not_found` |
| 16 | 租户 token + 平台 Host | 404 `not_found` |
| 17 | 平台 API + 平台 Host（无 token） | 401 `auth.required` |
| 18 | 平台 token + 租户 Host | 404 `not_found` |
| 19 | A token + A Host 读 B 的会话 id | 404 `not_found` |
| 20 | A token + A Host 读 B 的媒体 id | 404 `not_found` |
| 21 | A token + B Host 读 B 的媒体 id | 401 `auth.required` |

判定同时比对 HTTP 状态与稳定错误码，只看 message 文案不算通过。

## 5. 结果解读与常见失败

- **6/8 返回 200**：Host 与 token 的租户校验失效，最高危，先查 `require_auth` 的
  Host 绑定是否被绕过。
- **11 返回 200**：未知 Host 回落到了某个默认租户；检查 `tenant_domains` 种子数据
  与反代是否改写 Host。
- **12/13 返回 404**：域名记录被删或大小写不一致；暂停租户应保留 `tenant_domains`
  行并置 `tenants.status='suspended'`。
- **19/20 返回 200**：对象级越权，说明查询没带租户连接池/用户范围。
- **3/4 失败**：`normalize_host` 行为被改（大小写、端口、尾点），属于边界回归。
- **全部 3xx**：`AUDIT_BASE_URL` 用了 `http://`，改成 `https://`。
- **全部连接失败**：入口不是通配 Host 站点，或证书不覆盖测试 Host；用
  `AUDIT_CURL_EXTRA=--resolve ...` / `AUDIT_INSECURE=1`。

## 6. 留痕

被拒绝的请求发生在租户解析/认证中间件，**不会写入租户审计表**（审计只记录
通过认证的写操作）。验证"拒绝有留痕"要看反代 JSON 访问日志：

```sh
# Caddy：按 Host + 状态码筛出本次审计的拒绝请求
grep -E '"host":"(beta|audit-unknown)\.' /var/log/caddy/chiron-access.log | tail -n 50
```

建议把这批 401/404/503 接入告警，并在每次发布后跑一次本脚本与
`scripts/loadtest/` 的只读冒烟，作为租户边界的回归门禁。

## 7. 本机演练记录（2026-11-22）

用临时 PostgreSQL + chiron-server 起 3 个租户（A/B/暂停 C），三套 owner token 与
B 的 session/media 对象；脚本完整跑 21 项：

- **通过 21，失败 0，跳过 0**；
- A token 打 B/未知/暂停 Host 分别为 401/404/503，未出现回落默认租户；
- 平台 API 在租户 Host、租户 token 在平台 Host 均为 404；
- A token 读 B 的会话/媒体均为 404。

证据只在本机（未接反代 JSON 日志）；正式上线仍需在目标入口复跑并保留访问日志。
