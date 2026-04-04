//! Playlist operations: create, update, delete, list playlists

use serde_json::Value as JsonValue;
use std::cmp::Ordering;
use synctv_core::models::{
    PermissionBits, Playlist, PlaylistListSortBy as CorePlaylistListSortBy,
    SortDirection as CoreSortDirection, UserId,
};

use super::convert::playlist_to_proto;
use super::ClientApiImpl;
use crate::impls::ApiError;

fn compare_playlists(
    left: &Playlist,
    right: &Playlist,
    sort_by: CorePlaylistListSortBy,
    sort_direction: CoreSortDirection,
) -> Ordering {
    let ordering = match sort_by {
        CorePlaylistListSortBy::Name => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.position.cmp(&right.position)),
        CorePlaylistListSortBy::CreatedAt => left
            .created_at
            .cmp(&right.created_at)
            .then_with(|| left.position.cmp(&right.position)),
        CorePlaylistListSortBy::UpdatedAt => left
            .updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.position.cmp(&right.position)),
        CorePlaylistListSortBy::Position => left.position.cmp(&right.position).then_with(|| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        }),
    };

    match sort_direction {
        CoreSortDirection::Asc => ordering,
        CoreSortDirection::Desc => ordering.reverse(),
    }
}

impl ClientApiImpl {
    pub async fn create_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership and add media permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::ADD_MOVIE)
            .await
            .map_err(Self::map_room_access_error)?;

        let parent_id = if req.parent_id.is_empty() {
            None
        } else {
            crate::http::validation::validate_id(&req.parent_id, "parent_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid parent_id: {e}")))?;
            Some(synctv_core::models::PlaylistId::from_string(req.parent_id))
        };
        let source_provider = (!req.source_provider.is_empty()).then_some(req.source_provider);
        let source_config = if req.source_config.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice::<JsonValue>(&req.source_config).map_err(|e| {
                    ApiError::InvalidInput(format!("Invalid source_config JSON: {e}"))
                })?,
            )
        };
        let provider_instance_name =
            (!req.provider_instance_name.is_empty()).then_some(req.provider_instance_name);

        let service_req = synctv_core::service::playlist::CreatePlaylistRequest {
            room_id: rid.clone(),
            name: req.name,
            parent_id,
            position: None,
            source_provider,
            source_config,
            provider_instance_name,
        };
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let playlist = self
            .room_service
            .playlist_service()
            .create_playlist(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist structure change
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        let item_count = self
            .room_service
            .media_service()
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
        crate::http::validation::validate_id(&req.playlist_id, "playlist_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership and playlist management permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)
            .await
            .map_err(Self::map_room_access_error)?;

        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id);

        let name = if req.name.is_empty() {
            None
        } else {
            Some(req.name)
        };
        let position = if req.position == -1 {
            None
        } else {
            Some(req.position)
        };

        let service_req = synctv_core::service::playlist::SetPlaylistRequest {
            playlist_id,
            name,
            position,
        };
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let playlist = self
            .room_service
            .playlist_service()
            .set_playlist(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist update
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        let item_count = self
            .room_service
            .media_service()
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
        crate::http::validation::validate_id(&req.playlist_id, "playlist_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
        let _ = self
            .delete_entries(
                user_id,
                room_id,
                crate::proto::client::DeleteEntriesRequest {
                    playlist_ids: vec![req.playlist_id],
                    media_ids: Vec::new(),
                    force: req.force,
                },
            )
            .await?;

        Ok(crate::proto::client::DeletePlaylistResponse { success: true })
    }

    /// Get a single playlist by ID
    pub async fn get_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        playlist_id: &str,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        crate::http::validation::validate_id(playlist_id, "playlist_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let pid = synctv_core::models::PlaylistId::from_string(playlist_id.to_string());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        // Get playlist
        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(&pid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {playlist_id} not found")))?;

        // Count child folders and media files
        let child_folder_count = self
            .room_service
            .playlist_service()
            .count_children(&pid)
            .await
            .map_err(ApiError::from)? as i32;

        let media_count = self
            .room_service
            .media_service()
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
    ///
    /// Supports pagination via `page` and `page_size` fields.
    /// Default page_size is 50, maximum is 100.
    pub async fn list_playlists(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        if !req.parent_id.is_empty() {
            crate::http::validation::validate_id(&req.parent_id, "parent_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid parent_id: {e}")))?;
        }
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership before returning playlist data
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        // Pagination defaults and limits
        const DEFAULT_PAGE_SIZE: i32 = 50;
        const MAX_PAGE_SIZE: i32 = 100;

        let page = req.page.max(1) as usize;
        let page_size = if req.page_size <= 0 {
            DEFAULT_PAGE_SIZE as usize
        } else {
            req.page_size.min(MAX_PAGE_SIZE) as usize
        };
        let search = (!req.search.is_empty()).then(|| req.search.to_ascii_lowercase());
        let source_provider = (!req.source_provider.is_empty()).then_some(req.source_provider);
        let provider_instance_name =
            (!req.provider_instance_name.is_empty()).then_some(req.provider_instance_name);
        let sort_by = match crate::proto::client::PlaylistListSortBy::try_from(req.sort_by) {
            Ok(crate::proto::client::PlaylistListSortBy::Name) => CorePlaylistListSortBy::Name,
            Ok(crate::proto::client::PlaylistListSortBy::CreatedAt) => {
                CorePlaylistListSortBy::CreatedAt
            }
            Ok(crate::proto::client::PlaylistListSortBy::UpdatedAt) => {
                CorePlaylistListSortBy::UpdatedAt
            }
            _ => CorePlaylistListSortBy::Position,
        };
        let sort_direction = match crate::proto::client::SortDirection::try_from(req.sort_direction)
        {
            Ok(crate::proto::client::SortDirection::Desc) => CoreSortDirection::Desc,
            _ => CoreSortDirection::Asc,
        };

        let mut playlists = if req.parent_id.is_empty() {
            self.room_service
                .playlist_service()
                .get_top_level_playlists(&rid)
                .await
                .map_err(ApiError::from)?
        } else {
            let parent_id = synctv_core::models::PlaylistId::from_string(req.parent_id);
            let parent = self
                .room_service
                .playlist_service()
                .get_playlist(&parent_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Parent playlist not found".to_string()))?;
            if parent.room_id != rid {
                return Err(ApiError::Authorization(
                    "Parent playlist does not belong to this room".to_string(),
                ));
            }
            self.room_service
                .playlist_service()
                .get_children(&parent_id)
                .await
                .map_err(ApiError::from)?
        };

        playlists.retain(|playlist| {
            if let Some(search) = &search {
                if !playlist.name.to_ascii_lowercase().contains(search) {
                    return false;
                }
            }
            if let Some(source_provider) = source_provider.as_deref() {
                if playlist.source_provider.as_deref() != Some(source_provider) {
                    return false;
                }
            }
            if let Some(provider_instance_name) = provider_instance_name.as_deref() {
                if playlist.provider_instance_name.as_deref() != Some(provider_instance_name) {
                    return false;
                }
            }
            if let Some(dynamic_only) = req.dynamic_only {
                if playlist.is_dynamic() != dynamic_only {
                    return false;
                }
            }
            true
        });
        playlists.sort_by(|left, right| compare_playlists(left, right, sort_by, sort_direction));
        let total = playlists.len() as i32;
        let offset = (page - 1) * page_size;
        let playlists: Vec<Playlist> = playlists.into_iter().skip(offset).take(page_size).collect();

        // Batch-fetch media counts to avoid N+1 queries.
        let playlist_ids: Vec<&str> = playlists.iter().map(|pl| pl.id.as_str()).collect();
        let counts = self
            .room_service
            .media_service()
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

        Ok(crate::proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
            total,
        })
    }
}
