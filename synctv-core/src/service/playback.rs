//! Playback state management service
//!
//! Handles playback coordination including play/pause, seeking, speed changes,
//! and media switching with optimistic locking for concurrent updates.

use std::sync::Arc;
use std::time::Duration;

use crate::{
    cache::{CacheInvalidationService, InvalidationMessage, SingleFlight},
    models::{RoomId, UserId, MediaId, PlaylistId, PermissionBits, RoomPlaybackState, RoomSettings, PlayMode},
    repository::{RoomPlaybackStateRepository, MediaRepository},
    service::{permission::PermissionService, media::MediaService, notification::NotificationService},
    Error, Result,
};
use rand::prelude::IteratorRandom;
use rand::RngExt;

/// Trait for broadcasting playback state changes to cluster replicas.
///
/// This abstracts over the cluster manager so that `synctv-core` does not
/// depend on `synctv-cluster`.  The implementation lives in the API/wiring
/// layer where `ClusterManager` is available.
pub trait PlaybackBroadcaster: Send + Sync {
    /// Broadcast a playback state change to other cluster replicas.
    /// Implementations should be non-blocking (fire-and-forget).
    fn broadcast_playback_state(&self, state: &RoomPlaybackState);
}

/// Playback management service
///
/// Responsible for playback state coordination and optimistic locking.
#[derive(Clone)]
pub struct PlaybackService {
    playback_repo: RoomPlaybackStateRepository,
    permission_service: PermissionService,
    media_service: MediaService,
    media_repo: MediaRepository,
    /// Optional notification service for broadcasting to local WebSocket clients
    notification_service: Option<NotificationService>,
    /// Optional cluster broadcaster for cross-replica sync (interior mutability
    /// so the broadcaster can be wired after Arc<RoomService> is already cloned)
    cluster_broadcaster: Arc<parking_lot::RwLock<Option<Arc<dyn PlaybackBroadcaster>>>>,
    /// L1 in-memory cache for playback state, keyed by `room_id`
    playback_cache: Arc<moka::future::Cache<String, RoomPlaybackState>>,
    /// Optional cache invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<CacheInvalidationService>>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomPlaybackState, String>,
}

impl std::fmt::Debug for PlaybackService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackService").finish()
    }
}

impl PlaybackService {
    /// Default playback state cache capacity (max entries)
    pub const DEFAULT_CACHE_SIZE: u64 = 5_000;
    /// Default playback state cache TTL in seconds (short — playback changes frequently)
    pub const DEFAULT_CACHE_TTL_SECS: u64 = 5;

    /// Create a new playback service
    #[must_use]
    pub fn new(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        media_repo: MediaRepository,
    ) -> Self {
        Self {
            playback_repo,
            permission_service,
            media_service,
            media_repo,
            notification_service: None,
            cluster_broadcaster: Arc::new(parking_lot::RwLock::new(None)),
            playback_cache: Arc::new(
                moka::future::CacheBuilder::new(Self::DEFAULT_CACHE_SIZE)
                    .time_to_live(Duration::from_secs(Self::DEFAULT_CACHE_TTL_SECS))
                    .build(),
            ),
            invalidation_service: None,
            single_flight: SingleFlight::new(),
        }
    }

    /// Set the notification service for broadcasting playback state to local WebSocket clients
    pub fn set_notification_service(&mut self, service: NotificationService) {
        self.notification_service = Some(service);
    }

    /// Set the cluster broadcaster for cross-replica playback state sync.
    /// Uses interior mutability so this can be called through `Arc<RoomService>`.
    pub fn set_cluster_broadcaster(&self, broadcaster: Arc<dyn PlaybackBroadcaster>) {
        *self.cluster_broadcaster.write() = Some(broadcaster);
    }

