//! Shared optimistic locking retry utility
//!
//! Provides exponential backoff with jitter for operations that may encounter
//! `OptimisticLockConflict` errors. This pattern is used across multiple services
//! (`PlaybackService`, `RoomService`, `MemberService`, `RoomSettingsService`).

use std::future::Future;
use std::time::Duration;

use rand::RngExt;

use crate::{Error, Result};

/// Default maximum retry attempts for optimistic lock conflicts
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default base delay for exponential backoff (milliseconds)
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 5;

/// Default total timeout for retry operations (seconds)
pub const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Check whether an error is the exact retry-exhaustion outcome for the
/// provided optimistic-retry operation.
#[must_use]
pub fn is_retry_exhausted(error: &Error, error_msg: &str) -> bool {
    match error {
        Error::Internal(msg) => msg == error_msg,
        Error::Timeout(msg) => msg.starts_with(error_msg),
        _ => false,
    }
}

/// Retry an async operation that may fail with `OptimisticLockConflict`.
///
/// Uses exponential backoff with jitter to avoid thundering herd:
/// delay = `base_ms` * 2^attempt + `random(0..base_ms)`
///
/// # Arguments
/// * `max_retries` - Maximum number of attempts before giving up
/// * `base_backoff_ms` - Base delay in milliseconds (doubles each retry)
/// * `error_msg` - Error message to use when all retries are exhausted
/// * `operation` - Async closure that returns `Result<T>`. Called on each attempt.
///
/// # Returns
/// The successful result from the operation, or an error if all retries are exhausted.
pub async fn retry_with_optimistic_lock<F, Fut, T>(
    max_retries: u32,
    base_backoff_ms: u64,
    error_msg: &str,
    operation: F,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for attempt in 0..max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(Error::OptimisticLockConflict) if attempt + 1 < max_retries => {
                let backoff = base_backoff_ms * (1 << attempt);
                let jitter = rand::rng().random_range(0..base_backoff_ms);
                tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                continue;
            }
            Err(Error::OptimisticLockConflict) => {
                // Final attempt exhausted
                return Err(Error::Internal(error_msg.to_string()));
            }
            Err(e) => return Err(e),
        }
    }

    Err(Error::Internal(error_msg.to_string()))
}

