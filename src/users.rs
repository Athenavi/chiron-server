use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{rejection::JsonRejection, ConnectInfo, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::*,
    error::{ApiResult, AppError},
    state::AppState,
    tenant::TenantContext,
};

/// 单租户默认租户；local-session 与匿名审计行用它。
const DEFAULT_TENANT: i16 = 1;
/// 登录失败锁定：5 次 / 15 分钟（docs/PRD.md）。
const LOGIN_MAX_FAILURES: i64 = 5;
const LOGIN_WINDOW_SECONDS: i64 = 900;
const NICKNAME_MAX_CHARS: usize = 64;
const EMAIL_MAX_CHARS: usize = 254;

#[derive(Debug, Clone, FromRow)]
struct UserRow {
    id: Uuid,
    tenant_id: i16,
    email: String,
    username: Option<String>,
    nickname: Option<String>,
    name: String,
    password_hash: String,
    password_set: bool,
    role: String,
    status: String,
    must_change_password: bool,
    last_login_at: Option<DateTime<Utc>>,
}

impl UserRow {
    fn public(&self) -> PublicUser {
        PublicUser {
            id: self.id,
            email: self.email.clone(),
            username: self.username.clone(),
            name: self.name.clone(),
            nickname: self
                .nickname
                .clone()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| self.name.clone()),
            role: self.role.clone(),
            status: self.status.clone(),
            must_change_password: self.must_change_password,
            password_set: self.password_set,
            last_login_at: self.last_login_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PublicUser {
    pub id: Uuid,
    pub email: String,
    pub username: Option<String>,
    pub name: String,
    pub nickname: String,
    pub role: String,
    pub status: String,
    pub must_change_password: bool,
    pub password_set: bool,
    pub last_login_at: Option<DateTime<Utc>>,
}

const USER_COLUMNS: &str = "id, tenant_id, email, username, nickname, name, password_hash, \
     password_set, role, status, must_change_password, last_login_at";

// ── 纯逻辑（无 IO，可单测）──────────────────────────────────────────────

/// 登录锁定的剩余等待秒数；未达阈值返回 None。
pub fn login_lock_ttl(failures: i64, ttl_seconds: i64) -> Option<i64> {
    if failures < LOGIN_MAX_FAILURES {
        None
    } else {
        Some(ttl_seconds.max(1))
    }
}

pub fn is_admin_role(role: &str) -> bool {
    role == "owner" || role == "admin"
}

fn valid_email(v: &str) -> bool {
    let s = v.trim();
    if s.is_empty() || s.chars().count() > EMAIL_MAX_CHARS {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

fn valid_username(v: &str) -> bool {
    let len = v.chars().count();
    (3..=64).contains(&len)
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 空串表示"清除昵称"（落库为 NULL，展示时回落到 name）。
pub fn normalize_nickname(raw: &str) -> Result<Option<String>, AppError> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if t.chars().count() > NICKNAME_MAX_CHARS {
        return Err(AppError::bad_request(format!(
            "nickname must be at most {NICKNAME_MAX_CHARS} characters"
        )));
    }
    if t.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "nickname must not contain control characters",
        ));
    }
    Ok(Some(t.to_string()))
}

pub fn normalize_display_name(raw: &str) -> Result<String, AppError> {
    match normalize_nickname(raw)? {
        Some(v) => Ok(v),
        None => Err(AppError::bad_request("name must not be empty")),
    }
}

/// 末位 owner 保护只对破坏性动作生效；enable 不能因为"最后一个 owner"被锁死。
pub fn needs_last_owner_guard(target_role: &str, action: &str) -> bool {
    target_role == "owner" && matches!(action, "disable" | "delete" | "demote")
}

/// 管理员操作目标的纯判定（不查库），末位 owner 计数由 ensure_can_manage 补。
pub fn manage_guard(
    actor_id: Uuid,
    actor_role: &str,
    target_id: Uuid,
    target_role: &str,
    target_status: &str,
    action: &str,
) -> Result<(), AppError> {
    if target_status == "deleted" {
        return Err(AppError::not_found("user not found"));
    }
    if target_role == "owner" && actor_role != "owner" {
        return Err(AppError::forbidden(
            "only the platform owner can manage an owner",
        ));
    }
    if target_role == "admin" && actor_role != "owner" && target_id != actor_id {
        return Err(AppError::forbidden(
            "only the platform owner can manage an admin",
        ));
    }
    if matches!(action, "disable" | "delete" | "reset_password" | "demote") && target_id == actor_id
    {
        return Err(AppError::forbidden(
            "cannot perform this action on your own account",
        ));
    }
    Ok(())
}

fn ok(data: Value) -> Response {
    // 与 docs/API.md 一致：成功只包一层 data。
    Json(json!({ "data": data })).into_response()
}

fn with_cookie(mut resp: Response, cookie: axum::http::HeaderValue) -> Response {
    resp.headers_mut()
        .append(axum::http::header::SET_COOKIE, cookie);
    resp
}

fn invalid_json(rej: JsonRejection) -> AppError {
    AppError::invalid_body(rej)
}

/// 路径 id 手动解析：axum 的 Path 失败信息是纯文本，会破坏统一错误包络。
fn parse_uuid(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw.trim()).map_err(|_| AppError::invalid_path_param())
}

