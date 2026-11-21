//! S9 / P1-6：租户备份（pg_dump custom 格式）。
//!
//! 备份目录由 `CHIRON_BACKUP_DIR` 指定，空则禁用（默认不往宿主随便写文件）。
//! 密码通过 `PGPASSWORD` 环境变量传给 pg_dump，不出现在命令行与日志里。
//!
//! 两条入口复用同一段执行逻辑 [`run_backup`]：
//! * 手动 `POST /api/platform/v1/tenants/{id}/backup`（平台管理员，写审计）；
//! * 定时调度 [`spawn_scheduler`]：`CHIRON_BACKUP_SECS > 0` 才启动，leader 租约保证
//!   多实例只跑一份，逐租户执行，单个租户失败只记账不中断整轮。
//!
//! 保留策略（可选）：`CHIRON_BACKUP_KEEP_DAYS > 0` 时，成功备份后删除**本租户前缀**
//! 下早于该天数的 `.dump`；未配置（0）从不删文件 —— 备份删错不可恢复，必须显式开启。

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde_json::json;
use tokio::process::Command;
use url::Url;
use uuid::Uuid;

use crate::{
    control::{crypto, fetch_tenant, platform_audit, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
};

const BACKUP_TIMEOUT: Duration = Duration::from_secs(600);
const SCHEDULER_MIN_SECS: u64 = 300;

pub async fn backup_tenant(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    if !matches!(tenant.status.as_str(), "active" | "suspended") {
        return Err(AppError::conflict("tenant has no database to back up"));
    }
    if state.cfg.backup_dir.trim().is_empty() {
        return Err(backup_disabled());
    }

    let (file, bytes) = backup_locked(&state, &control.db, id, &tenant.slug).await?;
    prune_old(&state, &tenant.slug).await;

    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_backed_up",
        Some(id),
        &format!("file={file} bytes={bytes}"),
        "",
    )
    .await;

    Ok(Json(json!({ "data": { "file": file, "bytes": bytes } })).into_response())
}

/// 与迁移/物理删除互斥的备份入口：锁与备份在同一事务生命周期内。
async fn backup_locked(
    state: &AppState,
    control: &sqlx::PgPool,
    id: Uuid,
    slug: &str,
) -> Result<(String, u64), AppError> {
    let mut tx = control.begin().await?;
    if !super::try_tenant_op_lock(&mut tx, id).await? {
        return Err(AppError::conflict(
            "tenant migration or another backup is in progress",
        ));
    }
    let out = run_backup(state, id, slug).await?;
    tx.commit().await?;
    Ok(out)
}

/// 单个租户的一次备份：解析 DSN、落盘、返回 `(file, bytes)`。
///
/// 失败时清掉半截文件并把 pg_dump 的 stderr 脱敏后带出（密码替换为 `***`）。
pub(crate) async fn run_backup(
    state: &AppState,
    id: Uuid,
    slug: &str,
) -> Result<(String, u64), AppError> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let dir = state.cfg.backup_dir.trim().to_string();
    if dir.is_empty() {
        return Err(backup_disabled());
    }
    let dsn_enc: Option<String> = sqlx::query_scalar("SELECT dsn_enc FROM tenants WHERE id = $1")
        .bind(id)
        .fetch_one(&control.db)
        .await?;
    let dsn_enc = dsn_enc.ok_or_else(|| AppError::conflict("tenant is not provisioned"))?;
    let dsn = crypto::open(&state.cfg.secrets_key, id.as_bytes(), &dsn_enc).map_err(|msg| {
        tracing::error!(tenant = %id, error = %msg, "backup dsn decrypt failed");
        AppError::internal("tenant database credentials unavailable")
    })?;
    let mut url =
        Url::parse(&dsn).map_err(|e| AppError::internal(format!("invalid tenant dsn: {e}")))?;
    let password = url.password().unwrap_or_default().to_string();
    url.set_password(None)
        .map_err(|_| AppError::internal("cannot strip password from dsn"))?;

    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        tracing::error!(dir = %dir, error = %e, "create backup dir failed");
        AppError::internal("backup directory unavailable")
    })?;
    restrict_mode(&dir, 0o700).await;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let file = format!("{dir}/{}-{stamp}.dump", safe_segment(slug));
    // 先写 .partial，成功后再改名；超时/崩溃不会留下看起来有效的截断 dump。
    let partial = format!("{file}.partial");
    let mut cmd = Command::new("pg_dump");
    cmd.args([
        "--dbname",
        url.as_str(),
        "--format=custom",
        "--no-owner",
        "--file",
        &partial,
    ])
    .env("PGPASSWORD", &password)
    .env("PGCONNECT_TIMEOUT", "10")
    .kill_on_drop(true);
    let output = match tokio::time::timeout(BACKUP_TIMEOUT, cmd.output()).await {
        Ok(result) => result,
        Err(_) => {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(AppError::internal("pg_dump timed out"));
        }
    }
    .map_err(|e| {
            tracing::error!(error = %e, "pg_dump spawn failed");
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "backup_unavailable",
                "pg_dump is not available on this host",
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let encoded: String = url::form_urlencoded::byte_serialize(password.as_bytes()).collect();
        let safe = stderr.replace(&password, "***").replace(&encoded, "***");
        tracing::error!(%safe, "pg_dump failed");
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(AppError::internal(
            "pg_dump failed; see server logs for the redacted error",
        ));
    }
    tokio::fs::rename(&partial, &file).await.map_err(|e| {
        tracing::error!(error = %e, "backup rename failed");
        AppError::internal("backup finalize failed")
    })?;
    restrict_mode(&file, 0o600).await;
    let bytes = tokio::fs::metadata(&file)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    Ok((file, bytes))
}

