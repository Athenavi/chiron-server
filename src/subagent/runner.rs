//! 子 Agent 执行器：复用同一个 provider + 工具层 + 沙箱，产物落 `subagent_runs`。
//!
//! 与主循环的关系：
//! - 子 Agent 是**独立的 `run_agent` 调用**，预算/步数/工具次数都从父额度里切；
//! - 取消联动是一条 watcher 任务：父 run 取消 → 子 control 取消（不 drop 任务，
//!   让子循环自己收敛，事件顺序与主循环一致）；
//! - 子循环的正文增量经 [`ChildSink`] 转发到父会话 SSE（`subagent.delta`）。
//!
//! 子 Agent 没有独立审批通道，因此默认只给只读工具（`SubagentConfig::allow_mutating`）。

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    tree::{self, ChildBudget, ChildSpec, SubagentError, SubagentStatus},
    Invoker, ParentContext, SubagentConfig, SubagentHost,
};
use crate::{
    agent::{
        context,
        control::{self, Cancellation, RunControl},
        error::AgentError,
        persist,
        runner::{self, ApprovalVerdict, Budget, RunDeps, RunRequest, RunSink},
        tools::{self, Policy, ToolState, Toolset},
        web::WebClient,
        workspace::Sandbox,
    },
    provider::{ChatMessage, Provider, ToolCall, Usage},
    state::AppState,
};

/// 子 Agent 一次运行的最终结果。
#[derive(Debug)]
pub struct ChildOutcome {
    pub id: Uuid,
    pub status: SubagentStatus,
    pub summary: String,
    pub usage: Usage,
}

impl ChildOutcome {
    /// 回给父模型工具结果的形状（同时是 `subagent.completed.data` 的近似）。
    pub fn as_tool_result(&self) -> Value {
        json!({
            "subagent_id": self.id,
            "status": self.status.as_str(),
            "summary": self.summary,
            "usage": {
                "input_tokens": self.usage.input_tokens,
                "output_tokens": self.usage.output_tokens,
            },
        })
    }
}

/// 子循环 → 父会话的事件出口。
struct ChildSink {
    host: Arc<dyn SubagentHost>,
    id: Uuid,
    /// `(当前 message_id, 已累积正文)`：切换消息时重置，只保留最近一条。
    buf: Mutex<(Option<Uuid>, String)>,
}

impl ChildSink {
    fn new(host: Arc<dyn SubagentHost>, id: Uuid) -> Self {
        Self {
            host,
            id,
            buf: Mutex::new((None, String::new())),
        }
    }

    fn append(&self, message_id: Uuid, text: &str) {
        if let Ok(mut guard) = self.buf.lock() {
            if guard.0 != Some(message_id) {
                guard.0 = Some(message_id);
                guard.1.clear();
            }
            guard.1.push_str(text);
            // 只用于摘要，不需要留全部历史；超长时保留尾部（结论通常在后半段）。
            let cap = tree::SUMMARY_MAX_CHARS * 2;
            if guard.1.chars().count() > cap {
                let tail: String = guard
                    .1
                    .chars()
                    .skip(guard.1.chars().count() - cap)
                    .collect();
                guard.1 = tail;
            }
        }
    }

