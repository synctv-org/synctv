//! Centralized shutdown orchestration.
//!
//! `ShutdownCoordinator` collects all shutdown resources (cancellation tokens,
//! background task handles, and typed hooks) during initialization and executes
//! them in a deterministic order when the server shuts down.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// A shutdown hook that performs cleanup work (e.g., flushing audit buffers,
/// joining long-lived listener tasks).
pub trait ShutdownHook: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &str;
    /// Maximum time to wait before moving on.
    fn timeout(&self) -> Duration;
    /// Execute the hook.
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Preferred minimum per-task timeout when dividing global budget.
///
/// This is only used when the remaining total budget can still afford it for
/// every pending item. The overall shutdown budget always takes precedence.
const MIN_PER_TASK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoordinatorShutdownMode {
    Graceful,
    Force,
}

struct AbortOnDropJoinHandle {
    handle: Option<JoinHandle<()>>,
}

impl AbortOnDropJoinHandle {
    const fn new(handle: JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn wait(&mut self) -> Result<(), tokio::task::JoinError> {
        match self.handle.as_mut() {
            Some(handle) => handle.await,
            None => Ok(()),
        }
    }

    fn disarm(&mut self) {
        let _ = self.handle.take();
    }
}

impl Drop for AbortOnDropJoinHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn log_hook_task_join_result(task_name: &'static str, result: Result<(), tokio::task::JoinError>) {
    match result {
        Ok(()) => info!("Shutdown hook task '{task_name}' stopped"),
        Err(error) if error.is_cancelled() => info!("Shutdown hook task '{task_name}' cancelled"),
        Err(error) => warn!("Shutdown hook task '{task_name}' ended with join error: {error}"),
    }
}

/// Centralized collection of all shutdown resources.
///
/// Resources are executed in this order during graceful shutdown:
/// 1. Cancel all registered `CancellationToken`s (in registration order).
/// 2. Drain all background task `JoinHandle`s (budget-aware timeouts).
/// 3. Run all typed shutdown hooks (budget-aware timeouts).
///
/// Force shutdown preserves token cancellation and typed cleanup hooks, but
/// aborts registered background tasks immediately instead of waiting for them
/// to drain. This prevents stuck tasks from consuming the cleanup hook budget.
///
/// The `total_budget` limits the overall shutdown duration to stay within
/// K8s `terminationGracePeriodSeconds`. The remaining budget is divided
/// among pending tasks and hooks without ever exceeding the total budget.
pub struct ShutdownCoordinator {
    tokens: Vec<(&'static str, CancellationToken)>,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
    hooks: Vec<Box<dyn ShutdownHook>>,
    total_budget: Duration,
}

impl ShutdownCoordinator {
    /// Create a new coordinator with a total shutdown budget.
    ///
    /// The budget should match or be slightly less than the container's
    /// `terminationGracePeriodSeconds` to ensure all cleanup completes
    /// before the process is killed.
    pub fn new(total_budget: Duration) -> Self {
        Self {
            tokens: Vec::new(),
            tasks: Vec::new(),
            hooks: Vec::new(),
            total_budget,
        }
    }

    /// Register a new `CancellationToken` and return it.
    /// The token will be cancelled during shutdown in registration order.
    pub fn register_token(&mut self, name: &'static str) -> CancellationToken {
        let token = CancellationToken::new();
        self.tokens.push((name, token.clone()));
        token
    }

    /// Register a pre-existing `CancellationToken` that was created elsewhere.
    pub fn track_token(&mut self, name: &'static str, token: CancellationToken) {
        self.tokens.push((name, token));
    }

    /// Register a background task handle to be awaited during shutdown.
    pub fn register_task(&mut self, name: &'static str, handle: JoinHandle<()>) {
        self.tasks.push((name, handle));
    }

    /// Register a typed shutdown hook.
    pub fn register_hook(&mut self, hook: impl ShutdownHook + 'static) {
        self.hooks.push(Box::new(hook));
    }

    /// Execute the full shutdown sequence within the total budget.
    pub async fn shutdown(self) {
        let deadline = tokio::time::Instant::now() + self.total_budget;
        self.shutdown_with_deadline(deadline).await;
    }

