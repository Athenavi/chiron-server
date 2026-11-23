//! 控制面：租户注册表、域名、套餐、平台管理员与平台 API。
//!
//! 业务数据在每租户独立库；本模块只接触控制库 `chiron_control`，不查询租户业务表。
//! 平台 API 只允许平台 Host 访问；租户 Host 命中 `/api/platform/*` 一律 404。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    body::Bytes,
    extract::{Form, FromRequestParts, Path, Query, Request, State},
    http::{header::HOST, request::Parts, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Datelike, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{
    decode as jwt_decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{
    auth::{burn_password_time, hash_password, validate_password_complexity, verify_password},
    error::{ApiResult, AppError},
    state::AppState,
};

pub mod alipay;
pub mod backup;
pub mod byok;
pub mod crypto;
pub mod domains;
pub mod lifecycle;
pub mod provision;
pub mod settings;
pub mod tenant_migrations;
pub mod usage_rollup;

const PLATFORM_COOKIE: &str = "chiron_platform_token";
const PLATFORM_SCOPE: &str = "platform";
const PLATFORM_TOKEN_HOURS: i64 = 8;
const LOGIN_MAX_FAILURES: u32 = 5;
const LOGIN_LOCK_SECS: u64 = 300;
const RECOVERY_CODE_COUNT: usize = 10;

static LOGIN_FAILS: OnceLock<Mutex<HashMap<String, (u32, Instant)>>> = OnceLock::new();

/// 平台登录失败锁定：同邮箱 5 次失败锁 5 分钟（进程内；多实例下每个节点独立计数）。
fn login_locked(email: &str) -> Option<u64> {
    let mut map = LOGIN_FAILS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some((failures, at)) = map.get(email).copied() else {
        return None;
    };
    let elapsed = at.elapsed();
    if failures >= LOGIN_MAX_FAILURES && elapsed < Duration::from_secs(LOGIN_LOCK_SECS) {
        return Some(LOGIN_LOCK_SECS - elapsed.as_secs());
    }
    if elapsed >= Duration::from_secs(LOGIN_LOCK_SECS) {
        map.remove(email);
    }
    None
}

fn record_login_failure(email: &str) {
    let mut map = LOGIN_FAILS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = map.entry(email.to_string()).or_insert((0, Instant::now()));
    entry.0 = entry.0.saturating_add(1);
    entry.1 = Instant::now();
}

fn clear_login_failures(email: &str) {
    if let Some(map) = LOGIN_FAILS.get() {
        if let Ok(mut map) = map.lock() {
            map.remove(email);
        }
    }
}

fn login_fail_key(email: &str) -> String {
    let digest: String = Sha256::digest(email.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("chiron:platform:login:fail:{digest}")
}

/// 多实例共享的失败计数；Redis 不可用时回落到进程内计数。
async fn login_locked_shared(state: &AppState, email: &str) -> Option<u64> {
    if let Some(handle) = state.redis() {
        let mut conn = handle.conn().await;
        let key = login_fail_key(email);
        let count: i64 = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .ok()
            .and_then(|raw: Option<String>| raw.and_then(|v| v.parse().ok()))
            .unwrap_or(0);
        if count >= LOGIN_MAX_FAILURES as i64 {
            let ttl: i64 = redis::cmd("TTL")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .unwrap_or(-1);
            if ttl > 0 {
                return Some(ttl as u64);
            }
        }
    }
    login_locked(email)
}

async fn record_login_failure_shared(state: &AppState, email: &str) {
    if let Some(handle) = state.redis() {
        let mut conn = handle.conn().await;
        let key = login_fail_key(email);
        if let Ok(count) = redis::cmd("INCR")
            .arg(&key)
            .query_async::<i64>(&mut conn)
            .await
        {
            if count == 1 {
                let _: Result<i64, _> = redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(LOGIN_LOCK_SECS)
                    .query_async(&mut conn)
                    .await;
            }
            return;
        }
    }
    record_login_failure(email);
}

async fn clear_login_failures_shared(state: &AppState, email: &str) {
    if let Some(handle) = state.redis() {
        let mut conn = handle.conn().await;
        let _: Result<i64, _> = redis::cmd("DEL")
            .arg(login_fail_key(email))
            .query_async(&mut conn)
            .await;
    }
    clear_login_failures(email);
}

#[derive(Clone)]
pub struct ControlState {
    pub db: PgPool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlatformClaims {
    sub: String,
    email: String,
    scope: String,
    jti: String,
    iat: usize,
    exp: usize,
}

#[derive(Debug, Clone)]
pub struct PlatformAdmin {
    pub id: Uuid,
    pub email: String,
}

#[derive(Debug, FromRow)]
struct AdminRow {
    id: Uuid,
    email: String,
    status: String,
}

/// 登录专用行：额外带上 TOTP 状态（提取器不需要，避免影响已有查询）。
#[derive(Debug, FromRow)]
struct LoginRow {
    id: Uuid,
    email: String,
    password_hash: String,
    status: String,
    totp_enabled: bool,
    totp_secret_enc: Option<String>,
}

#[derive(Debug, FromRow, Serialize)]
pub struct PlanRow {
    pub id: String,
    pub name: String,
    pub isolation_tier: String,
    pub max_users: i32,
    pub max_concurrent_runs: i32,
    pub monthly_tokens: i64,
    pub max_storage_gb: i32,
    pub max_sandboxes: i32,
    pub price_cents: i32,
    pub overage_policy: String,
}

#[derive(Debug, FromRow, Serialize)]
pub struct TenantRow {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub plan_id: String,
    pub region: String,
    pub owner_email: String,
    pub db_name: String,
    pub provision_error: String,
    pub provisioned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow, Serialize)]
pub struct DomainRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub host: String,
    pub kind: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub tls_status: String,
}

fn ok(data: serde_json::Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

fn with_cookie(mut resp: Response, cookie: HeaderValue) -> Response {
    resp.headers_mut()
        .append(axum::http::header::SET_COOKIE, cookie);
    resp
}

fn cookie(state: &AppState, token: &str) -> HeaderValue {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{PLATFORM_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        PLATFORM_TOKEN_HOURS * 3600
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("chiron_platform_token=; Path=/; Max-Age=0"))
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for part in v.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix(&format!("{PLATFORM_COOKIE}=")) {
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn issue(state: &AppState, admin_id: Uuid, admin_email: &str) -> Result<String, AppError> {
    let now = Utc::now().timestamp() as usize;
    let claims = PlatformClaims {
        sub: admin_id.to_string(),
        email: admin_email.to_string(),
        scope: PLATFORM_SCOPE.to_string(),
        jti: Uuid::new_v4().to_string(),
        iat: now,
        exp: now + (PLATFORM_TOKEN_HOURS as usize) * 3600,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "platform jwt encode failed");
        AppError::internal("platform token issue failed")
    })
}

fn decode_platform(state: &AppState, token: &str) -> Result<PlatformClaims, AppError> {
    let validation = Validation::new(Algorithm::HS256);
    let claims = jwt_decode::<PlatformClaims>(
        token,
        &DecodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
        &validation,
    )
    .map(|d| d.claims)
    .map_err(|_| AppError::unauthorized("invalid platform token"))?;
    if claims.scope != PLATFORM_SCOPE {
        return Err(AppError::unauthorized("invalid platform token"));
    }
    Ok(claims)
}

impl FromRequestParts<Arc<AppState>> for PlatformAdmin {
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let state = state.clone();
        let token = token_from(&parts.headers);
        async move {
            let token = token.ok_or_else(|| AppError::unauthorized("missing platform token"))?;
            let claims = decode_platform(&state, &token)?;
            let control = state.control.as_ref().ok_or_else(|| {
                AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "control_unavailable",
                    "control plane is not configured",
                )
            })?;
            let row = sqlx::query_as::<_, AdminRow>(
                "SELECT id, email, status FROM platform_admins WHERE id = $1",
            )
            .bind(
                Uuid::parse_str(&claims.sub)
                    .map_err(|_| AppError::unauthorized("invalid platform token"))?,
            )
            .fetch_optional(&control.db)
            .await?
            .ok_or_else(|| AppError::unauthorized("platform admin not found"))?;
            if row.status != "active" {
                return Err(AppError::forbidden("platform admin disabled"));
            }
            Ok(PlatformAdmin {
                id: row.id,
                email: row.email,
            })
        }
    }
}

/// 平台 Host 门禁：配置了 `PLATFORM_HOST` 时只认它；开发态（空）只放行回环。
async fn platform_host_guard(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let expected = state.cfg.platform_host.trim().to_ascii_lowercase();
    if expected.is_empty() {
        // 未配置平台域名时，不能只信可伪造的 Host 头：必须来自本机回环。
        let peer = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|info| info.0);
        if !peer.is_some_and(|addr| addr.ip().is_loopback()) {
            return Err(AppError::not_found("not found"));
        }
    }
    let host = req
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(':').next().unwrap_or(v).to_ascii_lowercase())
        .unwrap_or_default();
    if !host_matches(&expected, &host) {
        return Err(AppError::not_found("not found"));
    }
    Ok(next.run(req).await)
}

pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let protected = Router::new()
        .route("/api/platform/v1/me", get(me))
        .route(
            "/api/platform/v1/settings/integrations/alipay",
            get(settings::get_alipay).put(settings::put_alipay),
        )
        .route(
            "/api/platform/v1/settings/alerts",
            get(settings::get_alerts).put(settings::put_alerts),
        )
        .route(
            "/api/platform/v1/settings/rate-limits",
            get(settings::get_rates).put(settings::put_rates),
        )
        .route(
            "/api/platform/v1/settings/media",
            get(settings::get_media).put(settings::put_media),
        )
        .route(
            "/api/platform/v1/settings/providers",
            get(settings::get_providers).put(settings::put_providers),
        )
        .route("/api/platform/v1/me/totp/setup", post(totp_setup))
        .route("/api/platform/v1/me/totp/enable", post(totp_enable))
        .route("/api/platform/v1/me/totp/disable", post(totp_disable))
        .route("/api/platform/v1/plans", get(list_plans))
        .route(
            "/api/platform/v1/tenants",
            get(list_tenants).post(create_tenant),
        )
        .route(
            "/api/platform/v1/tenants/{id}",
            get(get_tenant).patch(patch_tenant),
        )
        .route(
            "/api/platform/v1/tenants/{id}/suspend",
            post(suspend_tenant),
        )
        .route("/api/platform/v1/tenants/{id}/resume", post(resume_tenant))
        .route(
            "/api/platform/v1/tenants/{id}/provision",
            post(provision_tenant),
        )
        .route(
            "/api/platform/v1/tenants/{id}/owner-credentials",
            post(owner_credentials),
        )
        .route(
            "/api/platform/v1/tenants/{id}/provider-keys",
            get(byok::list_keys).put(byok::put_key),
        )
        .route(
            "/api/platform/v1/tenants/{id}/provider-keys/{provider}",
            axum::routing::delete(byok::delete_key),
        )
        .route(
            "/api/platform/v1/tenants/{id}/export",
            post(lifecycle::export_tenant),
        )
        .route(
            "/api/platform/v1/tenants/{id}/delete",
            post(lifecycle::delete_tenant),
        )
        .route(
            "/api/platform/v1/tenants/{id}/backup",
            post(backup::backup_tenant),
        )
        .route(
            "/api/platform/v1/migrations",
            get(tenant_migrations::status_handler),
        )
        .route(
            "/api/platform/v1/migrations/upgrade",
            post(tenant_migrations::upgrade_handler),
        )
        .route(
            "/api/platform/v1/tenants/{id}/domains",
            get(list_domains).post(add_domain),
        )
        .route(
            "/api/platform/v1/tenants/{id}/domains/{domain_id}/verify",
            post(domains::verify_domain),
        )
        // Caddy on-demand TLS ask 端点：仅平台 Host 可达，无登录态。
        .route("/api/platform/v1/tls/allow", get(domains::tls_allow))
        .route(
            "/api/platform/v1/tenants/{id}/domains/{domain_id}/probe-tls",
            post(domains::probe_tls_domain),
        )
        .route("/api/platform/v1/usage", get(usage))
        .route("/api/platform/v1/usage/export.csv", get(usage_export))
        .route("/api/platform/v1/tenants/{id}/bill", get(tenant_bill))
        .route("/api/platform/v1/tenants/{id}/bill.csv", get(tenant_bill_csv))
        .route(
            "/api/platform/v1/tenants/{id}/invoice",
            get(tenant_invoice),
        )
        .route(
            "/api/platform/v1/tenants/{id}/invoice/status",
            post(invoice_status),
        )
        .route(
            "/api/platform/v1/tenants/{id}/invoice/pay",
            post(alipay_pay),
        )
        .route(
            "/api/platform/v1/tenants/{id}/bill/reconcile",
            get(tenant_bill_reconcile),
        )
        .route("/api/platform/v1/regions", get(regions))
        .route(
            "/api/platform/v1/regions/backfill",
            post(region_backfill),
        )
        .route(
            "/api/platform/v1/usage/rollup",
            post(usage_rollup::rollup_handler),
        )
        .route("/api/platform/v1/audit", get(audit));

    let login = Router::new()
        .route("/api/platform/v1/auth/login", post(login))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::admin::limits::enforce,
        ));

    // 支付渠道回写：HMAC 鉴权，不依赖平台管理员会话。
    let webhook = Router::new()
        .route("/api/platform/v1/billing/webhook", post(billing_webhook))
        .route("/api/platform/v1/billing/alipay/notify", post(alipay_notify))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::admin::limits::enforce,
        ));

    Router::new()
        .merge(login)
        .merge(webhook)
        .merge(protected)
        .layer(axum::middleware::from_fn_with_state(
            state,
            platform_host_guard,
        ))
}

#[derive(Debug, Deserialize)]
struct LoginBody {
    email: String,
    password: String,
    /// 已启用 TOTP 时必填；未启用时忽略。
    #[serde(default)]
    totp_code: Option<String>,
}

async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let email = body.email.trim().to_ascii_lowercase();
    if let Some(retry) = login_locked_shared(&state, &email).await {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "auth.locked",
            format!("too many failed attempts; retry in {retry}s"),
        ));
    }
    let row = sqlx::query_as::<_, LoginRow>(
        "SELECT id, email, password_hash, status, totp_enabled, totp_secret_enc
           FROM platform_admins WHERE lower(email) = $1",
    )
    .bind(&email)
    .fetch_optional(&control.db)
    .await?;
    let Some(row) = row else {
        burn_password_time(&body.password);
        record_login_failure_shared(&state, &email).await;
        return Err(AppError::unauthorized("invalid email or password"));
    };
    if row.status != "active" {
        burn_password_time(&body.password);
        record_login_failure_shared(&state, &email).await;
        return Err(AppError::unauthorized("invalid email or password"));
    }
    if !verify_password(&row.password_hash, &body.password) {
        record_login_failure_shared(&state, &email).await;
        return Err(AppError::unauthorized("invalid email or password"));
    }
    // 密码通过后才检查 TOTP：未持有正确密码者无法据此判断账号是否开启二次验证。
    if row.totp_enabled {
        let provided = body.totp_code.as_deref().map(str::trim).unwrap_or("");
        if provided.is_empty() {
            return Err(AppError::new(
                StatusCode::UNAUTHORIZED,
                "auth.totp_required",
                "totp code required",
            ));
        }
        let enc = row
            .totp_secret_enc
            .as_deref()
            .ok_or_else(|| AppError::internal("totp enabled but secret is missing"))?;
        let secret = open_totp_secret(&state, row.id, enc)?;
        let step = totp::verify_step(&secret, provided, Utc::now().timestamp());
        let mut used_recovery = false;
        match step {
            Some(step) => {
                let updated = sqlx::query(
                    "UPDATE platform_admins SET totp_last_step = $2 \
                     WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)",
                )
                .bind(row.id)
                .bind(step)
                .execute(&control.db)
                .await?
                .rows_affected();
                if updated != 1 {
                    record_login_failure_shared(&state, &email).await;
                    return Err(AppError::new(
                        StatusCode::UNAUTHORIZED,
                        "auth.totp_invalid",
                        "totp code already used",
                    ));
                }
            }
            None => {
                used_recovery = consume_recovery_code(&state, &control.db, row.id, provided).await;
            }
        }
        if step.is_none() && !used_recovery {
            platform_audit(
                &state,
                Some(row.id),
                &row.email,
                "platform_login_totp_failed",
                None,
                "",
                "",
            )
            .await;
            record_login_failure_shared(&state, &email).await;
            return Err(AppError::new(
                StatusCode::UNAUTHORIZED,
                "auth.totp_invalid",
                "invalid totp or recovery code",
            ));
        }
    }
    clear_login_failures_shared(&state, &email).await;
    let token = issue(&state, row.id, &row.email)?;
    platform_audit(
        &state,
        Some(row.id),
        &row.email,
        "platform_login",
        None,
        "",
        "",
    )
    .await;
    Ok(with_cookie(
        ok(json!({ "token": token, "admin": { "id": row.id, "email": row.email } })),
        cookie(&state, &token),
    ))
}

async fn me(State(state): State<Arc<AppState>>, admin: PlatformAdmin) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let totp_enabled: bool =
        sqlx::query_scalar("SELECT totp_enabled FROM platform_admins WHERE id = $1")
            .bind(admin.id)
            .fetch_one(&control.db)
            .await?;
    Ok(ok(json!({
        "id": admin.id,
        "email": admin.email,
        "totp_enabled": totp_enabled,
    })))
}

async fn list_plans(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows = sqlx::query_as::<_, PlanRow>(
        "SELECT id, name, isolation_tier, max_users, max_concurrent_runs,
                monthly_tokens, max_storage_gb, max_sandboxes, price_cents, overage_policy
           FROM plans ORDER BY price_cents ASC, id ASC",
    )
    .fetch_all(&control.db)
    .await?;
    Ok(ok(json!({ "plans": rows })))
}

async fn list_tenants(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows = sqlx::query_as::<_, TenantRow>(
        "SELECT id, slug, name, status, plan_id, region, owner_email, db_name,
                provision_error, provisioned_at, created_at, updated_at
           FROM tenants WHERE status <> 'deleted'
          ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&control.db)
    .await?;
    Ok(ok(json!({ "tenants": rows })))
}

#[derive(Debug, Deserialize)]
struct CreateTenantBody {
    slug: String,
    name: String,
    plan_id: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    owner_email: String,
}

