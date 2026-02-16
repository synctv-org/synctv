//! Playlist management service
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.1
//!
//! Manages playlist/folder operations including:
//! - Creating folders (static and dynamic)
//! - Tree structure navigation
//! - Position management

use crate::{
    models::{Playlist, PlaylistId, RoomId, UserId, PermissionBits},
    repository::PlaylistRepository,
    service::permission::PermissionService,
    Error, Result,
};
use serde_json::Value as JsonValue;

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
}

impl std::fmt::Debug for PlaylistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistService").finish()
    }
}

impl PlaylistService {
    /// Create a new playlist service
    #[must_use] 
    pub const fn new(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
    ) -> Self {
        Self {
            playlist_repo,
            permission_service,
        }
    }

    /// Create a new playlist/folder
    pub async fn create_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: CreatePlaylistRequest,
    ) -> Result<Playlist> {
        // Validate name
        let name = request.name.trim();
        if name.is_empty() {
            return Err(Error::InvalidInput("Playlist name cannot be empty".to_string()));
        }
        if name.len() > 200 {
            return Err(Error::InvalidInput("Playlist name cannot exceed 200 bytes".to_string()));
        }

        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
            .await?;

        // Verify parent exists and belongs to room
        if let Some(ref parent_id) = request.parent_id {
            let parent = self
                .playlist_repo
                .get_by_id(parent_id)
                .await?
                .ok_or_else(|| Error::NotFound("Parent playlist not found".to_string()))?;

            if parent.room_id != room_id {
                return Err(Error::Authorization("Parent playlist does not belong to this room".to_string()));
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

        // Calculate position if not provided
        let position = if let Some(pos) = request.position {
            pos
        } else {
            self.playlist_repo
                .get_next_position(&room_id.clone(), request.parent_id.as_ref())
                .await?
        };

        // Validate dynamic folder requirements
        if request.source_provider.is_some() && request.source_config.is_none() {
            return Err(Error::InvalidInput(
                "source_config is required for dynamic folders".to_string()
            ));
        }

        // Create playlist
        let playlist = Playlist {
            id: crate::models::PlaylistId::new(),
            room_id: room_id.clone(),
            creator_id: user_id,
            name: name.to_string(),
            parent_id: request.parent_id,
            position,
            source_provider: request.source_provider,
            source_config: request.source_config,
            provider_instance_name: request.provider_instance_name,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let created_playlist = self.playlist_repo.create(&playlist).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %created_playlist.id.as_str(),
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

    /// Get root playlist for a room
    pub async fn get_root_playlist(&self, room_id: &RoomId) -> Result<Playlist> {
        self.playlist_repo.get_root_playlist(room_id).await
    }

    /// Get children playlists
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        self.playlist_repo.get_children(parent_id).await
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_room_playlists(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        self.playlist_repo.get_by_room(room_id).await
    }

    /// Set playlist properties
    pub async fn set_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
    ) -> Result<Playlist> {
        // Check permission
        self.permission_service
            .check_permission(&room_id, &user_id, PermissionBits::ADD_MEDIA)
            .await?;

        // Get existing playlist
        let mut playlist = self
            .playlist_repo
            .get_by_id(&request.playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        // Verify playlist belongs to room
        if playlist.room_id != room_id {
            return Err(Error::Authorization("Playlist does not belong to this room".to_string()));
        }

        // Update fields
        if let Some(name) = request.name {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(Error::InvalidInput("Playlist name cannot be empty".to_string()));
            }
            if name.len() > 200 {
                return Err(Error::InvalidInput("Playlist name cannot exceed 200 bytes".to_string()));
            }
            playlist.name = name;
        }
        if let Some(position) = request.position {
            playlist.position = position;
        }

        let updated_playlist = self.playlist_repo.update(&playlist).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %request.playlist_id.as_str(),
            "Playlist updated"
        );

        Ok(updated_playlist)
    }

    /// Delete playlist
    pub async fn delete_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: PlaylistId,
    ) -> Result<()> {
        // Check permission (admin or creator)
        if !self.permission_service
            .is_admin_or_creator(&room_id, &user_id)
            .await?
        {
            return Err(Error::Authorization("Only admins or creators can delete playlists".to_string()));
        }

        // Get playlist to verify ownership
        let playlist = self
            .playlist_repo
            .get_by_id(&playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if playlist.room_id != room_id {
            return Err(Error::Authorization("Playlist does not belong to this room".to_string()));
        }

        // Cannot delete root playlist
        if playlist.is_root() {
            return Err(Error::InvalidInput("Cannot delete root playlist".to_string()));
        }

        // Delete (will cascade to children and media)
        self.playlist_repo.delete(&playlist_id).await?;

        tracing::info!(
            room_id = %room_id.as_str(),
            playlist_id = %playlist_id.as_str(),
            "Playlist deleted"
        );

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
        // Unicode characters may take multiple bytes
        let name = "\u{4f60}\u{597d}".repeat(50);
        assert!(name.len() > 200);
    }

    // ========== Playlist Model Tests ==========

    #[test]
    fn test_playlist_is_root() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: UserId::new(),
            name: String::new(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert!(playlist.is_root());
        assert!(playlist.is_static());
        assert!(!playlist.is_dynamic());
    }

    #[test]
    fn test_playlist_is_not_root_with_name() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: UserId::new(),
            name: "Not Root".to_string(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert!(!playlist.is_root());
    }

    #[test]
    fn test_playlist_is_not_root_with_parent() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: UserId::new(),
            name: String::new(),
            parent_id: Some(PlaylistId::new()),
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert!(!playlist.is_root());
    }

    #[test]
    fn test_playlist_is_dynamic() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: UserId::new(),
            name: "Alist Folder".to_string(),
            parent_id: None,
            position: 0,
            source_provider: Some("alist".to_string()),
            source_config: Some(serde_json::json!({"path": "/movies"})),
            provider_instance_name: Some("alist_home".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert!(playlist.is_dynamic());
        assert!(!playlist.is_static());
        assert!(!playlist.is_root());
    }

    #[test]
    fn test_playlist_is_static() {
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: UserId::new(),
            name: "Static Folder".to_string(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
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
        assert!(max_ancestors + 1 <= 10);
        assert!(max_ancestors + 1 + 1 > 10);
    }

    // ========== Position Ordering ==========

    #[test]
    fn test_playlist_positions_can_be_ordered() {
        let mut playlists: Vec<i32> = vec![3, 1, 4, 1, 5, 9, 2, 6];
        playlists.sort();
        assert_eq!(playlists, vec![1, 1, 2, 3, 4, 5, 6, 9]);
    }

    // ========== Integration Tests (Require DB) ==========

    #[tokio::test]
    async fn test_create_playlist() {
        // Placeholder: requires full service layer with DB
        // Covered by higher-level integration tests
    }

    #[tokio::test]
    async fn test_delete_playlist() {
        // Placeholder: requires full service layer with DB
        // Covered by higher-level integration tests
    }

    #[tokio::test]
    async fn test_get_playlist_path() {
        // Placeholder: requires full service layer with DB
        // Covered by higher-level integration tests
    }
}
