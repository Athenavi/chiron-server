//! 会话事件总线与 SSE envelope。
//!
//! B2 是单进程实现：每个会话一个进程内环形回放窗口 + 全局 broadcast。
//! 多实例的 Redis Stream 版本在 B11 接上，接口（`publish` / `subscribe` 的语义）
//! 保持不变，SSE 端点不需要改。
//!
//! 约定（docs/API.md）：
//! - `seq` 在会话内单调递增，SSE 的 `id:` 就是 `seq`，用于 `Last-Event-ID`；
//! - 超出保留窗口时先发 `run.gap`，再回放窗口内的部分；
//! - 心跳也占用 `seq` 但不进回放窗口，避免把真实事件挤掉。

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

pub const HEARTBEAT_SECS: u64 = 15;
pub const DEFAULT_REPLAY_CAPACITY: usize = 1024;
const DEFAULT_BROADCAST_CAPACITY: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 全局唯一（`evt_<uuid>`），客户端按它去重。
    pub id: String,
    /// 会话内单调递增；SSE `id:` 字段。
    pub seq: i64,
    /// 会话级事件（heartbeat/gap）为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
    pub ts: DateTime<Utc>,
}

impl EventEnvelope {
    /// SSE 的 `data:` 负载，就是 envelope 本身。
    pub fn sse_data(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Default)]
struct ChannelState {
    events: VecDeque<EventEnvelope>,
    next_seq: i64,
}

pub struct EventBus {
    channels: Mutex<HashMap<Uuid, Arc<Mutex<ChannelState>>>>,
    tx: broadcast::Sender<(Uuid, EventEnvelope)>,
    replay_capacity: usize,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)] // B2 bus 的完整 API：SSE 改走 EventLog 后仅测试与 fallback 使用。
