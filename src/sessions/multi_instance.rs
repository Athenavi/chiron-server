//! B11 双实例冒烟：两个独立的 `AppState` + `EventLog`（共享后端）验证
//!
//! 1. 同一会话的 run 不会被两个实例同时执行（租约）；
//! 2. SSE 事件可跨实例续传（`Last-Event-ID` 从共享事件日志回放）；
//! 3. 租约到期后另一个实例可以接管（崩溃恢复路径）。
//!
//! 不依赖真 Redis：两个 `MemoryEventLog` 共享同一个 `Arc<EventBus>`，
//! 两个实例共享同一个 `MemoryLeaseStore`，等价于"两台机器连同一个 Redis"。
//! 配置了 `CHIRON_TEST_REDIS_URL` 时额外跑一组 Redis 集成测试。

use std::{sync::Arc, time::Duration};

use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::{
    admin::{limits::RateLimiter, runtime::RuntimeCounters},
    config::Config,
    sandbox::SandboxManager,
    sessions::{
        event_log::{EventLog, MemoryEventLog},
        events::EventBus,
        lease::{self, LeaseStore, MemoryLeaseStore},
        redis_log,
    },
    state::AppState,
};

fn instance(node: &str, bus: Arc<EventBus>, leases: Arc<MemoryLeaseStore>) -> Arc<AppState> {
    let cfg = Arc::new(Config::for_test(node));
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&cfg.database_url)
        .expect("lazy pool");
    Arc::new(AppState {
        db,
        redis: None,
        cfg: cfg.clone(),
        sandbox: Arc::new(SandboxManager::new(cfg.sandbox.clone())),
        limits: Arc::new(RateLimiter::new(cfg.rate_limit.clone())),
        alerts: Arc::new(std::sync::RwLock::new(
            crate::state::AlertSettings::from_config(&cfg),
        )),
        runtime: Arc::new(RuntimeCounters::default()),
        events: Arc::new(MemoryEventLog::shared(bus, 8)),
        leases,
        control: None,
        tenants: None,
    })
}

fn two_instances() -> (Arc<AppState>, Arc<AppState>) {
    let bus = Arc::new(EventBus::with_capacity(8, 64));
    let leases = MemoryLeaseStore::shared();
    (
        instance("node-a", bus.clone(), leases.clone()),
        instance("node-b", bus, leases),
    )
}

#[tokio::test]
async fn same_run_is_admitted_by_exactly_one_instance() {
    let (a, b) = two_instances();
    let run_id = Uuid::new_v4();
    let sid = Uuid::new_v4();
    let ttl = Duration::from_secs(5);

    // 每个实例的 registry 都允许本地登记（registry 只回答"是不是我在跑"），
    // 跨实例互斥由租约决定。
    let control_a = crate::agent::control::RunRegistry::new();
    let control_b = crate::agent::control::RunRegistry::new();
    assert!(control_a.register(run_id).is_some());
    assert!(control_b.register(run_id).is_some());

    assert!(lease::acquire_run(&a, run_id, &lease::owner_id(), ttl)
        .await
        .expect("lease backend"));
    // 另一实例（不同 owner）拿不到租约 → 不启动循环。
    let other = format!("{}-other", lease::owner_id());
    assert!(!lease::acquire_run(&b, run_id, &other, ttl)
        .await
        .expect("lease backend"));

    // 同一实例重复准入（例如 reconciler 接管自己的 run）是幂等的。
    assert!(lease::acquire_run(&a, run_id, &lease::owner_id(), ttl)
        .await
        .expect("lease backend"));

    // 会话级租约同样互斥。
    assert!(
        lease::acquire_session(a.leases.as_ref(), sid, "node-a", ttl)
            .await
            .expect("session lease")
    );
    assert!(
        !lease::acquire_session(b.leases.as_ref(), sid, "node-b", ttl)
            .await
            .expect("session lease")
    );
}

#[tokio::test]
async fn expired_lease_allows_takeover() {
    let (a, b) = two_instances();
    let run_id = Uuid::new_v4();
    let short = Duration::from_millis(40);

    assert!(lease::acquire_run(&a, run_id, "node-a", short)
        .await
        .expect("lease"));
    tokio::time::sleep(Duration::from_millis(60)).await;
    // 原实例已"死"（租约过期）：另一实例可接管。
    assert!(lease::acquire_run(&b, run_id, "node-b", short)
        .await
        .expect("lease"));
}

