//! `GET /api/v1/admin/runtime`：运行态面板（只读，owner/admin）。
//!
//! 回答"谁在用、哪里出错"的第一层：活跃 run 数（按状态）、SSE 连接数、
//! 沙箱容器数、provider 健康。容器数复用 `SandboxManager::list_containers`，
//! Docker 不可用时降级为 `count: null` + `error`，不把面板打成 500。
//!
//! provider 健康是把三方事实合并起来的：平台 env 是否配了 key、租户 key 池
//! （控制库 `tenant_provider_keys`，只算 enabled）、租户路由最近探测
//! （`tenant_model_routes.last_ok_at/last_probe_at/last_error`，只算 enabled）。
//! 响应只出现计数/时间与脱敏后的错误文本，永远不含 key 值。控制库未配置或
//! 聚合失败时降级为 `tenant_keys: null` / `routes: null`，不把面板打成 500。
//!
//! SSE 连接数没有现成的全局计数：这里用一个只覆盖
//! `/api/v1/sessions/{id}/events` 的中间件，在响应体真正结束（流被 drop）
//! 时减一，因此长连接和客户端断开都能反映出来。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    body::Body,
    extract::{Extension, Request, State},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    admin::{ensure_admin, ok},
    auth::CurrentUser,
    error::AppError,
    providers::PRESETS,
    state::AppState,
};

/// 单用户同时保持的 SSE 连接上限（多标签页/重连风暴时保护进程与 Redis）。
const MAX_SSE_PER_USER: usize = 16;

/// 进程内运行态计数。
#[derive(Debug, Default)]
pub struct RuntimeCounters {
    active_sse: AtomicUsize,
    total_sse: AtomicU64,
    per_user: Mutex<HashMap<Uuid, usize>>,
    egress_allowed: AtomicU64,
    egress_denied: AtomicU64,
}

impl RuntimeCounters {
    pub fn sse_opened(&self) {
        self.active_sse.fetch_add(1, Ordering::SeqCst);
        self.total_sse.fetch_add(1, Ordering::Relaxed);
    }

