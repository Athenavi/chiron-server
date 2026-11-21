//! Run 状态机、单活跃 run 约束与幂等键的纯逻辑。
//!
//! 这些函数不碰数据库，便于单测；SQL 侧只负责把约束映射成同样的结果：
//! - `ux_runs_one_active_per_session`（部分唯一索引）兜底并发提交；
//! - `ux_idempotency_scope` 兜底并发同键提交。

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// `queued → running → waiting_approval|waiting_user → completed|failed|cancelled`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
}

/// 与迁移里的部分唯一索引保持同一口径：queued 也算活跃。
/// SQL 里用字面量 `IN (...)`，避免 text[] 参数的类型推断问题。
pub const ACTIVE_STATUS_SQL: &str = "'queued', 'running', 'waiting_approval', 'waiting_user'";

#[cfg(test)]
pub const ACTIVE_STATUSES: [&str; 4] = ["queued", "running", "waiting_approval", "waiting_user"];

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::WaitingUser => "waiting_user",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "waiting_approval" => Self::WaitingApproval,
            "waiting_user" => Self::WaitingUser,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use RunStatus::*;
        match self {
            Queued => matches!(next, Running | Completed | Failed | Cancelled),
            Running => matches!(
                next,
                WaitingApproval | WaitingUser | Completed | Failed | Cancelled
            ),
            WaitingApproval | WaitingUser => {
                matches!(next, Running | Completed | Failed | Cancelled)
            }
            Completed | Failed | Cancelled => false,
        }
    }
}

pub fn ensure_transition(from: RunStatus, to: RunStatus) -> Result<RunStatus, AppError> {
    if from.can_transition_to(to) {
        Ok(to)
    } else {
        Err(AppError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "illegal run transition {} -> {}",
                from.as_str(),
                to.as_str()
            ),
        ))
    }
}

/// 中断只作用于活跃 run；终态返回 `None`（幂等：重复中断不报错，直接返回当前状态）。
pub fn interrupt_target(status: RunStatus) -> Option<RunStatus> {
    status.is_active().then_some(RunStatus::Cancelled)
}

pub fn run_busy_error() -> AppError {
    AppError::new(
        StatusCode::CONFLICT,
        "run.busy",
        "session already has an active run",
    )
}

/// 提交前的快速判断（真正的并发兜底仍是唯一索引）。
pub fn ensure_no_active_run(active: &[RunStatus]) -> Result<(), AppError> {
    if active.iter().any(|s| s.is_active()) {
        Err(run_busy_error())
    } else {
        Ok(())
    }
}

pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 200;

pub fn validate_idempotency_key(key: Option<&str>) -> Result<Option<&str>, AppError> {
    match key.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(k) if k.len() > MAX_IDEMPOTENCY_KEY_LEN => Err(AppError::bad_request(
            "Idempotency-Key must be at most 200 bytes",
        )),
        Some(k) if !k.chars().all(|c| c.is_ascii_graphic()) => Err(AppError::bad_request(
            "Idempotency-Key must be printable ASCII",
        )),
        Some(k) => Ok(Some(k)),
    }
}

/// 同键不同请求体 → 409；同键同请求体重放 → 返回首次结果。
pub fn request_hash(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" | "accept" | "approve" | "approved" => Ok(Self::Allow),
            "deny" | "denied" | "reject" | "rejected" => Ok(Self::Deny),
            _ => Err(AppError::bad_request("decision must be allow or deny")),
        }
    }
}

pub fn parse_decision(value: &str) -> Result<ApprovalDecision, AppError> {
    ApprovalDecision::parse(value)
}

