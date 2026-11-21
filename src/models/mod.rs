//! 租户模型服务：模型服务（key 池启停/池化）、模型目录、运行时路由，
//! 以及普通用户侧的模型清单与隐藏模型。
//!
//! 边界：
//! * key 池 / 模型目录 / 路由存**控制库**，只有租户 owner/admin 能改；
//! * 用户隐藏模型存**租户库** `user_hidden_models`，只影响前端选择，不阻止 API 显式调用；
//! * 普通用户 `/api/v1/models` 只返回**已启用**目录；目录为空时回落到平台 preset 列表，
//!   保证升级前就存在的租户行为不变；
//! * 提交消息时，目录非空且请求模型不在启用目录内 → `403 model_not_allowed`。

pub mod admin;

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    providers,
    state::AppState,
};

/// 单个用户最多隐藏多少个模型（覆盖写上限）。
pub const MAX_HIDDEN_MODELS: usize = 200;
const MAX_MODEL_CHARS: usize = 200;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/models", get(list_models))
        .route("/api/v1/models/hidden", get(list_hidden).put(put_hidden))
        .merge(admin::router())
}

pub(crate) fn ok(data: Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

fn control_unavailable() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "model services require the control plane (SaaS mode)",
    )
}

/// 当前请求所属的 SaaS 租户；单库形态没有该概念。
pub(crate) fn tenant_of(actor: &CurrentUser) -> Result<Uuid, AppError> {
    actor.tenant.ok_or_else(control_unavailable)
}

// ── 纯逻辑（无 IO，可单测）──────────────────────────────────────────────

fn strip_provider_prefix(model: &str) -> &str {
    match model.split_once(':') {
        Some((_, rest)) if !rest.trim().is_empty() => rest.trim(),
        _ => model,
    }
}

/// 两个模型名是否指同一个对外模型：`openai:gpt-5` 与 `gpt-5` 等价。
pub(crate) fn same_model(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_lowercase();
    let b = b.trim().to_ascii_lowercase();
    a == b || strip_provider_prefix(&a).eq_ignore_ascii_case(strip_provider_prefix(&b))
}

/// 请求模型是否在租户启用目录里（目录项同样可能是 `<provider>:<model>`）。
pub fn model_in_catalog(catalog: &[String], requested: &str) -> bool {
    let want = requested.trim();
    !want.is_empty() && catalog.iter().any(|entry| same_model(entry, want))
}

/// 目录/路由的默认 display_name：空串时回落到模型名。
pub(crate) fn display_name_of(model: &str, display_name: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        model.to_string()
    } else {
        name.to_string()
    }
}

/// 路由默认 upstream_model：去掉 `provider:`（或任意已知 preset）前缀。
pub(crate) fn default_upstream_model(model: &str, provider: &str) -> String {
    let raw = model.trim();
    if let Some((prefix, rest)) = raw.split_once(':') {
        let rest = rest.trim();
        let prefix = prefix.trim();
        if !rest.is_empty()
            && (prefix.eq_ignore_ascii_case(provider.trim()) || providers::find(prefix).is_some())
        {
            return rest.to_string();
        }
    }
    raw.to_string()
}

/// 模型名清洗：非空、无空白/控制字符、长度受限（同时避免路径段污染）。
pub(crate) fn clean_model(raw: &str) -> Result<String, AppError> {
    let model = raw.trim();
    if model.is_empty() {
        return Err(AppError::bad_request("model is required"));
    }
    if model.chars().count() > MAX_MODEL_CHARS {
        return Err(AppError::bad_request(format!(
            "model name must be at most {MAX_MODEL_CHARS} characters"
        )));
    }
    if model.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::bad_request(
            "model name must not contain whitespace",
        ));
    }
    Ok(model.to_string())
}

/// key 标签：默认 `default`，去空白、限长。
pub(crate) fn clean_label(raw: Option<&str>) -> Result<String, AppError> {
    let label = raw
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("default");
    if label.chars().count() > 64 {
        return Err(AppError::bad_request("label must be at most 64 characters"));
    }
    if label.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "label must not contain control chars",
        ));
    }
    Ok(label.to_string())
}

/// weight：池内加权随机用，必须 >= 1。
pub(crate) fn clean_weight(raw: Option<i32>) -> Result<Option<i32>, AppError> {
    match raw {
        None => Ok(None),
        Some(weight) if (1..=1000).contains(&weight) => Ok(Some(weight)),
        Some(_) => Err(AppError::bad_request("weight must be between 1 and 1000")),
    }
}

/// priority：数值小 = 优先，允许 0。
pub(crate) fn clean_priority(raw: Option<i32>) -> Result<Option<i32>, AppError> {
    match raw {
        None => Ok(None),
        Some(priority) if (0..=100_000).contains(&priority) => Ok(Some(priority)),
        Some(_) => Err(AppError::bad_request(
            "priority must be between 0 and 100000",
        )),
    }
}