#[derive(Debug, Deserialize)]
pub struct LoginBody {
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub username: String,
    pub password: String,
}

impl LoginBody {
    fn ident(&self) -> String {
        for v in [&self.identifier, &self.email, &self.username] {
            let s = v.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
        String::new()
    }
}

/// 登录失败计数要覆盖 ident 的所有别名（邮箱/用户名），否则换个写法就能绕过锁定。
fn ident_keys(ident: &str, user: &UserRow) -> Vec<String> {
    let mut keys = vec![ident.trim().to_lowercase()];
    for extra in [Some(user.email.as_str()), user.username.as_deref()] {
        let Some(raw) = extra else { continue };
        let key = raw.trim().to_lowercase();
        if !key.is_empty() && !keys.iter().any(|k| k == &key) {
            keys.push(key);
        }
    }
    keys
}

async fn record_login_failures(state: &AppState, scope: &str, keys: &[String]) {
    let Some(handle) = state.redis() else {
        return;
    };
    let mut conn = handle.conn().await;
    for key in keys {
        let redis_key = format!("login:fail:{scope}{key}");
        let n: i64 = redis::cmd("INCR")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await
            .unwrap_or(0);
        if n == 1 {
            let _: Result<redis::Value, _> = redis::cmd("EXPIRE")
                .arg(&redis_key)
                .arg(LOGIN_WINDOW_SECONDS)
                .query_async(&mut conn)
                .await;
        }
    }
}

async fn clear_login_failures(state: &AppState, scope: &str, keys: &[String]) {
    let Some(handle) = state.redis() else {
        return;
    };
    let mut conn = handle.conn().await;
    for key in keys {
        let redis_key = format!("login:fail:{scope}{key}");
        let _: Result<redis::Value, _> = redis::cmd("DEL")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await;
    }
}

async fn login_lock_remaining(state: &AppState, scope: &str, ident: &str) -> Option<i64> {
    let mut conn = state.redis()?.conn().await;
    let key = format!("login:fail:{scope}{}", ident.trim().to_lowercase());
    let count: Option<i64> = redis::cmd("GET")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .ok()?;
    // 阈值判定收敛到纯函数，避免"读 count 的地方"和"写 count 的地方"口径漂移。
    login_lock_ttl(count.unwrap_or(0), 0)?;
    let ttl: i64 = redis::cmd("TTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap_or(LOGIN_WINDOW_SECONDS);
    Some(ttl.max(1))
}

async fn audit(
    state: &AppState,
    tenant_id: i16,
    user_id: Option<Uuid>,
    action: &str,
    target: &str,
    detail: &str,
    ip: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO audit_logs (tenant_id, user_id, action, target, detail, ip)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(ip)
    .execute(&state.db())
    .await;
}

fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("")
        .to_string()
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    tenant: Option<Extension<TenantContext>>,
    body: Result<Json<LoginBody>, JsonRejection>,
) -> ApiResult<Response> {
    let body = body.map_err(invalid_json)?;
    let tenant_id = tenant.as_ref().map(|Extension(t)| t.id);
    if state.cfg.is_saas() && tenant_id.is_none() {
        return Err(AppError::not_found("unknown host"));
    }
    let ident = body.ident();
    if ident.is_empty() || body.password.is_empty() {
        return Err(AppError::bad_request(
            "email/username and password are required",
        ));
    }
    let ip = client_ip(&headers);
    let lock_scope = tenant_id.map(|id| format!("{id}:")).unwrap_or_default();
    if let Some(ttl) = login_lock_remaining(&state, &lock_scope, &ident).await {
        return Err(AppError::locked(ttl));
    }

    let row = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users
          WHERE lower(email) = lower($1)
             OR (username IS NOT NULL AND username <> '' AND lower(username) = lower($1))
          ORDER BY (tenant_id = 1) DESC, created_at ASC
          LIMIT 1"
    ))
    .bind(&ident)
    .fetch_optional(&state.db())
    .await?;

    let Some(user) = row else {
        // 账号不存在也要消耗一次 argon2，响应时间不泄露账号是否存在。
        burn_password_time(&body.password);
        let keys = vec![ident.trim().to_lowercase()];
        record_login_failures(&state, &lock_scope, &keys).await;
        audit(
            &state,
            DEFAULT_TENANT,
            None,
            "login_failed",
            "login",
            &format!(
                "reason=unknown_identifier identifier={}",
                mask_identifier(&ident)
            ),
            &ip,
        )
        .await;
        return Err(AppError::unauthorized("invalid email or password"));
    };
    if user.status != "active" {
        burn_password_time(&body.password);
        let keys = ident_keys(&ident, &user);
        record_login_failures(&state, &lock_scope, &keys).await;
        audit(
            &state,
            user.tenant_id,
            Some(user.id),
            "login_failed",
            "login",
            &format!("reason=inactive identifier={}", mask_identifier(&ident)),
            &ip,
        )
        .await;
        return Err(AppError::unauthorized("invalid email or password"));
    }
    if !verify_password(&user.password_hash, &body.password) {
        let keys = ident_keys(&ident, &user);
        record_login_failures(&state, &lock_scope, &keys).await;
        audit(
            &state,
            user.tenant_id,
            Some(user.id),
            "login_failed",
            "login",
            &format!("reason=bad_password identifier={}", mask_identifier(&ident)),
            &ip,
        )
        .await;
        return Err(AppError::unauthorized("invalid email or password"));
    }

    let keys = ident_keys(&ident, &user);
    clear_login_failures(&state, &lock_scope, &keys).await;
    let _ = sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
        .bind(user.id)
        .execute(&state.db())
        .await;

    let (token, _) = issue_token(
        &state,
        user.id,
        &user.email,
        &user.role,
        user.tenant_id,
        tenant_id,
        user.must_change_password,
    )
    .await?;
    audit(
        &state,
        user.tenant_id,
        Some(user.id),
        "login_success",
        "login",
        &format!("identifier={}", mask_identifier(&ident)),
        &ip,
    )
    .await;
    Ok(with_cookie(
        ok(json!({
            "token": token,
            "user": user.public(),
            "plan": tenant.as_ref().and_then(|Extension(t)| t.plan.as_ref()),
        })),
        token_cookie(&state, &token),
    ))
}

