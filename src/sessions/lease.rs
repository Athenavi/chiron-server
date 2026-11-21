//! 会话 / run 租约（B11）：Redis `SET NX PX` + 续租 + 释放，PG 唯一约束兜底。
//!
//! 口径（PRD §4.2）：
//! - 本实例的 run registry 只回答"这个 run 是不是我在跑"；
//! - 跨实例靠租约：拿不到租约的实例不启动循环，事件的 SSE 仍可由任意实例服务；
//! - PG 侧 `runs.lease_owner/lease_until`（CAS）是权威兜底：Redis 不可用时
//!   `pg_lease_backstop=true` 仍然保证同一 run 只被一个实例接管；
//! - `ux_runs_one_active_per_session` 保证同一会话不会有两个活跃 run。
//!
//! v1 不做跨节点迁移：[`container_node`] 只记录会话亲和，供 B12 路由使用。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{redis_ha::RedisHandle, state::AppState};

use super::run_state::ACTIVE_STATUS_SQL;

pub fn run_key(run_id: Uuid) -> String {
    format!("chiron:lease:run:{run_id}")
}

#[allow(dead_code)] // 会话级串行化入口（B12 路由复用）。
pub fn session_key(session_id: Uuid) -> String {
    format!("chiron:lease:session:{session_id}")
}

