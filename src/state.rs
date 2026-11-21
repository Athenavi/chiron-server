use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    admin::{limits::RateLimiter, runtime::RuntimeCounters},
    config::Config,
    control::ControlState,
    redis_ha::RedisHandle,
    sandbox::SandboxManager,
    sessions::{event_log::EventLog, lease::LeaseStore},
    tenant::TenantRegistry,
};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// R-2：可热替换的 Redis 句柄（Sentinel 主从切换时后台任务换连接）。
    /// `None` = 单节点/桌面形态，事件日志与租约走内存 fallback。
    pub redis: Option<RedisHandle>,
    pub cfg: Arc<Config>,
    pub sandbox: Arc<SandboxManager>,
    /// B10 限流状态（进程内；多实例版本换成 Redis 计数器）。
    pub limits: Arc<RateLimiter>,
    /// B10 运行态计数（SSE 连接等进程内指标）。
    pub runtime: Arc<RuntimeCounters>,
    /// B11 事件日志（内存 fallback 或 Redis Streams）：SSE 回放与实时续流。
    pub events: Arc<dyn EventLog>,
    /// B11 租约：跨实例"谁在跑这个 run / 谁当 leader"。
    pub leases: Arc<dyn LeaseStore>,
    /// 控制库；未配置时平台 API 返回 503。
    pub control: Option<Arc<ControlState>>,
    /// S2：Host → 租户连接池；未配置控制库时为 None（单库形态）。
    pub tenants: Option<Arc<TenantRegistry>>,
}

impl AppState {
    /// 返回共享句柄的 clone；具体连接按操作 `handle.conn().await` 取。
    pub fn redis(&self) -> Option<RedisHandle> {
        self.redis.clone()
    }

    /// 业务查询一律走这里：请求作用域内是当前租户库，否则回落到默认库。
    /// 返回 owned clone（`PgPool` 内部是 Arc，克隆很便宜）。
    pub fn db(&self) -> PgPool {
        crate::tenant::current().unwrap_or_else(|| self.db.clone())
    }
}
