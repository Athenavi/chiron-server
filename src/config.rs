use std::env;

use crate::{checkpoint::CheckpointConfig, sandbox::SandboxConfig, subagent::SubagentConfig};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    pub redis_url: String,
    /// 当前部署区域标识（`CHIRON_REGION`）；用于健康检查/指标与新租户默认 region。
    pub region: String,
    /// 多区域严格绑定：开启后 region 为空/default 的租户不再被任意区域服务。
    pub region_strict: bool,
    /// 只读数据面：拒绝所有租户写请求（读副本/备用区承接读流量）。
    pub read_only: bool,
    /// R-2：Sentinel 地址列表（`CHIRON_REDIS_SENTINEL_URLS`，逗号分隔）。
    /// 非空时优先走 Sentinel，`REDIS_URL` 只作单节点回退。
    pub redis_sentinel_urls: Vec<String>,
    /// Sentinel 监控的主节点名（`CHIRON_REDIS_SENTINEL_MASTER`，默认 `mymaster`）。
    pub redis_sentinel_master: String,
    /// Sentinel 发现的主节点密码（`CHIRON_REDIS_PASSWORD`，可选）。
    pub redis_password: String,
    pub jwt_secret: String,
    pub jwt_hours: i64,
    pub cookie_secure: bool,
    pub deploy_mode: String,
    pub service_mode: bool,
    /// 控制库（租户注册表/套餐/平台管理员）；为空 = 平台 API 关闭。
    pub control_database_url: String,
    /// 平台控制台 Host（如 platform.example.com）；空 = 仅开发回环。
    pub platform_host: String,
    /// 租户子域名基域（如 app.example.com），用于自动生成 <slug>.<base>。
    pub tenant_base_domain: String,
    pub platform_bootstrap_email: String,
    pub platform_bootstrap_password: String,
    /// 建库/建角色使用的管理连接串；空 = 复用 `CONTROL_DATABASE_URL`。
    /// 该角色需要 CREATEDB / CREATEROLE。
    pub provision_database_url: String,
    /// 控制库敏感字段加密密钥（base64 的 32 字节）。
    pub secrets_key: String,
    /// 每租户连接池上限。
    pub tenant_pool_max: u32,
    /// 连接池注册表最多缓存的租户数（超出按最久未用回收）。
    pub tenant_pool_max_tenants: usize,
    /// 空闲多久回收租户连接池（秒）。
    pub tenant_pool_idle_secs: u64,
    /// 定时汇总各租户用量的间隔（秒）；0 = 关闭。
    pub usage_rollup_secs: u64,
    /// 租户备份目录（pg_dump custom 格式）；空 = 禁用备份接口。
    pub backup_dir: String,
    /// 定时备份间隔（秒）；0 = 关闭（只保留手动接口）。
    pub backup_secs: u64,
    /// 备份保留天数；0 = 不自动删除（保留全部历史文件）。
    pub backup_keep_days: u64,
    /// 失败告警 Webhook（JSON POST）；空 = 关闭。
    pub alert_webhook_url: String,
    /// 同类告警冷却秒数；0 = 不去重。
    pub alert_cooldown_secs: u64,
    /// `/metrics` 可选 Bearer token；空 = 沿用未认证行为。
    pub metrics_token: String,
    /// 外部支付回写 HMAC 密钥；空 = 关闭 billing webhook。
    pub billing_webhook_secret: String,
    /// 支付宝开放平台应用与密钥；缺任一项则支付宝功能关闭。
    pub alipay_app_id: String,
    pub alipay_private_key: String,
    pub alipay_public_key: String,
    pub alipay_gateway: String,
    pub alipay_notify_url: String,
    pub alipay_return_url: String,
    /// 域名 TLS 状态探测间隔；0 = 关闭。
    pub tls_probe_secs: u64,
    /// 单域名 TLS 探测超时秒数（默认 5）。
    pub tls_probe_timeout_secs: u64,
    /// 活跃 run 告警阈值（0=关闭）。
    pub alert_active_runs: u64,
    /// 缓存租户池告警阈值（0=关闭）。
    pub alert_tenant_pools: u64,
    /// 单个看门狗窗口内 egress 拒绝数达到该值告警（0=关闭）。
    pub alert_egress_denied: u64,
    /// egress 代理监听地址（如 0.0.0.0:3128）；空 = 不启动代理。
    pub egress_proxy_bind: String,
    /// 代理是否放行明文 HTTP；默认关闭，避免凭据以明文出网。
    pub egress_allow_plaintext: bool,
    /// 沙箱内可见的代理地址（如 host.docker.internal:3128）。
    pub egress_proxy_addr: String,
    /// 媒体库根目录；空 = `<SANDBOX_DATA_ROOT>/media`。
    pub media_root: String,
    /// 媒体存储后端：local（默认）；s3/webdav 预留。
    pub media_backend: String,
    pub sandbox: SandboxConfig,
    pub agent: AgentConfig,
    /// B6：子 Agent 上限（深度/并发/预算）。
    pub subagent: SubagentConfig,
    /// B7：检查点与巡检。
    pub checkpoint: CheckpointConfig,

    pub memory: MemoryConfig,
    /// B9：MCP/插件/技能/市场。
    pub integration: IntegrationConfig,
    /// B10：三级限流。
    pub rate_limit: RateLimitConfig,
    /// B11：事件日志 / 租约 / 节点亲和。
    pub multi: MultiInstanceConfig,
}

