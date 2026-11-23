//! B10 限流：令牌桶 / 滑动窗口纯逻辑 + axum 中间件。
//!
//! 三级限流（PRD §9）：全局 → 租户 → 主体（用户或登录 IP）。
//! 主体级用配置里的 `RatePolicy`，租户 ×4、全局 ×20，避免小团队被单点拖垮。
//! 所有时间都由调用方传入 `Instant`，单测不依赖真实时钟。
//!
//! 触发时返回契约错误码 `quota.exceeded`（429）+ `Retry-After`。
//! v1 是进程内状态；多实例版本把 `Inner::entries` 换成 Redis 计数器即可，
//! 中间件签名不变。

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, RwLock},
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    auth::CurrentUser,
    config::{RateLimitConfig, RatePolicy},
    state::AppState,
    tenant::TenantContext,
};

/// 租户级额度相对主体的倍数。
const TENANT_FACTOR: u32 = 4;
/// 全局额度相对主体的倍数。
const GLOBAL_FACTOR: u32 = 20;
/// 每 N 次检查清理一次过期桶，避免 key 无限增长。
const PRUNE_EVERY: u64 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RateClass {
    /// 登录：按来源 IP 的滑动窗口（失败锁定之外的第二道闸）。
    Login,
    /// 提交一轮：按用户令牌桶。
    Submit,
    /// 工具审批：按用户令牌桶（工具执行本身在 agent 内部，审批是入口）。
    Tool,
    /// 大文件/外连/批量派生等重接口。
    Heavy,
}

impl RateClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Submit => "submit",
            Self::Tool => "tool",
            Self::Heavy => "heavy",
        }
    }

    fn policy(self, cfg: &RateLimitConfig) -> RatePolicy {
        match self {
            Self::Login => cfg.login,
            Self::Submit => cfg.submit,
            Self::Tool => cfg.tool,
            Self::Heavy => cfg.heavy,
        }
    }

    /// 登录用滑动窗口（窗口内最多 N 次），其它用令牌桶（允许突发、平滑恢复）。
    fn uses_window(self) -> bool {
        matches!(self, Self::Login)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Scope {
    Global,
    Tenant(String),
    Subject(String),
}

/// 经典令牌桶：容量 = 突发额度，按 `per_minute/60` 每秒补充。
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(burst: u32, per_minute: u32, now: Instant) -> Self {
        let capacity = burst.max(1) as f64;
        Self {
            capacity,
            refill_per_sec: per_minute.max(1) as f64 / 60.0,
            tokens: capacity,
            last: now,
        }
    }

    /// 成功扣一个令牌；失败返回"距离下一个令牌"的秒数（向上取整）。
    pub fn try_acquire(&mut self, now: Instant) -> Result<(), u64> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let missing = 1.0 - self.tokens;
        Err(ceil_secs(missing / self.refill_per_sec))
    }

    fn refill(&mut self, now: Instant) {
        if now <= self.last {
            return;
        }
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last = now;
    }

    fn tokens_at(&self, now: Instant) -> f64 {
        if now <= self.last {
            return self.tokens;
        }
        let elapsed = now.duration_since(self.last).as_secs_f64();
        (self.tokens + elapsed * self.refill_per_sec).min(self.capacity)
    }

    fn is_stale(&self, now: Instant) -> bool {
        self.tokens_at(now) >= self.capacity
    }
}

/// 滑动窗口：只保留窗口内的命中时间戳；判满时给出最早命中过期的时间。
#[derive(Clone, Debug)]
pub struct SlidingWindow {
    limit: usize,
    window: Duration,
    hits: VecDeque<Instant>,
}

impl SlidingWindow {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit: limit.max(1) as usize,
            window,
            hits: VecDeque::new(),
        }
    }

    pub fn try_acquire(&mut self, now: Instant) -> Result<(), u64> {
        self.evict(now);
        if self.hits.len() < self.limit {
            self.hits.push_back(now);
            return Ok(());
        }
        let oldest = *self.hits.front().unwrap_or(&now);
        let waited = now.saturating_duration_since(oldest);
        Err(ceil_secs(self.window.saturating_sub(waited).as_secs_f64()))
    }

    fn evict(&mut self, now: Instant) {
        while let Some(front) = self.hits.front().copied() {
            if now.saturating_duration_since(front) >= self.window {
                self.hits.pop_front();
            } else {
                break;
            }
        }
    }

    fn is_stale(&self, now: Instant) -> bool {
        match self.hits.front().copied() {
            None => true,
            Some(oldest) => now.saturating_duration_since(oldest) >= self.window,
        }
    }
}

