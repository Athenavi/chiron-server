//! 游标分页：不透明 base64 编码的 `(timestamp, id)`。
//!
//! 会话列表用 `updated_at DESC, id DESC`，消息历史用 `created_at, id ASC`；
//! 两者都只依赖这一对值，SQL 里用行比较 `(ts, id) > ($ts, $id)` 保证顺序稳定，
//! 出现同一时间戳时也不会漏行/重复行。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

use crate::error::AppError;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

impl Cursor {
    pub fn new(at: DateTime<Utc>, id: Uuid) -> Self {
        Self { at, id }
    }

    pub fn encode(&self) -> String {
        let raw = format!(
            "{}|{}",
            self.at.to_rfc3339_opts(SecondsFormat::Micros, true),
            self.id
        );
        URL_SAFE_NO_PAD.encode(raw)
    }

    pub fn decode(token: &str) -> Result<Self, AppError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token.trim())
            .map_err(|_| AppError::bad_request("invalid cursor"))?;
        let text = String::from_utf8(bytes).map_err(|_| AppError::bad_request("invalid cursor"))?;
        let (ts, id) = text
            .split_once('|')
            .ok_or_else(|| AppError::bad_request("invalid cursor"))?;
        let at = DateTime::parse_from_rfc3339(ts)
            .map_err(|_| AppError::bad_request("invalid cursor"))?
            .with_timezone(&Utc);
        let id = Uuid::parse_str(id).map_err(|_| AppError::bad_request("invalid cursor"))?;
        Ok(Self { at, id })
    }
}

/// `limit` 越界时回落到默认值（而不是报 400），便于前端传 `limit=0` 之类的脏值。
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    match limit {
        Some(v) if (1..=MAX_LIMIT).contains(&v) => v,
        _ => DEFAULT_LIMIT,
    }
}

/// 只有"取满一页"才可能还有更多；返回最后一行的游标，否则 `None`。
pub fn next_cursor(rows: usize, limit: i64, last: Option<Cursor>) -> Option<String> {
    if rows as i64 >= limit {
        last.map(|c| c.encode())
    } else {
        None
    }
}

pub fn decode_optional(token: Option<&str>) -> Result<Option<Cursor>, AppError> {
    match token.map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => Ok(Some(Cursor::decode(t)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> Cursor {
        let at = Utc.with_ymd_and_hms(2026, 11, 21, 12, 0, 0).unwrap();
        Cursor::new(
            at,
            Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
        )
    }

    #[test]
    fn cursor_roundtrip_keeps_order_key() {
        let c = sample();
        let decoded = Cursor::decode(&c.encode()).expect("decode");
        assert_eq!(decoded, c);
    }

    #[test]
    fn cursor_encoding_is_opaque_base64() {
        let token = sample().encode();
        assert!(!token.contains('|'));
        // URL-safe：不含 '+' '/' '='（'-' 属于 URL-safe 字母表，可能出现）。
        assert!(!token.contains('+') && !token.contains('/') && !token.contains('='));
    }

    #[test]
    fn garbage_cursors_are_bad_requests() {
        for bad in ["", "not-base64!!", "Zm9v", "MjAyNi0xMS0yMXxub3QtYS11dWlk"] {
            let err = Cursor::decode(bad).expect_err("must reject");
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn limit_is_clamped_to_contract_range() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(-5)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(100)), 100);
        assert_eq!(clamp_limit(Some(101)), DEFAULT_LIMIT);
    }

    #[test]
    fn next_cursor_only_when_page_full() {
        let last = Some(sample());
        assert!(next_cursor(3, 10, last.clone()).is_none());
        assert!(next_cursor(10, 10, last.clone()).is_some());
        // 满页但没有最后一行（理论上不会），也不输出游标。
        assert!(next_cursor(10, 10, None).is_none());
    }

    #[test]
    fn optional_cursor_treats_blank_as_none() {
        assert!(decode_optional(None).unwrap().is_none());
        assert!(decode_optional(Some("   ")).unwrap().is_none());
        assert!(decode_optional(Some(&sample().encode())).unwrap().is_some());
    }
}
