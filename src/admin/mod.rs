//! B10 管理面：审计、用量、运行态、沙箱容器与限流。
//!
//! 约定：
//! - 四个查询端点全部只读，不做任何管理写操作；
//! - 角色矩阵只有 owner/admin 能进（`ensure_admin`），admin 限定在自己租户；
//! - 限流是横切关注点：`limits::enforce` 挂在登录路由和受保护路由上，
//!   纯逻辑与 axum 中间件分离，单测不需要数据库或时钟。

pub mod audit;
pub mod containers;
pub mod cursor;
pub mod limits;
pub mod runtime;
pub mod usage;

use std::sync::Arc;

use axum::{
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{auth::CurrentUser, error::AppError, state::AppState};

/// 管理端路由，由 `main.rs` 合并进受保护路由（前面已有 `require_auth`）。
///
/// `/api/v1/admin/sandbox/containers` 原在 `main.rs`，现由本模块提供，
/// 路径与响应形状保持不变。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/admin/audit", get(audit::list_audit))
        .route("/api/v1/admin/usage", get(usage::get_usage))
        .route("/api/v1/admin/runtime", get(runtime::get_runtime))
        .route(
            "/api/v1/admin/sandbox/containers",
            get(containers::list_containers),
        )
}

/// 权限矩阵的唯一入口：owner/admin 放行，其余 403。
pub fn ensure_admin(actor: &CurrentUser) -> Result<(), AppError> {
    if is_admin_role(&actor.role) {
        Ok(())
    } else {
        Err(AppError::forbidden("insufficient permissions"))
    }
}

pub fn is_admin_role(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

pub fn ok(data: Value) -> Response {
    Json(json!({ "data": data })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn actor(role: &str, tenant_id: i16) -> CurrentUser {
        CurrentUser {
            id: Uuid::from_u128(1),
            email: "a@example.com".into(),
            role: role.into(),
            tenant_id,
            tenant: None,
            plan: None,
            must_change: false,
            jti: "jti".into(),
            iat: 0,
            exp: 0,
        }
    }

    /// 管理端权限矩阵（契约 §4）：owner/admin 可用，user 与未知角色 403。
    #[test]
    fn admin_permission_matrix() {
        for role in ["owner", "admin"] {
            let actor = actor(role, 1);
            assert!(ensure_admin(&actor).is_ok(), "{role} must pass");
            assert!(is_admin_role(role));
        }
        for role in ["user", "Admin", "", "root"] {
            let actor = actor(role, 1);
            let err = ensure_admin(&actor).expect_err("must reject");
            assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
            assert_eq!(err.code, "forbidden");
            assert!(!is_admin_role(role));
        }
    }

    #[test]
    fn success_envelope_wraps_data() {
        let resp = ok(json!({ "x": 1 }));
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }
}
