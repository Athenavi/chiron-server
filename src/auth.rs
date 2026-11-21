use std::sync::{Arc, OnceLock};

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
use axum::{
    extract::{Request, State},
    http::header::{AUTHORIZATION, COOKIE},
    http::{HeaderMap, HeaderValue},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{seq::SliceRandom, Rng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

const COOKIE_NAME: &str = "chiron_token";
const BLACKLIST_PREFIX: &str = "jwt:blacklist:";
const VERSION_PREFIX: &str = "jwt:sess_ver:";
const EPOCH_TTL_SECONDS: u64 = 90_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub role: String,
    pub tid: i16,
    /// 沿用 Go 网关的 claim 名：true 表示 must_change_password（首登/重置后未改密）。
    /// 改名会破坏已经签发/联调中的 token，因此保留 `mcp` 这个名字。
    pub mcp: bool,
    /// 会话版本：撤销时 Redis 自增；token 版本低于当前值即失效。
    #[serde(default)]
    pub sv: u64,
    /// S2：控制库租户 id；SaaS 形态必须与请求 Host 解析出的租户一致。
    #[serde(default)]
    pub tenant: Option<Uuid>,
    pub jti: String,
    pub iat: usize,
    pub exp: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub tenant_id: i16,
    /// 控制库租户 id（SaaS）；会话版本等 Redis key 用它做租户前缀。
    pub tenant: Option<Uuid>,
    /// 套餐上限（SaaS）；单库形态为 None（不限额）。
    pub plan: Option<crate::tenant::PlanLimits>,
    pub must_change: bool,
    pub jti: String,
    pub iat: usize,
    pub exp: usize,
}

pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| {
            tracing::error!(error = %e, "password hash failed");
            AppError::internal("password hashing failed")
        })
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// 账号不存在/禁用时也跑一次 argon2，避免用响应时间做账号枚举。
pub fn burn_password_time(password: &str) {
    static DUMMY_HASH: OnceLock<String> = OnceLock::new();
    let hash =
        DUMMY_HASH.get_or_init(|| hash_password(&Uuid::new_v4().to_string()).unwrap_or_default());
    if !hash.is_empty() {
        let _ = verify_password(hash, password);
    }
}

/// 与 Go 网关一致的复杂度：8-128 位，且包含大小写/数字/特殊字符各至少一个。
pub fn validate_password_complexity(password: &str) -> Result<(), String> {
    let len = password.chars().count();
    if len < 8 {
        return Err("password must be at least 8 characters".into());
    }
    if len > 128 {
        return Err("password must be at most 128 characters".into());
    }
    let has_upper = password.chars().any(|c| c.is_uppercase());
    let has_lower = password.chars().any(|c| c.is_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_special = password.chars().any(|c| !c.is_alphanumeric());
    if !(has_upper && has_lower && has_digit && has_special) {
        return Err("password must contain upper/lowercase letters, a digit and a symbol".into());
    }
    Ok(())
}

pub fn generate_temp_password() -> String {
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const DIGITS: &[u8] = b"23456789";
    const SYMBOLS: &[u8] = b"!@#$%^&*";
    const ALL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^&*";

    let mut rng = rand::thread_rng();
    let mut out: Vec<u8> = vec![
        UPPER[rng.gen_range(0..UPPER.len())],
        LOWER[rng.gen_range(0..LOWER.len())],
        DIGITS[rng.gen_range(0..DIGITS.len())],
        SYMBOLS[rng.gen_range(0..SYMBOLS.len())],
    ];
    while out.len() < 18 {
        out.push(ALL[rng.gen_range(0..ALL.len())]);
    }
    out.shuffle(&mut rng);
    String::from_utf8(out).unwrap_or_else(|_| "ChangeMe!2345678Aa".to_string())
}

pub async fn issue_token(
    state: &AppState,
    user_id: Uuid,
    email: &str,
    role: &str,
    tenant_id: i16,
    tenant: Option<Uuid>,
    must_change: bool,
) -> Result<(String, Claims), AppError> {
    let sv = session_version(state, &user_id.to_string(), tenant).await?;
    let now = Utc::now().timestamp() as usize;
    let exp = now + (state.cfg.jwt_hours.max(1) as usize) * 3600;
    let claims = Claims {
        sub: user_id.to_string(),
        email: email.to_string(),
        role: role.to_string(),
        tid: tenant_id,
        mcp: must_change,
        sv,
        tenant,
        jti: Uuid::new_v4().to_string(),
        iat: now,
        exp,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| {
        tracing::error!(error = %e, "jwt encode failed");
        AppError::internal("token issue failed")
    })?;
    Ok((token, claims))
}

pub fn decode_token(state: &AppState, token: &str) -> Result<Claims, AppError> {
    let validation = Validation::new(Algorithm::HS256);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| AppError::unauthorized("invalid or expired token"))
}

