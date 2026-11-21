//! B8：内存（结构化槽位 + 会话摘要）。
//!
//! v1 不引入 embeddings：persona/preferences/facts 作为结构化槽位落 PG，
//! 会话摘要走 `summary` 槽位。注入见 [`store::load_prompt`] 与
//! [`crate::agent::context::system_prompt_with_memory`]。
//!
//! 模块划分：
//! - [`model`]：纯逻辑（槽位、作用域、冲突决策、预算裁剪渲染），无 DB；
//! - [`store`]：PG 读写与注入装载；
//! - [`api`]：`/api/v1/memories` 路由。

mod api;
mod model;
mod store;

pub use api::router;
pub use store::load_prompt;

#[cfg(test)]
mod tests {
    use super::model::{self, Entry, Slot, Write};
    use uuid::Uuid;

    /// 端到端的纯函数链路：写入 → 冲突 → 删除 → 注入，不依赖 DB。
    #[test]
    fn write_patch_delete_round_trip_keeps_injection_consistent() {
        let session = Uuid::new_v4();
        let scope = model::session_scope(session);
        let mut entries: Vec<Entry> = Vec::new();

        let mut persona = Entry::new(Slot::Persona, model::GLOBAL_SCOPE, "Senior Rust dev");
        persona.id = Some(Uuid::new_v4());
        assert_eq!(model::apply(&mut entries, persona.clone()), Write::Insert);

        // 同一 persona 再写一次：单值槽位，高置信度覆盖。
        let mut summary = Entry::new(Slot::Summary, scope.clone(), "debugging sandbox leases");
        summary.id = Some(Uuid::new_v4());
        summary.source = "auto".to_string();
        model::apply(&mut entries, summary);

        let replacement = Entry::new(Slot::Persona, model::GLOBAL_SCOPE, "Staff Rust dev");
        let plan = model::apply(&mut entries, replacement);
        assert!(matches!(plan, Write::Replace { .. }));
        assert_eq!(
            entries.iter().filter(|e| e.slot == Slot::Persona).count(),
            1
        );

        let block = model::render(&entries, Some(&scope), 200);
        assert!(block.contains("Staff Rust dev"));
        assert!(block.contains("sandbox leases"));

        // 删除 persona 后，注入块里不再出现它。
        let persona_id = entries
            .iter()
            .find(|e| e.slot == Slot::Persona)
            .unwrap()
            .id
            .unwrap();
        model::remove(&mut entries, persona_id);
        let block = model::render(&entries, Some(&scope), 200);
        assert!(!block.contains("Staff Rust dev"));
        assert!(block.contains("sandbox leases"));
    }
}
