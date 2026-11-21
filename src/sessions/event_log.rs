//! 事件日志抽象（B11）：`append` / `replay` / `trim`。
//!
//! 单节点用内存实现（复用 B2 的 [`EventBus`]，作为 fallback）；多实例用
//! Redis Streams（见 [`super::redis_log`]）。SSE 的 `Last-Event-ID` 只认这个
//! 抽象，所以"哪台实例跑 run"与"哪台实例服务 SSE"解耦（PRD §4.2）。
//!
//! 语义与 `docs/API.md` 一致：
//! - `seq` 会话内单调递增；
//! - `replay` 超出保留窗口时返回 `gap`，SSE 先发 `run.gap` 再回放；
//! - `tail` 是"从当前游标继续"的实时流，内存实现走 broadcast，Redis 实现走
//!   Stream 轮询（250ms）。

use std::{pin::Pin, sync::Arc, sync::OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::events::{EventBus, EventEnvelope};

/// 缺失区间：客户端应从 `from_seq` 重新拉快照（events.md §4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub from_seq: i64,
    pub to_seq: i64,
}

/// 一次断线续传的结果。
#[derive(Clone, Debug, Default)]
pub struct Replay {
    pub gap: Option<Gap>,
    /// 窗口内 `seq > after` 的事件（按 seq 升序）。
    pub events: Vec<EventEnvelope>,
    /// 本次投递到的最后 seq；没有事件时回落到请求游标。
    pub last_seq: Option<i64>,
    /// 实现私有的游标（Redis 的 stream id），`tail` 用它继续读取。
    pub cursor: Option<String>,
}

impl Replay {
    pub fn new(
        gap: Option<Gap>,
        events: Vec<EventEnvelope>,
        after: Option<i64>,
        cursor: Option<String>,
    ) -> Self {
        let last_seq = events
            .last()
            .map(|e| e.seq)
            .or_else(|| gap.map(|g| g.to_seq).or(after));
        Self {
            gap,
            events,
            last_seq,
            cursor,
        }
    }
}

pub type EventTail = Pin<Box<dyn Stream<Item = EventEnvelope> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("event log backend unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait EventLog: Send + Sync {
    /// 追加事件。`seq` 由实现分配；写失败只记日志，不回滚业务状态
    /// （PG 是权威状态，SSE 可重连续传）。
    fn append(&self, session_id: Uuid, run_id: Option<Uuid>, kind: &str, data: Value);

    /// 断线续传：`after` 为客户端 `Last-Event-ID`（无则从头回放全部窗口）。
    async fn replay(&self, session_id: Uuid, after: Option<i64>) -> Replay;

    /// 从 `replay` 的结果继续实时投递（BEGIN 之后的新事件）。
    async fn tail(
        &self,
        session_id: Uuid,
        cursor: Option<String>,
        after_seq: Option<i64>,
    ) -> EventTail;

    /// 按保留窗口裁剪，返回删除条数。
    async fn trim(&self, session_id: Uuid) -> Result<u64, EventLogError>;

    /// 会话删除后清理其全部事件与 seq 键；默认实现无需清理。
    async fn purge(&self, _session_id: Uuid) -> Result<(), EventLogError> {
        Ok(())
    }
}

/// `run.gap` 事件：不占日志 seq，`seq` 取缺失区间上界，保证客户端看到的
/// `id:` 不回退（events.md §4：先 gap 再回放）。
pub fn gap_event(gap: Gap) -> EventEnvelope {
    EventEnvelope {
        id: format!("evt_{}", Uuid::new_v4().simple()),
        seq: gap.to_seq.max(gap.from_seq.saturating_sub(1)),
        run_id: None,
        kind: "run.gap".to_string(),
        data: json!({ "from_seq": gap.from_seq, "to_seq": gap.to_seq }),
        ts: Utc::now(),
    }
}

/// 心跳事件：不占日志 seq（多实例下 seq 由共享计数器分配，不能本地自增）。
pub fn heartbeat_event(seq: i64) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        id: format!("evt_{}", Uuid::new_v4().simple()),
        seq,
        run_id: None,
        kind: "heartbeat".to_string(),
        data: json!({
            "server_time": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
        ts: now,
    }
}

/// 内存实现：回放窗口 + broadcast，复用 B2 的 [`EventBus`]。
///
/// 多实例测试用同一个 `Arc<EventBus>` 构造两个 `MemoryEventLog`，
/// 等价于"两个进程共享一个 Redis"。
pub struct MemoryEventLog {
    bus: Arc<EventBus>,
    capacity: usize,
}

impl Default for MemoryEventLog {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryEventLog {
    pub fn new() -> Self {
        Self::with_capacity(super::events::DEFAULT_REPLAY_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self::shared(Arc::new(EventBus::with_capacity(capacity, 512)), capacity)
    }

    /// 共享后端（测试 / 同进程多实例）。
    pub fn shared(bus: Arc<EventBus>, capacity: usize) -> Self {
        Self {
            bus,
            capacity: capacity.max(1),
        }
    }

    #[allow(dead_code)] // 测试/诊断用。
    pub fn bus(&self) -> &Arc<EventBus> {
        &self.bus
    }
}

#[async_trait]
impl EventLog for MemoryEventLog {
    fn append(&self, session_id: Uuid, run_id: Option<Uuid>, kind: &str, data: Value) {
        self.bus.publish(session_id, run_id, kind, data);
    }

