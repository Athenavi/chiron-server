//! B6：子 Agent（PRD §7.5）。
//!
//! 模块划分：
//! - [`tree`]：纯逻辑——深度/并发/预算准入、状态机、取消联动、结果摘要；
//! - [`runner`]：子 Agent 执行器，复用 B5 的 provider + 工具层 + 沙箱，
//!   把子循环的增量转发到父会话 SSE（`subagent.*`）；
//! - [`store`]：`subagent_runs` 的 PG 读写与派生 API；
//! - [`api`]：`GET /runs/{id}/subagents` 等查询/取消路由。
//!
//! 父子关系只有一条约束：子 Agent 的一切预算都来自父 run 的剩余额度，
//! 父被取消时整棵子树跟着取消（PRD：父回合结束不强制杀子 Agent，
//! 由显式取消/会话删除触发）。

// 二进制 crate 里 `store`/`api` 的公共面与 provider 同口径，先关掉 dead_code。
#![allow(dead_code)]

pub mod api;
pub mod runner;
pub mod store;
pub mod tree;

use std::env;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::agent::error::AgentError;

pub use tree::{ChildBudget, ChildSpec, Limits, SubagentError, SubagentStatus};

/// 子 Agent 部署配置（全部来自 env，缺省值保守）。
#[derive(Clone, Debug)]
pub struct SubagentConfig {
    pub enabled: bool,
    pub limits: Limits,
    /// 子 Agent 是否允许执行写/执行类工具。
    ///
    /// 默认 `false`：子 Agent 没有独立的审批通道，默认只给只读工具，
    /// 需要写盘的活留给父 run——这样不会出现"子 Agent 绕过审批改文件"。
    pub allow_mutating: bool,
    /// 子 Agent 默认模型；为空表示继承父 run 的模型。
    pub model: String,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            limits: Limits::default(),
            allow_mutating: false,
            model: String::new(),
        }
    }
}

impl SubagentConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let limits = Limits {
            max_depth: env_usize("CHIRON_SUBAGENT_MAX_DEPTH", defaults.limits.max_depth),
            max_children: env_usize("CHIRON_SUBAGENT_MAX_CHILDREN", defaults.limits.max_children),
            max_concurrent: env_usize(
                "CHIRON_SUBAGENT_MAX_CONCURRENT",
                defaults.limits.max_concurrent,
            ),
            max_steps: env_usize("CHIRON_SUBAGENT_MAX_STEPS", defaults.limits.max_steps),
            max_tool_calls: env_usize(
                "CHIRON_SUBAGENT_MAX_TOOL_CALLS",
                defaults.limits.max_tool_calls,
            ),
            max_total_tokens: env_u64(
                "CHIRON_SUBAGENT_MAX_TOTAL_TOKENS",
                defaults.limits.max_total_tokens,
            ),
            max_input_tokens: env_u64(
                "CHIRON_SUBAGENT_MAX_INPUT_TOKENS",
                defaults.limits.max_input_tokens,
            ),
            timeout_secs: env_u64("CHIRON_SUBAGENT_TIMEOUT_SECS", defaults.limits.timeout_secs),
        }
        .sanitized();
        Self {
            enabled: env_bool("CHIRON_SUBAGENT_ENABLED", defaults.enabled),
            limits,
            allow_mutating: env_bool("CHIRON_SUBAGENT_ALLOW_MUTATING", defaults.allow_mutating),
            model: env::var("CHIRON_SUBAGENT_MODEL")
                .unwrap_or_default()
                .trim()
                .to_string(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// 父 run 的上下文：子 Agent 从这里继承模型/沙箱/租户与剩余预算。
#[derive(Clone, Debug)]
pub struct ParentContext {
    pub parent_run_id: Uuid,
    pub root_run_id: Uuid,
    pub parent_subagent_id: Option<Uuid>,
    pub session_id: Uuid,
    pub tenant_id: i16,
    pub user_id: Uuid,
    pub project_id: Option<String>,
    /// 父 run 的深度：0 表示顶层 run。
    pub depth: usize,
    /// 父 run 当前可用的 token 额度（用于切子预算）。
    pub remaining_tokens: u64,
}

impl ParentContext {
    pub fn child_depth(&self) -> usize {
        self.depth + 1
    }
}

/// 子 Agent 的落库/事件端口。生产实现见 [`store::PgHost`]，测试用内存 fake。
#[async_trait]
pub trait SubagentHost: Send + Sync {
    /// 准入判定（深度/并发由实现方补齐活跃数，预算可直接用 `tree::admit`）。
    async fn admit(&self, depth: usize) -> Result<ChildBudget, SubagentError>;

    /// 落 `subagent_runs` 并分配 id（同时发 `subagent.started`）。
    async fn started(
        &self,
        spec: &ChildSpec,
        depth: usize,
        budget: ChildBudget,
    ) -> Result<Uuid, AgentError>;

    /// 子 Agent 正文增量（转发为 `subagent.delta`）。
    async fn delta(&self, id: Uuid, text: &str);

    /// 终态：写 status/summary/usage 并发 `subagent.completed`。
    async fn finished(
        &self,
        id: Uuid,
        status: SubagentStatus,
        summary: &str,
        usage: &crate::provider::Usage,
    );
}

/// 工具层的调用入口：`subagent` 工具只依赖这个 trait，
/// 因此 `agent::tools` 的单测不需要数据库/真 provider。
#[async_trait]
pub trait Invoker: Send + Sync {
    async fn invoke(&self, args: &Value) -> Result<Value, AgentError>;
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_conservative() {
        let cfg = SubagentConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.allow_mutating, "子 Agent 默认不许写盘");
        assert!(cfg.limits.max_depth <= 2, "默认嵌套不超过两层");
        assert_eq!(cfg.model, "");
        assert!(!SubagentConfig::disabled().enabled);
    }

    #[test]
    fn child_depth_is_parent_plus_one() {
        let parent = ParentContext {
            parent_run_id: Uuid::new_v4(),
            root_run_id: Uuid::new_v4(),
            parent_subagent_id: None,
            session_id: Uuid::new_v4(),
            tenant_id: 1,
            user_id: Uuid::new_v4(),
            project_id: None,
            depth: 1,
            remaining_tokens: 10_000,
        };
        assert_eq!(parent.child_depth(), 2);
    }
}
