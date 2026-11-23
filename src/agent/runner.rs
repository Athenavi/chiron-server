//! Agent 主循环：provider 流式 → 工具循环 → 回填 → 再看下一轮。
//!
//! 这一层不碰数据库也不碰 HTTP：状态落库与事件发布都通过 [`RunSink`]，
//! 因此可以用 fake provider + fake sandbox 完整回放一段 transcript。
//!
//! 取消在轮次边界与工具边界生效（PRD §7.1）；steer 在每轮开始时注入。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;
use uuid::Uuid;

use super::{
    context::{self, assemble},
    control::RunControl,
    error::AgentError,
    tools::{self, Policy, ToolState, Toolset},
    workspace::Sandbox,
};
use crate::provider::{
    ChatMessage, ChatRequest, MessageAssembler, Provider, StreamDelta, ToolCall, Usage,
};

/// 单次 run 的预算。`max_input_tokens` 用于历史裁剪；`max_total_tokens` 用于累计用量熔断。
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub max_input_tokens: u64,
    pub max_total_tokens: u64,
    pub max_steps: usize,
    pub max_tool_calls: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_input_tokens: 60_000,
            max_total_tokens: 200_000,
            max_steps: 12,
            max_tool_calls: 40,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalVerdict {
    Allow,
    Deny,
}

