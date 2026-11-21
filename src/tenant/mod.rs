//! S2：Host → 租户解析、租户连接池与请求级数据库作用域。
//!
//! 只有配置了控制库（SaaS 形态）才启用；单库开发/测试形态下中间件直接放行，
//! `AppState::db()` 回落到默认库。
//!
//! 连接池按租户缓存（LRU 上限 + 空闲回收）；DSN 从控制库密文现解，明文只在内存。
//! 状态不缓存：暂停/删除必须立即生效，代价是每请求一次按 Host 索引的查询。

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::header::HOST,
    middleware::Next,
    response::Response,
};
use rand::Rng;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::task_local;
use uuid::Uuid;

use crate::{config::Config, control::crypto, error::AppError};

pub mod quota;

/// BYOK 池里的一条 key：明文只存在于进程内，`Debug` 不输出 key 本身。
#[derive(Clone, PartialEq, Eq)]
pub struct PooledKey {
    pub key: String,
    pub weight: u32,
}

impl std::fmt::Debug for PooledKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledKey")
            .field("weight", &self.weight)
            .finish_non_exhaustive()
    }
}

/// 运行时模型路由（控制库 `tenant_model_routes` 的 enabled 子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub model: String,
    pub provider: String,
    pub upstream_model: String,
    pub priority: i32,
    pub weight: i32,
    pub enabled: bool,
}

/// `provider -> 该 provider 的可用 key 池`。
pub type KeyPools = Arc<HashMap<String, Vec<PooledKey>>>;
pub type RouteList = Arc<Vec<Route>>;

task_local! {
    static CURRENT_DB: PgPool;
    static CURRENT_KEYS: KeyPools;
    static CURRENT_ROUTES: RouteList;
    static CURRENT_TIER: String;
    static CURRENT_EGRESS: String;
    static CURRENT_TENANT: Uuid;
}

/// 绑定租户库 + BYOK 池 + 模型路由 + 沙箱 tier（run 脱手执行时使用）。
pub async fn scope_with<F: Future>(
    db: PgPool,
    keys: KeyPools,
    routes: RouteList,
    tier: String,
    egress: String,
    tenant_id: Uuid,
    fut: F,
) -> F::Output {
    CURRENT_DB
        .scope(
            db,
            CURRENT_KEYS.scope(
                keys,
                CURRENT_ROUTES.scope(
                    routes,
                    CURRENT_TIER.scope(
                        tier,
                        CURRENT_EGRESS.scope(egress, CURRENT_TENANT.scope(tenant_id, fut)),
                    ),
                ),
            ),
        )
        .await
}

/// 当前任务绑定的租户库；未绑定返回 None。
pub fn current() -> Option<PgPool> {
    CURRENT_DB.try_with(PgPool::clone).ok()
}

pub fn current_keys() -> KeyPools {
    CURRENT_KEYS
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::new(HashMap::new()))
}

/// 当前任务绑定的模型路由；未绑定返回空列表。
pub fn current_routes() -> RouteList {
    CURRENT_ROUTES
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::new(Vec::new()))
}

/// 权重轮盘：`roll` 落在 `[0, 权重和)` 内；空池或权重全 0 返回 None。
pub fn weighted_index(weights: &[u32], roll: u64) -> Option<usize> {
    let mut acc: u64 = 0;
    for (idx, weight) in weights.iter().enumerate() {
        if *weight == 0 {
            continue;
        }
        acc += *weight as u64;
        if roll < acc {
            return Some(idx);
        }
    }
    None
}

/// 按 weight 加权随机选一条 key；`weight <= 0` 的条目不参与。
pub fn pick_key(pool: &[PooledKey]) -> Option<&PooledKey> {
    let weights: Vec<u32> = pool.iter().map(|key| key.weight).collect();
    let total: u64 = weights.iter().map(|w| *w as u64).sum();
    if total == 0 {
        return None;
    }
    let roll = rand::thread_rng().gen_range(0..total);
    weighted_index(&weights, roll).map(|idx| &pool[idx])
}

