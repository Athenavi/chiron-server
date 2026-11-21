//! R-2：Redis 多实例 / 高可用。
//!
//! 两件事：
//!
//! 1. [`RedisHandle`]：可热替换的连接句柄。所有业务按操作 `conn()` 取一个
//!    `ConnectionManager` clone（内部是 Arc，克隆很便宜），Sentinel 报告新主节点时
//!    后台任务 `replace()` 原子换掉底层连接，进程无需重启。
//! 2. [`SentinelOptions`] + [`spawn_refresher`]：从一组 Sentinel 解析 `<master>` 的
//!    当前主节点；每 15s 复查一次，只有地址变化且新连接可用时才换句柄。
//!
//! 为什么不直接用 `redis::sentinel::Sentinel`：redis 0.27 把该模块放在 `sentinel`
//! Cargo feature 后面（本仓 Cargo.toml 不在本次改动范围内），而 Sentinel 的发现协议
//! 本身只有两条命令，所以这里直接走 `SENTINEL get-master-addr-by-name`，并在连上
//! 候选主节点后用 `ROLE` 复核（防止切主窗口里连到已降级的旧主）。
//!
//! 限制：连接句柄只保证"尽快换到新主"，不保证故障瞬间的强一致。切换前已经取出的
//! 连接仍指向旧主（`ConnectionManager` 会自动重连旧地址），所以最长有一个刷新周期
//! （15s）的写窗口仍落在旧主上；事件流 `tail` 每轮重新取连接，窗口更短。启动时
//! Sentinel 不可达则视为无 Redis（团队模式直接拒绝启动），后台刷新不会重建句柄。

use std::{sync::Arc, time::Duration};

use redis::{
    aio::ConnectionManager, Client, ConnectionAddr, ErrorKind, RedisConnectionInfo, RedisError,
    RedisResult,
};
use tokio::sync::RwLock;

/// Sentinel 配置（来自 env，见 [`crate::config::Config`]）。
///
/// `urls` 必须是 `redis://host:port` 形式（`config.rs` 会把裸 `host:port` 补成
/// `redis://host:port`）。`password` 只作用于 Sentinel 发现的主节点。
#[derive(Clone, Debug)]
pub struct SentinelOptions {
    pub urls: Vec<String>,
    pub master: String,
    pub password: Option<String>,
}

impl SentinelOptions {
    /// 逐个 Sentinel 解析主节点并建连；返回 `(连接, 地址标签)`。
    ///
    /// 地址标签（`host:port`）只用于刷新任务的"是否变化"判定与日志。
    pub async fn resolve(&self) -> RedisResult<(ConnectionManager, String)> {
        let mut last_err: Option<RedisError> = None;
        for url in &self.urls {
            let (host, port) = match self.master_addr_from(url).await {
                Ok(Some(addr)) => addr,
                Ok(None) => {
                    last_err = Some(RedisError::from((
                        ErrorKind::ResponseError,
                        "sentinel does not know the requested master",
                    )));
                    continue;
                }
                Err(err) => {
                    last_err = Some(err);
                    continue;
                }
            };
            let label = format!("{host}:{port}");
            let client = Client::open(master_connection_info(
                &host,
                port,
                self.password.as_deref(),
            ))?;
            let mut conn = match ConnectionManager::new(client).await {
                Ok(conn) => conn,
                Err(err) => {
                    last_err = Some(err);
                    continue;
                }
            };
            match redis::cmd("ROLE")
                .query_async::<(String, i64, redis::Value)>(&mut conn)
                .await
            {
                // 切主窗口里 Sentinel 可能仍指向已降级的旧主：只接受 ROLE=master。
                Ok((role, _, _)) if role == "master" => return Ok((conn, label)),
                Ok((role, _, _)) => {
                    last_err = Some(RedisError::from((
                        ErrorKind::ReadOnly,
                        "sentinel-reported node is not a master",
                        role,
                    )));
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            RedisError::from((
                ErrorKind::InvalidClientConfig,
                "no sentinel urls configured",
            ))
        }))
    }

    /// `SENTINEL get-master-addr-by-name <master>`；未知主节点返回 `Ok(None)`。
    async fn master_addr_from(&self, sentinel_url: &str) -> RedisResult<Option<(String, u16)>> {
        let client = Client::open(sentinel_url.to_string())?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        redis::cmd("SENTINEL")
            .arg("get-master-addr-by-name")
            .arg(&self.master)
            .query_async(&mut conn)
            .await
    }
}

/// Sentinel 汇报的主节点连接信息：密码走 `CHIRON_REDIS_PASSWORD`，其余用默认值
/// （db 0 / RESP2）。
pub fn master_connection_info(
    host: &str,
    port: u16,
    password: Option<&str>,
) -> redis::ConnectionInfo {
    redis::ConnectionInfo {
        addr: ConnectionAddr::Tcp(host.to_string(), port),
        redis: RedisConnectionInfo {
            password: password.map(str::to_string),
            ..RedisConnectionInfo::default()
        },
    }
}

/// 可热替换的 Redis 连接句柄。
///
/// 单节点与 Sentinel 两种形态共用同一个句柄：Sentinel 形态下由后台刷新任务替换，
/// 单节点形态下句柄不会被替换（`ConnectionManager` 自己负责重连同一地址）。
#[derive(Clone)]
pub struct RedisHandle {
    inner: Arc<RwLock<ConnectionManager>>,
}

