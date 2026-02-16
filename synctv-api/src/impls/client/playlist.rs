//! Playlist operations: create, update, delete, list playlists

use synctv_core::models::{PermissionBits, RoomId, UserId};

use super::ClientApiImpl;
use super::convert::playlist_to_proto;

impl ClientApiImpl {
    pub async fn create_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let parent_id = if req.parent_id.is_empty() {
            None
        } else {
            Some(synctv_core::models::PlaylistId::from_string(req.parent_id))
        };

        let service_req = synctv_core::service::playlist::CreatePlaylistRequest {
            room_id: rid.clone(),
            name: req.name,
            parent_id,
            position: None,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        };

        let playlist = self.room_service.playlist_service()
            .create_playlist(rid, uid, service_req)
            .await
            .map_err(|e| e.to_string())?;

        let item_count = self.room_service.media_service()
            .count_playlist_media(&playlist.id)
            .await
            .unwrap_or(0) as i32;

        Ok(crate::proto::client::CreatePlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
        })
    }

    pub async fn update_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdatePlaylistRequest,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id);

        let name = if req.name.is_empty() { None } else { Some(req.name) };
        let position = if req.position == -1 { None } else { Some(req.position) };

        let service_req = synctv_core::service::playlist::SetPlaylistRequest {
            playlist_id,
            name,
            position,
        };

        let playlist = self.room_service.playlist_service()
            .set_playlist(rid, uid, service_req)
            .await
            .map_err(|e| e.to_string())?;

        let item_count = self.room_service.media_service()
            .count_playlist_media(&playlist.id)
            .await
            .unwrap_or(0) as i32;

        Ok(crate::proto::client::UpdatePlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
        })
    }

    pub async fn delete_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::DeletePlaylistRequest,
    ) -> Result<crate::proto::client::DeletePlaylistResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id);

        self.room_service.playlist_service()
            .delete_playlist(rid, uid, playlist_id)
            .await
            .map_err(|e| e.to_string())?;

        Ok(crate::proto::client::DeletePlaylistResponse { success: true })
    }

    pub async fn list_playlists(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning playlist data
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let playlists = if req.parent_id.is_empty() {
            // Get all playlists in room
            self.room_service.playlist_service()
                .get_room_playlists(&rid)
                .await
                .map_err(|e| e.to_string())?
        } else {
            // Get children of specific playlist
            let parent_id = synctv_core::models::PlaylistId::from_string(req.parent_id);
            self.room_service.playlist_service()
                .get_children(&parent_id)
                .await
                .map_err(|e| e.to_string())?
        };

        let mut proto_playlists = Vec::with_capacity(playlists.len());
        for pl in &playlists {
            let item_count = self.room_service.media_service()
                .count_playlist_media(&pl.id)
                .await
                .unwrap_or(0) as i32;
            proto_playlists.push(playlist_to_proto(pl, item_count));
        }

        Ok(crate::proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
        })
    }
}
