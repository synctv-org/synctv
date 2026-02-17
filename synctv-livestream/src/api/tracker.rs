// Stream tracker — multi-index lookup for active RTMP publishers
//
// Provides O(1) lookup by user_id, room_id, (room_id, media_id), and
// RTMP identifiers (app_name, stream_name).

use dashmap::DashMap;
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

/// Tracks active RTMP publishers with five cross-referenced indexes
/// for fast lookup in any direction:
///
/// 1. `user_id → Set<(room_id, media_id)>` — kick all streams for a user (supports multiple)
/// 2. `room_id → Set<media_id>` — kick all streams in a room
/// 3. `(room_id, media_id) → user_id` — find who is publishing a specific stream
/// 4. `(rtmp_app_name, rtmp_stream_name) → (room_id, media_id)` — map RTMP identifiers to logical stream
/// 5. `(room_id, media_id) → (rtmp_app_name, rtmp_stream_name)` — reverse map for cleanup
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
///   (app_name and stream_name can contain colons)
///
/// - **Publisher key** (used by `PublisherManager` and Redis):
///   `"{room_id}:{media_id}"` — matches the stream key format above
///
/// All mutations atomically update all indexes.
/// A single user may publish to multiple rooms/media simultaneously.
pub struct StreamTracker {
    /// `user_id` → Set of `"{room_id}:{media_id}"` composite keys
    by_user: DashMap<String, dashmap::DashSet<String>>,
    /// `room_id` → Set<`media_id`>
    by_room: DashMap<String, dashmap::DashSet<String>>,
    /// `"{room_id}:{media_id}"` → `user_id`
    by_stream: DashMap<String, String>,
    /// `"{app_name}\0{stream_name}"` → `"{room_id}:{media_id}"` (RTMP→logical)
    by_rtmp: DashMap<String, String>,
    /// `"{room_id}:{media_id}"` → `"{app_name}\0{stream_name}"` (logical→RTMP, for cleanup)
    rtmp_reverse: DashMap<String, String>,
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
            by_user: DashMap::new(),
            by_room: DashMap::new(),
            by_stream: DashMap::new(),
            by_rtmp: DashMap::new(),
            rtmp_reverse: DashMap::new(),
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
    /// Splits on the first `:` — room_id and media_id must not contain colons.
    fn parse_stream_key(key: &str) -> Option<(String, String)> {
        key.split_once(':').map(|(r, m)| (r.to_string(), m.to_string()))
    }

    /// Build an RTMP composite key from `(app_name, stream_name)`.
    ///
    /// Format: `"{app_name}\0{stream_name}"` — uses null byte separator to
    /// avoid ambiguity (RTMP app_name/stream_name can contain colons but
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

        // If another user was publishing this exact stream, remove them first
        if let Some((_, old_user)) = self.by_stream.remove(&sk) {
            if old_user != user_id {
                if let Some(user_set) = self.by_user.get(&old_user) {
                    user_set.remove(&sk);
                    if user_set.is_empty() {
                        drop(user_set);
                        self.by_user.remove(&old_user);
                    }
                }
            }
        }

        // Clean up any old RTMP mapping for this stream
        if let Some((_, old_rk)) = self.rtmp_reverse.remove(&sk) {
            self.by_rtmp.remove(&old_rk);
        }

        self.by_user
            .entry(user_id.clone())
            .or_default()
            .insert(sk.clone());

        self.by_room
            .entry(room_id)
            .or_default()
            .insert(media_id);

        self.by_stream.insert(sk.clone(), user_id);
        self.by_rtmp.insert(rk.clone(), sk.clone());
        self.rtmp_reverse.insert(sk, rk);

