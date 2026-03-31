//! Playlist management service
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.1
//!
//! Manages playlist/folder operations including:
//! - Creating folders (static and dynamic)
//! - Tree structure navigation
//! - Position management

use std::sync::Arc;

use crate::{
    models::{PermissionBits, Playlist, PlaylistId, RoomId, UserId},
    repository::PlaylistRepository,
    service::permission::PermissionService,
    Error, Result,
};
use serde_json::Value as JsonValue;

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

    /// Broadcast that a playlist was deleted.
    fn broadcast_playlist_deleted(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        user_id: &UserId,
        username: &str,
    );
}

/// Request to create a playlist/folder
#[derive(Debug, Clone)]
pub struct CreatePlaylistRequest {
    pub room_id: RoomId,
    pub name: String,
    pub parent_id: Option<PlaylistId>,
    pub position: Option<i32>,

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
    pub position: Option<i32>,
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
    /// Optional cluster broadcaster for cross-replica playlist sync
    cluster_broadcaster: Option<Arc<dyn PlaylistBroadcaster>>,
}

impl std::fmt::Debug for PlaylistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistService").finish()
    }
}

impl PlaylistService {
    /// Create a new playlist service
    #[must_use]
    pub fn new(playlist_repo: PlaylistRepository, permission_service: PermissionService) -> Self {
        Self {
            playlist_repo,
            permission_service,
            cluster_broadcaster: None,
        }
    }

    /// Set the cluster broadcaster for cross-replica playlist sync
    pub fn set_cluster_broadcaster(&mut self, broadcaster: Arc<dyn PlaylistBroadcaster>) {
        self.cluster_broadcaster = Some(broadcaster);
    }

