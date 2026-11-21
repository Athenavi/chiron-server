//! 父子 run 树的纯逻辑：深度/并发/预算准入、状态机、取消联动、结果摘要。
//!
//! 这一层不碰数据库也不碰 provider，因此可以被单测完整覆盖；生产路径
//! （`subagent::run`）只负责把这里算出的决策落到 `subagent_runs` 和事件上。
//!
//! 防止递归爆炸（PRD §7.5）有三道闸门，任何一道不过就明确失败：
//! 1. **深度**：子 Agent 的 `depth` 来自父 `depth + 1`，超过上限直接拒绝；
//! 2. **并发**：单个父 run 的活跃子数 + 整棵树活跃子数各有上限；
//! 3. **预算**：子预算从父的剩余额度里切，切不出来就拒绝，绝不借用父配额。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use uuid::Uuid;

/// 深度硬上限：即便 env 配错也不允许无限嵌套。
pub const MAX_DEPTH_HARD: usize = 4;
/// 子 Agent 最少要拿到的 token 预算，低于这个数不值得启动。
pub const MIN_CHILD_TOKENS: u64 = 2_000;
/// 摘要长度上限（落库与事件共用）。
pub const SUMMARY_MAX_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// 最大嵌套深度（1 = 只允许父 run 直接派生的子 Agent）。
    pub max_depth: usize,
    /// 单个父 run 同时活跃的子 Agent 数。
    pub max_children: usize,
    /// 整棵树同时活跃的子 Agent 数（跨父节点）。
    pub max_concurrent: usize,
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_total_tokens: u64,
    pub max_input_tokens: u64,
    pub timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_children: 3,
            max_concurrent: 6,
            max_steps: 6,
            max_tool_calls: 20,
            max_total_tokens: 60_000,
            max_input_tokens: 30_000,
            timeout_secs: 600,
        }
    }
}

impl Limits {
    /// 收敛可疑配置：0 一律抬到 1，深度不超过 [`MAX_DEPTH_HARD`]。
    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        self.max_depth = self.max_depth.clamp(1, MAX_DEPTH_HARD);
        self.max_children = self.max_children.max(1);
        self.max_concurrent = self.max_concurrent.max(1);
        self.max_steps = self.max_steps.max(1);
        self.max_tool_calls = self.max_tool_calls.max(1);
        self.max_total_tokens = self.max_total_tokens.max(MIN_CHILD_TOKENS);
        self.max_input_tokens = self
            .max_input_tokens
            .clamp(MIN_CHILD_TOKENS, self.max_total_tokens);
        self.timeout_secs = if self.timeout_secs == 0 {
            defaults.timeout_secs
        } else {
            self.timeout_secs
        };
        self
    }
}

/// 子 Agent 的预算（由父的剩余额度切出来）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildBudget {
    pub max_input_tokens: u64,
    pub max_total_tokens: u64,
    pub max_steps: usize,
    pub max_tool_calls: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRequest {
    /// 子 Agent 的深度（父 run 是 0）。
    pub depth: usize,
    /// 同一个父 run 下仍在活跃的子 Agent 数。
    pub active_siblings: usize,
    /// 整棵树仍在活跃的子 Agent 数。
    pub active_tree: usize,
    /// 父 run 还剩多少 token 额度。
    pub remaining_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentError {
    DepthExceeded { depth: usize, max: usize },
    ConcurrencyExceeded { active: usize, max: usize },
    BudgetExhausted { remaining: u64, needed: u64 },
    Cancelled,
    Invalid(String),
}

impl SubagentError {
    /// 落 `run.failed` / 工具错误里的稳定字符串。
    pub fn code(&self) -> &'static str {
        match self {
            Self::DepthExceeded { .. } => "subagent.depth_exceeded",
            Self::ConcurrencyExceeded { .. } => "subagent.concurrency_exceeded",
            Self::BudgetExhausted { .. } => "subagent.budget_exhausted",
            Self::Cancelled => "run.cancelled",
            Self::Invalid(_) => "subagent.invalid",
        }
    }
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DepthExceeded { depth, max } => {
                write!(f, "subagent depth {depth} exceeds limit {max}")
            }
            Self::ConcurrencyExceeded { active, max } => {
                write!(f, "subagent concurrency {active} reached limit {max}")
            }
            Self::BudgetExhausted { remaining, needed } => {
                write!(f, "parent token budget left {remaining} < needed {needed}")
            }
            Self::Cancelled => write!(f, "subagent cancelled"),
            Self::Invalid(msg) => write!(f, "invalid subagent request: {msg}"),
        }
    }
}

