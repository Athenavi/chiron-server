//! S5：租户 BYOK（平台侧写入，运行时仅内存可见）。
//!
//! 明文 key 只出现在写请求与运行中的进程内；控制库仅存 AES-256-GCM 密文，
//! AAD = `<tenant_uuid>:<provider>`，密文挪到别的租户/provider 会解密失败。
//! 接口从不回显 key，只返回 provider 与更新时间。
//!
//! 平台侧语义（契约）：PUT/LIST/DELETE 只读写 `label='default'` 这一条。
//! `tenant_provider_keys` 同时是租户级 key 池（0007 起同 provider 可按 label 挂
//! 多条），其余 label 归租户管理员在控制台维护，平台删除**不得**连带清掉。
//! GET 仍按 provider 聚合全部 label 的条数供平台观察池规模，并单列 default 是否存在。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    control::{crypto, fetch_tenant, platform_audit, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
    tenant::key_aad,
};

/// 平台侧唯一读写的 label；其余 label 由租户管理员维护。
pub const PLATFORM_KEY_LABEL: &str = "default";

/// 平台列表：按 provider 聚合全部 label（`keys_count` 便于判断池规模），
/// 另外单列 `label=$2` 是否存在/何时更新，平台只对这条负责。
const LIST_KEYS_SQL: &str = "\
SELECT provider,
       max(updated_at) AS updated_at,
       count(*)::bigint AS keys_count,
       bool_or(label = $2) AS default_present,
       max(updated_at) FILTER (WHERE label = $2) AS default_updated_at
  FROM tenant_provider_keys
 WHERE tenant_id = $1
 GROUP BY provider
 ORDER BY provider ASC";

/// 平台写入固定落 `label='default'`；label 以参数绑定，不拼进 SQL。
const UPSERT_DEFAULT_KEY_SQL: &str = "\
INSERT INTO tenant_provider_keys (tenant_id, provider, key_enc, label)
VALUES ($1, $2, $3, $4)
ON CONFLICT (tenant_id, provider, label) DO UPDATE
   SET key_enc = EXCLUDED.key_enc, updated_at = now()";

/// 平台删除只清 `label='default'`；租户自建的其它 label 必须保留。
const DELETE_DEFAULT_KEY_SQL: &str = "\
DELETE FROM tenant_provider_keys
 WHERE tenant_id = $1 AND provider = $2 AND label = $3";

#[derive(sqlx::FromRow, serde::Serialize)]
struct KeyRow {
    provider: String,
    /// 全部 label 里最近的更新时间（含租户自建 label）。
    updated_at: DateTime<Utc>,
    /// 该 provider 的 key 条数（池化后同一 provider 可有多条，这里按 provider 聚合）。
    keys_count: i64,
    /// 平台写入的 `label='default'` 是否存在。
    default_present: bool,
    /// `label='default'` 的最近更新时间；不存在时为 null。
    default_updated_at: Option<DateTime<Utc>>,
}

pub async fn list_keys(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    fetch_tenant(&control.db, id).await?;
    let rows = sqlx::query_as::<_, KeyRow>(LIST_KEYS_SQL)
        .bind(id)
        .bind(PLATFORM_KEY_LABEL)
        .fetch_all(&control.db)
        .await?;
    Ok(Json(json!({ "data": { "keys": rows } })).into_response())
}

#[derive(Debug, Deserialize)]
pub struct PutKeyBody {
    pub provider: String,
    pub api_key: String,
}

pub async fn put_key(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Json(body): Json<PutKeyBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let provider = body.provider.trim().to_ascii_lowercase();
    if crate::providers::find(&provider).is_none() {
        return Err(AppError::bad_request("unknown provider preset"));
    }
    let api_key = body.api_key.trim();
    if api_key.len() < 8 {
        return Err(AppError::bad_request("api_key looks too short"));
    }
    let enc = crypto::seal(
        &state.cfg.secrets_key,
        key_aad(id, &provider).as_bytes(),
        api_key,
    )
    .map_err(|msg| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;
    sqlx::query(UPSERT_DEFAULT_KEY_SQL)
        .bind(id)
        .bind(&provider)
        .bind(enc)
        .bind(PLATFORM_KEY_LABEL)
        .execute(&control.db)
        .await?;
    if let Some(registry) = state.tenants.as_ref() {
        registry.invalidate_keys(id);
    }
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_provider_key_updated",
        Some(tenant.id),
        &format!("provider={provider} label={PLATFORM_KEY_LABEL}"),
        "",
    )
    .await;
    Ok(Json(json!({
        "data": { "provider": provider, "label": PLATFORM_KEY_LABEL, "updated": true }
    }))
    .into_response())
}