impl RedisHandle {
    pub fn new(conn: ConnectionManager) -> Self {
        Self {
            inner: Arc::new(RwLock::new(conn)),
        }
    }

    /// 每个操作取一个独立 clone：`ConnectionManager` 内部是 Arc，克隆廉价，
    /// 且不会在命令执行期间持有读锁。
    pub async fn conn(&self) -> ConnectionManager {
        self.inner.read().await.clone()
    }

    /// 切换底层连接：后来的 `conn()` 走新主，进行中的命令用旧连接跑完。
    pub async fn replace(&self, conn: ConnectionManager) {
        *self.inner.write().await = conn;
    }

    /// 健康检查（`/readyz`）。失败不缓存，下一次抓取重新探测。
    pub async fn ping(&self) -> bool {
        let mut conn = self.conn().await;
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map(|reply| reply == "PONG")
            .unwrap_or(false)
    }
}

/// Sentinel 主节点复查间隔。
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// 后台任务：每 [`REFRESH_INTERVAL`] 重新解析主节点，地址变化时换句柄。
///
/// `initial_addr` 是启动时解析到的地址：第一轮解析到同一地址时不做无谓的重连。
/// 解析/连接失败只记 debug（Sentinel 抖动是常态，不能刷日志）；成功切换记 info。
pub fn spawn_refresher(
    handle: RedisHandle,
    options: SentinelOptions,
    initial_addr: Option<String>,
) {
    tokio::spawn(async move {
        let mut current = initial_addr;
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 第一次 tick 立即返回（启动时刚刚解析过），丢弃它。
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match options.resolve().await {
                Ok((conn, addr)) => {
                    if current.as_deref() == Some(addr.as_str()) {
                        continue;
                    }
                    handle.replace(conn).await;
                    tracing::info!(
                        master = %options.master,
                        addr = %addr,
                        previous = ?current,
                        "redis master changed; connection swapped"
                    );
                    current = Some(addr);
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        master = %options.master,
                        "redis sentinel refresh failed (will retry)"
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_connection_info_carries_password() {
        let info = master_connection_info("10.0.0.5", 6379, Some("s3cret"));
        match info.addr {
            ConnectionAddr::Tcp(host, port) => {
                assert_eq!(host, "10.0.0.5");
                assert_eq!(port, 6379);
            }
            other => panic!("unexpected addr: {other:?}"),
        }
        assert_eq!(info.redis.db, 0);
        assert_eq!(info.redis.password.as_deref(), Some("s3cret"));
        assert_eq!(info.redis.username, None);
    }

    #[test]
    fn master_connection_info_without_password() {
        let info = master_connection_info("127.0.0.1", 6380, None);
        assert_eq!(info.redis.password, None);
    }

    /// 可选：`CHIRON_TEST_REDIS_SENTINEL_URLS`（逗号分隔）+ 可选
    /// `CHIRON_TEST_REDIS_SENTINEL_MASTER`（默认 `mymaster`）时验证真实 Sentinel
    /// 解析（`SENTINEL get-master-addr-by-name` + `ROLE` 复核）。
    #[tokio::test]
    async fn sentinel_resolve_when_configured() {
        let Ok(urls) = std::env::var("CHIRON_TEST_REDIS_SENTINEL_URLS") else {
            eprintln!("skip: CHIRON_TEST_REDIS_SENTINEL_URLS not set");
            return;
        };
        let urls = crate::config::normalize_sentinel_urls(&urls);
        if urls.is_empty() {
            eprintln!("skip: CHIRON_TEST_REDIS_SENTINEL_URLS empty");
            return;
        }
        let options = SentinelOptions {
            urls,
            master: std::env::var("CHIRON_TEST_REDIS_SENTINEL_MASTER")
                .unwrap_or_else(|_| "mymaster".to_string()),
            password: std::env::var("CHIRON_TEST_REDIS_SENTINEL_PASSWORD").ok(),
        };
        let (conn, addr) = options.resolve().await.expect("sentinel resolve");
        assert!(!addr.is_empty(), "resolved master address should not be empty");
        let handle = RedisHandle::new(conn);
        assert!(handle.ping().await, "resolved master should answer PING");
    }

    /// 无 Redis 时只能验证纯函数；有 `CHIRON_TEST_REDIS_URL` 时验证换连接的语义。
    #[tokio::test]
    async fn handle_swap_works_when_test_redis_available() {
        let Ok(url) = std::env::var("CHIRON_TEST_REDIS_URL") else {
            eprintln!("skip: CHIRON_TEST_REDIS_URL not set");
            return;
        };
        let url = url.trim().to_string();
        if url.is_empty() {
            eprintln!("skip: CHIRON_TEST_REDIS_URL empty");
            return;
        }
        let connect = || async {
            let client = redis::Client::open(url.clone()).expect("redis url");
            redis::aio::ConnectionManager::new(client)
                .await
                .expect("redis connect")
        };

        let handle = RedisHandle::new(connect().await);
        assert!(handle.ping().await, "initial connection should answer PING");

        // clone 共享同一句柄：换连接对 clone 也可见。
        let observer = handle.clone();
        handle.replace(connect().await).await;
        assert!(
            observer.ping().await,
            "clone should see the swapped connection"
        );
    }
}
