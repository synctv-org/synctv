//! Playlist operations: create, update, delete, list playlists

use synctv_core::models::{PermissionBits, RoomId, UserId};

use crate::impls::ApiError;
use super::ClientApiImpl;
use super::convert::playlist_to_proto;

impl ClientApiImpl {
    pub async fn create_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

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
            .create_playlist(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist structure change
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(tx, synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::CacheInvalidate {
                    event_id: nanoid::nanoid!(16),
                    targets: vec![synctv_cluster::sync::CacheTarget::Room {
                        room_id: rid.as_str().to_string(),
                    }],
                    timestamp: chrono::Utc::now(),
                },
            });
        }

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
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id);

        let name = if req.name.is_empty() { None } else { Some(req.name) };
        let position = if req.position == -1 { None } else { Some(req.position) };

        let service_req = synctv_core::service::playlist::SetPlaylistRequest {
            playlist_id,
            name,
            position,
        };

        let playlist = self.room_service.playlist_service()
            .set_playlist(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist update
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(tx, synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::CacheInvalidate {
                    event_id: nanoid::nanoid!(16),
                    targets: vec![synctv_cluster::sync::CacheTarget::Room {
                        room_id: rid.as_str().to_string(),
                    }],
                    timestamp: chrono::Utc::now(),
                },
            });
        }

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
    ) -> Result<crate::proto::client::DeletePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership and playlist management permission
        self.room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id);

        self.room_service.playlist_service()
            .delete_playlist(rid.clone(), uid, playlist_id)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist deletion
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(tx, synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::CacheInvalidate {
                    event_id: nanoid::nanoid!(16),
                    targets: vec![synctv_cluster::sync::CacheTarget::Room {
                        room_id: rid.as_str().to_string(),
                    }],
                    timestamp: chrono::Utc::now(),
                },
            });
        }

        Ok(crate::proto::client::DeletePlaylistResponse { success: true })
    }

    /// Get a single playlist by ID
    pub async fn get_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        playlist_id: &str,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let pid = synctv_core::models::PlaylistId::from_string(playlist_id.to_string());

        // Check membership
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // Get playlist
        let playlist = self.room_service.playlist_service()
            .get_playlist(&pid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {playlist_id} not found")))?;

        // Count child folders and media files
        let child_folders = self.room_service.playlist_service()
            .get_children(&pid)
            .await
            .map_err(ApiError::from)?;
        let child_folder_count = child_folders.len() as i32;

        let media_count = self.room_service.media_service()
            .count_playlist_media(&pid)
            .await
            .unwrap_or(0) as i32;

        Ok(crate::proto::client::GetPlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, media_count)),
            child_folder_count,
            media_count,
        })
    }

    /// List playlists (folders) in a room or under a parent
    pub async fn list_playlists(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning playlist data
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let playlists = if req.parent_id.is_empty() {
            // Get all playlists in room
            self.room_service.playlist_service()
                .get_room_playlists(&rid)
                .await
                .map_err(ApiError::from)?
        } else {
            // Get children of specific playlist
            let parent_id = synctv_core::models::PlaylistId::from_string(req.parent_id);
            self.room_service.playlist_service()
                .get_children(&parent_id)
                .await
                .map_err(ApiError::from)?
        };

        // Batch-fetch media counts to avoid N+1 queries.
        let playlist_ids: Vec<&str> = playlists.iter().map(|pl| pl.id.as_str()).collect();
        let counts = self.room_service.media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .unwrap_or_default();

        let proto_playlists: Vec<_> = playlists
            .iter()
            .map(|pl| {
                let item_count = counts.get(pl.id.as_str()).copied().unwrap_or(0) as i32;
                playlist_to_proto(pl, item_count)
            })
            .collect();

        let total = proto_playlists.len() as i32;
        Ok(crate::proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
            total,
        })
    }
}
