//! 内存的纯逻辑：槽位、作用域、冲突决策与注入渲染。
//!
//! 这里不碰 DB，目的是让「预算裁剪 / 删除后不再注入 / 跨会话可见性 / 冲突覆盖」
//! 这些出口行为能单测覆盖，不必依赖真实 PG。
//!
//! 注入优先级：persona > preferences > facts > summary。persona/summary 是单值槽位
//! （同一作用域只保留一条），facts 按 key 去重，preferences 按内容去重。

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub const GLOBAL_SCOPE: &str = "global";
/// 单条记忆进入提示词前的字符上限，避免一条超长记忆挤掉其余槽位。
pub const MAX_ITEM_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Persona,
    Preferences,
    Facts,
    Summary,
}

impl Slot {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "persona" => Some(Self::Persona),
            "preferences" | "preference" => Some(Self::Preferences),
            "facts" | "fact" => Some(Self::Facts),
            "summary" => Some(Self::Summary),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Preferences => "preferences",
            Self::Facts => "facts",
            Self::Summary => "summary",
        }
    }

    /// 注入顺序；数字小者先注入。
    pub fn priority(self) -> u8 {
        match self {
            Self::Persona => 0,
            Self::Preferences => 1,
            Self::Facts => 2,
            Self::Summary => 3,
        }
    }

    /// 单值槽位：同一作用域内新的写入覆盖旧的（受置信度约束）。
    pub fn single_valued(self) -> bool {
        matches!(self, Self::Persona | Self::Summary)
    }
}

pub fn session_scope(session_id: Uuid) -> String {
    format!("session:{session_id}")
}

/// 作用域归一化：只接受 `global` 或 `session:<uuid>`。
///
/// 显式 `global` 优先；未给 scope 时才由 session_id 推出会话作用域。
pub fn normalize_scope(raw: Option<&str>, session_id: Option<Uuid>) -> Result<String, String> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(session_id
            .map(session_scope)
            .unwrap_or_else(|| GLOBAL_SCOPE.to_string())),
        Some(GLOBAL_SCOPE) => Ok(GLOBAL_SCOPE.to_string()),
        Some(value) => {
            let id = value
                .strip_prefix("session:")
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| "scope must be `global` or `session:<uuid>`".to_string())?;
            Ok(session_scope(id))
        }
    }
}

/// 从 `session:<uuid>` 作用域取回会话 id；global 返回 None。
pub fn scope_session_id(scope: &str) -> Option<Uuid> {
    if scope == GLOBAL_SCOPE {
        return None;
    }
    scope
        .strip_prefix("session:")
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// 一条记忆（与 DB 行解耦，测试可直接构造）。
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: Option<Uuid>,
    pub slot: Slot,
    pub scope: String,
    pub content: String,
    pub source: String,
    pub confidence: f32,
    pub updated_at: DateTime<Utc>,
}

impl Entry {
    /// 跨会话可见性：global 始终可见，会话级只在该会话可见。
    pub fn visible_in(&self, session_scope: Option<&str>) -> bool {
        if self.scope == GLOBAL_SCOPE {
            return true;
        }
        session_scope.is_some_and(|scope| scope == self.scope)
    }
}

/// 构造辅助只在测试/批量导入里用；生产代码直接构造 [`Entry`] 字面量。
#[cfg(test)]
impl Entry {
    pub fn new(slot: Slot, scope: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: None,
            slot,
            scope: scope.into(),
            content: content.into(),
            source: "user".to_string(),
            confidence: 1.0,
            updated_at: Utc::now(),
        }
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence;
        self
    }
}

/// 写入决策，`store` 据此落到 INSERT / UPDATE。
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Insert,
    Replace { id: Option<Uuid>, content: String },
    Keep { id: Option<Uuid> },
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// facts 冲突键：`键: 值` / `键=值` 冒号前的部分；没有分隔符时不参与按键覆盖。
pub fn fact_key(content: &str) -> Option<String> {
    let head = content
        .split([':', '：', '='])
        .next()?
        .trim()
        .to_lowercase();
    let whole = content.trim().to_lowercase();
    if head.is_empty() || head == whole {
        None
    } else {
        Some(head)
    }
}

fn conflicts(existing: &Entry, incoming: &Entry) -> bool {
    if existing.slot != incoming.slot || existing.scope != incoming.scope {
        return false;
    }
    if existing.slot.single_valued() {
        return true;
    }
    if normalize_text(&existing.content) == normalize_text(&incoming.content) {
        return true;
    }
    if existing.slot == Slot::Facts {
        if let (Some(a), Some(b)) = (fact_key(&existing.content), fact_key(&incoming.content)) {
            return a == b;
        }
    }
    false
}