    /// Execute the full shutdown sequence within the remaining time until `deadline`.
    ///
    /// This allows callers that already spent part of the process-level shutdown
    /// budget on higher-priority phases to pass the same absolute deadline down,
    /// ensuring the coordinator does not accidentally re-spend a fresh full
    /// budget and exceed the container termination window.
    pub async fn shutdown_with_deadline(self, deadline: tokio::time::Instant) {
        self.shutdown_with_deadline_and_mode(deadline, CoordinatorShutdownMode::Graceful)
            .await;
    }

    /// Execute a force shutdown sequence within the remaining time until `deadline`.
    ///
    /// Force mode still cancels tokens and runs typed cleanup hooks, but it
    /// does not spend the hook budget waiting for background tasks to finish
    /// naturally. Registered tasks are aborted first, then hooks get the
    /// remaining deadline.
    pub async fn shutdown_force_with_deadline(self, deadline: tokio::time::Instant) {
        self.shutdown_with_deadline_and_mode(deadline, CoordinatorShutdownMode::Force)
            .await;
    }

    async fn shutdown_with_deadline_and_mode(
        self,
        deadline: tokio::time::Instant,
        mode: CoordinatorShutdownMode,
    ) {
        info!(
            "Starting {} shutdown sequence (total budget: {}s)",
            mode.as_str(),
            self.total_budget.as_secs()
        );

        // Phase 1: Cancel all tokens (instant, no budget consumed)
        if !self.tokens.is_empty() {
            info!("Cancelling {} shutdown token(s)...", self.tokens.len());
            for (name, token) in &self.tokens {
                info!("Cancelling '{name}'");
                token.cancel();
            }
        }

        // Phase 2: Drain or abort background tasks.
        match mode {
            CoordinatorShutdownMode::Graceful => {
                Self::drain_background_tasks(self.tasks, self.hooks.len(), deadline).await;
            }
            CoordinatorShutdownMode::Force => {
                Self::abort_background_tasks(self.tasks).await;
            }
        }

        // Phase 3: Run typed shutdown hooks with budget-aware timeouts
        if !self.hooks.is_empty() {
            let remaining_hooks = self.hooks.len();
            info!("Running {} shutdown hook(s)...", remaining_hooks);
            for (i, hook) in self.hooks.into_iter().enumerate() {
                let name = hook.name().to_string();
                let remaining = remaining_hooks - i;
                let budget = Self::budget_per_item(deadline, remaining);
                // Use the smaller of the hook's own timeout and the budget
                let timeout = hook.timeout().min(budget);
                info!(
                    "Running shutdown hook '{name}' (timeout: {}s)...",
                    timeout.as_secs()
                );
                match tokio::time::timeout(timeout, hook.run()).await {
                    Ok(()) => {
                        info!("Shutdown hook '{name}' completed");
                    }
                    Err(_) => {
                        warn!(
                            "Shutdown hook '{name}' timed out after {}s, proceeding",
                            timeout.as_secs()
                        );
                    }
                }
            }
        }
    }

