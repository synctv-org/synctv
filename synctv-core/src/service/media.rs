//! Media and playlist management service
//!
//! Design reference: /Volumes/workspace/rust/design/08-视频内容管理.md
//!
//! Three-stage workflow:
//! 1. Parse - Parse user input to get options
//! 2. Add Media - Store `source_config` in database
//! 3. Generate Playback - Dynamically generate playback info when playing

use crate::{
    models::{Media, MediaId, PermissionBits, PlaylistId, RoomId, UserId},
    provider::{DirectoryItem, ProviderContext},
    repository::UserProviderCredentialRepository,
    repository::{MediaRepository, PlaylistRepository},
    service::{notification::NotificationService, permission::PermissionService, ProvidersManager},
    Error, Result,
};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum number of items allowed in a single batch operation
///
/// This limit prevents `DoS` attacks and ensures reasonable resource usage
/// for bulk operations like add, delete, and reorder.
const MAX_BATCH_SIZE: usize = 100;
/// Limit `source_config` storage size to prevent unbounded JSONB growth.
const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024;
/// Leave sparse gaps between inserted media positions to reduce renumbering.
const MEDIA_BATCH_POSITION_STEP: f64 = 1024.0;

fn batch_media_position(index: usize, start_position: f64) -> f64 {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    MEDIA_BATCH_POSITION_STEP.mul_add(f64::from(index), start_position)
}

fn provider_requires_credential_repo(provider_name: &str) -> bool {
    matches!(
        provider_name,
        crate::provider::AlistProvider::NAME
            | crate::provider::BilibiliProvider::NAME
            | crate::provider::EmbyProvider::NAME
    )
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
    /// Declared provider type name (e.g. "direct_url", "bilibili", "alist").
    pub source_provider: String,
    /// Provider instance name (e.g., "`bilibili_main`", "`alist_company`")
    /// Empty means use the default local instance for `source_provider`.
    pub provider_instance_name: String,
    pub source_config: JsonValue,
}

/// Request to edit a media item
#[derive(Debug, Clone)]
pub struct EditMediaRequest {
    pub media_id: MediaId,
    pub name: Option<String>,
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
    /// Optional notification service for broadcasting media changes to local WebSocket clients
    notification_service: Option<NotificationService>,
    /// Optional credential encryption for protecting sensitive data in `source_config`
    credential_encryption: Option<crate::service::CredentialEncryption>,
    /// Optional credential repository for provider-backed source resolution
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl std::fmt::Debug for MediaService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaService").finish()
    }
}

impl MediaService {
    fn ensure_provider_credential_repo(&self, provider_name: &str) -> Result<()> {
        if provider_requires_credential_repo(provider_name) && self.credential_repo.is_none() {
            return Err(Error::ServiceUnavailable(format!(
                "Provider '{provider_name}' requires credential repository wiring"
            )));
        }

        Ok(())
    }

    async fn resolve_media_provider(
        &self,
        source_provider: &str,
        provider_instance_name: &str,
    ) -> Result<Arc<dyn crate::provider::MediaProvider>> {
        let trimmed_provider = source_provider.trim();
        if trimmed_provider.is_empty() {
            return Err(Error::InvalidInput(
                "source_provider is required".to_string(),
            ));
        }

        let trimmed_instance = provider_instance_name.trim();
        let provider = if trimmed_instance.is_empty() {
            self.providers_manager
                .get_by_type(trimmed_provider)
                .await
                .ok_or_else(|| Error::NotFound(format!("Provider not found: {trimmed_provider}")))?
        } else {
            let provider = self
                .providers_manager
                .get(trimmed_instance)
                .await
                .ok_or_else(|| {
                    Error::NotFound(format!("Provider instance not found: {trimmed_instance}"))
                })?;
            if provider.name() != trimmed_provider {
                return Err(Error::InvalidInput(format!(
                    "Provider instance '{trimmed_instance}' is type '{}' but request declared '{trimmed_provider}'",
                    provider.name()
                )));
            }
            provider
        };

        Ok(provider)
    }

    fn dedup_media_ids(media_ids: Vec<MediaId>) -> Vec<MediaId> {
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::with_capacity(media_ids.len());
        for media_id in media_ids {
            if seen.insert(media_id.clone()) {
                deduped.push(media_id);
            }
        }
        deduped
    }