/// 落库 + 事件的最小接口。测试里用内存实现，生产实现见 `persist::DbSink`。
#[async_trait]
pub trait RunSink: Send + Sync {
    async fn started(&self, model: &str) -> Result<(), AgentError>;
    async fn delta(&self, message_id: Uuid, text: &str) -> Result<(), AgentError>;
    async fn reasoning(&self, text: &str) -> Result<(), AgentError>;
    async fn message_completed(
        &self,
        message_id: Uuid,
        content: &str,
        usage: &Usage,
        tool_calls: &[ToolCall],
    ) -> Result<(), AgentError>;
    async fn tool_call(
        &self,
        tool_call_id: Uuid,
        name: &str,
        args: &Value,
    ) -> Result<(), AgentError>;
    async fn tool_result(
        &self,
        tool_call_id: Uuid,
        status: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<(), AgentError>;
    async fn request_approval(
        &self,
        tool_call_id: Uuid,
        name: &str,
        args: &Value,
    ) -> Result<ApprovalVerdict, AgentError>;
    async fn usage(&self, usage: &Usage) -> Result<(), AgentError>;
    async fn completed(&self, usage: &Usage) -> Result<(), AgentError>;
    async fn failed(&self, code: &str, message: &str) -> Result<(), AgentError>;
    async fn cancelled(&self, reason: &str) -> Result<(), AgentError>;
}

pub struct RunRequest {
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub model: String,
    pub system_prompt: String,
    pub reasoning_effort: Option<String>,
    pub history: Vec<ChatMessage>,
    pub tools: Toolset,
    pub budget: Budget,
}

pub struct RunDeps {
    pub provider: Arc<dyn Provider>,
    pub sandbox: Arc<dyn Sandbox>,
    pub sink: Arc<dyn RunSink>,
    pub control: Arc<RunControl>,
    pub tool_state: ToolState,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub usage: Usage,
    pub steps: usize,
    pub text: String,
}

fn terminated(err: AgentError) -> AgentError {
    err
}

/// 跨轮的用量是相加的（provider 流的 `Usage::merge` 只处理同一轮流内分段）。
fn accumulate(total: &mut Usage, step: &Usage) {
    total.input_tokens += step.input_tokens;
    total.output_tokens += step.output_tokens;
    total.cache_read_input_tokens += step.cache_read_input_tokens;
    total.cache_creation_input_tokens += step.cache_creation_input_tokens;
}

/// 执行一次 run。返回 `Err(Cancelled)` 时调用方不应再写终态事件。
pub async fn run_agent(req: RunRequest, deps: RunDeps) -> Result<RunOutcome, AgentError> {
    let RunDeps {
        provider,
        sandbox,
        sink,
        control,
        tool_state,
    } = deps;

    sink.started(&req.model).await?;

    let mut messages = assemble(
        &req.system_prompt,
        &req.history,
        req.budget.max_input_tokens,
    );
    let tool_defs = req.tools.defs();
    let mut total = Usage::default();
    let mut tool_budget = req.budget.max_tool_calls;

    for step in 0..req.budget.max_steps.max(1) {
        if control.cancel.is_cancelled() {
            sink.cancelled("interrupted").await?;
            return Err(AgentError::Cancelled);
        }

        let steers = control.drain_steers();
        if !steers.is_empty() {
            messages.extend(context::steer_messages(&steers));
        }

        let estimated = context::estimate_messages(&messages);
        if estimated > req.budget.max_input_tokens {
            let message = format!(
                "context budget exceeded: ~{estimated} tokens > {}",
                req.budget.max_input_tokens
            );
            sink.failed("quota.exceeded", &message).await?;
            return Err(AgentError::Budget(message));
        }

        let mut chat = ChatRequest::new(req.model.clone()).with_messages(messages.clone());
        chat.session_id = Some(req.session_id.to_string());
        chat.reasoning_effort = req.reasoning_effort.clone();
        chat.tools = tool_defs.clone();
        let mut stream = match provider.complete_stream(chat).await {
            Ok(stream) => stream,
            Err(err) => {
                let err: AgentError = err.into();
                if err.is_cancelled() {
                    sink.cancelled("interrupted").await?;
                } else {
                    sink.failed(err.code(), &err.to_string()).await?;
                }
                return Err(err);
            }
        };

        let message_id = Uuid::new_v4();
        let mut assembler = MessageAssembler::new();
        loop {
            tokio::select! {
                biased;
                _ = control.cancel.cancelled() => {
                    // 停止时保留已生成的部分正文，避免前端只剩一条空回复。
                    let partial = assembler.text.trim();
                    if !partial.is_empty() {
                        sink.message_completed(message_id, &assembler.text, &assembler.usage, &[])
                            .await?;
                    }
                    sink.cancelled("interrupted").await?;
                    return Err(AgentError::Cancelled);
                }
                item = stream.next() => match item {
                    Some(Ok(delta)) => {
                        match &delta {
                            StreamDelta::Text(text) if !text.is_empty() => {
                                sink.delta(message_id, text).await?;
                            }
                            StreamDelta::Reasoning(text) if !text.is_empty() => {
                                sink.reasoning(text).await?;
                            }
                            _ => {}
                        }
                        assembler.push(delta);
                    }
                    Some(Err(err)) => {
                        let err = terminated(err.into());
                        sink.failed(err.code(), &err.to_string()).await?;
                        return Err(err);
                    }
                    None => break,
                }
            }
        }

        accumulate(&mut total, &assembler.usage);
        let mut calls = assembler.finish_into_tool_calls();
        let text = assembler.text.clone();
        sink.message_completed(message_id, &text, &assembler.usage, &calls)
            .await?;
        sink.usage(&total).await?;

        if total.total() > req.budget.max_total_tokens {
            let message = format!(
                "token budget exceeded: {} > {}",
                total.total(),
                req.budget.max_total_tokens
            );
            sink.failed("quota.exceeded", &message).await?;
            return Err(AgentError::Budget(message));
        }

        if calls.is_empty() {
            sink.completed(&total).await?;
            return Ok(RunOutcome {
                usage: total,
                steps: step + 1,
                text,
            });
        }

        // 模型有时不给 tool_call id；补一个稳定 id，保证 assistant/tool 消息成对。
        for call in calls.iter_mut() {
            if call.id.trim().is_empty() {
                call.id = format!("call_{}", Uuid::new_v4().simple());
            }
        }
        messages.push(ChatMessage::assistant_tool_calls(
            calls.clone(),
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            },
        ));

        for call in calls {
            if control.cancel.is_cancelled() {
                sink.cancelled("interrupted").await?;
                return Err(AgentError::Cancelled);
            }
            let tool_call_id = Uuid::new_v4();
            let args = call.arguments_json();
            let policy = req.tools.policy(&call.name);

            if policy == Policy::Deny {
                sink.tool_call(tool_call_id, &call.name, &args).await?;
                let error = format!("tool `{}` is denied by policy", call.name);
                sink.tool_result(tool_call_id, "cancelled", None, Some(&error))
                    .await?;
                messages.push(ChatMessage::tool_result(
                    &call.id,
                    serde_json::json!({ "error": error }).to_string(),
                ));
                continue;
            }

            if tool_budget == 0 {
                let error = "tool call budget exhausted for this run".to_string();
                sink.tool_result(tool_call_id, "failed", None, Some(&error))
                    .await?;
                messages.push(ChatMessage::tool_result(
                    &call.id,
                    serde_json::json!({ "error": error }).to_string(),
                ));
                continue;
            }
            tool_budget -= 1;

            sink.tool_call(tool_call_id, &call.name, &args).await?;

            if policy == Policy::Ask {
                match sink.request_approval(tool_call_id, &call.name, &args).await {
                    Ok(ApprovalVerdict::Allow) => {}
                    Ok(ApprovalVerdict::Deny) => {
                        let error = "tool call was rejected by the user".to_string();
                        sink.tool_result(tool_call_id, "cancelled", None, Some(&error))
                            .await?;
                        messages.push(ChatMessage::tool_result(
                            &call.id,
                            serde_json::json!({ "error": error }).to_string(),
                        ));
                        continue;
                    }
                    Err(AgentError::Cancelled) => {
                        sink.cancelled("interrupted").await?;
                        return Err(AgentError::Cancelled);
                    }
                    Err(err) => {
                        sink.failed(err.code(), &err.to_string()).await?;
                        return Err(err);
                    }
                }
            }

            match tools::execute(&tool_state, sandbox.as_ref(), &call.name, &args).await {
                Ok(result) => {
                    sink.tool_result(tool_call_id, "completed", Some(&result), None)
                        .await?;
                    messages.push(ChatMessage::tool_result(
                        &call.id,
                        serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
                    ));
                }
                Err(err) => {
                    let error = err.to_string();
                    sink.tool_result(tool_call_id, "failed", None, Some(&error))
                        .await?;
                    messages.push(ChatMessage::tool_result(
                        &call.id,
                        serde_json::json!({ "error": error }).to_string(),
                    ));
                }
            }
        }
    }

