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

/// Minimum per-task timeout when dividing global budget.
const MIN_PER_TASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Centralized collection of all shutdown resources.
///
/// Resources are executed in this order during shutdown:
/// 1. Cancel all registered `CancellationToken`s (in registration order).
/// 2. Drain all background task `JoinHandle`s (budget-aware timeouts).
/// 3. Run all typed shutdown hooks (budget-aware timeouts).
///
/// The `total_budget` limits the overall shutdown duration to stay within
/// K8s `terminationGracePeriodSeconds`. The remaining budget is divided
/// among pending tasks and hooks, with a per-item minimum of 5 seconds.
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
        info!(
            "Starting shutdown sequence (total budget: {}s)",
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

        // Phase 2: Drain background tasks with budget-aware timeouts
        if !self.tasks.is_empty() {
            let remaining_items = self.tasks.len() + self.hooks.len();
            info!(
                "Waiting for {} background task(s) to finish...",
                self.tasks.len()
            );
            for (i, (name, handle)) in self.tasks.into_iter().enumerate() {
                let remaining = remaining_items - i;
                let per_item = Self::budget_per_item(deadline, remaining);
                match tokio::time::timeout(per_item, handle).await {
                    Ok(Ok(())) => {
                        info!("Background task '{name}' finished");
                    }
                    Ok(Err(e)) => {
                        warn!("Background task '{name}' panicked: {e}");
                    }
                    Err(_) => {
                        warn!(
                            "Background task '{name}' did not finish within {}s, proceeding",
                            per_item.as_secs()
                        );
                    }
                }
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

    /// Compute the timeout for a single item given the remaining budget.
    ///
    /// Divides the time remaining until `deadline` equally among `remaining_items`,
    /// with a minimum of `MIN_PER_TASK_TIMEOUT` seconds per item.
    fn budget_per_item(deadline: tokio::time::Instant, remaining_items: usize) -> Duration {
        let now = tokio::time::Instant::now();
        if now >= deadline || remaining_items == 0 {
            return MIN_PER_TASK_TIMEOUT;
        }
        let remaining_budget = deadline - now;
        let per_item = remaining_budget / remaining_items as u32;
        per_item.max(MIN_PER_TASK_TIMEOUT)
    }
}

// -- Concrete shutdown hooks --------------------------------------------------

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
    pub service: Arc<synctv_core::cache::CacheInvalidationService>,
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
                let _ = task.await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let per = ShutdownCoordinator::budget_per_item(deadline, 100);
        assert_eq!(per, MIN_PER_TASK_TIMEOUT);
    }

    #[tokio::test]
    async fn test_budget_per_item_past_deadline() {
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let per = ShutdownCoordinator::budget_per_item(deadline, 5);
        assert_eq!(per, MIN_PER_TASK_TIMEOUT);
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
}