fn ceil_secs(secs: f64) -> u64 {
    if !secs.is_finite() || secs <= 0.0 {
        return 1;
    }
    (secs.ceil() as u64).max(1)
}

enum Entry {
    Bucket(TokenBucket),
    Window(SlidingWindow),
}

impl Entry {
    fn is_stale(&self, now: Instant) -> bool {
        match self {
            Self::Bucket(b) => b.is_stale(now),
            Self::Window(w) => w.is_stale(now),
        }
    }

    fn try_acquire(&mut self, now: Instant) -> Result<(), u64> {
        match self {
            Self::Bucket(b) => b.try_acquire(now),
            Self::Window(w) => w.try_acquire(now),
        }
    }
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    checks: u64,
}

pub struct RateLimiter {
    cfg: RwLock<RateLimitConfig>,
    inner: Mutex<Inner>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self {
            cfg: RwLock::new(cfg),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn set_config(&self, cfg: RateLimitConfig) {
        if let Ok(mut current) = self.cfg.write() {
            *current = cfg;
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.cfg
            .read()
            .map(|cfg| cfg.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 全局 → 租户 → 主体；任一级拒绝即短路，不扣后续层级的额度。
    pub fn check_request(
        &self,
        class: RateClass,
        tenant_id: Option<String>,
        subject: Option<String>,
        now: Instant,
    ) -> Result<(), u64> {
        self.check(class, &Scope::Global, now)?;
        if let Some(tenant_id) = tenant_id {
            self.check(class, &Scope::Tenant(tenant_id), now)?;
        }
        if let Some(subject) = subject {
            self.check(class, &Scope::Subject(subject), now)?;
        }
        Ok(())
    }

    fn check(&self, class: RateClass, scope: &Scope, now: Instant) -> Result<(), u64> {
        let key = match scope {
            Scope::Global => class.as_str().to_string(),
            Scope::Tenant(id) => format!("{}:tenant:{id}", class.as_str()),
            Scope::Subject(s) => format!("{}:subject:{s}", class.as_str()),
        };
        let (burst, per_minute) = scaled(class.policy(&self.config()), scope);

        let mut inner = self.lock();
        inner.checks = inner.checks.wrapping_add(1);
        if inner.checks % PRUNE_EVERY == 0 {
            inner.entries.retain(|_, entry| !entry.is_stale(now));
        }
        let entry = inner.entries.entry(key).or_insert_with(|| {
            if class.uses_window() {
                Entry::Window(SlidingWindow::new(per_minute, Duration::from_secs(60)))
            } else {
                Entry::Bucket(TokenBucket::new(burst, per_minute, now))
            }
        });
        entry.try_acquire(now)
    }

    /// 只用于测试/运维观测：当前活跃 key 数。
    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.lock().entries.len()
    }
}

fn scaled(policy: RatePolicy, scope: &Scope) -> (u32, u32) {
    let factor = match scope {
        Scope::Global => GLOBAL_FACTOR,
        Scope::Tenant(_) => TENANT_FACTOR,
        Scope::Subject(_) => 1,
    };
    (
        policy.burst.saturating_mul(factor),
        policy.per_minute.saturating_mul(factor),
    )
}

/// 按 `(method, path)` 判定该请求属于哪类限流；不是限流目标返回 `None`。
///
/// 只认契约里的三条入口，新增工具类 HTTP 路由时在这里加分支；
/// agent 内部的工具循环不经过 HTTP，不受此中间件影响。
pub fn class_for(method: &Method, path: &str) -> Option<RateClass> {
    let trimmed = path.trim_end_matches('/');
    let segs: Vec<&str> = trimmed.split('/').collect();
    if method == Method::GET {
        return match segs.as_slice() {
            ["", "api", "v1", "media", id] if !id.is_empty() => Some(RateClass::Heavy),
            _ => None,
        };
    }
    if method != Method::POST {
        return None;
    }
    match segs.as_slice() {
        ["", "api", "v1", "auth", "login"] => Some(RateClass::Login),
        ["", "api", "platform", "v1", "auth", "login"] => Some(RateClass::Login),
        ["", "api", "v1", "sessions", id, "messages"] if !id.is_empty() => Some(RateClass::Submit),
        ["", "api", "v1", "sessions", id, "steer"] if !id.is_empty() => Some(RateClass::Submit),
        ["", "api", "v1", "sessions", id, "approvals", approval]
            if !id.is_empty() && !approval.is_empty() =>
        {
            Some(RateClass::Tool)
        }
        ["", "api", "v1", "media"]
        | ["", "api", "platform", "v1", "billing", "webhook"]
        | ["", "api", "platform", "v1", "billing", "alipay", "notify"]
        | ["", "api", "v1", "mcp", "servers", _, "probe"]
        | ["", "api", "v1", "market", "items", _, "install"]
        | ["", "api", "v1", "sessions", _, "subagents"]
        | ["", "api", "v1", "admin", "media", _] => Some(RateClass::Heavy),
        _ => None,
    }
}

/// 登录限流的主体：优先 `X-Forwarded-For` 首段，其次 `X-Real-IP`。
/// 取不到时用固定 key（仍受全局桶约束，不会放行无限次）。
pub fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or("unknown")
        .to_string()
}

pub fn quota_response(retry_after: u64) -> Response {
    let wait = retry_after.max(1);
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({
            "error": {
                "code": "quota.exceeded",
                "message": format!("rate limit exceeded; retry in {wait}s"),
                "details": { "retry_after": wait },
            }
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&wait.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, value);
    }
    resp
}

/// 限流中间件：登录路由与受保护路由共用；非目标路径直接放行。
///
/// 受保护路由上它必须位于 `require_auth` 之内，才能拿到 `CurrentUser` 做
/// 租户/用户级计数。
pub async fn enforce(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let Some(class) = class_for(req.method(), req.uri().path()) else {
        return next.run(req).await;
    };
    if !state.limits.config().enabled {
        return next.run(req).await;
    }
    let actor = req.extensions().get::<CurrentUser>().cloned();
    let tenant_key = req
        .extensions()
        .get::<TenantContext>()
        .map(|ctx| ctx.id.to_string())
        .or_else(|| actor.as_ref().map(|a| format!("local:{}", a.tenant_id)));
    let (tenant_id, subject) = match (&class, actor) {
        (RateClass::Login, _) => (None, Some(client_key(req.headers()))),
        (_, Some(actor)) => (tenant_key, Some(actor.id.to_string())),
        (_, None) => (None, None),
    };
    match state
        .limits
        .check_request(class, tenant_id, subject, Instant::now())
    {
        Ok(()) => next.run(req).await,
        Err(retry_after) => {
            tracing::warn!(
                class = class.as_str(),
                path = req.uri().path(),
                retry_after,
                "rate limit exceeded"
            );
            quota_response(retry_after)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc as StdArc,
    };
    use uuid::Uuid;

    fn cfg() -> RateLimitConfig {
        RateLimitConfig {
            enabled: true,
            login: RatePolicy::new(10, 10),
            submit: RatePolicy::new(3, 30),
            tool: RatePolicy::new(2, 60),
            heavy: RatePolicy::new(2, 10),
        }
    }

    fn uuid(n: u128) -> String {
        Uuid::from_u128(n).to_string()
    }

    #[test]
    fn class_matching_only_covers_contract_entries() {
        let post = Method::POST;
        assert_eq!(
            class_for(&post, "/api/v1/auth/login"),
            Some(RateClass::Login)
        );
        assert_eq!(
            class_for(&post, "/api/platform/v1/auth/login"),
            Some(RateClass::Login)
        );
        assert_eq!(
            class_for(&post, "/api/v1/auth/login/"),
            Some(RateClass::Login)
        );
        assert_eq!(
            class_for(&post, &format!("/api/v1/sessions/{}/messages", uuid(1))),
            Some(RateClass::Submit)
        );
        assert_eq!(
            class_for(
                &post,
                &format!("/api/v1/sessions/{}/approvals/{}", uuid(1), uuid(2))
            ),
            Some(RateClass::Tool)
        );
        // 方法不符 / 路径不完整 / 无关路径都不限流。
        assert_eq!(class_for(&Method::GET, "/api/v1/auth/login"), None);
        assert_eq!(class_for(&post, "/api/v1/sessions//messages"), None);
        assert_eq!(class_for(&post, "/api/v1/sessions/x/messages/extra"), None);
        assert_eq!(class_for(&post, "/api/v1/admin/audit"), None);
        assert_eq!(class_for(&post, "/healthz"), None);
    }

    #[test]
    fn client_key_prefers_forwarded_then_real_ip() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_key(&headers), "unknown");
        headers.insert("x-real-ip", "10.0.0.9".parse().unwrap());
        assert_eq!(client_key(&headers), "10.0.0.9");
        headers.insert("x-forwarded-for", "1.2.3.4, 10.0.0.1".parse().unwrap());
        assert_eq!(client_key(&headers), "1.2.3.4");
        headers.insert("x-forwarded-for", "   ".parse().unwrap());
        assert_eq!(client_key(&headers), "10.0.0.9");
    }

    #[test]
    fn token_bucket_allows_burst_then_refills() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(3, 60, start);
        for _ in 0..3 {
            assert!(bucket.try_acquire(start).is_ok());
        }
        // 用光后立刻再取：需要等 1 秒（60/min = 1/s）。
        assert_eq!(bucket.try_acquire(start), Err(1));
        // 1 秒后恢复一个令牌。
        assert!(bucket.try_acquire(start + Duration::from_secs(1)).is_ok());
        assert!(bucket.try_acquire(start + Duration::from_secs(1)).is_err());
        // 长时间空闲不会超过容量。
        let later = start + Duration::from_secs(3600);
        for _ in 0..3 {
            assert!(bucket.try_acquire(later).is_ok());
        }
        assert!(bucket.try_acquire(later).is_err());
    }