fn valid_slug(s: &str) -> bool {
    let len = s.chars().count();
    (3..=32).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn valid_email(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && s.len() <= 254
        && !s.chars().any(|c| c.is_whitespace() || c == ',')
}

async fn create_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<CreateTenantBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let slug = body.slug.trim().to_ascii_lowercase();
    if !valid_slug(&slug) {
        return Err(AppError::bad_request(
            "slug must be 3-32 lowercase letters/digits/hyphen and not start/end with hyphen",
        ));
    }
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("name is required"));
    }
    let owner_email = body.owner_email.trim().to_ascii_lowercase();
    if !valid_email(&owner_email) {
        return Err(AppError::bad_request(
            "owner_email is required and must be a valid address",
        ));
    }
    let plan_exists: Option<String> = sqlx::query_scalar("SELECT id FROM plans WHERE id = $1")
        .bind(&body.plan_id)
        .fetch_optional(&control.db)
        .await?;
    if plan_exists.is_none() {
        return Err(AppError::bad_request("unknown plan_id"));
    }
    let db_name = provision::db_ident(&slug);
    let region = if body.region.trim().is_empty() {
        let configured = state.cfg.region.trim();
        if configured.is_empty() {
            "default".to_string()
        } else {
            configured.to_string()
        }
    } else {
        body.region.trim().to_string()
    };
    if state.cfg.region_strict
        && (region.is_empty() || region.eq_ignore_ascii_case("default"))
    {
        return Err(AppError::bad_request(
            "CHIRON_REGION_STRICT requires an explicit non-default region",
        ));
    }

    let tenant = sqlx::query_as::<_, TenantRow>(
        "INSERT INTO tenants (slug, name, status, plan_id, region, owner_email, db_name)
         VALUES ($1, $2, 'pending', $3, $4, $5, $6)
         RETURNING id, slug, name, status, plan_id, region, owner_email, db_name,
                   provision_error, provisioned_at, created_at, updated_at",
    )
    .bind(&slug)
    .bind(name)
    .bind(&body.plan_id)
    .bind(&region)
    .bind(&owner_email)
    .bind(&db_name)
    .fetch_one(&control.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.code().as_deref() == Some("23505") => {
            AppError::conflict("slug or db name already exists")
        }
        other => AppError::from(other),
    })?;

    if !state.cfg.tenant_base_domain.trim().is_empty() {
        let host = format!("{}.{}", slug, state.cfg.tenant_base_domain.trim());
        let _ = sqlx::query(
            "INSERT INTO tenant_domains (tenant_id, host, kind, tls_status)
             VALUES ($1, $2, 'subdomain', 'pending')",
        )
        .bind(tenant.id)
        .bind(host)
        .execute(&control.db)
        .await;
    }

    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_created",
        Some(tenant.id),
        &format!("slug={slug} plan={} region={region}", body.plan_id),
        "",
    )
    .await;
    if state.cfg.provisioning_ready() {
        let bg = state.clone();
        let tenant_id = tenant.id;
        tokio::spawn(async move {
            if let Err(err) = provision::provision_tenant(&bg, tenant_id).await {
                tracing::warn!(tenant = %tenant_id, error = %err, "tenant provisioning failed");
            }
        });
    } else {
        tracing::warn!(
            tenant = %tenant.id,
            "tenant created without provisioning: SECRETS_KEY / PROVISION_DATABASE_URL missing"
        );
    }
    Ok((StatusCode::CREATED, Json(json!({ "data": tenant }))).into_response())
}

async fn get_tenant(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    Ok(ok(json!({ "tenant": tenant })))
}

async fn patch_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let plan_id = body.get("plan_id").and_then(|v| v.as_str());
    let region = body
        .get("region")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let Some(region) = region {
        let valid = region.len() <= 64
            && region
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(AppError::bad_request(
                "region must be 1-64 chars of [a-z0-9_-]",
            ));
        }
        if state.cfg.region_strict && region.eq_ignore_ascii_case("default") {
            return Err(AppError::bad_request(
                "CHIRON_REGION_STRICT does not allow the default region",
            ));
        }
    }
    if let Some(plan) = plan_id {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM plans WHERE id = $1")
            .bind(plan)
            .fetch_optional(&control.db)
            .await?;
        if exists.is_none() {
            return Err(AppError::bad_request("unknown plan_id"));
        }
    }
    let (dsn_enc, dsn_db) = match body
        .get("dsn")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(dsn) => {
            if tenant.status != "suspended" {
                return Err(AppError::bad_request(
                    "suspend the tenant before changing its database dsn",
                ));
            }
            if state.cfg.secrets_key.trim().is_empty() {
                return Err(AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "provision_unavailable",
                    "SECRETS_KEY is required to store a tenant dsn",
                ));
            }
            let db_name = url::Url::parse(dsn)
                .map_err(|_| AppError::bad_request("dsn must be a valid postgres url"))?
                .path()
                .trim_start_matches('/')
                .to_string();
            if db_name.is_empty()
                || !db_name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(AppError::bad_request(
                    "dsn database name must be a safe lowercase identifier",
                ));
            }
            let sealed = crypto::seal(&state.cfg.secrets_key, id.as_bytes(), dsn)
                .map_err(|msg| AppError::internal(format!("encrypt tenant dsn: {msg}")))?;
            (Some(sealed), Some(db_name))
        }
        None => (None, None),
    };
    let updated = sqlx::query_as::<_, TenantRow>(
        "UPDATE tenants
            SET name = COALESCE($1, name), plan_id = COALESCE($2, plan_id),
                region = COALESCE($3, region), dsn_enc = COALESCE($4, dsn_enc),
                db_name = COALESCE($5, db_name), updated_at = now()
          WHERE id = $6
          RETURNING id, slug, name, status, plan_id, region, owner_email, db_name,
                    provision_error, provisioned_at, created_at, updated_at",
    )
    .bind(name)
    .bind(plan_id)
    .bind(region)
    .bind(dsn_enc)
    .bind(dsn_db)
    .bind(id)
    .fetch_one(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_updated",
        Some(tenant.id),
        "",
        "",
    )
    .await;
    Ok(ok(json!({ "tenant": updated })))
}

async fn suspend_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    set_tenant_status(state, admin, id, "suspended").await
}

async fn resume_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    set_tenant_status(state, admin, id, "active").await
}

/// 暂停后尽量让数据面静默：取消 active run、清理容器、断开租户库连接。
/// 跨实例的最终收敛仍依赖控制库状态与租约；失败只告警，不回滚暂停。
async fn quiesce_tenant(state: &AppState, tenant: &TenantRow) {
    let Some(control) = state.control.as_ref() else {
        return;
    };
    let enc: Option<String> = sqlx::query_scalar("SELECT dsn_enc FROM tenants WHERE id = $1")
        .bind(tenant.id)
        .fetch_optional(&control.db)
        .await
        .ok()
        .flatten();
    if let Some(enc) = enc {
        match crypto::open(&state.cfg.secrets_key, tenant.id.as_bytes(), &enc) {
            Ok(dsn) => {
                match sqlx::PgPool::connect(&dsn).await {
                    Ok(pool) => {
                        let sql = format!(
                            "UPDATE runs SET status = 'cancelled', \
                                 error = jsonb_build_object('reason', 'tenant_suspended'), \
                                 finished_at = now(), updated_at = now() \
                             WHERE status IN ({}) RETURNING id, session_id",
                            crate::sessions::run_state::ACTIVE_STATUS_SQL
                        );
                        let runs: Vec<(Uuid, Uuid)> = sqlx::query_as(&sql)
                            .fetch_all(&pool)
                            .await
                            .unwrap_or_default();
                        let _ = sqlx::query(
                            "UPDATE approvals SET status = 'cancelled', decided_at = now() \
                             WHERE status = 'pending'",
                        )
                        .execute(&pool)
                        .await;
                        pool.close().await;
                        for (run_id, session_id) in runs {
                            crate::agent::registry().cancel(run_id);
                            crate::sessions::event_log::global().append(
                                session_id,
                                Some(run_id),
                                "run.cancelled",
                                json!({ "reason": "tenant_suspended" }),
                            );
                        }
                    }
                    Err(e) => tracing::warn!(tenant = %tenant.id, error = %e, "quiesce connect failed"),
                }
            }
            Err(e) => tracing::warn!(tenant = %tenant.id, error = %e, "quiesce dsn decrypt failed"),
        }
    }
    if let Err(e) = state
        .sandbox
        .remove_tenant_containers(&tenant.id.to_string())
        .await
    {
        tracing::warn!(tenant = %tenant.id, error = %e.message, "quiesce container cleanup failed");
    }
    let admin_url = state.cfg.provisioning_database().to_string();
    if !admin_url.is_empty() && !tenant.db_name.trim().is_empty() {
        if let Ok(pool) = sqlx::PgPool::connect(&admin_url).await {
            let _ = sqlx::query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                  WHERE datname = $1 AND pid <> pg_backend_pid()",
            )
            .bind(tenant.db_name.trim())
            .execute(&pool)
            .await;
            pool.close().await;
        }
    }
}

async fn set_tenant_status(
    state: Arc<AppState>,
    admin: PlatformAdmin,
    id: Uuid,
    status: &'static str,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let updated = sqlx::query_as::<_, TenantRow>(
        "UPDATE tenants SET status = $1, updated_at = now() WHERE id = $2
         RETURNING id, slug, name, status, plan_id, region, owner_email, db_name,
                   provision_error, provisioned_at, created_at, updated_at",
    )
    .bind(status)
    .bind(id)
    .fetch_one(&control.db)
    .await?;
    if status == "suspended" {
        quiesce_tenant(&state, &updated).await;
    }
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        if status == "suspended" {
            "tenant_suspended"
        } else {
            "tenant_resumed"
        },
        Some(tenant.id),
        "",
        "",
    )
    .await;
    Ok(ok(json!({ "tenant": updated })))
}

