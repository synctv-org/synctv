// Shared lifecycle state and pool utilities for managed streams.
// Both PullStreamManager and ExternalPublishManager follow the same pattern:
// - Streams tracked in a DashMap with double-checked locking for creation
// - Subscriber counting, health checks, last-active tracking
// - Background cleanup task that stops idle streams
// This module extracts the common parts.

use anyhow::Result;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, info_span, warn, Instrument};

/// Common lifecycle state shared by all managed streams.
///
/// Handles subscriber counting, health tracking, last-active timestamps,
/// and task handle management. Embed this in your stream struct and delegate.
pub struct StreamLifecycle {
    subscriber_count: AtomicUsize,
    /// Stores unix timestamp seconds as `AtomicU64` (lock-free last-active tracking).
    last_active_secs: AtomicU64,
    is_running: Arc<AtomicBool>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
    /// Stored separately so `Drop` can abort the task without acquiring the async Mutex.
    /// Dropping a `JoinHandle` only detaches the task (it keeps running); calling
    /// `AbortHandle::abort()` actually cancels it.
    abort_handle: parking_lot::Mutex<Option<tokio::task::AbortHandle>>,
}

/// Get current unix timestamp in seconds.
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl StreamLifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriber_count: AtomicUsize::new(0),
            last_active_secs: AtomicU64::new(unix_now_secs()),
            is_running: Arc::new(AtomicBool::new(false)),
            task_handle: Mutex::new(None),
            abort_handle: parking_lot::Mutex::new(None),
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscriber_count.load(Ordering::Acquire)
    }

    pub fn increment_subscriber_count(&self) {
        self.subscriber_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn decrement_subscriber_count(&self) {
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

    /// Check if the stream is healthy.
    ///
    /// A stream is considered healthy if:
    /// 1. The `is_running` flag is true
    /// 2. If a task handle is set, it must still be running (not finished)
    ///
    /// If no task handle is set, we rely solely on the `is_running` flag.
    /// This is useful for unit tests or scenarios where task tracking isn't needed.
    pub async fn is_healthy(&self) -> bool {
        if !self.is_running.load(Ordering::Acquire) {
            return false;
        }

        // Check if the task is still running (if a task handle exists)
        // If the task finished (with or without panic), the stream is not healthy
        if let Some(handle) = self.task_handle.lock().await.as_ref() {
            !handle.is_finished()
        } else {
            // No task handle set - trust the is_running flag
            true
        }
    }

    pub fn set_running(&self) {
        self.is_running.store(true, Ordering::Release);
    }

    /// Mark as stopping -- new `is_healthy()` calls return false.
    pub fn mark_stopping(&self) {
        self.is_running.store(false, Ordering::Release);
    }

    /// Restore running state (used when cleanup detects a late subscriber).
    pub fn restore_running(&self) {
        self.is_running.store(true, Ordering::Release);
    }

    /// Atomically attempt to claim the stream for cleanup.
    ///
    /// Returns `true` if cleanup can proceed, `false` if a concurrent subscriber
    /// raced in.
    ///
    /// Protocol (mark-stopping + verify):
    /// 1. Mark stopping (`is_running = false`) so `is_healthy()` returns false
    ///    for any new subscriber attempting `get_existing()`.
    /// 2. Atomically read `subscriber_count` to verify it is still 0. The CAS
    ///    `compare_exchange(0, 0)` acts as an atomic load-with-acquire: if the
    ///    value is not 0, a concurrent subscriber incremented between the idle
    ///    check and now, and cleanup must abort.
    /// 3. If a subscriber raced in (CAS failed), restore running and return false.
    ///
    /// Safety of the 0->0 CAS: the value is intentionally unchanged because we
    /// only need to *verify* the count, not mutate it. The `get_existing()`
    /// double-check provides the complementary safety: after incrementing the
    /// subscriber count, it re-checks `is_healthy()`, so if `mark_stopping()`
    /// has already been called, the subscriber undoes its increment and treats
    /// the stream as gone. Together these two double-checks guarantee that
    /// either the subscriber is attached to a live stream, or cleanup sees the
    /// subscriber and backs off.
    pub fn try_claim_for_cleanup(&self) -> bool {
        // Step 1: Mark stopping to make is_healthy() return false for new subscribers.
        self.mark_stopping();

        // Step 2: Atomically verify subscriber_count is still 0.
        // CAS(0, 0) is an atomic read that fails if any subscriber incremented
        // between our idle check and now.
        let cas_result =
            self.subscriber_count
                .compare_exchange(0, 0, Ordering::AcqRel, Ordering::Acquire);

        if cas_result.is_err() {
            // A subscriber raced in — restore running and abort cleanup.
            self.restore_running();
            return false;
        }

        true
    }

    /// Clone the `is_running` flag for use in spawned tasks.
    /// Allows marking the stream as unhealthy from within the task.
    pub fn is_running_clone(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_running)
    }

    /// Returns elapsed seconds since last activity (lock-free).
    pub fn last_active_elapsed_secs(&self) -> u64 {
        let last = self.last_active_secs.load(Ordering::Acquire);
        unix_now_secs().saturating_sub(last)
    }

    /// Update last-active timestamp to now (lock-free).
    pub fn update_last_active_time(&self) {
        self.last_active_secs
            .store(unix_now_secs(), Ordering::Release);
    }

    pub async fn set_task_handle(&self, handle: tokio::task::JoinHandle<Result<()>>) {
        *self.abort_handle.lock() = Some(handle.abort_handle());
        *self.task_handle.lock().await = Some(handle);
    }

    pub async fn abort_task(&self) {
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
        // Mark as not running so any external health checks fail immediately
        self.is_running.store(false, Ordering::Release);

        // Abort the task via the stored AbortHandle. This doesn't need the async
        // Mutex and always succeeds — unlike try_lock() on the JoinHandle Mutex
        // which could fail under contention, leaving the task running (detached).
        if let Some(ah) = self.abort_handle.lock().take() {
            ah.abort();
        }
    }
}