fn mask_identifier(v: &str) -> String {
    let s = v.trim();
    let first = s.chars().next().unwrap_or('*');
    match s.find('@') {
        Some(at) if at > 1 => format!("{first}***{}", &s[at..]),
        _ if s.chars().count() > 2 => format!("{first}***"),
        _ => "***".to_string(),
    }
}

pub async fn session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    tenant: Option<Extension<TenantContext>>,
) -> ApiResult<Response> {
    let token =
        token_from_headers(&headers).ok_or_else(|| AppError::unauthorized("not authenticated"))?;
    let claims = decode_token(&state, &token)?;
    match tenant.as_ref().map(|Extension(t)| t.id) {
        Some(id) if claims.tenant == Some(id) => {}
        Some(_) => {
            return Err(AppError::unauthorized(
                "token does not belong to this tenant",
            ))
        }
        None if state.cfg.is_saas() => return Err(AppError::not_found("unknown host")),
        None => {}
    }
    ensure_active(&state, &claims).await?;
    let user_id =
        Uuid::parse_str(&claims.sub).map_err(|_| AppError::unauthorized("invalid token"))?;

    let row = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(user_id)
    .bind(claims.tid)
    .fetch_optional(&state.db())
    .await?;

    let Some(user) = row else {
        return Err(AppError::unauthorized("not authenticated"));
    };
    if user.status != "active" {
        return Err(AppError::unauthorized("not authenticated"));
    }
    Ok(ok(json!({
        "token": token,
        "user": user.public(),
        "plan": tenant.as_ref().and_then(|Extension(t)| t.plan.as_ref()),
    })))
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    Extension(current): Extension<CurrentUser>,
) -> ApiResult<Response> {
    blacklist_token(&state, &current.jti, current.exp).await;
    Ok(with_cookie(
        ok(json!({ "message": "logged out" })),
        clear_cookie(&state),
    ))
}

