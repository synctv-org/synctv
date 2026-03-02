// Stream tracker — multi-index lookup for active RTMP publishers
//
// Provides O(1) lookup by user_id, room_id, (room_id, media_id), and
// RTMP identifiers (app_name, stream_name).
//
// All five indexes are wrapped in a single `parking_lot::RwLock` so that
// insert/remove/cleanup operations are atomic across all maps. This prevents
// concurrent readers from observing partial state and eliminates races
// between cleanup and insert.

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info};

/// RAII guard that decrements a stream's subscriber count on drop.
///
/// Hold this for the lifetime of a viewer connection:
/// - **FLV**: lives in the streaming task — dropped when the viewer disconnects
/// - **HLS**: dropped at the end of each request (transient touch of `last_active_time`)
///
/// The cleanup task in both managers checks `subscriber_count == 0 && idle > 5 min`
/// before tearing down the stream, so this guard is essential for correct lifecycle.
///
/// The callback should use [`StreamLifecycle::decrement_subscriber_count`] which
/// has built-in underflow protection (saturates at zero instead of wrapping).
#[must_use = "dropping the guard immediately would decrement the subscriber count right away"]
pub struct StreamSubscriberGuard(Option<Box<dyn FnOnce() + Send>>);

impl StreamSubscriberGuard {
    /// Create a new guard that runs `on_drop` when dropped.
    ///
    /// The callback should use [`StreamLifecycle::decrement_subscriber_count`]
    /// which has built-in underflow protection.
    pub fn new(on_drop: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(on_drop)))
    }

    /// Disarm the guard without running the callback.
    ///
    /// Use this when the stream has already been cleaned up by another path
    /// (e.g., pool eviction) and decrementing would cause an underflow warning.
    pub fn disarm(&mut self) {
        self.0.take();
    }
}

