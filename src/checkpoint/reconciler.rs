//! Reconciler：扫描过期租约的活跃 run，按快照接管或明确判死（PRD §7.6）。
//!
//! 决策是纯函数 [`decide`]，扫描/落库只在 [`tick`] 里：
//! | 情况 | 动作 |
//! | :--- | :--- |
//! | 终态 / 租约仍有效 | `Ignore`（绝不抢活着的 run） |
//! | 接管次数超上限 | `Fail(run.recovery_exhausted)` |
//! | 有可用快照 | `Takeover`（CAS 抢租约 → 整理台账 → 续跑） |
//! | 无快照但还在热窗口 | `Wait`（再给原实例一个 tick） |
//! | 无快照且超过冷窗口 | `Fail(run.abandoned)` |

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{snapshot::CheckpointStatus, store, CheckpointConfig};
use crate::{agent::error::AgentError, sessions::run_state::ACTIVE_STATUS_SQL, state::AppState};

/// 单个 run 的健康快照（时间全部由调用方算好，方便单测）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHealth {
    pub status: String,
    /// `lease_until` 已过或从未占用。
    pub lease_expired: bool,
    /// 从 `now - updated_at` 得到的空闲秒数。
    pub idle_secs: i64,
    pub has_checkpoint: bool,
    pub attempts: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Ignore,
    Wait,
    Takeover,
    Fail {
        code: &'static str,
        message: &'static str,
    },
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Wait => "wait",
            Self::Takeover => "takeover",
            Self::Fail { .. } => "fail",
        }
    }
}

pub fn status_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

/// 巡检判定：顺序即优先级（活着 > 次数上限 > 有快照 > 热窗口 > 判死）。
pub fn decide(cfg: &CheckpointConfig, health: &RunHealth) -> Action {
    if status_is_terminal(&health.status) {
        return Action::Ignore;
    }
    if !health.lease_expired {
        return Action::Ignore;
    }
    if health.attempts >= cfg.max_attempts {
        return Action::Fail {
            code: "run.recovery_exhausted",
            message: "run was recovered too many times; giving up",
        };
    }
    if health.has_checkpoint {
        return Action::Takeover;
    }
    if health.idle_secs < cfg.hot_window_secs as i64 {
        // 刚过期：可能是实例正在优雅收尾/刚重启，再等一个 tick。
        return Action::Wait;
    }
    Action::Fail {
        code: "run.abandoned",
        message: "run had no checkpoint and its lease never came back",
    }
}

/// 批量判定（tick 用它把"候选 → 动作"一次算完；纯函数，便于单测）。
pub fn plan(cfg: &CheckpointConfig, items: &[(Uuid, RunHealth)]) -> Vec<(Uuid, Action)> {
    items
        .iter()
        .map(|(id, health)| (*id, decide(cfg, health)))
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub scanned: usize,
    pub taken_over: usize,
    pub failed: usize,
    pub skipped: usize,
    pub gc_deleted: u64,
}

