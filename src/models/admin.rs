//! 租户模型服务管理（owner/admin）：key 池、provider 启停、模型目录与运行时路由。
//!
//! 所有写操作都落租户库 `audit_logs`（action 形如 `model.key_added`），
//! 审计与错误信息里都**不出现明文 key**；接口只回 id/label/weight/enabled。

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::Response,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    clean_label, clean_model, clean_priority, clean_weight, control_unavailable,
    default_upstream_model, display_name_of, ok, tenant_of,
};
use crate::{
    admin::ensure_admin,
    auth::CurrentUser,
    control::crypto,
    error::{ApiResult, AppError},
    provider::{catalog, ModelInfo},
    providers,
    state::AppState,
    tenant::{key_aad, pick_key, PooledKey},
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/admin/models", get(overview))
        .route("/api/v1/admin/models/keys", post(add_key))
        .route(
            "/api/v1/admin/models/keys/{id}",
            patch(patch_key).delete(delete_key),
        )
        .route(
            "/api/v1/admin/models/providers/{provider}",
            patch(patch_provider),
        )
        .route("/api/v1/admin/models/catalog", post(add_catalog))
        .route(
            "/api/v1/admin/models/catalog/{model}",
            patch(patch_catalog).delete(delete_catalog),
        )
        .route("/api/v1/admin/models/routes", post(add_route))
        .route(
            "/api/v1/admin/models/routes/{id}",
            patch(patch_route).delete(delete_route),
        )
        // 自动发现 + 单个 provider 健康探测。
        .route("/api/v1/admin/models/discover", post(discover))
        .route(
            "/api/v1/admin/models/providers/{provider}/probe",
            post(probe),
        )
        // 契约里给出的短路径（不带 /api/v1）也接受，两个入口同一 handler。
        .route("/admin/models/providers/{provider}/probe", post(probe))
}

fn control_db(state: &AppState) -> Result<PgPool, AppError> {
    state
        .control
        .as_ref()
        .map(|control| control.db.clone())
        .ok_or_else(control_unavailable)
}

/// 写操作后的运行时缓存失效（key 池 30s / 路由 30s）。
fn invalidate_keys(state: &AppState, tenant: Uuid) {
    if let Some(registry) = state.tenants.as_ref() {
        registry.invalidate_keys(tenant);
    }
}

