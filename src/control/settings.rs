//! 平台集成配置（当前：支付宝）。值统一 AES-GCM 加密落控制库，读取时按需解密；
//! GET 只返回非敏感字段与密钥是否存在，绝不回显密钥。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::{RateLimitConfig, RatePolicy},
    control::{alipay, crypto, platform_audit, PlatformAdmin},
    error::{ApiResult, AppError},
    media::storage::{MediaStorageConfig, S3Options},
    state::{AlertSettings, AppState},
};

const APP_ID: &str = "alipay_app_id";
const PRIVATE_KEY: &str = "alipay_private_key";
const PUBLIC_KEY: &str = "alipay_public_key";
const NOTIFY_URL: &str = "alipay_notify_url";
const RETURN_URL: &str = "alipay_return_url";
const GATEWAY: &str = "alipay_gateway";
const ALERT_WEBHOOK: &str = "alert_webhook_url";
const ALERT_COOLDOWN: &str = "alert_cooldown_secs";
const ALERT_RUNS: &str = "alert_active_runs";
const ALERT_POOLS: &str = "alert_tenant_pools";
const ALERT_EGRESS: &str = "alert_egress_denied";
const RATE_ENABLED: &str = "rate_limit_enabled";
const RATE_LOGIN_BURST: &str = "rate_login_burst";
const RATE_LOGIN_RPM: &str = "rate_login_per_minute";
const RATE_SUBMIT_BURST: &str = "rate_submit_burst";
const RATE_SUBMIT_RPM: &str = "rate_submit_per_minute";
const RATE_TOOL_BURST: &str = "rate_tool_burst";
const RATE_TOOL_RPM: &str = "rate_tool_per_minute";
const RATE_HEAVY_BURST: &str = "rate_heavy_burst";
const RATE_HEAVY_RPM: &str = "rate_heavy_per_minute";
const MEDIA_BACKEND: &str = "media_backend";
const MEDIA_WEBDAV_URL: &str = "media_webdav_url";
const MEDIA_WEBDAV_USERNAME: &str = "media_webdav_username";
const MEDIA_WEBDAV_PASSWORD: &str = "media_webdav_password";
const MEDIA_WEBDAV_PREFIX: &str = "media_webdav_prefix";
const MEDIA_S3_BUCKET: &str = "media_s3_bucket";
const MEDIA_S3_REGION: &str = "media_s3_region";
const MEDIA_S3_ENDPOINT: &str = "media_s3_endpoint";
const MEDIA_S3_ACCESS_KEY: &str = "media_s3_access_key";
const MEDIA_S3_SECRET_KEY: &str = "media_s3_secret_key";
const MEDIA_S3_SESSION_TOKEN: &str = "media_s3_session_token";
const MEDIA_S3_PATH_STYLE: &str = "media_s3_path_style";
const MEDIA_S3_PREFIX: &str = "media_s3_prefix";
const PROVIDER_KEY_PREFIX: &str = "provider_key_";

#[derive(Debug, Clone, Default)]
pub struct AlipaySettings {
    pub app_id: String,
    pub private_key: String,
    pub public_key: String,
    pub notify_url: String,
    pub return_url: String,
    pub gateway: String,
}

impl AlipaySettings {
    pub fn configured(&self) -> bool {
        alipay::configured(
            &self.app_id,
            &self.private_key,
            &self.public_key,
            &self.notify_url,
        )
    }
}

fn aad(key: &str) -> String {
    format!("platform_setting:{key}")
}

async fn get_value(pool: &PgPool, secrets_key: &str, key: &str) -> Result<Option<String>, AppError> {
    let enc: Option<String> =
        sqlx::query_scalar("SELECT value_enc FROM platform_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    match enc {
        Some(enc) => crypto::open(secrets_key, aad(key).as_bytes(), &enc)
            .map(Some)
            .map_err(|msg| {
                tracing::error!(setting = key, error = %msg, "platform setting decrypt failed");
                AppError::internal("platform setting is unreadable")
            }),
        None => Ok(None),
    }
}

