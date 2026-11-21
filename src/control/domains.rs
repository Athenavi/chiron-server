//! S8：自定义域名 TXT 验证。
//!
//! 租户在域名下配置 `TXT chiron-verify=<tenant_uuid>`；平台侧触发解析并校验。
//! 子域名 `<slug>.<base>` 由平台自动创建，视为已信任，不走 TXT。
//! TLS 仍由反代 on-demand 签发；这里只负责"域名确实归该租户"。

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use hickory_resolver::TokioAsyncResolver;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    control::{fetch_tenant, platform_audit, DomainRow, PlatformAdmin},
    error::{ApiResult, AppError},
    state::AppState,
};

pub const VERIFY_PREFIX: &str = "chiron-verify";

pub fn expected_txt(tenant_id: Uuid) -> String {
    format!("{VERIFY_PREFIX}={tenant_id}")
}

pub async fn verify_domain(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path((id, domain_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    let tenant = fetch_tenant(&control.db, id).await?;
    let row: Option<(String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT host, kind, verified_at FROM tenant_domains
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(domain_id)
    .bind(id)
    .fetch_optional(&control.db)
    .await?;
    let (host, kind, _) = row.ok_or_else(|| AppError::not_found("domain not found"))?;
    if kind != "custom" {
        return Err(AppError::bad_request(
            "subdomain hosts are auto-trusted; only custom domains need TXT verification",
        ));
    }

    let expected = expected_txt(tenant.id);
    let resolver = TokioAsyncResolver::tokio_from_system_conf().map_err(|e| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "dns_unavailable",
            format!("resolver init failed: {e}"),
        )
    })?;
    let lookup = resolver
        .txt_lookup(host.trim_end_matches('.').to_string())
        .await;
    let found = match lookup {
        Ok(records) => records.iter().any(|record| {
            record
                .txt_data()
                .iter()
                .any(|chunk| chunk.as_ref() == expected.as_bytes())
        }),
        Err(e) => {
            return Err(AppError::bad_request(format!(
                "TXT lookup failed for {host}: {e}"
            )))
        }
    };
    if !found {
        return Err(AppError::bad_request(format!(
            "TXT record not found; expected \"{expected}\" on {host}"
        )));
    }

    let updated = sqlx::query_as::<_, DomainRow>(
        "UPDATE tenant_domains SET verified_at = now()
          WHERE id = $1 AND tenant_id = $2
          RETURNING id, tenant_id, host, kind, verified_at, tls_status",
    )
    .bind(domain_id)
    .bind(id)
    .fetch_one(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_domain_verified",
        Some(id),
        &host,
        "",
    )
    .await;
    Ok(Json(json!({ "data": { "domain": updated } })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct TlsAskQuery {
    #[serde(default)]
    pub domain: Option<String>,
}

/// `GET /api/platform/v1/tls/allow?domain=...`：Caddy on-demand TLS 的 ask 端点。
///
/// 2xx = 允许签发/续期；其它 = 拒绝。只允许平台 Host、活跃租户的子域名，以及
/// 已验证的自定义域名。端点本身不返回租户信息，避免枚举；生产环境应在反代层限制
/// 只允许 Caddy 访问。
pub async fn tls_allow(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TlsAskQuery>,
) -> Response {
    let Some(domain) = normalize_ask_domain(q.domain.as_deref()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let platform = normalize_ask_domain(Some(&state.cfg.platform_host));
    if platform.as_deref() == Some(domain.as_str()) {
        return StatusCode::OK.into_response();
    }
    let Some(control) = state.control.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let allowed: Result<Option<(i32,)>, sqlx::Error> = sqlx::query_as(
        "SELECT 1 FROM tenant_domains d JOIN tenants t ON t.id = d.tenant_id \
         WHERE lower(d.host) = $1 AND t.status = 'active' \
           AND (d.kind = 'subdomain' OR d.verified_at IS NOT NULL) LIMIT 1",
    )
    .bind(&domain)
    .fetch_optional(&control.db)
    .await;
    match allowed {
        Ok(Some(_)) => {
            // 只把 pending 推进到 requested；issued/failed 不被 ask 覆盖。
            let _ = sqlx::query(
                "UPDATE tenant_domains SET tls_status = 'requested' \
                 WHERE lower(host) = $1 AND tls_status = 'pending'",
            )
            .bind(&domain)
            .execute(&control.db)
            .await;
            StatusCode::OK.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "tls ask lookup failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// 归一化 ask 里的域名：小写、去末尾点与端口，拒绝通配符/空值/非法字符。
pub fn normalize_ask_domain(raw: Option<&str>) -> Option<String> {
    let mut value = raw?.trim().to_ascii_lowercase();
    if value.ends_with('.') {
        value.pop();
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        if port.chars().all(|c| c.is_ascii_digit()) {
            value = host.to_string();
        }
    }
    if value.is_empty()
        || value.len() > 253
        || value.contains('*')
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }
    if value
        .split('.')
        .any(|part| part.is_empty() || part.len() > 63)
    {
        return None;
    }
    Some(value)
}

/// `POST /api/platform/v1/tenants/{id}/domains/{domain_id}/probe-tls`：
/// 立即从控制面探测 `https://<host>/healthz`，回写 `tls_status` 并把失败原因
/// 截断后返回控制台（通常用于自动探测受网络路径影响时的现场确认）。
pub async fn probe_tls_domain(
    State(state): State<Arc<AppState>>,
    admin: PlatformAdmin,
    Path((id, domain_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let control = state.control.as_ref().ok_or_else(control_unavailable)?;
    fetch_tenant(&control.db, id).await?;
    let row: Option<(String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT host, kind, verified_at FROM tenant_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(domain_id)
    .bind(id)
    .fetch_optional(&control.db)
    .await?;
    let (host, kind, verified_at) =
        row.ok_or_else(|| AppError::not_found("domain not found"))?;
    if kind == "custom" && verified_at.is_none() {
        return Err(AppError::bad_request(
            "custom domain must be TXT-verified before TLS probing",
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::internal(format!("tls probe client: {e}")))?;
    let url = format!("https://{host}/healthz");
    let (status, error) = match client.head(&url).send().await {
        Ok(_) => ("issued", String::new()),
        Err(e) => ("failed", truncate_probe_error(&e.to_string())),
    };
    sqlx::query(
        "UPDATE tenant_domains SET tls_status = $2, tls_checked_at = now() \
         WHERE id = $1 AND tenant_id = $3",
    )
    .bind(domain_id)
    .bind(status)
    .bind(id)
    .execute(&control.db)
    .await?;
    platform_audit(
        &state,
        Some(admin.id),
        &admin.email,
        "tenant_domain_tls_probed",
        Some(id),
        &format!("host={host} status={status}"),
        "",
    )
    .await;
    Ok(Json(json!({ "data": {
        "domain_id": domain_id,
        "host": host,
        "tls_status": status,
        "error": error,
    } }))
    .into_response())
}

fn truncate_probe_error(raw: &str) -> String {
    raw.chars().take(300).collect()
}

fn control_unavailable() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "control_unavailable",
        "control plane is not configured (CONTROL_DATABASE_URL)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_ask_domains_are_normalized_and_bounded() {
        assert_eq!(
            normalize_ask_domain(Some(" Tenant.Example.COM. ")).as_deref(),
            Some("tenant.example.com")
        );
        assert_eq!(
            normalize_ask_domain(Some("tenant.example.com:443")).as_deref(),
            Some("tenant.example.com")
        );
        assert_eq!(normalize_ask_domain(None), None);
        assert_eq!(normalize_ask_domain(Some("")), None);
        assert_eq!(normalize_ask_domain(Some("*.example.com")), None);
        assert_eq!(normalize_ask_domain(Some("bad..example.com")), None);
        assert_eq!(normalize_ask_domain(Some("a/b")), None);
    }

    #[test]
    fn expected_txt_embeds_tenant_id() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        assert_eq!(
            expected_txt(id),
            "chiron-verify=11111111-2222-3333-4444-555555555555"
        );
    }
}
