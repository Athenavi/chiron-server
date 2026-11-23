//! 失败/容量告警：可选的 Webhook JSON POST（P1/R-6）。
//!
//! 只发送事件类型、租户 slug/id 与脱敏摘要；不发送 key、DSN 或请求头。
//! 发送是 fire-and-forget：Webhook 不可用不会阻塞主流程。容量告警在跨越阈值时
//! 发送一次，回落后重置，避免每轮重复刷屏。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::{stream, StreamExt};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::state::AppState;

const WATCHDOG_SECS: u64 = 300;

static LAST_ALERTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

/// 同类告警冷却：`cooldown == 0` 不去重；同 key 在窗口内只放行一次。
pub fn cooldown_allows(cooldown: u64, key: &str) -> bool {
    if cooldown == 0 {
        return true;
    }
    let now = Instant::now();
    let mut map = LAST_ALERTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = map.get(key).copied() {
        if now.duration_since(last).as_secs() < cooldown {
            return false;
        }
    }
    map.insert(key.to_string(), now);
    true
}

pub fn notify(state: &AppState, kind: &str, tenant: Option<&str>, detail: &str) {
    let settings = state
        .alerts
        .read()
        .map(|value| value.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
    let url = settings.webhook_url.trim().to_string();
    if url.is_empty() {
        return;
    }
    let cooldown_key = format!("{kind}|{}", tenant.unwrap_or("-"));
    if !cooldown_allows(settings.cooldown_secs, &cooldown_key) {
        tracing::debug!(kind, tenant = ?tenant, "alert suppressed by cooldown");
        return;
    }
    let kind = kind.to_string();
    let tenant = tenant.map(str::to_string);
    let detail = detail.chars().take(300).collect::<String>();
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!(error = %e, "alert webhook client build failed");
                return;
            }
        };
        let body = json!({
            "service": "chiron-server",
            "kind": kind,
            "tenant": tenant,
            "detail": detail,
            "ts": chrono::Utc::now(),
        });
        match client.post(&url).json(&body).send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => tracing::warn!(
                status = %response.status(),
                "alert webhook returned non-2xx"
            ),
            Err(e) => tracing::warn!(
                timeout = e.is_timeout(),
                connect = e.is_connect(),
                "alert webhook request failed"
            ),
        }
    });
}

/// 活跃 run 汇总：控制面存在时逐租户库统计（并发 4、单库 3s），否则回落到默认库。
async fn active_runs_total(state: &AppState) -> (i64, usize, usize) {
    let Some(control) = state.control.as_ref() else {
        let count = default_active_runs(state).await;
        return (count, 1, 0);
    };
    let key = state.cfg.secrets_key.trim();
    if key.is_empty() {
        let count = default_active_runs(state).await;
        return (count, 1, 0);
    }
    let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, dsn_enc FROM tenants \
         WHERE status = 'active' AND dsn_enc IS NOT NULL ORDER BY slug ASC",
    )
    .fetch_all(&control.db)
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        let count = default_active_runs(state).await;
        return (count, 1, 0);
    }

    let key = key.to_string();
    let results: Vec<Option<i64>> = stream::iter(rows)
        .map(|(id, dsn_enc)| {
            let key = key.clone();
            async move {
                let enc = dsn_enc?;
                let dsn = crate::control::crypto::open(&key, id.as_bytes(), &enc).ok()?;
                let pool = PgPoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(Duration::from_secs(3))
                    .connect(&dsn)
                    .await
                    .ok()?;
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*)::bigint FROM runs \
                     WHERE status IN ('queued', 'running', 'waiting_approval', 'waiting_user')",
                )
                .fetch_one(&pool)
                .await
                .unwrap_or(0);
                pool.close().await;
                Some(count)
            }
        })
        .buffer_unordered(4)
        .collect()
        .await;

    let ok = results.iter().filter(|row| row.is_some()).count();
    let failed = results.len() - ok;
    let total = results.into_iter().flatten().sum();
    (total, ok, failed)
}

async fn default_active_runs(state: &AppState) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM runs \
         WHERE status IN ('queued', 'running', 'waiting_approval', 'waiting_user')",
    )
    .fetch_one(&state.db())
    .await
    .unwrap_or(0)
}

/// 容量看门狗：活跃 run、租户池与 egress 拒绝峰值跨阈值时告警；配置热更新。
pub fn spawn_watchdog(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut runs_alerted = false;
        let mut pools_alerted = false;
        let mut last_denied = state.runtime.egress_denied_total();
        loop {
            tokio::time::sleep(Duration::from_secs(WATCHDOG_SECS)).await;
            let settings = state
                .alerts
                .read()
                .map(|value| value.clone())
                .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
            let runs_threshold = settings.active_runs;
            let pools_threshold = settings.tenant_pools;
            let egress_threshold = settings.egress_denied;
            if settings.webhook_url.trim().is_empty()
                && runs_threshold == 0
                && pools_threshold == 0
                && egress_threshold == 0
            {
                last_denied = state.runtime.egress_denied_total();
                continue;
            }
            if runs_threshold > 0 {
                let (active, ok, failed) = active_runs_total(&state).await;
                let high = active as u64 >= runs_threshold;
                if high && !runs_alerted {
                    notify(
                        &state,
                        "capacity_active_runs",
                        None,
                        &format!(
                            "active_runs={active} threshold={runs_threshold} \
                             tenants_ok={ok} tenants_failed={failed}"
                        ),
                    );
                }
                runs_alerted = high;
            }
            if pools_threshold > 0 {
                let pools = state.tenants.as_ref().map(|t| t.cached_pools()).unwrap_or(0) as u64;
                let high = pools >= pools_threshold;
                if high && !pools_alerted {
                    notify(
                        &state,
                        "capacity_tenant_pools",
                        None,
                        &format!("cached_pools={pools} threshold={pools_threshold}"),
                    );
                }
                pools_alerted = high;
            }
            if egress_threshold > 0 {
                let now = state.runtime.egress_denied_total();
                let delta = now.saturating_sub(last_denied);
                last_denied = now;
                if delta >= egress_threshold {
                    notify(
                        &state,
                        "egress_denied_spike",
                        None,
                        &format!("denied_in_window={delta} threshold={egress_threshold}"),
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::cooldown_allows;

    #[test]
    fn cooldown_suppresses_duplicates_until_key_changes() {        assert!(cooldown_allows(60, "test:dup"));
        assert!(!cooldown_allows(60, "test:dup"));
        assert!(cooldown_allows(60, "test:other"));
        assert!(cooldown_allows(0, "test:dup"));
    }
}
