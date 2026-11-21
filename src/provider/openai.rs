//! OpenAI 兼容协议适配（`/chat/completions` 流式 + tool calls + `/models`）。
//!
//! 覆盖 PRESETS 里除 Anthropic 协议以外的全部 preset：直连、聚合网关、
//! 本地推理服务（Ollama / vLLM / LM Studio）以及自定义 OpenAI 兼容端点。
//! base_url / headers / 模型全部由调用方注入，适配器不内置厂商分支。

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{pin_mut, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, RETRY_AFTER};
use serde_json::{json, Map, Value};

use super::sse::{SseEvent, SseParser};
use super::{
    retry_after_from_header, ChatMessage, ChatRequest, DeltaStream, FinishReason, ModelInfo,
    Provider, ProviderError, Role, StreamDelta, ToolChoice, ToolDef, Usage,
};
use crate::providers::Protocol;

/// 默认单请求总超时（含流式读取整段）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// OpenAI 兼容适配器。
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    headers: Vec<(String, String)>,
    timeout: Duration,
    include_usage: bool,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            api_key: None,
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            include_usage: true,
            client: reqwest::Client::new(),
        }
    }

    /// 空 key 视为无 key（本地 Ollama 等 `requires_key = false` 的 preset）。
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.api_key = if key.trim().is_empty() {
            None
        } else {
            Some(key)
        };
        self
    }

    /// 自定义头（OpenRouter 的 `HTTP-Referer`、网关的 trace 头等）。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 有些网关不认 `stream_options`，可关掉（代价是没有 usage）。
    pub fn with_include_usage(mut self, include: bool) -> Self {
        self.include_usage = include;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // content-type 交给 `.json()`（重复设置会被网关记成两个值）
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(key) = &self.api_key {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(AUTHORIZATION, value);
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
        let mut body = Map::new();
        body.insert("model".into(), Value::String(request.model.clone()));
        body.insert("stream".into(), Value::Bool(true));
        body.insert(
            "messages".into(),
            Value::Array(request.messages.iter().map(message_to_openai).collect()),
        );
        if self.include_usage {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".into(),
                Value::Array(request.tools.iter().map(tool_to_openai).collect()),
            );
        }
        if let Some(choice) = &request.tool_choice {
            body.insert("tool_choice".into(), tool_choice_to_openai(choice));
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if let Some(max_tokens) = request.max_tokens {
            body.insert("max_tokens".into(), json!(max_tokens));
        }
        if !request.stop.is_empty() {
            body.insert("stop".into(), json!(&request.stop));
        }
        // 厂商私有开关（reasoning/effort 等）原样并入，不做猜测性映射。
        for (key, value) in &request.extra {
            body.insert(key.clone(), value.clone());
        }
        Value::Object(body)
    }
}

fn message_to_openai(message: &ChatMessage) -> Value {
    match message.role {
        Role::System | Role::User => json!({
            "role": message.role.as_str(),
            "content": message.content.as_deref().unwrap_or_default(),
        }),
        Role::Assistant => {
            let mut obj = Map::new();
            obj.insert("role".into(), json!("assistant"));
            let content = match message.content.as_deref() {
                Some(text) if !text.is_empty() => json!(text),
                _ => Value::Null,
            };
            obj.insert("content".into(), content);
            if !message.tool_calls.is_empty() {
                let calls: Vec<Value> = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        let arguments = if call.arguments.trim().is_empty() {
                            "{}"
                        } else {
                            call.arguments.as_str()
                        };
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": arguments},
                        })
                    })
                    .collect();
                obj.insert("tool_calls".into(), Value::Array(calls));
            }
            Value::Object(obj)
        }
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.as_deref().unwrap_or_default(),
            "content": message.content.as_deref().unwrap_or_default(),
        }),
    }
}

fn tool_to_openai(tool: &ToolDef) -> Value {
    let parameters = if tool.parameters.is_null() {
        json!({"type": "object", "properties": {}})
    } else {
        tool.parameters.clone()
    };
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        },
    })
}

fn tool_choice_to_openai(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

/// 把一块 SSE data 解成 delta；文本与工具通道互不阻塞。
pub(crate) fn parse_stream_chunk(value: &Value) -> Result<Vec<StreamDelta>, ProviderError> {
    let mut out = Vec::new();
    if let Some(error) = value.get("error") {
        let kind = error
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("upstream_error");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream error");
        return Err(ProviderError::stream_error(kind, message));
    }
    if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
        out.push(StreamDelta::Usage(parse_usage(usage)));
    }
    let choice = match value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        Some(choice) => choice,
        None => return Ok(out),
    };
    if let Some(delta) = choice.get("delta") {
        if let Some(text) = delta.get("content").and_then(content_text) {
            if !text.is_empty() {
                out.push(StreamDelta::Text(text));
            }
        }
        // DeepSeek 用 reasoning_content，OpenRouter 部分模型用 reasoning。
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| delta.get("reasoning").and_then(Value::as_str))
        {
            if !reasoning.is_empty() {
                out.push(StreamDelta::Reasoning(reasoning.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !name.is_empty() {
                    out.push(StreamDelta::ToolCallStart {
                        index,
                        id: id.to_string(),
                        name: name.to_string(),
                    });
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    if !arguments.is_empty() {
                        out.push(StreamDelta::ToolCallDelta {
                            index,
                            arguments: arguments.to_string(),
                        });
                    }
                }
            }
        }
    }
    if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
        out.push(StreamDelta::Finish(FinishReason::from_openai(finish)));
    }
    Ok(out)
}