/// 模型名归一：`openai:gpt-5` 与 `gpt-5` 视为同一个对外模型。
fn canonical_model(model: &str) -> &str {
    let trimmed = model.trim();
    match trimmed.split_once(':') {
        Some((_, rest)) if !rest.trim().is_empty() => rest.trim(),
        _ => trimmed,
    }
}

fn route_matches(route_model: &str, wanted: &str) -> bool {
    let a = route_model.trim().to_ascii_lowercase();
    let b = wanted.trim().to_ascii_lowercase();
    a == b || canonical_model(&a).eq_ignore_ascii_case(canonical_model(&b))
}

/// 命中的全部启用路由：priority 小者优先，同级按 weight 大者优先，
/// 再按 provider 字典序保证确定性（失败转移按这个顺序逐个尝试）。
pub fn matching_routes<'a>(routes: &'a [Route], model: &str) -> Vec<&'a Route> {
    if model.trim().is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<&Route> = routes
        .iter()
        .filter(|route| route.enabled && route_matches(&route.model, model))
        .collect();
    hits.sort_by(|a, b| {
        (a.priority, std::cmp::Reverse(a.weight), a.provider.as_str()).cmp(&(
            b.priority,
            std::cmp::Reverse(b.weight),
            b.provider.as_str(),
        ))
    });
    hits
}

/// 当前租户套餐的沙箱隔离档位；未绑定返回 None（沿用全局 sandbox.runtime）。
pub fn current_tier() -> Option<String> {
    CURRENT_TIER.try_with(String::clone).ok()
}

/// 当前租户的 egress 模式（bridge/none）；未绑定沿用全局 sandbox.network。
pub fn current_egress() -> Option<String> {
    CURRENT_EGRESS.try_with(String::clone).ok()
}

/// 控制库租户 uuid；沙箱标签/工作区/容器名用它区分租户。
pub fn current_tenant() -> Option<Uuid> {
    CURRENT_TENANT.try_with(|id| *id).ok()
}

/// 当前二进制内嵌的租户迁移最高版本；控制库 `tenants.schema_version` 与之对齐。
pub fn target_schema_version() -> i64 {
    sqlx::migrate!("./migrations")
        .migrations
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct TenantContext {
    pub id: Uuid,
    /// 套餐上限；未配置控制库（单库形态）时为空表示不限额。
    pub plan: Option<PlanLimits>,
}

/// 套餐上限（来自控制库 `plans`；实际是计划值，用量在租户库计量）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanLimits {
    pub plan_id: String,
    pub isolation_tier: String,
    pub egress_mode: String,
    pub max_users: i32,
    pub max_concurrent_runs: i32,
    pub monthly_tokens: i64,
    /// 0 = 不限额。
    pub monthly_cost_cents: i64,
    pub max_storage_gb: i32,
    pub max_sandboxes: i32,
    /// 为空 = 不限模型。
    pub model_allowlist: Vec<String>,
    /// egress 代理白名单（`*.suffix` 通配）；空 = 全部拒绝。
    pub egress_allowlist: Vec<String>,
    /// `block`（默认）到达月额度即拒绝；`allow` 允许超额继续并在账单体现。
    pub overage_policy: String,
}

#[derive(Debug, Clone)]
pub struct TenantInfo {
    pub id: Uuid,
    pub slug: String,
    /// 控制库 `tenants.region`；空/`default` 表示未绑定具体区域。
    pub region: String,
    dsn_enc: String,
    pub plan: PlanLimits,
}

struct PoolEntry {
    pool: PgPool,
    /// 建池时使用的密文 DSN；PATCH dsn/切区后必须重建连接池。
    dsn_enc: String,
    last_used: Instant,
}

