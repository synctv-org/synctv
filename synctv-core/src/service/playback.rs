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
        BilibiliMediaSourceConfig, ChatMessage, ChatMessageType, ChatMetadata,
        ChatPlaybackChangedMetadata, ChatPlaybackMetadata, MediaId, PlayMode, PlaybackChangeReason,
        PlaybackHistoryEntry, PlaybackHistoryPage, PlaybackSourceIdentity, PlaybackSourceMetadata,
        PlaylistId, ProviderTarget, RealtimeEvent, RoomId, RoomPlaybackState, RoomSettings,
        SourceProvider, TwitchTargetKind, UserId,
    },
    repository::{
        chat::InsertChatMessageEvent,
        realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
        AppendPlaybackHistoryEntry, ChatRepository, PlaybackHistoryDirection,
        PlaybackHistoryRepository, PlaybackSourceMetadataRepository, RoomPlaybackStateRepository,
    },
    service::{
        media::BackendPlaybackRequest, media::MediaService, NotificationService, PermissionService,
        UserService,
    },
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
    history_repo: PlaybackHistoryRepository,
    chat_repo: ChatRepository,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    notification_service: Option<NotificationService>,
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
    pub notification_service: Option<NotificationService>,
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
            notification_service: None,
        }
    }
}

struct PlaybackSwitchCommand {
    room_id: RoomId,
    actor_user_id: UserId,
    recorded_actor_user_id: Option<UserId>,
    target: SwitchPlaybackTarget,
    bypass_room_permissions: bool,
    outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
}

enum PlaybackHistoryTransition {
    AppendEntry {
        selected_by_user_id: Option<UserId>,
        names: Option<PlaybackSourceNames>,
    },
    SelectEntry(PlaybackHistoryEntry),
}

#[derive(Debug, Clone, Default)]
struct PlaybackSourceNames {
    media_name: Option<String>,
    playlist_name: Option<String>,
}

#[derive(Debug, Clone)]
struct PreflightMetadata {
    identity: PlaybackSourceIdentity,
    is_live: bool,
    duration_seconds: Option<f64>,
    media_name: Option<String>,
    playlist_name: Option<String>,
}

fn live_status_for_target(target: &ProviderTarget) -> Option<bool> {
    match target {
        ProviderTarget::Bilibili(crate::models::BilibiliTarget::Live { .. })
        | ProviderTarget::Twitch(crate::models::TwitchTarget {
            kind: TwitchTargetKind::Live,
            ..
        }) => Some(true),
        ProviderTarget::Bilibili(
            crate::models::BilibiliTarget::Video { .. }
            | crate::models::BilibiliTarget::VideoPart { .. }
            | crate::models::BilibiliTarget::PgcEpisode { .. },
        )
        | ProviderTarget::Twitch(crate::models::TwitchTarget {
            kind: TwitchTargetKind::Video | TwitchTargetKind::Clip,
            ..
        })
        | ProviderTarget::Alist(_)
        | ProviderTarget::Emby(_)
        | ProviderTarget::Cloudreve(_)
        | ProviderTarget::Fnos(_)
        | ProviderTarget::Qnap(_)
        | ProviderTarget::Synology(_)
        | ProviderTarget::Nextcloud(_)
        | ProviderTarget::Seafile(_)
        | ProviderTarget::TrueNas(_) => Some(false),
        ProviderTarget::Youtube(_) | ProviderTarget::Douyin(_) | ProviderTarget::TikTok(_) => None,
    }
}

fn normalized_provider_duration(is_live: bool, duration_seconds: Option<f64>) -> Option<f64> {
    (!is_live)
        .then_some(duration_seconds)
        .flatten()
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}

