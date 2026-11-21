//! 管理面游标分页：不透明 base64 编码的 `(timestamp, bigserial id)`。
//!
//! `audit_logs.id` 是 `bigserial`（不是 uuid），所以不能复用会话模块的
//! `Cursor`；排序键同为 `(created_at DESC, id DESC)`，用行比较保证同一时间戳
//! 下不漏行、不重复。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, SecondsFormat, Utc};

use crate::error::AppError;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub at: DateTime<Utc>,
    pub id: i64,
}

impl Cursor {
    pub fn new(at: DateTime<Utc>, id: i64) -> Self {
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
        let id = id
            .parse::<i64>()
            .map_err(|_| AppError::bad_request("invalid cursor"))?;
        Ok(Self { at, id })
    }
}

/// `limit` 越界时回落到默认值（契约 1..=100），避免脏参数直接把查询打成 400。
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    match limit {
        Some(v) if (1..=MAX_LIMIT).contains(&v) => v,
        _ => DEFAULT_LIMIT,
    }
}

/// 只有"取满一页"才可能还有更多。
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
    use chrono::{TimeZone, Timelike};

    fn sample() -> Cursor {
        let at = Utc
            .with_ymd_and_hms(2026, 11, 21, 12, 0, 0)
            .unwrap()
            .with_nanosecond(123_456_000)
            .unwrap();
        Cursor::new(at, 9_007_199_254_740_993)
    }

    #[test]
    fn cursor_roundtrip_keeps_order_key() {
        let c = sample();
        assert_eq!(Cursor::decode(&c.encode()).expect("decode"), c);
    }

    #[test]
    fn cursor_encoding_is_opaque_base64() {
        let token = sample().encode();
        assert!(!token.contains('|'));
        assert!(!token.contains('+') && !token.contains('/') && !token.contains('='));
    }

    #[test]
    fn garbage_cursors_are_bad_requests() {
        for bad in [
            "",
            "not-base64!!",
            "Zm9v",
            "MjAyNi0xMS0yMXxub3QtYS1udW1iZXI",
        ] {
            let err = Cursor::decode(bad).expect_err("must reject");
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(err.code, "bad_request");
        }
    }

    #[test]
    fn limit_is_clamped_to_contract_range() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(-1)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(100)), 100);
        assert_eq!(clamp_limit(Some(101)), DEFAULT_LIMIT);
    }

    #[test]
    fn next_cursor_only_when_page_full() {
        let last = Some(sample());
        assert!(next_cursor(9, 10, last.clone()).is_none());
        assert_eq!(
            next_cursor(10, 10, last).expect("cursor"),
            sample().encode()
        );
        assert!(next_cursor(10, 10, None).is_none());
    }

    #[test]
    fn optional_cursor_treats_blank_as_none() {
        assert!(decode_optional(None).unwrap().is_none());
        assert!(decode_optional(Some("   ")).unwrap().is_none());
        assert!(decode_optional(Some(&sample().encode())).unwrap().is_some());
    }
}