/// 准入判定。顺序是"深度 → 并发 → 预算"，错误信息因此始终指向最根本的原因。
pub fn admit(limits: Limits, req: AdmissionRequest) -> Result<ChildBudget, SubagentError> {
    let limits = limits.sanitized();
    if req.depth == 0 {
        return Err(SubagentError::Invalid("depth must be >= 1".into()));
    }
    if req.depth > limits.max_depth {
        return Err(SubagentError::DepthExceeded {
            depth: req.depth,
            max: limits.max_depth,
        });
    }
    if req.active_siblings >= limits.max_children {
        return Err(SubagentError::ConcurrencyExceeded {
            active: req.active_siblings,
            max: limits.max_children,
        });
    }
    if req.active_tree >= limits.max_concurrent {
        return Err(SubagentError::ConcurrencyExceeded {
            active: req.active_tree,
            max: limits.max_concurrent,
        });
    }

    // 只切一半：父 run 自己还要继续跑完最后几轮。
    let share = req.remaining_tokens / 2;
    let tokens = share.min(limits.max_total_tokens);
    if tokens < MIN_CHILD_TOKENS {
        return Err(SubagentError::BudgetExhausted {
            remaining: req.remaining_tokens,
            needed: MIN_CHILD_TOKENS,
        });
    }
    Ok(ChildBudget {
        max_input_tokens: limits.max_input_tokens.min(tokens),
        max_total_tokens: tokens,
        max_steps: limits.max_steps,
        max_tool_calls: limits.max_tool_calls,
    })
}

/// 子 Agent 状态机：`queued → running → completed|failed|cancelled`，
/// 终态粘滞；`queued` 可以直接被取消（还没开始就撤销）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use SubagentStatus::*;
        match self {
            Queued => matches!(next, Running | Cancelled | Failed),
            Running => matches!(next, Completed | Failed | Cancelled),
            Completed | Failed | Cancelled => false,
        }
    }
}

/// 派生请求：工具参数里只有 `task`/`agent`/`model` 三个有意义字段，
/// 其余一律忽略（模型不能借参数改深度或预算）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildSpec {
    pub agent: String,
    pub task: String,
    pub model: Option<String>,
}

pub const TASK_MAX_CHARS: usize = 8_000;

impl ChildSpec {
    pub fn from_args(args: &Value) -> Result<Self, SubagentError> {
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| SubagentError::Invalid("missing argument `task`".into()))?;
        if task.chars().count() > TASK_MAX_CHARS {
            return Err(SubagentError::Invalid(format!(
                "task must be at most {TASK_MAX_CHARS} characters"
            )));
        }
        let agent = args
            .get("agent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or("general")
            .to_string();
        let model = args
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        Ok(Self {
            agent,
            task: task.to_string(),
            model,
        })
    }
}

/// 进程内父子关系（只用于取消联动与深度查询；权威状态在 PG）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub parent: Option<Uuid>,
    pub depth: usize,
}

#[derive(Debug, Default)]
pub struct RunTree {
    nodes: BTreeMap<Uuid, Node>,
}

impl RunTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root(&mut self, run_id: Uuid) {
        self.nodes.entry(run_id).or_insert(Node {
            parent: None,
            depth: 0,
        });
    }

    /// 登记子节点；父节点不存在时按 `depth - 1` 补一个（幂等）。
    pub fn add_child(&mut self, child: Uuid, parent: Uuid, depth: usize) {
        let depth = depth.max(1);
        self.nodes.entry(parent).or_insert(Node {
            parent: None,
            depth: depth - 1,
        });
        self.nodes.insert(
            child,
            Node {
                parent: Some(parent),
                depth,
            },
        );
    }

    pub fn depth(&self, id: Uuid) -> usize {
        self.nodes.get(&id).map(|n| n.depth).unwrap_or(0)
    }

    pub fn parent(&self, id: Uuid) -> Option<Uuid> {
        self.nodes.get(&id).and_then(|n| n.parent)
    }

    pub fn children_of(&self, parent: Uuid) -> Vec<Uuid> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.parent == Some(parent))
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 取消联动：取消父 = 取消整棵子树。返回值保持"父先子后"的稳定顺序，
    /// 调用方按序写库/发事件即可。
    pub fn cancel_subtree(&self, root: Uuid) -> Vec<Uuid> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut queue = vec![root];
        while let Some(id) = queue.pop() {
            if !seen.insert(id) {
                continue;
            }
            out.push(id);
            let mut children = self.children_of(id);
            children.sort_unstable();
            // 栈是后进先出：反转后子节点仍按 id 升序处理。
            queue.extend(children.into_iter().rev());
        }
        out
    }

    pub fn remove(&mut self, id: Uuid) {
        self.nodes.remove(&id);
    }
}