async fn put_value(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    secrets_key: &str,
    key: &str,
    value: &str,
    admin_id: Uuid,
) -> Result<(), AppError> {
    let value = value.trim();
    if value.is_empty() {
        sqlx::query("DELETE FROM platform_settings WHERE key = $1")
            .bind(key)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    }
    let sealed = crypto::seal(secrets_key, aad(key).as_bytes(), value)
        .map_err(|msg| AppError::internal(format!("encrypt setting: {msg}")))?;
    sqlx::query(
        "INSERT INTO platform_settings (key, value_enc, updated_by, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (key) DO UPDATE
            SET value_enc = EXCLUDED.value_enc,
                updated_by = EXCLUDED.updated_by,
                updated_at = now()",
    )
    .bind(key)
    .bind(sealed)
    .bind(admin_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 控制库优先，环境变量兜底；两者都没有时返回空串。
pub async fn load_alipay(state: &AppState) -> Result<AlipaySettings, AppError> {
    let control = state
        .control
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured"))?;
    let secrets = state.cfg.secrets_key.trim();
    let db = if secrets.is_empty() {
        None
    } else {
        Some(secrets)
    };
    let mut out = AlipaySettings {
        app_id: state.cfg.alipay_app_id.clone(),
        private_key: state.cfg.alipay_private_key.clone(),
        public_key: state.cfg.alipay_public_key.clone(),
        notify_url: state.cfg.alipay_notify_url.clone(),
        return_url: state.cfg.alipay_return_url.clone(),
        gateway: state.cfg.alipay_gateway.clone(),
    };
    if let Some(secrets) = db {
        for (key, slot) in [
            (APP_ID, &mut out.app_id),
            (PRIVATE_KEY, &mut out.private_key),
            (PUBLIC_KEY, &mut out.public_key),
            (NOTIFY_URL, &mut out.notify_url),
            (RETURN_URL, &mut out.return_url),
            (GATEWAY, &mut out.gateway),
        ] {
            if let Some(value) = get_value(&control.db, secrets, key).await? {
                *slot = value;
            }
        }
    }
    Ok(out)
}

async fn number_setting(
    state: &AppState,
    key: &str,
    fallback: u64,
) -> Result<u64, AppError> {
    let Some(control) = state.control.as_ref() else {
        return Ok(fallback);
    };
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Ok(fallback);
    }
    let Some(raw) = get_value(&control.db, secrets, key).await? else {
        return Ok(fallback);
    };
    Ok(raw.trim().parse::<u64>().unwrap_or(fallback))
}

pub async fn load_alerts(state: &AppState) -> Result<AlertSettings, AppError> {
    let mut out = AlertSettings::from_config(&state.cfg);
    if let Some(control) = state.control.as_ref() {
        let secrets = state.cfg.secrets_key.trim();
        if !secrets.is_empty() {
            if let Some(value) = get_value(&control.db, secrets, ALERT_WEBHOOK).await? {
                out.webhook_url = value;
            }
        }
    }
    out.cooldown_secs = number_setting(state, ALERT_COOLDOWN, out.cooldown_secs).await?;
    out.active_runs = number_setting(state, ALERT_RUNS, out.active_runs).await?;
    out.tenant_pools = number_setting(state, ALERT_POOLS, out.tenant_pools).await?;
    out.egress_denied = number_setting(state, ALERT_EGRESS, out.egress_denied).await?;
    Ok(out)
}

pub async fn load_rate_limit(state: &AppState) -> Result<RateLimitConfig, AppError> {
    let base = state.cfg.rate_limit.clone();
    let enabled = number_setting(state, RATE_ENABLED, base.enabled as u64).await? != 0;
    let login_burst = number_setting(state, RATE_LOGIN_BURST, base.login.burst as u64).await?;
    let login_rpm = number_setting(state, RATE_LOGIN_RPM, base.login.per_minute as u64).await?;
    let submit_burst = number_setting(state, RATE_SUBMIT_BURST, base.submit.burst as u64).await?;
    let submit_rpm = number_setting(state, RATE_SUBMIT_RPM, base.submit.per_minute as u64).await?;
    let tool_burst = number_setting(state, RATE_TOOL_BURST, base.tool.burst as u64).await?;
    let tool_rpm = number_setting(state, RATE_TOOL_RPM, base.tool.per_minute as u64).await?;
    let heavy_burst = number_setting(state, RATE_HEAVY_BURST, base.heavy.burst as u64).await?;
    let heavy_rpm = number_setting(state, RATE_HEAVY_RPM, base.heavy.per_minute as u64).await?;
    let policy = |burst: u64, rpm: u64| RatePolicy::new(burst.min(u32::MAX as u64) as u32, rpm.min(u32::MAX as u64) as u32);
    Ok(RateLimitConfig {
        enabled,
        login: policy(login_burst, login_rpm),
        submit: policy(submit_burst, submit_rpm),
        tool: policy(tool_burst, tool_rpm),
        heavy: policy(heavy_burst, heavy_rpm),
    })
}

/// 启动时把控制库设置应用到进程内运行态。
async fn settings_map(
    state: &AppState,
    keys: &[&str],
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let mut out = std::collections::HashMap::new();
    let Some(control) = state.control.as_ref() else {
        return Ok(out);
    };
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Ok(out);
    }
    let wanted: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value_enc FROM platform_settings WHERE key = ANY($1)",
    )
    .bind(&wanted)
    .fetch_all(&control.db)
    .await?;
    for (key, enc) in rows {
        match crypto::open(secrets, aad(&key).as_bytes(), &enc) {
            Ok(value) if !value.trim().is_empty() => {
                out.insert(key, value);
            }
            Ok(_) => {}
            Err(msg) => {
                tracing::error!(setting = %key, error = %msg, "platform setting decrypt failed");
                return Err(AppError::internal("platform setting is unreadable"));
            }
        }
    }
    Ok(out)
}

