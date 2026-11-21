//! 内存落库：列表、写入（冲突合并/覆盖）、编辑、删除、清空、注入装载。

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::model::{self, Entry, Slot, Write};
use crate::error::{ApiResult, AppError};

const COLUMNS: &str = "id, tenant_id, user_id, scope, slot, content, source, confidence, \
                       session_id, created_at, updated_at";
const MAX_CONTENT_CHARS: usize = 4_000;
const LIST_LIMIT: i64 = 500;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct MemoryRow {
    pub id: Uuid,
    pub tenant_id: i16,
    pub user_id: Uuid,
    pub scope: String,
    pub slot: String,
    pub content: String,
    pub source: String,
    pub confidence: f32,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MemoryRow {
    pub fn slot(&self) -> Option<Slot> {
        Slot::parse(&self.slot)
    }

    pub fn as_entry(&self) -> Option<Entry> {
        Some(Entry {
            id: Some(self.id),
            slot: self.slot()?,
            scope: self.scope.clone(),
            content: self.content.clone(),
            source: self.source.clone(),
            confidence: self.confidence,
            updated_at: self.updated_at,
        })
    }
}

pub struct NewMemory {
    pub tenant_id: i16,
    pub user_id: Uuid,
    pub slot: Slot,
    pub scope: String,
    pub session_id: Option<Uuid>,
    pub content: String,
    pub source: String,
    pub confidence: f32,
}

#[derive(Debug, Default)]
pub struct Patch {
    pub content: Option<String>,
    pub slot: Option<Slot>,
    pub source: Option<String>,
    pub confidence: Option<f32>,
}

pub fn validate_content(raw: &str) -> ApiResult<String> {
    let content = raw.trim();
    if content.is_empty() {
        return Err(AppError::bad_request("memory content must not be empty"));
    }
    if content.chars().count() > MAX_CONTENT_CHARS {
        return Err(AppError::bad_request(format!(
            "memory content exceeds {MAX_CONTENT_CHARS} characters"
        )));
    }
    Ok(content.to_string())
}

pub fn validate_source(raw: &str) -> ApiResult<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "user" | "auto" | "import" => Ok(raw.trim().to_ascii_lowercase()),
        _ => Err(AppError::bad_request("source must be user, auto or import")),
    }
}

pub fn validate_confidence(raw: f32) -> ApiResult<f32> {
    if raw.is_finite() {
        Ok(raw.clamp(0.0, 1.0))
    } else {
        Err(AppError::bad_request(
            "confidence must be a number between 0 and 1",
        ))
    }
}

