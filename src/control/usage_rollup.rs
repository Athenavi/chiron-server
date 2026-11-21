//! S4：把各租户库的 usage_events 汇总到控制库 `tenant_usage_daily`。
//!
//! 平台侧按需触发 + 后台 leader 租约定时任务；窗口内按天重算并覆盖，避免重复累加。
//!
//! 存储字节数（P1-4）口径：
//! * `sum(media_assets.bytes)`：租户库的媒体元数据（每个租户一个库，故不按 tenant_id 过滤）；
//! * 工作区目录：宿主 `<SANDBOX_DATA_ROOT>/<tenant>` 的递归文件大小。请求态目录段是
//!   租户 uuid 前 8 位，后台/非请求态是完整 uuid，两种历史命名都尝试累加；
//! * 任一项失败都只告警并按 0 计，**不让整个 rollup 失败**（存储统计是尽力而为）。
//!   宿主路径不可见（容器内未挂载 data_root）时工作区部分恒为 0。
//! * 存储是"当前快照"而非当日增量：只写进当天那一行，其它天写 0，避免跨天重复累加。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::{
    control::{crypto, platform_audit, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
};

const PER_TENANT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct RollupBody {
    #[serde(default)]
    pub days: Option<i32>,
}

pub async fn rollup_handler(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    body: Option<Json<RollupBody>>,
) -> ApiResult<Response> {
    let body = body.map(|Json(b)| b).unwrap_or(RollupBody { days: None });
    let days = body.days.unwrap_or(30).clamp(1, 90);
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    crypto::decode_key(&state.cfg.secrets_key).map_err(|msg| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;

    let tenants: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, slug, dsn_enc FROM tenants
          WHERE status IN ('active', 'suspended') ORDER BY slug ASC",
    )
    .fetch_all(&control.db)
    .await?;

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut rows_total = 0usize;
    let mut detail = Vec::with_capacity(tenants.len());
    for (id, slug, dsn_enc) in tenants {
        let Some(dsn_enc) = dsn_enc else {
            failed += 1;
            detail.push(json!({ "id": id, "slug": slug, "error": "not provisioned" }));
            continue;
        };
        let outcome = tokio::time::timeout(
            PER_TENANT_TIMEOUT,
            rollup_one(&state, &control.db, id, &dsn_enc, days),
        )
        .await;
        match outcome {
            Ok(Ok(rows)) => {
                ok += 1;
                rows_total += rows;
                detail.push(json!({ "id": id, "slug": slug, "days": rows }));
            }
            Ok(Err(err)) => {
                failed += 1;
                detail.push(json!({ "id": id, "slug": slug, "error": err }));
            }
            Err(_) => {
                failed += 1;
                detail.push(json!({ "id": id, "slug": slug, "error": "rollup timed out" }));
            }
        }
    }

    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_usage_rollup",
        None,
        &format!("days={days} ok={ok} failed={failed} rows={rows_total}"),
        "",
    )
    .await;

    Ok(Json(json!({
        "data": {
            "days": days,
            "tenants_ok": ok,
            "tenants_failed": failed,
            "rows": rows_total,
            "detail": detail,
        }
    }))
    .into_response())
}

