use crate::{
    models::{Playlist, PlaylistId, PlaylistSourceConfig, RoomId, SourceProvider, UserId},
    Error, Result,
};

use super::{dynamic, PlaylistService, RealtimeOutboxPlaylistEventFactory};

/// Request to create a playlist/folder
#[derive(Debug, Clone)]
pub struct CreatePlaylistRequest {
    pub room_id: RoomId,
    pub name: String,
    pub description: String,
    pub parent_id: Option<PlaylistId>,

    // Dynamic folder fields
    pub source_provider: Option<SourceProvider>,
    pub source_config: Option<PlaylistSourceConfig>,
    pub provider_instance_name: Option<String>,
}

impl PlaylistService {
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
        if request.name.chars().count() > crate::validation::PLAYLIST_NAME_MAX {
            return Err(Error::InvalidInput(format!(
                "Playlist name cannot exceed {} characters",
                crate::validation::PLAYLIST_NAME_MAX
            )));
        }
        if request.description.chars().count() > crate::validation::PLAYLIST_DESCRIPTION_MAX {
            return Err(Error::InvalidInput(format!(
                "Playlist description cannot exceed {} characters",
                crate::validation::PLAYLIST_DESCRIPTION_MAX
            )));
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
            dynamic::normalize_dynamic_playlist_fields(
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
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
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
}
