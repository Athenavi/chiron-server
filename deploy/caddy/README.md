# deploy/Caddyfile 多租户说明（P1-5）

本目录服务于 `deploy/Caddyfile`：单实例 Caddy 终结 TLS 并按 Host 透传给服务端。
服务端才是租户边界（查控制库 `tenant_domains`），Caddy 只负责"为这个域名签证书"和
"把请求原样转给 127.0.0.1:8088"。

| 文件 | 作用 |
| :--- | :--- |
| `../Caddyfile` | 可部署配置：通配 Host + on-demand TLS |
| `wildcard-dns-tls.example` | 追加片段：用 DNS-01 通配符证书替代按需签发（需自定义 Caddy 构建） |

## 1. 域名与 DNS 前置条件

三类域名共用同一个入口：

1. **平台控制台** `PLATFORM_HOST`（如 `platform.example.com`）。
2. **租户子域名** `<slug>.<TENANT_BASE_DOMAIN>`（如 `acme.apps.example.com`）。
   开通租户时服务端自动写入 `tenant_domains(kind='subdomain')`，视为已信任，不走 TXT。
3. **租户自定义域名**：平台 API `POST /api/platform/v1/tenants/{id}/domains` 绑定，
   需要 DNS TXT `chiron-verify=<tenant_uuid>` 并通过
   `POST /api/platform/v1/tenants/{id}/domains/{domain_id}/verify` 验证归属。

DNS 侧要求：

- 子域名模式：`*.apps.example.com` 与 `apps.example.com` 的 A/AAAA（或 CNAME）
  指向本机/负载均衡；也可以逐租户建记录，二者对按需签发等价。
- 自定义域名：由租户在自有 DNS 里把域名 CNAME/A 指向同一入口；TXT 只用于归属验证，
  与证书签发无关。
- **CAA**：若 base 域配置了 CAA，必须允许 Caddy 默认使用的 ACME CA（Let's Encrypt
  与 ZeroSSL）；也可以全局 `acme_ca` 固定一家后只放行该家。CAA 拒绝时握手失败，
  日志里能看到 `urn:ietf:params:acme:error:caa`。
- 公网必须能访问 80/443：HTTP-01 挑战走 80，按需签发的证书都是单域名证书。
- 如果前面还有 CDN/WAF 终结 TLS，Caddy 就不再需要签发证书；此时应关闭本文件的
  `tls { on_demand }` 并把 CDN 回源 Host 配成原始租户域名，否则服务端解析不到租户。

## 2. on-demand TLS 的 ask 端点契约

`deploy/Caddyfile` 的全局块要求环境变量 `CHIRON_TLS_ASK_URL`，没有默认值。
Caddy 的行为（见 Caddy 文档 `on_demand_tls`）：

- 首次为该域名签发时，Caddy 发出 `GET <CHIRON_TLS_ASK_URL>?domain=<fqdn>`；
- **2xx** = 允许签发；其它状态码 = 拒绝，TLS 握手直接失败（不会去 CA 排队）；
- 已有证书的续期不再询问 ask。

本仓库内置了该端点：`GET /api/platform/v1/tls/allow?domain=<fqdn>`（仅平台 Host 可达，
无登录态）。把 `CHIRON_TLS_ASK_URL` 指到它即可，例如
`https://platform.example.com/api/platform/v1/tls/allow`。它满足：

| 项 | 要求 |
| :--- | :--- |
| 方法/参数 | `GET`，只读 `domain` 查询参数；不要求也不应要求 Cookie |
| 判定 | 规范化（小写、去端口、去尾点）后精确匹配 `tenant_domains.host`；租户 `status='active'`；`kind='subdomain'` 或 `custom` 且 `verified_at IS NOT NULL`；平台 Host 放行 |
| 拒绝 | 未知名、暂停/未开通租户、未验证的自定义域名一律 4xx，不要 5xx 混用 |
| 延迟 | 毫秒级；单次索引查询，不做 DNS/HTTP 调用 |
| 网络 | 只监听回环或内网；不要暴露公网。URL 里不要放长期凭据（会进日志） |
| 滥用 | `interval`/`burst` 已不再推荐（官方建议移除），必须在端点自己做每域名/IP 的失败计数与负缓存 |
| 可用性 | ask 挂掉时 **新域名** 握手全部失败（旧证书仍可用）。要有自己的健康检查与告警 |
| 输出 | 响应体不参与判定；不要回显租户 id/邮箱等内部信息 |

示例请求（URL 只是占位，必须换成你自己的）：

```sh
curl -i -H "Host: platform.example.com" \
  "http://127.0.0.1:8088/api/platform/v1/tls/allow?domain=acme.apps.example.com"
# 200 → 允许签发；404/400 → 拒绝
```

需要排查"为什么不签发"时，先手工打一次 ask，再对照 `journalctl -u caddy` 里的
`obtaining certificate` 记录；两边都通过才可能失败在 DNS/CAA。

## 3. 签发、续期与存储

