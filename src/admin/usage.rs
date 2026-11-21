//! `GET /api/v1/admin/usage`：用量聚合（只读，owner/admin）。
//!
//! 数据来源是 B5 写入的 `usage_events`（一次 run 一行终值）与 `tool_calls`
//! （工具耗时 = `updated_at - created_at`）。SQL 只按时间窗口取出分组合计，
//! 跨组汇总（总量、平均值重算）放在纯函数里，单测不依赖数据库。
//!
//! 时间窗口：`days`（默认 7，1..=90）或显式 `from`/`to`（RFC3339，`to` 开区间）。

use std::sync::Arc;

use axum::{
    extract::{Extension, Query, State},
    response::Response,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    admin::{ensure_admin, ok},
    auth::CurrentUser,
    error::AppError,
    state::AppState,
};

const DEFAULT_DAYS: i64 = 7;
const MAX_DAYS: i64 = 90;

#[derive(Debug, Default, Deserialize)]
pub struct UsageQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// 窗口解析：显式 `from`/`to` 优先；缺省回落到 `now - days`。
pub fn resolve_window(
    now: DateTime<Utc>,
    days: Option<i64>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    let days = days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
    let from = match parse_time("from", from)? {
        Some(t) => t,
        None => now - Duration::days(days),
    };
    let to = parse_time("to", to)?.unwrap_or(now);
    if from >= to {
        return Err(AppError::bad_request("from must be before to"));
    }
    Ok((from, to))
}

fn parse_time(name: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, AppError> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| AppError::bad_request(format!("invalid {name} timestamp (RFC3339)"))),
        None => Ok(None),
    }
}