fn invalidate_routes(state: &AppState, tenant: Uuid) {
    if let Some(registry) = state.tenants.as_ref() {
        registry.invalidate_routes(tenant);
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PoolProviderRow {
    provider: String,
    keys_count: i64,
    enabled: bool,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub(super) struct PoolKeyRow {
    id: Uuid,
    provider: String,
    label: String,
    weight: i32,
    enabled: bool,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub(super) struct CatalogRow {
    model: String,
    display_name: String,
    enabled: bool,
    source: String,
    discovered_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub(super) struct RouteRow {
    id: Uuid,
    model: String,
    provider: String,
    upstream_model: String,
    priority: i32,
    weight: i32,
    enabled: bool,
    last_ok_at: Option<DateTime<Utc>>,
    last_error: String,
    last_probe_at: Option<DateTime<Utc>>,
}

const KEY_COLUMNS: &str = "id, provider, label, weight, enabled, updated_at";
const CATALOG_COLUMNS: &str = "model, display_name, enabled, source, discovered_at, created_at";
const ROUTE_COLUMNS: &str = "id, model, provider, upstream_model, priority, weight, enabled, \
     last_ok_at, last_error, last_probe_at";

/// provider 级摘要：标签来自 preset 表，计数/启停状态来自 key 池。
fn provider_summary(provider: &str, keys_count: i64, enabled: bool) -> Value {
    json!({
        "provider": provider,
        "label": providers::find(provider)
            .map(|preset| preset.label)
            .unwrap_or(provider),
        "keys_count": keys_count,
        "enabled": enabled,
    })
}

/// 当前租户的模型服务概览（keys 不含明文）。
pub async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;

    let pool: Vec<PoolProviderRow> = sqlx::query_as(
        "SELECT provider, count(*) AS keys_count, bool_or(enabled) AS enabled
           FROM tenant_provider_keys
          WHERE tenant_id = $1
          GROUP BY provider
          ORDER BY provider ASC",
    )
    .bind(tenant)
    .fetch_all(&db)
    .await?;
    // provider 清单来自 preset 表（可接入哪些服务），keys_count/enabled 来自当前 key 池。
    let stats: HashMap<&str, (i64, bool)> = pool
        .iter()
        .map(|row| (row.provider.as_str(), (row.keys_count, row.enabled)))
        .collect();
    let mut providers: Vec<Value> = providers::PRESETS
        .iter()
        .map(|preset| {
            let (keys_count, enabled) = stats.get(preset.id).copied().unwrap_or((0, false));
            provider_summary(preset.id, keys_count, enabled)
        })
        .collect();
    // 池里出现但 preset 表不认识的 provider（历史数据）也要让管理员看见。
    for row in pool
        .iter()
        .filter(|row| providers::find(&row.provider).is_none())
    {
        providers.push(provider_summary(&row.provider, row.keys_count, row.enabled));
    }

    let keys = sqlx::query_as::<_, PoolKeyRow>(&format!(
        "SELECT {KEY_COLUMNS} FROM tenant_provider_keys
          WHERE tenant_id = $1 ORDER BY provider ASC, label ASC"
    ))
    .bind(tenant)
    .fetch_all(&db)
    .await?;

    let models = sqlx::query_as::<_, CatalogRow>(&format!(
        "SELECT {CATALOG_COLUMNS} FROM tenant_models
          WHERE tenant_id = $1 ORDER BY model ASC"
    ))
    .bind(tenant)
    .fetch_all(&db)
    .await?;

    let routes = sqlx::query_as::<_, RouteRow>(&format!(
        "SELECT {ROUTE_COLUMNS} FROM tenant_model_routes
          WHERE tenant_id = $1 ORDER BY model ASC, priority ASC, provider ASC"
    ))
    .bind(tenant)
    .fetch_all(&db)
    .await?;

    Ok(ok(json!({
        "providers": providers,
        "keys": keys,
        "models": models,
        "routes": routes,
    })))
}

// ── key 池 ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddKeyBody {
    pub provider: String,
    pub api_key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub weight: Option<i32>,
}

/// 新增/覆盖一条 key：同 `(provider, label)` upsert，明文只在此刻进入进程。
pub async fn add_key(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<AddKeyBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;

    let provider = body.provider.trim().to_ascii_lowercase();
    if providers::find(&provider).is_none() {
        return Err(AppError::bad_request("unknown provider preset"));
    }
    let label = clean_label(body.label.as_deref())?;
    let weight = clean_weight(body.weight)?.unwrap_or(1);
    let api_key = body.api_key.trim();
    if api_key.len() < 8 {
        return Err(AppError::bad_request("api_key looks too short"));
    }
    let enc = crypto::seal(
        &state.cfg.secrets_key,
        key_aad(tenant, &provider).as_bytes(),
        api_key,
    )
    .map_err(|msg| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;

    let row = sqlx::query_as::<_, PoolKeyRow>(&format!(
        "INSERT INTO tenant_provider_keys (tenant_id, provider, label, key_enc, weight, enabled)
         VALUES ($1, $2, $3, $4, $5, true)
         ON CONFLICT (tenant_id, provider, label) DO UPDATE
            SET key_enc = EXCLUDED.key_enc,
                weight = EXCLUDED.weight,
                enabled = true,
                updated_at = now()
         RETURNING {KEY_COLUMNS}"
    ))
    .bind(tenant)
    .bind(&provider)
    .bind(&label)
    .bind(enc)
    .bind(weight)
    .fetch_one(&db)
    .await?;

    invalidate_keys(&state, tenant);
    crate::integration::audit(
        &state,
        &actor,
        "model.key_added",
        &provider,
        &format!("label={label} weight={weight}"),
    )
    .await;
    Ok(ok(json!({ "key": row })))
}

#[derive(Debug, Deserialize)]
pub struct PatchKeyBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub weight: Option<i32>,
}

pub async fn patch_key(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchKeyBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let weight = clean_weight(body.weight)?;
    if body.enabled.is_none() && weight.is_none() {
        return Err(AppError::bad_request("nothing to update"));
    }

    let row = sqlx::query_as::<_, PoolKeyRow>(&format!(
        "UPDATE tenant_provider_keys
            SET enabled = COALESCE($3, enabled),
                weight = COALESCE($4, weight),
                updated_at = now()
          WHERE tenant_id = $1 AND id = $2
         RETURNING {KEY_COLUMNS}"
    ))
    .bind(tenant)
    .bind(id)
    .bind(body.enabled)
    .bind(weight)
    .fetch_optional(&db)
    .await?
    .ok_or_else(|| AppError::not_found("provider key not found"))?;

    invalidate_keys(&state, tenant);
    crate::integration::audit(
        &state,
        &actor,
        "model.key_updated",
        &row.provider,
        &format!(
            "label={} enabled={} weight={}",
            row.label, row.enabled, row.weight
        ),
    )
    .await;
    Ok(ok(json!({ "key": row })))
}

pub async fn delete_key(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;

    let provider: Option<String> = sqlx::query_scalar(
        "DELETE FROM tenant_provider_keys WHERE tenant_id = $1 AND id = $2 RETURNING provider",
    )
    .bind(tenant)
    .bind(id)
    .fetch_optional(&db)
    .await?;
    let Some(provider) = provider else {
        return Err(AppError::not_found("provider key not found"));
    };

    invalidate_keys(&state, tenant);
    crate::integration::audit(&state, &actor, "model.key_deleted", &provider, "").await;
    Ok(ok(json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
pub struct ToggleProviderBody {
    pub enabled: bool,
}

/// 启用/禁用某个 provider 的全部 key（"启用/禁用模型服务"）。
pub async fn patch_provider(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(provider): Path<String>,
    Json(body): Json<ToggleProviderBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let provider = provider.trim().to_ascii_lowercase();

    let affected = sqlx::query(
        "UPDATE tenant_provider_keys
            SET enabled = $3, updated_at = now()
          WHERE tenant_id = $1 AND provider = $2",
    )
    .bind(tenant)
    .bind(&provider)
    .bind(body.enabled)
    .execute(&db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("provider has no keys"));
    }

    invalidate_keys(&state, tenant);
    crate::integration::audit(
        &state,
        &actor,
        "model.provider_toggled",
        &provider,
        &format!("enabled={} keys={affected}", body.enabled),
    )
    .await;
    // 与控制台契约一致：`{ provider: { provider, label, keys_count, enabled } }`。
    Ok(ok(json!({
        "provider": provider_summary(&provider, affected as i64, body.enabled),
    })))
}

// ── 模型目录 ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddCatalogBody {
    pub model: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

pub async fn add_catalog(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<AddCatalogBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let model = clean_model(&body.model)?;
    let explicit = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let display_name = display_name_of(&model, explicit.unwrap_or(""));

    let row = sqlx::query_as::<_, CatalogRow>(&format!(
        "INSERT INTO tenant_models (tenant_id, model, display_name)
         VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, model) DO UPDATE
            SET display_name = CASE WHEN $4 THEN EXCLUDED.display_name
                                    ELSE tenant_models.display_name END,
                enabled = true,
                updated_at = now()
         RETURNING {CATALOG_COLUMNS}"
    ))
    .bind(tenant)
    .bind(&model)
    .bind(&display_name)
    .bind(explicit.is_some())
    .fetch_one(&db)
    .await?;

    crate::integration::audit(&state, &actor, "model.catalog_added", &model, "").await;
    Ok(ok(json!({ "model": row })))
}

#[derive(Debug, Deserialize)]
pub struct PatchCatalogBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub display_name: Option<String>,
}

pub async fn patch_catalog(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(model): Path<String>,
    Json(body): Json<PatchCatalogBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let model = clean_model(&model)?;
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .map(str::to_string);
    if body.enabled.is_none() && display_name.is_none() {
        return Err(AppError::bad_request("nothing to update"));
    }

    let row = sqlx::query_as::<_, CatalogRow>(&format!(
        "UPDATE tenant_models
            SET enabled = COALESCE($3, enabled),
                display_name = COALESCE($4, display_name),
                updated_at = now()
          WHERE tenant_id = $1 AND model = $2
         RETURNING {CATALOG_COLUMNS}"
    ))
    .bind(tenant)
    .bind(&model)
    .bind(body.enabled)
    .bind(display_name)
    .fetch_optional(&db)
    .await?
    .ok_or_else(|| AppError::not_found("model not found"))?;

    crate::integration::audit(
        &state,
        &actor,
        "model.catalog_updated",
        &model,
        &format!("enabled={}", row.enabled),
    )
    .await;
    Ok(ok(json!({ "model": row })))
}

pub async fn delete_catalog(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(model): Path<String>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let model = clean_model(&model)?;

    let affected = sqlx::query("DELETE FROM tenant_models WHERE tenant_id = $1 AND model = $2")
        .bind(tenant)
        .bind(&model)
        .execute(&db)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("model not found"));
    }

    crate::integration::audit(&state, &actor, "model.catalog_deleted", &model, "").await;
    Ok(ok(json!({ "deleted": true })))
}

// ── 模型路由 ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddRouteBody {
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub weight: Option<i32>,
}

