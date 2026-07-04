//! Media and playlist management service
//!
//! Design reference: external design doc 08-video-content-management.md
//!
//! Three-stage workflow:
//! 1. Parse - Parse user input to get options
//! 2. Add Media - Store `source_config` in database
//! 3. Generate Playback - Dynamically generate playback info when playing

use crate::repository::realtime_outbox::RealtimeOutboxRepository;
use crate::{
    models::{
        normalize_provider_instance_name, FromProviderParams, Media, MediaId, MediaSourceConfig,
        PlaylistId, RoomId, SourceProvider, UserId,
    },
    provider::{
        provider_requires_credential_repo, PlaybackResult, ProviderAccessService, ProviderContext,
        ProviderStoreResolver, SourceConfig,
    },
    repository::{realtime_outbox::NewRealtimeOutboxEvent, UserProviderCredentialRepository},
    repository::{MediaRepository, PlaylistRepository, UserRepository},
    service::{
        notification::{MediaAddedNotification, NotificationService},
        provider_binding::resolve_credential_provider_instance_binding,
        source_config::validate_source_config_size,
        FileStorageService, PermissionService, ProvidersManager,
    },
    Error, Result,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

mod cover;
mod dynamic;
mod helpers;
pub use cover::CreateMediaCoverUploadSession;
use helpers::{
    batch_media_position, dedup_media_ids, ensure_media_creator_can_edit,
    media_source_config_error, media_source_prepare_error, validate_media_name, MAX_BATCH_SIZE,
    MEDIA_BATCH_PREPARE_CONCURRENCY,
};

pub type RealtimeOutboxMediaEventFactory =
    Arc<dyn Fn(&Media) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxMediaBatchEventFactory =
    Arc<dyn Fn(&[Media]) -> Result<Vec<NewRealtimeOutboxEvent>> + Send + Sync>;

#[derive(Default)]
pub struct MediaServiceRuntime {
    pub credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    pub credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    pub provider_access_service: Option<Arc<dyn ProviderAccessService>>,
    pub provider_stores: Option<Arc<dyn ProviderStoreResolver>>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
}

/// Request to add a media item
///
/// Design note: According to the three-stage workflow,
/// clients should call parse endpoint first, then construct `source_config`,
/// and finally call `add_media` with the validated `source_config`.
///
/// Uses provider registry pattern - `provider_instance_name` identifies which
/// provider instance to use (e.g., "`bilibili_main`", "`alist_company`").
#[derive(Debug, Clone)]
pub struct AddMediaRequest {
    pub playlist_id: Option<PlaylistId>,
    pub name: String,
    pub description: String,
    pub source_provider: SourceProvider,
    /// Provider instance name (e.g., "`bilibili_main`", "`alist_company`")
    /// `None` means use the default local instance for `source_provider`.
    pub provider_instance_name: Option<String>,
    pub source_config: MediaSourceConfig,
}

/// Request to edit a media item
#[derive(Debug, Clone)]
pub struct EditMediaRequest {
    pub media_id: MediaId,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MoveMediaRequest {
    pub media_ids: Vec<MediaId>,
    pub source_playlist_id: Option<PlaylistId>,
    pub target_playlist_id: Option<PlaylistId>,
    pub all_from_scope: bool,
    pub before_media_id: Option<MediaId>,
    pub after_media_id: Option<MediaId>,
}

struct PreparedMediaSource {
    provider_name: String,
    provider_instance_name: Option<String>,
    source_config: MediaSourceConfig,
}

pub struct BackendPlaybackRequest<'a> {
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<&'a crate::models::ProviderTarget>,
}

/// Media management service
///
/// Responsible for media operations based on the new architecture:
/// - Media belongs to a playlist (not directly to room)
/// - Media stores `source_config` (persistent configuration)
/// - Playback info is generated dynamically by providers
/// - Uses provider registry pattern to avoid enum switching
#[derive(Clone)]
pub struct MediaService {
    media_repo: MediaRepository,
    playlist_repo: PlaylistRepository,
    permission_service: PermissionService,
    providers_manager: Arc<ProvidersManager>,
    /// Local room event bus for media/domain notifications
    notification_service: NotificationService,
    /// Credential encryption used by credential-backed providers.
    credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    /// Repository used by credential-backed providers.
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    /// Typed provider credential/session access cache.
    provider_access_service: Option<Arc<dyn ProviderAccessService>>,
    /// Provider-scoped stores for playback cache and locks.
    provider_stores: Option<Arc<dyn ProviderStoreResolver>>,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl std::fmt::Debug for MediaService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaService").finish()
    }
}

