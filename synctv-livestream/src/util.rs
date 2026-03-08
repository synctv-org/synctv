//! Shared utilities for the livestream crate.

use rand::RngExt;
use std::future::Future;
use tokio::task::JoinHandle;

/// Exponential backoff with jitter.
///
/// Delays for `initial_ms * 2^(attempt-1)` capped at `max_ms`, with +/- 25% jitter
/// to prevent thundering herd on retry storms.
pub async fn backoff(attempt: u32, initial_ms: u64, max_ms: u64) {
    let base = initial_ms.saturating_mul(1u64 << attempt.min(16));
    let capped = base.min(max_ms);
    // Add jitter: +/- 25% using proper RNG
    let jitter_range = capped / 4;
    let random_offset = if jitter_range > 0 {
        rand::rng().random_range(0..=(jitter_range * 2))
    } else {
        0
    };
    let delay = (capped.saturating_sub(jitter_range) + random_offset).min(max_ms);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
}

/// Best-effort spawn that does nothing when no Tokio runtime is available.
///
/// This is intended for `Drop` paths and other fire-and-forget cleanup where
/// panicking during runtime teardown would be worse than skipping async cleanup.
pub fn try_spawn<F>(future: F) -> Option<JoinHandle<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::runtime::Handle::try_current()
        .ok()
        .map(|handle| handle.spawn(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_spawn_returns_none_without_runtime() {
        let result = try_spawn(async {});
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_spawn_spawns_when_runtime_exists() {
        let handle = try_spawn(async { 42 }).expect("runtime should be available");
        assert_eq!(handle.await.expect("task should complete"), 42);
    }
}