/// 启动后台巡检。`enabled=false` 时什么都不做（单测/桌面模式可关）。
pub fn spawn(state: Arc<AppState>) {
    if !state.cfg.checkpoint.enabled {
        tracing::info!("checkpoint reconciler disabled");
        return;
    }
    let interval = Duration::from_secs(state.cfg.checkpoint.interval_secs.max(5));
    tokio::spawn(async move {
        tracing::info!(?interval, "checkpoint reconciler started");
        loop {
            if let Err(err) = tick_all(&state).await {
                tracing::warn!(error = %err, "reconciler tick failed");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// 多租户形态下逐个活跃租户执行；单库形态仍走默认库。
async fn tick_all(state: &Arc<AppState>) -> Result<TickReport, AgentError> {
    let Some(registry) = state.tenants.clone() else {
        return tick(state).await;
    };
    let tenants = registry
        .active_tenants()
        .await
        .map_err(|e| AgentError::Internal(e.message))?;
    let mut total = TickReport::default();
    for tenant in tenants {
        match registry.scope_tenant(&tenant, tick(state)).await {
            Ok(Ok(report)) => {
                total.scanned += report.scanned;
                total.taken_over += report.taken_over;
                total.failed += report.failed;
                total.skipped += report.skipped;
                total.gc_deleted += report.gc_deleted;
            }
            Ok(Err(err)) => tracing::warn!(
                tenant = %tenant.id, error = %err, "reconciler tenant tick failed"
            ),
            Err(err) => tracing::warn!(
                tenant = %tenant.id, error = %err.message,
                "reconciler tenant scope failed"
            ),
        }
    }
    Ok(total)
}

struct Candidate {
    run_id: Uuid,
    session_id: Uuid,
    status: String,
    attempts: i32,
    idle_secs: i64,
    lease_expired: bool,
}

async fn candidates(db: &PgPool, grace_secs: u64) -> Result<Vec<Candidate>, AgentError> {
    let rows: Vec<(Uuid, Uuid, String, i32, i64, bool)> = sqlx::query_as(&format!(
        "SELECT r.id, r.session_id, r.status, r.recover_attempts, \
                GREATEST(EXTRACT(EPOCH FROM (now() - r.updated_at))::bigint, 0), \
                (r.lease_until IS NULL OR r.lease_until < now()) \
         FROM runs r \
         WHERE r.status IN ({ACTIVE_STATUS_SQL}) \
           AND (r.lease_until IS NULL OR r.lease_until < now()) \
           AND r.updated_at < now() - make_interval(secs => $1::int) \
         ORDER BY r.updated_at ASC LIMIT 32"
    ))
    .bind(grace_secs.min(i32::MAX as u64) as i32)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(run_id, session_id, status, attempts, idle_secs, lease_expired)| Candidate {
                run_id,
                session_id,
                status,
                attempts,
                idle_secs,
                lease_expired,
            },
        )
        .collect())
}

/// 一次巡检：扫描 → 判定 → 接管/判死 → GC。
pub async fn tick(state: &Arc<AppState>) -> Result<TickReport, AgentError> {
    let cfg = &state.cfg.checkpoint;
    if !cfg.enabled {
        return Ok(TickReport::default());
    }
    let mut report = TickReport::default();
    let rows = candidates(&state.db(), cfg.lease_secs).await?;
    report.scanned = rows.len();

    for row in rows {
        // 本实例正在跑的 run 有句柄；租约理论上也在续，这里再兜一层。
        if crate::agent::registry().get(row.run_id).is_some() {
            report.skipped += 1;
            continue;
        }
        let checkpoint = store::latest_usable(&state.db(), row.run_id).await?;
        let health = RunHealth {
            status: row.status.clone(),
            lease_expired: row.lease_expired,
            idle_secs: row.idle_secs,
            has_checkpoint: checkpoint.is_some(),
            attempts: row.attempts,
        };
        match decide(cfg, &health) {
            Action::Ignore | Action::Wait => report.skipped += 1,
            Action::Fail { code, message } => {
                if store::mark_failed(&state.db(), row.run_id, code, message)
                    .await?
                    .is_some()
                {
                    report.failed += 1;
                    tracing::warn!(run_id = %row.run_id, code, "run marked failed by reconciler");
                } else {
                    report.skipped += 1;
                }
            }
            Action::Takeover => {
                let owner = owner_id();
                if !store::acquire_lease(&state.db(), row.run_id, &owner, cfg.lease_secs).await? {
                    // 别的实例先抢到了：什么都不做。
                    report.skipped += 1;
                    continue;
                }
                let (tools, approvals) =
                    store::settle_interrupted_tools(&state.db(), row.run_id).await?;
                if let Some(checkpoint) = &checkpoint {
                    store::mark_status(&state.db(), checkpoint.id, CheckpointStatus::Restored)
                        .await?;
                    sqlx::query(
                        "UPDATE runs SET last_checkpoint_seq = $2, updated_at = now() WHERE id = $1",
                    )
                    .bind(row.run_id)
                    .bind(checkpoint.seq)
                    .execute(&state.db())
                    .await
                    .map_err(|e| AgentError::Internal(e.to_string()))?;
                }
                tracing::info!(
                    run_id = %row.run_id,
                    checkpoint_seq = checkpoint.as_ref().map(|c| c.seq),
                    tools,
                    approvals,
                    "run taken over by reconciler"
                );
                report.taken_over += 1;
                crate::agent::start_run(state.clone(), state.db(), row.run_id, row.session_id);
            }
        }
    }

    report.gc_deleted = store::gc(&state.db(), cfg.retain_per_run).await?;
    if report.taken_over + report.failed + (report.gc_deleted > 0) as usize > 0 {
        tracing::info!(?report, "reconciler tick finished");
    }
    Ok(report)
}

/// 租约 owner 标识：单节点下用进程 id + 启动时间，多实例（B11）换成实例 id。
fn owner_id() -> String {
    static OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OWNER
        .get_or_init(|| {
            let now: DateTime<Utc> = Utc::now();
            format!("pid-{}-{}", std::process::id(), now.timestamp())
        })
        .clone()
}

/// 恢复接口复用同一份租约 owner。
pub fn lease_owner() -> String {
    owner_id()
}

/// 写一个"恢复中"标记事件之前的占位：把快照摘要塞进 run.error 便于排查。
pub fn recovery_note(checkpoint_seq: Option<i32>) -> serde_json::Value {
    json!({ "recovered_at": Utc::now(), "checkpoint_seq": checkpoint_seq })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CheckpointConfig {
        CheckpointConfig {
            enabled: true,
            interval_secs: 30,
            lease_secs: 60,
            hot_window_secs: 120,
            cold_window_secs: 3_600,
            retain_per_run: 20,
            retention_days: 7,
            workspace: true,
            repo: ".".into(),
            max_attempts: 3,
        }
    }

    fn health(
        status: &str,
        expired: bool,
        idle: i64,
        checkpoint: bool,
        attempts: i32,
    ) -> RunHealth {
        RunHealth {
            status: status.into(),
            lease_expired: expired,
            idle_secs: idle,
            has_checkpoint: checkpoint,
            attempts,
        }
    }

    #[test]
    fn live_runs_are_never_touched() {
        let cfg = cfg();
        assert_eq!(
            decide(&cfg, &health("running", false, 10_000, true, 0)),
            Action::Ignore,
            "租约有效时不抢"
        );
        for terminal in ["completed", "failed", "cancelled"] {
            assert_eq!(
                decide(&cfg, &health(terminal, true, 10_000, true, 0)),
                Action::Ignore
            );
            assert!(status_is_terminal(terminal));
        }
        assert!(!status_is_terminal("waiting_approval"));
    }

    #[test]
    fn runs_with_checkpoint_are_taken_over() {
        let cfg = cfg();
        for idle in [0, 60, 100_000] {
            assert_eq!(
                decide(&cfg, &health("running", true, idle, true, 0)),
                Action::Takeover
            );
        }
    }

    #[test]
    fn checkpointless_runs_wait_then_die() {
        let cfg = cfg();
        assert_eq!(
            decide(&cfg, &health("running", true, 10, false, 0)),
            Action::Wait,
            "热窗口内再等等"
        );
        assert_eq!(
            decide(&cfg, &health("running", true, 121, false, 0)),
            Action::Fail {
                code: "run.abandoned",
                message: "run had no checkpoint and its lease never came back"
            }
        );
    }

    #[test]
    fn too_many_recoveries_gives_up() {
        let cfg = cfg();
        assert_eq!(
            decide(&cfg, &health("running", true, 100_000, true, 3)),
            Action::Fail {
                code: "run.recovery_exhausted",
                message: "run was recovered too many times; giving up"
            }
        );
        assert_eq!(
            decide(&cfg, &health("running", true, 100_000, true, 2)),
            Action::Takeover
        );
    }

    #[test]
    fn plan_maps_every_candidate_and_keeps_order() {
        let cfg = cfg();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let actions = plan(
            &cfg,
            &[
                (a, health("running", true, 1, true, 0)),
                (b, health("running", false, 1, true, 0)),
                (c, health("failed", true, 1, true, 0)),
            ],
        );
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], (a, Action::Takeover));
        assert_eq!(actions[1], (b, Action::Ignore));
        assert_eq!(actions[2], (c, Action::Ignore));
        assert_eq!(Action::Takeover.as_str(), "takeover");
    }

    #[test]
    fn owner_id_is_stable_within_process() {
        assert_eq!(lease_owner(), lease_owner());
        assert!(!lease_owner().is_empty());
        assert!(recovery_note(Some(3))["checkpoint_seq"] == 3);
    }
}
