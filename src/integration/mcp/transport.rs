//! MCP 传输层：stdio / SSE / streamable HTTP。
//!
//! 三者只在"字节怎么进出"上不同，握手与工具语义在 [`super::client`]。
//! 传输层不做业务判断，只负责把一条 JSON 消息发出去、把下一条收回来。

use std::collections::{BTreeMap, VecDeque};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;
use std::process::Stdio;

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
    task::JoinHandle,
};

use crate::provider::sse::SseParser;

use super::{
    framing::{FrameDecoder, Framing},
    jsonrpc::McpError,
    McpServerSpec, TransportKind,
};

/// 一条消息的收发通道。`close` 必须幂等，`recv` 在连接断开时返回错误。
#[async_trait]
pub trait Transport: Send {
    async fn send(&mut self, message: &Value) -> Result<(), McpError>;
    async fn recv(&mut self) -> Result<Value, McpError>;
    async fn close(&mut self);
}

/// 按 spec 建连（stdio 会真的拉进程，调用方需先做命令白名单校验）。
pub async fn connect(spec: &McpServerSpec) -> Result<Box<dyn Transport>, McpError> {
    match spec.transport {
        TransportKind::Stdio => Ok(Box::new(StdioTransport::spawn(spec).await?)),
        TransportKind::Http => Ok(Box::new(HttpTransport::new(spec)?)),
        TransportKind::Sse => Ok(Box::new(SseTransport::connect(spec).await?)),
    }
}

// ---------------------------------------------------------------- stdio ----

/// 子进程的 stdin/stdout。泛型是为了让测试用内存管道跑同一份 I/O 代码。
pub struct StdioTransport<R, W> {
    reader: R,
    writer: W,
    decoder: FrameDecoder,
    child: Option<Child>,
}

impl StdioTransport<ChildStdout, ChildStdin> {
    /// `command` 直接 exec，不经过 shell：参数里的 `;`、`$(...)` 只是普通字符。
    pub async fn spawn(spec: &McpServerSpec) -> Result<Self, McpError> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr 丢掉：外部 server 的日志不能混进 JSON-RPC 流。
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| McpError::Transport(format!("spawn `{}` failed: {e}", spec.command)))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child stdout not captured".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child stdin not captured".into()))?;
        Ok(Self {
            reader: stdout,
            writer: stdin,
            decoder: FrameDecoder::new(spec.framing),
            child: Some(child),
        })
    }
}

impl<R, W> StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// 测试/嵌入用：直接给一对管道。
    pub fn from_parts(reader: R, writer: W, framing: Framing) -> Self {
        Self {
            reader,
            writer,
            decoder: FrameDecoder::new(framing),
            child: None,
        }
    }

    async fn read_message(&mut self) -> Result<Value, McpError> {
        loop {
            if let Some(frame) = self.decoder.pop()? {
                return serde_json::from_slice(&frame)
                    .map_err(|e| McpError::Protocol(format!("invalid JSON frame: {e}")));
            }
            let mut chunk = [0u8; 8192];
            let read = self
                .reader
                .read(&mut chunk)
                .await
                .map_err(|e| McpError::Transport(format!("read failed: {e}")))?;
            if read == 0 {
                return Err(McpError::Transport("connection closed".into()));
            }
            self.decoder.push(&chunk[..read])?;
        }
    }
}

