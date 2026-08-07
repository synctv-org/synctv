//! Shared retry utility for optimistic lock conflicts.

use std::future::Future;
use std::time::Duration;

use rand::RngExt;

use crate::{Error, Result};

/// Default maximum retry attempts for optimistic lock conflicts
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default base delay for exponential backoff (milliseconds)
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 5;

/// Check whether an error is the retry-exhaustion outcome for an operation.
#[must_use]
pub fn is_retry_exhausted(error: &Error, error_msg: &str) -> bool {
    match error {
        Error::Internal(msg) => msg == error_msg,
        Error::Timeout(msg) => msg.starts_with(error_msg),
        _ => false,
    }
}

async fn retry_optimistic_lock_loop<F, Fut, T>(
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
                tokio::time::sleep(retry_delay(base_backoff_ms, attempt)).await;
            }
            Err(Error::OptimisticLockConflict) => {
                return Err(Error::Internal(error_msg.to_string()));
            }
            Err(error) => return Err(error),
        }
    }

    Err(Error::Internal(error_msg.to_string()))
}

fn retry_delay(base_backoff_ms: u64, attempt: u32) -> Duration {
    let backoff = base_backoff_ms.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
    let jitter = if base_backoff_ms == 0 {
        0
    } else {
        rand::rng().random_range(0..base_backoff_ms)
    };

    Duration::from_millis(backoff.saturating_add(jitter))
}

/// Retry an async operation that may fail with `OptimisticLockConflict`.
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
    retry_optimistic_lock_loop(max_retries, base_backoff_ms, error_msg, operation).await
}