#[tokio::test]
async fn events_resume_across_instances_by_last_event_id() {
    let (a, b) = two_instances();
    let sid = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    a.events
        .append(sid, Some(run_id), "run.started", json!({ "model": "m" }));
    a.events
        .append(sid, Some(run_id), "message.delta", json!({ "delta": "he" }));
    a.events.append(
        sid,
        Some(run_id),
        "message.delta",
        json!({ "delta": "llo" }),
    );

    // 实例 B 从来没有服务过这个会话，也能完整回放。
    let full = b.events.replay(sid, None).await;
    assert!(full.gap.is_none());
    assert_eq!(
        full.events
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["run.started", "message.delta", "message.delta"]
    );

    // 客户端带 Last-Event-ID=1 重连到 B：只补 seq 2 之后的事件。
    let resumed = b.events.replay(sid, Some(1)).await;
    assert!(resumed.gap.is_none());
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].data["delta"], "llo");
    assert_eq!(resumed.last_seq, Some(2));

    // run 在 A 上继续产出，B 的续流能跟上。
    a.events.append(
        sid,
        Some(run_id),
        "run.completed",
        json!({ "status": "completed" }),
    );
    let resumed = b.events.replay(sid, resumed.last_seq).await;
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].kind, "run.completed");
}

#[tokio::test]
async fn live_tail_works_across_instances() {
    use futures_util::StreamExt;

    let (a, b) = two_instances();
    let sid = Uuid::new_v4();
    let cursor = a.events.replay(sid, None).await.last_seq;
    let mut tail = b.events.tail(sid, None, cursor).await;

    a.events
        .append(sid, None, "tool.call", json!({ "name": "read_file" }));

    let envelope = tokio::time::timeout(Duration::from_millis(500), tail.next())
        .await
        .expect("tail should deliver shared event")
        .expect("stream item");
    assert_eq!(envelope.kind, "tool.call");
}

/// 可选：`CHIRON_TEST_REDIS_URL` 存在时验证 Redis Streams 与租约。
#[tokio::test]
async fn redis_integration_when_configured() {
    use crate::sessions::{event_log, lease::RedisLeaseStore, redis_log::RedisEventLog};

    let Ok(url) = std::env::var("CHIRON_TEST_REDIS_URL") else {
        eprintln!("skip: CHIRON_TEST_REDIS_URL not set");
        return;
    };
    let url = url.trim().to_string();
    if url.is_empty() {
        eprintln!("skip: CHIRON_TEST_REDIS_URL empty");
        return;
    }

    let client = redis::Client::open(url).expect("redis url");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("redis connect");

    let log = RedisEventLog::with_connection(conn.clone(), 64, 60, 50);
    let sid = Uuid::new_v4();
    log.append(sid, None, "run.started", json!({ "model": "m" }));
    log.append(sid, None, "message.delta", json!({ "delta": "x" }));

    // 写入是"入队 + 后台 XADD"，轮询等落流。
    let mut replay = log.replay(sid, None).await;
    for _ in 0..40 {
        if replay.events.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        replay = log.replay(sid, None).await;
    }
    assert_eq!(replay.events.len(), 2, "append should reach the stream");
    // seq 由 `INCR` 分配，从 1 开始（客户端 Last-Event-ID 用同一口径）。
    assert_eq!(replay.events[0].seq, 1);
    assert_eq!(replay.events[1].seq, 2);

    // 只要第 2 条：after=1 是已消费游标，且不产生 gap。
    let resumed = log.replay(sid, Some(1)).await;
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.gap, None);

    // 保留窗口：写满后 trim 至少不报错。
    assert!(log.trim(sid).await.is_ok());

    // 租约：两个实例共享同一 Redis，只有一个能拿到。
    let a_store = RedisLeaseStore::new(conn.clone());
    let b_store = RedisLeaseStore::new(conn);
    let key = lease::run_key(Uuid::new_v4());
    assert!(a_store
        .acquire(&key, "node-a", Duration::from_secs(2))
        .await
        .expect("acquire"));
    assert!(!b_store
        .acquire(&key, "node-b", Duration::from_secs(2))
        .await
        .expect("acquire"));
    assert!(a_store
        .renew(&key, "node-a", Duration::from_secs(2))
        .await
        .expect("renew"));
    assert!(!b_store.release(&key, "node-b").await.expect("release"));
    assert!(a_store.release(&key, "node-a").await.expect("release"));

    // 清理测试键（避免污染本地 Redis）。
    let mut cleanup = redis::aio::ConnectionManager::new(
        redis::Client::open(std::env::var("CHIRON_TEST_REDIS_URL").expect("url")).expect("client"),
    )
    .await
    .expect("connect");
    let _: Result<i64, _> = redis::cmd("DEL")
        .arg(redis_log::key_prefix().to_string() + &sid.to_string())
        .arg(redis_log::key_prefix().to_string() + &sid.to_string() + ":seq")
        .query_async(&mut cleanup)
        .await;

    // 避免 `event_log` 未使用告警：全局日志在测试里可能是内存实现。
    let _ = event_log::global();
}
