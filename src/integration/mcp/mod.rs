//! MCP 客户端（PRD §7.8 / TASKS B9）。
//!
//! 分层：
//! * [`jsonrpc`]：JSON-RPC 2.0 消息与错误归一；
//! * [`framing`]：NDJSON / Content-Length 分帧（不碰 I/O）；
//! * [`transport`]：stdio / SSE / streamable HTTP 三条通道；
//! * [`schema`]：`tools/list` 结果与调用参数的 schema 校验；
//! * [`client`]：握手、工具发现、工具调用（超时 + 取消）；
//! * [`api`]：`/api/v1/mcp/*` 路由。
//!
//! 安全口径：外部 server 一律不可信。命令走白名单且不经 shell，URL 做 SSRF 校验，
//! 工具 schema/参数都过校验，单帧与工具数都有上限。

pub mod api;
pub mod client;
pub mod framing;
pub mod jsonrpc;
pub mod schema;
pub mod transport;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{config::IntegrationConfig, error::AppError};

pub use framing::Framing;
pub use jsonrpc::McpError;
pub use schema::McpTool;

/// 对外暴露的传输方式（与 `mcp_servers.transport` 列一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Stdio,
    Sse,
    Http,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Sse => "sse",
            Self::Http => "http",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "stdio" | "process" => Some(Self::Stdio),
            "sse" => Some(Self::Sse),
            "http" | "streamable-http" | "streamable" => Some(Self::Http),
            _ => None,
        }
    }
}

impl Serialize for TransportKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TransportKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        TransportKind::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown transport `{raw}`")))
    }
}

/// 建连所需的全部信息（与 DB 行解耦，便于测试与 Agent 装配）。
#[derive(Clone, Debug, PartialEq)]
pub struct McpServerSpec {
    pub name: String,
    pub transport: TransportKind,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub framing: Framing,
    /// 空 = 全部允许（仍受 enabled/租户边界约束）。
    pub tool_allowlist: Vec<String>,
}

impl McpServerSpec {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: TransportKind::Stdio,
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            framing: Framing::Ndjson,
            tool_allowlist: Vec::new(),
        }
    }

    pub fn cross_origin(
        name: impl Into<String>,
        transport: TransportKind,
        url: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            transport,
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: url.into(),
            headers: BTreeMap::new(),
            framing: Framing::Ndjson,
            tool_allowlist: Vec::new(),
        }
    }

    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tool_allowlist.is_empty() || self.tool_allowlist.iter().any(|t| t == tool)
    }

    /// 同步校验（无 DNS）。DNS/私网判定在 [`Self::check_network`]。
    pub fn validate(&self, cfg: &IntegrationConfig) -> Result<(), McpError> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(McpError::Blocked("server name must be 1..=64 chars".into()));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err(McpError::Blocked("server name has control chars".into()));
        }
        for (key, value) in &self.env {
            if key.trim().is_empty() || key.contains('=') || key.chars().any(|c| c.is_control()) {
                return Err(McpError::Blocked(format!("invalid env name `{key}`")));
            }
            if ENV_DENYLIST
                .iter()
                .any(|blocked| key.eq_ignore_ascii_case(blocked))
            {
                return Err(McpError::Blocked(format!(
                    "env `{key}` is not allowed (loader/option injection)"
                )));
            }
            if value.chars().any(|c| c.is_control()) {
                return Err(McpError::Blocked(format!("env `{key}` has control chars")));
            }
        }
        for header in self.headers.keys() {
            if !is_header_name(header) {
                return Err(McpError::Blocked(format!("invalid header name `{header}`")));
            }
        }
        for value in self.headers.values() {
            if value.contains('\r') || value.contains('\n') {
                return Err(McpError::Blocked("header value has CR/LF".into()));
            }
        }

        match self.transport {
            TransportKind::Stdio => {
                let command = self.command.trim();
                if command.is_empty() {
                    return Err(McpError::Blocked(
                        "stdio transport requires a command".into(),
                    ));
                }
                if command.chars().any(|c| c.is_control()) {
                    return Err(McpError::Blocked("command has control chars".into()));
                }
                if !cfg.command_allowed(command) {
                    return Err(McpError::Blocked(format!(
                        "command `{command}` is not in CHIRON_MCP_COMMANDS"
                    )));
                }
                if self.args.iter().any(|a| a.contains('\0')) {
                    return Err(McpError::Blocked("argument contains NUL".into()));
                }
                if self.args.len() > 64 {
                    return Err(McpError::Blocked("too many arguments".into()));
                }
            }
            TransportKind::Sse | TransportKind::Http => {
                validate_url(&self.url)?;
            }
        }
        if self.tool_allowlist.len() > schema::MAX_TOOLS {
            return Err(McpError::Blocked("tool allowlist too large".into()));
        }
        Ok(())
    }

    /// 出网校验：复用 Agent 的 SSRF 白名单（域名解析后逐地址判定）。
    pub async fn check_network(&self, cfg: &IntegrationConfig) -> Result<(), McpError> {
        if matches!(self.transport, TransportKind::Stdio) {
            return Ok(());
        }
        let url = match url::Url::parse(self.url.trim()) {
            Ok(url) => url,
            Err(e) => return Err(McpError::Blocked(format!("invalid url: {e}"))),
        };
        crate::agent::web::WebClient::new(cfg.mcp_allow_private)
            .check_url(&url)
            .await
            .map(|_| ())
            .map_err(|e| McpError::Blocked(e.to_string()))
    }
}

