//! Anthropic Messages 协议适配（流式 + tool use 块 + cache/usage 口径）。
//!
//! 与 OpenAI 适配器的差异集中在三处，别的都走同一套公共类型：
//! 1. 请求体：`system` 是顶层字符串，消息 content 是 block 数组；
//! 2. 工具：`input_schema` + `tool_use`/`tool_result` 块；
//! 3. usage：input/output 分两次 SSE 事件给出，且带 cache_read/cache_creation。

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{pin_mut, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, RETRY_AFTER};
use serde_json::{json, Map, Value};

use super::sse::{SseEvent, SseParser};
use super::{
    retry_after_from_header, ChatMessage, ChatRequest, DeltaStream, FinishReason, ModelInfo,
    Provider, ProviderError, Role, StreamDelta, ToolChoice, ToolDef, Usage,
};
use crate::providers::Protocol;

/// Messages API 版本头。
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// `max_tokens` 在 Messages API 是必填项，缺省按 4096 兜底。
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    version: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            headers: Vec::new(),
            timeout: Duration::from_secs(300),
            version: ANTHROPIC_VERSION.to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.api_key = if key.trim().is_empty() {
            None
        } else {
            Some(key)
        };
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// preset 的 base_url 有两种：官方 `https://api.anthropic.com`（无 `/v1`）与
    /// 网关 `https://opencode.ai/zen`（同样不带 `/v1`），因此这里统一补齐。
    fn v1_root(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            base.to_string()
        } else {
            format!("{base}/v1")
        }
    }

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // content-type 交给 `.json()`，避免重复头
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_str(&self.version)
                .unwrap_or_else(|_| HeaderValue::from_static(ANTHROPIC_VERSION)),
        );
        if let Some(key) = &self.api_key {
            if let Ok(value) = HeaderValue::from_str(key) {
                headers.insert(HeaderName::from_static("x-api-key"), value);
            }
        }
        for (name, value) in &self.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        headers
    }

    fn request_body(&self, request: &ChatRequest) -> Value {
        let converted = messages_to_anthropic(&request.messages);
        let mut body = Map::new();
        body.insert("model".into(), Value::String(request.model.clone()));
        body.insert(
            "max_tokens".into(),
            json!(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1)),
        );
        body.insert("stream".into(), Value::Bool(true));
        body.insert("messages".into(), Value::Array(converted.messages));
        if let Some(system) = converted.system {
            body.insert("system".into(), Value::String(system));
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".into(),
                Value::Array(request.tools.iter().map(tool_to_anthropic).collect()),
            );
        }
        if let Some(choice) = &request.tool_choice {
            if let Some(value) = tool_choice_to_anthropic(choice) {
                body.insert("tool_choice".into(), value);
            }
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if !request.stop.is_empty() {
            body.insert("stop_sequences".into(), json!(&request.stop));
        }
        for (key, value) in &request.extra {
            body.insert(key.clone(), value.clone());
        }
        Value::Object(body)
    }
}

struct ConvertedMessages {
    system: Option<String>,
    messages: Vec<Value>,
}

/// Anthropic 要求 user/assistant 交替（tool_result 属于 user 轮），
/// 因此相邻同角色消息要合并成一个 content block 数组。
fn messages_to_anthropic(messages: &[ChatMessage]) -> ConvertedMessages {
    let mut system_parts: Vec<String> = Vec::new();
    let mut merged: Vec<(&'static str, Vec<Value>)> = Vec::new();

    for message in messages {
        let (role, blocks) = match message.role {
            Role::System => {
                if let Some(text) = message.content.as_deref() {
                    if !text.is_empty() {
                        system_parts.push(text.to_string());
                    }
                }
                continue;
            }
            Role::User => ("user", vec![text_block(message.content.as_deref())]),
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(text) = message.content.as_deref() {
                    if !text.is_empty() {
                        blocks.push(text_block(Some(text)));
                    }
                }
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments_json(),
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(text_block(Some("")));
                }
                ("assistant", blocks)
            }
            Role::Tool => {
                let id = message.tool_call_id.as_deref().unwrap_or_default();
                let text = message.content.as_deref().unwrap_or_default();
                (
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": text,
                    })],
                )
            }
        };
        match merged.last_mut() {
            Some((last_role, last_blocks)) if *last_role == role => {
                last_blocks.extend(blocks);
            }
            _ => merged.push((role, blocks)),
        }
    }

    ConvertedMessages {
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        },
        messages: merged
            .into_iter()
            .map(|(role, blocks)| json!({"role": role, "content": blocks}))
            .collect(),
    }
}