async fn list_domains(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows = sqlx::query_as::<_, DomainRow>(
        "SELECT id, tenant_id, host, kind, verified_at, tls_status
           FROM tenant_domains WHERE tenant_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&control.db)
    .await?;
    Ok(ok(json!({ "domains": rows })))
}

#[derive(Debug, Deserialize)]
struct AddDomainBody {
    host: String,
}

async fn add_domain(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Json(body): Json<AddDomainBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let host = body.host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.contains('/') || host.contains(':') {
        return Err(AppError::bad_request("invalid host"));
    }
    let custom_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tenant_domains WHERE tenant_id = $1 AND kind = 'custom'",
    )
    .bind(id)
    .fetch_one(&control.db)
    .await?;
    if custom_count >= 1 {
        return Err(AppError::conflict(
            "each tenant can bind at most one custom domain",
        ));
    }
    let row = sqlx::query_as::<_, DomainRow>(
        "INSERT INTO tenant_domains (tenant_id, host, kind, tls_status)
         VALUES ($1, $2, 'custom', 'pending')
         RETURNING id, tenant_id, host, kind, verified_at, tls_status",
    )
    .bind(id)
    .bind(&host)
    .fetch_one(&control.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.code().as_deref() == Some("23505") => {
            AppError::conflict("host already bound to another tenant")
        }
        other => AppError::from(other),
    })?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_domain_added",
        Some(tenant.id),
        &host,
        "",
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({ "data": row }))).into_response())
}

async fn provision_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_provision_requested",
        Some(id),
        "",
        "",
    )
    .await;
    let tenant = provision::provision_tenant(&state, id).await?;
    Ok(ok(json!({ "tenant": tenant })))
}

/// 一次性领取 owner 临时密码：读取 → 解密 → 清空密文（条件更新防并发重复领取）。
async fn owner_credentials(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let key = state.cfg.secrets_key.trim().to_string();
    crypto::decode_key(&key).map_err(|msg| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT owner_email, owner_temp_password_enc FROM tenants
          WHERE id = $1 AND status <> 'deleted'",
    )
    .bind(id)
    .fetch_optional(&control.db)
    .await?;
    let (email, enc) = row.ok_or_else(|| AppError::not_found("tenant not found"))?;
    let enc = enc.ok_or_else(|| AppError::not_found("no pending owner credentials"))?;
    let password = crypto::open(&key, id.as_bytes(), &enc)
        .map_err(|msg| AppError::internal(format!("decrypt owner password: {msg}")))?;
    sqlx::query(
        "UPDATE tenants SET owner_temp_password_enc = NULL, updated_at = now()
          WHERE id = $1 AND owner_temp_password_enc = $2",
    )
    .bind(id)
    .bind(&enc)
    .execute(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_owner_credentials_revealed",
        Some(id),
        "",
        "",
    )
    .await;
    Ok(ok(json!({ "email": email, "password": password })))
}

async fn usage(State(state): State<Arc<AppState>>, _admin: PlatformAdmin) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<String>,
            chrono::NaiveDate,
            i64,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT u.tenant_id, t.slug, u.day, u.runs, u.tokens_in, u.tokens_out,
                u.cost_cents, u.storage_bytes
           FROM tenant_usage_daily u
           LEFT JOIN tenants t ON t.id = u.tenant_id
          ORDER BY u.day DESC, t.slug ASC LIMIT 500",
    )
    .fetch_all(&control.db)
    .await?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(tid, slug, day, runs, tin, tout, cost, storage)| {
            json!({
                "tenant_id": tid,
                "slug": slug,
                "day": day,
                "runs": runs,
                "tokens_in": tin,
                "tokens_out": tout,
                "cost_cents": cost,
                "storage_bytes": storage,
            })
        })
        .collect();
    Ok(ok(json!({ "usage": items })))
}

#[derive(Debug, Deserialize)]
struct UsageExportQuery {
    #[serde(default)]
    days: Option<i64>,
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

async fn usage_export(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Query(q): Query<UsageExportQuery>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let days = q.days.unwrap_or(90).clamp(1, 366);
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<String>,
            chrono::NaiveDate,
            i64,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT u.tenant_id, t.slug, u.day, u.runs, u.tokens_in, u.tokens_out,
                u.cost_cents, u.storage_bytes
           FROM tenant_usage_daily u
           LEFT JOIN tenants t ON t.id = u.tenant_id
          WHERE u.day >= current_date - $1::int
          ORDER BY u.day ASC, t.slug ASC",
    )
    .bind(days as i32)
    .fetch_all(&control.db)
    .await?;

    let mut csv = String::from(
        "tenant_id,slug,day,runs,tokens_in,tokens_out,cost_cents,storage_bytes\n",
    );
    for (tid, slug, day, runs, tin, tout, cost, storage) in rows {
        let slug = slug.unwrap_or_default();
        csv.push_str(&format!(
            "{},{},{day},{runs},{tin},{tout},{cost},{storage}\n",
            csv_field(&tid.to_string()),
            csv_field(&slug)
        ));
    }
    let filename = format!("chiron-usage-{}.csv", Utc::now().format("%Y%m%d"));
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        csv,
    )
        .into_response())
}

#[derive(Debug, Default, Deserialize)]
struct BillQuery {
    /// `YYYY-MM`；缺省当月。
    #[serde(default)]
    month: Option<String>,
}

fn bill_month(raw: Option<&str>) -> Result<(String, chrono::NaiveDate, chrono::NaiveDate), AppError> {
    let label = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Utc::now().format("%Y-%m").to_string());
    let start = chrono::NaiveDate::parse_from_str(&format!("{label}-01"), "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("month must be YYYY-MM"))?;
    let (year, month) = if start.month() == 12 {
        (start.year() + 1, 1)
    } else {
        (start.year(), start.month() + 1)
    };
    let next = chrono::NaiveDate::from_ymd_opt(year, month, 1).expect("valid next month");
    Ok((label, start, next.pred_opt().expect("valid month end")))
}

type BillRow = (chrono::NaiveDate, i64, i64, i64, i64, i64);

async fn bill_rows(
    state: &AppState,
    tenant_id: Uuid,
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
) -> Result<Vec<BillRow>, AppError> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    fetch_tenant(&control.db, tenant_id).await?;
    Ok(sqlx::query_as(
        "SELECT day, runs, tokens_in, tokens_out, cost_cents, storage_bytes
           FROM tenant_usage_daily
          WHERE tenant_id = $1 AND day >= $2 AND day <= $3
          ORDER BY day ASC",
    )
    .bind(tenant_id)
    .bind(start)
    .bind(end)
    .fetch_all(&control.db)
    .await?)
}

fn bill_totals(rows: &[BillRow]) -> (i64, i64, i64, i64, i64) {
    rows.iter().fold((0, 0, 0, 0, 0), |acc, row| {
        (
            acc.0 + row.1,
            acc.1 + row.2,
            acc.2 + row.3,
            acc.3 + row.4,
            acc.4 + row.5,
        )
    })
}

async fn tenant_bill(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Query(q): Query<BillQuery>,
) -> ApiResult<Response> {
    let (month, start, end) = bill_month(q.month.as_deref())?;
    let rows = bill_rows(&state, id, start, end).await?;
    let totals = bill_totals(&rows);
    let days: Vec<_> = rows
        .iter()
        .map(|row| {
            json!({
                "day": row.0,
                "runs": row.1,
                "tokens_in": row.2,
                "tokens_out": row.3,
                "cost_cents": row.4,
                "storage_bytes": row.5,
            })
        })
        .collect();
    Ok(ok(json!({
        "tenant_id": id,
        "month": month,
        "from": start,
        "to": end,
        "totals": {
            "runs": totals.0,
            "tokens_in": totals.1,
            "tokens_out": totals.2,
            "cost_cents": totals.3,
            "storage_bytes": totals.4,
        },
        "days": days,
    })))
}

async fn tenant_bill_csv(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Query(q): Query<BillQuery>,
) -> ApiResult<Response> {
    let (month, start, end) = bill_month(q.month.as_deref())?;
    let rows = bill_rows(&state, id, start, end).await?;
    let mut csv = String::from("day,runs,tokens_in,tokens_out,cost_cents,storage_bytes\n");
    for row in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            row.0, row.1, row.2, row.3, row.4, row.5
        ));
    }
    let filename = format!("chiron-bill-{month}-{id}.csv");
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/csv; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        csv,
    )
        .into_response())
}

async fn tenant_invoice(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Query(q): Query<BillQuery>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let (month, start, end) = bill_month(q.month.as_deref())?;
    let rows = bill_rows(&state, id, start, end).await?;
    let totals = bill_totals(&rows);
    let (plan_name, price_cents): (String, i32) =
        sqlx::query_as("SELECT name, price_cents FROM plans WHERE id = $1")
            .bind(&tenant.plan_id)
            .fetch_one(&control.db)
            .await?;
    let paid: Option<(String, String, String, i64, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT provider, external_id, status, amount_cents, paid_at
           FROM tenant_invoice_status WHERE tenant_id = $1 AND month = $2",
    )
    .bind(id)
    .bind(&month)
    .fetch_optional(&control.db)
    .await?;
    let payment = match paid {
        Some((provider, external_id, status, amount_cents, paid_at)) => json!({
            "provider": provider,
            "external_id": external_id,
            "status": status,
            "amount_cents": amount_cents,
            "paid_at": paid_at,
        }),
        None => json!({ "provider": "external", "status": "pending" }),
    };
    let base_cents = i64::from(price_cents);
    let usage_cents = totals.3;
    let number = format!(
        "INV-{}-{}",
        tenant.slug.to_ascii_uppercase(),
        month.replace('-', "")
    );
    Ok(ok(json!({
        "number": number,
        "tenant_id": id,
        "tenant_slug": tenant.slug,
        "month": month,
        "from": start,
        "to": end,
        "plan_id": tenant.plan_id,
        "plan_name": plan_name,
        "base_cents": base_cents,
        "usage_cents": usage_cents,
        "total_cents": base_cents.saturating_add(usage_cents),
        "payment": payment,
        "lines": [
            { "kind": "plan", "description": plan_name, "amount_cents": base_cents },
            {
                "kind": "usage",
                "description": "tokens and runs for the month",
                "amount_cents": usage_cents,
                "runs": totals.0,
                "tokens_in": totals.1,
                "tokens_out": totals.2,
            }
        ],
    })))
}