/// 冲突处理：高置信度覆盖，低置信度保留；相同内容视为同一条。
pub fn decide(existing: &[Entry], incoming: &Entry) -> Write {
    for row in existing {
        if !conflicts(row, incoming) {
            continue;
        }
        if incoming.confidence >= row.confidence {
            return Write::Replace {
                id: row.id,
                content: incoming.content.trim().to_string(),
            };
        }
        return Write::Keep { id: row.id };
    }
    Write::Insert
}

/// 纯内存版的写入（测试与批量导入复用）。
#[cfg(test)]
pub fn apply(existing: &mut Vec<Entry>, incoming: Entry) -> Write {
    let plan = decide(existing, &incoming);
    match plan.clone() {
        Write::Insert => existing.push(incoming),
        Write::Replace { id, content } => {
            let target = match id {
                Some(id) => existing.iter_mut().find(|row| row.id == Some(id)),
                // 测试里条目可以没有 id：按冲突规则找回被覆盖的那条。
                None => existing.iter_mut().find(|row| conflicts(row, &incoming)),
            };
            if let Some(row) = target {
                row.content = content;
                row.confidence = incoming.confidence;
                row.source = incoming.source;
                row.updated_at = incoming.updated_at;
            }
        }
        // Keep 表示旧值更可信，保持原样。
        Write::Keep { .. } => {}
    }
    plan
}

#[cfg(test)]
pub fn remove(existing: &mut Vec<Entry>, id: Uuid) {
    existing.retain(|row| row.id != Some(id));
}

pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 按优先级排序后的可见条目。
pub fn select<'a>(entries: &'a [Entry], session_scope: Option<&str>) -> Vec<&'a Entry> {
    let mut visible: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.visible_in(session_scope))
        .collect();
    visible.sort_by(|a, b| {
        a.slot
            .priority()
            .cmp(&b.slot.priority())
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    visible
}

/// 渲染注入块。预算按"字符数 / 4"估算，装不下的低优先级条目直接丢弃；
/// 返回空串表示没有可注入内容（调用方不要追加空标题）。
pub fn render(entries: &[Entry], session_scope: Option<&str>, budget_tokens: u64) -> String {
    if budget_tokens == 0 {
        return String::new();
    }
    let mut out = String::from(
        "## Memory\n\
         The entries below are user memories from earlier sessions. They may be stale: \
         explicit instructions in the conversation always win.\n",
    );
    let mut used = estimate_tokens(&out);
    let mut section: Option<Slot> = None;

    for entry in select(entries, session_scope) {
        let content = truncate(entry.content.trim(), MAX_ITEM_CHARS);
        if content.is_empty() {
            continue;
        }
        let mut addition = String::new();
        if section != Some(entry.slot) {
            addition.push_str(&format!("\n### {}\n", entry.slot.as_str()));
        }
        addition.push_str(&format!("- [{}] {content}\n", entry.source));
        let cost = estimate_tokens(&addition);
        if used + cost > budget_tokens {
            // 低优先级条目按顺序丢弃，保证 persona/preferences 先占预算。
            break;
        }
        used += cost;
        out.push_str(&addition);
        section = Some(entry.slot);
    }

    if section.is_none() {
        return String::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: Slot, scope: &str, content: &str, confidence: f32) -> Entry {
        Entry::new(slot, scope, content).with_confidence(confidence)
    }

    #[test]
    fn slots_parse_and_rank_by_priority() {
        assert_eq!(Slot::parse("Persona"), Some(Slot::Persona));
        assert_eq!(Slot::parse(" facts "), Some(Slot::Facts));
        assert_eq!(Slot::parse("unknown"), None);
        assert!(Slot::Persona.priority() < Slot::Preferences.priority());
        assert!(Slot::Preferences.priority() < Slot::Facts.priority());
        assert!(Slot::Facts.priority() < Slot::Summary.priority());
        assert!(Slot::Persona.single_valued());
        assert!(Slot::Summary.single_valued());
        assert!(!Slot::Facts.single_valued());
    }

    #[test]
    fn scope_is_normalized_and_validated() {
        let id = Uuid::new_v4();
        assert_eq!(normalize_scope(None, None).unwrap(), "global");
        assert_eq!(
            normalize_scope(None, Some(id)).unwrap(),
            format!("session:{id}")
        );
        assert_eq!(normalize_scope(Some("global"), Some(id)).unwrap(), "global");
        assert_eq!(
            normalize_scope(Some("session:garbage"), None),
            Err("scope must be `global` or `session:<uuid>`".to_string())
        );
        assert_eq!(session_scope(id), format!("session:{id}"));
        assert_eq!(scope_session_id(&session_scope(id)), Some(id));
        assert_eq!(scope_session_id(GLOBAL_SCOPE), None);
    }

    #[test]
    fn persona_conflict_overwrites_only_with_enough_confidence() {
        let mut stored = vec![entry(Slot::Persona, GLOBAL_SCOPE, "old persona", 0.6)];
        let plan = apply(
            &mut stored,
            entry(Slot::Persona, GLOBAL_SCOPE, "new persona", 0.9),
        );
        assert!(matches!(plan, Write::Replace { .. }));
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].content, "new persona");

        let plan = apply(
            &mut stored,
            entry(Slot::Persona, GLOBAL_SCOPE, "weak persona", 0.2),
        );
        assert!(matches!(plan, Write::Keep { .. }));
        assert_eq!(stored[0].content, "new persona");
    }

    #[test]
    fn duplicate_preferences_collapse_into_one_row() {
        let mut stored = vec![entry(
            Slot::Preferences,
            GLOBAL_SCOPE,
            "Answer in Chinese",
            0.8,
        )];
        let plan = apply(
            &mut stored,
            entry(Slot::Preferences, GLOBAL_SCOPE, "answer   in chinese", 0.9),
        );
        assert!(matches!(plan, Write::Replace { .. }));
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].confidence, 0.9);

        apply(
            &mut stored,
            entry(Slot::Preferences, GLOBAL_SCOPE, "Prefer tabs", 0.7),
        );
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn facts_conflict_on_same_key_and_keep_distinct_keys() {
        let mut stored = vec![entry(Slot::Facts, GLOBAL_SCOPE, "editor: neovim", 0.5)];
        apply(
            &mut stored,
            entry(Slot::Facts, GLOBAL_SCOPE, "editor: vim", 0.9),
        );
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].content, "editor: vim");

        apply(
            &mut stored,
            entry(Slot::Facts, GLOBAL_SCOPE, "os: linux", 0.6),
        );
        assert_eq!(stored.len(), 2);
        assert_eq!(fact_key("editor: neovim").as_deref(), Some("editor"));
        assert_eq!(fact_key("no separator"), None);
    }

    #[test]
    fn cross_session_reads_global_plus_own_session_only() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let entries = vec![
            entry(Slot::Persona, GLOBAL_SCOPE, "prefers Rust", 1.0),
            entry(Slot::Facts, &session_scope(a), "project: chiron", 1.0),
        ];
        let in_a = render(&entries, Some(&session_scope(a)), 400);
        assert!(in_a.contains("prefers Rust"));
        assert!(in_a.contains("project: chiron"));

        let in_b = render(&entries, Some(&session_scope(b)), 400);
        assert!(in_b.contains("prefers Rust"));
        assert!(!in_b.contains("project: chiron"));
    }

    #[test]
    fn deleted_memory_is_no_longer_injected() {
        let id = Uuid::new_v4();
        let mut kept = entry(Slot::Facts, GLOBAL_SCOPE, "editor: neovim", 1.0);
        kept.id = Some(id);
        let mut entries = vec![
            kept,
            entry(Slot::Preferences, GLOBAL_SCOPE, "Prefer tabs", 1.0),
        ];
        assert!(render(&entries, None, 400).contains("neovim"));
        remove(&mut entries, id);
        let block = render(&entries, None, 400);
        assert!(!block.contains("neovim"));
        assert!(block.contains("Prefer tabs"));
    }

    #[test]
    fn budget_keeps_high_priority_slots_and_drops_facts() {
        let entries = vec![
            entry(Slot::Persona, GLOBAL_SCOPE, "p".repeat(80).as_str(), 1.0),
            entry(Slot::Preferences, GLOBAL_SCOPE, "Prefer tabs", 1.0),
            entry(Slot::Facts, GLOBAL_SCOPE, "editor: neovim", 1.0),
        ];
        let block = render(&entries, None, 400);
        assert!(block.contains("### persona"));
        assert!(block.contains("### facts"));

        let full_persona = render(&entries[..1], None, 1_000);
        // 预算刚好装下「标题 + persona」：偏好与 facts 必须被丢弃。
        let tight_budget = estimate_tokens(&full_persona) + 2;
        let tight = render(&entries, None, tight_budget);
        assert!(tight.contains("### persona"), "{tight}");
        assert!(!tight.contains("### preferences"), "{tight}");
        assert!(!tight.contains("### facts"), "{tight}");
        assert_eq!(render(&entries, None, 0), "");
        assert_eq!(render(&[], None, 100), "");
    }

    #[test]
    fn long_entries_are_truncated_per_item() {
        let long = "x".repeat(600);
        let entries = vec![entry(Slot::Facts, GLOBAL_SCOPE, &long, 1.0)];
        let block = render(&entries, None, 400);
        assert!(block.contains('…'));
        let line = block.lines().find(|l| l.starts_with("- [")).unwrap();
        assert!(line.chars().count() <= MAX_ITEM_CHARS + 12, "{line}");
    }
}