/// Trait for streams managed by [`StreamPool`].
pub trait ManagedStream: Send + Sync + 'static {
    fn lifecycle(&self) -> &StreamLifecycle;
    fn stream_key(&self) -> String;
}

/// Creation lock entry with last access time for cleanup
struct CreationLockEntry {
    lock: Arc<tokio::sync::Mutex<()>>,
    /// Unix timestamp seconds stored as `AtomicU64`.
    /// `AtomicUsize` would overflow on 32-bit targets in 2038.
    last_accessed: AtomicU64,
}

impl CreationLockEntry {
    fn new() -> Self {
        Self {
            lock: Arc::new(tokio::sync::Mutex::new(())),
            last_accessed: AtomicU64::new(unix_now_secs()),
        }
    }

    fn touch(&self) {
        self.last_accessed.store(unix_now_secs(), Ordering::Relaxed);
    }

    fn age_seconds(&self) -> u64 {
        let last = self.last_accessed.load(Ordering::Relaxed);
        unix_now_secs().saturating_sub(last)
    }
}

/// Generic stream pool with double-checked locking and idle cleanup.
///
/// Provides the common infrastructure for both `PullStreamManager` and
/// `ExternalPublishManager`: creation locks, fast-path reuse of healthy
/// streams, and background idle cleanup.
pub struct StreamPool<S: ManagedStream> {
    pub streams: Arc<DashMap<String, Arc<S>>>,
    creation_locks: Arc<DashMap<String, Arc<CreationLockEntry>>>,
    pub cleanup_check_interval: Duration,
    pub idle_timeout: Duration,
    /// Maximum age of unused creation locks before cleanup (prevents memory leak)
    creation_lock_max_age: Duration,
    /// Cancellation token for cleanup tasks — cancelled on shutdown.
    cancel_token: CancellationToken,
}