/// 控制库 rollup vs 租户库原始 usage_events/runs，暴露日汇总漂移。
async fn tenant_bill_reconcile(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Query(q): Query<BillQuery>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let (month, start, end) = bill_month(q.month.as_deref())?;
    let enc: Option<String> = sqlx::query_scalar("SELECT dsn_enc FROM tenants WHERE id = $1")
        .bind(id)
        .fetch_optional(&control.db)
        .await?
        .flatten();
    let enc = enc.ok_or_else(|| AppError::conflict("tenant is not provisioned"))?;
    let dsn = crypto::open(&state.cfg.secrets_key, id.as_bytes(), &enc)
        .map_err(|_| AppError::internal("tenant database credentials unavailable"))?;
    let pool = sqlx::PgPool::connect(&dsn)
        .await
        .map_err(|_| AppError::internal("tenant database unavailable"))?;
    let raw: (i64, i64, i64) = sqlx::query_as(
        "SELECT COALESCE(sum(input_tokens + cache_read_input_tokens
                             + cache_creation_input_tokens), 0)::bigint,
                COALESCE(sum(output_tokens), 0)::bigint,
                COALESCE(round(sum(cost) * 100), 0)::bigint
           FROM usage_events
          WHERE (created_at AT TIME ZONE 'UTC')::date BETWEEN $1 AND $2",
    )
    .bind(start)
    .bind(end)
    .fetch_one(&pool)
    .await?;
    let raw_runs: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM runs
          WHERE (created_at AT TIME ZONE 'UTC')::date BETWEEN $1 AND $2",
    )
    .bind(start)
    .bind(end)
    .fetch_one(&pool)
    .await?;
    pool.close().await;

    let totals = bill_totals(&bill_rows(&state, id, start, end).await?);
    let rollup = (totals.1, totals.2, totals.3, totals.0);
    let tenant_raw = (raw.0, raw.1, raw.2, raw_runs);
    let drift = rollup != tenant_raw;
    Ok(ok(json!({
        "tenant_id": id,
        "tenant_slug": tenant.slug,
        "month": month,
        "from": start,
        "to": end,
        "rollup": {
            "tokens_in": rollup.0,
            "tokens_out": rollup.1,
            "cost_cents": rollup.2,
            "runs": rollup.3,
        },
        "tenant_db": {
            "tokens_in": tenant_raw.0,
            "tokens_out": tenant_raw.1,
            "cost_cents": tenant_raw.2,
            "runs": tenant_raw.3,
        },
        "drift": drift,
    })))
}

#[derive(Debug, Deserialize)]
struct InvoiceStatusBody {
    month: String,
    status: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    external_id: String,
    #[serde(default)]
    amount_cents: i64,
    #[serde(default)]
    paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct BillingWebhookBody {
    tenant_id: Uuid,
    month: String,
    status: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    external_id: String,
    #[serde(default)]
    amount_cents: i64,
    #[serde(default)]
    paid_at: Option<DateTime<Utc>>,
}

fn verify_billing_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let hex = header
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(header.trim());
    if hex.len() != 64 {
        return false;
    }
    let mut bytes = [0u8; 32];
    for (idx, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let part = std::str::from_utf8(pair).unwrap_or("");
        let Ok(byte) = u8::from_str_radix(part, 16) else {
            return false;
        };
        bytes[idx] = byte;
    }
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&bytes).is_ok()
}

fn validate_invoice_status(status: &str) -> Result<String, AppError> {
    let status = status.trim().to_ascii_lowercase();
    if matches!(status.as_str(), "pending" | "paid" | "failed" | "refunded") {
        Ok(status)
    } else {
        Err(AppError::bad_request(
            "status must be pending, paid, failed or refunded",
        ))
    }
}

async fn upsert_invoice_status(
    pool: &PgPool,
    tenant_id: Uuid,
    month: &str,
    provider: &str,
    external_id: &str,
    status: &str,
    amount_cents: i64,
    paid_at: Option<DateTime<Utc>>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO tenant_invoice_status
             (tenant_id, month, provider, external_id, status, amount_cents, paid_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())
         ON CONFLICT (tenant_id, month) DO UPDATE
            SET provider = EXCLUDED.provider,
                external_id = EXCLUDED.external_id,
                status = EXCLUDED.status,
                amount_cents = EXCLUDED.amount_cents,
                paid_at = EXCLUDED.paid_at,
                updated_at = now()",
    )
    .bind(tenant_id)
    .bind(month)
    .bind(provider)
    .bind(external_id)
    .bind(status)
    .bind(amount_cents)
    .bind(paid_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn billing_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let secret = state.cfg.billing_webhook_secret.trim();
    if secret.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_webhook_disabled",
            "set CHIRON_BILLING_WEBHOOK_SECRET to enable billing callbacks",
        ));
    }
    let signature = headers
        .get("x-chiron-signature")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !verify_billing_signature(secret, &body, signature) {
        return Err(AppError::unauthorized("invalid billing signature"));
    }
    let payload: BillingWebhookBody = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("invalid billing payload"))?;
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    fetch_tenant(&control.db, payload.tenant_id).await?;
    let (month, _, _) = bill_month(Some(&payload.month))?;
    let status = validate_invoice_status(&payload.status)?;
    if payload.amount_cents < 0 {
        return Err(AppError::bad_request("amount_cents must be >= 0"));
    }
    if payload.provider.chars().count() > 64 || payload.external_id.chars().count() > 128 {
        return Err(AppError::bad_request("provider/external_id is too long"));
    }
    upsert_invoice_status(
        &control.db,
        payload.tenant_id,
        &month,
        payload.provider.trim(),
        payload.external_id.trim(),
        &status,
        payload.amount_cents,
        payload.paid_at,
    )
    .await?;
    platform_audit(
        &state,
        None,
        "billing_webhook",
        "tenant_invoice_status",
        Some(payload.tenant_id),
        &format!("month={month} status={status}"),
        "",
    )
    .await;
    Ok(ok(json!({
        "tenant_id": payload.tenant_id,
        "month": month,
        "status": status,
    })))
}

async fn invoice_status(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    body: Result<Json<InvoiceStatusBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    fetch_tenant(&control.db, id).await?;
    let (month, _, _) = bill_month(Some(&body.month))?;
    let status = body.status.trim().to_ascii_lowercase();
    if !matches!(status.as_str(), "pending" | "paid" | "failed" | "refunded") {
        return Err(AppError::bad_request(
            "status must be pending, paid, failed or refunded",
        ));
    }
    if body.amount_cents < 0 {
        return Err(AppError::bad_request("amount_cents must be >= 0"));
    }
    if body.provider.chars().count() > 64 || body.external_id.chars().count() > 128 {
        return Err(AppError::bad_request(
            "provider/external_id is too long",
        ));
    }
    sqlx::query(
        "INSERT INTO tenant_invoice_status
             (tenant_id, month, provider, external_id, status, amount_cents, paid_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())
         ON CONFLICT (tenant_id, month) DO UPDATE
            SET provider = EXCLUDED.provider,
                external_id = EXCLUDED.external_id,
                status = EXCLUDED.status,
                amount_cents = EXCLUDED.amount_cents,
                paid_at = EXCLUDED.paid_at,
                updated_at = now()",
    )
    .bind(id)
    .bind(&month)
    .bind(body.provider.trim())
    .bind(body.external_id.trim())
    .bind(&status)
    .bind(body.amount_cents)
    .bind(body.paid_at)
    .execute(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_invoice_status",
        Some(id),
        &format!("month={month} status={status}"),
        "",
    )
    .await;
    Ok(ok(json!({
        "tenant_id": id,
        "month": month,
        "status": status,
        "provider": body.provider.trim(),
        "external_id": body.external_id.trim(),
        "amount_cents": body.amount_cents,
        "paid_at": body.paid_at,
    })))
}

#[derive(Debug, Deserialize)]
struct InvoicePayBody {
    #[serde(default)]
    month: Option<String>,
}

async fn invoice_total_cents(
    state: &AppState,
    tenant: &TenantRow,
    id: Uuid,
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
) -> Result<i64, AppError> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let price: i32 = sqlx::query_scalar("SELECT price_cents FROM plans WHERE id = $1")
        .bind(&tenant.plan_id)
        .fetch_one(&control.db)
        .await?;
    let usage = bill_totals(&bill_rows(state, id, start, end).await?).3;
    Ok(i64::from(price).saturating_add(usage))
}