/// 隐藏模型清洗：只接受字符串、去空、去重（保序）、上限 200。
pub(crate) fn normalize_hidden(items: &[Value]) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let Some(raw) = item.as_str() else {
            continue;
        };
        let model = raw.trim();
        if model.is_empty() {
            continue;
        }
        if model.chars().count() > MAX_MODEL_CHARS {
            return Err(AppError::bad_request(format!(
                "model name must be at most {MAX_MODEL_CHARS} characters"
            )));
        }
        if !out.iter().any(|seen| seen == model) {
            out.push(model.to_string());
        }
    }
    if out.len() > MAX_HIDDEN_MODELS {
        return Err(AppError::bad_request(format!(
            "at most {MAX_HIDDEN_MODELS} hidden models are allowed"
        )));
    }
    Ok(out)
}

/// 平台 preset 列表（旧响应形状，供没有租户目录的部署/租户回落）。
pub(crate) fn preset_providers() -> Vec<Value> {
    providers::PRESETS
        .iter()
        .map(|p| {
            json!({
                "provider": p.id,
                "label": p.label,
                "protocol": p.protocol.as_str(),
                "base_url": p.base_url,
                "model_prefixes": p.model_prefixes,
                "requires_key": p.requires_key,
                "model_discovery": p.model_discovery,
            })
        })
        .collect()
}

/// 用户侧模型清单：`models` 是启用目录（带每个模型可用的 provider），
/// `providers` 保持旧契约 = 平台 preset 清单（控制台用它做 provider 下拉）。
pub(crate) fn catalog_payload(models: &[EnabledModel], routes: &[(String, String)]) -> Value {
    let entries: Vec<Value> = models
        .iter()
        .map(|entry| {
            let mut route_providers: Vec<&str> = routes
                .iter()
                .filter(|(model, _)| same_model(model, &entry.model))
                .map(|(_, provider)| provider.as_str())
                .collect();
            route_providers.sort_unstable();
            route_providers.dedup();
            json!({
                "model": entry.model,
                "display_name": display_name_of(&entry.model, &entry.display_name),
                "providers": route_providers,
            })
        })
        .collect();
    json!({ "models": entries, "providers": preset_providers() })
}

// ── 租户库查询（用户侧）────────────────────────────────────────────────

/// 租户已启用目录（控制库）；控制面未配置时返回空 = 回落平台 preset。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct EnabledModel {
    pub model: String,
    pub display_name: String,
}

pub(crate) async fn enabled_models(
    state: &AppState,
    tenant: Uuid,
) -> Result<Vec<EnabledModel>, AppError> {
    let Some(control) = state.control.as_ref() else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query_as::<_, EnabledModel>(
        "SELECT model, display_name FROM tenant_models
          WHERE tenant_id = $1 AND enabled
          ORDER BY model ASC",
    )
    .bind(tenant)
    .fetch_all(&control.db)
    .await?;
    Ok(rows)
}

/// 提交消息前的目录校验：目录为空（未配置/单库形态）一律放行。
pub async fn ensure_model_allowed(
    state: &AppState,
    tenant: Option<Uuid>,
    model: &str,
) -> Result<(), AppError> {
    let Some(tenant) = tenant else {
        return Ok(());
    };
    let catalog: Vec<String> = enabled_models(state, tenant)
        .await?
        .into_iter()
        .map(|entry| entry.model)
        .collect();
    if catalog.is_empty() || model_in_catalog(&catalog, model) {
        return Ok(());
    }
    Err(AppError::new(
        StatusCode::FORBIDDEN,
        "model_not_allowed",
        format!("model `{model}` is not enabled for this tenant"),
    ))
}

// ── 普通用户 API ──────────────────────────────────────────────────────

pub async fn list_models(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    if let (Some(tenant), Some(control)) = (actor.tenant, state.control.as_ref()) {
        let models = enabled_models(&state, tenant).await?;
        if !models.is_empty() {
            let routes: Vec<(String, String)> = sqlx::query_as(
                "SELECT model, provider FROM tenant_model_routes
                  WHERE tenant_id = $1 AND enabled
                  ORDER BY model ASC, priority ASC, provider ASC",
            )
            .bind(tenant)
            .fetch_all(&control.db)
            .await?;
            return Ok(ok(catalog_payload(&models, &routes)));
        }
    }
    // 回落：旧租户/单库形态仍拿平台 preset 列表。
    Ok(ok(json!({ "models": [], "providers": preset_providers() })))
}

pub async fn list_hidden(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let models: Vec<String> = sqlx::query_scalar(
        "SELECT model FROM user_hidden_models
          WHERE user_id = $1 AND tenant_id = $2
          ORDER BY model ASC",
    )
    .bind(actor.id)
    .bind(actor.tenant_id)
    .fetch_all(&state.db())
    .await?;
    Ok(ok(json!({ "models": models })))
}

#[derive(Debug, Deserialize)]
pub struct PutHiddenBody {
    #[serde(default)]
    pub models: Value,
}