pub fn leader_key(name: &str) -> String {
    format!("chiron:lease:leader:{name}")
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("lease backend unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait LeaseStore: Send + Sync {
    /// `SET key owner NX PX ttl`：只有未占用或已过期才成功。
    async fn acquire(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError>;
    /// 只有持有者能续租；租约已被别人拿走时返回 `false`。
    async fn renew(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError>;
    /// 只有持有者能释放（幂等：未持有时无操作）。
    async fn release(&self, key: &str, owner: &str) -> Result<bool, LeaseError>;
    async fn owner(&self, key: &str) -> Result<Option<String>, LeaseError>;
}

#[derive(Clone, Debug)]
struct Held {
    owner: String,
    expires_at: Instant,
}

/// 进程内实现：等价于"单节点 Redis"，多实例测试用同一个 `Arc` 共享。
#[derive(Default)]
pub struct MemoryLeaseStore {
    leases: Mutex<HashMap<String, Held>>,
}

impl MemoryLeaseStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)] // 多实例测试共享同一后端。
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Held>> {
        self.leases.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl LeaseStore for MemoryLeaseStore {
    async fn acquire(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError> {
        let now = Instant::now();
        let mut leases = self.lock();
        match leases.get(key) {
            Some(held) if held.expires_at > now && held.owner != owner => Ok(false),
            _ => {
                leases.insert(
                    key.to_string(),
                    Held {
                        owner: owner.to_string(),
                        expires_at: now + ttl.max(Duration::from_millis(1)),
                    },
                );
                Ok(true)
            }
        }
    }

    async fn renew(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError> {
        let now = Instant::now();
        let mut leases = self.lock();
        match leases.get_mut(key) {
            Some(held) if held.owner == owner && held.expires_at > now => {
                held.expires_at = now + ttl.max(Duration::from_millis(1));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn release(&self, key: &str, owner: &str) -> Result<bool, LeaseError> {
        let mut leases = self.lock();
        if leases.get(key).is_some_and(|held| held.owner == owner) {
            leases.remove(key);
            return Ok(true);
        }
        Ok(false)
    }

    async fn owner(&self, key: &str) -> Result<Option<String>, LeaseError> {
        let now = Instant::now();
        Ok(self
            .lock()
            .get(key)
            .filter(|held| held.expires_at > now)
            .map(|held| held.owner.clone()))
    }
}

/// Redis 实现：`SET NX PX` + 比较-续租/释放（Lua，保证不会误删别人的租约）。
///
/// R-2：持有可热替换的句柄，每个操作取一次连接；Sentinel 切主后租约自动落到新主
/// （旧主上的租约随 TTL 过期，不会造成双持有——但切换瞬间两边各有一份短窗口）。
pub struct RedisLeaseStore {
    handle: RedisHandle,
}

const RENEW_SCRIPT: &str = "if redis.call('get', KEYS[1]) == ARGV[1] then \
     return redis.call('pexpire', KEYS[1], ARGV[2]) else return 0 end";
const RELEASE_SCRIPT: &str =
    "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end";

impl RedisLeaseStore {
    /// 单连接构造（测试与单节点路径）：包一层句柄，语义不变。
    #[allow(dead_code)] // 生产路径用 `with_handle`；保留给单实例测试。
    pub fn new(conn: ConnectionManager) -> Self {
        Self::with_handle(RedisHandle::new(conn))
    }

    /// R-2：主入口（与 `AppState.redis` 共用同一个句柄）。
    pub fn with_handle(handle: RedisHandle) -> Self {
        Self { handle }
    }
}

fn millis(ttl: Duration) -> u64 {
    ttl.as_millis().max(1) as u64
}

#[async_trait]
impl LeaseStore for RedisLeaseStore {
    async fn acquire(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError> {
        let mut conn = self.handle.conn().await;
        let result: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(owner)
            .arg("PX")
            .arg(millis(ttl))
            .arg("NX")
            .query_async(&mut conn)
            .await
            .map_err(|e| LeaseError::Unavailable(e.to_string()))?;
        Ok(result.as_deref() == Some("OK"))
    }

    async fn renew(&self, key: &str, owner: &str, ttl: Duration) -> Result<bool, LeaseError> {
        let mut conn = self.handle.conn().await;
        let renewed: i64 = redis::cmd("EVAL")
            .arg(RENEW_SCRIPT)
            .arg(1)
            .arg(key)
            .arg(owner)
            .arg(millis(ttl))
            .query_async(&mut conn)
            .await
            .map_err(|e| LeaseError::Unavailable(e.to_string()))?;
        Ok(renewed == 1)
    }

    async fn release(&self, key: &str, owner: &str) -> Result<bool, LeaseError> {
        let mut conn = self.handle.conn().await;
        let released: i64 = redis::cmd("EVAL")
            .arg(RELEASE_SCRIPT)
            .arg(1)
            .arg(key)
            .arg(owner)
            .query_async(&mut conn)
            .await
            .map_err(|e| LeaseError::Unavailable(e.to_string()))?;
        Ok(released == 1)
    }

    async fn owner(&self, key: &str) -> Result<Option<String>, LeaseError> {
        let mut conn = self.handle.conn().await;
        let owner: Option<String> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| LeaseError::Unavailable(e.to_string()))?;
        Ok(owner)
    }
}

/// 本实例的租约 owner（与 `runs.lease_owner` 同口径，PG CAS 才能认可）。
pub fn owner_id() -> String {
    crate::checkpoint::reconciler::lease_owner()
}

/// PG 兜底 CAS：只抢活跃 run，不增加 `recover_attempts`（那不是"恢复"）。
async fn pg_acquire(
    db: &PgPool,
    run_id: Uuid,
    owner: &str,
    ttl: Duration,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "UPDATE runs SET lease_owner = $2, \
             lease_until = now() + make_interval(secs => $3::int), updated_at = now() \
         WHERE id = $1 AND status IN ({ACTIVE_STATUS_SQL}) \
           AND (lease_owner IS NULL OR lease_owner = $2 \
                OR lease_until IS NULL OR lease_until < now())"
    );
    let affected = sqlx::query(&sql)
        .bind(run_id)
        .bind(owner)
        .bind(ttl.as_secs().max(1).min(i32::MAX as u64) as i32)
        .execute(db)
        .await?
        .rows_affected();
    Ok(affected == 1)
}

/// 跨实例准入：Redis 租约（快路径）+ PG CAS（兜底）。
///
/// 返回 `false` 表示"别的实例在跑这个 run"，调用方不得启动循环。
pub async fn acquire_run(
    state: &AppState,
    run_id: Uuid,
    owner: &str,
    ttl: Duration,
) -> Result<bool, LeaseError> {
    let key = run_key(run_id);
    let mut acquired = state.leases.acquire(&key, owner, ttl).await?;
    if !acquired {
        // 同一 owner 重复准入（reconciler 接管自己的 run）视为成功。
        acquired = state.leases.owner(&key).await?.as_deref() == Some(owner);
    }
    if !acquired {
        return Ok(false);
    }
    if state.cfg.multi.pg_lease_backstop {
        match pg_acquire(&state.db(), run_id, owner, ttl).await {
            Ok(true) => {}
            Ok(false) => {
                let _ = state.leases.release(&key, owner).await;
                return Ok(false);
            }
            Err(err) => {
                // DB 抖动时不因为兜底不可用而放弃已经拿到的 Redis 租约；
                // sessions 的唯一索引与 DbSink 的 CAS 仍是最后防线。
                tracing::warn!(%run_id, error = %err, "pg lease backstop unavailable");
            }
        }
    }
    Ok(true)
}

pub async fn renew_run(state: &AppState, run_id: Uuid, owner: &str, ttl: Duration) -> bool {
    if !state
        .leases
        .renew(&run_key(run_id), owner, ttl)
        .await
        .unwrap_or(false)
    {
        return false;
    }
    if !state.cfg.multi.pg_lease_backstop {
        return true;
    }
    let sql = format!(
        "UPDATE runs SET lease_until = now() + make_interval(secs => $3::int), updated_at = now() \
         WHERE id = $1 AND lease_owner = $2 AND status IN ({ACTIVE_STATUS_SQL})"
    );
    match sqlx::query(&sql)
        .bind(run_id)
        .bind(owner)
        .bind(ttl.as_secs().max(1).min(i32::MAX as u64) as i32)
        .execute(&state.db())
        .await
    {
        Ok(done) => done.rows_affected() == 1,
        Err(err) => {
            tracing::warn!(%run_id, error = %err, "pg lease renew failed");
            false
        }
    }
}

pub async fn release_run(state: &AppState, run_id: Uuid, owner: &str) {
    if let Err(err) = state.leases.release(&run_key(run_id), owner).await {
        tracing::warn!(%run_id, error = %err, "run lease release failed");
    }
}

/// 会话级租约（同一会话的写请求串行化 / 未来跨节点路由用）。
#[allow(dead_code)]
pub async fn acquire_session(
    leases: &dyn LeaseStore,
    session_id: Uuid,
    owner: &str,
    ttl: Duration,
) -> Result<bool, LeaseError> {
    leases.acquire(&session_key(session_id), owner, ttl).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_is_exclusive_until_expiry() {
        let store = MemoryLeaseStore::new();
        let key = run_key(Uuid::new_v4());

        assert!(store
            .acquire(&key, "a", Duration::from_millis(40))
            .await
            .unwrap());
        assert!(!store
            .acquire(&key, "b", Duration::from_millis(40))
            .await
            .unwrap());
        assert_eq!(store.owner(&key).await.unwrap().as_deref(), Some("a"));

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(store
            .acquire(&key, "b", Duration::from_millis(40))
            .await
            .unwrap());
        assert!(!store
            .renew(&key, "a", Duration::from_secs(1))
            .await
            .unwrap());
        assert!(store
            .renew(&key, "b", Duration::from_secs(1))
            .await
            .unwrap());
        assert!(!store.release(&key, "a").await.unwrap());
        assert!(store.release(&key, "b").await.unwrap());
    }

    #[tokio::test]
    async fn renew_keeps_lease_alive() {
        let store = MemoryLeaseStore::new();
        let key = session_key(Uuid::new_v4());
        // Windows 定时器粒度可达 ~15ms，留足余量，避免把调度抖动当成逻辑回归。
        assert!(store
            .acquire(&key, "a", Duration::from_millis(500))
            .await
            .unwrap());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(store
            .renew(&key, "a", Duration::from_millis(500))
            .await
            .unwrap());
        tokio::time::sleep(Duration::from_millis(100)).await;
        // 原租约已到期，但续租把它推后了：别人仍抢不到。
        assert!(!store
            .acquire(&key, "b", Duration::from_millis(500))
            .await
            .unwrap());
    }

    #[test]
    fn keys_are_scoped() {
        let id = Uuid::nil();
        assert_eq!(run_key(id), format!("chiron:lease:run:{id}"));
        assert_eq!(session_key(id), format!("chiron:lease:session:{id}"));
        assert_eq!(leader_key("gc"), "chiron:lease:leader:gc");
        assert!(owner_id().starts_with("pid-"));
    }
}
