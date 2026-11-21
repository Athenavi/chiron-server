//! `GET /api/v1/admin/sandbox/containers`：沙箱容器列表（只读，owner/admin）。
//!
//! 复用 `SandboxManager::list_containers`（`docker ps -a --filter label`），
//! 从 `main.rs` 迁入管理模块，响应形状保持不变（`data.containers`）。
//! 清理动作（`reclaim_idle` / `cleanup_user`）留到管理后台的交互设计确定后再接。

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    response::Response,
};
use serde_json::json;

use crate::{
    admin::{ensure_admin, ok},
    auth::CurrentUser,
    error::AppError,
    state::AppState,
};

pub async fn list_containers(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
) -> Result<Response, AppError> {
    ensure_admin(&actor)?;
    let mut items = state.sandbox.list_containers().await?;
    // SaaS 下只能看本租户；桌面单库形态没有租户上下文，保持全量。
    if let Some(tenant) = crate::tenant::current_tenant() {
        items.retain(|item| has_tenant_label(&item.labels, tenant));
    }
    Ok(ok(json!({ "containers": items })))
}

fn has_tenant_label(labels: &str, tenant: uuid::Uuid) -> bool {
    let wanted = format!("chiron.tenant-id={tenant}");
    labels.split(',').any(|pair| pair.trim() == wanted)
}

/// 容器状态的粗分类，供面板做"运行中/已退出"两个计数（纯函数，便于单测）。
pub fn count_by_state(statuses: impl IntoIterator<Item = String>) -> (usize, usize) {
    let mut running = 0;
    let mut stopped = 0;
    for status in statuses {
        if status.trim_start().starts_with("Up") {
            running += 1;
        } else {
            stopped += 1;
        }
    }
    (running, stopped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_states_are_split_into_running_and_stopped() {
        let (running, stopped) = count_by_state([
            "Up 2 hours".to_string(),
            "Exited (0) 1 hour ago".to_string(),
            "Created".to_string(),
            "Up 5 seconds (healthy)".to_string(),
        ]);
        assert_eq!(running, 2);
        assert_eq!(stopped, 2);
    }

    #[test]
    fn tenant_label_filter_rejects_other_tenants() {
        let tenant = uuid::Uuid::from_u128(7);
        let other = uuid::Uuid::from_u128(8);
        assert!(has_tenant_label(
            &format!("chiron.managed-by=agent-platform,chiron.tenant-id={tenant}"),
            tenant
        ));
        assert!(!has_tenant_label(
            &format!("chiron.managed-by=agent-platform,chiron.tenant-id={other}"),
            tenant
        ));
    }

    #[test]
    fn empty_container_list_counts_zero() {
        let (running, stopped) = count_by_state(Vec::new());
        assert_eq!((running, stopped), (0, 0));
    }
}
