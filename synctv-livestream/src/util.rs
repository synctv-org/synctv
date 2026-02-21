//! Shared utilities for the livestream crate.

use rand::RngExt;

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
