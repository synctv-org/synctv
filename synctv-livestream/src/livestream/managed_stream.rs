// Shared lifecycle state and pool utilities for managed streams.

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, warn, Instrument};

/// Shared lifecycle state for pull streams and external publish streams.
///
/// The cleanup contract is: active viewer paths increment or touch this state,
/// idle cleanup claims only streams with zero subscribers, and the claim marks
/// the stream unhealthy before teardown. HLS requests usually touch lifecycle
/// per playlist/segment request; FLV holds a subscriber guard for the streaming
/// task lifetime.
pub(crate) struct StreamLifecycle {
    subscriber_count: AtomicUsize,
    last_active_secs: AtomicU64,
    is_running: Arc<AtomicBool>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
    abort_handle: parking_lot::Mutex<Option<tokio::task::AbortHandle>>,
}

/// Get current unix timestamp in seconds.
fn unix_now_secs() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(error) => {
            tracing::warn!(%error, "system clock is before Unix epoch");
            0
        }
    }
}

impl StreamLifecycle {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            subscriber_count: AtomicUsize::new(0),
            last_active_secs: AtomicU64::new(unix_now_secs()),
            is_running: Arc::new(AtomicBool::new(false)),
            task_handle: Mutex::new(None),
            abort_handle: parking_lot::Mutex::new(None),
        }
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.subscriber_count.load(Ordering::Acquire)
    }

    pub(crate) fn increment_subscriber_count(&self) {
        self.subscriber_count.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn decrement_subscriber_count(&self) {
        let result = self
            .subscriber_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    None
                }
            });
        if result.is_err() {
            warn!("Attempted to decrement subscriber count below zero");
        }
    }

    pub(crate) async fn is_healthy(&self) -> bool {
        if !self.is_running.load(Ordering::Acquire) {
            return false;
        }

        if let Some(handle) = self.task_handle.lock().await.as_ref() {
            !handle.is_finished()
        } else {
            true
        }
    }

    pub(crate) fn set_running(&self) {
        self.is_running.store(true, Ordering::Release);
    }

    pub(crate) fn mark_stopping(&self) {
        self.is_running.store(false, Ordering::Release);
    }

    pub(crate) fn restore_running(&self) {
        self.is_running.store(true, Ordering::Release);
    }

    /// Claim the stream for idle cleanup after marking it unhealthy.
    ///
    /// This compare/exchange is the race gate between cleanup and a new viewer.
    /// A concurrent subscriber restores the stream to running and keeps it in
    /// the pool.
    pub(crate) fn try_claim_for_cleanup(&self) -> bool {
        self.mark_stopping();

        let cas_result =
            self.subscriber_count
                .compare_exchange(0, 0, Ordering::AcqRel, Ordering::Acquire);

        if cas_result.is_err() {
            self.restore_running();
            return false;
        }

        true
    }

    pub(crate) fn is_running_clone(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_running)
    }

    pub(crate) fn last_active_elapsed_secs(&self) -> u64 {
        let last = self.last_active_secs.load(Ordering::Acquire);
        unix_now_secs().saturating_sub(last)
    }

    pub(crate) fn update_last_active_time(&self) {
        self.last_active_secs
            .store(unix_now_secs(), Ordering::Release);
    }

    pub(crate) async fn set_task_handle(&self, handle: tokio::task::JoinHandle<Result<()>>) {
        *self.abort_handle.lock() = Some(handle.abort_handle());
        *self.task_handle.lock().await = Some(handle);
    }

    pub(crate) async fn abort_task(&self) {
        if let Some(handle) = self.task_handle.lock().await.take() {
            handle.abort();
        }
    }
}

impl Default for StreamLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StreamLifecycle {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Release);

        if let Some(ah) = self.abort_handle.lock().take() {
            ah.abort();
        }
    }
}

/// Trait for streams managed by [`StreamPool`].
#[async_trait]
pub(crate) trait ManagedStream: Send + Sync + 'static {
    fn lifecycle(&self) -> &StreamLifecycle;

    async fn stop_managed(&self) {
        self.lifecycle().mark_stopping();
        self.lifecycle().abort_task().await;
    }
}