fn text_block(text: Option<&str>) -> Value {
    json!({"type": "text", "text": text.unwrap_or_default()})
}

fn tool_to_anthropic(tool: &ToolDef) -> Value {
    let schema = if tool.parameters.is_null() {
        json!({"type": "object", "properties": {}})
    } else {
        tool.parameters.clone()
    };
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": schema,
    })
}

/// Anthropic 没有 "none"（不传工具即可），返回 None 表示省略该字段。
fn tool_choice_to_anthropic(choice: &ToolChoice) -> Option<Value> {
    match choice {
        ToolChoice::Auto => Some(json!({"type": "auto"})),
        ToolChoice::None => None,
        ToolChoice::Required => Some(json!({"type": "any"})),
        ToolChoice::Named(name) => Some(json!({"type": "tool", "name": name})),
    }
}

fn parse_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: value
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

/// 把一条 Anthropic SSE 事件解成 delta（`ping` / `signature_delta` 等直接忽略）。
pub(crate) fn parse_anthropic_event(event: &SseEvent) -> Result<Vec<StreamDelta>, ProviderError> {
    if event.data.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&event.data)
        .map_err(|e| ProviderError::decode(format!("invalid Anthropic SSE data: {e}")))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| event.event_name());

    match kind {
        "error" => {
            let error = value.get("error");
            let error_kind = error
                .and_then(|e| e.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("error");
            let message = error
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("anthropic stream error");
            Err(ProviderError::stream_error(error_kind, message))
        }
        "message_start" => {
            let mut out = Vec::new();
            if let Some(usage) = value.pointer("/message/usage") {
                out.push(StreamDelta::Usage(parse_usage(usage)));
            }
            Ok(out)
        }
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let block = value.get("content_block");
            let block_type = block
                .and_then(|b| b.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if block_type == "tool_use" {
                let id = block
                    .and_then(|b| b.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = block
                    .and_then(|b| b.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Ok(vec![StreamDelta::ToolCallStart {
                    index,
                    id: id.to_string(),
                    name: name.to_string(),
                }]);
            }
            Ok(Vec::new())
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(delta) = value.get("delta") else {
                return Ok(Vec::new());
            };
            let delta_type = delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut out = Vec::new();
            match delta_type {
                "text_delta" => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            out.push(StreamDelta::Text(text.to_string()));
                        }
                    }
                }
                "thinking_delta" => {
                    if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                        if !text.is_empty() {
                            out.push(StreamDelta::Reasoning(text.to_string()));
                        }
                    }
                }
                "input_json_delta" => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        if !partial.is_empty() {
                            out.push(StreamDelta::ToolCallDelta {
                                index,
                                arguments: partial.to_string(),
                            });
                        }
                    }
                }
                // signature_delta / citations_delta 等暂无对应语义。
                _ => {}
            }
            Ok(out)
        }
        "message_delta" => {
            let mut out = Vec::new();
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                out.push(StreamDelta::Finish(FinishReason::from_anthropic(reason)));
            }
            if let Some(usage) = value.get("usage") {
                out.push(StreamDelta::Usage(parse_usage(usage)));
            }
            Ok(out)
        }
        // message_stop 的结束原因由 message_delta 给出，这里不重复。
        "message_stop" | "ping" | "content_block_stop" => Ok(Vec::new()),
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn parse_models(text: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| ProviderError::decode(format!("invalid /models payload: {e}")))?;
    let mut out = Vec::new();
    if let Some(items) = value.get("data").and_then(Value::as_array) {
        for item in items {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                out.push(ModelInfo {
                    id: id.to_string(),
                    display_name: item
                        .get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    owned_by: None,
                    created: None,
                    provider: None,
                });
            }
        }
    }
    if out.is_empty() {
        return Err(ProviderError::decode("no models found in /models payload"));
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    async fn complete_stream(&self, request: ChatRequest) -> Result<DeltaStream, ProviderError> {
        if self.base_url.trim().is_empty() {
            return Err(ProviderError::config(
                "anthropic base_url is empty (custom-anthropic needs an explicit url)",
            ));
        }
        let body = self.request_body(&request);
        let response = self
            .client
            .post(format!("{}/messages", self.v1_root()))
            .headers(self.build_headers())
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after_from_header(
                response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
            );
            let text = response.text().await.unwrap_or_default();
            let mut error = ProviderError::from_status(status.as_u16(), &text);
            if let ProviderError::RateLimited { retry_after_ms, .. } = &mut error {
                *retry_after_ms = retry_after;
            }
            return Err(error);
        }

        let bytes = response.bytes_stream();
        let stream = async_stream::try_stream! {
            pin_mut!(bytes);
            let mut parser = SseParser::new();
            let mut pending: Vec<SseEvent> = Vec::new();
            let mut done = false;
            while !done {
                match bytes.next().await {
                    Some(Ok(chunk)) => pending.extend(parser.push(&chunk)),
                    Some(Err(e)) => {
                        let failed: Result<(), ProviderError> =
                            Err(ProviderError::network(e.to_string()));
                        failed?;
                    }
                    None => {
                        if let Some(event) = parser.finish() {
                            pending.push(event);
                        }
                        done = true;
                    }
                }
                for event in pending.drain(..) {
                    for delta in parse_anthropic_event(&event)? {
                        yield delta;
                    }
                }
            }
        };
        let stream: DeltaStream = Box::pin(stream);
        Ok(stream)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        if self.base_url.trim().is_empty() {
            return Err(ProviderError::config("anthropic base_url is empty"));
        }
        let response = self
            .client
            .get(format!("{}/models", self.v1_root()))
            .headers(self.build_headers())
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ProviderError::from_reqwest(&e))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::from_status(status.as_u16(), &text));
        }
        parse_models(&text)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        extract::State,
        http::StatusCode,
        response::Response,
        routing::{get, post},
        Router,
    };
    use futures_util::StreamExt;

    use super::*;
    use crate::provider::testutil::{self, Recorder};
    use crate::provider::{MessageAssembler, ToolCall};

    #[derive(Clone)]
    struct Mock {
        rec: Recorder,
        sse: &'static str,
        status: StatusCode,
        extra_headers: &'static [(&'static str, &'static str)],
        models: &'static str,
    }

    async fn messages_handler(State(mock): State<Mock>, req: axum::extract::Request) -> Response {
        mock.rec.record(req).await;
        testutil::sse(mock.sse, mock.status, mock.extra_headers)
    }

    async fn models_handler(State(mock): State<Mock>, req: axum::extract::Request) -> Response {
        mock.rec.record(req).await;
        testutil::json(mock.models, StatusCode::OK)
    }

    async fn server(mock: Mock) -> String {
        let app = Router::new()
            .route("/v1/messages", post(messages_handler))
            .route("/v1/models", get(models_handler))
            .with_state(mock);
        testutil::spawn(app).await
    }

    fn provider(base: &str) -> AnthropicProvider {
        AnthropicProvider::new("anthropic", base)
            .with_api_key("sk-ant-test")
            .with_timeout(Duration::from_secs(10))
    }

    async fn collect(stream: DeltaStream) -> Result<Vec<StreamDelta>, ProviderError> {
        let mut stream = stream;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item?);
        }
        Ok(out)
    }

    fn mock(sse: &'static str) -> Mock {
        Mock {
            rec: Recorder::new(),
            sse,
            status: StatusCode::OK,
            extra_headers: &[],
            models: "{}",
        }
    }

    #[test]
    fn v1_root_is_normalized_for_official_and_gateway_bases() {
        assert_eq!(
            AnthropicProvider::new("anthropic", "https://api.anthropic.com").v1_root(),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            AnthropicProvider::new("opencode-anthropic", "https://opencode.ai/zen/").v1_root(),
            "https://opencode.ai/zen/v1"
        );
        assert_eq!(
            AnthropicProvider::new("custom-anthropic", "https://x/v1").v1_root(),
            "https://x/v1"
        );
    }

    #[tokio::test]
    async fn parses_text_tool_use_and_split_usage() {
        let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":100,"output_tokens":1,"cache_read_input_tokens":80,"cache_creation_input_tokens":20}}}

event: ping
data: {"type":"ping"}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"he"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"llo"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}