/// 把 PG 唯一约束冲突映射成领域错误；其他错误原样返回。
pub fn is_unique_violation(err: &sqlx::Error, constraint: &str) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            db.code().as_deref() == Some("23505")
                && db.constraint().map(|c| c == constraint).unwrap_or(false)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_roundtrips_through_text() {
        for status in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::WaitingApproval,
            RunStatus::WaitingUser,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert_eq!(RunStatus::parse(status.as_str()), Some(status));
            assert_eq!(status.is_active(), !status.is_terminal());
        }
        assert!(RunStatus::parse("pause").is_none());
    }

    #[test]
    fn happy_path_transitions_are_allowed() {
        use RunStatus::*;
        let path = [Queued, Running, WaitingApproval, Running, Completed];
        for pair in path.windows(2) {
            assert!(pair[0].can_transition_to(pair[1]));
        }
        assert!(Running.can_transition_to(Cancelled));
        assert!(WaitingUser.can_transition_to(Cancelled));
    }

    #[test]
    fn terminal_statuses_are_sticky() {
        use RunStatus::*;
        for from in [Completed, Failed, Cancelled] {
            for to in [
                Queued,
                Running,
                WaitingApproval,
                WaitingUser,
                Completed,
                Failed,
                Cancelled,
            ] {
                assert!(!from.can_transition_to(to));
            }
            assert!(interrupt_target(from).is_none());
        }
        assert_eq!(interrupt_target(Queued), Some(RunStatus::Cancelled));
        assert_eq!(interrupt_target(Running), Some(RunStatus::Cancelled));
    }

    #[test]
    fn queued_run_blocks_second_submission() {
        let active = [RunStatus::Queued];
        let err = ensure_no_active_run(&active).expect_err("must be busy");
        assert_eq!(err.code, "run.busy");
        assert_eq!(err.status, StatusCode::CONFLICT);

        // 活跃集合里只要有一个非终态就拒绝。
        let mixed = [RunStatus::Completed, RunStatus::WaitingUser];
        assert!(ensure_no_active_run(&mixed).is_err());

        // 全是终态才允许提交。
        assert!(ensure_no_active_run(&[RunStatus::Completed, RunStatus::Cancelled]).is_ok());
        assert!(ensure_no_active_run(&[]).is_ok());
    }

    #[test]
    fn active_status_list_matches_index_definition() {
        // 迁移里的部分唯一索引用了同一组字面量；改一处就要改两处。
        let migration = include_str!("../../migrations/0001_init.sql");
        for status in ACTIVE_STATUSES {
            assert!(
                migration.contains(&format!("'{}'", status)),
                "missing active status {status} in migration"
            );
        }
        assert!(migration.contains("ux_runs_one_active_per_session"));
    }

    #[test]
    fn illegal_transition_is_conflict() {
        let err = ensure_transition(RunStatus::Completed, RunStatus::Running)
            .expect_err("terminal cannot restart");
        assert_eq!(err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn idempotency_key_validation() {
        assert_eq!(validate_idempotency_key(None).unwrap(), None);
        assert_eq!(validate_idempotency_key(Some("  ")).unwrap(), None);
        assert_eq!(
            validate_idempotency_key(Some(" abc-123 ")).unwrap(),
            Some("abc-123")
        );
        assert!(validate_idempotency_key(Some(&"k".repeat(201))).is_err());
        assert!(validate_idempotency_key(Some("bad\nkey")).is_err());
    }

    #[test]
    fn request_hash_is_stable_and_payload_sensitive() {
        assert_eq!(request_hash(b"same"), request_hash(b"same"));
        assert_ne!(request_hash(b"same"), request_hash(b"other"));
        assert_eq!(request_hash(b"same").len(), 64);
    }

    #[test]
    fn approval_decision_aliases() {
        assert_eq!(
            ApprovalDecision::parse("allow").unwrap(),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalDecision::parse(" APPROVE ").unwrap(),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalDecision::parse("deny").unwrap(),
            ApprovalDecision::Deny
        );
        assert_eq!(
            ApprovalDecision::parse("reject").unwrap(),
            ApprovalDecision::Deny
        );
        assert!(ApprovalDecision::parse("maybe").is_err());
        assert_eq!(ApprovalDecision::Allow.as_str(), "allow");
    }
}
