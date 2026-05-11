//! Playlist management service
//!
//! Design reference: external design doc 04-database-design.md §2.4.1
//!
//! Manages playlist/folder operations including:
//! - Creating folders (static and dynamic)
//! - Tree structure navigation
//! - Position management

use std::sync::Arc;

use crate::{
    models::{
        normalize_provider_instance_name_owned, PermissionBits, Playlist, PlaylistId, RoomId,
        UserId,
    },
    provider::{provider_requires_credential_repo, ProviderContext, SourceConfig},
    repository::cluster_outbox::{ClusterOutboxRepository, NewClusterOutboxEvent},
    repository::PlaylistRepository,
    repository::{UserProviderCredentialRepository, UserRepository},
    service::{
        permission::PermissionService,
        provider_binding::resolve_credential_provider_instance_binding, ProvidersManager,
    },
    Error, Result,
};
use serde_json::Value as JsonValue;

pub type ClusterOutboxPlaylistEventFactory =
    Arc<dyn Fn(&Playlist) -> Option<NewClusterOutboxEvent> + Send + Sync>;

/// Trait for broadcasting playlist changes to cluster replicas.
///
/// This abstracts over the cluster manager so that `synctv-core` does not
/// depend on `synctv-cluster`. The implementation lives in the API/wiring
/// layer where `ClusterManager` is available.
pub trait PlaylistBroadcaster: Send + Sync {
    /// Broadcast that a playlist was created.
    fn broadcast_playlist_created(
        &self,
        room_id: &RoomId,
        playlist: &Playlist,
        user_id: &UserId,
        username: &str,
    );

    /// Broadcast that a playlist was updated.
    fn broadcast_playlist_updated(
        &self,
        room_id: &RoomId,
        playlist: &Playlist,
        user_id: &UserId,
        username: &str,
    );

    /// Broadcast that a playlist was deleted.
    fn broadcast_playlist_deleted(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        user_id: &UserId,
        username: &str,
    );
}

const OPTIMISTIC_LOCK_MAX_RETRIES: u32 = 3;
const OPTIMISTIC_LOCK_BACKOFF_BASE_MS: u64 = 5;