#[derive(sqlx::FromRow)]
struct ResolvedRow {
    id: Uuid,
    slug: String,
    region: String,
    status: String,
    dsn_enc: Option<String>,
    plan_id: String,
    isolation_tier: String,
    egress_mode: String,
    max_users: i32,
    max_concurrent_runs: i32,
    monthly_tokens: i64,
    monthly_cost_cents: i64,
    max_storage_gb: i32,
    max_sandboxes: i32,
    model_allowlist: Vec<String>,
    egress_allowlist: Vec<String>,
    overage_policy: String,
}

/// BYOK 池与模型路由的缓存窗口；平台/租户侧改动会立即失效。
const CACHE_TTL: Duration = Duration::from_secs(30);

pub struct TenantRegistry {
    control: PgPool,
    secrets_key: String,
    region: String,
    region_strict: bool,
    pool_max: u32,
    max_pools: usize,
    idle: Duration,
    pools: Mutex<HashMap<Uuid, PoolEntry>>,
    keys: Mutex<HashMap<Uuid, (Instant, KeyPools)>>,
    routes: Mutex<HashMap<Uuid, (Instant, RouteList)>>,
}

pub(crate) fn key_aad(tenant_id: Uuid, provider: &str) -> String {
    format!("{tenant_id}:{provider}")
}

impl TenantRegistry {
    pub fn new(control: PgPool, cfg: &Config) -> Self {
        Self {
            control,
            secrets_key: cfg.secrets_key.trim().to_string(),
            region: cfg.region.trim().to_string(),
            region_strict: cfg.region_strict,
            pool_max: cfg.tenant_pool_max.max(1),
            max_pools: cfg.tenant_pool_max_tenants.max(1),
            idle: Duration::from_secs(cfg.tenant_pool_idle_secs.max(30)),
            pools: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
        }
    }

    /// 平台/租户侧改了 key 池后立即失效缓存。
    pub fn invalidate_keys(&self, tenant_id: Uuid) {
        if let Ok(mut keys) = self.keys.lock() {
            keys.remove(&tenant_id);
        }
    }