async fn text_setting(
    state: &AppState,
    key: &str,
    fallback: String,
) -> Result<String, AppError> {
    Ok(settings_map(state, &[key])
        .await?
        .remove(key)
        .unwrap_or(fallback))
}

fn env_string(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

static MEDIA_CACHE: OnceLock<Mutex<Option<(Instant, MediaStorageConfig)>>> = OnceLock::new();

fn media_cache() -> &'static Mutex<Option<(Instant, MediaStorageConfig)>> {
    MEDIA_CACHE.get_or_init(|| Mutex::new(None))
}

pub async fn load_media(state: &AppState) -> Result<MediaStorageConfig, AppError> {
    if let Ok(cache) = media_cache().lock() {
        if let Some((at, cfg)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(5) {
                return Ok(cfg.clone());
            }
        }
    }
    let cfg = load_media_uncached(state).await?;
    if let Ok(mut cache) = media_cache().lock() {
        *cache = Some((Instant::now(), cfg.clone()));
    }
    Ok(cfg)
}

fn invalidate_media_cache() {
    if let Ok(mut cache) = media_cache().lock() {
        *cache = None;
    }
}

async fn load_media_uncached(state: &AppState) -> Result<MediaStorageConfig, AppError> {
    let backend = text_setting(
        state,
        MEDIA_BACKEND,
        state.cfg.media_backend.trim().to_ascii_lowercase(),
    )
    .await?
    .trim()
    .to_ascii_lowercase();
    let webdav_url = text_setting(
        state,
        MEDIA_WEBDAV_URL,
        env_string("CHIRON_MEDIA_WEBDAV_URL"),
    )
    .await?;
    let webdav_username = text_setting(
        state,
        MEDIA_WEBDAV_USERNAME,
        env_string("CHIRON_MEDIA_WEBDAV_USERNAME"),
    )
    .await?;
    let webdav_password = text_setting(
        state,
        MEDIA_WEBDAV_PASSWORD,
        env_string("CHIRON_MEDIA_WEBDAV_PASSWORD"),
    )
    .await?;
    let webdav_prefix = text_setting(
        state,
        MEDIA_WEBDAV_PREFIX,
        env_string("CHIRON_MEDIA_WEBDAV_PREFIX"),
    )
    .await?;
    let path_style = text_setting(
        state,
        MEDIA_S3_PATH_STYLE,
        env_string("CHIRON_MEDIA_S3_PATH_STYLE"),
    )
    .await?;
    let path_style = match path_style.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            return Err(AppError::bad_request(
                "media_s3_path_style must be true/false",
            ))
        }
    };
    let s3 = S3Options {
        bucket: text_setting(state, MEDIA_S3_BUCKET, env_string("CHIRON_MEDIA_S3_BUCKET")).await?,
        region: text_setting(
            state,
            MEDIA_S3_REGION,
            env_string("CHIRON_MEDIA_S3_REGION"),
        )
        .await?,
        endpoint: {
            let value = text_setting(
                state,
                MEDIA_S3_ENDPOINT,
                env_string("CHIRON_MEDIA_S3_ENDPOINT"),
            )
            .await?;
            (!value.trim().is_empty()).then_some(value)
        },
        access_key: text_setting(
            state,
            MEDIA_S3_ACCESS_KEY,
            env_string("CHIRON_MEDIA_S3_ACCESS_KEY"),
        )
        .await?,
        secret_key: text_setting(
            state,
            MEDIA_S3_SECRET_KEY,
            env_string("CHIRON_MEDIA_S3_SECRET_KEY"),
        )
        .await?,
        session_token: {
            let value = text_setting(
                state,
                MEDIA_S3_SESSION_TOKEN,
                env_string("CHIRON_MEDIA_S3_SESSION_TOKEN"),
            )
            .await?;
            (!value.trim().is_empty()).then_some(value)
        },
        path_style,
        prefix: {
            let value = text_setting(
                state,
                MEDIA_S3_PREFIX,
                env_string("CHIRON_MEDIA_S3_PREFIX"),
            )
            .await?;
            (!value.trim().is_empty()).then_some(value)
        },
    };
    Ok(MediaStorageConfig {
        backend,
        local_root: state.cfg.media_root_path(),
        webdav_url,
        webdav_username,
        webdav_password,
        webdav_prefix,
        s3: Some(s3),
    })
}

