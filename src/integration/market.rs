//! 市场：目录 + 来源白名单 + 安装/卸载到租户或用户。
//!
//! v1 不做真实下载：目录条目自带 manifest（声明式），安装时仍然完整过一遍
//! 插件/技能清单校验，再落成 `plugins` / `skills` 行——"已安装"只由这两张表
//! 表达，卸载就是按 `source = market:<item_id>` 删除。

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Extension, Path, Query, State},
    response::Response,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    integration::{audit, is_admin, ok},
    state::AppState,
};

use super::{
    plugins::{unique_or, PluginManifest},
    skills::SkillManifest,
};

/// 来源白名单。条目可以是精确值，或 `prefix:*` 前缀。
#[derive(Clone, Debug)]
pub struct SourcePolicy {
    patterns: Vec<String>,
}

impl SourcePolicy {
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }

    pub fn from_config(cfg: &crate::config::IntegrationConfig) -> Self {
        Self::new(cfg.market_sources.clone())
    }

    pub fn allows(&self, source: &str) -> bool {
        let source = source.trim().to_ascii_lowercase();
        if source.is_empty() {
            return false;
        }
        self.patterns.iter().any(|pattern| {
            let pattern = pattern.trim().to_ascii_lowercase();
            if pattern.is_empty() {
                return false;
            }
            match pattern.strip_suffix('*') {
                Some(prefix) => source.starts_with(prefix),
                None => source == pattern,
            }
        })
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct MarketItemRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub item_id: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub description: String,
    pub source: String,
    pub source_url: String,
    pub checksum: String,
    pub signature: String,
    pub manifest: Value,
    pub published_at: DateTime<Utc>,
}

pub const ITEM_COLUMNS: &str = "id, tenant_id, item_id, name, kind, version, description, source, \
     source_url, checksum, signature, manifest, published_at";

impl MarketItemRow {
    fn as_json(&self) -> Value {
        json!({
            "id": self.id,
            "item_id": self.item_id,
            "name": self.name,
            "kind": self.kind,
            "version": self.version,
            "description": self.description,
            "source": self.source,
            "source_url": self.source_url,
            "checksum": self.checksum,
            "signed": !self.signature.trim().is_empty(),
            "published_at": self.published_at,
        })
    }

    /// 安装标记：`market:<item_id>`，卸载/审计按它定位。
    fn install_source(&self) -> String {
        format!("market:{}", self.item_id)
    }
}

#[derive(Debug, Deserialize)]
pub struct ListItemsQuery {
    #[serde(default)]
    pub kind: Option<String>,
}

