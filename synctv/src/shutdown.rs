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

/// Centralized collection of all shutdown resources.
///
/// Resources are executed in this order during shutdown:
/// 1. Cancel all registered `CancellationToken`s (in registration order).
/// 2. Drain all background task `JoinHandle`s (with per-task timeouts).
/// 3. Run all typed shutdown hooks (with per-hook timeouts).
pub struct ShutdownCoordinator {
    tokens: Vec<(&'static str, CancellationToken)>,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
    hooks: Vec<Box<dyn ShutdownHook>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            tokens: Vec::new(),
            tasks: Vec::new(),
            hooks: Vec::new(),
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

    /// Execute the full shutdown sequence.
    pub async fn shutdown(self) {
        // Phase 1: Cancel all tokens
        if !self.tokens.is_empty() {
            info!("Cancelling {} shutdown token(s)...", self.tokens.len());
            for (name, token) in &self.tokens {
                info!("Cancelling '{name}'");
                token.cancel();
            }
        }

        // Phase 2: Drain background tasks
        if !self.tasks.is_empty() {
            info!("Waiting for {} background task(s) to finish...", self.tasks.len());
            for (name, handle) in self.tasks {
                match tokio::time::timeout(Duration::from_secs(30), handle).await {
                    Ok(Ok(())) => {
                        info!("Background task '{name}' finished");
                    }
                    Ok(Err(e)) => {
                        warn!("Background task '{name}' panicked: {e}");
                    }
                    Err(_) => {
                        warn!("Background task '{name}' did not finish within 30s, proceeding");
                    }
                }
            }
        }

        // Phase 3: Run typed shutdown hooks
        if !self.hooks.is_empty() {
            info!("Running {} shutdown hook(s)...", self.hooks.len());
            for hook in self.hooks {
                let name = hook.name().to_string();
                let timeout = hook.timeout();
                info!("Running shutdown hook '{name}' (timeout: {}s)...", timeout.as_secs());
                match tokio::time::timeout(timeout, hook.run()).await {
                    Ok(()) => {
                        info!("Shutdown hook '{name}' completed");
                    }
                    Err(_) => {
                        warn!("Shutdown hook '{name}' timed out after {}s, proceeding", timeout.as_secs());
                    }
                }
            }
        }
    }
}

// -- Concrete shutdown hooks --------------------------------------------------

/// Flushes the audit service buffer before the database pool is closed.
pub struct AuditFlushHook {
    pub handle: Arc<Mutex<Option<synctv_core::service::AuditFlushHandle>>>,
}

impl ShutdownHook for AuditFlushHook {
    fn name(&self) -> &str {
        "audit_flush"
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(60)
    }
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let mut guard = self.handle.lock().await;
            if let Some(flush_handle) = guard.take() {
                let progress_interval = Duration::from_secs(10);
                let flush_timeout = Duration::from_secs(60);
                let mut elapsed = Duration::ZERO;
                let flush_fut = flush_handle.shutdown();
                tokio::pin!(flush_fut);
                loop {
                    match tokio::time::timeout(progress_interval, &mut flush_fut).await {
                        Ok(()) => {
                            info!("Audit service buffer flushed successfully");
                            return;
                        }
                        Err(_) => {
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
                }
            }
        })
    }
}

/// Joins the PostgreSQL LISTEN settings task before the pool is closed.
pub struct SettingsListenHook {
    pub task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ShutdownHook for SettingsListenHook {
    fn name(&self) -> &str {
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
