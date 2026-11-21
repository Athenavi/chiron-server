//! S9（核心）：租户导出与物理删除。
//!
//! 导出：按固定表清单用 `json_agg` 打包成一份 JSON（带 Content-Disposition 下载），
//! 供停用/迁移前留档；大表有行数上限，避免把控制面内存打爆。
//! 删除：必须先 suspend，确认 slug 后终止连接、DROP DATABASE/ROLE、释放域名与 BYOK，
//! 租户行保留为 deleted（审计需要），原 slug 加后缀释放以便复用。

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::{
    control::{crypto, fetch_tenant, platform_audit, provision, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
};

const EXPORT_TABLES: &[&str] = &[
    "users",
    "audit_logs",
    "sessions",
    "messages",
    "turns",
    "runs",
    "tool_calls",
    "approvals",
    "idempotency_keys",
    "usage_events",
    "subagent_runs",
    "checkpoints",
    "memories",
    "mcp_servers",
    "plugins",
    "skills",
    "market_items",
    "user_containers",
    "background_jobs",
];
const EXPORT_ROW_CAP: i64 = 50_000;

async fn tenant_dsn(state: &AppState, tenant_id: Uuid, dsn_enc: &str) -> Result<String, AppError> {
    crypto::open(&state.cfg.secrets_key, tenant_id.as_bytes(), dsn_enc).map_err(|msg| {
        tracing::error!(tenant = %tenant_id, error = %msg, "tenant dsn decrypt failed");
        AppError::internal("tenant database credentials unavailable")
    })
}

pub async fn export_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    if !matches!(tenant.status.as_str(), "active" | "suspended") {
        return Err(AppError::conflict("tenant has no database to export"));
    }
    let dsn_enc: Option<String> = sqlx::query_scalar("SELECT dsn_enc FROM tenants WHERE id = $1")
        .bind(id)
        .fetch_one(&control.db)
        .await?;
    let dsn_enc = dsn_enc.ok_or_else(|| AppError::conflict("tenant is not provisioned"))?;
    let dsn = tenant_dsn(&state, id, &dsn_enc).await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&dsn)
        .await
        .map_err(|e| {
            tracing::error!(tenant = %id, error = %e, "export connect failed");
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "tenant_database_unavailable",
                "tenant database unavailable",
            )
        })?;

    let mut tables = Map::new();
    for table in EXPORT_TABLES {
        // 导出给迁移/留档，但哈希与 MCP 凭证不是可迁移业务数据。
        let scrub = match *table {
            "users" => " - 'password_hash'",
            "mcp_servers" => " - 'env' - 'headers'",
            _ => "",
        };
        let sql = format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(t){scrub}), '[]'::jsonb)
               FROM (SELECT * FROM {table} LIMIT {EXPORT_ROW_CAP}) t"
        );
        let value: Value = sqlx::query_scalar(&sql)
            .fetch_one(&pool)
            .await
            .map_err(|e| format!("export {table}: {e}"))
            .map_err(AppError::internal)?;
        tables.insert((*table).to_string(), value);
    }
    pool.close().await;

    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_exported",
        Some(id),
        "tables=19",
        "",
    )
    .await;

    let body = Json(json!({
        "data": {
            "tenant": { "id": tenant.id, "slug": tenant.slug, "name": tenant.name },
            "exported_at": Utc::now(),
            "tables": tables,
        }
    }))
    .into_response();
    let mut resp = body;
    let filename = format!("chiron-{}-export.json", tenant.slug);
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        resp.headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct DeleteBody {
    pub confirm: String,
    /// 默认 true：删除物理库前先做一次完整备份，备份失败则拒绝删除。
    #[serde(default = "backup_required")]
    pub backup_before: bool,
}

fn backup_required() -> bool {
    true
}

pub fn can_delete(status: &str, confirm: &str, slug: &str) -> Result<(), AppError> {
    if status != "suspended" {
        return Err(AppError::conflict("suspend the tenant before deleting it"));
    }
    if confirm.trim() != slug {
        return Err(AppError::bad_request("confirm must equal the tenant slug"));
    }
    Ok(())
}