/// 结果摘要：状态 + 尾部正文。超长时保留开头（结论优先）并显式标注截断。
pub fn summarize(status: SubagentStatus, text: &str) -> String {
    let body = text.trim();
    let prefix = format!("[{}] ", status.as_str());
    let room = SUMMARY_MAX_CHARS.saturating_sub(prefix.chars().count());
    let mut out = String::with_capacity(SUMMARY_MAX_CHARS);
    out.push_str(&prefix);
    if body.chars().count() <= room {
        out.push_str(body);
    } else {
        let head: String = body.chars().take(room.saturating_sub(3)).collect();
        out.push_str(&head);
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> Limits {
        Limits {
            max_depth: 2,
            max_children: 2,
            max_concurrent: 3,
            max_steps: 4,
            max_tool_calls: 8,
            max_total_tokens: 10_000,
            max_input_tokens: 5_000,
            timeout_secs: 60,
        }
    }

    fn req(depth: usize, siblings: usize, tree: usize, remaining: u64) -> AdmissionRequest {
        AdmissionRequest {
            depth,
            active_siblings: siblings,
            active_tree: tree,
            remaining_tokens: remaining,
        }
    }

    #[test]
    fn depth_limit_rejects_nested_explosion() {
        let err = admit(limits(), req(3, 0, 0, 100_000)).expect_err("too deep");
        assert_eq!(err.code(), "subagent.depth_exceeded");
        assert!(admit(limits(), req(2, 0, 0, 100_000)).is_ok());
        // depth 0 是父 run 自己，不能派生。
        assert_eq!(
            admit(limits(), req(0, 0, 0, 100_000)).unwrap_err().code(),
            "subagent.invalid"
        );
    }

    #[test]
    fn concurrency_limits_cover_siblings_and_whole_tree() {
        let err = admit(limits(), req(1, 2, 0, 100_000)).expect_err("siblings full");
        assert_eq!(err.code(), "subagent.concurrency_exceeded");
        let err = admit(limits(), req(1, 0, 3, 100_000)).expect_err("tree full");
        assert_eq!(err.code(), "subagent.concurrency_exceeded");
        assert!(admit(limits(), req(1, 1, 2, 100_000)).is_ok());
    }

    #[test]
    fn child_budget_is_half_of_parent_remaining_and_capped() {
        let budget = admit(limits(), req(1, 0, 0, 4_000)).expect("admitted");
        assert_eq!(budget.max_total_tokens, 2_000);
        assert_eq!(budget.max_input_tokens, 2_000, "input 不超过总预算");
        assert_eq!(budget.max_steps, 4);

        let budget = admit(limits(), req(1, 0, 0, 200_000)).expect("admitted");
        assert_eq!(budget.max_total_tokens, 10_000, "不超过单 Agent 上限");
        assert_eq!(budget.max_input_tokens, 5_000);
    }

    #[test]
    fn budget_exhausted_when_parent_has_too_little_left() {
        let err = admit(limits(), req(1, 0, 0, 3_000)).expect_err("too little");
        assert_eq!(err.code(), "subagent.budget_exhausted");
        assert!(err.to_string().contains("3000"));
    }

    #[test]
    fn limits_sanitize_hostile_values() {
        let hostile = Limits {
            max_depth: 99,
            max_children: 0,
            max_concurrent: 0,
            max_steps: 0,
            max_tool_calls: 0,
            max_total_tokens: 10,
            max_input_tokens: u64::MAX,
            timeout_secs: 0,
        }
        .sanitized();
        assert_eq!(hostile.max_depth, MAX_DEPTH_HARD);
        assert_eq!(hostile.max_children, 1);
        assert_eq!(hostile.max_total_tokens, MIN_CHILD_TOKENS);
        assert!(hostile.max_input_tokens <= hostile.max_total_tokens);
        assert!(hostile.timeout_secs > 0);
    }

    #[test]
    fn status_machine_is_terminal_sticky() {
        use SubagentStatus::*;
        for (from, to) in [
            (Queued, Running),
            (Queued, Cancelled),
            (Running, Completed),
            (Running, Failed),
            (Running, Cancelled),
        ] {
            assert!(from.can_transition_to(to), "{from:?} -> {to:?}");
        }
        for terminal in [Completed, Failed, Cancelled] {
            assert!(terminal.is_terminal());
            for to in [Queued, Running, Completed, Failed, Cancelled] {
                assert!(!terminal.can_transition_to(to));
            }
        }
        assert!(!Queued.can_transition_to(Completed));
        for status in [Queued, Running, Completed, Failed, Cancelled] {
            assert_eq!(SubagentStatus::parse(status.as_str()), Some(status));
        }
        assert!(SubagentStatus::parse("paused").is_none());
    }

    #[test]
    fn child_spec_ignores_privileged_fields() {
        let spec = ChildSpec::from_args(&json!({
            "task": "  summarize repo  ",
            "agent": " reviewer ",
            "model": "claude-sonnet-4",
            "depth": 0,
            "budget": 999999,
        }))
        .expect("valid");
        assert_eq!(spec.task, "summarize repo");
        assert_eq!(spec.agent, "reviewer");
        assert_eq!(spec.model.as_deref(), Some("claude-sonnet-4"));

        let default = ChildSpec::from_args(&json!({"task": "x"})).expect("valid");
        assert_eq!(default.agent, "general");
        assert!(default.model.is_none());

        assert!(ChildSpec::from_args(&json!({"task": "   "})).is_err());
        assert!(ChildSpec::from_args(&json!({})).is_err());
        assert!(ChildSpec::from_args(&json!({"task": "x".repeat(TASK_MAX_CHARS + 1)})).is_err());
    }

    #[test]
    fn tree_tracks_depth_and_cancels_whole_subtree() {
        let mut tree = RunTree::new();
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let a1 = Uuid::new_v4();
        tree.add_root(root);
        tree.add_child(a, root, 1);
        tree.add_child(b, root, 1);
        tree.add_child(a1, a, 2);

        assert_eq!(tree.depth(root), 0);
        assert_eq!(tree.depth(a1), 2);
        assert_eq!(tree.parent(a1), Some(a));
        let mut kids = tree.children_of(root);
        kids.sort_unstable();
        let mut want = vec![a, b];
        want.sort_unstable();
        assert_eq!(kids, want);
        assert_eq!(tree.children_of(a), vec![a1]);
        assert_eq!(tree.len(), 4);

        // 父先子后：root 一定在首位；同层顺序按 uuid 排序（稳定即可）。
        let cancelled = tree.cancel_subtree(root);
        assert_eq!(cancelled[0], root);
        let pos_a = cancelled.iter().position(|x| *x == a).expect("a");
        let pos_a1 = cancelled.iter().position(|x| *x == a1).expect("a1");
        assert!(pos_a < pos_a1, "父节点必须先于孙节点: {cancelled:?}");
        let mut sorted = cancelled.clone();
        sorted.sort_unstable();
        let mut expected = vec![root, a, a1, b];
        expected.sort_unstable();
        assert_eq!(sorted, expected);

        // 重复取消幂等（同一集合）。
        let mut again = tree.cancel_subtree(root);
        again.sort_unstable();
        assert_eq!(again, sorted);

        tree.remove(a1);
        let after = tree.cancel_subtree(root);
        assert_eq!(after.len(), 3);
        assert!(after.contains(&a) && after.contains(&b));
    }

    #[test]
    fn summary_keeps_prefix_and_truncates_body() {
        let short = summarize(SubagentStatus::Completed, " all good ");
        assert_eq!(short, "[completed] all good");

        let long = summarize(SubagentStatus::Failed, &"x".repeat(SUMMARY_MAX_CHARS * 2));
        assert!(long.chars().count() <= SUMMARY_MAX_CHARS);
        assert!(long.starts_with("[failed] "));
        assert!(long.ends_with("..."));
    }
}