pub async fn add_route(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<AddRouteBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let model = clean_model(&body.model)?;
    let provider = body.provider.trim().to_ascii_lowercase();
    if providers::find(&provider).is_none() {
        return Err(AppError::bad_request("unknown provider preset"));
    }
    let upstream_model = match body.upstream_model.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => clean_model(value)?,
        _ => default_upstream_model(&model, &provider),
    };
    let priority = clean_priority(body.priority)?.unwrap_or(100);
    let weight = clean_weight(body.weight)?.unwrap_or(1);

    let row = sqlx::query_as::<_, RouteRow>(&format!(
        "INSERT INTO tenant_model_routes
             (tenant_id, model, provider, upstream_model, priority, weight)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (tenant_id, model, provider) DO UPDATE
            SET upstream_model = EXCLUDED.upstream_model,
                priority = EXCLUDED.priority,
                weight = EXCLUDED.weight,
                enabled = true,
                updated_at = now()
         RETURNING {ROUTE_COLUMNS}"
    ))
    .bind(tenant)
    .bind(&model)
    .bind(&provider)
    .bind(&upstream_model)
    .bind(priority)
    .bind(weight)
    .fetch_one(&db)
    .await?;

    invalidate_routes(&state, tenant);
    crate::integration::audit(
        &state,
        &actor,
        "model.route_added",
        &model,
        &format!(
            "provider={provider} upstream={upstream_model} priority={priority} weight={weight}"
        ),
    )
    .await;
    Ok(ok(json!({ "route": row })))
}