/// B11 多实例参数（v1 单节点部署也走同一套接口）。
///
/// - `node_id` 写进 `user_containers.node`，用于容器亲和；
/// - 事件日志保留窗口：条数 `event_capacity` + 时间 `event_retention_secs`；
/// - `pg_lease_backstop` 是 Redis 之外的 PG CAS 兜底（关掉只影响测试）。
#[derive(Clone, Debug)]
pub struct MultiInstanceConfig {
    pub node_id: String,
    /// 事件日志保留条数（Redis `XADD MAXLEN ~`）。
    pub event_capacity: usize,
    /// 事件日志保留时长（秒）。
    pub event_retention_secs: u64,
    /// run 租约 TTL（秒）：拿不到就说明别的实例在跑。
    pub lease_ttl_secs: u64,
    /// Redis 事件日志的 tail 轮询间隔（毫秒）。
    pub tail_poll_ms: u64,
    /// 事件日志保留窗口维护任务（leader 每 60s 一次）。
    pub maintenance_enabled: bool,
    /// Redis 租约之外的 PG CAS 兜底。
    pub pg_lease_backstop: bool,
}

impl Default for MultiInstanceConfig {
    fn default() -> Self {
        Self {
            node_id: "local".to_string(),
            event_capacity: 1024,
            event_retention_secs: 24 * 60 * 60,
            lease_ttl_secs: 60,
            tail_poll_ms: 250,
            maintenance_enabled: true,
            pg_lease_backstop: true,
        }
    }
}

impl MultiInstanceConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            node_id: env_or("CHIRON_NODE_ID", &d.node_id),
            event_capacity: env_parse("CHIRON_EVENT_CAPACITY", d.event_capacity).max(16),
            event_retention_secs: env_parse("CHIRON_EVENT_RETENTION_SECS", d.event_retention_secs)
                .max(60),
            lease_ttl_secs: env_parse("CHIRON_LEASE_TTL_SECS", d.lease_ttl_secs).max(5),
            tail_poll_ms: env_parse("CHIRON_EVENT_TAIL_POLL_MS", d.tail_poll_ms).clamp(50, 5_000),
            maintenance_enabled: env_flag("CHIRON_EVENT_MAINTENANCE", d.maintenance_enabled),
            pg_lease_backstop: env_flag("CHIRON_PG_LEASE_BACKSTOP", d.pg_lease_backstop),
        }
    }
}

/// 单级限流参数：`burst` 用于令牌桶容量，`per_minute` 是稳态速率。
///
/// 实际生效的是三级桶：用户（或登录 IP）按本参数，租户 ×4，全局 ×20
/// （见 `admin::limits`）。倍数写死在代码里，避免 env 组合爆炸。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatePolicy {
    pub burst: u32,
    pub per_minute: u32,
}

impl RatePolicy {
    pub fn new(burst: u32, per_minute: u32) -> Self {
        Self {
            burst: burst.max(1),
            per_minute: per_minute.max(1),
        }
    }