- 首次握手触发签发；Caddy 会自动重试失败（退避），不需要人工触发。
- 续期：`renewal_window_ratio` 默认 `0.3333`（90 天证书剩 30 天开始续），
  `renew_interval` 默认 `10m` 扫描一次。续期同样需要 80/443 可达（HTTP-01）。
- Caddy 的证书、私钥、ACME 账号在数据目录（默认 `/var/lib/caddy/.local/share/caddy`
  或 `$XDG_DATA_HOME/caddy`）。**必须持久化**：容器里挂 volume，重装机器前先备份。
  数据目录丢失 = 全部域名重新签发，会撞 CA 速率限制
  （Let's Encrypt 对同一注册域名的精确域名有每 7 天 50 张的上限）。
- 多实例：只有共享 `storage` 模块时才可多实例同域，否则会互相抢签发。
- 测试环境用 staging CA，避免消耗生产配额：

  ```caddyfile
  {
      acme_ca https://acme-staging-v02.api.letsencrypt.org/directory
  }
  ```

- 证书状态回写：服务端 `tenant_domains.tls_status` 目前只在平台侧流转，Caddy 不会
  回写；控制台显示 `pending` 不代表证书未签发。以握手/浏览器证书与 Caddy 日志为准。

## 4. 校验、上线与回滚

```sh
# 语法/适配校验（ask URL 是必需变量，随便填一个占位即可，不会发起请求）
CHIRON_TLS_ASK_URL=http://127.0.0.1:9000/tls/ask \
CHIRON_ACME_EMAIL=ops@example.com \
  caddy validate --config deploy/Caddyfile

# 查看实际生效的 JSON（含站点与 TLS 策略）
CHIRON_TLS_ASK_URL=... caddy adapt --config deploy/Caddyfile --pretty

# 上线
install -m 0644 deploy/Caddyfile /etc/caddy/Caddyfile
systemctl reload caddy

# 回滚
install -m 0644 /etc/caddy/Caddyfile.bak /etc/caddy/Caddyfile && systemctl reload caddy
```

`deploy/Caddyfile` 保留了原有行为：SSE 路径 `flush_interval -1`、70MB 请求上限、
安全响应头（HSTS/nosniff/DENY/no-referrer/Permissions-Policy/COOP、隐藏 Server），
并显式覆盖 `X-Forwarded-For`（客户端伪造 IP 会绕过登录限流与审计）。
反向代理必须透传 `Host`，否则所有租户都会 404。

`https://` 是通配站点，Caddy 仍会在 80 端口提供 ACME HTTP-01 挑战与
HTTP→HTTPS 跳转。如果某版本/前置代理导致 80 端口不跳转，显式加一个站点即可
（挑战路由优先于自定义站点，不会被这条吞掉）：

```caddyfile
http:// {
	redir https://{host}{uri} permanent
}
```

单站点部署（只有一个域名、不需要按需签发）可以把站点地址从 `https://` 改成
`{$CHIRON_DOMAIN:chat.example.com}` 并删掉 `tls { on_demand }` 块，其余指令不变；
此时不再需要 `CHIRON_TLS_ASK_URL`。

## 5. 通配符证书（可选）

`wildcard-dns-tls.example` 给出 `*.<base>` + DNS-01 的写法。它需要：

1. 用 `xcaddy build --with github.com/caddy-dns/<provider>` 构建带 DNS 插件的 Caddy；
2. DNS 提供商 API 凭据放进环境变量（Caddy 会在 `_acme-challenge.<base>` 写 TXT，
   验证后删除；凭据只需该 zone 的编辑权限，不要用账号级 root key）；
3. base 域的 CAA 允许该 CA；
4. 递归解析器能及时看到新 TXT（多数提供商秒级生效；若用自建权威 DNS，
   注意 NOTIFY/AXFR 与 TTL，否则会卡在 `propagation` 阶段）。

通配符的好处是新租户上线不需要等任何签发流程；代价是证书覆盖整个 base 域，
且自定义域名仍然要走 on-demand（通配符不覆盖外部域名）。stock Caddy 没有 DNS
插件，直接套用会报 `unknown module`。

DNS-01 不需要 80/443 对公网开放，适合入口在内网/被 CDN 遮挡的部署；代价是持有
DNS API 凭据，泄露即可被签任意证书，凭据要按最小权限轮换。

## 6. 常见故障

| 现象 | 排查 |
| :--- | :--- |
| 握手失败、无证书 | ask 返回非 2xx；`journalctl -u caddy` 搜 `on-demand`/`obtaining certificate` |
| 所有租户 404 | Host 被改写；确认 `header_up Host {host}` 未被删除 |
| SSE 一次性输出或卡住 | `flush_interval -1` 丢失；或上游前面还有一层缓冲代理 |
| 续期失败 | 80/443 被防火墙挡住；CAA 拒绝；数据目录不可写 |
| 上传 413 | `request_body max_size` 低于实际体；旧版 Caddy 不支持该指令 |
| 证书配额耗尽 | 数据目录未持久化、或 ask 放行了过多域名；临时切 staging/其它 CA |
