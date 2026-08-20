use crate::{
    models::{Playlist, PlaylistId, RoomId},
    Error, Result,
};

use super::PlaylistService;

impl PlaylistService {
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
