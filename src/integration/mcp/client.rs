//! MCP 客户端：握手 → 工具发现 → 工具调用。
//!
//! 所有请求都带超时与可选取消（`oneshot` 从 run 的取消令牌派生），
//! 服务端主动发来的通知忽略、请求回 `-32601`（我们不实现 sampling/roots）。

use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::{
    jsonrpc::{response_outcome, McpError, RpcMessage, PROTOCOL_VERSION},
    schema::{self, McpTool},
    transport::{self, Transport},
    McpServerSpec,
};

#[derive(Clone, Copy, Debug)]
pub struct McpLimits {
    pub request_timeout: Duration,
}

impl Default for McpLimits {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
        }
    }
}

impl McpLimits {
    pub fn from_config(cfg: &crate::config::IntegrationConfig) -> Self {
        Self {
            request_timeout: Duration::from_millis(cfg.mcp_request_timeout_ms.max(1_000)),
        }
    }
}

/// 探测结果（写回 `mcp_servers` 的 protocol_version/server_info/tools）。
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub protocol_version: String,
    pub server_info: Value,
    pub tools: Vec<McpTool>,
}

pub struct McpClient {
    transport: Box<dyn Transport>,
    server: String,
    limits: McpLimits,
    next_id: u64,
    tools: Vec<McpTool>,
    protocol_version: String,
    server_info: Value,
}

impl McpClient {
    pub fn new(
        transport: Box<dyn Transport>,
        server: impl Into<String>,
        limits: McpLimits,
    ) -> Self {
        Self {
            transport,
            server: server.into(),
            limits,
            next_id: 1,
            tools: Vec::new(),
            protocol_version: String::new(),
            server_info: Value::Null,
        }
    }

    pub async fn connect(spec: &McpServerSpec, limits: McpLimits) -> Result<Self, McpError> {
        let transport = transport::connect(spec).await?;
        Ok(Self::new(transport, spec.name.clone(), limits))
    }

    pub fn server_name(&self) -> &str {
        &self.server
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn server_info(&self) -> &Value {
        &self.server_info
    }

    /// 已完成发现结果的工具（不含白名单过滤）。
    pub fn cached_tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// `initialize` 握手 + `notifications/initialized`。
    pub async fn initialize(&mut self) -> Result<Value, McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"roots": {"listChanged": false}},
            "clientInfo": {"name": "chiron-server", "version": env!("CARGO_PKG_VERSION")}
        });
        let result = self.request("initialize", Some(params), None).await?;
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION)
            .to_string();
        self.server_info = result.get("serverInfo").cloned().unwrap_or(Value::Null);
        // 通知失败不致命：很多 server 直接忽略它。
        let _ = self
            .transport
            .send(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .await;
        Ok(result)
    }

    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", None, None).await?;
        let tools = schema::validate_tool_list(&result)?;
        self.tools = tools.clone();
        Ok(tools)
    }

    /// 调用工具。`arguments` 会先按发现到的 schema 校验，再发给对端。
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        cancel: Option<oneshot::Receiver<()>>,
    ) -> Result<Value, McpError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
            .ok_or_else(|| McpError::UnknownTool(name.to_string()))?;
        schema::validate_arguments(&tool.input_schema, &arguments)?;

        let params = json!({
            "name": name,
            "arguments": if arguments.is_null() { json!({}) } else { arguments },
        });
        let result = self.request("tools/call", Some(params), cancel).await?;
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(McpError::Rpc {
                code: 0,
                message: format!("tool `{name}` reported an error"),
                data: Some(result),
            });
        }
        Ok(result)
    }

    /// 一整套探测：握手 + 发现，结束后关连接。
    pub async fn probe(mut self) -> Result<ProbeResult, McpError> {
        self.initialize().await?;
        let tools = self.list_tools().await?;
        let out = ProbeResult {
            protocol_version: self.protocol_version.clone(),
            server_info: self.server_info.clone(),
            tools,
        };
        self.close().await;
        Ok(out)
    }

    pub async fn close(&mut self) {
        self.transport.close().await;
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        cancel: Option<oneshot::Receiver<()>>,
    ) -> Result<Value, McpError> {
        let timeout = self.limits.request_timeout;
        let method_owned = method.to_string();
        let outcome = tokio::select! {
            biased;
            _ = wait_cancel(cancel) => Err(McpError::Cancelled),
            result = tokio::time::timeout(timeout, self.request_loop(method, params)) => {
                match result {
                    Ok(result) => result,
                    Err(_) => Err(McpError::Timeout {
                        method: method_owned,
                        timeout_ms: timeout.as_millis() as u64,
                    }),
                }
            }
        };
        outcome
    }

    async fn request_loop(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let payload = serde_json::to_value(RpcMessage::request(id, method, params))
            .map_err(|e| McpError::Protocol(format!("encode failed: {e}")))?;
        self.transport.send(&payload).await?;

        loop {
            let raw = self.transport.recv().await?;
            let reply = RpcMessage::from_value(raw)?;
            if reply.method().is_some() {
                self.reject_server_request(reply).await;
                continue;
            }
            // 迟到的其他响应（超时后到达）：丢弃，不污染当前请求。
            if reply.id() != Some(&json!(id)) {
                continue;
            }
            return match response_outcome(reply) {
                Some(outcome) => outcome,
                None => Err(McpError::Protocol(
                    "response carried neither result nor error".into(),
                )),
            };
        }
    }

    /// server → client 的通知忽略；请求一律回不支持。
    async fn reject_server_request(&mut self, message: RpcMessage) {
        let Some(id) = message.id().cloned() else {
            return;
        };
        let error = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "client does not implement server requests"}
        });
        let _ = self.transport.send(&error).await;
    }
}