    async fn drain_background_tasks(
        tasks: Vec<(&'static str, JoinHandle<()>)>,
        hook_count: usize,
        deadline: tokio::time::Instant,
    ) {
        if tasks.is_empty() {
            return;
        }

        let remaining_items = tasks.len() + hook_count;
        info!(
            "Waiting for {} background task(s) to finish...",
            tasks.len()
        );
        for (i, (name, mut handle)) in tasks.into_iter().enumerate() {
            let remaining = remaining_items - i;
            let per_item = Self::budget_per_item(deadline, remaining);
            match tokio::time::timeout(per_item, &mut handle).await {
                Ok(Ok(())) => {
                    info!("Background task '{name}' finished");
                }
                Ok(Err(e)) => {
                    warn!("Background task '{name}' panicked: {e}");
                }
                Err(_) => {
                    warn!(
                        "Background task '{name}' did not finish within {}s, aborting",
                        per_item.as_secs()
                    );
                    handle.abort();
                    match handle.await {
                        Ok(()) => info!("Background task '{name}' aborted cleanly"),
                        Err(e) if e.is_cancelled() => {
                            info!("Background task '{name}' aborted");
                        }
                        Err(e) => {
                            warn!("Background task '{name}' failed after abort: {e}");
                        }
                    }
                }
            }
        }
    }

    async fn abort_background_tasks(tasks: Vec<(&'static str, JoinHandle<()>)>) {
        if tasks.is_empty() {
            return;
        }

        warn!(
            "Force shutdown: aborting {} background task(s) without drain wait",
            tasks.len()
        );
        for (name, handle) in &tasks {
            warn!("Force shutdown: aborting background task '{name}'");
            handle.abort();
        }

        tokio::task::yield_now().await;

        for (name, handle) in tasks {
            if !handle.is_finished() {
                warn!("Force shutdown: background task '{name}' did not stop immediately");
                continue;
            }

            match handle.await {
                Ok(()) => info!("Background task '{name}' aborted cleanly"),
                Err(e) if e.is_cancelled() => info!("Background task '{name}' aborted"),
                Err(e) => warn!("Background task '{name}' failed after forced abort: {e}"),
            }
        }
    }

    /// Compute the timeout for a single item given the remaining budget.
    ///
    /// Divides the time remaining until `deadline` equally among
    /// `remaining_items`.
    ///
    /// When the remaining budget is large enough to still afford
    /// `MIN_PER_TASK_TIMEOUT` for every pending item, the returned timeout is at
    /// least that preferred minimum. Otherwise the equal-share budget is used so
    /// the coordinator never exceeds the total shutdown budget.
    fn budget_per_item(deadline: tokio::time::Instant, remaining_items: usize) -> Duration {
        let now = tokio::time::Instant::now();
        if now >= deadline || remaining_items == 0 {
            return Duration::ZERO;
        }

        let remaining_budget = deadline - now;
        let remaining_items_u32 = u32::try_from(remaining_items).unwrap_or(u32::MAX);
        let equal_share = remaining_budget / remaining_items_u32;
        let preferred_total = MIN_PER_TASK_TIMEOUT
            .checked_mul(remaining_items_u32)
            .unwrap_or(Duration::MAX);

        if remaining_budget >= preferred_total {
            equal_share.max(MIN_PER_TASK_TIMEOUT)
        } else {
            equal_share
        }
    }
}

impl CoordinatorShutdownMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Graceful => "graceful",
            Self::Force => "force",
        }
    }
}

/// Flushes the audit service buffer before the database pool is closed.
pub struct AuditFlushHook {
    pub handle: Arc<Mutex<Option<synctv_core::service::AuditFlushHandle>>>,
}

impl ShutdownHook for AuditFlushHook {
    fn name(&self) -> &'static str {
        "audit_flush"
    }
    fn timeout(&self) -> Duration {
        Duration::from_mins(1)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut guard = self.handle.lock().await;
            if let Some(flush_handle) = guard.take() {
                let progress_interval = Duration::from_secs(10);
                let flush_timeout = Duration::from_mins(1);
                let mut elapsed = Duration::ZERO;
                let flush_fut = flush_handle.shutdown();
                tokio::pin!(flush_fut);
                loop {
                    if tokio::time::timeout(progress_interval, &mut flush_fut).await == Ok(()) {
                        info!("Audit service buffer flushed successfully");
                        return;
                    }
                    elapsed += progress_interval;
                    if elapsed >= flush_timeout {
                        warn!("Audit service flush timed out; some buffered events may be lost");
                        return;
                    }
                    info!(
                        "Audit service flush still in progress ({}/{}s elapsed)...",
                        elapsed.as_secs(),
                        flush_timeout.as_secs()
                    );
                }
            }
        })
    }
}

/// Stops the `CacheInvalidationService` (signals shutdown, trims Redis stream).
pub struct CacheInvalidationStopHook {
    pub service: Arc<dyn synctv_core::cache::CacheInvalidationRuntime>,
    pub listener_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ShutdownHook for CacheInvalidationStopHook {
    fn name(&self) -> &'static str {
        "cache_invalidation_stop"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(10)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.service.stop().await;
            let mut guard = self.listener_task.lock().await;
            if let Some(task) = guard.take() {
                task.abort();
                log_hook_task_join_result("cache_invalidation_stop", task.await);
            }
            info!("Cache invalidation service stopped");
        })
    }
}