    fn from_env(prefix: &str, default: Self) -> Self {
        Self::new(
            env_parse(&format!("CHIRON_RL_{prefix}_BURST"), default.burst),
            env_parse(
                &format!("CHIRON_RL_{prefix}_PER_MINUTE"),
                default.per_minute,
            ),
        )
    }
}

/// B10 限流配置。
///
/// 登录按 IP 计数（窗口式），提交与工具审批按用户计数（令牌桶）；
/// 三级检查顺序固定为 全局 → 租户 → 主体，避免先扣主体额度再被全局拒绝。
#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub login: RatePolicy,
    pub submit: RatePolicy,
    pub tool: RatePolicy,
    pub heavy: RatePolicy,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            login: RatePolicy::new(10, 10),
            submit: RatePolicy::new(10, 30),
            tool: RatePolicy::new(30, 120),
            heavy: RatePolicy::new(4, 20),
        }
    }
}

impl RateLimitConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            enabled: env_flag("CHIRON_RATE_LIMIT_ENABLED", defaults.enabled),
            login: RatePolicy::from_env("LOGIN", defaults.login),
            submit: RatePolicy::from_env("SUBMIT", defaults.submit),
            tool: RatePolicy::from_env("TOOL", defaults.tool),
            heavy: RatePolicy::from_env("HEAVY", defaults.heavy),
        }
    }
}

/// B5 Agent 运行时配置。
///
/// 模型/密钥全部走 env：`CHIRON_DEFAULT_MODEL`（缺省 `openai:gpt-5-mini` 之类的
/// `<provider>:<model>` 写法也可）、`CHIRON_DEFAULT_PROVIDER`，密钥沿用各 preset 的
/// `*_API_KEY`（如 `OPENAI_API_KEY`）。模型不存在时 run 会明确失败，不会静默换模型。
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub default_model: String,
    /// 为空表示按模型名前缀推断 provider。
    pub default_provider: String,
    /// 追加在默认系统提示之后的部署级提示（技能/内存装配点在 B8/B9）。
    pub system_prompt: String,
    pub max_steps: usize,
    pub max_input_tokens: u64,
    pub max_total_tokens: u64,
    pub max_tool_calls: usize,
    pub approval_timeout_secs: u64,
    pub web_fetch_allow_private: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_model: "gpt-5-mini".to_string(),
            default_provider: String::new(),
            system_prompt: String::new(),
            max_steps: 12,
            max_input_tokens: 60_000,
            max_total_tokens: 200_000,
            max_tool_calls: 40,
            approval_timeout_secs: 600,
            web_fetch_allow_private: false,
        }
    }
}

impl AgentConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            default_model: env_or("CHIRON_DEFAULT_MODEL", &defaults.default_model),
            default_provider: env_or("CHIRON_DEFAULT_PROVIDER", ""),
            system_prompt: env_or("CHIRON_SYSTEM_PROMPT", ""),
            max_steps: env_parse("CHIRON_MAX_STEPS", defaults.max_steps),
            max_input_tokens: env_parse("CHIRON_MAX_INPUT_TOKENS", defaults.max_input_tokens),
            max_total_tokens: env_parse("CHIRON_MAX_TOTAL_TOKENS", defaults.max_total_tokens),
            max_tool_calls: env_parse("CHIRON_MAX_TOOL_CALLS", defaults.max_tool_calls),
            approval_timeout_secs: env_parse(
                "CHIRON_APPROVAL_TIMEOUT_SECS",
                defaults.approval_timeout_secs,
            ),
            web_fetch_allow_private: parse_bool(env::var("CHIRON_WEB_FETCH_ALLOW_PRIVATE").ok()),
        }
    }
}

/// B8 内存注入配置。
///
/// v1 只注入结构化槽位（persona/preferences/facts）与会话摘要，不做向量检索；
/// 实际预算还会被 `context::memory_budget`（总输入预算的 1/4）再压一次。
#[derive(Clone, Debug)]
pub struct MemoryConfig {
    pub enabled: bool,
    /// 单轮内存注入的 token 上限（粗估：4 字符 ≈ 1 token）。
    pub inject_tokens: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            inject_tokens: 800,
        }
    }
}