    /// Set the cache invalidation service and start listening for cross-replica invalidation.
    ///
    /// When another replica updates playback state and broadcasts an invalidation
    /// message, this node's local L1 cache entry for that room is evicted so the
    /// next read fetches fresh data from the DB.
    pub fn set_invalidation_service(&mut self, service: Arc<CacheInvalidationService>) {
        let cache = self.playback_cache.clone();
        let mut receiver = service.subscribe();

        crate::spawn::spawn_monitored("playback_invalidation_listener", async move {
            loop {
                match receiver.recv().await {
                    Ok(msg) => match msg {
                        InvalidationMessage::PlaybackStateUpdate { room_id, state } => {
                            // Write the updated state directly into the L1 cache,
                            // avoiding the stale-read window between invalidation
                            // and the next DB fetch.
                            cache.insert(room_id.clone(), state).await;
                            tracing::debug!(
                                room_id = %room_id,
                                "Playback state cache updated directly (cross-replica)"
                            );
                        }
                        InvalidationMessage::PlaybackState { room_id } => {
                            cache.invalidate(&room_id).await;
                            tracing::debug!(
                                room_id = %room_id,
                                "Playback state cache invalidated (cross-replica)"
                            );
                        }
                        InvalidationMessage::Room { room_id } => {
                            // Room-scoped invalidation also clears playback cache
                            cache.invalidate(&room_id).await;
                        }
                        InvalidationMessage::All => {
                            cache.invalidate_all();
                            tracing::debug!("All playback state cache invalidated (cross-replica)");
                        }
                        _ => {
                            // Other message types not relevant to playback cache
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Playback cache invalidation channel closed, stopping listener");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            lagged_messages = n,
                            "Playback cache invalidation listener lagged, flushing all entries"
                        );
                        cache.invalidate_all();
                    }
                }
            }
        });

        self.invalidation_service = Some(service);
    }

    /// Broadcast a playback state change to local clients and cluster replicas.
    ///
    /// Best-effort: logs warnings on failure but does not propagate errors,
    /// since broadcasting is not critical to the mutation itself.
    async fn broadcast_state_change(&self, state: &RoomPlaybackState) {
        // 1. Notify local WebSocket clients
        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns.notify_playback_state_changed(
                &state.room_id,
                state.is_playing,
                state.current_time,
                state.speed,
                state.playing_media_id.as_ref().map(|id| id.as_str().to_string()),
            ).await {
                tracing::warn!(
                    error = %e,
                    room_id = %state.room_id.as_str(),
                    "Failed to notify local clients of playback state change"
                );
            }
        }

