//! `user_containers.node`：容器是节点本地资源，会话按节点亲和（PRD §4.2）。
//!
//! v1 单节点：所有容器在同一节点，`node` 默认取 `CHIRON_NODE_ID`；
//! 接口先就绪，供 B12 的跨节点路由与故障重建使用。**不做**跨节点迁移。

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::ApiResult,
    sessions::{load_session, ok},
    state::AppState,
};

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ContainerRow {
    pub user_id: Uuid,
    pub tenant_id: i16,
    pub node: String,
    pub container_name: Option<String>,
    pub status: String,
    pub last_seen_at: DateTime<Utc>,
}

/// 读取某用户的容器归属（不存在返回 `None`）。
pub async fn container_of(
    db: &sqlx::PgPool,
    user_id: Uuid,
) -> Result<Option<ContainerRow>, sqlx::Error> {
    sqlx::query_as::<_, ContainerRow>(
        "SELECT user_id, tenant_id, node, container_name, status, last_seen_at \
         FROM user_containers WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// 会话应该落在哪个节点：容器记录优先，缺行时用本实例节点。
#[allow(dead_code)] // B12 的跨节点路由入口。
pub async fn node_for_user(state: &AppState, user_id: Uuid) -> Result<String, sqlx::Error> {
    Ok(container_of(&state.db(), user_id)
        .await?
        .filter(|row| !row.node.trim().is_empty())
        .map(|row| row.node)
        .unwrap_or_else(|| state.cfg.multi.node_id.clone()))
}

/// 落库/刷新容器归属（沙箱创建与心跳调用；B12 的容器生命周期接入）。
#[allow(dead_code)]
pub async fn upsert_container(
    db: &sqlx::PgPool,
    tenant_id: i16,
    user_id: Uuid,
    node: &str,
    container_name: Option<&str>,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_containers (tenant_id, user_id, node, container_name, status, last_seen_at) \
         VALUES ($1, $2, $3, $4, $5, now()) \
         ON CONFLICT (user_id) DO UPDATE SET node = EXCLUDED.node, \
             container_name = EXCLUDED.container_name, status = EXCLUDED.status, \
             last_seen_at = now()",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node)
    .bind(container_name)
    .bind(status)
    .execute(db)
    .await?;
    Ok(())
}

/// `GET /api/v1/sessions/{id}/container`：会话的节点亲和（只读）。
///
/// 返回 `container_node`（容器实际所在节点）与 `serving_node`（当前实例），
/// `local=true` 表示可以直接在本实例上 docker exec。
pub async fn session_container(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, id).await?;
    let container = container_of(&state.db(), session.user_id).await?;
    let node = container
        .as_ref()
        .map(|row| row.node.clone())
        .unwrap_or_else(|| state.cfg.multi.node_id.clone());
    let serving_node = state.cfg.multi.node_id.clone();
    Ok(ok(json!({
        "session_id": id,
        "user_id": session.user_id,
        "container_node": node,
        "serving_node": serving_node,
        "local": node == serving_node,
        "container": container,
        "migration": "unsupported", // v1 不做跨节点迁移（PRD §4.2）
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_row_serializes_affinity_fields() {
        let row = ContainerRow {
            user_id: Uuid::nil(),
            tenant_id: 1,
            node: "node-a".into(),
            container_name: Some("chiron-agent-t1-u1".into()),
            status: "running".into(),
            last_seen_at: Utc::now(),
        };
        let value = serde_json::to_value(&row).expect("serialize");
        assert_eq!(value["node"], "node-a");
        assert_eq!(value["status"], "running");
    }
}