pub async fn profile(
    State(state): State<Arc<AppState>>,
    Extension(current): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let row = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(current.id)
    .bind(current.tenant_id)
    .fetch_optional(&state.db())
    .await?;
    let Some(user) = row else {
        return Err(AppError::unauthorized("not authenticated"));
    };
    if user.status != "active" {
        return Err(AppError::unauthorized("not authenticated"));
    }
    Ok(ok(json!({ "user": user.public() })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileBody {
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// 预留：`users` 还没有 settings 列（B1 报告已记录），显式拒绝而不是静默丢弃。
    #[serde(default)]
    pub settings: Option<Value>,
}

/// `PATCH /api/v1/auth/profile`：只能改自己的昵称/显示名，角色与状态不可自改。
pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    Extension(current): Extension<CurrentUser>,
    headers: HeaderMap,
    body: Result<Json<UpdateProfileBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(invalid_json)?;
    if body.settings.is_some() {
        return Err(AppError::bad_request(
            "profile settings are not persisted yet: users.settings column pending (B1)",
        ));
    }
    if body.nickname.is_none() && body.name.is_none() {
        return Err(AppError::bad_request(
            "nothing to update: provide nickname or name",
        ));
    }

    // Option<Option<String>>：外层 None=字段缺省不改，内层 None=清空昵称。
    let nickname: Option<Option<String>> = match body.nickname {
        Some(raw) => Some(normalize_nickname(&raw)?),
        None => None,
    };
    let name: Option<String> = match body.name {
        Some(raw) => Some(normalize_display_name(&raw)?),
        None => None,
    };
    let nickname_set = nickname.is_some();
    let nickname_value = nickname.flatten();
    let name_set = name.is_some();

    let updated = sqlx::query_as::<_, UserRow>(&format!(
        "UPDATE users
            SET nickname = CASE WHEN $1::boolean THEN $2::text ELSE nickname END,
                name     = CASE WHEN $3::boolean THEN $4::text ELSE name END,
                updated_at = now()
          WHERE id = $5 AND tenant_id = $6 AND status = 'active'
        RETURNING {USER_COLUMNS}"
    ))
    .bind(nickname_set)
    .bind(nickname_value)
    .bind(name_set)
    .bind(name)
    .bind(current.id)
    .bind(current.tenant_id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::unauthorized("not authenticated"))?;

    audit(
        &state,
        current.tenant_id,
        Some(current.id),
        "profile_updated",
        &current.id.to_string(),
        &format!(
            "nickname={} name={}",
            if nickname_set { "set" } else { "unchanged" },
            if name_set { "set" } else { "unchanged" }
        ),
        &client_ip(&headers),
    )
    .await;

    Ok(ok(json!({ "user": updated.public() })))
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordBody {
    #[serde(default)]
    pub old_password: String,
    pub new_password: String,
}

pub async fn change_password(
    State(state): State<Arc<AppState>>,
    Extension(current): Extension<CurrentUser>,
    tenant: Option<Extension<TenantContext>>,
    headers: HeaderMap,
    body: Result<Json<ChangePasswordBody>, JsonRejection>,
) -> ApiResult<Response> {
    let body = body.map_err(invalid_json)?;
    validate_password_complexity(&body.new_password).map_err(AppError::bad_request)?;
    let row = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(current.id)
    .bind(current.tenant_id)
    .fetch_optional(&state.db())
    .await?;
    let Some(user) = row else {
        return Err(AppError::unauthorized("not authenticated"));
    };
    if user.status != "active" {
        return Err(AppError::unauthorized("not authenticated"));
    }
    if user.password_set {
        if body.old_password.is_empty() {
            return Err(AppError::bad_request("current password is required"));
        }
        if !verify_password(&user.password_hash, &body.old_password) {
            return Err(AppError::bad_request("current password is incorrect"));
        }
        if body.old_password == body.new_password {
            return Err(AppError::bad_request(
                "new password must be different from the current password",
            ));
        }
    }
    let hash = hash_password(&body.new_password)?;
    sqlx::query(
        "UPDATE users
            SET password_hash = $1, password_set = true, must_change_password = false,
                password_changed_at = now(), updated_at = now()
          WHERE id = $2 AND tenant_id = $3",
    )
    .bind(hash)
    .bind(user.id)
    .bind(user.tenant_id)
    .execute(&state.db())
    .await?;

    revoke_sessions(&state, user.id, current.tenant).await?;
    let (token, _) = issue_token(
        &state,
        user.id,
        &user.email,
        &user.role,
        user.tenant_id,
        tenant.as_ref().map(|Extension(t)| t.id),
        false,
    )
    .await?;
    audit(
        &state,
        user.tenant_id,
        Some(user.id),
        if user.password_set {
            "password_changed"
        } else {
            "password_set"
        },
        "password",
        "",
        &client_ip(&headers),
    )
    .await;
    Ok(with_cookie(
        ok(json!({
            "token": token,
            "user": PublicUser {
                must_change_password: false,
                password_set: true,
                ..user.public()
            }
        })),
        token_cookie(&state, &token),
    ))
}

/// 本机回环免口令：owner 就是运行程序的本机用户；首次调用创建 owner（随机密码、
/// password_set=false，随后可通过改密接口设置密码）。
pub async fn local_session(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // 反向代理（Caddy/Nginx）会把远端请求变成 127.0.0.1；只要出现转发头就拒绝。
    let proxied = ["x-forwarded-for", "x-real-ip", "forwarded"]
        .iter()
        .any(|name| headers.contains_key(*name));
    // 团队/SaaS 形态由登录流程进入；只有裸桌面进程允许回环引导。
    if state.cfg.is_team() || state.cfg.is_saas() {
        return Err(AppError::not_found("not found"));
    }
    if proxied {
        return Err(AppError::forbidden(
            "local session is not available behind a proxy",
        ));
    }
    if !addr.ip().is_loopback() {
        return Err(AppError::forbidden(
            "local session is only available from this machine",
        ));
    }
    let tenant_id: i16 = DEFAULT_TENANT;
    let email = "owner@chiron.local";
    let ip = addr.ip().to_string();

    let existing = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE tenant_id = $1 AND lower(email) = lower($2)"
    ))
    .bind(tenant_id)
    .bind(email)
    .fetch_optional(&state.db())
    .await?;

    let (user, created) = match existing {
        Some(u) => (u, false),
        None => {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
                .fetch_one(&state.db())
                .await?;
            if count > 0 {
                return Err(AppError::conflict("owner account is not provisioned"));
            }
            let hash = hash_password(&Uuid::new_v4().to_string())?;
            let raw_name = std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_else(|_| "Local".to_string());
            let name = normalize_nickname(&raw_name)?.unwrap_or_else(|| "Local".to_string());
            let inserted = sqlx::query_as::<_, UserRow>(&format!(
                "INSERT INTO users (tenant_id, email, name, nickname, password_hash, password_set,
                                    role, status, must_change_password)
                 VALUES ($1, $2, $3, $3, $4, false, 'owner', 'active', false)
                 RETURNING {USER_COLUMNS}"
            ))
            .bind(tenant_id)
            .bind(email)
            .bind(name)
            .bind(hash)
            .fetch_one(&state.db())
            .await?;
            (inserted, true)
        }
    };

    if user.status != "active" {
        return Err(AppError::forbidden("owner account is not active"));
    }
    let (token, _) = issue_token(
        &state,
        user.id,
        &user.email,
        &user.role,
        user.tenant_id,
        None,
        user.must_change_password,
    )
    .await?;
    audit(
        &state,
        tenant_id,
        Some(user.id),
        if created {
            "owner_provisioned"
        } else {
            "local_session"
        },
        "local_session",
        &format!("created={created} password_set={}", user.password_set),
        &ip,
    )
    .await;
    Ok(with_cookie(
        ok(json!({ "token": token, "user": user.public() })),
        token_cookie(&state, &token),
    ))
}

