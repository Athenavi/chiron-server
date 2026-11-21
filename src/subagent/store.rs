//! `subagent_runs` 的 PG 读写与 `subagent.*` 事件。
//!
//! 事件全部发到**父会话**的 SSE 频道（PRD §7.5），前端因此能在父对话里看到
//! 子 Agent 的进度；落库永远先于发事件（与 runs 同口径）。
//!
//! 本模块的准入只是"补齐活跃数"，真正的判定在 [`super::tree::admit`]；
//! 读不到活跃数时按失败关闭（宁可拒绝派生，也不放开并发上限）。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{
    tree::{self, ChildBudget, ChildSpec, SubagentError, SubagentStatus},
    ParentContext, SubagentConfig, SubagentHost,
};
use crate::{agent::error::AgentError, provider::Usage, sessions::event_log};

/// 与迁移里的部分索引同口径（活跃子 Agent 判定）。
const ACTIVE: &str = "'queued', 'running'";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SubagentRow {
    pub id: Uuid,
    pub parent_run_id: Uuid,
    pub root_run_id: Uuid,
    pub parent_subagent_id: Option<Uuid>,
    pub session_id: Uuid,
    pub depth: i16,
    pub agent: String,
    pub task: String,
    pub status: String,
    pub model: Option<String>,
    pub budget: Value,
    pub usage: Value,
    pub summary: Option<String>,
    pub error: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub const SUBAGENT_COLUMNS: &str =
    "id, parent_run_id, root_run_id, parent_subagent_id, session_id, \
     depth, agent, task, status, model, budget, usage, summary, error, \
     created_at, updated_at, started_at, finished_at";

/// 父 run 的子树。`root_run_id` 让"整棵树"一次查全，不必递归。
pub async fn list_for_run(
    db: &PgPool,
    run_id: Uuid,
    limit: i64,
) -> Result<Vec<SubagentRow>, AgentError> {
    let sql = format!(
        "SELECT {SUBAGENT_COLUMNS} FROM subagent_runs \
         WHERE root_run_id = $1 OR parent_run_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT $2"
    );
    sqlx::query_as::<_, SubagentRow>(&sql)
        .bind(run_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Option<SubagentRow>, AgentError> {
    let sql = format!("SELECT {SUBAGENT_COLUMNS} FROM subagent_runs WHERE id = $1");
    sqlx::query_as::<_, SubagentRow>(&sql)
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

/// 会话内的全部子 Agent（跨父 run；前端"子运行树"用它）。
pub async fn list_for_session(
    db: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> Result<Vec<SubagentRow>, AgentError> {
    let sql = format!(
        "SELECT {SUBAGENT_COLUMNS} FROM subagent_runs WHERE session_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT $2"
    );
    sqlx::query_as::<_, SubagentRow>(&sql)
        .bind(session_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))
}

/// 取消子 Agent 及其整棵子树（递归 CTE，终态粘滞）。
pub async fn cancel_subtree(db: &PgPool, id: Uuid, reason: &str) -> Result<Vec<Uuid>, AgentError> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "WITH RECURSIVE tree AS ( \
             SELECT id FROM subagent_runs WHERE id = $1 \
             UNION ALL \
             SELECT s.id FROM subagent_runs s JOIN tree t ON s.parent_subagent_id = t.id \
         ) \
         UPDATE subagent_runs SET status = 'cancelled', \
             error = jsonb_build_object('reason', $2::text), \
             finished_at = now(), updated_at = now() \
         WHERE id IN (SELECT id FROM tree) AND status IN ('queued', 'running') \
         RETURNING id",
    )
    .bind(id)
    .bind(reason)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 父 run 被取消/会话删除时的联动：整棵子树全部取消。
pub async fn cancel_for_run(
    db: &PgPool,
    parent_run_id: Uuid,
    reason: &str,
) -> Result<Vec<Uuid>, AgentError> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE subagent_runs SET status = 'cancelled', \
             error = jsonb_build_object('reason', $2::text), \
             finished_at = now(), updated_at = now() \
         WHERE (parent_run_id = $1 OR root_run_id = $1) AND status IN ('queued', 'running') \
         RETURNING id",
    )
    .bind(parent_run_id)
    .bind(reason)
    .fetch_all(db)
    .await
    .map_err(|e| AgentError::Internal(e.to_string()))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 生产端口：准入读活跃数，落库写 `subagent_runs`，事件发父会话。
pub struct PgHost {
    db: PgPool,
    cfg: SubagentConfig,
    parent: ParentContext,
    model: String,
}

impl PgHost {
    pub fn new(db: PgPool, cfg: SubagentConfig, parent: ParentContext, model: String) -> Self {
        Self {
            db,
            cfg,
            parent,
            model,
        }
    }

    fn publish(&self, kind: &str, data: Value) {
        event_log::global().append(
            self.parent.session_id,
            Some(self.parent.root_run_id),
            kind,
            data,
        );
    }

    /// `(同一父下活跃数, 整棵树活跃数)`；失败时按上限已满处理（失败关闭）。
    async fn active_counts(&self) -> Result<(usize, usize), SubagentError> {
        let row: (i64, i64) = sqlx::query_as(&format!(
            "SELECT \
               (SELECT count(*) FROM subagent_runs WHERE parent_run_id = $1 AND status IN ({ACTIVE})), \
               (SELECT count(*) FROM subagent_runs WHERE root_run_id = $2 AND status IN ({ACTIVE}))"
        ))
        .bind(self.parent.parent_run_id)
        .bind(self.parent.root_run_id)
        .fetch_one(&self.db)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "subagent active count failed");
            SubagentError::Invalid("cannot verify subagent concurrency".into())
        })?;
        Ok((row.0.max(0) as usize, row.1.max(0) as usize))
    }
}

