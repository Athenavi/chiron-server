mod admin;
mod agent;
mod alerts;
mod auth;
mod checkpoint;
mod config;
mod control;
mod egress;
mod error;
mod integration;
mod media;
mod memory;
mod metrics;
mod models;
mod provider;
mod providers;
mod redis_ha;
mod sandbox;
mod sessions;
mod state;
mod subagent;
mod tenant;
mod tls_probe;
mod users;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{config::Config, sandbox::SandboxManager, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    // P1-8：/metrics 的 uptime 从进程启动开始算，而不是从第一次抓取。
    metrics::init();

    let cfg = Arc::new(Config::from_env()?);
    tracing::info!(
        bind = %cfg.bind_addr,
        team = cfg.is_team(),
        sandbox_scope = ?cfg.sandbox.scope,
        "starting chiron-server"
    );

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&db).await?;
    tracing::info!("migrations applied");

    // R-2：Sentinel 优先（可自动切主），否则单节点 `REDIS_URL`。
    let sentinel = cfg.sentinel_configured().then(|| redis_ha::SentinelOptions {
        urls: cfg.redis_sentinel_urls.clone(),
        master: cfg.redis_sentinel_master.clone(),
        password: (!cfg.redis_password.trim().is_empty()).then(|| cfg.redis_password.clone()),
    });
    let (redis, master_addr) = match sentinel.as_ref() {
        Some(options) => match options.resolve().await {
            Ok((conn, addr)) => {
                tracing::info!(
                    master = %options.master,
                    addr = %addr,
                    "redis master resolved via sentinel"
                );
                (Some(redis_ha::RedisHandle::new(conn)), Some(addr))
            }
            Err(e) => {
                tracing::error!(error = %e, "redis sentinel resolve failed");
                (None, None)
            }
        },
        None if cfg.redis_url.is_empty() => (None, None),
        None => match redis::Client::open(cfg.redis_url.clone()) {
            Ok(client) => match redis::aio::ConnectionManager::new(client).await {
                Ok(conn) => (Some(redis_ha::RedisHandle::new(conn)), None),
                Err(e) => {
                    tracing::error!(error = %e, "redis connect failed");
                    (None, None)
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "redis url invalid");
                (None, None)
            }
        },
    };
    if cfg.is_team() && redis.is_none() {
        anyhow::bail!("team/service mode requires a reachable Redis (session revocation)");
    }

    // R-2：每 15s 复查 Sentinel；主节点地址变化且新连接可用时才换句柄。
    if let (Some(handle), Some(options)) = (redis.as_ref(), sentinel.as_ref()) {
        redis_ha::spawn_refresher(handle.clone(), options.clone(), master_addr);
    }

    tracing::info!("redis resolved");
    // 控制库：可选；配置后启用平台 API 与租户注册表。
    let control = if cfg.control_database_url.trim().is_empty() {
        None
    } else {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&cfg.control_database_url)
            .await?;
        sqlx::migrate!("./migrations_control").run(&pool).await?;
        Some(Arc::new(control::ControlState { db: pool }))
    };
    if !cfg.secrets_key.trim().is_empty() {
        control::crypto::decode_key(&cfg.secrets_key)
            .map_err(|msg| anyhow::anyhow!("invalid SECRETS_KEY: {msg}"))?;
    }
    if control.is_some() && !cfg.provisioning_ready() {
        tracing::warn!(
            "tenant provisioning disabled: set SECRETS_KEY and PROVISION_DATABASE_URL (CREATEDB/CREATEROLE)"
        );
    }
    let tenants = control.as_ref().map(|c| {
        tracing::info!("tenant routing enabled (Host -> per-tenant database pool)");
        Arc::new(tenant::TenantRegistry::new(c.db.clone(), &cfg))
    });

    let sandbox = Arc::new(
        SandboxManager::new(cfg.sandbox.clone())
            .with_egress(cfg.secrets_key.clone(), cfg.egress_proxy_addr.clone()),
    );
    // B11：REDIS_URL 存在时事件日志与租约走 Redis（跨实例）；否则内存 fallback。
    let (events, leases): (
        Arc<dyn sessions::event_log::EventLog>,
        Arc<dyn sessions::lease::LeaseStore>,
    ) = match redis.as_ref() {
        Some(handle) => {
            tracing::info!(node = %cfg.multi.node_id, "multi-instance backends: redis");
            (
                Arc::new(sessions::redis_log::RedisEventLog::with_handle(
                    handle.clone(),
                    cfg.multi.event_capacity,
                    cfg.multi.event_retention_secs,
                    cfg.multi.tail_poll_ms,
                )),
                Arc::new(sessions::lease::RedisLeaseStore::with_handle(handle.clone())),
            )
        }
        None => {
            tracing::info!(node = %cfg.multi.node_id, "multi-instance backends: in-memory (single node)");
            (
                Arc::new(sessions::event_log::MemoryEventLog::with_capacity(
                    cfg.multi.event_capacity,
                )),
                Arc::new(sessions::lease::MemoryLeaseStore::new()),
            )
        }
    };
    sessions::event_log::install(events.clone());
    tracing::info!("event log installed");

    let state = Arc::new(AppState {
        db,
        redis,
        cfg: cfg.clone(),
        sandbox,
        control: control.clone(),
        tenants,
        limits: Arc::new(admin::limits::RateLimiter::new(cfg.rate_limit.clone())),
        runtime: Arc::new(admin::runtime::RuntimeCounters::default()),
        events,
        leases,
    });

    tracing::info!("state built");

    if let Some(control_state) = control.as_ref() {
        control::bootstrap_admin(&state, control_state).await;
    }
    let platform_router = control::router(state.clone());

    let protected = Router::new()
        .route("/api/v1/auth/password", post(users::change_password))
        .route("/api/v1/auth/logout", post(users::logout))
        .route(
            "/api/v1/auth/profile",
            get(users::profile).patch(users::update_profile),
        )
        .route(
            "/api/v1/admin/users",
            get(users::list_users).post(users::create_user),
        )
        .route("/api/v1/admin/users/{id}", delete(users::delete_user))
        .route(
            "/api/v1/admin/users/{id}/password/reset",
            post(users::reset_password),
        )
        .route(
            "/api/v1/admin/users/{id}/disable",
            post(users::disable_user),
        )
        .route("/api/v1/admin/users/{id}/enable", post(users::enable_user))
        .route(
            "/api/v1/admin/users/{id}/force-logout",
            post(users::force_logout),
        )
        .merge(models::router())
        .merge(memory::router())
        .merge(media::router())
        .merge(sessions::router())
        .merge(subagent::api::router())
        .merge(checkpoint::api::router())
        .merge(integration::router())
        .merge(admin::router())
        // 层序（从内到外）：SSE 计数 → 限流 → 认证。限流依赖 CurrentUser 做
        // 租户/用户级计数，必须在认证之后执行；SSE 计数只关心最终连接生命周期。
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin::runtime::track_sse,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin::limits::enforce,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // 登录是唯一未认证的写入口，单独挂限流（按来源 IP）。
    // 租户中间件包住全部 /api/v1：先按 Host 解析租户并绑定连接池，再认证。
    let tenant_api = Router::new()
        .route(
            "/api/v1/auth/login",
            post(users::login).layer(middleware::from_fn_with_state(
                state.clone(),
                admin::limits::enforce,
            )),
        )
        .route("/api/v1/auth/session", get(users::session))
        .route("/api/v1/auth/local-session", post(users::local_session))
        .merge(protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            tenant::tenant_middleware,
        ));

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readyz))
        // P1-8：Prometheus 抓取端点。**未认证**，且刻意放在租户中间件之外
        // （抓取方不带租户 Host/Cookie）；只暴露计数，实测应限制在可信网络。
        .route("/metrics", get(metrics::handler))
        .merge(tenant_api)
        .merge(platform_router)
        .with_state(state.clone());

    // B7：租约过期扫描 + 按检查点接管/判死（后台任务；关掉配置即不启动）。
    checkpoint::reconciler::spawn(state.clone());
    // B11：事件日志保留窗口裁剪（leader 租约 + PG 幂等键；单节点同样生效）。
    sessions::jobs::spawn_event_log_maintenance(state.clone());
    // S9：租户用量定时汇总（leader 租约；CHIRON_USAGE_ROLLUP_SECS=0 关闭）。
    control::usage_rollup::spawn_scheduler(state.clone());
    // P1-6：租户定时备份（leader 租约；CHIRON_BACKUP_SECS=0 或未配 CHIRON_BACKUP_DIR 关闭）。
    control::backup::spawn_scheduler(state.clone());
    // R-5：域名 TLS 状态探测（默认关闭；Caddy 不回调，只能主动探测）。
    tls_probe::spawn_scheduler(state.clone());
    // R-6：容量阈值看门狗（跨阈值告警一次，回落后重置）。
    alerts::spawn_watchdog(state.clone());
    // S7：租户 egress 正向代理（需要控制库；CHIRON_EGRESS_PROXY_BIND 为空则关闭）。
    if !cfg.egress_proxy_bind.trim().is_empty() {
        if let Err(e) = egress::spawn(state.clone(), cfg.egress_proxy_bind.trim().to_string()).await
        {
            tracing::error!(error = %e, "egress proxy failed to start");
        }
    }

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": "chiron-server",
            "team_mode": state.cfg.is_team(),
            "region": state.cfg.region,
            "sandbox_scope": format!("{:?}", state.cfg.sandbox.scope).to_lowercase(),
        })),
    )
}

/// 静态 preset 目录由 `models::router` 的 `/api/v1/models` 提供。
async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    let pg = sqlx::query("SELECT 1").execute(&state.db()).await.is_ok();
    let redis = match state.redis() {
        Some(handle) => handle.ping().await,
        None => false,
    };
    let docker = tokio::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    let docker_required = state.cfg.is_team();
    // Redis 在桌面/单节点模式是可选后端（内存 fallback）；团队模式才强制。
    let redis_required = state.cfg.is_team();
    let ok = pg && (!redis_required || redis) && (!docker_required || docker);
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if ok { "ok" } else { "unavailable" },
            "components": {
                "postgres": pg,
                "redis": redis,
                "docker": docker,
                "docker_required": docker_required,
                "tenant_pools": state.tenants.as_ref().map(|t| t.cached_pools()),
            },
            "region": state.cfg.region,
            "region_strict": state.cfg.region_strict,
            "read_only": state.cfg.read_only,
        })),
    )
        .into_response()
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .init();
}