pub async fn list_items(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(query): Query<ListItemsQuery>,
) -> ApiResult<Response> {
    let rows = sqlx::query_as::<_, MarketItemRow>(&format!(
        "SELECT {ITEM_COLUMNS} FROM market_items \
         WHERE tenant_id = $1 AND ($2::text IS NULL OR kind = $2::text) \
         ORDER BY published_at DESC LIMIT 200"
    ))
    .bind(actor.tenant_id)
    .bind(query.kind.as_deref())
    .fetch_all(&state.db())
    .await?;

    // 目录可能包含收紧白名单之前发布的行：列表与安装都按当前策略过滤。
    let policy = SourcePolicy::from_config(&state.cfg.integration);
    let items: Vec<Value> = rows
        .iter()
        .filter(|row| policy.allows(&row.source))
        .map(MarketItemRow::as_json)
        .collect();
    Ok(ok(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
pub struct PublishItemBody {
    pub item_id: String,
    pub name: String,
    /// plugin | skill
    pub kind: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    pub manifest: Value,
}

/// 目录发布（管理动作）：只接受白名单来源 + 合法 manifest + 校验和。
pub async fn publish_item(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    body: Result<Json<PublishItemBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    if !is_admin(&actor) {
        return Err(AppError::forbidden(
            "only owner/admin can publish market items",
        ));
    }
    let policy = SourcePolicy::from_config(&state.cfg.integration);
    if !policy.allows(&body.source) {
        return Err(AppError::forbidden(format!(
            "source `{}` is not in the market allowlist",
            body.source
        )));
    }
    let item_id = body.item_id.trim();
    if item_id.is_empty() || item_id.chars().count() > 128 {
        return Err(AppError::bad_request("item_id must be 1..=128 chars"));
    }
    if !item_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':'))
    {
        return Err(AppError::bad_request("item_id has illegal characters"));
    }
    let checksum = body.checksum.unwrap_or_default().trim().to_string();
    if checksum.is_empty() {
        return Err(AppError::bad_request(
            "checksum is required (never install unverifiable content)",
        ));
    }
    if !checksum.chars().all(|c| c.is_ascii_hexdigit()) || checksum.len() != 64 {
        return Err(AppError::bad_request(
            "checksum must be a 64-char sha256 hex digest",
        ));
    }
    let signature = body.signature.unwrap_or_default().trim().to_string();
    if state.cfg.integration.market_require_signature && signature.is_empty() {
        return Err(AppError::forbidden(
            "this deployment requires signed market items",
        ));
    }

    // 清单按 kind 完整校验：目录里的坏条目不能等到安装时才暴露。
    match body.kind.trim() {
        "plugin" => {
            PluginManifest::parse(&body.manifest)?;
        }
        "skill" => {
            SkillManifest::parse(&body.manifest)?;
        }
        other => {
            return Err(AppError::bad_request(format!(
                "market item kind `{other}` is not supported"
            )))
        }
    }

    let row = sqlx::query_as::<_, MarketItemRow>(&format!(
        "INSERT INTO market_items \
           (tenant_id, item_id, name, kind, version, description, source, source_url, checksum, \
            signature, manifest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (tenant_id, item_id) DO UPDATE SET name = EXCLUDED.name, \
            kind = EXCLUDED.kind, version = EXCLUDED.version, \
            description = EXCLUDED.description, source = EXCLUDED.source, \
            source_url = EXCLUDED.source_url, checksum = EXCLUDED.checksum, \
            signature = EXCLUDED.signature, manifest = EXCLUDED.manifest, \
            published_at = now() \
         RETURNING {ITEM_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(item_id)
    .bind(body.name.trim())
    .bind(body.kind.trim())
    .bind(body.version.unwrap_or_else(|| "0.0.0".into()))
    .bind(body.description.unwrap_or_default())
    .bind(body.source.trim())
    .bind(body.source_url.unwrap_or_default())
    .bind(checksum)
    .bind(signature)
    .bind(&body.manifest)
    .fetch_one(&state.db())
    .await
    .map_err(|e| unique_or(e, "market item conflict"))?;

    audit(
        &state,
        &actor,
        "market.publish",
        &row.item_id,
        &format!("kind={} source={}", row.kind, row.source),
    )
    .await;
    Ok(ok(row.as_json()))
}

#[derive(Debug, Deserialize)]
pub struct InstallBody {
    /// user（默认，装到自己名下）| tenant（仅 owner/admin）。
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn install_item(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<InstallBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    if !state.cfg.integration.enabled {
        return Err(AppError::forbidden("integration features are disabled"));
    }
    let item = load_item(&state, &actor, &id).await?;
    let scope = body.scope.unwrap_or_else(|| "user".into());
    let owner: Option<Uuid> = match scope.as_str() {
        "user" => Some(actor.id),
        "tenant" => {
            if !is_admin(&actor) {
                return Err(AppError::forbidden(
                    "only owner/admin can install for the tenant",
                ));
            }
            None
        }
        other => {
            return Err(AppError::bad_request(format!(
                "scope must be user or tenant, got `{other}`"
            )))
        }
    };

    let source = item.install_source();
    let name = match item.kind.as_str() {
        "plugin" => {
            let manifest = PluginManifest::parse(&item.manifest)?;
            let manifest_value = serde_json::to_value(&manifest)
                .map_err(|e| AppError::internal(format!("manifest serialize failed: {e}")))?;
            let plugin_name = manifest.name.trim().to_string();
            // 插件的工具来自 MCP server：安装时就把它建出来，否则"装了但调不到"。
            let server_id = match manifest.mcp.clone() {
                Some(config) => {
                    let spec = config.into_spec(&plugin_name)?;
                    spec.validate(&state.cfg.integration)?;
                    spec.check_network(&state.cfg.integration).await?;
                    let row: (Uuid,) = sqlx::query_as(
                        "INSERT INTO mcp_servers \
                           (tenant_id, owner_user_id, name, transport, command, args, env, url, \
                            headers) \
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
                    .map_err(|e| unique_or(e, "an mcp server for this plugin already exists"))?;
                    Some(row.0)
                }
                None => None,
            };
            sqlx::query(
                "INSERT INTO plugins \
                   (tenant_id, owner_user_id, name, kind, version, source, mcp_server_id, manifest, \
                    enabled) \
                 VALUES ($1, $2, $3, 'mcp', $4, $5, $6, $7, true)",
            )
            .bind(actor.tenant_id)
            .bind(owner)
            .bind(&plugin_name)
            .bind(&item.version)
            .bind(&source)
            .bind(server_id)
            .bind(&manifest_value)
            .execute(&state.db())
            .await
            .map_err(|e| unique_or(e, "this plugin is already installed in scope"))?;
            plugin_name
        }
        "skill" => {
            let manifest = SkillManifest::parse(&item.manifest)?;
            let (prompt, resources, scripts) = manifest.assembly();
            let manifest_value = serde_json::to_value(&manifest)
                .map_err(|e| AppError::internal(format!("manifest serialize failed: {e}")))?;
            let skill_name = manifest.name.trim().to_string();
            sqlx::query(
                "INSERT INTO skills \
                   (tenant_id, owner_user_id, name, version, description, manifest, prompt, \
                    resources, scripts, source, enabled) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, true)",
            )
            .bind(actor.tenant_id)
            .bind(owner)
            .bind(&skill_name)
            .bind(&item.version)
            .bind(manifest.description.trim())
            .bind(&manifest_value)
            .bind(prompt)
            .bind(resources)
            .bind(scripts)
            .bind(&source)
            .execute(&state.db())
            .await
            .map_err(|e| unique_or(e, "this skill is already installed in scope"))?;
            skill_name
        }
        other => {
            return Err(AppError::bad_request(format!(
                "market item kind `{other}` cannot be installed"
            )))
        }
    };

    audit(
        &state,
        &actor,
        "market.install",
        &format!("{}:{}", item.kind, item.item_id),
        &format!("scope={scope} name={name}"),
    )
    .await;
    Ok(ok(json!({
        "item_id": item.item_id,
        "kind": item.kind,
        "name": name,
        "scope": scope,
    })))
}

#[derive(Debug, Deserialize)]
pub struct UninstallBody {
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn uninstall_item(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<UninstallBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let item = load_item(&state, &actor, &id).await?;
    let scope = body.scope.unwrap_or_else(|| "user".into());
    let owner: Option<Uuid> = match scope.as_str() {
        "user" => Some(actor.id),
        "tenant" => {
            if !is_admin(&actor) {
                return Err(AppError::forbidden(
                    "only owner/admin can uninstall tenant items",
                ));
            }
            None
        }
        other => {
            return Err(AppError::bad_request(format!(
                "scope must be user or tenant, got `{other}`"
            )))
        }
    };
    let source = item.install_source();
    let table = if item.kind == "plugin" {
        "plugins"
    } else {
        "skills"
    };
    let deleted = sqlx::query(&format!(
        "DELETE FROM {table} WHERE tenant_id = $1 AND source = $2 \
         AND ((owner_user_id IS NULL AND $3::uuid IS NULL) OR owner_user_id = $3::uuid)"
    ))
    .bind(actor.tenant_id)
    .bind(&source)
    .bind(owner)
    .execute(&state.db())
    .await?
    .rows_affected();

    audit(
        &state,
        &actor,
        "market.uninstall",
        &format!("{}:{}", item.kind, item.item_id),
        &format!("scope={scope} removed={deleted}"),
    )
    .await;
    Ok(ok(json!({ "item_id": item.item_id, "removed": deleted })))
}

/// 路径参数既接受 UUID 也接受 `item_id`（前端两种都有）。
async fn load_item(state: &AppState, actor: &CurrentUser, id: &str) -> ApiResult<MarketItemRow> {
    sqlx::query_as::<_, MarketItemRow>(&format!(
        "SELECT {ITEM_COLUMNS} FROM market_items \
         WHERE tenant_id = $1 AND (id::text = $2 OR item_id = $2)"
    ))
    .bind(actor.tenant_id)
    .bind(id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("market item not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_supports_exact_and_prefix_patterns() {
        let policy = SourcePolicy::new(vec!["official".into(), "partner:*".into()]);
        assert!(policy.allows("official"));
        assert!(policy.allows("OFFICIAL "));
        assert!(policy.allows("partner:acme"));
        assert!(!policy.allows("partner"));
        assert!(!policy.allows("random"));
        assert!(!policy.allows(""));
        // 空策略拒绝一切，不会因配置缺失而放开。
        assert!(!SourcePolicy::new(Vec::new()).allows("official"));
    }

    #[test]
    fn rows_mark_install_source_and_signature() {
        let row = MarketItemRow {
            id: Uuid::nil(),
            tenant_id: 1,
            item_id: "official/fs-tools".into(),
            name: "fs-tools".into(),
            kind: "plugin".into(),
            version: "1.0.0".into(),
            description: String::new(),
            source: "official".into(),
            source_url: "https://market.test/fs-tools".into(),
            checksum: "a".repeat(64),
            signature: String::new(),
            manifest: json!({}),
            published_at: Utc::now(),
        };
        assert_eq!(row.install_source(), "market:official/fs-tools");
        let json = row.as_json();
        assert_eq!(json["signed"], false);
    }
}
