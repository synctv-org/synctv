//! Playback state management service
//!
//! Handles playback coordination including play/pause, seeking, speed changes,
//! and media switching with optimistic locking for concurrent updates.

use std::sync::Arc;
use std::time::Duration;

use crate::{
    cache::{
        CacheDomain, CacheInvalidationRuntime, CloneableError, ConsistencyCoordinator,
        PlaybackStateCache, SingleFlight, VersionFenceReservation, VersionFenceStore,
    },
    models::{
        BilibiliMediaSourceConfig, MediaId, PlayMode, PlaybackSourceIdentity,
        PlaybackSourceMetadata, PlaylistId, ProviderTarget, RoomId, RoomPlaybackState,
        RoomSettings, SourceProvider, UserId,
    },
    repository::{
        realtime_outbox::RealtimeOutboxRepository, PlaybackSourceMetadataRepository,
        RoomPlaybackStateRepository,
    },
    service::{media::BackendPlaybackRequest, media::MediaService, PermissionService, UserService},
    Clock, Error, Result, SystemClock,
};
use rand::prelude::IteratorRandom;

mod cache_read;
mod invalidation;
mod types;
use invalidation::PlaybackInvalidationRuntime;
#[cfg(test)]
use types::MAX_PLAYBACK_POSITION_SECONDS;
use types::{
    previous_progress_position_for_source_transition, validate_playback_speed_value,
    validate_position_update_source, validate_seek_position, validate_switch_target, NextTarget,
};
pub use types::{
    PlaybackSourceExpectation, PlaybackStatePatch, PlaybackStateUpdateRequest,
    RealtimeOutboxPlaybackStateEventFactory, SeekResponse, SwitchPlaybackTarget,
};

/// Playback management service
///
/// Responsible for playback state coordination and optimistic locking.
#[derive(Clone)]
pub struct PlaybackService {
    clock: Arc<dyn Clock>,
    playback_repo: RoomPlaybackStateRepository,
    source_metadata_repo: PlaybackSourceMetadataRepository,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    permission_service: PermissionService,
    media_service: MediaService,
    user_service: UserService,
    /// L1 in-memory cache for playback state, keyed by `room_id`
    playback_cache: Arc<moka::future::Cache<String, RoomPlaybackState>>,
    /// Optional L2 cache (Redis) for cross-replica consistency.
    /// Held behind interior mutability so listeners started before L2 wiring
    /// still observe the final cache configuration.
    l2_cache: Arc<parking_lot::RwLock<Option<PlaybackStateCache>>>,
    /// Optional cache invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    consistency: ConsistencyCoordinator,
    /// Shared lifecycle state for the background invalidation listener.
    invalidation_runtime: Arc<PlaybackInvalidationRuntime>,
    /// `SingleFlight` to prevent thundering herd on cache miss.
    /// Uses `String` key (`room_id`) and `String` error (since `Error` is not `Clone`).
    single_flight: SingleFlight<String, RoomPlaybackState, CloneableError>,
}

pub struct PlaybackServiceRuntime {
    pub clock: Arc<dyn Clock>,
    pub invalidation_service: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub l2_cache: Option<PlaybackStateCache>,
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub source_metadata_repo: Option<PlaybackSourceMetadataRepository>,
}

impl PlaybackServiceRuntime {
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            invalidation_service: None,
            l2_cache: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            realtime_outbox: None,
            source_metadata_repo: None,
        }
    }
}

struct PlaybackSwitchCommand {
    room_id: RoomId,
    actor_user_id: UserId,
    target: SwitchPlaybackTarget,
    bypass_room_permissions: bool,
    outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
}

fn live_status_for_media_source(
    provider: SourceProvider,
    source_config: &crate::models::MediaSourceConfig,
) -> Option<bool> {
    match (provider, source_config) {
        (SourceProvider::DirectUrl, crate::models::MediaSourceConfig::DirectUrl(config)) => {
            config.inferred_live_status()
        }
        (SourceProvider::Bilibili, crate::models::MediaSourceConfig::Bilibili(config)) => {
            Some(matches!(config, BilibiliMediaSourceConfig::Live(_)))
        }
        (
            SourceProvider::Alist
            | SourceProvider::Emby
            | SourceProvider::Cloudreve
            | SourceProvider::Cctv
            | SourceProvider::Fnos
            | SourceProvider::Synology
            | SourceProvider::Seafile,
            _,
        ) => Some(false),
        (SourceProvider::Rtmp | SourceProvider::LiveProxy, _) => Some(true),
        (SourceProvider::Douyin, crate::models::MediaSourceConfig::Douyin(config)) => Some(
            matches!(config, crate::models::DouyinMediaSourceConfig::Live { .. }),
        ),
        (SourceProvider::TikTok, crate::models::MediaSourceConfig::TikTok(config)) => Some(
            matches!(config, crate::models::TikTokMediaSourceConfig::Live { .. }),
        ),
        _ => None,
    }
}