fn extract_bearer_or_cookie(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for part in v.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

pub fn token_cookie(state: &AppState, token: &str) -> HeaderValue {
    let max_age = state.cfg.jwt_hours * 3600;
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("chiron_token=deleted; Path=/; Max-Age=0"))
}

pub fn clear_cookie(state: &AppState) -> HeaderValue {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("chiron_token=deleted; Path=/; Max-Age=0"))
}

async fn redis_get_int(state: &AppState, key: &str) -> Result<Option<i64>, AppError> {
    let Some(handle) = state.redis() else {
        return Ok(None);
    };
    let mut conn = handle.conn().await;
    let raw: Option<String> = redis::cmd("GET").arg(key).query_async(&mut conn).await?;
    Ok(raw.and_then(|v| v.parse::<i64>().ok()))
}

pub async fn is_blacklisted(state: &AppState, jti: &str) -> Result<bool, AppError> {
    let key = format!("{BLACKLIST_PREFIX}{jti}");
    Ok(redis_get_int(state, &key).await?.is_some())
}

pub async fn revoke_sessions(
    state: &AppState,
    user_id: Uuid,
    tenant: Option<Uuid>,
) -> Result<(), AppError> {
    let scope = version_scope(tenant);
    let Some(handle) = state.redis() else {
        // 单节点/桌面模式没有 Redis：用进程内版本号兜底，保证禁用/改密仍能踢下线。
        let mut map = memory_versions().lock().unwrap_or_else(|e| e.into_inner());
        *map.entry(format!("{scope}{user_id}")).or_insert(0) += 1;
        return Ok(());
    };
    let mut conn = handle.conn().await;
    let key = format!("{VERSION_PREFIX}{scope}{user_id}");
    let _: i64 = redis::cmd("INCR").arg(&key).query_async(&mut conn).await?;
    // 版本键必须活得比任何已签发 token 更久，否则过期后旧 token 会"复活"。
    let jwt_ttl = (state.cfg.jwt_hours.max(1) as u64) * 3600;
    let ttl = (jwt_ttl + 3600).max(EPOCH_TTL_SECONDS);
    let _: redis::Value = redis::cmd("EXPIRE")
        .arg(&key)
        .arg(ttl)
        .query_async(&mut conn)
        .await?;
    Ok(())
}

/// 当前会话版本；无记录（从未撤销）= 0。
fn memory_versions() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static VERSIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    VERSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

async fn session_version(
    state: &AppState,
    user_id: &str,
    tenant: Option<Uuid>,
) -> Result<u64, AppError> {
    let scope = version_scope(tenant);
    if state.redis().is_none() {
        let map = memory_versions().lock().unwrap_or_else(|e| e.into_inner());
        return Ok(map.get(&format!("{scope}{user_id}")).copied().unwrap_or(0));
    }
    let key = format!("{VERSION_PREFIX}{scope}{user_id}");
    Ok(redis_get_int(state, &key).await?.unwrap_or(0).max(0) as u64)
}

/// 租户前缀，保证跨租户同名标识（与本地测试）互不影响。
fn version_scope(tenant: Option<Uuid>) -> String {
    tenant.map(|id| format!("{id}:")).unwrap_or_default()
}

/// token 的会话版本低于当前值 ⇒ 已撤销。
pub fn revoked_by_version(current: u64, token_sv: u64) -> bool {
    current > token_sv
}

pub async fn blacklist_token(state: &AppState, jti: &str, exp: usize) {
    let Some(handle) = state.redis() else {
        return;
    };
    let now = Utc::now().timestamp() as usize;
    let ttl = exp.saturating_sub(now).max(1) as u64;
    let mut conn = handle.conn().await;
    let key = format!("{BLACKLIST_PREFIX}{jti}");
    let _: Result<redis::Value, _> = redis::cmd("SET")
        .arg(key)
        .arg(1)
        .arg("EX")
        .arg(ttl)
        .query_async(&mut conn)
        .await;
}

/// 用户级纪元判定：纪元秒 > token 的 iat 即视为已吊销。
/// 同一秒内新签发的 token（改密后立即换发）仍然有效，这是有意的。
/// 单 token 黑名单 + 用户级纪元，两关都过才算有效。
pub async fn ensure_active(state: &AppState, claims: &Claims) -> Result<(), AppError> {
    if is_blacklisted(state, &claims.jti).await? {
        return Err(AppError::unauthorized("token has been revoked"));
    }
    if revoked_by_version(
        session_version(state, &claims.sub, claims.tenant).await?,
        claims.sv,
    ) {
        return Err(AppError::unauthorized("token has been revoked"));
    }
    Ok(())
}

