//! 子 Agent 的 HTTP 面：查询父子树、显式派生、取消。
//!
//! 与 contract §5 同口径：派生/取消只返回"已接受"，最终状态经 `subagent.*` 事件下发。

use std::sync::Arc;

use axum::response::Response;
use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{store, tree, Invoker, ParentContext};
use crate::{
    agent::{control, persist, tools::Toolset},
    auth::CurrentUser,
    error::{ApiResult, AppError},
    sessions::{is_admin, ok, run_state::ACTIVE_STATUS_SQL},
    state::AppState,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/runs/{id}/subagents", get(list_run_subagents))
        .route(
            "/api/v1/sessions/{id}/subagents",
            get(list_session_subagents).post(derive_subagent),
        )
        .route("/api/v1/subagents/{id}/cancel", post(cancel_subagent))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListSubagentsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// run 的归属校验：越权与不存在一律 404。
async fn load_run(
    state: &AppState,
    actor: &CurrentUser,
    run_id: Uuid,
) -> ApiResult<(Uuid, String, Option<String>)> {
    let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT r.session_id, r.status, t.model FROM runs r \
         JOIN sessions s ON s.id = r.session_id \
         LEFT JOIN turns t ON t.id = r.turn_id \
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

pub async fn list_run_subagents(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(run_id): Path<Uuid>,
    Query(q): Query<ListSubagentsQuery>,
) -> ApiResult<Response> {
    load_run(&state, &actor, run_id).await?;
    let rows = store::list_for_run(&state.db(), run_id, q.limit.unwrap_or(100))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ok(json!({
        "run_id": run_id,
        "subagents": rows,
        "count": rows.len(),
    })))
}

/// 会话级子 Agent 列表（契约 §Subagents）。
pub async fn list_session_subagents(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<ListSubagentsQuery>,
) -> ApiResult<Response> {
    crate::sessions::load_session(&state, &actor, session_id).await?;
    let rows = store::list_for_session(&state.db(), session_id, q.limit.unwrap_or(100))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(ok(json!({
        "session_id": session_id,
        "subagents": rows,
        "count": rows.len(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct DeriveBody {
    #[serde(default)]
    pub task: Value,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// 显式派生：给会话当前的活跃 run 挂一个子 Agent。
///
/// 子 Agent 的 id 由 `subagent.started` 事件下发（落库先于事件），
/// 因此这里只回 `accepted`，避免 HTTP 响应等待整个子任务跑完。
pub async fn derive_subagent(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(session_id): Path<Uuid>,
    body: Json<DeriveBody>,
) -> ApiResult<Response> {
    if !state.cfg.subagent.enabled {
        return Err(AppError::conflict("subagent is disabled"));
    }
    crate::sessions::load_session(&state, &actor, session_id).await?;

    let task = match &body.task {
        Value::String(s) => s.trim().to_string(),
        other => crate::agent::context::content_to_text(other)
            .trim()
            .to_string(),
    };
    if task.is_empty() {
        return Err(AppError::bad_request("task must not be empty"));
    }
    if task.chars().count() > tree::TASK_MAX_CHARS {
        return Err(AppError::bad_request(format!(
            "task must be at most {} characters",
            tree::TASK_MAX_CHARS
        )));
    }

    let active_sql = format!(
        "SELECT r.id, s.user_id, s.tenant_id, s.project_id, s.model, t.model \
         FROM runs r JOIN sessions s ON s.id = r.session_id \
         LEFT JOIN turns t ON t.id = r.turn_id \
         WHERE r.session_id = $1 AND r.status IN ({ACTIVE_STATUS_SQL}) \
         ORDER BY r.created_at DESC, r.id DESC LIMIT 1"
    );
    // (run_id, user_id, tenant_id, project_id, session_model, turn_model)
    type ActiveRun = (
        Uuid,
        Uuid,
        i16,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<ActiveRun> = sqlx::query_as(&active_sql)
        .bind(session_id)
        .fetch_optional(&state.db())
        .await?;
    let Some((run_id, user_id, tenant_id, project_id, session_model, turn_model)) = row else {
        return Err(AppError::not_found("no active run"));
    };

    let requested = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| turn_model.or(session_model))
        .unwrap_or_else(|| state.cfg.agent.default_model.clone());
    // 子 Agent 的 model 也受套餐白名单与租户启用目录约束。
    crate::tenant::quota::ensure_model_allowed(actor.plan.as_ref(), &requested)?;
    crate::models::ensure_model_allowed(&state, actor.tenant, &requested).await?;
    let (_, model) = persist::resolve_model(&requested, &state.cfg.agent.default_provider)
        .map_err(|e| AppError::bad_request(e.to_string()))?;

    let parent = ParentContext {
        parent_run_id: run_id,
        root_run_id: run_id,
        parent_subagent_id: None,
        session_id,
        tenant_id,
        user_id,
        project_id,
        depth: 0,
        remaining_tokens: state.cfg.agent.max_total_tokens,
    };
    let invoker: Arc<dyn Invoker> = super::runner::build_session_invoker(
        state.clone(),
        parent,
        model.clone(),
        Toolset::from_env(),
    );

    let mut args = json!({ "task": task });
    if let Some(agent) = body
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        args["agent"] = json!(agent);
    }
    if let Some(model) = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args["model"] = json!(model);
    }

    let db = state.db();
    let keys = crate::tenant::current_keys();
    let routes = crate::tenant::current_routes();
    let tier = crate::tenant::current_tier().unwrap_or_default();
    let egress = crate::tenant::current_egress().unwrap_or_default();
    tokio::spawn(crate::tenant::scope_with(
        db,
        keys,
        routes,
        tier,
        egress,
        crate::tenant::current_tenant().unwrap_or_default(),
        async move {
            match invoker.invoke(&args).await {
                Ok(result) => tracing::info!(?result, "derived subagent finished"),
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "derived subagent failed")
                }
            }
        },
    ));

    Ok(ok(json!({
        "session_id": session_id,
        "parent_run_id": run_id,
        "model": model,
        "accepted": true,
    })))
}

pub async fn cancel_subagent(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let owner: Option<(Uuid,)> = sqlx::query_as(
        "SELECT s.id FROM subagent_runs sa \
         JOIN sessions s ON s.id = sa.session_id \
         WHERE sa.id = $1 AND s.tenant_id = $2 AND ($3::bool OR s.user_id = $4) \
           AND s.status <> 'deleted'",
    )
    .bind(id)
    .bind(actor.tenant_id)
    .bind(is_admin(&actor))
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?;
    let (session_id,) = owner.ok_or_else(|| AppError::not_found("subagent not found"))?;

    let cancelled = store::cancel_subtree(&state.db(), id, "user_cancel")
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    // 本实例上的子循环按取消令牌收敛；不在本实例的（多实例）由租约侧处理。
    for sub_id in &cancelled {
        control::registry().cancel(*sub_id);
    }

    Ok(ok(json!({
        "subagent_id": id,
        "session_id": session_id,
        "cancelled": cancelled,
        "accepted": true,
    })))
}