fn live_status_for_playlist_source(
    provider: SourceProvider,
    source_config: &crate::models::PlaylistSourceConfig,
) -> Option<bool> {
    match provider {
        SourceProvider::Alist
        | SourceProvider::Emby
        | SourceProvider::Synology
        | SourceProvider::Nextcloud
        | SourceProvider::Seafile
        | SourceProvider::TrueNas => (source_config.provider() == provider).then_some(false),
        SourceProvider::DirectUrl
        | SourceProvider::Bilibili
        | SourceProvider::Rtmp
        | SourceProvider::LiveProxy
        | SourceProvider::Cloudreve
        | SourceProvider::Twitch
        | SourceProvider::Huya
        | SourceProvider::Douyu
        | SourceProvider::Douyin
        | SourceProvider::AcFun
        | SourceProvider::Cctv
        | SourceProvider::Fnos
        | SourceProvider::Qnap
        | SourceProvider::Youtube
        | SourceProvider::TikTok => None,
    }
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
    /// Maximum time to wait for the invalidation listener to stop.
    const INVALIDATION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

    async fn insert_playback_outbox_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        state: &RoomPlaybackState,
        outbox_event_factory: Option<&RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(state))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    /// Create a new playback service
    #[must_use]
    pub fn new(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        user_service: UserService,
    ) -> Self {
        Self::new_with_runtime(
            playback_repo,
            permission_service,
            media_service,
            user_service,
            PlaybackServiceRuntime::local_only(),
        )
    }

    /// Create a playback service with optional cache runtime dependencies wired
    /// at construction time.
    #[must_use]
    pub fn new_with_runtime(
        playback_repo: RoomPlaybackStateRepository,
        permission_service: PermissionService,
        media_service: MediaService,
        user_service: UserService,
        runtime: PlaybackServiceRuntime,
    ) -> Self {
        let source_metadata_repo = runtime
            .source_metadata_repo
            .unwrap_or_else(|| PlaybackSourceMetadataRepository::new(playback_repo.pool().clone()));
        Self {
            clock: runtime.clock,
            playback_repo,
            source_metadata_repo,
            realtime_outbox: runtime.realtime_outbox,
            permission_service,
            media_service,
            user_service,
            playback_cache: Arc::new(
                moka::future::CacheBuilder::new(Self::DEFAULT_CACHE_SIZE)
                    .time_to_live(Duration::from_secs(Self::DEFAULT_CACHE_TTL_SECS))
                    .build(),
            ),
            l2_cache: Arc::new(parking_lot::RwLock::new(runtime.l2_cache)),
            invalidation_service: runtime.invalidation_service,
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
            invalidation_runtime: Arc::new(PlaybackInvalidationRuntime::new()),
            single_flight: SingleFlight::new(),
        }
    }

    async fn ensure_creator_is_active(
        &self,
        creator_id: Option<&UserId>,
        resource_kind: &'static str,
    ) -> Result<()> {
        let Some(creator_id) = creator_id else {
            return Ok(());
        };

        match self.user_service.get_user(creator_id).await {
            Ok(user) if user.status.is_active() => Ok(()),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::Authorization(format!(
                "{resource_kind} is unavailable because its creator is not active"
            ))),
            Err(error) => Err(error),
        }
    }

    pub const fn has_invalidation_service(&self) -> bool {
        self.invalidation_service.is_some()
    }

    #[must_use]
    pub const fn source_metadata_repository(&self) -> &PlaybackSourceMetadataRepository {
        &self.source_metadata_repo
    }

    pub async fn get_playback_source_metadata(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<Option<PlaybackSourceMetadata>> {
        self.source_metadata_repo.get(identity).await
    }

    pub async fn upsert_provider_playback_source_metadata(
        &self,
        identity: &PlaybackSourceIdentity,
        is_live: bool,
        duration_seconds: Option<f64>,
    ) -> Result<PlaybackSourceMetadata> {
        self.source_metadata_repo
            .upsert_provider_source_metadata(identity, is_live, duration_seconds)
            .await
    }

    pub async fn mark_probeable_playback_source_metadata_unknown_if_absent(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<PlaybackSourceMetadata> {
        self.source_metadata_repo
            .mark_probeable_unknown_if_absent(identity)
            .await
    }

    pub async fn source_live_status_for_state(
        &self,
        state: &RoomPlaybackState,
    ) -> Result<Option<bool>> {
        if let Some(media_id) = state.playing_media_id {
            let Some(media) = self
                .media_service
                .get_room_media(&state.room_id, &media_id)
                .await?
            else {
                return Ok(None);
            };
            return Ok(live_status_for_media_source(
                media.source_provider,
                &media.source_config,
            ));
        }

        if let Some(playlist_id) = state.playing_playlist_id {
            let Some(playlist) = self
                .media_service
                .get_room_playlist(&state.room_id, &playlist_id)
                .await?
            else {
                return Ok(None);
            };
            return Ok(
                match (playlist.source_provider, playlist.source_config.as_ref()) {
                    (Some(provider), Some(source_config)) => {
                        live_status_for_playlist_source(provider, source_config)
                    }
                    _ => None,
                },
            );
        }

        Ok(None)
    }

    async fn reject_position_update_for_live_source(
        &self,
        state: &RoomPlaybackState,
    ) -> Result<()> {
        let Some(identity) = PlaybackSourceIdentity::from_state(state)? else {
            return Ok(());
        };
        if self
            .source_metadata_repo
            .get(&identity)
            .await?
            .is_some_and(|metadata| metadata.is_live == Some(true))
        {
            return Err(Error::InvalidInput(
                "live playback does not accept position updates".to_string(),
            ));
        }
        if self.source_live_status_for_state(state).await? == Some(true) {
            return Err(Error::InvalidInput(
                "live playback does not accept position updates".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn generate_backend_playback_for_source(
        &self,
        request: BackendPlaybackRequest<'_>,
    ) -> Result<Option<crate::provider::PlaybackResult>> {
        self.media_service
            .generate_backend_playback_for_source(request)
            .await
    }

    fn playback_domain(room_id: &RoomId) -> CacheDomain {
        CacheDomain::Playback { room_id: *room_id }
    }

    async fn advance_playback_version_fence(&self, room_id: &RoomId, version: i64) -> Result<()> {
        if !self.consistency.is_authoritative() {
            return Ok(());
        }

        self.consistency
            .set_version_at_least(&Self::playback_domain(room_id), version)
            .await?;
        Ok(())
    }

    async fn seed_playback_version_fence_after_reload(&self, room_id: &RoomId, version: i64) {
        if let Err(error) = self.advance_playback_version_fence(room_id, version).await {
            tracing::warn!(
                room_id = %room_id,
                version,
                error = %error,
                "Failed to seed playback version fence after DB reload"
            );
        }
    }

    async fn begin_playback_write_from_db_version(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = Self::playback_domain(room_id);
        self.consistency
            .begin_observed_write(&domain, db_version)
            .await
    }

    async fn commit_playback_write(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        self.consistency
            .commit_reserved_write(&Self::playback_domain(room_id), reservation, version)
            .await?;
        Ok(())
    }

    async fn finalize_committed_playback_write_best_effort(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .commit_playback_write(room_id, reservation, version)
            .await
        {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                version,
                operation,
                "Failed to finalize playback fence after committed DB write"
            );
        }
    }

    async fn abort_playback_write(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
    ) {
        self.consistency
            .abort_reserved_write(&Self::playback_domain(room_id), reservation)
            .await;
    }

    async fn persist_playback_state_update_with_previous_progress(
        &self,
        state: &RoomPlaybackState,
        observed_version: i64,
        previous_progress_position: Option<f64>,
        outbox_event_factory: Option<&RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        let reservation = self
            .begin_playback_write_from_db_version(&state.room_id, observed_version)
            .await?;
        let new_version = reservation
            .as_ref()
            .map_or(state.version + 1, |reservation| reservation.version);
        let mut tx = self.playback_repo.pool().begin().await?;
        let result = async {
            let updated_state = self
                .playback_repo
                .update_with_exact_version_executor_and_previous_progress(
                    state,
                    new_version,
                    previous_progress_position,
                    &mut tx,
                )
                .await?;
            self.insert_playback_outbox_tx(&mut tx, &updated_state, outbox_event_factory)
                .await?;
            Ok(updated_state)
        }
        .await;

        let updated_state = match result {
            Ok(updated_state) => {
                if let Err(error) = tx.commit().await {
                    self.abort_playback_write(&state.room_id, reservation.as_ref())
                        .await;
                    return Err(error.into());
                }
                updated_state
            }
            Err(error) => {
                if let Err(rollback_error) = tx.rollback().await {
                    tracing::warn!(
                        room_id = %state.room_id,
                        error = %rollback_error,
                        "Failed to roll back playback state update transaction"
                    );
                }
                self.abort_playback_write(&state.room_id, reservation.as_ref())
                    .await;
                return Err(error);
            }
        };

        self.finalize_committed_playback_write_best_effort(
            &state.room_id,
            reservation.as_ref(),
            updated_state.version,
            "persist_playback_state_update",
        )
        .await;
        Ok(updated_state)
    }

    /// Play/pause playback
    pub async fn set_playing(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playing: bool,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::PLAY_CONTROL,
            )
            .await?;

        let state = self
            .update_state(room_id, |state| {
                if !playing {
                    // Snapshot the computed playback position before pausing so that
                    // the stored position reflects where the user actually was.
                    // Without this, resuming would jump back to the last persisted time.
                    state.position = state.computed_position();
                }
                state.is_playing = playing;
                state.updated_at = self.clock.now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        Ok(state)
    }

    /// Seek to position.
    ///
    /// If the optimistic lock retries are exhausted (e.g., during rapid seek
    /// bursts), falls back to returning the latest playback state as a
    /// degraded response so the client knows the playback position, rather
    /// than receiving a bare error.
    ///
    /// Returns a `SeekResponse` containing:
    /// - `state`: The current playback state (always valid)
    /// - `seek_applied`: Whether the requested position was successfully applied
    /// - `message`: Optional explanation when seek was not applied
    pub async fn seek(
        &self,
        room_id: RoomId,
        user_id: UserId,
        position: f64,
    ) -> Result<SeekResponse> {
        let result = self
            .update_playback_state(PlaybackStateUpdateRequest::new(
                room_id,
                user_id,
                PlaybackStatePatch::new(None, Some(position), None),
            ))
            .await;

        match result {
            Ok(state) => Ok(SeekResponse::success(state)),
            Err(error)
                if crate::service::optimistic_retry::is_retry_exhausted(
                    &error,
                    Self::UPDATE_STATE_RETRY_EXHAUSTED,
                ) =>
            {
                // Degraded response: seek failed due to contention, but return
                // the latest state so the client can display the playback position.
                tracing::warn!(
                    room_id = %room_id,
                    requested_time = position,
                    "Seek failed after max retries, returning latest state as degraded response"
                );
                let state = self.get_state(&room_id).await?;
                Ok(SeekResponse::degraded(
                    state,
                    "Seek could not be applied due to concurrent modifications; returning current state",
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// Change playback speed
    pub async fn change_speed(
        &self,
        room_id: RoomId,
        user_id: UserId,
        speed: f64,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CHANGE_PLAYBACK_RATE,
            )
            .await?;

        validate_playback_speed_value(speed)?;

        let state = self
            .update_state(room_id, |state| {
                // Snapshot the computed playback position before changing speed so that
                // the stored position reflects where the user actually was at the
                // old speed. Without this, the position would be wrong because
                // computed_position() uses speed to extrapolate from updated_at.
                state.position = state.computed_position();
                state.speed = speed;
                state.updated_at = self.clock.now();
                // version is incremented by the SQL UPDATE, not here
            })
            .await?;

        Ok(state)
    }

    /// Switch playback target.
    ///
    /// Valid targets are mutually exclusive:
    /// - static media: `media_id` only
    /// - dynamic playlist item: `playlist_id` + `target`
    pub async fn switch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
    ) -> Result<RoomPlaybackState> {
        self.switch_with_outbox(room_id, user_id, media_id, playlist_id, target, None)
            .await
    }

    pub async fn switch_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(PlaybackSwitchCommand {
            room_id,
            actor_user_id: user_id,
            target: SwitchPlaybackTarget {
                media_id,
                playlist_id,
                target,
            },
            bypass_room_permissions: false,
            outbox_event_factory,
        })
        .await
    }

    /// Management-only playback switch that is authorized outside the room permission graph.
    ///
    /// Callers must validate global admin/root identity before invoking this method.
    pub async fn admin_switch(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
    ) -> Result<RoomPlaybackState> {
        self.admin_switch_with_outbox(room_id, actor_user_id, media_id, playlist_id, target, None)
            .await
    }

    pub async fn admin_switch_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(PlaybackSwitchCommand {
            room_id,
            actor_user_id,
            target: SwitchPlaybackTarget {
                media_id,
                playlist_id,
                target,
            },
            bypass_room_permissions: true,
            outbox_event_factory,
        })
        .await
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
        let enabled = settings.auto_play.value.enabled;
        let mode = settings.auto_play.value.mode;

        if !enabled {
            return Ok(None);
        }

        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "play_next failed after maximum retry attempts",
            || async {
                // Get current state (fresh on every retry)
                let state = match self.playback_repo.get(room_id).await? {
                    Some(s) => s,
                    None => self.playback_repo.create_or_get(room_id).await?,
                };

                let next_target = if let Some(ref playlist_id) = state.playing_playlist_id {
                    let playlist = self
                        .media_service
                        .get_room_playlist(room_id, playlist_id)
                        .await?
                        .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
                    match self
                        .ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                        .await
                    {
                        Ok(()) => {}
                        Err(Error::Authorization(_)) => {
                            return self
                                .stop_playback_for_unavailable_creator(room_id, "playlist")
                                .await;
                        }
                        Err(error) => return Err(error),
                    }
                    let Some(current_target) = state.target.as_ref() else {
                        return Ok(None);
                    };
                    self.media_service
                        .next_dynamic_playlist_item(room_id, playlist_id, current_target, mode)
                        .await
                        .and_then(|item| {
                            item.map(|item| {
                                Ok(NextTarget::Dynamic {
                                    playlist_id: playlist.id,
                                    target: item.target,
                                })
                            })
                            .transpose()
                        })?
                } else {
                    let playlist = if let Some(ref current_id) = state.playing_media_id {
                        let current_media = self
                            .media_service
                            .get_room_media(room_id, current_id)
                            .await?
                            .ok_or_else(|| Error::NotFound("Current media not found".to_string()))?;

                        match self
                            .ensure_creator_is_active(current_media.creator_id.as_ref(), "Media")
                            .await
                        {
                            Ok(()) => {}
                            Err(Error::Authorization(_)) => {
                                return self
                                    .stop_playback_for_unavailable_creator(room_id, "media")
                                    .await;
                            }
                            Err(error) => return Err(error),
                        }

                        if let Some(ref playlist_id) = current_media.playlist_id {
                            self.media_service
                                .get_room_playlist_media(room_id, playlist_id)
                                .await?
                        } else {
                            self.media_service.get_room_root_media(room_id).await?
                        }
                    } else {
                        self.media_service.get_room_root_media(room_id).await?
                    };

                    if playlist.is_empty() {
                        return Ok(None);
                    }

                    let next_media = match mode {
                        PlayMode::Sequential => {
                            if let Some(ref current_id) = state.playing_media_id {
                                match playlist.iter().position(|m| &m.id == current_id) {
                                    Some(pos) if pos + 1 < playlist.len() => {
                                        Some(&playlist[pos + 1])
                                    }
                                    Some(_) => None,
                                    None => {
                                        tracing::warn!(
                                            room_id = %room_id,
                                            media_id = %current_id,
                                            "Sequential: current media no longer present, falling back to first available item"
                                        );
                                        playlist.first()
                                    }
                                }
                            } else {
                                playlist.first()
                            }
                        }
                        PlayMode::RepeatOne => {
                            if let Some(ref current_id) = state.playing_media_id {
                                playlist.iter().find(|m| &m.id == current_id).or_else(|| {
                                    tracing::warn!(
                                        room_id = %room_id,
                                        media_id = %current_id,
                                        "RepeatOne: current media no longer present, falling back to first available item"
                                    );
                                    playlist.first()
                                })
                            } else {
                                playlist.first()
                            }
                        }
                        PlayMode::RepeatAll => {
                            if let Some(ref current_id) = state.playing_media_id {
                                if let Some(pos) = playlist.iter().position(|m| &m.id == current_id)
                                {
                                    Some(&playlist[(pos + 1) % playlist.len()])
                                } else {
                                    tracing::warn!(
                                        room_id = %room_id,
                                        media_id = %current_id,
                                        "RepeatAll: current media no longer present, falling back to first available item"
                                    );
                                    playlist.first()
                                }
                            } else {
                                playlist.first()
                            }
                        }
                        PlayMode::Shuffle => {
                            if let Some(ref current_id) = state.playing_media_id {
                                let other_media = playlist
                                    .iter()
                                    .filter(|m| &m.id != current_id)
                                    .collect::<Vec<_>>();
                                if other_media.is_empty() {
                                    playlist.iter().find(|m| &m.id == current_id)
                                } else {
                                    other_media.into_iter().choose(&mut rand::rng())
                                }
                            } else {
                                playlist.first()
                            }
                        }
                    };

                    next_media.cloned().map(NextTarget::Static)
                };

                let Some(next_target) = next_target else {
                    let observed_version = state.version;
                    let mut ended_state = state;
                    let ended_position = ended_state.computed_position();
                    ended_state.position = ended_position;
                    ended_state.is_playing = false;
                    ended_state.updated_at = self.clock.now();

                    let saved_state = self
                        .persist_playback_state_update_with_previous_progress(
                            &ended_state,
                            observed_version,
                            None,
                            None,
                        )
                        .await?;
                    self.write_playback_cache(&saved_state).await;

                    self.broadcast_invalidation(room_id, &saved_state, "play_next_ended")
                        .await;

                    tracing::info!(
                        room_id = %room_id,
                        mode = ?mode,
                        position = ended_position,
                        "Playlist ended"
                    );
                    return Ok(Some(saved_state));
                };

                // Apply update to the fetched state and try to save with optimistic locking
                let mut updated_state = state;
                let previous_state = updated_state.clone();
                match &next_target {
                    NextTarget::Static(next) => {
                        match self
                            .ensure_creator_is_active(next.creator_id.as_ref(), "Media")
                            .await
                        {
                            Ok(()) => {}
                            Err(Error::Authorization(_)) => {
                                return self
                                    .stop_playback_for_unavailable_creator(room_id, "media")
                                    .await;
                            }
                            Err(error) => return Err(error),
                        }
                        updated_state.playing_media_id = Some(next.id);
                        updated_state.playing_playlist_id = None;
                        updated_state.target = None;
                    }
                    NextTarget::Dynamic {
                        playlist_id,
                        target,
                    } => {
                        let playlist = self
                            .media_service
                            .get_room_playlist(room_id, playlist_id)
                            .await?
                            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
                        match self
                            .ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                            .await
                        {
                            Ok(()) => {}
                            Err(Error::Authorization(_)) => {
                                return self
                                    .stop_playback_for_unavailable_creator(room_id, "playlist")
                                    .await;
                            }
                            Err(error) => return Err(error),
                        }
                        updated_state.playing_media_id = None;
                        updated_state.playing_playlist_id = Some(*playlist_id);
                        updated_state.target = Some(target.clone());
                    }
                }
                let observed_version = updated_state.version;
                updated_state.position = 0.0;
                updated_state.is_playing = true;
                updated_state.updated_at = self.clock.now();
                let previous_progress_position =
                    previous_progress_position_for_source_transition(&previous_state, &updated_state);

                let saved_state = self
                    .persist_playback_state_update_with_previous_progress(
                        &updated_state,
                        observed_version,
                        previous_progress_position,
                        None,
                    )
                    .await?;
                self.write_playback_cache(&saved_state).await;

                self.broadcast_invalidation(room_id, &saved_state, "play_next")
                    .await;

                tracing::info!(
                    room_id = %room_id,
                    target = ?next_target,
                    mode = ?mode,
                    "Auto-played next media"
                );

                Ok(Some(saved_state))
            },
        )
        .await
    }

    /// Check if the current backend-known source duration has elapsed and advance.
    pub async fn check_and_auto_play(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        _position: f64,
    ) -> Result<Option<RoomPlaybackState>> {
        let enabled = settings.auto_play.value.enabled;

        if !enabled {
            return Ok(None);
        }

        let state = self.get_state(room_id).await?;
        self.auto_advance_state_if_due(&state, settings).await
    }

    async fn auto_advance_state_if_due(
        &self,
        state: &RoomPlaybackState,
        settings: &RoomSettings,
    ) -> Result<Option<RoomPlaybackState>> {
        if !settings.auto_play.value.enabled || !state.is_playing {
            return Ok(None);
        }

        let Some(identity) = PlaybackSourceIdentity::from_state(state)? else {
            return Ok(None);
        };
        let Some(metadata) = self.source_metadata_repo.get(&identity).await? else {
            return Ok(None);
        };
        let Some(duration_seconds) = metadata.duration_seconds else {
            return Ok(None);
        };

        if state.computed_position() >= duration_seconds - 1.0 {
            self.play_next(&state.room_id, settings).await
        } else {
            Ok(None)
        }
    }

    pub async fn auto_advance_due_sources_for_rooms(
        &self,
        settings_repo: &crate::repository::RoomSettingsRepository,
        room_ids: &[RoomId],
        limit: i64,
    ) -> Result<usize> {
        // The caller passes rooms active on this process. In a cluster the same
        // room can be active on several nodes, so duplicate scans are expected.
        // Playback state updates below still go through transactional optimistic
        // locking, which is the cross-node guard against advancing twice.
        if room_ids.is_empty() {
            return Ok(0);
        }

        let candidates = self
            .source_metadata_repo
            .list_active_finite_sources_for_rooms(room_ids, limit)
            .await?;
        let mut advanced = 0_usize;

        for (_metadata, state) in candidates {
            let settings = settings_repo.get(&state.room_id).await?;
            if self
                .auto_advance_state_if_due(&state, &settings)
                .await?
                .is_some()
            {
                advanced += 1;
            }
        }

        Ok(advanced)
    }

    /// Maximum retry attempts for optimistic lock conflicts.
    ///
    /// Playback writes are bursty and user-driven, so this budget is slightly
    /// higher than the generic default to absorb short-lived write storms
    /// without leaking avoidable failures back to callers.
    const MAX_RETRIES: u32 = 5;
    /// Base delay for exponential backoff (milliseconds)
    const BACKOFF_BASE_MS: u64 = 5;
    const UPDATE_STATE_RETRY_EXHAUSTED: &str =
        "Playback state update failed after maximum retry attempts";

    /// Update playback state with generic update function.
    ///
    /// Uses optimistic locking with automatic retry on version conflicts.
    /// Retries use exponential backoff with jitter to avoid thundering herd.
    pub async fn update_state<F>(&self, room_id: RoomId, update_fn: F) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        self.update_state_with_outbox(room_id, update_fn, None)
            .await
    }

    pub async fn update_state_with_outbox<F>(
        &self,
        room_id: RoomId,
        update_fn: F,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState),
    {
        self.update_state_checked_with_outbox(
            room_id,
            |state| {
                update_fn(state);
                Ok(())
            },
            outbox_event_factory,
        )
        .await
    }

    async fn update_state_checked_with_outbox<F>(
        &self,
        room_id: RoomId,
        update_fn: F,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState>
    where
        F: Fn(&mut RoomPlaybackState) -> Result<()>,
    {
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let update_fn = &update_fn;
                let outbox_event_factory = outbox_event_factory.clone();
                async move {
                    let mut state = match self.playback_repo.get(&room_id).await? {
                        Some(s) => s,
                        None => self.playback_repo.create_or_get(&room_id).await?,
                    };

                    let observed_version = state.version;
                    let previous_state = state.clone();
                    update_fn(&mut state)?;
                    let previous_progress_position =
                        previous_progress_position_for_source_transition(&previous_state, &state);

                    let updated_state = self
                        .persist_playback_state_update_with_previous_progress(
                            &state,
                            observed_version,
                            previous_progress_position,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                    self.write_playback_cache(&updated_state).await;

                    self.broadcast_invalidation(&room_id, &updated_state, "update_state")
                        .await;

                    Ok(updated_state)
                }
            },
        )
        .await
    }

    /// Reset playback to initial state
    pub async fn reset(&self, room_id: RoomId, user_id: UserId) -> Result<RoomPlaybackState> {
        self.reset_with_outbox(room_id, user_id, None).await
    }

    pub async fn reset_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, user_id, false, outbox_event_factory)
            .await
    }

    /// Management-only playback reset that bypasses room membership-derived permissions.
    ///
    /// Callers must validate global admin/root identity before invoking this method.
    pub async fn admin_reset(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
    ) -> Result<RoomPlaybackState> {
        self.admin_reset_with_outbox(room_id, actor_user_id, None)
            .await
    }

    pub async fn admin_reset_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.reset_internal(room_id, actor_user_id, true, outbox_event_factory)
            .await
    }

    pub async fn broadcast_playback_reset_after_force_delete(
        &self,
        state: RoomPlaybackState,
    ) -> RoomPlaybackState {
        self.invalidate_playback_cache(&state.room_id).await;
        self.broadcast_invalidation(
            &state.room_id,
            &state,
            "broadcast_playback_reset_after_force_delete",
        )
        .await;
        state
    }

    pub async fn reset_playback_for_creator(
        &self,
        creator_id: &UserId,
    ) -> Result<Vec<RoomPlaybackState>> {
        self.reset_playback_for_creator_with_outbox(creator_id, None)
            .await
    }

    pub async fn reset_playback_for_creator_with_outbox(
        &self,
        creator_id: &UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Vec<RoomPlaybackState>> {
        let states = {
            let mut tx = self.playback_repo.pool().begin().await?;
            let impacted_states = self
                .playback_repo
                .find_playback_for_creator_with_executor(creator_id, &mut *tx)
                .await?;
            let mut reset_states = Vec::with_capacity(impacted_states.len());
            let mut reservations = Vec::with_capacity(impacted_states.len());

            let reset_result: Result<()> = async {
                for mut state in impacted_states {
                    let reservation = self
                        .begin_playback_write_from_db_version(&state.room_id, state.version)
                        .await?;
                    let reserved_version = reservation
                        .as_ref()
                        .map_or(state.version + 1, |reservation| reservation.version);
                    let room_id = state.room_id;
                    reservations.push((room_id, reservation));

                    let previous_state = state.clone();
                    state.playing_media_id = None;
                    state.playing_playlist_id = None;
                    state.target = None;
                    state.position = 0.0;
                    state.speed = 1.0;
                    state.is_playing = false;
                    state.updated_at = self.clock.now();
                    let previous_progress_position =
                        previous_progress_position_for_source_transition(&previous_state, &state);

                    let updated = self
                        .playback_repo
                        .update_with_exact_version_executor_and_previous_progress(
                            &state,
                            reserved_version,
                            previous_progress_position,
                            &mut tx,
                        )
                        .await?;
                    self.insert_playback_outbox_tx(
                        &mut tx,
                        &updated,
                        outbox_event_factory.as_ref(),
                    )
                    .await?;
                    reset_states.push(updated);
                }
                Ok(())
            }
            .await;

            if let Err(error) = reset_result {
                for (room_id, reservation) in &reservations {
                    self.abort_playback_write(room_id, reservation.as_ref())
                        .await;
                }
                return Err(error);
            }

            if let Err(error) = tx.commit().await {
                for (room_id, reservation) in &reservations {
                    self.abort_playback_write(room_id, reservation.as_ref())
                        .await;
                }
                return Err(error.into());
            }
            for (state, (_, reservation)) in reset_states.iter().zip(reservations.iter()) {
                self.finalize_committed_playback_write_best_effort(
                    &state.room_id,
                    reservation.as_ref(),
                    state.version,
                    "reset_playback_for_creator",
                )
                .await;
            }
            reset_states
        };

        for state in &states {
            self.write_playback_cache(state).await;
            self.broadcast_invalidation(&state.room_id, state, "reset_playback_for_creator")
                .await;
        }

        Ok(states)
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
    pub async fn get_position(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.computed_position())
    }

    async fn switch_internal(&self, command: PlaybackSwitchCommand) -> Result<RoomPlaybackState> {
        let PlaybackSwitchCommand {
            room_id,
            actor_user_id,
            target,
            bypass_room_permissions,
            outbox_event_factory,
        } = command;

        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &actor_user_id,
                    crate::models::RoomPermission::CHANGE_CURRENT_MEDIA,
                )
                .await?;
        }
        validate_switch_target(&target)?;

        if target.media_id.is_none() && target.playlist_id.is_none() {
            let state = self
                .update_state_with_outbox(
                    room_id,
                    |state| {
                        state.playing_media_id = None;
                        state.playing_playlist_id = None;
                        state.target = None;
                        state.position = 0.0;
                        state.speed = 1.0;
                        state.is_playing = false;
                        state.updated_at = self.clock.now();
                    },
                    outbox_event_factory,
                )
                .await?;

            return Ok(state);
        }

        if let Some(ref media_id) = target.media_id {
            let media = self
                .media_service
                .get_room_media(&room_id, media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            self.ensure_creator_is_active(media.creator_id.as_ref(), "Media")
                .await?;
        }

        if let Some(ref playlist_id) = target.playlist_id {
            let playlist = self
                .media_service
                .get_room_playlist(&room_id, playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            if !playlist.is_dynamic() {
                return Err(Error::InvalidInput(
                    "playlist_id playback target must reference a dynamic playlist".to_string(),
                ));
            }

            self.ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                .await?;

            let resolved = self
                .media_service
                .resolve_dynamic_playlist_item(
                    room_id,
                    actor_user_id,
                    playlist_id,
                    target.target.as_ref().ok_or_else(|| {
                        Error::InvalidInput(
                            "target is required for dynamic playlist playback".to_string(),
                        )
                    })?,
                )
                .await?;
            if resolved.is_none() {
                return Err(Error::NotFound(
                    "Dynamic playlist item not found".to_string(),
                ));
            }
        }

        let state = self
            .update_state_with_outbox(
                room_id,
                |state| {
                    state.playing_media_id.clone_from(&target.media_id);
                    state.playing_playlist_id.clone_from(&target.playlist_id);
                    state.target.clone_from(&target.target);
                    state.position = 0.0;
                    state.is_playing = true;
                    state.updated_at = self.clock.now();
                },
                outbox_event_factory,
            )
            .await?;

        Ok(state)
    }

    async fn reset_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::PLAY_CONTROL,
                )
                .await?;
        }

        let state = self
            .update_state_with_outbox(
                room_id,
                |state| {
                    state.is_playing = false;
                    state.position = 0.0;
                    state.speed = 1.0;
                    state.playing_media_id = None;
                    state.playing_playlist_id = None;
                    state.target = None;
                    state.updated_at = self.clock.now();
                },
                outbox_event_factory,
            )
            .await?;

        Ok(state)
    }

    async fn stop_playback_for_unavailable_creator(
        &self,
        room_id: &RoomId,
        resource_kind: &'static str,
    ) -> Result<Option<RoomPlaybackState>> {
        tracing::warn!(
            room_id = %room_id,
            resource_kind,
            "Stopping playback because the target creator is not active"
        );

        let state = self
            .update_state(*room_id, |state| {
                state.playing_media_id = None;
                state.playing_playlist_id = None;
                state.target = None;
                state.position = 0.0;
                state.speed = 1.0;
                state.is_playing = false;
                state.updated_at = self.clock.now();
            })
            .await?;

        Ok(Some(state))
    }

    /// Get current playback speed
    pub async fn get_speed(&self, room_id: &RoomId) -> Result<f64> {
        let state = self.get_state(room_id).await?;
        Ok(state.speed)
    }

    pub async fn update_playback_state(
        &self,
        request: PlaybackStateUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        self.update_playback_state_internal(request, false).await
    }

    /// Management-only multi-field playback state update. Callers must validate
    /// global admin/root identity before this bypasses room membership checks.
    pub async fn admin_update_playback_state(
        &self,
        request: PlaybackStateUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        self.update_playback_state_internal(request, true).await
    }

    async fn update_playback_state_internal(
        &self,
        request: PlaybackStateUpdateRequest,
        bypass_permission: bool,
    ) -> Result<RoomPlaybackState> {
        let PlaybackStateUpdateRequest {
            room_id,
            actor_user_id,
            patch,
            expected_version,
            expected_source,
            outbox_event_factory,
        } = request;
        let PlaybackStatePatch {
            playing,
            position,
            speed,
        } = patch;

        // Check permissions based on what's being updated
        let mut required_permissions = Vec::new();
        if playing.is_some() {
            required_permissions.push(crate::models::RoomPermission::PLAY_CONTROL);
        }
        if position.is_some() {
            required_permissions.push(crate::models::RoomPermission::PLAY_CONTROL);
        }
        if speed.is_some() {
            required_permissions.push(crate::models::RoomPermission::CHANGE_PLAYBACK_RATE);
        }
        if !required_permissions.is_empty() && !bypass_permission {
            self.permission_service
                .check_permissions(&room_id, &actor_user_id, &required_permissions)
                .await?;
        }

        if let Some(ct) = position {
            validate_seek_position(ct)?;
        }

        if let Some(s) = speed {
            validate_playback_speed_value(s)?;
        }

        let apply_update = |state: &mut RoomPlaybackState| {
            // Snapshot the computed playback position before changing is_playing
            // or speed, just like set_playing() and change_speed() do individually.
            // Without this, pausing or changing speed via update_multiple would
            // store the wrong position.
            let needs_snapshot = matches!(playing, Some(false)) || speed.is_some();
            if needs_snapshot && position.is_none() {
                // Only snapshot if the caller didn't provide an explicit position
                state.position = state.computed_position();
            }

            if let Some(p) = playing {
                state.is_playing = p;
            }
            if let Some(ct) = position {
                state.position = ct;
            }
            if let Some(s) = speed {
                state.speed = s;
            }
            state.updated_at = self.clock.now();
            // version is incremented by the SQL UPDATE, not here
        };

        let state = if expected_version.is_some() || expected_source.is_some() {
            let mut state = match self.playback_repo.get(&room_id).await? {
                Some(state) => state,
                None => self.playback_repo.create_or_get(&room_id).await?,
            };

            if position.is_some() {
                validate_position_update_source(&state)?;
                self.reject_position_update_for_live_source(&state).await?;
            }
            if expected_version.is_some_and(|expected| state.version != expected) {
                return Err(Error::OptimisticLockConflict);
            }
            if expected_source
                .as_ref()
                .map(|expected| expected.matches(&state).map(|matches| !matches))
                .transpose()?
                .unwrap_or(false)
            {
                return Err(Error::OptimisticLockConflict);
            }

            let observed_version = state.version;
            apply_update(&mut state);
            let updated_state = self
                .persist_playback_state_update_with_previous_progress(
                    &state,
                    observed_version,
                    None,
                    outbox_event_factory.as_ref(),
                )
                .await?;
            self.write_playback_cache(&updated_state).await;

            self.broadcast_invalidation(&room_id, &updated_state, "update_state")
                .await;

            updated_state
        } else if position.is_some() {
            crate::service::optimistic_retry::retry_with_optimistic_lock(
                Self::MAX_RETRIES,
                Self::BACKOFF_BASE_MS,
                Self::UPDATE_STATE_RETRY_EXHAUSTED,
                || {
                    let outbox_event_factory = outbox_event_factory.clone();
                    let apply_update = &apply_update;
                    async move {
                        let mut state = match self.playback_repo.get(&room_id).await? {
                            Some(state) => state,
                            None => self.playback_repo.create_or_get(&room_id).await?,
                        };
                        validate_position_update_source(&state)?;
                        self.reject_position_update_for_live_source(&state).await?;

                        let observed_version = state.version;
                        let previous_state = state.clone();
                        apply_update(&mut state);
                        let previous_progress_position =
                            previous_progress_position_for_source_transition(
                                &previous_state,
                                &state,
                            );

                        let updated_state = self
                            .persist_playback_state_update_with_previous_progress(
                                &state,
                                observed_version,
                                previous_progress_position,
                                outbox_event_factory.as_ref(),
                            )
                            .await?;
                        self.write_playback_cache(&updated_state).await;

                        self.broadcast_invalidation(&room_id, &updated_state, "update_state")
                            .await;

                        Ok(updated_state)
                    }
                },
            )
            .await?
        } else {
            self.update_state_with_outbox(room_id, apply_update, outbox_event_factory)
                .await?
        };

        Ok(state)
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
