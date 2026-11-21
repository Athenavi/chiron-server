//! 后台任务（reconciler / GC / 市场同步）的队列接口与单节点实现。
//!
//! 多实例口径（PRD §4.2）：后台任务用 Redis Streams 消费者组，任务幂等键落 PG。
//! v1 单节点先落"接口 + 内存队列 + leader 租约 + PG 幂等键"：
//! - [`JobQueue`] 是消费者组接口（`publish` / `claim` / `ack`）；
//! - [`MemoryJobQueue`] 是单节点实现；Redis 版在 B12 用同一 trait 接上
//!   （`XADD` / `XREADGROUP` / `XACK`，语义一一对应）；
//! - [`run_once`] 是 PG 幂等键：同一 `(consumer, idem_key)` 只执行一次。
//!
//! 现在接上的后台任务是事件日志保留窗口裁剪（[`spawn_event_log_maintenance`]），
//! reconciler/GC 在 B11 之后按同一模式接入。

// 消费者组接口先冻结（B12 接 Redis Streams 实现），单节点用内存实现；
// 与 `agent` / `checkpoint` 同口径关掉 dead_code。
#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;

use crate::state::AppState;

use super::{event_log, lease};

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job backend unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct JobSpec {
    /// 队列名（Redis 版对应一个 stream key）。
    pub queue: String,
    pub payload: Value,
    /// 幂等键：同一任务重复投递/重复消费时只生效一次（落 PG 的 `background_jobs`）。
    pub idem_key: String,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub queue: String,
    pub payload: Value,
    pub idem_key: String,
    pub attempts: u32,
}

#[async_trait]
pub trait JobQueue: Send + Sync {
    /// 投递；返回队列内 id。
    async fn publish(&self, spec: JobSpec) -> Result<String, JobError>;
    /// 以消费者身份拉取最多 `count` 个未确认任务（Redis: `XREADGROUP >`）。
    async fn claim(&self, consumer: &str, count: usize) -> Result<Vec<Job>, JobError>;
    /// 确认完成（Redis: `XACK`）。
    async fn ack(&self, job: &Job) -> Result<(), JobError>;
}

/// 单节点队列：进程内 + 在途集合（`claim` 过但未 `ack` 的任务不会被重复拉取）。
#[derive(Default)]
pub struct MemoryJobQueue {
    inner: Mutex<QueueState>,
    counter: Mutex<u64>,
}

#[derive(Default)]
struct QueueState {
    pending: Vec<Job>,
    in_flight: HashMap<String, Job>,
    done: usize,
}

impl MemoryJobQueue {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn done_count(&self) -> usize {
        self.lock().done
    }

    pub fn pending_count(&self) -> usize {
        self.lock().pending.len()
    }
}

#[async_trait]
impl JobQueue for MemoryJobQueue {
    async fn publish(&self, spec: JobSpec) -> Result<String, JobError> {
        let id = {
            let mut counter = self.counter.lock().unwrap_or_else(|e| e.into_inner());
            *counter += 1;
            format!("job-{}", *counter)
        };
        self.lock().pending.push(Job {
            id: id.clone(),
            queue: spec.queue,
            payload: spec.payload,
            idem_key: spec.idem_key,
            attempts: 0,
        });
        Ok(id)
    }

    async fn claim(&self, _consumer: &str, count: usize) -> Result<Vec<Job>, JobError> {
        let mut state = self.lock();
        let take = count.max(1).min(state.pending.len());
        let claimed: Vec<Job> = state.pending.drain(..take).collect();
        for job in &claimed {
            state.in_flight.insert(job.id.clone(), job.clone());
        }
        Ok(claimed)
    }

    async fn ack(&self, job: &Job) -> Result<(), JobError> {
        let mut state = self.lock();
        if state.in_flight.remove(&job.id).is_some() {
            state.done += 1;
        }
        Ok(())
    }
}

/// PG 幂等键：第一次执行返回 `true`，之后同键返回 `false`。
pub async fn run_once(db: &PgPool, consumer: &str, idem_key: &str) -> Result<bool, JobError> {
    let affected = sqlx::query(
        "INSERT INTO background_jobs (consumer, idem_key) VALUES ($1, $2) \
         ON CONFLICT (consumer, idem_key) DO NOTHING",
    )
    .bind(consumer)
    .bind(idem_key)
    .execute(db)
    .await
    .map_err(|e| JobError::Unavailable(e.to_string()))?
    .rows_affected();
    Ok(affected == 1)
}

/// 单节点 leader 租约：多实例下只有拿到 `chiron:lease:leader:<name>` 的实例跑定时任务。
pub struct LeaderGuard {
    state: Arc<AppState>,
    key: String,
    owner: String,
    ttl: Duration,
}