/// Joins the `PostgreSQL` LISTEN settings task before the pool is closed.
pub struct SettingsListenHook {
    pub task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ShutdownHook for SettingsListenHook {
    fn name(&self) -> &'static str {
        "settings_listen_task"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut guard = self.task.lock().await;
            if let Some(task) = guard.take() {
                let mut task = AbortOnDropJoinHandle::new(task);
                log_hook_task_join_result("settings_listen_task", task.wait().await);
                task.disarm();
            }
        })
    }
}

/// Stops the provider invalidation Pub/Sub listener before process exit.
pub struct ProviderInvalidationHook {
    pub cancel: CancellationToken,
    pub task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ShutdownHook for ProviderInvalidationHook {
    fn name(&self) -> &'static str {
        "provider_invalidation_listener"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(15)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.cancel.cancel();
            let mut guard = self.task.lock().await;
            if let Some(task) = guard.take() {
                let mut task = AbortOnDropJoinHandle::new(task);
                log_hook_task_join_result("provider_invalidation_listener", task.wait().await);
                task.disarm();
            }
        })
    }
}

/// Stops the cache fence repair worker before process exit.
pub struct CacheFenceRepairHook {
    pub cancel: CancellationToken,
    pub task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ShutdownHook for CacheFenceRepairHook {
    fn name(&self) -> &'static str {
        "cache_fence_repair_worker"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(15)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.cancel.cancel();
            let mut guard = self.task.lock().await;
            if let Some(task) = guard.take() {
                let mut task = AbortOnDropJoinHandle::new(task);
                log_hook_task_join_result("cache_fence_repair_worker", task.wait().await);
                task.disarm();
            }
        })
    }
}

/// Generic shutdown hook for services with a `shutdown()` method.
///
/// Replaces four identical implementations (HealthMonitor, RealtimeManager,
/// PermissionService, PlaybackService) that all just call `.shutdown().await`.
pub struct SimpleShutdownHook<T: SimpleShutdownTarget> {
    name: &'static str,
    timeout: Duration,
    target: T,
}

impl<T: SimpleShutdownTarget> SimpleShutdownHook<T> {
    pub fn new(name: &'static str, timeout: Duration, target: T) -> Self {
        Self {
            name,
            timeout,
            target,
        }
    }
}

impl<T: SimpleShutdownTarget> ShutdownHook for SimpleShutdownHook<T> {
    fn name(&self) -> &'static str {
        self.name
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.target.shutdown().await;
        })
    }
}

/// Trait for types that can be shut down via a simple `shutdown()` call.
#[async_trait::async_trait]
pub trait SimpleShutdownTarget: Send + Sync + 'static {
    async fn shutdown(&self);
}

#[async_trait::async_trait]
impl SimpleShutdownTarget for Arc<dyn synctv_cluster::discovery::ClusterHealthRuntime> {
    async fn shutdown(&self) {
        (**self).shutdown().await;
    }
}

#[async_trait::async_trait]
impl SimpleShutdownTarget for Arc<synctv_realtime::sync::RealtimeManager> {
    async fn shutdown(&self) {
        (**self).shutdown().await;
    }
}

#[async_trait::async_trait]
impl SimpleShutdownTarget for synctv_core::service::PermissionService {
    async fn shutdown(&self) {
        synctv_core::service::PermissionService::shutdown(self).await;
    }
}

#[async_trait::async_trait]
impl SimpleShutdownTarget for synctv_core::service::PlaybackService {
    async fn shutdown(&self) {
        synctv_core::service::PlaybackService::shutdown(self).await;
    }
}

/// Stops a room settings cache invalidation listener task.
pub struct RoomSettingsServiceShutdownHook {
    pub name: &'static str,
    pub service: Arc<dyn RoomSettingsShutdownTarget>,
}

