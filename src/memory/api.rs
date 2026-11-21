//! `/api/v1/memories`：列表/新增/编辑/删除/清空。
//!
//! 记忆属于当前用户（按 tenant + user 过滤）；管理员也不跨用户读写，
//! 避免"后台顺手改别人偏好"。作用域只允许 `global` 或 `session:<uuid>`。

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    model::{self, Slot},
    store::{self, NewMemory, Patch},
};
use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    state::AppState,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/memories",
            get(list_memories)
                .post(create_memory)
                .delete(clear_memories),
        )
        .route(
            "/api/v1/memories/{id}",
            axum::routing::patch(update_memory).delete(delete_memory),
        )
}

fn ok(data: Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub slot: Option<String>,
}

fn parse_scope(raw: Option<&str>, session_id: Option<Uuid>) -> ApiResult<String> {
    model::normalize_scope(raw, session_id).map_err(AppError::bad_request)
}

fn parse_slot(raw: &str) -> ApiResult<Slot> {
    Slot::parse(raw)
        .ok_or_else(|| AppError::bad_request("slot must be persona, preferences, facts or summary"))
}

pub async fn list_memories(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let scope = match q.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_scope(Some(raw), None)?),
        None => None,
    };
    let slot = match q.slot.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_slot(raw)?),
        None => None,
    };
    let rows = store::list(
        &state.db(),
        actor.tenant_id,
        actor.id,
        scope.as_deref(),
        slot,
    )
    .await?;
    Ok(ok(json!({ "memories": rows })))
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub slot: String,
    pub content: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
}

pub async fn create_memory(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let slot = parse_slot(&body.slot)?;
    let scope = parse_scope(body.scope.as_deref(), body.session_id)?;
    let session_id = model::scope_session_id(&scope);
    // `summary` 天然属于某个会话；没有会话作用域时不允许写入。
    if slot == Slot::Summary && session_id.is_none() {
        return Err(AppError::bad_request(
            "summary memories require scope `session:<uuid>` or session_id",
        ));
    }
    // 会话级记忆必须挂在可见会话上，否则外键错误会变成 500。
    if let Some(sid) = session_id {
        crate::sessions::load_session(&state, &actor, sid).await?;
    }
    let row = store::create(
        &state.db(),
        NewMemory {
            tenant_id: actor.tenant_id,
            user_id: actor.id,
            slot,
            scope,
            session_id,
            content: body.content,
            source: body.source.unwrap_or_else(|| "user".to_string()),
            confidence: body.confidence.unwrap_or(1.0),
        },
    )
    .await?;
    Ok(ok(json!({ "memory": row })))
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
}

pub async fn update_memory(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Option<Json<UpdateBody>>,
) -> ApiResult<Response> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let slot = body.slot.as_deref().map(parse_slot).transpose()?;
    let row = store::update(
        &state.db(),
        actor.tenant_id,
        actor.id,
        id,
        Patch {
            content: body.content,
            slot,
            source: body.source,
            confidence: body.confidence,
        },
    )
    .await?;
    Ok(ok(json!({ "memory": row })))
}

pub async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    store::delete(&state.db(), actor.tenant_id, actor.id, id).await?;
    Ok(ok(json!({ "id": id, "deleted": true })))
}

/// 清空：`?scope=` / `?slot=` 可选，缺省清空当前用户的全部记忆。
pub async fn clear_memories(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let scope = match q.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_scope(Some(raw), None)?),
        None => None,
    };
    let slot = match q.slot.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(parse_slot(raw)?),
        None => None,
    };
    let deleted = store::clear(
        &state.db(),
        actor.tenant_id,
        actor.id,
        scope.as_deref(),
        slot,
    )
    .await?;
    Ok(ok(json!({ "deleted": deleted })))
}