event: message_stop
data: {"type":"message_stop"}

"#;
        let base = format!("{}/v1", server(mock(sse)).await);
        let stream = provider(&base)
            .complete_stream(
                ChatRequest::new("claude-sonnet-4").with_messages(vec![ChatMessage::user("hi")]),
            )
            .await
            .expect("stream starts");
        let deltas = collect(stream).await.expect("stream parses");

        let mut assembler = MessageAssembler::new();
        assembler.extend(deltas);
        assert_eq!(assembler.text, "hello");
        assert_eq!(assembler.finish, Some(FinishReason::ToolCalls));
        assert_eq!(assembler.usage.input_tokens, 100);
        assert_eq!(assembler.usage.output_tokens, 42);
        assert_eq!(assembler.usage.cache_read_input_tokens, 80);
        assert_eq!(assembler.usage.cache_creation_input_tokens, 20);
        let calls = assembler.finish_into_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments_json()["path"], "a.rs");
    }

    #[tokio::test]
    async fn sends_system_tool_blocks_and_merges_user_turns() {
        let m = mock("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        let rec = m.rec.clone();
        let base = format!("{}/v1", server(m).await);

        let mut request = ChatRequest::new("claude-sonnet-4")
            .with_messages(vec![
                ChatMessage::system("sys1"),
                ChatMessage::system("sys2"),
                ChatMessage::user("hi"),
                ChatMessage::assistant_tool_calls(
                    vec![ToolCall::new("toolu_1", "read", "{\"path\":\"a.rs\"}")],
                    Some("ok".to_string()),
                ),
                ChatMessage::tool_result("toolu_1", "ok"),
                ChatMessage::user("next"),
            ])
            .with_tools(vec![ToolDef::new(
                "read",
                "read a file",
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            )]);
        request.tool_choice = Some(ToolChoice::Named("read".into()));
        request.max_tokens = Some(512);
        request.temperature = Some(0.3);
        request.stop = vec!["END".into()];
        request
            .extra
            .insert("metadata".into(), json!({"user_id": "u1"}));

        let stream = provider(&base)
            .complete_stream(request)
            .await
            .expect("stream starts");
        assert!(collect(stream).await.expect("stream parses").is_empty());

        let rec = rec.last();
        assert_eq!(rec.path, "/v1/messages");
        assert_eq!(rec.header("x-api-key"), Some("sk-ant-test"));
        assert_eq!(rec.header("anthropic-version"), Some(ANTHROPIC_VERSION));
        assert_eq!(rec.header("accept"), Some("text/event-stream"));
        assert_eq!(rec.json("/model"), &json!("claude-sonnet-4"));
        assert_eq!(rec.json("/max_tokens"), &json!(512));
        assert_eq!(rec.json("/stream"), &json!(true));
        assert_eq!(rec.json("/system"), &json!("sys1\n\nsys2"));
        assert_eq!(rec.json("/messages/0/role"), &json!("user"));
        assert_eq!(rec.json("/messages/0/content/0/text"), &json!("hi"));
        assert_eq!(rec.json("/messages/1/role"), &json!("assistant"));
        assert_eq!(rec.json("/messages/1/content/0/type"), &json!("text"));
        assert_eq!(rec.json("/messages/1/content/1/type"), &json!("tool_use"));
        assert_eq!(rec.json("/messages/1/content/1/name"), &json!("read"));
        assert_eq!(rec.json("/messages/1/content/1/input/path"), &json!("a.rs"));
        // tool_result 与紧随其后的 user 文本合并成同一个 user 轮
        assert_eq!(rec.json("/messages/2/role"), &json!("user"));
        assert_eq!(
            rec.json("/messages/2/content/0/type"),
            &json!("tool_result")
        );
        assert_eq!(
            rec.json("/messages/2/content/0/tool_use_id"),
            &json!("toolu_1")
        );
        assert_eq!(rec.json("/messages/2/content/1/text"), &json!("next"));
        assert_eq!(rec.json("/tools/0/name"), &json!("read"));
        assert_eq!(rec.json("/tools/0/input_schema/type"), &json!("object"));
        assert_eq!(rec.json("/tool_choice/type"), &json!("tool"));
        assert_eq!(rec.json("/tool_choice/name"), &json!("read"));
        assert_eq!(rec.json("/stop_sequences/0"), &json!("END"));
        assert_eq!(rec.json("/metadata/user_id"), &json!("u1"));
    }

    #[tokio::test]
    async fn tool_choice_none_omits_field() {
        let m = mock("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        let rec = m.rec.clone();
        let base = format!("{}/v1", server(m).await);

        let mut request = ChatRequest::new("claude-sonnet-4")
            .with_messages(vec![ChatMessage::user("hi")])
            .with_tools(vec![ToolDef::no_args("ls", "list files")]);
        request.tool_choice = Some(ToolChoice::None);

        let stream = provider(&base)
            .complete_stream(request)
            .await
            .expect("stream starts");
        let _ = collect(stream).await.expect("stream parses");
        assert_eq!(rec.last().json("/tool_choice"), &Value::Null);
    }

    #[tokio::test]
    async fn error_event_is_not_retryable() {
        let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let base = format!("{}/v1", server(mock(sse)).await);
        let stream = provider(&base)
            .complete_stream(ChatRequest::new("claude-sonnet-4"))
            .await
            .expect("stream starts");
        let error = match collect(stream).await {
            Ok(_) => panic!("error event must fail"),
            Err(e) => e,
        };
        assert!(!error.is_retryable());
        assert!(matches!(error, ProviderError::StreamError { .. }));
    }

    #[tokio::test]
    async fn rate_limit_is_retryable_and_auth_is_fatal() {
        let limited = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::TOO_MANY_REQUESTS,
            extra_headers: &[("retry-after", "1.5")],
            models: "{}",
        };
        let base = format!("{}/v1", server(limited).await);
        let error = provider(&base)
            .complete_stream(ChatRequest::new("claude-sonnet-4"))
            .await
            .err()
            .expect("429 must fail");
        assert!(error.is_retryable());
        assert_eq!(error.retry_after_ms(), Some(1500));

        let unauthorized = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::UNAUTHORIZED,
            extra_headers: &[],
            models: "{}",
        };
        let base = format!("{}/v1", server(unauthorized).await);
        let error = provider(&base)
            .complete_stream(ChatRequest::new("claude-sonnet-4"))
            .await
            .err()
            .expect("401 must fail");
        assert!(!error.is_retryable());
        assert!(matches!(error, ProviderError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn lists_models_with_display_name() {
        let m = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::OK,
            extra_headers: &[],
            models: r#"{"data":[{"id":"claude-sonnet-4","display_name":"Claude Sonnet 4"},{"id":"claude-sonnet-4"}]}"#,
        };
        let rec = m.rec.clone();
        let base = format!("{}/v1", server(m).await);
        let models = provider(&base).list_models().await.expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4");
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Sonnet 4"));
        assert_eq!(rec.last().path, "/v1/models");
    }

    #[tokio::test]
    async fn rejects_empty_base_url() {
        let error = AnthropicProvider::new("custom-anthropic", "")
            .complete_stream(ChatRequest::new("claude-sonnet-4"))
            .await
            .err()
            .expect("empty base_url must fail");
        assert!(matches!(error, ProviderError::Config(_)));
    }
}