pub async fn apply_runtime(state: &AppState) -> Result<(), AppError> {
    if state.control.is_none() || state.cfg.secrets_key.trim().is_empty() {
        return Ok(());
    }
    let alerts = load_alerts(state).await?;
    if let Ok(mut current) = state.alerts.write() {
        *current = alerts;
    }
    state.limits.set_config(load_rate_limit(state).await?);
    crate::provider::catalog::set_platform_keys(load_provider_keys(state).await?);
    Ok(())
}

pub async fn load_provider_keys(
    state: &AppState,
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let mut out = std::collections::HashMap::new();
    let Some(control) = state.control.as_ref() else {
        return Ok(out);
    };
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Ok(out);
    }
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM platform_settings WHERE key LIKE 'provider_key_%'",
    )
    .fetch_all(&control.db)
    .await?;
    for key in keys {
        let Some(id) = key.strip_prefix(PROVIDER_KEY_PREFIX) else {
            continue;
        };
        if let Some(value) = get_value(&control.db, secrets, &key).await? {
            if !value.trim().is_empty() {
                out.insert(id.to_string(), value);
            }
        }
    }
    Ok(out)
}

fn provider_env_configured(id: &str) -> bool {
    crate::providers::find(id)
        .and_then(|preset| std::env::var(preset.api_key_env).ok())
        .is_some_and(|value| !value.trim().is_empty())
}