        // Track active stream metrics
        synctv_core::metrics::http::STREAMS_ACTIVE.inc();
    }

    /// Remove ALL tracking entries for a user. Returns list of `(room_id, media_id)`.
    #[must_use]
    pub fn remove_user(&self, user_id: &str) -> Vec<(String, String)> {
        let mut removed = Vec::new();
        if let Some((_, keys)) = self.by_user.remove(user_id) {
            for key in keys.iter() {
                self.by_stream.remove(key.as_str());
                // Clean up RTMP mapping
                if let Some((_, rk)) = self.rtmp_reverse.remove(key.as_str()) {
                    self.by_rtmp.remove(&rk);
                }
                if let Some((room_id, media_id)) = Self::parse_stream_key(&key) {
                    if let Some(set) = self.by_room.get(&room_id) {
                        set.remove(&media_id);
                        if set.is_empty() {
                            drop(set);
                            self.by_room.remove(&room_id);
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
        let sk = Self::stream_key(room_id, media_id);
        if let Some((_, user_id)) = self.by_stream.remove(&sk) {
            // Clean up RTMP mapping
            if let Some((_, rk)) = self.rtmp_reverse.remove(&sk) {
                self.by_rtmp.remove(&rk);
            }
            if let Some(user_set) = self.by_user.get(&user_id) {
                user_set.remove(&sk);
                if user_set.is_empty() {
                    drop(user_set);
                    self.by_user.remove(&user_id);
                }
            }
            if let Some(set) = self.by_room.get(room_id) {
                set.remove(media_id);
                if set.is_empty() {
                    drop(set);
                    self.by_room.remove(room_id);
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
    /// Uses the RTMP→logical mapping to resolve `(room_id, media_id)` from the
    /// RTMP identifiers, then removes all tracking entries.
    ///
    /// Returns `Some((user_id, room_id, media_id))` if found, `None` otherwise.
    pub fn remove_by_app_stream(&self, app_name: &str, stream_name: &str) -> Option<(String, String, String)> {
        let rk = Self::rtmp_key(app_name, stream_name);

        // Look up logical stream from RTMP mapping
        if let Some((_, sk)) = self.by_rtmp.remove(&rk) {
            self.rtmp_reverse.remove(&sk);
            if let Some((room_id, media_id)) = Self::parse_stream_key(&sk) {
                if let Some(user_id) = self.remove_stream_internal(&room_id, &media_id) {
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
        if let Some(user_id) = self.remove_stream(app_name, stream_name) {
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

    /// Internal: remove stream without touching RTMP maps (already cleaned by caller).
    fn remove_stream_internal(&self, room_id: &str, media_id: &str) -> Option<String> {
        let sk = Self::stream_key(room_id, media_id);
        if let Some((_, user_id)) = self.by_stream.remove(&sk) {
            if let Some(user_set) = self.by_user.get(&user_id) {
                user_set.remove(&sk);
                if user_set.is_empty() {
                    drop(user_set);
                    self.by_user.remove(&user_id);
                }
            }
            if let Some(set) = self.by_room.get(room_id) {
                set.remove(media_id);
                if set.is_empty() {
                    drop(set);
                    self.by_room.remove(room_id);
                }
            }
            synctv_core::metrics::http::STREAMS_ACTIVE.dec();
            Some(user_id)
        } else {
            None
        }
    }

    /// Get all (`room_id`, `media_id`) pairs for a user.
    #[must_use]
    pub fn get_user_streams(&self, user_id: &str) -> Vec<(String, String)> {
        self.by_user
            .get(user_id)
            .map(|set| {
                set.iter()
                    .filter_map(|key| Self::parse_stream_key(&key))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all `media_ids` currently publishing in a room.
    #[must_use]
    pub fn get_room_streams(&self, room_id: &str) -> Vec<String> {
        self.by_room
            .get(room_id)
            .map(|set| set.iter().map(|e| e.clone()).collect())
            .unwrap_or_default()
    }

    /// Get `user_id` publishing a specific (`room_id`, `media_id`).
    #[must_use]
    pub fn get_stream_user(&self, room_id: &str, media_id: &str) -> Option<String> {
        self.by_stream.get(&Self::stream_key(room_id, media_id)).map(|e| e.value().clone())
    }

    /// Get RTMP identifiers (`app_name`, `stream_name`) for a logical (`room_id`, `media_id`).
    ///
    /// This is needed because `StreamHub` uses the original RTMP identifiers,
    /// not the logical (`room_id`, `media_id`) pair.
    #[must_use]
    pub fn get_rtmp_identifiers(&self, room_id: &str, media_id: &str) -> Option<(String, String)> {
        let sk = Self::stream_key(room_id, media_id);
        self.rtmp_reverse.get(&sk).and_then(|rk| {
            // rtmp_key format is "{app_name}\0{stream_name}"
            rk.split_once('\0').map(|(app, stream)| (app.to_string(), stream.to_string()))
        })
    }

    /// Iterate over all stream entries. Provides `("room_id:media_id", user_id)`.
    pub fn iter_streams(&self) -> impl Iterator<Item = dashmap::mapref::multiple::RefMulti<'_, String, String>> {
        self.by_stream.iter()
    }

    /// Number of tracked streams.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_stream.len()
    }

    /// Whether the tracker is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_stream.is_empty()
    }

    /// Remove stale index entries that are orphaned from the primary `by_stream` map.
    ///
    /// When a publisher crashes without a clean `on_unpublish`, secondary indexes
    /// (`by_user`, `by_room`, `by_rtmp`, `rtmp_reverse`) can retain references to
    /// streams that no longer exist in `by_stream`. This method scans each index
    /// and removes entries whose stream key is no longer present.
    ///
    /// Returns the number of stale entries removed.
    pub fn cleanup_stale_entries(&self) -> usize {
        let mut removed = 0usize;

        // Clean by_user: remove stream keys that are not in by_stream
        let user_keys: Vec<String> = self.by_user.iter().map(|e| e.key().clone()).collect();
        for user_id in user_keys {
            if let Some(user_set) = self.by_user.get(&user_id) {
                let stale: Vec<String> = user_set
                    .iter()
                    .filter(|sk| !self.by_stream.contains_key(sk.as_str()))
                    .map(|sk| sk.clone())
                    .collect();
                for sk in &stale {
                    user_set.remove(sk);
                    removed += 1;
                }
                if user_set.is_empty() {
                    drop(user_set);
                    self.by_user.remove(&user_id);
                }
            }
        }

        // Clean by_room: remove media_ids whose stream key is not in by_stream
        let room_keys: Vec<String> = self.by_room.iter().map(|e| e.key().clone()).collect();
        for room_id in room_keys {
            if let Some(room_set) = self.by_room.get(&room_id) {
                let stale: Vec<String> = room_set
                    .iter()
                    .filter(|media_id| {
                        !self.by_stream.contains_key(&Self::stream_key(&room_id, media_id))
                    })
                    .map(|media_id| media_id.clone())
                    .collect();
                for media_id in &stale {
                    room_set.remove(media_id);
                    removed += 1;
                }
                if room_set.is_empty() {
                    drop(room_set);
                    self.by_room.remove(&room_id);
                }
            }
        }

        // Clean by_rtmp: remove entries whose stream key is not in by_stream
        let rtmp_keys: Vec<(String, String)> = self
            .by_rtmp
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (rk, sk) in rtmp_keys {
            if !self.by_stream.contains_key(&sk) {
                self.by_rtmp.remove(&rk);
                removed += 1;
            }
        }

        // Clean rtmp_reverse: remove entries whose stream key is not in by_stream
        let reverse_keys: Vec<String> = self
            .rtmp_reverse
            .iter()
            .map(|e| e.key().clone())
            .collect();
        for sk in reverse_keys {
            if !self.by_stream.contains_key(&sk) {
                self.rtmp_reverse.remove(&sk);
                removed += 1;
            }
        }

        if removed > 0 {
            info!(removed, "Cleaned up stale stream tracker entries");
        }

        removed
    }

    /// Spawn a periodic background task that calls `cleanup_stale_entries`
    /// every `interval` duration. Returns the `JoinHandle` for the task.
    pub fn start_periodic_cleanup(
        self: &Arc<Self>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let tracker = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                tracker.cleanup_stale_entries();
            }
        })
    }
}