async fn alipay_pay(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    body: Result<Json<InvoicePayBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let alipay_cfg = settings::load_alipay(&state).await?;
    if !alipay_cfg.configured() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "alipay_disabled",
            "configure Alipay in the platform console or ALIPAY_* env vars",
        ));
    }
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let (month, start, end) = bill_month(body.month.as_deref())?;
    let current: Option<String> = sqlx::query_scalar(
        "SELECT status FROM tenant_invoice_status WHERE tenant_id = $1 AND month = $2",
    )
    .bind(id)
    .bind(&month)
    .fetch_optional(&control.db)
    .await?;
    if current.as_deref() == Some("paid") {
        return Err(AppError::conflict("invoice is already paid"));
    }
    let total = invoice_total_cents(&state, &tenant, id, start, end).await?;
    if total <= 0 {
        return Err(AppError::bad_request("invoice total must be positive"));
    }
    let out_trade_no = alipay::out_trade_no(id, &month);
    let subject = format!("Chiron {} {}", tenant.slug, month);
    let pay_url = alipay::page_pay_url(
        &alipay_cfg.app_id,
        &alipay_cfg.private_key,
        &alipay_cfg.gateway,
        &alipay_cfg.notify_url,
        &alipay_cfg.return_url,
        &out_trade_no,
        total,
        &subject,
    )
    .map_err(|e| AppError::internal(format!("alipay sign failed: {e}")))?;
    upsert_invoice_status(
        &control.db,
        id,
        &month,
        "alipay",
        &out_trade_no,
        "pending",
        total,
        None,
    )
    .await?;
    Ok(ok(json!({
        "tenant_id": id,
        "month": month,
        "out_trade_no": out_trade_no,
        "total_cents": total,
        "pay_url": pay_url,
    })))
}

async fn alipay_notify(
    State(state): State<Arc<AppState>>,
    Form(params): Form<BTreeMap<String, String>>,
) -> ApiResult<Response> {
    let alipay_cfg = settings::load_alipay(&state).await?;
    if !alipay_cfg.configured() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "alipay_disabled",
            "alipay is not configured",
        ));
    }
    if !alipay::verify_notify(&alipay_cfg.public_key, &params) {
        return Err(AppError::unauthorized("invalid alipay signature"));
    }
    if params.get("app_id").map(String::as_str) != Some(alipay_cfg.app_id.trim()) {
        return Err(AppError::unauthorized("alipay app_id mismatch"));
    }
    let out_trade_no = params
        .get("out_trade_no")
        .ok_or_else(|| AppError::bad_request("missing out_trade_no"))?;
    let (tenant_id, month) = alipay::parse_out_trade_no(out_trade_no)
        .ok_or_else(|| AppError::bad_request("invalid out_trade_no"))?;
    let trade_status = params
        .get("trade_status")
        .ok_or_else(|| AppError::bad_request("missing trade_status"))?;
    let status = alipay::notify_status(trade_status)
        .ok_or_else(|| AppError::bad_request("unknown trade_status"))?;
    let amount = alipay::yuan_to_cents(
        params
            .get("total_amount")
            .map(String::as_str)
            .unwrap_or_default(),
    )
    .ok_or_else(|| AppError::bad_request("invalid total_amount"))?;
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, tenant_id).await?;
    let (month, start, end) = bill_month(Some(&month))?;
    let stored: Option<i64> = sqlx::query_scalar(
        "SELECT amount_cents FROM tenant_invoice_status WHERE tenant_id = $1 AND month = $2",
    )
    .bind(tenant_id)
    .bind(&month)
    .fetch_optional(&control.db)
    .await?;
    // 以创建支付单时的金额为准，避免支付期间 rollup 变化导致合法通知被拒。
    let expected = match stored {
        Some(amount) if amount > 0 => amount,
        _ => invoice_total_cents(&state, &tenant, tenant_id, start, end).await?,
    };
    if amount != expected {
        return Err(AppError::bad_request("alipay amount does not match invoice"));
    }
    let trade_no = params
        .get("trade_no")
        .map(String::as_str)
        .unwrap_or(out_trade_no);
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT status FROM tenant_invoice_status WHERE tenant_id = $1 AND month = $2",
    )
    .bind(tenant_id)
    .bind(&month)
    .fetch_optional(&control.db)
    .await?;
    if existing.as_deref() == Some("refunded")
        || (existing.as_deref() == Some("paid") && matches!(status, "pending" | "failed"))
    {
        // 乱序/重复通知不能把已支付或已退款状态降级。
        return Ok((StatusCode::OK, "success").into_response());
    }
    upsert_invoice_status(
        &control.db,
        tenant_id,
        &month,
        "alipay",
        trade_no,
        status,
        amount,
        (status == "paid").then(Utc::now),
    )
    .await?;
    platform_audit(
        &state,
        None,
        "alipay",
        "tenant_invoice_status",
        Some(tenant_id),
        &format!("month={month} status={status} trade_no={trade_no}"),
        "",
    )
    .await;
    Ok((StatusCode::OK, "success").into_response())
}

async fn regions(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT region, count(*)::bigint,
                count(*) FILTER (WHERE status = 'active')::bigint
           FROM tenants WHERE status <> 'deleted'
          GROUP BY region ORDER BY region ASC",
    )
    .fetch_all(&control.db)
    .await?;
    let local = state.cfg.region.trim().to_string();
    let unbound: i64 = rows
        .iter()
        .filter(|(region, ..)| region.trim().is_empty() || region.eq_ignore_ascii_case("default"))
        .map(|(_, all, _)| *all)
        .sum();
    let redis = match state.redis() {
        Some(handle) => Some(handle.ping().await),
        None => None,
    };
    Ok(ok(json!({
        "local_region": local,
        "control_ok": true,
        "redis": redis,
        "unbound_tenants": unbound,
        "warning": (!local.is_empty() && unbound > 0)
            .then_some("tenants without an explicit region can be served by any region"),
        "regions": rows.into_iter().map(|(region, total, active)| json!({
            "region": region,
            "tenants": total,
            "active": active,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
struct RegionBackfillBody {
    region: String,
    #[serde(default)]
    dry_run: bool,
}

fn valid_region(raw: &str) -> bool {
    let value = raw.trim();
    !value.is_empty()
        && !value.eq_ignore_ascii_case("default")
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

async fn region_backfill(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    body: Result<Json<RegionBackfillBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    if !valid_region(&body.region) {
        return Err(AppError::bad_request(
            "region must be 1-64 chars of [A-Za-z0-9._-]",
        ));
    }
    let region = body.region.trim().to_string();
    let candidates: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, slug FROM tenants
          WHERE status <> 'deleted'
            AND (btrim(region) = '' OR lower(region) = 'default')
          ORDER BY slug ASC",
    )
    .fetch_all(&control.db)
    .await?;
    let mut updated = 0usize;
    if !body.dry_run && !candidates.is_empty() {
        updated = sqlx::query(
            "UPDATE tenants SET region = $1, updated_at = now()
              WHERE status <> 'deleted'
                AND (btrim(region) = '' OR lower(region) = 'default')",
        )
        .bind(&region)
        .execute(&control.db)
        .await?
        .rows_affected() as usize;
        platform_audit(
            &state,
            Some(admin.id),
            &admin.email,
            "region_backfilled",
            None,
            &format!("region={region} tenants={updated}"),
            "",
        )
        .await;
    }
    Ok(ok(json!({
        "region": region,
        "dry_run": body.dry_run,
        "candidates": candidates.iter().map(|(id, slug)| json!({ "id": id, "slug": slug })).collect::<Vec<_>>(),
        "candidate_count": candidates.len(),
        "updated": updated,
    })))
}

async fn audit(State(state): State<Arc<AppState>>, _admin: PlatformAdmin) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let rows = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            Option<Uuid>,
            String,
            String,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, actor_email, action, tenant_id, detail, ip, created_at
           FROM platform_audit ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&control.db)
    .await?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, email, action, tid, detail, ip, at)| {
            json!({
                "id": id,
                "actor_email": email,
                "action": action,
                "tenant_id": tid,
                "detail": detail,
                "ip": ip,
                "created_at": at,
            })
        })
        .collect();
    Ok(ok(json!({ "audit": items })))
}

fn control_unavailable() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

/// 租户级互斥：迁移、备份、物理删除共用同一把事务级 advisory lock。
pub(crate) fn tenant_op_lock_key(id: Uuid) -> String {
    format!("chiron:tenant-op:{id}")
}

pub(crate) async fn try_tenant_op_lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<bool, AppError> {
    let key = tenant_op_lock_key(id);
    Ok(sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtext($1))")
        .bind(key)
        .fetch_one(&mut **tx)
        .await?)
}

pub(crate) async fn fetch_tenant(pool: &PgPool, id: Uuid) -> Result<TenantRow, AppError> {
    sqlx::query_as::<_, TenantRow>(
        "SELECT id, slug, name, status, plan_id, region, owner_email, db_name,
                provision_error, provisioned_at, created_at, updated_at
           FROM tenants WHERE id = $1 AND status <> 'deleted'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("tenant not found"))
}

#[allow(clippy::too_many_arguments)]
async fn platform_audit(
    state: &AppState,
    actor_id: Option<Uuid>,
    actor_email: &str,
    action: &str,
    tenant_id: Option<Uuid>,
    detail: &str,
    ip: &str,
) {
    let Some(control) = state.control.as_ref() else {
        return;
    };
    let _ = sqlx::query(
        "INSERT INTO platform_audit (actor_id, actor_email, action, tenant_id, detail, ip)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(actor_id)
    .bind(actor_email)
    .bind(action)
    .bind(tenant_id)
    .bind(detail)
    .bind(ip)
    .execute(&control.db)
    .await;
}

/// 启动时引导平台管理员：仅当控制库还没有管理员且 env 提供了凭据。
pub async fn bootstrap_admin(state: &AppState, control: &ControlState) {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM platform_admins")
        .fetch_one(&control.db)
        .await
        .unwrap_or(0);
    if count > 0 {
        return;
    }
    let email = state
        .cfg
        .platform_bootstrap_email
        .trim()
        .to_ascii_lowercase();
    let password = state.cfg.platform_bootstrap_password.as_str();
    if email.is_empty() || password.is_empty() {
        tracing::warn!(
            "no platform admin exists; set PLATFORM_BOOTSTRAP_EMAIL/PASSWORD to bootstrap"
        );
        return;
    }
    if let Err(msg) = validate_password_complexity(password) {
        tracing::error!("PLATFORM_BOOTSTRAP_PASSWORD rejected: {msg}");
        return;
    }
    let hash = match hash_password(password) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "platform admin bootstrap hash failed");
            return;
        }
    };
    match sqlx::query("INSERT INTO platform_admins (email, password_hash) VALUES ($1, $2)")
        .bind(&email)
        .bind(hash)
        .execute(&control.db)
        .await
    {
        Ok(_) => tracing::info!(email = %email, "platform admin bootstrapped"),
        Err(e) => tracing::error!(error = %e, "platform admin bootstrap failed"),
    }
}

