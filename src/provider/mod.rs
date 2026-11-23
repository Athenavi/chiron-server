//! Provider 协议层（B3）：公共类型 + 统一 trait + 两个适配器 + preset 目录。
//!
//! 设计口径（PRD §7.2）：27 个 preset 只有 OpenAI 兼容与 Anthropic Messages 两种
//! 协议，因此这里只写两个适配器；厂商差异全部落到 `catalog` 的能力矩阵上。
//!
//! B3 只做协议层：不依赖 axum 路由、不碰数据库。`pub` 面为 B4/B5 预留，
//! 因此在二进制 crate 里统一关掉 dead_code。
#![allow(dead_code)]

pub mod anthropic;
pub mod catalog;
pub mod failover;
pub mod openai;
pub mod sse;

#[cfg(test)]
pub(crate) mod testutil;

use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::Protocol;

/// 流式增量。适配器把两种协议的 SSE 归一到这 6 种事件上。
#[derive(Clone, Debug, PartialEq)]
pub enum StreamDelta {
    /// 正文增量。
    Text(String),
    /// 思维链增量（OpenAI `reasoning_content` / Anthropic `thinking_delta`）。
    Reasoning(String),
    /// 工具调用开始（含 id 与名字，只发一次）。
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// 工具调用参数 JSON 片段（需按 index 拼接）。
    ToolCallDelta { index: usize, arguments: String },
    /// usage 增量（两家都可能分多次给：input 在前、output 在后）。
    Usage(Usage),
    /// 结束原因。
    Finish(FinishReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// 厂商自定义值，原样保留。
    Other(String),
}

impl FinishReason {
    pub fn from_openai(raw: &str) -> Self {
        match raw {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" | "function_call" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn from_anthropic(raw: &str) -> Self {
        match raw {
            "end_turn" | "stop_sequence" => Self::Stop,
            "max_tokens" => Self::Length,
            "tool_use" => Self::ToolCalls,
            "refusal" => Self::ContentFilter,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::Other(v) => v,
        }
    }
}

/// token 用量。cache 字段按旧引擎口径（Anthropic 显式上报，OpenAI 走 cached_tokens）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// 流式里 usage 常常分两段到达（先 input 后 output），逐字段取较大值合并。
    pub fn merge(&mut self, other: &Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .max(other.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .max(other.cache_creation_input_tokens);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// 一次工具调用（参数是模型原样吐出的 JSON 字符串，避免双重转义差异）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// 参数解析失败时返回空对象，调用方仍能看到 name/id。
    pub fn arguments_json(&self) -> Value {
        if self.arguments.trim().is_empty() {
            return Value::Object(Default::default());
        }
        serde_json::from_str(&self.arguments).unwrap_or_else(|_| Value::Object(Default::default()))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// assistant 轮里模型要发起的工具调用。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// role = tool 时，回填的是哪个 tool_call。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>, content: Option<String>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// 工具声明。`parameters` 是 JSON Schema（两家都直接吃）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
}

impl ToolDef {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// 无参工具：`{"type":"object","properties":{}}`。
    pub fn no_args(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(
            name,
            description,
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named(String),
}

/// 与路由/DB 无关的纯请求体；`extra` 用于厂商私有开关（如 reasoning），
/// 适配器把它原样并进请求 JSON，不猜字段。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    /// 租户会话 ID；OpenCode Go 等网关要求用它做路由会话。
    pub session_id: Option<String>,
    /// low/medium/high；None = 不显式发送推理强度。
    pub reasoning_effort: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub extra: serde_json::Map<String, Value>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// 该模型来自哪个 preset（静态目录与远端发现合并时标注）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            owned_by: None,
            created: None,
            provider: None,
        }
    }
}

/// 静态目录 + 远端 `/models` 的合并去重（顺序：先静态、后远端新增）。
pub fn merge_models(static_ids: &[&str], remote: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = Vec::new();
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for id in static_ids {
        if seen.insert((*id).to_string(), ()).is_none() {
            out.push(ModelInfo::new(*id));
        }
    }
    for m in remote {
        if seen.insert(m.id.clone(), ()).is_none() {
            out.push(m);
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider config error: {0}")]
    Config(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("request rejected (HTTP {status}): {message}")]
    Request { status: u16, message: String },
    #[error("upstream error (HTTP {status}): {message}")]
    Upstream { status: u16, message: String },
    #[error("rate limited: {message}")]
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("decode error: {0}")]
    Decode(String),
    /// 上游在流中显式报错（OpenAI `{"error":...}` / Anthropic `error` 事件）。
    #[error("upstream stream error ({kind}): {message}")]
    StreamError { kind: String, message: String },
    #[error("cancelled")]
    Cancelled,
}

impl ProviderError {
    /// 只有"再试一次可能成功"的错误才可重试：限流、5xx、超时、连接失败。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Timeout(_) | Self::Network(_) | Self::Cancelled => {
                true
            }
            Self::Upstream { status, .. } => *status >= 500 || *status == 408 || *status == 409,
            _ => false,
        }
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// HTTP 状态 → 错误分类（两家上游的状态码语义一致）。
    pub fn from_status(status: u16, body: &str) -> Self {
        let message = truncate(body, 512);
        match status {
            401 | 403 => Self::Unauthorized(message),
            429 => Self::RateLimited {
                retry_after_ms: None,
                message,
            },
            408 => Self::Timeout(message),
            400 | 404 | 405 | 413 | 415 | 422 => Self::Request { status, message },
            s if s >= 500 => Self::Upstream { status, message },
            _ => Self::Request { status, message },
        }
    }

    pub fn from_reqwest(err: &reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout(err.to_string())
        } else if err.is_decode() {
            Self::Decode(err.to_string())
        } else {
            Self::Network(err.to_string())
        }
    }