/// Retry an async operation with a total timeout limit.
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
    tokio::time::timeout(
        timeout,
        retry_optimistic_lock_loop(max_retries, base_backoff_ms, error_msg, operation),
    )
    .await
    .map_err(|_| Error::Timeout(format!("{error_msg} (timeout after {timeout:?})")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{err, ok};
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_succeeds_on_first_try() {
        let result =
            retry_with_optimistic_lock(3, 5, "failed", || async { Ok::<_, Error>(42) }).await;

        assert_eq!(ok(result, "retry should succeed on first try"), 42);
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

        assert_eq!(ok(result, "retry should eventually succeed"), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_zero_backoff_retries_without_delay() {
        let attempts = AtomicU32::new(0);

        let result = retry_with_optimistic_lock(2, 0, "failed", || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(Error::OptimisticLockConflict)
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(ok(result, "zero backoff retry should succeed"), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_exhausts_retries() {
        let result = retry_with_optimistic_lock(3, 1, "all retries exhausted", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;

        assert!(result.is_err());
        match err(result, "retries should be exhausted") {
            Error::Internal(msg) => assert_eq!(msg, "all retries exhausted"),
            other => std::panic::panic_any(format!("Expected Internal error, got: {other:?}")),
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
                if attempt < 2 {
                    Err(Error::OptimisticLockConflict)
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(ok(result, "retry should succeed on last attempt"), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_zero_retries_returns_error_immediately() {
        let result = retry_with_optimistic_lock(0, 1, "no retries allowed", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;

        assert!(result.is_err());
        match err(result, "zero retries should fail") {
            Error::Internal(msg) => assert_eq!(msg, "no retries allowed"),
            other => std::panic::panic_any(format!("Expected Internal error, got: {other:?}")),
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
        match err(result, "single retry should fail immediately") {
            Error::Internal(msg) => assert_eq!(msg, "single attempt"),
            other => std::panic::panic_any(format!("Expected Internal error, got: {other:?}")),
        }
    }

    #[tokio::test]
    async fn test_one_retry_succeeds() {
        let result =
            retry_with_optimistic_lock(1, 1, "single attempt", || async { Ok::<_, Error>(42) })
                .await;

        assert_eq!(ok(result, "single retry should succeed"), 42);
    }

    #[tokio::test]
    async fn test_preserves_non_conflict_error_type() {
        let result = retry_with_optimistic_lock(3, 1, "failed", || async {
            Err::<i32, _>(Error::Authorization("access denied".to_string()))
        })
        .await;

        assert!(result.is_err());
        match err(result, "authorization error should be preserved") {
            Error::Authorization(msg) => assert_eq!(msg, "access denied"),
            other => {
                std::panic::panic_any(format!("Expected Authorization error, got: {other:?}"));
            }
        }
    }

    #[tokio::test]
    async fn test_different_error_types_not_retried() {
        use std::sync::atomic::AtomicI32;

        let attempts = AtomicI32::new(0);

        let result = retry_with_optimistic_lock(3, 1, "failed", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>(Error::InvalidInput("bad input".to_string())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

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

        let start = Instant::now();

        let result = retry_with_optimistic_lock(3, 10, "timed out", || async {
            Err::<i32, _>(Error::OptimisticLockConflict)
        })
        .await;
        assert!(result.is_err());

        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() >= 25,
            "Expected at least 25ms delay, got {elapsed:?}"
        );
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

        assert_eq!(ok(result, "string result should succeed"), "success");
    }

    #[tokio::test]
    async fn test_option_return_type() {
        let result =
            retry_with_optimistic_lock(3, 1, "failed", || async { Ok::<_, Error>(Some(42)) }).await;

        assert_eq!(ok(result, "option result should succeed"), Some(42));
    }

    #[tokio::test]
    async fn test_vec_return_type() {
        let result =
            retry_with_optimistic_lock(3, 1, "failed", || async { Ok::<_, Error>(vec![1, 2, 3]) })
                .await;

        assert_eq!(ok(result, "vec result should succeed"), vec![1, 2, 3]);
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

        let result1 = match handle1.await {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("first retry task should join: {error}")),
        };
        let result2 = match handle2.await {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("second retry task should join: {error}")),
        };

        assert_eq!(ok(result1, "first retry task should succeed"), 1);
        assert_eq!(ok(result2, "second retry task should succeed"), 2);
        assert_eq!(counter1.load(Ordering::SeqCst), 2);
        assert_eq!(counter2.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_timeout_succeeds_on_first_try() {
        let result =
            retry_with_optimistic_lock_timeout(3, 5, Duration::from_secs(5), "failed", || async {
                Ok::<_, Error>(42)
            })
            .await;

        assert_eq!(ok(result, "timeout retry should succeed on first try"), 42);
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

        assert_eq!(ok(result, "timeout retry should eventually succeed"), 42);
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
        match err(result, "timeout retry should exhaust retries") {
            Error::Internal(msg) => assert_eq!(msg, "all retries exhausted"),
            other => std::panic::panic_any(format!("Expected Internal error, got: {other:?}")),
        }
    }

    #[tokio::test]
    async fn test_timeout_exceeded() {
        use std::time::Instant;

        let start = Instant::now();

        let result = retry_with_optimistic_lock_timeout(
            10,
            10,
            Duration::from_millis(50),
            "timeout test",
            || async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Err::<i32, _>(Error::OptimisticLockConflict)
            },
        )
        .await;

        let elapsed = start.elapsed();

        assert!(result.is_err());
        match err(result, "retry should time out") {
            Error::Timeout(msg) => {
                assert!(msg.contains("timeout test"));
                assert!(msg.contains("timeout"));
            }
            other => std::panic::panic_any(format!("Expected Timeout error, got: {other:?}")),
        }

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
        match err(result, "not found error should be preserved") {
            Error::NotFound(msg) => assert_eq!(msg, "not found"),
            other => std::panic::panic_any(format!("Expected NotFound error, got: {other:?}")),
        }
    }

    #[tokio::test]
    async fn test_timeout_completes_within_limit() {
        use std::time::Instant;

        let start = Instant::now();

        let result =
            retry_with_optimistic_lock_timeout(3, 1, Duration::from_secs(5), "failed", || async {
                Ok::<_, Error>(42)
            })
            .await;

        let elapsed = start.elapsed();

        assert_eq!(ok(result, "timeout retry should complete"), 42);
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
        assert!(!is_retry_exhausted(
            &error,
            "operation failed after retries"
        ));
    }

    #[test]
    fn test_is_retry_exhausted_matches_timeout_prefix() {
        let error = Error::Timeout("operation failed after retries (timeout after 5s)".to_string());
        assert!(is_retry_exhausted(&error, "operation failed after retries"));
    }
}