/// 可选：平台 API 的 `Host` 校验辅助（单测用）。
pub fn host_matches(expected: &str, host_header: &str) -> bool {
    let host = host_header
        .split(':')
        .next()
        .unwrap_or(host_header)
        .to_ascii_lowercase();
    if expected.trim().is_empty() {
        matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "")
    } else {
        host == expected.trim().to_ascii_lowercase()
    }
}

/// AAD 绑定管理员 id：密文换到别的管理员行会解密失败。
fn totp_aad(admin_id: Uuid) -> String {
    format!("platform_totp:{admin_id}")
}

fn totp_unavailable(msg: String) -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, "totp_unavailable", msg)
}

/// 解密落库的 pending/active 密钥，返回原始 secret 字节。
fn open_totp_secret(state: &AppState, admin_id: Uuid, enc: &str) -> Result<Vec<u8>, AppError> {
    let key = state.cfg.secrets_key.trim();
    crypto::decode_key(key).map_err(totp_unavailable)?;
    let encoded =
        crypto::open(key, totp_aad(admin_id).as_bytes(), enc).map_err(|msg| {
            AppError::internal(format!("decrypt platform totp secret: {msg}"))
        })?;
    totp::base32_decode(&encoded)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| AppError::internal("stored platform totp secret is not valid base32"))
}

fn invalid_totp() -> AppError {
    AppError::new(
        StatusCode::BAD_REQUEST,
        "auth.totp_invalid",
        "invalid totp code",
    )
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `otpauth://` Key URI（RFC 6238 附录）；label 里的 `:` 需保持字面量。
fn otpauth_url(email: &str, secret: &str) -> String {
    let issuer = "Chiron Platform";
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period=30",
        percent_encode(issuer),
        percent_encode(email),
        secret,
        percent_encode(issuer)
    )
}

/// 生成 pending secret 并加密落库；不改变 `totp_enabled`。
async fn totp_setup(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let enabled: bool =
        sqlx::query_scalar("SELECT totp_enabled FROM platform_admins WHERE id = $1")
            .bind(admin.id)
            .fetch_one(&control.db)
            .await?;
    if enabled {
        return Err(AppError::conflict(
            "totp is already enabled; disable it before enrolling again",
        ));
    }
    let key = state.cfg.secrets_key.trim().to_string();
    crypto::decode_key(&key).map_err(totp_unavailable)?;
    let secret = totp::random_secret();
    let sealed = crypto::seal(&key, totp_aad(admin.id).as_bytes(), &secret)
        .map_err(|msg| AppError::internal(format!("encrypt platform totp secret: {msg}")))?;
    sqlx::query(
        "UPDATE platform_admins SET totp_secret_enc = $1, updated_at = now() WHERE id = $2",
    )
    .bind(&sealed)
    .bind(admin.id)
    .execute(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "platform_totp_setup",
        None,
        "",
        "",
    )
    .await;
    // 唯一一次明文返回 secret。
    Ok(ok(json!({
        "secret": secret,
        "otpauth_url": otpauth_url(&admin.email, &secret),
    })))
}

#[derive(Debug, Deserialize)]
struct TotpCodeBody {
    #[serde(default)]
    code: String,
}

async fn totp_enable(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<TotpCodeBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let row: Option<(bool, Option<String>)> = sqlx::query_as(
        "SELECT totp_enabled, totp_secret_enc FROM platform_admins WHERE id = $1",
    )
    .bind(admin.id)
    .fetch_optional(&control.db)
    .await?;
    let (enabled, enc) = row.ok_or_else(|| AppError::not_found("platform admin not found"))?;
    if enabled {
        return Err(AppError::conflict("totp is already enabled"));
    }
    let enc = enc.ok_or_else(|| {
        AppError::bad_request("no pending totp setup; call /api/platform/v1/me/totp/setup first")
    })?;
    let secret = open_totp_secret(&state, admin.id, &enc)?;
    if !totp::verify(&secret, &body.code, Utc::now().timestamp()) {
        return Err(invalid_totp());
    }
    sqlx::query(
        "UPDATE platform_admins SET totp_enabled = true, totp_last_step = NULL, \
             updated_at = now() WHERE id = $1",
    )
    .bind(admin.id)
    .execute(&control.db)
    .await?;
    let recovery_codes = generate_recovery_codes();
    sqlx::query("DELETE FROM platform_admin_recovery_codes WHERE admin_id = $1")
        .bind(admin.id)
        .execute(&control.db)
        .await?;
    let key = state.cfg.secrets_key.trim();
    for code in &recovery_codes {
        sqlx::query(
            "INSERT INTO platform_admin_recovery_codes (admin_id, code_hash) VALUES ($1, $2)",
        )
        .bind(admin.id)
        .bind(recovery_hash(key, code))
        .execute(&control.db)
        .await?;
    }
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "platform_totp_enabled",
        None,
        "",
        "",
    )
    .await;
    Ok(ok(json!({
        "totp_enabled": true,
        "recovery_codes": recovery_codes,
    })))
}

async fn totp_disable(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<TotpCodeBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let row: Option<(bool, Option<String>)> = sqlx::query_as(
        "SELECT totp_enabled, totp_secret_enc FROM platform_admins WHERE id = $1",
    )
    .bind(admin.id)
    .fetch_optional(&control.db)
    .await?;
    let (enabled, enc) = row.ok_or_else(|| AppError::not_found("platform admin not found"))?;
    if !enabled {
        return Err(AppError::conflict("totp is not enabled"));
    }
    let enc = enc.ok_or_else(|| AppError::internal("totp enabled but secret is missing"))?;
    let secret = open_totp_secret(&state, admin.id, &enc)?;
    if !totp::verify(&secret, &body.code, Utc::now().timestamp()) {
        return Err(invalid_totp());
    }
    sqlx::query(
        "UPDATE platform_admins
            SET totp_enabled = false, totp_secret_enc = NULL, totp_last_step = NULL,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(admin.id)
    .execute(&control.db)
    .await?;
    sqlx::query("DELETE FROM platform_admin_recovery_codes WHERE admin_id = $1")
        .bind(admin.id)
        .execute(&control.db)
        .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "platform_totp_disabled",
        None,
        "",
        "",
    )
    .await;
    Ok(ok(json!({ "totp_enabled": false })))
}

fn normalize_recovery_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn recovery_hash(key: &str, raw: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac key");
    mac.update(normalize_recovery_code(raw).as_bytes());
    hex(&mac.finalize().into_bytes())
}

fn generate_recovery_codes() -> Vec<String> {
    use rand::{distributions::Alphanumeric, Rng};
    let mut rng = rand::thread_rng();
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let raw: String = (&mut rng)
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>()
                .to_ascii_uppercase();
            format!("{}-{}", &raw[..5], &raw[5..])
        })
        .collect()
}

