//! 会话 / 消息 / Run / SSE。
//!
//! 只暴露 `router()`，由 `main.rs` 合并进受保护路由；事件走
//! [`event_log`] 抽象（内存 bus 是 fallback，多实例用 Redis Streams）。

mod containers;
mod cursor;
pub(crate) mod event_log;
mod events;
pub mod files_api;
pub(crate) mod jobs;
pub(crate) mod lease;
mod messages_api;
#[cfg(test)]
mod multi_instance;
pub(crate) mod redis_log;
pub(crate) mod run_state;
mod runs_api;
mod sessions_api;
mod sse;

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{auth::CurrentUser, error::AppError, state::AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/sessions",
            get(sessions_api::list_sessions).post(sessions_api::create_session),
        )
        .route(
            "/api/v1/sessions/{id}",
            get(sessions_api::get_session)
                .patch(sessions_api::update_session)
                .delete(sessions_api::delete_session),
        )
        .route(
            "/api/v1/sessions/{id}/messages",
            get(messages_api::list_messages).post(messages_api::create_message),
        )
        .route("/api/v1/sessions/{id}/events", get(sse::session_events))
        .route("/api/v1/sessions/{id}/files", get(files_api::list_files))
        .route(
            "/api/v1/sessions/{id}/files/download",
            get(files_api::download_file),
        )
        .route(
            "/api/v1/sessions/{id}/container",
            get(containers::session_container),
        )
        .route(
            "/api/v1/sessions/{id}/interrupt",
            post(runs_api::interrupt_session),
        )
        .route("/api/v1/sessions/{id}/steer", post(runs_api::steer_session))
        .route(
            "/api/v1/sessions/{id}/approvals/{approval_id}",
            post(runs_api::resolve_approval),
        )
        .route("/api/v1/runs/{id}", get(runs_api::get_run))
}

pub fn ok(data: Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

pub(crate) fn is_admin(actor: &CurrentUser) -> bool {
    matches!(actor.role.as_str(), "owner" | "admin")
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct SessionRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub user_id: Uuid,
    pub parent_session_id: Option<Uuid>,
    pub title: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// 会话级对话偏好；缺失字段用安全默认值。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ConversationSettings {
    /// ask（默认）| auto | readonly
    pub tool_authorization: String,
    /// concise | balanced（默认）| detailed
    pub reply_style: String,
    /// auto（默认）| zh | en
    pub reply_language: String,
    /// auto（默认）| zh | en
    pub thinking_language: String,
    /// auto（默认）| low | medium | high
    pub reasoning_effort: String,
}

impl Default for ConversationSettings {
    fn default() -> Self {
        Self {
            tool_authorization: "ask".into(),
            reply_style: "balanced".into(),
            reply_language: "auto".into(),
            thinking_language: "auto".into(),
            reasoning_effort: "auto".into(),
        }
    }
}

impl ConversationSettings {
    pub fn from_metadata(metadata: &serde_json::Value) -> Self {
        let parsed: ConversationSettings = metadata
            .get("chat")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        parsed.validate().unwrap_or_default()
    }

    pub fn validate(self) -> Result<Self, AppError> {
        for (label, value, allowed) in [
            (
                "tool_authorization",
                self.tool_authorization.as_str(),
                &["ask", "auto", "readonly"][..],
            ),
            (
                "reply_style",
                self.reply_style.as_str(),
                &["concise", "balanced", "detailed"][..],
            ),
            (
                "reply_language",
                self.reply_language.as_str(),
                &["auto", "zh", "en"][..],
            ),
            (
                "thinking_language",
                self.thinking_language.as_str(),
                &["auto", "zh", "en"][..],
            ),
            (
                "reasoning_effort",
                self.reasoning_effort.as_str(),
                &["auto", "low", "medium", "high"][..],
            ),
        ] {
            if !allowed.contains(&value) {
                return Err(AppError::bad_request(format!("invalid {label}")));
            }
        }
        Ok(self)
    }

    pub fn prompt_suffix(&self) -> String {
        let mut lines = Vec::new();
        match self.reply_style.as_str() {
            "concise" => lines.push("Reply concisely; omit filler and repeated context."),
            "detailed" => lines.push("Give a thorough answer with reasoning, examples and edge cases."),
            _ => {}
        }
        match self.reply_language.as_str() {
            "zh" => lines.push("Reply in Chinese."),
            "en" => lines.push("Reply in English."),
            _ => {}
        }
        match self.thinking_language.as_str() {
            "zh" => lines.push("Think through the problem in Chinese before answering."),
            "en" => lines.push("Think through the problem in English before answering."),
            _ => {}
        }
        lines.join("\n")
    }
}

pub(crate) const SESSION_COLUMNS: &str = "id, tenant_id, user_id, parent_session_id, title, \
     project_id, model, status, metadata, created_at, updated_at, archived_at, deleted_at";

/// 读取会话并做租户/所属校验；越权与不存在一律 404，避免泄露存在性。
pub(crate) async fn load_session(
    state: &AppState,
    actor: &CurrentUser,
    id: Uuid,
) -> Result<SessionRow, AppError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions \
         WHERE id = $1 AND tenant_id = $2 AND ($3::bool OR user_id = $4) AND status <> 'deleted'"
    );
    sqlx::query_as::<_, SessionRow>(&sql)
        .bind(id)
        .bind(actor.tenant_id)
        .bind(is_admin(actor))
        .bind(actor.id)
        .fetch_optional(&state.db())
        .await?
        .ok_or_else(|| AppError::not_found("session not found"))
}

/// 消息内容可以是字符串或结构化块数组；空内容不是一次有效提交。
pub(crate) fn validate_content(content: &Value) -> Result<(), AppError> {
    let is_empty = match content {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    };
    if is_empty {
        return Err(AppError::bad_request("content must not be empty"));
    }
    if serde_json::to_vec(content).map(|v| v.len()).unwrap_or(0) > 256 * 1024 {
        return Err(AppError::new(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "message content exceeds 256 KiB",
        ));
    }
    Ok(())
}

pub(crate) fn validate_title(title: Option<String>) -> Result<Option<String>, AppError> {
    match title {
        None => Ok(None),
        Some(t) => {
            let t = t.trim().to_string();
            if t.chars().count() > 200 {
                return Err(AppError::bad_request(
                    "title must be at most 200 characters",
                ));
            }
            if t.is_empty() {
                Ok(None)
            } else {
                Ok(Some(t))
            }
        }
    }
}