/// 备份含全租户明文数据：目录 0700、文件 0600（非 Unix 平台交给部署 ACL）。
async fn restrict_mode(path: &str, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
        {
            tracing::warn!(path = %path, error = %e, "backup permission hardening failed");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn safe_segment(raw: &str) -> String {    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

fn control_unavailable() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

fn backup_disabled() -> AppError {
    AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "backup_disabled",
        "CHIRON_BACKUP_DIR is not configured",
    )
}

/// 从备份文件名里解析时间戳（`<slug>-<YYYYMMDDTHHMMSSZ>.dump`）。
fn parse_stamp(file_name: &str, slug: &str) -> Option<chrono::NaiveDateTime> {
    let prefix = format!("{slug}-");
    let rest = file_name.strip_prefix(&prefix)?;
    let stamp = rest.strip_suffix(".dump")?;
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()
}

/// 保留策略：只处理本租户前缀的文件，`keep_days == 0` 时什么都不做。
async fn prune_old(state: &AppState, slug: &str) {
    let keep_days = state.cfg.backup_keep_days;
    let dir = state.cfg.backup_dir.trim();
    if keep_days == 0 || dir.is_empty() {
        return;
    }
    let cutoff = Utc::now() - chrono::Duration::days(keep_days as i64);
    let cutoff = cutoff.naive_utc();
    let slug = safe_segment(slug);
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let mut removed = 0usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stamp) = parse_stamp(&name, &slug) else {
            continue;
        };
        if stamp >= cutoff {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(file = %name, error = %e, "backup prune failed"),
        }
    }
    if removed > 0 {
        tracing::info!(tenant = %slug, removed, keep_days, "old backups pruned");
    }
}

/// 定时备份：leader 租约 + 逐租户执行；`CHIRON_BACKUP_SECS=0` 或未配置备份目录时关闭。
pub fn spawn_scheduler(state: Arc<AppState>) {
    let secs = state.cfg.backup_secs;
    if secs == 0 || state.cfg.backup_dir.trim().is_empty() {
        tracing::info!("backup scheduler disabled");
        return;
    }
    if state.cfg.control_database_url.trim().is_empty() {
        tracing::warn!("backup scheduler disabled: control database not configured");
        return;
    }
    tokio::spawn(async move {
        let every = Duration::from_secs(secs.max(SCHEDULER_MIN_SECS));
        loop {
            tokio::time::sleep(every).await;
            let Some(control) = state.control.clone() else {
                return;
            };
            let owner = crate::sessions::lease::owner_id();
            let key = crate::sessions::lease::leader_key("backup");
            match state.leases.acquire(&key, &owner, every * 2).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "backup leader lease failed");
                    continue;
                }
            }
            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT id, slug FROM tenants
                  WHERE status = 'active' AND dsn_enc IS NOT NULL
                  ORDER BY slug ASC",
            )
            .fetch_all(&control.db)
            .await
            .unwrap_or_default();
            let (mut ok, mut failed) = (0usize, 0usize);
            for (id, slug) in rows {
                match tokio::time::timeout(
                    BACKUP_TIMEOUT,
                    backup_locked(&state, &control.db, id, &slug),
                )
                .await
                {
                    Ok(Ok((file, bytes))) => {
                        ok += 1;
                        prune_old(&state, &slug).await;
                        platform_audit(
                            &state,
                            None,
                            "scheduler",
                            "tenant_backed_up",
                            Some(id),
                            &format!("file={file} bytes={bytes} scheduled=true"),
                            "",
                        )
                        .await;
                    }
                    Ok(Err(e)) => {
                        failed += 1;
                        tracing::warn!(
                            tenant = %slug, code = %e.code, error = %e.message,
                            "scheduled backup failed"
                        );
                        crate::alerts::notify(
                            &state,
                            "backup_failed",
                            Some(&slug),
                            &format!("code={} {}", e.code, e.message),
                        );
                    }
                    Err(_) => {
                        failed += 1;
                        tracing::warn!(tenant = %slug, "scheduled backup timed out");
                        crate::alerts::notify(&state, "backup_timeout", Some(&slug), "pg_dump timeout");
                    }
                }
            }
            tracing::info!(ok, failed, "scheduled tenant backup finished");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_filename_is_sanitized() {
        assert_eq!(safe_segment("acme/../x"), "acme____x");
        assert_eq!(safe_segment("Acme-Team_1").len(), 11);
    }

    #[test]
    fn stamp_parsing_is_scoped_to_tenant_prefix() {
        let parsed = parse_stamp("acme-20260101T030000Z.dump", "acme").expect("stamp");
        assert_eq!(
            parsed,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .expect("date")
                .and_hms_opt(3, 0, 0)
                .expect("time")
        );
        // 其它租户前缀 / 非 .dump / 非法时间戳都不认，避免误删。
        assert!(parse_stamp("acme-x-20260101T030000Z.dump", "acme-x").is_some());
        assert!(parse_stamp("acme2-20260101T030000Z.dump", "acme").is_none());
        assert!(parse_stamp("acme-20260101T030000Z.sql", "acme").is_none());
        assert!(parse_stamp("acme-latest.dump", "acme").is_none());
    }
}
