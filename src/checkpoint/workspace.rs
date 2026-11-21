//! 工作区 Git 快照（PRD §7.6）。
//!
//! 设计口径：
//! - 快照 = `git commit-tree` 产生的 commit（不移动分支），再用
//!   `refs/chiron/checkpoints/<run_id>/<seq>` 钉住，防 GC；
//! - 对象存储在仓库自身的 CAS 目录里，不额外拷贝一份工作区；
//! - 恢复只做 `git reset --hard <sha>`：**不删未跟踪文件**，避免把用户
//!   在快照之后新建的文件当成垃圾清掉；
//! - 仓库未初始化/无 git 时返回 `None`，快照只是尽力而为，不能让 run 失败。

use uuid::Uuid;

use crate::agent::{error::AgentError, workspace::ManagerSandbox};

/// 快照 ref 名。用 uuid + seq，天然可排序、可枚举、不会与用户分支冲突。
pub fn snapshot_ref(run_id: Uuid, seq: u32) -> String {
    format!("refs/chiron/checkpoints/{run_id}/{seq}")
}

/// 从 `git commit-tree` 的 stdout 里取 commit sha（最后一行）。
/// 只接受 40/64 位十六进制，避免把告警文本当 sha 存进库。
pub fn parse_commit_sha(stdout: &str) -> Option<String> {
    stdout.lines().rev().map(str::trim).find_map(|line| {
        let ok = matches!(line.len(), 40 | 64) && line.chars().all(|c| c.is_ascii_hexdigit());
        ok.then(|| line.to_ascii_lowercase())
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub commit: String,
    pub reference: String,
}

/// 抓一次工作区快照。`None` 表示这个工作区不是 git 仓库（或不可读），
/// 调用方仍应记录 run 快照——恢复能力退化为"消息 + 预算"。
pub async fn capture(
    sandbox: &ManagerSandbox,
    repo: &str,
    run_id: Uuid,
    seq: u32,
    label: &str,
) -> Result<Option<WorkspaceSnapshot>, AgentError> {
    let message = format!("chiron checkpoint {label} run={run_id} seq={seq}");
    let Some(commit) = sandbox.commit_snapshot(repo, &message).await? else {
        return Ok(None);
    };
    let reference = snapshot_ref(run_id, seq);
    match sandbox.pin_snapshot(repo, &reference, &commit).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!(%run_id, seq, "pin checkpoint ref failed"),
        Err(err) => tracing::warn!(%run_id, seq, error = %err, "pin checkpoint ref failed"),
    }
    Ok(Some(WorkspaceSnapshot { commit, reference }))
}

/// 恢复工作区；返回 git 是否成功（失败只记日志，由调用方决定是否继续）。
pub async fn restore(
    sandbox: &ManagerSandbox,
    repo: &str,
    commit: &str,
) -> Result<bool, AgentError> {
    let out = sandbox.restore_snapshot(repo, commit).await?;
    if !out.success() {
        tracing::warn!(stderr = %out.stderr, commit, "workspace restore failed");
    }
    Ok(out.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_name_is_namespaced_and_stable() {
        let run = Uuid::nil();
        assert_eq!(
            snapshot_ref(run, 3),
            "refs/chiron/checkpoints/00000000-0000-0000-0000-000000000000/3"
        );
        assert_ne!(snapshot_ref(run, 3), snapshot_ref(run, 4));
    }

    #[test]
    fn commit_sha_is_taken_from_last_valid_line() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_commit_sha(&format!("warning: something\n{sha}\n")),
            Some(sha.to_string())
        );
        assert_eq!(
            parse_commit_sha(&format!("{sha}\n{sha}\n")),
            Some(sha.to_string())
        );
        assert_eq!(parse_commit_sha("fatal: not a git repository"), None);
        assert_eq!(parse_commit_sha(""), None);
        // 短 sha / 非十六进制都不认。
        assert_eq!(parse_commit_sha("0123456"), None);
        assert_eq!(parse_commit_sha(&"z".repeat(40)), None);
    }

    #[test]
    fn parse_is_case_insensitive() {
        let upper = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        assert_eq!(parse_commit_sha(upper).unwrap(), upper.to_ascii_lowercase());
    }
}
