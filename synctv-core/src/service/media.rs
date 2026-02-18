//! Media and playlist management service
//!
//! Design reference: /Volumes/workspace/rust/design/08-视频内容管理.md
//!
//! Three-stage workflow:
//! 1. Parse - Parse user input to get options
//! 2. Add Media - Store `source_config` in database
//! 3. Generate Playback - Dynamically generate playback info when playing

use crate::{
    models::{Media, MediaId, PlaylistId, RoomId, UserId, PermissionBits},
    repository::{MediaRepository, PlaylistRepository},
    service::{permission::PermissionService, ProvidersManager},
    provider::{ProviderContext, DirectoryItem},
    Error, Result,
};
use serde_json::Value as JsonValue;
use std::sync::Arc;

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
    pub playlist_id: PlaylistId,
    pub name: String,
    /// Provider instance name (e.g., "`bilibili_main`", "`alist_company`")
    /// The provider will be looked up from the provider registry
    pub provider_instance_name: String,
    pub source_config: JsonValue,
}

/// Request to edit a media item
#[derive(Debug, Clone)]
pub struct EditMediaRequest {
    pub media_id: MediaId,
    pub name: Option<String>,
    pub position: Option<i32>,
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
}

impl std::fmt::Debug for MediaService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaService").finish()
    }
}

