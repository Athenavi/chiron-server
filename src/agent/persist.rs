//! 落库与事件：把主循环的 `RunSink` 落到 PG + `sessions::events`。
//!
//! 顺序约定（PRD §7.1）：所有状态变更先写 PG，再发事件；终态更新只对活跃 run 生效，
//! 因此"用户先中断、循环后收尾"不会把状态改回去，也不会重复发终态事件。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    context::{self, content_to_text},
    control::{self, RunControl},
    error::AgentError,
    runner::{run_agent, ApprovalVerdict, Budget, RunDeps, RunRequest, RunSink},
    tools::{ToolState, Toolset},
    web::WebClient,
    workspace::ManagerSandbox,
};
use crate::{
    provider::{ChatMessage, Role, ToolCall, Usage},
    providers,
    sessions::{event_log, ConversationSettings},
    state::AppState,
};

/// 与迁移里的部分唯一索引同口径（`sessions::run_state` 的常量不跨模块导出）。
const ACTIVE: &str = "'queued', 'running', 'waiting_approval', 'waiting_user'";
const HISTORY_LIMIT: i64 = 200;

type SessionRow = (i16, Uuid, Option<String>, Option<String>, String, Value);

/// 启动一次 run：注册进程内句柄并 spawn 循环。重复启动直接忽略。
///
/// B11：启动前先拿跨实例租约（Redis `SET NX PX`，PG CAS 兜底）。拿不到说明
/// 另一个实例已经在跑这个 run，本实例只登记、不执行——SSE 仍可服务该会话。
pub fn start_run(state: Arc<AppState>, db: sqlx::PgPool, run_id: Uuid, session_id: Uuid) {
    let Some(control) = control::registry().register(run_id) else {
        tracing::warn!(%run_id, "run already registered; skip duplicate start");
        return;
    };
    // 脱手的任务不继承请求的 task-local：显式把租户库/BYOK 池/模型路由/tier 带进 spawn。
    let keys = crate::tenant::current_keys();
    let routes = crate::tenant::current_routes();
    let tier = crate::tenant::current_tier().unwrap_or_default();
    let egress = crate::tenant::current_egress().unwrap_or_default();
    tokio::spawn(crate::tenant::scope_with(
        db,
        keys,
        routes,
        tier,
        egress,
        crate::tenant::current_tenant().unwrap_or_default(),
        async move {
            let owner = crate::sessions::lease::owner_id();
            let ttl = std::time::Duration::from_secs(state.cfg.multi.lease_ttl_secs.max(5));
            match crate::sessions::lease::acquire_run(&state, run_id, &owner, ttl).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(%run_id, "run lease held elsewhere; not starting here");
                    control::registry().finish(run_id);
                    return;
                }
                Err(err) => {
                    // 租约后端不可用：不阻塞单节点执行，PG 侧（唯一索引 + DbSink CAS）兜底。
                    tracing::warn!(%run_id, error = %err, "run lease backend unavailable");
                }
            }

            let renew = {
                let state = state.clone();
                let owner = owner.clone();
                tokio::spawn(async move {
                    let every = (ttl / 3).max(std::time::Duration::from_secs(1));
                    loop {
                        tokio::time::sleep(every).await;
                        if !crate::sessions::lease::renew_run(&state, run_id, &owner, ttl).await {
                            tracing::warn!(%run_id, "run lease lost; stopping to avoid duplicate execution");
                            crate::agent::registry().cancel(run_id);
                            break;
                        }
                    }
                })
            };

            if let Err(err) = execute(state.clone(), run_id, session_id, control).await {
                if err.is_cancelled() {
                    tracing::info!(%run_id, "run cancelled");
                } else {
                    tracing::warn!(%run_id, code = err.code(), error = %err, "run ended with error");
                }
            }
            renew.abort();
            crate::sessions::lease::release_run(&state, run_id, &owner).await;
            control::registry().finish(run_id);
        },
    ));
}