async fn consume_recovery_code(
    state: &AppState,
    db: &PgPool,
    admin_id: Uuid,
    provided: &str,
) -> bool {
    let key = state.cfg.secrets_key.trim();
    if key.is_empty() {
        return false;
    }
    let normalized = normalize_recovery_code(provided);
    if normalized.len() < 8 {
        return false;
    }
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, code_hash FROM platform_admin_recovery_codes \
         WHERE admin_id = $1 AND used_at IS NULL",
    )
    .bind(admin_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let wanted = recovery_hash(key, &normalized);
    for (id, stored) in rows {
        if !constant_time_eq(wanted.as_bytes(), stored.as_bytes()) {
            continue;
        }
        let updated = sqlx::query(
            "UPDATE platform_admin_recovery_codes SET used_at = now() \
             WHERE id = $1 AND used_at IS NULL",
        )
        .bind(id)
        .execute(db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
        if updated == 1 {
            platform_audit(
                state,
                Some(admin_id),
                "",
                "platform_login_recovery_used",
                None,
                "",
                "",
            )
            .await;
            return true;
        }
    }
    false
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// RFC 6238 TOTP（SHA1 / 30s / 6 位，±1 步容错），仅平台管理员二次验证使用。
mod totp {
    use hmac::{Hmac, Mac};
    use rand::RngCore;
    use sha1::Sha1;

    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    const STEP_SECS: i64 = 30;
    const DIGITS: u32 = 6;
    /// 20 字节（160 bit）密钥，Base32 后 32 字符，兼容主流验证器。
    const SECRET_BYTES: usize = 20;

    pub(super) fn random_secret() -> String {
        let mut raw = [0u8; SECRET_BYTES];
        rand::thread_rng().fill_bytes(&mut raw);
        base32_encode(&raw)
    }

    pub(super) fn base32_encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
        let mut buffer: u32 = 0;
        let mut bits: u32 = 0;
        for &byte in data {
            buffer = (buffer << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
        }
        out
    }

    /// 容忍大小写、空白与 `=` 填充；遇到非法字符返回 `None`。
    pub(super) fn base32_decode(text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len() * 5 / 8);
        let mut buffer: u32 = 0;
        let mut bits: u32 = 0;
        for ch in text.chars() {
            if ch == '=' || ch.is_whitespace() || ch == '-' {
                continue;
            }
            let upper = ch.to_ascii_uppercase();
            let value = ALPHABET
                .iter()
                .position(|&c| char::from(c) == upper)? as u32;
            buffer = (buffer << 5) | value;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push(((buffer >> bits) & 0xff) as u8);
            }
        }
        Some(out)
    }

    /// RFC 4226 HOTP：`HMAC-SHA1(counter)` 动态截断后取 31 位。
    fn hotp(secret: &[u8], counter: u64) -> u32 {
        let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("hmac accepts any key length");
        mac.update(&counter.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let offset = (digest[19] & 0x0f) as usize;
        (u32::from(digest[offset] & 0x7f) << 24)
            | (u32::from(digest[offset + 1]) << 16)
            | (u32::from(digest[offset + 2]) << 8)
            | u32::from(digest[offset + 3])
    }

    pub(super) fn code_at(secret: &[u8], unix: i64, digits: u32) -> String {
        let counter = unix.max(0) as u64 / STEP_SECS as u64;
        let modulo = 10u32.pow(digits);
        let value = hotp(secret, counter) % modulo;
        format!("{value:0width$}", width = digits as usize)
    }

    /// 接受当前步与前后各一步（时钟漂移容错）。
    pub(super) fn verify(secret: &[u8], code: &str, unix: i64) -> bool {
        verify_step(secret, code, unix).is_some()
    }

    /// 返回命中的时间步，供调用方持久化以拒绝重放。
    pub(super) fn verify_step(secret: &[u8], code: &str, unix: i64) -> Option<i64> {
        let code = code.trim();
        if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        (-1..=1i64).find_map(|delta| {
            let at = unix + delta * STEP_SECS;
            if at >= 0 && constant_time_eq(code.as_bytes(), code_at(secret, at, DIGITS).as_bytes())
            {
                Some(at / STEP_SECS)
            } else {
                None
            }
        })
    }

    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// RFC 6238 附录 B 的 SHA1 测试向量（密钥为 ASCII "12345678901234567890"）。
        #[test]
        fn rfc6238_sha1_vectors() {
            let secret = b"12345678901234567890";
            assert_eq!(code_at(secret, 59, 8), "94287082");
            assert_eq!(code_at(secret, 1_111_111_109, 8), "07081804");
            assert_eq!(code_at(secret, 1_111_111_111, 8), "14050471");
            assert_eq!(code_at(secret, 1_234_567_890, 8), "89005924");
            assert_eq!(code_at(secret, 2_000_000_000, 8), "69279037");
            assert_eq!(code_at(secret, 20_000_000_000, 8), "65353130");
            // 同密钥 6 位：RFC 4226 计数器 0 的 HOTP 值为 755224。
            assert_eq!(code_at(secret, 0, 6), "755224");
        }

        #[test]
        fn base32_roundtrip_and_rfc4648_vector() {
            // RFC 4648 "foobar" -> MZXW6YTBOI（无填充）。
            assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
            assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");

            for raw in [
                vec![],
                vec![0u8],
                b"12345678901234567890".to_vec(),
                (0u8..=255).collect::<Vec<_>>(),
            ] {
                let encoded = base32_encode(&raw);
                assert_eq!(base32_decode(&encoded).unwrap(), raw, "encoded={encoded}");
            }
            assert!(base32_decode("MZXW6!").is_none());
            // 生成器：32 字符 Base32，解回 20 字节，且带随机性。
            let a = random_secret();
            let b = random_secret();
            assert_eq!(a.len(), 32);
            assert_eq!(base32_decode(&a).unwrap().len(), 20);
            assert_ne!(a, b);
        }

        #[test]
        fn accepts_one_step_of_drift_only() {
            let secret = base32_decode("MZXW6YTBOI").unwrap();
            let now = 1_700_000_010i64;
            let code = code_at(&secret, now, DIGITS);
            assert!(verify(&secret, &code, now));
            assert!(verify(&secret, &code, now + STEP_SECS));
            assert!(verify(&secret, &code, now - STEP_SECS));
            assert!(!verify(&secret, &code, now + 2 * STEP_SECS));
            assert!(!verify(&secret, &code, now - 2 * STEP_SECS));
        }

        #[test]
        fn rejects_wrong_or_malformed_codes() {
            let secret = base32_decode("MZXW6YTBOI").unwrap();
            let now = 1_700_000_010i64;
            let good = code_at(&secret, now, DIGITS);
            let wrong = if good == "000000" { "000001" } else { "000000" };
            assert!(!verify(&secret, wrong, now));
            assert!(!verify(&secret, "", now));
            assert!(!verify(&secret, "12345", now));
            assert!(!verify(&secret, "1234567", now));
            assert!(!verify(&secret, "abcdef", now));
            // 其他密钥生成的码必须被拒绝。
            let other = base32_decode("ONSWG4TFOQ").unwrap();
            assert!(!verify(&other, &good, now));
            // 空白包裹但时间窗内仍可接受。
            assert!(verify(&secret, &format!(" {good} "), now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_backfill_region_is_safe() {
        assert!(valid_region("cn-east"));
        assert!(valid_region("cn-east-2.prod"));
        assert!(!valid_region(""));
        assert!(!valid_region("cn east"));
        assert!(!valid_region("../etc"));
        assert!(!valid_region("default"));
        assert!(!valid_region(&"x".repeat(65)));
    }

    #[test]
    fn billing_webhook_signature_roundtrip() {
        let body = br#"{"tenant_id":"x"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(verify_billing_signature(
            "secret",
            body,
            &format!("sha256={hex}")
        ));
        assert!(verify_billing_signature("secret", body, &hex));
        assert!(!verify_billing_signature("other", body, &hex));
        assert!(!verify_billing_signature("secret", b"tampered", &hex));
    }

    #[test]
    fn slug_validation() {
        assert!(valid_slug("acme"));
        assert!(valid_slug("acme-team-1"));
        assert!(!valid_slug("-acme"));
        assert!(!valid_slug("acme-"));
        assert!(!valid_slug("ACME"));
        assert!(!valid_slug("ab"));
        assert!(!valid_slug("acme_team"));
    }

    #[test]
    fn host_guard_matches_platform_or_loopback() {
        assert!(host_matches(
            "platform.example.com",
            "platform.example.com:443"
        ));
        assert!(!host_matches("platform.example.com", "acme.example.com"));
        assert!(host_matches("", "127.0.0.1:8088"));
        assert!(host_matches("", "localhost"));
        assert!(!host_matches("", "acme.example.com"));
    }

    #[test]
    fn email_validation_for_owner() {
        assert!(valid_email("owner@acme.com"));
        assert!(valid_email("a.b+c@sub.acme.io"));
        assert!(!valid_email(""));
        assert!(!valid_email("owner"));
        assert!(!valid_email("@acme.com"));
        assert!(!valid_email("owner@acme"));
        assert!(!valid_email("owner@acme."));
        assert!(!valid_email("own er@acme.com"));
        assert!(!valid_email("a@b.com,c@d.com"));
    }

    #[test]
    fn recovery_codes_are_formatted_and_hashes_normalize_input() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        assert!(codes.iter().all(|code| code.len() == 11 && code.as_bytes()[5] == b'-'));
        assert_eq!(
            recovery_hash("secret", "ABCDE-FGHIJ"),
            recovery_hash("secret", "abcde fghij")
        );
        assert_ne!(
            recovery_hash("secret", "ABCDE-FGHIJ"),
            recovery_hash("other", "ABCDE-FGHIJ")
        );
    }

    #[test]
    fn bill_month_range_handles_leap_and_year_end() {
        let (label, start, end) = bill_month(Some("2024-02")).unwrap();
        assert_eq!(label, "2024-02");
        assert_eq!(start.to_string(), "2024-02-01");
        assert_eq!(end.to_string(), "2024-02-29");
        let (_, start, end) = bill_month(Some("2026-12")).unwrap();
        assert_eq!(start.to_string(), "2026-12-01");
        assert_eq!(end.to_string(), "2026-12-31");
        assert!(bill_month(Some("2026-13")).is_err());
        assert!(bill_month(Some("2026/01")).is_err());
    }

    #[test]
    fn bill_totals_sum_daily_rows() {
        let rows = vec![
            (chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 2, 10, 20, 3, 100),
            (chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 1, 5, 7, 4, 50),
        ];
        assert_eq!(bill_totals(&rows), (3, 15, 27, 7, 150));
    }

    #[test]
    fn usage_csv_fields_are_escaped() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn platform_login_locks_after_repeated_failures() {
        let email = "lock-test@example.com";
        clear_login_failures(email);
        for _ in 0..LOGIN_MAX_FAILURES {
            record_login_failure(email);
        }
        assert!(login_locked(email).is_some());
        clear_login_failures(email);
        assert!(login_locked(email).is_none());
    }
}