impl LeaderGuard {
    pub async fn try_acquire(state: Arc<AppState>, name: &str, ttl: Duration) -> Option<Self> {
        let owner = format!("{}:{}", state.cfg.multi.node_id, lease::owner_id());
        let key = lease::leader_key(name);
        match state.leases.acquire(&key, &owner, ttl).await {
            Ok(true) => Some(Self {
                state,
                key,
                owner,
                ttl,
            }),
            Ok(false) => None,
            Err(err) => {
                // 单节点（无 Redis）时内存租约永远可用；真失败就跳过这轮。
                tracing::warn!(error = %err, leader = name, "leader acquire failed");
                None
            }
        }
    }

    pub async fn renew(&self) -> bool {
        self.state
            .leases
            .renew(&self.key, &self.owner, self.ttl)
            .await
            .unwrap_or(false)
    }

    pub async fn release(self) {
        let _ = self.state.leases.release(&self.key, &self.owner).await;
    }
}

const MAINTENANCE_QUEUE: &str = "event_log_trim";
const MAINTENANCE_INTERVAL_SECS: u64 = 60;

/// 事件日志保留窗口裁剪：leader 每 60s 对不同会话做一次 `trim`，
/// 幂等键 `event_log_trim:<分钟桶>` 保证重复 tick 只生效一次。
pub fn spawn_event_log_maintenance(state: Arc<AppState>) {
    if !state.cfg.multi.maintenance_enabled {
        tracing::info!("event log maintenance disabled");
        return;
    }
    tokio::spawn(async move {
        let ttl = Duration::from_secs((MAINTENANCE_INTERVAL_SECS * 3).max(30));
        loop {
            if let Some(leader) =
                LeaderGuard::try_acquire(state.clone(), "event_log_trim", ttl).await
            {
                let bucket = chrono::Utc::now().timestamp() / MAINTENANCE_INTERVAL_SECS as i64;
                let idem = format!("{MAINTENANCE_QUEUE}:{bucket}");
                match run_once(&state.db(), MAINTENANCE_QUEUE, &idem).await {
                    Ok(true) => {
                        if let Err(err) = trim_all(&state).await {
                            tracing::warn!(error = %err, "event log maintenance failed");
                        }
                    }
                    Ok(false) => tracing::debug!(bucket, "event log maintenance already done"),
                    Err(err) => {
                        tracing::warn!(error = %err, "maintenance idempotency key unavailable");
                    }
                }
                leader.release().await;
            }
            tokio::time::sleep(Duration::from_secs(MAINTENANCE_INTERVAL_SECS)).await;
        }
    });
}

async fn trim_all(state: &Arc<AppState>) -> Result<(), JobError> {
    let Some(registry) = state.tenants.clone() else {
        return trim_recent_sessions(state).await;
    };
    let tenants = registry
        .active_tenants()
        .await
        .map_err(|e| JobError::Unavailable(e.message))?;
    for tenant in tenants {
        match registry
            .scope_tenant(&tenant, trim_recent_sessions(state))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(
                tenant = %tenant.id, error = %err, "event log maintenance tick failed"
            ),
            Err(err) => tracing::warn!(
                tenant = %tenant.id, error = %err.message,
                "event log maintenance tenant scope failed"
            ),
        }
    }
    Ok(())
}

async fn trim_recent_sessions(state: &AppState) -> Result<(), JobError> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM sessions WHERE status <> 'deleted' \
           AND updated_at > now() - interval '24 hours' \
         ORDER BY updated_at DESC LIMIT 100",
    )
    .fetch_all(&state.db())
    .await
    .map_err(|e| JobError::Unavailable(e.to_string()))?;

    let log = event_log::global();
    let mut trimmed = 0u64;
    for (session_id,) in rows {
        match log.trim(session_id).await {
            Ok(n) => trimmed += n,
            Err(err) => tracing::warn!(%session_id, error = %err, "event log trim failed"),
        }
    }
    tracing::info!(trimmed, "event log retention window enforced");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claim_moves_to_in_flight_and_ack_completes() {
        let queue = MemoryJobQueue::new();
        let id = queue
            .publish(JobSpec {
                queue: "gc".into(),
                payload: serde_json::json!({ "run": 1 }),
                idem_key: "gc:1".into(),
            })
            .await
            .expect("publish");
        assert_eq!(queue.pending_count(), 1);

        let claimed = queue.claim("node-a", 10).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);
        assert_eq!(claimed[0].idem_key, "gc:1");
        // 在途任务不会被同实例重复领取。
        assert!(queue.claim("node-a", 10).await.expect("claim").is_empty());

        queue.ack(&claimed[0]).await.expect("ack");
        assert_eq!(queue.done_count(), 1);
        queue.ack(&claimed[0]).await.expect("ack is idempotent");
        assert_eq!(queue.done_count(), 1);
    }

    #[tokio::test]
    async fn claim_respects_count() {
        let queue = MemoryJobQueue::new();
        for i in 0..5 {
            queue
                .publish(JobSpec {
                    queue: "gc".into(),
                    payload: serde_json::json!({ "i": i }),
                    idem_key: format!("gc:{i}"),
                })
                .await
                .expect("publish");
        }
        assert_eq!(queue.claim("node-a", 2).await.expect("claim").len(), 2);
        assert_eq!(queue.pending_count(), 3);
    }
}
