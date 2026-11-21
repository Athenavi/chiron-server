use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 统一错误体：`{"error":{"code","message"}}`（见 docs/API.md §1/§2）。
/// `code` 使用契约里的稳定字符串，前端据此分支；HTTP 状态码与 code 一一对应。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AppError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

pub type ApiResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }

    /// 未登录 / 令牌无效 / 令牌已吊销 / 账号不可用：对外统一 401，避免账号枚举。
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "auth.required", msg)
    }

    /// 登录失败锁定；`retry_after` 秒后重试。
    pub fn locked(retry_after: i64) -> Self {
        let wait = retry_after.max(1);
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "auth.locked",
            format!("too many attempts; retry in {wait}s"),
        )
    }

    /// 首次登录 / 管理员重置密码后必须先改密，其余接口一律拒绝。
    pub fn password_change_required() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "auth.password_change_required",
            "password change required before using this service",
        )
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", msg)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg)
    }

    /// axum 的 `Json` 拒绝信息默认是纯文本，这里统一收敛成错误包络。
    pub fn invalid_body(rej: JsonRejection) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("invalid request body: {}", rej.body_text()),
        )
    }

    /// 路径参数（如 Uuid）解析失败同样走统一错误包络。
    pub fn invalid_path_param() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid path parameter",
        )
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "database error");
        AppError::internal("database error")
    }
}

impl From<redis::RedisError> for AppError {
    fn from(e: redis::RedisError) -> Self {
        tracing::error!(error = %e, "redis error");
        AppError::internal("session store unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约 §2 的错误码映射：前端只认 code，不能因重构而漂移。
    #[test]
    fn contract_error_codes_map_to_status() {
        let e = AppError::unauthorized("invalid or expired token");
        assert_eq!(e.status, StatusCode::UNAUTHORIZED);
        assert_eq!(e.code, "auth.required");

        let e = AppError::locked(0);
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.code, "auth.locked");
        assert!(e.message.contains("retry in 1s"), "message: {}", e.message);

        let e = AppError::password_change_required();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(e.code, "auth.password_change_required");

        assert_eq!(AppError::forbidden("x").code, "forbidden");
        assert_eq!(AppError::not_found("x").status, StatusCode::NOT_FOUND);
        assert_eq!(AppError::not_found("x").code, "not_found");
        assert_eq!(AppError::conflict("x").status, StatusCode::CONFLICT);
        assert_eq!(AppError::conflict("x").code, "conflict");
        assert_eq!(AppError::bad_request("x").status, StatusCode::BAD_REQUEST);
        assert_eq!(AppError::internal("x").code, "internal_error");
    }

    #[tokio::test]
    async fn error_body_is_unified_envelope() {
        use http_body_util::BodyExt;

        let resp = AppError::forbidden("insufficient permissions").into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(
            v.get("data").is_none(),
            "error responses must not carry data"
        );
        assert_eq!(v["error"]["code"], "forbidden");
        assert_eq!(v["error"]["message"], "insufficient permissions");
    }
}
