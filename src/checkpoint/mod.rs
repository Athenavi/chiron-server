//! B7：检查点 / 恢复 / 巡检（PRD §7.6）。
//!
//! 模块划分：
//! - [`workspace`]：工作区 Git 快照（`git commit-tree` + `refs/chiron/...`）；
//! - [`snapshot`]：run 快照结构、状态机与预算游标（纯逻辑，可单测）；
//! - [`store`]：快照落库、最新快照查找、保留策略与 GC；
//! - [`reconciler`]：租约过期扫描 → 接管/判死（决策是纯函数，扫描是后台任务）；
//! - [`api`]：`POST /runs/{id}/recover`、`GET|POST /runs/{id}/checkpoints`。
//!
//! 一致性口径：
//! - 恢复用租约 CAS（`runs.lease_owner/lease_until`）防重复接管；
//! - 恢复不重跑已完成的工具调用：快照里 `settled_tool_calls` 保留，崩溃时
//!   `running/awaiting_approval` 的工具一律标 failed（幂等台账）；
//! - 巡检只在租约过期后动手，绝不抢一个还活着的 run。

// 二进制 crate 里这些公共面与 provider 同口径，先关掉 dead_code。
#![allow(dead_code)]

pub mod api;
pub mod reconciler;
pub mod snapshot;
pub mod store;
pub mod workspace;

use std::env;

#[derive(Clone, Debug)]
pub struct CheckpointConfig {
    /// 关掉后不抓快照、不启 reconciler（recover 接口返回 409）。
    pub enabled: bool,
    /// reconciler 扫描间隔。
    pub interval_secs: u64,
    /// 接管时租约时长：另一实例要等这么久之后才能再抢。
    pub lease_secs: u64,
    /// 租约刚过期时的宽限期（给原实例一次收尾机会）。
    pub hot_window_secs: u64,
    /// 超过这个空闲时长仍无快照 → 判 abandoned（不再等待）。
    pub cold_window_secs: u64,
    /// 同一个 run 保留多少个快照（超出按 seq 从旧到新删）。
    pub retain_per_run: i64,
    /// 快照行过期时间（GC 只删过期的非活跃 run）。
    pub retention_days: i64,
    /// 是否抓工作区 git 快照。
    pub workspace: bool,
    /// 工作区仓库相对路径（`/workspace` 下）。
    pub repo: String,
    /// 接管尝试上限：超过后直接判 failed，避免无限自动重试。
    pub max_attempts: i32,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            lease_secs: 60,
            hot_window_secs: 120,
            cold_window_secs: 3_600,
            retain_per_run: 20,
            retention_days: 7,
            workspace: true,
            repo: ".".to_string(),
            max_attempts: 3,
        }
    }
}

impl CheckpointConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            enabled: bool_env("CHIRON_CHECKPOINT_ENABLED", d.enabled),
            interval_secs: u64_env("CHIRON_CHECKPOINT_INTERVAL_SECS", d.interval_secs).max(5),
            lease_secs: u64_env("CHIRON_CHECKPOINT_LEASE_SECS", d.lease_secs).max(10),
            hot_window_secs: u64_env("CHIRON_CHECKPOINT_HOT_WINDOW_SECS", d.hot_window_secs),
            cold_window_secs: u64_env("CHIRON_CHECKPOINT_COLD_WINDOW_SECS", d.cold_window_secs)
                .max(60),
            retain_per_run: i64_env("CHIRON_CHECKPOINT_RETAIN_PER_RUN", d.retain_per_run).max(1),
            retention_days: i64_env("CHIRON_CHECKPOINT_RETENTION_DAYS", d.retention_days).max(1),
            workspace: bool_env("CHIRON_CHECKPOINT_WORKSPACE", d.workspace),
            repo: env::var("CHIRON_CHECKPOINT_REPO")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or(d.repo),
            max_attempts: i32_env("CHIRON_CHECKPOINT_MAX_ATTEMPTS", d.max_attempts).max(1),
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

fn bool_env(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn u64_env(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn i64_env(name: &str, default: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn i32_env(name: &str, default: i32) -> i32 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_ordered() {
        let cfg = CheckpointConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.workspace);
        assert!(cfg.interval_secs >= 5);
        assert!(cfg.lease_secs > 0);
        assert!(cfg.cold_window_secs > cfg.hot_window_secs);
        assert!(cfg.retain_per_run >= 1);
        assert!(cfg.retention_days >= 1);
        assert!(cfg.max_attempts >= 1);
        assert!(!CheckpointConfig::disabled().enabled);
    }
}
