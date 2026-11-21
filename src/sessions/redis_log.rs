//! Redis Streams 事件日志：按会话分 key（`chiron:evt:<session>`）。
//!
//! 只在配置了 `REDIS_URL` 时启用（[`super::event_log::install`]），否则用内存
//! fallback。写入是"入队 + 后台 XADD"，因此 `append` 保持同步签名（调用方是
//! agent 循环，不能为发事件阻塞或失败）。
//!
//! - `seq` 用 `INCR chiron:evt:<session>:seq` 分配，跨实例单调；
//! - 保留窗口由 `XADD MAXLEN ~ capacity` + 定期 `XTRIM MINID`（时间窗）维护；
//! - `tail` 轮询 `XRANGE (cursor +`（要求 Redis 6.2+ 的排他区间），
//!   延迟由 `poll_ms` 控制。

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use redis::aio::ConnectionManager;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::redis_ha::RedisHandle;

use super::{
    event_log::{EventLog, EventLogError, EventTail, Gap, Replay},
    events::EventEnvelope,
};

const MINID_EVERY: u64 = 64;
/// 事件写队列上限：Redis 变慢时宁可丢弃可重连续传的事件，也不让进程 OOM。
const APPEND_QUEUE: usize = 4096;

struct AppendCmd {
    session_id: Uuid,
    run_id: Option<Uuid>,
    kind: String,
    data: Value,
}

pub struct RedisEventLog {
    tx: mpsc::Sender<AppendCmd>,
    /// R-2：所有读写按操作从句柄取连接，Sentinel 切主后自动走新主。
    handle: RedisHandle,
    capacity: usize,
    retention_secs: u64,
    poll_ms: u64,
}

fn stream_key(session_id: Uuid) -> String {
    format!("chiron:evt:{session_id}")
}

fn seq_key(session_id: Uuid) -> String {
    format!("chiron:evt:{session_id}:seq")
}

impl RedisEventLog {
    /// 单连接构造（测试与单节点路径）：包一层句柄，语义不变。
    #[allow(dead_code)] // 生产路径用 `with_handle`；保留给单实例测试。
    pub fn with_connection(
        conn: ConnectionManager,
        capacity: usize,
        retention_secs: u64,
        poll_ms: u64,
    ) -> Self {
        Self::with_handle(RedisHandle::new(conn), capacity, retention_secs, poll_ms)
    }

    /// R-2：主入口。句柄可被 Sentinel 刷新任务替换，读写始终取当前连接。
    pub fn with_handle(
        handle: RedisHandle,
        capacity: usize,
        retention_secs: u64,
        poll_ms: u64,
    ) -> Self {
        let (tx, rx) = mpsc::channel(APPEND_QUEUE);
        let log = Self {
            tx,
            handle: handle.clone(),
            capacity: capacity.max(1),
            retention_secs: retention_secs.max(1),
            poll_ms: poll_ms.clamp(50, 5_000),
        };
        tokio::spawn(writer(handle, rx, log.capacity, log.retention_secs));
        log
    }
}

async fn writer(
    handle: RedisHandle,
    mut rx: mpsc::Receiver<AppendCmd>,
    capacity: usize,
    retention_secs: u64,
) {
    let mut written: u64 = 0;
    while let Some(cmd) = rx.recv().await {
        // 每条事件取一次连接：切主后下一条即走新主，不在后台任务里长期钉住旧连接。
        let mut conn = handle.conn().await;
        let seq: Result<i64, _> = redis::cmd("INCR")
            .arg(seq_key(cmd.session_id))
            .query_async(&mut conn)
            .await;
        let seq = match seq {
            Ok(seq) => seq,
            Err(err) => {
                tracing::warn!(error = %err, session = %cmd.session_id, "event seq alloc failed");
                continue;
            }
        };
        let envelope = EventEnvelope {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            seq,
            run_id: cmd.run_id,
            kind: cmd.kind,
            data: cmd.data,
            ts: Utc::now(),
        };
        let payload = match serde_json::to_string(&envelope) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(error = %err, "event serialize failed");
                continue;
            }
        };
        let added: Result<String, _> = redis::cmd("XADD")
            .arg(stream_key(cmd.session_id))
            .arg("MAXLEN")
            .arg("~")
            .arg(capacity)
            .arg("*")
            .arg("env")
            .arg(payload)
            .query_async(&mut conn)
            .await;
        if let Err(err) = added {
            tracing::warn!(error = %err, session = %cmd.session_id, "event XADD failed");
            continue;
        }
        written += 1;
        if written % MINID_EVERY == 0 {
            let cutoff = Utc::now().timestamp_millis() - (retention_secs as i64 * 1_000);
            let _: Result<i64, _> = redis::cmd("XTRIM")
                .arg(stream_key(cmd.session_id))
                .arg("MINID")
                .arg(format!("{cutoff}-0"))
                .query_async(&mut conn)
                .await;
        }
    }
}

