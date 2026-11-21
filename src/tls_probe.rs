//! R-5：域名 TLS 状态探测。
//!
//! Caddy 的 on-demand ask 只能告诉我们"允许签发"，成功/失败不会回调。这里用定时
//! HTTPS 探测把 `tenant_domains.tls_status` 推进到 `issued`/`failed`：
//! * 只探测活跃租户、已信任的子域名或已验证的自定义域名；
//! * 未 issued 的域名才探测，避免每轮打全部域名；
//! * 探测本身失败只写状态与日志，不阻塞其他域名；失败会触发告警 Webhook。
//! * 默认关闭（`CHIRON_TLS_PROBE_SECS=0`），由运维显式开启。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use reqwest::redirect::Policy;
use uuid::Uuid;

use crate::{agent::web::WebClient, state::AppState};

const MIN_SECS: u64 = 300;
const MAX_TARGETS: i64 = 100;

pub fn spawn_scheduler(state: Arc<AppState>) {
    let secs = state.cfg.tls_probe_secs;
    if secs == 0 || state.control.is_none() {
        tracing::info!("tls probe scheduler disabled");
        return;
    }
    tokio::spawn(async move {
        let every = Duration::from_secs(secs.max(MIN_SECS));
        loop {
            tokio::time::sleep(every).await;
            let Some(control) = state.control.clone() else {
                return;
            };
            let owner = crate::sessions::lease::owner_id();
            let key = crate::sessions::lease::leader_key("tls_probe");
            match state.leases.acquire(&key, &owner, every * 2).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "tls probe leader lease failed");
                    continue;
                }
            }

            let rows: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT d.id, d.host FROM tenant_domains d \
                 JOIN tenants t ON t.id = d.tenant_id \
                 WHERE t.status = 'active' AND d.tls_status <> 'issued' \
                   AND (d.kind = 'subdomain' OR d.verified_at IS NOT NULL) \
                 ORDER BY d.created_at ASC LIMIT $1",
            )
            .bind(MAX_TARGETS)
            .fetch_all(&control.db)
            .await
            .unwrap_or_default();

            let guard = WebClient::new(WebClient::allow_private_from_env());
            let (mut issued, mut failed) = (0usize, 0usize);
            for (id, host) in rows {
                let url = format!("https://{host}/healthz");
                let parsed = match url::Url::parse(&url) {
                    Ok(url) => url,
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                };
                let pin = match guard.check_url(&parsed).await {
                    Ok(pin) => pin,
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(%host, error = %e, "tls probe target rejected");
                        continue;
                    }
                };
                let mut builder = reqwest::Client::builder()
                    .timeout(Duration::from_secs(state.cfg.tls_probe_timeout_secs.max(1)))
                    .redirect(Policy::none());
                if let Some((pin_host, addr)) = pin {
                    builder = builder.resolve(&pin_host, addr);
                }
                let client = match builder.build() {
                    Ok(client) => client,
                    Err(e) => {
                        tracing::warn!(error = %e, "tls probe client build failed");
                        continue;
                    }
                };
                match client.head(&url).send().await {
                    Ok(_) => {
                        issued += 1;
                        let _ = sqlx::query(
                            "UPDATE tenant_domains SET tls_status = 'issued', \
                                 tls_checked_at = now() WHERE id = $1",
                        )
                        .bind(id)
                        .execute(&control.db)
                        .await;
                    }
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(%host, error = %e, "tls probe failed");
                        let _ = sqlx::query(
                            "UPDATE tenant_domains SET tls_status = 'failed', \
                                 tls_checked_at = now() WHERE id = $1",
                        )
                        .bind(id)
                        .execute(&control.db)
                        .await;
                        crate::alerts::notify(
                            &state,
                            "tls_failed",
                            Some(&host),
                            &format!("tls probe failed at {}", Utc::now()),
                        );
                    }
                }
            }
            if issued + failed > 0 {
                tracing::info!(issued, failed, "tls probe finished");
            }
        }
    });
}