// ── 管理员：用户管理 ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Pagination {
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub per_page: Option<i64>,
}

fn require_admin(actor: &CurrentUser) -> Result<(), AppError> {
    if is_admin_role(&actor.role) {
        Ok(())
    } else {
        Err(AppError::forbidden("insufficient permissions"))
    }
}

pub async fn list_users(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(page): Query<Pagination>,
) -> ApiResult<Response> {
    require_admin(&actor)?;
    let per_page = page.per_page.unwrap_or(20).clamp(1, 100);
    let page_no = page.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * per_page;

    // 软删除账号不出现在管理列表里（审计行仍保留）。
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE tenant_id = $1 AND status <> 'deleted'",
    )
    .bind(actor.tenant_id)
    .fetch_one(&state.db())
    .await?;
    let rows = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE tenant_id = $1 AND status <> 'deleted'
          ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(actor.tenant_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db())
    .await?;

    let users: Vec<PublicUser> = rows.iter().map(UserRow::public).collect();
    Ok(ok(json!({
        "users": users,
        "total": total,
        "page": page_no,
        "per_page": per_page,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserBody {
    pub email: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub initial_password: String,
}

fn default_role() -> String {
    "user".to_string()
}

pub async fn create_user(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    body: Result<Json<CreateUserBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(invalid_json)?;
    require_admin(&actor)?;
    crate::tenant::quota::ensure_user_slot(&state, actor.plan.as_ref()).await?;
    let email = body.email.trim().to_lowercase();
    let username = body.username.trim().to_lowercase();
    let role = body.role.trim().to_lowercase();
    if email.is_empty() {
        return Err(AppError::bad_request("email is required"));
    }
    if !valid_email(&email) {
        return Err(AppError::bad_request("email is not a valid address"));
    }
    if !["user", "admin", "owner"].contains(&role.as_str()) {
        return Err(AppError::bad_request("invalid role"));
    }
    if role != "user" && actor.role != "owner" {
        return Err(AppError::forbidden(
            "only the platform owner can create admin or owner accounts",
        ));
    }
    if !username.is_empty() && !valid_username(&username) {
        return Err(AppError::bad_request(
            "username must be 3-64 chars of letters, digits, dot, underscore or hyphen",
        ));
    }
    let display = {
        let n = if !body.nickname.trim().is_empty() {
            body.nickname.trim()
        } else if !body.name.trim().is_empty() {
            body.name.trim()
        } else if !username.is_empty() {
            username.as_str()
        } else {
            email.split('@').next().unwrap_or("user")
        };
        normalize_display_name(n)?
    };
    // 昵称为空就落 NULL，展示时由 UserRow::public 回落到 name。
    let nickname = normalize_nickname(&body.nickname)?;

    let password = if body.initial_password.is_empty() {
        generate_temp_password()
    } else {
        validate_password_complexity(&body.initial_password).map_err(AppError::bad_request)?;
        body.initial_password
    };
    let hash = hash_password(&password)?;

    let inserted = sqlx::query_as::<_, UserRow>(&format!(
        "INSERT INTO users (tenant_id, email, username, nickname, name, password_hash,
                            password_set, role, status, must_change_password)
         VALUES ($1, $2, NULLIF($3, ''), $4, $5, $6, true, $7, 'active', true)
         RETURNING {USER_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(&email)
    .bind(&username)
    .bind(nickname)
    .bind(&display)
    .bind(hash)
    .bind(&role)
    .fetch_one(&state.db())
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.code().as_deref() == Some("23505") => {
            AppError::conflict("email or username already exists")
        }
        other => AppError::from(other),
    })?;

    audit(
        &state,
        actor.tenant_id,
        Some(actor.id),
        "user_created",
        &inserted.id.to_string(),
        &format!("email={} role={}", mask_identifier(&email), role),
        &client_ip(&headers),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "user": inserted.public(),
                "initial_password": password,
            }
        })),
    )
        .into_response())
}

