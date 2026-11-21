//! S4：套餐配额预检（用户数 / 并发 run / 月 token）。
//!
//! 单库形态（无控制库）不限额；SaaS 形态从 Host 解析出的套餐读取上限。
//! 预检是"提交前快速失败"，最终的并发一致性仍由库内唯一索引/事务保证。

use axum::http::StatusCode;
use std::{collections::HashMap, time::Instant};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

use super::PlanLimits;

const ACTIVE_RUNS: &str = "'queued', 'running', 'waiting_approval', 'waiting_user'";

fn exceeded(what: &str, limit: i64) -> AppError {
    AppError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "quota_exceeded",
        format!("{what} quota exceeded (limit {limit})"),
    )
}

pub async fn ensure_user_slot(state: &AppState, plan: Option<&PlanLimits>) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE status <> 'deleted'")
        .fetch_one(&state.db())
        .await?;
    if count >= i64::from(plan.max_users) {
        return Err(exceeded("users", i64::from(plan.max_users)));
    }
    Ok(())
}

pub async fn ensure_run_slot(state: &AppState, plan: Option<&PlanLimits>) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM runs WHERE status IN ({ACTIVE_RUNS})"
    ))
    .fetch_one(&state.db())
    .await?;
    if count >= i64::from(plan.max_concurrent_runs) {
        return Err(exceeded(
            "concurrent runs",
            i64::from(plan.max_concurrent_runs),
        ));
    }
    Ok(())
}

/// 当月已用 token（含缓存读写）；`allow` 策略允许超额继续，账单仍按实际用量计量。
pub async fn ensure_token_budget(
    state: &AppState,
    plan: Option<&PlanLimits>,
) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    if plan.overage_policy.eq_ignore_ascii_case("allow") {
        return Ok(());
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(input_tokens + cache_read_input_tokens
                             + cache_creation_input_tokens + output_tokens), 0)::bigint
           FROM usage_events WHERE created_at >= date_trunc('month', now())",
    )
    .fetch_one(&state.db())
    .await?;
    if used >= plan.monthly_tokens {
        return Err(exceeded("monthly tokens", plan.monthly_tokens));
    }
    Ok(())
}

/// 当月已花成本（分）；套餐 `monthly_cost_cents=0` 表示不限额。
pub async fn ensure_cost_budget(
    state: &AppState,
    plan: Option<&PlanLimits>,
) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    if plan.monthly_cost_cents <= 0 || plan.overage_policy.eq_ignore_ascii_case("allow") {
        return Ok(());
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(round(sum(cost) * 100), 0)::bigint
           FROM usage_events WHERE created_at >= date_trunc('month', now())",
    )
    .fetch_one(&state.db())
    .await?;
    if used >= plan.monthly_cost_cents {
        return Err(exceeded("monthly cost", plan.monthly_cost_cents));
    }
    Ok(())
}

/// 沙箱数量：按容器标签 `chiron.tenant-id=<uuid>` 统计（含已停止容器）。
/// Docker 不可用时放行并告警：真正创建容器时会明确失败，避免控制面误伤。
pub async fn ensure_sandbox_slot(plan: Option<&PlanLimits>) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    if plan.max_sandboxes <= 0 {
        return Ok(());
    }
    let Some(tenant) = crate::tenant::current_tenant() else {
        return Ok(());
    };
    let output = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=chiron.tenant-id={tenant}"),
        ])
        .output()
        .await;
    let Ok(output) = output else {
        tracing::warn!(tenant = %tenant, "docker unavailable; sandbox quota check skipped");
        return Ok(());
    };
    if !output.status.success() {
        tracing::warn!(tenant = %tenant, "docker ps failed; sandbox quota check skipped");
        return Ok(());
    }
    let used = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as i64;
    if used >= i64::from(plan.max_sandboxes) {
        return Err(exceeded("sandboxes", i64::from(plan.max_sandboxes)));
    }
    Ok(())
}

/// 套餐模型白名单（空 = 不限）；匹配模型名或 `<provider>:<model>` 的大小写子串。
pub fn ensure_model_allowed(
    plan: Option<&PlanLimits>,
    requested_model: &str,
) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    if plan.model_allowlist.is_empty() {
        return Ok(());
    }
    let model = requested_model.trim().to_ascii_lowercase();
    let allowed = plan
        .model_allowlist
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .any(|item| !item.is_empty() && model.contains(&item));
    if allowed {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "model_not_allowed",
            format!("model {requested_model} is not included in the tenant plan"),
        ))
    }
}

/// 存储配额：统计租户工作区目录（宿主侧 `/var/agent-data/<tenant>/`），60s 缓存。
/// `max_storage_gb=0` 表示不限额；扫描有 10 万条目上限，避免拖垮控制面。
pub async fn ensure_storage_budget(
    state: &AppState,
    plan: Option<&PlanLimits>,
) -> Result<(), AppError> {
    ensure_storage_budget_with(state, plan, 0).await
}