fn providers_json(keys: &std::collections::HashMap<String, String>) -> serde_json::Value {
    let providers: Vec<serde_json::Value> = crate::providers::PRESETS
        .iter()
        .map(|preset| {
            json!({
                "id": preset.id,
                "label": preset.label,
                "requires_key": preset.requires_key,
                "configured": keys.contains_key(preset.id) || provider_env_configured(preset.id),
                "stored_in_db": keys.contains_key(preset.id),
            })
        })
        .collect();
    json!({ "providers": providers })
}

pub async fn get_providers(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    let keys = load_provider_keys(&state).await?;
    Ok(Json(json!({ "data": providers_json(&keys) })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct ProviderKeysBody {
    #[serde(default)]
    pub keys: std::collections::HashMap<String, String>,
}

pub async fn put_providers(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<ProviderKeysBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured")
    })?;
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "secrets_key_required",
            "SECRETS_KEY is required to store provider keys",
        ));
    }
    for (id, value) in &body.keys {
        if crate::providers::find(id).is_none() {
            return Err(AppError::bad_request(format!("unknown provider: {id}")));
        }
        if value.trim().chars().count() > 4096 {
            return Err(AppError::bad_request(format!("provider key for {id} is too long")));
        }
    }
    let mut tx = control.db.begin().await?;
    for (id, value) in &body.keys {
        write_setting(
            &mut tx,
            secrets,
            &format!("{PROVIDER_KEY_PREFIX}{id}"),
            value,
            admin.id,
        )
        .await?;
    }
    tx.commit().await?;
    let keys = load_provider_keys(&state).await?;
    crate::provider::catalog::set_platform_keys(keys.clone());
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "provider_keys_updated",
        None,
        &format!("providers={}", body.keys.keys().cloned().collect::<Vec<_>>().join(",")),
        "",
    )
    .await;
    Ok(Json(json!({ "data": providers_json(&keys) })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct AlertBody {
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
    #[serde(default)]
    pub active_runs: Option<u64>,
    #[serde(default)]
    pub tenant_pools: Option<u64>,
    #[serde(default)]
    pub egress_denied: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RateBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub login_burst: Option<u32>,
    #[serde(default)]
    pub login_per_minute: Option<u32>,
    #[serde(default)]
    pub submit_burst: Option<u32>,
    #[serde(default)]
    pub submit_per_minute: Option<u32>,
    #[serde(default)]
    pub tool_burst: Option<u32>,
    #[serde(default)]
    pub tool_per_minute: Option<u32>,
    #[serde(default)]
    pub heavy_burst: Option<u32>,
    #[serde(default)]
    pub heavy_per_minute: Option<u32>,
}

async fn write_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    secrets: &str,
    key: &str,
    value: &str,
    admin_id: Uuid,
) -> Result<(), AppError> {
    put_value(tx, secrets, key, value, admin_id).await
}

fn alerts_json(value: &AlertSettings) -> serde_json::Value {
    json!({
        "webhook_configured": !value.webhook_url.trim().is_empty(),
        "cooldown_secs": value.cooldown_secs,
        "active_runs": value.active_runs,
        "tenant_pools": value.tenant_pools,
        "egress_denied": value.egress_denied,
    })
}

pub async fn get_alerts(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    Ok(Json(json!({ "data": alerts_json(&load_alerts(&state).await?) })).into_response())
}

pub async fn put_alerts(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<AlertBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured")
    })?;
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "secrets_key_required",
            "SECRETS_KEY is required to store alert settings",
        ));
    }
    if let Some(url) = body.webhook_url.as_deref() {
        if !valid_url(url, true) {
            return Err(AppError::bad_request("webhook_url must be an http(s) URL"));
        }
    }
    let mut tx = control.db.begin().await?;
    if let Some(url) = body.webhook_url.as_deref() {
        write_setting(&mut tx, secrets, ALERT_WEBHOOK, url, admin.id).await?;
    }
    for (key, value) in [
        (ALERT_COOLDOWN, body.cooldown_secs),
        (ALERT_RUNS, body.active_runs),
        (ALERT_POOLS, body.tenant_pools),
        (ALERT_EGRESS, body.egress_denied),
    ] {
        if let Some(value) = value {
            write_setting(&mut tx, secrets, key, &value.to_string(), admin.id).await?;
        }
    }
    tx.commit().await?;
    let value = load_alerts(&state).await?;
    if let Ok(mut current) = state.alerts.write() {
        *current = value.clone();
    }
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "alert_settings_updated",
        None,
        "",
        "",
    )
    .await;
    Ok(Json(json!({ "data": alerts_json(&value) })).into_response())
}

