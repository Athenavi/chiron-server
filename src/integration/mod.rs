//! B9：MCP / 插件 / 技能 / 市场（PRD §7.8）。
//!
//! 边界（重要）：MCP 是唯一的外部能力入口。插件只是"MCP server + 元数据"，
//! 技能是声明式提示词/资源（脚本由 B5 的沙箱工具循环执行），市场只做
//! "目录 + 来源白名单 + 安装/卸载"。因此本模块不新增任何执行路径。
//!
//! 对外只暴露：
//! * [`router`]：`/api/v1/mcp/*`、`/plugins`、`/skills`、`/market/*`；
//! * [`tools_for_session`]：B5/B6 装配工具时把 MCP 工具接进 `ToolDef` 列表；
//! * [`skills_prompt`]：技能提示词块（拼进系统提示）。

// 与 provider/agent 同一口径：这些是 B5/B6/B10 的公共面，二进制 crate 里
// 暂时没有全部调用点，模块级关掉 dead_code，避免门禁误报。
#![allow(dead_code)]

pub mod market;
pub mod mcp;
pub mod plugins;
pub mod skills;

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::{
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{auth::CurrentUser, error::AppError, provider::ToolDef, state::AppState};

use mcp::McpServerRow;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/mcp/servers",
            get(mcp::api::list_servers).post(mcp::api::create_server),
        )
        .route(
            "/api/v1/mcp/servers/{id}",
            patch(mcp::api::update_server).delete(mcp::api::delete_server),
        )
        .route(
            "/api/v1/mcp/servers/{id}/probe",
            post(mcp::api::probe_server),
        )
        .route("/api/v1/mcp/tools", get(mcp::api::list_tools))
        .route(
            "/api/v1/plugins",
            get(plugins::list_plugins).post(plugins::create_plugin),
        )
        .route(
            "/api/v1/plugins/{id}",
            patch(plugins::patch_plugin).delete(plugins::delete_plugin),
        )
        .route(
            "/api/v1/skills",
            get(skills::list_skills).post(skills::create_skill),
        )
        .route(
            "/api/v1/skills/{id}",
            patch(skills::patch_skill).delete(skills::delete_skill),
        )
        .route(
            "/api/v1/market/items",
            get(market::list_items).post(market::publish_item),
        )
        .route(
            "/api/v1/market/items/{id}/install",
            post(market::install_item),
        )
        .route(
            "/api/v1/market/items/{id}/uninstall",
            post(market::uninstall_item),
        )
}

pub(crate) fn ok(data: Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

pub(crate) fn is_admin(actor: &CurrentUser) -> bool {
    matches!(actor.role.as_str(), "owner" | "admin")
}

/// 可见不等于可写：共享对象（owner_user_id IS NULL）只有 owner/admin 能改删。
pub(crate) fn ensure_can_modify(
    actor: &CurrentUser,
    owner_user_id: Option<Uuid>,
) -> Result<(), AppError> {
    if is_admin(actor) || owner_user_id == Some(actor.id) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "only the creator or a tenant admin can modify this object",
        ))
    }
}

/// 审计一行。写失败不能影响主流程，因此只记日志。
pub(crate) async fn audit(
    state: &AppState,
    actor: &CurrentUser,
    action: &str,
    target: &str,
    detail: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO audit_logs (tenant_id, user_id, action, target, detail, ip)
         VALUES ($1, $2, $3, $4, $5, '')",
    )
    .bind(actor.tenant_id)
    .bind(actor.id)
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(&state.db())
    .await;
}

/// 会话可见的 server 行（admin 看整个租户，普通用户看自己的 + 租户共享）。
pub(crate) async fn visible_servers(
    state: &AppState,
    tenant_id: i16,
    user_id: Uuid,
    is_admin: bool,
) -> Result<Vec<McpServerRow>, sqlx::Error> {
    sqlx::query_as::<_, McpServerRow>(&format!(
        "SELECT {} FROM mcp_servers \
         WHERE tenant_id = $1 AND enabled \
           AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC",
        mcp::SERVER_COLUMNS
    ))
    .bind(tenant_id)
    .bind(is_admin)
    .bind(user_id)
    .fetch_all(&state.db())
    .await
}

