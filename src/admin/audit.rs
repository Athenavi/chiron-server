//! `GET /api/v1/admin/audit`：审计查询（只读，owner/admin）。
//!
//! 过滤口径对齐 `docs/PRD.md` B10：租户 / 用户 / 动作 / 时间；
//! 分页用 `(created_at DESC, id DESC)` 的不透明游标（`admin::cursor`）。
//!
//! 租户隔离：admin 只能看自己 token 里的租户；owner 可以用 `tenant_id`
//! 参数跨租户查询（单租户部署下等价于默认值）。所有过滤参数解析与租户
//! 归一化都是纯函数，单测不需要数据库。

use std::sync::Arc;

use axum::{
    extract::{Extension, Query, State},
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    admin::{cursor, ensure_admin, ok},
    auth::CurrentUser,
    error::AppError,
    state::AppState,
};

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub tenant_id: i16,
    pub user_id: Option<Uuid>,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub ip: String,
    pub created_at: DateTime<Utc>,
}

/// 原始查询参数：时间/游标保持字符串，统一由 `build_filters` 校验。
#[derive(Debug, Default, Deserialize)]
pub struct RawAuditQuery {
    #[serde(default)]
    pub tenant_id: Option<i16>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFilters {
    pub tenant_id: i16,
    pub user_id: Option<Uuid>,
    pub action: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub cursor: Option<cursor::Cursor>,
    pub limit: i64,
}

/// 归一化查询参数。
///
/// - `role == "owner"` 时允许 `tenant_id` 过滤，缺省取 token 里的租户；
/// - admin 只能看自己租户，传了别的租户直接忽略（不泄露其它租户是否存在）；
/// - 空字符串一律视为未传；时间必须是 RFC3339。
pub fn build_filters(
    role: &str,
    actor_tenant_id: i16,
    raw: RawAuditQuery,
) -> Result<AuditFilters, AppError> {
    let tenant_id = if role == "owner" {
        raw.tenant_id.unwrap_or(actor_tenant_id)
    } else {
        actor_tenant_id
    };

    let user_id = match raw.user_id.as_deref().map(str::trim) {
        Some(v) if !v.is_empty() => {
            Some(Uuid::parse_str(v).map_err(|_| AppError::bad_request("invalid user_id filter"))?)
        }
        _ => None,
    };

    let action = raw
        .action
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let from = parse_time("from", raw.from.as_deref())?;
    let to = parse_time("to", raw.to.as_deref())?;
    if let (Some(from), Some(to)) = (from, to) {
        if from >= to {
            return Err(AppError::bad_request("from must be before to"));
        }
    }

    Ok(AuditFilters {
        tenant_id,
        user_id,
        action,
        from,
        to,
        cursor: cursor::decode_optional(raw.cursor.as_deref())?,
        limit: cursor::clamp_limit(raw.limit),
    })
}

fn parse_time(name: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, AppError> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| AppError::bad_request(format!("invalid {name} timestamp (RFC3339)"))),
        None => Ok(None),
    }
}