    async fn replay(&self, session_id: Uuid, after: Option<i64>) -> Replay {
        let (missing, events) = self.bus.window(session_id, after);
        Replay::new(
            missing.map(|(from_seq, to_seq)| Gap { from_seq, to_seq }),
            events,
            after,
            None,
        )
    }

    async fn tail(
        &self,
        session_id: Uuid,
        _cursor: Option<String>,
        after_seq: Option<i64>,
    ) -> EventTail {
        let mut receiver = self.bus.tail();
        let mut last = after_seq;
        Box::pin(async_stream::stream! {
            loop {
                match receiver.recv().await {
                    Ok((event_session, envelope)) => {
                        if event_session != session_id {
                            continue;
                        }
                        if last.is_some_and(|seq| envelope.seq <= seq) {
                            continue;
                        }
                        last = Some(envelope.seq);
                        yield envelope;
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        // 广播积压：告知 gap，SSE 客户端据此重拉快照。
                        if let Some(seq) = last {
                            yield gap_event(Gap {
                                from_seq: seq + 1,
                                to_seq: seq + missed.max(1) as i64,
                            });
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    async fn trim(&self, session_id: Uuid) -> Result<u64, EventLogError> {
        Ok(self.bus.trim_to(session_id, self.capacity))
    }
}

/// 进程级事件日志。`main` 启动时按配置 [`install`]；未安装时退回内存实现，
/// 这样单测与桌面模式不需要 Redis。
///
/// 注意：`global` 与 `install` 必须共享同一个 static，否则 run 写入的
/// 日志和 SSE 读取的日志会是两个不同实例（单节点丢事件、Redis 模式更严重）。
static GLOBAL_LOG: OnceLock<Arc<dyn EventLog>> = OnceLock::new();

pub fn global() -> Arc<dyn EventLog> {
    GLOBAL_LOG
        .get_or_init(|| Arc::new(MemoryEventLog::new()))
        .clone()
}

/// 安装全局事件日志（只允许一次；重复安装被忽略并告警）。
pub fn install(log: Arc<dyn EventLog>) {
    if GLOBAL_LOG.set(log).is_err() {
        tracing::warn!("event log already installed; ignoring reinstall");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Arc<MemoryEventLog> {
        Arc::new(MemoryEventLog::with_capacity(4))
    }

    #[tokio::test]
    async fn append_then_replay_without_cursor_returns_window() {
        let log = memory();
        let sid = Uuid::new_v4();
        log.append(sid, None, "run.started", json!({}));
        log.append(sid, None, "message.delta", json!({ "delta": "hi" }));

        let replay = log.replay(sid, None).await;
        assert!(replay.gap.is_none());
        assert_eq!(replay.events.len(), 2);
        assert_eq!(replay.last_seq, Some(1));
    }

    #[tokio::test]
    async fn stale_cursor_gets_gap_and_retained_events() {
        let log = memory();
        let sid = Uuid::new_v4();
        for i in 0..10 {
            log.append(sid, None, "message.delta", json!({ "i": i }));
        }
        log.trim(sid).await.expect("trim");

        let replay = log.replay(sid, Some(2)).await;
        let gap = replay.gap.expect("gap");
        assert_eq!(gap.from_seq, 3);
        assert_eq!(gap.to_seq, 5);
        assert_eq!(
            replay.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![6, 7, 8, 9]
        );
        assert_eq!(replay.last_seq, Some(9));
    }

    #[tokio::test]
    async fn tail_delivers_only_matching_session_after_cursor() {
        use futures_util::StreamExt;

        let log = memory();
        let sid = Uuid::new_v4();
        let other = Uuid::new_v4();
        let old = log
            .bus()
            .publish(sid, None, "message.delta", json!({ "n": 0 }));
        // 先订阅（broadcast 只投递订阅之后的事件），再追加。
        let mut tail = log.tail(sid, None, Some(old.seq)).await;
        log.append(sid, None, "message.delta", json!({ "n": 1 }));
        log.append(other, None, "run.started", json!({}));
        let next = tokio::time::timeout(std::time::Duration::from_millis(200), tail.next())
            .await
            .expect("tail should deliver")
            .expect("stream item");
        assert_eq!(next.seq, old.seq + 1);
        assert_eq!(next.data["n"], 1);
    }

    #[test]
    fn gap_and_heartbeat_events_are_not_logged() {
        let gap = gap_event(Gap {
            from_seq: 3,
            to_seq: 5,
        });
        assert_eq!(gap.kind, "run.gap");
        assert_eq!(gap.seq, 5, "gap 的 seq 取缺失上界，不回退");

        let hb = heartbeat_event(7);
        assert_eq!(hb.kind, "heartbeat");
        assert!(hb.run_id.is_none());
    }
}