pub fn parse_user_filter(raw: Option<&str>) -> Result<Option<Uuid>, AppError> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            Ok(Some(Uuid::parse_str(v).map_err(|_| {
                AppError::bad_request("invalid user_id filter")
            })?))
        }
        None => Ok(None),
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ModelUsageRow {
    pub model: String,
    pub runs: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct UserUsageRow {
    pub user_id: Uuid,
    pub email: Option<String>,
    pub nickname: Option<String>,
    pub runs: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ToolUsageRow {
    pub name: String,
    pub calls: i64,
    pub failures: i64,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

/// 聚合总量。`total_tokens` 只算输入+输出（缓存单列，避免重复计数）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageTotals {
    pub runs: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cost: f64,
}

pub fn summarize_models(models: &[ModelUsageRow]) -> UsageTotals {
    let mut totals = UsageTotals {
        runs: 0,
        sessions: 0,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        cost: 0.0,
    };
    for m in models {
        // 分组内 sessions 互不相交未知，这里只是求和；
        // HTTP 层会用一次 `COUNT(DISTINCT session_id)` 覆盖成精确值。
        totals.sessions = totals.sessions.saturating_add(m.sessions);
        totals.runs = totals.runs.saturating_add(m.runs);
        totals.input_tokens = totals.input_tokens.saturating_add(m.input_tokens);
        totals.output_tokens = totals.output_tokens.saturating_add(m.output_tokens);
        totals.cache_read_input_tokens = totals
            .cache_read_input_tokens
            .saturating_add(m.cache_read_input_tokens);
        totals.cache_creation_input_tokens = totals
            .cache_creation_input_tokens
            .saturating_add(m.cache_creation_input_tokens);
        totals.cost += m.cost;
    }
    totals.total_tokens = totals.input_tokens.saturating_add(totals.output_tokens);
    totals
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolSummary {
    pub calls: i64,
    pub failures: i64,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

pub fn summarize_tools(tools: &[ToolUsageRow]) -> ToolSummary {
    let mut calls = 0i64;
    let mut failures = 0i64;
    let mut total_ms = 0.0f64;
    let mut max_ms = 0.0f64;
    for t in tools {
        calls = calls.saturating_add(t.calls);
        failures = failures.saturating_add(t.failures);
        total_ms += t.total_ms;
        if t.max_ms > max_ms {
            max_ms = t.max_ms;
        }
    }
    let avg_ms = if calls > 0 {
        total_ms / calls as f64
    } else {
        0.0
    };
    ToolSummary {
        calls,
        failures,
        total_ms,
        avg_ms,
        max_ms,
    }
}

pub async fn get_usage(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<UsageQuery>,
) -> Result<Response, AppError> {
    ensure_admin(&actor)?;
    let (from, to) = resolve_window(Utc::now(), q.days, q.from.as_deref(), q.to.as_deref())?;
    let user_id = parse_user_filter(q.user_id.as_deref())?;
    let tenant_id = actor.tenant_id;

    let models = sqlx::query_as::<_, ModelUsageRow>(
        "SELECT COALESCE(model, '(unknown)') AS model, \
                COUNT(*)::bigint AS runs, \
                COUNT(DISTINCT session_id)::bigint AS sessions, \
                COALESCE(SUM(input_tokens), 0)::bigint AS input_tokens, \
                COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens, \
                COALESCE(SUM(cache_read_input_tokens), 0)::bigint AS cache_read_input_tokens, \
                COALESCE(SUM(cache_creation_input_tokens), 0)::bigint AS cache_creation_input_tokens, \
                COALESCE(SUM(cost), 0)::float8 AS cost \
         FROM usage_events \
         WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3 \
           AND ($4::uuid IS NULL OR user_id = $4) \
         GROUP BY COALESCE(model, '(unknown)') \
         ORDER BY (COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0)) DESC \
         LIMIT 100",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(user_id)
    .fetch_all(&state.db())
    .await?;

    let users = sqlx::query_as::<_, UserUsageRow>(
        "SELECT e.user_id, u.email, u.nickname, \
                COUNT(*)::bigint AS runs, \
                COUNT(DISTINCT e.session_id)::bigint AS sessions, \
                COALESCE(SUM(e.input_tokens), 0)::bigint AS input_tokens, \
                COALESCE(SUM(e.output_tokens), 0)::bigint AS output_tokens, \
                COALESCE(SUM(e.cost), 0)::float8 AS cost \
         FROM usage_events e \
         LEFT JOIN users u ON u.id = e.user_id \
         WHERE e.tenant_id = $1 AND e.created_at >= $2 AND e.created_at < $3 \
           AND ($4::uuid IS NULL OR e.user_id = $4) \
         GROUP BY e.user_id, u.email, u.nickname \
         ORDER BY (COALESCE(SUM(e.input_tokens), 0) + COALESCE(SUM(e.output_tokens), 0)) DESC \
         LIMIT 100",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(user_id)
    .fetch_all(&state.db())
    .await?;

    let tools = sqlx::query_as::<_, ToolUsageRow>(
        "SELECT tc.name, \
                COUNT(*)::bigint AS calls, \
                COUNT(*) FILTER (WHERE tc.status = 'failed')::bigint AS failures, \
                COALESCE(SUM(EXTRACT(EPOCH FROM (tc.updated_at - tc.created_at)) * 1000), 0)::float8 AS total_ms, \
                COALESCE(AVG(EXTRACT(EPOCH FROM (tc.updated_at - tc.created_at)) * 1000), 0)::float8 AS avg_ms, \
                COALESCE(MAX(EXTRACT(EPOCH FROM (tc.updated_at - tc.created_at)) * 1000), 0)::float8 AS max_ms \
         FROM tool_calls tc \
         JOIN sessions s ON s.id = tc.session_id \
         WHERE s.tenant_id = $1 AND tc.created_at >= $2 AND tc.created_at < $3 \
           AND tc.status IN ('completed', 'failed') \
           AND ($4::uuid IS NULL OR s.user_id = $4) \
         GROUP BY tc.name \
         ORDER BY COALESCE(SUM(EXTRACT(EPOCH FROM (tc.updated_at - tc.created_at)) * 1000), 0) DESC \
         LIMIT 50",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(user_id)
    .fetch_all(&state.db())
    .await?;

    let totals = summarize_models(&models);
    // 同一会话会跨模型分组，`summarize_models` 的 sessions 只是上界；
    // 面板要的是精确活跃会话数，单独再取一次。
    let distinct_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT session_id)::bigint FROM usage_events \
         WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3 \
           AND ($4::uuid IS NULL OR user_id = $4)",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(user_id)
    .fetch_one(&state.db())
    .await?;
    let mut totals = totals;
    totals.sessions = distinct_sessions;
    let tool_totals = summarize_tools(&tools);

    Ok(ok(json!({
        "window": { "from": from, "to": to },
        "totals": totals,
        "by_model": models,
        "by_user": users,
        "tools": {
            "totals": tool_totals,
            "by_tool": tools,
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, 12, 0, 0).unwrap()
    }

    fn model(name: &str, runs: i64, input: i64, output: i64, cost: f64) -> ModelUsageRow {
        ModelUsageRow {
            model: name.to_string(),
            runs,
            sessions: runs,
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cost,
        }
    }

    fn tool(name: &str, calls: i64, failures: i64, total_ms: f64, max_ms: f64) -> ToolUsageRow {
        ToolUsageRow {
            name: name.to_string(),
            calls,
            failures,
            total_ms,
            avg_ms: if calls > 0 {
                total_ms / calls as f64
            } else {
                0.0
            },
            max_ms,
        }
    }

    #[test]
    fn default_window_is_seven_days() {
        let (from, to) = resolve_window(now(), None, None, None).expect("window");
        assert_eq!(to, now());
        assert_eq!(from, now() - Duration::days(7));
    }

    #[test]
    fn days_are_clamped_and_explicit_bounds_win() {
        let (from, _) = resolve_window(now(), Some(0), None, None).expect("window");
        assert_eq!(from, now() - Duration::days(1));
        let (from, _) = resolve_window(now(), Some(365), None, None).expect("window");
        assert_eq!(from, now() - Duration::days(90));
        let (from, to) = resolve_window(
            now(),
            Some(30),
            Some("2026-11-01T00:00:00Z"),
            Some("2026-11-02T00:00:00.500Z"),
        )
        .expect("window");
        assert_eq!(from, Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap());
        assert_eq!(
            to,
            Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
                .unwrap()
                .with_nanosecond(500_000_000)
                .unwrap()
        );
    }

    #[test]
    fn window_rejects_inverted_or_bad_bounds() {
        let err = resolve_window(
            now(),
            None,
            Some("2026-11-02T00:00:00Z"),
            Some("2026-11-01T00:00:00Z"),
        )
        .expect_err("must reject");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(resolve_window(now(), None, Some("nope"), None).is_err());
        assert!(resolve_window(now(), None, None, Some("2026-02-30T00:00:00Z")).is_err());
    }

    #[test]
    fn user_filter_accepts_blank_and_uuid() {
        assert!(parse_user_filter(None).unwrap().is_none());
        assert!(parse_user_filter(Some("  ")).unwrap().is_none());
        let id = Uuid::from_u128(0x18f2_f8c0_0000_7000_8000_0000_0000_0001);
        assert_eq!(parse_user_filter(Some(&id.to_string())).unwrap(), Some(id));
        assert!(parse_user_filter(Some("bogus")).is_err());
    }

    #[test]
    fn model_totals_sum_tokens_runs_and_cost() {
        let rows = vec![
            model("gpt-5-mini", 2, 100, 40, 0.25),
            model("claude", 1, 10, 5, 0.125),
        ];
        let totals = summarize_models(&rows);
        assert_eq!(totals.runs, 3);
        assert_eq!(totals.sessions, 3);
        assert_eq!(totals.input_tokens, 110);
        assert_eq!(totals.output_tokens, 45);
        assert_eq!(totals.total_tokens, 155);
        assert!((totals.cost - 0.375).abs() < 1e-9);
    }

    #[test]
    fn cache_tokens_are_reported_separately() {
        let mut row = model("gpt", 1, 1000, 100, 0.0);
        row.cache_read_input_tokens = 900;
        row.cache_creation_input_tokens = 50;
        let totals = summarize_models(&[row]);
        assert_eq!(totals.total_tokens, 1100);
        assert_eq!(totals.cache_read_input_tokens, 900);
        assert_eq!(totals.cache_creation_input_tokens, 50);
    }

    #[test]
    fn empty_usage_has_zero_totals() {
        let totals = summarize_models(&[]);
        assert_eq!(
            totals,
            UsageTotals {
                runs: 0,
                sessions: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost: 0.0,
            }
        );
        let tools = summarize_tools(&[]);
        assert_eq!(tools.calls, 0);
        assert_eq!(tools.avg_ms, 0.0);
    }

    #[test]
    fn tool_summary_recomputes_average_from_totals() {
        let rows = vec![
            tool("read_file", 2, 0, 100.0, 60.0),
            tool("run_code", 3, 1, 900.0, 500.0),
        ];
        let summary = summarize_tools(&rows);
        assert_eq!(summary.calls, 5);
        assert_eq!(summary.failures, 1);
        assert!((summary.total_ms - 1000.0).abs() < 1e-9);
        assert!((summary.avg_ms - 200.0).abs() < 1e-9);
        assert!((summary.max_ms - 500.0).abs() < 1e-9);
    }
}