fn preflight_can_defer_generation(error: &Error) -> bool {
    matches!(
        error,
        Error::ServiceUnavailable(_) | Error::Timeout(_) | Error::RateLimited(_)
    )
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
    if source_config.provider() != provider {
        return None;
    }
    match source_config {
        crate::models::PlaylistSourceConfig::Youtube(
            crate::models::YoutubePlaylistSourceConfig::Channel { content, .. },
        ) => Some(matches!(
            content,
            crate::models::YoutubeChannelContent::Live
        )),
        crate::models::PlaylistSourceConfig::Douyin(_)
        | crate::models::PlaylistSourceConfig::TikTok(_)
        | crate::models::PlaylistSourceConfig::Alist(_)
        | crate::models::PlaylistSourceConfig::Emby(_)
        | crate::models::PlaylistSourceConfig::Cloudreve(_)
        | crate::models::PlaylistSourceConfig::Fnos(_)
        | crate::models::PlaylistSourceConfig::Qnap(_)
        | crate::models::PlaylistSourceConfig::Synology(_)
        | crate::models::PlaylistSourceConfig::Nextcloud(_)
        | crate::models::PlaylistSourceConfig::Seafile(_)
        | crate::models::PlaylistSourceConfig::TrueNas(_) => Some(false),
        crate::models::PlaylistSourceConfig::Bilibili(_)
        | crate::models::PlaylistSourceConfig::Twitch(_)
        | crate::models::PlaylistSourceConfig::Youtube(_) => None,
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
    const MAX_CLIENT_PLAYBACK_CLOCK_SKEW_MILLIS: i64 = 30_000;

    fn client_elapsed_seconds(
        received_at: chrono::DateTime<chrono::Utc>,
        client_time_millis: Option<i64>,
    ) -> Result<f64> {
        let Some(client_millis) = client_time_millis else {
            return Ok(0.0);
        };
        let skew_millis = received_at.timestamp_millis().saturating_sub(client_millis);
        if skew_millis.unsigned_abs() > Self::MAX_CLIENT_PLAYBACK_CLOCK_SKEW_MILLIS.unsigned_abs() {
            return Err(Error::InvalidInput(format!(
                "client playback timestamp differs from server time by {skew_millis}ms"
            )));
        }
        Ok(Duration::from_millis(skew_millis.max(0).unsigned_abs()).as_secs_f64())
    }

    fn compensate_client_position(
        position: f64,
        playing: bool,
        speed: f64,
        elapsed_seconds: f64,
    ) -> f64 {
        if playing {
            position + elapsed_seconds * speed
        } else {
            position
        }
    }

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
            history_repo: PlaybackHistoryRepository::new(playback_repo.pool().clone()),
            chat_repo: ChatRepository::new(playback_repo.pool().clone()),
            playback_repo,
            source_metadata_repo,
            realtime_outbox: runtime.realtime_outbox,
            notification_service: runtime.notification_service,
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

    async fn ensure_history_entry_available(&self, entry: &PlaybackHistoryEntry) -> Result<()> {
        if let Some(media_id) = entry.media_id {
            let media = self
                .media_service
                .get_room_media(&entry.room_id, &media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playback history media not found".to_string()))?;
            self.ensure_creator_is_active(media.creator_id.as_ref(), "Media")
                .await?;

            if let Some(playlist_id) = entry.playlist_id {
                let playlist = self
                    .media_service
                    .get_room_playlist(&entry.room_id, &playlist_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("Playback history playlist not found".to_string())
                    })?;
                if playlist.is_dynamic() || media.playlist_id != Some(playlist_id) {
                    return Err(Error::InvalidInput(
                        "Playback history media does not belong to its static playlist".to_string(),
                    ));
                }
                self.ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                    .await?;
            }

            return Ok(());
        }
        if let Some(playlist_id) = entry.playlist_id {
            let playlist = self
                .media_service
                .get_room_playlist(&entry.room_id, &playlist_id)
                .await?
                .ok_or_else(|| {
                    Error::NotFound("Playback history playlist not found".to_string())
                })?;
            return self
                .ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                .await;
        }
        Err(Error::Internal(
            "Playback history entry has no source".to_string(),
        ))
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
        media_name: Option<&str>,
        playlist_name: Option<&str>,
    ) -> Result<PlaybackSourceMetadata> {
        self.source_metadata_repo
            .upsert_provider_source_metadata(
                identity,
                is_live,
                duration_seconds,
                media_name,
                playlist_name,
            )
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

    pub async fn update_playback_source_metadata_names(
        &self,
        identity: &PlaybackSourceIdentity,
        media_name: Option<&str>,
        playlist_name: Option<&str>,
    ) -> Result<()> {
        self.source_metadata_repo
            .update_names_if_present(identity, media_name, playlist_name)
            .await
    }

    pub async fn source_live_status_for_state(
        &self,
        state: &RoomPlaybackState,
    ) -> Result<Option<bool>> {
        if let Some(status) = state.target.as_ref().and_then(live_status_for_target) {
            return Ok(Some(status));
        }

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
            if let Some(position) = previous_progress_position {
                self.history_repo
                    .save_cursor_position_on_conn(&state.room_id, position, &mut tx)
                    .await?;
            }
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

    pub async fn chat_playback_metadata_for_state(
        &self,
        state: &RoomPlaybackState,
        position: f64,
    ) -> Result<Option<ChatPlaybackMetadata>> {
        let Some(identity) = PlaybackSourceIdentity::from_state(state)? else {
            return Ok(None);
        };
        let source_metadata = self.source_metadata_repo.get(&identity).await?;
        let is_live = match source_metadata
            .as_ref()
            .and_then(|metadata| metadata.is_live)
        {
            Some(is_live) => Some(is_live),
            None => self.source_live_status_for_state(state).await?,
        };
        let duration_seconds = source_metadata
            .as_ref()
            .and_then(|metadata| metadata.duration_seconds);
        let names = source_metadata
            .as_ref()
            .map(|metadata| PlaybackSourceNames {
                media_name: metadata.media_name.clone(),
                playlist_name: metadata.playlist_name.clone(),
            });

        Ok(Some(ChatPlaybackMetadata {
            media_id: state.playing_media_id,
            playlist_id: state.playing_playlist_id,
            target: state.target.clone(),
            target_hash: None,
            position_seconds: ChatPlaybackMetadata::position_for_source(
                position,
                is_live,
                duration_seconds,
            ),
            media_name: names.as_ref().and_then(|names| names.media_name.clone()),
            playlist_name: names.and_then(|names| names.playlist_name),
        }))
    }

    async fn insert_playback_changed_chat_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        previous: &RoomPlaybackState,
        current: &RoomPlaybackState,
        metadata: ChatPlaybackChangedMetadata,
    ) -> Result<Option<RealtimeEvent>> {
        if previous.playing_media_id == current.playing_media_id
            && previous.playing_playlist_id == current.playing_playlist_id
            && previous.target == current.target
        {
            return Ok(None);
        }
        let Some(()) = current
            .playing_media_id
            .map(|_| ())
            .or_else(|| current.playing_playlist_id.map(|_| ()))
        else {
            return Ok(None);
        };

        let event_actor = if let Some(actor) = metadata.actor_user_id {
            actor
        } else {
            sqlx::query_scalar::<_, UserId>("SELECT created_by FROM rooms WHERE id = $1")
                .bind(current.room_id.as_i64())
                .fetch_one(&mut **tx)
                .await?
        };
        let destination_name = metadata
            .to
            .media_name
            .as_deref()
            .or(metadata.to.playlist_name.as_deref())
            .unwrap_or("Unknown media");
        let mut message = ChatMessage::new(
            current.room_id,
            event_actor,
            format!("Playback changed to {destination_name}"),
        );
        message.user_id = None;
        message.message_type = ChatMessageType::SystemPlaybackChanged;
        message.metadata = Some(ChatMetadata::PlaybackChanged(metadata));
        let occurred_at = self.clock.now();
        message.created_at = occurred_at;
        let event_id = synctv_common::snanoid!(16);
        let logged = self
            .chat_repo
            .insert_message_event_in_tx(
                tx,
                InsertChatMessageEvent {
                    message: &message,
                    attachments: &[],
                    mentions: &[],
                    actor_user_id: event_actor,
                    event_id: &event_id,
                    occurred_at,
                },
            )
            .await?;
        let event = RealtimeEvent::ChatMessageEvent {
            event_id: logged.event.event_id.clone(),
            room_id: current.room_id,
            actor_user_id: event_actor,
            event: logged.event,
            timestamp: occurred_at,
        };
        if let Some(outbox) = &self.realtime_outbox {
            outbox
                .insert_with_executor(
                    &NewRealtimeOutboxEvent {
                        id: event.event_id().to_string(),
                        enqueue_outbox: true,
                        aggregate_type: "room".to_string(),
                        aggregate_id: current.room_id.to_string(),
                        event_type: event.event_type().to_string(),
                        event_version: 1,
                        aggregate_version: None,
                        payload: event.clone(),
                    },
                    &mut **tx,
                )
                .await?;
        }
        Ok(Some(event))
    }

    async fn resolve_playback_source_names(
        &self,
        state: &RoomPlaybackState,
    ) -> Result<PlaybackSourceNames> {
        if let Some(media_id) = state.playing_media_id {
            let Some(media) = self
                .media_service
                .get_room_media(&state.room_id, &media_id)
                .await?
            else {
                return Ok(PlaybackSourceNames::default());
            };
            let playlist_name = match media.playlist_id {
                Some(playlist_id) => self
                    .media_service
                    .get_room_playlist(&state.room_id, &playlist_id)
                    .await?
                    .map(|playlist| playlist.name),
                None => None,
            };
            return Ok(PlaybackSourceNames {
                media_name: Some(media.name),
                playlist_name,
            });
        }
        if let Some(playlist_id) = state.playing_playlist_id {
            let playlist_name = self
                .media_service
                .get_room_playlist(&state.room_id, &playlist_id)
                .await?
                .map(|playlist| playlist.name);
            return Ok(PlaybackSourceNames {
                media_name: None,
                playlist_name,
            });
        }
        Ok(PlaybackSourceNames::default())
    }

    async fn persist_source_transition(
        &self,
        state: &RoomPlaybackState,
        previous: &RoomPlaybackState,
        history_transition: PlaybackHistoryTransition,
        reason: PlaybackChangeReason,
        actor_user_id: Option<UserId>,
        outbox_event_factory: Option<&RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        let names = match &history_transition {
            PlaybackHistoryTransition::SelectEntry(entry) => PlaybackSourceNames {
                media_name: entry.media_name.clone(),
                playlist_name: entry.playlist_name.clone(),
            },
            PlaybackHistoryTransition::AppendEntry { names, .. } => {
                if let Some(names) = names.clone() {
                    names
                } else {
                    self.resolve_playback_source_names(state).await?
                }
            }
        };
        let from_chat_metadata = self
            .chat_playback_metadata_for_state(previous, previous.computed_position())
            .await?;
        let mut to_chat_metadata = self
            .chat_playback_metadata_for_state(state, state.position)
            .await?
            .ok_or_else(|| {
                Error::Internal("playback transition has no target source".to_string())
            })?;
        to_chat_metadata.media_name = names.media_name.clone();
        to_chat_metadata.playlist_name = names.playlist_name.clone();
        let reservation = self
            .begin_playback_write_from_db_version(&state.room_id, previous.version)
            .await?;
        let new_version = reservation
            .as_ref()
            .map_or(previous.version + 1, |reservation| reservation.version);
        let mut tx = self.playback_repo.pool().begin().await?;
        let result = async {
            self.history_repo
                .save_cursor_position_on_conn(&state.room_id, previous.computed_position(), &mut tx)
                .await?;
            let previous_progress_position =
                previous_progress_position_for_source_transition(previous, state);
            let mut updated = self
                .playback_repo
                .update_with_exact_version_executor_and_previous_progress(
                    state,
                    new_version,
                    previous_progress_position,
                    &mut tx,
                )
                .await?;
            match history_transition {
                PlaybackHistoryTransition::AppendEntry {
                    selected_by_user_id,
                    ..
                } => {
                    let entry = self
                        .history_repo
                        .append_entry_on_conn(
                            AppendPlaybackHistoryEntry {
                                room_id: &updated.room_id,
                                media_id: updated.playing_media_id,
                                playlist_id: updated.playing_playlist_id,
                                target: updated.target.as_ref(),
                                position_seconds: updated.position,
                                selected_by_user_id,
                                media_name: names.media_name.as_deref(),
                                playlist_name: names.playlist_name.as_deref(),
                            },
                            &mut tx,
                        )
                        .await?;
                    updated.history_cursor_id = Some(entry.id);
                }
                PlaybackHistoryTransition::SelectEntry(entry) => {
                    self.history_repo
                        .set_cursor_on_conn(&updated.room_id, &entry, &mut tx)
                        .await?;
                    updated.history_cursor_id = Some(entry.id);
                }
            }
            self.insert_playback_outbox_tx(&mut tx, &updated, outbox_event_factory)
                .await?;
            let chat_event = self
                .insert_playback_changed_chat_tx(
                    &mut tx,
                    previous,
                    &updated,
                    ChatPlaybackChangedMetadata {
                        from: from_chat_metadata,
                        to: to_chat_metadata,
                        reason,
                        actor_user_id,
                    },
                )
                .await?;
            Ok((updated, chat_event))
        }
        .await;
        let (updated, chat_event) = match result {
            Ok(result) => {
                tx.commit().await?;
                result
            }
            Err(error) => {
                let _ = tx.rollback().await;
                self.abort_playback_write(&state.room_id, reservation.as_ref())
                    .await;
                return Err(error);
            }
        };
        self.finalize_committed_playback_write_best_effort(
            &updated.room_id,
            reservation.as_ref(),
            updated.version,
            "persist_source_transition",
        )
        .await;
        if let (Some(notification_service), Some(chat_event)) =
            (&self.notification_service, chat_event)
        {
            let _ = notification_service.notify_committed_realtime_event(chat_event);
        }
        Ok(updated)
    }

    pub async fn list_playback_history(
        &self,
        room_id: &RoomId,
        before_entry_id: Option<i64>,
        limit: i32,
    ) -> Result<PlaybackHistoryPage> {
        self.history_repo
            .list(room_id, before_entry_id, limit)
            .await
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
                crate::models::RoomPermission::CONTROL_PLAYBACK_STATE,
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
                crate::models::RoomPermission::CONTROL_PLAYBACK_STATE,
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
            recorded_actor_user_id: Some(user_id),
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
        self.admin_switch_with_outbox(
            room_id,
            actor_user_id,
            Some(actor_user_id),
            SwitchPlaybackTarget {
                media_id,
                playlist_id,
                target,
            },
            None,
        )
        .await
    }

    pub async fn admin_switch_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        recorded_actor_user_id: Option<UserId>,
        target: SwitchPlaybackTarget,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.switch_internal(PlaybackSwitchCommand {
            room_id,
            actor_user_id,
            recorded_actor_user_id,
            target,
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
        self.play_next_internal(
            room_id,
            settings,
            None,
            true,
            PlaybackChangeReason::AutoAdvance,
            None,
        )
        .await
    }

    pub async fn play_next_for_user(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        settings: &RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Option<RoomPlaybackState>> {
        self.permission_service
            .check_permission(
                room_id,
                &user_id,
                crate::models::RoomPermission::NAVIGATE_PLAYBACK,
            )
            .await?;
        self.play_next_internal(
            room_id,
            settings,
            Some(user_id),
            false,
            PlaybackChangeReason::Next,
            outbox_event_factory,
        )
        .await
    }

    async fn play_next_internal(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        actor_user_id: Option<UserId>,
        require_auto_play_enabled: bool,
        reason: PlaybackChangeReason,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Option<RoomPlaybackState>> {
        let enabled = settings.auto_play.value.enabled;
        let mode = settings.auto_play.value.mode;

        if require_auto_play_enabled && !enabled {
            return Ok(None);
        }

        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            "play_next failed after maximum retry attempts",
            || {
                let outbox_event_factory = outbox_event_factory.clone();
                async move {
                // Get current state (fresh on every retry)
                let state = match self.playback_repo.get(room_id).await? {
                    Some(s) => s,
                    None => self.playback_repo.create_or_get(room_id).await?,
                };

                let history_cursor_entry = self.history_repo.cursor_entry(room_id).await?;
                if let Some(cursor_entry) = &history_cursor_entry {
                    if let Some(next) = self
                        .history_repo
                        .adjacent_entry(
                            room_id,
                            cursor_entry.id,
                            PlaybackHistoryDirection::Next,
                        )
                        .await?
                    {
                        self.ensure_history_entry_available(&next).await?;
                        let preflight_metadata = self
                            .preflight_history_entry(actor_user_id.as_ref(), room_id, &next)
                            .await?;
                        let previous = state.clone();
                        let mut updated = state;
                        updated.playing_media_id = next.media_id;
                        updated.playing_playlist_id = next.playlist_id;
                        updated.target = next.target.clone();
                        updated.position = next.position_seconds;
                        updated.is_playing = true;
                        updated.updated_at = self.clock.now();
                        self.persist_preflight_metadata(preflight_metadata.as_ref())
                            .await?;
                        let saved = self
                            .persist_source_transition(
                                &updated,
                                &previous,
                                PlaybackHistoryTransition::SelectEntry(next),
                                reason,
                                actor_user_id,
                                outbox_event_factory.as_ref(),
                            )
                            .await?;
                        self.write_playback_cache(&saved).await;
                        self.broadcast_invalidation(room_id, &saved, "play_next_history")
                            .await;
                        return Ok(Some(saved));
                    }
                }

                let selection_state = if state.playing_media_id.is_none()
                    && state.playing_playlist_id.is_none()
                {
                    history_cursor_entry.as_ref().map_or_else(
                        || state.clone(),
                        |entry| {
                            let mut selection = state.clone();
                            selection.playing_media_id = entry.media_id;
                            selection.playing_playlist_id = entry.playlist_id;
                            selection.target.clone_from(&entry.target);
                            selection.position = entry.position_seconds;
                            selection
                        },
                    )
                } else {
                    state.clone()
                };

                let next_target = if let (None, Some(playlist_id)) = (
                    selection_state.playing_media_id,
                    selection_state.playing_playlist_id,
                ) {
                    let playlist = self
                        .media_service
                        .get_room_playlist(room_id, &playlist_id)
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
                    let Some(current_target) = selection_state.target.as_ref() else {
                        return Ok(None);
                    };
                    self.media_service
                        .next_dynamic_playlist_item(room_id, &playlist_id, current_target, mode)
                        .await
                        .and_then(|item| {
                            item.map(|item| {
                                Ok(NextTarget::Dynamic {
                                    playlist_id: playlist.id,
                                    media_name: item.name,
                                    source_config: item.source_config,
                                    target: item.target,
                                })
                            })
                            .transpose()
                        })?
                } else {
                    let playlist = if let Some(ref current_id) = selection_state.playing_media_id {
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
                            if let Some(ref current_id) = selection_state.playing_media_id {
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
                            if let Some(ref current_id) = selection_state.playing_media_id {
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
                            if let Some(ref current_id) = selection_state.playing_media_id {
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
                            if let Some(ref current_id) = selection_state.playing_media_id {
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
                    if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
                        return Ok(None);
                    }
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
                            Some(ended_position),
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
                let transition_names = match &next_target {
                    NextTarget::Static(_) => None,
                    NextTarget::Dynamic { media_name, .. } => {
                        let playlist_id = match &next_target {
                            NextTarget::Dynamic { playlist_id, .. } => playlist_id,
                            NextTarget::Static(_) => unreachable!(),
                        };
                        let playlist = self.media_service.get_room_playlist(room_id, playlist_id).await?
                            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
                        Some(PlaybackSourceNames {
                            media_name: Some(media_name.clone()),
                            playlist_name: Some(playlist.name),
                        })
                    }
                };
                let preflight_metadata = match &next_target {
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
                        let metadata = self
                            .preflight_static_media(
                                next.creator_id.as_ref().or(actor_user_id.as_ref()),
                                next,
                            )
                            .await?;
                        updated_state.playing_media_id = Some(next.id);
                        updated_state.playing_playlist_id = next.playlist_id;
                        updated_state.target = None;
                        metadata
                    }
                    NextTarget::Dynamic {
                        playlist_id,
                        source_config,
                        media_name,
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
                        let metadata = self.preflight_dynamic_playlist_item(
                            playlist.creator_id.as_ref().or(actor_user_id.as_ref()),
                            &playlist,
                            media_name,
                            source_config,
                            target,
                        )
                        .await?;
                        updated_state.playing_media_id = None;
                        updated_state.playing_playlist_id = Some(*playlist_id);
                        updated_state.target = Some(target.clone());
                        metadata
                    }
                };
                updated_state.position = 0.0;
                updated_state.is_playing = true;
                updated_state.updated_at = self.clock.now();
                self.persist_preflight_metadata(preflight_metadata.as_ref())
                    .await?;
                let saved_state = self
                    .persist_source_transition(
                        &updated_state,
                        &previous_state,
                        PlaybackHistoryTransition::AppendEntry {
                            selected_by_user_id: actor_user_id,
                            names: transition_names,
                        },
                        reason,
                        actor_user_id,
                        outbox_event_factory.as_ref(),
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
            }
            },
        )
        .await
    }

    async fn play_history_entry_internal(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        entry_id: i64,
        reason: PlaybackChangeReason,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.permission_service
            .check_permission(
                room_id,
                &user_id,
                crate::models::RoomPermission::NAVIGATE_PLAYBACK,
            )
            .await?;
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let outbox_event_factory = outbox_event_factory.clone();
                async move {
                    let mut conn = self.playback_repo.pool().acquire().await?;
                    let entry = self
                        .history_repo
                        .get_on_conn(room_id, entry_id, &mut conn)
                        .await?;
                    drop(conn);
                    self.ensure_history_entry_available(&entry).await?;
                    let preflight_metadata = self
                        .preflight_history_entry(Some(&user_id), room_id, &entry)
                        .await?;
                    let previous = match self.playback_repo.get(room_id).await? {
                        Some(state) => state,
                        None => self.playback_repo.create_or_get(room_id).await?,
                    };
                    let mut state = previous.clone();
                    state.playing_media_id = entry.media_id;
                    state.playing_playlist_id = entry.playlist_id;
                    state.target.clone_from(&entry.target);
                    state.position = entry.position_seconds;
                    state.is_playing = true;
                    state.updated_at = self.clock.now();
                    self.persist_preflight_metadata(preflight_metadata.as_ref())
                        .await?;
                    let saved = self
                        .persist_source_transition(
                            &state,
                            &previous,
                            PlaybackHistoryTransition::SelectEntry(entry),
                            reason,
                            Some(user_id),
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                    self.write_playback_cache(&saved).await;
                    self.broadcast_invalidation(room_id, &saved, "play_history_entry")
                        .await;
                    Ok(saved)
                }
            },
        )
        .await
    }

    pub async fn play_previous_for_user(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Option<RoomPlaybackState>> {
        self.permission_service
            .check_permission(
                room_id,
                &user_id,
                crate::models::RoomPermission::NAVIGATE_PLAYBACK,
            )
            .await?;
        crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let outbox_event_factory = outbox_event_factory.clone();
                async move {
                    let state = match self.playback_repo.get(room_id).await? {
                        Some(state) => state,
                        None => self.playback_repo.create_or_get(room_id).await?,
                    };
                    let Some(cursor_entry) = self.history_repo.cursor_entry(room_id).await? else {
                        return Ok(None);
                    };
                    let entry = if state.playing_media_id.is_none()
                        && state.playing_playlist_id.is_none()
                    {
                        cursor_entry
                    } else {
                        let Some(previous) = self
                            .history_repo
                            .adjacent_entry(
                                room_id,
                                cursor_entry.id,
                                PlaybackHistoryDirection::Previous,
                            )
                            .await?
                        else {
                            return Ok(None);
                        };
                        previous
                    };
                    self.ensure_history_entry_available(&entry).await?;
                    let preflight_metadata = self
                        .preflight_history_entry(Some(&user_id), room_id, &entry)
                        .await?;
                    let previous = state.clone();
                    let mut updated = state;
                    updated.playing_media_id = entry.media_id;
                    updated.playing_playlist_id = entry.playlist_id;
                    updated.target.clone_from(&entry.target);
                    updated.position = 0.0;
                    updated.is_playing = true;
                    updated.updated_at = self.clock.now();
                    self.persist_preflight_metadata(preflight_metadata.as_ref())
                        .await?;
                    let saved = self
                        .persist_source_transition(
                            &updated,
                            &previous,
                            PlaybackHistoryTransition::SelectEntry(entry),
                            PlaybackChangeReason::Previous,
                            Some(user_id),
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                    self.write_playback_cache(&saved).await;
                    self.broadcast_invalidation(room_id, &saved, "play_previous")
                        .await;
                    Ok(Some(saved))
                }
            },
        )
        .await
    }

    pub async fn play_history_entry_for_user(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        entry_id: i64,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        if entry_id <= 0 {
            return Err(Error::InvalidInput(
                "entry_id must be a positive integer".to_string(),
            ));
        }
        self.permission_service
            .check_permission(
                room_id,
                &user_id,
                crate::models::RoomPermission::VIEW_PLAYBACK_HISTORY,
            )
            .await?;
        self.play_history_entry_internal(
            room_id,
            user_id,
            entry_id,
            PlaybackChangeReason::HistoryEntry,
            outbox_event_factory,
        )
        .await
    }

    /// Check if the current backend-known source duration has elapsed and advance.
    pub async fn check_and_auto_play(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        position: f64,
    ) -> Result<Option<RoomPlaybackState>> {
        self.check_and_auto_play_with_outbox(room_id, settings, position, None)
            .await
    }

    pub async fn check_and_auto_play_with_outbox(
        &self,
        room_id: &RoomId,
        settings: &RoomSettings,
        _position: f64,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<Option<RoomPlaybackState>> {
        let enabled = settings.auto_play.value.enabled;

        if !enabled {
            return Ok(None);
        }

        let state = self.get_state(room_id).await?;
        self.auto_advance_state_if_due(&state, settings, outbox_event_factory)
            .await
    }

    async fn auto_advance_state_if_due(
        &self,
        state: &RoomPlaybackState,
        settings: &RoomSettings,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
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
            self.play_next_internal(
                &state.room_id,
                settings,
                None,
                true,
                PlaybackChangeReason::AutoAdvance,
                outbox_event_factory,
            )
            .await
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
        self.auto_advance_due_sources_for_rooms_with_outbox(settings_repo, room_ids, limit, None)
            .await
    }

    pub async fn auto_advance_due_sources_for_rooms_with_outbox(
        &self,
        settings_repo: &crate::repository::RoomSettingsRepository,
        room_ids: &[RoomId],
        limit: i64,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
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
                .auto_advance_state_if_due(&state, &settings, outbox_event_factory.clone())
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
            recorded_actor_user_id,
            target,
            bypass_room_permissions,
            outbox_event_factory,
        } = command;

        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &actor_user_id,
                    crate::models::RoomPermission::NAVIGATE_PLAYBACK,
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

        let media = if let Some(ref media_id) = target.media_id {
            let media = self
                .media_service
                .get_room_media(&room_id, media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            self.ensure_creator_is_active(media.creator_id.as_ref(), "Media")
                .await?;
            Some(media)
        } else {
            None
        };

        let mut preflight_metadata = None;
        let mut dynamic_media_name = None;
        let mut dynamic_playlist_name = None;
        if let Some(ref playlist_id) = target.playlist_id {
            let playlist = self
                .media_service
                .get_room_playlist(&room_id, playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            self.ensure_creator_is_active(playlist.creator_id.as_ref(), "Playlist")
                .await?;

            if let Some(media) = media.as_ref() {
                if playlist.is_dynamic() {
                    return Err(Error::InvalidInput(
                        "static media playlist context must reference a static playlist"
                            .to_string(),
                    ));
                }
                if media.playlist_id.as_ref() != Some(playlist_id) {
                    return Err(Error::InvalidInput(
                        "media does not belong to the specified playlist".to_string(),
                    ));
                }
            } else {
                if !playlist.is_dynamic() {
                    return Err(Error::InvalidInput(
                        "dynamic playback target must reference a dynamic playlist".to_string(),
                    ));
                }

                let requested_target = target.target.as_ref().ok_or_else(|| {
                    Error::InvalidInput(
                        "target is required for dynamic playlist playback".to_string(),
                    )
                })?;
                let resolver_user_id = playlist
                    .creator_id
                    .as_ref()
                    .copied()
                    .unwrap_or(actor_user_id);
                let resolved = self
                    .media_service
                    .resolve_dynamic_playlist_item(
                        room_id,
                        resolver_user_id,
                        playlist_id,
                        requested_target,
                    )
                    .await?;
                let resolved = resolved.ok_or_else(|| {
                    Error::NotFound("Dynamic playlist item not found".to_string())
                })?;
                preflight_metadata = self
                    .preflight_dynamic_playlist_item(
                        playlist.creator_id.as_ref().or(Some(&resolver_user_id)),
                        &playlist,
                        &resolved.name,
                        &resolved.source_config,
                        requested_target,
                    )
                    .await?;
                dynamic_media_name = Some(resolved.name);
                dynamic_playlist_name = Some(playlist.name.clone());
            }
        }

        if let Some(media) = media.as_ref() {
            preflight_metadata = self
                .preflight_static_media(media.creator_id.as_ref(), media)
                .await?;
        }

        let state = crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::MAX_RETRIES,
            Self::BACKOFF_BASE_MS,
            Self::UPDATE_STATE_RETRY_EXHAUSTED,
            || {
                let target = target.clone();
                let outbox_event_factory = outbox_event_factory.clone();
                let dynamic_media_name = dynamic_media_name.clone();
                let dynamic_playlist_name = dynamic_playlist_name.clone();
                let preflight_metadata = preflight_metadata.clone();
                async move {
                    let previous = match self.playback_repo.get(&room_id).await? {
                        Some(state) => state,
                        None => self.playback_repo.create_or_get(&room_id).await?,
                    };
                    let mut state = previous.clone();
                    state.playing_media_id = target.media_id;
                    state.playing_playlist_id = target.playlist_id;
                    state.target = target.target;
                    state.position = 0.0;
                    state.is_playing = true;
                    state.updated_at = self.clock.now();
                    self.persist_preflight_metadata(preflight_metadata.as_ref())
                        .await?;
                    let state = self
                        .persist_source_transition(
                            &state,
                            &previous,
                            PlaybackHistoryTransition::AppendEntry {
                                selected_by_user_id: recorded_actor_user_id,
                                names: dynamic_media_name.clone().map(|media_name| {
                                    PlaybackSourceNames {
                                        media_name: Some(media_name),
                                        playlist_name: dynamic_playlist_name.clone(),
                                    }
                                }),
                            },
                            PlaybackChangeReason::Selected,
                            recorded_actor_user_id,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                    self.write_playback_cache(&state).await;
                    self.broadcast_invalidation(&room_id, &state, "switch")
                        .await;
                    Ok(state)
                }
            },
        )
        .await?;

        Ok(state)
    }

    /// Generate the provider playback result before committing a source change.
    ///
    /// Playback state is shared by the room, while generated URLs and headers
    /// are user-specific. The preflight validates the source with the resource
    /// creator's provider credentials and prevents an invalid source from being
    /// announced to every room member. The actual per-viewer playback response
    /// is still generated by the API after the state commit.
    async fn preflight_static_media(
        &self,
        viewer_user_id: Option<&UserId>,
        media: &crate::models::Media,
    ) -> Result<Option<PreflightMetadata>> {
        let provider = match self
            .media_service
            .providers_manager()
            .resolve_provider(
                media.source_provider,
                media.provider_instance_name.as_deref(),
            )
            .await
        {
            Ok(provider) => provider,
            Err(error) if preflight_can_defer_generation(&error) => {
                tracing::debug!(error = %error, media_id = %media.id, "Deferring playback preflight until data-plane generation");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut ctx = self.media_service.build_provider_context(
            provider.name(),
            media.creator_id.as_ref().or(viewer_user_id),
            &media.room_id,
            media.creator_id.as_ref(),
            media.provider_instance_name.as_deref(),
        );
        ctx = ctx.with_media_id(media.id);
        let result = match provider.generate_playback(&ctx, &media.source_config).await {
            Ok(result) => result,
            Err(error) => {
                let error = Error::from(error);
                if preflight_can_defer_generation(&error) {
                    tracing::debug!(error = %error, media_id = %media.id, "Deferring playback preflight until data-plane generation");
                    return Ok(None);
                }
                return Err(error);
            }
        };
        let is_live = result.is_live.unwrap_or(false);
        Ok(Some(PreflightMetadata {
            identity: PlaybackSourceIdentity::static_media(media.room_id, media.id),
            is_live,
            duration_seconds: normalized_provider_duration(is_live, result.duration_seconds),
            media_name: Some(media.name.clone()),
            playlist_name: None,
        }))
    }

    async fn preflight_dynamic_playlist_item(
        &self,
        viewer_user_id: Option<&UserId>,
        playlist: &crate::models::Playlist,
        item_name: &str,
        source_config: &crate::models::MediaSourceConfig,
        target: &ProviderTarget,
    ) -> Result<Option<PreflightMetadata>> {
        let source_provider = playlist.source_provider.ok_or_else(|| {
            Error::InvalidInput("Dynamic playlist provider is missing".to_string())
        })?;
        if playlist.source_config.is_none() {
            return Err(Error::InvalidInput(
                "Dynamic playlist source config is missing".to_string(),
            ));
        }
        let provider = match self
            .media_service
            .providers_manager()
            .resolve_provider(source_provider, playlist.provider_instance_name.as_deref())
            .await
        {
            Ok(provider) => provider,
            Err(error) if preflight_can_defer_generation(&error) => {
                tracing::debug!(error = %error, playlist_id = %playlist.id, "Deferring playback preflight until data-plane generation");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let ctx = self.media_service.build_provider_context(
            provider.name(),
            playlist.creator_id.as_ref().or(viewer_user_id),
            &playlist.room_id,
            playlist.creator_id.as_ref(),
            playlist.provider_instance_name.as_deref(),
        );
        let result = match provider.generate_playback(&ctx, source_config).await {
            Ok(result) => result,
            Err(error) => {
                let error = Error::from(error);
                if preflight_can_defer_generation(&error) {
                    tracing::debug!(error = %error, playlist_id = %playlist.id, "Deferring playback preflight until data-plane generation");
                    return Ok(None);
                }
                return Err(error);
            }
        };
        let identity =
            PlaybackSourceIdentity::dynamic_playlist(playlist.room_id, playlist.id, target)?;
        let is_live = result.is_live.unwrap_or(false);
        Ok(Some(PreflightMetadata {
            identity,
            is_live,
            duration_seconds: normalized_provider_duration(is_live, result.duration_seconds),
            media_name: Some(item_name.to_string()),
            playlist_name: Some(playlist.name.clone()),
        }))
    }

    async fn preflight_history_entry(
        &self,
        viewer_user_id: Option<&UserId>,
        room_id: &RoomId,
        entry: &PlaybackHistoryEntry,
    ) -> Result<Option<PreflightMetadata>> {
        if let Some(media_id) = entry.media_id {
            let media = self
                .media_service
                .get_room_media(room_id, &media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;
            return self
                .preflight_static_media(media.creator_id.as_ref().or(viewer_user_id), &media)
                .await;
        }
        let (Some(playlist_id), Some(target)) = (entry.playlist_id.as_ref(), entry.target.as_ref())
        else {
            return Err(Error::InvalidInput(
                "Playback history entry has no playable source".to_string(),
            ));
        };
        let playlist = self
            .media_service
            .get_room_playlist(room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        let credential_owner = playlist.creator_id.as_ref().ok_or_else(|| {
            Error::Authorization("Dynamic playlist has no active credential owner".to_string())
        })?;
        let resolver_user = playlist
            .creator_id
            .as_ref()
            .unwrap_or(viewer_user_id.unwrap_or(credential_owner));
        let item = self
            .media_service
            .resolve_dynamic_playlist_item(*room_id, *resolver_user, playlist_id, target)
            .await?
            .ok_or_else(|| Error::NotFound("Dynamic playlist item not found".to_string()))?;
        self.preflight_dynamic_playlist_item(
            Some(resolver_user),
            &playlist,
            &item.name,
            &item.source_config,
            target,
        )
        .await
    }

    async fn persist_preflight_metadata(&self, metadata: Option<&PreflightMetadata>) -> Result<()> {
        let Some(metadata) = metadata else {
            return Ok(());
        };
        self.source_metadata_repo
            .upsert_provider_source_metadata(
                &metadata.identity,
                metadata.is_live,
                metadata.duration_seconds,
                metadata.media_name.as_deref(),
                metadata.playlist_name.as_deref(),
            )
            .await?;
        Ok(())
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
                    crate::models::RoomPermission::CONTROL_PLAYBACK_STATE,
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
            client_time_millis,
            outbox_event_factory,
        } = request;
        let PlaybackStatePatch {
            playing,
            position,
            speed,
        } = patch;

        if (playing.is_some() || position.is_some() || speed.is_some()) && !bypass_permission {
            self.permission_service
                .check_permissions(
                    &room_id,
                    &actor_user_id,
                    &[crate::models::RoomPermission::CONTROL_PLAYBACK_STATE],
                )
                .await?;
        }

        if let Some(ct) = position {
            validate_seek_position(ct)?;
        }

        if let Some(s) = speed {
            validate_playback_speed_value(s)?;
        }

        let received_at = self.clock.now();
        let client_elapsed_seconds = Self::client_elapsed_seconds(received_at, client_time_millis)?;

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
                let target_playing = playing.unwrap_or(state.is_playing);
                let target_speed = speed.unwrap_or(state.speed);
                state.position = Self::compensate_client_position(
                    ct,
                    target_playing,
                    target_speed,
                    client_elapsed_seconds,
                );
            }
            if let Some(s) = speed {
                state.speed = s;
            }
            state.updated_at = received_at;
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
