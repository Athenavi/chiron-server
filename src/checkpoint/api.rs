//! 检查点/恢复的 HTTP 面。
//!
//! - `GET  /api/v1/runs/{id}/checkpoints`：列出快照（seq 倒序）；
//! - `POST /api/v1/runs/{id}/checkpoints`：立即抓一份快照（前端"手动存档"）；
//! - `POST /api/v1/runs/{id}/recover`：用户重试/手动接管。
//!
//! 恢复是**租约 CAS**：抢不到就 409，绝不出现两个实例同时跑同一个 run。

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{snapshot::CheckpointStatus, store};
use crate::{
    agent::control,
    auth::CurrentUser,
    error::{ApiResult, AppError},
    sessions::{is_admin, ok},
    state::AppState,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/runs/{id}/checkpoints",
            get(list_checkpoints).post(create_checkpoint),
        )
        .route("/api/v1/runs/{id}/recover", post(recover_run))
        .route(
            "/api/v1/sessions/{id}/checkpoints",
            get(list_session_checkpoints),
        )
        .route("/api/v1/checkpoints/{id}/restore", post(restore_checkpoint))
}

type RunInfo = (Uuid, String, Option<Uuid>, i32);

/// 归属校验 + 恢复需要的最小 run 信息；越权与不存在一律 404。
async fn load_run(state: &AppState, actor: &CurrentUser, run_id: Uuid) -> ApiResult<RunInfo> {
    let row: Option<RunInfo> = sqlx::query_as(
        "SELECT r.session_id, r.status, r.turn_id, r.recover_attempts FROM runs r \
         JOIN sessions s ON s.id = r.session_id \
         WHERE r.id = $1 AND s.tenant_id = $2 AND ($3::bool OR s.user_id = $4) \
           AND s.status <> 'deleted'",
    )
    .bind(run_id)
    .bind(actor.tenant_id)
    .bind(is_admin(actor))
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?;
    row.ok_or_else(|| AppError::not_found("run not found"))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn list_checkpoints(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(run_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    load_run(&state, &actor, run_id).await?;
    let rows = store::list(&state.db(), run_id, q.limit.unwrap_or(20))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ok(
        json!({ "run_id": run_id, "checkpoints": rows, "count": rows.len() }),
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    #[serde(default)]
    pub label: Option<String>,
}

/// 会话级快照列表（契约 §Checkpoints）。
pub async fn list_session_checkpoints(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    crate::sessions::load_session(&state, &actor, session_id).await?;
    let rows = store::list_for_session(&state.db(), session_id, q.limit.unwrap_or(20))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ok(json!({
        "session_id": session_id,
        "checkpoints": rows,
        "count": rows.len(),
    })))
}

/// 工作区恢复：把工作区 checkout 回某个快照的 commit。
///
/// 只动工作区，不动 run 状态——run 级恢复走 `POST /runs/{id}/recover`。
pub async fn restore_checkpoint(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    if !state.cfg.checkpoint.enabled {
        return Err(AppError::conflict("checkpoints are disabled"));
    }
    let owner: Option<(i16, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT s.tenant_id, s.user_id, s.project_id FROM checkpoints c \
         JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 AND s.tenant_id = $2 AND ($3::bool OR s.user_id = $4) \
           AND s.status <> 'deleted'",
    )
    .bind(id)
    .bind(actor.tenant_id)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?;
    let Some((tenant_id, user_id, project_id)) = owner else {
        return Err(AppError::not_found("checkpoint not found"));
    };

    let checkpoint = store::get(&state.db(), id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
        .ok_or_else(|| AppError::not_found("checkpoint not found"))?;
    let usable = super::snapshot::CheckpointStatus::parse(&checkpoint.status)
        .map(|s| s.is_usable())
        .unwrap_or(false);
    if !usable {
        return Err(AppError::conflict("checkpoint is not usable"));
    }
    let Some(commit) = checkpoint.workspace_commit.as_deref() else {
        return Err(AppError::conflict(
            "checkpoint has no workspace snapshot (run snapshot only)",
        ));
    };

    let sandbox = crate::agent::workspace::ManagerSandbox::new(
        state.sandbox.clone(),
        tenant_id,
        user_id.to_string(),
        project_id,
    );
    let restored = super::workspace::restore(&sandbox, &state.cfg.checkpoint.repo, commit)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if !restored {
        return Err(AppError::internal("workspace restore failed"));
    }
    store::mark_status(&state.db(), id, CheckpointStatus::Restored)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok(ok(json!({
        "checkpoint_id": id,
        "run_id": checkpoint.run_id,
        "commit": commit,
        "restored": true,
    })))
}

pub async fn create_checkpoint(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(run_id): Path<Uuid>,
    body: Option<Json<CreateBody>>,
) -> ApiResult<Response> {
    load_run(&state, &actor, run_id).await?;
    if !state.cfg.checkpoint.enabled {
        return Err(AppError::conflict("checkpoints are disabled"));
    }
    let label = body
        .and_then(|Json(b)| b.label)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "manual".to_string());

    let id = store::capture(&state, run_id, &label)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ok(json!({
        "run_id": run_id,
        "checkpoint_id": id,
        "label": label,
        "created": id.is_some(),
    })))
}

/// 用户重试/手动接管。
///
/// 只有"活跃但没有本实例句柄"的 run 可以被接管；租约 CAS 失败返回 409，
/// 让前端提示"正在别处恢复/仍在运行"。
pub async fn recover_run(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(run_id): Path<Uuid>,
) -> ApiResult<Response> {
    let (session_id, status, _turn_id, attempts) = load_run(&state, &actor, run_id).await?;
    if !state.cfg.checkpoint.enabled {
        return Err(AppError::conflict("checkpoints are disabled"));
    }
    if crate::sessions::run_state::RunStatus::parse(&status)
        .map(|s| s.is_terminal())
        .unwrap_or(true)
    {
        return Err(AppError::conflict(
            "run is already finished; submit a new message",
        ));
    }
    if control::registry().get(run_id).is_some() {
        return Err(AppError::conflict("run is executing on this instance"));
    }

    let checkpoint = store::latest_usable(&state.db(), run_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    let owner = super::reconciler::lease_owner();
    let acquired =
        store::acquire_lease(&state.db(), run_id, &owner, state.cfg.checkpoint.lease_secs)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    if !acquired {
        return Err(AppError::conflict("run is leased by another instance"));
    }

    let (tools, approvals) = store::settle_interrupted_tools(&state.db(), run_id)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if let Some(checkpoint) = &checkpoint {
        store::mark_status(&state.db(), checkpoint.id, CheckpointStatus::Restored)
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
        sqlx::query("UPDATE runs SET last_checkpoint_seq = $2 WHERE id = $1")
            .bind(run_id)
            .bind(checkpoint.seq)
            .execute(&state.db())
            .await?;
    }

    // 恢复 = 重新驱动主循环：历史/预算从 PG + 快照游标重建，已完成的工具不会重跑。
    crate::agent::start_run(state.clone(), state.db(), run_id, session_id);

    Ok(ok(json!({
        "run_id": run_id,
        "session_id": session_id,
        "status": "recovering",
        "checkpoint_seq": checkpoint.as_ref().map(|c| c.seq),
        "attempts": attempts + 1,
        "settled_tools": tools,
        "cancelled_approvals": approvals,
        "accepted": true,
    })))
}
