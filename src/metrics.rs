//! P1-8：`GET /metrics`（Prometheus 文本格式，手写，不引入新依赖）。
//!
//! 口径与限制（改动前先读）：
//! * **默认未认证**：挂载在根路由，不经租户中间件（抓取方不带 Host/租户 Cookie）。
//!   因此只能在可信网络或反代白名单后暴露；配置 `CHIRON_METRICS_TOKEN` 后要求
//!   `Authorization: Bearer <token>`。内容不含任何租户名/密钥，只有计数。
//! * 进程内指标（SSE、租户连接池、uptime）是**单实例**视角：多实例部署应对每个实例
//!   分别抓取后 `sum by (job)`。
//! * 活跃 run 来自本进程默认库（`state.db`）。SaaS 下 run 分散在各租户库，抓取时逐租户
//!   建连接代价过高，故**不聚合**：该指标只反映默认租户库，租户维度请查
//!   `/api/v1/admin/runtime`。
//! * 模型路由失败取自控制库 `tenant_model_routes.last_error`（健康探测结果），控制库
//!   不可用时降级为 0 并打日志，不让抓取 500。
//! * 无直方图/摘要：只有 gauge / counter，避免自研分位数统计。

use std::{
    fmt::Write as _,
    sync::{Arc, OnceLock},
    time::Instant,
};

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sqlx::PgPool;

use crate::state::AppState;

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// 与迁移里的"活跃 run"四态一致，缺的补 0，避免 Prometheus 侧序列时有时无。
const ACTIVE_RUN_STATES: [&str; 4] = [
    "queued",
    "running",
    "waiting_approval",
    "waiting_user",
];

static STARTED: OnceLock<Instant> = OnceLock::new();

/// 进程启动时调用一次；未调用时 uptime 从首次抓取开始算（会偏小）。
pub fn init() {
    let _ = STARTED.set(Instant::now());
}

fn uptime_secs() -> u64 {
    STARTED
        .get()
        .map(|at| at.elapsed().as_secs())
        .unwrap_or(0)
}

/// 一次抓取的快照。与渲染分开，便于单元测试。
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub region: String,
    pub uptime_secs: u64,
    pub tenant_pools: usize,
    pub sse_active: usize,
    pub sse_opened_total: u64,
    pub egress_allowed_total: u64,
    pub egress_denied_total: u64,
    /// 活跃 run 数，按状态；未出现的状态渲染为 0。
    pub active_runs: Vec<(String, i64)>,
    /// 控制库路由总数 / 最近一次探测失败（`last_error` 非空）数。
    pub model_routes_total: i64,
    pub model_routes_failing: i64,
}

pub async fn handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !metrics_authorized(&state.cfg.metrics_token, &headers) {
        return (StatusCode::UNAUTHORIZED, "metrics token required").into_response();
    }
    let snapshot = collect(&state).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        render(&snapshot),
    )
        .into_response()
}

/// token 为空时保持旧行为（未认证）；否则要求 Bearer 精确匹配。
pub fn metrics_authorized(expected: &str, headers: &HeaderMap) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return true;
    }
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(token.trim().as_bytes(), expected.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

async fn collect(state: &AppState) -> Snapshot {
    let active_runs = active_runs(&state.db).await;

    // 控制库不可用（单库形态）时不报错，只把路由指标报 0。
    let (model_routes_total, model_routes_failing) = match state.control.as_ref() {
        Some(control) => model_route_health(&control.db).await,
        None => (0, 0),
    };

    Snapshot {
        region: state.cfg.region.clone(),
        uptime_secs: uptime_secs(),
        tenant_pools: state.tenants.as_ref().map(|t| t.cached_pools()).unwrap_or(0),
        sse_active: state.runtime.sse_active(),
        sse_opened_total: state.runtime.sse_total(),
        egress_allowed_total: state.runtime.egress_allowed_total(),
        egress_denied_total: state.runtime.egress_denied_total(),
        active_runs,
        model_routes_total,
        model_routes_failing,
    }
}

async fn active_runs(db: &PgPool) -> Vec<(String, i64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*)::bigint FROM runs
          WHERE status IN ('queued', 'running', 'waiting_approval', 'waiting_user')
          GROUP BY status",
    )
    .fetch_all(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "metrics: active runs query failed");
        e
    })
    .unwrap_or_default();
    // 复用 runtime 面板的口径（过滤 + 去零），保证两处数字一致。
    crate::admin::runtime::summarize_active_runs(&rows)
        .into_iter()
        .collect()
}

/// 控制库里"启用中但最近一次探测失败"的路由数。旧库缺列时按 0 处理（启动会跑迁移，
/// 这里只是兜底）。
async fn model_route_health(db: &PgPool) -> (i64, i64) {
    let row: Result<(i64, i64), sqlx::Error> = sqlx::query_as(
        "SELECT COUNT(*)::bigint,
                COUNT(*) FILTER (WHERE last_error <> '')::bigint
           FROM tenant_model_routes
          WHERE enabled",
    )
    .fetch_one(db)
    .await;
    match row {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(error = %e, "metrics: model route health query failed");
            (0, 0)
        }
    }
}

