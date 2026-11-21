//! Agent 循环的错误类型。
//!
//! 错误码与 `docs/API.md` §2 对齐：run 终态事件里的 `code` 直接取自这里，
//! 前端不必再映射一层。

use crate::{error::AppError, provider::ProviderError};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider failed: {0}")]
    Provider(String),
    #[error("sandbox unavailable: {0}")]
    Sandbox(String),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("budget exceeded: {0}")]
    Budget(String),
    #[error("run cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

impl AgentError {
    /// 写进 `run.failed.data.code` 的稳定字符串。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Provider(_) => "provider.failed",
            Self::Sandbox(_) => "sandbox.unavailable",
            Self::Tool(_) => "tool.failed",
            Self::Budget(_) => "quota.exceeded",
            Self::Cancelled => "run.cancelled",
            Self::Internal(_) => "internal_error",
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// 工具/沙箱的 `AppError` 收敛成 agent 错误；404/403 等仍是工具失败。
    pub fn from_app(err: AppError) -> Self {
        tracing::warn!(code = err.code, message = %err.message, "agent component failed");
        Self::Sandbox(err.message)
    }
}

impl From<ProviderError> for AgentError {
    fn from(err: ProviderError) -> Self {
        if matches!(err, ProviderError::Cancelled) {
            return Self::Cancelled;
        }
        Self::Provider(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_contract() {
        assert_eq!(AgentError::Provider("x".into()).code(), "provider.failed");
        assert_eq!(
            AgentError::Sandbox("x".into()).code(),
            "sandbox.unavailable"
        );
        assert_eq!(AgentError::Tool("x".into()).code(), "tool.failed");
        assert_eq!(AgentError::Budget("x".into()).code(), "quota.exceeded");
        assert_eq!(AgentError::Cancelled.code(), "run.cancelled");
        assert_eq!(AgentError::Internal("x".into()).code(), "internal_error");
        assert!(AgentError::Cancelled.is_cancelled());
    }

    #[test]
    fn provider_cancelled_maps_to_agent_cancelled() {
        let err: AgentError = ProviderError::Cancelled.into();
        assert!(err.is_cancelled());
    }
}
