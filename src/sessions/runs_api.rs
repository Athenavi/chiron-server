//! Run 查询、中断、steer、审批的最小状态接口。
//!
//! 这里的 HTTP 响应只表示"已接受"（events.md §5）：最终状态以 SSE 事件为准。

use std::sync::Arc;

use axum::response::Response;
use axum::{
    extract::{Extension, Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    sessions::{
        event_log, is_admin, load_session, ok,
        run_state::{
            ensure_transition, interrupt_target, parse_decision, RunStatus, ACTIVE_STATUS_SQL,
        },
        validate_content,
    },
    state::AppState,
};

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct RunRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub status: String,
    pub error: Option<Value>,
    pub usage: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub lease_owner: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
}

const RUN_COLUMNS: &str = "r.id, r.session_id, r.turn_id, r.status, r.error, r.usage, \
     r.created_at, r.updated_at, r.started_at, r.finished_at, r.lease_owner, r.lease_until";

pub async fn get_run(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Response> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM runs r JOIN sessions s ON s.id = r.session_id \
         WHERE r.id = $1 AND s.tenant_id = $2 AND ($3::bool OR s.user_id = $4) \
           AND s.status <> 'deleted'"
    );
    let run = sqlx::query_as::<_, RunRow>(&sql)
        .bind(run_id)
        .bind(actor.tenant_id)
        .bind(is_admin(&actor))
        .bind(actor.id)
        .fetch_optional(&state.db())
        .await?
        .ok_or_else(|| AppError::not_found("run not found"))?;

    let turn: Option<(String, Option<String>, Value)> =
        sqlx::query_as("SELECT status, model, usage FROM turns WHERE id = $1")
            .bind(run.turn_id)
            .fetch_optional(&state.db())
            .await?;
    let turn = turn.map(|(status, model, usage)| {
        json!({ "id": run.turn_id, "status": status, "model": model, "usage": usage })
    });

    Ok(ok(json!({ "run": run, "turn": turn })))
}

#[derive(Debug, Default, Deserialize)]
pub struct InterruptBody {
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn interrupt_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Option<Json<InterruptBody>>,
) -> ApiResult<Response> {
    load_session(&state, &actor, id).await?;
    let reason = body
        .and_then(|Json(b)| b.reason)
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "user_interrupt".to_string());

    let active_sql = format!(
        "SELECT id, status FROM runs WHERE session_id = $1 AND status IN ({}) \
         ORDER BY created_at DESC, id DESC LIMIT 1",
        ACTIVE_STATUS_SQL
    );
    let active: Option<(Uuid, String)> = sqlx::query_as(&active_sql)
        .bind(id)
        .fetch_optional(&state.db())
        .await?;

    let (run_id, status) = active.ok_or_else(|| AppError::not_found("no active run"))?;
    let current =
        RunStatus::parse(&status).ok_or_else(|| AppError::internal("unknown run status"))?;
    let next = interrupt_target(current).ok_or_else(|| AppError::not_found("no active run"))?;
    ensure_transition(current, next)?;

    // 只更新仍在活跃态的 run，重复中断不会把终态改回去。
    let interrupt_sql = format!(
        "UPDATE runs SET status = $2, \
             error = jsonb_build_object('reason', $3::text), \
             finished_at = now(), updated_at = now() \
         WHERE id = $1 AND status IN ({})",
        ACTIVE_STATUS_SQL
    );
    let affected = sqlx::query(&interrupt_sql)
        .bind(run_id)
        .bind(next.as_str())
        .bind(reason.as_str())
        .execute(&state.db())
        .await?
        .rows_affected();

    if affected == 0 {
        return Err(AppError::conflict("run is no longer active"));
    }
    sqlx::query(
        "UPDATE approvals SET status = 'cancelled', decided_at = now() \
         WHERE run_id = $1 AND status = 'pending'",
    )
    .bind(run_id)
    .execute(&state.db())
    .await?;

    // 让本实例上的循环在轮次/工具边界收敛（DB 终态已写，循环不会覆盖）。
    crate::agent::registry().cancel(run_id);
    // B6：取消联动——父 run 的子 Agent 整棵子树一起停。
    crate::agent::persist::cancel_children(&state, run_id, "parent_interrupted").await;

    event_log::global().append(
        id,
        Some(run_id),
        "run.cancelled",
        json!({ "reason": reason }),
    );

    Ok(ok(json!({
        "run_id": run_id,
        "status": next.as_str(),
        "accepted": true,
        "reason": reason,
    })))
}