/// Prometheus 文本暴露：HELP/TYPE + 一行一个样本。
pub fn render(snapshot: &Snapshot) -> String {
    let mut out = String::with_capacity(1024);
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    out.push_str("# HELP chiron_build_info Build info (always 1).\n");
    out.push_str("# TYPE chiron_build_info gauge\n");
    let _ = writeln!(
        out,
        "chiron_build_info{{version=\"{}\",profile=\"{profile}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );

    out.push_str("# HELP chiron_region_info Deployment region label (always 1).\n");
    out.push_str("# TYPE chiron_region_info gauge\n");
    let _ = writeln!(
        out,
        "chiron_region_info{{region=\"{}\"}} 1",
        escape(if snapshot.region.is_empty() {
            "default"
        } else {
            &snapshot.region
        })
    );

    out.push_str("# HELP chiron_uptime_seconds Seconds since process start.\n");
    out.push_str("# TYPE chiron_uptime_seconds gauge\n");
    let _ = writeln!(out, "chiron_uptime_seconds {}", snapshot.uptime_secs);

    out.push_str("# HELP chiron_tenant_pools_cached Cached per-tenant database pools.\n");
    out.push_str("# TYPE chiron_tenant_pools_cached gauge\n");
    let _ = writeln!(out, "chiron_tenant_pools_cached {}", snapshot.tenant_pools);

    out.push_str("# HELP chiron_sse_connections_active Currently open SSE connections.\n");
    out.push_str("# TYPE chiron_sse_connections_active gauge\n");
    let _ = writeln!(
        out,
        "chiron_sse_connections_active {}",
        snapshot.sse_active
    );

    out.push_str("# HELP chiron_sse_connections_opened_total SSE connections opened since start.\n");
    out.push_str("# TYPE chiron_sse_connections_opened_total counter\n");
    let _ = writeln!(
        out,
        "chiron_sse_connections_opened_total {}",
        snapshot.sse_opened_total
    );
    out.push_str("# HELP chiron_egress_allowed_total Egress connections allowed by policy.\n");
    out.push_str("# TYPE chiron_egress_allowed_total counter\n");
    let _ = writeln!(
        out,
        "chiron_egress_allowed_total {}",
        snapshot.egress_allowed_total
    );
    out.push_str("# HELP chiron_egress_denied_total Egress connections denied by policy.\n");
    out.push_str("# TYPE chiron_egress_denied_total counter\n");
    let _ = writeln!(
        out,
        "chiron_egress_denied_total {}",
        snapshot.egress_denied_total
    );

    out.push_str("# HELP chiron_active_runs Active runs by status (default database only).\n");
    out.push_str("# TYPE chiron_active_runs gauge\n");
    for status in ACTIVE_RUN_STATES {
        let count = snapshot
            .active_runs
            .iter()
            .find(|(s, _)| s == status)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        let _ = writeln!(
            out,
            "chiron_active_runs{{status=\"{}\"}} {count}",
            escape(status)
        );
    }

    out.push_str("# HELP chiron_model_routes_total Enabled model routes in the control plane.\n");
    out.push_str("# TYPE chiron_model_routes_total gauge\n");
    let _ = writeln!(out, "chiron_model_routes_total {}", snapshot.model_routes_total);

    out.push_str(
        "# HELP chiron_model_routes_failing Enabled model routes whose last probe failed.\n",
    );
    out.push_str("# TYPE chiron_model_routes_failing gauge\n");
    let _ = writeln!(
        out,
        "chiron_model_routes_failing {}",
        snapshot.model_routes_failing
    );

    out
}

/// label 值转义（Prometheus 文本格式：`\` `"` 换行）。
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            region: "test-region".to_string(),
            uptime_secs: 42,
            tenant_pools: 3,
            sse_active: 2,
            sse_opened_total: 9,
            egress_allowed_total: 11,
            egress_denied_total: 4,
            active_runs: vec![("running".to_string(), 5)],
            model_routes_total: 7,
            model_routes_failing: 2,
        }
    }

    #[test]
    fn render_exposes_expected_series() {
        let text = render(&snapshot());
        assert!(text.contains("# TYPE chiron_uptime_seconds gauge"));
        assert!(text.contains("chiron_region_info{region=\"test-region\"} 1"));
        assert!(text.contains("\nchiron_uptime_seconds 42\n"));
        assert!(text.contains("\nchiron_tenant_pools_cached 3\n"));
        assert!(text.contains("\nchiron_egress_allowed_total 11\n"));
        assert!(text.contains("\nchiron_egress_denied_total 4\n"));
        assert!(text.contains("\nchiron_sse_connections_active 2\n"));
        assert!(text.contains("\nchiron_sse_connections_opened_total 9\n"));
        assert!(text.contains("\nchiron_active_runs{status=\"running\"} 5\n"));
        assert!(text.contains("\nchiron_model_routes_total 7\n"));
        assert!(text.contains("\nchiron_model_routes_failing 2\n"));
        assert!(text.contains("chiron_build_info{version=\""));
        // 没有出现过的状态补 0，避免序列缺失。
        for status in ACTIVE_RUN_STATES {
            assert!(
                text.contains(&format!("chiron_active_runs{{status=\"{status}\"}}")),
                "missing status {status}"
            );
        }
        assert!(text.contains("chiron_active_runs{status=\"queued\"} 0"));
    }

    #[test]
    fn every_sample_line_has_value() {
        let text = render(&snapshot());
        for line in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            // 样本行必须以 ` <number>` 结尾；label 里有空格也只是 `{a="b"}` 内。
            let value = line.rsplit(' ').next().unwrap_or_default();
            assert!(
                value.parse::<f64>().is_ok(),
                "sample line without numeric value: {line}"
            );
        }
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
        assert_eq!(escape("running"), "running");
    }

    #[test]
    fn uptime_defaults_to_zero_before_init() {
        // 不依赖 init 是否已调用，只要求不 panic 且单调（未初始化时为 0）。
        let _ = uptime_secs();
        init();
        init();
        assert!(uptime_secs() < 3600);
    }

    #[test]
    fn metrics_token_is_optional_and_checked() {
        let mut headers = HeaderMap::new();
        assert!(metrics_authorized("", &headers));
        assert!(!metrics_authorized("secret", &headers));
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert!(metrics_authorized("secret", &headers));
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!metrics_authorized("secret", &headers));
    }
}
