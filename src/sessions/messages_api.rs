//! 消息历史（cursor）与"提交一轮"（幂等键 + 创建 queued run）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::response::Response;
use axum::{
    extract::{Extension, Path, Query, State},
    http::HeaderMap,
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
        cursor, load_session, ok, run_state::ensure_no_active_run, run_state::is_unique_violation,
        run_state::request_hash, run_state::run_busy_error, run_state::validate_idempotency_key,
        run_state::RunStatus, run_state::ACTIVE_STATUS_SQL, validate_content,
    },
    state::AppState,
};

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct MessageRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub turn_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub role: String,
    pub kind: String,
    pub content: Value,
    pub created_at: DateTime<Utc>,
}

const MESSAGE_COLUMNS: &str = "id, session_id, turn_id, run_id, role, kind, content, created_at";

/// 历史消息里带出的工具终态；实时事件不回放，刷新后靠它还原工具卡。
#[derive(Debug, FromRow)]
struct HistoryToolCall {
    id: Uuid,
    run_id: Uuid,
    name: String,
    status: String,
    result: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListMessagesQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn list_messages(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Query(q): Query<ListMessagesQuery>,
) -> ApiResult<Response> {
    load_session(&state, &actor, id).await?;
    let limit = cursor::clamp_limit(q.limit);
    let page = cursor::decode_optional(q.cursor.as_deref())?;

    let rows = sqlx::query_as::<_, MessageRow>(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages \
         WHERE session_id = $1 \
           AND ($2::timestamptz IS NULL OR (created_at, id) > ($2::timestamptz, $3::uuid)) \
         ORDER BY created_at ASC, id ASC LIMIT $4"
    ))
    .bind(id)
    .bind(page.as_ref().map(|c| c.at))
    .bind(page.as_ref().map(|c| c.id).unwrap_or_else(Uuid::nil))
    .bind(limit)
    .fetch_all(&state.db())
    .await?;

    let next = cursor::next_cursor(
        rows.len(),
        limit,
        rows.last().map(|r| cursor::Cursor::new(r.created_at, r.id)),
    );

    // 工具卡的终态只存在 tool_calls 表里：按 run 取回，按调用名顺序配对进消息内容。
    let run_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.run_id).collect();
    let tool_rows: Vec<HistoryToolCall> = if run_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, run_id, name, status, result, error FROM tool_calls \
             WHERE session_id = $1 AND run_id = ANY($2) ORDER BY created_at, id",
        )
        .bind(id)
        .bind(&run_ids)
        .fetch_all(&state.db())
        .await?
    };
    let mut pending_tools: HashMap<Uuid, Vec<HistoryToolCall>> = HashMap::new();
    for tool in tool_rows {
        pending_tools.entry(tool.run_id).or_default().push(tool);
    }

    let messages: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let mut value = serde_json::to_value(&row).unwrap_or(Value::Null);
            if let Some(run_id) = row.run_id {
                if let Some(calls) = value
                    .get_mut("content")
                    .and_then(|content| content.get_mut("tool_calls"))
                    .and_then(Value::as_array_mut)
                {
                    if let Some(queue) = pending_tools.get_mut(&run_id) {
                        for call in calls {
                            let Some(name) = call.get("name").and_then(Value::as_str) else {
                                continue;
                            };
                            let Some(pos) =
                                queue.iter().position(|tool| tool.name.as_str() == name)
                            else {
                                continue;
                            };
                            let done = queue.remove(pos);
                            if let Some(object) = call.as_object_mut() {
                                // 与实时 tool.call/tool.result 使用同一个 id，重连后不会重复成两张卡。
                                object.insert(
                                    "tool_call_id".into(),
                                    Value::String(done.id.to_string()),
                                );
                                object.insert("status".into(), Value::String(done.status));
                                if let Some(result) = done.result {
                                    object.insert("result".into(), result);
                                }
                                if let Some(error) = done.error {
                                    object.insert("error".into(), Value::String(error));
                                }
                            }
                        }
                    }
                }
            }
            value
        })
        .collect();

    Ok(ok(json!({ "messages": messages, "next_cursor": next })))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateMessageBody {
    #[serde(default)]
    pub content: Value,
    #[serde(default)]
    pub model: Option<String>,
    /// 客户端回显用，不参与服务端语义。
    #[serde(default)]
    pub client_id: Option<String>,
}

/// 同键同请求返回首次结果；同键不同请求 409。
async fn load_idempotent(
    state: &AppState,
    actor: &CurrentUser,
    endpoint: &str,
    key: &str,
) -> ApiResult<Option<(String, Value)>> {
    let row: Option<(String, Value)> = sqlx::query_as(
        "SELECT request_hash, response FROM idempotency_keys \
         WHERE tenant_id = $1 AND user_id = $2 AND endpoint = $3 AND idem_key = $4 \
           AND expires_at > now()",
    )
    .bind(actor.tenant_id)
    .bind(actor.id)
    .bind(endpoint)
    .bind(key)
    .fetch_optional(&state.db())
    .await?;
    Ok(row)
}

fn conflict_on_key_mismatch(stored_hash: &str, hash: &str) -> Result<(), AppError> {
    if stored_hash == hash {
        Ok(())
    } else {
        Err(AppError::conflict(
            "Idempotency-Key reused with a different request",
        ))
    }
}

