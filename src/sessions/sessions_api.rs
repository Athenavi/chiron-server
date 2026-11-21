//! 会话 CRUD：列表（cursor）、创建、详情、更新（标题/归档）、软删除。

use std::collections::HashMap;
use std::sync::Arc;

use axum::response::Response;
use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    sessions::{
        cursor, event_log, is_admin, load_session, ok, validate_title, SessionRow, SESSION_COLUMNS,
    },
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// `active` / `archived` / `all`（默认不含归档）。
    #[serde(default)]
    pub status: Option<String>,
}

pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<ListSessionsQuery>,
) -> ApiResult<Response> {
    let limit = cursor::clamp_limit(q.limit);
    let page = cursor::decode_optional(q.cursor.as_deref())?;
    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("active") => "active",
        Some("archived") => "archived",
        Some("all") => "all",
        Some(_) => {
            return Err(AppError::bad_request(
                "status must be active, archived or all",
            ))
        }
    };

    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions \
         WHERE tenant_id = $1 AND ($2::bool OR user_id = $3) AND status <> 'deleted' \
           AND ($4 = 'all' OR status = $4) \
           AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5::timestamptz, $6::uuid)) \
         ORDER BY updated_at DESC, id DESC LIMIT $7"
    ))
    .bind(actor.tenant_id)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .bind(status)
    .bind(page.as_ref().map(|c| c.at))
    .bind(page.as_ref().map(|c| c.id).unwrap_or_else(Uuid::nil))
    .bind(limit)
    .fetch_all(&state.db())
    .await?;

    let next = cursor::next_cursor(
        rows.len(),
        limit,
        rows.last().map(|r| cursor::Cursor::new(r.updated_at, r.id)),
    );

    // 列表页需要「哪些会话在等审批」，否则离开会话就看不见 waiting_approval。
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let active_sql = format!(
        "SELECT DISTINCT ON (session_id) session_id, id, status FROM runs \
         WHERE session_id = ANY($1) AND status IN ({}) \
         ORDER BY session_id, created_at DESC, id DESC",
        super::run_state::ACTIVE_STATUS_SQL
    );
    let active_rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(&active_sql)
        .bind(&ids)
        .fetch_all(&state.db())
        .await?;
    let active_runs: HashMap<Uuid, (Uuid, String)> = active_rows
        .into_iter()
        .map(|(session_id, run_id, status)| (session_id, (run_id, status)))
        .collect();

    let approval_rows: Vec<(Uuid, i64)> = if ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT session_id, count(*)::bigint FROM approvals \
             WHERE session_id = ANY($1) AND status = 'pending' GROUP BY session_id",
        )
        .bind(&ids)
        .fetch_all(&state.db())
        .await?
    };
    let approval_counts: HashMap<Uuid, i64> = approval_rows.into_iter().collect();

    let sessions: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut value = serde_json::to_value(row).unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                let active = active_runs.get(&row.id);
                object.insert(
                    "active_run".to_string(),
                    match active {
                        Some((run_id, status)) => {
                            json!({ "run_id": run_id, "status": status })
                        }
                        None => Value::Null,
                    },
                );
                object.insert(
                    "pending_approvals".to_string(),
                    json!(approval_counts.get(&row.id).copied().unwrap_or(0)),
                );
            }
            value
        })
        .collect();

    let (pending_total,): (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM approvals a JOIN sessions s ON s.id = a.session_id \
         WHERE s.tenant_id = $1 AND ($2::bool OR s.user_id = $3) \
           AND s.status = 'active' AND a.status = 'pending'",
    )
    .bind(actor.tenant_id)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .fetch_one(&state.db())
    .await?;

    Ok(ok(json!({
        "sessions": sessions,
        "next_cursor": next,
        "pending_approvals_total": pending_total,
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateSessionBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<Uuid>,
}

/// project_id 会拼进宿主工作区路径：只允许单段安全标识符，拒绝点段。
fn validate_project_id(raw: &str) -> Result<String, AppError> {
    let value = raw.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(AppError::bad_request(
            "project_id must be 1-64 chars of [A-Za-z0-9_-]",
        ));
    }
    Ok(value.to_string())
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    body: Option<Json<CreateSessionBody>>,
) -> ApiResult<Response> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let title = validate_title(body.title)?.unwrap_or_else(|| "New session".to_string());
    if let Some(parent) = body.parent_session_id {
        // 分支会话：父会话必须可见，但不必是本人（管理视角允许）。
        load_session(&state, &actor, parent).await?;
    }
    let project_id = match body.project_id.map(|p| p.trim().to_string()) {
        Some(p) if !p.is_empty() => Some(validate_project_id(&p)?),
        _ => None,
    };

    let row = sqlx::query_as::<_, SessionRow>(&format!(
        "INSERT INTO sessions (tenant_id, user_id, parent_session_id, title, project_id, model) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {SESSION_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(actor.id)
    .bind(body.parent_session_id)
    .bind(title)
    .bind(project_id)
    .bind(body.model)
    .fetch_one(&state.db())
    .await?;

    Ok(ok(json!({ "session": row })))
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, id).await?;
    let active_sql = format!(
        "SELECT id, status FROM runs WHERE session_id = $1 \
         AND status IN ({}) ORDER BY created_at DESC, id DESC LIMIT 1",
        super::run_state::ACTIVE_STATUS_SQL
    );
    let active_run: Option<(Uuid, String)> = sqlx::query_as(&active_sql)
        .bind(id)
        .fetch_optional(&state.db())
        .await?;

    Ok(ok(json!({
        "session": session,
        "active_run": active_run.map(|(run_id, status)| json!({
            "run_id": run_id,
            "status": status,
        })),
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateSessionBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// true 归档，false 取消归档。
    #[serde(default)]
    pub archived: Option<bool>,
}

pub async fn update_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Option<Json<UpdateSessionBody>>,
) -> ApiResult<Response> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    if body.title.is_none() && body.model.is_none() && body.archived.is_none() {
        return Err(AppError::bad_request("nothing to update"));
    }
    let title = validate_title(body.title)?;
    let model = body
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());

    let row = sqlx::query_as::<_, SessionRow>(&format!(
        "UPDATE sessions SET \
           title = COALESCE($3, title), \
           model = COALESCE($4, model), \
           status = CASE \
               WHEN $5::bool IS TRUE THEN 'archived' \
               WHEN $5::bool IS FALSE THEN 'active' \
               ELSE status END, \
           archived_at = CASE \
               WHEN $5::bool IS TRUE THEN COALESCE(archived_at, now()) \
               WHEN $5::bool IS FALSE THEN NULL \
               ELSE archived_at END, \
           updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND ($6::bool OR user_id = $7) \
           AND status <> 'deleted' \
         RETURNING {SESSION_COLUMNS}"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .bind(title)
    .bind(model)
    .bind(body.archived)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("session not found"))?;

    Ok(ok(json!({ "session": row })))
}

/// 软删除：先取消活跃 run（事件里告知前端），会话行保留 `status='deleted'`。
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    load_session(&state, &actor, id).await?;

    let mut tx = state.db().begin().await?;
    let cancel_sql = format!(
        "UPDATE runs SET status = 'cancelled', \
             error = jsonb_build_object('reason', 'session_deleted'), \
             finished_at = now(), updated_at = now() \
         WHERE session_id = $1 AND status IN ({}) RETURNING id",
        super::run_state::ACTIVE_STATUS_SQL
    );
    let cancelled: Vec<(Uuid,)> = sqlx::query_as(&cancel_sql)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE approvals SET status = 'cancelled', decided_at = now() \
         WHERE session_id = $1 AND status = 'pending'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    let deleted: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE sessions SET status = 'deleted', deleted_at = now(), updated_at = now() \
         WHERE id = $1 AND status <> 'deleted' RETURNING id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;

    if deleted.is_none() {
        return Err(AppError::not_found("session not found"));
    }
    let cancelled_ids: Vec<Uuid> = cancelled.iter().map(|(run_id,)| *run_id).collect();
    for run_id in &cancelled_ids {
        event_log::global().append(
            id,
            Some(*run_id),
            "run.cancelled",
            json!({ "reason": "session_deleted" }),
        );
        // B6：会话删除是显式取消源，子 Agent 子树跟着停（PRD §7.5）。
        crate::agent::persist::cancel_children(&state, *run_id, "session_deleted").await;
    }

    // 给取消事件一个落库/推送窗口，再清掉整条会话的事件与 seq key。
    let purge_id = id;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        if let Err(err) = event_log::global().purge(purge_id).await {
            tracing::warn!(%purge_id, error = %err, "event log purge failed");
        }
    });

    let data: Value = json!({
        "id": id,
        "status": "deleted",
        "cancelled_run_ids": cancelled_ids,
    });
    Ok(ok(data))
}