pub async fn delete_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
    Json(body): Json<DeleteBody>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    can_delete(&tenant.status, &body.confirm, &tenant.slug)?;

    // 与迁移/备份互斥；锁随事务提交/回滚释放，提前返回也不会泄漏。
    let mut op_tx = control.db.begin().await?;
    if !super::try_tenant_op_lock(&mut op_tx, id).await? {
        return Err(AppError::conflict(
            "tenant migration or backup is in progress; retry later",
        ));
    }
    let backup_file = if body.backup_before {
        if state.cfg.backup_dir.trim().is_empty() {
            return Err(AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backup_required",
                "set CHIRON_BACKUP_DIR or pass backup_before=false to delete",
            ));
        }
        let (file, _) = super::backup::run_backup(&state, id, &tenant.slug).await?;
        Some(file)
    } else {
        None
    };

    let admin_url = state.cfg.provisioning_database().to_string();
    if admin_url.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            "PROVISION_DATABASE_URL / CONTROL_DATABASE_URL is not configured",
        ));
    }
    let db = tenant.db_name.trim().to_string();
    if db.is_empty()
        || !db
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(AppError::internal(
            "tenant db_name is not a safe identifier",
        ));
    }

    let admin_pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&admin_url)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "delete admin connect failed");
            AppError::internal("provisioning database unavailable")
        })?;
    let _ = sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
          WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(&db)
    .execute(&admin_pool)
    .await;
    let drop_db = format!("DROP DATABASE IF EXISTS {}", provision::quote_ident(&db));
    sqlx::query(&drop_db)
        .execute(&admin_pool)
        .await
        .map_err(|e| AppError::internal(format!("drop database failed: {e}")))?;
    let drop_role = format!("DROP ROLE IF EXISTS {}", provision::quote_ident(&db));
    let role_dropped = sqlx::query(&drop_role)
        .execute(&admin_pool)
        .await
        .map(|_| true)
        .unwrap_or(false);
    admin_pool.close().await;

    let short = id.to_string();
    let short = &short[..8.min(short.len())];
    sqlx::query(
        "UPDATE tenants
            SET status = 'deleted', slug = $2, db_name = $3, dsn_enc = NULL, db_user = '',
                owner_temp_password_enc = NULL, provision_error = '',
                updated_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(format!("{}-deleted-{short}", tenant.slug))
    .bind(format!("chiron_deleted_{short}"))
    .execute(&mut *op_tx)
    .await?;
    sqlx::query("DELETE FROM tenant_domains WHERE tenant_id = $1")
        .bind(id)
        .execute(&mut *op_tx)
        .await?;
    sqlx::query("DELETE FROM tenant_provider_keys WHERE tenant_id = $1")
        .bind(id)
        .execute(&mut *op_tx)
        .await?;
    op_tx.commit().await?;

    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_deleted",
        Some(id),
        &format!(
            "db={db} role_dropped={role_dropped} backup={}",
            backup_file.as_deref().unwrap_or("skipped")
        ),
        "",
    )
    .await;

    Ok(Json(json!({
        "data": {
            "tenant_id": id,
            "database_dropped": db,
            "role_dropped": role_dropped,
            "backup_file": backup_file,
        }
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
    fn delete_requires_suspend_and_slug_confirmation() {
        assert!(can_delete("active", "acme", "acme").is_err());
        assert!(can_delete("suspended", "wrong", "acme").is_err());
        assert!(can_delete("suspended", "acme", "acme").is_ok());
    }

    #[test]
    fn delete_backs_up_unless_explicitly_skipped() {
        let default: DeleteBody = serde_json::from_str(r#"{"confirm":"acme"}"#).unwrap();
        assert!(default.backup_before);
        let skipped: DeleteBody =
            serde_json::from_str(r#"{"confirm":"acme","backup_before":false}"#).unwrap();
        assert!(!skipped.backup_before);
    }
}