#[derive(Debug, Deserialize)]
pub struct SteerBody {
    #[serde(default)]
    pub content: Value,
    #[serde(default)]
    #[allow(dead_code)]
    pub client_id: Option<String>,
}

/// 注入下一轮约束：写一条 `kind='steer'` 的用户消息，不新建 turn/run。
pub async fn steer_session(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Json<SteerBody>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, id).await?;
    if session.status != "active" {
        return Err(AppError::conflict("session is archived"));
    }
    validate_content(&body.content)?;

    let (message_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO messages (session_id, role, kind, content) \
         VALUES ($1, 'user', 'steer', $2) RETURNING id",
    )
    .bind(id)
    .bind(body.content.clone())
    .fetch_one(&state.db())
    .await?;

    let active_sql = format!(
        "SELECT id FROM runs WHERE session_id = $1 AND status IN ({}) \
         ORDER BY created_at DESC, id DESC LIMIT 1",
        ACTIVE_STATUS_SQL
    );
    let active: Option<(Uuid,)> = sqlx::query_as(&active_sql)
        .bind(id)
        .fetch_optional(&state.db())
        .await?;
    let active_run_id = active.map(|(run_id,)| run_id);

    // steer 在下一轮生效：消息已落库，同时注入本实例正在跑的循环。
    if let Some(run_id) = active_run_id {
        let text = crate::agent::context::content_to_text(&body.content);
        crate::agent::registry().push_steer(run_id, &text);
    }

    sqlx::query("UPDATE sessions SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&state.db())
        .await?;

    Ok(ok(json!({
        "session_id": id,
        "message_id": message_id,
        "active_run_id": active_run_id,
        "accepted": true,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ApprovalBody {
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn resolve_approval(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path((id, approval_id)): Path<(Uuid, Uuid)>,
    body: Json<ApprovalBody>,
) -> ApiResult<Response> {
    let Json(body) = body;
    load_session(&state, &actor, id).await?;
    let decision = parse_decision(&body.decision)?;
    let note = body
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());

    let approval: Option<(String, Uuid)> = sqlx::query_as(
        "SELECT a.status, a.run_id FROM approvals a \
         JOIN sessions s ON s.id = a.session_id \
         WHERE a.id = $1 AND a.session_id = $2 AND s.tenant_id = $3 \
           AND ($4::bool OR s.user_id = $5)",
    )
    .bind(approval_id)
    .bind(id)
    .bind(actor.tenant_id)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?;

    let (status, run_id) = approval.ok_or_else(|| AppError::not_found("approval not found"))?;
    if status != "pending" {
        return Err(AppError::conflict("approval already resolved"));
    }

    let affected = sqlx::query(
        "UPDATE approvals SET status = 'resolved', decision = $2, decided_by = $3, \
             decided_at = now(), \
             payload = CASE WHEN $4::text IS NULL THEN payload \
                            ELSE payload || jsonb_build_object('note', $4::text) END \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(approval_id)
    .bind(decision.as_str())
    .bind(actor.id)
    .bind(note.as_deref())
    .execute(&state.db())
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::conflict("approval already resolved"));
    }

    // 该 run 不再有 pending 审批时，从 waiting_approval 回到 running。
    sqlx::query(
        "UPDATE runs SET status = 'running', updated_at = now() \
         WHERE id = $1 AND status = 'waiting_approval' \
           AND NOT EXISTS (SELECT 1 FROM approvals WHERE run_id = $1 AND status = 'pending')",
    )
    .bind(run_id)
    .execute(&state.db())
    .await?;

    // 循环在 waiting_approval 上等待；决策落库后唤醒它继续（或按拒绝处理）。
    crate::agent::registry().wake(run_id);

    event_log::global().append(
        id,
        Some(run_id),
        "approval.resolved",
        json!({ "approval_id": approval_id, "decision": decision.as_str() }),
    );

    Ok(ok(json!({
        "approval_id": approval_id,
        "run_id": run_id,
        "decision": decision.as_str(),
        "status": "resolved",
        "accepted": true,
    })))
}