    let message = format!("agent exceeded {} steps", req.budget.max_steps);
    sink.failed("run.step_limit", &message).await?;
    Err(AgentError::Internal(message))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };

    use axum::{
        extract::{Request, State},
        http::StatusCode,
        routing::post,
        Router,
    };
    use serde_json::{json, Value};

    use super::*;
    use crate::agent::{control::Cancellation, tools::Toolset, web::WebClient, workspace::FakeSandbox};
    use crate::provider::openai::OpenAiProvider;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<(String, Value)>>,
        verdict: Mutex<Option<ApprovalVerdict>>,
        completed_tool_calls: Mutex<usize>,
        cancel_on_delta: Mutex<Option<Cancellation>>,
    }

    impl RecordingSink {
        fn new(verdict: ApprovalVerdict) -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
                verdict: Mutex::new(Some(verdict)),
                completed_tool_calls: Mutex::new(0),
                cancel_on_delta: Mutex::new(None),
            })
        }

        fn cancel_on_delta(&self, cancel: Cancellation) {
            *self.cancel_on_delta.lock().unwrap() = Some(cancel);
        }

        fn push(&self, kind: &str, data: Value) {
            self.events.lock().unwrap().push((kind.to_string(), data));
        }

        fn kinds(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .map(|(kind, _)| kind.clone())
                .collect()
        }

        fn find(&self, kind: &str) -> Vec<Value> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, data)| data.clone())
                .collect()
        }
    }

    #[async_trait]
    impl RunSink for RecordingSink {
        async fn started(&self, model: &str) -> Result<(), AgentError> {
            self.push("run.started", json!({ "model": model }));
            Ok(())
        }

        async fn delta(&self, message_id: Uuid, text: &str) -> Result<(), AgentError> {
            self.push(
                "message.delta",
                json!({ "message_id": message_id, "delta": text }),
            );
            if let Some(cancel) = self.cancel_on_delta.lock().unwrap().take() {
                cancel.cancel();
            }
            Ok(())
        }

        async fn reasoning(&self, text: &str) -> Result<(), AgentError> {
            self.push("reasoning.delta", json!({ "delta": text }));
            Ok(())
        }

        async fn message_completed(
            &self,
            message_id: Uuid,
            content: &str,
            usage: &Usage,
            tool_calls: &[ToolCall],
        ) -> Result<(), AgentError> {
            *self.completed_tool_calls.lock().unwrap() += tool_calls.len();
            self.push(
                "message.completed",
                json!({
                    "message_id": message_id,
                    "content": content,
                    "usage": usage,
                    "tool_calls": tool_calls.len(),
                }),
            );
            Ok(())
        }

        async fn tool_call(
            &self,
            tool_call_id: Uuid,
            name: &str,
            args: &Value,
        ) -> Result<(), AgentError> {
            self.push(
                "tool.call",
                json!({ "tool_call_id": tool_call_id, "name": name, "args": args }),
            );
            Ok(())
        }

        async fn tool_result(
            &self,
            tool_call_id: Uuid,
            status: &str,
            result: Option<&Value>,
            error: Option<&str>,
        ) -> Result<(), AgentError> {
            self.push(
                "tool.result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": status,
                    "result": result,
                    "error": error,
                }),
            );
            Ok(())
        }

        async fn request_approval(
            &self,
            tool_call_id: Uuid,
            name: &str,
            args: &Value,
        ) -> Result<ApprovalVerdict, AgentError> {
            self.push(
                "approval.required",
                json!({ "tool_call_id": tool_call_id, "name": name, "args": args }),
            );
            let verdict = self.verdict.lock().unwrap().expect("verdict");
            let decision = match verdict {
                ApprovalVerdict::Allow => "allow",
                ApprovalVerdict::Deny => "deny",
            };
            self.push("approval.resolved", json!({ "decision": decision }));
            Ok(verdict)
        }

        async fn usage(&self, usage: &Usage) -> Result<(), AgentError> {
            self.push("usage.updated", json!(usage));
            Ok(())
        }

        async fn completed(&self, usage: &Usage) -> Result<(), AgentError> {
            self.push("run.completed", json!({ "usage": usage }));
            Ok(())
        }

        async fn failed(&self, code: &str, message: &str) -> Result<(), AgentError> {
            self.push("run.failed", json!({ "code": code, "message": message }));
            Ok(())
        }

        async fn cancelled(&self, reason: &str) -> Result<(), AgentError> {
            self.push("run.cancelled", json!({ "reason": reason }));
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeProviderState {
        responses: Arc<Vec<String>>,
        counter: Arc<AtomicUsize>,
        bodies: Arc<Mutex<Vec<Value>>>,
    }

    async fn chat_handler(
        State(state): State<FakeProviderState>,
        req: Request,
    ) -> axum::response::Response {
        let bytes = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap_or_default();
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            state.bodies.lock().unwrap().push(value);
        }
        let index = state.counter.fetch_add(1, Ordering::SeqCst);
        let body = state
            .responses
            .get(index.min(state.responses.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        crate::provider::testutil::sse(&body, StatusCode::OK, &[])
    }

    async fn spawn_provider(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let state = FakeProviderState {
            responses: Arc::new(responses),
            counter: Arc::new(AtomicUsize::new(0)),
            bodies: bodies.clone(),
        };
        let router = Router::new()
            .route("/chat/completions", post(chat_handler))
            .with_state(state);
        let base = crate::provider::testutil::spawn(router).await;
        (base, bodies)
    }

    fn tool_call_sse(name: &str, arguments: Value) -> String {
        let chunk = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments.to_string(),
                        }
                    }]
                }
            }]
        });
        format!(
            "data: {chunk}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n\
             data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":5}}}}\n\n\
             data: [DONE]\n\n"
        )
    }

    fn text_sse(text: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
             data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":3,\"completion_tokens\":2}}}}\n\n\
             data: [DONE]\n\n"
        )
    }

    fn request() -> RunRequest {
        RunRequest {
            session_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            model: "fake-model".into(),
            system_prompt: context::DEFAULT_SYSTEM_PROMPT.to_string(),
            reasoning_effort: None,
            history: vec![ChatMessage::user("list the workspace")],
            tools: Toolset::allow_all(),
            budget: Budget::default(),
        }
    }

    fn deps(
        base: &str,
        sink: Arc<RecordingSink>,
        sandbox: Arc<FakeSandbox>,
        control: Arc<RunControl>,
    ) -> RunDeps {
        RunDeps {
            provider: Arc::new(OpenAiProvider::new("fake", base)),
            sandbox,
            sink,
            control,
            tool_state: ToolState::new(Arc::new(WebClient::new(true))),
        }
    }

    #[tokio::test]
    async fn text_tool_text_roundtrip_emits_contract_events() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("hello.txt"), "hi")
            .await
            .unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));

        let (base, bodies) = spawn_provider(vec![
            tool_call_sse("ls", json!({"path": "."})),
            text_sse("done"),
        ])
        .await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        let outcome = run_agent(
            request(),
            deps(&base, sink.clone(), sandbox.clone(), control),
        )
        .await
        .expect("run succeeds");

        assert_eq!(outcome.steps, 2);
        assert_eq!(outcome.text, "done");
        assert_eq!(outcome.usage.input_tokens, 13);
        assert_eq!(outcome.usage.output_tokens, 7);

        let kinds = sink.kinds();
        let expected = [
            "run.started",
            "message.completed",
            "tool.call",
            "tool.result",
            "message.delta",
            "message.completed",
            "run.completed",
        ];
        for kind in expected {
            assert!(
                kinds.iter().any(|k| k == kind),
                "missing {kind} in {kinds:?}"
            );
        }
        let tool_result = &sink.find("tool.result")[0];
        assert_eq!(tool_result["status"], "completed");
        assert_eq!(tool_result["result"]["entries"][0]["name"], "hello.txt");

        // 第二轮请求里应带上 assistant tool_calls 与 tool 回填。
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let roles: Vec<&str> = bodies[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert!(roles.contains(&"tool"), "roles: {roles:?}");
        assert_eq!(
            bodies[1]["messages"].as_array().unwrap().last().unwrap()["tool_call_id"],
            "call_1"
        );
    }

    #[tokio::test]
    async fn approval_denial_blocks_execution_and_feedbacks_error() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, bodies) = spawn_provider(vec![
            tool_call_sse("write", json!({"path": "x.txt", "content": "boom"})),
            text_sse("skipped"),
        ])
        .await;

        let sink = RecordingSink::new(ApprovalVerdict::Deny);
        let control = Arc::new(RunControl::default());
        let mut req = request();
        req.tools = Toolset::allow_all().with(tools::WRITE, Policy::Ask);
        let outcome = run_agent(req, deps(&base, sink.clone(), sandbox.clone(), control))
            .await
            .expect("run completes after denial");
        assert_eq!(outcome.steps, 2);

        let result = &sink.find("tool.result")[0];
        assert_eq!(result["status"], "cancelled");
        assert!(result["error"].as_str().unwrap().contains("rejected"));
        assert!(
            !dir.path().join("x.txt").exists(),
            "denied tool must not write"
        );
        assert_eq!(sink.find("approval.required").len(), 1);

        let bodies = bodies.lock().unwrap();
        let last = &bodies[1]["messages"].as_array().unwrap().last().unwrap();
        assert!(last["content"].as_str().unwrap().contains("rejected"));
        assert_eq!(last["role"], "tool");
    }

    #[tokio::test]
    async fn cancellation_mid_stream_keeps_partial_text() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, _) = spawn_provider(vec![text_sse("partial answer")]).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        // 收到首段正文后取消，模拟用户在生成中途点停止。
        sink.cancel_on_delta(control.cancel.clone());

        let err = run_agent(request(), deps(&base, sink.clone(), sandbox, control))
            .await
            .expect_err("cancelled");
        assert!(err.is_cancelled());

        let completed = sink.find("message.completed");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["content"], "partial answer");
        let kinds = sink.kinds();
        let completed_at = kinds.iter().position(|k| k == "message.completed").unwrap();
        let cancelled_at = kinds.iter().position(|k| k == "run.cancelled").unwrap();
        assert!(completed_at < cancelled_at, "kinds: {kinds:?}");
        assert!(!kinds.contains(&"run.completed".to_string()));
    }

    #[tokio::test]
    async fn cancellation_before_start_reports_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, _) = spawn_provider(vec![text_sse("never")]).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        control.cancel.cancel();
        let err = run_agent(request(), deps(&base, sink.clone(), sandbox, control))
            .await
            .expect_err("cancelled");
        assert!(err.is_cancelled());
        assert!(sink.kinds().contains(&"run.cancelled".to_string()));
        assert!(!sink.kinds().contains(&"run.completed".to_string()));
    }

    #[tokio::test]
    async fn step_limit_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, _) = spawn_provider(vec![tool_call_sse("ls", json!({}))]).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        let mut req = request();
        req.budget.max_steps = 2;
        req.budget.max_tool_calls = 10;
        let err = run_agent(req, deps(&base, sink.clone(), sandbox, control))
            .await
            .expect_err("step limit");
        assert!(matches!(err, AgentError::Internal(_)));
        let failed = sink.find("run.failed");
        assert_eq!(failed[0]["code"], "run.step_limit");
    }

    #[tokio::test]
    async fn token_budget_is_enforced_from_provider_usage() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, _) = spawn_provider(vec![tool_call_sse("ls", json!({}))]).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        let mut req = request();
        req.budget.max_total_tokens = 10;
        let err = run_agent(req, deps(&base, sink.clone(), sandbox, control))
            .await
            .expect_err("budget");
        assert!(matches!(err, AgentError::Budget(_)));
        assert_eq!(sink.find("run.failed")[0]["code"], "quota.exceeded");
    }

    #[tokio::test]
    async fn steer_is_injected_into_next_round() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, bodies) = spawn_provider(vec![text_sse("ok")]).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        control.push_steer("keep it to one line");
        run_agent(request(), deps(&base, sink, sandbox, control))
            .await
            .expect("run");

        let bodies = bodies.lock().unwrap();
        let joined = serde_json::to_string(&bodies[0]["messages"]).unwrap();
        assert!(joined.contains("[steer]"));
        assert!(joined.contains("keep it to one line"));
    }

    #[tokio::test]
    async fn denied_tool_policy_is_not_executed() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let (base, _) = spawn_provider(vec![
            tool_call_sse("shell", json!({"command": "rm -rf /"})),
            text_sse("ok"),
        ])
        .await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        let mut req = request();
        req.tools = Toolset::allow_all().with(tools::SHELL, Policy::Deny);
        run_agent(req, deps(&base, sink.clone(), sandbox.clone(), control))
            .await
            .expect("run completes");
        assert!(sandbox.calls().is_empty(), "denied shell must not run");
        let result = &sink.find("tool.result")[0];
        assert_eq!(result["status"], "cancelled");
        assert!(!sink.kinds().contains(&"approval.required".to_string()));
    }

    #[tokio::test]
    async fn provider_error_fails_run_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let router = Router::new().route(
            "/chat/completions",
            post(|| async {
                crate::provider::testutil::json(
                    "{\"error\":{\"message\":\"nope\"}}",
                    StatusCode::BAD_REQUEST,
                )
            }),
        );
        let base = crate::provider::testutil::spawn(router).await;

        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        let control = Arc::new(RunControl::default());
        let err = run_agent(request(), deps(&base, sink.clone(), sandbox, control))
            .await
            .expect_err("provider failure");
        assert!(matches!(err, AgentError::Provider(_)));
        assert_eq!(sink.find("run.failed")[0]["code"], "provider.failed");
    }

    #[test]
    fn budget_defaults_are_sane() {
        let budget = Budget::default();
        assert!(budget.max_input_tokens < budget.max_total_tokens);
        assert!(budget.max_steps >= 8);
        assert!(budget.max_tool_calls >= 20);
    }

    // 保证测试引用的辅助结构没有被优化掉。
    #[test]
    fn recorded_events_preserve_submission_order() {
        let sink = RecordingSink::new(ApprovalVerdict::Allow);
        sink.push("a", Value::Null);
        sink.push("b", Value::Null);
        assert_eq!(sink.kinds(), vec!["a".to_string(), "b".to_string()]);
        let _ = BTreeMap::<String, ()>::new();
    }
}