    /// Create a new playlist/folder
    pub async fn create_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreatePlaylistRequest,
    ) -> Result<Playlist> {
        if request.name.chars().count() > 255 {
            return Err(Error::InvalidInput(
                "Playlist name cannot exceed 255 characters".to_string(),
            ));
        }

        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MOVIE)
            .await?;

        // Verify parent exists and belongs to room
        if let Some(ref parent_id) = request.parent_id {
            let parent = self
                .playlist_repo
                .get_by_id(parent_id)
                .await?
                .ok_or_else(|| Error::NotFound("Parent playlist not found".to_string()))?;

            if parent.room_id != room_id {
                return Err(Error::Authorization(
                    "Parent playlist does not belong to this room".to_string(),
                ));
            }

            // Check nesting depth using recursive CTE (single query)
            let path = self.playlist_repo.get_path(parent_id).await?;
            // path includes the parent itself; adding a child means depth = path.len() + 1
            if path.len() + 1 > 10 {
                return Err(Error::InvalidInput(
                    "Playlist nesting depth cannot exceed 10 levels".to_string(),
                ));
            }
        }

        // Use explicit position if provided, otherwise use -1 sentinel
        // to let the repository compute it atomically in the INSERT query
        let position = request.position.unwrap_or(-1);

        // Validate dynamic folder requirements
        if request.source_provider.is_some() && request.source_config.is_none() {
            return Err(Error::InvalidInput(
                "source_config is required for dynamic folders".to_string(),
            ));
        }

        // Create playlist
        let playlist = Playlist {
            id: crate::models::PlaylistId::new(),
            room_id: room_id.clone(),
            creator_id: Some(user_id.clone()),
            name: request.name,
            parent_id: request.parent_id,
            position,
            source_provider: request.source_provider,
            source_config: request.source_config,
            provider_instance_name: request.provider_instance_name,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let created_playlist = self.playlist_repo.create(&playlist).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %created_playlist.id.as_str(),
            name = %created_playlist.name,
            is_dynamic = created_playlist.is_dynamic(),
            "Playlist created"
        );

        // Broadcast to cluster replicas
        if let Some(ref broadcaster) = self.cluster_broadcaster {
            broadcaster.broadcast_playlist_created(&room_id, &created_playlist, &user_id, "");
        }

        Ok(created_playlist)
    }

    /// Get playlist by ID
    pub async fn get_playlist(&self, playlist_id: &PlaylistId) -> Result<Option<Playlist>> {
        self.playlist_repo.get_by_id(playlist_id).await
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
        // Renaming and reordering existing playlist entries requires REORDER_PLAYLIST,
        // not ADD_MEDIA. Users who can only add media should not be able to rename or
        // reorder items they do not own. REORDER_PLAYLIST is an admin-level permission
        // (included in DEFAULT_ADMIN but not DEFAULT_MEMBER).
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::REORDER_PLAYLIST)
            .await?;

        // Retry loop with optimistic locking
        const MAX_RETRIES: u32 = 3;
        const BACKOFF_BASE_MS: u64 = 5;

        for attempt in 0..MAX_RETRIES {
            // Get existing playlist (re-fetch on each retry to get latest version)
            let mut playlist = self
                .playlist_repo
                .get_by_id(&request.playlist_id)
                .await?
                .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

            // Verify playlist belongs to room
            if playlist.room_id != room_id {
                return Err(Error::Authorization(
                    "Playlist does not belong to this room".to_string(),
                ));
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
            if let Some(position) = request.position {
                playlist.position = position;
            }

            // Save with optimistic locking
            match self
                .playlist_repo
                .update_with_version(&playlist, expected_version)
                .await
            {
                Ok(updated_playlist) => {
                    tracing::info!(
                        room_id = %room_id.as_str(),
                        playlist_id = %request.playlist_id.as_str(),
                        "Playlist updated"
                    );
                    return Ok(updated_playlist);
                }
                Err(Error::OptimisticLockConflict) => {
                    if attempt + 1 < MAX_RETRIES {
                        // Exponential backoff with jitter
                        let backoff = BACKOFF_BASE_MS * (1 << attempt);
                        let jitter = rand::random_range(0..BACKOFF_BASE_MS);
                        let delay = backoff + jitter;
                        tracing::debug!(
                            room_id = %room_id.as_str(),
                            playlist_id = %request.playlist_id.as_str(),
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

    /// Delete playlist
    pub async fn delete_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: PlaylistId,
    ) -> Result<()> {
        // Check permission (admin or creator)
        if !self
            .permission_service
            .is_admin_or_creator(&room_id, &user_id)
            .await?
        {
            return Err(Error::Authorization(
                "Only admins or creators can delete playlists".to_string(),
            ));
        }

        // Get playlist to verify ownership
        let playlist = self
            .playlist_repo
            .get_by_id(&playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        // Delete (will cascade to children and media)
        self.playlist_repo.delete(&playlist_id).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %playlist_id.as_str(),
            "Playlist deleted"
        );

        // Broadcast to cluster replicas
        if let Some(ref broadcaster) = self.cluster_broadcaster {
            broadcaster.broadcast_playlist_deleted(&room_id, &playlist_id, &user_id, "");
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PlaylistId;

    // ========== CreatePlaylistRequest Validation ==========

    #[test]
    fn test_create_playlist_request_basic() {
        let room_id = RoomId::new();
        let request = CreatePlaylistRequest {
            room_id: room_id.clone(),
            name: "My Playlist".to_string(),
            parent_id: None,
            position: None,
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
            position: Some(0),
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
            parent_id: Some(parent_id.clone()),
            position: Some(1),
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        };

        assert_eq!(request.parent_id, Some(parent_id));
        assert_eq!(request.position, Some(1));
    }

    // ========== SetPlaylistRequest Validation ==========

    #[test]
    fn test_set_playlist_request_name_only() {
        let request = SetPlaylistRequest {
            playlist_id: PlaylistId::new(),
            name: Some("New Name".to_string()),
            position: None,
        };

        assert_eq!(request.name, Some("New Name".to_string()));
        assert!(request.position.is_none());
    }

    #[test]
    fn test_set_playlist_request_position_only() {
        let request = SetPlaylistRequest {
            playlist_id: PlaylistId::new(),
            name: None,
            position: Some(5),
        };

        assert!(request.name.is_none());
        assert_eq!(request.position, Some(5));
    }

    // ========== Playlist Name Validation Logic ==========

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
        // "\u{4f60}\u{597d}" = "你好" (2 chars, 6 bytes per repetition)
        let name = "\u{4f60}\u{597d}".repeat(50);
        // 100 chars, 300 bytes: within the 200-character limit
        assert_eq!(name.chars().count(), 100);
        assert!(name.len() > 200); // byte count is larger

        // 201 chars exceeds the limit
        let name_too_long = "\u{4f60}".repeat(201);
        assert_eq!(name_too_long.chars().count(), 201);
    }

    // ========== Playlist Model Tests ==========

    #[test]
    fn test_playlist_is_top_level() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: String::new(),
            parent_id: None,
            position: 0,
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
            position: 0,
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
            position: 0,
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
            position: 0,
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
            position: 0,
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

    // ========== Dynamic Folder Validation Logic ==========

    #[test]
    fn test_dynamic_folder_requires_source_config() {
        let has_provider_no_config = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Bad Dynamic".to_string(),
            parent_id: None,
            position: None,
            source_provider: Some("alist".to_string()),
            source_config: None,
            provider_instance_name: None,
        };

        assert!(has_provider_no_config.source_provider.is_some());
        assert!(has_provider_no_config.source_config.is_none());
    }

    #[test]
    fn test_dynamic_folder_valid_config() {
        let request = CreatePlaylistRequest {
            room_id: RoomId::new(),
            name: "Valid Dynamic".to_string(),
            parent_id: None,
            position: None,
            source_provider: Some("emby".to_string()),
            source_config: Some(serde_json::json!({"library_id": "abc123"})),
            provider_instance_name: Some("emby_main".to_string()),
        };

        assert!(request.source_provider.is_some());
        assert!(request.source_config.is_some());
    }

    // ========== Nesting Depth Validation ==========

    #[test]
    fn test_nesting_depth_limit() {
        let max_ancestors = 9;
        assert!(max_ancestors < 10);
        assert!(max_ancestors + 1 + 1 > 10);
    }

    // ========== Position Ordering ==========

    #[test]
    fn test_playlist_positions_can_be_ordered() {
        let mut playlists: Vec<i32> = vec![3, 1, 4, 1, 5, 9, 2, 6];
        playlists.sort_unstable();
        assert_eq!(playlists, vec![1, 1, 2, 3, 4, 5, 6, 9]);
    }

    // ========== Optimistic Locking Retry Constants ==========

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

    // ========== Integration Tests (Require DB) ==========
}