    /// 租户侧改了模型路由后立即失效缓存。
    pub fn invalidate_routes(&self, tenant_id: Uuid) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.remove(&tenant_id);
        }
    }

    /// 租户 BYOK 池（30s 缓存，只含 enabled）：`provider -> [(key, weight)]`。
    /// 单条解密失败只告警并跳过，不影响同 provider 的其他 key。
    pub async fn keys(&self, tenant_id: Uuid) -> Result<KeyPools, AppError> {
        if let Ok(cache) = self.keys.lock() {
            if let Some((at, pools)) = cache.get(&tenant_id) {
                if at.elapsed() < CACHE_TTL {
                    return Ok(pools.clone());
                }
            }
        }
        let rows: Vec<(String, String, i32)> = sqlx::query_as(
            "SELECT provider, key_enc, weight FROM tenant_provider_keys
              WHERE tenant_id = $1 AND enabled
              ORDER BY provider ASC, label ASC",
        )
        .bind(tenant_id)
        .fetch_all(&self.control)
        .await?;
        let mut pools: HashMap<String, Vec<PooledKey>> = HashMap::new();
        for (provider, enc, weight) in rows {
            match crypto::open(
                &self.secrets_key,
                key_aad(tenant_id, &provider).as_bytes(),
                &enc,
            ) {
                Ok(key) => pools.entry(provider).or_default().push(PooledKey {
                    key,
                    weight: weight.max(1) as u32,
                }),
                Err(msg) => tracing::warn!(
                    tenant = %tenant_id, provider = %provider, error = %msg,
                    "tenant provider key decrypt failed"
                ),
            }
        }
        let arc = Arc::new(pools);
        if let Ok(mut cache) = self.keys.lock() {
            cache.insert(tenant_id, (Instant::now(), arc.clone()));
        }
        Ok(arc)
    }

    /// 租户模型路由（30s 缓存，只含 enabled，按 priority 升序）。
    pub async fn routes(&self, tenant_id: Uuid) -> Result<RouteList, AppError> {
        if let Ok(cache) = self.routes.lock() {
            if let Some((at, routes)) = cache.get(&tenant_id) {
                if at.elapsed() < CACHE_TTL {
                    return Ok(routes.clone());
                }
            }
        }
        let rows: Vec<(String, String, String, i32, i32, bool)> = sqlx::query_as(
            "SELECT model, provider, upstream_model, priority, weight, enabled
               FROM tenant_model_routes
              WHERE tenant_id = $1 AND enabled
              ORDER BY priority ASC, model ASC, provider ASC",
        )
        .bind(tenant_id)
        .fetch_all(&self.control)
        .await?;
        let list: Vec<Route> = rows
            .into_iter()
            .map(
                |(model, provider, upstream_model, priority, weight, enabled)| Route {
                    model,
                    provider,
                    upstream_model,
                    priority,
                    weight,
                    enabled,
                },
            )
            .collect();
        let arc = Arc::new(list);
        if let Ok(mut cache) = self.routes.lock() {
            cache.insert(tenant_id, (Instant::now(), arc.clone()));
        }
        Ok(arc)
    }

    pub fn cached_pools(&self) -> usize {
        self.pools.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Host → 租户。未知 Host 404；暂停/未开通/失败分别 503，且不回落默认租户。
    pub async fn resolve(&self, host_header: &str) -> Result<TenantInfo, AppError> {
        let host = normalize_host(host_header);
        if host.is_empty() {
            return Err(AppError::not_found("unknown host"));
        }
        let row: Option<ResolvedRow> = sqlx::query_as(
            "SELECT t.id, t.slug, t.region, t.status, t.dsn_enc,
                    p.id AS plan_id, p.isolation_tier, p.egress_mode, p.max_users, p.max_concurrent_runs,
                    p.monthly_tokens, p.monthly_cost_cents, p.max_storage_gb, p.max_sandboxes,
                    p.model_allowlist, p.egress_allowlist, p.overage_policy
               FROM tenant_domains d
               JOIN tenants t ON t.id = d.tenant_id
               JOIN plans p ON p.id = t.plan_id
              WHERE lower(d.host) = $1 AND t.status <> 'deleted'",
        )
        .bind(&host)
        .fetch_optional(&self.control)
        .await?;
        let Some(row) = row else {
            return Err(AppError::not_found("unknown host"));
        };
        match row.status.as_str() {
            "active" => {
                let dsn_enc = row.dsn_enc.ok_or_else(|| {
                    AppError::new(
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "tenant_not_ready",
                        "tenant database is not provisioned",
                    )
                })?;
                Ok(TenantInfo {
                    id: row.id,
                    slug: row.slug,
                    region: row.region,
                    dsn_enc,
                    plan: PlanLimits {
                        plan_id: row.plan_id,
                        isolation_tier: row.isolation_tier,
                        egress_mode: row.egress_mode,
                        max_users: row.max_users,
                        max_concurrent_runs: row.max_concurrent_runs,
                        monthly_tokens: row.monthly_tokens,
                        monthly_cost_cents: row.monthly_cost_cents,
                        max_storage_gb: row.max_storage_gb,
                        max_sandboxes: row.max_sandboxes,
                        model_allowlist: row.model_allowlist,
                        egress_allowlist: row.egress_allowlist,
                        overage_policy: row.overage_policy,
                    },
                })
            }
            "suspended" => Err(AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "tenant_suspended",
                "tenant is suspended",
            )),
            _ => Err(AppError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "tenant_not_ready",
                "tenant is not active",
            )),
        }
    }

    /// 所有已开通的活跃租户（后台任务按租户库遍历，不能落到默认库）。
    pub async fn active_tenants(&self) -> Result<Vec<TenantInfo>, AppError> {
        let rows: Vec<ResolvedRow> = sqlx::query_as(
            "SELECT t.id, t.slug, t.region, t.status, t.dsn_enc,
                    p.id AS plan_id, p.isolation_tier, p.egress_mode, p.max_users, p.max_concurrent_runs,
                    p.monthly_tokens, p.monthly_cost_cents, p.max_storage_gb, p.max_sandboxes,
                    p.model_allowlist, p.egress_allowlist, p.overage_policy
               FROM tenants t
               JOIN plans p ON p.id = t.plan_id
              WHERE t.status = 'active' AND t.dsn_enc IS NOT NULL
              ORDER BY t.slug ASC",
        )
        .fetch_all(&self.control)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if !region_allows_strict(self.region_strict, &self.region, &row.region) {
                continue;
            }
            let Some(dsn_enc) = row.dsn_enc else {
                continue;
            };
            out.push(TenantInfo {
                id: row.id,
                slug: row.slug,
                region: row.region,
                dsn_enc,
                plan: PlanLimits {
                    plan_id: row.plan_id,
                    isolation_tier: row.isolation_tier,
                    egress_mode: row.egress_mode,
                    max_users: row.max_users,
                    max_concurrent_runs: row.max_concurrent_runs,
                    monthly_tokens: row.monthly_tokens,
                    monthly_cost_cents: row.monthly_cost_cents,
                    max_storage_gb: row.max_storage_gb,
                    max_sandboxes: row.max_sandboxes,
                    model_allowlist: row.model_allowlist,
                    egress_allowlist: row.egress_allowlist,
                    overage_policy: row.overage_policy,
                },
            });
        }
        Ok(out)
    }

    /// 在租户作用域内执行一段后台逻辑（库、BYOK 池、路由、tier 都与请求态一致）。
    pub async fn scope_tenant<F: Future>(
        &self,
        tenant: &TenantInfo,
        fut: F,
    ) -> Result<F::Output, AppError> {
        let pool = self.pool(tenant).await?;
        let keys = self.keys(tenant.id).await?;
        let routes = self.routes(tenant.id).await?;
        Ok(scope_with(
            pool,
            keys,
            routes,
            tenant.plan.isolation_tier.clone(),
            tenant.plan.egress_mode.clone(),
            tenant.id,
            fut,
        )
        .await)
    }

    /// 取（或建）该租户的连接池。DSN 只在建池时解密一次。
    pub async fn pool(&self, tenant: &TenantInfo) -> Result<PgPool, AppError> {
        if let Some((pool, idle)) = self.lookup(tenant.id, &tenant.dsn_enc) {
            if idle < self.idle {
                return Ok(pool);
            }
        }
        let dsn = crypto::open(&self.secrets_key, tenant.id.as_bytes(), &tenant.dsn_enc).map_err(
            |msg| {
                tracing::error!(tenant = %tenant.id, error = %msg, "tenant dsn decrypt failed");
                AppError::internal("tenant database credentials unavailable")
            },
        )?;
        let pool = PgPoolOptions::new()
            .max_connections(self.pool_max)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&dsn)
            .await
            .map_err(|e| {
                tracing::error!(tenant = %tenant.id, error = %e, "tenant pool connect failed");
                AppError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "tenant_database_unavailable",
                    "tenant database unavailable",
                )
            })?;
        self.insert(tenant.id, pool.clone(), tenant.dsn_enc.clone());
        Ok(pool)
    }

    fn lookup(&self, id: Uuid, dsn_enc: &str) -> Option<(PgPool, Duration)> {
        let mut pools = self.pools.lock().ok()?;
        let stale = pools
            .get(&id)
            .is_some_and(|entry| entry.dsn_enc != dsn_enc);
        if stale {
            let old = pools.remove(&id);
            drop(pools);
            if let Some(old) = old {
                tokio::spawn(async move { old.pool.close().await });
            }
            return None;
        }
        let entry = pools.get_mut(&id)?;
        let idle = entry.last_used.elapsed();
        entry.last_used = Instant::now();
        Some((entry.pool.clone(), idle))
    }

    fn insert(&self, id: Uuid, pool: PgPool, dsn_enc: String) {
        let evicted = {
            let Ok(mut pools) = self.pools.lock() else {
                return;
            };
            pools.insert(
                id,
                PoolEntry {
                    pool: pool.clone(),
                    dsn_enc,
                    last_used: Instant::now(),
                },
            );
            if pools.len() <= self.max_pools {
                None
            } else {
                let victim = pools
                    .iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(k, _)| *k);
                victim.and_then(|k| pools.remove(&k)).map(|e| e.pool)
            }
        };
        if let Some(old) = evicted {
            tokio::spawn(async move { old.close().await });
        }
    }
}