#[async_trait]
impl SubagentHost for PgHost {
    async fn admit(&self, depth: usize) -> Result<ChildBudget, SubagentError> {
        let (siblings, tree_active) = self.active_counts().await?;
        tree::admit(
            self.cfg.limits,
            tree::AdmissionRequest {
                depth,
                active_siblings: siblings,
                active_tree: tree_active,
                remaining_tokens: self.parent.remaining_tokens,
            },
        )
    }

    async fn started(
        &self,
        spec: &ChildSpec,
        depth: usize,
        budget: ChildBudget,
    ) -> Result<Uuid, AgentError> {
        let budget_json = json!({
            "max_input_tokens": budget.max_input_tokens,
            "max_total_tokens": budget.max_total_tokens,
            "max_steps": budget.max_steps,
            "max_tool_calls": budget.max_tool_calls,
        });
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO subagent_runs \
               (parent_run_id, root_run_id, parent_subagent_id, session_id, depth, agent, task, \
                status, model, budget, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'running', $8, $9, now()) \
             RETURNING id",
        )
        .bind(self.parent.parent_run_id)
        .bind(self.parent.root_run_id)
        .bind(self.parent.parent_subagent_id)
        .bind(self.parent.session_id)
        .bind(depth as i16)
        .bind(spec.agent.as_str())
        .bind(spec.task.as_str())
        .bind(self.model.as_str())
        .bind(&budget_json)
        .fetch_one(&self.db)
        .await
        .map_err(|e| AgentError::Internal(e.to_string()))?;

        self.publish(
            "subagent.started",
            json!({
                "subagent_id": id,
                "parent_run_id": self.parent.parent_run_id,
                "agent": spec.agent,
                "depth": depth,
                "task": spec.task,
                "budget": budget_json,
            }),
        );
        Ok(id)
    }

    async fn delta(&self, id: Uuid, text: &str) {
        self.publish(
            "subagent.delta",
            json!({ "subagent_id": id, "delta": text }),
        );
    }

    async fn finished(&self, id: Uuid, status: SubagentStatus, summary: &str, usage: &Usage) {
        let usage_json = serde_json::to_value(usage).unwrap_or_else(|_| json!({}));
        let result = sqlx::query(
            "UPDATE subagent_runs SET status = $2, summary = $3, usage = $4, \
                 finished_at = now(), updated_at = now() \
             WHERE id = $1 AND status IN ('queued', 'running')",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(summary)
        .bind(&usage_json)
        .execute(&self.db)
        .await;
        match result {
            Ok(done) if done.rows_affected() == 0 => {
                // 已被取消（终态粘滞）：不再发 completed，避免两个终态。
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, %id, "persist subagent completion failed");
                return;
            }
        }
        self.publish(
            "subagent.completed",
            json!({
                "subagent_id": id,
                "status": status.as_str(),
                "summary": summary,
                "usage": usage_json,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_status_literals_match_migration() {
        // 迁移里的部分索引与这里必须同口径；改一处就要改两处。
        let migration = include_str!("../../migrations/0001_init.sql");
        assert!(migration.contains("ix_subagent_runs_active"));
        for status in ["queued", "running"] {
            assert!(
                migration.contains(&format!("'{}'", status)),
                "missing {status}"
            );
        }
    }

    #[test]
    fn columns_cover_row_fields() {
        for field in [
            "parent_run_id",
            "root_run_id",
            "parent_subagent_id",
            "depth",
            "summary",
            "budget",
            "usage",
        ] {
            assert!(SUBAGENT_COLUMNS.contains(field), "missing {field}");
        }
    }
}