async fn load_target(state: &AppState, tenant_id: i16, id: Uuid) -> Result<UserRow, AppError> {
    sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("user not found"))
}

/// 纯判定 manage_guard + 末位 owner 的库内计数。
async fn ensure_can_manage(
    state: &AppState,
    actor: &CurrentUser,
    target: &UserRow,
    action: &str,
) -> Result<(), AppError> {
    manage_guard(
        actor.id,
        &actor.role,
        target.id,
        &target.role,
        &target.status,
        action,
    )?;
    if needs_last_owner_guard(&target.role, action) {
        let owners: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE role = 'owner' AND status <> 'deleted'",
        )
        .fetch_one(&state.db())
        .await?;
        if owners <= 1 {
            return Err(AppError::forbidden(
                "cannot remove or demote the last owner",
            ));
        }
    }
    Ok(())
}

pub async fn reset_password(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> ApiResult<Response> {
    let id = parse_uuid(&raw_id)?;
    require_admin(&actor)?;
    let target = load_target(&state, actor.tenant_id, id).await?;
    ensure_can_manage(&state, &actor, &target, "reset_password").await?;

    let password = generate_temp_password();
    let hash = hash_password(&password)?;
    sqlx::query(
        "UPDATE users SET password_hash = $1, password_set = true, must_change_password = true,
                          password_changed_at = now(), updated_at = now()
          WHERE id = $2 AND tenant_id = $3",
    )
    .bind(hash)
    .bind(id)
    .bind(actor.tenant_id)
    .execute(&state.db())
    .await?;
    revoke_sessions(&state, id, actor.tenant).await?;
    audit(
        &state,
        actor.tenant_id,
        Some(actor.id),
        "password_reset",
        &id.to_string(),
        &format!("email={}", mask_identifier(&target.email)),
        &client_ip(&headers),
    )
    .await;
    Ok(ok(json!({ "temporary_password": password })))
}

pub async fn disable_user(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> ApiResult<Response> {
    let id = parse_uuid(&raw_id)?;
    apply_user_status(state, actor, headers, id, "disabled").await
}

pub async fn enable_user(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> ApiResult<Response> {
    let id = parse_uuid(&raw_id)?;
    apply_user_status(state, actor, headers, id, "active").await
}

async fn apply_user_status(
    state: Arc<AppState>,
    actor: CurrentUser,
    headers: HeaderMap,
    id: Uuid,
    status: &'static str,
) -> ApiResult<Response> {
    require_admin(&actor)?;
    let target = load_target(&state, actor.tenant_id, id).await?;
    ensure_can_manage(
        &state,
        &actor,
        &target,
        if status == "disabled" {
            "disable"
        } else {
            "enable"
        },
    )
    .await?;
    sqlx::query(
        "UPDATE users SET status = $1, updated_at = now() WHERE id = $2 AND tenant_id = $3",
    )
    .bind(status)
    .bind(id)
    .bind(actor.tenant_id)
    .execute(&state.db())
    .await?;
    if status == "disabled" {
        revoke_sessions(&state, id, actor.tenant).await?;
    }
    audit(
        &state,
        actor.tenant_id,
        Some(actor.id),
        if status == "disabled" {
            "user_disabled"
        } else {
            "user_enabled"
        },
        &id.to_string(),
        &format!("email={}", mask_identifier(&target.email)),
        &client_ip(&headers),
    )
    .await;
    Ok(ok(json!({ "status": status })))
}

pub async fn force_logout(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> ApiResult<Response> {
    let id = parse_uuid(&raw_id)?;
    require_admin(&actor)?;
    let target = load_target(&state, actor.tenant_id, id).await?;
    ensure_can_manage(&state, &actor, &target, "force_logout").await?;
    revoke_sessions(&state, id, actor.tenant).await?;
    audit(
        &state,
        actor.tenant_id,
        Some(actor.id),
        "forced_logout",
        &id.to_string(),
        &format!("email={}", mask_identifier(&target.email)),
        &client_ip(&headers),
    )
    .await;
    Ok(ok(json!({ "status": "revoked" })))
}

pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
) -> ApiResult<Response> {
    let id = parse_uuid(&raw_id)?;
    require_admin(&actor)?;
    let target = load_target(&state, actor.tenant_id, id).await?;
    ensure_can_manage(&state, &actor, &target, "delete").await?;
    sqlx::query(
        "UPDATE users
            SET status = 'deleted',
                email = 'deleted+' || id::text || '@invalid.local',
                username = 'deleted_' || id::text,
                updated_at = now()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(actor.tenant_id)
    .execute(&state.db())
    .await?;
    revoke_sessions(&state, id, actor.tenant).await?;
    audit(
        &state,
        actor.tenant_id,
        Some(actor.id),
        "user_deleted",
        &id.to_string(),
        &format!("email={}", mask_identifier(&target.email)),
        &client_ip(&headers),
    )
    .await;
    Ok(ok(json!({ "status": "deleted" })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn login_lock_uses_5_failure_threshold() {
        assert_eq!(login_lock_ttl(0, 900), None);
        assert_eq!(login_lock_ttl(4, 900), None);
        assert_eq!(login_lock_ttl(5, 900), Some(900));
        assert_eq!(login_lock_ttl(9, 42), Some(42));
        // TTL 已过期（0/负数）时至少 1 秒，避免 429 带 0 秒无意义提示。
        assert_eq!(login_lock_ttl(5, 0), Some(1));
        assert_eq!(login_lock_ttl(5, -3), Some(1));
    }

    #[test]
    fn nickname_normalization() {
        assert_eq!(normalize_nickname("  ").unwrap(), None);
        assert_eq!(normalize_nickname("\t\n").unwrap(), None);
        assert_eq!(
            normalize_nickname("  张三  ").unwrap(),
            Some("张三".to_string())
        );
        assert!(
            normalize_nickname("a\u{0007}b").is_err(),
            "control chars rejected"
        );
        let too_long = "a".repeat(NICKNAME_MAX_CHARS + 1);
        assert!(normalize_nickname(&too_long).is_err());
        let at_limit = "a".repeat(NICKNAME_MAX_CHARS);
        assert!(normalize_nickname(&at_limit).is_ok());
        assert!(normalize_display_name("   ").is_err());
        assert_eq!(normalize_display_name(" Owner ").unwrap(), "Owner");
    }

    #[test]
    fn email_and_username_validation() {
        assert!(valid_email("user@example.com"));
        assert!(valid_email("owner@chiron.local"));
        for bad in [
            "",
            "user",
            "user@",
            "@example.com",
            "user@example",
            "a b@example.com",
            "u@@ex.com",
        ] {
            assert!(!valid_email(bad), "{bad} must be rejected");
        }
        assert!(!valid_email(&format!(
            "{}@x.com",
            "a".repeat(EMAIL_MAX_CHARS)
        )));

        assert!(valid_username("ab.c_1-2"));
        assert!(!valid_username("ab"), "too short");
        assert!(!valid_username(&"a".repeat(65)), "too long");
        assert!(!valid_username("has space"));
        assert!(!valid_username("bad/name"));
    }

    #[test]
    fn admin_guard_blocks_privilege_escalation() {
        let owner = uid();
        let admin = uid();
        let other_admin = uid();
        let user = uid();

        // admin 不能碰 owner
        assert!(manage_guard(admin, "admin", owner, "owner", "active", "disable").is_err());
        // admin 不能碰其他 admin
        assert!(manage_guard(
            admin,
            "admin",
            other_admin,
            "admin",
            "active",
            "reset_password"
        )
        .is_err());
        // owner 能管 admin / user
        assert!(manage_guard(owner, "owner", admin, "admin", "active", "reset_password").is_ok());
        assert!(manage_guard(owner, "owner", user, "user", "active", "disable").is_ok());
        // admin 能管普通 user
        assert!(manage_guard(admin, "admin", user, "user", "active", "disable").is_ok());
        // 普通用户没有任何管理权
        assert!(manage_guard(user, "user", admin, "admin", "active", "disable").is_err());

        // 不能禁用/删除/重置自己
        for action in ["disable", "delete", "reset_password"] {
            let err = manage_guard(owner, "owner", owner, "owner", "active", action).unwrap_err();
            assert_eq!(err.status, StatusCode::FORBIDDEN);
            assert_eq!(err.code, "forbidden");
        }
        // 可以给自己 force_logout / enable（非破坏性）
        assert!(manage_guard(owner, "owner", owner, "owner", "active", "force_logout").is_ok());

        // 软删除目标统一 404，避免暴露"曾经存在"
        let err = manage_guard(owner, "owner", user, "user", "deleted", "disable").unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn last_owner_guard_only_for_destructive_actions() {
        assert!(needs_last_owner_guard("owner", "disable"));
        assert!(needs_last_owner_guard("owner", "delete"));
        assert!(!needs_last_owner_guard("owner", "enable"));
        assert!(!needs_last_owner_guard("owner", "force_logout"));
        assert!(!needs_last_owner_guard("owner", "reset_password"));
        assert!(!needs_last_owner_guard("admin", "disable"));
        assert!(!needs_last_owner_guard("user", "delete"));
    }

    #[test]
    fn login_body_ident_precedence() {
        let mut body = LoginBody {
            identifier: String::new(),
            email: " a@b.com ".into(),
            username: "alice".into(),
            password: "x".into(),
        };
        assert_eq!(body.ident(), "a@b.com");
        body.identifier = "alice".into();
        assert_eq!(body.ident(), "alice");
        body.identifier = "   ".into();
        assert_eq!(body.ident(), "a@b.com");
    }

    #[test]
    fn audit_masks_identifiers() {
        assert_eq!(mask_identifier("alice@example.com"), "a***@example.com");
        assert_eq!(mask_identifier("alice"), "a***");
        assert_eq!(mask_identifier("ab"), "***");
        assert_eq!(mask_identifier(""), "***");
    }

    #[test]
    fn login_keys_cover_all_aliases_without_duplicates() {
        let user = UserRow {
            id: uid(),
            tenant_id: DEFAULT_TENANT,
            email: "Alice@Example.com".into(),
            username: Some("alice".into()),
            nickname: None,
            name: "Alice".into(),
            password_hash: String::new(),
            password_set: true,
            role: "user".into(),
            status: "active".into(),
            must_change_password: false,
            last_login_at: None,
        };
        assert_eq!(
            ident_keys("alice@example.com", &user),
            vec!["alice@example.com".to_string(), "alice".to_string()]
        );
        let keys = ident_keys("alice", &user);
        assert_eq!(
            keys,
            vec!["alice".to_string(), "alice@example.com".to_string()]
        );
    }

    #[test]
    fn admin_role_detection() {
        assert!(is_admin_role("owner"));
        assert!(is_admin_role("admin"));
        assert!(!is_admin_role("user"));
        assert!(!is_admin_role("Admin"));
        assert!(!is_admin_role(""));
    }
}