pub(crate) async fn rollup_one(
    state: &AppState,
    control: &sqlx::PgPool,
    tenant_id: Uuid,
    dsn_enc: &str,
    days: i32,
) -> Result<usize, String> {
    let dsn = crypto::open(&state.cfg.secrets_key, tenant_id.as_bytes(), dsn_enc)?;
    let password = url::Url::parse(&dsn)
        .ok()
        .and_then(|u| u.password().map(str::to_string));
    let sanitize = |m: String| match password.as_deref() {
        Some(pw) if !pw.is_empty() => m.replace(pw, "***"),
        _ => m,
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&dsn)
        .await
        .map_err(|e| sanitize(format!("connect tenant database: {e}")))?;

    let usage: Vec<(chrono::NaiveDate, i64, i64, i64)> = sqlx::query_as(
        "SELECT (created_at AT TIME ZONE 'UTC')::date AS day,
                COALESCE(sum(input_tokens + cache_read_input_tokens
                             + cache_creation_input_tokens), 0)::bigint AS tin,
                COALESCE(sum(output_tokens), 0)::bigint AS tout,
                COALESCE(round(sum(cost) * 100), 0)::bigint AS cost_cents
           FROM usage_events
          WHERE created_at >= now() - make_interval(days => $1)
          GROUP BY day ORDER BY day ASC",
    )
    .bind(days)
    .fetch_all(&pool)
    .await
    .map_err(|e| sanitize(format!("aggregate usage: {e}")))?;

    let runs: Vec<(chrono::NaiveDate, i64)> = sqlx::query_as(
        "SELECT (created_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint
           FROM runs
          WHERE created_at >= now() - make_interval(days => $1)
          GROUP BY day ORDER BY day ASC",
    )
    .bind(days)
    .fetch_all(&pool)
    .await
    .map_err(|e| sanitize(format!("aggregate runs: {e}")))?;

    // 存储快照必须在本租户连接池关闭前取（媒体字节来自租户库）。两项都尽力而为。
    let media_bytes = media_bytes(&pool).await;
    pool.close().await;
    let workspace_bytes = workspace_bytes(&state.cfg.sandbox.data_root, tenant_id).await;
    let storage_bytes = media_bytes.saturating_add(workspace_bytes.min(i64::MAX as u64) as i64);

    let runs_by_day: HashMap<chrono::NaiveDate, i64> = runs.into_iter().collect();
    // 当天没有用量事件时也补一行，让"当前存储"有地方落且不会把当日 runs 覆盖成 0。
    let today = chrono::Utc::now().date_naive();
    let mut usage_days = usage;
    if !usage_days.iter().any(|(day, ..)| *day == today) {
        usage_days.push((today, 0, 0, 0));
    }
    usage_days.sort_by_key(|(day, ..)| *day);

    let mut rows = 0usize;
    for (day, tin, tout, cost) in usage_days {
        let day_runs = runs_by_day.get(&day).copied().unwrap_or(0);
        // 只有当天那行记录存储快照：跨天重复写入会让按窗口 sum(storage_bytes) 变成倍数。
        let day_storage = if day == today { storage_bytes } else { 0 };
        sqlx::query(
            "INSERT INTO tenant_usage_daily
                 (tenant_id, day, runs, tokens_in, tokens_out, cost_cents, storage_bytes)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (tenant_id, day) DO UPDATE
                SET runs = EXCLUDED.runs, tokens_in = EXCLUDED.tokens_in,
                    tokens_out = EXCLUDED.tokens_out, cost_cents = EXCLUDED.cost_cents,
                    storage_bytes = EXCLUDED.storage_bytes",
        )
        .bind(tenant_id)
        .bind(day)
        .bind(day_runs)
        .bind(tin)
        .bind(tout)
        .bind(cost)
        .bind(day_storage)
        .execute(control)
        .await
        .map_err(|e| format!("upsert tenant_usage_daily: {e}"))?;
        rows += 1;
    }
    Ok(rows)
}

/// 租户库媒体占用（字节）。旧租户库可能还没有 `media_assets`（迁移未跑完），
/// 这种情况只告警并按 0 计，不影响用量汇总。
async fn media_bytes(pool: &sqlx::PgPool) -> i64 {
    match sqlx::query_scalar::<_, i64>("SELECT COALESCE(sum(bytes), 0)::bigint FROM media_assets")
        .fetch_one(pool)
        .await
    {
        Ok(bytes) => bytes.max(0),
        Err(e) => {
            tracing::warn!(error = %e, "tenant media bytes unavailable, counted as 0");
            0
        }
    }
}

/// 宿主工作区目录的候选路径。
///
/// 沙箱目录段在请求态是租户 uuid 前 8 位（见 `sandbox::workspace_dir`），在无请求
/// 上下文时是完整 uuid；两种命名都可能存在历史数据，所以都纳入统计。
fn workspace_dirs(data_root: &Path, tenant_id: Uuid) -> Vec<PathBuf> {
    let full = tenant_id.to_string();
    let mut dirs = vec![data_root.join(&full)];
    let short = full[..8].to_string();
    if short != full {
        dirs.push(data_root.join(short));
    }
    dirs
}