/// 给一次会话装配可调用工具（B5/B6 用）。
///
/// 只返回"能真的调到"的工具：server 必须 enabled 且已探测出工具，
/// 同时满足 server 的 tool_allowlist 与（若存在）插件的 tools 白名单。
/// 重名一律去重（先到先得），保证模型侧的工具名唯一。
pub async fn tools_for_session(
    state: &AppState,
    tenant_id: i16,
    user_id: Uuid,
    is_admin: bool,
) -> Vec<ToolDef> {
    if !state.cfg.integration.enabled {
        return Vec::new();
    }
    let servers = match visible_servers(state, tenant_id, user_id, is_admin).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "integration: load mcp servers failed");
            return Vec::new();
        }
    };
    if servers.is_empty() {
        return Vec::new();
    }

    // 插件的工具白名单会进一步收窄 server 暴露的工具。
    let plugin_filters = sqlx::query_as::<_, (Option<Uuid>, Value)>(
        "SELECT mcp_server_id, manifest FROM plugins \
         WHERE tenant_id = $1 AND enabled AND mcp_server_id IS NOT NULL \
           AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3)",
    )
    .bind(tenant_id)
    .bind(is_admin)
    .bind(user_id)
    .fetch_all(&state.db())
    .await
    .unwrap_or_default();

    let mut out: Vec<ToolDef> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in &servers {
        let plugin_allow: Vec<String> = plugin_filters
            .iter()
            .filter(|(server_id, _)| *server_id == Some(row.id))
            .flat_map(|(_, manifest)| {
                manifest
                    .get("tools")
                    .map(mcp::string_list)
                    .unwrap_or_default()
            })
            .collect();
        for tool in row.allowed_tools() {
            if !plugin_allow.is_empty() && !plugin_allow.iter().any(|t| t == &tool.name) {
                continue;
            }
            let def = tool.to_tool_def(&row.name);
            if seen.insert(def.name.clone()) {
                out.push(def);
            }
        }
    }

    // 技能脚本工具：同样按"声明即工具"装配，执行仍走沙箱。
    for def in skill_tools(state, tenant_id, user_id, is_admin).await {
        if seen.insert(def.name.clone()) {
            out.push(def);
        }
    }
    out
}

/// 技能脚本工具（单独暴露，便于只装配技能的场景）。
pub async fn skill_tools(
    state: &AppState,
    tenant_id: i16,
    user_id: Uuid,
    is_admin: bool,
) -> Vec<ToolDef> {
    if !state.cfg.integration.enabled {
        return Vec::new();
    }
    let rows = sqlx::query_as::<_, (String, Value)>(
        "SELECT name, manifest FROM skills \
         WHERE tenant_id = $1 AND enabled \
           AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC",
    )
    .bind(tenant_id)
    .bind(is_admin)
    .bind(user_id)
    .fetch_all(&state.db())
    .await
    .unwrap_or_default();

    let mut out = Vec::new();
    for (name, manifest) in rows {
        if let Ok(manifest) = skills::SkillManifest::parse(&manifest) {
            out.extend(skills::script_tools(&name, &manifest));
        }
    }
    out
}

/// 技能提示词块：拼在系统提示尾部（B8/B9 的装配点）。
pub async fn skills_prompt(
    state: &AppState,
    tenant_id: i16,
    user_id: Uuid,
    is_admin: bool,
) -> String {
    if !state.cfg.integration.enabled {
        return String::new();
    }
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT name, prompt FROM skills \
         WHERE tenant_id = $1 AND enabled AND length(btrim(prompt)) > 0 \
           AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC",
    )
    .bind(tenant_id)
    .bind(is_admin)
    .bind(user_id)
    .fetch_all(&state.db())
    .await
    .unwrap_or_default();

    let mut blocks = Vec::new();
    for (name, prompt) in rows {
        let block = if prompt.trim_start().starts_with("## Skill:") {
            prompt
        } else {
            format!("## Skill: {name}\n{}", prompt.trim())
        };
        blocks.push(block);
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_and_plain_user_roles_are_recognized() {
        let admin = CurrentUser {
            id: Uuid::nil(),
            email: "a@b.c".into(),
            role: "admin".into(),
            tenant_id: 1,
            tenant: None,
            plan: None,
            must_change: false,
            jti: String::new(),
            iat: 0,
            exp: 0,
        };
        assert!(is_admin(&admin));
        let user = CurrentUser {
            role: "user".into(),
            ..admin
        };
        assert!(!is_admin(&user));
    }
}