pub struct Subscription {
    /// 需要先投递的 `run.gap`（客户端据此拉快照）。
    pub gap: Option<EventEnvelope>,
    /// 窗口内 `seq > Last-Event-ID` 的事件。
    pub replay: Vec<EventEnvelope>,
    pub receiver: broadcast::Receiver<(Uuid, EventEnvelope)>,
    /// 已投递到的最大 `seq`，用于 broadcast 溢出时计算 gap。
    pub last_seq: Option<i64>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_REPLAY_CAPACITY, DEFAULT_BROADCAST_CAPACITY)
    }

    pub fn with_capacity(replay_capacity: usize, broadcast_capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(broadcast_capacity.max(1));
        Self {
            channels: Mutex::new(HashMap::new()),
            tx,
            replay_capacity: replay_capacity.max(1),
        }
    }

    fn channel(&self, session_id: Uuid) -> Arc<Mutex<ChannelState>> {
        let mut channels = self.lock_channels();
        channels
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(ChannelState::default())))
            .clone()
    }

    fn lock_channels(&self) -> MutexGuard<'_, HashMap<Uuid, Arc<Mutex<ChannelState>>>> {
        self.channels.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_channel(channel: &Mutex<ChannelState>) -> MutexGuard<'_, ChannelState> {
        channel.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn allocate_seq(state: &mut ChannelState) -> i64 {
        let seq = state.next_seq;
        state.next_seq += 1;
        seq
    }

    fn envelope(seq: i64, run_id: Option<Uuid>, kind: &str, data: Value) -> EventEnvelope {
        EventEnvelope {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            seq,
            run_id,
            kind: kind.to_string(),
            data,
            ts: Utc::now(),
        }
    }

    /// 写事件：分配 seq、进回放窗口、广播。返回落库前客户端应看到的完整 envelope。
    pub fn publish(
        &self,
        session_id: Uuid,
        run_id: Option<Uuid>,
        kind: &str,
        data: Value,
    ) -> EventEnvelope {
        let channel = self.channel(session_id);
        let mut state = Self::lock_channel(&channel);
        let seq = Self::allocate_seq(&mut state);
        let env = Self::envelope(seq, run_id, kind, data);
        state.events.push_back(env.clone());
        while state.events.len() > self.replay_capacity {
            state.events.pop_front();
        }
        // 持锁发送：保证新订阅者要么看到快照，要么收到广播，不会两样都看到。
        let _ = self.tx.send((session_id, env.clone()));
        env
    }

    /// 心跳：占 seq 但不进窗口。
    #[allow(dead_code)]
    pub fn heartbeat(&self, session_id: Uuid) -> EventEnvelope {
        let channel = self.channel(session_id);
        let mut state = Self::lock_channel(&channel);
        let seq = Self::allocate_seq(&mut state);
        let now = Utc::now();
        Self::envelope(
            seq,
            None,
            "heartbeat",
            json!({ "server_time": now.to_rfc3339_opts(SecondsFormat::Secs, true) }),
        )
    }

    /// `run.gap`：占 seq 但不进窗口。
    #[allow(dead_code)]
    pub fn gap(&self, session_id: Uuid, from_seq: i64, to_seq: i64) -> EventEnvelope {
        let channel = self.channel(session_id);
        let mut state = Self::lock_channel(&channel);
        let seq = Self::allocate_seq(&mut state);
        Self::envelope(
            seq,
            None,
            "run.gap",
            json!({ "from_seq": from_seq, "to_seq": to_seq }),
        )
    }

    /// 断点续传：先订阅、再取窗口快照，两步在同一把会话锁内完成。
    #[allow(dead_code)]
    pub fn subscribe(&self, session_id: Uuid, last_event_id: Option<i64>) -> Subscription {
        let channel = self.channel(session_id);
        let mut state = Self::lock_channel(&channel);
        let receiver = self.tx.subscribe();

        let (missing, replay) = Self::window_locked(&state, last_event_id);
        let mut gap = None;
        if let Some((from_seq, to_seq)) = missing {
            gap = Some(Self::envelope(
                Self::allocate_seq(&mut state),
                None,
                "run.gap",
                json!({ "from_seq": from_seq, "to_seq": to_seq }),
            ));
        }

        let last_seq = gap
            .as_ref()
            .map(|e| e.seq)
            .or_else(|| replay.last().map(|e| e.seq))
            .or(last_event_id);

        Subscription {
            gap,
            replay,
            receiver,
            last_seq,
        }
    }

    /// 回放窗口快照（不分配 seq、不订阅广播）。B11 的 `EventLog` 抽象用它。
    ///
    /// 返回 `(缺失区间, 窗口内 seq >= after+1 的事件)`；缺失区间为
    /// `Some((from_seq, to_seq))` 时调用方应先发 `run.gap`（events.md §4）。
    pub fn window(
        &self,
        session_id: Uuid,
        after_seq: Option<i64>,
    ) -> (Option<(i64, i64)>, Vec<EventEnvelope>) {
        let channel = self.channel(session_id);
        let state = Self::lock_channel(&channel);
        Self::window_locked(&state, after_seq)
    }

    fn window_locked(
        state: &ChannelState,
        after_seq: Option<i64>,
    ) -> (Option<(i64, i64)>, Vec<EventEnvelope>) {
        let Some(last) = after_seq else {
            return (None, state.events.iter().cloned().collect());
        };
        let next = last.saturating_add(1);
        let mut missing = None;
        if let Some(first_retained) = state.events.front().map(|e| e.seq) {
            if next < first_retained {
                missing = Some((next, first_retained - 1));
            }
        }
        let events = state
            .events
            .iter()
            .filter(|e| e.seq >= next)
            .cloned()
            .collect();
        (missing, events)
    }

    /// 订阅广播（跨会话共享一个 channel，调用方按 session 过滤）。
    pub fn tail(&self) -> broadcast::Receiver<(Uuid, EventEnvelope)> {
        self.tx.subscribe()
    }

    /// 会话内已分配的最大 seq（含心跳/占位），没有事件时为 `None`。
    #[allow(dead_code)]
    pub fn latest_seq(&self, session_id: Uuid) -> Option<i64> {
        let channel = self.channel(session_id);
        let state = Self::lock_channel(&channel);
        (state.next_seq > 0).then(|| state.next_seq - 1)
    }

    /// 保留窗口裁剪到 `keep` 条，返回被裁掉的数量。
    pub fn trim_to(&self, session_id: Uuid, keep: usize) -> u64 {
        let channel = self.channel(session_id);
        let mut state = Self::lock_channel(&channel);
        let keep = keep.max(1);
        let mut dropped = 0;
        while state.events.len() > keep {
            state.events.pop_front();
            dropped += 1;
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_is_monotonic_per_session() {
        let bus = EventBus::with_capacity(16, 16);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        let first = bus.publish(a, None, "run.started", json!({"model": "m"}));
        let second = bus.publish(a, None, "message.delta", json!({"delta": "hi"}));
        let other = bus.publish(b, None, "run.started", json!({}));

        assert_eq!(first.seq, 0);
        assert_eq!(second.seq, 1);
        assert_eq!(other.seq, 0, "seq 是会话内的，不跨会话共享");
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn replay_respects_last_event_id() {
        let bus = EventBus::with_capacity(16, 16);
        let sid = Uuid::new_v4();
        for i in 0..3 {
            bus.publish(sid, None, "message.delta", json!({ "i": i }));
        }

        let full = bus.subscribe(sid, None);
        assert!(full.gap.is_none());
        assert_eq!(full.replay.len(), 3);

        let resumed = bus.subscribe(sid, Some(0));
        assert!(resumed.gap.is_none());
        assert_eq!(
            resumed.replay.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(resumed.last_seq, Some(2));
    }

    #[test]
    fn old_cursor_gets_gap_then_window() {
        let bus = EventBus::with_capacity(4, 16);
        let sid = Uuid::new_v4();
        for i in 0..10 {
            bus.publish(sid, None, "message.delta", json!({ "i": i }));
        }
        // 窗口只保留 seq 6..=9。
        assert_eq!(bus.subscribe(sid, None).replay.len(), 4);

        let sub = bus.subscribe(sid, Some(2));
        let gap = sub.gap.expect("must report gap");
        assert_eq!(gap.kind, "run.gap");
        assert_eq!(gap.data["from_seq"], 3);
        assert_eq!(gap.data["to_seq"], 5);
        assert_eq!(
            sub.replay.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![6, 7, 8, 9]
        );
        // gap 也占 seq，因此 last_seq 是 gap 的 seq。
        assert_eq!(sub.last_seq, Some(gap.seq));
    }

    #[test]
    fn heartbeat_takes_seq_but_not_window_slot() {
        let bus = EventBus::with_capacity(4, 16);
        let sid = Uuid::new_v4();
        let a = bus.publish(sid, None, "run.started", json!({}));
        let hb = bus.heartbeat(sid);
        let b = bus.publish(sid, None, "run.completed", json!({}));

        assert_eq!(a.seq, 0);
        assert_eq!(hb.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(
            bus.subscribe(sid, None).replay.len(),
            2,
            "heartbeat 不占回放窗口"
        );

        // 从心跳的 seq 之后续传：不会重复投递心跳，也不会误报 gap。
        let sub = bus.subscribe(sid, Some(hb.seq));
        assert!(sub.gap.is_none());
        assert_eq!(
            sub.replay.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn subscriber_gets_broadcast_but_no_duplicate_of_snapshot() {
        let bus = EventBus::with_capacity(16, 16);
        let sid = Uuid::new_v4();
        let old = bus.publish(sid, None, "message.delta", json!({"n": 0}));
        let mut sub = bus.subscribe(sid, None);
        assert_eq!(sub.replay.len(), 1);
        assert_eq!(sub.replay[0].id, old.id);

        let fresh = bus.publish(sid, None, "message.delta", json!({"n": 1}));
        // 快照里没有新事件，随后由 broadcast 送达。
        assert_eq!(sub.replay.len(), 1);
        let (got_sid, got) = sub.receiver.try_recv().expect("broadcast");
        assert_eq!(got_sid, sid);
        assert_eq!(got.id, fresh.id);

        // 其他会话的事件会被 SSE 层按 session_id 过滤掉。
        let other = Uuid::new_v4();
        bus.publish(other, None, "run.started", json!({}));
        let (got_sid, _) = sub.receiver.try_recv().expect("broadcast");
        assert_ne!(got_sid, sid);
    }

    #[test]
    fn gap_envelope_has_expected_shape() {
        let bus = EventBus::new();
        let sid = Uuid::new_v4();
        let gap = bus.gap(sid, 5, 9);
        assert_eq!(gap.kind, "run.gap");
        assert_eq!(gap.data, json!({"from_seq": 5, "to_seq": 9}));
        assert!(gap.run_id.is_none());
    }

    #[test]
    fn envelope_serializes_per_contract() {
        let bus = EventBus::new();
        let sid = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let env = bus.publish(
            sid,
            Some(run_id),
            "run.started",
            json!({"model": "x", "turn_id": null}),
        );
        let value: Value = serde_json::from_str(&env.sse_data()).expect("json");
        assert!(value.get("id").is_some());
        assert_eq!(value["seq"], 0);
        assert_eq!(value["type"], "run.started");
        assert_eq!(value["run_id"], run_id.to_string());
        assert!(value.get("ts").is_some());
        assert_eq!(value["data"]["model"], "x");
    }
}