struct CreationLockEntry {
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl CreationLockEntry {
    fn new() -> Self {
        Self {
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

pub(crate) struct CreationLockGuard {
    key: String,
    creation_locks: Arc<DashMap<String, Arc<CreationLockEntry>>>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for CreationLockGuard {
    fn drop(&mut self) {
        self.creation_locks.remove(&self.key);
    }
}

pub(crate) struct StreamPool<S: ManagedStream> {
    pub(crate) streams: Arc<DashMap<String, Arc<S>>>,
    creation_locks: Arc<DashMap<String, Arc<CreationLockEntry>>>,
    pub(crate) cleanup_check_interval: Duration,
    pub(crate) idle_timeout: Duration,
    cancel_token: CancellationToken,
}

impl<S: ManagedStream> StreamPool<S> {
    #[must_use]
    pub(crate) fn new(cleanup_check_interval: Duration, idle_timeout: Duration) -> Self {
        Self {
            streams: Arc::new(DashMap::new()),
            creation_locks: Arc::new(DashMap::new()),
            cleanup_check_interval,
            idle_timeout,
            cancel_token: CancellationToken::new(),
        }
    }

    pub(crate) async fn stop_all(&self) {
        let keys: Vec<String> = self.streams.iter().map(|e| e.key().clone()).collect();
        for key in &keys {
            if let Some((_, stream)) = self.streams.remove(key) {
                stream.stop_managed().await;
            }
        }
        self.creation_locks.clear();
        debug!("Stopped all managed streams ({} removed)", keys.len());
    }

    /// Returns a healthy stream with its subscriber count already incremented.
    pub(crate) async fn get_existing(&self, stream_key: &str) -> Option<Arc<S>> {
        if let Some(stream) = self.streams.get(stream_key) {
            if stream.lifecycle().is_healthy().await {
                stream.lifecycle().increment_subscriber_count();

                if stream.lifecycle().is_healthy().await {
                    stream.lifecycle().update_last_active_time();
                    return Some(stream.clone());
                }

                stream.lifecycle().decrement_subscriber_count();
            }
            drop(stream);
            self.streams.remove(stream_key);
        }
        None
    }

    pub(crate) async fn acquire_creation_lock(&self, stream_key: &str) -> CreationLockGuard {
        let entry = self
            .creation_locks
            .entry(stream_key.to_string())
            .or_insert_with(|| Arc::new(CreationLockEntry::new()));
        let lock = Arc::clone(&entry.lock);
        CreationLockGuard {
            key: stream_key.to_string(),
            creation_locks: Arc::clone(&self.creation_locks),
            _guard: lock.lock_owned().await,
        }
    }

    pub(crate) fn insert_and_cleanup<F>(
        &self,
        stream_key: String,
        stream: Arc<S>,
        on_idle_cleanup: F,
    ) where
        F: Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        self.streams.insert(stream_key.clone(), Arc::clone(&stream));

        let streams = Arc::clone(&self.streams);
        let check_interval = self.cleanup_check_interval;
        let idle_timeout = self.idle_timeout;
        let child_token = self.cancel_token.child_token();

        let span = info_span!("stream_cleanup", stream_key = %stream_key);
        tokio::spawn(
            async move {
                tokio::select! {
                    () = child_token.cancelled() => {
                        debug!("Cleanup task cancelled for {} (shutdown)", stream_key);
                    }
                    result = Self::cleanup_loop(
                        &stream_key,
                        &stream,
                        &streams,
                        check_interval,
                        idle_timeout,
                        &on_idle_cleanup,
                    ) => {
                        if let Err(e) = result {
                            error!("Cleanup task failed for {}: {}", stream_key, e);
                            stream.lifecycle().abort_task().await;
                            streams.remove(&stream_key);
                        }
                    }
                }
            }
            .instrument(span),
        );
    }

    async fn cleanup_loop<F>(
        stream_key: &str,
        stream: &Arc<S>,
        streams: &Arc<DashMap<String, Arc<S>>>,
        check_interval: Duration,
        idle_timeout: Duration,
        on_idle_cleanup: &F,
    ) -> Result<()>
    where
        F: Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let mut interval = tokio::time::interval(check_interval);

        loop {
            interval.tick().await;

            if stream.lifecycle().subscriber_count() == 0 {
                let idle_secs = stream.lifecycle().last_active_elapsed_secs();

                if idle_secs > idle_timeout.as_secs() {
                    if !stream.lifecycle().try_claim_for_cleanup() {
                        let current_count = stream.lifecycle().subscriber_count();
                        debug!(
                            "Cleanup aborted for {}: {} late subscriber(s) detected after mark_stopping",
                            stream_key,
                            current_count,
                        );

                        let is_same_instance = streams
                            .get(stream_key)
                            .is_some_and(|map_entry| Arc::ptr_eq(map_entry.value(), stream));
                        if !is_same_instance {
                            debug!(
                                "Cleanup exiting for {}: stream was replaced by concurrent viewer",
                                stream_key,
                            );
                            stream.lifecycle().mark_stopping();
                            break;
                        }
                        continue;
                    }

                    info!(
                        "Auto cleanup: Stopping stream {} (idle for {}s)",
                        stream_key, idle_secs
                    );

                    on_idle_cleanup(stream_key).await;

                    streams.remove(stream_key);
                    stream.stop_managed().await;
                    break;
                }
            } else {
                stream.lifecycle().update_last_active_time();
            }
        }
        Ok(())
    }
}

impl<S: ManagedStream> Drop for StreamPool<S> {
    fn drop(&mut self) {
        self.cancel_token.cancel();

        for entry in self.streams.iter() {
            entry
                .value()
                .lifecycle()
                .is_running
                .store(false, Ordering::Release);
            if let Some(ah) = entry.value().lifecycle().abort_handle.lock().take() {
                ah.abort();
            }
        }

        let count = self.streams.len();
        self.streams.clear();
        self.creation_locks.clear();
        if count > 0 {
            debug!("StreamPool dropped, cleaned up {} streams", count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    struct TestStream {
        lifecycle: StreamLifecycle,
        stop_count: AtomicUsize,
    }

    #[async_trait]
    impl ManagedStream for TestStream {
        fn lifecycle(&self) -> &StreamLifecycle {
            &self.lifecycle
        }

        async fn stop_managed(&self) {
            self.stop_count.fetch_add(1, Ordering::AcqRel);
            self.lifecycle.mark_stopping();
        }
    }

    #[tokio::test]
    async fn test_stream_lifecycle_subscriber_count() {
        let lc = StreamLifecycle::new();
        lc.increment_subscriber_count();
        assert_eq!(lc.subscriber_count(), 1);
        lc.increment_subscriber_count();
        assert_eq!(lc.subscriber_count(), 2);
        lc.decrement_subscriber_count();
        assert_eq!(lc.subscriber_count(), 1);
        lc.decrement_subscriber_count();
        assert_eq!(lc.subscriber_count(), 0);
        // Underflow should be a no-op
        lc.decrement_subscriber_count();
        assert_eq!(lc.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_stream_lifecycle_health() {
        let lc = StreamLifecycle::new();
        assert!(!lc.is_healthy().await);

        lc.set_running();
        assert!(lc.is_healthy().await);

        lc.mark_stopping();
        assert!(!lc.is_healthy().await);

        lc.restore_running();
        assert!(lc.is_healthy().await);
    }

    #[tokio::test]
    async fn test_stream_pool_get_existing_empty() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));
        assert!(pool.get_existing("key").await.is_none());
    }

    #[tokio::test]
    async fn test_stream_pool_get_existing_healthy() -> TestResult {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();

        pool.streams.insert("room:media".to_string(), stream);

        let found = pool.get_existing("room:media").await;
        let found = found.ok_or_else(|| test_error("healthy stream should exist"))?;
        assert_eq!(found.lifecycle().subscriber_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_stream_pool_get_existing_unhealthy_removed() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        // Not running, so unhealthy

        pool.streams.insert("room:media".to_string(), stream);

        let found = pool.get_existing("room:media").await;
        assert!(found.is_none());
        assert!(pool.streams.is_empty());
    }

    /// Test that try_claim_for_cleanup succeeds when subscriber count is 0.
    #[tokio::test]
    async fn test_try_claim_for_cleanup_succeeds_with_zero_subscribers() {
        let lc = StreamLifecycle::new();
        lc.set_running();
        assert!(lc.is_healthy().await);

        // Claim for cleanup should succeed (count == 0)
        let claimed = lc.try_claim_for_cleanup();
        assert!(
            claimed,
            "Should claim for cleanup when subscriber_count == 0"
        );

        // After claim, stream should not be healthy (marked stopping)
        assert!(!lc.is_healthy().await);
    }

    /// Test that try_claim_for_cleanup fails when subscriber count > 0,
    /// simulating a concurrent subscriber that raced in.
    #[tokio::test]
    async fn test_try_claim_for_cleanup_fails_with_active_subscriber() {
        let lc = StreamLifecycle::new();
        lc.set_running();
        lc.increment_subscriber_count();

        // Claim for cleanup should fail (count == 1)
        let claimed = lc.try_claim_for_cleanup();
        assert!(
            !claimed,
            "Should not claim for cleanup when subscriber_count > 0"
        );

        // Stream should still be healthy (restore_running was called)
        assert!(lc.is_healthy().await);
        assert_eq!(lc.subscriber_count(), 1);
    }

    /// Test that concurrent subscribe during cleanup is handled gracefully.
    /// Simulates the race: cleanup claims the stream, then a subscriber tries to attach.
    #[tokio::test]
    async fn test_concurrent_subscribe_during_cleanup_handled_gracefully() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();
        pool.streams
            .insert("room:media".to_string(), stream.clone());

        // Simulate cleanup claiming the stream
        let claimed = stream.lifecycle().try_claim_for_cleanup();
        assert!(claimed);

        // Now try to get_existing - should fail because stream is marked stopping
        let found = pool.get_existing("room:media").await;
        assert!(
            found.is_none(),
            "get_existing should return None for a stream being cleaned up"
        );
    }

    /// Test that get_existing correctly handles the double-check pattern
    /// when a stream becomes unhealthy between initial check and increment.
    #[tokio::test]
    async fn test_get_existing_double_check_on_concurrent_stop() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();
        pool.streams
            .insert("room:media".to_string(), stream.clone());

        // First get_existing should succeed
        let found = pool.get_existing("room:media").await;
        assert!(found.is_some());
        assert_eq!(stream.lifecycle().subscriber_count(), 1);

        // Decrement the subscriber (simulating disconnect)
        stream.lifecycle().decrement_subscriber_count();

        // Mark stopping (simulating cleanup)
        stream.lifecycle().mark_stopping();

        // get_existing should fail for stopped stream
        let found = pool.get_existing("room:media").await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_stop_all_uses_stream_specific_stop_protocol() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();
        pool.streams
            .insert("room:media".to_string(), stream.clone());

        pool.stop_all().await;

        assert_eq!(
            stream.stop_count.load(Ordering::Acquire),
            1,
            "stop_all must call the stream-specific stop protocol"
        );
        assert!(pool.streams.is_empty());
        assert!(!stream.lifecycle().is_healthy().await);
    }

    #[tokio::test]
    async fn test_idle_cleanup_stops_stream_and_removes_pool_entry() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_millis(10), Duration::from_millis(20));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();

        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_count_for_task = Arc::clone(&cleanup_count);
        pool.insert_and_cleanup(
            "room:media".to_string(),
            Arc::clone(&stream),
            move |_stream_key| {
                let cleanup_count = Arc::clone(&cleanup_count_for_task);
                Box::pin(async move {
                    cleanup_count.fetch_add(1, Ordering::AcqRel);
                })
            },
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !pool.streams.contains_key("room:media") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("idle cleanup should remove the stream");

        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert_eq!(stream.stop_count.load(Ordering::Acquire), 1);
        assert!(!stream.lifecycle().is_healthy().await);
    }

    #[tokio::test]
    async fn test_idle_cleanup_waits_for_subscriber_guard_release() -> TestResult {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_millis(10), Duration::from_millis(20));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            stop_count: AtomicUsize::new(0),
        });
        stream.lifecycle().set_running();
        stream.lifecycle().increment_subscriber_count();

        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let cleanup_count_for_task = Arc::clone(&cleanup_count);
        pool.insert_and_cleanup(
            "room:media".to_string(),
            Arc::clone(&stream),
            move |_stream_key| {
                let cleanup_count = Arc::clone(&cleanup_count_for_task);
                Box::pin(async move {
                    cleanup_count.fetch_add(1, Ordering::AcqRel);
                })
            },
        );

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            pool.streams.contains_key("room:media"),
            "active subscribers should keep the stream in the pool"
        );
        assert_eq!(cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(stream.stop_count.load(Ordering::Acquire), 0);

        stream.lifecycle().decrement_subscriber_count();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !pool.streams.contains_key("room:media") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| test_error("idle cleanup should run after subscriber release"))?;

        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert_eq!(stream.stop_count.load(Ordering::Acquire), 1);
        Ok(())
    }
}