#[derive(Debug, Deserialize)]
pub struct PatchRouteBody {
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub weight: Option<i32>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub async fn patch_route(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchRouteBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let upstream_model = match body.upstream_model.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => Some(clean_model(value)?),
        _ => None,
    };
    let priority = clean_priority(body.priority)?;
    let weight = clean_weight(body.weight)?;
    if upstream_model.is_none() && priority.is_none() && weight.is_none() && body.enabled.is_none()
    {
        return Err(AppError::bad_request("nothing to update"));
    }

    let row = sqlx::query_as::<_, RouteRow>(&format!(
        "UPDATE tenant_model_routes
            SET upstream_model = COALESCE($3, upstream_model),
                priority = COALESCE($4, priority),
                weight = COALESCE($5, weight),
                enabled = COALESCE($6, enabled),
                updated_at = now()
          WHERE tenant_id = $1 AND id = $2
         RETURNING {ROUTE_COLUMNS}"
    ))
    .bind(tenant)
    .bind(id)
    .bind(upstream_model)
    .bind(priority)
    .bind(weight)
    .bind(body.enabled)
    .fetch_optional(&db)
    .await?
    .ok_or_else(|| AppError::not_found("route not found"))?;

    invalidate_routes(&state, tenant);
    crate::integration::audit(
        &state,
        &actor,
        "model.route_updated",
        &row.model,
        &format!(
            "provider={} upstream={} priority={} weight={} enabled={}",
            row.provider, row.upstream_model, row.priority, row.weight, row.enabled
        ),
    )
    .await;
    Ok(ok(json!({ "route": row })))
}