/// 读取整个保留窗口（`MAXLEN ~ capacity` 已限制条数）。
async fn read_stream(
    conn: &mut ConnectionManager,
    session_id: Uuid,
) -> Result<(Vec<(String, EventEnvelope)>, Option<String>), EventLogError> {
    let raw: Vec<(String, Vec<String>)> = redis::cmd("XRANGE")
        .arg(stream_key(session_id))
        .arg("-")
        .arg("+")
        .query_async(conn)
        .await
        .map_err(|e| EventLogError::Unavailable(e.to_string()))?;
    let mut entries = Vec::with_capacity(raw.len());
    let mut last_id = None;
    for (id, fields) in raw {
        last_id = Some(id.clone());
        let Some(pos) = fields.iter().position(|f| f == "env") else {
            continue;
        };
        let Some(payload) = fields.get(pos + 1) else {
            continue;
        };
        match serde_json::from_str::<EventEnvelope>(payload) {
            Ok(envelope) => entries.push((id, envelope)),
            Err(err) => tracing::warn!(error = %err, stream_id = %id, "skip malformed event"),
        }
    }
    Ok((entries, last_id))
}

#[async_trait]
impl EventLog for RedisEventLog {
    fn append(&self, session_id: Uuid, run_id: Option<Uuid>, kind: &str, data: Value) {
        let cmd = AppendCmd {
            session_id,
            run_id,
            kind: kind.to_string(),
            data,
        };
        if self.tx.try_send(cmd).is_err() {
            tracing::warn!(%session_id, "event log queue full/closed; dropping event");
        }
    }

    async fn replay(&self, session_id: Uuid, after: Option<i64>) -> Replay {
        let mut conn = self.handle.conn().await;
        let (entries, last_id) = match read_stream(&mut conn, session_id).await {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(error = %err, %session_id, "event replay read failed");
                return Replay::new(None, Vec::new(), after, None);
            }
        };
        let mut events: Vec<EventEnvelope> = entries
            .into_iter()
            .map(|(_, envelope)| envelope)
            .filter(|e| after.map_or(true, |seq| e.seq > seq))
            .collect();
        events.sort_by_key(|e| e.seq);

        let gap = after.and_then(|after| {
            let first = events.first().map(|e| e.seq)?;
            (first > after.saturating_add(1)).then_some(Gap {
                from_seq: after + 1,
                to_seq: first - 1,
            })
        });
        Replay::new(gap, events, after, last_id)
    }

    async fn tail(
        &self,
        session_id: Uuid,
        cursor: Option<String>,
        after_seq: Option<i64>,
    ) -> EventTail {
        let mut cursor = cursor.unwrap_or_else(|| "0-0".to_string());
        let mut last_seq = after_seq;
        let poll = Duration::from_millis(self.poll_ms);
        // 长连接流按轮取连接：切主后最多一个 poll 周期就恢复读取。
        let handle = self.handle.clone();

        Box::pin(async_stream::stream! {
            loop {
                let mut conn = handle.conn().await;
                let raw: Result<Vec<(String, Vec<String>)>, _> = redis::cmd("XRANGE")
                    .arg(stream_key(session_id))
                    .arg(format!("({cursor}"))
                    .arg("+")
                    .query_async(&mut conn)
                    .await;
                match raw {
                    Ok(entries) => {
                        for (id, fields) in entries {
                            cursor = id.clone();
                            let Some(pos) = fields.iter().position(|f| f == "env") else {
                                continue;
                            };
                            let Some(payload) = fields.get(pos + 1) else {
                                continue;
                            };
                            let Ok(envelope) =
                                serde_json::from_str::<EventEnvelope>(payload)
                            else {
                                continue;
                            };
                            if last_seq.is_some_and(|seq| envelope.seq <= seq) {
                                continue;
                            }
                            last_seq = Some(envelope.seq);
                            yield envelope;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, %session_id, "event tail read failed");
                        tokio::time::sleep(poll).await;
                        continue;
                    }
                }
                tokio::time::sleep(poll).await;
            }
        })
    }

    async fn trim(&self, session_id: Uuid) -> Result<u64, EventLogError> {
        let mut conn = self.handle.conn().await;
        let before: i64 = redis::cmd("XLEN")
            .arg(stream_key(session_id))
            .query_async(&mut conn)
            .await
            .map_err(|e| EventLogError::Unavailable(e.to_string()))?;
        let _: Result<i64, _> = redis::cmd("XTRIM")
            .arg(stream_key(session_id))
            .arg("MAXLEN")
            .arg("~")
            .arg(self.capacity)
            .query_async(&mut conn)
            .await;
        let cutoff = Utc::now().timestamp_millis() - (self.retention_secs as i64 * 1_000);
        let _: Result<i64, _> = redis::cmd("XTRIM")
            .arg(stream_key(session_id))
            .arg("MINID")
            .arg(format!("{cutoff}-0"))
            .query_async(&mut conn)
            .await;
        let after: i64 = redis::cmd("XLEN")
            .arg(stream_key(session_id))
            .query_async(&mut conn)
            .await
            .map_err(|e| EventLogError::Unavailable(e.to_string()))?;
        Ok((before - after).max(0) as u64)
    }

    async fn purge(&self, session_id: Uuid) -> Result<(), EventLogError> {
        let mut conn = self.handle.conn().await;
        redis::cmd("DEL")
            .arg(stream_key(session_id))
            .arg(seq_key(session_id))
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| EventLogError::Unavailable(e.to_string()))
    }
}

/// 事件流键前缀（维护任务扫描用）。
#[allow(dead_code)]
pub fn key_prefix() -> &'static str {
    "chiron:evt:"
}

/// `tail` 用 `(id` 排他区间，该语法从 Redis 6.2 起才有；
/// 桌面/部署默认 Redis 7（见 docker-compose.dev.yml）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_session_scoped() {
        let sid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(stream_key(sid), format!("chiron:evt:{sid}"));
        assert_eq!(seq_key(sid), format!("chiron:evt:{sid}:seq"));
    }
}