/// Retry an async operation with a total timeout limit.
///
/// Like `retry_with_optimistic_lock`, but wraps the entire retry loop in a timeout.
/// This prevents scenarios where slow database operations combined with retries
/// cause unacceptably long request times.
///
/// # Arguments
/// * `max_retries` - Maximum number of attempts before giving up
/// * `base_backoff_ms` - Base delay in milliseconds (doubles each retry)
/// * `timeout` - Maximum total time for all retries combined
/// * `error_msg` - Error message to use when all retries are exhausted
/// * `operation` - Async closure that returns `Result<T>`. Called on each attempt.
///
/// # Returns
/// The successful result from the operation, or an error if:
/// - All retries are exhausted (Internal error)
/// - Total timeout exceeded (Timeout error)
/// - Operation returns a non-conflict error (propagated as-is)
pub async fn retry_with_optimistic_lock_timeout<F, Fut, T>(
    max_retries: u32,
    base_backoff_ms: u64,
    timeout: Duration,
    error_msg: &str,
    operation: F,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    tokio::time::timeout(timeout, async {
        for attempt in 0..max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(Error::OptimisticLockConflict) if attempt + 1 < max_retries => {
                    let backoff = base_backoff_ms * (1 << attempt);
                    let jitter = rand::rng().random_range(0..base_backoff_ms);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff + jitter)).await;
                    continue;
                }
                Err(Error::OptimisticLockConflict) => {
                    return Err(Error::Internal(error_msg.to_string()));
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::Internal(error_msg.to_string()))
    })
    .await
    .map_err(|_| Error::Timeout(format!("{error_msg} (timeout after {timeout:?})")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_succeeds_on_first_try() {
        let result =
            retry_with_optimistic_lock(3, 5, "failed", || async { Ok::<_, Error>(42) }).await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_succeeds_after_retry() {
        let attempts = AtomicU32::new(0);

        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(Error::OptimisticLockConflict)
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_exhausts_retries() {
        let result = retry_with_optimistic_lock(3, 1, "all retries exhausted", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => assert_eq!(msg, "all retries exhausted"),
            other => panic!("Expected Internal error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_non_conflict_error_not_retried() {
        let attempts = AtomicU32::new(0);

        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>(Error::NotFound("not found".to_string())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_succeeds_on_last_attempt() {
        let attempts = AtomicU32::new(0);

        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                // Fail first 2 attempts, succeed on 3rd (last chance)
                if attempt < 2 {
                    Err(Error::OptimisticLockConflict)
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_zero_retries_returns_error_immediately() {
        let result = retry_with_optimistic_lock(0, 1, "no retries allowed", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => assert_eq!(msg, "no retries allowed"),
            other => panic!("Expected Internal error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_one_retry_fails_immediately() {
        let attempts = AtomicU32::new(0);

        let result = retry_with_optimistic_lock(1, 1, "single attempt", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>(Error::OptimisticLockConflict) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        match result.unwrap_err() {
            Error::Internal(msg) => assert_eq!(msg, "single attempt"),
            other => panic!("Expected Internal error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_one_retry_succeeds() {
        let result =
            retry_with_optimistic_lock(1, 1, "single attempt", || async { Ok::<_, Error>(42) })
                .await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_preserves_non_conflict_error_type() {
        let result = retry_with_optimistic_lock(3, 1, "failed", || async {
            Err::<i32, _>(Error::Authorization("access denied".to_string()))
        })
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Authorization(msg) => assert_eq!(msg, "access denied"),
            other => panic!("Expected Authorization error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_different_error_types_not_retried() {
        use std::sync::atomic::AtomicI32;

        let attempts = AtomicI32::new(0);

        // Test InvalidInput error
        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>(Error::InvalidInput("bad input".to_string())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // Reset and test AlreadyExists error
        attempts.store(0, Ordering::SeqCst);
        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>(Error::AlreadyExists("already there".to_string())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_returns_unit_type() {
        let result =
            retry_with_optimistic_lock(3, 1, "failed", || async { Ok::<_, Error>(()) }).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_exponential_backoff_timing() {
        use std::time::Instant;

        // With base_backoff_ms=10 and 3 retries that all fail:
        // attempt 0: backoff = 10 * 1 = 10ms + jitter
        // attempt 1: backoff = 10 * 2 = 20ms + jitter
        // Total: at least 30ms (without jitter)
        let start = Instant::now();

        let _ = retry_with_optimistic_lock(3, 10, "timed out", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;

        let elapsed = start.elapsed();
        // Should be at least 30ms (10 + 20) but allow for some variance
        assert!(
            elapsed.as_millis() >= 25,
            "Expected at least 25ms delay, got {elapsed:?}"
        );
        // Should not be excessively long (with jitter, max ~80ms)
        assert!(
            elapsed.as_millis() < 150,
            "Expected less than 150ms delay, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_string_return_type() {
        let result = retry_with_optimistic_lock(3, 1, "failed", || async {
            Ok::<_, Error>("success".to_string())
        })
        .await;

        assert_eq!(result.unwrap(), "success");
    }

    #[tokio::test]
    async fn test_option_return_type() {
        let result =
            retry_with_optimistic_lock(3, 1, "failed", || async { Ok::<_, Error>(Some(42)) }).await;

        assert_eq!(result.unwrap(), Some(42));
    }

    #[tokio::test]
    async fn test_vec_return_type() {
        let result =
            retry_with_optimistic_lock(3, 1, "failed", || async { Ok::<_, Error>(vec![1, 2, 3]) })
                .await;

        assert_eq!(result.unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_concurrent_retries_independent() {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        let counter1 = Arc::new(AtomicU32::new(0));
        let counter2 = Arc::new(AtomicU32::new(0));

        let c1 = counter1.clone();
        let handle1 = tokio::spawn(async move {
            retry_with_optimistic_lock(3, 1, "failed", move || {
                let attempt = c1.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 1 {
                        Err(Error::OptimisticLockConflict)
                    } else {
                        Ok(1)
                    }
                }
            })
            .await
        });

        let c2 = counter2.clone();
        let handle2 = tokio::spawn(async move {
            retry_with_optimistic_lock(3, 1, "failed", move || {
                let attempt = c2.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(Error::OptimisticLockConflict)
                    } else {
                        Ok(2)
                    }
                }
            })
            .await
        });

        let result1 = handle1.await.unwrap();
        let result2 = handle2.await.unwrap();

        assert_eq!(result1.unwrap(), 1);
        assert_eq!(result2.unwrap(), 2);
        assert_eq!(counter1.load(Ordering::SeqCst), 2);
        assert_eq!(counter2.load(Ordering::SeqCst), 3);
    }

    // ========== Timeout Tests ==========

    #[tokio::test]
    async fn test_timeout_succeeds_on_first_try() {
        let result =
            retry_with_optimistic_lock_timeout(3, 5, Duration::from_secs(5), "failed", || async {
                Ok::<_, Error>(42)
            })
            .await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_timeout_succeeds_after_retry() {
        let attempts = AtomicU32::new(0);

        let result =
            retry_with_optimistic_lock_timeout(3, 1, Duration::from_secs(5), "failed", || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(Error::OptimisticLockConflict)
                    } else {
                        Ok(42)
                    }
                }
            })
            .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_timeout_exhausts_retries() {
        let result = retry_with_optimistic_lock_timeout(
            3,
            1,
            Duration::from_secs(5),
            "all retries exhausted",
            || async { Err::<i32, _>(Error::OptimisticLockConflict) },
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => assert_eq!(msg, "all retries exhausted"),
            other => panic!("Expected Internal error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_timeout_exceeded() {
        use std::time::Instant;

        let start = Instant::now();

        // Use a very short timeout (50ms) with slow operations
        let result = retry_with_optimistic_lock_timeout(
            10,                        // Many retries
            10,                        // 10ms base backoff
            Duration::from_millis(50), // 50ms timeout
            "timeout test",
            || async {
                // Each operation takes 30ms, so 2 operations = 60ms > 50ms timeout
                tokio::time::sleep(Duration::from_millis(30)).await;
                Err::<i32, _>(Error::OptimisticLockConflict)
            },
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Timeout(msg) => {
                assert!(msg.contains("timeout test"));
                assert!(msg.contains("timeout"));
            }
            other => panic!("Expected Timeout error, got: {other:?}"),
        }

        // Should timeout around 50ms, not wait for all 10 retries
        assert!(
            elapsed.as_millis() < 200,
            "Expected timeout around 50ms, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_timeout_preserves_non_conflict_error() {
        let result =
            retry_with_optimistic_lock_timeout(3, 1, Duration::from_secs(5), "failed", || async {
                Err::<i32, _>(Error::NotFound("not found".to_string()))
            })
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::NotFound(msg) => assert_eq!(msg, "not found"),
            other => panic!("Expected NotFound error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_timeout_completes_within_limit() {
        use std::time::Instant;

        let start = Instant::now();

        // Fast operations with generous timeout should complete quickly
        let result =
            retry_with_optimistic_lock_timeout(3, 1, Duration::from_secs(5), "failed", || async {
                Ok::<_, Error>(42)
            })
            .await;

        let elapsed = start.elapsed();

        assert_eq!(result.unwrap(), 42);
        // Should complete almost instantly
        assert!(
            elapsed.as_millis() < 100,
            "Expected fast completion, got {elapsed:?}"
        );
    }

    #[test]
    fn test_is_retry_exhausted_matches_exact_internal_message() {
        let error = Error::Internal("operation failed after retries".to_string());
        assert!(is_retry_exhausted(&error, "operation failed after retries"));
    }

    #[test]
    fn test_is_retry_exhausted_rejects_partial_internal_match() {
        let error = Error::Internal("wrapper: operation failed after retries".to_string());
        assert!(!is_retry_exhausted(&error, "operation failed after retries"));
    }

    #[test]
    fn test_is_retry_exhausted_matches_timeout_prefix() {
        let error = Error::Timeout(
            "operation failed after retries (timeout after 5s)".to_string(),
        );
        assert!(is_retry_exhausted(&error, "operation failed after retries"));
    }
}
