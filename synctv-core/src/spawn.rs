//! Monitored task spawning utility
//!
//! Wraps `tokio::spawn` to catch panics from fire-and-forget tasks and log them
//! via `tracing::error!`, plus increment a Prometheus counter. Without this,
//! panics inside `tokio::spawn` are silently swallowed when the `JoinHandle` is
//! dropped.

use std::future::Future;

use futures::FutureExt;
use tokio::task::JoinHandle;

/// Spawn a monitored async task.
///
/// Behaves like `tokio::spawn`, but wraps the future with panic detection.
/// If the task panics, the panic is caught, logged with `tracing::error!`,
/// and a Prometheus counter is incremented before re-panicking (so the
/// `JoinHandle` still reports the panic if awaited).
///
/// # Arguments
/// * `name` - A static label for the task (used in logs and metrics).
/// * `future` - The future to execute.
pub fn spawn_monitored<F>(name: &'static str, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;

        match result {
            Ok(value) => value,
            Err(panic_payload) => {
                let msg = panic_message(&panic_payload);

                tracing::error!(
                    task_name = name,
                    panic_message = %msg,
                    "Spawned task panicked"
                );

                crate::metrics::task::TASK_PANICS_TOTAL
                    .with_label_values(&[name])
                    .inc();

                // Re-panic so the JoinHandle still reports the error if awaited
                std::panic::resume_unwind(panic_payload);
            }
        }
    })
}

/// Extract a human-readable message from a panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_spawn_monitored_success() {
        let result = spawn_monitored("test_success", async { 42 })
            .await
            .expect("monitored task should complete");
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_spawn_monitored_catches_panic() {
        let handle = spawn_monitored("test_panic", async {
            panic!("intentional test panic");
        });

        let result = handle.await;
        assert!(result.is_err());
        assert!(result.unwrap_err().is_panic());
    }

    #[tokio::test]
    async fn test_spawn_monitored_runs_to_completion() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        spawn_monitored("test_completion", async move {
            flag_clone.store(true, Ordering::SeqCst);
        })
        .await
        .expect("monitored task should complete");

        assert!(flag.load(Ordering::SeqCst));
    }
}