    fn take_text(&self) -> String {
        self.buf.lock().map(|g| g.1.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl RunSink for ChildSink {
    async fn started(&self, _model: &str) -> Result<(), AgentError> {
        Ok(())
    }

    async fn delta(&self, message_id: Uuid, text: &str) -> Result<(), AgentError> {
        self.append(message_id, text);
        self.host.delta(self.id, text).await;
        Ok(())
    }

    async fn reasoning(&self, _text: &str) -> Result<(), AgentError> {
        Ok(())
    }

    async fn message_completed(
        &self,
        message_id: Uuid,
        content: &str,
        _usage: &Usage,
        _tool_calls: &[ToolCall],
    ) -> Result<(), AgentError> {
        if !content.is_empty() {
            if let Ok(mut guard) = self.buf.lock() {
                guard.0 = Some(message_id);
                guard.1 = content.to_string();
            }
        }
        Ok(())
    }

    async fn tool_call(
        &self,
        _tool_call_id: Uuid,
        name: &str,
        _args: &Value,
    ) -> Result<(), AgentError> {
        self.host
            .delta(self.id, &format!("\n[subagent] tool `{name}`"))
            .await;
        Ok(())
    }

    async fn tool_result(
        &self,
        _tool_call_id: Uuid,
        _status: &str,
        _result: Option<&Value>,
        _error: Option<&str>,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// 子 Agent 没有审批通道：任何 `ask` 都按拒绝处理（工具集里通常已无 `ask`）。
    async fn request_approval(
        &self,
        _tool_call_id: Uuid,
        _name: &str,
        _args: &Value,
    ) -> Result<ApprovalVerdict, AgentError> {
        Ok(ApprovalVerdict::Deny)
    }

    async fn usage(&self, _usage: &Usage) -> Result<(), AgentError> {
        Ok(())
    }

    async fn completed(&self, _usage: &Usage) -> Result<(), AgentError> {
        Ok(())
    }

    async fn failed(&self, _code: &str, _message: &str) -> Result<(), AgentError> {
        Ok(())
    }

    async fn cancelled(&self, _reason: &str) -> Result<(), AgentError> {
        Ok(())
    }
}

/// 嵌套子 Agent 的构造钩子：`(新 id, 新深度, 子预算 token)` → 新的调用器。
pub type NestedFactory = Arc<dyn Fn(Uuid, usize, u64) -> Arc<dyn Invoker> + Send + Sync>;

/// 子 Agent 依赖（provider/沙箱由调用方解析好，便于用 fake 回放）。
pub struct ChildDeps {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub sandbox: Arc<dyn Sandbox>,
    pub web: Arc<WebClient>,
    pub toolset: Toolset,
    pub system_prompt: String,
    pub limits: tree::Limits,
    /// 父 run 的取消令牌：取消父 = 取消子。
    pub parent_cancel: Cancellation,
}

/// 跑一个子 Agent。返回 `Err` 表示准入/落库失败（父可继续），
/// 子循环自身的失败收敛为 `ChildOutcome { status: Failed }`。
pub async fn run_child(
    host: Arc<dyn SubagentHost>,
    parent: &ParentContext,
    spec: &ChildSpec,
    deps: ChildDeps,
    nested: Option<NestedFactory>,
) -> Result<ChildOutcome, AgentError> {
    if deps.parent_cancel.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    let depth = parent.child_depth();
    let budget: ChildBudget = host.admit(depth).await.map_err(|err| match err {
        SubagentError::Cancelled => AgentError::Cancelled,
        other => AgentError::Tool(other.to_string()),
    })?;
    let id = host.started(spec, depth, budget).await?;

    // 取消联动：父取消 → 子取消。watcher 在子 run 结束后被 abort。
    let child_control = control::registry()
        .register(id)
        .unwrap_or_else(|| Arc::new(RunControl::default()));
    let watcher = {
        let parent_cancel = deps.parent_cancel.clone();
        let child_cancel = child_control.cancel.clone();
        tokio::spawn(async move {
            parent_cancel.cancelled().await;
            child_cancel.cancel();
        })
    };

    // 嵌套：只有还有深度余量时才给子 Agent 挂 `subagent` 工具。
    let subagent_invoker = nested.map(|factory| factory(id, depth, budget.max_total_tokens));
    let toolset = match &subagent_invoker {
        Some(_) => deps.toolset.clone().with(tools::SUBAGENT, Policy::Allow),
        None => deps.toolset.clone().with(tools::SUBAGENT, Policy::Deny),
    };

    let request = RunRequest {
        session_id: parent.session_id,
        run_id: id,
        model: deps.model.clone(),
        system_prompt: deps.system_prompt.clone(),
        history: vec![ChatMessage::user(spec.task.clone())],
        budget: Budget {
            max_input_tokens: budget.max_input_tokens,
            max_total_tokens: budget.max_total_tokens,
            max_steps: budget.max_steps,
            max_tool_calls: budget.max_tool_calls,
        },
        tools: toolset,
    };
    let sink = Arc::new(ChildSink::new(host.clone(), id));
    let run_deps = RunDeps {
        provider: deps.provider,
        sandbox: deps.sandbox,
        sink: sink.clone(),
        control: child_control.clone(),
        tool_state: ToolState::new(deps.web).with_subagent_opt(subagent_invoker),
    };

    let timeout = Duration::from_secs(deps.limits.timeout_secs.max(1));
    let result = tokio::time::timeout(timeout, runner::run_agent(request, run_deps)).await;
    watcher.abort();
    control::registry().finish(id);

    let (status, summary, usage, error) = match result {
        Ok(Ok(outcome)) => (
            SubagentStatus::Completed,
            tree::summarize(SubagentStatus::Completed, &outcome.text),
            outcome.usage,
            None::<String>,
        ),
        Ok(Err(AgentError::Cancelled)) => (
            SubagentStatus::Cancelled,
            tree::summarize(SubagentStatus::Cancelled, "cancelled before completion"),
            Usage::default(),
            Some("cancelled".to_string()),
        ),
        Ok(Err(err)) => (
            SubagentStatus::Failed,
            tree::summarize(SubagentStatus::Failed, &sink.take_text()),
            Usage::default(),
            Some(err.to_string()),
        ),
        Err(_elapsed) => {
            child_control.cancel.cancel();
            (
                SubagentStatus::Failed,
                tree::summarize(SubagentStatus::Failed, &sink.take_text()),
                Usage::default(),
                Some(format!("subagent timed out after {}s", timeout.as_secs())),
            )
        }
    };

    host.finished(id, status, &summary, &usage).await;
    if let Some(err) = error {
        // 错误只进日志：父模型看到的是工具结果里的 status，原因不吞掉。
        tracing::info!(%id, status = status.as_str(), error = %err, "subagent finished with error");
    }
    Ok(ChildOutcome {
        id,
        status,
        summary,
        usage,
    })
}

/// 子 Agent 的工具策略降级：`Ask` 没有审批通道，只能变 `Deny`（或显式放行）。
pub fn child_policy(base: Policy, allow_mutating: bool) -> Policy {
    match base {
        Policy::Ask if allow_mutating => Policy::Allow,
        Policy::Ask => Policy::Deny,
        other => other,
    }
}

/// 把父工具集整表降级；只读工具保持不变。
pub fn downgrade_toolset(toolset: &Toolset, allow_mutating: bool) -> Toolset {
    let mut out = toolset.clone();
    for name in tools::ALL_TOOLS {
        let next = child_policy(out.policy(name), allow_mutating);
        out = out.with(name, next);
    }
    out
}

/// 生产调用器：`agent::tools` 的 `subagent` 工具走这里。
pub struct ProductionInvoker {
    state: Arc<AppState>,
    parent: ParentContext,
    model: String,
    toolset: Toolset,
    sandbox: Arc<dyn Sandbox>,
    web: Arc<WebClient>,
    parent_cancel: Cancellation,
    config: SubagentConfig,
}

impl ProductionInvoker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Arc<AppState>,
        parent: ParentContext,
        model: String,
        toolset: Toolset,
        sandbox: Arc<dyn Sandbox>,
        web: Arc<WebClient>,
        parent_cancel: Cancellation,
        config: SubagentConfig,
    ) -> Self {
        Self {
            state,
            parent,
            model,
            toolset,
            sandbox,
            web,
            parent_cancel,
            config,
        }
    }

    /// 子 Agent 的工具集：`Ask` 一律降级（默认 Deny），保留只读工具。
    fn child_toolset(&self) -> Toolset {
        downgrade_toolset(&self.toolset, self.config.allow_mutating)
    }

    /// 嵌套调用器：子 Agent 自己也能派生（深度上限由 `admit` 兜底）。
    #[allow(clippy::too_many_arguments)]
    fn nested_factory(
        state: Arc<AppState>,
        root: ParentContext,
        toolset: Toolset,
        sandbox: Arc<dyn Sandbox>,
        web: Arc<WebClient>,
        cancel: Cancellation,
        config: SubagentConfig,
        model: String,
    ) -> NestedFactory {
        Arc::new(move |id: Uuid, depth: usize, tokens: u64| {
            let child_parent = ParentContext {
                parent_run_id: id,
                root_run_id: root.root_run_id,
                parent_subagent_id: Some(id),
                session_id: root.session_id,
                tenant_id: root.tenant_id,
                user_id: root.user_id,
                project_id: root.project_id.clone(),
                depth,
                remaining_tokens: tokens,
            };
            Arc::new(ProductionInvoker::new(
                state.clone(),
                child_parent,
                model.clone(),
                toolset.clone(),
                sandbox.clone(),
                web.clone(),
                cancel.clone(),
                config.clone(),
            )) as Arc<dyn Invoker>
        })
    }
}

#[async_trait]
impl Invoker for ProductionInvoker {
    async fn invoke(&self, args: &Value) -> Result<Value, AgentError> {
        if !self.config.enabled {
            return Err(AgentError::Tool("subagent tool is disabled".into()));
        }
        let spec = ChildSpec::from_args(args).map_err(|err| AgentError::Tool(err.to_string()))?;
        let depth = self.parent.child_depth();
        if depth > self.config.limits.max_depth {
            return Err(AgentError::Tool(
                SubagentError::DepthExceeded {
                    depth,
                    max: self.config.limits.max_depth,
                }
                .to_string(),
            ));
        }

        let requested = spec.model.clone().unwrap_or_else(|| {
            if self.config.model.is_empty() {
                self.model.clone()
            } else {
                self.config.model.clone()
            }
        });
        // 与主循环同一套编排：路由 + key 池生成候选，失败转移在首个流建立前生效；
        // 没有路由时退回解析结果。
        let resolved = persist::resolve_model(&requested, &self.state.cfg.agent.default_provider);
        let lookup = match &resolved {
            Ok((provider_id, model)) => format!("{provider_id}:{model}"),
            Err(_) => requested.clone(),
        };
        let pools = crate::tenant::current_keys();
        let routes = crate::tenant::current_routes();
        let candidates = crate::provider::failover::plan_candidates(
            &lookup,
            resolved.as_ref().ok().cloned(),
            routes.as_slice(),
            pools.as_ref(),
        );
        let Some(first) = candidates.first().cloned() else {
            return Err(match resolved {
                Err(err) => err,
                Ok((provider_id, model)) => {
                    AgentError::Provider(format!("no provider candidate for {provider_id}:{model}"))
                }
            });
        };
        let provider_id = first.provider;
        let model = first.upstream_model;

        let failover =
            crate::provider::failover::FailoverProvider::new(requested.clone(), candidates);
        failover
            .precheck()
            .map_err(|e| AgentError::Provider(format!("{provider_id}: {e}")))?;
        let provider: Arc<dyn Provider> = Arc::new(failover);

        let host: Arc<dyn SubagentHost> = Arc::new(super::store::PgHost::new(
            self.state.db(),
            self.state.cfg.subagent.clone(),
            self.parent.clone(),
            model.clone(),
        ));

        let toolset = self.child_toolset();
        let deps = ChildDeps {
            provider,
            model: model.clone(),
            sandbox: self.sandbox.clone(),
            web: self.web.clone(),
            toolset: toolset.clone(),
            system_prompt: context::system_prompt(Some(&self.state.cfg.agent.system_prompt)),
            limits: self.config.limits,
            parent_cancel: self.parent_cancel.clone(),
        };
        let nested = (depth < self.config.limits.max_depth).then(|| {
            Self::nested_factory(
                self.state.clone(),
                self.parent.clone(),
                toolset,
                self.sandbox.clone(),
                self.web.clone(),
                self.parent_cancel.clone(),
                self.config.clone(),
                model,
            )
        });

        let outcome = run_child(host, &self.parent, &spec, deps, nested).await?;
        Ok(outcome.as_tool_result())
    }
}

/// 派生 API 用：按会话/父 run 构造一个调用器（不经过父模型）。
pub fn build_session_invoker(
    state: Arc<AppState>,
    parent: ParentContext,
    model: String,
    toolset: Toolset,
) -> Arc<dyn Invoker> {
    let sandbox: Arc<dyn Sandbox> = Arc::new(crate::agent::workspace::ManagerSandbox::new(
        state.sandbox.clone(),
        parent.tenant_id,
        parent.user_id.to_string(),
        parent.project_id.clone(),
    ));
    let web = Arc::new(WebClient::new(state.cfg.agent.web_fetch_allow_private));
    let control = control::registry()
        .get(parent.parent_run_id)
        .unwrap_or_else(|| Arc::new(RunControl::default()));
    let cancel = control.cancel.clone();
    let config = state.cfg.subagent.clone();
    Arc::new(ProductionInvoker::new(
        state, parent, model, toolset, sandbox, web, cancel, config,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex as StdMutex,
    };

    use axum::{extract::State, http::StatusCode, routing::post, Router};

    use super::*;
    use crate::{agent::workspace::FakeSandbox, provider::openai::OpenAiProvider};

    #[derive(Default)]
    struct FakeHost {
        limits: tree::Limits,
        remaining: u64,
        started: StdMutex<Vec<(String, usize, u64)>>,
        deltas: StdMutex<Vec<String>>,
        finished: StdMutex<Vec<(Uuid, String, String)>>,
        active: AtomicUsize,
    }

    impl FakeHost {
        fn new(limits: tree::Limits, remaining: u64) -> Arc<Self> {
            Arc::new(Self {
                limits,
                remaining,
                active: AtomicUsize::new(0),
                ..Self::default()
            })
        }
    }

    #[async_trait]
    impl SubagentHost for FakeHost {
        async fn admit(&self, depth: usize) -> Result<ChildBudget, SubagentError> {
            tree::admit(
                self.limits,
                tree::AdmissionRequest {
                    depth,
                    active_siblings: self.active.load(Ordering::SeqCst),
                    active_tree: self.active.load(Ordering::SeqCst),
                    remaining_tokens: self.remaining,
                },
            )
        }

        async fn started(
            &self,
            spec: &ChildSpec,
            depth: usize,
            budget: ChildBudget,
        ) -> Result<Uuid, AgentError> {
            self.active.fetch_add(1, Ordering::SeqCst);
            let id = Uuid::new_v4();
            if let Ok(mut started) = self.started.lock() {
                started.push((spec.task.clone(), depth, budget.max_total_tokens));
            }
            Ok(id)
        }

        async fn delta(&self, _id: Uuid, text: &str) {
            if let Ok(mut deltas) = self.deltas.lock() {
                deltas.push(text.to_string());
            }
        }

        async fn finished(&self, id: Uuid, status: SubagentStatus, summary: &str, _usage: &Usage) {
            self.active.fetch_sub(1, Ordering::SeqCst);
            if let Ok(mut finished) = self.finished.lock() {
                finished.push((id, status.as_str().to_string(), summary.to_string()));
            }
        }
    }

    async fn chat_handler(State(body): State<String>) -> axum::response::Response {
        crate::provider::testutil::sse(&body, StatusCode::OK, &[])
    }

    async fn provider(body: String) -> Arc<dyn Provider> {
        let router = Router::new()
            .route("/chat/completions", post(chat_handler))
            .with_state(body);
        let base = crate::provider::testutil::spawn(router).await;
        Arc::new(OpenAiProvider::new("fake", &base))
    }

    fn text_sse(text: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
             data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":3,\"completion_tokens\":2}}}}\n\n\
             data: [DONE]\n\n"
        )
    }

    fn parent() -> ParentContext {
        ParentContext {
            parent_run_id: Uuid::new_v4(),
            root_run_id: Uuid::new_v4(),
            parent_subagent_id: None,
            session_id: Uuid::new_v4(),
            tenant_id: 1,
            user_id: Uuid::new_v4(),
            project_id: None,
            depth: 0,
            remaining_tokens: 40_000,
        }
    }

    fn spec() -> ChildSpec {
        ChildSpec {
            agent: "general".into(),
            task: "look around".into(),
            model: None,
        }
    }

    fn deps(
        provider: Arc<dyn Provider>,
        sandbox: Arc<FakeSandbox>,
        cancel: Cancellation,
    ) -> ChildDeps {
        ChildDeps {
            provider,
            model: "fake-model".into(),
            sandbox,
            web: Arc::new(WebClient::new(true)),
            toolset: Toolset::allow_all(),
            system_prompt: context::DEFAULT_SYSTEM_PROMPT.to_string(),
            limits: tree::Limits::default(),
            parent_cancel: cancel,
        }
    }

    #[tokio::test]
    async fn child_runs_under_parent_and_reports_summary() {
        let host = FakeHost::new(tree::Limits::default(), 40_000);
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let provider = provider(text_sse("child result")).await;

        let outcome = run_child(
            host.clone(),
            &parent(),
            &spec(),
            deps(provider, sandbox, Cancellation::new()),
            None,
        )
        .await
        .expect("child completes");

        assert_eq!(outcome.status, SubagentStatus::Completed);
        assert!(
            outcome.summary.contains("child result"),
            "{}",
            outcome.summary
        );
        assert_eq!(outcome.usage.output_tokens, 2);

        let started = host.started.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].1, 1, "子 Agent 深度是父 + 1");
        assert_eq!(started[0].2, 20_000, "子预算 = 父剩余的一半");

        let deltas = host.deltas.lock().unwrap();
        assert!(deltas.iter().any(|d| d.contains("child result")));
        let finished = host.finished.lock().unwrap();
        assert_eq!(finished[0].1, "completed");
        assert!(finished[0].2.starts_with("[completed]"));
        assert_eq!(host.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parent_cancel_prevents_child_start() {
        let host = FakeHost::new(tree::Limits::default(), 40_000);
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let provider = provider(text_sse("never")).await;
        let cancel = Cancellation::new();
        cancel.cancel();

        let err = run_child(
            host.clone(),
            &parent(),
            &spec(),
            deps(provider, sandbox, cancel),
            None,
        )
        .await
        .expect_err("cancelled");
        assert!(err.is_cancelled());
        assert!(
            host.started.lock().unwrap().is_empty(),
            "父已取消时不应落库/启动子 Agent"
        );
    }

    #[tokio::test]
    async fn budget_shortfall_rejects_before_start() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(FakeSandbox::new(dir.path().to_path_buf()));
        let provider = provider(text_sse("x")).await;

        let host = FakeHost::new(tree::Limits::default(), 1_000);
        let err = run_child(
            host.clone(),
            &parent(),
            &spec(),
            deps(provider, sandbox, Cancellation::new()),
            None,
        )
        .await
        .expect_err("budget");
        assert!(err.to_string().contains("token budget"), "{err}");
        assert!(host.started.lock().unwrap().is_empty());
        assert_eq!(err.code(), "tool.failed");
    }

    #[tokio::test]
    async fn child_toolset_denies_mutating_tools_by_default() {
        // 子 Agent 没有审批通道：`ask` 必须降级为 deny，只读工具保持放行。
        let base = Toolset::default_set();
        let strict = downgrade_toolset(&base, false);
        assert_eq!(strict.policy(tools::SHELL), Policy::Deny);
        assert_eq!(strict.policy(tools::WRITE), Policy::Deny);
        assert_eq!(strict.policy(tools::WEB_FETCH), Policy::Deny);
        assert_eq!(strict.policy(tools::READ), Policy::Allow);
        assert_eq!(strict.policy(tools::GREP), Policy::Allow);
        // 显式 deny 不会被 allow_mutating 抬回来。
        assert_eq!(strict.policy("bogus"), Policy::Deny);

        let permissive = downgrade_toolset(&base, true);
        assert_eq!(permissive.policy(tools::SHELL), Policy::Allow);
        assert_eq!(
            child_policy(Policy::Deny, true),
            Policy::Deny,
            "显式 deny 优先于 allow_mutating"
        );
        assert_eq!(child_policy(Policy::Allow, false), Policy::Allow);
    }
}