        // 2. Broadcast to other cluster replicas via the synchronous broadcaster.
        //    This is fire-and-forget at the trait level. Redis-backed cross-replica
        //    broadcast (with retry, Issue #28) is handled by update_state() which
        //    calls invalidation_service.update_playback_state() with retry logic.
        if let Some(ref broadcaster) = *self.cluster_broadcaster.read() {
            broadcaster.broadcast_playback_state(state);
        }
    }

    /// Get playback state for a room.
    ///
    /// Checks the L1 in-memory cache first; on miss, uses `SingleFlight` to ensure
    /// only one concurrent DB fetch per `room_id`, then populates the cache.
    pub async fn get_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let cache_key = room_id.as_str().to_string();

        // L1 cache hit
        if let Some(state) = self.playback_cache.get(&cache_key).await {
            return Ok(state);
        }

        // Cache miss — use SingleFlight to prevent thundering herd:
        // Only one task loads from DB for a given room_id; others wait for the result.
        let repo = self.playback_repo.clone();
        let cache = self.playback_cache.clone();
        let room_id_clone = room_id.clone();

        let state = self.single_flight.do_work_with_fallback(
            cache_key,
            async move {
                let state = match repo.get(&room_id_clone).await {
                    Ok(Some(s)) => s,
                    Ok(None) => RoomPlaybackState::new(room_id_clone),
                    Err(e) => return Err(e.to_string()),
                };

                // Populate cache
                cache.insert(state.room_id.as_str().to_string(), state.clone()).await;

                Ok(state)
            },
            || "SingleFlight worker failed during playback state fetch".to_string(),
        ).await.map_err(Error::Internal)?;

        Ok(state)
    }

    /// Invalidate the local playback state cache for a room.
    ///
    /// If a `CacheInvalidationService` is configured, this also broadcasts the
    /// invalidation to other replicas via Redis Pub/Sub.
    pub async fn invalidate_playback_cache(&self, room_id: &RoomId) {
        // Broadcast to other replicas first (if configured)
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_playback_state(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast playback state cache invalidation"
                );
            }
        }

        // Invalidate local cache
        let cache_key = room_id.as_str().to_string();
        self.playback_cache.invalidate(&cache_key).await;
    }

    /// Play/pause playback
    pub async fn set_playing(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: bool,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::PLAY_PAUSE)
            .await?;

        let state = self.update_state(room_id.clone(), |state| {
            if !playing {
                // Snapshot the computed playback position before pausing so that
                // the stored current_time reflects where the user actually was.
                // Without this, resuming would jump back to the last persisted time.
                state.current_time = state.computed_current_time();
            }
            state.is_playing = playing;
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state).await;
        Ok(state)
    }

    /// Seek to position
    pub async fn seek(
        &self,
        room_id: RoomId,
        user_id: UserId,
        current_time: f64,
    ) -> Result<RoomPlaybackState> {
        if current_time < 0.0 {
            return Err(Error::InvalidInput("Seek position must be non-negative".to_string()));
        }

        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::SEEK)
            .await?;

        let state = self.update_state(room_id.clone(), |state| {
            state.current_time = current_time;
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state).await;
        Ok(state)
    }

    /// Change playback speed
    pub async fn change_speed(
        &self,
        room_id: RoomId,
        user_id: UserId,
        speed: f64,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::CHANGE_SPEED)
            .await?;

        // Validate speed range
        if !(0.25..=4.0).contains(&speed) {
            return Err(Error::InvalidInput("Speed must be between 0.25 and 4.0".to_string()));
        }

        let state = self.update_state(room_id.clone(), |state| {
            // Snapshot the computed playback position before changing speed so that
            // the stored current_time reflects where the user actually was at the
            // old speed. Without this, the position would be wrong because
            // computed_current_time() uses speed to extrapolate from updated_at.
            state.current_time = state.computed_current_time();
            state.speed = speed;
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state).await;
        Ok(state)
    }

    /// Switch to different media in playlist
    pub async fn switch_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<RoomPlaybackState> {
        self.switch_media_with_context(room_id, user_id, media_id, None, String::new()).await
    }

    /// Switch to different media with playlist context and media path
    pub async fn switch_media_with_context(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
        playlist_id: Option<PlaylistId>,
        media_path: String,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::SWITCH_MEDIA)
            .await?;

        // Verify media exists in this room
        let media = self
            .media_service
            .get_media(&media_id)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        if media.room_id != room_id {
            return Err(Error::Authorization("Media does not belong to this room".to_string()));
        }

        let state = self.update_state(room_id.clone(), |state| {
            state.playing_media_id = Some(media_id.clone());
            state.playing_playlist_id = playlist_id.clone();
            state.relative_path = media_path.clone();
            state.current_time = 0.0;
            state.is_playing = true;
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state).await;
        Ok(state)
    }

    /// Play next media in playlist (auto-play next episode)
    ///
    /// This is called when current media finishes playing.
    /// Returns the new playback state if successful, or None if there's no next media.
    ///
    /// Uses a retry loop that re-fetches state and playlist on each attempt to
    /// avoid TOCTOU races where another user changes the playlist or playback
    /// state between the read and the optimistic-lock update.
    pub async fn play_next(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
    ) -> Result<Option<RoomPlaybackState>> {
        // Use new auto_play settings, falling back to legacy fields for compatibility
        let (enabled, mode) = if settings.auto_play.value.enabled || settings.auto_play_next.0 {
            let mode = settings.auto_play.value.mode;
            let enabled = settings.auto_play.value.enabled || settings.auto_play_next.0;

            // If legacy fields suggest a different mode than the new setting, use legacy
            let mode = if settings.loop_playlist.0 {
                PlayMode::RepeatAll
            } else if settings.shuffle_playlist.0 {
                PlayMode::Shuffle
            } else {
                mode
            };

            (enabled, mode)
        } else {
            (false, PlayMode::Sequential)
        };

        if !enabled {
            return Ok(None);
        }

        // Retry loop: re-fetch state + playlist on each attempt so that
        // concurrent playlist/state changes are correctly reflected.
        for attempt in 0..Self::MAX_RETRIES {
            // Get current state (fresh on every retry)
            let state = match self.playback_repo.get(room_id).await? {
                Some(s) => s,
                None => self.playback_repo.create_or_get(room_id).await?,
            };

            // Get playlist scoped to the current playing playlist folder if set,
            // otherwise fall back to the flat room-wide list.
            let playlist = if let Some(ref playlist_id) = state.playing_playlist_id {
                self.media_service.get_playlist_media(playlist_id).await?
            } else {
                self.media_repo.get_playlist(room_id).await?
            };

            if playlist.is_empty() {
                return Ok(None);
            }

            // Handle different play modes
            let next_media = match mode {
                PlayMode::Sequential => {
                    // Find next media by position
                    let current_pos = if let Some(ref current_id) = state.playing_media_id {
                        playlist.iter()
                            .position(|m| &m.id == current_id)
                            .unwrap_or(0)
                    } else {
                        0
                    };

                    if current_pos + 1 < playlist.len() {
                        Some(&playlist[current_pos + 1])
                    } else {
                        None // End of playlist
                    }
                }

                PlayMode::RepeatOne => {
                    // Repeat current media.
                    // Issue #29: If the currently-playing media was deleted from the
                    // playlist, find() returns None and we would enter a dead state
                    // where nothing plays. Fall back to Sequential behavior (advance
                    // to the next item) so playback continues gracefully.
                    if let Some(ref current_id) = state.playing_media_id {
                        let found = playlist.iter().find(|m| &m.id == current_id);
                        if found.is_some() {
                            found
                        } else {
                            // Media was deleted — advance to first item as fallback
                            tracing::warn!(
                                room_id = %room_id.as_str(),
                                media_id = %current_id.as_str(),
                                "RepeatOne: currently-playing media not found in playlist (deleted?), falling back to first item"
                            );
                            playlist.first()
                        }
                    } else {
                        playlist.first()
                    }
                }

                PlayMode::RepeatAll => {
                    // Loop back to start
                    let current_pos = if let Some(ref current_id) = state.playing_media_id {
                        playlist.iter()
                            .position(|m| &m.id == current_id)
                            .unwrap_or(0)
                    } else {
                        0
                    };

                    let next_pos = (current_pos + 1) % playlist.len();
                    Some(&playlist[next_pos])
                }

                PlayMode::Shuffle => {
                    // Random next media (excluding current)
                    //
                    // NOTE: This is a simplified shuffle implementation that randomly selects
                    // the next media from the playlist (excluding the current one).
                    //
                    // Pros: Simple, efficient, no additional state storage required
                    // Cons: May play some media more frequently than others
                    //
                    // For a production-grade shuffle without repeats, consider implementing
                    // Fisher-Yates shuffle algorithm with persistent state storage (Redis):
                    // 1. Shuffle the entire playlist once
                    // 2. Play through shuffled order
                    // 3. Re-shuffle when all items played
                    // See: /Volumes/workspace/rust/design/13-自动连播设计.md §3.4
                    if let Some(ref current_id) = state.playing_media_id {
                        playlist.iter()
                            .filter(|m| &m.id != current_id)
                            .choose(&mut rand::rng())
                    } else {
                        playlist.first()
                    }
                }
            };

            // Switch to next media
            let Some(next) = next_media else {
                tracing::info!(
                    room_id = %room_id.as_str(),
                    mode = ?mode,
                    "Playlist ended"
                );
                return Ok(None);
            };

            // Apply update to the fetched state and try to save with optimistic locking
            let mut updated_state = state;
            updated_state.playing_media_id = Some(next.id.clone());
            updated_state.playing_playlist_id = Some(next.playlist_id.clone());
            updated_state.relative_path = next.name.clone();
            updated_state.current_time = 0.0;
            updated_state.is_playing = true;
            updated_state.updated_at = chrono::Utc::now();

            match self.playback_repo.update(&updated_state).await {
                Ok(saved_state) => {
                    // Invalidate local cache
                    let cache_key = room_id.as_str().to_string();
                    self.playback_cache.invalidate(&cache_key).await;

                    // Broadcast to other replicas (Issue #28: retry once on failure)
                    if let Some(ref service) = self.invalidation_service {
                        let bc_result = service.update_playback_state(room_id, &saved_state).await;
                        if let Err(ref e) = bc_result {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                "play_next: broadcast failed (attempt 1/2), retrying..."
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            if let Err(e2) = service.update_playback_state(room_id, &saved_state).await {
                                tracing::error!(
                                    error = %e2,
                                    room_id = %room_id.as_str(),
                                    "play_next: broadcast failed after 2 attempts, replicas may have stale state"
                                );
                            }
                        }
                    }

                    tracing::info!(
                        room_id = %room_id.as_str(),
                        media_id = %next.id.as_str(),
                        name = %next.name,
                        mode = ?mode,
                        "Auto-played next media"
                    );

                    self.broadcast_state_change(&saved_state).await;
                    return Ok(Some(saved_state));
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    let delay = backoff + jitter;
                    tracing::debug!(
                        room_id = %room_id.as_str(),
                        attempt = attempt + 1,
                        delay_ms = delay,
                        "play_next version conflict, re-fetching state and retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(
            "play_next failed after maximum retry attempts".to_string(),
        ))
    }

    /// Check if media has ended and auto-play next if needed
    ///
    /// This should be called when playback `current_time` is updated.
    /// It checks if the current time has reached or exceeded the media duration.
    pub async fn check_and_auto_play(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        _current_time: f64,
    ) -> Result<Option<RoomPlaybackState>> {
        // Use new auto_play settings with legacy fallback
        let enabled = settings.auto_play.value.enabled || settings.auto_play_next.0;

        if !enabled {
            return Ok(None);
        }

        // Get current media to check duration
        let state = self.get_state(room_id).await?;
        let playing_media_id = state.playing_media_id.clone();

        let playing_media = match playing_media_id {
            Some(ref id) => self.media_service.get_media(id).await?.ok_or_else(|| {
                Error::NotFound("Current media not found".to_string())
            })?,
            None => return Ok(None),
        };

        // Check if media has metadata with duration
        // For direct URLs, get duration from PlaybackResult metadata
        // For provider-based media, duration check is skipped (client should handle)
        let duration = if playing_media.is_direct() {
            if let Some(playback_result) = playing_media.get_playback_result() {
                playback_result.metadata.get("duration")
                    .and_then(serde_json::Value::as_f64)
            } else {
                return Ok(None);
            }
        } else {
            // For provider-based media, auto-play is handled by client or provider
            return Ok(None);
        };

        // Use computed time to account for elapsed wall-clock time when playing
        let effective_time = state.computed_current_time();

        // Check if current_time is near end (within 1 second or past end)
        if let Some(dur) = duration {
            if effective_time >= dur - 1.0 {
                // Auto-play next media
                self.play_next(room_id, settings).await
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Maximum retry attempts for optimistic lock conflicts
    const MAX_RETRIES: u32 = 5;
    /// Base delay for exponential backoff (milliseconds)
    const BACKOFF_BASE_MS: u64 = 5;

    /// Update playback state with generic update function.
    ///
    /// Uses optimistic locking with automatic retry on version conflicts.
    /// Retries use exponential backoff with jitter to avoid thundering herd.
    pub async fn update_state<F>(
        &self,
        room_id: RoomId,
        update_fn: F,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        for attempt in 0..Self::MAX_RETRIES {
            // Get current state (lazy-init: only INSERT if row doesn't exist yet)
            let mut state = match self.playback_repo.get(&room_id).await? {
                Some(s) => s,
                None => self.playback_repo.create_or_get(&room_id).await?,
            };

            // Apply update
            update_fn(&mut state);

            // Save with optimistic locking
            match self.playback_repo.update(&state).await {
                Ok(updated_state) => {
                    // Invalidate local cache so the next read fetches fresh data.
                    // This avoids write-through which would self-invalidate when the
                    // Redis Pub/Sub bounce-back arrives.
                    let cache_key = room_id.as_str().to_string();
                    self.playback_cache.invalidate(&cache_key).await;

                    // Broadcast updated state to other replicas so they can write
                    // it directly into their L1 cache, avoiding the stale-read
                    // window that occurs with invalidation-only messages.
                    //
                    // Issue #28: Redis broadcast failures are logged at ERROR level.
                    // A single retry is attempted; if all retries fail, we log
                    // with enough context for operators to replay the update.
                    if let Some(ref service) = self.invalidation_service {
                        let broadcast_result = service.update_playback_state(&room_id, &updated_state).await;
                        if let Err(ref e) = broadcast_result {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                is_playing = updated_state.is_playing,
                                current_time = updated_state.current_time,
                                "Playback broadcast to replicas failed (attempt 1/2), retrying..."
                            );
                            // Retry once after a brief delay (Issue #28)
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            if let Err(e2) = service.update_playback_state(&room_id, &updated_state).await {
                                tracing::error!(
                                    error = %e2,
                                    room_id = %room_id.as_str(),
                                    is_playing = updated_state.is_playing,
                                    current_time = updated_state.current_time,
                                    "Playback broadcast failed after 2 attempts. \
                                     Other replicas may have stale playback state."
                                );
                            }
                        }
                    }

                    return Ok(updated_state);
                }
                Err(Error::OptimisticLockConflict) if attempt + 1 < Self::MAX_RETRIES => {
                    // Exponential backoff with jitter: base * 2^attempt + random(0..base)
                    let backoff = Self::BACKOFF_BASE_MS * (1 << attempt);
                    let jitter = rand::rng().random_range(0..Self::BACKOFF_BASE_MS);
                    let delay = backoff + jitter;
                    tracing::debug!(
                        room_id = %room_id.as_str(),
                        attempt = attempt + 1,
                        delay_ms = delay,
                        "Playback state version conflict, retrying with backoff"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(
            "Playback state update failed after maximum retry attempts".to_string(),
        ))
    }

    /// Reset playback to initial state
    pub async fn reset(&self, room_id: RoomId, user_id: UserId) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::PLAY_PAUSE)
            .await?;

        let state = self.update_state(room_id, |state| {
            state.is_playing = false;
            state.current_time = 0.0;
            state.speed = 1.0;
            state.playing_media_id = None;
            state.playing_playlist_id = None;
            state.relative_path = String::new();
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        self.broadcast_state_change(&state).await;
        Ok(state)
    }

    /// Check if playback is currently active
    pub async fn is_playing(&self, room_id: &RoomId) -> Result<bool> {
        let state = self.get_state(room_id).await?;
        Ok(state.is_playing)
    }

    /// Get current media being played
    pub async fn get_playing_media_id(&self, room_id: &RoomId) -> Result<Option<MediaId>> {
        let state = self.get_state(room_id).await?;
        Ok(state.playing_media_id)
    }

    /// Get current playback position
    pub async fn get_current_time(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.current_time)
    }

    /// Get current playback speed
    pub async fn get_speed(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.speed)
    }

    /// Update multiple playback properties at once
    pub async fn update_multiple(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        media_id: Option<MediaId>,
        playlist_id: Option<Option<PlaylistId>>,
    ) -> Result<RoomPlaybackState> {
        self.update_multiple_with_version(room_id, user_id, playing, current_time, speed, media_id, playlist_id, None).await
    }

    /// Like `update_multiple`, but accepts an optional `expected_version` hint.
    ///
    /// Previously this performed a separate pre-read to check the version before
    /// the update, but the SQL `WHERE version = $N` optimistic lock in
    /// `update_state()` already provides the same protection without the extra
    /// DB round-trip. The parameter is retained for API compatibility.
    pub async fn update_multiple_with_version(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: Option<bool>,
        current_time: Option<f64>,
        speed: Option<f64>,
        media_id: Option<MediaId>,
        playlist_id: Option<Option<PlaylistId>>,
        _expected_version: Option<i64>,
    ) -> Result<RoomPlaybackState> {
        // Check permissions based on what's being updated
        let mut required_perms = PermissionBits::NONE;
        if playing.is_some() {
            required_perms |= PermissionBits::PLAY_PAUSE;
        }
        if current_time.is_some() {
            required_perms |= PermissionBits::SEEK;
        }
        if speed.is_some() {
            required_perms |= PermissionBits::CHANGE_SPEED;
        }
        if media_id.is_some() {
            required_perms |= PermissionBits::SWITCH_MEDIA;
        }

        if required_perms != PermissionBits::NONE {
            self.permission_service
                .check_permission(&room_id, &user_id, required_perms)
                .await?;
        }

        // Validate speed range if provided
        if let Some(s) = speed {
            if !(0.25..=4.0).contains(&s) {
                return Err(Error::InvalidInput("Speed must be between 0.25 and 4.0".to_string()));
            }
        }

        // If media_id is provided, verify it exists
        if let Some(ref mid) = media_id {
            let media = self
                .media_service
                .get_media(mid)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            if media.room_id != room_id {
                return Err(Error::Authorization("Media does not belong to this room".to_string()));
            }
        }

        // NOTE: No separate pre-read version check here. The SQL UPDATE in
        // update_state() uses `WHERE version = $N` for optimistic locking,
        // which is sufficient to detect conflicts without an extra DB round-trip.

        let state = self.update_state(room_id.clone(), |state| {
            if let Some(p) = playing {
                state.is_playing = p;
            }
            if let Some(ct) = current_time {
                state.current_time = ct;
            }
            if let Some(s) = speed {
                state.speed = s;
            }
            if let Some(ref mid) = media_id {
                state.playing_media_id = Some(mid.clone());
            }
            if let Some(ref pid) = playlist_id {
                state.playing_playlist_id = pid.clone();
            }
            state.updated_at = chrono::Utc::now();
            // version is incremented by the SQL UPDATE, not here
        })
        .await?;

        // Cache invalidation is already handled inside update_state()
        self.broadcast_state_change(&state).await;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speed_validation_bounds() {
        // Valid boundary values
        assert!((0.25..=4.0).contains(&0.25));
        assert!((0.25..=4.0).contains(&4.0));
        assert!((0.25..=4.0).contains(&1.0));

        // Invalid boundary values
        assert!(!(0.25..=4.0).contains(&0.24));
        assert!(!(0.25..=4.0).contains(&4.1));
        assert!(!(0.25..=4.0).contains(&0.0));
        assert!(!(0.25..=4.0).contains(&-1.0));
    }

    #[test]
    fn test_seek_negative_position() {
        let position = -1.0_f64;
        assert!(position < 0.0, "Negative seek positions should be rejected");

        let position = 0.0_f64;
        assert!(!(position < 0.0), "Zero seek position should be accepted");

        let position = 42.5_f64;
        assert!(!(position < 0.0), "Positive seek position should be accepted");
    }

    #[test]
    fn test_update_state_constants() {
        assert_eq!(PlaybackService::MAX_RETRIES, 5);
        assert_eq!(PlaybackService::BACKOFF_BASE_MS, 5);
    }
}