/// 去掉端口/尾点并转小写；IPv6 字面量原样保留方括号内容。
pub fn normalize_host(host_header: &str) -> String {
    let raw = host_header.trim();
    let without_port = if raw.starts_with('[') {
        match raw.find(']') {
            Some(idx) => &raw[..=idx],
            None => raw,
        }
    } else {
        raw.split(':').next().unwrap_or(raw)
    };
    without_port.trim_end_matches('.').to_ascii_lowercase()
}

/// 租户 API 的 Host 门禁：解析租户 → 绑定连接池 → 注入 `TenantContext`。
pub async fn tenant_middleware(
    State(state): State<Arc<crate::state::AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if state.cfg.read_only
        && !matches!(
            req.method(),
            &axum::http::Method::GET | &axum::http::Method::HEAD | &axum::http::Method::OPTIONS
        )
    {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "region_read_only",
            "this region is read-only",
        ));
    }
    let Some(registry) = state.tenants.clone() else {
        return Ok(next.run(req).await);
    };
    let host = req
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let tenant = registry.resolve(&host).await?;
    if !region_allows_strict(state.cfg.region_strict, &state.cfg.region, &tenant.region) {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "tenant_wrong_region",
            "tenant belongs to another region",
        ));
    }
    let pool = registry.pool(&tenant).await?;
    let keys = registry.keys(tenant.id).await?;
    let routes = registry.routes(tenant.id).await?;
    tracing::debug!(tenant = %tenant.id, slug = %tenant.slug, "tenant resolved");
    let plan = Some(tenant.plan.clone());
    let tier = tenant.plan.isolation_tier.clone();
    let egress = tenant.plan.egress_mode.clone();
    req.extensions_mut().insert(TenantContext {
        id: tenant.id,
        plan,
    });
    Ok(scope_with(pool, keys, routes, tier, egress, tenant.id, next.run(req)).await)
}

