//! `/api/v1/mcp/*`：server 管理、探测、已发现工具。

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{rejection::JsonRejection, Extension, Path, State},
    response::Response,
    Json,
};
use serde_json::{json, Value};

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    integration::{audit, is_admin, ok},
    state::AppState,
};

use super::{
    client::{McpClient, McpLimits},
    jsonrpc::McpError,
    McpServerRow, McpServerSpec, TransportKind, SERVER_COLUMNS,
};

impl From<McpError> for AppError {
    fn from(err: McpError) -> Self {
        use axum::http::StatusCode;
        match &err {
            McpError::Timeout { .. } => {
                AppError::new(StatusCode::GATEWAY_TIMEOUT, "mcp.timeout", err.to_string())
            }
            McpError::Cancelled => {
                AppError::new(StatusCode::CONFLICT, "mcp.cancelled", err.to_string())
            }
            McpError::Blocked(_) => {
                AppError::new(StatusCode::FORBIDDEN, "mcp.blocked", err.to_string())
            }
            McpError::Schema(_) | McpError::UnknownTool(_) => {
                AppError::new(StatusCode::BAD_REQUEST, err.code(), err.to_string())
            }
            _ => AppError::new(StatusCode::BAD_GATEWAY, "mcp.failed", err.to_string()),
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ServerBody {
    /// 创建时必填；PATCH 时缺省表示不改名。
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub tool_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// true = 租户共享（仅 owner/admin 可建）。
    #[serde(default)]
    pub shared: Option<bool>,
}

impl ServerBody {
    /// PATCH 语义：只覆盖请求里出现的字段，其余沿用 `base`。
    fn apply(self, mut base: McpServerSpec) -> ApiResult<McpServerSpec> {
        if let Some(name) = self.name {
            base.name = name.trim().to_string();
        }
        if let Some(transport) = self.transport {
            base.transport = TransportKind::parse(&transport)
                .ok_or_else(|| AppError::bad_request("transport must be stdio, sse or http"))?;
        }
        if let Some(command) = self.command {
            base.command = command.trim().to_string();
        }
        if let Some(args) = self.args {
            base.args = args;
        }
        if let Some(env) = self.env {
            base.env = env;
        }
        if let Some(url) = self.url {
            base.url = url.trim().to_string();
        }
        if let Some(headers) = self.headers {
            base.headers = headers;
        }
        if let Some(allowlist) = self.tool_allowlist {
            base.tool_allowlist = allowlist;
        }
        Ok(base)
    }
}

/// 可见范围：admin 看整个租户；普通用户看自己的 + 租户共享。
fn visible(actor: &CurrentUser) -> (bool, uuid::Uuid) {
    (is_admin(actor), actor.id)
}

pub async fn list_servers(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let (admin, user) = visible(&actor);
    let rows = sqlx::query_as::<_, McpServerRow>(&format!(
        "SELECT {SERVER_COLUMNS} FROM mcp_servers \
         WHERE tenant_id = $1 AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY created_at ASC"
    ))
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(user)
    .fetch_all(&state.db())
    .await?;
    Ok(ok(json!({ "servers": rows })))
}

pub async fn create_server(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    body: Result<Json<ServerBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let shared = body.shared.unwrap_or(false);
    if shared && !is_admin(&actor) {
        return Err(AppError::forbidden(
            "only owner/admin can create shared servers",
        ));
    }
    let enabled = body.enabled.unwrap_or(true);
    let spec = body.apply(McpServerSpec::stdio("", ""))?;
    if spec.name.trim().is_empty() {
        return Err(AppError::bad_request("name is required"));
    }
    spec.validate(&state.cfg.integration)?;
    // 建的时候先做一次 SSRF 校验，避免把一个打不通/被禁的地址存进库。
    spec.check_network(&state.cfg.integration).await?;

    let row = sqlx::query_as::<_, McpServerRow>(&format!(
        "INSERT INTO mcp_servers \
           (tenant_id, owner_user_id, name, transport, command, args, env, url, headers, \
            tool_allowlist, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING {SERVER_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(if shared { None } else { Some(actor.id) })
    .bind(&spec.name)
    .bind(spec.transport.as_str())
    .bind(&spec.command)
    .bind(json!(spec.args))
    .bind(json!(spec.env))
    .bind(&spec.url)
    .bind(json!(spec.headers))
    .bind(json!(spec.tool_allowlist))
    .bind(enabled)
    .fetch_one(&state.db())
    .await
    .map_err(unique_conflict)?;

    audit(
        &state,
        &actor,
        "mcp.server.create",
        &spec.name,
        &format!("transport={} shared={shared}", spec.transport.as_str()),
    )
    .await;
    Ok(ok(json!(row)))
}

pub async fn update_server(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<uuid::Uuid>,
    body: Result<Json<ServerBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let existing = load_server(&state, &actor, id).await?;
    super::super::ensure_can_modify(&actor, existing.owner_user_id)?;
    let enabled_override = body.enabled;
    let spec = body.apply(existing.spec()?)?;
    spec.validate(&state.cfg.integration)?;
    spec.check_network(&state.cfg.integration).await?;

    let row = sqlx::query_as::<_, McpServerRow>(&format!(
        "UPDATE mcp_servers SET name = $2, transport = $3, command = $4, args = $5, env = $6, \
            url = $7, headers = $8, tool_allowlist = $9, enabled = $10, updated_at = now() \
         WHERE id = $1 AND tenant_id = $11 \
         RETURNING {SERVER_COLUMNS}"
    ))
    .bind(id)
    .bind(&spec.name)
    .bind(spec.transport.as_str())
    .bind(&spec.command)
    .bind(json!(spec.args))
    .bind(json!(spec.env))
    .bind(&spec.url)
    .bind(json!(spec.headers))
    .bind(json!(spec.tool_allowlist))
    .bind(enabled_override.unwrap_or(existing.enabled))
    .bind(actor.tenant_id)
    .fetch_one(&state.db())
    .await
    .map_err(unique_conflict)?;

    audit(
        &state,
        &actor,
        "mcp.server.update",
        &spec.name,
        "config replaced",
    )
    .await;
    Ok(ok(json!(row)))
}

pub async fn delete_server(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Response> {
    let existing = load_server(&state, &actor, id).await?;
    super::super::ensure_can_modify(&actor, existing.owner_user_id)?;
    sqlx::query("DELETE FROM mcp_servers WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(actor.tenant_id)
        .execute(&state.db())
        .await?;
    audit(&state, &actor, "mcp.server.delete", &existing.name, "").await;
    Ok(ok(json!({ "id": id })))
}

/// 探测：握手 + `tools/list`，成功才覆盖 tools/server_info，失败只写 last_error。
pub async fn probe_server(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Response> {
    let existing = load_server(&state, &actor, id).await?;
    super::super::ensure_can_modify(&actor, existing.owner_user_id)?;
    let spec = existing.spec()?;
    spec.validate(&state.cfg.integration)?;
    spec.check_network(&state.cfg.integration).await?;

    let limits = McpLimits::from_config(&state.cfg.integration);
    let client = McpClient::connect(&spec, limits).await?;
    match client.probe().await {
        Ok(probe) => {
            let tools: Vec<Value> = probe
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "inputSchema": tool.input_schema,
                    })
                })
                .collect();
            sqlx::query(
                "UPDATE mcp_servers SET protocol_version = $2, server_info = $3, tools = $4, \
                    last_probe_at = now(), last_error = '', updated_at = now() \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&probe.protocol_version)
            .bind(&probe.server_info)
            .bind(json!(tools))
            .execute(&state.db())
            .await?;
            audit(
                &state,
                &actor,
                "mcp.server.probe",
                &existing.name,
                &format!("ok tools={}", probe.tools.len()),
            )
            .await;
            Ok(ok(json!({
                "protocol_version": probe.protocol_version,
                "server_info": probe.server_info,
                "tools": tools,
            })))
        }
        Err(err) => {
            sqlx::query(
                "UPDATE mcp_servers SET last_error = $2, last_probe_at = now(), updated_at = now() \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(err.to_string())
            .execute(&state.db())
            .await?;
            audit(
                &state,
                &actor,
                "mcp.server.probe",
                &existing.name,
                &format!("failed: {}", err.code()),
            )
            .await;
            Err(err.into())
        }
    }
}

/// 已发现工具（跨 server 汇总，受 enabled 与白名单过滤）。
pub async fn list_tools(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let (admin, user) = visible(&actor);
    let rows = sqlx::query_as::<_, McpServerRow>(&format!(
        "SELECT {SERVER_COLUMNS} FROM mcp_servers \
         WHERE tenant_id = $1 AND enabled AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC"
    ))
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(user)
    .fetch_all(&state.db())
    .await?;

    let mut tools = Vec::new();
    for row in &rows {
        for tool in row.allowed_tools() {
            tools.push(json!({
                "server": row.name,
                "name": tool.name,
                "qualified_name": tool.qualified_name(&row.name),
                "description": tool.description,
                "input_schema": tool.input_schema,
            }));
        }
    }
    Ok(ok(json!({ "tools": tools })))
}

async fn load_server(
    state: &AppState,
    actor: &CurrentUser,
    id: uuid::Uuid,
) -> ApiResult<McpServerRow> {
    let (admin, user) = visible(actor);
    sqlx::query_as::<_, McpServerRow>(&format!(
        "SELECT {SERVER_COLUMNS} FROM mcp_servers \
         WHERE id = $1 AND tenant_id = $2 AND ($3::bool OR owner_user_id IS NULL OR owner_user_id = $4)"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(user)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("mcp server not found"))
}

fn unique_conflict(err: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(db) = &err {
        if db.code().as_deref() == Some("23505") {
            return AppError::conflict("an mcp server with this name already exists in scope");
        }
    }
    err.into()
}