    pub fn sse_closed(&self) {
        // 用 CAS 做饱和减一，避免 `fetch_update`（新工具链已重命名）的 MSRV 问题。
        let mut current = self.active_sse.load(Ordering::SeqCst);
        loop {
            if current == 0 {
                return;
            }
            match self.active_sse.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub fn sse_active(&self) -> usize {
        self.active_sse.load(Ordering::SeqCst)
    }

    pub fn sse_total(&self) -> u64 {
        self.total_sse.load(Ordering::Relaxed)
    }

    pub fn egress_allowed(&self) {
        self.egress_allowed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn egress_denied(&self) {
        self.egress_denied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn egress_allowed_total(&self) -> u64 {
        self.egress_allowed.load(Ordering::Relaxed)
    }

    pub fn egress_denied_total(&self) -> u64 {
        self.egress_denied.load(Ordering::Relaxed)
    }

    /// 用户级限流：超过上限返回 false，不增加全局计数。
    pub fn sse_try_open(&self, user: Uuid) -> bool {
        let mut users = self
            .per_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = users.entry(user).or_insert(0);
        if *count >= MAX_SSE_PER_USER {
            return false;
        }
        *count += 1;
        drop(users);
        self.sse_opened();
        true
    }

    pub fn sse_close_user(&self, user: Uuid) {
        if let Ok(mut users) = self.per_user.lock() {
            if let Some(count) = users.get_mut(&user) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    users.remove(&user);
                }
            }
        }
        self.sse_closed();
    }
}

/// 连接关闭（或响应构造失败）时归还计数。
struct SseGuard {
    counters: Arc<RuntimeCounters>,
    user: Option<Uuid>,
}

impl Drop for SseGuard {
    fn drop(&mut self) {
        match self.user {
            Some(user) => self.counters.sse_close_user(user),
            None => self.counters.sse_closed(),
        }
    }
}

/// 只对会话 SSE 端点计数：路径形如 `/api/v1/sessions/{id}/events`。
pub fn is_sse_path(path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    let segs: Vec<&str> = trimmed.split('/').collect();
    matches!(
        segs.as_slice(),
        ["", "api", "v1", "sessions", id, "events"] if !id.is_empty()
    )
}

/// SSE 连接计数中间件：非 SSE 请求零开销直通。
pub async fn track_sse(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if !is_sse_path(req.uri().path()) {
        return next.run(req).await;
    }
    let counters = state.runtime.clone();
    let user = req.extensions().get::<CurrentUser>().map(|actor| actor.id);
    match user {
        Some(user) => {
            if !counters.sse_try_open(user) {
                return crate::admin::limits::quota_response(30);
            }
        }
        None => counters.sse_opened(),
    }
    let resp = next.run(req).await;
    if !resp.status().is_success() {
        match user {
            Some(user) => counters.sse_close_user(user),
            None => counters.sse_closed(),
        }
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let guard = SseGuard { counters, user };
    let stream = async_stream::stream! {
        // guard 随生成器状态存活；流被 drop（客户端断开）时自动减一。
        let _guard = guard;
        let mut data = body.into_data_stream();
        while let Some(item) = data.next().await {
            yield item;
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

/// 活跃 run 按状态汇总；只统计迁移里定义的四类活跃状态。
pub fn summarize_active_runs(rows: &[(String, i64)]) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for (status, count) in rows {
        if matches!(
            status.as_str(),
            "queued" | "running" | "waiting_approval" | "waiting_user"
        ) && *count > 0
        {
            *out.entry(status.clone()).or_insert(0) += *count;
        }
    }
    out
}

/// 租户 key 池摘要（只含计数与时间，不含任何 key 内容）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TenantKeyHealth {
    /// 该 provider 在池里的全部 label 条数（含停用）。
    pub total: i64,
    /// 其中 enabled 的条数——只有这些会参与运行时选路。
    pub enabled: i64,
    /// `enabled > 0`：租户侧至少有一条可用 key。
    pub available: bool,
    /// enabled 条目里最近一次更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

/// 租户路由探测摘要（`tenant_model_routes` 的探测列）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteHealth {
    /// enabled 路由条数（停用路由不参与运行时选路）。
    pub enabled: i64,
    /// 最近一次成功探测时间。
    pub last_ok_at: Option<DateTime<Utc>>,
    /// 最近一次探测时间（无论成败）。
    pub last_probe_at: Option<DateTime<Utc>>,
    /// 最近一次失败原因；写入时已脱敏截断，读出时再兜一层。空串=从未失败/已恢复。
    pub last_error: String,
}

/// provider 健康：`providers` 数组里的单条。
///
/// 旧字段（`id`/`protocol`/`requires_key`/`api_key_configured`/`status`）语义不变，
/// 其余字段都是新增，便于控制台按"env 已配 / 租户池可用 / 最近探测成功"着色。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderHealth {
    pub id: String,
    /// 展示名，取自 preset；历史遗留 provider 用 id 兜底（与模型页一致）。
    pub label: String,
    pub protocol: &'static str,
    pub requires_key: bool,
    /// 平台环境变量是否配置了该 provider 的 key（旧字段，语义不变）。
    pub api_key_configured: bool,
    /// `missing_key` / `error` / `ok` / `unknown`。
    pub status: &'static str,
    /// 当前是否至少有一个可用凭据；无需 key 的 provider 恒为 true。
    pub available: bool,
    /// `platform_env` / `tenant_pool` / `platform_env+tenant_pool` / `none`。
    pub key_source: &'static str,
    /// 租户 key 池摘要；无 key 行或控制面不可用时为 null。
    pub tenant_keys: Option<TenantKeyHealth>,
    /// 租户路由探测摘要；无路由行或控制面不可用时为 null。
    pub routes: Option<RouteHealth>,
}

/// 控制库聚合出的 provider 事实（内部用，不直接序列化）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderFacts {
    pub keys_total: i64,
    pub keys_enabled: i64,
    pub keys_updated_at: Option<DateTime<Utc>>,
    pub routes_total: i64,
    pub routes_enabled: i64,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_error: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct KeyAggRow {
    provider: String,
    keys_total: i64,
    keys_enabled: i64,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RouteAggRow {
    provider: String,
    routes_total: i64,
    routes_enabled: i64,
    last_ok_at: Option<DateTime<Utc>>,
    last_probe_at: Option<DateTime<Utc>>,
    last_error: String,
}

/// key 池聚合：`count(*)` 含停用，`keys_enabled` 只算启用（运行时只认启用）。
const KEY_FACTS_SQL: &str = "\
SELECT provider,
       count(*)::bigint AS keys_total,
       (count(*) FILTER (WHERE enabled))::bigint AS keys_enabled,
       max(updated_at) FILTER (WHERE enabled) AS updated_at
  FROM tenant_provider_keys
 WHERE tenant_id = $1
 GROUP BY provider
 ORDER BY provider ASC";

/// 路由探测聚合：时间与最近错误都只看 enabled 路由，避免停用路由的旧错误误导。
const ROUTE_FACTS_SQL: &str = "\
SELECT provider,
       count(*)::bigint AS routes_total,
       (count(*) FILTER (WHERE enabled))::bigint AS routes_enabled,
       max(last_ok_at) FILTER (WHERE enabled) AS last_ok_at,
       max(last_probe_at) FILTER (WHERE enabled) AS last_probe_at,
       coalesce((array_agg(last_error ORDER BY last_probe_at DESC NULLS LAST)
                 FILTER (WHERE enabled))[1], '') AS last_error
  FROM tenant_model_routes
 WHERE tenant_id = $1
 GROUP BY provider
 ORDER BY provider ASC";

/// 读出时再兜一层脱敏：截断到 300 字符，密钥样式的 token 一律 `[redacted]`。
pub fn sanitize_route_error(raw: &str) -> String {
    const MAX_CHARS: usize = 300;
    let mut out = String::new();
    let mut redact_next = false;
    for token in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let bare = token.trim_matches(|c: char| {
            matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | ':' | '[' | ']')
        });
        if redact_next || looks_like_secret(bare) {
            out.push_str("[redacted]");
            redact_next = false;
            continue;
        }
        redact_next = bare.eq_ignore_ascii_case("bearer");
        out.push_str(token);
    }
    if out.chars().count() > MAX_CHARS {
        return out.chars().take(MAX_CHARS).collect();
    }
    out
}

/// 粗判一个 token 是否是密钥：已知前缀，或足够长的随机串。
fn looks_like_secret(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    const PREFIXES: [&str; 9] = [
        "sk-", "sk_", "gsk_", "xai-", "aiza", "akia", "ghp_", "hf_", "api_key=",
    ];
    if PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }
    let chars = token.chars().count();
    chars >= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '=' | '.'))
}