/// 提交一轮：
/// 1. 校验会话可写、内容非空；
/// 2. 幂等键命中则直接回放；
/// 3. 事务内写 user 消息 + turn + queued run；唯一索引冲突映射 `run.busy`；
/// 4. 返回 `run_id`，执行由 B5 的 agent 循环接手。
pub async fn create_message(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Json<CreateMessageBody>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, id).await?;
    if session.status != "active" {
        return Err(AppError::conflict("session is archived"));
    }
    validate_content(&body.content)?;

    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| session.model.clone());

    let endpoint = format!("POST /api/v1/sessions/{id}/messages");
    let idem_key =
        validate_idempotency_key(headers.get("idempotency-key").and_then(|v| v.to_str().ok()))?;
    let hash = request_hash(
        &serde_json::to_vec(&*body).map_err(|_| AppError::bad_request("invalid body"))?,
    );

    if let Some(key) = idem_key {
        if let Some((stored_hash, stored)) = load_idempotent(&state, &actor, &endpoint, key).await?
        {
            conflict_on_key_mismatch(&stored_hash, &hash)?;
            return Ok(ok(stored));
        }
    }

    // 先做一次友好判断；并发窗口由部分唯一索引兜底。
    let active_sql = format!(
        "SELECT status FROM runs WHERE session_id = $1 AND status IN ({})",
        ACTIVE_STATUS_SQL
    );
    let active_rows: Vec<(String,)> = sqlx::query_as(&active_sql)
        .bind(id)
        .fetch_all(&state.db())
        .await?;
    let active: Vec<RunStatus> = active_rows
        .iter()
        .filter_map(|(s,)| RunStatus::parse(s))
        .collect();
    ensure_no_active_run(&active)?;
    crate::tenant::quota::ensure_run_slot(&state, actor.plan.as_ref()).await?;
    crate::tenant::quota::ensure_token_budget(&state, actor.plan.as_ref()).await?;
    crate::tenant::quota::ensure_cost_budget(&state, actor.plan.as_ref()).await?;
    crate::tenant::quota::ensure_sandbox_slot(actor.plan.as_ref()).await?;
    crate::tenant::quota::ensure_storage_budget(&state, actor.plan.as_ref()).await?;
    let effective_model = model
        .clone()
        .unwrap_or_else(|| state.cfg.agent.default_model.clone());
    crate::tenant::quota::ensure_model_allowed(actor.plan.as_ref(), &effective_model)?;
    // 租户模型目录：非空时只允许目录内已启用的模型（隐藏模型不影响这里）。
    crate::models::ensure_model_allowed(&state, actor.tenant, &effective_model).await?;

    let mut tx = state.db().begin().await?;

    let (message_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO messages (session_id, role, kind, content) \
         VALUES ($1, 'user', 'chat', $2) RETURNING id",
    )
    .bind(id)
    .bind(body.content.clone())
    .fetch_one(&mut *tx)
    .await?;

    let (turn_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO turns (session_id, user_message_id, status, model) \
         VALUES ($1, $2, 'queued', $3) RETURNING id",
    )
    .bind(id)
    .bind(message_id)
    .bind(model.as_deref())
    .fetch_one(&mut *tx)
    .await?;

    let run_id: Uuid = match sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO runs (session_id, turn_id, status) \
         VALUES ($1, $2, 'queued') RETURNING id",
    )
    .bind(id)
    .bind(turn_id)
    .fetch_one(&mut *tx)
    .await
    {
        Ok((run_id,)) => run_id,
        Err(e) if is_unique_violation(&e, "ux_runs_one_active_per_session") => {
            return Err(run_busy_error());
        }
        Err(e) => return Err(e.into()),
    };

    sqlx::query("UPDATE messages SET turn_id = $2, run_id = $3 WHERE id = $1")
        .bind(message_id)
        .bind(turn_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sessions SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let data = json!({
        "session_id": id,
        "message_id": message_id,
        "turn_id": turn_id,
        "run_id": run_id,
        "status": "queued",
        "model": model,
    });

    if let Some(key) = idem_key {
        let inserted = sqlx::query(
            "INSERT INTO idempotency_keys \
               (tenant_id, user_id, endpoint, idem_key, request_hash, status_code, response) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(actor.tenant_id)
        .bind(actor.id)
        .bind(endpoint.as_str())
        .bind(key)
        .bind(hash.as_str())
        .bind(200i16)
        .bind(data.clone())
        .execute(&mut *tx)
        .await;

        match inserted {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e, "ux_idempotency_scope") => {
                // 并发的同键请求刚写入：放弃本次写入，回放首次结果。
                drop(tx);
                if let Some((stored_hash, stored)) =
                    load_idempotent(&state, &actor, &endpoint, key).await?
                {
                    conflict_on_key_mismatch(&stored_hash, &hash)?;
                    return Ok(ok(stored));
                }
                return Err(AppError::conflict("idempotency key race, retry"));
            }
            Err(e) => return Err(e.into()),
        }
    }

    tx.commit().await?;

    // B5：queued run 已落库，交给 agent 循环执行；HTTP 响应只表示"已接受"。
    // 事件（run.started / message.delta / ...）经 SSE 推送。
    crate::agent::start_run(state.clone(), state.db(), run_id, id);

    Ok(ok(data))
}
