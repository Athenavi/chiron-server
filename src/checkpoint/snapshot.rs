//! Run 快照：消息、工具状态、预算游标、provider 游标（PRD §7.6）。
//!
//! 快照刻意只存"恢复所需的最小状态"，不存完整 transcript：
//! - 消息：`messages` 表在快照之后仍会追加，恢复时以快照为准补齐尾部；
//! - 工具状态：只记 `tool_call_id → status`，用于"已完成的不重跑"；
//! - 预算游标：步数/工具次数/token 用量，恢复后从游标继续扣；
//! - provider 游标：模型与 provider id，保证恢复不会换模型。
//!
//! 状态机：`created → ready → restored | discarded`。`created` 表示行已写、
//! 快照体还没补齐（例如工作区 commit 失败），这种行不会被用来恢复。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 工作区快照与 run 快照可以分开抓，因此需要区分种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Run,
    Workspace,
    Both,
}

impl CheckpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Workspace => "workspace",
            Self::Both => "both",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "run" => Self::Run,
            "workspace" => Self::Workspace,
            "both" => Self::Both,
            _ => return None,
        })
    }

    pub fn has_run(self) -> bool {
        matches!(self, Self::Run | Self::Both)
    }

    pub fn has_workspace(self) -> bool {
        matches!(self, Self::Workspace | Self::Both)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Created,
    Ready,
    Restored,
    Discarded,
}

