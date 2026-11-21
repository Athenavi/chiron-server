//! B5：Agent 主循环（PRD §7）。
//!
//! 模块划分：
//! - [`context`]：系统提示 + 历史裁剪 + 预算；
//! - [`tools`] / [`workspace`]：工具注册表、策略与沙箱执行；
//! - [`web`]：Rust 侧 `web_fetch`（SSRF 防护）；
//! - [`runner`]：provider 流式 → 工具循环 → 回填（不碰 DB，可回放测试）；
//! - [`control`]：取消令牌、run registry、steer 队列与审批唤醒；
//! - [`persist`]：把循环的 `RunSink` 落到 PG 与 `sessions::events`。
//!
//! 入口是 [`start_run`]：`POST /sessions/{id}/messages` 创建 queued run 后调用。

// 二进制 crate 里这些是 B6–B10 的公共面（子 Agent、检查点、MCP 会复用）；
// 与 provider 模块同一口径，先在模块级关掉 dead_code，避免门禁误报。
#![allow(dead_code)]

pub mod context;
pub mod control;
pub mod error;
pub mod persist;
pub mod runner;
pub mod tools;
pub mod web;
pub mod workspace;

pub use control::registry;
pub use persist::start_run;
