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
        normalize_provider_instance_name_owned, CompleteFileUploadSession,
        CompleteFileUploadSessionResult, FileObjectDownload, FileRangeRequest, FileReferenceTarget,
        FileUploadManifestPart, FileUploadSessionCreateResult, GetFileObject, Playlist, PlaylistId,
        RoomId, SourceProvider, SubmittedFileReference, UserId,
    },
    provider::{provider_requires_credential_repo, ProviderContext, SourceConfig},
    repository::realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
    repository::PlaylistRepository,
    repository::UserProviderCredentialRepository,
    service::{
        file_storage::FileStorageContext, optimistic_retry, permission::PermissionService,
        playlist_cover_upload_policy,
        provider_binding::resolve_credential_provider_instance_binding,
        source_config::validate_source_config_size, FileStorageCleanupOrigin, FileStorageService,
        ProvidersManager,
    },
    Error, Result,
};
use serde_json::Value as JsonValue;

pub type RealtimeOutboxPlaylistEventFactory =
    Arc<dyn Fn(&Playlist) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;

fn normalize_dynamic_playlist_fields(
    source_provider: Option<SourceProvider>,
    source_config: Option<JsonValue>,
    provider_instance_name: Option<String>,
) -> Result<(Option<SourceProvider>, Option<JsonValue>, Option<String>)> {
    let normalized_provider = source_provider;
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
    pub description: String,
    pub parent_id: Option<PlaylistId>,

    // Dynamic folder fields
    pub source_provider: Option<SourceProvider>,
    pub source_config: Option<JsonValue>,
    pub provider_instance_name: Option<String>,
}

/// Request to set playlist properties
#[derive(Debug, Clone)]
pub struct SetPlaylistRequest {
    pub playlist_id: PlaylistId,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreatePlaylistCoverUploadSession {
    pub client_cover_id: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: JsonValue,
}

fn ensure_playlist_creator_can_edit(playlist: &Playlist, user_id: &UserId) -> Result<()> {
    if playlist.creator_id.as_ref() == Some(user_id) {
        Ok(())
    } else {
        Err(Error::Authorization(
            "Only the playlist creator can edit playlists".to_string(),
        ))
    }
}

struct DynamicPlaylistValidationDeps<'a> {
    providers_manager: &'a ProvidersManager,
    credential_encryption: Option<&'a crate::credential_encryption::CredentialEncryption>,
    credential_repo: Option<&'a Arc<UserProviderCredentialRepository>>,
}

async fn validate_dynamic_playlist_source_with_dependencies(
    deps: DynamicPlaylistValidationDeps<'_>,
    room_id: &RoomId,
    user_id: &UserId,
    source_provider: SourceProvider,
    source_config: JsonValue,
    provider_instance_name: Option<String>,
) -> Result<(SourceProvider, JsonValue, Option<String>)> {
    let trimmed_instance = normalize_provider_instance_name_owned(provider_instance_name);
    validate_source_config_size(&source_config)?;

    let provider = deps
        .providers_manager
        .resolve_provider(source_provider, trimmed_instance.as_deref())
        .await?;
    let provider_name = source_provider.as_str();

    if provider.as_dynamic_folder().is_none() {
        return Err(Error::InvalidInput(format!(
            "Provider {provider_name} does not support dynamic folders"
        )));
    }
    ensure_provider_credential_repo_available(provider_name, deps.credential_repo)?;

    // NOTE: ProviderContext building is repeated twice below due to lifetime constraints.
    // The first context validates against trimmed_instance, the second against bound_instance.
    // Extracting to a helper causes lifetime conflicts because ProviderContext borrows
    // the instance name reference.

    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(*user_id)
        .with_room_id(*room_id)
        .with_credential_owner_id(*user_id);
    if let Some(provider_instance_name) = trimmed_instance.as_deref() {
        ctx = ctx.with_provider_instance_name(provider_instance_name);
    }
    if let Some(repo) = deps.credential_repo {
        ctx = ctx.with_credential_repo(repo);
    }
    if let Some(enc) = deps.credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }

    provider
        .validate_source_config(&ctx, SourceConfig::dynamic_playlist(&source_config))
        .await
        .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

    let bound_instance = resolve_credential_provider_instance_binding(
        provider.as_ref(),
        deps.credential_repo,
        &ctx,
        &source_config,
        trimmed_instance.as_deref(),
    )
    .await?;
    let provider = if bound_instance == trimmed_instance {
        provider
    } else {
        let provider = deps
            .providers_manager
            .resolve_provider(source_provider, bound_instance.as_deref())
            .await?;
        if provider.as_dynamic_folder().is_none() {
            return Err(Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            )));
        }
        provider
    };

    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(*user_id)
        .with_room_id(*room_id)
        .with_credential_owner_id(*user_id);
    if let Some(provider_instance_name) = bound_instance.as_deref() {
        ctx = ctx.with_provider_instance_name(provider_instance_name);
    }
    if let Some(repo) = deps.credential_repo {
        ctx = ctx.with_credential_repo(repo);
    }
    if let Some(enc) = deps.credential_encryption {
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

    Ok((source_provider, prepared_source_config, bound_instance))
}