/// 工作区占用（字节）：路径不存在/无权限读时记 0 并告警，绝不让 rollup 失败。
async fn workspace_bytes(data_root: &Path, tenant_id: Uuid) -> u64 {
    let mut total = 0u64;
    for dir in workspace_dirs(data_root, tenant_id) {
        match dir_size(&dir).await {
            Ok(0) => {}
            Ok(bytes) => total = total.saturating_add(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                dir = %dir.display(), error = %e,
                "workspace size unavailable, counted as 0"
            ),
        }
    }
    total
}

/// 目录递归大小（字节）。不跟随符号链接（`DirEntry::metadata` 即 link 本身），
/// 因此既不会循环遍历，也不会把链接指向的外部数据算进租户配额。
pub(crate) async fn dir_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

fn control_unavailable() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

/// 后台定时汇总：leader 租约（多实例只有一个节点执行），默认 1h，窗口 3 天。
pub fn spawn_scheduler(state: Arc<AppState>) {
    let secs = state.cfg.usage_rollup_secs;
    if secs == 0 || state.cfg.control_database_url.trim().is_empty() {
        tracing::info!("usage rollup scheduler disabled");
        return;
    }
    tokio::spawn(async move {
        let every = Duration::from_secs(secs.max(60));
        loop {
            tokio::time::sleep(every).await;
            let Some(control) = state.control.clone() else {
                return;
            };
            let owner = crate::sessions::lease::owner_id();
            let key = crate::sessions::lease::leader_key("usage_rollup");
            match state.leases.acquire(&key, &owner, every * 2).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "usage rollup leader lease failed");
                    continue;
                }
            }
            let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
                "SELECT id, slug, dsn_enc FROM tenants WHERE status = 'active' ORDER BY slug",
            )
            .fetch_all(&control.db)
            .await
            .unwrap_or_default();
            let (mut ok, mut failed) = (0usize, 0usize);
            for (id, _slug, enc) in rows {
                let Some(enc) = enc else {
                    failed += 1;
                    continue;
                };
                match tokio::time::timeout(
                    PER_TENANT_TIMEOUT,
                    rollup_one(&state, &control.db, id, &enc, 3),
                )
                .await
                {
                    Ok(Ok(_)) => ok += 1,
                    _ => failed += 1,
                }
            }
            tracing::info!(ok, failed, "scheduled usage rollup finished");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dir_size_sums_nested_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        tokio::fs::create_dir_all(tmp.path().join("a/b"))
            .await
            .expect("mkdir");
        tokio::fs::write(tmp.path().join("a/one.bin"), vec![0u8; 10])
            .await
            .expect("write");
        tokio::fs::write(tmp.path().join("a/b/two.bin"), vec![0u8; 5])
            .await
            .expect("write");
        assert_eq!(dir_size(tmp.path()).await.expect("size"), 15);
        // 不存在的目录必须报错（调用方据此降级为 0，而不是静默当作 0 字节）。
        let missing = dir_size(&tmp.path().join("nope")).await;
        assert_eq!(
            missing.expect_err("missing dir").kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn workspace_dirs_cover_short_and_full_tenant_segments() {
        let id = Uuid::parse_str("018f2f8c-0000-7000-8000-000000000001").expect("uuid");
        let dirs = workspace_dirs(Path::new("/data"), id);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/data/018f2f8c-0000-7000-8000-000000000001"),
                PathBuf::from("/data/018f2f8c"),
            ]
        );
    }

    #[tokio::test]
    async fn workspace_bytes_ignores_missing_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let id = Uuid::parse_str("018f2f8c-0000-7000-8000-000000000001").expect("uuid");
        assert_eq!(workspace_bytes(tmp.path(), id).await, 0);
        let short = tmp.path().join("018f2f8c");
        tokio::fs::create_dir_all(&short).await.expect("mkdir");
        tokio::fs::write(short.join("w.txt"), vec![0u8; 7])
            .await
            .expect("write");
        assert_eq!(workspace_bytes(tmp.path(), id).await, 7);
    }
}
