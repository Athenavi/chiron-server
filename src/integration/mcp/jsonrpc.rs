//! JSON-RPC 2.0 消息（MCP 的线格式）。
//!
//! MCP 只在 JSON-RPC 之上加语义：`initialize` / `tools/list` / `tools/call` 与
//! `notifications/initialized`。这里只做消息建模与错误归一，传输细节在
//! [`super::transport`]，分帧在 [`super::framing`]。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const JSONRPC_VERSION: &str = "2.0";

/// 我们声明支持的协议版本。服务端回别的版本时按协商结果记下来，
/// 但只有这个版本的工具结构是我们校验过的。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// 单条 JSON-RPC 消息：请求 / 响应（result）/ 错误（error）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcMessage {
    #[serde(default = "default_version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(flatten)]
    pub body: RpcBody,
}

fn default_version() -> String {
    JSONRPC_VERSION.to_string()
}

/// 用 flatten 建模，避免为三种消息各写一份信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcBody {
    Request {
        method: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
    Result {
        result: Value,
    },
    Error {
        error: RpcErrorObject,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcMessage {
    pub fn request(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: default_version(),
            id: Some(json!(id)),
            body: RpcBody::Request {
                method: method.into(),
                params,
            },
        }
    }

    /// 通知没有 id：接收方不回包。
    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: default_version(),
            id: None,
            body: RpcBody::Request {
                method: method.into(),
                params,
            },
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, McpError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| McpError::Protocol(format!("invalid JSON-RPC frame: {e}")))?;
        Self::from_value(value)
    }

    /// 传输层已经把字节解析成 `Value`，这里只做形状与版本校验。
    pub fn from_value(value: Value) -> Result<Self, McpError> {
        let msg: Self = serde_json::from_value(value)
            .map_err(|e| McpError::Protocol(format!("not a JSON-RPC message: {e}")))?;
        if msg.jsonrpc != JSONRPC_VERSION {
            return Err(McpError::Protocol(format!(
                "unsupported jsonrpc version `{}`",
                msg.jsonrpc
            )));
        }
        Ok(msg)
    }

    pub fn id(&self) -> Option<&Value> {
        self.id.as_ref()
    }

    /// 服务端主动发来的请求/通知（如 `notifications/message`），调用方可以忽略。
    pub fn method(&self) -> Option<&str> {
        match &self.body {
            RpcBody::Request { method, .. } => Some(method),
            _ => None,
        }
    }
}

/// 把 `result` / `error` 归一成 `Result`，非响应消息返回 `None`。
pub fn response_outcome(msg: RpcMessage) -> Option<Result<Value, McpError>> {
    match msg.body {
        RpcBody::Result { result } => Some(Ok(result)),
        RpcBody::Error { error } => Some(Err(McpError::Rpc {
            code: error.code,
            message: error.message,
            data: error.data,
        })),
        RpcBody::Request { .. } => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpError {
    /// 进程/连接层面的失败（spawn 失败、管道断开、HTTP 非 2xx）。
    Transport(String),
    /// 对端说的不是 MCP：帧不合法、版本不符、结果形状不对。
    Protocol(String),
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// 超时与取消必须与业务错误分开，前端/审计要能区分。
    Timeout {
        method: String,
        timeout_ms: u64,
    },
    Cancelled,
    UnknownTool(String),
    /// 工具 schema 不合法（发现阶段）或参数不满足 schema（调用阶段）。
    Schema(String),
    /// 被安全策略拒绝：来源白名单、SSRF、命令白名单。
    Blocked(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(m) => write!(f, "transport error: {m}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
            Self::Rpc { code, message, .. } => write!(f, "json-rpc error {code}: {message}"),
            Self::Timeout { method, timeout_ms } => {
                write!(f, "timeout after {timeout_ms}ms calling {method}")
            }
            Self::Cancelled => write!(f, "cancelled"),
            Self::UnknownTool(name) => write!(f, "unknown tool `{name}`"),
            Self::Schema(m) => write!(f, "invalid schema: {m}"),
            Self::Blocked(m) => write!(f, "blocked: {m}"),
        }
    }
}

impl std::error::Error for McpError {}

impl McpError {
    /// 写进 API 错误包络的稳定 code（前端只认这个）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "mcp.timeout",
            Self::Cancelled => "mcp.cancelled",
            Self::Blocked(_) => "mcp.blocked",
            Self::Schema(_) => "mcp.invalid_schema",
            Self::UnknownTool(_) => "mcp.unknown_tool",
            _ => "mcp.failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_roundtrips_through_json() {
        let msg = RpcMessage::request(7, "tools/list", None);
        let text = serde_json::to_vec(&msg).unwrap();
        let back = RpcMessage::parse(&text).unwrap();
        assert_eq!(back.id(), Some(&json!(7)));
        assert_eq!(back.method(), Some("tools/list"));
        assert_eq!(back.body, msg.body);
    }

    #[test]
    fn result_and_error_are_classified() {
        let ok = RpcMessage::parse(br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#).unwrap();
        assert_eq!(response_outcome(ok).unwrap().unwrap(), json!({"tools": []}));

        let bad = RpcMessage::parse(
            br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nope"}}"#,
        )
        .unwrap();
        let err = response_outcome(bad).unwrap().unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32601, .. }));
        assert_eq!(err.code(), "mcp.failed");
    }

    #[test]
    fn server_initiated_notification_is_not_a_response() {
        let n =
            RpcMessage::parse(br#"{"jsonrpc":"2.0","method":"notifications/message"}"#).unwrap();
        assert!(response_outcome(n).is_none());
    }

    #[test]
    fn wrong_version_and_garbage_are_protocol_errors() {
        let err = RpcMessage::parse(br#"{"jsonrpc":"1.0","id":1,"result":{}}"#).unwrap_err();
        assert!(matches!(err, McpError::Protocol(_)));
        let err = RpcMessage::parse(b"not json").unwrap_err();
        assert!(matches!(err, McpError::Protocol(_)));
    }

    #[test]
    fn notification_has_no_id() {
        let msg = RpcMessage::notification("notifications/initialized", None);
        let value = serde_json::to_value(&msg).unwrap();
        assert!(value.get("id").is_none());
        assert_eq!(value["method"], "notifications/initialized");
    }
}