fn rate_json(cfg: &RateLimitConfig) -> serde_json::Value {
    json!({
        "enabled": cfg.enabled,
        "login": { "burst": cfg.login.burst, "per_minute": cfg.login.per_minute },
        "submit": { "burst": cfg.submit.burst, "per_minute": cfg.submit.per_minute },
        "tool": { "burst": cfg.tool.burst, "per_minute": cfg.tool.per_minute },
        "heavy": { "burst": cfg.heavy.burst, "per_minute": cfg.heavy.per_minute },
    })
}

pub async fn get_rates(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    Ok(Json(json!({ "data": rate_json(&state.limits.config()) })).into_response())
}

pub async fn put_rates(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<RateBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured")
    })?;
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "secrets_key_required",
            "SECRETS_KEY is required to store rate settings",
        ));
    }
    for value in [
        body.login_burst,
        body.login_per_minute,
        body.submit_burst,
        body.submit_per_minute,
        body.tool_burst,
        body.tool_per_minute,
        body.heavy_burst,
        body.heavy_per_minute,
    ]
    .into_iter()
    .flatten()
    {
        if value == 0 || value > 100_000 {
            return Err(AppError::bad_request("rate values must be 1..100000"));
        }
    }
    let mut tx = control.db.begin().await?;
    if let Some(enabled) = body.enabled {
        write_setting(&mut tx, secrets, RATE_ENABLED, if enabled { "1" } else { "0" }, admin.id).await?;
    }
    for (key, value) in [
        (RATE_LOGIN_BURST, body.login_burst),
        (RATE_LOGIN_RPM, body.login_per_minute),
        (RATE_SUBMIT_BURST, body.submit_burst),
        (RATE_SUBMIT_RPM, body.submit_per_minute),
        (RATE_TOOL_BURST, body.tool_burst),
        (RATE_TOOL_RPM, body.tool_per_minute),
        (RATE_HEAVY_BURST, body.heavy_burst),
        (RATE_HEAVY_RPM, body.heavy_per_minute),
    ] {
        if let Some(value) = value {
            write_setting(&mut tx, secrets, key, &value.to_string(), admin.id).await?;
        }
    }
    tx.commit().await?;
    state.limits.set_config(load_rate_limit(&state).await?);
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "rate_settings_updated",
        None,
        "",
        "",
    )
    .await;
    Ok(Json(json!({ "data": rate_json(&state.limits.config()) })).into_response())
}

async fn media_json(state: &AppState) -> Result<serde_json::Value, AppError> {
    let cfg = load_media(state).await?;
    Ok(json!({
        "backend": cfg.backend,
        "webdav": {
            "url": cfg.webdav_url,
            "username": cfg.webdav_username,
            "prefix": cfg.webdav_prefix,
            "has_password": !cfg.webdav_password.is_empty(),
        },
        "s3": {
            "bucket": cfg.s3.as_ref().map(|v| v.bucket.clone()).unwrap_or_default(),
            "region": cfg.s3.as_ref().map(|v| v.region.clone()).unwrap_or_default(),
            "endpoint": cfg.s3.as_ref().and_then(|v| v.endpoint.clone()),
            "path_style": cfg.s3.as_ref().and_then(|v| v.path_style),
            "prefix": cfg.s3.as_ref().and_then(|v| v.prefix.clone()),
            "has_access_key": cfg.s3.as_ref().map(|v| !v.access_key.is_empty()).unwrap_or(false),
            "has_secret_key": cfg.s3.as_ref().map(|v| !v.secret_key.is_empty()).unwrap_or(false),
            "has_session_token": cfg.s3.as_ref().and_then(|v| v.session_token.as_ref()).is_some(),
        }
    }))
}

