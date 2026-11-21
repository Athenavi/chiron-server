//! 上下文装配：系统提示 + 历史裁剪 + token 预算。
//!
//! 口径：预算按"字符数 / 4"粗估即可（provider 侧另有真实 usage 记账）。
//! 裁剪从最老的消息开始丢，保证最近上下文完整；被丢部分用一条摘要提示占位，
//! 让模型知道"更早的对话已被省略"，而不是静默截断。

use crate::provider::{ChatMessage, Role};

/// 粗估 token：4 字符 ≈ 1 token，CJK 更密，这里宁可高估。
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

pub fn estimate_messages(messages: &[ChatMessage]) -> u64 {
    messages
        .iter()
        .map(|m| {
            let content = m.content.as_deref().unwrap_or_default();
            let calls = m
                .tool_calls
                .iter()
                .map(|c| c.name.chars().count() + c.arguments.chars().count())
                .sum::<usize>() as u64;
            4 + estimate_tokens(content) + calls.div_ceil(4)
        })
        .sum()
}

/// 默认系统提示。技能/内存（B8/B9）以后拼在 `extra` 里。
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Chiron, a coding agent running inside the user's sandboxed workspace.\n\
- Work only inside /workspace; all file paths you pass to tools are relative to it.\n\
- Prefer reading before editing; keep edits minimal and explain them briefly.\n\
- Use tools when you need facts from the workspace; do not invent file contents.\n\
- When the task is ambiguous, ask the user with the ask_user tool instead of guessing.\n\
- Answer in the user's language.";

pub fn system_prompt(extra: Option<&str>) -> String {
    match extra.map(str::trim).filter(|s| !s.is_empty()) {
        Some(extra) => format!("{DEFAULT_SYSTEM_PROMPT}\n\n{extra}"),
        None => DEFAULT_SYSTEM_PROMPT.to_string(),
    }
}

/// 内存注入最多占总输入预算的 1/4：系统提示与最近历史优先，内存只是补充。
pub fn memory_budget(total_budget: u64) -> u64 {
    total_budget / 4
}

/// 系统提示 + 内存块（B8）。内存为空时不追加任何内容，避免多出空标题。
pub fn system_prompt_with_memory(extra: Option<&str>, memory: Option<&str>) -> String {
    let base = system_prompt(extra);
    match memory.map(str::trim).filter(|s| !s.is_empty()) {
        Some(block) => format!("{base}\n\n{block}"),
        None => base,
    }
}

/// 带内存的装配：内存并进 system 消息，再由 [`assemble`] 统一算预算。
/// 这样内存不会额外占一条消息，历史裁剪口径保持不变。
pub fn assemble_with_memory(
    system: &str,
    memory: Option<&str>,
    history: &[ChatMessage],
    budget_tokens: u64,
) -> Vec<ChatMessage> {
    let system = match memory.map(str::trim).filter(|s| !s.is_empty()) {
        Some(block) => format!("{system}\n\n{block}"),
        None => system.to_string(),
    };
    assemble(&system, history, budget_tokens)
}

/// 历史裁剪：在预算内保留最近的对话，丢弃最老的部分。
///
/// 返回 `(system, kept)`。`budget_tokens` 至少容纳系统提示与最后一条消息，
/// 否则只保留最后一条——宁可超一点也不能给出空上下文。
pub fn assemble(system: &str, history: &[ChatMessage], budget_tokens: u64) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(history.len() + 2);
    out.push(ChatMessage::system(system));

    let mut remaining = budget_tokens.saturating_sub(estimate_tokens(system));
    let mut kept_rev: Vec<ChatMessage> = Vec::new();
    for message in history.iter().rev() {
        let cost = estimate_messages(std::slice::from_ref(message));
        if cost > remaining {
            if kept_rev.is_empty() {
                // 最后一条再大也必须保留，否则模型拿不到用户问题。
                kept_rev.push(message.clone());
            }
            break;
        }
        remaining -= cost;
        kept_rev.push(message.clone());
    }
    let dropped = history.len().saturating_sub(kept_rev.len());
    if dropped > 0 {
        out.push(ChatMessage::system(format!(
            "[context] {dropped} earlier message(s) were omitted to fit the token budget."
        )));
    }
    out.extend(kept_rev.into_iter().rev());
    out
}

/// 历史里的 steer 约束：作为下一轮 user 消息追加，保证"下一轮生效"。
pub fn steer_messages(steers: &[String]) -> Vec<ChatMessage> {
    let joined = steers
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        Vec::new()
    } else {
        vec![ChatMessage::user(format!(
            "[steer] Additional constraints for this turn:\n{joined}"
        ))]
    }
}