impl MediaService {
    async fn insert_media_outbox_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        media: &Media,
        outbox_event_factory: Option<&RealtimeOutboxMediaEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(media))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    async fn insert_media_batch_outbox_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        media: &[Media],
        outbox_event_factory: Option<&RealtimeOutboxMediaBatchEventFactory>,
    ) -> Result<()> {
        if let Some(events) = outbox_event_factory
            .map(|factory| factory(media))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_many_with_executor(&events, tx).await?;
            }
        }
        Ok(())
    }

    pub(super) fn build_provider_context<'a>(
        &'a self,
        provider_name: &str,
        user_id: Option<&'a UserId>,
        room_id: &'a RoomId,
        credential_owner_id: Option<&'a UserId>,
        provider_instance_name: Option<&'a str>,
    ) -> ProviderContext<'a> {
        let mut ctx = ProviderContext::new("synctv").with_room_id(*room_id);
        if let Some(user_id) = user_id {
            ctx = ctx.with_user_id(*user_id);
        }
        if let Some(credential_owner_id) = credential_owner_id {
            ctx = ctx.with_credential_owner_id(*credential_owner_id);
        }
        if let Some(provider_instance_name) =
            normalize_provider_instance_name(provider_instance_name)
        {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(service) = self.provider_access_service.clone() {
            ctx = ctx.with_provider_access_service(service);
        }
        if let Some(provider_stores) = &self.provider_stores {
            ctx = ctx.with_store(provider_stores.load(provider_name));
        }
        ctx
    }

    pub(super) fn ensure_provider_credential_repo(&self, provider_name: &str) -> Result<()> {
        if provider_requires_credential_repo(provider_name) && self.credential_repo.is_none() {
            return Err(Error::ServiceUnavailable(format!(
                "Provider '{provider_name}' requires credential repository wiring"
            )));
        }

        Ok(())
    }

    async fn resolve_media_provider(
        &self,
        source_provider: SourceProvider,
        provider_instance_name: Option<&str>,
    ) -> Result<Arc<dyn crate::provider::MediaProvider>> {
        let provider = self
            .providers_manager
            .resolve_provider(source_provider, provider_instance_name)
            .await?;

        Ok(provider)
    }

    async fn resolve_actor_username(&self, user_id: &UserId) -> Result<String> {
        UserRepository::new(self.media_repo.pool().clone())
            .get_by_id(user_id)
            .await?
            .map(|user| user.username)
            .ok_or_else(|| Error::NotFound("Actor user not found".to_string()))
    }

    async fn prepare_media_source(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        source_provider: SourceProvider,
        provider_instance_name: Option<&str>,
        source_config: MediaSourceConfig,
        item_name: Option<&str>,
    ) -> Result<PreparedMediaSource> {
        let explicit_provider_instance =
            normalize_provider_instance_name(provider_instance_name).map(str::to_string);
        let config_provider = source_config.provider();
        let source_config = source_config
            .ensure_provider(source_provider)
            .map_err(Error::InvalidInput)?;
        validate_source_config_size(&source_config)?;

        let provider = self
            .resolve_media_provider(config_provider, explicit_provider_instance.as_deref())
            .await?;
        self.ensure_provider_credential_repo(provider.name())?;

        let dependency_ctx = self.build_provider_context(
            provider.name(),
            Some(user_id),
            room_id,
            Some(user_id),
            explicit_provider_instance.as_deref(),
        );

        provider
            .validate_source_config(&dependency_ctx, SourceConfig::media(&source_config))
            .await
            .map_err(|error| media_source_config_error(item_name, error))?;

        let bound_provider_instance = resolve_credential_provider_instance_binding(
            provider.as_ref(),
            self.credential_repo.as_ref(),
            &dependency_ctx,
            SourceConfig::media(&source_config),
            explicit_provider_instance.as_deref(),
        )
        .await?;
        let provider = if bound_provider_instance == explicit_provider_instance {
            provider
        } else {
            self.resolve_media_provider(config_provider, bound_provider_instance.as_deref())
                .await?
        };
        let ctx = self.build_provider_context(
            provider.name(),
            Some(user_id),
            room_id,
            Some(user_id),
            bound_provider_instance.as_deref(),
        );

        provider
            .validate_source_config(&ctx, SourceConfig::media(&source_config))
            .await
            .map_err(|error| media_source_config_error(item_name, error))?;

        let prepared_source_config = provider
            .prepare_source_config(&ctx, SourceConfig::media(&source_config))
            .await
            .map_err(|error| media_source_prepare_error(item_name, error))?
            .into_media()
            .map_err(|error| media_source_config_error(item_name, error))?;

        Ok(PreparedMediaSource {
            provider_name: provider.name().to_string(),
            provider_instance_name: bound_provider_instance,
            source_config: prepared_source_config,
        })
    }

    async fn validate_and_prepare_media_batch(
        &self,
        user_id: UserId,
        room_id: RoomId,
        items: Vec<AddMediaRequest>,
    ) -> Result<Vec<(AddMediaRequest, PreparedMediaSource)>> {
        futures::stream::iter(items.into_iter().map(|item| {
            let service = self.clone();
            async move {
                validate_media_name(&item.name)?;
                if item.description.chars().count() > 5000 {
                    return Err(Error::InvalidInput(format!(
                        "Media description for item '{}' cannot exceed 5000 characters",
                        item.name
                    )));
                }

                let prepared_source = service
                    .prepare_media_source(
                        &user_id,
                        &room_id,
                        item.source_provider,
                        item.provider_instance_name.as_deref(),
                        item.source_config.clone(),
                        Some(&item.name),
                    )
                    .await?;

                Ok((item, prepared_source))
            }
        }))
        .buffered(MEDIA_BATCH_PREPARE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
    }

    /// Create a new media service
    #[must_use]
    pub fn new(
        media_repo: MediaRepository,
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        notification_service: NotificationService,
    ) -> Self {
        Self::new_with_runtime(
            media_repo,
            playlist_repo,
            permission_service,
            providers_manager,
            notification_service,
            MediaServiceRuntime::default(),
        )
    }

    #[must_use]
    pub fn new_with_runtime(
        media_repo: MediaRepository,
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        notification_service: NotificationService,
        runtime: MediaServiceRuntime,
    ) -> Self {
        assert!(
            runtime.credential_repo.is_none() || runtime.credential_encryption.is_some(),
            "provider credential repository wiring requires credential encryption"
        );
        Self {
            media_repo,
            playlist_repo,
            permission_service,
            providers_manager,
            notification_service,
            credential_encryption: runtime.credential_encryption,
            credential_repo: runtime.credential_repo,
            provider_access_service: runtime.provider_access_service,
            provider_stores: runtime.provider_stores,
            realtime_outbox: runtime.realtime_outbox,
            file_storage_service: runtime.file_storage_service,
        }
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn FileStorageService>> {
        self.file_storage_service.as_ref()
    }

    /// Get a reference to the providers manager
    #[must_use]
    pub const fn providers_manager(&self) -> &Arc<ProvidersManager> {
        &self.providers_manager
    }

    /// Get the credential encryption used for provider source resolution, if configured.
    #[must_use]
    pub const fn credential_encryption(
        &self,
    ) -> Option<&crate::credential_encryption::CredentialEncryption> {
        self.credential_encryption.as_ref()
    }

    /// Get the credential repository used for provider source resolution, if configured.
    #[must_use]
    pub const fn credential_repo(&self) -> Option<&Arc<UserProviderCredentialRepository>> {
        self.credential_repo.as_ref()
    }

    /// Add media to a playlist
    ///
    /// Three-stage workflow - Stage 2:
    /// 1. Client calls parse endpoint (Stage 1)
    /// 2. Client constructs `source_config`
    /// 3. Client calls `add_media` with `source_config`
    /// 4. Service validates using provider and stores in database
    pub async fn add_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: AddMediaRequest,
    ) -> Result<Media> {
        self.add_media_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn add_media_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: AddMediaRequest,
        outbox_event_factory: Option<RealtimeOutboxMediaEventFactory>,
    ) -> Result<Media> {
        validate_media_name(&request.name)?;
        if request.description.chars().count() > 5000 {
            return Err(Error::InvalidInput(
                "Media description cannot exceed 5000 characters".to_string(),
            ));
        }

        // Check permission
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        if let Some(ref playlist_id) = request.playlist_id {
            let playlist = self
                .playlist_repo
                .get_by_room_and_id(&room_id, playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
            debug_assert_eq!(playlist.room_id, room_id);
        }

        let prepared_source = self
            .prepare_media_source(
                &user_id,
                &room_id,
                request.source_provider,
                request.provider_instance_name.as_deref(),
                request.source_config,
                None,
            )
            .await?;

        // Use a transaction to atomically get the next position and insert,
        // preventing concurrent adds from getting the same position
        let mut tx = self.media_repo.pool().begin().await?;

        // Get next position in playlist (locked with FOR UPDATE)
        let position = self
            .media_repo
            .get_next_append_position_with_tx(&room_id, request.playlist_id.as_ref(), &mut tx)
            .await?;

        // Store the provider type and optional instance binding separately.
        // Source config remains provider-owned and must not carry instance
        // routing metadata.
        let media = Media::from_provider_with_params(FromProviderParams {
            playlist_id: request.playlist_id,
            room_id,
            creator_id: Some(user_id),
            name: request.name.clone(),
            description: request.description.clone(),
            source_config: prepared_source.source_config,
            source_provider: request.source_provider,
            provider_instance_name: prepared_source.provider_instance_name.clone(),
            position,
        });
        let created_media = self
            .media_repo
            .create_with_executor(&media, &mut *tx)
            .await?;

        self.insert_media_outbox_tx(&mut tx, &created_media, outbox_event_factory.as_ref())
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id,
            media_id = %created_media.id,
            name = %created_media.name,
            source_provider = prepared_source.provider_name,
            provider_instance_name = prepared_source.provider_instance_name.as_deref().unwrap_or(""),
            "Media added to playlist"
        );
        let actor_username = match self.resolve_actor_username(&user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    media_id = %created_media.id,
                    "Skipped media added notification because actor username lookup failed"
                );
                return Ok(created_media);
            }
        };

        let subscriber_count = self.notification_service.notify_media_added(
            &room_id,
            &MediaAddedNotification {
                user_id: &user_id,
                username: &actor_username,
                media_id: created_media.id,
                title: &created_media.name,
                url: "", // URL is generated dynamically at playback time
                position: created_media.position,
            },
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                media_id = %created_media.id,
                "Media added event had no local subscribers"
            );
        }

        Ok(created_media)
    }

    /// Add multiple media items to a playlist
    pub async fn add_media_batch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
        items: Vec<AddMediaRequest>,
    ) -> Result<Vec<Media>> {
        self.add_media_batch_with_outbox(room_id, user_id, playlist_id, items, None)
            .await
    }

    pub async fn add_media_batch_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
        items: Vec<AddMediaRequest>,
        outbox_event_factory: Option<RealtimeOutboxMediaBatchEventFactory>,
    ) -> Result<Vec<Media>> {
        // Check permission
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        if let Some(ref playlist_id) = playlist_id {
            let playlist = self
                .playlist_repo
                .get_by_room_and_id(&room_id, playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
            debug_assert_eq!(playlist.room_id, room_id);
        }

        if items.is_empty() {
            return Ok(Vec::new());
        }

        if items.len() > MAX_BATCH_SIZE {
            return Err(Error::InvalidInput(format!(
                "Batch size exceeds maximum of {MAX_BATCH_SIZE}"
            )));
        }

        let mut validated_items = self
            .validate_and_prepare_media_batch(user_id, room_id, items)
            .await?;

        for (item, _) in &mut validated_items {
            item.playlist_id = playlist_id;
        }

        // Use a transaction to atomically get the next position and batch insert,
        // preventing concurrent adds from getting the same position
        let mut tx = self.media_repo.pool().begin().await?;

        // Get starting position (locked with FOR UPDATE)
        let start_position = self
            .media_repo
            .get_next_append_position_with_tx(&room_id, playlist_id.as_ref(), &mut tx)
            .await?;

        // Create media items with provider info
        let mut media_items = Vec::with_capacity(validated_items.len());
        for (index, (item, prepared_source)) in validated_items.into_iter().enumerate() {
            let media = Media::from_provider_with_params(FromProviderParams {
                playlist_id: item.playlist_id,
                room_id,
                creator_id: Some(user_id),
                name: item.name,
                description: item.description,
                source_config: prepared_source.source_config,
                source_provider: item.source_provider,
                provider_instance_name: prepared_source.provider_instance_name,
                position: batch_media_position(index, start_position)?,
            });
            media_items.push(media);
        }

        // Batch insert within the transaction
        let created_items = self
            .media_repo
            .create_batch_with_executor(&media_items, &mut *tx)
            .await?;

        self.insert_media_batch_outbox_tx(&mut tx, &created_items, outbox_event_factory.as_ref())
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id,
            count = created_items.len(),
            "Batch added media to playlist"
        );
        let actor_username = match self.resolve_actor_username(&user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped batch media added notifications because actor username lookup failed"
                );
                return Ok(created_items);
            }
        };

        for item in &created_items {
            let subscriber_count = self.notification_service.notify_media_added(
                &room_id,
                &MediaAddedNotification {
                    user_id: &user_id,
                    username: &actor_username,
                    media_id: item.id,
                    title: &item.name,
                    url: "",
                    position: item.position,
                },
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    media_id = %item.id,
                    "Media added event had no local subscribers"
                );
            }
        }

        Ok(created_items)
    }

    /// Maximum retry attempts for optimistic lock conflicts on media edits
    const EDIT_MAX_RETRIES: u32 = 3;

    /// Edit media item
    ///
    /// Uses optimistic locking via `version` column to detect concurrent
    /// modifications. If another edit changes the row between our read and
    /// write, the conditional UPDATE returns no rows and we retry with fresh
    /// data.
    pub async fn edit_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: EditMediaRequest,
    ) -> Result<Media> {
        self.edit_media_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn edit_media_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: EditMediaRequest,
        outbox_event_factory: Option<RealtimeOutboxMediaEventFactory>,
    ) -> Result<Media> {
        let updated_media = crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::EDIT_MAX_RETRIES,
            crate::service::optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            &format!(
                "Media edit failed after maximum retry attempts for media_id={}",
                request.media_id
            ),
            || async {
                // Get existing media (fresh on every retry)
                let mut media = self
                    .media_repo
                    .get_by_room_and_id(&room_id, &request.media_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

                ensure_media_creator_can_edit(&media, &user_id)?;

                // Check permission: client media edits are creator-owned. Global
                // administrators use admin_edit_media_with_outbox instead of this
                // member-facing path. Creating media resources includes
                // maintaining resources created by the same actor.
                // IMPORTANT: Use check_permission_no_cache to ensure fresh permissions on each retry.
                // This prevents a race condition where:
                // 1. Permission is granted and cached on first attempt
                // 2. Permission is revoked by admin before retry
                // 3. Retry would succeed with stale cached permission
                // By bypassing cache, we ensure each retry checks current permission state.
                self.permission_service
                    .check_permission_no_cache(
                        &room_id,
                        &user_id,
                        crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
                    )
                    .await?;

                // Capture the version before applying changes to detect concurrent edits
                let expected_version = media.version;

                // Update fields
                if let Some(ref name) = request.name {
                    validate_media_name(name)?;
                    media.name = name.clone();
                }
                if let Some(ref description) = request.description {
                    if description.chars().count() > 5000 {
                        return Err(Error::InvalidInput(
                            "Media description cannot exceed 5000 characters".to_string(),
                        ));
                    }
                    media.description = description.clone();
                }
                let mut tx = self.media_repo.pool().begin().await?;
                // Conditional update: only succeed if no other edit changed the row
                match self
                    .media_repo
                    .update_with_version_with_executor(&media, expected_version, &mut *tx)
                    .await
                {
                    Ok(Some(updated_media)) => {
                        self.insert_media_outbox_tx(
                            &mut tx,
                            &updated_media,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                        tx.commit().await?;
                        Ok(updated_media)
                    }
                    Ok(None) => {
                        tx.rollback().await?;
                        Err(Error::OptimisticLockConflict)
                    }
                    Err(e) => Err(e),
                }
            },
        )
        .await?;

        tracing::info!(
            room_id = %room_id,
            media_id = %request.media_id,
            "Media edited"
        );
        let actor_username = match self.resolve_actor_username(&user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    media_id = %updated_media.id,
                    "Skipped media updated notification because actor username lookup failed"
                );
                return Ok(updated_media);
            }
        };

        let subscriber_count = self.notification_service.notify_media_updated(
            &room_id,
            &user_id,
            &actor_username,
            updated_media.id,
            &updated_media.name,
            updated_media.position,
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                media_id = %updated_media.id,
                "Media updated event had no local subscribers"
            );
        }

        Ok(updated_media)
    }

    /// Edit media item as a global admin.
    pub async fn admin_edit_media(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        actor_username: &str,
        request: EditMediaRequest,
    ) -> Result<Media> {
        self.admin_edit_media_with_outbox(room_id, admin_user_id, actor_username, request, None)
            .await
    }

    pub async fn admin_edit_media_with_outbox(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        actor_username: &str,
        request: EditMediaRequest,
        outbox_event_factory: Option<RealtimeOutboxMediaEventFactory>,
    ) -> Result<Media> {
        let updated_media = crate::service::optimistic_retry::retry_with_optimistic_lock(
            Self::EDIT_MAX_RETRIES,
            crate::service::optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            &format!(
                "Media edit failed after maximum retry attempts for media_id={}",
                request.media_id
            ),
            || async {
                let mut media = self
                    .media_repo
                    .get_by_room_and_id(&room_id, &request.media_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

                let expected_version = media.version;

                if let Some(ref name) = request.name {
                    validate_media_name(name)?;
                    media.name = name.clone();
                }
                let mut tx = self.media_repo.pool().begin().await?;
                match self
                    .media_repo
                    .update_with_version_with_executor(&media, expected_version, &mut *tx)
                    .await
                {
                    Ok(Some(updated_media)) => {
                        self.insert_media_outbox_tx(
                            &mut tx,
                            &updated_media,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                        tx.commit().await?;
                        Ok(updated_media)
                    }
                    Ok(None) => {
                        tx.rollback().await?;
                        Err(Error::OptimisticLockConflict)
                    }
                    Err(e) => Err(e),
                }
            },
        )
        .await?;

        tracing::info!(
            room_id = %room_id,
            admin_user_id = %admin_user_id,
            media_id = %request.media_id,
            "Media edited by admin"
        );
        let subscriber_count = self.notification_service.notify_media_updated(
            &room_id,
            &admin_user_id,
            actor_username,
            updated_media.id,
            &updated_media.name,
            updated_media.position,
        );
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                media_id = %updated_media.id,
                "Media updated event had no local subscribers"
            );
        }

        Ok(updated_media)
    }

    /// Get media by ID
    pub async fn get_media(&self, media_id: &MediaId) -> Result<Option<Media>> {
        self.media_repo.get_by_id(media_id).await
    }

    /// Get media by ID, scoped to a room.
    pub async fn get_room_media(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<Option<Media>> {
        self.media_repo.get_by_room_and_id(room_id, media_id).await
    }

    /// Get multiple media items by IDs in a single query
    pub async fn get_media_batch(&self, media_ids: &[MediaId]) -> Result<Vec<Media>> {
        self.media_repo.get_by_ids(media_ids).await
    }

    pub async fn generate_backend_playback_for_source(
        &self,
        request: BackendPlaybackRequest<'_>,
    ) -> Result<Option<PlaybackResult>> {
        // Shared backend playback entrypoint used by request adapters and
        // background workers. External entrypoints should converge here;
        // provider adapters own mode selection, signing, headers, manifests,
        // subtitles, and lifecycle metadata inside `generate_playback`.
        match (request.media_id, request.playlist_id) {
            (Some(media_id), None) => {
                let Some(media) = self.get_room_media(&request.room_id, &media_id).await? else {
                    return Ok(None);
                };
                let provider = self
                    .resolve_media_provider(
                        media.source_provider,
                        media.provider_instance_name.as_deref(),
                    )
                    .await?;
                let ctx = self
                    .build_provider_context(
                        provider.name(),
                        None,
                        &request.room_id,
                        media.creator_id.as_ref(),
                        media.provider_instance_name.as_deref(),
                    )
                    .with_media_id(media.id);
                let result = provider
                    .generate_playback(&ctx, &media.source_config)
                    .await?;
                Ok(Some(result))
            }
            (None, Some(playlist_id)) => {
                let prepared = self
                    .prepare_dynamic_playlist(&request.room_id, &playlist_id)
                    .await?;
                let ctx = self.dynamic_playlist_context(&prepared, None, None);
                let Some(target) = request.target else {
                    return Err(Error::InvalidInput(
                        "target is required for dynamic playlist playback".to_string(),
                    ));
                };
                let Some(item) = prepared
                    .dynamic_folder()?
                    .resolve_item(&ctx, &prepared.playlist, target)
                    .await?
                else {
                    return Ok(None);
                };
                let result = prepared
                    .provider
                    .generate_playback(&ctx, &item.source_config)
                    .await?;
                Ok(Some(result))
            }
            _ => Err(Error::InvalidInput(
                "playback source must reference exactly one media or playlist".to_string(),
            )),
        }
    }

    /// Get multiple media items by IDs, scoped to a room.
    pub async fn get_room_media_batch(
        &self,
        room_id: &RoomId,
        media_ids: &[MediaId],
    ) -> Result<Vec<Media>> {
        self.media_repo
            .get_by_room_and_ids_with_executor(room_id, media_ids, self.media_repo.pool())
            .await
    }

    /// Get all media in a playlist.
    pub async fn get_playlist_media(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        self.media_repo.get_by_playlist(playlist_id).await
    }

    /// Get all media in a playlist, scoped to a room.
    pub async fn get_room_playlist_media(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<Media>> {
        self.media_repo
            .get_by_room_and_playlist(room_id, playlist_id)
            .await
    }

    /// Get media directly under the room root.
    pub async fn get_room_root_media(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        self.media_repo.get_room_root(room_id).await
    }

    /// Get paginated media in a playlist.
    pub async fn get_playlist_media_paginated(
        &self,
        playlist_id: &PlaylistId,
        pagination: crate::models::PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        pagination.validate()?;
        self.media_repo
            .get_playlist_paginated(playlist_id, pagination)
            .await
    }

    /// Get paginated media directly under the room root.
    pub async fn get_room_root_media_paginated(
        &self,
        room_id: &RoomId,
        pagination: crate::models::PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        pagination.validate()?;
        self.media_repo
            .get_room_root_paginated(room_id, pagination)
            .await
    }

    /// Get media items from a playlist with limit and offset (no count query).
    ///
    /// This is a simpler version of `get_playlist_media_paginated` that doesn't
    /// return the total count, useful when you only need the items.
    pub async fn get_playlist_media_offset_limit(
        &self,
        playlist_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Media>> {
        self.media_repo
            .get_by_playlist_limit_offset(playlist_id, limit, offset)
            .await
    }

    /// Get room-root media items with limit and offset (no count query).
    pub async fn get_room_root_media_offset_limit(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Media>> {
        self.media_repo
            .get_room_root_limit_offset(room_id, limit, offset)
            .await
    }

    pub async fn move_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MoveMediaRequest,
    ) -> Result<Vec<Media>> {
        self.move_media_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn move_media_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MoveMediaRequest,
        outbox_event_factory: Option<RealtimeOutboxMediaBatchEventFactory>,
    ) -> Result<Vec<Media>> {
        self.move_media_internal(room_id, user_id, None, request, false, outbox_event_factory)
            .await
    }

    pub async fn admin_move_media(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        actor_username: &str,
        request: MoveMediaRequest,
    ) -> Result<Vec<Media>> {
        self.admin_move_media_with_outbox(room_id, admin_user_id, actor_username, request, None)
            .await
    }

    pub async fn admin_move_media_with_outbox(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        actor_username: &str,
        request: MoveMediaRequest,
        outbox_event_factory: Option<RealtimeOutboxMediaBatchEventFactory>,
    ) -> Result<Vec<Media>> {
        self.move_media_internal(
            room_id,
            admin_user_id,
            Some(actor_username),
            request,
            true,
            outbox_event_factory,
        )
        .await
    }

    async fn move_media_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        actor_username: Option<&str>,
        request: MoveMediaRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxMediaBatchEventFactory>,
    ) -> Result<Vec<Media>> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission_no_cache(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::REORDER_MEDIA_RESOURCES,
                )
                .await?;
        }

        let has_before = request.before_media_id.is_some();
        let has_after = request.after_media_id.is_some();
        if has_before && has_after {
            return Err(Error::InvalidInput(
                "At most one of before_media_id or after_media_id may be set".to_string(),
            ));
        }

        if request.all_from_scope && !request.media_ids.is_empty() {
            return Err(Error::InvalidInput(
                "media_ids cannot be provided when all_from_scope is true".to_string(),
            ));
        }

        if !request.all_from_scope && request.source_playlist_id.is_some() {
            return Err(Error::InvalidInput(
                "source_playlist_id is only valid when all_from_scope is true".to_string(),
            ));
        }

        let explicit_media_ids = dedup_media_ids(request.media_ids);
        if !request.all_from_scope && explicit_media_ids.is_empty() {
            return Err(Error::InvalidInput(
                "At least one media_id is required".to_string(),
            ));
        }

        if !request.all_from_scope && explicit_media_ids.len() > MAX_BATCH_SIZE {
            return Err(Error::InvalidInput(format!(
                "Batch size exceeds maximum of {MAX_BATCH_SIZE}"
            )));
        }

        let mut tx = self.media_repo.pool().begin().await?;

        if let Some(ref source_playlist_id) = request.source_playlist_id {
            let playlists = self
                .playlist_repo
                .get_by_room_and_ids_with_executor(
                    &room_id,
                    std::slice::from_ref(source_playlist_id),
                    &mut *tx,
                )
                .await?;
            let source_playlist = playlists
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound("Source playlist not found".to_string()))?;
            if source_playlist.is_dynamic() {
                return Err(Error::InvalidInput(
                    "Source playlist must be static".to_string(),
                ));
            }
        }

        if let Some(ref target_playlist_id) = request.target_playlist_id {
            let playlists = self
                .playlist_repo
                .get_by_room_and_ids_with_executor(
                    &room_id,
                    std::slice::from_ref(target_playlist_id),
                    &mut *tx,
                )
                .await?;
            let target_playlist = playlists
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound("Target playlist not found".to_string()))?;
            if target_playlist.is_dynamic() {
                return Err(Error::InvalidInput(
                    "Target playlist must be static".to_string(),
                ));
            }
        }

        let original_media = if request.all_from_scope {
            self.media_repo
                .get_scope_with_executor(&room_id, request.source_playlist_id.as_ref(), &mut *tx)
                .await?
        } else {
            let fetched = self
                .media_repo
                .get_by_room_and_ids_with_executor(&room_id, &explicit_media_ids, &mut *tx)
                .await?;
            if fetched.len() != explicit_media_ids.len() {
                return Err(Error::NotFound("Media not found".to_string()));
            }
            let mut fetched_map = HashMap::with_capacity(fetched.len());
            for media in fetched {
                fetched_map.insert(media.id, media);
            }
            explicit_media_ids
                .iter()
                .map(|media_id| {
                    fetched_map
                        .remove(media_id)
                        .ok_or_else(|| Error::NotFound("Media not found".to_string()))
                })
                .collect::<Result<Vec<_>>>()?
        };

        if request.all_from_scope && original_media.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let original_scope_by_id: HashMap<MediaId, Option<PlaylistId>> = original_media
            .iter()
            .map(|media| (media.id, media.playlist_id))
            .collect();
        let media_ids: Vec<MediaId> = original_media.iter().map(|media| media.id).collect();

        let moved = self
            .media_repo
            .move_batch_to_scope_with_tx(
                &room_id,
                &media_ids,
                request.target_playlist_id.as_ref(),
                request.before_media_id.as_ref(),
                request.after_media_id.as_ref(),
                &mut tx,
            )
            .await?;

        self.insert_media_batch_outbox_tx(&mut tx, &moved, outbox_event_factory.as_ref())
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id,
            moved_count = moved.len(),
            "Media moved"
        );

        let moved_within_same_scope = moved.iter().all(|media| {
            original_scope_by_id
                .get(&media.id)
                .is_some_and(|original_scope| *original_scope == media.playlist_id)
        });
        let actor_username = if let Some(actor_username) = actor_username {
            actor_username.to_string()
        } else {
            match self.resolve_actor_username(&user_id).await {
                Ok(username) => username,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Skipped media move notifications because actor username lookup failed"
                    );
                    return Ok(moved);
                }
            }
        };

        if moved_within_same_scope {
            if moved.len() == 1 {
                let media = &moved[0];
                let subscriber_count = self.notification_service.notify_media_updated(
                    &room_id,
                    &user_id,
                    &actor_username,
                    media.id,
                    &media.name,
                    media.position,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media.id,
                        "Media moved event had no local subscribers"
                    );
                }
            } else {
                let moved_ids: Vec<MediaId> = moved.iter().map(|media| media.id).collect();
                let subscriber_count = self.notification_service.notify_playlist_reordered(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    &moved_ids,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        "Playlist reordered event had no local subscribers"
                    );
                }
            }
        } else {
            for media in &moved {
                if let Some(original_scope) = original_scope_by_id.get(&media.id) {
                    if *original_scope == media.playlist_id {
                        let subscriber_count = self.notification_service.notify_media_updated(
                            &room_id,
                            &user_id,
                            &actor_username,
                            media.id,
                            &media.name,
                            media.position,
                        );
                        if subscriber_count == 0 {
                            tracing::debug!(
                                room_id = %room_id,
                                media_id = %media.id,
                                "Media moved event had no local subscribers"
                            );
                        }
                    } else {
                        let subscriber_count = self.notification_service.notify_media_removed(
                            &room_id,
                            Some(&user_id),
                            &actor_username,
                            media.id,
                        );
                        if subscriber_count == 0 {
                            tracing::debug!(
                                room_id = %room_id,
                                media_id = %media.id,
                                "Moved media removal event had no local subscribers"
                            );
                        }
                        let subscriber_count = self.notification_service.notify_media_added(
                            &room_id,
                            &MediaAddedNotification {
                                user_id: &user_id,
                                username: &actor_username,
                                media_id: media.id,
                                title: &media.name,
                                url: "",
                                position: media.position,
                            },
                        );
                        if subscriber_count == 0 {
                            tracing::debug!(
                                room_id = %room_id,
                                media_id = %media.id,
                                "Moved media add event had no local subscribers"
                            );
                        }
                    }
                }
            }
        }

        Ok(moved)
    }

    /// Delete all media in a playlist (single query, no N+1).
    pub async fn delete_playlist_media(&self, playlist_id: &PlaylistId) -> Result<usize> {
        self.media_repo.delete_playlist(playlist_id).await
    }

    /// Delete all media directly under the room root.
    pub async fn delete_room_root_media(&self, room_id: &RoomId) -> Result<usize> {
        self.media_repo.delete_room_root(room_id).await
    }

    pub async fn count_playlist_media(&self, playlist_id: &PlaylistId) -> Result<i64> {
        self.media_repo.count_by_playlist(playlist_id).await
    }

    pub async fn count_all_media(&self) -> Result<i64> {
        self.media_repo.count_all().await
    }

    pub async fn count_room_playlist_media(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<i64> {
        self.media_repo
            .count_by_room_and_playlist(room_id, playlist_id)
            .await
    }

    pub async fn count_playlist_media_accessible(&self, playlist_id: &PlaylistId) -> Result<i64> {
        self.media_repo
            .count_by_playlist_accessible(playlist_id)
            .await
    }

    pub async fn count_room_root_media(&self, room_id: &RoomId) -> Result<i64> {
        self.media_repo.count_room_root(room_id).await
    }

    /// Batch count media items across multiple playlists
    pub async fn count_playlist_media_batch(
        &self,
        playlist_ids: &[PlaylistId],
    ) -> Result<std::collections::HashMap<PlaylistId, i64>> {
        self.media_repo.count_by_playlists_batch(playlist_ids).await
    }

    pub async fn count_playlist_media_batch_accessible(
        &self,
        playlist_ids: &[PlaylistId],
    ) -> Result<std::collections::HashMap<PlaylistId, i64>> {
        self.media_repo
            .count_by_playlists_batch_accessible(playlist_ids)
            .await
    }

    /// Get playlist metadata needed by playback/media orchestration.
    pub async fn get_playlist(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Option<crate::models::Playlist>> {
        self.playlist_repo.get_by_id(playlist_id).await
    }

    /// Get playlist metadata scoped to a room.
    pub async fn get_room_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Option<crate::models::Playlist>> {
        self.playlist_repo
            .get_by_room_and_id(room_id, playlist_id)
            .await
    }
}

#[cfg(test)]
mod tests;
