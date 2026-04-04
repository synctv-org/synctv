//! Message deduplication for cross-cluster synchronization
//!
//! Prevents duplicate processing of events when:
//! - Multiple Redis subscribers exist
//! - Network issues cause retries
//! - Events are published multiple times
//!
//! Uses `moka::sync::Cache` with TTL-based expiration, eliminating the need
//! for a manual cleanup task.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Deduplication key for events
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct DedupKey {
    pub event_type: String,
    pub room_id: String,
    pub user_id: String,
    /// Extra discriminator for events without `room_id/user_id` (e.g. `SystemNotification` message)
    pub extra: String,
    pub timestamp_ms: i64,
    /// Content hash to prevent false positives on same-millisecond events
    /// with different payloads (e.g. two chat messages in the same ms)
    pub content_hash: u64,
}

impl DedupKey {
    /// Create a deduplication key from a cluster event.
    #[must_use]
    pub fn from_event(event: &crate::sync::events::ClusterEvent) -> Self {
        let eid = event.event_id();
        assert!(
            !eid.is_empty(),
            "cluster events must carry a non-empty event_id"
        );
        Self {
            event_type: event.event_type().to_string(),
            room_id: event
                .room_id()
                .map_or_else(|| "global".to_string(), |id| id.as_str().to_string()),
            user_id: String::new(),
            extra: eid.to_string(),
            timestamp_ms: event.timestamp().timestamp_millis(),
            content_hash: 0,
        }
    }
}

/// Default dedup TTL: 15 minutes.
///
/// The dedup TTL must account for:
/// 1. Catchup window: 5 minutes (300s) - maximum disconnection duration
/// 2. Retry buffers: up to 5+ minutes - worst-case retry buffer duration
/// 3. Safety margin: 5 minutes - buffer for overlapping scenarios
///
/// Total: 15 minutes (900s)
///
/// During a disconnection, events accumulate in Redis Streams and are replayed
/// on reconnect. If the dedup TTL is shorter than the disconnect window plus
/// retry buffer duration, events delivered via live Pub/Sub before the disconnect
/// may have already been evicted from the dedup cache, causing them to be
/// re-processed when replayed from the stream.
pub const DEFAULT_DEDUP_TTL: Duration = Duration::from_mins(15);

/// Message deduplicator using moka TTL cache.
///
/// Entries are automatically evicted after `dedup_window` via moka's built-in
/// TTL support, eliminating the need for a manual cleanup task.
#[derive(Clone)]
pub struct MessageDeduplicator {
    /// Cache of dedup keys with TTL-based expiration
    cache: moka::sync::Cache<DedupKey, ()>,
}

impl MessageDeduplicator {
    /// Create a new deduplicator
    ///
    /// # Arguments
    /// * `dedup_window` - How long to remember events for deduplication. Should
    ///   be at least 2x the maximum expected disconnection window (see
    ///   [`DEFAULT_DEDUP_TTL`] for rationale).
    #[must_use]
    pub fn new(dedup_window: Duration) -> Self {
        let cache = moka::sync::Cache::builder()
            .time_to_live(dedup_window)
            .build();
        Self { cache }
    }

    /// Create with default settings (15 minute window).
    ///
    /// The 15-minute window ensures dedup entries survive reconnection scenarios
    /// where the catchup window is 5 minutes plus retry buffers up to 5+ minutes.
    /// See [`DEFAULT_DEDUP_TTL`] for the full rationale.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_DEDUP_TTL)
    }

    /// Get the number of tracked events
    #[must_use]
    pub fn len(&self) -> usize {
        // Run pending tasks to get accurate count
        self.cache.run_pending_tasks();
        self.cache.entry_count() as usize
    }

    /// Check if there are any tracked events
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if an event should be processed (not a duplicate)
    ///
    /// Returns `true` if this is a new event, `false` if it's a duplicate
    /// within the dedup window.
    ///
    /// Uses moka's atomic `get_with()` to avoid the TOCTOU race between
    /// `get()` and `insert()`. Under concurrent dispatch (e.g., live Pub/Sub
    /// event + catch-up Stream event arriving simultaneously), only one caller
    /// will see `is_new = true`; all others get `false`.
    #[must_use]
    pub fn should_process(&self, key: &DedupKey) -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        let is_new = AtomicBool::new(false);
        self.cache.get_with(key.clone(), || {
            is_new.store(true, Ordering::Relaxed);
        });
        is_new.load(Ordering::Relaxed)
    }

    /// Mark an event as processed
    pub fn mark_processed(&self, key: DedupKey) {
        self.cache.insert(key, ());
    }

    /// Clear all tracked events (for testing)
    pub fn clear(&self) {
        self.cache.invalidate_all();
        self.cache.run_pending_tasks();
    }
}

impl Default for MessageDeduplicator {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use synctv_core::models::id::{RoomId, UserId};

    #[tokio::test]
    async fn test_dedup_basic() {
        let dedup = MessageDeduplicator::with_defaults();

        let key = DedupKey {
            event_type: "chat".to_string(),
            room_id: "room1".to_string(),
            user_id: "user1".to_string(),
            extra: String::new(),
            timestamp_ms: 1000,
            content_hash: 0,
        };

        // First call should return true
        assert!(dedup.should_process(&key));

        // Immediate second call should return false (duplicate)
        assert!(!dedup.should_process(&key));

        // Wait for expiration (simulated by clearing)
        dedup.clear();

        // After expiration, should process again
        assert!(dedup.should_process(&key));
    }

    #[tokio::test]
    async fn test_dedup_concurrent_should_process() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dedup = Arc::new(MessageDeduplicator::with_defaults());
        let key = DedupKey {
            event_type: "chat".to_string(),
            room_id: "room1".to_string(),
            user_id: "user1".to_string(),
            extra: String::new(),
            timestamp_ms: 1000,
            content_hash: 0,
        };

        let success_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(10));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let dedup = dedup.clone();
            let key = key.clone();
            let count = success_count.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                if dedup.should_process(&key) {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        // Exactly one thread should have processed the event
        assert_eq!(
            success_count.load(Ordering::Relaxed),
            1,
            "Expected exactly 1 successful should_process, got {}",
            success_count.load(Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn test_dedup_from_event() {
        let dedup = MessageDeduplicator::with_defaults();

        let event = crate::sync::events::ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "test".to_string(),
            message: "Hello".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let key = DedupKey::from_event(&event);

        assert!(dedup.should_process(&key));
        assert!(!dedup.should_process(&key));
    }

    #[test]
    #[should_panic(expected = "cluster events must carry a non-empty event_id")]
    fn test_dedup_from_event_rejects_empty_event_id() {
        let event = crate::sync::events::ClusterEvent::ChatMessage {
            event_id: String::new(),
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "test".to_string(),
            message: "Hello".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let _ = DedupKey::from_event(&event);
    }

    #[test]
    fn test_dedup_ttl_is_fifteen_minutes() {
        // The dedup TTL must be at least 3x the catchup window to account for:
        // 1. Catchup window: 5 minutes (300s)
        // 2. Retry buffers: up to 5+ minutes
        // 3. Safety margin: 5 minutes
        // Total: 15 minutes (900s)
        assert_eq!(DEFAULT_DEDUP_TTL, Duration::from_mins(15));
    }
}