pub async fn list_audit(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(raw): Query<RawAuditQuery>,
) -> Result<Response, AppError> {
    ensure_admin(&actor)?;
    let f = build_filters(&actor.role, actor.tenant_id, raw)?;

    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, tenant_id, user_id, action, target, detail, ip, created_at \
         FROM audit_logs \
         WHERE tenant_id = $1 \
           AND ($2::uuid IS NULL OR user_id = $2) \
           AND ($3::text IS NULL OR action = $3) \
           AND ($4::timestamptz IS NULL OR created_at >= $4) \
           AND ($5::timestamptz IS NULL OR created_at < $5) \
           AND ($6::timestamptz IS NULL OR (created_at, id) < ($6::timestamptz, $7::bigint)) \
         ORDER BY created_at DESC, id DESC \
         LIMIT $8",
    )
    .bind(f.tenant_id)
    .bind(f.user_id)
    .bind(f.action.as_deref())
    .bind(f.from)
    .bind(f.to)
    .bind(f.cursor.as_ref().map(|c| c.at))
    .bind(f.cursor.as_ref().map(|c| c.id).unwrap_or(0))
    .bind(f.limit)
    .fetch_all(&state.db())
    .await?;

    let next = cursor::next_cursor(
        rows.len(),
        f.limit,
        rows.last().map(|r| cursor::Cursor::new(r.created_at, r.id)),
    );
    Ok(ok(json!({
        "audit": rows,
        "next_cursor": next,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sqlx::postgres::PgPoolOptions;

    fn raw() -> RawAuditQuery {
        RawAuditQuery::default()
    }

    #[test]
    fn admin_is_pinned_to_own_tenant() {
        let mut q = raw();
        q.tenant_id = Some(9);
        let f = build_filters("admin", 1, q).expect("filters");
        assert_eq!(f.tenant_id, 1);
    }

    #[test]
    fn owner_may_filter_other_tenant_and_defaults_to_own() {
        let mut q = raw();
        q.tenant_id = Some(9);
        assert_eq!(build_filters("owner", 1, q).unwrap().tenant_id, 9);
        assert_eq!(
            build_filters("owner", 1, RawAuditQuery::default())
                .unwrap()
                .tenant_id,
            1
        );
    }

    #[test]
    fn blank_filters_are_dropped() {
        let mut q = raw();
        q.user_id = Some("   ".into());
        q.action = Some("  ".into());
        q.from = Some("".into());
        let f = build_filters("admin", 1, q).expect("filters");
        assert!(f.user_id.is_none());
        assert!(f.action.is_none());
        assert!(f.from.is_none());
        assert_eq!(f.limit, cursor::DEFAULT_LIMIT);
    }

    #[test]
    fn valid_filters_are_parsed() {
        let mut q = raw();
        q.user_id = Some("018f2f8c-0000-7000-8000-000000000001".into());
        q.action = Some(" auth.login ".into());
        q.from = Some("2026-11-01T00:00:00Z".into());
        q.to = Some("2026-11-02T00:00:00+00:00".into());
        q.limit = Some(20);
        let f = build_filters("admin", 1, q).expect("filters");
        assert_eq!(
            f.user_id,
            Some(Uuid::parse_str("018f2f8c-0000-7000-8000-000000000001").unwrap())
        );
        assert_eq!(f.action.as_deref(), Some("auth.login"));
        assert_eq!(
            f.from,
            Some(Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(
            f.to,
            Some(Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0).unwrap())
        );
        assert_eq!(f.limit, 20);
    }

    #[test]
    fn bad_input_is_a_bad_request() {
        let cases = [
            RawAuditQuery {
                user_id: Some("not-a-uuid".into()),
                ..Default::default()
            },
            RawAuditQuery {
                from: Some("yesterday".into()),
                ..Default::default()
            },
            RawAuditQuery {
                to: Some("2026-13-01T00:00:00Z".into()),
                ..Default::default()
            },
            RawAuditQuery {
                from: Some("2026-11-02T00:00:00Z".into()),
                to: Some("2026-11-01T00:00:00Z".into()),
                ..Default::default()
            },
            RawAuditQuery {
                cursor: Some("!!".into()),
                ..Default::default()
            },
        ];
        for q in cases {
            let err = build_filters("admin", 1, q).expect_err("must reject");
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(err.code, "bad_request");
        }
    }

    #[test]
    fn pagination_flag_follows_limit() {
        let at = Utc.with_ymd_and_hms(2026, 11, 21, 12, 0, 0).unwrap();
        let last = Some(cursor::Cursor::new(at, 42));
        assert!(cursor::next_cursor(9, 10, last.clone()).is_none());
        assert!(cursor::next_cursor(10, 10, last).is_some());
    }

    /// 无库时直接跳过；有 `CHIRON_TEST_DATABASE_URL` 时验证 SQL 形状能执行。
    #[tokio::test]
    async fn audit_query_executes_against_optional_db() {
        let Ok(url) = std::env::var("CHIRON_TEST_DATABASE_URL") else {
            return;
        };
        let pool = match PgPoolOptions::new().max_connections(1).connect(&url).await {
            Ok(pool) => pool,
            Err(_) => return,
        };
        let f = build_filters("admin", 1, RawAuditQuery::default()).expect("filters");
        let result = sqlx::query_as::<_, AuditRow>(
            "SELECT id, tenant_id, user_id, action, target, detail, ip, created_at \
             FROM audit_logs \
             WHERE tenant_id = $1 \
               AND ($2::uuid IS NULL OR user_id = $2) \
               AND ($3::text IS NULL OR action = $3) \
               AND ($4::timestamptz IS NULL OR created_at >= $4) \
               AND ($5::timestamptz IS NULL OR created_at < $5) \
               AND ($6::timestamptz IS NULL OR (created_at, id) < ($6::timestamptz, $7::bigint)) \
             ORDER BY created_at DESC, id DESC \
             LIMIT $8",
        )
        .bind(f.tenant_id)
        .bind(f.user_id)
        .bind(f.action.as_deref())
        .bind(f.from)
        .bind(f.to)
        .bind(f.cursor.as_ref().map(|c| c.at))
        .bind(f.cursor.as_ref().map(|c| c.id).unwrap_or(0))
        .bind(f.limit)
        .fetch_all(&pool)
        .await;
        assert!(result.is_ok(), "audit SQL must execute: {result:?}");
    }
}