async fn wait_cancel(cancel: Option<oneshot::Receiver<()>>) {
    match cancel {
        Some(rx) => {
            let _ = rx.await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// 把 `tools/call` 结果里的文本内容拼成一段字符串（写回消息/事件用）。
pub fn result_text(result: &Value) -> String {
    let Some(items) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut out = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    out.push(text.to_string());
                }
            }
            Some("resource") => {
                if let Some(text) = item
                    .get("resource")
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                {
                    out.push(text.to_string());
                }
            }
            _ => out.push(item.to_string()),
        }
    }
    if out.is_empty() {
        result.to_string()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::mcp::transport::mock::MockTransport;

    fn init_reply(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mock", "version": "1.0.0"}
            }
        })
    }

    fn tools_reply(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": [{
                "name": "echo",
                "description": "echo back",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }
            }]}
        })
    }

    fn call_reply(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"content": [{"type": "text", "text": "hi"}], "isError": false}
        })
    }

    fn limits() -> McpLimits {
        McpLimits {
            request_timeout: Duration::from_millis(500),
        }
    }

    #[tokio::test]
    async fn handshake_discover_and_call() {
        let mock = MockTransport::new(vec![
            // 服务端先来一条通知：必须被忽略且不影响 id 匹配。
            json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info"}}),
            init_reply(1),
            tools_reply(2),
            call_reply(3),
        ]);
        let mut client = McpClient::new(Box::new(mock), "mock", limits());

        client.initialize().await.unwrap();
        assert_eq!(client.protocol_version(), "2025-06-18");
        assert_eq!(client.server_info()["name"], "mock");

        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(client.cached_tools()[0].name, "echo");

        let result = client
            .call_tool("echo", json!({"text": "hi"}), None)
            .await
            .unwrap();
        assert_eq!(result_text(&result), "hi");
    }

    #[tokio::test]
    async fn call_rejects_unknown_tool_and_bad_arguments() {
        let mock = MockTransport::new(vec![init_reply(1), tools_reply(2)]);
        let mut client = McpClient::new(Box::new(mock), "mock", limits());
        client.initialize().await.unwrap();
        client.list_tools().await.unwrap();

        let err = client.call_tool("nope", json!({}), None).await.unwrap_err();
        assert!(matches!(err, McpError::UnknownTool(_)));

        let err = client.call_tool("echo", json!({}), None).await.unwrap_err();
        assert!(matches!(err, McpError::Schema(_)), "{err}");
    }

    #[tokio::test]
    async fn timeout_and_cancel_are_distinct() {
        // 没有应答：必须超时而不是挂死。
        let mock = MockTransport::blocking(Vec::new());
        let mut client = McpClient::new(
            Box::new(mock),
            "mock",
            McpLimits {
                request_timeout: Duration::from_millis(50),
            },
        );
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, McpError::Timeout { .. }), "{err}");
        assert_eq!(err.code(), "mcp.timeout");

        // 取消令牌先触发：错误是 Cancelled 而非 Timeout。
        let mock = MockTransport::blocking(vec![init_reply(1), tools_reply(2)]);
        let mut client = McpClient::new(
            Box::new(mock),
            "mock",
            McpLimits {
                request_timeout: Duration::from_secs(30),
            },
        );
        client.initialize().await.unwrap();
        client.list_tools().await.unwrap();
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();
        let err = client
            .call_tool("echo", json!({"text": "x"}), Some(rx))
            .await
            .unwrap_err();
        // 取消优先于超时：两个错误码必须可区分。
        assert!(matches!(err, McpError::Cancelled), "{err}");
        assert_eq!(err.code(), "mcp.cancelled");
    }

    #[tokio::test]
    async fn rpc_error_and_iserror_are_surfaced() {
        let mock = MockTransport::new(vec![
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32602, "message": "bad params"}}),
        ]);
        let mut client = McpClient::new(Box::new(mock), "mock", limits());
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32602, .. }));

        let mock = MockTransport::new(vec![
            init_reply(1),
            tools_reply(2),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {"content": [], "isError": true}
            }),
        ]);
        let mut client = McpClient::new(Box::new(mock), "mock", limits());
        client.initialize().await.unwrap();
        client.list_tools().await.unwrap();
        let err = client
            .call_tool("echo", json!({"text": "x"}), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reported an error"));
    }

    #[tokio::test]
    async fn server_initiated_request_gets_method_not_found() {
        let mock = MockTransport::new(vec![
            json!({"jsonrpc": "2.0", "id": 42, "method": "sampling/createMessage"}),
            init_reply(1),
        ]);
        let mut client = McpClient::new(Box::new(mock), "mock", limits());
        client.initialize().await.unwrap();
        // 客户端自己的 id 从 1 开始；对 42 的拒绝不应干扰握手。
        assert_eq!(client.protocol_version(), "2025-06-18");
    }

    #[test]
    fn result_text_flattens_content_blocks() {
        let result = json!({"content": [
            {"type": "text", "text": "a"},
            {"type": "resource", "resource": {"text": "b"}},
            {"type": "image", "data": "x"}
        ]});
        assert_eq!(
            result_text(&result),
            "a\nb\n{\"data\":\"x\",\"type\":\"image\"}"
        );
        assert_eq!(result_text(&json!({"ok": true})), "{\"ok\":true}");
    }
}