impl MemoryConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            enabled: match env::var("CHIRON_MEMORY_ENABLED").ok() {
                Some(raw) => parse_bool(Some(raw)),
                None => defaults.enabled,
            },
            inject_tokens: env_parse("CHIRON_MEMORY_INJECT_TOKENS", defaults.inject_tokens),
        }
    }
}

fn env_or(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<T>().ok())
        .unwrap_or(default)
}

/// B9 集成配置（MCP / 插件 / 技能 / 市场）。
///
/// 默认值按"宁严勿松"：stdio 只能拉起白名单里的二进制，市场来源只认
/// `official` 与 `partner:*`。放宽必须显式改 env，且会写进审计。
#[derive(Clone, Debug)]
pub struct IntegrationConfig {
    /// 单次 MCP 请求（握手/发现/调用）的超时。
    pub mcp_request_timeout_ms: u64,
    /// 单台 server 允许暴露的工具上限。
    pub mcp_max_tools: usize,
    /// stdio 允许拉起的可执行文件（按 basename 匹配，忽略 `.exe`）。
    /// 空数组表示全部拒绝，不允许"空即全放"。
    pub mcp_commands: Vec<String>,
    /// 允许访问私网地址（仅本地开发；生产应保持 false 以挡 SSRF）。
    pub mcp_allow_private: bool,
    /// 市场来源白名单，支持 `xxx:*` 前缀写法。
    pub market_sources: Vec<String>,
    /// 安装市场条目时是否强制带签名。
    pub market_require_signature: bool,
    /// 插件/技能总开关；关掉后路由仍存在但一律 403。
    pub enabled: bool,
}

/// 默认不拉起任何宿主可执行文件：MCP stdio 会继承服务端环境变量，
/// 必须由运维显式配置可信命令（空 = 全部拒绝）。
pub const DEFAULT_MCP_COMMANDS: &[&str] = &[];
pub const DEFAULT_MARKET_SOURCES: &[&str] = &["official", "partner:*"];

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            mcp_request_timeout_ms: 20_000,
            mcp_max_tools: 128,
            mcp_commands: DEFAULT_MCP_COMMANDS.iter().map(|s| s.to_string()).collect(),
            mcp_allow_private: false,
            market_sources: DEFAULT_MARKET_SOURCES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            market_require_signature: false,
            enabled: true,
        }
    }
}

impl IntegrationConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let commands = env_list("CHIRON_MCP_COMMANDS");
        let sources = env_list("CHIRON_MARKET_SOURCES");
        Self {
            mcp_request_timeout_ms: env_parse(
                "CHIRON_MCP_TIMEOUT_MS",
                defaults.mcp_request_timeout_ms,
            ),
            mcp_max_tools: env_parse("CHIRON_MCP_MAX_TOOLS", defaults.mcp_max_tools),
            // 显式设置过就用设置值（含空列表 = 全禁），否则用保守默认。
            mcp_commands: if commands.is_empty() {
                defaults.mcp_commands
            } else {
                commands
            },
            mcp_allow_private: parse_bool(env::var("CHIRON_MCP_ALLOW_PRIVATE").ok()),
            market_sources: if sources.is_empty() {
                defaults.market_sources
            } else {
                sources
            },
            market_require_signature: parse_bool(env::var("CHIRON_MARKET_REQUIRE_SIGNATURE").ok()),
            enabled: !parse_bool(env::var("CHIRON_INTEGRATION_DISABLED").ok()),
        }
    }

    /// basename 是否在白名单内（`npx` 与 `/usr/bin/npx`、`npx.cmd` 都算同一项）。
    pub fn command_allowed(&self, command: &str) -> bool {
        let base = command_basename(command);
        self.mcp_commands
            .iter()
            .any(|allowed| command_basename(allowed) == base)
    }
}