pub async fn delete_route(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;

    let model: Option<String> = sqlx::query_scalar(
        "DELETE FROM tenant_model_routes WHERE tenant_id = $1 AND id = $2 RETURNING model",
    )
    .bind(tenant)
    .bind(id)
    .fetch_optional(&db)
    .await?;
    let Some(model) = model else {
        return Err(AppError::not_found("route not found"));
    };

    invalidate_routes(&state, tenant);
    crate::integration::audit(&state, &actor, "model.route_deleted", &model, "").await;
    Ok(ok(json!({ "deleted": true })))
}

// ── 自动发现 / 健康探测 ───────────────────────────────────────────────

/// 探测结果里的路由状态只写这两列；错误文本统一脱敏并截断 300 字符。
const MAX_ERROR_CHARS: usize = 300;

#[derive(Debug, Deserialize)]
pub struct DiscoverBody {
    /// 只发现这一个 provider；缺省 = 全部"可用 provider"。
    #[serde(default)]
    pub provider: Option<String>,
    /// 发现结果写入租户目录（并补默认路由）。
    #[serde(default)]
    pub persist: bool,
}

/// 租户 key 池（已解密，仅内存）；控制面/注册表缺失时为空 = 只用平台 env。
async fn tenant_pools(
    state: &AppState,
    tenant: Uuid,
) -> Result<HashMap<String, Vec<PooledKey>>, AppError> {
    match state.tenants.as_ref() {
        Some(registry) => Ok(registry.keys(tenant).await?.as_ref().clone()),
        None => Ok(HashMap::new()),
    }
}

/// 平台 env 里该 preset 是否配了 key（不读租户池、不泄露 key 值）。
fn platform_key_available(provider: &str) -> bool {
    catalog::resolve_with(provider, None, None)
        .map(|resolved| resolved.api_key.is_some())
        .unwrap_or(false)
}

/// "可用 provider"：租户池里有 enabled key，或平台 env 有该 preset 的 key。
fn available_providers(pools: &HashMap<String, Vec<PooledKey>>) -> Vec<String> {
    providers::PRESETS
        .iter()
        .filter(|preset| {
            pools
                .get(preset.id)
                .map(|pool| !pool.is_empty())
                .unwrap_or(false)
                || platform_key_available(preset.id)
        })
        .map(|preset| preset.id.to_string())
        .collect()
}

/// 目录统一存 `<provider>:<model>`；远端已带同名前缀时不重复拼。
pub(crate) fn unified_model_name(provider: &str, upstream: &str) -> String {
    let provider = provider.trim();
    let upstream = upstream.trim();
    match upstream.split_once(':') {
        Some((prefix, rest))
            if !rest.trim().is_empty() && prefix.trim().eq_ignore_ascii_case(provider) =>
        {
            upstream.to_string()
        }
        _ => format!("{provider}:{upstream}"),
    }
}

/// 错误脱敏：去掉可能出现的 key 明文；再做长度截断（默认 300 字符）。
pub(crate) fn sanitize_error(raw: &str, key: Option<&str>) -> String {
    let redacted = match key.map(str::trim).filter(|secret| !secret.is_empty()) {
        Some(secret) => raw.replace(secret, "[redacted]"),
        None => raw.to_string(),
    };
    truncate_chars(&redacted, MAX_ERROR_CHARS)
}

