//! Singleflight for cache stampede protection
//!
//! Wraps the `async_singleflight` crate to prevent cache stampede (thundering herd)
//! by ensuring that only one request executes for a given key when multiple
//! concurrent requests miss the cache simultaneously.
//!
//! # Example
//! ```
//! use synctv_core::cache::SingleFlight;
//!
//! # async fn example() {
//! let sf = SingleFlight::<String, String, String>::new();
//! let result = sf.do_work("user:123".to_string(), async {
//!     Ok("user_data".to_string())
//! }).await;
//! # }
//! ```

use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

/// Error type for `SingleFlight` operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum SingleFlightError<E> {
    /// The worker task panicked or was cancelled
    #[error("SingleFlight worker failed - leader dropped or panicked")]
    WorkerFailed,
    /// The underlying operation failed
    #[error("{0}")]
    Inner(E),
}

/// `SingleFlight` prevents duplicate concurrent function executions.
///
/// When multiple tasks attempt to execute the same operation (by key)
/// simultaneously, only one execution proceeds while others wait for the result.
///
/// Backed by the `async_singleflight` crate which handles leader failure
/// and automatic retry.
#[derive(Clone)]
pub struct SingleFlight<K, V, E>
where
    K: Hash + Eq + Clone + Debug + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    group: Arc<async_singleflight::Group<K, V, E>>,
}

impl<K, V, E> SingleFlight<K, V, E>
where
    K: Hash + Eq + Clone + Debug + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    /// Create a new `SingleFlight` instance
    #[must_use]
    pub fn new() -> Self {
        Self {
            group: Arc::new(async_singleflight::Group::new()),
        }
    }

    /// Execute a function only once for a given key
    ///
    /// If another call for the same key is in progress, this will wait for
    /// that result instead of executing the function again.
    ///
    /// Uses `work()` which automatically retries if the leader is dropped.
    /// Returns `Err(None)` from the library maps to `WorkerFailed`.
    pub async fn do_work<Fut>(&self, key: K, f: Fut) -> Result<V, SingleFlightError<E>>
    where
        Fut: std::future::Future<Output = Result<V, E>> + Send,
    {
        // Group::work returns Result<V, Option<E>>:
        //   Ok(v)       => success
        //   Err(Some(e)) => inner error from the function
        //   Err(None)    => leader failed/dropped (after retry attempts)
        self.group
            .work(&key, f)
            .await
            .map_err(|opt_err| match opt_err {
                Some(inner) => SingleFlightError::Inner(inner),
                None => SingleFlightError::WorkerFailed,
            })
    }

    /// Execute a function only once for a given key, with legacy error type
    ///
    /// Converts `SingleFlightError` back to `E` using the provided error factory
    /// for worker failures.
    pub async fn do_work_with_fallback<Fut, Ef>(
        &self,
        key: K,
        f: Fut,
        error_factory: Ef,
    ) -> Result<V, E>
    where
        Fut: std::future::Future<Output = Result<V, E>> + Send,
        Ef: FnOnce() -> E,
    {
        self.do_work(key, f).await.map_err(|e| match e {
            SingleFlightError::WorkerFailed => error_factory(),
            SingleFlightError::Inner(err) => err,
        })
    }
}