#[async_trait]
impl<R, W> Transport for StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let frame = self.decoder.framing().encode(message);
        self.writer
            .write_all(&frame)
            .await
            .map_err(|e| McpError::Transport(format!("write failed: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| McpError::Transport(format!("flush failed: {e}")))
    }

    async fn recv(&mut self) -> Result<Value, McpError> {
        self.read_message().await
    }

    async fn close(&mut self) {
        let _ = self.writer.shutdown().await;
        if let Some(child) = self.child.as_mut() {
            // 先礼后兵：关闭 stdin 后仍不退出的，直接 kill。
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

// ----------------------------------------------------------------- http ----

/// streamable HTTP：一个 POST 对应一条请求，响应可能是 JSON 或 SSE。
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
    inbox: VecDeque<Value>,
}

impl HttpTransport {
    pub fn new(spec: &McpServerSpec) -> Result<Self, McpError> {
        if spec.url.trim().is_empty() {
            return Err(McpError::Transport("http transport requires url".into()));
        }
        let client = reqwest::Client::builder()
            .user_agent("chiron-mcp/1")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpError::Transport(format!("http client failed: {e}")))?;
        Ok(Self {
            client,
            url: spec.url.clone(),
            headers: spec.headers.clone(),
            session_id: None,
            inbox: VecDeque::new(),
        })
    }

    /// 把一次 HTTP 响应体拆成若干 JSON-RPC 消息（JSON 单条/批量，或 SSE）。
    fn absorb_body(content_type: &str, body: &[u8]) -> Result<Vec<Value>, McpError> {
        if content_type.contains("text/event-stream") {
            let mut parser = SseParser::new();
            let mut events = parser.push(body);
            if let Some(last) = parser.finish() {
                events.push(last);
            }
            let mut out = Vec::new();
            for event in events {
                if event.is_done() || event.data.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&event.data)
                    .map_err(|e| McpError::Protocol(format!("invalid SSE payload: {e}")))?;
                out.extend(split_batch(value)?);
            }
            return Ok(out);
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            // 通知的 202 响应没有 body，是合法的。
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_slice(body)
            .map_err(|e| McpError::Protocol(format!("invalid JSON response: {e}")))?;
        split_batch(value)
    }
}

fn split_batch(value: Value) -> Result<Vec<Value>, McpError> {
    match value {
        Value::Array(items) => {
            if items.len() > 64 {
                return Err(McpError::Protocol("batch too large".into()));
            }
            Ok(items)
        }
        other => Ok(vec![other]),
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(message);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(session) = &self.session_id {
            request = request.header("mcp-session-id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("request failed: {e}")))?;
        let status = response.status();
        // 服务端可能在 initialize 时分配会话 id，后续请求必须带上。
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session.to_string());
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = response
            .bytes()
            .await
            .map_err(|e| McpError::Transport(format!("body failed: {e}")))?;
        if !status.is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {}: {}",
                status.as_u16(),
                truncate(&String::from_utf8_lossy(&body), 256)
            )));
        }
        for value in Self::absorb_body(&content_type, &body)? {
            self.inbox.push_back(value);
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Value, McpError> {
        self.inbox
            .pop_front()
            .ok_or_else(|| McpError::Transport("no pending http response".into()))
    }

    async fn close(&mut self) {
        self.inbox.clear();
    }
}

// ------------------------------------------------------------------ sse ----

/// 旧版 SSE 传输：GET 长连接收响应，POST 到 `endpoint` 事件给的地址。
pub struct SseTransport {
    client: reqwest::Client,
    post_url: String,
    headers: BTreeMap<String, String>,
    inbox: mpsc::Receiver<Value>,
    streamer: JoinHandle<()>,
}

impl SseTransport {
    pub async fn connect(spec: &McpServerSpec) -> Result<Self, McpError> {
        if spec.url.trim().is_empty() {
            return Err(McpError::Transport("sse transport requires url".into()));
        }
        let client = reqwest::Client::builder()
            .user_agent("chiron-mcp/1")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpError::Transport(format!("http client failed: {e}")))?;
        let mut request = client.get(&spec.url).header("accept", "text/event-stream");
        for (name, value) in &spec.headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("sse connect failed: {e}")))?;
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "sse connect HTTP {}",
                response.status().as_u16()
            )));
        }

        let base = spec.url.clone();
        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        let mut endpoint: Option<String> = None;

        // 先读到 `endpoint` 事件；拿到 POST 地址前不算连上。
        while endpoint.is_none() {
            let Some(chunk) = stream.next().await else {
                return Err(McpError::Transport(
                    "sse stream closed before endpoint event".into(),
                ));
            };
            let chunk = chunk.map_err(|e| McpError::Transport(format!("sse read failed: {e}")))?;
            for event in parser.push(&chunk) {
                if event.event_name() == "endpoint" {
                    endpoint = Some(resolve_endpoint(&base, event.data.trim())?);
                }
            }
        }

        let post_url = endpoint.ok_or_else(|| McpError::Transport("no endpoint".into()))?;
        let (tx, rx) = mpsc::channel::<Value>(64);
        let streamer = tokio::spawn(async move {
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else { break };
                for event in parser.push(&chunk) {
                    if event.event_name() != "message" || event.is_done() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&event.data) {
                        if tx.send(value).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        Ok(Self {
            client,
            post_url,
            headers: spec.headers.clone(),
            inbox: rx,
            streamer,
        })
    }
}

/// `endpoint` 可能是相对路径，也可能是同源绝对 URL；跨源一律拒绝，
/// 防止 SSE 服务端把后续 POST 指向内网或第三方地址。
fn resolve_endpoint(base: &str, endpoint: &str) -> Result<String, McpError> {
    let base_url =
        url::Url::parse(base).map_err(|e| McpError::Transport(format!("invalid sse url: {e}")))?;
    let target = url::Url::parse(endpoint)
        .or_else(|_| base_url.join(endpoint))
        .map_err(|e| McpError::Transport(format!("invalid endpoint url: {e}")))?;
    if target.scheme() != base_url.scheme()
        || target.host_str() != base_url.host_str()
        || target.port_or_known_default() != base_url.port_or_known_default()
    {
        return Err(McpError::Transport(
            "sse endpoint must stay on the configured origin".into(),
        ));
    }
    Ok(target.to_string())
}

#[async_trait]
impl Transport for SseTransport {
    async fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let mut request = self
            .client
            .post(&self.post_url)
            .header("content-type", "application/json")
            .json(message);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("sse post failed: {e}")))?;
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "sse post HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Value, McpError> {
        self.inbox
            .recv()
            .await
            .ok_or_else(|| McpError::Transport("sse stream closed".into()))
    }

    async fn close(&mut self) {
        self.streamer.abort();
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

/// 单元测试用的内存传输：脚本化应答 + 记录收到的方法名。
#[cfg(test)]
pub(crate) mod mock {
    use super::*;

    pub struct MockTransport {
        /// 依次弹出的应答；剧本耗光后按 `block` 决定挂起还是断开。
        pub replies: VecDeque<Value>,
        pub sent: Vec<Value>,
        pub closed: bool,
        /// true = 剧本耗光后一直挂起（模拟"对端不回"以触发超时/取消）。
        pub block: bool,
    }

    impl MockTransport {
        pub fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: replies.into(),
                sent: Vec::new(),
                closed: false,
                block: false,
            }
        }

        pub fn blocking(replies: Vec<Value>) -> Self {
            Self {
                block: true,
                ..Self::new(replies)
            }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn send(&mut self, message: &Value) -> Result<(), McpError> {
            self.sent.push(message.clone());
            Ok(())
        }

        async fn recv(&mut self) -> Result<Value, McpError> {
            match self.replies.pop_front() {
                Some(reply) => Ok(reply),
                None if self.block => std::future::pending::<Result<Value, McpError>>().await,
                None => Err(McpError::Transport("mock exhausted".into())),
            }
        }

        async fn close(&mut self) {
            self.closed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    #[test]
    fn absorb_json_and_sse_bodies() {
        let single =
            HttpTransport::absorb_body("application/json", br#"{"id":1,"result":{}}"#).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0]["id"], 1);

        let batch =
            HttpTransport::absorb_body("application/json", br#"[{"id":1},{"id":2}]"#).unwrap();
        assert_eq!(batch.len(), 2);

        let sse = HttpTransport::absorb_body(
            "text/event-stream",
            b"event: message\ndata: {\"id\":3,\"result\":{}}\n\n",
        )
        .unwrap();
        assert_eq!(sse.len(), 1);
        assert_eq!(sse[0]["id"], 3);

        // 通知的 202：空 body 不算错误。
        assert!(HttpTransport::absorb_body("application/json", b"")
            .unwrap()
            .is_empty());
        assert!(HttpTransport::absorb_body("application/json", b"not json").is_err());
    }

    #[test]
    fn sse_endpoint_resolution_handles_relative_paths() {
        assert_eq!(
            resolve_endpoint("http://127.0.0.1:9000/sse", "/messages?session=1").unwrap(),
            "http://127.0.0.1:9000/messages?session=1"
        );
        assert_eq!(
            resolve_endpoint("http://127.0.0.1:9000/sse", "http://127.0.0.1:9000/messages").unwrap(),
            "http://127.0.0.1:9000/messages"
        );
        assert!(resolve_endpoint("http://127.0.0.1:9000/sse", "http://other/x").is_err());
        assert!(resolve_endpoint("not a url", "/x").is_err());
    }

    #[test]
    fn http_transport_requires_url() {
        let mut spec = McpServerSpec::stdio("s", "npx");
        spec.transport = TransportKind::Http;
        assert!(HttpTransport::new(&spec).is_err());
        spec.url = "https://example.test/mcp".into();
        assert!(HttpTransport::new(&spec).is_ok());
    }

    /// stdio 的分帧/读写路径：用内存管道跑真实 I/O，不需要外部进程。
    #[tokio::test]
    async fn stdio_transport_frames_over_pipes() {
        let (client_writer, server_reader) = tokio::io::duplex(8192);
        let (server_writer, client_reader) = tokio::io::duplex(8192);

        // mock server：一行请求 → 一行应答，id 原样回显。
        let server = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(server_reader);
            let mut writer = server_writer;
            let mut line = String::new();
            loop {
                line.clear();
                let read = reader.read_line(&mut line).await.unwrap();
                if read == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}});
                writer
                    .write_all(&Framing::Ndjson.encode(&reply))
                    .await
                    .unwrap();
                writer.flush().await.unwrap();
            }
        });

        let mut transport =
            StdioTransport::from_parts(client_reader, client_writer, Framing::Ndjson);
        transport
            .send(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .await
            .unwrap();
        let reply = transport.recv().await.unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["ok"], true);

        transport.close().await;
        let _ = server.await;
    }

    /// 真实的 stdio 子进程（只依赖 sh，在任何 unix CI 上都能跑）。
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_spawns_a_jsonrpc_mock_server() {
        use crate::integration::mcp::{McpClient, McpLimits};
        use std::time::Duration;

        let script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"mock"}}}' ;;
    *'"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"d","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}' ;;
    *'"tools/call"'*) printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}]}}' ;;
  esac
done
"#;
        let mut spec = McpServerSpec::stdio("mock", "sh");
        spec.args = vec!["-c".into(), script.into()];
        let cfg = crate::config::IntegrationConfig {
            mcp_commands: vec!["sh".into()],
            ..Default::default()
        };
        spec.validate(&cfg).unwrap();

        let limits = McpLimits {
            request_timeout: Duration::from_secs(10),
        };
        let mut client = McpClient::connect(&spec, limits).await.unwrap();
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let result = client
            .call_tool("echo", json!({"text": "ping"}), None)
            .await
            .unwrap();
        assert_eq!(
            crate::integration::mcp::client::result_text(&result),
            "pong"
        );
        client.close().await;
    }
}