fn truncate_chars(raw: &str, max: usize) -> String {
    let mut out: String = raw.chars().take(max).collect();
    if raw.chars().count() > max {
        out.push('…');
    }
    out
}

/// 用池内加权选一条 key 构建 provider 并拉取 `/models`；失败返回脱敏后的文本。
async fn probe_provider(provider: &str, key: Option<&str>) -> Result<Vec<ModelInfo>, String> {
    let built = catalog::build_by_id_with_key(provider, key)
        .map_err(|err| sanitize_error(&err.to_string(), key))?;
    built
        .list_models()
        .await
        .map_err(|err| sanitize_error(&err.to_string(), key))
}

/// `POST /admin/models/discover`：逐 provider 拉远端模型目录（单个失败不影响其他）。
///
/// `persist=true` 时把发现的模型写入 `tenant_models`（source=discovered），
/// 并为**新**模型补一条默认路由（`upstream_model` 存裸模型名）。
pub async fn discover(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<DiscoverBody>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;

    let pools = tenant_pools(&state, tenant).await?;
    let targets: Vec<String> = match body
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        Some(provider) => {
            let provider = provider.to_ascii_lowercase();
            if providers::find(&provider).is_none() {
                return Err(AppError::bad_request("unknown provider preset"));
            }
            vec![provider]
        }
        None => available_providers(&pools),
    };

    let existing: std::collections::HashSet<String> =
        sqlx::query_scalar("SELECT model FROM tenant_models WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(&db)
            .await?
            .into_iter()
            .collect();

    let mut results: Vec<Value> = Vec::new();
    let mut persisted = 0usize;
    for provider in targets {
        let key = pools
            .get(&provider)
            .and_then(|pool| pick_key(pool))
            .map(|key| key.key.clone());
        match probe_provider(&provider, key.as_deref()).await {
            Ok(models) => {
                let mut entries: Vec<Value> = Vec::new();
                let mut fresh: Vec<(String, String)> = Vec::new();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for model in models {
                    let upstream = model.id.trim();
                    if upstream.is_empty() {
                        continue;
                    }
                    let unified = unified_model_name(&provider, upstream);
                    if clean_model(&unified).is_err() || !seen.insert(unified.clone()) {
                        continue;
                    }
                    let exists = existing.contains(&unified);
                    if !exists {
                        fresh.push((unified.clone(), default_upstream_model(upstream, &provider)));
                    }
                    entries.push(json!({ "model": unified, "exists": exists }));
                }

                if body.persist {
                    for (unified, upstream_model) in &fresh {
                        let inserted = sqlx::query(
                            "INSERT INTO tenant_models
                                 (tenant_id, model, display_name, enabled, source, discovered_at)
                             VALUES ($1, $2, $3, true, 'discovered', now())
                             ON CONFLICT (tenant_id, model) DO NOTHING",
                        )
                        .bind(tenant)
                        .bind(unified)
                        .bind(display_name_of(unified, ""))
                        .execute(&db)
                        .await?
                        .rows_affected();
                        if inserted == 0 {
                            continue;
                        }
                        persisted += 1;
                        // 只给新模型补一条默认路由；已有路由不覆盖管理员配置。
                        sqlx::query(
                            "INSERT INTO tenant_model_routes
                                 (tenant_id, model, provider, upstream_model, priority, weight, enabled)
                             VALUES ($1, $2, $3, $4, 100, 1, true)
                             ON CONFLICT (tenant_id, model, provider) DO NOTHING",
                        )
                        .bind(tenant)
                        .bind(unified)
                        .bind(&provider)
                        .bind(upstream_model)
                        .execute(&db)
                        .await?;
                    }
                }

                results.push(json!({
                    "provider": provider,
                    "ok": true,
                    "models": entries,
                }));
            }
            Err(error) => results.push(json!({
                "provider": provider,
                "ok": false,
                "models": [],
                "error": error,
            })),
        }
    }

    if body.persist {
        invalidate_routes(&state, tenant);
    }
    crate::integration::audit(
        &state,
        &actor,
        "model.discovered",
        &body.provider.clone().unwrap_or_else(|| "all".to_string()),
        &format!("providers={} persisted={persisted}", results.len()),
    )
    .await;

    Ok(ok(json!({ "results": results })))
}

