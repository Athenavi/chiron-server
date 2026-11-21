//! S1：租户库开通 —— 建角色、建库、跑租户迁移、建 owner。
//!
//! 开通是可重试的幂等流程：
//! 1. CAS `pending|failed → provisioning`，保证同一时刻只有一个执行者；
//! 2. 角色/库按 `IF NOT EXISTS` 语义处理，已存在则复用并修正所有权；
//! 3. 租户库跑内嵌迁移；已有 owner 就不再生成临时密码；
//! 4. 成功 `→ active`，失败 `→ failed` 并记录脱敏后的错误。
//!
//! 临时密码只以 AES-256-GCM 密文落控制库，操作员一次性领取后即清除。

use std::time::Duration;

use rand::{distributions::Alphanumeric, Rng};
use sqlx::postgres::PgPoolOptions;
use url::Url;
use uuid::Uuid;

use crate::{
    auth::{generate_temp_password, hash_password},
    error::AppError,
    state::AppState,
};

use super::{crypto, fetch_tenant, platform_audit, ControlState, TenantRow};

const PROVISION_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// slug → 库名/角色名：`acme-team` → `chiron_t_acme_team`。
pub fn db_ident(slug: &str) -> String {
    format!(
        "chiron_t_{}",
        slug.trim().to_ascii_lowercase().replace('-', "_")
    )
}

/// 把管理 DSN 改写成某租户库的 DSN（保留 host/port/sslmode 等查询参数）。
pub fn tenant_dsn(admin_url: &str, db: &str, user: &str, password: &str) -> Result<String, String> {
    let mut url =
        Url::parse(admin_url).map_err(|e| format!("invalid provision database url: {e}"))?;
    url.set_username(user)
        .map_err(|_| "provision url cannot carry a username".to_string())?;
    url.set_password(Some(password))
        .map_err(|_| "provision url cannot carry a password".to_string())?;
    url.set_path(&format!("/{db}"));
    Ok(url.to_string())
}

pub(crate) fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn random_secret(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

struct Provisioned {
    dsn_enc: String,
    owner_temp_password: Option<String>,
    had_owner: bool,
}

/// 开通入口：平台 API 同步调用，也由创建租户后的后台任务调用。
pub async fn provision_tenant(state: &AppState, tenant_id: Uuid) -> Result<TenantRow, AppError> {
    let control = state.control.as_ref().ok_or_else(|| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "control_unavailable",
            "control plane is not configured",
        )
    })?;

    let admin_url = state.cfg.provisioning_database().to_string();
    if admin_url.is_empty() {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            "PROVISION_DATABASE_URL / CONTROL_DATABASE_URL is not configured",
        ));
    }
    let key = state.cfg.secrets_key.clone();
    crypto::decode_key(&key).map_err(|msg| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "provision_unavailable",
            msg,
        )
    })?;

    let claimed = sqlx::query_as::<_, TenantRow>(
        "UPDATE tenants
            SET status = 'provisioning', provision_error = '', updated_at = now()
          WHERE id = $1 AND status IN ('pending', 'failed')
          RETURNING id, slug, name, status, plan_id, region, owner_email, db_name,
                    provision_error, provisioned_at, created_at, updated_at",
    )
    .bind(tenant_id)
    .fetch_optional(&control.db)
    .await?;

    let Some(tenant) = claimed else {
        let current = fetch_tenant(&control.db, tenant_id).await?;
        return Ok(current);
    };

    let role_password = random_secret(40);
    let outcome = tokio::time::timeout(
        PROVISION_TIMEOUT,
        run_steps(&tenant, &admin_url, &role_password, &key),
    )
    .await;

    match outcome {
        Ok(Ok(provisioned)) => {
            let owner_enc = match provisioned.owner_temp_password.as_deref() {
                Some(pw) => Some(
                    crypto::seal(&key, tenant_id.as_bytes(), pw)
                        .map_err(|msg| AppError::internal(format!("seal owner password: {msg}")))?,
                ),
                None => None,
            };
            let updated = sqlx::query_as::<_, TenantRow>(
                "UPDATE tenants
                    SET status = 'active', db_user = $2, dsn_enc = $3,
                        owner_temp_password_enc = $4, provisioned_at = now(),
                        schema_version = $5, migrated_at = now(),
                        provision_error = '', updated_at = now()
                  WHERE id = $1
                  RETURNING id, slug, name, status, plan_id, region, owner_email, db_name,
                            provision_error, provisioned_at, created_at, updated_at",
            )
            .bind(tenant_id)
            .bind(&tenant.db_name)
            .bind(&provisioned.dsn_enc)
            .bind(owner_enc)
            .bind(crate::tenant::target_schema_version())
            .fetch_one(&control.db)
            .await?;
            platform_audit(
                state,
                None,
                "system",
                "tenant_provisioned",
                Some(tenant_id),
                &format!(
                    "db={} owner_created={}",
                    tenant.db_name, !provisioned.had_owner
                ),
                "",
            )
            .await;
            Ok(updated)
        }
        Ok(Err(raw)) => {
            finish_failed(control, tenant_id, &raw, &role_password).await;
            platform_audit(
                state,
                None,
                "system",
                "tenant_provision_failed",
                Some(tenant_id),
                &format!("db={}", tenant.db_name),
                "",
            )
            .await;
            Err(AppError::internal(format!(
                "tenant provisioning failed: {}",
                redact(&raw, &role_password)
            )))
        }
        Err(_) => {
            let raw = format!(
                "provisioning timed out after {}s",
                PROVISION_TIMEOUT.as_secs()
            );
            finish_failed(control, tenant_id, &raw, &role_password).await;
            Err(AppError::internal(raw))
        }
    }
}