/// DB 里的 `messages.content`（字符串或结构化块）转成纯文本。
pub fn content_to_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn role_of(raw: &str) -> Option<Role> {
    match raw {
        "system" => Some(Role::System),
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "tool" => Some(Role::Tool),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimate_is_character_based() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn short_history_is_kept_verbatim_and_ordered() {
        let history = vec![
            ChatMessage::user("first"),
            ChatMessage::assistant("second"),
            ChatMessage::user("third"),
        ];
        let assembled = assemble("sys", &history, 10_000);
        assert_eq!(assembled.len(), 4);
        assert_eq!(assembled[0].role, Role::System);
        assert_eq!(assembled[1].content.as_deref(), Some("first"));
        assert_eq!(assembled[3].content.as_deref(), Some("third"));
    }

    #[test]
    fn trim_drops_oldest_and_leaves_a_note() {
        let history: Vec<ChatMessage> = (0..20)
            .map(|i| ChatMessage::user(format!("message number {i} with some padding text")))
            .collect();
        let assembled = assemble("sys", &history, 40);
        // 第一条是 system，第二条是省略提示。
        assert_eq!(assembled[0].role, Role::System);
        assert_eq!(assembled[1].role, Role::System);
        assert!(assembled[1]
            .content
            .as_deref()
            .unwrap()
            .contains("earlier message"));
        let last = assembled.last().unwrap();
        assert_eq!(
            last.content.as_deref(),
            Some("message number 19 with some padding text")
        );
        assert!(assembled.len() < history.len() + 2);
    }

    #[test]
    fn oversized_last_message_is_kept() {
        let history = vec![ChatMessage::user("x".repeat(10_000))];
        let assembled = assemble("sys", &history, 10);
        assert_eq!(assembled.len(), 2);
        assert_eq!(assembled[1].content.as_deref().unwrap().len(), 10_000);
    }

    #[test]
    fn steer_messages_are_grouped_into_one_user_turn() {
        let msgs = steer_messages(&["a".into(), "  ".into(), "b".into()]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.as_deref().unwrap().contains("[steer]"));
        assert!(steer_messages(&[]).is_empty());
    }

    #[test]
    fn memory_block_joins_system_prompt_and_keeps_trimming_intact() {
        let memory = "## Memory\n### persona\n- [user] Prefers Rust\n";
        let system = system_prompt_with_memory(Some("deploy hint"), Some(memory));
        assert!(system.contains(DEFAULT_SYSTEM_PROMPT));
        assert!(system.contains("deploy hint"));
        assert!(system.contains("Prefers Rust"));

        let history: Vec<ChatMessage> = (0..20)
            .map(|i| ChatMessage::user(format!("message number {i} with some padding text")))
            .collect();
        let assembled = assemble_with_memory("sys", Some(memory), &history, 60);

        // 系统消息仍然只有一条（内存并入其中），随后是省略提示。
        assert_eq!(assembled[0].role, Role::System);
        assert!(assembled[0]
            .content
            .as_deref()
            .unwrap()
            .contains("Prefers Rust"));
        assert_eq!(assembled[1].role, Role::System);
        assert!(assembled[1]
            .content
            .as_deref()
            .unwrap()
            .contains("earlier message"));
        assert!(assembled.len() < history.len() + 3);
        assert_eq!(
            assembled.last().unwrap().content.as_deref(),
            Some("message number 19 with some padding text")
        );
    }

    #[test]
    fn empty_memory_is_not_injected() {
        assert_eq!(system_prompt_with_memory(None, None), DEFAULT_SYSTEM_PROMPT);
        assert_eq!(
            system_prompt_with_memory(None, Some("   \n ")),
            DEFAULT_SYSTEM_PROMPT
        );
        let history = vec![ChatMessage::user("hi")];
        let assembled = assemble_with_memory("sys", Some(""), &history, 10_000);
        assert_eq!(assembled.len(), 2);
        assert_eq!(assembled[0].content.as_deref(), Some("sys"));
    }

    #[test]
    fn memory_budget_is_a_quarter_of_input_budget() {
        assert_eq!(memory_budget(60_000), 15_000);
        assert_eq!(memory_budget(3), 0);
    }

    #[test]
    fn content_blocks_flatten_to_text() {
        assert_eq!(content_to_text(&json!("hello")), "hello");
        assert_eq!(
            content_to_text(&json!([{"type": "text", "text": "a"}, {"text": "b"}])),
            "a\nb"
        );
        assert_eq!(content_to_text(&json!({"text": "c"})), "c");
        assert_eq!(content_to_text(&json!(null)), "");
    }

    #[test]
    fn role_mapping_covers_db_values() {
        assert_eq!(role_of("system"), Some(Role::System));
        assert_eq!(role_of("user"), Some(Role::User));
        assert_eq!(role_of("assistant"), Some(Role::Assistant));
        assert_eq!(role_of("tool"), Some(Role::Tool));
        assert_eq!(role_of("narrator"), None);
    }
}