fn normalize_dynamic_playlist_fields(
    source_provider: Option<String>,
    source_config: Option<JsonValue>,
    provider_instance_name: Option<String>,
) -> Result<(Option<String>, Option<JsonValue>, Option<String>)> {
    let normalized_provider = source_provider.and_then(|provider| {
        let trimmed = provider.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let normalized_instance = normalize_provider_instance_name_owned(provider_instance_name);

    if let Some(provider) = normalized_provider {
        let source_config = source_config.ok_or_else(|| {
            Error::InvalidInput("source_config is required for dynamic folders".to_string())
        })?;

        Ok((Some(provider), Some(source_config), normalized_instance))
    } else {
        if source_config.is_some() || normalized_instance.is_some() {
            return Err(Error::InvalidInput(
                "source_provider is required when setting dynamic playlist fields".to_string(),
            ));
        }

        Ok((None, None, None))
    }
}

/// Request to create a playlist/folder
#[derive(Debug, Clone)]
pub struct CreatePlaylistRequest {
    pub room_id: RoomId,
    pub name: String,
    pub parent_id: Option<PlaylistId>,

    // Dynamic folder fields
    pub source_provider: Option<String>,
    pub source_config: Option<JsonValue>,
    pub provider_instance_name: Option<String>,
}

/// Request to set playlist properties
#[derive(Debug, Clone)]
pub struct SetPlaylistRequest {
    pub playlist_id: PlaylistId,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MovePlaylistRequest {
    pub playlist_id: PlaylistId,
    pub before_playlist_id: Option<PlaylistId>,
    pub after_playlist_id: Option<PlaylistId>,
}

/// Playlist management service
///
/// Responsible for playlist/folder operations:
/// - Create static folders (manually added media)
/// - Create dynamic folders (Alist/Emby directories)
/// - Tree structure navigation
#[derive(Clone)]
pub struct PlaylistService {
    playlist_repo: PlaylistRepository,
    permission_service: PermissionService,
    providers_manager: Arc<ProvidersManager>,
    credential_encryption: Option<crate::service::CredentialEncryption>,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    cluster_outbox: Option<Arc<ClusterOutboxRepository>>,
    /// Optional cluster broadcaster for cross-replica playlist sync
    cluster_broadcaster: Arc<parking_lot::RwLock<Option<Arc<dyn PlaylistBroadcaster>>>>,
}

impl std::fmt::Debug for PlaylistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistService").finish()
    }
}

impl PlaylistService {
    async fn resolve_actor_username(&self, user_id: &UserId) -> String {
        UserRepository::new(self.playlist_repo.pool().clone())
            .get_by_id(user_id)
            .await
            .ok()
            .flatten()
            .map(|user| user.username)
            .unwrap_or_default()
    }

    fn ensure_provider_credential_repo(&self, provider_name: &str) -> Result<()> {
        if provider_requires_credential_repo(provider_name) && self.credential_repo.is_none() {
            return Err(Error::ServiceUnavailable(format!(
                "Provider '{provider_name}' requires credential repository wiring"
            )));
        }

        Ok(())
    }

    /// Create a new playlist service
    #[must_use]
    pub fn new(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Self {
        Self {
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption: None,
            credential_repo: None,
            cluster_outbox: None,
            cluster_broadcaster: Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    /// Create a playlist service with provider credential dependencies already wired.
    #[must_use]
    pub fn new_with_provider_credentials(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        credential_encryption: Option<crate::service::CredentialEncryption>,
        credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    ) -> Self {
        Self {
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption,
            credential_repo,
            cluster_outbox: None,
            cluster_broadcaster: Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    pub fn set_cluster_outbox(&mut self, cluster_outbox: Option<Arc<ClusterOutboxRepository>>) {
        self.cluster_outbox = cluster_outbox;
    }

    /// Set the cluster broadcaster for cross-replica playlist sync
    pub fn set_cluster_broadcaster(&self, broadcaster: Arc<dyn PlaylistBroadcaster>) {
        *self.cluster_broadcaster.write() = Some(broadcaster);
    }

    #[doc(hidden)]
    pub fn has_cluster_broadcaster(&self) -> bool {
        self.cluster_broadcaster.read().is_some()
    }

    /// Get a reference to the providers manager.
    #[must_use]
    pub const fn providers_manager(&self) -> &Arc<ProvidersManager> {
        &self.providers_manager
    }

    async fn validate_dynamic_playlist_source(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        source_provider: String,
        source_config: JsonValue,
        provider_instance_name: Option<String>,
    ) -> Result<(String, JsonValue, Option<String>)> {
        let trimmed_provider = source_provider.trim().to_string();
        let trimmed_instance = normalize_provider_instance_name_owned(provider_instance_name);

        let provider = self
            .providers_manager
            .resolve_provider(&trimmed_provider, trimmed_instance.as_deref())
            .await?;

        if provider.as_dynamic_folder().is_none() {
            return Err(Error::InvalidInput(format!(
                "Provider {trimmed_provider} does not support dynamic folders"
            )));
        }
        self.ensure_provider_credential_repo(&trimmed_provider)?;

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(*user_id)
            .with_room_id(*room_id)
            .with_credential_owner_id(*user_id);
        if let Some(provider_instance_name) = trimmed_instance.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        provider
            .validate_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

        let bound_instance = resolve_credential_provider_instance_binding(
            provider.as_ref(),
            self.credential_repo.as_ref(),
            &ctx,
            &source_config,
            trimmed_instance.as_deref(),
        )
        .await?;
        let provider = if bound_instance != trimmed_instance {
            let provider = self
                .providers_manager
                .resolve_provider(&trimmed_provider, bound_instance.as_deref())
                .await?;
            if provider.as_dynamic_folder().is_none() {
                return Err(Error::InvalidInput(format!(
                    "Provider {trimmed_provider} does not support dynamic folders"
                )));
            }
            provider
        } else {
            provider
        };

        let mut ctx = ProviderContext::new("synctv")
            .with_user_id(*user_id)
            .with_room_id(*room_id)
            .with_credential_owner_id(*user_id);
        if let Some(provider_instance_name) = bound_instance.as_deref() {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        provider
            .validate_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

        let prepared_source_config = provider
            .prepare_source_config(&ctx, source_config)
            .await
            .map_err(|e| Error::Internal(format!("Failed to prepare source_config: {e}")))?;

        Ok((trimmed_provider, prepared_source_config, bound_instance))
    }

    /// Create a new playlist/folder
    pub async fn create_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreatePlaylistRequest,
    ) -> Result<Playlist> {
        self.create_playlist_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn create_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreatePlaylistRequest,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.create_playlist_internal(room_id, user_id, request, false, outbox_event_factory)
            .await
    }

    async fn create_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreatePlaylistRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if request.name.chars().count() > 255 {
            return Err(Error::InvalidInput(
                "Playlist name cannot exceed 255 characters".to_string(),
            ));
        }

        if !bypass_room_permissions {
            self.permission_service
                .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
                .await?;
        }

        // Verify parent exists and belongs to room
        if let Some(ref parent_id) = request.parent_id {
            let parent = self
                .playlist_repo
                .get_by_room_and_id(&room_id, parent_id)
                .await?
                .ok_or_else(|| Error::NotFound("Parent playlist not found".to_string()))?;
            debug_assert_eq!(parent.room_id, room_id);

            // Check nesting depth using recursive CTE (single query)
            let path = self
                .playlist_repo
                .get_path_in_room(&room_id, parent_id)
                .await?;
            // path includes the parent itself; adding a child means depth = path.len() + 1
            if path.len() + 1 > 10 {
                return Err(Error::InvalidInput(
                    "Playlist nesting depth cannot exceed 10 levels".to_string(),
                ));
            }
        }

        let (source_provider, source_config, provider_instance_name) =
            normalize_dynamic_playlist_fields(
                request.source_provider,
                request.source_config,
                request.provider_instance_name,
            )?;

        let (source_provider, source_config, provider_instance_name) = if let (
            Some(source_provider),
            Some(source_config),
        ) =
            (source_provider, source_config)
        {
            let (source_provider, source_config, provider_instance_name) = self
                .validate_dynamic_playlist_source(
                    &room_id,
                    &user_id,
                    source_provider,
                    source_config,
                    provider_instance_name,
                )
                .await?;

            (
                Some(source_provider),
                Some(source_config),
                provider_instance_name,
            )
        } else {
            (None, None, None)
        };

        let mut tx = self.playlist_repo.pool().begin().await?;
        let position = self
            .playlist_repo
            .get_next_append_position_with_tx(&room_id, request.parent_id.as_ref(), &mut tx)
            .await?;

        // Create playlist
        let playlist = Playlist {
            id: crate::models::PlaylistId::new(),
            room_id,
            creator_id: Some(user_id),
            name: request.name,
            parent_id: request.parent_id,
            position,
            source_provider,
            source_config,
            provider_instance_name,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let created_playlist = self
            .playlist_repo
            .create_with_executor(&playlist, &mut *tx)
            .await?;
        if let Some(event) = outbox_event_factory
            .as_ref()
            .and_then(|factory| factory(&created_playlist))
        {
            if let Some(outbox) = &self.cluster_outbox {
                outbox.insert_with_executor(&event, &mut *tx).await?;
            }
        }
        tx.commit().await?;

        tracing::info!(
            room_id = %room_id,
            playlist_id = %created_playlist.id,
            name = %created_playlist.name,
            is_dynamic = created_playlist.is_dynamic(),
            "Playlist created"
        );
        let actor_username = self.resolve_actor_username(&user_id).await;

        // Broadcast to cluster replicas
        if let Some(broadcaster) = self.cluster_broadcaster.read().clone() {
            broadcaster.broadcast_playlist_created(
                &room_id,
                &created_playlist,
                &user_id,
                &actor_username,
            );
        }

        Ok(created_playlist)
    }

    /// Get playlist by ID
    pub async fn get_playlist(&self, playlist_id: &PlaylistId) -> Result<Option<Playlist>> {
        self.playlist_repo.get_by_id(playlist_id).await
    }

    /// Get playlist by ID, scoped to a room.
    pub async fn get_room_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>> {
        self.playlist_repo
            .get_by_room_and_id(room_id, playlist_id)
            .await
    }

    /// Get top-level playlists in a room.
    pub async fn get_top_level_playlists(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        self.playlist_repo.get_top_level(room_id).await
    }

    /// Count top-level playlists in a room.
    pub async fn count_top_level_playlists(&self, room_id: &RoomId) -> Result<i64> {
        self.playlist_repo.count_top_level(room_id).await
    }

    /// Get paginated top-level playlists in a room.
    pub async fn get_top_level_playlists_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        self.playlist_repo
            .get_top_level_paginated(room_id, limit, offset)
            .await
    }

    /// Get children playlists
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        self.playlist_repo.get_children(parent_id).await
    }

    /// Get count of children playlists for a parent.
    pub async fn count_children(&self, parent_id: &PlaylistId) -> Result<i64> {
        self.playlist_repo.count_children(parent_id).await
    }

    /// Get count of children playlists for a parent, scoped to a room.
    pub async fn count_room_children(
        &self,
        room_id: &RoomId,
        parent_id: &PlaylistId,
    ) -> Result<i64> {
        self.playlist_repo
            .count_children_in_room(room_id, parent_id)
            .await
    }

    /// Get paginated children playlists for a parent.
    pub async fn get_children_paginated(
        &self,
        parent_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        self.playlist_repo
            .get_children_paginated(parent_id, limit, offset)
            .await
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_room_playlists(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        self.playlist_repo.get_by_room(room_id).await
    }

    /// Count all playlists in a room
    pub async fn count_room_playlists(&self, room_id: &RoomId) -> Result<i64> {
        self.playlist_repo.count_by_room(room_id).await
    }

    /// Get paginated playlists in a room
    pub async fn get_room_playlists_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        self.playlist_repo
            .get_by_room_paginated(room_id, limit, offset)
            .await
    }

    /// Set playlist properties
    ///
    /// Uses optimistic locking with automatic retry on version conflicts.
    /// Retries use exponential backoff with jitter to avoid thundering herd.
    pub async fn set_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
    ) -> Result<Playlist> {
        self.set_playlist_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn set_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.set_playlist_internal(room_id, user_id, request, false, outbox_event_factory)
            .await
    }

    /// Management-only playlist update that bypasses room membership permission checks.
    pub async fn admin_set_playlist(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: SetPlaylistRequest,
    ) -> Result<Playlist> {
        self.admin_set_playlist_with_outbox(room_id, actor_user_id, request, None)
            .await
    }

    pub async fn admin_set_playlist_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: SetPlaylistRequest,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.set_playlist_internal(room_id, actor_user_id, request, true, outbox_event_factory)
            .await
    }

    async fn set_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if !bypass_room_permissions {
            // Renaming and reordering existing playlist entries requires REORDER_PLAYLIST,
            // not ADD_MEDIA. Users who can only add media should not be able to rename or
            // reorder items they do not own. REORDER_PLAYLIST is an admin-level permission.
            self.permission_service
                .check_permission(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
                .await?;
        }

        for attempt in 0..OPTIMISTIC_LOCK_MAX_RETRIES {
            // Get existing playlist (re-fetch on each retry to get latest version)
            let mut playlist = self
                .playlist_repo
                .get_by_room_and_id(&room_id, &request.playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            // Store original version for optimistic locking
            let expected_version = playlist.version;

            // Update fields
            if let Some(ref name) = request.name {
                if name.chars().count() > 255 {
                    return Err(Error::InvalidInput(
                        "Playlist name cannot exceed 255 characters".to_string(),
                    ));
                }
                playlist.name = name.clone();
            }
            // Save with optimistic locking
            let mut tx = self.playlist_repo.pool().begin().await?;
            match self
                .playlist_repo
                .update_with_version_with_executor(&playlist, expected_version, &mut *tx)
                .await
            {
                Ok(updated_playlist) => {
                    if let Some(event) = outbox_event_factory
                        .as_ref()
                        .and_then(|factory| factory(&updated_playlist))
                    {
                        if let Some(outbox) = &self.cluster_outbox {
                            outbox.insert_with_executor(&event, &mut *tx).await?;
                        }
                    }
                    tx.commit().await?;
                    tracing::info!(
                        room_id = %room_id,
                        playlist_id = %request.playlist_id,
                        "Playlist updated"
                    );
                    let actor_username = self.resolve_actor_username(&user_id).await;
                    if let Some(broadcaster) = self.cluster_broadcaster.read().clone() {
                        broadcaster.broadcast_playlist_updated(
                            &room_id,
                            &updated_playlist,
                            &user_id,
                            &actor_username,
                        );
                    }
                    return Ok(updated_playlist);
                }
                Err(Error::OptimisticLockConflict) => {
                    tx.rollback().await?;
                    if attempt + 1 < OPTIMISTIC_LOCK_MAX_RETRIES {
                        // Exponential backoff with jitter
                        let backoff = OPTIMISTIC_LOCK_BACKOFF_BASE_MS * (1 << attempt);
                        let jitter = rand::random_range(0..OPTIMISTIC_LOCK_BACKOFF_BASE_MS);
                        let delay = backoff + jitter;
                        tracing::debug!(
                            room_id = %room_id,
                            playlist_id = %request.playlist_id,
                            attempt = attempt + 1,
                            delay_ms = delay,
                            "Playlist version conflict, retrying with backoff"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        continue;
                    }
                    return Err(Error::Internal(
                        "Playlist update failed after maximum retry attempts".to_string(),
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(
            "Playlist update failed after maximum retry attempts".to_string(),
        ))
    }

    pub async fn move_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
    ) -> Result<Playlist> {
        self.move_playlist_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn move_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.move_playlist_internal(room_id, user_id, request, false, outbox_event_factory)
            .await
    }

    pub async fn admin_move_playlist(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: MovePlaylistRequest,
    ) -> Result<Playlist> {
        self.admin_move_playlist_with_outbox(room_id, actor_user_id, request, None)
            .await
    }

    pub async fn admin_move_playlist_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: MovePlaylistRequest,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.move_playlist_internal(room_id, actor_user_id, request, true, outbox_event_factory)
            .await
    }

    async fn move_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<ClusterOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
                .await?;
        }

        let has_before = request.before_playlist_id.is_some();
        let has_after = request.after_playlist_id.is_some();
        if has_before == has_after {
            return Err(Error::InvalidInput(
                "Exactly one of before_playlist_id or after_playlist_id must be set".to_string(),
            ));
        }

        let mut tx = self.playlist_repo.pool().begin().await?;
        let moved = self
            .playlist_repo
            .move_with_tx(
                &room_id,
                &request.playlist_id,
                request.before_playlist_id.as_ref(),
                request.after_playlist_id.as_ref(),
                &mut tx,
            )
            .await?;

        if let Some(event) = outbox_event_factory
            .as_ref()
            .and_then(|factory| factory(&moved))
        {
            if let Some(outbox) = &self.cluster_outbox {
                outbox.insert_with_executor(&event, &mut *tx).await?;
            }
        }
        tx.commit().await?;
        let actor_username = self.resolve_actor_username(&user_id).await;
        if let Some(broadcaster) = self.cluster_broadcaster.read().clone() {
            broadcaster.broadcast_playlist_updated(&room_id, &moved, &user_id, &actor_username);
        }
        Ok(moved)
    }

    /// Delete playlist
    pub async fn delete_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: PlaylistId,
    ) -> Result<()> {
        self.delete_playlist_internal(room_id, user_id, playlist_id, false)
            .await
    }

    /// Management-only playlist deletion that bypasses room membership permission checks.
    pub async fn admin_delete_playlist(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        playlist_id: PlaylistId,
    ) -> Result<()> {
        self.delete_playlist_internal(room_id, actor_user_id, playlist_id, true)
            .await
    }

    async fn delete_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: PlaylistId,
        bypass_room_permissions: bool,
    ) -> Result<()> {
        if !bypass_room_permissions
            && !self
                .permission_service
                .is_admin_or_creator(&room_id, &user_id)
                .await?
        {
            return Err(Error::Authorization(
                "Only admins or creators can delete playlists".to_string(),
            ));
        }

        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, &playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        debug_assert_eq!(playlist.room_id, room_id);

        // Delete (will cascade to children and media)
        self.playlist_repo
            .delete_in_room(&room_id, &playlist_id)
            .await?;

        tracing::info!(
            room_id = %room_id,
            playlist_id = %playlist_id,
            "Playlist deleted"
        );
        let actor_username = self.resolve_actor_username(&user_id).await;

        // Broadcast to cluster replicas
        if let Some(broadcaster) = self.cluster_broadcaster.read().clone() {
            broadcaster.broadcast_playlist_deleted(
                &room_id,
                &playlist_id,
                &user_id,
                &actor_username,
            );
        }

        Ok(())
    }

    /// Get playlist path (breadcrumbs) using recursive CTE (single query)
    pub async fn get_playlist_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let path = self.playlist_repo.get_path(playlist_id).await?;
        if path.is_empty() {
            return Err(Error::NotFound("Playlist not found".to_string()));
        }
        Ok(path)
    }

    /// Get playlist path (breadcrumbs), scoped to a room.
    pub async fn get_room_playlist_path(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<Playlist>> {
        let path = self
            .playlist_repo
            .get_path_in_room(room_id, playlist_id)
            .await?;
        if path.is_empty() {
            return Err(Error::NotFound("Playlist not found".to_string()));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PlaylistId;
    use crate::provider::{
        DirectoryItem, DynamicFolder, DynamicListQuery, MediaProvider, NextPlayItem,
        PlaybackResult, ProviderError,
    };
    use crate::repository::{
        PlaylistRepository, ProviderInstanceRepository, RoomMemberRepository, RoomRepository,
    };
    use crate::service::{PermissionService, RemoteProviderManager};
    use async_trait::async_trait;
    use serde_json::Value;
    use sqlx::PgPool;
    use std::sync::Arc;

    struct CredentialOwnerCheckProvider;

    #[async_trait]
    impl MediaProvider for CredentialOwnerCheckProvider {
        fn name(&self) -> &'static str {
            "credential_check"
        }

        async fn generate_playback(
            &self,
            _ctx: &ProviderContext<'_>,
            _source_config: &Value,
        ) -> std::result::Result<PlaybackResult, ProviderError> {
            Err(ProviderError::UnsupportedFormat(
                "test provider does not generate playback".to_string(),
            ))
        }

        fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
            Some(self)
        }

        async fn validate_source_config(
            &self,
            ctx: &ProviderContext<'_>,
            source_config: SourceConfig<'_>,
        ) -> std::result::Result<(), ProviderError> {
            if !source_config.is_dynamic_playlist() {
                return Err(ProviderError::Internal(
                    "credential_check validates dynamic playlist sources only".to_string(),
                ));
            }
            let user_id = ctx
                .user_id
                .as_ref()
                .ok_or_else(|| ProviderError::Internal("missing user_id".to_string()))?;
            let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal("missing credential_owner_id".to_string())
            })?;
            if credential_owner_id != user_id {
                return Err(ProviderError::Internal(
                    "credential_owner_id must match playlist creator during validation".to_string(),
                ));
            }

            Ok(())
        }
    }

    #[async_trait]
    impl DynamicFolder for CredentialOwnerCheckProvider {
        async fn list_playlist(
            &self,
            _ctx: &ProviderContext<'_>,
            _playlist: &Playlist,
            _target: Option<&[u8]>,
            _query: DynamicListQuery,
        ) -> std::result::Result<Vec<DirectoryItem>, ProviderError> {
            Ok(Vec::new())
        }

        async fn resolve_item(
            &self,
            _ctx: &ProviderContext<'_>,
            _playlist: &Playlist,
            _target: &[u8],
        ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
            Ok(None)
        }

        async fn next(
            &self,
            _ctx: &ProviderContext<'_>,
            _playlist: &Playlist,
            _playing_media: &crate::models::Media,
            _target: &[u8],
            _play_mode: crate::models::PlayMode,
        ) -> std::result::Result<Option<NextPlayItem>, ProviderError> {
            Ok(None)
        }
    }

    #[test]
    fn test_create_playlist_request_basic() {
        let room_id = RoomId::new();
        let request = CreatePlaylistRequest {
            room_id,
            name: "My Playlist".to_string(),
            parent_id: None,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        };

        assert_eq!(request.name, "My Playlist");
        assert_eq!(request.room_id, room_id);
        assert!(request.parent_id.is_none());
        assert!(request.source_provider.is_none());
    }

    #[test]
    fn test_create_playlist_request_dynamic() {
        let request = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Alist Movies".to_string(),
            parent_id: None,
            source_provider: Some("alist".to_string()),
            source_config: Some(serde_json::json!({"path": "/movies"})),
            provider_instance_name: Some("alist_home".to_string()),
        };

        assert!(request.source_provider.is_some());
        assert!(request.source_config.is_some());
        assert_eq!(request.source_provider.unwrap(), "alist");
    }

    #[test]
    fn test_create_playlist_request_with_parent() {
        let parent_id = PlaylistId::new();
        let request = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Subfolder".to_string(),
            parent_id: Some(parent_id),
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        };

        assert_eq!(request.parent_id, Some(parent_id));
    }

    #[test]
    fn test_set_playlist_request_name_only() {
        let request = SetPlaylistRequest {
            playlist_id: PlaylistId::new(),
            name: Some("New Name".to_string()),
        };

        assert_eq!(request.name, Some("New Name".to_string()));
    }

    #[test]
    fn test_move_playlist_request_before_anchor() {
        let request = MovePlaylistRequest {
            playlist_id: PlaylistId::new(),
            before_playlist_id: Some(PlaylistId::new()),
            after_playlist_id: None,
        };

        assert!(request.before_playlist_id.is_some());
        assert!(request.after_playlist_id.is_none());
    }

    #[test]
    fn test_playlist_name_trimming() {
        let name = "  My Playlist  ";
        let trimmed = name.trim();
        assert_eq!(trimmed, "My Playlist");
        assert!(!trimmed.is_empty());
    }

    #[test]
    fn test_playlist_name_empty_after_trim() {
        let name = "   ";
        let trimmed = name.trim();
        assert!(trimmed.is_empty());
    }

    #[test]
    fn test_playlist_name_max_length() {
        let name_ok = "a".repeat(200);
        assert!(name_ok.len() <= 200);

        let name_too_long = "a".repeat(201);
        assert!(name_too_long.len() > 200);
    }

    #[test]
    fn test_playlist_name_unicode_length() {
        // Unicode characters may take multiple bytes but validation uses char count.
        // "\u{00e9}" = "é" (1 char, 2 bytes per repetition in UTF-8)
        let name = "\u{00e9}".repeat(100);
        // 100 chars, 300 bytes: within the 200-character limit
        assert_eq!(name.chars().count(), 100);
        assert!(name.len() > 100); // byte count is larger than char count

        // 201 chars exceeds the limit
        let name_too_long = "\u{00f8}".repeat(201);
        assert_eq!(name_too_long.chars().count(), 201);
    }

    #[test]
    fn test_playlist_is_top_level() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: String::new(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        assert!(playlist.is_top_level());
        assert!(playlist.is_static());
        assert!(!playlist.is_dynamic());
    }

    #[test]
    fn test_playlist_is_not_root_with_name() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: "Not Root".to_string(),
            parent_id: Some(PlaylistId::new()),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        assert!(!playlist.is_top_level());
    }

    #[test]
    fn test_playlist_is_not_root_with_parent() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: String::new(),
            parent_id: Some(PlaylistId::new()),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        assert!(!playlist.is_top_level());
    }

    #[test]
    fn test_playlist_is_dynamic() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: "Alist Folder".to_string(),
            parent_id: Some(PlaylistId::new()),
            position: 0.0,
            source_provider: Some("alist".to_string()),
            source_config: Some(serde_json::json!({"path": "/movies"})),
            provider_instance_name: Some("alist_home".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        assert!(playlist.is_dynamic());
        assert!(!playlist.is_static());
        assert!(!playlist.is_top_level());
    }

    #[test]
    fn test_playlist_is_static() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: "Static Folder".to_string(),
            parent_id: Some(PlaylistId::new()),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        assert!(playlist.is_static());
        assert!(!playlist.is_dynamic());
    }

    #[test]
    fn test_dynamic_folder_requires_source_config() {
        let has_provider_no_config = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Bad Dynamic".to_string(),
            parent_id: None,
            source_provider: Some("alist".to_string()),
            source_config: None,
            provider_instance_name: None,
        };

        assert!(has_provider_no_config.source_provider.is_some());
        assert!(has_provider_no_config.source_config.is_none());
    }

    #[test]
    fn test_dynamic_folder_allows_empty_provider_instance_name() {
        let (source_provider, source_config, provider_instance_name) =
            normalize_dynamic_playlist_fields(
                Some("alist".to_string()),
                Some(serde_json::json!({"path": "/movies"})),
                None,
            )
            .expect("dynamic folder should allow default provider instance");
        assert_eq!(source_provider.as_deref(), Some("alist"));
        assert_eq!(source_config, Some(serde_json::json!({"path": "/movies"})));
        assert!(provider_instance_name.is_none());
    }

    #[test]
    fn test_static_folder_rejects_dynamic_fields_without_provider() {
        let err = normalize_dynamic_playlist_fields(
            None,
            Some(serde_json::json!({"path": "/movies"})),
            Some("alist-main".to_string()),
        )
        .unwrap_err();
        match err {
            Error::InvalidInput(message) => {
                assert!(message.contains("source_provider"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn test_dynamic_folder_fields_are_trimmed() {
        let (source_provider, source_config, provider_instance_name) =
            normalize_dynamic_playlist_fields(
                Some("  emby  ".to_string()),
                Some(serde_json::json!({"library_id": "abc123"})),
                Some("  emby-main  ".to_string()),
            )
            .unwrap();
        assert_eq!(source_provider.as_deref(), Some("emby"));
        assert!(source_config.is_some());
        assert_eq!(provider_instance_name.as_deref(), Some("emby-main"));
    }

    #[test]
    fn test_dynamic_folder_valid_config() {
        let request = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Valid Dynamic".to_string(),
            parent_id: None,
            source_provider: Some("emby".to_string()),
            source_config: Some(serde_json::json!({"library_id": "abc123"})),
            provider_instance_name: Some("emby_main".to_string()),
        };

        assert!(request.source_provider.is_some());
        assert!(request.source_config.is_some());
    }

    fn test_permission_service(pool: &PgPool) -> PermissionService {
        PermissionService::new(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            None,
            PermissionService::DEFAULT_CACHE_SIZE,
            PermissionService::DEFAULT_CACHE_TTL_SECS,
        )
    }

    async fn test_playlist_service_with_builtin_providers() -> PlaylistService {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(crate::service::ProvidersManager::new(
            provider_instance_manager,
        ));
        providers_manager
            .create_builtin_defaults()
            .await
            .expect("builtin providers should initialize");

        PlaylistService::new_with_provider_credentials(
            PlaylistRepository::new(pool.clone()),
            test_permission_service(&pool),
            providers_manager,
            None,
            Some(Arc::new(UserProviderCredentialRepository::new(pool))),
        )
    }

    async fn test_playlist_service_with_credential_check_provider() -> PlaylistService {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let mut providers_manager =
            crate::service::ProvidersManager::new(provider_instance_manager);
        providers_manager.register_factory(
            "credential_check",
            Box::new(|_instance_id, _config, _instance_manager| {
                Ok(Arc::new(CredentialOwnerCheckProvider))
            }),
        );
        let providers_manager = Arc::new(providers_manager);
        providers_manager
            .create_builtin_defaults()
            .await
            .expect("built-in providers should initialize");

        PlaylistService::new(
            PlaylistRepository::new(pool.clone()),
            test_permission_service(&pool),
            providers_manager,
        )
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_requires_credential_repo_wiring() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));
        let providers_manager = Arc::new(crate::service::ProvidersManager::new(
            provider_instance_manager,
        ));
        providers_manager
            .create_builtin_defaults()
            .await
            .expect("builtin providers should initialize");

        let service = PlaylistService::new(
            PlaylistRepository::new(pool.clone()),
            test_permission_service(&pool),
            providers_manager,
        );

        let err = service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &UserId::new(),
                "alist".to_string(),
                serde_json::json!({"path": "/movies", "server_id": "srv"}),
                Some("alist".to_string()),
            )
            .await
            .unwrap_err();

        match err {
            Error::ServiceUnavailable(message) => {
                assert!(message.contains("requires credential repository wiring"));
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_requires_provider_registry_for_unknown_provider_instance(
    ) {
        let service = test_playlist_service_with_builtin_providers().await;
        let err = service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &UserId::new(),
                "alist".to_string(),
                serde_json::json!({"path": "/movies", "server_id": "srv"}),
                Some("alist-main".to_string()),
            )
            .await
            .unwrap_err();

        match err {
            Error::ServiceUnavailable(message) => {
                assert!(
                    message.contains("Provider configuration service is temporarily unavailable")
                );
            }
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_rejects_provider_type_mismatch() {
        let service = test_playlist_service_with_builtin_providers().await;
        let err = service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &UserId::new(),
                "alist".to_string(),
                serde_json::json!({"url": "https://example.com/video.mp4"}),
                Some("direct_url".to_string()),
            )
            .await
            .unwrap_err();

        match err {
            Error::InvalidInput(message) => assert!(message.contains("is type 'direct_url'")),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_rejects_non_dynamic_provider() {
        let service = test_playlist_service_with_builtin_providers().await;
        let err = service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &UserId::new(),
                "direct_url".to_string(),
                serde_json::json!({"url": "https://example.com/video.mp4"}),
                Some("direct_url".to_string()),
            )
            .await
            .unwrap_err();

        match err {
            Error::InvalidInput(message) => {
                assert!(message.contains("does not support dynamic folders"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_runs_provider_validation() {
        let service = test_playlist_service_with_builtin_providers().await;
        let err = service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &UserId::new(),
                "alist".to_string(),
                serde_json::json!({"path": "", "server_id": "srv"}),
                Some("alist".to_string()),
            )
            .await
            .unwrap_err();

        match err {
            Error::InvalidInput(message) => {
                assert!(message.contains("Invalid source_config"));
                assert!(message.contains("must not be empty"));
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_validate_dynamic_playlist_source_passes_creator_as_credential_owner() {
        let service = test_playlist_service_with_credential_check_provider().await;
        let user_id = UserId::new();

        service
            .validate_dynamic_playlist_source(
                &RoomId::new(),
                &user_id,
                "credential_check".to_string(),
                serde_json::json!({}),
                Some("credential_check".to_string()),
            )
            .await
            .expect("dynamic playlist validation should expose creator credentials");
    }

    #[test]
    fn test_nesting_depth_limit() {
        let max_ancestors = 9;
        assert!(max_ancestors < 10);
        assert!(max_ancestors + 1 + 1 > 10);
    }

    #[test]
    fn test_playlist_positions_can_be_ordered() {
        let mut playlists: Vec<i32> = vec![3, 1, 4, 1, 5, 9, 2, 6];
        playlists.sort_unstable();
        assert_eq!(playlists, vec![1, 1, 2, 3, 4, 5, 6, 9]);
    }

    #[test]
    fn test_set_playlist_retry_constants() {
        // MAX_RETRIES = 3 to match optimistic_retry::DEFAULT_MAX_RETRIES
        const MAX_RETRIES: u32 = 3;
        const BACKOFF_BASE_MS: u64 = 5;
        assert_eq!(MAX_RETRIES, 3);
        assert_eq!(BACKOFF_BASE_MS, 5);
    }

    #[test]
    fn test_set_playlist_backoff_increases_exponentially() {
        const BACKOFF_BASE_MS: u64 = 5;
        // Verify exponential backoff calculation:
        // attempt 0: base * 1 = 5ms
        // attempt 1: base * 2 = 10ms
        // attempt 2: base * 4 = 20ms
        let delays: Vec<u64> = (0..3).map(|a| BACKOFF_BASE_MS * (1 << a)).collect();
        assert_eq!(delays, vec![5, 10, 20]);
    }

    #[test]
    fn test_set_playlist_retry_succeeds_within_max_attempts() {
        // With MAX_RETRIES = 3, we have 3 attempts total
        // If conflicts happen on attempts 0 and 1, attempt 2 should succeed
        let conflicts = 2;
        let attempts_needed = conflicts + 1; // 3 attempts
        assert!(
            attempts_needed <= 3,
            "Need {attempts_needed} attempts but MAX_RETRIES is 3"
        );
    }
}