/// 区域绑定检查：本地未配置区域、租户未绑定（空/`default`）或两者一致时放行。
pub(crate) fn region_allows(local: &str, tenant: &str) -> bool {
    let local = local.trim();
    let tenant = tenant.trim();
    local.is_empty() || tenant.is_empty() || local.eq_ignore_ascii_case(tenant)
}

/// 多区域严格模式：区域未绑定的租户不允许被任意区域接管。
pub(crate) fn region_allows_strict(strict: bool, local: &str, tenant: &str) -> bool {
    if !strict || local.trim().is_empty() {
        return region_allows(local, tenant);
    }
    let tenant = tenant.trim();
    !tenant.is_empty()
        && !tenant.eq_ignore_ascii_case("default")
        && region_allows(local, tenant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_binding_fails_closed_only_for_distinct_regions() {
        assert!(region_allows("", "cn-east"));
        assert!(region_allows("cn-east", ""));
        assert!(!region_allows("cn-east", "default"));
        assert!(region_allows("CN-EAST", "cn-east"));
        assert!(!region_allows("cn-east", "us-west"));
        assert!(!region_allows_strict(true, "cn-east", ""));
        assert!(!region_allows_strict(true, "cn-east", "default"));
        assert!(region_allows_strict(true, "cn-east", "cn-east"));
        assert!(region_allows_strict(false, "cn-east", ""));
    }

    #[test]
    fn weighted_selection_respects_weights_and_edges() {
        // 权重 [1, 3]：roll 0 落在第一档，1..=3 落在第二档。
        assert_eq!(weighted_index(&[1, 3], 0), Some(0));
        for roll in 1..=3 {
            assert_eq!(weighted_index(&[1, 3], roll), Some(1), "roll={roll}");
        }
        assert_eq!(weighted_index(&[0, 2], 0), Some(1), "零权重跳过");
        assert_eq!(weighted_index(&[0, 2], 1), Some(1));
        assert_eq!(weighted_index(&[2, 0], 1), Some(0));
        assert_eq!(weighted_index(&[], 0), None);
        assert_eq!(weighted_index(&[0, 0], 0), None);
        // roll 超界（不应发生）返回 None，而不是越界 panic。
        assert_eq!(weighted_index(&[1], 5), None);
    }

    #[test]
    fn pick_key_never_returns_zero_weight_and_always_picks_a_member() {
        let pool = vec![
            PooledKey {
                key: "a".into(),
                weight: 0,
            },
            PooledKey {
                key: "b".into(),
                weight: 1,
            },
        ];
        for _ in 0..50 {
            assert_eq!(pick_key(&pool).map(|k| k.key.as_str()), Some("b"));
        }
        assert!(pick_key(&[]).is_none());
        assert!(pick_key(&[PooledKey {
            key: "z".into(),
            weight: 0
        }])
        .is_none());
    }

    #[test]
    fn pooled_key_debug_never_leaks_the_secret() {
        let key = PooledKey {
            key: "sk-super-secret".into(),
            weight: 2,
        };
        let debug = format!("{key:?}");
        assert!(!debug.contains("sk-super-secret"), "{debug}");
    }

    fn first_route<'a>(routes: &'a [Route], model: &str) -> Option<&'a Route> {
        matching_routes(routes, model).into_iter().next()
    }

    fn route(model: &str, provider: &str, upstream: &str, priority: i32, weight: i32) -> Route {
        Route {
            model: model.into(),
            provider: provider.into(),
            upstream_model: upstream.into(),
            priority,
            weight,
            enabled: true,
        }
    }

    #[test]
    fn route_matching_accepts_plain_and_provider_prefixed_models() {
        let routes = vec![route("gpt-5", "openai", "gpt-5", 100, 1)];
        for wanted in ["gpt-5", "openai:gpt-5", " GPT-5 "] {
            assert!(first_route(&routes, wanted).is_some(), "{wanted}");
        }
        assert!(first_route(&routes, "claude-sonnet-4").is_none());
        assert!(first_route(&routes, "").is_none());

        let routes = vec![route("openai:gpt-5", "custom", "gpt-5-custom", 100, 1)];
        let hit = first_route(&routes, "gpt-5").expect("前缀路由也能命中裸模型名");
        assert_eq!(hit.provider, "custom");
        assert_eq!(hit.upstream_model, "gpt-5-custom");
    }

    #[test]
    fn route_selection_prefers_priority_then_weight() {
        let routes = vec![
            route("gpt-5", "openai", "gpt-5", 200, 1),
            route("gpt-5", "openrouter", "gpt-5", 100, 1),
            route("gpt-5", "siliconflow", "gpt-5", 100, 5),
        ];
        let hit = first_route(&routes, "gpt-5").expect("route");
        assert_eq!(hit.provider, "siliconflow", "同 priority 取 weight 大者");

        let mut disabled = route("gpt-5", "openai", "gpt-5", 1, 1);
        disabled.enabled = false;
        let routes = vec![disabled, route("gpt-5", "openrouter", "gpt-5", 50, 1)];
        assert_eq!(
            first_route(&routes, "gpt-5").map(|r| r.provider.as_str()),
            Some("openrouter"),
            "disabled 路由不参与"
        );
    }

    #[test]
    fn host_normalization() {
        assert_eq!(
            normalize_host("Acme.Apps.Example.COM:443"),
            "acme.apps.example.com"
        );
        assert_eq!(
            normalize_host("acme.apps.example.com."),
            "acme.apps.example.com"
        );
        assert_eq!(normalize_host("[::1]:8088"), "[::1]");
        assert_eq!(normalize_host(" 127.0.0.1:18088 "), "127.0.0.1");
        assert_eq!(normalize_host(""), "");
    }
}