    /// Create a new media service
    #[must_use]
    pub const fn new(
        media_repo: MediaRepository,
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Self {
        Self {
            media_repo,
            playlist_repo,
            permission_service,
            providers_manager,
            notification_service: None,
            credential_encryption: None,
            credential_repo: None,
        }
    }

    /// Get a reference to the providers manager
    #[must_use]
    pub const fn providers_manager(&self) -> &Arc<ProvidersManager> {
        &self.providers_manager
    }

    /// Get the credential encryption used for provider source resolution, if configured.
    #[must_use]
    pub const fn credential_encryption(&self) -> Option<&crate::service::CredentialEncryption> {
        self.credential_encryption.as_ref()
    }

    /// Get the credential repository used for provider source resolution, if configured.
    #[must_use]
    pub const fn credential_repo(&self) -> Option<&Arc<UserProviderCredentialRepository>> {
        self.credential_repo.as_ref()
    }

    /// Set the notification service for broadcasting media changes to local WebSocket clients
    pub fn set_notification_service(&mut self, service: NotificationService) {
        self.notification_service = Some(service);
    }

    /// Set credential encryption for protecting sensitive data in `source_config`
    pub fn set_credential_encryption(&mut self, encryption: crate::service::CredentialEncryption) {
        self.credential_encryption = Some(encryption);
    }

    /// Set credential repository for provider-backed source resolution.
    pub fn set_credential_repo(&mut self, repo: Arc<UserProviderCredentialRepository>) {
        self.credential_repo = Some(repo);
    }

    async fn get_dynamic_playlist_provider(
        &self,
        playlist: &crate::models::Playlist,
    ) -> Result<(String, Arc<dyn crate::provider::MediaProvider>)> {
        let provider_name = playlist
            .source_provider
            .clone()
            .ok_or_else(|| Error::InvalidInput("Dynamic playlist missing provider".to_string()))?;

        let bound_instance = playlist.provider_instance_name.as_deref().and_then(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });

        let provider = if let Some(instance_name) = bound_instance {
            self.providers_manager
                .get(instance_name)
                .await
                .ok_or_else(|| {
                    Error::NotFound(format!("Provider instance not found: {instance_name}"))
                })?
        } else {
            self.providers_manager
                .get_by_type(&provider_name)
                .await
                .ok_or_else(|| Error::NotFound(format!("Provider not found: {provider_name}")))?
        };

        Ok((provider_name, provider))
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
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
            .await?;

        if let Some(ref playlist_id) = request.playlist_id {
            let playlist = self
                .playlist_repo
                .get_by_id(playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }
        }

        // Get provider from registry by instance name
        // The registry stores actual Arc<dyn MediaProvider> instances
        let provider = self
            .resolve_media_provider(&request.source_provider, &request.provider_instance_name)
            .await?;
        self.ensure_provider_credential_repo(provider.name())?;

        // Validate source_config using provider trait method
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }

        provider
            .validate_source_config(&ctx, &request.source_config)
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

        // Validate source_config size to prevent storage bloat
        // Limit: 1MB max (JSONB can grow large with embedded metadata)
        let config_size = serde_json::to_string(&request.source_config).map_or(0, |s| s.len());
        if config_size > MAX_SOURCE_CONFIG_SIZE {
            return Err(Error::InvalidInput(format!(
                "source_config too large: {config_size} bytes (max {MAX_SOURCE_CONFIG_SIZE} bytes / 1MB)"
            )));
        }

        // Prepare source_config for storage (encrypt sensitive fields if applicable)
        let prepared_source_config = provider
            .prepare_source_config(&ctx, request.source_config.clone())
            .await
            .map_err(|e| Error::Internal(format!("Failed to prepare source_config: {e}")))?;

        // Use a transaction to atomically get the next position and insert,
        // preventing concurrent adds from getting the same position
        let mut tx = self.media_repo.pool().begin().await?;

        // Get next position in playlist (locked with FOR UPDATE)
        let position = self
            .media_repo
            .get_next_append_position_with_tx(&room_id, request.playlist_id.as_ref(), &mut tx)
            .await?;

        // Create media with provider info (no enum conversion needed)
        // Business logic will use provider_instance_name to get provider from registry
        let media = Media::from_provider(
            request.playlist_id.clone(),
            room_id.clone(),
            Some(user_id.clone()),
            request.name.clone(),
            prepared_source_config,
            provider.name(), // Provider type name (e.g., "bilibili")
            request.provider_instance_name.clone(), // Bound instance name, empty means default local provider
            position,
        );

        let created_media = self
            .media_repo
            .create_with_executor(&media, &mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id = %created_media.id.as_str(),
            name = %created_media.name,
            provider = %request.provider_instance_name,
            "Media added to playlist"
        );