pub async fn delete_key(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path((id, provider)): Path<(Uuid, String)>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let provider = provider.trim().to_ascii_lowercase();
    // 只删平台自己写入的 label；租户管理员建的其他 label 一律保留，
    // 没有 default 行时按 404 返回（不能把"删到了别的 label"当成成功）。
    let affected = sqlx::query(DELETE_DEFAULT_KEY_SQL)
        .bind(id)
        .bind(&provider)
        .bind(PLATFORM_KEY_LABEL)
        .execute(&control.db)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found(
            "default provider key not found (tenant-managed labels are not deleted by the platform)",
        ));
    }
    if let Some(registry) = state.tenants.as_ref() {
        registry.invalidate_keys(id);
    }
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_provider_key_deleted",
        Some(tenant.id),
        &format!("provider={provider} label={PLATFORM_KEY_LABEL}"),
        "",
    )
    .await;
    Ok(Json(json!({
        "data": { "provider": provider, "label": PLATFORM_KEY_LABEL, "deleted": true }
    }))
    .into_response())
}

fn control_unavailable() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_label_is_default() {
        assert_eq!(PLATFORM_KEY_LABEL, "default");
    }

    #[test]
    fn delete_sql_targets_only_the_default_label() {
        let sql = DELETE_DEFAULT_KEY_SQL.to_ascii_lowercase();
        assert!(sql.starts_with("delete from tenant_provider_keys"));
        // 删除必须同时限定 tenant/provider/label 三个条件。
        assert!(
            sql.contains("where tenant_id = $1 and provider = $2 and label = $3"),
            "删除缺少 label 条件会误删租户自建 label：{sql}"
        );
    }

    #[test]
    fn upsert_and_list_bind_label_as_parameter() {
        assert!(UPSERT_DEFAULT_KEY_SQL.contains("VALUES ($1, $2, $3, $4)"));
        assert!(UPSERT_DEFAULT_KEY_SQL.contains("ON CONFLICT (tenant_id, provider, label)"));
        assert!(LIST_KEYS_SQL.contains("label = $2"));
        // label 一律参数化，不在 SQL 里硬编码。
        assert!(!UPSERT_DEFAULT_KEY_SQL.contains("'default'"));
        assert!(!LIST_KEYS_SQL.contains("'default'"));
        assert!(!DELETE_DEFAULT_KEY_SQL.contains("'default'"));
    }

    #[test]
    fn platform_delete_leaves_tenant_managed_labels_intact() {
        // 纯逻辑模拟删除后的行集合：平台范围只有 label='default'。
        let rows = vec![
            ("openai".to_string(), "default".to_string()),
            ("openai".to_string(), "team-a".to_string()),
            ("openai".to_string(), "team-b".to_string()),
            ("anthropic".to_string(), "default".to_string()),
        ];
        let deleted: Vec<_> = rows
            .iter()
            .filter(|(provider, label)| provider == "openai" && label == PLATFORM_KEY_LABEL)
            .collect();
        assert_eq!(deleted.len(), 1, "每个 provider 至多删掉 default 一条");
        let after: Vec<(String, String)> = rows
            .into_iter()
            .filter(|(provider, label)| {
                !(provider == "openai" && label == PLATFORM_KEY_LABEL)
            })
            .collect();
        assert_eq!(
            after,
            vec![
                ("openai".to_string(), "team-a".to_string()),
                ("openai".to_string(), "team-b".to_string()),
                ("anthropic".to_string(), "default".to_string()),
            ],
            "租户自建 label 与其他 provider 的 default 都保留"
        );
    }

    #[test]
    fn list_row_keeps_aggregate_and_adds_default_columns() {
        let now = Utc::now();
        let row = KeyRow {
            provider: "openai".into(),
            updated_at: now,
            keys_count: 3,
            default_present: true,
            default_updated_at: Some(now),
        };
        let value = serde_json::to_value(&row).expect("json");
        // 旧字段仍在（向后兼容）。
        assert_eq!(value["provider"], "openai");
        assert_eq!(value["keys_count"], 3);
        assert!(value["updated_at"].is_string());
        // 新增字段说明 default 是否存在。
        assert_eq!(value["default_present"], true);
        assert!(value["default_updated_at"].is_string());
        // 不回显任何 key 内容。
        assert!(!value.to_string().to_ascii_lowercase().contains("key_enc"));
    }
}