    #[test]
    fn token_bucket_retry_after_rounds_up() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(1, 6, start);
        assert!(bucket.try_acquire(start).is_ok());
        // 6/min = 0.1/s，缺 1 个令牌 => 10 秒。
        assert_eq!(bucket.try_acquire(start), Err(10));
    }

    #[test]
    fn sliding_window_expires_old_hits() {
        let start = Instant::now();
        let mut window = SlidingWindow::new(2, Duration::from_secs(60));
        assert!(window.try_acquire(start).is_ok());
        assert!(window.try_acquire(start + Duration::from_secs(10)).is_ok());
        // 窗口满：最早一票在 60 秒后过期，现在只等了 10 秒。
        assert_eq!(window.try_acquire(start + Duration::from_secs(10)), Err(50));
        // 最早一票过期后恢复一个名额。
        assert!(window.try_acquire(start + Duration::from_secs(60)).is_ok());
        // 第二票也过期后再次恢复。
        assert!(window.try_acquire(start + Duration::from_secs(71)).is_ok());
    }

    #[test]
    fn window_staleness_drives_pruning() {
        let start = Instant::now();
        let mut window = SlidingWindow::new(5, Duration::from_secs(60));
        assert!(window.try_acquire(start).is_ok());
        assert!(!window.is_stale(start));
        assert!(window.is_stale(start + Duration::from_secs(60)));
        let mut bucket = TokenBucket::new(2, 60, start);
        assert!(bucket.try_acquire(start).is_ok());
        assert!(!bucket.is_stale(start));
        assert!(bucket.is_stale(start + Duration::from_secs(120)));
    }

    #[test]
    fn three_levels_scale_from_subject_policy() {
        let start = Instant::now();
        // submit: burst 3/pro_minute 30 => 主体 3，租户 12，全局 60。
        let limiter = RateLimiter::new(cfg());
        let subject = uuid(7);
        for _ in 0..3 {
            assert!(limiter
                .check_request(RateClass::Submit, Some("1".to_string()), Some(subject.clone()), start)
                .is_ok());
        }
        assert!(limiter
            .check_request(RateClass::Submit, Some("1".to_string()), Some(subject.clone()), start)
            .is_err());
        // 同租户其他用户仍可用（租户桶还够，全局桶也够）。
        assert!(limiter
            .check_request(RateClass::Submit, Some("1".to_string()), Some(uuid(8)), start)
            .is_ok());
        // 全局桶容量 60：上面已用 4 次（第 4 次在主体级被拒，但全局/租户
        // 先扣后拒，所以全局实际用了 5 次），还能过 55 次。每次换租户/主体，
        // 让租户级（12）不成为瓶颈，单独验证全局桶。
        let mut passed = 0;
        for i in 0..60u128 {
            if limiter
                .check_request(
                    RateClass::Submit,
                    Some((100 + i as i16).to_string()),
                    Some(uuid(1000 + i)),
                    start,
                )
                .is_ok()
            {
                passed += 1;
            }
        }
        assert_eq!(passed, 55, "global bucket must cap at 20x burst");
    }

    #[test]
    fn rejected_global_does_not_consume_subject_quota() {
        let start = Instant::now();
        let limiter = RateLimiter::new(cfg());
        // tool：主体容量 2、租户 8、全局 40。20 个租户各 1 个主体用满全局。
        for i in 0..20u128 {
            for _ in 0..2 {
                assert!(limiter
                    .check_request(
                        RateClass::Tool,
                        Some((1 + i as i16).to_string()),
                        Some(uuid(500 + i)),
                        start,
                    )
                    .is_ok());
            }
        }
        // 新主体被全局桶拒绝（租户/主体都没被扣）。
        assert!(limiter
            .check_request(RateClass::Tool, Some("999".to_string()), Some(uuid(999)), start)
            .is_err());
        // 只有全局桶回补后，新主体才重新拿到满额突发。
        let later = start + Duration::from_secs(2);
        for _ in 0..2 {
            assert!(limiter
                .check_request(RateClass::Tool, Some("999".to_string()), Some(uuid(999)), later)
                .is_ok());
        }
        assert!(limiter.tracked_keys() > 0);
    }

    #[test]
    fn concurrent_acquires_never_exceed_capacity() {
        let start = Instant::now();
        let limiter = StdArc::new(RateLimiter::new(RateLimitConfig {
            enabled: true,
            login: RatePolicy::new(10, 10),
            submit: RatePolicy::new(100, 6000),
            tool: RatePolicy::new(10, 10),
            heavy: RatePolicy::new(10, 10),
        }));
        let ok = StdArc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let limiter = limiter.clone();
            let ok = ok.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    // submit 主体级容量 100（租户/全局更高），同一 now 不回补。
                    if limiter
                        .check_request(
                            RateClass::Submit,
                            Some("1".to_string()),
                            Some("fixed-subject".to_string()),
                            start,
                        )
                        .is_ok()
                    {
                        ok.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }));
        }
        for h in handles {
            h.join().expect("thread");
        }
        assert_eq!(ok.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn quota_response_has_stable_code_and_retry_after() {
        let resp = quota_response(0);
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "1");
        let resp = quota_response(7);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "7");
    }

    #[test]
    fn class_policies_come_from_config() {
        let cfg = cfg();
        assert_eq!(RateClass::Login.policy(&cfg), RatePolicy::new(10, 10));
        assert_eq!(RateClass::Submit.policy(&cfg), RatePolicy::new(3, 30));
        assert_eq!(RateClass::Tool.policy(&cfg), RatePolicy::new(2, 60));
        assert!(RateClass::Login.uses_window());
        assert!(!RateClass::Submit.uses_window());
        assert!(!RateClass::Tool.uses_window());
    }

    #[test]
    fn policy_clamps_zero_to_one() {
        assert_eq!(RatePolicy::new(0, 0), RatePolicy::new(1, 1));
        assert_eq!(RatePolicy::new(5, 0).per_minute, 1);
    }
}