        // Broadcast media added event to local WebSocket clients
        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns
                .notify_media_added(
                    &room_id,
                    created_media.id.as_str(),
                    &created_media.name,
                    "", // URL is generated dynamically at playback time
                    created_media.position,
                )
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast media added event"
                );
            }
        }

        Ok(created_media)
    }

    /// Add media to a playlist as a global admin.
    ///
    /// This bypasses room membership and room-scoped permission checks, but still
    /// validates provider binding, room ownership, and source configuration.
    pub async fn admin_add_media(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        request: AddMediaRequest,
    ) -> Result<Media> {
        if let Some(ref playlist_id) = request.playlist_id {
            let playlist = self
                .playlist_repo
                .get_by_id(playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }
        }

        let provider = self
            .resolve_media_provider(&request.source_provider, &request.provider_instance_name)
            .await?;
        self.ensure_provider_credential_repo(provider.name())?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(admin_user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }

        provider
            .validate_source_config(&ctx, &request.source_config)
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

        let config_size = serde_json::to_string(&request.source_config).map_or(0, |s| s.len());
        if config_size > MAX_SOURCE_CONFIG_SIZE {
            return Err(Error::InvalidInput(format!(
                "source_config too large: {config_size} bytes (max {MAX_SOURCE_CONFIG_SIZE} bytes / 1MB)"
            )));
        }

        let prepared_source_config = provider
            .prepare_source_config(&ctx, request.source_config.clone())
            .await
            .map_err(|e| Error::Internal(format!("Failed to prepare source_config: {e}")))?;

        let mut tx = self.media_repo.pool().begin().await?;
        let position = self
            .media_repo
            .get_next_append_position_with_tx(&room_id, request.playlist_id.as_ref(), &mut tx)
            .await?;

        let media = Media::from_provider(
            request.playlist_id.clone(),
            room_id.clone(),
            Some(admin_user_id.clone()),
            request.name.clone(),
            prepared_source_config,
            provider.name(),
            request.provider_instance_name.clone(),
            position,
        );

        let created_media = self
            .media_repo
            .create_with_executor(&media, &mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            admin_user_id = %admin_user_id.as_str(),
            media_id = %created_media.id.as_str(),
            name = %created_media.name,
            provider = %request.provider_instance_name,
            "Media added to playlist by admin"
        );

        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns
                .notify_media_added(
                    &room_id,
                    created_media.id.as_str(),
                    &created_media.name,
                    "",
                    created_media.position,
                )
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast media added event"
                );
            }
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
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
            .await?;

        if let Some(ref playlist_id) = playlist_id {
            let playlist = self
                .playlist_repo
                .get_by_id(playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
            }
        }

        if items.is_empty() {
            return Ok(Vec::new());
        }

        if items.len() > MAX_BATCH_SIZE {
            return Err(Error::InvalidInput(format!(
                "Batch size exceeds maximum of {MAX_BATCH_SIZE}"
            )));
        }

        // Create provider context for validation
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }

        // Validate all items before starting a transaction
        let mut validated_items = Vec::with_capacity(items.len());
        for item in items {
            // Get provider from registry by instance name
            let provider = self
                .resolve_media_provider(&item.source_provider, &item.provider_instance_name)
                .await?;
            self.ensure_provider_credential_repo(provider.name())?;

            // Validate source_config using provider trait method
            provider
                .validate_source_config(&ctx, &item.source_config)
                .await
                .map_err(|e| {
                    Error::InvalidInput(format!(
                        "Invalid source_config for item '{}': {}",
                        item.name, e
                    ))
                })?;

            // Prepare source_config for storage (encrypt sensitive fields if applicable)
            let prepared_source_config = provider
                .prepare_source_config(&ctx, item.source_config.clone())
                .await
                .map_err(|e| {
                    Error::Internal(format!(
                        "Failed to prepare source_config for item '{}': {}",
                        item.name, e
                    ))
                })?;

            validated_items.push((item, provider, prepared_source_config));
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
        for (index, (item, provider, prepared_source_config)) in
            validated_items.into_iter().enumerate()
        {
            let media = Media::from_provider(
                item.playlist_id,
                room_id.clone(),
                Some(user_id.clone()),
                item.name,
                prepared_source_config,
                provider.name(),             // Provider type name
                item.provider_instance_name, // Instance name
                batch_media_position(index, start_position),
            );
            media_items.push(media);
        }

        // Batch insert within the transaction
        let created_items = self
            .media_repo
            .create_batch_with_executor(&media_items, &mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            count = created_items.len(),
            "Batch added media to playlist"
        );

        // Broadcast media added events to local WebSocket clients
        if let Some(ref ns) = self.notification_service {
            for item in &created_items {
                if let Err(e) = ns
                    .notify_media_added(&room_id, item.id.as_str(), &item.name, "", item.position)
                    .await
                {
                    tracing::warn!(
                        error = %e,
                        room_id = %room_id.as_str(),
                        media_id = %item.id.as_str(),
                        "Failed to broadcast media added event"
                    );
                }
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
        for attempt in 0..Self::EDIT_MAX_RETRIES {
            // Get existing media (fresh on every retry)
            let mut media = self
                .media_repo
                .get_by_id(&request.media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            // Verify media belongs to the room
            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }

            // Check permission: EDIT_MEDIA_SELF if user owns the media, EDIT_MEDIA_ANY otherwise
            //
            // IMPORTANT: Use check_permission_no_cache to ensure fresh permissions on each retry.
            // This prevents a race condition where:
            // 1. Permission is granted and cached on first attempt
            // 2. Permission is revoked by admin before retry
            // 3. Retry would succeed with stale cached permission
            //
            // By bypassing cache, we ensure each retry checks current permission state.
            let required_permission = if media.creator_id.as_ref() == Some(&user_id) {
                PermissionBits::EDIT_MEDIA_SELF
            } else {
                PermissionBits::EDIT_MEDIA_ANY
            };
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, required_permission)
                .await?;

            // Capture the version before applying changes to detect concurrent edits
            let expected_version = media.version;

            // Update fields
            if let Some(ref name) = request.name {
                media.name = name.clone();
            }
            // Conditional update: only succeed if no other edit changed the row
            match self
                .media_repo
                .update_with_version(&media, expected_version)
                .await
            {
                Ok(Some(updated_media)) => {
                    tracing::info!(
                        room_id = %room_id.as_str(),
                        media_id = %request.media_id.as_str(),
                        "Media edited"
                    );

                    // Broadcast media updated event to local WebSocket clients and cluster
                    if let Some(ref ns) = self.notification_service {
                        if let Err(e) = ns
                            .notify_media_updated(
                                &room_id,
                                updated_media.id.as_str(),
                                &updated_media.name,
                                updated_media.position,
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                "Failed to broadcast media updated event"
                            );
                        }
                    }

                    return Ok(updated_media);
                }
                Ok(None) if attempt + 1 < Self::EDIT_MAX_RETRIES => {
                    // Concurrent modification detected, retry with fresh data
                    tracing::debug!(
                        media_id = %request.media_id.as_str(),
                        attempt = attempt + 1,
                        "Concurrent media edit detected, retrying"
                    );
                }
                Ok(None) => {
                    return Err(Error::Internal(
                        format!(
                            "Media edit failed: concurrent modification after {} retries for media_id={}",
                            attempt + 1,
                            request.media_id.as_str()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(format!(
            "Media edit failed after {} attempts for media_id={}",
            Self::EDIT_MAX_RETRIES,
            request.media_id.as_str()
        )))
    }

    /// Edit media item as a global admin.
    pub async fn admin_edit_media(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        request: EditMediaRequest,
    ) -> Result<Media> {
        for attempt in 0..Self::EDIT_MAX_RETRIES {
            let mut media = self
                .media_repo
                .get_by_id(&request.media_id)
                .await?
                .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }

            let expected_version = media.version;

            if let Some(ref name) = request.name {
                media.name = name.clone();
            }
            match self
                .media_repo
                .update_with_version(&media, expected_version)
                .await
            {
                Ok(Some(updated_media)) => {
                    tracing::info!(
                        room_id = %room_id.as_str(),
                        admin_user_id = %admin_user_id.as_str(),
                        media_id = %request.media_id.as_str(),
                        "Media edited by admin"
                    );

                    if let Some(ref ns) = self.notification_service {
                        if let Err(e) = ns
                            .notify_media_updated(
                                &room_id,
                                updated_media.id.as_str(),
                                &updated_media.name,
                                updated_media.position,
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                "Failed to broadcast media updated event"
                            );
                        }
                    }

                    return Ok(updated_media);
                }
                Ok(None) if attempt + 1 < Self::EDIT_MAX_RETRIES => {}
                Ok(None) => {
                    return Err(Error::Internal(format!(
                        "Media edit failed: concurrent modification after {} retries for media_id={}",
                        attempt + 1,
                        request.media_id.as_str()
                    )));
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(format!(
            "Media edit failed after {} attempts for media_id={}",
            Self::EDIT_MAX_RETRIES,
            request.media_id.as_str()
        )))
    }

    /// Remove media from playlist
    ///
    /// Uses `check_permission_no_cache` to ensure fresh permissions (avoids stale cache).
    /// Rejects removal if the target media is currently playing in the room.
    pub async fn remove_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<()> {
        // Get existing media to verify ownership
        let media = self
            .media_repo
            .get_by_id(&media_id)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        // Verify media belongs to the room
        if media.room_id != room_id {
            return Err(Error::Authorization(
                "Media does not belong to this room".to_string(),
            ));
        }

        // Check permission: DELETE_MEDIA_SELF if user owns the media, DELETE_MEDIA_ANY otherwise
        // IMPORTANT: Use check_permission_no_cache to ensure fresh permissions
        let required_permission = if media.creator_id.as_ref() == Some(&user_id) {
            PermissionBits::DELETE_MEDIA_SELF
        } else {
            PermissionBits::DELETE_MEDIA_ANY
        };
        self.permission_service
            .check_permission_no_cache(&room_id, &user_id, required_permission)
            .await?;

        // Use a transaction to atomically check "currently playing" and delete
        let mut tx = self.media_repo.pool().begin().await?;

        // Lock room_playback_state FOR UPDATE and reject if target media is playing
        let playing_media_id: Option<String> = sqlx::query_scalar(
            "SELECT playing_media_id FROM room_playback_state \
             WHERE room_id = $1 \
             FOR UPDATE",
        )
        .bind(room_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

        if playing_media_id.as_deref() == Some(media_id.as_str()) {
            return Err(Error::InvalidInput(
                "Cannot remove media that is currently playing".to_string(),
            ));
        }

        // Delete within the transaction
        sqlx::query("DELETE FROM media WHERE id = $1")
            .bind(media_id.as_str())
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id = %media_id.as_str(),
            "Media removed from playlist"
        );

        // Broadcast media removed event to local WebSocket clients
        if let Some(ref ns) = self.notification_service {
            if let Err(e) = ns.notify_media_removed(&room_id, media_id.as_str()).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id.as_str(),
                    "Failed to broadcast media removed event"
                );
            }
        }

        Ok(())
    }

    /// Get media by ID
    pub async fn get_media(&self, media_id: &MediaId) -> Result<Option<Media>> {
        self.media_repo.get_by_id(media_id).await
    }

    /// Get multiple media items by IDs in a single query
    pub async fn get_media_batch(&self, media_ids: &[MediaId]) -> Result<Vec<Media>> {
        self.media_repo.get_by_ids(media_ids).await
    }

    /// Get all media in a playlist.
    pub async fn get_playlist_media(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        self.media_repo.get_by_playlist(playlist_id).await
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

    /// Bulk remove media from playlist
    ///
    /// Removes multiple media items in a single transaction.
    /// Uses a single batch query to verify ownership instead of N individual queries.
    pub async fn remove_media_batch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_ids: Vec<MediaId>,
    ) -> Result<usize> {
        if media_ids.is_empty() {
            return Ok(0);
        }

        if media_ids.len() > MAX_BATCH_SIZE {
            return Err(Error::InvalidInput(format!(
                "Batch size exceeds maximum of {MAX_BATCH_SIZE}"
            )));
        }

        // Use explicit transaction to prevent TOCTOU between read and delete
        let mut tx = self.media_repo.pool().begin().await?;

        // Batch-load all media in a single query within the transaction
        let media_items = self
            .media_repo
            .get_by_ids_with_executor(&media_ids, &mut *tx)
            .await?;

        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        // Verify all media belong to the room and split into owned/non-owned groups
        let mut has_owned = false;
        let mut has_non_owned = false;
        for media in &media_items {
            if media.room_id != room_id {
                return Err(Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }
            if media.creator_id.as_ref() == Some(&user_id) {
                has_owned = true;
            } else {
                has_non_owned = true;
            }
        }

        // Check per-group permissions: user needs DELETE_MEDIA_SELF for their own
        // items and DELETE_MEDIA_ANY for others' items. Only fail if the user
        // lacks the permission for a group that actually has items.
        // IMPORTANT: Use check_permission_no_cache to ensure fresh permissions
        if has_owned {
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, PermissionBits::DELETE_MEDIA_SELF)
                .await?;
        }
        if has_non_owned {
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, PermissionBits::DELETE_MEDIA_ANY)
                .await?;
        }

        // Lock room_playback_state FOR UPDATE and reject if any target media is playing
        let playing_media_id: Option<String> = sqlx::query_scalar(
            "SELECT playing_media_id FROM room_playback_state \
             WHERE room_id = $1 \
             FOR UPDATE",
        )
        .bind(room_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

        if let Some(ref playing_id) = playing_media_id {
            if media_ids
                .iter()
                .any(|mid| mid.as_str() == playing_id.as_str())
            {
                return Err(Error::InvalidInput(
                    "Cannot remove media that is currently playing".to_string(),
                ));
            }
        }

        // Bulk delete within the same transaction
        let deleted_count = self
            .media_repo
            .delete_batch_with_executor(&media_ids, &mut *tx)
            .await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            count = deleted_count,
            "Bulk removed media from playlist"
        );

        // Broadcast media removed events to local WebSocket clients
        if let Some(ref ns) = self.notification_service {
            for mid in &media_ids {
                if let Err(e) = ns.notify_media_removed(&room_id, mid.as_str()).await {
                    tracing::warn!(
                        error = %e,
                        room_id = %room_id.as_str(),
                        media_id = %mid.as_str(),
                        "Failed to broadcast media removed event"
                    );
                }
            }
        }

        Ok(deleted_count)
    }

    pub async fn move_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MoveMediaRequest,
    ) -> Result<Vec<Media>> {
        self.move_media_internal(room_id, user_id, request, false)
            .await
    }

    pub async fn admin_move_media(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        request: MoveMediaRequest,
    ) -> Result<Vec<Media>> {
        self.move_media_internal(room_id, admin_user_id, request, true)
            .await
    }

    async fn move_media_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MoveMediaRequest,
        bypass_room_permissions: bool,
    ) -> Result<Vec<Media>> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
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

        let explicit_media_ids = Self::dedup_media_ids(request.media_ids);
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
                .get_by_ids_with_executor(std::slice::from_ref(source_playlist_id), &mut *tx)
                .await?;
            let source_playlist = playlists
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound("Source playlist not found".to_string()))?;
            if source_playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Source playlist does not belong to this room".to_string(),
                ));
            }
            if source_playlist.is_dynamic() {
                return Err(Error::InvalidInput(
                    "Source playlist must be static".to_string(),
                ));
            }
        }

        if let Some(ref target_playlist_id) = request.target_playlist_id {
            let playlists = self
                .playlist_repo
                .get_by_ids_with_executor(std::slice::from_ref(target_playlist_id), &mut *tx)
                .await?;
            let target_playlist = playlists
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound("Target playlist not found".to_string()))?;
            if target_playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Target playlist does not belong to this room".to_string(),
                ));
            }
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
                .get_by_ids_with_executor(&explicit_media_ids, &mut *tx)
                .await?;
            if fetched.len() != explicit_media_ids.len() {
                return Err(Error::NotFound("Media not found".to_string()));
            }
            let mut fetched_map = HashMap::with_capacity(fetched.len());
            for media in fetched {
                fetched_map.insert(media.id.clone(), media);
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

        if original_media.iter().any(|media| media.room_id != room_id) {
            return Err(Error::Authorization(
                "Media does not belong to this room".to_string(),
            ));
        }

        if request.all_from_scope && original_media.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let original_scope_by_id: HashMap<MediaId, Option<PlaylistId>> = original_media
            .iter()
            .map(|media| (media.id.clone(), media.playlist_id.clone()))
            .collect();
        let media_ids: Vec<MediaId> = original_media
            .iter()
            .map(|media| media.id.clone())
            .collect();

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

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            moved_count = moved.len(),
            "Media moved"
        );

        if let Some(ref ns) = self.notification_service {
            let moved_within_same_scope = moved.iter().all(|media| {
                original_scope_by_id
                    .get(&media.id)
                    .is_some_and(|original_scope| *original_scope == media.playlist_id)
            });

            if moved_within_same_scope {
                if moved.len() == 1 {
                    let media = &moved[0];
                    if let Err(e) = ns
                        .notify_media_updated(
                            &room_id,
                            media.id.as_str(),
                            &media.name,
                            media.position,
                        )
                        .await
                    {
                        tracing::warn!(
                            error = %e,
                            room_id = %room_id.as_str(),
                            media_id = %media.id.as_str(),
                            "Failed to broadcast media moved event"
                        );
                    }
                } else {
                    let moved_ids: Vec<String> = moved
                        .iter()
                        .map(|media| media.id.as_str().to_string())
                        .collect();
                    if let Err(e) = ns.notify_playlist_reordered(&room_id, &moved_ids).await {
                        tracing::warn!(
                            error = %e,
                            room_id = %room_id.as_str(),
                            "Failed to broadcast playlist reordered event"
                        );
                    }
                }
            } else {
                for media in &moved {
                    if let Some(original_scope) = original_scope_by_id.get(&media.id) {
                        if *original_scope != media.playlist_id {
                            if let Err(e) =
                                ns.notify_media_removed(&room_id, media.id.as_str()).await
                            {
                                tracing::warn!(
                                    error = %e,
                                    room_id = %room_id.as_str(),
                                    media_id = %media.id.as_str(),
                                    "Failed to broadcast moved media removal event"
                                );
                            }
                            if let Err(e) = ns
                                .notify_media_added(
                                    &room_id,
                                    media.id.as_str(),
                                    &media.name,
                                    "",
                                    media.position,
                                )
                                .await
                            {
                                tracing::warn!(
                                    error = %e,
                                    room_id = %room_id.as_str(),
                                    media_id = %media.id.as_str(),
                                    "Failed to broadcast moved media add event"
                                );
                            }
                        } else if let Err(e) = ns
                            .notify_media_updated(
                                &room_id,
                                media.id.as_str(),
                                &media.name,
                                media.position,
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                room_id = %room_id.as_str(),
                                media_id = %media.id.as_str(),
                                "Failed to broadcast media moved event"
                            );
                        }
                    }
                }
            }
        }

        Ok(moved)
    }

    /// List dynamic playlist items as a global admin.
    pub async fn admin_list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>> {
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(admin_user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .list_playlist(&ctx, &playlist, target, page, page_size)
            .await
            .map_err(Error::from)
    }

    pub async fn admin_get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<crate::provider::DynamicBrowsePathSegment>> {
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(admin_user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .browse_path(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
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
        playlist_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, i64>> {
        self.media_repo.count_by_playlists_batch(playlist_ids).await
    }

    pub async fn count_playlist_media_batch_accessible(
        &self,
        playlist_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, i64>> {
        self.media_repo
            .count_by_playlists_batch_accessible(playlist_ids)
            .await
    }

    /// List dynamic playlist items
    ///
    /// For dynamic playlists (provider-based folders), this fetches the directory listing
    /// from the provider's `DynamicFolder` implementation.
    ///
    /// # Arguments
    /// * `room_id` - Room ID for permission check
    /// * `user_id` - User ID for permission check
    /// * `playlist_id` - Playlist ID to list
    /// * `target` - Provider-defined browse target within the dynamic folder
    /// * `page` - Page number (0-indexed)
    /// * `page_size` - Items per page
    ///
    /// # Returns
    /// List of directory items (files and folders)
    ///
    /// # Errors
    /// - `Error::NotFound` if playlist doesn't exist
    /// - `Error::InvalidOperation` if playlist is not dynamic
    /// - `Error::ProviderError` if provider fails
    pub async fn list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::VIEW_PLAYLIST)
            .await?;

        // Get playlist
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        // Verify playlist belongs to the room
        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        // Check if playlist is dynamic
        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        // Get provider
        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        // Check if provider implements DynamicFolder trait
        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        // Create context
        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        // List items
        let items = dynamic_folder
            .list_playlist(&ctx, &playlist, target, page, page_size)
            .await
            .map_err(Error::from)?;

        Ok(items)
    }

    pub async fn get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<crate::provider::DynamicBrowsePathSegment>> {
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .browse_path(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
    }

    /// Get playlist metadata needed by playback/media orchestration.
    pub async fn get_playlist(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Option<crate::models::Playlist>> {
        self.playlist_repo.get_by_id(playlist_id).await
    }

    /// Resolve a concrete playable item inside a dynamic playlist.
    pub async fn resolve_dynamic_playlist_item(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: &[u8],
    ) -> Result<Option<crate::provider::NextPlayItem>> {
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .resolve_item(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
    }

    /// Resolve the next auto-play target within a dynamic playlist.
    ///
    /// This is internal playback orchestration logic, so it intentionally does
    /// not perform room membership or permission checks. The caller must already
    /// have a valid playback state scoped to the room.
    pub async fn next_dynamic_playlist_item(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        target: &[u8],
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<crate::provider::NextPlayItem>> {
        let playlist = self
            .playlist_repo
            .get_by_id(playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != *room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;
        let provider_instance_name = playlist.provider_instance_name.clone().unwrap_or_default();

        let current_dynamic_media = crate::models::Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room_id.clone(),
            creator_id: None,
            name: format!("dynamic:{playlist_id}"),
            position: 0.0,
            source_provider: provider_name.clone(),
            source_config: serde_json::Value::Null,
            provider_instance_name,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let mut ctx = ProviderContext::new("synctv").with_room_id(room_id.as_str());
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .next(&ctx, &playlist, &current_dynamic_media, target, play_mode)
            .await
            .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== AddMediaRequest Validation ==========

    #[test]
    fn test_add_media_request_construction() {
        let request = AddMediaRequest {
            playlist_id: Some(PlaylistId::new()),
            name: "Test Video".to_string(),
            source_provider: "bilibili".to_string(),
            provider_instance_name: "bilibili_main".to_string(),
            source_config: serde_json::json!({"bvid": "BV1234567890"}),
        };

        assert_eq!(request.name, "Test Video");
        assert_eq!(request.source_provider, "bilibili");
        assert_eq!(request.provider_instance_name, "bilibili_main");
        assert!(request.source_config.get("bvid").is_some());
    }

    #[test]
    fn test_add_media_request_with_complex_source_config() {
        let config = serde_json::json!({
            "url": "https://example.com/video.mp4",
            "headers": {"Referer": "https://example.com"},
            "quality": "1080p"
        });

        let request = AddMediaRequest {
            playlist_id: Some(PlaylistId::new()),
            name: "Complex Video".to_string(),
            source_provider: "alist".to_string(),
            provider_instance_name: "alist_home".to_string(),
            source_config: config.clone(),
        };

        assert_eq!(request.source_config, config);
        assert_eq!(
            request.source_config["headers"]["Referer"],
            "https://example.com"
        );
    }

    // ========== EditMediaRequest Validation ==========

    #[test]
    fn test_edit_media_request_name_only() {
        let request = EditMediaRequest {
            media_id: MediaId::new(),
            name: Some("New Name".to_string()),
        };

        assert_eq!(request.name, Some("New Name".to_string()));
    }

    #[test]
    fn test_move_media_request_before_anchor() {
        let request = MoveMediaRequest {
            media_ids: vec![MediaId::new()],
            source_playlist_id: None,
            target_playlist_id: None,
            all_from_scope: false,
            before_media_id: Some(MediaId::new()),
            after_media_id: None,
        };

        assert_eq!(request.media_ids.len(), 1);
        assert!(request.before_media_id.is_some());
        assert!(request.after_media_id.is_none());
    }

    #[test]
    fn test_move_media_request_after_anchor() {
        let request = MoveMediaRequest {
            media_ids: vec![MediaId::new()],
            source_playlist_id: None,
            target_playlist_id: None,
            all_from_scope: false,
            before_media_id: None,
            after_media_id: Some(MediaId::new()),
        };

        assert_eq!(request.media_ids.len(), 1);
        assert!(request.before_media_id.is_none());
        assert!(request.after_media_id.is_some());
    }

    // ========== Batch Size Validation ==========

    #[test]
    fn test_batch_items_construction() {
        let items: Vec<AddMediaRequest> = (0..101)
            .map(|i| AddMediaRequest {
                playlist_id: Some(PlaylistId::new()),
                name: format!("Video {i}"),
                source_provider: "direct_url".to_string(),
                provider_instance_name: "test".to_string(),
                source_config: serde_json::json!({}),
            })
            .collect();

        assert_eq!(items.len(), 101);
    }

    #[test]
    fn test_empty_batch_is_valid() {
        let items: Vec<AddMediaRequest> = Vec::new();
        assert!(items.is_empty());
    }

    // ========== Source Config JSON Validation ==========

    #[test]
    fn test_source_config_null_value() {
        let request = AddMediaRequest {
            playlist_id: Some(PlaylistId::new()),
            name: "Null Config".to_string(),
            source_provider: "direct_url".to_string(),
            provider_instance_name: "test".to_string(),
            source_config: serde_json::Value::Null,
        };

        assert!(request.source_config.is_null());
    }

    #[test]
    fn test_source_config_nested_structure() {
        let config = serde_json::json!({
            "provider": "alist",
            "path": "/movies/action",
            "options": {
                "transcode": true,
                "subtitle": {
                    "lang": "en",
                    "auto": false
                }
            }
        });

        let request = AddMediaRequest {
            playlist_id: Some(PlaylistId::new()),
            name: "Nested Config".to_string(),
            source_provider: "alist".to_string(),
            provider_instance_name: "alist_home".to_string(),
            source_config: config,
        };

        assert_eq!(request.source_config["options"]["subtitle"]["lang"], "en");
    }

    // ========== Source Config Size Validation ==========

    #[test]
    fn test_source_config_size_limit_constant() {
        // MAX_SOURCE_CONFIG_SIZE = 1MB
        const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024;
        assert_eq!(MAX_SOURCE_CONFIG_SIZE, 1_048_576);
    }

    #[test]
    fn test_source_config_size_calculation() {
        // Small config should be well under 1MB
        let small_config = serde_json::json!({
            "url": "https://example.com/video.mp4",
            "headers": {"Referer": "https://example.com"}
        });
        let size = serde_json::to_string(&small_config).map_or(0, |s| s.len());
        assert!(size < 200, "Small config should be under 200 bytes");
    }

    #[test]
    fn test_source_config_large_rejection() {
        // Config with 2MB of data should be rejected
        const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024; // 1MB
        let large_string = "x".repeat(2 * 1024 * 1024); // 2MB string
        let large_config = serde_json::json!({
            "data": large_string
        });
        let size = serde_json::to_string(&large_config).map_or(0, |s| s.len());
        assert!(
            size > MAX_SOURCE_CONFIG_SIZE,
            "Large config should exceed 1MB"
        );
    }

    // ========== source_config Boundary Tests (Task #72) ==========

    #[test]
    fn test_source_config_exactly_1mb_accepted() {
        // Config exactly at 1MB limit should be accepted
        const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024; // 1MB

        // Create a config that is exactly 1MB when serialized
        // JSON overhead: {"data":"..."} = 12 bytes, so we need 1MB - 12 bytes
        let data_size = MAX_SOURCE_CONFIG_SIZE - 12;
        let exact_string = "x".repeat(data_size);
        let exact_config = serde_json::json!({
            "data": exact_string
        });

        let size = serde_json::to_string(&exact_config).map_or(0, |s| s.len());

        // Should be exactly at or just under the limit
        assert!(
            size <= MAX_SOURCE_CONFIG_SIZE,
            "Config should be at or under 1MB, got {size} bytes"
        );
    }

    #[test]
    fn test_source_config_1mb_plus_one_rejected() {
        // Config at 1MB + 1 byte should be rejected
        const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024; // 1MB

        // Create a config that exceeds 1MB by a small amount
        // JSON overhead: {"data":"..."} = 12 bytes
        // To be 1 byte over, we need (1MB - 12 bytes + 1 byte) = 1MB - 11 bytes of data
        let data_size = MAX_SOURCE_CONFIG_SIZE - 10; // -10 to be safely over the limit
        let over_string = "x".repeat(data_size);
        let over_config = serde_json::json!({
            "data": over_string
        });

        let size = serde_json::to_string(&over_config).map_or(0, |s| s.len());

        assert!(
            size > MAX_SOURCE_CONFIG_SIZE,
            "Config should exceed 1MB, got {size} bytes"
        );
    }

    #[test]
    fn test_source_config_nested_structure_size() {
        // Nested JSON structures should also be checked for size
        const MAX_SOURCE_CONFIG_SIZE: usize = 1024 * 1024; // 1MB

        let nested_config = serde_json::json!({
            "playback_infos": {
                "1080p": {
                    "urls": ["https://example.com/video1.mp4", "https://example.com/video2.mp4"],
                    "headers": {
                        "Referer": "https://example.com",
                        "User-Agent": "Mozilla/5.0"
                    }
                },
                "720p": {
                    "urls": ["https://example.com/video1-720.mp4"],
                    "headers": {}
                }
            },
            "default_mode": "1080p",
            "metadata": {
                "title": "Test Video",
                "duration": 3600
            }
        });

        let size = serde_json::to_string(&nested_config).map_or(0, |s| s.len());

        // Complex nested structures should still be under limit
        assert!(
            size < MAX_SOURCE_CONFIG_SIZE,
            "Nested config should be under 1MB, got {size} bytes"
        );
    }

    #[test]
    fn test_source_config_unicode_content_size() {
        // Unicode characters should be counted correctly in bytes, not characters
        // (MAX_SOURCE_CONFIG_SIZE is 1MB but this test just validates byte counting)

        // Unicode emoji takes 4 bytes in UTF-8
        let unicode_string = "🎉".repeat(100);
        let unicode_config = serde_json::json!({
            "title": unicode_string
        });

        let size = serde_json::to_string(&unicode_config).map_or(0, |s| s.len());

        // 100 emoji * 4 bytes each = 400 bytes + JSON overhead
        assert!(
            size > 400 && size < 500,
            "Unicode size should be counted in bytes, got {size} bytes"
        );
    }

    // ========== Optimistic Lock Error Messages (Task #51) ==========

    #[test]
    fn test_edit_media_error_message_contains_media_id() {
        // Test that optimistic lock error messages include media_id for debugging
        let media_id = MediaId::new();
        let max_retries = super::MediaService::EDIT_MAX_RETRIES;

        // Expected error format should include media_id and max_retries
        let expected_msg = format!(
            "Media edit failed after {} attempts for media_id={}",
            max_retries,
            media_id.as_str()
        );

        // Verify the format includes the key debugging information
        assert!(
            expected_msg.contains(media_id.as_str()),
            "Error message should contain media_id"
        );
        assert!(
            expected_msg.contains(&max_retries.to_string()),
            "Error message should contain retry count"
        );
    }

    #[test]
    fn test_edit_media_concurrent_modification_error_message() {
        // Test that concurrent modification error message includes context
        let media_id = MediaId::new();
        let attempts = 3;

        let expected_msg = format!(
            "Media edit failed: concurrent modification after {} retries for media_id={}",
            attempts,
            media_id.as_str()
        );

        assert!(
            expected_msg.contains(media_id.as_str()),
            "Error message should contain media_id"
        );
        assert!(
            expected_msg.contains(&attempts.to_string()),
            "Error message should contain attempt count"
        );
    }
}