async fn execute(
    state: Arc<AppState>,
    run_id: Uuid,
    session_id: Uuid,
    control: Arc<RunControl>,
) -> Result<(), AgentError> {
    let run: Option<(Uuid, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, turn_id, status FROM runs WHERE id = $1 AND session_id = $2")
            .bind(run_id)
            .bind(session_id)
            .fetch_optional(&state.db())
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
    let Some((_, turn_id, status)) = run else {
        tracing::warn!(%run_id, "run disappeared before start");
        return Ok(());
    };
    if !matches!(
        status.as_str(),
        "queued" | "running" | "waiting_approval" | "waiting_user"
    ) {
        tracing::info!(%run_id, %status, "run is already terminal; nothing to do");
        return Ok(());
    }

    let session: Option<SessionRow> = sqlx::query_as(
        "SELECT tenant_id, user_id, project_id, model, status, metadata FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.db())
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    let Some((tenant_id, user_id, project_id, session_model, session_status, metadata)) = session
    else {
        tracing::warn!(%run_id, "session missing");
        return Ok(());
    };
    if session_status == "deleted" {
        tracing::info!(%run_id, "session deleted; skip run");
        return Ok(());
    }

    let turn_model: Option<String> = match turn_id {
        Some(turn) => sqlx::query_scalar("SELECT model FROM turns WHERE id = $1")
            .bind(turn)
            .fetch_optional(&state.db())
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?
            .flatten(),
        None => None,
    };

    let sink = Arc::new(DbSink {
        db: state.db(),
        session_id,
        run_id,
        turn_id,
        control: control.clone(),
        approval_timeout_secs: state.cfg.agent.approval_timeout_secs,
        lease_secs: state.cfg.checkpoint.lease_secs,
    });

    let requested_model = turn_model
        .or(session_model)
        .unwrap_or_else(|| state.cfg.agent.default_model.clone());
    // 运行时编排：先按既有规则解析 `<provider>:<model>`，再用租户路由 + key 池生成
    // 候选列表交给 `FailoverProvider`（首个流建立前逐个尝试）。没有路由时退回解析
    // 结果，行为与升级前一致；解析失败但原始名有路由时也能命中。
    let resolved = resolve_model(&requested_model, &state.cfg.agent.default_provider);
    let lookup = match &resolved {
        Ok((provider_id, model)) => format!("{provider_id}:{model}"),
        Err(_) => requested_model.clone(),
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
        // 解析失败且没有任何路由：沿用原错误，不猜一个端点把请求发出去。
        let err = match resolved {
            Err(err) => err,
            Ok((provider_id, model)) => {
                AgentError::Provider(format!("no provider candidate for {provider_id}:{model}"))
            }
        };
        sink.failed(err.code(), &err.to_string()).await?;
        return Err(err);
    };
    let provider_id = first.provider;
    let model = first.upstream_model;

    let failover =
        crate::provider::failover::FailoverProvider::new(requested_model.clone(), candidates);
    // 早失败：所有候选的 provider 配置都不可构建（缺 key / 缺 base_url）时不进历史加载。
    if let Err(err) = failover.precheck() {
        let err = AgentError::Provider(format!("{provider_id}: {err}"));
        sink.failed(err.code(), &err.to_string()).await?;
        return Err(err);
    }
    let provider: Arc<dyn crate::provider::Provider> = Arc::new(failover);

    let history = match load_history(&state.db(), session_id).await {
        Ok(history) => history,
        Err(err) => {
            sink.failed(err.code(), &err.to_string()).await?;
            return Err(err);
        }
    };

    let sandbox = Arc::new(ManagerSandbox::new(
        state.sandbox.clone(),
        tenant_id,
        user_id.to_string(),
        project_id.clone(),
    ));
    let web = Arc::new(WebClient::new(state.cfg.agent.web_fetch_allow_private));

    // B6：父 run 的上下文。子 Agent 从这里继承模型/沙箱与剩余预算额度。
    let parent = crate::subagent::ParentContext {
        parent_run_id: run_id,
        root_run_id: run_id,
        parent_subagent_id: None,
        session_id,
        tenant_id,
        user_id,
        project_id,
        depth: 0,
        remaining_tokens: state.cfg.agent.max_total_tokens,
    };
    let toolset = {
        let mut set = Toolset::from_env();
        let settings = ConversationSettings::from_metadata(&metadata);
        match settings.tool_authorization.as_str() {
            "auto" => {
                set.set_mutating(crate::agent::tools::Policy::Allow);
                set = set.with(crate::agent::tools::SUBAGENT, crate::agent::tools::Policy::Allow);
            }
            "readonly" => {
                set.set_mutating(crate::agent::tools::Policy::Deny);
                set = set.with(crate::agent::tools::SUBAGENT, crate::agent::tools::Policy::Deny);
            }
            _ => {
                set.set_mutating(crate::agent::tools::Policy::Ask);
                set = set.with(crate::agent::tools::SUBAGENT, crate::agent::tools::Policy::Ask);
            }
        }
        set
    };
    let conversation = ConversationSettings::from_metadata(&metadata);
    let prompt_suffix = conversation.prompt_suffix();
    let deployment_prompt = if prompt_suffix.is_empty() {
        state.cfg.agent.system_prompt.clone()
    } else if state.cfg.agent.system_prompt.trim().is_empty() {
        prompt_suffix
    } else {
        format!("{}\n\n{}", state.cfg.agent.system_prompt, prompt_suffix)
    };
    let reasoning_effort = (conversation.reasoning_effort != "auto")
        .then_some(conversation.reasoning_effort.clone());
    let subagent_invoker = crate::subagent::runner::build_session_invoker(
        state.clone(),
        parent,
        model.clone(),
        toolset.clone(),
    );

    // B8：跨会话内存按预算注入系统提示；内存不可用时静默降级为空。
    let memory = if state.cfg.memory.enabled {
        let budget = context::memory_budget(state.cfg.agent.max_input_tokens)
            .min(state.cfg.memory.inject_tokens);
        crate::memory::load_prompt(&state.db(), tenant_id, user_id, session_id, budget).await
    } else {
        String::new()
    };

    let request = RunRequest {
        session_id,
        run_id,
        model,
        system_prompt: context::system_prompt_with_memory(
            Some(&deployment_prompt),
            Some(&memory),
        ),
        reasoning_effort,
        history,
        tools: toolset,
        budget: Budget {
            max_input_tokens: state.cfg.agent.max_input_tokens,
            max_total_tokens: state.cfg.agent.max_total_tokens,
            max_steps: state.cfg.agent.max_steps,
            max_tool_calls: state.cfg.agent.max_tool_calls,
        },
    };
    let deps = RunDeps {
        provider,
        sandbox,
        sink,
        control,
        tool_state: ToolState::new(web).with_subagent(subagent_invoker),
    };

    let outcome = run_agent(request, deps).await;
    if matches!(outcome, Err(ref err) if err.is_cancelled()) {
        // 取消联动（PRD §7.5）：父被取消时整棵子 Agent 子树一起停。
        cancel_children(&state, run_id, "parent_interrupted").await;
    }
    if outcome.is_ok() {
        // B7：run 结束抓一份快照（消息 + 工具状态 + 预算游标 + 工作区 git）。
        if let Err(err) = crate::checkpoint::store::capture(&state, run_id, "run.completed").await {
            tracing::warn!(%run_id, error = %err, "checkpoint capture failed");
        }
    }
    outcome.map(|_| ())
}

/// 父 run 被取消/会话删除时联动取消子 Agent：先落库（终态粘滞），再通知本实例的循环。
pub async fn cancel_children(state: &AppState, parent_run_id: Uuid, reason: &str) {
    match crate::subagent::store::cancel_for_run(&state.db(), parent_run_id, reason).await {
        Ok(ids) => {
            for id in ids {
                crate::agent::registry().cancel(id);
            }
        }
        Err(err) => tracing::warn!(%parent_run_id, error = %err, "cancel subagents failed"),
    }
}

/// 历史消息 → provider 消息。`tool_result` 行只在 run 内配对，重放时跳过；
/// assistant 的 tool_calls 不回放（避免新一轮出现无配对的 tool 消息）。
pub async fn load_history(db: &PgPool, session_id: Uuid) -> Result<Vec<ChatMessage>, AgentError> {
    let rows: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT role, kind, content FROM messages \
         WHERE session_id = $1 AND kind <> 'tool_result' \
         ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(session_id)
    .bind(HISTORY_LIMIT)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;

    let mut messages = Vec::with_capacity(rows.len());
    for (role, _kind, content) in rows.into_iter().rev() {
        let text = content_to_text(&content);
        if text.trim().is_empty() {
            continue;
        }
        let message = match context::role_of(&role) {
            Some(Role::System) => ChatMessage::system(text),
            Some(Role::User) => ChatMessage::user(text),
            Some(Role::Assistant) => ChatMessage::assistant(text),
            // tool 行已被 SQL 过滤；未知 role 当用户输入处理更有信息量。
            Some(Role::Tool) | None => ChatMessage::user(text),
        };
        messages.push(message);
    }
    Ok(messages)
}

/// `<provider>:<model>` 或按 preset 前缀推断 provider。
///
/// 显式 provider 优先级最高；否则用部署默认 provider，再退化为前缀推断。
/// 推断不出来就明确失败——不猜一个端点把请求发出去。
pub fn resolve_model(model: &str, default_provider: &str) -> Result<(String, String), AgentError> {
    let raw = model.trim();
    if raw.is_empty() {
        return Err(AgentError::Provider(
            "no model configured (set CHIRON_DEFAULT_MODEL or session model)".into(),
        ));
    }

    let explicit = raw.split_once(':').filter(|(provider, name)| {
        !name.trim().is_empty() && providers::find(provider.trim()).is_some()
    });
    if let Some((provider, name)) = explicit {
        return finish_resolution(provider.trim().to_string(), name.trim().to_string());
    }

    // 直接写 preset id（如 `ollama`）：模型名就用 id 本身，端点由 preset 决定。
    if providers::find(raw).is_some() {
        return finish_resolution(raw.to_string(), raw.to_string());
    }

    if let Some(preset) = providers::find(default_provider.trim()) {
        if model_matches(preset, raw) {
            return finish_resolution(preset.id.to_string(), raw.to_string());
        }
        return Err(AgentError::Provider(format!(
            "model `{raw}` does not belong to provider `{}`",
            preset.id
        )));
    }

    for preset in providers::PRESETS {
        if !preset.model_prefixes.is_empty() && model_matches(preset, raw) {
            return finish_resolution(preset.id.to_string(), raw.to_string());
        }
    }
    let family = providers::model_family(raw);
    if !family.is_empty() && providers::find(family).is_some() {
        return finish_resolution(family.to_string(), raw.to_string());
    }

    Err(AgentError::Provider(format!(
        "unknown model `{raw}`; set CHIRON_DEFAULT_MODEL or use `<provider>:<model>`"
    )))
}

fn model_matches(preset: &providers::Preset, model: &str) -> bool {
    if preset.model_prefixes.is_empty() {
        return true;
    }
    let lower = model.to_ascii_lowercase();
    preset
        .model_prefixes
        .iter()
        .any(|prefix| lower.starts_with(&prefix.to_ascii_lowercase()))
}

fn finish_resolution(provider: String, model: String) -> Result<(String, String), AgentError> {
    crate::provider::catalog::check_plan_with(&provider, &model, &[])
        .map_err(|e| AgentError::Provider(e.to_string()))?;
    Ok((provider, model))
}

struct DbSink {
    db: PgPool,
    session_id: Uuid,
    run_id: Uuid,
    turn_id: Option<Uuid>,
    control: Arc<RunControl>,
    approval_timeout_secs: u64,
    /// B7：本实例持有租约的时长；每轮 usage 续租，终态释放。
    lease_secs: u64,
}

impl DbSink {
    fn publish(&self, kind: &str, data: Value) {
        event_log::global().append(self.session_id, Some(self.run_id), kind, data);
    }

    fn lease_owner(&self) -> String {
        crate::checkpoint::reconciler::lease_owner()
    }
}

#[async_trait]
impl RunSink for DbSink {
    async fn started(&self, model: &str) -> Result<(), AgentError> {
        sqlx::query(&format!(
            "UPDATE runs SET status = 'running', started_at = COALESCE(started_at, now()), \
                 lease_owner = $2, \
                 lease_until = now() + make_interval(secs => $3::int), \
                 updated_at = now() WHERE id = $1 AND status IN ({ACTIVE})"
        ))
        .bind(self.run_id)
        .bind(self.lease_owner())
        .bind(self.lease_secs.min(i32::MAX as u64) as i32)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        if let Some(turn_id) = self.turn_id {
            sqlx::query(
                "UPDATE turns SET status = 'running', started_at = COALESCE(started_at, now()) \
                 WHERE id = $1 AND status IN ('queued', 'running')",
            )
            .bind(turn_id)
            .execute(&self.db)
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        }
        self.publish(
            "run.started",
            json!({ "model": model, "turn_id": self.turn_id }),
        );
        Ok(())
    }

    async fn delta(&self, message_id: Uuid, text: &str) -> Result<(), AgentError> {
        self.publish(
            "message.delta",
            json!({ "message_id": message_id, "delta": text }),
        );
        Ok(())
    }

    async fn reasoning(&self, text: &str) -> Result<(), AgentError> {
        self.publish("reasoning.delta", json!({ "delta": text }));
        Ok(())
    }

    async fn message_completed(
        &self,
        message_id: Uuid,
        content: &str,
        usage: &Usage,
        tool_calls: &[ToolCall],
    ) -> Result<(), AgentError> {
        let payload = if tool_calls.is_empty() {
            json!(content)
        } else {
            json!({
                "text": content,
                "tool_calls": tool_calls.iter().map(|c| json!({
                    "id": c.id,
                    "name": c.name,
                    "arguments": c.arguments,
                })).collect::<Vec<_>>(),
            })
        };
        sqlx::query(
            "INSERT INTO messages (id, session_id, turn_id, run_id, role, kind, content) \
             VALUES ($1, $2, $3, $4, 'assistant', 'chat', $5) ON CONFLICT (id) DO NOTHING",
        )
        .bind(message_id)
        .bind(self.session_id)
        .bind(self.turn_id)
        .bind(self.run_id)
        .bind(payload)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        self.publish(
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
        sqlx::query(
            "INSERT INTO tool_calls (id, session_id, run_id, name, args, status) \
             VALUES ($1, $2, $3, $4, $5, 'running') \
             ON CONFLICT (id) DO UPDATE SET status = 'running', updated_at = now()",
        )
        .bind(tool_call_id)
        .bind(self.session_id)
        .bind(self.run_id)
        .bind(name)
        .bind(args.clone())
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        self.publish(
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
        sqlx::query(
            "UPDATE tool_calls SET status = $2, result = $3, error = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(tool_call_id)
        .bind(status)
        .bind(result.cloned())
        .bind(error)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        self.publish(
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

    /// `ask` 策略：落 pending 审批、run 进入 `waiting_approval`，然后等唤醒。
    /// 中断（取消令牌）与超时都会收敛，不会把 run 永久挂在等待态。
    async fn request_approval(
        &self,
        tool_call_id: Uuid,
        name: &str,
        args: &Value,
    ) -> Result<ApprovalVerdict, AgentError> {
        let approval_id = Uuid::new_v4();
        let timeout = ChronoDuration::seconds(self.approval_timeout_secs.max(1) as i64);
        let expires_at = Utc::now() + timeout;

        sqlx::query(
            "INSERT INTO approvals (id, session_id, run_id, tool_call_id, status, payload, expires_at) \
             VALUES ($1, $2, $3, $4, 'pending', $5, $6)",
        )
        .bind(approval_id)
        .bind(self.session_id)
        .bind(self.run_id)
        .bind(tool_call_id)
        .bind(json!({ "name": name, "args": args }))
        .bind(expires_at)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'awaiting_approval', updated_at = now() WHERE id = $1",
        )
        .bind(tool_call_id)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        sqlx::query(&format!(
            "UPDATE runs SET status = 'waiting_approval', updated_at = now() \
             WHERE id = $1 AND status IN ({ACTIVE})"
        ))
        .bind(self.run_id)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;

        self.publish(
            "approval.required",
            json!({
                "approval_id": approval_id,
                "tool_call_id": tool_call_id,
                "name": name,
                "args": args,
                "expires_at": expires_at,
            }),
        );

        loop {
            let remaining = expires_at.signed_duration_since(Utc::now());
            if remaining <= ChronoDuration::zero() {
                self.expire_approval(approval_id, tool_call_id).await?;
                return Err(AgentError::Tool(format!("approval for `{name}` timed out")));
            }
            let sleep = tokio::time::sleep(remaining.to_std().unwrap_or_default());
            tokio::pin!(sleep);
            tokio::select! {
                biased;
                _ = self.control.cancel.cancelled() => {
                    sqlx::query(
                        "UPDATE approvals SET status = 'cancelled', decided_at = now() \
                         WHERE id = $1 AND status = 'pending'",
                    )
                    .bind(approval_id)
                    .execute(&self.db)
                    .await
                    .map_err(|e| AgentError::Internal(e.to_string()))?;
                    return Err(AgentError::Cancelled);
                }
                _ = self.control.wait_wake() => {}
                _ = &mut sleep => {
                    self.expire_approval(approval_id, tool_call_id).await?;
                    return Err(AgentError::Tool(format!(
                        "approval for `{name}` timed out"
                    )));
                }
            }

            let row: Option<(String, Option<String>)> =
                sqlx::query_as("SELECT status, decision FROM approvals WHERE id = $1")
                    .bind(approval_id)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(|e| AgentError::Internal(e.to_string()))?;
            let Some((status, decision)) = row else {
                return Err(AgentError::Internal("approval row disappeared".into()));
            };
            match (status.as_str(), decision.as_deref()) {
                ("resolved", Some("allow")) => {
                    self.resume_after_approval(tool_call_id, "running").await?;
                    return Ok(ApprovalVerdict::Allow);
                }
                ("resolved", _) => {
                    self.resume_after_approval(tool_call_id, "cancelled")
                        .await?;
                    return Ok(ApprovalVerdict::Deny);
                }
                ("cancelled" | "expired", _) => return Err(AgentError::Cancelled),
                _ => continue,
            }
        }
    }

    async fn usage(&self, usage: &Usage) -> Result<(), AgentError> {
        let usage_json = serde_json::to_value(usage).unwrap_or_else(|_| json!({}));
        // 每轮续租：reconciler 因此不会把"还在跑的 run"当成僵尸接管。
        sqlx::query(
            "UPDATE runs SET usage = $2, \
                 lease_until = now() + make_interval(secs => $3::int), updated_at = now() \
             WHERE id = $1",
        )
        .bind(self.run_id)
        .bind(usage_json)
        .bind(self.lease_secs.min(i32::MAX as u64) as i32)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        if let Some(turn_id) = self.turn_id {
            sqlx::query("UPDATE turns SET usage = $2 WHERE id = $1")
                .bind(turn_id)
                .bind(serde_json::to_value(usage).unwrap_or_else(|_| json!({})))
                .execute(&self.db)
                .await
                .map_err(|e| AgentError::Internal(e.to_string()))?;
        }
        self.publish(
            "usage.updated",
            json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost": 0.0,
            }),
        );
        Ok(())
    }

    async fn completed(&self, usage: &Usage) -> Result<(), AgentError> {
        let usage_json = serde_json::to_value(usage).unwrap_or_else(|_| json!({}));
        let affected = sqlx::query(&format!(
            "UPDATE runs SET status = 'completed', usage = $2, finished_at = now(), \
                 lease_owner = NULL, lease_until = NULL, \
                 updated_at = now() WHERE id = $1 AND status IN ({ACTIVE})"
        ))
        .bind(self.run_id)
        .bind(usage_json.clone())
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?
        .rows_affected();
        if affected == 0 {
            // 已被中断/其他终态抢先：不发 run.completed，避免契约里出现两个终态。
            return Ok(());
        }
        if let Some(turn_id) = self.turn_id {
            sqlx::query(
                "UPDATE turns SET status = 'completed', usage = $2, finished_at = now() \
                 WHERE id = $1",
            )
            .bind(turn_id)
            .bind(usage_json.clone())
            .execute(&self.db)
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        }
        self.record_usage(usage).await?;
        self.publish(
            "run.completed",
            json!({ "status": "completed", "usage": usage_json }),
        );
        Ok(())
    }

    async fn failed(&self, code: &str, message: &str) -> Result<(), AgentError> {
        let error = json!({ "code": code, "message": message });
        let affected = sqlx::query(&format!(
            "UPDATE runs SET status = 'failed', error = $2, finished_at = now(), \
                 lease_owner = NULL, lease_until = NULL, \
                 updated_at = now() WHERE id = $1 AND status IN ({ACTIVE})"
        ))
        .bind(self.run_id)
        .bind(error.clone())
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?
        .rows_affected();
        if affected == 0 {
            return Ok(());
        }
        if let Some(turn_id) = self.turn_id {
            sqlx::query("UPDATE turns SET status = 'failed', finished_at = now() WHERE id = $1")
                .bind(turn_id)
                .execute(&self.db)
                .await
                .map_err(|e| AgentError::Internal(e.to_string()))?;
        }
        self.publish("run.failed", json!({ "code": code, "message": message }));
        Ok(())
    }

    async fn cancelled(&self, reason: &str) -> Result<(), AgentError> {
        let affected = sqlx::query(&format!(
            "UPDATE runs SET status = 'cancelled', \
                 error = jsonb_build_object('reason', $2::text), finished_at = now(), \
                 lease_owner = NULL, lease_until = NULL, \
                 updated_at = now() WHERE id = $1 AND status IN ({ACTIVE})"
        ))
        .bind(self.run_id)
        .bind(reason)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?
        .rows_affected();
        if affected == 0 {
            // `interrupt` 接口通常已经写过终态；这里不重复发 run.cancelled。
            return Ok(());
        }
        if let Some(turn_id) = self.turn_id {
            sqlx::query(
                "UPDATE turns SET status = 'cancelled', finished_at = now() \
                 WHERE id = $1 AND status IN ('queued', 'running', 'waiting_approval', 'waiting_user')",
            )
            .bind(turn_id)
            .execute(&self.db)
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        }
        self.publish("run.cancelled", json!({ "reason": reason }));
        Ok(())
    }
}

impl DbSink {
    /// usage 台账（B10 的统计口径）；一次 run 只记一行终值。
    async fn record_usage(&self, usage: &Usage) -> Result<(), AgentError> {
        sqlx::query(
            "INSERT INTO usage_events \
               (tenant_id, user_id, session_id, run_id, model, \
                input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens) \
             SELECT s.tenant_id, s.user_id, s.id, r.id, t.model, $2, $3, $4, $5 \
             FROM runs r \
             JOIN sessions s ON s.id = r.session_id \
             LEFT JOIN turns t ON t.id = r.turn_id \
             WHERE r.id = $1",
        )
        .bind(self.run_id)
        .bind(usage.input_tokens as i64)
        .bind(usage.output_tokens as i64)
        .bind(usage.cache_read_input_tokens as i64)
        .bind(usage.cache_creation_input_tokens as i64)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        Ok(())
    }
}

impl DbSink {
    async fn expire_approval(
        &self,
        approval_id: Uuid,
        tool_call_id: Uuid,
    ) -> Result<(), AgentError> {
        sqlx::query(
            "UPDATE approvals SET status = 'expired', decided_at = now() \
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(approval_id)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        self.resume_after_approval(tool_call_id, "failed").await
    }

    async fn resume_after_approval(
        &self,
        tool_call_id: Uuid,
        tool_status: &str,
    ) -> Result<(), AgentError> {
        sqlx::query("UPDATE tool_calls SET status = $2, updated_at = now() WHERE id = $1")
            .bind(tool_call_id)
            .bind(tool_status)
            .execute(&self.db)
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        sqlx::query(
            "UPDATE runs SET status = 'running', updated_at = now() \
             WHERE id = $1 AND status = 'waiting_approval'",
        )
        .bind(self.run_id)
        .execute(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_provider_prefix_wins() {
        let (provider, model) = resolve_model("openai:gpt-5", "").expect("explicit");
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-5");

        let (provider, model) = resolve_model("ollama:llama3", "").expect("local");
        assert_eq!(provider, "ollama");
        assert_eq!(model, "llama3");
    }

    #[test]
    fn default_provider_validates_model_family() {
        let (provider, model) = resolve_model("gpt-5", "openai").expect("gpt under openai");
        assert_eq!((provider.as_str(), model.as_str()), ("openai", "gpt-5"));

        let err = resolve_model("claude-sonnet-4", "openai").expect_err("wrong family");
        assert!(err.to_string().contains("does not belong"), "{err}");
        assert_eq!(err.code(), "provider.failed");
    }

    #[test]
    fn model_is_inferred_from_preset_prefixes() {
        for (model, provider) in [
            ("gpt-5-mini", "openai"),
            ("claude-sonnet-4", "anthropic"),
            ("gemini-2.5-pro", "google"),
            ("deepseek-chat", "deepseek"),
            ("qwen3-max", "dashscope"),
            ("glm-4.6", "zhipu"),
            ("grok-4", "xai"),
        ] {
            let (got, name) = resolve_model(model, "").unwrap_or_else(|e| panic!("{model}: {e}"));
            assert_eq!(got, provider, "{model}");
            assert_eq!(name, model);
        }
    }

    #[test]
    fn preset_id_as_model_uses_that_preset() {
        let (provider, model) = resolve_model("ollama", "").expect("preset id");
        assert_eq!((provider.as_str(), model.as_str()), ("ollama", "ollama"));
    }

    #[test]
    fn unknown_or_empty_model_fails_explicitly() {
        let err = resolve_model("totally-unknown-model", "").expect_err("unknown");
        assert!(err.to_string().contains("unknown model"));
        assert_eq!(err.code(), "provider.failed");

        let err = resolve_model("   ", "").expect_err("empty");
        assert!(err.to_string().contains("no model configured"));
    }

    #[test]
    fn subscription_plan_is_enforced() {
        let err = resolve_model("opencode-go:claude-sonnet-4", "").expect_err("plan");
        assert!(err.to_string().contains("subscription plan"), "{err}");
        let ok = resolve_model("opencode-go:glm-4.6", "").expect("allowed");
        assert_eq!(ok.0, "opencode-go");
    }

    #[test]
    fn usage_serializes_for_event_payloads() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 4,
            ..Default::default()
        };
        let value = serde_json::to_value(usage).expect("usage json");
        assert_eq!(value["input_tokens"], 10);
        assert_eq!(value["output_tokens"], 4);
        assert_eq!(value["cache_read_input_tokens"], 0);
    }
}