fn command_basename(raw: &str) -> String {
    let trimmed = raw.trim();
    let name = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    name
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .map(|raw| {
            raw.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// R-2：Sentinel 地址列表解析（逗号分隔；裸 `host:port` 视为 `redis://host:port`）。
pub fn normalize_sentinel_urls(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(normalize_redis_url)
        .collect()
}

/// Sentinel 地址既接受 `redis://host:port`，也接受裸 `host:port`（运维常见写法；
/// redis 的 URL 解析只认完整 URL）。
fn normalize_redis_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("redis://{trimmed}")
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        // 只读当前目录的 .env：dotenv() 会向父目录查找，单仓开发时会误读
        // monorepo 根部的 .env（例如把 REDIS_URL 带进来）。服务端必须有自己的边界。
        if std::path::Path::new(".env").is_file() {
            dotenvy::from_path(".env").ok();
        }

        let jwt_secret = env::var("JWT_SECRET").unwrap_or_default();
        if jwt_secret.len() < 32 {
            anyhow::bail!("JWT_SECRET must be at least 32 characters");
        }
        let lower_secret = jwt_secret.to_ascii_lowercase();
        if lower_secret.contains("change-me")
            || lower_secret.contains("changeme")
            || lower_secret.contains("replace-me")
            || lower_secret == "secret"
        {
            anyhow::bail!("JWT_SECRET looks like a placeholder; generate a random secret");
        }

        let deploy_mode = env::var("CHIRON_DEPLOY_MODE")
            .unwrap_or_else(|_| "desktop".to_string())
            .to_lowercase();
        if deploy_mode != "desktop" && deploy_mode != "team" {
            anyhow::bail!("CHIRON_DEPLOY_MODE must be desktop or team");
        }
        let service_mode = parse_bool(env::var("CHIRON_SERVICE_MODE").ok());
        let redis_url = env::var("REDIS_URL").unwrap_or_default();
        let redis_sentinel_urls = env::var("CHIRON_REDIS_SENTINEL_URLS")
            .map(|raw| normalize_sentinel_urls(&raw))
            .unwrap_or_default();
        let redis_sentinel_master = env_or("CHIRON_REDIS_SENTINEL_MASTER", "mymaster");
        let redis_password = env::var("CHIRON_REDIS_PASSWORD")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();

        // 团队模式没有注册入口，且会话吊销依赖 Redis 纪元。缺 Redis 时拒绝启动，
        // 不让"禁用/改密后旧 token 仍有效"悄悄成立。
        // R-2：Sentinel 形态没有 REDIS_URL，只要配置了 Sentinel 列表即可。
        if (deploy_mode == "team" || service_mode)
            && redis_url.is_empty()
            && redis_sentinel_urls.is_empty()
        {
            anyhow::bail!(
                "team/service mode requires REDIS_URL or CHIRON_REDIS_SENTINEL_URLS (session revocation)"
            );
        }
        let metrics_token = env::var("CHIRON_METRICS_TOKEN").unwrap_or_default();
        if (deploy_mode == "team" || service_mode) && metrics_token.trim().is_empty() {
            anyhow::bail!("team/service mode requires CHIRON_METRICS_TOKEN for /metrics");
        }
        // 团队/服务形态默认 Secure；纯回环调试需显式 COOKIE_SECURE=0。
        let cookie_secure = match env::var("COOKIE_SECURE") {
            Ok(value) => parse_bool(Some(value)),
            Err(_) => deploy_mode == "team" || service_mode,
        };

        Ok(Self {
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8088".to_string()),
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgresql://chiron:chiron@127.0.0.1:5432/chiron".to_string()),
            redis_url,
            region: env::var("CHIRON_REGION").unwrap_or_default(),
            region_strict: env_flag("CHIRON_REGION_STRICT", false),
            read_only: env_flag("CHIRON_READ_ONLY", false),
            redis_sentinel_urls,
            redis_sentinel_master,
            redis_password,
            jwt_secret,
            jwt_hours: env::var("JWT_HOURS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(24),
            cookie_secure,
            deploy_mode,
            service_mode,
            control_database_url: env::var("CONTROL_DATABASE_URL").unwrap_or_default(),
            platform_host: env::var("PLATFORM_HOST").unwrap_or_default(),
            tenant_base_domain: env::var("TENANT_BASE_DOMAIN").unwrap_or_default(),
            platform_bootstrap_email: env::var("PLATFORM_BOOTSTRAP_EMAIL").unwrap_or_default(),
            platform_bootstrap_password: env::var("PLATFORM_BOOTSTRAP_PASSWORD")
                .unwrap_or_default(),
            provision_database_url: env::var("PROVISION_DATABASE_URL").unwrap_or_default(),
            secrets_key: env::var("SECRETS_KEY").unwrap_or_default(),
            tenant_pool_max: env_parse("CHIRON_TENANT_POOL_MAX", 10u32).max(1),
            tenant_pool_max_tenants: env_parse("CHIRON_TENANT_POOLS_MAX", 64usize).max(1),
            tenant_pool_idle_secs: env_parse("CHIRON_TENANT_POOL_IDLE_SECS", 300u64).max(30),
            usage_rollup_secs: env_parse("CHIRON_USAGE_ROLLUP_SECS", 3600u64),
            backup_dir: env::var("CHIRON_BACKUP_DIR").unwrap_or_default(),
            // 默认关闭：定时 pg_dump 会对每个租户库加读锁，节奏必须由运维显式决定。
            backup_secs: env_parse("CHIRON_BACKUP_SECS", 0u64),
            // 默认 0 = 不删除任何历史备份（删错不可恢复，必须显式配置）。
            backup_keep_days: env_parse("CHIRON_BACKUP_KEEP_DAYS", 0u64),
            alert_webhook_url: env::var("CHIRON_ALERT_WEBHOOK_URL").unwrap_or_default(),
            alert_cooldown_secs: env_parse("CHIRON_ALERT_COOLDOWN_SECS", 1800u64),
            metrics_token,
            billing_webhook_secret: env::var("CHIRON_BILLING_WEBHOOK_SECRET").unwrap_or_default(),
            alipay_app_id: env::var("ALIPAY_APP_ID").unwrap_or_default(),
            alipay_private_key: env::var("ALIPAY_PRIVATE_KEY").unwrap_or_default(),
            alipay_public_key: env::var("ALIPAY_PUBLIC_KEY").unwrap_or_default(),
            alipay_gateway: env::var("ALIPAY_GATEWAY")
                .unwrap_or_else(|_| "https://openapi.alipay.com/gateway.do".to_string()),
            alipay_notify_url: env::var("ALIPAY_NOTIFY_URL").unwrap_or_default(),
            alipay_return_url: env::var("ALIPAY_RETURN_URL").unwrap_or_default(),
            tls_probe_secs: env_parse("CHIRON_TLS_PROBE_SECS", 0u64),
            tls_probe_timeout_secs: env_parse("CHIRON_TLS_PROBE_TIMEOUT_SECS", 5u64),
            alert_active_runs: env_parse("CHIRON_ALERT_ACTIVE_RUNS", 0u64),
            alert_tenant_pools: env_parse("CHIRON_ALERT_TENANT_POOLS", 0u64),
            alert_egress_denied: env_parse("CHIRON_ALERT_EGRESS_DENIED", 0u64),
            egress_proxy_bind: env::var("CHIRON_EGRESS_PROXY_BIND").unwrap_or_default(),
            egress_allow_plaintext: env_flag("CHIRON_EGRESS_ALLOW_PLAINTEXT", false),
            egress_proxy_addr: env::var("CHIRON_EGRESS_PROXY_ADDR")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "host.docker.internal:3128".to_string()),
            media_root: env::var("CHIRON_MEDIA_ROOT").unwrap_or_default(),
            media_backend: env::var("CHIRON_MEDIA_BACKEND")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "local".to_string()),
            sandbox: SandboxConfig::from_env(),
            agent: AgentConfig::from_env(),
            subagent: SubagentConfig::from_env(),
            checkpoint: CheckpointConfig::from_env(),

            memory: MemoryConfig::from_env(),
            integration: IntegrationConfig::from_env(),
            rate_limit: RateLimitConfig::from_env(),
            multi: MultiInstanceConfig::from_env(),
        })
    }

    /// 沙箱隔离 tier 之外，媒体库根目录（宿主/容器内）。
    pub fn media_root_path(&self) -> std::path::PathBuf {
        if self.media_root.trim().is_empty() {
            self.sandbox.data_root.join("media")
        } else {
            std::path::PathBuf::from(self.media_root.trim())
        }
    }

    pub fn is_team(&self) -> bool {
        self.deploy_mode == "team" || self.service_mode
    }

    /// R-2：是否优先用 Sentinel 解析主节点（否则走单节点 `REDIS_URL`）。
    pub fn sentinel_configured(&self) -> bool {
        !self.redis_sentinel_urls.is_empty()
    }

    /// 建库用的管理连接：未单独配置时复用控制库 DSN（生产建议独立角色）。
    pub fn provisioning_database(&self) -> &str {
        if self.provision_database_url.trim().is_empty() {
            self.control_database_url.trim()
        } else {
            self.provision_database_url.trim()
        }
    }

    /// 开通租户需要管理连接 + 加密密钥；缺一不可。
    pub fn provisioning_ready(&self) -> bool {
        !self.provisioning_database().is_empty() && !self.secrets_key.trim().is_empty()
    }

    /// SaaS 形态：配置了控制库即按 Host 解析租户。
    pub fn is_saas(&self) -> bool {
        !self.control_database_url.trim().is_empty()
    }

    /// 测试用配置：不读 env、不需要可达的 PG/Redis。
    #[cfg(test)]
    pub fn for_test(node_id: &str) -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            database_url: "postgresql://chiron:chiron@127.0.0.1:5432/chiron".to_string(),
            redis_url: String::new(),
            region: String::new(),
            region_strict: false,
            read_only: false,
            redis_sentinel_urls: Vec::new(),
            redis_sentinel_master: "mymaster".to_string(),
            redis_password: String::new(),
            control_database_url: String::new(),
            platform_host: String::new(),
            tenant_base_domain: String::new(),
            platform_bootstrap_email: String::new(),
            platform_bootstrap_password: String::new(),
            provision_database_url: String::new(),
            secrets_key: String::new(),
            tenant_pool_max: 10,
            tenant_pool_max_tenants: 64,
            tenant_pool_idle_secs: 300,
            usage_rollup_secs: 0,
            backup_dir: String::new(),
            backup_secs: 0,
            backup_keep_days: 0,
            alert_webhook_url: String::new(),
            alert_cooldown_secs: 1800,
            metrics_token: String::new(),
            billing_webhook_secret: String::new(),
            alipay_app_id: String::new(),
            alipay_private_key: String::new(),
            alipay_public_key: String::new(),
            alipay_gateway: "https://openapi.alipay.com/gateway.do".to_string(),
            alipay_notify_url: String::new(),
            alipay_return_url: String::new(),
            tls_probe_secs: 0,
            tls_probe_timeout_secs: 5,
            alert_active_runs: 0,
            alert_tenant_pools: 0,
            alert_egress_denied: 0,
            egress_proxy_bind: String::new(),
            egress_allow_plaintext: false,
            egress_proxy_addr: "host.docker.internal:3128".to_string(),
            media_root: String::new(),
            media_backend: "local".to_string(),
            jwt_secret: "test-secret-test-secret-test-secret".to_string(),
            jwt_hours: 1,
            cookie_secure: false,
            deploy_mode: "desktop".to_string(),
            service_mode: false,
            sandbox: SandboxConfig::from_env(),
            agent: AgentConfig::default(),
            subagent: SubagentConfig::default(),
            checkpoint: CheckpointConfig::default(),
            memory: MemoryConfig::default(),
            integration: IntegrationConfig::default(),
            rate_limit: RateLimitConfig::default(),
            multi: MultiInstanceConfig {
                node_id: node_id.to_string(),
                // 测试不连 PG：只验证 Redis/内存租约语义。
                pg_lease_backstop: false,
                maintenance_enabled: false,
                ..MultiInstanceConfig::default()
            },
        }
    }
}

fn parse_bool(v: Option<String>) -> bool {
    let normalized = v.map(|s| s.trim().to_ascii_lowercase());
    matches!(normalized.as_deref(), Some("1" | "true" | "yes" | "on"))
}

/// 与 `parse_bool` 不同：缺省时保留默认值（限流默认开启，不能被"未设置"关掉）。
fn env_flag(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => parse_bool(Some(value)),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_urls_accept_bare_host_port() {
        assert_eq!(normalize_redis_url("127.0.0.1:26379"), "redis://127.0.0.1:26379");
        assert_eq!(
            normalize_redis_url(" redis://sentinel:26379 "),
            "redis://sentinel:26379"
        );
        assert_eq!(normalize_redis_url(""), "");
    }

    #[test]
    fn sentinel_is_optional() {
        let cfg = Config::for_test("node-a");
        assert!(!cfg.sentinel_configured());
        assert_eq!(cfg.redis_sentinel_master, "mymaster");
    }
}