pub async fn get_media(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    Ok(Json(json!({ "data": media_json(&state).await? })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct MediaBody {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub webdav_url: Option<String>,
    #[serde(default)]
    pub webdav_username: Option<String>,
    #[serde(default)]
    pub webdav_password: Option<String>,
    #[serde(default)]
    pub webdav_prefix: Option<String>,
    #[serde(default)]
    pub s3_bucket: Option<String>,
    #[serde(default)]
    pub s3_region: Option<String>,
    #[serde(default)]
    pub s3_endpoint: Option<String>,
    #[serde(default)]
    pub s3_access_key: Option<String>,
    #[serde(default)]
    pub s3_secret_key: Option<String>,
    #[serde(default)]
    pub s3_session_token: Option<String>,
    #[serde(default)]
    pub s3_path_style: Option<bool>,
    #[serde(default)]
    pub s3_prefix: Option<String>,
}

pub async fn put_media(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<MediaBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(|| {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured")
    })?;
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "secrets_key_required",
            "SECRETS_KEY is required to store media settings",
        ));
    }
    if let Some(backend) = body.backend.as_deref() {
        let backend = backend.trim().to_ascii_lowercase();
        if !backend.is_empty() && !matches!(backend.as_str(), "local" | "webdav" | "s3") {
            return Err(AppError::bad_request("backend must be local, webdav or s3"));
        }
    }
    for (label, value) in [
        ("webdav_url", body.webdav_url.as_deref()),
        ("s3_endpoint", body.s3_endpoint.as_deref()),
    ] {
        if let Some(value) = value {
            if !valid_url(value, true) {
                return Err(AppError::bad_request(format!("{label} must be an http(s) URL")));
            }
        }
    }
    let mut tx = control.db.begin().await?;
    for (key, value) in [
        (MEDIA_BACKEND, body.backend.as_deref()),
        (MEDIA_WEBDAV_URL, body.webdav_url.as_deref()),
        (MEDIA_WEBDAV_USERNAME, body.webdav_username.as_deref()),
        (MEDIA_WEBDAV_PASSWORD, body.webdav_password.as_deref()),
        (MEDIA_WEBDAV_PREFIX, body.webdav_prefix.as_deref()),
        (MEDIA_S3_BUCKET, body.s3_bucket.as_deref()),
        (MEDIA_S3_REGION, body.s3_region.as_deref()),
        (MEDIA_S3_ENDPOINT, body.s3_endpoint.as_deref()),
        (MEDIA_S3_ACCESS_KEY, body.s3_access_key.as_deref()),
        (MEDIA_S3_SECRET_KEY, body.s3_secret_key.as_deref()),
        (MEDIA_S3_SESSION_TOKEN, body.s3_session_token.as_deref()),
        (MEDIA_S3_PREFIX, body.s3_prefix.as_deref()),
    ] {
        if let Some(value) = value {
            write_setting(&mut tx, secrets, key, value, admin.id).await?;
        }
    }
    if let Some(path_style) = body.s3_path_style {
        write_setting(
            &mut tx,
            secrets,
            MEDIA_S3_PATH_STYLE,
            if path_style { "1" } else { "0" },
            admin.id,
        )
        .await?;
    }
    tx.commit().await?;
    invalidate_media_cache();
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "media_settings_updated",
        None,
        "",
        "",
    )
    .await;
    Ok(Json(json!({ "data": media_json(&state).await? })).into_response())
}

