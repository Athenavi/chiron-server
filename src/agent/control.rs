//! Run 控制面：取消令牌、进程内 run registry、steer 队列与审批唤醒。
//!
//! 设计口径（PRD §7.4）：
//! - 中断在**轮次边界与工具边界**生效：循环在每轮/每个工具前检查 `is_cancelled()`；
//! - 取消不是 drop 任务，而是让循环自己收敛并写终态，保证事件顺序稳定；
//! - 审批恢复与 steer 都是"唤醒 run"，因此 registry 只提供通知，不持有业务状态。

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Debug, Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: Notify,
}

/// 可克隆的取消令牌；`cancelled()` 在已取消时立即返回。
#[derive(Clone, Debug, Default)]
pub struct Cancellation {
    inner: Arc<CancelInner>,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    /// 幂等：重复取消不重复唤醒。
    pub fn cancel(&self) {
        if !self.inner.flag.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst)
    }

    /// 等到取消为止。先建 `Notified` 再查标志，避免"检查后、等待前"丢唤醒。
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// 单个 run 的进程内句柄：取消令牌 + steer 队列 + 审批唤醒。
#[derive(Debug, Default)]
pub struct RunControl {
    pub cancel: Cancellation,
    wakes: Notify,
    steers: Mutex<Vec<String>>,
}

impl RunControl {
    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }

    /// 审批被决策 / 外部状态变化时唤醒循环。
    pub fn wake(&self) {
        self.wakes.notify_waiters();
    }

    /// 等待一次唤醒；`timeout` 到期或取消都返回，由调用方回读数据库判定。
    pub async fn wait_wake(&self) {
        let notified = self.wakes.notified();
        notified.await;
    }

    pub fn push_steer(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.steers.lock() {
            if guard.len() >= 16 {
                guard.remove(0);
            }
            guard.push(text.to_string());
        }
        self.wake();
    }

    /// 取走全部待注入约束（每轮开始时调用）。
    pub fn drain_steers(&self) -> Vec<String> {
        self.steers
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default()
    }
}

/// 进程内 run 注册表。多实例版本（B11）把"哪个实例在跑"换成租约，
/// 接口保持不变：注册成功 = 本实例负责执行。
#[derive(Default)]
pub struct RunRegistry {
    runs: Mutex<HashMap<Uuid, Arc<RunControl>>>,
}

impl RunRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Uuid, Arc<RunControl>>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 注册失败（已在运行）返回 `None`，调用方不应重复启动循环。
    pub fn register(&self, run_id: Uuid) -> Option<Arc<RunControl>> {
        let mut guard = self.lock();
        if guard.contains_key(&run_id) {
            return None;
        }
        let control = Arc::new(RunControl::default());
        guard.insert(run_id, control.clone());
        Some(control)
    }

    pub fn get(&self, run_id: Uuid) -> Option<Arc<RunControl>> {
        self.lock().get(&run_id).cloned()
    }

    /// 取消：run 不在本实例时返回 false（多实例下由租约归属实例处理）。
    pub fn cancel(&self, run_id: Uuid) -> bool {
        match self.get(run_id) {
            Some(control) => {
                control.request_cancel();
                true
            }
            None => false,
        }
    }

    pub fn wake(&self, run_id: Uuid) -> bool {
        match self.get(run_id) {
            Some(control) => {
                control.wake();
                true
            }
            None => false,
        }
    }

    pub fn push_steer(&self, run_id: Uuid, text: &str) -> bool {
        match self.get(run_id) {
            Some(control) => {
                control.push_steer(text);
                true
            }
            None => false,
        }
    }

    pub fn finish(&self, run_id: Uuid) {
        self.lock().remove(&run_id);
    }

    pub fn active_count(&self) -> usize {
        self.lock().len()
    }
}

/// 进程级单例：与 `sessions::event_log::global()` 同一生命周期口径。
///
/// 多实例下 registry 仍然只记本实例的 run：跨实例互斥由
/// `sessions::lease`（Redis SET NX PX + PG CAS）保证，事件由
/// `sessions::event_log` 共享，因此 registry 不需要分布式化。
pub fn registry() -> &'static RunRegistry {
    static REGISTRY: std::sync::OnceLock<RunRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(RunRegistry::new)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn cancellation_is_idempotent_and_observable() {
        let token = Cancellation::new();
        assert!(!token.is_cancelled());
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
        tokio::time::timeout(Duration::from_millis(50), token.cancelled())
            .await
            .expect("cancelled() returns immediately");
    }

    #[tokio::test]
    async fn waiting_future_wakes_on_cancel() {
        let token = Cancellation::new();
        let waiter = {
            let token = token.clone();
            tokio::spawn(async move { token.cancelled().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter should wake")
            .expect("join");
    }

    #[test]
    fn registry_rejects_duplicate_registration() {
        let registry = RunRegistry::new();
        let run = Uuid::new_v4();
        assert!(registry.register(run).is_some());
        assert!(registry.register(run).is_none());
        assert_eq!(registry.active_count(), 1);
        registry.finish(run);
        assert_eq!(registry.active_count(), 0);
        assert!(!registry.cancel(run));
    }

    #[test]
    fn registry_cancel_and_steer_route_to_control() {
        let registry = RunRegistry::new();
        let run = Uuid::new_v4();
        let control = registry.register(run).expect("registered");
        assert!(registry.push_steer(run, "keep it short"));
        registry.push_steer(run, "   ");
        assert_eq!(control.drain_steers(), vec!["keep it short".to_string()]);
        assert!(control.drain_steers().is_empty());

        assert!(registry.cancel(run));
        assert!(control.cancel.is_cancelled());
        assert!(!registry.cancel(Uuid::new_v4()));
    }

    #[test]
    fn steer_queue_trims_and_drops_empty() {
        let control = RunControl::default();
        control.push_steer("  a  ");
        control.push_steer("");
        assert_eq!(control.drain_steers(), vec!["a".to_string()]);
    }
}