/// 覆盖写：去重、只接受字符串、≤200 条；空数组表示清空。
pub async fn put_hidden(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<PutHiddenBody>,
) -> ApiResult<Response> {
    let items = match body.models {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        _ => return Err(AppError::bad_request("models must be an array")),
    };
    let models = normalize_hidden(&items)?;

    let mut tx = state.db().begin().await?;
    sqlx::query("DELETE FROM user_hidden_models WHERE user_id = $1")
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
    for model in &models {
        sqlx::query(
            "INSERT INTO user_hidden_models (user_id, tenant_id, model)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, model) DO NOTHING",
        )
        .bind(actor.id)
        .bind(actor.tenant_id)
        .bind(model)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(ok(json!({ "models": models })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matching_accepts_prefixed_and_bare_names() {
        let catalog = vec!["gpt-5".to_string(), "qwen3-max".to_string()];
        assert!(model_in_catalog(&catalog, "gpt-5"));
        assert!(model_in_catalog(&catalog, "openai:gpt-5"));
        assert!(model_in_catalog(&catalog, " GPT-5 "));
        assert!(model_in_catalog(&catalog, "dashscope:qwen3-max"));
        assert!(!model_in_catalog(&catalog, "gpt-5-mini"));
        assert!(!model_in_catalog(&catalog, ""));
        assert!(!model_in_catalog(&[], "gpt-5"), "空目录由调用方回退");
        assert!(model_in_catalog(&["openai:gpt-5".to_string()], "gpt-5"));
    }

    #[test]
    fn hidden_models_are_deduped_and_cleaned() {
        let items = vec![
            json!("gpt-5"),
            json!(" gpt-5 "),
            json!("qwen3"),
            json!(42),
            json!(null),
            json!(""),
        ];
        assert_eq!(
            normalize_hidden(&items).expect("ok"),
            vec!["gpt-5".to_string(), "qwen3".to_string()]
        );
        assert!(normalize_hidden(&[]).expect("empty ok").is_empty());
    }

    #[test]
    fn hidden_models_are_capped() {
        let items: Vec<Value> = (0..=MAX_HIDDEN_MODELS)
            .map(|i| json!(format!("m{i}")))
            .collect();
        let err = normalize_hidden(&items).expect_err("too many");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("at most"), "{}", err.message);
    }

    #[test]
    fn upstream_default_strips_known_provider_prefixes() {
        assert_eq!(default_upstream_model("openai:gpt-5", "openai"), "gpt-5");
        assert_eq!(default_upstream_model("gpt-5", "openai"), "gpt-5");
        assert_eq!(
            default_upstream_model("dashscope:qwen3-max", "alibaba"),
            "qwen3-max",
        );
        // 非 `provider:model` 的冒号内容保持原样。
        assert_eq!(default_upstream_model("a:b:c", "x"), "a:b:c");
    }

    #[test]
    fn catalog_payload_groups_enabled_routes_per_model() {
        let models = vec![
            EnabledModel {
                model: "gpt-5".into(),
                display_name: "GPT-5".into(),
            },
            EnabledModel {
                model: "qwen3-max".into(),
                display_name: String::new(),
            },
        ];
        let routes = vec![
            ("gpt-5".to_string(), "openai".to_string()),
            ("gpt-5".to_string(), "openai".to_string()),
            ("openai:gpt-5".to_string(), "openrouter".to_string()),
            ("qwen3-max".to_string(), "dashscope".to_string()),
        ];
        let payload = catalog_payload(&models, &routes);
        let entries = payload["models"].as_array().expect("models");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["display_name"], "GPT-5");
        assert_eq!(
            entries[0]["providers"],
            json!(["openai", "openrouter"]),
            "同 provider 去重，前缀路由也归到同一模型"
        );
        assert_eq!(
            entries[1]["display_name"], "qwen3-max",
            "空 display_name 回落模型名"
        );
        assert_eq!(entries[1]["providers"], json!(["dashscope"]));

        // `providers` 保持旧契约：始终是完整平台 preset 清单（控制台用它做下拉）。
        let providers = payload["providers"].as_array().expect("providers");
        assert_eq!(providers.len(), providers::PRESETS.len());
        assert_eq!(providers[0]["provider"], "openai");
        assert_eq!(providers[0]["label"], "OpenAI");
    }

    #[test]
    fn model_and_weight_validation_bounds() {
        assert_eq!(clean_model("  gpt-5 ").unwrap(), "gpt-5");
        assert!(clean_model("").is_err());
        assert!(clean_model("gpt 5").is_err());
        assert!(clean_weight(Some(1)).unwrap() == Some(1));
        assert!(clean_weight(Some(0)).is_err());
        assert!(clean_weight(Some(1001)).is_err());
        assert!(clean_weight(None).unwrap().is_none());
        assert_eq!(clean_label(None).unwrap(), "default");
        assert_eq!(clean_label(Some(" pool-a ")).unwrap(), "pool-a");
        assert!(clean_priority(Some(0)).unwrap() == Some(0));
        assert!(clean_priority(Some(-1)).is_err());
    }
}