impl<S: ManagedStream> StreamPool<S> {
    #[must_use]
    pub fn new(cleanup_check_interval: Duration, idle_timeout: Duration) -> Self {
        Self {
            streams: Arc::new(DashMap::new()),
            creation_locks: Arc::new(DashMap::new()),
            cleanup_check_interval,
            idle_timeout,
            // Clean up creation locks that haven't been used for 10 minutes
            creation_lock_max_age: Duration::from_mins(10),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Cancel all cleanup tasks. Call during server shutdown.
    pub fn cancel_all(&self) {
        self.cancel_token.cancel();
    }

    /// Stop all managed streams: abort their tasks and clear the pool.
    ///
    /// Call this during graceful shutdown to ensure all streams are cleaned up.
    pub async fn stop_all(&self) {
        let keys: Vec<String> = self.streams.iter().map(|e| e.key().clone()).collect();
        for key in &keys {
            if let Some((_, stream)) = self.streams.remove(key) {
                stream.lifecycle().mark_stopping();
                stream.lifecycle().abort_task().await;
            }
        }
        self.creation_locks.clear();
        debug!("Stopped all managed streams ({} removed)", keys.len());
    }

    /// Try to reuse an existing healthy stream (fast path, no lock).
    ///
    /// **Subscriber count contract**: If this returns `Some`, the subscriber count
    /// has already been incremented exactly once. The caller MUST NOT increment
    /// again -- and MUST eventually call `decrement_subscriber_count()` once when
    /// the viewer disconnects.
    ///
    /// Returns `None` and removes the unhealthy entry if the stream is stale.
    ///
    /// **TOCTOU mitigation**: The health check and subscriber-count increment are
    /// not natively atomic (the `is_running` flag and `subscriber_count` are separate
    /// atomics). To close the race window where the stream becomes unhealthy between
    /// the `is_healthy()` check and `increment_subscriber_count()`, we use the same
    /// double-check protocol as `cleanup_loop`:
    ///   1. Check healthy.
    ///   2. Increment subscriber count.
    ///   3. Re-check healthy. If now unhealthy, undo the increment and return None.
    ///
    /// The cleanup loop performs the symmetric check: it calls `mark_stopping()` then
    /// re-reads `subscriber_count()`. If it observes count > 0 it aborts cleanup and
    /// calls `restore_running()`. Together these two double-checks guarantee that
    /// either the subscriber is attached to a live stream, or cleanup sees the
    /// subscriber and backs off.
    pub async fn get_existing(&self, stream_key: &str) -> Option<Arc<S>> {
        if let Some(stream) = self.streams.get(stream_key) {
            // Step 1: Initial health check.
            if stream.lifecycle().is_healthy().await {
                // Step 2: Optimistically increment the subscriber count.
                stream.lifecycle().increment_subscriber_count();

                // Step 3: Re-check health after incrementing to detect the race where
                // the cleanup task called mark_stopping() between steps 1 and 2.
                // The cleanup loop in cleanup_loop() re-checks subscriber_count after
                // mark_stopping(), so if we see is_healthy() == false here, the cleanup
                // task has already committed to stopping — undo the increment and fall
                // through to treat the stream as gone.
                if stream.lifecycle().is_healthy().await {
                    stream.lifecycle().update_last_active_time();
                    return Some(stream.clone());
                }

                // Cleanup claimed the stream between our two checks — undo the increment.
                stream.lifecycle().decrement_subscriber_count();
            }
            drop(stream);
            self.streams.remove(stream_key);
        }
        None
    }

    /// Acquire the per-key creation lock. Hold the returned guard while
    /// creating the stream to prevent duplicate creation.
    pub async fn acquire_creation_lock(
        &self,
        stream_key: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = self
            .creation_locks
            .entry(stream_key.to_string())
            .or_insert_with(|| Arc::new(CreationLockEntry::new()));
        entry.touch();
        let lock = Arc::clone(&entry.lock);
        lock.lock_owned().await
    }

    /// Remove the creation lock for a stream key (called when stream is destroyed)
    pub fn remove_creation_lock(&self, stream_key: &str) {
        self.creation_locks.remove(stream_key);
    }

    /// Periodically clean up old unused creation locks to prevent memory leak
    pub fn cleanup_old_creation_locks(&self) {
        let max_age = self.creation_lock_max_age;
        self.creation_locks
            .retain(|_key, entry| entry.age_seconds() < max_age.as_secs());
    }

    /// Start a background task that periodically cleans up stale creation locks.
    ///
    /// This should be called once during initialization to prevent memory leaks
    /// from failed stream creation attempts that leave orphaned lock entries.
    /// The task respects the pool's `CancellationToken` and will stop on shutdown.
    #[must_use]
    pub fn start_creation_lock_cleanup(&self) -> tokio::task::JoinHandle<()> {
        let creation_locks = Arc::clone(&self.creation_locks);
        let max_age = self.creation_lock_max_age;
        let check_interval = self.cleanup_check_interval;
        let child_token = self.cancel_token.child_token();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);
            loop {
                tokio::select! {
                    () = child_token.cancelled() => {
                        debug!("Creation lock cleanup task cancelled (shutdown)");
                        break;
                    }
                    _ = interval.tick() => {
                        let before = creation_locks.len();
                        creation_locks.retain(|_key, entry| {
                            entry.age_seconds() < max_age.as_secs()
                        });
                        let after = creation_locks.len();
                        if before != after {
                            debug!(
                                "Cleaned up {} stale creation lock entries",
                                before - after
                            );
                        }
                    }
                }
            }
        })
    }

    /// Insert a stream and spawn the idle cleanup task.
    ///
    /// `on_idle_cleanup` is called during cleanup, before stopping the stream.
    /// Use it for extra teardown (e.g., Redis unregistration).
    /// The cleanup task respects the pool's `CancellationToken` and will exit on shutdown.
    pub fn insert_and_cleanup<F>(&self, stream_key: String, stream: Arc<S>, on_idle_cleanup: F)
    where
        F: Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        self.streams.insert(stream_key.clone(), Arc::clone(&stream));

        let streams = Arc::clone(&self.streams);
        let creation_locks = Arc::clone(&self.creation_locks);
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
                        &creation_locks,
                        check_interval,
                        idle_timeout,
                        &on_idle_cleanup,
                    ) => {
                        if let Err(e) = result {
                            error!("Cleanup task failed for {}: {}", stream_key, e);
                            stream.lifecycle().abort_task().await;
                            streams.remove(&stream_key);
                            creation_locks.remove(&stream_key);
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
        creation_locks: &Arc<DashMap<String, Arc<CreationLockEntry>>>,
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
                    // Use atomic claim to prevent race between subscriber count
                    // check and stream removal. try_claim_for_cleanup() atomically marks
                    // stopping and verifies subscriber_count is still 0 via CAS.
                    if !stream.lifecycle().try_claim_for_cleanup() {
                        let current_count = stream.lifecycle().subscriber_count();
                        debug!(
                            "Cleanup aborted for {}: {} late subscriber(s) detected after mark_stopping",
                            stream_key,
                            current_count,
                        );

                        // Verify we're still the same stream in the DashMap using Arc
                        // pointer equality. A concurrent viewer may have seen us as
                        // unhealthy (during mark_stopping), removed us, and created a
                        // replacement stream. If the stream in the map is a different
                        // instance, exit to avoid two concurrent streams for the same key.
                        let is_same_instance = streams
                            .get(stream_key)
                            .is_some_and(|map_entry| Arc::ptr_eq(map_entry.value(), stream));
                        if !is_same_instance {
                            debug!(
                                "Cleanup exiting for {}: stream was replaced by concurrent viewer",
                                stream_key,
                            );
                            // Undo restore_running since we're the stale instance
                            stream.lifecycle().mark_stopping();
                            break;
                        }
                        continue;
                    }

                    info!(
                        "Auto cleanup: Stopping stream {} (idle for {}s)",
                        stream_key, idle_secs
                    );

                    // Run extra cleanup (e.g., Redis unregistration)
                    on_idle_cleanup(stream_key).await;

                    // Remove from map and stop
                    streams.remove(stream_key);
                    // Also remove the creation lock to prevent memory leak
                    creation_locks.remove(stream_key);
                    stream.lifecycle().abort_task().await;
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
        // Cancel all background cleanup tasks
        self.cancel_token.cancel();

        // Abort tasks for all remaining streams via their AbortHandles.
        // This is reliable even under async Mutex contention.
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

    struct TestStream {
        lifecycle: StreamLifecycle,
        key: String,
    }

    impl ManagedStream for TestStream {
        fn lifecycle(&self) -> &StreamLifecycle {
            &self.lifecycle
        }
        fn stream_key(&self) -> String {
            self.key.clone()
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
    async fn test_stream_pool_get_existing_healthy() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            key: "room:media".to_string(),
        });
        stream.lifecycle().set_running();

        pool.streams.insert("room:media".to_string(), stream);

        let found = pool.get_existing("room:media").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().lifecycle().subscriber_count(), 1);
    }

    #[tokio::test]
    async fn test_stream_pool_get_existing_unhealthy_removed() {
        let pool: StreamPool<TestStream> =
            StreamPool::new(Duration::from_mins(1), Duration::from_mins(5));

        let stream = Arc::new(TestStream {
            lifecycle: StreamLifecycle::new(),
            key: "room:media".to_string(),
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
            key: "room:media".to_string(),
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
            key: "room:media".to_string(),
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
}
