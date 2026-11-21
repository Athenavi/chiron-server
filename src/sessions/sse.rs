//! `GET /api/v1/sessions/{id}/events`：SSE + 心跳 + 回放 + `Last-Event-ID`。
//!
//! envelope / seq / gap 语义见 `docs/API.md`。回放与实时续流都走
//! [`EventLog`](super::event_log::EventLog) 抽象，所以 SSE 可以落在任意实例：
//! 内存实现读回放窗口 + broadcast，Redis 实现读 Stream（PRD §4.2）。

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderMap, HeaderValue},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
};
use futures_util::StreamExt;
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::ApiResult,
    sessions::{
        event_log::{gap_event, heartbeat_event, EventLog},
        events::{EventEnvelope, HEARTBEAT_SECS},
        load_session,
    },
    state::AppState,
};

fn sse_event(env: &EventEnvelope) -> Event {
    // SSE 的 `id:` 用 seq，浏览器重连时自动带回 `Last-Event-ID`。
    Event::default()
        .id(env.seq.to_string())
        .data(env.sse_data())
}

fn parse_last_event_id(headers: &HeaderMap) -> Option<i64> {
    let raw = headers.get("last-event-id")?.to_str().ok()?.trim();
    raw.parse::<i64>().ok().filter(|v| *v >= 0)
}

pub async fn session_events(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    load_session(&state, &actor, id).await?;
    let log: Arc<dyn EventLog> = state.events.clone();
    let cursor = parse_last_event_id(&headers);

    let replay = log.replay(id, cursor).await;
    let after_seq = replay.last_seq;
    let tail_cursor = replay.cursor.clone();
    let mut tail = log.tail(id, tail_cursor, after_seq).await;
    // 回放与订阅之间可能落新事件：再补一次回放；SSE 循环按 seq 去重。
    let catchup = log.replay(id, after_seq).await;

    let stream = async_stream::stream! {
        let mut last_seq = after_seq;
        if let Some(gap) = replay.gap {
            let envelope = gap_event(gap);
            last_seq = Some(envelope.seq);
            yield Ok::<Event, Infallible>(sse_event(&envelope));
        }
        for envelope in replay.events {
            last_seq = Some(envelope.seq);
            yield Ok::<Event, Infallible>(sse_event(&envelope));
        }
        for envelope in catchup.events {
            if last_seq.is_some_and(|seq| envelope.seq <= seq) {
                continue;
            }
            last_seq = Some(envelope.seq);
            yield Ok::<Event, Infallible>(sse_event(&envelope));
        }

        let mut ticker = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                received = tail.next() => match received {
                    Some(envelope) => {
                        // 实现侧已按 seq 去重；这里再兜一层，防止回放/续流重叠。
                        if last_seq.is_some_and(|seq| envelope.seq <= seq) {
                            continue;
                        }
                        last_seq = Some(envelope.seq);
                        yield Ok::<Event, Infallible>(sse_event(&envelope));
                    }
                    None => break,
                },
                _ = ticker.tick() => {
                    // 心跳不占日志 seq（多实例下 seq 是共享计数器分配的）。
                    let heartbeat = heartbeat_event(last_seq.unwrap_or(0));
                    yield Ok::<Event, Infallible>(sse_event(&heartbeat));
                }
            }
        }
    };

    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    // 反向代理（nginx/Caddy）默认缓冲会卡住增量事件；显式关闭。
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_last_id(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", value.parse().expect("header value"));
        headers
    }

    #[test]
    fn missing_or_invalid_last_event_id_is_none() {
        assert_eq!(parse_last_event_id(&HeaderMap::new()), None);
        assert_eq!(parse_last_event_id(&headers_with_last_id("  ")), None);
        assert_eq!(parse_last_event_id(&headers_with_last_id("evt_abc")), None);
        assert_eq!(parse_last_event_id(&headers_with_last_id("-1")), None);
    }

    #[test]
    fn numeric_last_event_id_is_a_seq() {
        assert_eq!(parse_last_event_id(&headers_with_last_id("0")), Some(0));
        assert_eq!(parse_last_event_id(&headers_with_last_id(" 42 ")), Some(42));
    }
}