fn ensure_provider_credential_repo_available(
    provider_name: &str,
    credential_repo: Option<&Arc<UserProviderCredentialRepository>>,
) -> Result<()> {
    if provider_requires_credential_repo(provider_name) && credential_repo.is_none() {
        return Err(Error::ServiceUnavailable(format!(
            "Provider '{provider_name}' requires credential repository wiring"
        )));
    }

    Ok(())
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
    /// Credential encryption used by credential-backed providers.
    credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    /// Repository used by credential-backed providers.
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

const PLAYLIST_COVER_REFERENCE_KIND: &str = "playlist_cover";

fn playlist_cover_storage_scope(room_id: RoomId, playlist_id: PlaylistId) -> String {
    format!(
        "rooms/{}/playlists/{}/cover",
        room_id.as_i64(),
        playlist_id.as_i64()
    )
}

fn playlist_cover_reference_target(
    playlist_id: PlaylistId,
    file: &crate::models::StoredFileReference,
) -> FileReferenceTarget {
    file.reference_target(
        PLAYLIST_COVER_REFERENCE_KIND,
        playlist_id.as_i64().to_string(),
    )
}

impl std::fmt::Debug for PlaylistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistService").finish()
    }
}

impl PlaylistService {
    async fn insert_playlist_outbox_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        playlist: &Playlist,
        outbox_event_factory: Option<&RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(playlist))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
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
            realtime_outbox: None,
            file_storage_service: None,
        }
    }

    /// Create a playlist service with provider credential dependencies already wired.
    #[must_use]
    pub fn new_with_provider_credentials(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
        credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    ) -> Self {
        Self::new_with_runtime(
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption,
            credential_repo,
            None,
            None,
        )
    }

    #[must_use]
    pub fn new_with_runtime(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
        credential_repo: Option<Arc<UserProviderCredentialRepository>>,
        realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
        file_storage_service: Option<Arc<dyn FileStorageService>>,
    ) -> Self {
        assert!(
            credential_repo.is_none() || credential_encryption.is_some(),
            "provider credential repository wiring requires credential encryption"
        );
        Self {
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption,
            credential_repo,
            realtime_outbox,
            file_storage_service,
        }
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn FileStorageService>> {
        self.file_storage_service.as_ref()
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
        source_provider: SourceProvider,
        source_config: JsonValue,
        provider_instance_name: Option<String>,
    ) -> Result<(SourceProvider, JsonValue, Option<String>)> {
        validate_dynamic_playlist_source_with_dependencies(
            DynamicPlaylistValidationDeps {
                providers_manager: &self.providers_manager,
                credential_encryption: self.credential_encryption.as_ref(),
                credential_repo: self.credential_repo.as_ref(),
            },
            room_id,
            user_id,
            source_provider,
            source_config,
            provider_instance_name,
        )
        .await
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if request.name.chars().count() > 255 {
            return Err(Error::InvalidInput(
                "Playlist name cannot exceed 255 characters".to_string(),
            ));
        }
        if request.description.chars().count() > 5000 {
            return Err(Error::InvalidInput(
                "Playlist description cannot exceed 5000 characters".to_string(),
            ));
        }

        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
                )
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
            description: request.description,
            cover_file_reference_id: None,
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
        self.insert_playlist_outbox_tx(&mut tx, &created_playlist, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        tracing::info!(
            room_id = %room_id,
            playlist_id = %created_playlist.id,
            name = %created_playlist.name,
            is_dynamic = created_playlist.is_dynamic(),
            "Playlist created"
        );
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        let playlist_id = request.playlist_id;
        let updated_playlist = optimistic_retry::retry_with_optimistic_lock(
            optimistic_retry::DEFAULT_MAX_RETRIES,
            optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            "Playlist update failed after maximum retry attempts",
            || async {
                // Get existing playlist (re-fetch on each retry to get latest version)
                let mut playlist = self
                    .playlist_repo
                    .get_by_room_and_id(&room_id, &request.playlist_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

                if !bypass_room_permissions {
                    ensure_playlist_creator_can_edit(&playlist, &user_id)?;
                    self.permission_service
                        .check_permission_no_cache(
                            &room_id,
                            &user_id,
                            crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
                        )
                        .await?;
                }

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
                if let Some(ref description) = request.description {
                    if description.chars().count() > 5000 {
                        return Err(Error::InvalidInput(
                            "Playlist description cannot exceed 5000 characters".to_string(),
                        ));
                    }
                    playlist.description = description.clone();
                }
                // Save with optimistic locking
                let mut tx = self.playlist_repo.pool().begin().await?;
                match self
                    .playlist_repo
                    .update_with_version_with_executor(&playlist, expected_version, &mut *tx)
                    .await
                {
                    Ok(updated_playlist) => {
                        self.insert_playlist_outbox_tx(
                            &mut tx,
                            &updated_playlist,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                        tx.commit().await?;
                        Ok(updated_playlist)
                    }
                    Err(Error::OptimisticLockConflict) => {
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
            playlist_id = %playlist_id,
            "Playlist updated"
        );
        Ok(updated_playlist)
    }

    pub async fn create_cover_upload_session(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
        request: CreatePlaylistCoverUploadSession,
    ) -> Result<FileUploadSessionCreateResult> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for playlist covers".to_string())
        })?;
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, &playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        storage
            .create_upload_session(crate::models::CreateFileUploadSession {
                user_id,
                storage_scope: playlist_cover_storage_scope(room_id, playlist_id),
                client_file_id: request.client_cover_id,
                filename: None,
                mime_type: request.mime_type,
                size_bytes: request.size_bytes,
                width: request.width,
                height: request.height,
                parts: request.parts,
                metadata: request.metadata,
                policy: playlist_cover_upload_policy(),
            })
            .await
    }

    pub async fn store_cover_upload_object(
        &self,
        encoded_object_key: &str,
        upload_token: &str,
        content_type: Option<&str>,
        range: Option<crate::models::FileUploadRange>,
        data: Vec<u8>,
    ) -> Result<crate::models::StoreFileUploadResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput(
                    "file storage is not configured for playlist covers".to_string(),
                )
            })?
            .store_upload(crate::models::StoreFileUpload {
                encoded_object_key: encoded_object_key.to_string(),
                upload_token: upload_token.to_string(),
                content_type: content_type.map(str::to_string),
                range,
                data,
            })
            .await
    }

    pub async fn complete_cover_upload_session(
        &self,
        request: CompleteFileUploadSession,
    ) -> Result<CompleteFileUploadSessionResult> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidInput(
                    "file storage is not configured for playlist covers".to_string(),
                )
            })?
            .complete_upload_session(request)
            .await
    }

    pub async fn get_cover_object(
        &self,
        encoded_object_key: &str,
        read_token: &str,
    ) -> Result<crate::models::FileBlob> {
        self.get_cover_object_range(encoded_object_key, read_token, None)
            .await
    }

    pub async fn get_cover_object_range(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<crate::models::FileBlob> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn get_cover_object_stream(
        &self,
        encoded_object_key: &str,
        read_token: &str,
        range: Option<FileRangeRequest>,
    ) -> Result<FileObjectDownload> {
        self.file_storage_service
            .as_ref()
            .ok_or_else(|| Error::NotFound("File object not found".to_string()))?
            .get_object_stream(GetFileObject {
                encoded_object_key: encoded_object_key.to_string(),
                read_token: read_token.to_string(),
                range,
            })
            .await
    }

    pub async fn update_cover(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
        file: SubmittedFileReference,
    ) -> Result<Playlist> {
        let storage = self.file_storage_service.as_ref().ok_or_else(|| {
            Error::InvalidInput("file storage is not configured for playlist covers".to_string())
        })?;
        let mut tx = self.playlist_repo.pool().begin().await?;
        let mut playlist = self
            .playlist_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &playlist_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        let storage_scope = playlist_cover_storage_scope(room_id, playlist_id);
        let upload_policy = playlist_cover_upload_policy();
        let prepared = storage
            .prepare_submitted_files(
                FileStorageContext {
                    user_id,
                    storage_scope: &storage_scope,
                    database_object_route_prefix: &upload_policy.database_object_route_prefix,
                    client_request_id: None,
                },
                vec![file],
            )
            .await?;
        let file = prepared
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidInput("playlist cover file is required".to_string()))?;
        let new_reference_id = crate::repository::FileStorageRepository::insert_reference_in_tx(
            &mut tx,
            &file.storage_backend,
            &file.object_key,
            PLAYLIST_COVER_REFERENCE_KIND,
            &playlist_id.as_i64().to_string(),
            None,
            &file.metadata,
        )
        .await?
        .ok_or_else(|| {
            Error::InvalidInput("playlist cover file object is not registered".to_string())
        })?;
        let old_reference = if let Some(reference_id) = playlist.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.playlist_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| playlist_cover_reference_target(playlist_id, &reference))
        } else {
            None
        };

        playlist.cover_file_reference_id = Some(new_reference_id);
        let updated_playlist = self
            .playlist_repo
            .update_with_version_with_executor(&playlist, playlist.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let Some(old_reference) = old_reference {
            if old_reference.storage_backend != file.storage_backend
                || old_reference.object_key != file.object_key
            {
                storage
                    .delete_files(
                        FileStorageCleanupOrigin::ReferenceReleased,
                        &[old_reference],
                    )
                    .await?;
            }
        }

        Ok(updated_playlist)
    }

    pub async fn clear_cover(
        &self,
        room_id: RoomId,
        playlist_id: PlaylistId,
        user_id: UserId,
    ) -> Result<Playlist> {
        let mut tx = self.playlist_repo.pool().begin().await?;
        let mut playlist = self
            .playlist_repo
            .get_by_room_and_id_for_update_with_executor(&room_id, &playlist_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;
        ensure_playlist_creator_can_edit(&playlist, &user_id)?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &user_id,
                crate::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await?;

        let old_reference = if let Some(reference_id) = playlist.cover_file_reference_id {
            crate::repository::FileStorageRepository::new(self.playlist_repo.pool().clone())
                .get_reference_by_id(reference_id)
                .await?
                .map(|reference| playlist_cover_reference_target(playlist_id, &reference))
        } else {
            None
        };
        playlist.cover_file_reference_id = None;
        let updated_playlist = self
            .playlist_repo
            .update_with_version_with_executor(&playlist, playlist.version, &mut *tx)
            .await?;
        tx.commit().await?;

        if let (Some(storage), Some(reference)) =
            (self.file_storage_service.as_ref(), old_reference)
        {
            storage
                .delete_files(FileStorageCleanupOrigin::ReferenceReleased, &[reference])
                .await?;
        }

        Ok(updated_playlist)
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
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
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::REORDER_MEDIA_RESOURCES,
                )
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

        self.insert_playlist_outbox_tx(&mut tx, &moved, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;
        Ok(moved)
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
mod tests;