async fn sanitized(state: &AppState) -> Result<serde_json::Value, AppError> {
    let control = state
        .control
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured"))?;
    let settings = load_alipay(state).await?;
    let db_keys: Vec<String> = if state.cfg.secrets_key.trim().is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT key FROM platform_settings WHERE key IN ($1, $2, $3, $4, $5, $6)",
        )
        .bind(APP_ID)
        .bind(PRIVATE_KEY)
        .bind(PUBLIC_KEY)
        .bind(NOTIFY_URL)
        .bind(RETURN_URL)
        .bind(GATEWAY)
        .fetch_all(&control.db)
        .await
        .unwrap_or_default()
    };
    Ok(json!({
        "alipay": {
            "app_id": settings.app_id,
            "notify_url": settings.notify_url,
            "return_url": settings.return_url,
            "gateway": settings.gateway,
            "has_private_key": !settings.private_key.trim().is_empty(),
            "has_public_key": !settings.public_key.trim().is_empty(),
            "configured": settings.configured(),
            "stored_in_db": db_keys,
            "secrets_key_configured": !state.cfg.secrets_key.trim().is_empty(),
        }
    }))
}

pub async fn get_alipay(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Response> {
    Ok(Json(json!({ "data": sanitized(&state).await? })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct AlipayBody {
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub notify_url: Option<String>,
    #[serde(default)]
    pub return_url: Option<String>,
    #[serde(default)]
    pub gateway: Option<String>,
}

fn valid_url(raw: &str, allow_empty: bool) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return allow_empty;
    }
    url::Url::parse(raw)
        .map(|u| matches!(u.scheme(), "http" | "https") && u.username().is_empty())
        .unwrap_or(false)
}

pub async fn put_alipay(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<AlipayBody>,
) -> ApiResult<Response> {
    let control = state
        .control
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "control_unavailable", "control plane is not configured"))?;
    let secrets = state.cfg.secrets_key.trim();
    if secrets.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "secrets_key_required",
            "SECRETS_KEY is required to store integration settings",
        ));
    }
    for (label, value, allow_empty) in [
        ("notify_url", body.notify_url.as_deref(), true),
        ("return_url", body.return_url.as_deref(), true),
        ("gateway", body.gateway.as_deref(), true),
    ] {
        if let Some(value) = value {
            if !valid_url(value, allow_empty) {
                return Err(AppError::bad_request(format!(
                    "{label} must be an http(s) URL without credentials"
                )));
            }
        }
    }
    if let Some(app_id) = body.app_id.as_deref() {
        if app_id.trim().chars().count() > 64 {
            return Err(AppError::bad_request("app_id is too long"));
        }
    }
    if let Some(key) = body.private_key.as_deref().filter(|v| !v.trim().is_empty()) {
        alipay::validate_private_key(key)
            .map_err(|e| AppError::bad_request(format!("invalid private key: {e}")))?;
    }
    if let Some(key) = body.public_key.as_deref().filter(|v| !v.trim().is_empty()) {
        alipay::validate_public_key(key)
            .map_err(|e| AppError::bad_request(format!("invalid public key: {e}")))?;
    }

    let mut tx = control.db.begin().await?;
    for (key, value) in [
        (APP_ID, body.app_id.as_deref()),
        (PRIVATE_KEY, body.private_key.as_deref()),
        (PUBLIC_KEY, body.public_key.as_deref()),
        (NOTIFY_URL, body.notify_url.as_deref()),
        (RETURN_URL, body.return_url.as_deref()),
        (GATEWAY, body.gateway.as_deref()),
    ] {
        if let Some(value) = value {
            put_value(&mut tx, secrets, key, value, admin.id).await?;
        }
    }
    tx.commit().await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "integration_settings_updated",
        None,
        "provider=alipay",
        "",
    )
    .await;
    Ok(Json(json!({ "data": sanitized(&state).await? })).into_response())
}