/// `POST /admin/models/providers/{provider}/probe`：探测单个 provider 的 `/models`，
/// 成功清除该 provider 全部路由的 `last_error`，失败写入脱敏后的原因。
pub async fn probe(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(provider): Path<String>,
) -> ApiResult<Response> {
    ensure_admin(&actor)?;
    let tenant = tenant_of(&actor)?;
    let db = control_db(&state)?;
    let provider = provider.trim().to_ascii_lowercase();
    if providers::find(&provider).is_none() {
        return Err(AppError::bad_request("unknown provider preset"));
    }

    let pools = tenant_pools(&state, tenant).await?;
    let key = pools
        .get(&provider)
        .and_then(|pool| pick_key(pool))
        .map(|key| key.key.clone());

    match probe_provider(&provider, key.as_deref()).await {
        Ok(models) => {
            sqlx::query(
                "UPDATE tenant_model_routes
                    SET last_ok_at = now(), last_probe_at = now(), last_error = ''
                  WHERE tenant_id = $1 AND provider = $2",
            )
            .bind(tenant)
            .bind(&provider)
            .execute(&db)
            .await?;
            crate::integration::audit(
                &state,
                &actor,
                "model.provider_probed",
                &provider,
                &format!("ok=true models={}", models.len()),
            )
            .await;
            Ok(ok(json!({ "ok": true, "models": models.len() })))
        }
        Err(error) => {
            sqlx::query(
                "UPDATE tenant_model_routes
                    SET last_probe_at = now(), last_error = $3
                  WHERE tenant_id = $1 AND provider = $2",
            )
            .bind(tenant)
            .bind(&provider)
            .bind(&error)
            .execute(&db)
            .await?;
            crate::integration::audit(
                &state,
                &actor,
                "model.provider_probed",
                &provider,
                "ok=false",
            )
            .await;
            crate::alerts::notify(
                &state,
                "model_probe_failed",
                Some(&tenant.to_string()),
                &format!("provider={provider} {error}"),
            );
            Ok(ok(json!({ "ok": false, "models": 0, "error": error })))
        }
    }
}

/// 管理端响应统一包络（与其它租户 API 一致：`{"data": ...}`）。
#[cfg(test)]
fn envelope(data: Value) -> Response {
    ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_router_registers_expected_paths() {
        // 路由表本身不触发 IO：能构建即说明 handler 的 extractor 组合是合法的。
        let _ = router();
    }

    #[test]
    fn success_envelope_wraps_data() {
        let resp = envelope(json!({ "ok": true }));
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn discovered_model_names_are_provider_prefixed_once() {
        assert_eq!(
            unified_model_name("openai", "gpt-5"),
            "openai:gpt-5",
            "裸模型名补前缀"
        );
        assert_eq!(
            unified_model_name("openai", "openai:gpt-5"),
            "openai:gpt-5",
            "已带同名前缀不重复"
        );
        // 其他前缀不是本 provider 的命名空间，仍按本 provider 归一。
        assert_eq!(
            unified_model_name("openrouter", "openai:gpt-5"),
            "openrouter:openai:gpt-5"
        );
    }

    #[test]
    fn probe_errors_are_redacted_and_truncated() {
        let message = format!(
            "upstream said 401 for key sk-secret-123 {}",
            "x".repeat(400)
        );
        let sanitized = sanitize_error(&message, Some("sk-secret-123"));
        assert!(!sanitized.contains("sk-secret-123"), "{sanitized}");
        assert!(sanitized.contains("[redacted]"));
        assert_eq!(
            sanitized.chars().count(),
            MAX_ERROR_CHARS + 1,
            "截断 + 省略号"
        );
        // 无 key 时只截断。
        assert_eq!(truncate_chars("abc", 300), "abc");
    }
}