impl<K, V, E> Default for SingleFlight<K, V, E>
where
    K: Hash + Eq + Clone + Debug + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_singleflight_single_request() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let counter_clone = counter.clone();
        let result = sf
            .do_work("key1".to_string(), async move {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            })
            .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_singleflight_deduplicates_concurrent_requests() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = vec![];
        for _ in 0..10 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.do_work("same_key".to_string(), async move {
                    sleep(Duration::from_millis(50)).await;
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(123)
                })
                .await
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap().unwrap();
            assert_eq!(result, 123);
        }

        // The function should have been called only once
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_singleflight_different_keys() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let sf1 = sf.clone();
        let c1 = counter.clone();
        let h1 = tokio::spawn(async move {
            sf1.do_work("key1".to_string(), async move {
                c1.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            })
            .await
        });

        let sf2 = sf.clone();
        let c2 = counter.clone();
        let h2 = tokio::spawn(async move {
            sf2.do_work("key2".to_string(), async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok(2)
            })
            .await
        });

        let r1 = h1.await.unwrap().unwrap();
        let r2 = h2.await.unwrap().unwrap();

        assert_eq!(r1, 1);
        assert_eq!(r2, 2);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_singleflight_error_propagation() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();

        let result = sf
            .do_work("error_key".to_string(), async {
                Err("test error".to_string())
            })
            .await;

        match result {
            Err(SingleFlightError::Inner(msg)) => assert_eq!(msg, "test error"),
            _ => panic!("Expected Inner error"),
        }
    }

    #[tokio::test]
    async fn test_singleflight_fallback_wrapper() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let counter_clone = counter.clone();
        let result = sf
            .do_work_with_fallback(
                "key1".to_string(),
                async move {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                    Ok(42)
                },
                || "worker failed".to_string(),
            )
            .await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_singleflight_recovery_after_error() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();

        // First request fails
        let result = sf
            .do_work("fail_key".to_string(), async {
                Err("intentional error".to_string())
            })
            .await;
        assert!(result.is_err());

        // Second request with same key should work
        let result = sf.do_work("fail_key".to_string(), async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    /// Stress test: 100 concurrent tasks on same key - thundering herd protection
    #[tokio::test]
    async fn test_singleflight_thundering_herd_100_tasks() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = vec![];
        for _ in 0..100 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.do_work("hot_key".to_string(), async move {
                    sleep(Duration::from_millis(20)).await;
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(999)
                })
                .await
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap().unwrap();
            assert_eq!(result, 999);
        }

        // The function should have been called only once (or very few times
        // if the library retries on leader failure)
        let actual = counter.load(Ordering::SeqCst);
        assert!(
            actual <= 3,
            "Expected at most 3 executions (leader + retries), got {actual}"
        );
    }

    /// Stress test: multiple keys concurrently
    #[tokio::test]
    async fn test_singleflight_multiple_keys_concurrent_stress() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = vec![];
        // 10 keys, 10 tasks each = 100 total tasks
        for key_idx in 0..10 {
            for _ in 0..10 {
                let sf = sf.clone();
                let counter = counter.clone();
                let key = format!("key_{key_idx}");
                handles.push(tokio::spawn(async move {
                    sf.do_work(key, async move {
                        sleep(Duration::from_millis(10)).await;
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(key_idx)
                    })
                    .await
                }));
            }
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }

        // Each key should execute only once (or a small number of retries)
        let actual = counter.load(Ordering::SeqCst);
        assert!(
            actual <= 30,
            "Expected at most ~30 executions (10 keys * ~3 max), got {actual}"
        );
        // But must be at least 10 (one per key)
        assert!(
            actual >= 10,
            "Expected at least 10 executions (one per key), got {actual}"
        );
    }

    /// Test: error from one key doesn't affect other keys
    #[tokio::test]
    async fn test_singleflight_error_isolation() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::new();

        let mut handles = vec![];

        // Key "fail" always errors
        for _ in 0..5 {
            let sf = sf.clone();
            handles.push(tokio::spawn(async move {
                sf.do_work("fail".to_string(), async {
                    sleep(Duration::from_millis(5)).await;
                    Err("always fails".to_string())
                })
                .await
            }));
        }

        // Key "success" always succeeds
        for _ in 0..5 {
            let sf = sf.clone();
            handles.push(tokio::spawn(async move {
                sf.do_work("success".to_string(), async {
                    sleep(Duration::from_millis(5)).await;
                    Ok(42)
                })
                .await
            }));
        }

        let mut fail_count = 0;
        let mut success_count = 0;

        for handle in handles {
            match handle.await.unwrap() {
                Ok(v) => {
                    assert_eq!(v, 42);
                    success_count += 1;
                }
                Err(_) => fail_count += 1,
            }
        }

        assert_eq!(success_count, 5);
        assert_eq!(fail_count, 5);
    }

    /// Test: clone shares the same underlying group
    #[test]
    fn test_singleflight_clone_shares_state() {
        let sf1: SingleFlight<String, i32, String> = SingleFlight::new();
        let sf2 = sf1.clone();
        // Both clones share the same Arc<Group>
        assert!(Arc::ptr_eq(&sf1.group, &sf2.group));
    }

    /// Test: Default trait produces a new instance
    #[test]
    fn test_singleflight_default() {
        let sf: SingleFlight<String, i32, String> = SingleFlight::default();
        // Just verify it was created successfully
        assert!(Arc::strong_count(&sf.group) >= 1);
    }
}