impl Drop for StreamSubscriberGuard {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// Legacy type alias — prefer `StreamTracker` for new code.
pub type UserStreamTracker = Arc<StreamTracker>;

/// Inner state holding all five indexes, protected by an outer `RwLock`.
///
/// Using regular `HashMap`/`HashSet` instead of `DashMap`/`DashSet` since
/// the outer lock already provides synchronization.
struct StreamTrackerInner {
    /// `user_id` -> Set of `"{room_id}:{media_id}"` composite keys
    by_user: HashMap<String, HashSet<String>>,
    /// `room_id` -> Set<`media_id`>
    by_room: HashMap<String, HashSet<String>>,
    /// `"{room_id}:{media_id}"` -> `user_id`
    by_stream: HashMap<String, String>,
    /// `"{app_name}\0{stream_name}"` -> `"{room_id}:{media_id}"` (RTMP->logical)
    by_rtmp: HashMap<String, String>,
    /// `"{room_id}:{media_id}"` -> `"{app_name}\0{stream_name}"` (logical->RTMP, for cleanup)
    rtmp_reverse: HashMap<String, String>,
}

impl StreamTrackerInner {
    fn new() -> Self {
        Self {
            by_user: HashMap::new(),
            by_room: HashMap::new(),
            by_stream: HashMap::new(),
            by_rtmp: HashMap::new(),
            rtmp_reverse: HashMap::new(),
        }
    }
}

/// Tracks active RTMP publishers with five cross-referenced indexes
/// for fast lookup in any direction:
///
/// 1. `user_id -> Set<(room_id, media_id)>` — kick all streams for a user (supports multiple)
/// 2. `room_id -> Set<media_id>` — kick all streams in a room
/// 3. `(room_id, media_id) -> user_id` — find who is publishing a specific stream
/// 4. `(rtmp_app_name, rtmp_stream_name) -> (room_id, media_id)` — map RTMP identifiers to logical stream
/// 5. `(room_id, media_id) -> (rtmp_app_name, rtmp_stream_name)` — reverse map for cleanup
///
/// The RTMP mapping is needed because `stream_name` in RTMP may be a JWT token,
/// not the `media_id`. On unpublish, we only know `(app_name, stream_name)` and
/// need to resolve the logical `(room_id, media_id)`.
///
/// ## Key Formats
///
/// All internal composite keys use a consistent format:
///
/// - **Stream key** (`by_stream`, `by_user` sets, `rtmp_reverse` keys):
///   `"{room_id}:{media_id}"` — colon-separated, e.g. `"room123:media456"`
///
/// - **RTMP key** (`by_rtmp` keys, `rtmp_reverse` values):
///   `"{app_name}\0{stream_name}"` — null-byte-separated to avoid ambiguity
///   (`app_name` and `stream_name` can contain colons)
///
/// - **Publisher key** (used by `PublisherManager` and Redis):
///   `"{room_id}:{media_id}"` — matches the stream key format above
///
/// All mutations atomically update all indexes under a single write lock.
/// A single user may publish to multiple rooms/media simultaneously.
pub struct StreamTracker {
    inner: RwLock<StreamTrackerInner>,
}

impl Default for StreamTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(StreamTrackerInner::new()),
        }
    }

    /// Build a composite key from `(room_id, media_id)`.
    ///
    /// Format: `"{room_id}:{media_id}"` — used as the canonical key across
    /// `by_stream`, `by_user` sets, `rtmp_reverse` keys, and `PublisherManager`.
    fn stream_key(room_id: &str, media_id: &str) -> String {
        format!("{room_id}:{media_id}")
    }

    /// Parse a composite stream key back into `(room_id, media_id)`.
    ///
    /// Splits on the first `:` — `room_id` and `media_id` must not contain colons.
    fn parse_stream_key(key: &str) -> Option<(String, String)> {
        key.split_once(':')
            .map(|(r, m)| (r.to_string(), m.to_string()))
    }

    /// Build an RTMP composite key from `(app_name, stream_name)`.
    ///
    /// Format: `"{app_name}\0{stream_name}"` — uses null byte separator to
    /// avoid ambiguity (RTMP `app_name/stream_name` can contain colons but
    /// not null bytes).
    fn rtmp_key(app_name: &str, stream_name: &str) -> String {
        format!("{app_name}\0{stream_name}")
    }

    /// Register that `user_id` is publishing `(room_id, media_id)` via RTMP
    /// with the given `(rtmp_app_name, rtmp_stream_name)` identifiers.
    ///
    /// The RTMP mapping is essential because `rtmp_stream_name` is typically
    /// a JWT token, not the logical `media_id`.
    ///
    /// A user may publish to multiple streams simultaneously.
    pub fn insert(
        &self,
        user_id: String,
        room_id: String,
        media_id: String,
        rtmp_app_name: &str,
        rtmp_stream_name: &str,
    ) {
        let sk = Self::stream_key(&room_id, &media_id);
        let rk = Self::rtmp_key(rtmp_app_name, rtmp_stream_name);

        let mut inner = self.inner.write();

        // If another user was publishing this exact stream, remove them first
        if let Some(old_user) = inner.by_stream.remove(&sk) {
            if old_user != user_id {
                if let Some(user_set) = inner.by_user.get_mut(&old_user) {
                    user_set.remove(&sk);
                    if user_set.is_empty() {
                        inner.by_user.remove(&old_user);
                    }
                }
            }
        }

        // Clean up any old RTMP mapping for this stream
        if let Some(old_rk) = inner.rtmp_reverse.remove(&sk) {
            inner.by_rtmp.remove(&old_rk);
        }

        inner
            .by_user
            .entry(user_id.clone())
            .or_default()
            .insert(sk.clone());

        inner.by_room.entry(room_id).or_default().insert(media_id);

        inner.by_stream.insert(sk.clone(), user_id);
        inner.by_rtmp.insert(rk.clone(), sk.clone());
        inner.rtmp_reverse.insert(sk, rk);

        // Track active stream metrics
        synctv_core::metrics::http::STREAMS_ACTIVE.inc();
    }

    /// Remove ALL tracking entries for a user. Returns list of `(room_id, media_id)`.
    #[must_use]
    pub fn remove_user(&self, user_id: &str) -> Vec<(String, String)> {
        let mut removed = Vec::new();
        let mut inner = self.inner.write();

        if let Some(keys) = inner.by_user.remove(user_id) {
            for key in &keys {
                inner.by_stream.remove(key.as_str());
                // Clean up RTMP mapping
                if let Some(rk) = inner.rtmp_reverse.remove(key.as_str()) {
                    inner.by_rtmp.remove(&rk);
                }
                if let Some((room_id, media_id)) = Self::parse_stream_key(key) {
                    if let Some(set) = inner.by_room.get_mut(&room_id) {
                        set.remove(&media_id);
                        if set.is_empty() {
                            inner.by_room.remove(&room_id);
                        }
                    }
                    removed.push((room_id, media_id));
                }
            }
            // Decrement stream count for all removed streams
            let count = removed.len();
            if count > 0 {
                synctv_core::metrics::http::STREAMS_ACTIVE.sub(count as i64);
            }
        }
        removed
    }

    /// Remove tracking by (`room_id`, `media_id`). Returns the `user_id` if present.
    #[must_use]
    pub fn remove_stream(&self, room_id: &str, media_id: &str) -> Option<String> {
        let mut inner = self.inner.write();
        Self::remove_stream_locked(&mut inner, room_id, media_id, true)
    }

    /// Internal: remove stream with write lock already held.
    /// If `clean_rtmp` is true, also removes the RTMP mappings.
    fn remove_stream_locked(
        inner: &mut StreamTrackerInner,
        room_id: &str,
        media_id: &str,
        clean_rtmp: bool,
    ) -> Option<String> {
        let sk = Self::stream_key(room_id, media_id);
        if let Some(user_id) = inner.by_stream.remove(&sk) {
            if clean_rtmp {
                // Clean up RTMP mapping
                if let Some(rk) = inner.rtmp_reverse.remove(&sk) {
                    inner.by_rtmp.remove(&rk);
                }
            }
            if let Some(user_set) = inner.by_user.get_mut(&user_id) {
                user_set.remove(&sk);
                if user_set.is_empty() {
                    inner.by_user.remove(&user_id);
                }
            }
            if let Some(set) = inner.by_room.get_mut(room_id) {
                set.remove(media_id);
                if set.is_empty() {
                    inner.by_room.remove(room_id);
                }
            }
            synctv_core::metrics::http::STREAMS_ACTIVE.dec();
            Some(user_id)
        } else {
            None
        }
    }

    /// Remove by RTMP identifiers (`app_name`, `stream_name`) — used by `on_unpublish`.
    ///
    /// Uses the RTMP->logical mapping to resolve `(room_id, media_id)` from the
    /// RTMP identifiers, then removes all tracking entries.
    ///
    /// Returns `Some((user_id, room_id, media_id))` if found, `None` otherwise.
    pub fn remove_by_app_stream(
        &self,
        app_name: &str,
        stream_name: &str,
    ) -> Option<(String, String, String)> {
        let rk = Self::rtmp_key(app_name, stream_name);

        let mut inner = self.inner.write();

        // Look up logical stream from RTMP mapping
        if let Some(sk) = inner.by_rtmp.remove(&rk) {
            inner.rtmp_reverse.remove(&sk);
            if let Some((room_id, media_id)) = Self::parse_stream_key(&sk) {
                // Use clean_rtmp=false since we already removed the RTMP mappings above
                if let Some(user_id) =
                    Self::remove_stream_locked(&mut inner, &room_id, &media_id, false)
                {
                    debug!(
                        user_id = %user_id,
                        room_id = %room_id,
                        media_id = %media_id,
                        rtmp_app = %app_name,
                        "Removed publisher from tracker on unpublish (RTMP mapping)"
                    );
                    return Some((user_id, room_id, media_id));
                }
            }
        }

        // Fallback: try direct stream key match (app_name = room_id, stream_name = media_id)
        if let Some(user_id) = Self::remove_stream_locked(&mut inner, app_name, stream_name, true) {
            debug!(
                user_id = %user_id,
                room_id = %app_name,
                media_id = %stream_name,
                "Removed publisher from tracker on unpublish (direct match)"
            );
            return Some((user_id, app_name.to_string(), stream_name.to_string()));
        }

        None
    }

    /// Get all (`room_id`, `media_id`) pairs for a user.
    #[must_use]
    pub fn get_user_streams(&self, user_id: &str) -> Vec<(String, String)> {
        let inner = self.inner.read();
        inner
            .by_user
            .get(user_id)
            .map(|set| {
                set.iter()
                    .filter_map(|key| Self::parse_stream_key(key))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all `media_ids` currently publishing in a room.
    #[must_use]
    pub fn get_room_streams(&self, room_id: &str) -> Vec<String> {
        let inner = self.inner.read();
        inner
            .by_room
            .get(room_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get `user_id` publishing a specific (`room_id`, `media_id`).
    #[must_use]
    pub fn get_stream_user(&self, room_id: &str, media_id: &str) -> Option<String> {
        let inner = self.inner.read();
        inner
            .by_stream
            .get(&Self::stream_key(room_id, media_id))
            .cloned()
    }

    /// Get RTMP identifiers (`app_name`, `stream_name`) for a logical (`room_id`, `media_id`).
    ///
    /// This is needed because `StreamHub` uses the original RTMP identifiers,
    /// not the logical (`room_id`, `media_id`) pair.
    #[must_use]
    pub fn get_rtmp_identifiers(&self, room_id: &str, media_id: &str) -> Option<(String, String)> {
        let inner = self.inner.read();
        let sk = Self::stream_key(room_id, media_id);
        inner.rtmp_reverse.get(&sk).and_then(|rk| {
            // rtmp_key format is "{app_name}\0{stream_name}"
            rk.split_once('\0')
                .map(|(app, stream)| (app.to_string(), stream.to_string()))
        })
    }

    /// Iterate over all stream entries. Returns owned `Vec` of `(stream_key, user_id)`.
    #[must_use]
    pub fn iter_streams(&self) -> Vec<(String, String)> {
        let inner = self.inner.read();
        inner
            .by_stream
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Number of tracked streams.
    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.read();
        inner.by_stream.len()
    }

    /// Whether the tracker is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let inner = self.inner.read();
        inner.by_stream.is_empty()
    }

    /// Clear all tracking entries.
    ///
    /// Called during `StreamHub` restart cleanup to ensure stale entries
    /// don't persist after Redis publishers are cleaned up.
    /// Without this, the tracker retains entries for publishers that
    /// no longer exist in Redis, causing incorrect stream lookups.
    pub fn clear(&self) {
        let mut inner = self.inner.write();

        // Calculate how many streams we're removing for metrics
        let stream_count = inner.by_stream.len();

        inner.by_user.clear();
        inner.by_room.clear();
        inner.by_stream.clear();
        inner.by_rtmp.clear();
        inner.rtmp_reverse.clear();

        // Decrement stream count for all removed streams
        if stream_count > 0 {
            synctv_core::metrics::http::STREAMS_ACTIVE.sub(stream_count as i64);
            info!(removed = stream_count, "Cleared all stream tracker entries");
        }
    }

    /// Remove stale index entries that are orphaned from the primary `by_stream` map.
    ///
    /// When a publisher crashes without a clean `on_unpublish`, secondary indexes
    /// (`by_user`, `by_room`, `by_rtmp`, `rtmp_reverse`) can retain references to
    /// streams that no longer exist in `by_stream`. This method scans each index
    /// and removes entries whose stream key is no longer present.
    ///
    /// Holds the write lock for the entire operation, preventing concurrent
    /// inserts from racing with the cleanup scan.
    ///
    /// Returns the number of stale entries removed.
    pub fn cleanup_stale_entries(&self) -> usize {
        let mut inner = self.inner.write();
        let mut removed = 0usize;

        // Collect the set of valid stream keys for cross-referencing.
        // This avoids borrow-checker issues from borrowing inner.by_stream
        // while mutating other fields in the same struct.
        let valid_streams: HashSet<String> = inner.by_stream.keys().cloned().collect();

        // Clean by_user: remove stream keys that are not in by_stream
        let user_keys: Vec<String> = inner.by_user.keys().cloned().collect();
        for user_id in user_keys {
            if let Some(user_set) = inner.by_user.get_mut(&user_id) {
                let before = user_set.len();
                user_set.retain(|sk| valid_streams.contains(sk));
                removed += before - user_set.len();
                if user_set.is_empty() {
                    inner.by_user.remove(&user_id);
                }
            }
        }

        // Clean by_room: remove media_ids whose stream key is not in by_stream
        let room_keys: Vec<String> = inner.by_room.keys().cloned().collect();
        for room_id in room_keys {
            if let Some(room_set) = inner.by_room.get_mut(&room_id) {
                let before = room_set.len();
                room_set.retain(|media_id| {
                    valid_streams.contains(&Self::stream_key(&room_id, media_id))
                });
                removed += before - room_set.len();
                if room_set.is_empty() {
                    inner.by_room.remove(&room_id);
                }
            }
        }

        // Clean by_rtmp: remove entries whose stream key is not in by_stream
        let before = inner.by_rtmp.len();
        inner.by_rtmp.retain(|_rk, sk| valid_streams.contains(sk));
        removed += before - inner.by_rtmp.len();

        // Clean rtmp_reverse: remove entries whose stream key is not in by_stream
        let before = inner.rtmp_reverse.len();
        inner
            .rtmp_reverse
            .retain(|sk, _rk| valid_streams.contains(sk));
        removed += before - inner.rtmp_reverse.len();

        if removed > 0 {
            info!(removed, "Cleaned up stale stream tracker entries");
        }

        removed
    }

    /// Spawn a periodic background task that calls `cleanup_stale_entries`
    /// every `interval` duration. Returns the `JoinHandle` for the task.
    ///
    /// The task shuts down gracefully when `cancel` is cancelled, matching
    /// the pattern used by other background tasks in the codebase (HLS cleanup,
    /// TTL refresh, data cleanup, etc.).
    #[must_use]
    pub fn start_periodic_cleanup(
        self: &Arc<Self>,
        interval: std::time::Duration,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let tracker = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        tracker.cleanup_stale_entries();
                    }
                    () = cancel.cancelled() => {
                        info!("Stream tracker periodic cleanup cancelled, shutting down");
                        return;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clear_removes_all_entries() {
        let tracker = StreamTracker::new();

        // Insert some entries
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
            "room1",
            "token1",
        );
        tracker.insert(
            "user2".to_string(),
            "room2".to_string(),
            "media2".to_string(),
            "room2",
            "token2",
        );

        // Verify entries exist
        assert!(!tracker.is_empty());
        assert_eq!(tracker.len(), 2);

        // Clear the tracker
        tracker.clear();

        // Verify all entries are removed
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);
        assert!(tracker.get_user_streams("user1").is_empty());
        assert!(tracker.get_user_streams("user2").is_empty());
        assert!(tracker.get_room_streams("room1").is_empty());
        assert!(tracker.get_room_streams("room2").is_empty());
    }

    #[test]
    fn test_clear_on_empty_tracker() {
        let tracker = StreamTracker::new();

        // Clear on empty tracker should not panic
        tracker.clear();
        assert!(tracker.is_empty());
    }

    #[test]
    fn test_clear_allows_new_entries() {
        let tracker = StreamTracker::new();

        // Insert and clear
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
            "room1",
            "token1",
        );
        tracker.clear();

        // Insert new entries after clear
        tracker.insert(
            "user2".to_string(),
            "room2".to_string(),
            "media2".to_string(),
            "room2",
            "token2",
        );

        // Verify new entry exists
        assert_eq!(tracker.len(), 1);
        let user_streams = tracker.get_user_streams("user2");
        assert_eq!(user_streams.len(), 1);
        assert_eq!(user_streams[0], ("room2".to_string(), "media2".to_string()));
    }

    /// Test that clear properly handles `StreamHub` restart scenario
    #[test]
    fn test_clear_streamhub_restart_scenario() {
        let tracker = Arc::new(StreamTracker::new());

        // Simulate multiple publishers from different users/rooms
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
            "room1",
            "jwt_token_1",
        );
        tracker.insert(
            "user2".to_string(),
            "room1".to_string(),
            "media2".to_string(),
            "room1",
            "jwt_token_2",
        );
        tracker.insert(
            "user3".to_string(),
            "room2".to_string(),
            "media3".to_string(),
            "room2",
            "jwt_token_3",
        );

        assert_eq!(tracker.len(), 3);

        // Simulate StreamHub restart: clear tracker
        tracker.clear();

        // All entries should be gone
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);

        // Verify all indexes are cleared
        assert!(tracker.get_user_streams("user1").is_empty());
        assert!(tracker.get_user_streams("user2").is_empty());
        assert!(tracker.get_user_streams("user3").is_empty());
        assert!(tracker.get_room_streams("room1").is_empty());
        assert!(tracker.get_room_streams("room2").is_empty());
        assert!(tracker.get_stream_user("room1", "media1").is_none());
        assert!(tracker.get_stream_user("room1", "media2").is_none());
        assert!(tracker.get_stream_user("room2", "media3").is_none());
        assert!(tracker
            .get_rtmp_identifiers("room1", "jwt_token_1")
            .is_none());
    }

    #[tokio::test]
    async fn test_periodic_cleanup_cancellation() {
        // L12: Verify that start_periodic_cleanup shuts down when CancellationToken is cancelled
        let tracker = Arc::new(StreamTracker::new());
        let cancel = tokio_util::sync::CancellationToken::new();

        let handle = tracker.start_periodic_cleanup(
            std::time::Duration::from_secs(3600), // Long interval so it blocks on tick
            cancel.clone(),
        );

        // Cancel the token
        cancel.cancel();

        // The task should complete promptly (within a few ms)
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Periodic cleanup task should exit promptly after cancellation"
        );
    }
}
