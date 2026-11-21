//! 技能：声明式 manifest + 提示词/资源，脚本只"装配"不"执行"。
//!
//! 技能不引入新运行时：脚本是用户资产，落在工作区里，执行必须经过
//! [`crate::sandbox::Sandbox`]（B5 的工具循环）。这里只提供
//! * [`SkillManifest`] 的解析与校验；
//! * [`script_tools`]：把脚本声明成 `ToolDef`（名字带技能前缀，避免撞名）；
//! * [`assemble_command`]：把一次调用折成沙箱命令串（逐个 shell_quote）。

use std::sync::Arc;

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
    provider::ToolDef,
    sandbox::{shell_quote, tenant_rel_path},
    state::AppState,
};

use super::plugins::unique_or;

pub const MAX_RESOURCES: usize = 64;
pub const MAX_SCRIPTS: usize = 32;
pub const MAX_PROMPT_CHARS: usize = 16_000;
pub const MAX_ARGS: usize = 8;

/// 技能不接受"可执行"类字段：脚本路径是声明，执行入口在沙箱。
const FORBIDDEN_MANIFEST_KEYS: &[&str] = &[
    "hooks",
    "entrypoint",
    "run",
    "command",
    "bin",
    "postinstall",
    "preinstall",
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: String,
    /// 注入系统提示的说明（装配时由 [`prompt_block`] 包一层标题）。
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub resources: Vec<SkillResource>,
    #[serde(default)]
    pub scripts: Vec<SkillScript>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillResource {
    /// 相对工作区/技能目录的路径。
    pub path: String,
    #[serde(default)]
    pub description: String,
    /// 小段内联内容（大文件放工作区，按 path 读）。
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillScript {
    pub name: String,
    pub path: String,
    /// python | sh | node
    pub language: String,
    #[serde(default)]
    pub description: String,
    /// 位置参数名（按声明顺序从调用参数取值）。
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl SkillScript {
    pub fn interpreter(&self) -> Option<&'static str> {
        match self.language.trim().to_ascii_lowercase().as_str() {
            "python" | "python3" | "py" => Some("python3"),
            "sh" | "bash" | "shell" => Some("sh"),
            "node" | "js" | "javascript" => Some("node"),
            _ => None,
        }
    }
}

impl SkillManifest {
    pub fn parse(raw: &Value) -> ApiResult<Self> {
        let object = raw
            .as_object()
            .ok_or_else(|| AppError::bad_request("skill manifest must be an object"))?;
        for key in object.keys() {
            if FORBIDDEN_MANIFEST_KEYS
                .iter()
                .any(|blocked| key.eq_ignore_ascii_case(blocked))
            {
                return Err(AppError::bad_request(format!(
                    "skill manifest key `{key}` is not supported (scripts only run in the sandbox)"
                )));
            }
        }
        let manifest: Self = serde_json::from_value(raw.clone())
            .map_err(|e| AppError::bad_request(format!("invalid skill manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> ApiResult<()> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(AppError::bad_request("skill name must be 1..=64 chars"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        {
            return Err(AppError::bad_request(
                "skill name may only contain letters, digits, space, dot, dash, underscore",
            ));
        }
        if self.prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(AppError::bad_request("skill prompt is too long"));
        }
        if self.resources.len() > MAX_RESOURCES {
            return Err(AppError::bad_request("too many skill resources"));
        }
        if self.scripts.len() > MAX_SCRIPTS {
            return Err(AppError::bad_request("too many skill scripts"));
        }
        for resource in &self.resources {
            safe_rel_path(&resource.path)?;
            if let Some(content) = &resource.content {
                if content.len() > 256 * 1024 {
                    return Err(AppError::bad_request("inline resource is too large"));
                }
            }
        }
        for script in &self.scripts {
            if !is_ident(&script.name) || script.name.chars().count() > 48 {
                return Err(AppError::bad_request(format!(
                    "script name `{}` must be an identifier",
                    script.name
                )));
            }
            safe_rel_path(&script.path)?;
            if script.interpreter().is_none() {
                return Err(AppError::bad_request(format!(
                    "script `{}` has unsupported language `{}`",
                    script.name, script.language
                )));
            }
            if script.args.len() > MAX_ARGS {
                return Err(AppError::bad_request("too many script arguments"));
            }
            for arg in &script.args {
                if !is_ident(arg) {
                    return Err(AppError::bad_request(format!(
                        "script argument `{arg}` must be an identifier"
                    )));
                }
            }
            if script.args.iter().any(|a| a == &script.name) {
                return Err(AppError::bad_request(
                    "script name collides with its arguments",
                ));
            }
        }
        // 脚本重名会让工具名撞车。
        for (idx, script) in self.scripts.iter().enumerate() {
            if self.scripts[..idx].iter().any(|s| s.name == script.name) {
                return Err(AppError::bad_request(format!(
                    "duplicate script name `{}`",
                    script.name
                )));
            }
        }
        Ok(())
    }

    /// 写进 `skills` 行的装配结果：提示词块、资源清单、脚本清单。
    pub fn assembly(&self) -> (String, Value, Value) {
        (
            prompt_block(self),
            json!(self.resources),
            json!(self.scripts),
        )
    }
}

fn is_ident(raw: &str) -> bool {
    !raw.is_empty()
        && raw.chars().count() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// 相对路径钳制：不许绝对路径、盘符、`..` 回溯。
fn safe_rel_path(raw: &str) -> ApiResult<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 256 {
        return Err(AppError::bad_request("path must be 1..=256 chars"));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') || trimmed.contains(':') {
        return Err(AppError::forbidden(
            "paths must be relative to the workspace",
        ));
    }
    if trimmed
        .split(['/', '\\'])
        .any(|segment| segment == ".." || segment == "~")
    {
        return Err(AppError::forbidden(
            "paths must not traverse outside the workspace",
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(AppError::bad_request("path has control chars"));
    }
    tenant_rel_path(trimmed).map_err(|e| AppError::bad_request(e.message))?;
    Ok(())
}

/// 系统提示里的技能块：标题 + 描述 + 声明式正文 + 资源/脚本索引。
pub fn prompt_block(manifest: &SkillManifest) -> String {
    let mut out = format!("## Skill: {}\n", manifest.name.trim());
    if !manifest.description.trim().is_empty() {
        out.push_str(manifest.description.trim());
        out.push('\n');
    }
    if !manifest.prompt.trim().is_empty() {
        out.push_str(manifest.prompt.trim());
        out.push('\n');
    }
    if !manifest.resources.is_empty() {
        out.push_str("Resources:\n");
        for resource in &manifest.resources {
            out.push_str(&format!(
                "- {}",
                safe_layout_label(&resource.path, &resource.description)
            ));
            out.push('\n');
        }
    }
    if !manifest.scripts.is_empty() {
        out.push_str("Scripts (run through the sandbox):\n");
        for script in &manifest.scripts {
            out.push_str(&format!(
                "- {} ({}): {}\n",
                script.name,
                script.path,
                script.description.trim()
            ));
        }
    }
    out
}

fn safe_layout_label(path: &str, description: &str) -> String {
    let description = description.trim();
    if description.is_empty() {
        path.to_string()
    } else {
        format!("{path} — {description}")
    }
}

/// 技能脚本对模型暴露的工具名：`<skill>_<script>`，字符集与 function name 一致。
pub fn script_tool_name(skill: &str, script: &str) -> String {
    let mut out = String::new();
    for ch in skill
        .chars()
        .chain(std::iter::once('_'))
        .chain(script.chars())
    {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' && out.ends_with('_') {
            continue;
        }
        out.push(mapped);
    }
    out.trim_matches('_').chars().take(64).collect()
}

/// 脚本 → 工具声明（参数全部是 string，按声明顺序位置传参）。
pub fn script_tools(skill: &str, manifest: &SkillManifest) -> Vec<ToolDef> {
    manifest
        .scripts
        .iter()
        .map(|script| {
            let mut properties = serde_json::Map::new();
            for arg in &script.args {
                properties.insert(arg.clone(), json!({"type": "string"}));
            }
            let description = if script.description.trim().is_empty() {
                format!(
                    "Run `{}` from skill `{}` in the sandbox.",
                    script.path, skill
                )
            } else {
                format!("{} (skill `{skill}`)", script.description.trim())
            };
            ToolDef::new(
                script_tool_name(skill, &script.name),
                description,
                json!({
                    "type": "object",
                    "properties": properties,
                    "required": script.args.clone(),
                    "additionalProperties": false,
                }),
            )
        })
        .collect()
}

/// 装配沙箱命令：解释器 + 钳制后的脚本路径 + 逐个 shell_quote 的位置参数。
/// 只返回命令串；执行必须由调用方交给 [`crate::sandbox::Sandbox`]。
pub fn assemble_command(script: &SkillScript, args: &Value) -> ApiResult<String> {
    let interpreter = script
        .interpreter()
        .ok_or_else(|| AppError::bad_request("unsupported script language"))?;
    safe_rel_path(&script.path)?;
    let mut command = format!("{} {}", interpreter, shell_quote(script.path.trim()));
    for arg in &script.args {
        let value = args
            .get(arg)
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::bad_request(format!("missing argument `{arg}`")))?;
        if value.chars().count() > 4000 || value.chars().any(|c| c.is_control()) {
            return Err(AppError::bad_request(format!(
                "argument `{arg}` is not acceptable"
            )));
        }
        command.push(' ');
        command.push_str(&shell_quote(value));
    }
    Ok(command)
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct SkillRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub owner_user_id: Option<Uuid>,
    pub name: String,
    pub version: String,
    pub description: String,
    pub manifest: Value,
    pub prompt: String,
    pub resources: Value,
    pub scripts: Value,
    pub source: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const SKILL_COLUMNS: &str = "id, tenant_id, owner_user_id, name, version, description, \
     manifest, prompt, resources, scripts, source, enabled, created_at, updated_at";

#[derive(Debug, Deserialize)]
pub struct CreateSkillBody {
    pub manifest: Value,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchSkillBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub manifest: Option<Value>,
}

pub async fn list_skills(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> ApiResult<Response> {
    let admin = is_admin(&actor);
    let rows = sqlx::query_as::<_, SkillRow>(&format!(
        "SELECT {SKILL_COLUMNS} FROM skills \
         WHERE tenant_id = $1 AND ($2::bool OR owner_user_id IS NULL OR owner_user_id = $3) \
         ORDER BY name ASC"
    ))
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(actor.id)
    .fetch_all(&state.db())
    .await?;
    Ok(ok(json!({ "skills": rows })))
}

pub async fn create_skill(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    body: Result<Json<CreateSkillBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    if !state.cfg.integration.enabled {
        return Err(AppError::forbidden("integration features are disabled"));
    }
    let manifest = SkillManifest::parse(&body.manifest)?;
    let shared = body.shared.unwrap_or(false);
    if shared && !is_admin(&actor) {
        return Err(AppError::forbidden(
            "only owner/admin can create shared skills",
        ));
    }
    let (prompt, resources, scripts) = manifest.assembly();
    let manifest_value = serde_json::to_value(&manifest)
        .map_err(|e| AppError::internal(format!("manifest serialize failed: {e}")))?;

    let row = sqlx::query_as::<_, SkillRow>(&format!(
        "INSERT INTO skills \
           (tenant_id, owner_user_id, name, version, description, manifest, prompt, resources, \
            scripts, source, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'local', $10) \
         RETURNING {SKILL_COLUMNS}"
    ))
    .bind(actor.tenant_id)
    .bind(if shared { None } else { Some(actor.id) })
    .bind(manifest.name.trim())
    .bind(manifest.version.clone().unwrap_or_else(|| "0.0.0".into()))
    .bind(manifest.description.trim())
    .bind(&manifest_value)
    .bind(prompt)
    .bind(resources)
    .bind(scripts)
    .bind(body.enabled.unwrap_or(true))
    .fetch_one(&state.db())
    .await
    .map_err(|e| unique_or(e, "a skill with this name already exists in scope"))?;

    audit(&state, &actor, "skill.create", &row.name, "").await;
    Ok(ok(json!(row)))
}

pub async fn patch_skill(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    body: Result<Json<PatchSkillBody>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body.map_err(AppError::invalid_body)?;
    let existing = load_skill(&state, &actor, id).await?;
    super::ensure_can_modify(&actor, existing.owner_user_id)?;
    let (manifest_value, prompt, resources, scripts) = match body.manifest {
        Some(raw) => {
            let manifest = SkillManifest::parse(&raw)?;
            let (prompt, resources, scripts) = manifest.assembly();
            let value = serde_json::to_value(&manifest)
                .map_err(|e| AppError::internal(format!("manifest serialize failed: {e}")))?;
            (value, prompt, resources, scripts)
        }
        None => (
            existing.manifest.clone(),
            existing.prompt.clone(),
            existing.resources.clone(),
            existing.scripts.clone(),
        ),
    };

    let row = sqlx::query_as::<_, SkillRow>(&format!(
        "UPDATE skills SET enabled = $2, manifest = $3, prompt = $4, resources = $5, \
            scripts = $6, updated_at = now() \
         WHERE id = $1 AND tenant_id = $7 RETURNING {SKILL_COLUMNS}"
    ))
    .bind(id)
    .bind(body.enabled.unwrap_or(existing.enabled))
    .bind(&manifest_value)
    .bind(prompt)
    .bind(resources)
    .bind(scripts)
    .bind(actor.tenant_id)
    .fetch_one(&state.db())
    .await?;

    audit(
        &state,
        &actor,
        "skill.update",
        &row.name,
        if row.enabled { "enabled" } else { "disabled" },
    )
    .await;
    Ok(ok(json!(row)))
}

pub async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let existing = load_skill(&state, &actor, id).await?;
    super::ensure_can_modify(&actor, existing.owner_user_id)?;
    sqlx::query("DELETE FROM skills WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(actor.tenant_id)
        .execute(&state.db())
        .await?;
    audit(&state, &actor, "skill.delete", &existing.name, "").await;
    Ok(ok(json!({ "id": id })))
}

async fn load_skill(state: &AppState, actor: &CurrentUser, id: Uuid) -> ApiResult<SkillRow> {
    let admin = is_admin(actor);
    sqlx::query_as::<_, SkillRow>(&format!(
        "SELECT {SKILL_COLUMNS} FROM skills \
         WHERE id = $1 AND tenant_id = $2 AND ($3::bool OR owner_user_id IS NULL OR owner_user_id = $4)"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(actor.id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("skill not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Value {
        json!({
            "name": "pdf-tools",
            "version": "1.0.0",
            "description": "extract text from pdfs",
            "prompt": "Use extract_pdf for pdf tasks.",
            "resources": [{"path": "resources/guide.md", "description": "how to"}],
            "scripts": [{
                "name": "extract",
                "path": "scripts/extract.py",
                "language": "python",
                "description": "extract text",
                "args": ["file", "pages"]
            }]
        })
    }

    #[test]
    fn parses_and_assembles_prompt() {
        let m = SkillManifest::parse(&manifest()).unwrap();
        let (prompt, resources, scripts) = m.assembly();
        assert!(prompt.contains("## Skill: pdf-tools"));
        assert!(prompt.contains("resources/guide.md"));
        assert!(prompt.contains("extract (scripts/extract.py)"));
        assert_eq!(resources.as_array().unwrap().len(), 1);
        assert_eq!(scripts.as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_traversal_absolute_paths_and_exec_keys() {
        let mut bad = manifest();
        bad["scripts"][0]["path"] = json!("../../etc/passwd");
        assert!(SkillManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["resources"][0]["path"] = json!("/etc/passwd");
        assert!(SkillManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["resources"][0]["path"] = json!("C:\\secrets.txt");
        assert!(SkillManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["run"] = json!("bash -c 'curl evil'");
        let err = SkillManifest::parse(&bad).unwrap_err();
        assert!(err.message.contains("not supported"));
    }

    #[test]
    fn rejects_bad_scripts() {
        let mut bad = manifest();
        bad["scripts"][0]["language"] = json!("ruby");
        assert!(SkillManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["scripts"][0]["name"] = json!("bad name");
        assert!(SkillManifest::parse(&bad).is_err());

        let mut bad = manifest();
        bad["scripts"][0]["args"] = json!(["$PATH"]);
        assert!(SkillManifest::parse(&bad).is_err());

        let mut dup = manifest();
        dup["scripts"] = json!([
            {"name": "s", "path": "a.py", "language": "python"},
            {"name": "s", "path": "b.py", "language": "python"}
        ]);
        assert!(SkillManifest::parse(&dup).is_err());
    }

    #[test]
    fn script_tools_expose_typed_args() {
        let m = SkillManifest::parse(&manifest()).unwrap();
        let tools = script_tools("PDF Tools", &m);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "pdf_tools_extract");
        assert_eq!(tools[0].parameters["required"], json!(["file", "pages"]));
        assert_eq!(tools[0].parameters["properties"]["file"]["type"], "string");
        assert_eq!(tools[0].parameters["additionalProperties"], false);
    }

    #[test]
    fn assemble_command_quotes_every_argument() {
        let script = SkillScript {
            name: "extract".into(),
            path: "scripts/extract.py".into(),
            language: "python".into(),
            description: String::new(),
            args: vec!["file".into(), "pages".into()],
            timeout_secs: None,
        };
        let command = assemble_command(
            &script,
            &json!({
                "file": "a b.pdf",
                "pages": "1; rm -rf /"
            }),
        )
        .unwrap();
        assert_eq!(
            command,
            "python3 scripts/extract.py 'a b.pdf' '1; rm -rf /'"
        );

        // 缺参数直接拒绝，不会拼出半条命令。
        assert!(assemble_command(&script, &json!({"file": "x"})).is_err());
        // 控制字符（换行注入）拒绝。
        assert!(assemble_command(&script, &json!({"file": "x", "pages": "1\n2"})).is_err());
    }

    #[test]
    fn unsafe_paths_never_reach_the_command() {
        let script = SkillScript {
            name: "x".into(),
            path: "../outside.sh".into(),
            language: "sh".into(),
            description: String::new(),
            args: Vec::new(),
            timeout_secs: None,
        };
        assert!(assemble_command(&script, &json!({})).is_err());
    }

    #[test]
    fn tool_names_are_collision_free_and_bounded() {
        assert_eq!(
            script_tool_name("PDF Tools", "extract"),
            "pdf_tools_extract"
        );
        let long = script_tool_name(&"s".repeat(80), "script");
        assert_eq!(long.chars().count(), 64);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }
}