#[async_trait::async_trait]
pub trait RoomSettingsShutdownTarget: Send + Sync {
    async fn shutdown(&self);
}

#[async_trait::async_trait]
impl RoomSettingsShutdownTarget for synctv_core::service::RoomSettingsService {
    async fn shutdown(&self) {
        synctv_core::service::RoomSettingsService::shutdown(self).await;
    }
}

impl RoomSettingsServiceShutdownHook {
    pub fn new(name: &'static str, service: synctv_core::service::RoomSettingsService) -> Self {
        Self {
            name,
            service: Arc::new(service),
        }
    }
}

impl ShutdownHook for RoomSettingsServiceShutdownHook {
    fn name(&self) -> &str {
        self.name
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(10)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.service.shutdown().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_budget_per_item_divides_evenly() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let per = ShutdownCoordinator::budget_per_item(deadline, 3);
        // 30s / 3 = 10s
        assert!(per >= Duration::from_secs(9));
        assert!(per <= Duration::from_secs(11));
    }

    #[tokio::test]
    async fn test_budget_per_item_respects_minimum() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let per = ShutdownCoordinator::budget_per_item(deadline, 2);
        assert!(per >= MIN_PER_TASK_TIMEOUT);
    }

    #[tokio::test]
    async fn test_budget_per_item_past_deadline() {
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let per = ShutdownCoordinator::budget_per_item(deadline, 5);
        assert_eq!(per, Duration::ZERO);
    }

    #[tokio::test]
    async fn test_budget_per_item_never_exceeds_total_budget() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let per = ShutdownCoordinator::budget_per_item(deadline, 100);

        assert!(
            per < Duration::from_secs(1),
            "per-item budget should shrink with constrained total budget, got {per:?}"
        );
    }

    #[tokio::test]
    async fn test_shutdown_coordinator_completes_within_budget() {
        let budget = Duration::from_secs(10);
        let mut coord = ShutdownCoordinator::new(budget);

        let token = coord.register_token("test_token");

        let handle = tokio::spawn(async move {
            token.cancelled().await;
        });
        coord.register_task("test_task", handle);

        let start = tokio::time::Instant::now();
        coord.shutdown().await;
        let elapsed = start.elapsed();

        // Should complete quickly (tasks respond to cancel)
        assert!(elapsed < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_shutdown_cancels_registered_tokens() {
        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        let token1 = coord.register_token("token1");
        let token2 = coord.register_token("token2");

        assert!(!token1.is_cancelled());
        assert!(!token2.is_cancelled());

        coord.shutdown().await;

        assert!(token1.is_cancelled());
        assert!(token2.is_cancelled());
    }

    #[tokio::test]
    async fn test_shutdown_runs_hooks() {
        struct RecordingHook {
            events: Arc<Mutex<Vec<&'static str>>>,
        }

        impl ShutdownHook for RecordingHook {
            fn name(&self) -> &'static str {
                "recording_hook"
            }

            fn timeout(&self) -> Duration {
                Duration::from_secs(1)
            }

            fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(async move {
                    self.events.lock().await.push("hook");
                })
            }
        }

        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        let events = Arc::new(Mutex::new(Vec::new()));
        coord.register_hook(RecordingHook {
            events: Arc::clone(&events),
        });

        coord.shutdown().await;

        assert_eq!(*events.lock().await, vec!["hook"]);
    }

    #[tokio::test]
    async fn test_shutdown_order_is_tokens_then_tasks_then_hooks() {
        struct RecordingHook {
            events: Arc<Mutex<Vec<&'static str>>>,
        }

        impl ShutdownHook for RecordingHook {
            fn name(&self) -> &'static str {
                "recording_hook"
            }

            fn timeout(&self) -> Duration {
                Duration::from_secs(1)
            }

            fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(async move {
                    self.events.lock().await.push("hook");
                })
            }
        }

        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        let token = coord.register_token("token");
        let events = Arc::new(Mutex::new(Vec::new()));
        let task_events = Arc::clone(&events);
        let task_token = token.clone();

        coord.register_task(
            "task",
            tokio::spawn(async move {
                task_token.cancelled().await;
                task_events.lock().await.push("task");
            }),
        );
        coord.register_hook(RecordingHook {
            events: Arc::clone(&events),
        });

        coord.shutdown().await;

        assert!(token.is_cancelled(), "shutdown must cancel tokens first");
        assert_eq!(
            *events.lock().await,
            vec!["task", "hook"],
            "hooks must run after tasks have observed token cancellation"
        );
    }

    #[tokio::test]
    async fn test_shutdown_with_deadline_respects_external_remaining_budget() {
        let mut coord = ShutdownCoordinator::new(Duration::from_secs(30));
        coord.register_task(
            "stuck_task",
            tokio::spawn(async move {
                std::future::pending::<()>().await;
            }),
        );

        let start = tokio::time::Instant::now();
        coord
            .shutdown_with_deadline(start + Duration::from_millis(50))
            .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "external deadline should cap shutdown duration, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_force_shutdown_aborts_tasks_before_running_hooks() {
        struct RecordingHook {
            ran: Arc<AtomicBool>,
        }

        impl ShutdownHook for RecordingHook {
            fn name(&self) -> &'static str {
                "recording_hook"
            }

            fn timeout(&self) -> Duration {
                Duration::from_secs(1)
            }

            fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(async move {
                    self.ran.store(true, Ordering::SeqCst);
                })
            }
        }

        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        let hook_ran = Arc::new(AtomicBool::new(false));

        for _ in 0..32 {
            coord.register_task(
                "stuck_task",
                tokio::spawn(async move {
                    std::future::pending::<()>().await;
                }),
            );
        }
        coord.register_hook(RecordingHook {
            ran: Arc::clone(&hook_ran),
        });

        let start = tokio::time::Instant::now();
        coord
            .shutdown_force_with_deadline(start + Duration::from_millis(100))
            .await;

        assert!(
            hook_ran.load(Ordering::SeqCst),
            "force shutdown must not let stuck background tasks consume the hook budget"
        );
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "force shutdown should abort registered tasks promptly"
        );
    }

    #[tokio::test]
    async fn test_shutdown_coordinator_aborts_timed_out_tasks() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let budget = Duration::from_secs(1);
        let mut coord = ShutdownCoordinator::new(budget);
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped);

        let handle = tokio::spawn(async move {
            let _drop_flag = DropFlag(dropped_for_task);
            std::future::pending::<()>().await;
        });
        coord.register_task("stuck_task", handle);

        coord.shutdown().await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !dropped.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out background task should be aborted so its future is dropped"
        );
    }

    #[tokio::test]
    async fn test_shutdown_coordinator_aborts_timed_out_settings_hook_task() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let budget = Duration::from_millis(50);
        let mut coord = ShutdownCoordinator::new(budget);
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped);

        let task = tokio::spawn(async move {
            let _drop_flag = DropFlag(dropped_for_task);
            std::future::pending::<()>().await;
        });
        coord.register_hook(SettingsListenHook {
            task: Arc::new(Mutex::new(Some(task))),
        });

        coord.shutdown().await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !dropped.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out shutdown hook must abort the owned background task instead of detaching it"
        );
    }

    #[tokio::test]
    async fn test_cache_fence_repair_hook_cancels_and_joins_worker() {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let task_finished = Arc::clone(&finished);
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            task_finished.store(true, Ordering::SeqCst);
        });
        let task_slot = Arc::new(Mutex::new(Some(task)));

        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        coord.register_hook(CacheFenceRepairHook {
            cancel: cancel.clone(),
            task: Arc::clone(&task_slot),
        });

        coord.shutdown().await;

        assert!(
            cancel.is_cancelled(),
            "cache fence repair hook must cancel the worker token"
        );
        assert!(
            finished.load(Ordering::SeqCst),
            "cache fence repair hook must wait for the worker to exit after cancellation"
        );
        assert!(
            task_slot.lock().await.is_none(),
            "cache fence repair hook must consume the tracked join handle"
        );
    }

    #[tokio::test]
    async fn test_cache_invalidation_stop_hook_joins_listener_task() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let service = Arc::new(synctv_core::cache::CacheInvalidationService::new(
            "test-node".to_string(),
            "test:cache:invalidate".to_string(),
        ));
        let mut coord = ShutdownCoordinator::new(Duration::from_millis(50));
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped);
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_task = Arc::clone(&started);
        let task = tokio::spawn(async move {
            let _drop_flag = DropFlag(dropped_for_task);
            started_for_task.notify_one();
            loop {
                tokio::task::yield_now().await;
            }
        });

        coord.register_hook(CacheInvalidationStopHook {
            service,
            listener_task: Arc::new(Mutex::new(Some(task))),
        });

        started.notified().await;
        coord.shutdown().await;

        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cache invalidation listener task should be aborted promptly during shutdown");

        assert!(
            dropped.load(Ordering::SeqCst),
            "cache invalidation shutdown must not detach the local listener task"
        );
    }

    #[tokio::test]
    async fn test_cache_invalidation_stop_hook_does_not_consume_full_timeout_budget() {
        let service = Arc::new(synctv_core::cache::CacheInvalidationService::new(
            "test-node".to_string(),
            "test:cache:invalidate".to_string(),
        ));
        let mut coord = ShutdownCoordinator::new(Duration::from_millis(250));
        let started = Arc::new(tokio::sync::Notify::new());
        let started_for_task = Arc::clone(&started);
        let task = tokio::spawn(async move {
            started_for_task.notify_one();
            loop {
                tokio::task::yield_now().await;
            }
        });
        coord.register_hook(CacheInvalidationStopHook {
            service,
            listener_task: Arc::new(Mutex::new(Some(task))),
        });

        started.notified().await;
        let start = tokio::time::Instant::now();
        coord.shutdown().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(150),
            "cache invalidation shutdown should abort the local listener promptly instead of burning the hook timeout budget: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_realtime_manager_shutdown_hook_runs_manager_shutdown() {
        use synctv_realtime::sync::{RealtimeConfig, RealtimeManager, RoomMessageHub};

        let manager = Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "hook-test-node".to_string(),
                dedup_window: Duration::from_secs(1),
                critical_channel_capacity: 16,
                publish_channel_capacity: 16,
                key_prefix: "hook-test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("realtime manager should initialize"),
        );

        let cancel_token = manager.cancel_token().clone();
        assert!(
            !cancel_token.is_cancelled(),
            "manager should start with an active cancel token"
        );

        let mut coord = ShutdownCoordinator::new(Duration::from_secs(1));
        coord.register_hook(SimpleShutdownHook::new(
            "realtime_manager",
            Duration::from_secs(15),
            manager,
        ));
        coord.shutdown().await;

        assert!(
            cancel_token.is_cancelled(),
            "realtime manager shutdown hook must invoke RealtimeManager::shutdown"
        );
    }

    struct CountingRoomSettingsShutdownTarget {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl RoomSettingsShutdownTarget for CountingRoomSettingsShutdownTarget {
        async fn shutdown(&self) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn test_room_settings_shutdown_hook_uses_instance_name_and_runs_target() {
        let room_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chat_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let room_hook = RoomSettingsServiceShutdownHook {
            name: "room_service_settings",
            service: Arc::new(CountingRoomSettingsShutdownTarget {
                calls: Arc::clone(&room_calls),
            }),
        };
        let chat_hook = RoomSettingsServiceShutdownHook {
            name: "chat_service_settings",
            service: Arc::new(CountingRoomSettingsShutdownTarget {
                calls: Arc::clone(&chat_calls),
            }),
        };

        assert_eq!(
            ShutdownHook::name(&room_hook),
            "room_service_settings",
            "room service hook must expose its configured shutdown name"
        );
        assert_eq!(
            ShutdownHook::name(&chat_hook),
            "chat_service_settings",
            "chat service hook must expose its configured shutdown name"
        );
        assert_ne!(
            ShutdownHook::name(&room_hook),
            ShutdownHook::name(&chat_hook)
        );

        Box::new(room_hook).run().await;
        Box::new(chat_hook).run().await;

        assert_eq!(room_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(chat_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