impl CheckpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Ready => "ready",
            Self::Restored => "restored",
            Self::Discarded => "discarded",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "created" => Self::Created,
            "ready" => Self::Ready,
            "restored" => Self::Restored,
            "discarded" => Self::Discarded,
            _ => return None,
        })
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use CheckpointStatus::*;
        match self {
            Created => matches!(next, Ready | Discarded),
            Ready => matches!(next, Restored | Discarded),
            Restored => matches!(next, Discarded),
            Discarded => false,
        }
    }

    /// 只有 ready/restored 的行能被恢复（created 可能缺半个快照）。
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Ready | Self::Restored)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetCursor {
    pub steps_used: usize,
    pub tool_calls_used: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl BudgetCursor {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// 恢复后的剩余额度（不会为负）。
    pub fn remaining(
        &self,
        max_steps: usize,
        max_tool_calls: usize,
        max_tokens: u64,
    ) -> ResumePlan {
        ResumePlan {
            steps_remaining: max_steps.saturating_sub(self.steps_used),
            tool_calls_remaining: max_tool_calls.saturating_sub(self.tool_calls_used),
            tokens_remaining: max_tokens.saturating_sub(self.total_tokens()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumePlan {
    pub steps_remaining: usize,
    pub tool_calls_remaining: usize,
    pub tokens_remaining: u64,
}

impl ResumePlan {
    pub fn is_viable(&self) -> bool {
        self.steps_remaining > 0 && self.tokens_remaining > 0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMessage {
    pub role: String,
    #[serde(default)]
    pub kind: String,
    pub content: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotTool {
    pub tool_call_id: Uuid,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub args: Value,
}

/// 恢复所需的全部状态。字段全部可选/有默认，老版本快照也能反序列化。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub run_id: Uuid,
    pub session_id: Uuid,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
    pub seq: u32,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub budget: BudgetCursor,
    #[serde(default)]
    pub remaining_tokens: u64,
    #[serde(default)]
    pub messages: Vec<SnapshotMessage>,
    #[serde(default)]
    pub tools: Vec<SnapshotTool>,
    #[serde(default)]
    pub todo: Vec<Value>,
    #[serde(default)]
    pub steers: Vec<String>,
    #[serde(default)]
    pub workspace_commit: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl RunSnapshot {
    pub fn new(run_id: Uuid, session_id: Uuid, turn_id: Option<Uuid>, seq: u32) -> Self {
        Self {
            run_id,
            session_id,
            turn_id,
            seq,
            model: None,
            provider: None,
            budget: BudgetCursor::default(),
            remaining_tokens: 0,
            messages: Vec::new(),
            tools: Vec::new(),
            todo: Vec::new(),
            steers: Vec::new(),
            workspace_commit: None,
            created_at: Utc::now(),
        }
    }

    /// 下一个快照序号（同一个 run 内单调递增）。
    pub fn next_seq(&self) -> u32 {
        self.seq.saturating_add(1)
    }

    /// 已完成/已取消的工具调用：恢复时这些不能再跑（幂等台账）。
    pub fn settled_tool_calls(&self) -> Vec<Uuid> {
        self.tools
            .iter()
            .filter(|t| matches!(t.status.as_str(), "completed" | "cancelled" | "failed"))
            .map(|t| t.tool_call_id)
            .collect()
    }

    /// 崩溃时仍在跑的工具：恢复时一律标 failed，避免"以为跑过其实没跑完"。
    pub fn interrupted_tool_calls(&self) -> Vec<Uuid> {
        self.tools
            .iter()
            .filter(|t| {
                matches!(
                    t.status.as_str(),
                    "running" | "awaiting_approval" | "pending"
                )
            })
            .map(|t| t.tool_call_id)
            .collect()
    }

    /// 快照本身的合法性：序号、归属、消息角色。
    pub fn validate(&self, previous_seq: Option<u32>) -> Result<(), String> {
        if self.session_id.is_nil() || self.run_id.is_nil() {
            return Err("snapshot must reference a run and a session".into());
        }
        if let Some(prev) = previous_seq {
            if self.seq <= prev {
                return Err(format!(
                    "snapshot seq {} is not after previous seq {prev}",
                    self.seq
                ));
            }
        }
        if self.remaining_tokens == 0 && self.budget.steps_used > 0 {
            return Err("snapshot has steps but no remaining token budget".into());
        }
        for message in &self.messages {
            if !matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            ) {
                return Err(format!("unknown snapshot message role `{}`", message.role));
            }
        }
        Ok(())
    }

    pub fn size_bytes(&self) -> u64 {
        serde_json::to_vec(self)
            .map(|v| v.len() as u64)
            .unwrap_or(0)
    }
}

/// 快照的比较口径：同一 run 内 `(seq, created_at)` 越大越新。
pub fn is_newer(
    candidate_seq: u32,
    candidate_at: DateTime<Utc>,
    current: Option<(u32, DateTime<Utc>)>,
) -> bool {
    match current {
        None => true,
        Some((seq, at)) => (candidate_seq, candidate_at) > (seq, at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(seq: u32) -> RunSnapshot {
        RunSnapshot::new(Uuid::new_v4(), Uuid::new_v4(), None, seq)
    }

    #[test]
    fn checkpoint_kind_and_status_roundtrip() {
        for kind in [
            CheckpointKind::Run,
            CheckpointKind::Workspace,
            CheckpointKind::Both,
        ] {
            assert_eq!(CheckpointKind::parse(kind.as_str()), Some(kind));
        }
        assert!(CheckpointKind::Both.has_run() && CheckpointKind::Both.has_workspace());
        assert!(!CheckpointKind::Run.has_workspace());
        assert!(CheckpointKind::parse("full").is_none());

        for status in [
            CheckpointStatus::Created,
            CheckpointStatus::Ready,
            CheckpointStatus::Restored,
            CheckpointStatus::Discarded,
        ] {
            assert_eq!(CheckpointStatus::parse(status.as_str()), Some(status));
        }
        assert!(!CheckpointStatus::Created.is_usable());
        assert!(CheckpointStatus::Ready.is_usable());
        assert!(CheckpointStatus::Restored.is_usable());
        assert!(!CheckpointStatus::Discarded.is_usable());
    }

    #[test]
    fn status_machine_only_moves_forward() {
        use CheckpointStatus::*;
        assert!(Created.can_transition_to(Ready));
        assert!(Created.can_transition_to(Discarded));
        assert!(Ready.can_transition_to(Restored));
        assert!(Ready.can_transition_to(Discarded));
        assert!(Restored.can_transition_to(Discarded));
        assert!(!Restored.can_transition_to(Ready), "恢复过的快照不能回退");
        assert!(!Discarded.can_transition_to(Ready));
        assert!(!Ready.can_transition_to(Created));
        assert!(!Created.can_transition_to(Restored), "必须经过 ready");
    }

    #[test]
    fn resume_plan_subtracts_cursor_and_never_goes_negative() {
        let cursor = BudgetCursor {
            steps_used: 3,
            tool_calls_used: 7,
            input_tokens: 900,
            output_tokens: 100,
        };
        assert_eq!(cursor.total_tokens(), 1_000);
        let plan = cursor.remaining(10, 20, 5_000);
        assert_eq!(plan.steps_remaining, 7);
        assert_eq!(plan.tool_calls_remaining, 13);
        assert_eq!(plan.tokens_remaining, 4_000);
        assert!(plan.is_viable());

        let exhausted = BudgetCursor {
            steps_used: 10,
            ..BudgetCursor::default()
        }
        .remaining(10, 20, 5_000);
        assert_eq!(exhausted.steps_remaining, 0);
        assert!(!exhausted.is_viable());

        let broke = cursor.remaining(10, 20, 500);
        assert_eq!(broke.tokens_remaining, 0);
    }

    #[test]
    fn tool_ledger_separates_settled_and_interrupted() {
        let mut snap = snapshot(1);
        snap.tools = vec![
            SnapshotTool {
                tool_call_id: Uuid::new_v4(),
                name: "read".into(),
                status: "completed".into(),
                args: json!({}),
            },
            SnapshotTool {
                tool_call_id: Uuid::new_v4(),
                name: "shell".into(),
                status: "running".into(),
                args: json!({"command": "sleep 10"}),
            },
            SnapshotTool {
                tool_call_id: Uuid::new_v4(),
                name: "write".into(),
                status: "cancelled".into(),
                args: json!({}),
            },
        ];
        assert_eq!(snap.settled_tool_calls().len(), 2);
        assert_eq!(snap.interrupted_tool_calls().len(), 1);
        assert_eq!(snap.tools[1].name, "shell");
    }

    #[test]
    fn validate_rejects_bad_snapshots() {
        let mut snap = snapshot(2);
        assert!(snap.validate(None).is_ok());
        assert!(snap.validate(Some(1)).is_ok());
        assert!(snap.validate(Some(2)).is_err(), "seq 必须严格递增");

        snap.messages.push(SnapshotMessage {
            role: "narrator".into(),
            kind: "chat".into(),
            content: json!("x"),
        });
        assert!(snap.validate(Some(1)).is_err());

        let mut broke = snapshot(3);
        broke.budget.steps_used = 2;
        broke.remaining_tokens = 0;
        assert!(broke.validate(None).is_err(), "有步数就必须有剩余预算");

        let nil = RunSnapshot::new(Uuid::nil(), Uuid::nil(), None, 0);
        assert!(nil.validate(None).is_err());
    }

    #[test]
    fn sequence_and_recency_helpers() {
        let snap = snapshot(4);
        assert_eq!(snap.next_seq(), 5);
        let now = Utc::now();
        assert!(is_newer(1, now, None));
        assert!(is_newer(2, now, Some((1, now))));
        assert!(!is_newer(1, now, Some((2, now))));
    }

    #[test]
    fn snapshot_serializes_with_defaults_for_older_rows() {
        let run_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let minimal = json!({
            "run_id": run_id,
            "session_id": session_id,
            "seq": 1,
            "created_at": Utc::now(),
        });
        let snap: RunSnapshot = serde_json::from_value(minimal).expect("defaults");
        assert_eq!(snap.run_id, run_id);
        assert!(snap.messages.is_empty());
        assert_eq!(snap.budget, BudgetCursor::default());
        assert!(snap.size_bytes() > 0);
    }
}