/// `content` 既可能是字符串，也可能是 `[{type:"text", text:"..."}]`。
fn content_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let mut out = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    Some(out)
}

fn parse_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: 0,
    }
}

/// `/models` 两种常见形状：OpenAI `{data:[{id}]}`、Ollama `{models:[{name}]}`。
pub(crate) fn parse_models(text: &str) -> Result<Vec<ModelInfo>, ProviderError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| ProviderError::decode(format!("invalid /models payload: {e}")))?;
    let mut out: Vec<ModelInfo> = Vec::new();
    if let Some(items) = value.get("data").and_then(Value::as_array) {
        for item in items {
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                out.push(ModelInfo {
                    id: id.to_string(),
                    display_name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string()),
                    owned_by: item
                        .get("owned_by")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string()),
                    created: item.get("created").and_then(Value::as_i64),
                    provider: None,
                });
            }
        }
    } else if let Some(items) = value.get("models").and_then(Value::as_array) {
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| item.get("model").and_then(Value::as_str))
                .or_else(|| item.get("name").and_then(Value::as_str));
            if let Some(id) = id {
                out.push(ModelInfo::new(id));
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

fn sse_event_deltas(event: &SseEvent) -> Result<Vec<StreamDelta>, ProviderError> {
    if event.is_done() || event.data.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&event.data)
        .map_err(|e| ProviderError::decode(format!("invalid SSE data: {e}")))?;
    parse_stream_chunk(&value)
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn protocol(&self) -> Protocol {
        Protocol::Openai
    }

    async fn complete_stream(&self, request: ChatRequest) -> Result<DeltaStream, ProviderError> {
        if self.base_url.trim().is_empty() {
            return Err(ProviderError::config(
                "openai-compatible base_url is empty (oneapi/custom presets need an explicit url)",
            ));
        }
        let body = self.request_body(&request);
        let response = self
            .client
            .post(self.endpoint("chat/completions"))
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
                    for delta in sse_event_deltas(&event)? {
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
            return Err(ProviderError::config("openai-compatible base_url is empty"));
        }
        let response = self
            .client
            .get(self.endpoint("models"))
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

    async fn chat(State(mock): State<Mock>, req: axum::extract::Request) -> Response {
        mock.rec.record(req).await;
        testutil::sse(mock.sse, mock.status, mock.extra_headers)
    }

    async fn models(State(mock): State<Mock>, req: axum::extract::Request) -> Response {
        mock.rec.record(req).await;
        testutil::json(mock.models, StatusCode::OK)
    }

    async fn server(mock: Mock) -> String {
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/models", get(models))
            .with_state(mock);
        format!("{}/v1", testutil::spawn(app).await)
    }

    fn provider(base: &str) -> OpenAiProvider {
        OpenAiProvider::new("openai", base)
            .with_api_key("sk-test")
            .with_header("x-custom", "1")
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

    #[tokio::test]
    async fn parses_text_tool_calls_finish_and_usage() {
        let sse = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"你好"}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"pa"}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":64}}}

data: [DONE]

"#;
        let base = server(mock(sse)).await;
        let stream = provider(&base)
            .complete_stream(ChatRequest::new("gpt-5").with_messages(vec![ChatMessage::user("hi")]))
            .await
            .expect("stream starts");
        let deltas = collect(stream).await.expect("stream parses");

        let mut assembler = MessageAssembler::new();
        assembler.extend(deltas);
        assert_eq!(assembler.text, "你好");
        assert_eq!(assembler.finish, Some(FinishReason::ToolCalls));
        assert_eq!(assembler.usage.input_tokens, 120);
        assert_eq!(assembler.usage.output_tokens, 30);
        assert_eq!(assembler.usage.cache_read_input_tokens, 64);
        let calls = assembler.finish_into_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments_json()["path"], "a.rs");
    }

    #[tokio::test]
    async fn sends_tools_messages_and_custom_headers() {
        let m = mock("data: [DONE]\n\n");
        let rec = m.rec.clone();
        let base = server(m).await;

        let mut request = ChatRequest::new("gpt-5")
            .with_messages(vec![
                ChatMessage::system("sys"),
                ChatMessage::user("hi"),
                ChatMessage::assistant_tool_calls(
                    vec![ToolCall::new("call_1", "read", "{\"path\":\"a.rs\"}")],
                    None,
                ),
                ChatMessage::tool_result("call_1", "ok"),
            ])
            .with_tools(vec![ToolDef::new(
                "read",
                "read a file",
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            )]);
        request.tool_choice = Some(ToolChoice::Named("read".into()));
        request.temperature = Some(0.2);
        request.max_tokens = Some(1024);
        request.stop = vec!["</stop>".into()];
        request
            .extra
            .insert("reasoning".into(), json!({"effort": "low"}));

        let stream = provider(&base)
            .complete_stream(request)
            .await
            .expect("stream starts");
        assert!(collect(stream).await.expect("stream parses").is_empty());

        let rec = rec.last();
        assert_eq!(rec.path, "/v1/chat/completions");
        assert_eq!(rec.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(rec.header("x-custom"), Some("1"));
        assert_eq!(rec.header("content-type"), Some("application/json"));
        assert_eq!(rec.json("/model"), &json!("gpt-5"));
        assert_eq!(rec.json("/stream"), &json!(true));
        assert_eq!(rec.json("/stream_options/include_usage"), &json!(true));
        assert_eq!(rec.json("/messages/0/role"), &json!("system"));
        assert_eq!(rec.json("/messages/1/content"), &json!("hi"));
        assert_eq!(
            rec.json("/messages/2/tool_calls/0/function/name"),
            &json!("read")
        );
        assert_eq!(
            rec.json("/messages/2/tool_calls/0/function/arguments"),
            &json!("{\"path\":\"a.rs\"}")
        );
        assert_eq!(rec.json("/messages/3/role"), &json!("tool"));
        assert_eq!(rec.json("/messages/3/tool_call_id"), &json!("call_1"));
        assert_eq!(rec.json("/tools/0/function/name"), &json!("read"));
        assert_eq!(
            rec.json("/tools/0/function/parameters/type"),
            &json!("object")
        );
        assert_eq!(rec.json("/tool_choice/function/name"), &json!("read"));
        assert!((rec.json("/temperature").as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(rec.json("/max_tokens"), &json!(1024));
        assert_eq!(rec.json("/stop/0"), &json!("</stop>"));
        assert_eq!(rec.json("/reasoning/effort"), &json!("low"));
    }

    #[tokio::test]
    async fn classifies_rate_limit_as_retryable_with_retry_after() {
        let m = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::TOO_MANY_REQUESTS,
            extra_headers: &[("retry-after", "2")],
            models: "{}",
        };
        let base = server(m).await;
        let error = provider(&base)
            .complete_stream(ChatRequest::new("gpt-5"))
            .await
            .err()
            .expect("429 must fail the stream");
        assert!(error.is_retryable());
        assert_eq!(error.retry_after_ms(), Some(2000));
        assert!(matches!(error, ProviderError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn classifies_auth_failure_as_fatal() {
        let m = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::UNAUTHORIZED,
            extra_headers: &[],
            models: "{}",
        };
        let base = server(m).await;
        let error = provider(&base)
            .complete_stream(ChatRequest::new("gpt-5"))
            .await
            .err()
            .expect("401 must fail the stream");
        assert!(!error.is_retryable());
        assert!(matches!(error, ProviderError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn surfaces_upstream_error_event_from_stream() {
        let sse =
            "data: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad model\"}}\n\n";
        let base = server(mock(sse)).await;
        let stream = provider(&base)
            .complete_stream(ChatRequest::new("nope"))
            .await
            .expect("stream starts");
        let error = match collect(stream).await {
            Ok(_) => panic!("error event must fail"),
            Err(e) => e,
        };
        assert!(matches!(error, ProviderError::StreamError { .. }));
        assert!(!error.is_retryable());
    }

    #[tokio::test]
    async fn lists_and_dedupes_models() {
        let m = Mock {
            rec: Recorder::new(),
            sse: "",
            status: StatusCode::OK,
            extra_headers: &[],
            models: r#"{"data":[{"id":"gpt-5"},{"id":"gpt-5-mini"},{"id":"gpt-5"}]}"#,
        };
        let rec = m.rec.clone();
        let base = server(m).await;
        let models = provider(&base).list_models().await.expect("models");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5", "gpt-5-mini"]);
        assert_eq!(rec.last().path, "/v1/models");
        assert_eq!(rec.last().header("authorization"), Some("Bearer sk-test"));
    }

    #[tokio::test]
    async fn rejects_empty_base_url_before_hitting_network() {
        let error = OpenAiProvider::new("custom", "")
            .complete_stream(ChatRequest::new("gpt-5"))
            .await
            .err()
            .expect("empty base_url must fail");
        assert!(matches!(error, ProviderError::Config(_)));
    }
}