/// 会劫持子进程加载/行为的变量一律禁设（否则白名单命令可被间接变成任意代码）。
const ENV_DENYLIST: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "PYTHONPATH",
];

fn validate_url(raw: &str) -> Result<(), McpError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(McpError::Blocked("url is required".into()));
    }
    let url =
        url::Url::parse(trimmed).map_err(|e| McpError::Blocked(format!("invalid url: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(McpError::Blocked("url must be http(s)".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(McpError::Blocked("url must not embed credentials".into()));
    }
    if url.host_str().is_none() {
        return Err(McpError::Blocked("url has no host".into()));
    }
    Ok(())
}

fn is_header_name(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `mcp_servers` 行。`tools` 是最近一次 `tools/list` 的原始结果（已校验）。
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct McpServerRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub owner_user_id: Option<Uuid>,
    pub name: String,
    pub transport: String,
    pub command: String,
    pub args: Value,
    pub env: Value,
    pub url: String,
    pub headers: Value,
    pub tool_allowlist: Value,
    pub enabled: bool,
    pub protocol_version: Option<String>,
    pub server_info: Value,
    pub tools: Value,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_error: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const SERVER_COLUMNS: &str = "id, tenant_id, owner_user_id, name, transport, command, args, \
     env, url, headers, tool_allowlist, enabled, protocol_version, server_info, tools, \
     last_probe_at, last_error, created_at, updated_at";

impl McpServerRow {
    pub fn spec(&self) -> Result<McpServerSpec, AppError> {
        let transport = TransportKind::parse(&self.transport).ok_or_else(|| {
            AppError::internal(format!("stored transport `{}` invalid", self.transport))
        })?;
        Ok(McpServerSpec {
            name: self.name.clone(),
            transport,
            command: self.command.clone(),
            args: string_list(&self.args),
            env: string_map(&self.env),
            url: self.url.clone(),
            headers: string_map(&self.headers),
            framing: Framing::Ndjson,
            tool_allowlist: string_list(&self.tool_allowlist),
        })
    }

    /// 从 `tools` 列还原出校验过的工具（库里可能被旧版本写过，再校验一次）。
    pub fn tool_specs(&self) -> Vec<McpTool> {
        schema::validate_tool_list(&json!({ "tools": self.tools })).unwrap_or_default()
    }

    /// 允许暴露给模型的工具（受 enabled 与白名单约束）。
    pub fn allowed_tools(&self) -> Vec<McpTool> {
        if !self.enabled {
            return Vec::new();
        }
        let spec = match self.spec() {
            Ok(spec) => spec,
            Err(_) => return Vec::new(),
        };
        self.tool_specs()
            .into_iter()
            .filter(|tool| spec.allows_tool(&tool.name))
            .collect()
    }
}

pub(crate) fn string_list(raw: &Value) -> Vec<String> {
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .take(schema::MAX_TOOLS * 2)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn string_map(raw: &Value) -> BTreeMap<String, String> {
    raw.as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .take(64)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> IntegrationConfig {
        IntegrationConfig {
            mcp_commands: vec!["npx".into(), "uvx".into()],
            ..IntegrationConfig::default()
        }
    }

    #[test]
    fn stdio_is_denied_by_default() {
        let spec = McpServerSpec::stdio("fs", "npx");
        assert!(spec.validate(&IntegrationConfig::default()).is_err());
    }

    #[test]
    fn stdio_spec_requires_allowlisted_command() {
        let mut spec = McpServerSpec::stdio("fs", "npx");
        assert!(spec.validate(&cfg()).is_ok());

        spec.command = "/usr/local/bin/uvx".into();
        assert!(spec.validate(&cfg()).is_ok(), "basename match");

        spec.command = "bash".into();
        let err = spec.validate(&cfg()).unwrap_err();
        assert!(err.to_string().contains("CHIRON_MCP_COMMANDS"));

        spec.command = "npx".into();
        spec.env.insert("LD_PRELOAD".into(), "/tmp/evil.so".into());
        assert!(spec.validate(&cfg()).is_err(), "loader injection");
    }

    #[test]
    fn http_spec_requires_sane_url() {
        let mut spec =
            McpServerSpec::cross_origin("web", TransportKind::Http, "https://x.test/mcp");
        assert!(spec.validate(&cfg()).is_ok());

        spec.url = "ftp://x.test/mcp".into();
        assert!(spec.validate(&cfg()).is_err());
        spec.url = "https://user:pw@x.test/mcp".into();
        assert!(spec.validate(&cfg()).is_err());
        spec.url = "not a url".into();
        assert!(spec.validate(&cfg()).is_err());

        spec.url = "https://x.test/mcp".into();
        spec.headers.insert("bad header".into(), "v".into());
        assert!(spec.validate(&cfg()).is_err());
        spec.headers.clear();
        spec.headers.insert("authorization".into(), "a\r\nb".into());
        assert!(spec.validate(&cfg()).is_err());
    }

    #[test]
    fn tool_allowlist_filters_and_empty_means_all() {
        let mut spec = McpServerSpec::stdio("fs", "npx");
        assert!(spec.allows_tool("read"));
        spec.tool_allowlist = vec!["read".into()];
        assert!(spec.allows_tool("read"));
        assert!(!spec.allows_tool("write"));
    }

    #[test]
    fn transport_kind_roundtrips() {
        for kind in [
            TransportKind::Stdio,
            TransportKind::Sse,
            TransportKind::Http,
        ] {
            let text = serde_json::to_string(&kind).unwrap();
            let back: TransportKind = serde_json::from_str(&text).unwrap();
            assert_eq!(back, kind);
        }
        assert!(serde_json::from_str::<TransportKind>("\"carrier-pigeon\"").is_err());
        assert_eq!(
            TransportKind::parse("streamable-http"),
            Some(TransportKind::Http)
        );
    }

    #[test]
    fn row_helpers_tolerate_junk_json() {
        assert!(string_list(&json!("nope")).is_empty());
        assert!(string_map(&json!([1, 2])).is_empty());
        assert_eq!(string_list(&json!(["a", " ", 3])), vec!["a"]);
    }
}