/// `must_change_password` 期间只放行这四条（契约 §4）：
/// 改密、登出、读/改本人资料、自身会话。
fn allow_before_password_change(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/auth/password"
            | "/api/v1/auth/logout"
            | "/api/v1/auth/profile"
            | "/api/v1/auth/session"
    )
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = extract_bearer_or_cookie(req.headers())
        .ok_or_else(|| AppError::unauthorized("missing authorization"))?;
    let claims = decode_token(&state, &token)?;

    // Host 与 token 双校验：A 租户的 token 拿到 B 租户域名上必须 401。
    let host_tenant = req.extensions().get::<crate::tenant::TenantContext>();
    match host_tenant {
        Some(ctx) if claims.tenant == Some(ctx.id) => {}
        Some(_) => {
            return Err(AppError::unauthorized(
                "token does not belong to this tenant",
            ))
        }
        None => {
            if state.cfg.is_saas() {
                return Err(AppError::unauthorized("tenant not resolved"));
            }
        }
    }
    let plan = host_tenant.and_then(|ctx| ctx.plan.clone());

    let user_id =
        Uuid::parse_str(&claims.sub).map_err(|_| AppError::unauthorized("invalid token"))?;

    if is_blacklisted(&state, &claims.jti).await? {
        return Err(AppError::unauthorized("token has been revoked"));
    }
    if revoked_by_version(
        session_version(&state, &claims.sub, claims.tenant).await?,
        claims.sv,
    ) {
        return Err(AppError::unauthorized("token has been revoked"));
    }
    if claims.mcp && !allow_before_password_change(req.uri().path()) {
        return Err(AppError::password_change_required());
    }

    req.extensions_mut().insert(CurrentUser {
        id: user_id,
        email: claims.email.clone(),
        role: claims.role.clone(),
        tenant_id: claims.tid,
        tenant: claims.tenant,
        plan,
        must_change: claims.mcp,
        jti: claims.jti.clone(),
        iat: claims.iat,
        exp: claims.exp,
    });
    Ok(next.run(req).await)
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    extract_bearer_or_cookie(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_password_always_passes_policy() {
        for _ in 0..200 {
            let pwd = generate_temp_password();
            assert_eq!(pwd.chars().count(), 18);
            assert!(
                validate_password_complexity(&pwd).is_ok(),
                "weak temp password: {pwd}"
            );
        }
    }

    #[test]
    fn temporary_password_is_not_a_fixed_constant() {
        let a = generate_temp_password();
        let b = generate_temp_password();
        assert_ne!(a, b, "temp passwords must be random per call");
    }

    #[test]
    fn password_policy_rejects_weak_values() {
        assert!(validate_password_complexity("short").is_err());
        assert!(validate_password_complexity("alllowercase123!").is_err());
        assert!(validate_password_complexity("ALLUPPERCASE123!").is_err());
        assert!(validate_password_complexity("NoDigitsHere!").is_err());
        assert!(validate_password_complexity("NoSymbols123").is_err());
        assert!(validate_password_complexity("GoodPass1!").is_ok());
    }

    #[test]
    fn password_policy_boundaries() {
        assert!(
            validate_password_complexity("Aa1!aaaa").is_ok(),
            "exactly 8 chars is fine"
        );
        assert!(
            validate_password_complexity("Aa1!aaa").is_err(),
            "7 chars is too short"
        );
        let max = format!("Aa1!{}", "a".repeat(124));
        assert_eq!(max.chars().count(), 128);
        assert!(validate_password_complexity(&max).is_ok());
        let over = format!("Aa1!{}", "a".repeat(125));
        assert!(validate_password_complexity(&over).is_err());
        assert!(validate_password_complexity("").is_err());
        assert!(
            validate_password_complexity("Aa1!aaaa ").is_ok(),
            "trailing space counts as symbol"
        );
    }

    #[test]
    fn epoch_revokes_only_tokens_issued_before_revocation() {
        assert!(!revoked_by_version(0, 0));
        assert!(!revoked_by_version(1, 1), "刷新后的新 token 必须有效");
        assert!(revoked_by_version(1, 0));
    }

    /// 首登改密期间的放行列表必须与契约 §4 完全一致，多放行一条就是提权缺口。
    #[test]
    fn password_change_allowlist_matches_contract() {
        for path in [
            "/api/v1/auth/password",
            "/api/v1/auth/logout",
            "/api/v1/auth/profile",
            "/api/v1/auth/session",
        ] {
            assert!(allow_before_password_change(path), "{path} must be allowed");
        }
        for path in [
            "/api/v1/admin/users",
            "/api/v1/admin/users/00000000-0000-0000-0000-000000000000/disable",
            "/api/v1/sessions",
            "/api/v1/models",
            "/api/v1/auth/refresh",
            "/api/v1/auth/login",
            "/api/v1/auth/profile/extra",
        ] {
            assert!(
                !allow_before_password_change(path),
                "{path} must be blocked"
            );
        }
    }

    #[test]
    fn password_hash_roundtrip() {
        let hash = hash_password("GoodPass1!").expect("hash");
        assert!(verify_password(&hash, "GoodPass1!"));
        assert!(!verify_password(&hash, "GoodPass1?"));
        assert!(!verify_password("not-a-phc-hash", "GoodPass1!"));
    }
}
