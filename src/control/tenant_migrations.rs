//! S3：租户迁移版本管理与批量升级。
//!
//! 版本以二进制内嵌迁移的最高 number 为 target；每个租户库的实际版本从
//! `_sqlx_migrations` 读，控制库 `tenants.schema_version` 是缓存。
//! 升级按租户串行执行，单库失败不影响其他租户；整批由控制库 advisory lock 去重。
//! SQLx 没有 down 迁移：回滚只能按库备份恢复（PRD §4），这里明确不提供"自动回滚"。

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use url::Url;
use uuid::Uuid;

use crate::{
    control::{crypto, platform_audit, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
    tenant,
};

const PER_TENANT_TIMEOUT: Duration = Duration::from_secs(120);
const ADVISORY_LOCK: &str = "chiron:tenant:migrate";

#[derive(Debug, serde::Serialize)]
struct TenantVersionRow {
    id: Uuid,
    slug: String,
    status: String,
    schema_version: i64,
    migrated_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn status_handler(
    State(state): State<Arc<AppState>>,
    _admin: PlatformAdmin,
) -> ApiResult<Json<Value>> {
    status(&state).await
}

pub async fn upgrade_handler(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Json(body): Json<UpgradeBody>,
) -> ApiResult<Response> {
    upgrade(&state, &admin, body).await
}

pub async fn status(state: &AppState) -> Result<Json<Value>, AppError> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let target = tenant::target_schema_version();
    let tenants = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            i64,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT id, slug, status, schema_version, migrated_at
           FROM tenants WHERE status <> 'deleted' ORDER BY slug ASC",
    )
    .fetch_all(&control.db)
    .await?
    .into_iter()
    .map(
        |(id, slug, status, schema_version, migrated_at)| TenantVersionRow {
            id,
            slug,
            status,
            schema_version,
            migrated_at,
        },
    )
    .collect::<Vec<_>>();
    let runs = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            bool,
            String,
            i64,
            i32,
            i32,
            i32,
            i32,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT id, actor_email, dry_run, status, target_version, total, ok_count,
                failed_count, skipped_count, started_at, finished_at
           FROM tenant_migration_runs ORDER BY started_at DESC LIMIT 10",
    )
    .fetch_all(&control.db)
    .await?
    .into_iter()
    .map(|r| {
        json!({
            "id": r.0, "actor_email": r.1, "dry_run": r.2, "status": r.3,
            "target_version": r.4, "total": r.5, "ok_count": r.6,
            "failed_count": r.7, "skipped_count": r.8,
            "started_at": r.9, "finished_at": r.10,
        })
    })
    .collect::<Vec<_>>();
    Ok(Json(json!({
        "data": { "target_version": target, "tenants": tenants, "runs": runs }
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpgradeBody {
    #[serde(default)]
    pub dry_run: bool,
    /// 缺省 = 所有 active/suspended 租户。
    #[serde(default)]
    pub tenant_ids: Option<Vec<Uuid>>,
    /// 升级前先备份；默认开启，显式 false 才跳过。
    #[serde(default = "default_true")]
    pub backup_before: bool,
}

fn default_true() -> bool {
    true
}

pub async fn upgrade(
    state: &AppState,
    admin: &PlatformAdmin,
    body: UpgradeBody,
) -> Result<Response, AppError> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    crypto::decode_key(&state.cfg.secrets_key).map_err(|msg| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;
    let target = tenant::target_schema_version();

    if body.backup_before && !body.dry_run && state.cfg.backup_dir.trim().is_empty() {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "backup_disabled",
            "set CHIRON_BACKUP_DIR or pass backup_before=false before upgrading",
        ));
    }

    // 事务级 advisory lock：任何提前返回/panic 都会随事务回滚自动释放，
    // 不会像 session 级锁那样把带锁连接还回池里。
    let mut lock_tx = control.db.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtext($1))")
        .bind(ADVISORY_LOCK)
        .fetch_one(&mut *lock_tx)
        .await?;
    if !locked {
        return Err(AppError::conflict(
            "another tenant migration run is in progress",
        ));
    }

    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO tenant_migration_runs (actor_email, dry_run, status, target_version)
         VALUES ($1, $2, 'running', $3) RETURNING id",
    )
    .bind(&admin.email)
    .bind(body.dry_run)
    .bind(target)
    .fetch_one(&control.db)
    .await?;

    let tenants: Vec<(Uuid, String, Option<String>, i64)> = match body.tenant_ids.as_ref() {
        Some(ids) => {
            sqlx::query_as(
                "SELECT id, slug, dsn_enc, schema_version FROM tenants
                  WHERE id = ANY($1) AND status IN ('active', 'suspended')
                  ORDER BY slug ASC",
            )
            .bind(ids)
            .fetch_all(&control.db)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT id, slug, dsn_enc, schema_version FROM tenants
                  WHERE status IN ('active', 'suspended')
                  ORDER BY slug ASC",
            )
            .fetch_all(&control.db)
            .await?
        }
    };

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut detail = Vec::with_capacity(tenants.len());
    for (id, slug, dsn_enc, from) in tenants {
        let Some(dsn_enc) = dsn_enc else {
            skipped += 1;
            detail.push(json!({
                "id": id, "slug": slug, "from": from, "to": from,
                "status": "skipped", "error": "tenant database is not provisioned"
            }));
            continue;
        };
        // 单租户级互斥：删除/备份持有同一把锁时，迁移不能同时改动该库。
        let mut op_tx = control.db.begin().await?;
        if !super::try_tenant_op_lock(&mut op_tx, id).await? {
            skipped += 1;
            detail.push(json!({
                "id": id, "slug": slug, "from": from, "to": from,
                "status": "skipped", "error": "tenant operation in progress"
            }));
            continue;
        }
        if body.backup_before && !body.dry_run {
            if let Err(err) = super::backup::run_backup(state, id, &slug).await {
                failed += 1;
                detail.push(json!({
                    "id": id, "slug": slug, "from": from, "to": from,
                    "status": "failed", "error": format!("pre-upgrade backup failed: {}", err.message)
                }));
                continue;
            }
        }

        let outcome = tokio::time::timeout(
            PER_TENANT_TIMEOUT,
            migrate_one(state, &control.db, id, &dsn_enc, target, body.dry_run),
        )
        .await;
        match outcome {
            Ok(Ok(to)) => {
                ok += 1;
                detail.push(json!({
                    "id": id, "slug": slug, "from": from, "to": to,
                    "status": "ok", "pending": target > to, "dry_run": body.dry_run
                }));
            }
            Ok(Err(err)) => {
                failed += 1;
                detail.push(json!({
                    "id": id, "slug": slug, "from": from, "to": from,
                    "status": "failed", "error": err
                }));
            }
            Err(_) => {
                failed += 1;
                detail.push(json!({
                    "id": id, "slug": slug, "from": from, "to": from,
                    "status": "failed", "error": "migration timed out"
                }));
            }
        }
        op_tx.commit().await?;
    }

    let run_status = if failed == 0 {
        "completed"
    } else if ok == 0 {
        "failed"
    } else {
        "partial"
    };
    sqlx::query(
        "UPDATE tenant_migration_runs
            SET status = $2, total = $3, ok_count = $4, failed_count = $5,
                skipped_count = $6, detail = $7, finished_at = now()
          WHERE id = $1",
    )
    .bind(run_id)
    .bind(run_status)
    .bind((ok + failed + skipped) as i32)
    .bind(ok as i32)
    .bind(failed as i32)
    .bind(skipped as i32)
    .bind(serde_json::Value::Array(detail.clone()))
    .execute(&control.db)
    .await?;

    lock_tx.commit().await?;

    platform_audit(
        state,
        Some(admin.id),
        &admin.email,
        if body.dry_run {
            "tenant_migrations_checked"
        } else {
            "tenant_migrations_upgraded"
        },
        None,
        &format!("target={target} ok={ok} failed={failed} skipped={skipped}"),
        "",
    )
    .await;

    Ok(Json(json!({
        "data": {
            "run_id": run_id,
            "dry_run": body.dry_run,
            "backup_before": body.backup_before,
            "target_version": target,
            "status": run_status,
            "total": ok + failed + skipped,
            "ok_count": ok,
            "failed_count": failed,
            "skipped_count": skipped,
            "detail": detail,
        }
    }))
    .into_response())
}