pub async fn list(
    db: &PgPool,
    tenant_id: i16,
    user_id: Uuid,
    scope: Option<&str>,
    slot: Option<Slot>,
) -> ApiResult<Vec<MemoryRow>> {
    let rows = sqlx::query_as::<_, MemoryRow>(&format!(
        "SELECT {COLUMNS} FROM memories \
         WHERE tenant_id = $1 AND user_id = $2 \
           AND ($3::text IS NULL OR scope = $3) \
           AND ($4::text IS NULL OR slot = $4) \
         ORDER BY updated_at DESC, id DESC LIMIT $5"
    ))
    .bind(tenant_id)
    .bind(user_id)
    .bind(scope)
    .bind(slot.map(Slot::as_str))
    .bind(LIST_LIMIT)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// 写入：同一 (scope, slot) 内按 `model::decide` 合并/覆盖，避免重复条目。
pub async fn create(db: &PgPool, new: NewMemory) -> ApiResult<MemoryRow> {
    let content = validate_content(&new.content)?;
    let source = validate_source(&new.source)?;
    let confidence = validate_confidence(new.confidence)?;

    let mut tx = db.begin().await?;
    let existing = sqlx::query_as::<_, MemoryRow>(&format!(
        "SELECT {COLUMNS} FROM memories \
         WHERE tenant_id = $1 AND user_id = $2 AND scope = $3 \
         ORDER BY updated_at DESC, id DESC LIMIT $4"
    ))
    .bind(new.tenant_id)
    .bind(new.user_id)
    .bind(&new.scope)
    .bind(LIST_LIMIT)
    .fetch_all(&mut *tx)
    .await?;
    let entries: Vec<Entry> = existing.iter().filter_map(MemoryRow::as_entry).collect();

    let incoming = Entry {
        id: None,
        slot: new.slot,
        scope: new.scope.clone(),
        content: content.clone(),
        source: source.clone(),
        confidence,
        updated_at: Utc::now(),
    };
    let slot_name = new.slot.as_str();
    let row = match model::decide(&entries, &incoming) {
        Write::Insert => {
            sqlx::query_as::<_, MemoryRow>(&format!(
                "INSERT INTO memories \
                   (tenant_id, user_id, scope, slot, content, source, confidence, session_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLUMNS}"
            ))
            .bind(new.tenant_id)
            .bind(new.user_id)
            .bind(&new.scope)
            .bind(slot_name)
            .bind(&content)
            .bind(&source)
            .bind(confidence)
            .bind(new.session_id)
            .fetch_one(&mut *tx)
            .await?
        }
        Write::Replace {
            id: Some(id),
            content,
        } => {
            sqlx::query_as::<_, MemoryRow>(&format!(
                "UPDATE memories SET content = $3, source = $4, confidence = $5, \
                     session_id = COALESCE($6, session_id), updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
            ))
            .bind(id)
            .bind(new.tenant_id)
            .bind(content)
            .bind(&source)
            .bind(confidence)
            .bind(new.session_id)
            .fetch_one(&mut *tx)
            .await?
        }
        // 旧条目置信度更高：不覆盖，但仍刷新 session 归属并返回它。
        Write::Keep { id: Some(id) } => {
            sqlx::query_as::<_, MemoryRow>(&format!(
                "UPDATE memories SET session_id = COALESCE($2, session_id), updated_at = now() \
                 WHERE id = $1 RETURNING {COLUMNS}"
            ))
            .bind(id)
            .bind(new.session_id)
            .fetch_one(&mut *tx)
            .await?
        }
        Write::Replace { id: None, .. } | Write::Keep { id: None } => {
            return Err(AppError::internal("memory conflict without a row id"));
        }
    };
    tx.commit().await?;
    Ok(row)
}

pub async fn update(
    db: &PgPool,
    tenant_id: i16,
    user_id: Uuid,
    id: Uuid,
    patch: Patch,
) -> ApiResult<MemoryRow> {
    if patch.content.is_none()
        && patch.slot.is_none()
        && patch.source.is_none()
        && patch.confidence.is_none()
    {
        return Err(AppError::bad_request("nothing to update"));
    }
    let content = patch.content.as_deref().map(validate_content).transpose()?;
    let source = patch.source.as_deref().map(validate_source).transpose()?;
    let confidence = patch.confidence.map(validate_confidence).transpose()?;

    let current = sqlx::query_as::<_, MemoryRow>(&format!(
        "SELECT {COLUMNS} FROM memories WHERE id = $1 AND tenant_id = $2 AND user_id = $3"
    ))
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::not_found("memory not found"))?;

    let slot = patch
        .slot
        .unwrap_or_else(|| current.slot().unwrap_or(Slot::Facts));
    if slot.single_valued() && slot.as_str() != current.slot {
        let occupied: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM memories \
             WHERE tenant_id = $1 AND user_id = $2 AND scope = $3 AND slot = $4 AND id <> $5",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&current.scope)
        .bind(slot.as_str())
        .bind(id)
        .fetch_optional(db)
        .await?;
        if occupied.is_some() {
            return Err(AppError::conflict(
                "a memory already occupies this single-valued slot",
            ));
        }
    }

    let row = sqlx::query_as::<_, MemoryRow>(&format!(
        "UPDATE memories SET \
           content = COALESCE($3, content), \
           slot = COALESCE($4, slot), \
           source = COALESCE($5, source), \
           confidence = COALESCE($6, confidence), \
           updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(tenant_id)
    .bind(content)
    .bind(patch.slot.map(Slot::as_str))
    .bind(source)
    .bind(confidence)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::not_found("memory not found"))?;
    Ok(row)
}

pub async fn delete(db: &PgPool, tenant_id: i16, user_id: Uuid, id: Uuid) -> ApiResult<()> {
    let affected =
        sqlx::query("DELETE FROM memories WHERE id = $1 AND tenant_id = $2 AND user_id = $3")
            .bind(id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(db)
            .await?
            .rows_affected();
    if affected == 0 {
        return Err(AppError::not_found("memory not found"));
    }
    Ok(())
}

/// 清空：`scope` 缺省为全部；也可只清某个槽位。
pub async fn clear(
    db: &PgPool,
    tenant_id: i16,
    user_id: Uuid,
    scope: Option<&str>,
    slot: Option<Slot>,
) -> ApiResult<u64> {
    let affected = sqlx::query(
        "DELETE FROM memories WHERE tenant_id = $1 AND user_id = $2 \
           AND ($3::text IS NULL OR scope = $3) \
           AND ($4::text IS NULL OR slot = $4)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(scope)
    .bind(slot.map(Slot::as_str))
    .execute(db)
    .await?
    .rows_affected();
    Ok(affected)
}

/// 装配注入块：global + 当前会话级条目，按预算裁剪。
///
/// 失败只记日志并返回空串——内存不可用不应让整轮对话失败。
pub async fn load_prompt(
    db: &PgPool,
    tenant_id: i16,
    user_id: Uuid,
    session_id: Uuid,
    budget_tokens: u64,
) -> String {
    if budget_tokens == 0 {
        return String::new();
    }
    let scope = model::session_scope(session_id);
    let rows = sqlx::query_as::<_, MemoryRow>(&format!(
        "SELECT {COLUMNS} FROM memories \
         WHERE tenant_id = $1 AND user_id = $2 AND (scope = 'global' OR scope = $3) \
         ORDER BY updated_at DESC, id DESC LIMIT $4"
    ))
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(LIST_LIMIT)
    .fetch_all(db)
    .await;

    match rows {
        Ok(rows) => {
            let entries: Vec<Entry> = rows.iter().filter_map(MemoryRow::as_entry).collect();
            model::render(&entries, Some(&scope), budget_tokens)
        }
        Err(err) => {
            tracing::warn!(error = %err, "load memories failed; continue without memory");
            String::new()
        }
    }
}