/// 状态优先级：没有可用凭据 > 最近一次探测失败 > 最近探测成功 > 未探测。
///
/// "最近一次"用 `last_probe_at` 与 `last_ok_at` 比较；探测成功会写
/// `last_ok_at = last_probe_at = now()` 并清空 `last_error`（见 `models::admin`）。
pub fn resolve_provider_status(
    requires_key: bool,
    platform_env_configured: bool,
    facts: Option<&ProviderFacts>,
) -> &'static str {
    let tenant_available = facts.map(|f| f.keys_enabled > 0).unwrap_or(false);
    if requires_key && !platform_env_configured && !tenant_available {
        return "missing_key";
    }
    let Some(facts) = facts else {
        return "unknown";
    };
    let failed_since_last_ok = !facts.last_error.is_empty()
        && facts
            .last_ok_at
            .map_or(true, |ok| facts.last_probe_at.map_or(true, |probe| probe > ok));
    if failed_since_last_ok {
        return "error";
    }
    match facts.last_ok_at {
        Some(ok) if facts.last_probe_at.map_or(true, |probe| ok >= probe) => "ok",
        _ => "unknown",
    }
}

pub fn provider_key_source(
    platform_env_configured: bool,
    tenant_keys_enabled: i64,
) -> &'static str {
    match (platform_env_configured, tenant_keys_enabled > 0) {
        (true, true) => "platform_env+tenant_pool",
        (true, false) => "platform_env",
        (false, true) => "tenant_pool",
        (false, false) => "none",
    }
}

