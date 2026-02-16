//! Shared optimistic locking retry utility
//!
//! Provides exponential backoff with jitter for operations that may encounter
//! `OptimisticLockConflict` errors. This pattern is used across multiple services
//! (PlaybackService, RoomService, MemberService, RoomSettingsService).

use std::future::Future;

use rand::RngExt;

use crate::{Error, Result};

/// Default maximum retry attempts for optimistic lock conflicts
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default base delay for exponential backoff (milliseconds)
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 5;

/// Retry an async operation that may fail with `OptimisticLockConflict`.
///
/// Uses exponential backoff with jitter to avoid thundering herd:
/// delay = base_ms * 2^attempt + random(0..base_ms)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_succeeds_on_first_try() {
        let result = retry_with_optimistic_lock(3, 5, "failed", || async {
            Ok::<_, Error>(42)
        })
        .await;

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
            other => panic!("Expected Internal error, got: {:?}", other),
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
}