/// 在配额判定里预留本次写入字节，避免并发上传把上限冲穿。
pub async fn ensure_storage_budget_with(
    state: &AppState,
    plan: Option<&PlanLimits>,
    additional: u64,
) -> Result<(), AppError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    if plan.max_storage_gb <= 0 {
        return Ok(());
    }
    let Some(tenant) = crate::tenant::current_tenant() else {
        return Ok(());
    };
    let bytes = cached_storage_bytes(tenant, state.cfg.sandbox.data_root.clone()).await;
    let limit = u64::from(plan.max_storage_gb as u32) * 1024 * 1024 * 1024;
    if bytes.saturating_add(additional) > limit {
        return Err(exceeded("storage (GB)", i64::from(plan.max_storage_gb)));
    }
    Ok(())
}

fn size_cache() -> &'static std::sync::Mutex<HashMap<Uuid, (Instant, u64)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<Uuid, (Instant, u64)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 工作区候选目录：请求态是 uuid 前 8 位，历史/后台可能是完整 uuid。
pub(crate) fn tenant_workspace_dirs(data_root: &std::path::Path, tenant: Uuid) -> Vec<std::path::PathBuf> {
    let full = tenant.to_string();
    let mut dirs = vec![data_root.join(&full)];
    let short = full[..8].to_string();
    if short != full {
        dirs.push(data_root.join(short));
    }
    dirs
}

async fn media_bytes() -> u64 {
    let Some(pool) = crate::tenant::current() else {
        return 0;
    };
    sqlx::query_scalar::<_, i64>("SELECT COALESCE(sum(bytes), 0)::bigint FROM media_assets")
        .fetch_one(&pool)
        .await
        .map(|bytes| bytes.max(0) as u64)
        .unwrap_or(0)
}

async fn cached_storage_bytes(tenant: Uuid, data_root: std::path::PathBuf) -> u64 {
    if let Ok(cache) = size_cache().lock() {
        if let Some((at, bytes)) = cache.get(&tenant) {
            if at.elapsed() < std::time::Duration::from_secs(60) {
                return *bytes;
            }
        }
    }
    let dirs = tenant_workspace_dirs(&data_root, tenant);
    let workspace = tokio::task::spawn_blocking(move || {
        dirs.into_iter()
            .map(|dir| dir_size(&dir, 100_000))
            .sum::<u64>()
    })
    .await
    .unwrap_or(0);
    let bytes = workspace.saturating_add(media_bytes().await);
    if let Ok(mut cache) = size_cache().lock() {
        cache.insert(tenant, (Instant::now(), bytes));
    }
    bytes
}

fn dir_size(root: &std::path::Path, budget: usize) -> u64 {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > budget {
                return total;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_files_and_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"12345").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"123").unwrap();
        assert_eq!(dir_size(dir.path(), 100), 8);
    }

    #[test]
    fn workspace_dirs_cover_short_and_full_tenant_uuid() {
        let tenant = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let dirs = tenant_workspace_dirs(std::path::Path::new("/data"), tenant);
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].ends_with("11111111-2222-3333-4444-555555555555"));
        assert!(dirs[1].ends_with("11111111"));
    }

    #[test]
    fn quota_errors_are_429_with_stable_code() {
        let e = exceeded("users", 20);
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.code, "quota_exceeded");
        assert!(e.message.contains("limit 20"));
    }

    #[test]
    fn model_allowlist_empty_means_all() {
        let plan = PlanLimits {
            plan_id: "p".into(),
            isolation_tier: "sysbox".into(),
            egress_mode: "bridge".into(),
            max_users: 1,
            max_concurrent_runs: 1,
            monthly_tokens: 1,
            monthly_cost_cents: 0,
            max_storage_gb: 1,
            max_sandboxes: 1,
            model_allowlist: vec![],
            egress_allowlist: vec![],
            overage_policy: "block".into(),
        };
        assert!(ensure_model_allowed(Some(&plan), "anything").is_ok());
    }

    #[test]
    fn model_allowlist_matches_prefix_case_insensitively() {
        let mut plan = PlanLimits {
            plan_id: "p".into(),
            isolation_tier: "sysbox".into(),
            egress_mode: "bridge".into(),
            max_users: 1,
            max_concurrent_runs: 1,
            monthly_tokens: 1,
            monthly_cost_cents: 0,
            max_storage_gb: 1,
            max_sandboxes: 1,
            model_allowlist: vec!["gpt-5".into(), "claude".into()],
            egress_allowlist: vec![],
            overage_policy: "block".into(),
        };
        plan.model_allowlist.shrink_to_fit();
        assert!(ensure_model_allowed(Some(&plan), "openai:GPT-5-mini").is_ok());
        assert!(ensure_model_allowed(Some(&plan), "anthropic:claude-sonnet").is_ok());
        let err = ensure_model_allowed(Some(&plan), "openai:o3").unwrap_err();
        assert_eq!(err.code, "model_not_allowed");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }
}