impl MediaService {
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
        }
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

        // Verify playlist belongs to room
        let playlist = self
            .playlist_repo
            .get_by_id(&request.playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization("Playlist does not belong to this room".to_string()));
        }

        // Get provider from registry by instance name
        // The registry stores actual Arc<dyn MediaProvider> instances
        let provider = self
            .providers_manager
            .get(&request.provider_instance_name)
            .await
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Provider instance not found: {}",
                    request.provider_instance_name
                ))
            })?;

        // Validate source_config using provider trait method
        let ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());

        provider
            .validate_source_config(&ctx, &request.source_config)
            .await
            .map_err(|e| Error::InvalidInput(format!("Invalid source_config: {e}")))?;

        // Use a transaction to atomically get the next position and insert,
        // preventing concurrent adds from getting the same position
        let mut tx = self.media_repo.pool().begin().await?;

        // Get next position in playlist (locked with FOR UPDATE)
        let position = self.media_repo.get_next_position_with_tx(&request.playlist_id, &mut tx).await?;

        // Create media with provider info (no enum conversion needed)
        // Business logic will use provider_instance_name to get provider from registry
        let media = Media::from_provider(
            request.playlist_id.clone(),
            room_id.clone(),
            Some(user_id.clone()),
            request.name.clone(),
            request.source_config.clone(),
            provider.name(),  // Provider type name (e.g., "bilibili")
            request.provider_instance_name.clone(),  // Instance name (e.g., "bilibili_main")
            position,
        );

        let created_media = self.media_repo.create_with_executor(&media, &mut *tx).await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %request.playlist_id.as_str(),
            media_id = %created_media.id.as_str(),
            name = %created_media.name,
            provider = %request.provider_instance_name,
            "Media added to playlist"
        );

        Ok(created_media)
    }

    /// Add multiple media items to a playlist
    pub async fn add_media_batch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: PlaylistId,
        items: Vec<AddMediaRequest>,
    ) -> Result<Vec<Media>> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
            .await?;

        // Verify playlist belongs to room
        let playlist = self
            .playlist_repo
            .get_by_id(&playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization("Playlist does not belong to this room".to_string()));
        }

        if items.is_empty() {
            return Ok(Vec::new());
        }

        if items.len() > 100 {
            return Err(Error::InvalidInput(
                "Batch size cannot exceed 100 items".to_string(),
            ));
        }

        // Create provider context for validation
        let ctx = ProviderContext::new("synctv")
            .with_user_id(user_id.as_str())
            .with_room_id(room_id.as_str());

        // Validate all items before starting a transaction
        let mut validated_items = Vec::with_capacity(items.len());
        for item in items {
            // Get provider from registry by instance name
            let provider = self
                .providers_manager
                .get(&item.provider_instance_name)
                .await
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "Provider instance not found: {}",
                        item.provider_instance_name
                    ))
                })?;

            // Validate source_config using provider trait method
            provider
                .validate_source_config(&ctx, &item.source_config)
                .await
                .map_err(|e| Error::InvalidInput(format!("Invalid source_config for item '{}': {}", item.name, e)))?;

            validated_items.push((item, provider));
        }

        // Use a transaction to atomically get the next position and batch insert,
        // preventing concurrent adds from getting the same position
        let mut tx = self.media_repo.pool().begin().await?;

        // Get starting position (locked with FOR UPDATE)
        let start_position = self.media_repo.get_next_position_with_tx(&playlist_id, &mut tx).await?;

        // Create media items with provider info
        let mut media_items = Vec::with_capacity(validated_items.len());
        for (index, (item, provider)) in validated_items.into_iter().enumerate() {
            let media = Media::from_provider(
                item.playlist_id,
                room_id.clone(),
                Some(user_id.clone()),
                item.name,
                item.source_config,
                provider.name(),  // Provider type name
                item.provider_instance_name,  // Instance name
                start_position + i32::try_from(index).unwrap_or(i32::MAX),
            );
            media_items.push(media);
        }

        // Batch insert within the transaction
        let created_items = self.media_repo.create_batch_with_executor(&media_items, &mut *tx).await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %playlist_id.as_str(),
            count = created_items.len(),
            "Batch added media to playlist"
        );

        Ok(created_items)
    }

    /// Edit media item
    pub async fn edit_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: EditMediaRequest,
    ) -> Result<Media> {
        // Get existing media
        let mut media = self
            .media_repo
            .get_by_id(&request.media_id)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        // Verify media belongs to the room
        if media.room_id != room_id {
            return Err(Error::Authorization("Media does not belong to this room".to_string()));
        }

        // Check permission: EDIT_MOVIE_SELF if user owns the media, EDIT_MOVIE_ANY otherwise
        let required_permission = if media.creator_id.as_ref() == Some(&user_id) {
            PermissionBits::EDIT_MOVIE_SELF
        } else {
            PermissionBits::EDIT_MOVIE_ANY
        };
        self.permission_service
            .check_permission(&room_id, &user_id, required_permission)
            .await?;

        // Update fields
        if let Some(name) = request.name {
            media.name = name;
        }
        if let Some(position) = request.position {
            media.position = position;
        }

        let updated_media = self.media_repo.update(&media).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id = %request.media_id.as_str(),
            "Media edited"
        );

        Ok(updated_media)
    }

    /// Remove media from playlist
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
            return Err(Error::Authorization("Media does not belong to this room".to_string()));
        }

        // Check permission: DELETE_MOVIE_SELF if user owns the media, DELETE_MOVIE_ANY otherwise
        let required_permission = if media.creator_id.as_ref() == Some(&user_id) {
            PermissionBits::DELETE_MOVIE_SELF
        } else {
            PermissionBits::DELETE_MOVIE_ANY
        };
        self.permission_service
            .check_permission(&room_id, &user_id, required_permission)
            .await?;

        // Soft delete
        self.media_repo.delete(&media_id).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id = %media_id.as_str(),
            "Media removed from playlist"
        );

        Ok(())
    }

    /// Get media by ID
    pub async fn get_media(&self, media_id: &MediaId) -> Result<Option<Media>> {
        self.media_repo.get_by_id(media_id).await
    }

    /// Get all media in a playlist
    pub async fn get_playlist_media(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        self.media_repo.get_by_playlist(playlist_id).await
    }

    /// Get paginated media in a playlist
    pub async fn get_playlist_media_paginated(
        &self,
        playlist_id: &PlaylistId,
        pagination: crate::models::PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        self.media_repo.get_playlist_paginated(playlist_id, pagination).await
    }

    /// Swap positions of two media items
    pub async fn swap_media_positions(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id1: MediaId,
        media_id2: MediaId,
    ) -> Result<()> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
            .await?;

        // Verify both media exist and belong to the room
        let media1 = self
            .media_repo
            .get_by_id(&media_id1)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        let media2 = self
            .media_repo
            .get_by_id(&media_id2)
            .await?
            .ok_or_else(|| Error::NotFound("Media not found".to_string()))?;

        if media1.room_id != room_id || media2.room_id != room_id {
            return Err(Error::Authorization("Media does not belong to this room".to_string()));
        }

        // Swap positions
        self.media_repo.swap_positions(&media_id1, &media_id2).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            media_id1 = %media_id1.as_str(),
            media_id2 = %media_id2.as_str(),
            "Media positions swapped"
        );

        Ok(())
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

        // Use explicit transaction to prevent TOCTOU between read and delete
        let mut tx = self.media_repo.pool().begin().await?;

        // Batch-load all media in a single query within the transaction
        let media_items = self.media_repo.get_by_ids_with_executor(&media_ids, &mut *tx).await?;

        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound("One or more media items not found".to_string()));
        }

        // Verify all media belong to the room and check ownership
        let mut all_owned_by_user = true;
        for media in &media_items {
            if media.room_id != room_id {
                return Err(Error::Authorization("Media does not belong to this room".to_string()));
            }
            if media.creator_id.as_ref() != Some(&user_id) {
                all_owned_by_user = false;
            }
        }

        // Check permission: DELETE_MOVIE_SELF if user owns all items, DELETE_MOVIE_ANY otherwise
        let required_permission = if all_owned_by_user {
            PermissionBits::DELETE_MOVIE_SELF
        } else {
            PermissionBits::DELETE_MOVIE_ANY
        };
        self.permission_service
            .check_permission(&room_id, &user_id, required_permission)
            .await?;

        // Bulk delete within the same transaction
        let deleted_count = self.media_repo.delete_batch_with_executor(&media_ids, &mut *tx).await?;

        tx.commit().await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            count = deleted_count,
            "Bulk removed media from playlist"
        );

        Ok(deleted_count)
    }

    /// Bulk reorder media items
    ///
    /// Reorders multiple media items to new positions in a single transaction.
    /// Uses a single batch query to verify room ownership instead of N individual queries.
    pub async fn reorder_media_batch(
        &self,
        room_id: RoomId,
        user_id: UserId,
        updates: Vec<(MediaId, i32)>,
    ) -> Result<()> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
            .await?;

        if updates.is_empty() {
            return Ok(());
        }

        // Batch-load all media in a single query to verify room ownership
        let media_ids: Vec<MediaId> = updates.iter().map(|(id, _)| id.clone()).collect();
        let media_items = self.media_repo.get_by_ids(&media_ids).await?;

        if media_items.len() != updates.len() {
            return Err(Error::NotFound("One or more media items not found".to_string()));
        }

        for media in &media_items {
            if media.room_id != room_id {
                return Err(Error::Authorization("Media does not belong to this room".to_string()));
            }
        }

        // Bulk reorder
        self.media_repo.reorder_batch(&updates).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            count = updates.len(),
            "Bulk reordered media in playlist"
        );

        Ok(())
    }

    /// Count media items in a playlist
    /// Delete all media in a playlist (single query, no N+1)
    pub async fn delete_by_playlist(&self, playlist_id: &PlaylistId) -> Result<usize> {
        self.media_repo.delete_by_playlist(playlist_id).await
    }

    pub async fn count_playlist_media(&self, playlist_id: &PlaylistId) -> Result<i64> {
        self.media_repo.count_by_playlist(playlist_id).await
    }

    /// Batch count media items across multiple playlists
    pub async fn count_playlist_media_batch(&self, playlist_ids: &[&str]) -> Result<std::collections::HashMap<String, i64>> {
        self.media_repo.count_by_playlists_batch(playlist_ids).await
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
    /// * `relative_path` - Relative path within the dynamic folder (empty for root)
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
        relative_path: Option<&str>,
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
            return Err(Error::Authorization("Playlist does not belong to this room".to_string()));
        }

        // Check if playlist is dynamic
        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        // Get provider
        let provider_name = playlist.source_provider.as_ref()
            .ok_or_else(|| Error::InvalidInput("Dynamic playlist missing provider".to_string()))?;

        let provider = self.providers_manager
            .get_by_type(provider_name)
            .await
            .ok_or_else(|| Error::NotFound(format!("Provider not found: {provider_name}")))?;

        // Check if provider implements DynamicFolder trait
        let dynamic_folder = provider.as_dynamic_folder()
            .ok_or_else(|| Error::InvalidInput(format!("Provider {provider_name} does not support dynamic folders")))?;

        // Create context
        let ctx = ProviderContext {
            user_id: Some(user_id.as_str()),
            room_id: Some(room_id.as_str()),
            base_url: None,
            key_prefix: "synctv",
            db: None,
            redis: None,
        };

        // List items
        let items = dynamic_folder
            .list_playlist(&ctx, &playlist, relative_path, page, page_size)
            .await
            .map_err(|e| Error::Internal(format!("Failed to list playlist items: {e}")))?;

        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== AddMediaRequest Validation ==========

    #[test]
    fn test_add_media_request_construction() {
        let request = AddMediaRequest {
            playlist_id: PlaylistId::new(),
            name: "Test Video".to_string(),
            provider_instance_name: "bilibili_main".to_string(),
            source_config: serde_json::json!({"bvid": "BV1234567890"}),
        };

        assert_eq!(request.name, "Test Video");
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
            playlist_id: PlaylistId::new(),
            name: "Complex Video".to_string(),
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
            position: None,
        };

        assert_eq!(request.name, Some("New Name".to_string()));
        assert!(request.position.is_none());
    }

    #[test]
    fn test_edit_media_request_position_only() {
        let request = EditMediaRequest {
            media_id: MediaId::new(),
            name: None,
            position: Some(5),
        };

        assert!(request.name.is_none());
        assert_eq!(request.position, Some(5));
    }

    #[test]
    fn test_edit_media_request_both_fields() {
        let request = EditMediaRequest {
            media_id: MediaId::new(),
            name: Some("Updated".to_string()),
            position: Some(10),
        };

        assert_eq!(request.name, Some("Updated".to_string()));
        assert_eq!(request.position, Some(10));
    }

    // ========== Batch Size Validation ==========

    #[test]
    fn test_batch_items_construction() {
        let items: Vec<AddMediaRequest> = (0..101)
            .map(|i| AddMediaRequest {
                playlist_id: PlaylistId::new(),
                name: format!("Video {i}"),
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
            playlist_id: PlaylistId::new(),
            name: "Null Config".to_string(),
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
            playlist_id: PlaylistId::new(),
            name: "Nested Config".to_string(),
            provider_instance_name: "alist_home".to_string(),
            source_config: config,
        };

        assert_eq!(
            request.source_config["options"]["subtitle"]["lang"],
            "en"
        );
    }

}