fn health_entry(
    id: String,
    label: String,
    protocol: &'static str,
    requires_key: bool,
    env_configured: bool,
    facts: Option<&ProviderFacts>,
) -> ProviderHealth {
    let tenant_keys = facts.filter(|f| f.keys_total > 0).map(|f| TenantKeyHealth {
        total: f.keys_total,
        enabled: f.keys_enabled,
        available: f.keys_enabled > 0,
        updated_at: f.keys_updated_at,
    });
    let routes = facts.filter(|f| f.routes_total > 0).map(|f| RouteHealth {
        enabled: f.routes_enabled,
        last_ok_at: f.last_ok_at,
        last_probe_at: f.last_probe_at,
        last_error: sanitize_route_error(&f.last_error),
    });
    let tenant_enabled = facts.map(|f| f.keys_enabled).unwrap_or(0);
    ProviderHealth {
        available: !requires_key || env_configured || tenant_enabled > 0,
        key_source: provider_key_source(env_configured, tenant_enabled),
        status: resolve_provider_status(requires_key, env_configured, facts),
        id,
        label,
        protocol,
        requires_key,
        api_key_configured: env_configured,
        tenant_keys,
        routes,
    }
}

/// 合并平台 env + 租户 key 池 + 路由探测。
///
/// `facts = None` 表示控制面不可用/聚合失败，此时租户相关字段为 null，
/// `status` 退回只看 env。池里出现但 preset 不认识的 provider 追加在末尾。
pub fn provider_health_tenant_aware(
    is_configured: impl Fn(&str) -> bool,
    facts: Option<&HashMap<String, ProviderFacts>>,
) -> Vec<ProviderHealth> {
    let mut out: Vec<ProviderHealth> = PRESETS
        .iter()
        .map(|p| {
            health_entry(
                p.id.to_string(),
                p.label.to_string(),
                p.protocol.as_str(),
                p.requires_key,
                is_configured(p.api_key_env),
                facts.and_then(|map| map.get(p.id)),
            )
        })
        .collect();
    if let Some(map) = facts {
        let mut extras: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|id| crate::providers::find(id).is_none())
            .collect();
        extras.sort_unstable();
        for id in extras {
            out.push(health_entry(
                id.to_string(),
                id.to_string(),
                "unknown",
                true,
                false,
                map.get(id),
            ));
        }
    }
    out
}

/// 把两条聚合查询的结果并成 `provider -> facts`。
fn merge_provider_facts(
    keys: Vec<KeyAggRow>,
    routes: Vec<RouteAggRow>,
) -> HashMap<String, ProviderFacts> {
    let mut out: HashMap<String, ProviderFacts> = HashMap::new();
    for row in keys {
        let fact = out.entry(row.provider).or_default();
        fact.keys_total = row.keys_total;
        fact.keys_enabled = row.keys_enabled;
        fact.keys_updated_at = row.updated_at;
    }
    for row in routes {
        let fact = out.entry(row.provider).or_default();
        fact.routes_total = row.routes_total;
        fact.routes_enabled = row.routes_enabled;
        fact.last_ok_at = row.last_ok_at;
        fact.last_probe_at = row.last_probe_at;
        fact.last_error = sanitize_route_error(&row.last_error);
    }
    out
}