fn redact(raw: &str, secret: &str) -> String {
    raw.replace(secret, "***").chars().take(300).collect()
}

async fn run_steps(
    tenant: &TenantRow,
    admin_url: &str,
    role_password: &str,
    key: &str,
) -> Result<Provisioned, String> {
    let ident = tenant.db_name.as_str();
    if ident.is_empty()
        || !ident
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("tenant db_name is not a safe identifier".into());
    }

    let admin = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect(admin_url)
        .await
        .map_err(|e| format!("connect provisioning database: {e}"))?;

    let role_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(ident)
            .fetch_one(&admin)
            .await
            .map_err(|e| format!("query pg_roles: {e}"))?;
    if !role_exists {
        // 角色名来自校验过的 slug，密码是我们生成的字面量；仍做标识符转义。
        let sql = format!(
            "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION",
            quote_ident(ident),
            role_password.replace('\'', "''")
        );
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .map_err(|e| format!("create role: {e}"))?;
    } else {
        // 重试会重新生成密码；角色已存在时必须同步，否则后面用新 DSN 连接必失败。
        let sql = format!(
            "ALTER ROLE {} WITH LOGIN PASSWORD '{}'",
            quote_ident(ident),
            role_password.replace('\'', "''")
        );
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .map_err(|e| format!("rotate role password: {e}"))?;
    }

    let db_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(ident)
            .fetch_one(&admin)
            .await
            .map_err(|e| format!("query pg_database: {e}"))?;
    if !db_exists {
        let sql = format!(
            "CREATE DATABASE {} OWNER {}",
            quote_ident(ident),
            quote_ident(ident)
        );
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .map_err(|e| format!("create database: {e}"))?;
    } else {
        let sql = format!(
            "ALTER DATABASE {} OWNER TO {}",
            quote_ident(ident),
            quote_ident(ident)
        );
        sqlx::query(&sql)
            .execute(&admin)
            .await
            .map_err(|e| format!("fix database owner: {e}"))?;
    }
    admin.close().await;

    let dsn = tenant_dsn(admin_url, ident, ident, role_password)?;
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect(&dsn)
        .await
        .map_err(|e| format!("connect tenant database: {e}"))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| format!("tenant migrations: {e}"))?;

    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE role = 'owner' AND status <> 'deleted'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("count owners: {e}"))?;

    let mut owner_temp_password = None;
    let had_owner = owners > 0;
    if !had_owner {
        let email = tenant.owner_email.trim().to_ascii_lowercase();
        if email.is_empty() {
            return Err("owner_email is required to create the tenant owner".into());
        }
        let password = generate_temp_password();
        let hash = hash_password(&password).map_err(|e| format!("hash owner password: {e}"))?;
        let display = email.split('@').next().unwrap_or("owner").to_string();
        sqlx::query(
            "INSERT INTO users (tenant_id, email, nickname, name, password_hash,
                                password_set, role, status, must_change_password)
             VALUES (1, $1, NULL, $2, $3, true, 'owner', 'active', true)",
        )
        .bind(&email)
        .bind(&display)
        .bind(hash)
        .execute(&pool)
        .await
        .map_err(|e| format!("create owner: {e}"))?;
        owner_temp_password = Some(password);
    }
    pool.close().await;

    let dsn_enc = crypto::seal(key, tenant.id.as_bytes(), &dsn)?;
    Ok(Provisioned {
        dsn_enc,
        owner_temp_password,
        had_owner,
    })
}

async fn finish_failed(control: &ControlState, tenant_id: Uuid, raw: &str, secret: &str) {
    let safe = redact(raw, secret);
    let _ = sqlx::query(
        "UPDATE tenants SET status = 'failed', provision_error = $2, updated_at = now()
          WHERE id = $1 AND status = 'provisioning'",
    )
    .bind(tenant_id)
    .bind(&safe)
    .execute(&control.db)
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_becomes_safe_identifier() {
        assert_eq!(db_ident("acme"), "chiron_t_acme");
        assert_eq!(db_ident("Acme-Team-1"), "chiron_t_acme_team_1");
        assert_eq!(quote_ident(r#"a"b"#), r#""a""b""#);
    }

    #[test]
    fn tenant_dsn_keeps_host_and_query() {
        let dsn = tenant_dsn(
            "postgresql://admin:pw@db.internal:6432/chiron_control?sslmode=require",
            "chiron_t_acme",
            "chiron_t_acme",
            "s3cret",
        )
        .unwrap();
        let parsed = Url::parse(&dsn).unwrap();
        assert_eq!(parsed.host_str(), Some("db.internal"));
        assert_eq!(parsed.port(), Some(6432));
        assert_eq!(parsed.path(), "/chiron_t_acme");
        assert_eq!(parsed.username(), "chiron_t_acme");
        assert_eq!(parsed.password(), Some("s3cret"));
        assert_eq!(parsed.query(), Some("sslmode=require"));
    }

    #[test]
    fn tenant_dsn_rejects_garbage() {
        assert!(tenant_dsn("not a url", "db", "u", "p").is_err());
    }
}