    pub fn network(msg: impl Into<String>) -> Self {
        Self::Network(msg.into())
    }

    pub fn decode(msg: impl Into<String>) -> Self {
        Self::Decode(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub fn stream_error(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self::StreamError {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 解析 `Retry-After`（秒或 HTTP 日期都见过；只处理秒，解析不了就 None）。
pub(crate) fn retry_after_from_header(value: Option<&str>) -> Option<u64> {
    let raw = value?.trim();
    if let Ok(secs) = raw.parse::<f64>() {
        if secs >= 0.0 {
            return Some((secs * 1000.0).round() as u64);
        }
    }
    None
}

/// 流式返回：`Result<StreamDelta, ProviderError>` 的 boxed stream。
pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<StreamDelta, ProviderError>> + Send>>;

/// 适配器统一入口。路由层只依赖这个 trait 与公共类型。
#[async_trait]
pub trait Provider: Send + Sync {
    /// preset id（与 `providers::PRESETS[].id` 对齐）。
    fn id(&self) -> &str;

    /// 接入协议。
    fn protocol(&self) -> Protocol;

    /// 发起流式补全；返回的流在首个 delta 前不会阻塞调用方。
    async fn complete_stream(&self, request: ChatRequest) -> Result<DeltaStream, ProviderError>;

    /// 远端模型目录发现（`GET /models`）。
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
}

/// 把流式 delta 拼回完整消息（路由层落库 / golden 测试共用）。
#[derive(Debug, Default)]
pub struct MessageAssembler {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish: Option<FinishReason>,
    slots: BTreeMap<usize, ToolCall>,
}

impl MessageAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: StreamDelta) {
        match delta {
            StreamDelta::Text(t) => self.text.push_str(&t),
            StreamDelta::Reasoning(r) => self.reasoning.push_str(&r),
            StreamDelta::ToolCallStart { index, id, name } => {
                let slot = self.slots.entry(index).or_default();
                if !id.is_empty() {
                    slot.id = id;
                }
                if !name.is_empty() {
                    slot.name = name;
                }
            }
            StreamDelta::ToolCallDelta { index, arguments } => {
                self.slots
                    .entry(index)
                    .or_default()
                    .arguments
                    .push_str(&arguments);
            }
            StreamDelta::Usage(u) => self.usage.merge(&u),
            StreamDelta::Finish(f) => self.finish = Some(f),
        }
    }

    pub fn extend(&mut self, deltas: Vec<StreamDelta>) {
        for d in deltas {
            self.push(d);
        }
    }

    /// 按 index 排序后产出最终工具调用列表。
    pub fn finish_into_tool_calls(&mut self) -> Vec<ToolCall> {
        self.tool_calls = self.slots.values().cloned().collect();
        self.tool_calls.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_joins_split_tool_arguments_in_index_order() {
        let mut a = MessageAssembler::new();
        a.extend(vec![
            StreamDelta::Text("hi".into()),
            StreamDelta::ToolCallStart {
                index: 1,
                id: "call_b".into(),
                name: "grep".into(),
            },
            StreamDelta::ToolCallStart {
                index: 0,
                id: "call_a".into(),
                name: "read".into(),
            },
            StreamDelta::ToolCallDelta {
                index: 0,
                arguments: "{\"pa".into(),
            },
            StreamDelta::ToolCallDelta {
                index: 0,
                arguments: "th\":\"a.rs\"}".into(),
            },
            StreamDelta::Usage(Usage {
                input_tokens: 10,
                ..Default::default()
            }),
            StreamDelta::Usage(Usage {
                output_tokens: 5,
                ..Default::default()
            }),
            StreamDelta::Finish(FinishReason::ToolCalls),
        ]);
        let calls = a.finish_into_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments_json()["path"], "a.rs");
        assert_eq!(a.usage.input_tokens, 10);
        assert_eq!(a.usage.output_tokens, 5);
        assert_eq!(a.finish, Some(FinishReason::ToolCalls));
        assert_eq!(a.text, "hi");
    }

    #[test]
    fn error_classification_matches_retry_semantics() {
        assert!(ProviderError::from_status(429, "").is_retryable());
        assert!(ProviderError::from_status(503, "").is_retryable());
        assert!(ProviderError::from_status(408, "").is_retryable());
        assert!(!ProviderError::from_status(401, "").is_retryable());
        assert!(!ProviderError::from_status(400, "").is_retryable());
        assert!(!ProviderError::from_status(404, "").is_retryable());
        assert!(!ProviderError::from_status(422, "").is_retryable());
        assert!(!ProviderError::Config("x".into()).is_retryable());
    }

    #[test]
    fn retry_after_parses_seconds() {
        assert_eq!(retry_after_from_header(Some("2")), Some(2000));
        assert_eq!(retry_after_from_header(Some("0.5")), Some(500));
        assert_eq!(retry_after_from_header(Some("nonsense")), None);
        assert_eq!(retry_after_from_header(None), None);
    }

    #[test]
    fn merge_models_dedupes_static_first() {
        let merged = merge_models(
            &["gpt-5", "gpt-5-mini"],
            vec![
                ModelInfo::new("gpt-5"),
                ModelInfo::new("gpt-6"),
                ModelInfo::new("gpt-6"),
            ],
        );
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5", "gpt-5-mini", "gpt-6"]);
    }
}