async fn fetch_provider_facts(
    control: &PgPool,
    tenant_id: Uuid,
) -> Result<HashMap<String, ProviderFacts>, AppError> {
    let keys = sqlx::query_as::<_, KeyAggRow>(KEY_FACTS_SQL)
        .bind(tenant_id)
        .fetch_all(control)
        .await?;
    let routes = sqlx::query_as::<_, RouteAggRow>(ROUTE_FACTS_SQL)
        .bind(tenant_id)
        .fetch_all(control)
        .await?;
    Ok(merge_provider_facts(keys, routes))
}

fn env_configured(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

pub async fn get_runtime(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> Result<Response, AppError> {
    ensure_admin(&actor)?;

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT r.status, COUNT(*)::bigint \
         FROM runs r JOIN sessions s ON s.id = r.session_id \
         WHERE s.tenant_id = $1 \
           AND r.status IN ('queued', 'running', 'waiting_approval', 'waiting_user') \
         GROUP BY r.status",
    )
    .bind(actor.tenant_id)
    .fetch_all(&state.db())
    .await?;
    let active_runs = summarize_active_runs(&rows);
    let active_total: i64 = active_runs.values().sum();

    let containers = match state.sandbox.list_containers().await {
        Ok(items) => {
            let (running, stopped) =
                crate::admin::containers::count_by_state(items.iter().map(|c| c.status.clone()));
            json!({
                "count": items.len(),
                "running": running,
                "stopped": stopped,
                "error": null,
            })
        }
        Err(e) => json!({
            "count": null,
            "running": null,
            "stopped": null,
            "error": e.message,
        }),
    };

    // 租户级 provider 事实：控制库 + 控制面租户 uuid 都在才有；任何一边缺失
    // 或聚合失败都降级为 null（面板不 500，只是少两列事实）。
    let provider_facts = match (state.control.as_ref(), actor.tenant) {
        (Some(control), Some(tenant_id)) => match fetch_provider_facts(&control.db, tenant_id).await
        {
            Ok(facts) => Some(facts),
            Err(err) => {
                tracing::warn!(
                    tenant = %tenant_id, error = %err,
                    "runtime provider facts unavailable"
                );
                None
            }
        },
        _ => None,
    };
    let providers = provider_health_tenant_aware(env_configured, provider_facts.as_ref());

    Ok(ok(json!({
        "tenant_id": actor.tenant_id,
        "active_runs": active_runs,
        "active_runs_total": active_total,
        "sse": {
            "active": state.runtime.sse_active(),
            "total_opened": state.runtime.sse_total(),
        },
        "containers": containers,
        "providers": providers,
        // 租户事实是否可用：false 时 providers[].tenant_keys/routes 恒为 null。
        "providers_tenant_scoped": provider_facts.is_some(),
        "generated_at": Utc::now(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[test]
    fn sse_path_matching_is_exact() {
        assert!(is_sse_path(
            "/api/v1/sessions/018f2f8c-0000-7000-8000-000000000001/events"
        ));
        assert!(is_sse_path(
            "/api/v1/sessions/018f2f8c-0000-7000-8000-000000000001/events/"
        ));
        assert!(!is_sse_path("/api/v1/sessions//events"));
        assert!(!is_sse_path("/api/v1/sessions/x/events/extra"));
        assert!(!is_sse_path("/api/v1/sessions/x/messages"));
        assert!(!is_sse_path("/api/v1/admin/runtime"));
        assert!(!is_sse_path("/healthz"));
    }

    #[test]
    fn counters_open_and_close_are_balanced() {
        let counters = RuntimeCounters::default();
        counters.sse_opened();
        counters.sse_opened();
        assert_eq!(counters.sse_active(), 2);
        assert_eq!(counters.sse_total(), 2);
        counters.sse_closed();
        assert_eq!(counters.sse_active(), 1);
        // 多减一次也不会下溢成巨大值。
        counters.sse_closed();
        counters.sse_closed();
        assert_eq!(counters.sse_active(), 0);
        assert_eq!(counters.sse_total(), 2);
    }

    #[test]
    fn sse_guard_releases_on_drop() {
        let counters = StdArc::new(RuntimeCounters::default());
        counters.sse_opened();
        assert_eq!(counters.sse_active(), 1);
        {
            let _guard = SseGuard {
                counters: counters.clone(),
                user: None,
            };
        }
        assert_eq!(counters.sse_active(), 0);
    }

    #[test]
    fn sse_per_user_cap_is_enforced() {
        let counters = RuntimeCounters::default();
        let user = Uuid::from_u128(42);
        for _ in 0..MAX_SSE_PER_USER {
            assert!(counters.sse_try_open(user));
        }
        assert!(!counters.sse_try_open(user), "cap exceeded");
        counters.sse_close_user(user);
        assert!(counters.sse_try_open(user), "slot released");
        assert_eq!(counters.sse_active(), MAX_SSE_PER_USER);
        for _ in 0..MAX_SSE_PER_USER {
            counters.sse_close_user(user);
        }
        assert_eq!(counters.sse_active(), 0);
    }

    #[test]
    fn active_run_summary_ignores_terminal_and_empty() {
        let rows = vec![
            ("running".to_string(), 3),
            ("queued".to_string(), 1),
            ("waiting_approval".to_string(), 2),
            ("completed".to_string(), 9),
            ("failed".to_string(), 4),
            ("running".to_string(), 0),
        ];
        let summary = summarize_active_runs(&rows);
        assert_eq!(summary.get("running"), Some(&3));
        assert_eq!(summary.get("queued"), Some(&1));
        assert_eq!(summary.get("waiting_approval"), Some(&2));
        assert!(!summary.contains_key("completed"));
        assert!(!summary.contains_key("failed"));
        assert_eq!(summary.values().sum::<i64>(), 6);
    }

    #[test]
    fn fact_queries_are_tenant_scoped_and_count_only_enabled() {
        for sql in [KEY_FACTS_SQL, ROUTE_FACTS_SQL] {
            let lower = sql.to_ascii_lowercase();
            assert!(lower.contains("where tenant_id = $1"), "必须按租户过滤：{sql}");
            assert!(lower.contains("group by provider"), "{sql}");
        }
        // 停用的 key / 路由不能计入"可用"事实。
        assert!(KEY_FACTS_SQL.contains("FILTER (WHERE enabled)"));
        assert!(ROUTE_FACTS_SQL.contains("FILTER (WHERE enabled)"));
    }

    #[test]
    fn provider_health_maps_missing_keys_to_status() {
        let health = provider_health_tenant_aware(|name| name == "OPENAI_API_KEY", None);
        let openai = health.iter().find(|p| p.id == "openai").expect("openai");
        assert!(openai.api_key_configured);
        assert_eq!(openai.status, "unknown");
        let anthropic = health
            .iter()
            .find(|p| p.id == "anthropic")
            .expect("anthropic");
        assert!(anthropic.requires_key);
        assert!(!anthropic.api_key_configured);
        assert_eq!(anthropic.status, "missing_key");
        assert_eq!(health.len(), PRESETS.len());
        // 不泄露密钥内容，只暴露布尔。
        assert!(!format!("{health:?}").contains("sk-"));
    }

    #[test]
    fn tenant_pool_and_route_health_merge_into_provider_entry() {
        let now = Utc::now();
        let mut facts = HashMap::new();
        facts.insert(
            "openai".to_string(),
            ProviderFacts {
                keys_total: 3,
                keys_enabled: 2,
                keys_updated_at: Some(now),
                routes_total: 4,
                routes_enabled: 3,
                last_ok_at: Some(now),
                last_probe_at: Some(now),
                last_error: String::new(),
            },
        );
        let health = provider_health_tenant_aware(|_| false, Some(&facts));
        let openai = health.iter().find(|p| p.id == "openai").expect("openai");
        assert!(!openai.api_key_configured, "平台 env 未配置");
        assert_eq!(openai.key_source, "tenant_pool");
        assert!(openai.available);
        assert_eq!(openai.status, "ok");
        let keys = openai.tenant_keys.as_ref().expect("tenant keys");
        assert_eq!((keys.total, keys.enabled), (3, 2));
        assert!(keys.available);
        assert_eq!(keys.updated_at, Some(now));
        let routes = openai.routes.as_ref().expect("routes");
        assert_eq!(routes.enabled, 3);
        assert!(routes.last_error.is_empty());

        // 没有租户事实的 provider 仍按 env 判断，租户字段为 null。
        let anthropic = health
            .iter()
            .find(|p| p.id == "anthropic")
            .expect("anthropic");
        assert_eq!(anthropic.status, "missing_key");
        assert!(anthropic.tenant_keys.is_none());
        assert!(anthropic.routes.is_none());
        assert_eq!(anthropic.key_source, "none");
        assert_eq!(health.len(), PRESETS.len());
    }

    #[test]
    fn key_source_reports_platform_env_and_pool() {
        assert_eq!(provider_key_source(false, 0), "none");
        assert_eq!(provider_key_source(true, 0), "platform_env");
        assert_eq!(provider_key_source(false, 2), "tenant_pool");
        assert_eq!(
            provider_key_source(true, 2),
            "platform_env+tenant_pool",
            "平台 env 与租户池同时可用时两者都要反映"
        );
    }

    #[test]
    fn provider_status_orders_missing_key_then_failure_then_success() {
        let now = Utc::now();
        // 最近一次探测失败（错误时间晚于上次成功）→ error。
        let failed = ProviderFacts {
            keys_total: 1,
            keys_enabled: 1,
            routes_total: 1,
            routes_enabled: 1,
            last_ok_at: Some(now - chrono::Duration::minutes(10)),
            last_probe_at: Some(now),
            last_error: "upstream 401".into(),
            ..Default::default()
        };
        assert_eq!(resolve_provider_status(true, false, Some(&failed)), "error");

        // 从未成功过但探测报错 → error。
        let never_ok = ProviderFacts {
            keys_enabled: 1,
            last_probe_at: Some(now),
            last_error: "connect timeout".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_provider_status(true, false, Some(&never_ok)),
            "error"
        );

        // 有 key、未探测 → unknown。
        let untested = ProviderFacts {
            keys_enabled: 1,
            ..Default::default()
        };
        assert_eq!(
            resolve_provider_status(true, false, Some(&untested)),
            "unknown"
        );

        // 没有可用凭据时优先报 missing_key，历史错误不覆盖掉"缺 key"。
        let no_key = ProviderFacts {
            last_probe_at: Some(now),
            last_error: "upstream 401".into(),
            keys_enabled: 0,
            ..Default::default()
        };
        assert_eq!(
            resolve_provider_status(true, false, Some(&no_key)),
            "missing_key"
        );

        // 成功探测（last_ok == last_probe）且无错误 → ok。
        let ok = ProviderFacts {
            keys_enabled: 1,
            last_ok_at: Some(now),
            last_probe_at: Some(now),
            ..Default::default()
        };
        assert_eq!(resolve_provider_status(true, false, Some(&ok)), "ok");

        // env 已配置就不再是 missing_key；无需 key 的 provider 同样不会。
        assert_eq!(resolve_provider_status(true, true, None), "unknown");
        assert_eq!(resolve_provider_status(false, false, None), "unknown");
    }

    #[test]
    fn route_errors_are_sanitized_and_truncated() {
        let raw = format!(
            "request failed key sk-live-abcdef123456 \
             Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345 {}",
            "y".repeat(400)
        );
        let out = sanitize_route_error(&raw);
        assert!(!out.contains("sk-live-abcdef123456"), "{out}");
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz012345"), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
        assert!(
            out.chars().count() <= 300,
            "截断到 300 字符，实际 {}",
            out.chars().count()
        );
        // 普通错误保持可读，空串仍为空。
        assert_eq!(
            sanitize_route_error("upstream 401 unauthorized"),
            "upstream 401 unauthorized"
        );
        assert!(sanitize_route_error("").is_empty());
    }

    #[test]
    fn merge_provider_facts_combines_key_and_route_rows() {
        let now = Utc::now();
        let keys = vec![KeyAggRow {
            provider: "openai".into(),
            keys_total: 2,
            keys_enabled: 1,
            updated_at: Some(now),
        }];
        let routes = vec![RouteAggRow {
            provider: "openai".into(),
            routes_total: 2,
            routes_enabled: 1,
            last_ok_at: None,
            last_probe_at: Some(now),
            last_error: "bad key sk-leaked-value-123".into(),
        }];
        let merged = merge_provider_facts(keys, routes);
        let openai = merged.get("openai").expect("openai");
        assert_eq!(openai.keys_total, 2);
        assert_eq!(openai.keys_enabled, 1);
        assert_eq!(openai.routes_total, 2);
        assert_eq!(openai.routes_enabled, 1);
        // 聚合时就把错误脱敏，后面不会把原样错误带进响应。
        assert!(!openai.last_error.contains("sk-leaked-value-123"));
        assert!(openai.last_error.contains("[redacted]"));
    }

    #[test]
    fn unknown_pool_providers_are_listed_after_presets() {
        let mut facts = HashMap::new();
        facts.insert(
            "acme-custom".to_string(),
            ProviderFacts {
                keys_total: 1,
                keys_enabled: 1,
                ..Default::default()
            },
        );
        let health = provider_health_tenant_aware(|_| false, Some(&facts));
        assert_eq!(health.len(), PRESETS.len() + 1);
        let last = health.last().expect("custom provider");
        assert_eq!(last.id, "acme-custom");
        assert_eq!(last.protocol, "unknown");
        assert!(last.available, "有启用 key 就应显示可用");
        assert!(last.tenant_keys.is_some());
        assert!(last.routes.is_none());
        assert_eq!(last.status, "unknown");
    }

    #[test]
    fn serialized_provider_entry_keeps_legacy_fields_and_adds_tenant_facts() {
        let mut facts = HashMap::new();
        facts.insert(
            "openai".to_string(),
            ProviderFacts {
                keys_total: 1,
                keys_enabled: 1,
                routes_total: 1,
                routes_enabled: 1,
                last_error: String::new(),
                ..Default::default()
            },
        );
        let health = provider_health_tenant_aware(|name| name == "OPENAI_API_KEY", Some(&facts));
        let value = serde_json::to_value(&health).expect("json");
        let openai = value
            .as_array()
            .expect("array")
            .iter()
            .find(|v| v["id"] == "openai")
            .expect("openai");
        // 旧字段仍在（向后兼容）。
        assert_eq!(openai["api_key_configured"], true);
        assert_eq!(openai["requires_key"], true);
        assert!(openai["protocol"].is_string());
        assert!(openai["status"].is_string());
        // 新增字段。
        assert_eq!(openai["tenant_keys"]["enabled"], 1);
        assert_eq!(openai["routes"]["enabled"], 1);
        assert_eq!(openai["routes"]["last_error"], "");
        assert_eq!(openai["key_source"], "platform_env+tenant_pool");
        assert_eq!(openai["available"], true);
        // 整个响应里没有任何 key 值。
        assert!(!value.to_string().contains("sk-"));
    }
}
