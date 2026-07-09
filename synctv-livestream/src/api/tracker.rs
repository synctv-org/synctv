// Stream tracker — multi-index lookup for active RTMP publishers
// Provides O(1) lookup by user_id, room_id, and (room_id, media_id).
// All indexes are wrapped in a single `parking_lot::RwLock` so that
// insert/remove/clear operations are atomic across all maps. This prevents
// concurrent readers from observing partial state and eliminates races
// between lifecycle operations.

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info};

/// Logical stream identity used by the application layer.
///
/// Storage currently encodes this as `"{room_id}:{media_id}"`; callers should
/// use this type instead of hand-building that string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StreamKey {
    room_id: String,
    media_id: String,
}

impl StreamKey {
    fn new(room_id: impl Into<String>, media_id: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            media_id: media_id.into(),
        }
    }

    fn encode(&self) -> String {
        format!("{}:{}", self.room_id, self.media_id)
    }

    fn decode(value: &str) -> Option<Self> {
        value.split_once(':').map(|(room_id, media_id)| Self {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
        })
    }

    fn into_pair(self) -> (String, String) {
        (self.room_id, self.media_id)
    }
}

/// RAII guard that decrements a stream's subscriber count on drop.
///
/// Hold this for the lifetime of a viewer connection:
/// - **FLV**: lives in the streaming task — dropped when the viewer disconnects
/// - **HLS**: dropped at the end of each request (transient touch of `last_active_time`)
///
/// The cleanup task in both managers checks `subscriber_count == 0 && idle > 5 min`
/// before tearing down the stream, so this guard is essential for correct lifecycle.
/// Provider playback that returns HLS or FLV URLs must route viewers through
/// code paths that create this guard or touch the matching lifecycle state.
///
/// The callback should use [`StreamLifecycle::decrement_subscriber_count`] which
/// has built-in underflow protection (saturates at zero instead of wrapping).
#[must_use = "dropping the guard immediately would decrement the subscriber count right away"]
pub struct StreamSubscriberGuard(Option<Box<dyn FnOnce() + Send>>);

impl std::fmt::Debug for StreamSubscriberGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSubscriberGuard")
            .field("armed", &self.0.is_some())
            .finish()
    }
}

impl StreamSubscriberGuard {
    /// Create a new guard that runs `on_drop` when dropped.
    ///
    /// The callback should use [`StreamLifecycle::decrement_subscriber_count`]
    /// which has built-in underflow protection.
    pub(crate) fn new(on_drop: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(on_drop)))
    }
}

impl Drop for StreamSubscriberGuard {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// Inner state holding all indexes, protected by an outer `RwLock`.
///
/// Using regular `HashMap`/`HashSet` instead of `DashMap`/`DashSet` since
/// the outer lock already provides synchronization.
struct StreamTrackerInner {
    /// `user_id` -> Set of encoded [`StreamKey`] values
    by_user: HashMap<String, HashSet<String>>,
    /// `room_id` -> Set<`media_id`>
    by_room: HashMap<String, HashSet<String>>,
    /// encoded [`StreamKey`] -> `user_id`
    by_stream: HashMap<String, String>,
}

impl StreamTrackerInner {
    fn new() -> Self {
        Self {
            by_user: HashMap::new(),
            by_room: HashMap::new(),
            by_stream: HashMap::new(),
        }
    }
}

/// Tracks active RTMP publishers with cross-referenced indexes
/// for fast lookup in any direction:
///
/// 1. `user_id -> Set<(room_id, media_id)>` — kick all streams for a user (supports multiple)
/// 2. `room_id -> Set<media_id>` — kick all streams in a room
/// 3. `(room_id, media_id) -> user_id` — find who is publishing a specific stream
///
/// ## Key Formats
///
/// All internal composite keys use a consistent format:
///
/// - **Stream key** (`by_stream`, `by_user` sets):
///   `"{room_id}:{media_id}"` — colon-separated, e.g. `"room123:media456"`
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

    fn stream_key(room_id: &str, media_id: &str) -> String {
        StreamKey::new(room_id, media_id).encode()
    }

    fn parse_stream_key(key: &str) -> Option<(String, String)> {
        StreamKey::decode(key).map(StreamKey::into_pair)
    }

    /// Register that `user_id` is publishing `(room_id, media_id)` via RTMP
    /// A user may publish to multiple streams simultaneously.
    pub fn insert(&self, user_id: String, room_id: String, media_id: String) {
        let sk = Self::stream_key(&room_id, &media_id);

        let mut inner = self.inner.write();

        // If another user was publishing this exact stream, remove them first.
        let previous_user = inner.by_stream.remove(&sk);
        let is_new_stream = previous_user.is_none();
        if let Some(old_user) = previous_user {
            if old_user != user_id {
                if let Some(user_set) = inner.by_user.get_mut(&old_user) {
                    user_set.remove(&sk);
                    if user_set.is_empty() {
                        inner.by_user.remove(&old_user);
                    }
                }
            }
        }

        inner
            .by_user
            .entry(user_id.clone())
            .or_default()
            .insert(sk.clone());

        inner.by_room.entry(room_id).or_default().insert(media_id);

        inner.by_stream.insert(sk.clone(), user_id);

        if is_new_stream {
            synctv_core::metrics::application::STREAMS_ACTIVE.inc();
        }
    }