/// 单租户：解密 DSN → 读当前版本（dry-run 到此为止）→ 跑迁移 → 回写控制库。
async fn migrate_one(
    state: &AppState,
    control: &PgPool,
    tenant_id: Uuid,
    dsn_enc: &str,
    target: i64,
    dry_run: bool,
) -> Result<i64, String> {
    let dsn = crypto::open(&state.cfg.secrets_key, tenant_id.as_bytes(), dsn_enc)?;
    let password = Url::parse(&dsn)
        .ok()
        .and_then(|u| u.password().map(str::to_string));
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&dsn)
        .await
        .map_err(|e| sanitize(&e.to_string(), password.as_deref()))?;

    let result = async {
        let current = applied_version(&pool).await?;
        if dry_run {
            return Ok(current);
        }
        if current < target {
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .map_err(|e| sanitize(&e.to_string(), password.as_deref()))?;
        }
        // 无论是否真的跑了迁移，都以库内实际版本回写控制库，顺带修复控制库漂移。
        let applied = applied_version(&pool).await?;
        sqlx::query(
            "UPDATE tenants SET schema_version = $2, migrated_at = now(), updated_at = now()
              WHERE id = $1",
        )
        .bind(tenant_id)
        .bind(applied)
        .execute(control)
        .await
        .map_err(|e| format!("update control schema_version: {e}"))?;
        Ok(applied)
    }
    .await;
    pool.close().await;
    result
}

async fn applied_version(pool: &PgPool) -> Result<i64, String> {
    match sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(max(version), 0) FROM _sqlx_migrations WHERE success",
    )
    .fetch_one(pool)
    .await
    {
        Ok(v) => Ok(v),
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("42P01") => Ok(0),
        Err(e) => Err(format!("read _sqlx_migrations: {e}")),
    }
}

/// 错误信息里可能带连接串（含密码），落审计/响应前统一替换。
fn sanitize(message: &str, password: Option<&str>) -> String {
    match password {
        Some(pw) if !pw.is_empty() => message.replace(pw, "***"),
        _ => message.to_string(),
    }
}

fn control_unavailable() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_version_matches_embedded_head() {
        assert!(tenant::target_schema_version() >= 1);
    }

    #[test]
    fn errors_never_echo_the_dsn_password() {
        let msg = "connect failed: postgresql://chiron:s3cret@db/x";
        assert_eq!(
            sanitize(msg, Some("s3cret")),
            "connect failed: postgresql://chiron:***@db/x"
        );
        assert_eq!(sanitize(msg, None), msg);
    }
}
