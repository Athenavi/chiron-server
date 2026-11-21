//! 检查点的落库、最新快照查找、保留策略与恢复台账。
//!
//! 顺序口径与 runs 一致：先写 PG，再发 `checkpoint.created`。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{
    snapshot::{
        BudgetCursor, CheckpointKind, CheckpointStatus, RunSnapshot, SnapshotMessage, SnapshotTool,
    },
    workspace,
};
use crate::{
    agent::{error::AgentError, workspace::ManagerSandbox},
    sessions::{event_log, run_state::ACTIVE_STATUS_SQL},
    state::AppState,
};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CheckpointRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub seq: i32,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub snapshot: Value,
    pub workspace_commit: Option<String>,
    pub workspace_ref: Option<String>,
    pub size_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

const CHECKPOINT_COLUMNS: &str = "id, run_id, session_id, seq, kind, label, status, snapshot, \
     workspace_commit, workspace_ref, size_bytes, created_at, expires_at";

/// 同一个 run 内的下一个快照序号。
pub async fn next_seq(db: &PgPool, run_id: Uuid) -> Result<u32, AgentError> {
    let (next,): (i32,) =
        sqlx::query_as("SELECT COALESCE(max(seq), -1) + 1 FROM checkpoints WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(db)
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
    Ok(next.max(0) as u32)
}

/// 读快照所需的最小状态：run/session/turn + 消息 + 工具 + 用量。
pub async fn build_snapshot(
    db: &PgPool,
    run_id: Uuid,
    seq: u32,
    remaining_tokens: u64,
) -> Result<RunSnapshot, AgentError> {
    let run: Option<(Uuid, Option<Uuid>, Value, Option<String>)> = sqlx::query_as(
        "SELECT r.session_id, r.turn_id, r.usage, t.model FROM runs r \
         LEFT JOIN turns t ON t.id = r.turn_id WHERE r.id = $1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    let Some((session_id, turn_id, usage, model)) = run else {
        return Err(AgentError::Internal(format!("run {run_id} not found")));
    };

    let messages: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT role, kind, content FROM messages WHERE run_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT 500",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;

    let tools: Vec<(Uuid, String, String, Value)> = sqlx::query_as(
        "SELECT id, name, status, args FROM tool_calls WHERE run_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT 500",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;

    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut snapshot = RunSnapshot::new(run_id, session_id, turn_id, seq);
    snapshot.model = model;
    snapshot.remaining_tokens = remaining_tokens;
    snapshot.messages = messages
        .into_iter()
        .map(|(role, kind, content)| SnapshotMessage {
            role,
            kind,
            content,
        })
        .collect();
    snapshot.tools = tools
        .into_iter()
        .map(|(tool_call_id, name, status, args)| SnapshotTool {
            tool_call_id,
            name,
            status,
            args,
        })
        .collect();
    snapshot.budget = BudgetCursor {
        steps_used: snapshot
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .count(),
        tool_calls_used: snapshot.tools.len(),
        input_tokens,
        output_tokens,
    };
    Ok(snapshot)
}

/// 写一行快照（`(run_id, seq)` 唯一，重复写按最新覆盖）。
pub struct NewCheckpoint<'a> {
    pub kind: CheckpointKind,
    pub label: &'a str,
    pub status: CheckpointStatus,
    pub workspace_commit: Option<&'a str>,
    pub workspace_ref: Option<&'a str>,
    pub retention_days: i64,
}

pub async fn insert(
    db: &PgPool,
    snapshot: &RunSnapshot,
    entry: NewCheckpoint<'_>,
) -> Result<Uuid, AgentError> {
    let payload = serde_json::to_value(snapshot).unwrap_or_else(|_| json!({}));
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO checkpoints \
           (run_id, session_id, seq, kind, label, status, snapshot, workspace_commit, \
            workspace_ref, size_bytes, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                 now() + make_interval(days => $11::int)) \
         ON CONFLICT (run_id, seq) DO UPDATE SET \
            status = EXCLUDED.status, snapshot = EXCLUDED.snapshot, \
            workspace_commit = EXCLUDED.workspace_commit, \
            workspace_ref = EXCLUDED.workspace_ref, \
            size_bytes = EXCLUDED.size_bytes, updated_at = now() \
         RETURNING id",
    )
    .bind(snapshot.run_id)
    .bind(snapshot.session_id)
    .bind(snapshot.seq as i32)
    .bind(entry.kind.as_str())
    .bind(entry.label)
    .bind(entry.status.as_str())
    .bind(payload)
    .bind(entry.workspace_commit)
    .bind(entry.workspace_ref)
    .bind(snapshot.size_bytes() as i64)
    .bind(entry.retention_days)
    .fetch_one(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    Ok(id)
}

/// 最近一个可用快照（`ready|restored`）。
pub async fn latest_usable(db: &PgPool, run_id: Uuid) -> Result<Option<CheckpointRow>, AgentError> {
    let sql = format!(
        "SELECT {CHECKPOINT_COLUMNS} FROM checkpoints \
         WHERE run_id = $1 AND status IN ('ready', 'restored') \
         ORDER BY seq DESC, created_at DESC LIMIT 1"
    );
    sqlx::query_as::<_, CheckpointRow>(&sql)
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

pub async fn list(db: &PgPool, run_id: Uuid, limit: i64) -> Result<Vec<CheckpointRow>, AgentError> {
    let sql = format!(
        "SELECT {CHECKPOINT_COLUMNS} FROM checkpoints WHERE run_id = $1 \
         ORDER BY seq DESC, created_at DESC LIMIT $2"
    );
    sqlx::query_as::<_, CheckpointRow>(&sql)
        .bind(run_id)
        .bind(limit.clamp(1, 100))
        .fetch_all(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

/// 会话级快照列表（契约 §Checkpoints）。
pub async fn list_for_session(
    db: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> Result<Vec<CheckpointRow>, AgentError> {
    let sql = format!(
        "SELECT {CHECKPOINT_COLUMNS} FROM checkpoints WHERE session_id = $1 \
         ORDER BY created_at DESC, seq DESC LIMIT $2"
    );
    sqlx::query_as::<_, CheckpointRow>(&sql)
        .bind(session_id)
        .bind(limit.clamp(1, 100))
        .fetch_all(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Option<CheckpointRow>, AgentError> {
    let sql = format!("SELECT {CHECKPOINT_COLUMNS} FROM checkpoints WHERE id = $1");
    sqlx::query_as::<_, CheckpointRow>(&sql)
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

pub async fn mark_status(
    db: &PgPool,
    id: Uuid,
    status: CheckpointStatus,
) -> Result<bool, AgentError> {
    let affected = sqlx::query(
        "UPDATE checkpoints SET status = $2, updated_at = now() WHERE id = $1 AND status <> $2",
    )
    .bind(id)
    .bind(status.as_str())
    .execute(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?
    .rows_affected();
    Ok(affected > 0)
}

/// 抓一份完整快照（run 状态 + 可选工作区 git），并发 `checkpoint.created`。
///
/// 任何一步失败都**不让 run 失败**：检查点只是恢复能力的增强。
pub async fn capture(
    state: &Arc<AppState>,
    run_id: Uuid,
    label: &str,
) -> Result<Option<Uuid>, AgentError> {
    let cfg = &state.cfg.checkpoint;
    if !cfg.enabled {
        return Ok(None);
    }
    let seq = next_seq(&state.db(), run_id).await?;
    let mut snapshot =
        build_snapshot(&state.db(), run_id, seq, state.cfg.agent.max_total_tokens).await?;
    if let Err(err) = snapshot.validate(None) {
        tracing::warn!(%run_id, %err, "skip invalid checkpoint snapshot");
        return Ok(None);
    }

    let mut workspace_commit = None;
    let mut workspace_ref = None;
    let mut kind = CheckpointKind::Run;
    if cfg.workspace {
        let owner: Option<(i16, Uuid, Option<String>)> =
            sqlx::query_as("SELECT tenant_id, user_id, project_id FROM sessions WHERE id = $1")
                .bind(snapshot.session_id)
                .fetch_optional(&state.db())
                .await
                .map_err(|e| AgentError::Internal(e.to_string()))?;
        if let Some((tenant_id, user_id, project_id)) = owner {
            let sandbox = ManagerSandbox::new(
                state.sandbox.clone(),
                tenant_id,
                user_id.to_string(),
                project_id,
            );
            match workspace::capture(&sandbox, &cfg.repo, run_id, seq, label).await {
                Ok(Some(snap)) => {
                    snapshot.workspace_commit = Some(snap.commit.clone());
                    workspace_commit = Some(snap.commit);
                    workspace_ref = Some(snap.reference);
                    kind = CheckpointKind::Both;
                }
                Ok(None) => {
                    tracing::info!(%run_id, "workspace is not a git repo; storing run snapshot only");
                }
                Err(err) => tracing::warn!(%run_id, error = %err, "workspace snapshot failed"),
            }
        }
    }

    let id = insert(
        &state.db(),
        &snapshot,
        NewCheckpoint {
            kind,
            label,
            status: CheckpointStatus::Ready,
            workspace_commit: workspace_commit.as_deref(),
            workspace_ref: workspace_ref.as_deref(),
            retention_days: cfg.retention_days,
        },
    )
    .await?;

    event_log::global().append(
        snapshot.session_id,
        Some(run_id),
        "checkpoint.created",
        json!({ "checkpoint_id": id, "kind": kind.as_str(), "label": label, "seq": seq }),
    );
    Ok(Some(id))
}

/// 恢复前的台账整理：崩溃时仍在跑的工具标 failed、挂起的审批取消。
/// 这样恢复后的循环不会把"半截工具"当成已完成，也不会重复执行已完成的调用。
pub async fn settle_interrupted_tools(db: &PgPool, run_id: Uuid) -> Result<(u64, u64), AgentError> {
    let tools = sqlx::query(
        "UPDATE tool_calls SET status = 'failed', error = 'interrupted by recovery', \
             updated_at = now() \
         WHERE run_id = $1 AND status IN ('running', 'awaiting_approval', 'pending')",
    )
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?
    .rows_affected();

    let approvals = sqlx::query(
        "UPDATE approvals SET status = 'cancelled', decided_at = now() \
         WHERE run_id = $1 AND status = 'pending'",
    )
    .bind(run_id)
    .execute(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?
    .rows_affected();
    Ok((tools, approvals))
}

/// 租约 CAS：只有过期/未占用/属于本实例的活跃 run 能被接管。
///
/// 返回 `false` 表示已被别的实例抢走（或 run 已经终态），调用方必须放弃。
pub async fn acquire_lease(
    db: &PgPool,
    run_id: Uuid,
    owner: &str,
    lease_secs: u64,
) -> Result<bool, AgentError> {
    let sql = format!(
        "UPDATE runs SET lease_owner = $2, \
             lease_until = now() + make_interval(secs => $3::int), \
             recover_attempts = recover_attempts + 1, updated_at = now() \
         WHERE id = $1 AND status IN ({ACTIVE_STATUS_SQL}) \
           AND (lease_owner IS NULL OR lease_owner = $2 \
                OR lease_until IS NULL OR lease_until < now())"
    );
    let affected = sqlx::query(&sql)
        .bind(run_id)
        .bind(owner)
        .bind(lease_secs as i32)
        .execute(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?
        .rows_affected();
    Ok(affected == 1)
}

/// 巡检/恢复判定失败时写的终态（只对活跃 run 生效）。
pub async fn mark_failed(
    db: &PgPool,
    run_id: Uuid,
    code: &str,
    message: &str,
) -> Result<Option<(Uuid, Option<Uuid>)>, AgentError> {
    let sql = format!(
        "UPDATE runs SET status = 'failed', \
             error = jsonb_build_object('code', $2::text, 'message', $3::text), \
             finished_at = now(), updated_at = now() \
         WHERE id = $1 AND status IN ({ACTIVE_STATUS_SQL}) \
         RETURNING session_id, turn_id"
    );
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(&sql)
        .bind(run_id)
        .bind(code)
        .bind(message)
        .fetch_optional(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;
    if let Some((session_id, turn_id)) = row {
        event_log::global().append(
            session_id,
            Some(run_id),
            "run.failed",
            json!({ "code": code, "message": message }),
        );
        if let Some(turn_id) = turn_id {
            let _ = sqlx::query("UPDATE turns SET status = 'failed', finished_at = now() WHERE id = $1 AND status IN ('queued', 'running', 'waiting_approval', 'waiting_user')")
                .bind(turn_id)
                .execute(db)
                .await;
        }
    }
    Ok(row)
}

/// 保留策略的纯逻辑：返回应该删掉的 seq（旧的先删，保留最新 `keep` 个）。
pub fn retention_candidates(seqs: &[i32], keep: i64) -> Vec<i32> {
    if keep <= 0 {
        return seqs.to_vec();
    }
    let keep = keep as usize;
    if seqs.len() <= keep {
        return Vec::new();
    }
    let mut sorted = seqs.to_vec();
    sorted.sort_unstable();
    let cut = sorted.len() - keep;
    sorted.into_iter().take(cut).collect()
}

/// GC：删过期行 + 每个 run 只保留 `keep` 个最新快照。
/// 只动非活跃 run 之外的过期行，绝不删仍在跑的 run 的最新快照。
pub async fn gc(db: &PgPool, keep: i64) -> Result<u64, AgentError> {
    let expired = sqlx::query(&format!(
        "DELETE FROM checkpoints c WHERE c.expires_at < now() \
           AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = c.run_id AND r.status IN ({ACTIVE_STATUS_SQL}))"
    ))
    .execute(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?
    .rows_affected();

    let excess = sqlx::query(
        "DELETE FROM checkpoints c WHERE c.seq NOT IN ( \
             SELECT seq FROM checkpoints k WHERE k.run_id = c.run_id \
             ORDER BY k.seq DESC LIMIT $1 \
         )",
    )
    .bind(keep.max(1))
    .execute(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?
    .rows_affected();
    Ok(expired + excess)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_keeps_newest_and_drops_oldest() {
        assert_eq!(retention_candidates(&[0, 1, 2, 3, 4], 2), vec![0, 1, 2]);
        assert!(retention_candidates(&[0, 1], 5).is_empty());
        assert_eq!(retention_candidates(&[], 3), Vec::<i32>::new());
        assert_eq!(
            retention_candidates(&[5, 1, 3], 0),
            vec![5, 1, 3],
            "keep=0 视为全删（调用方会先 clamp 到 1）"
        );
        // 乱序输入按 seq 排序后再决定。
        assert_eq!(retention_candidates(&[7, 2, 9], 2), vec![2]);
    }

    #[test]
    fn migration_matches_store_contract() {
        let migration = include_str!("../../migrations/0001_init.sql");
        for needle in [
            "ux_checkpoints_run_seq",
            "checkpoints_status_check",
            "('created', 'ready', 'restored', 'discarded')",
            "expires_at",
            "workspace_commit",
        ] {
            assert!(migration.contains(needle), "missing {needle}");
        }
        for column in ["recover_attempts", "last_checkpoint_seq"] {
            assert!(migration.contains(column), "missing {column}");
        }
        // 列清单与实际行结构保持同步。
        for column in [
            "run_id",
            "session_id",
            "seq",
            "kind",
            "status",
            "snapshot",
            "workspace_ref",
            "size_bytes",
        ] {
            assert!(CHECKPOINT_COLUMNS.contains(column), "missing {column}");
        }
    }
}