    /// Remove tracking by (`room_id`, `media_id`). Returns the `user_id` if present.
    #[must_use]
    pub fn remove_stream(&self, room_id: &str, media_id: &str) -> Option<String> {
        let mut inner = self.inner.write();
        Self::remove_stream_locked(&mut inner, room_id, media_id)
    }

    /// Internal: remove stream with write lock already held.
    fn remove_stream_locked(
        inner: &mut StreamTrackerInner,
        room_id: &str,
        media_id: &str,
    ) -> Option<String> {
        let sk = Self::stream_key(room_id, media_id);
        if let Some(user_id) = inner.by_stream.remove(&sk) {
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
            synctv_core::metrics::application::STREAMS_ACTIVE.dec();
            Some(user_id)
        } else {
            None
        }
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

    /// Clear all tracking entries.
    ///
    /// Called during `StreamHub` restart cleanup to ensure stale entries
    /// don't persist after Redis publishers are cleaned up.
    /// Without this, the tracker retains entries for publishers that
    /// no longer exist in Redis, causing incorrect stream lookups.
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.write();

        // Calculate how many streams we're removing for metrics
        let stream_count = inner.by_stream.len();

        inner.by_user.clear();
        inner.by_room.clear();
        inner.by_stream.clear();

        // Decrement stream count for all removed streams
        if let Ok(metric_count) = i64::try_from(stream_count) {
            if metric_count > 0 {
                synctv_core::metrics::application::STREAMS_ACTIVE.sub(metric_count);
            }
            info!(removed = stream_count, "Cleared all stream tracker entries");
        } else {
            debug!(
                stream_count,
                "Skipped metric decrement because stream count exceeded i64"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_stream_key_roundtrip() {
        let stream_key = StreamKey::new("room1", "media1").encode();
        assert_eq!(
            StreamKey::decode(&stream_key).map(StreamKey::into_pair),
            Some(("room1".to_string(), "media1".to_string()))
        );
    }

    #[test]
    fn test_clear_removes_all_entries() {
        let tracker = StreamTracker::new();

        // Insert some entries
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );
        tracker.insert(
            "user2".to_string(),
            "room2".to_string(),
            "media2".to_string(),
        );

        // Verify entries exist
        assert_eq!(tracker.get_room_streams("room1"), vec!["media1"]);
        assert_eq!(tracker.get_room_streams("room2"), vec!["media2"]);

        // Clear the tracker
        tracker.clear();

        // Verify all entries are removed
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
        assert!(tracker.get_user_streams("user1").is_empty());
    }

    #[test]
    fn test_clear_allows_new_entries() {
        let tracker = StreamTracker::new();

        // Insert and clear
        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );
        tracker.clear();

        // Insert new entries after clear
        tracker.insert(
            "user2".to_string(),
            "room2".to_string(),
            "media2".to_string(),
        );

        // Verify new entry exists
        let user_streams = tracker.get_user_streams("user2");
        assert_eq!(user_streams.len(), 1);
        assert_eq!(user_streams[0], ("room2".to_string(), "media2".to_string()));
    }

    #[test]
    fn test_insert_replaces_existing_stream_owner() {
        let tracker = StreamTracker::new();

        tracker.insert(
            "user1".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );
        tracker.insert(
            "user2".to_string(),
            "room1".to_string(),
            "media1".to_string(),
        );

        assert!(tracker.get_user_streams("user1").is_empty());
        assert_eq!(
            tracker.get_user_streams("user2"),
            vec![("room1".to_string(), "media1".to_string())]
        );
        assert_eq!(tracker.get_room_streams("room1"), vec!["media1"]);
        assert_eq!(
            tracker.get_stream_user("room1", "media1"),
            Some("user2".to_string())
        );
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
        );
        tracker.insert(
            "user2".to_string(),
            "room1".to_string(),
            "media2".to_string(),
        );
        tracker.insert(
            "user3".to_string(),
            "room2".to_string(),
            "media3".to_string(),
        );

        assert_eq!(tracker.get_room_streams("room1").len(), 2);
        assert_eq!(tracker.get_room_streams("room2").len(), 1);

        // Simulate StreamHub restart: clear tracker
        tracker.clear();

        // All entries should be gone
        // Verify all indexes are cleared
        assert!(tracker.get_user_streams("user1").is_empty());
        assert!(tracker.get_user_streams("user2").is_empty());
        assert!(tracker.get_user_streams("user3").is_empty());
        assert!(tracker.get_room_streams("room1").is_empty());
        assert!(tracker.get_room_streams("room2").is_empty());
        assert!(tracker.get_stream_user("room1", "media1").is_none());
        assert!(tracker.get_stream_user("room1", "media2").is_none());
        assert!(tracker.get_stream_user("room2", "media3").is_none());
    }
}
