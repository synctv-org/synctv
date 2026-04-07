//! Playlist operations: create, update, delete, list playlists

use serde_json::Value as JsonValue;
use synctv_core::models::{
    PermissionBits, PlaylistListSortBy as CorePlaylistListSortBy,
    SortDirection as CoreSortDirection, UserId,
};
use synctv_core::service::playlist::{
    CreatePlaylistRequest as CoreCreatePlaylistRequest,
    MovePlaylistRequest as CoreMovePlaylistRequest, SetPlaylistRequest as CoreSetPlaylistRequest,
};

use super::convert::playlist_to_proto_with_availability;
use super::ClientApiImpl;
use crate::impls::ApiError;

pub(crate) fn build_create_playlist_request(
    room_id: &synctv_core::models::RoomId,
    req: crate::proto::client::CreatePlaylistRequest,
) -> Result<CoreCreatePlaylistRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::CreatePlaylistRequest {
        name,
        parent_id,
        source_provider,
        source_config,
        provider_instance_name,
    } = req;

    let parent_id = crate::impls::proto_validated_optional_playlist_id(parent_id);
    let source_provider = (!source_provider.is_empty()).then_some(source_provider);
    let source_config = if source_config.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<JsonValue>(&source_config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?,
        )
    };
    let provider_instance_name =
        (!provider_instance_name.is_empty()).then_some(provider_instance_name);

    Ok(CoreCreatePlaylistRequest {
        room_id: room_id.clone(),
        name,
        parent_id,
        source_provider,
        source_config,
        provider_instance_name,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_create_playlist_request, build_delete_playlist_request, build_move_playlist_request,
        build_update_playlist_request,
    };
    use synctv_core::models::RoomId;

    #[test]
    fn build_create_playlist_request_rejects_invalid_proto_payload() {
        let error = build_create_playlist_request(
            &RoomId::new(),
            crate::proto::client::CreatePlaylistRequest {
                name: "a".repeat(256),
                parent_id: String::new(),
                source_provider: "alist".into(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
            },
        )
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(
                    message.contains("name") || message.contains("dynamic"),
                    "{message}"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn build_create_playlist_request_parses_dynamic_payload() {
        let room_id = RoomId::new();
        let request = build_create_playlist_request(
            &room_id,
            crate::proto::client::CreatePlaylistRequest {
                name: "Dynamic".into(),
                parent_id: String::new(),
                source_provider: "alist".into(),
                source_config: serde_json::to_vec(&serde_json::json!({"path":"/tv"})).unwrap(),
                provider_instance_name: "alist-main".into(),
            },
        )
        .expect("valid request");

        assert_eq!(request.room_id, room_id);
        assert_eq!(request.name, "Dynamic");
        assert_eq!(request.source_provider.as_deref(), Some("alist"));
        assert_eq!(
            request.provider_instance_name.as_deref(),
            Some("alist-main")
        );
        assert_eq!(
            request.source_config,
            Some(serde_json::json!({"path":"/tv"}))
        );
    }

    #[test]
    fn build_create_playlist_request_parses_proto_validated_parent_id() {
        let room_id = RoomId::new();
        let parent_id = synctv_common::snanoid!(12);
        let request = build_create_playlist_request(
            &room_id,
            crate::proto::client::CreatePlaylistRequest {
                name: "Child".into(),
                parent_id: parent_id.clone(),
                source_provider: String::new(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
            },
        )
        .expect("valid request");

        assert_eq!(
            request.parent_id.as_ref().map(|id| id.as_str()),
            Some(parent_id.as_str())
        );
    }

    #[test]
    fn build_update_playlist_request_rejects_long_name() {
        let error = build_update_playlist_request(crate::proto::client::UpdatePlaylistRequest {
            playlist_id: "playlist-1".into(),
            name: "a".repeat(256),
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("name"), "{message}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn build_move_playlist_request_requires_anchor() {
        let error = build_move_playlist_request(crate::proto::client::MovePlaylistRequest {
            playlist_id: "playlist-1".into(),
            anchor: None,
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("anchor"), "{message}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn build_delete_playlist_request_rejects_invalid_playlist_id() {
        let error = build_delete_playlist_request(crate::proto::client::DeletePlaylistRequest {
            playlist_id: "bad-playlist".into(),
            force: false,
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("playlist_id"), "{message}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn build_delete_playlist_request_parses_playlist_id() {
        let playlist_id = synctv_common::snanoid!(12);
        let request = build_delete_playlist_request(crate::proto::client::DeletePlaylistRequest {
            playlist_id: playlist_id.clone(),
            force: true,
        })
        .unwrap();

        assert_eq!(request.0.as_str(), playlist_id);
        assert!(request.1);
    }
}

pub(crate) fn build_update_playlist_request(
    req: crate::proto::client::UpdatePlaylistRequest,
) -> Result<CoreSetPlaylistRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(CoreSetPlaylistRequest {
        playlist_id: crate::impls::proto_validated_playlist_id(req.playlist_id),
        name: if req.name.is_empty() {
            None
        } else {
            Some(req.name)
        },
    })
}

pub(crate) fn build_move_playlist_request(
    req: crate::proto::client::MovePlaylistRequest,
) -> Result<CoreMovePlaylistRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::MovePlaylistRequest {
        playlist_id,
        anchor,
    } = req;

    let anchor = anchor.ok_or_else(|| {
        ApiError::InvalidInput(
            "Exactly one of before_playlist_id or after_playlist_id must be set".to_string(),
        )
    })?;

    let (before_playlist_id, after_playlist_id) = match anchor {
        crate::proto::client::move_playlist_request::Anchor::BeforePlaylistId(anchor_id) => (
            Some(crate::impls::proto_validated_playlist_id(anchor_id)),
            None,
        ),
        crate::proto::client::move_playlist_request::Anchor::AfterPlaylistId(anchor_id) => (
            None,
            Some(crate::impls::proto_validated_playlist_id(anchor_id)),
        ),
    };

    Ok(CoreMovePlaylistRequest {
        playlist_id: crate::impls::proto_validated_playlist_id(playlist_id),
        before_playlist_id,
        after_playlist_id,
    })
}

pub(crate) fn build_delete_playlist_request(
    req: crate::proto::client::DeletePlaylistRequest,
) -> Result<(synctv_core::models::PlaylistId, bool), ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok((
        crate::impls::proto_validated_playlist_id(req.playlist_id),
        req.force,
    ))
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
            .check_permission(&rid, &uid, PermissionBits::ADD_MEDIA)
            .await
            .map_err(Self::map_room_access_error)?;

        let service_req = build_create_playlist_request(&rid, req)?;
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
            playlist: Some(playlist_to_proto_with_availability(
                &playlist, item_count, true,
            )),
        })
    }

    pub async fn update_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdatePlaylistRequest,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership and playlist management permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)
            .await
            .map_err(Self::map_room_access_error)?;

        let service_req = build_update_playlist_request(req)?;
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
            playlist: Some(playlist_to_proto_with_availability(
                &playlist, item_count, true,
            )),
        })
    }

    pub async fn move_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::MovePlaylistRequest,
    ) -> Result<crate::proto::client::MovePlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        self.room_service
            .check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)
            .await
            .map_err(Self::map_room_access_error)?;

        let service_req = build_move_playlist_request(req)?;
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let playlist = self
            .room_service
            .playlist_service()
            .move_playlist(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        let item_count = self
            .room_service
            .media_service()
            .count_playlist_media(&playlist.id)
            .await
            .unwrap_or(0) as i32;

        Ok(crate::proto::client::MovePlaylistResponse {
            playlist: Some(playlist_to_proto_with_availability(
                &playlist, item_count, true,
            )),
        })
    }

    pub async fn delete_playlist(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::DeletePlaylistRequest,
    ) -> Result<crate::proto::client::DeletePlaylistResponse, ApiError> {
        let (playlist_id, force) = build_delete_playlist_request(req)?;
        let _ = self
            .delete_entries(
                user_id,
                room_id,
                crate::proto::client::DeleteEntriesRequest {
                    playlist_ids: vec![playlist_id.as_str().to_string()],
                    media_ids: Vec::new(),
                    force,
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
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let pid = crate::impls::parse_playlist_id_param(playlist_id, "playlist_id")?;

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
        let playlist_availability = self
            .room_service
            .playlist_availability(&playlist)
            .await
            .map_err(ApiError::from)?;

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
            playlist: Some(playlist_to_proto_with_availability(
                &playlist,
                media_count,
                playlist_availability.is_available(),
            )),
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
        crate::impls::validate_proto_request(&req)?;

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

        let parent_id = if req.parent_id.is_empty() {
            None
        } else {
            let parent_id = crate::impls::proto_validated_playlist_id(req.parent_id.clone());
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
            Some(parent_id)
        };
        let query = synctv_core::models::PlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page as u32),
                Some(page_size as u32),
            ),
            search,
            source_provider,
            provider_instance_name,
            dynamic_only: req.dynamic_only,
            availability: match crate::proto::client::ResourceAvailabilityFilter::try_from(
                req.availability,
            )
            .unwrap_or(crate::proto::client::ResourceAvailabilityFilter::All)
            {
                crate::proto::client::ResourceAvailabilityFilter::All => None,
                crate::proto::client::ResourceAvailabilityFilter::Available => Some(true),
                crate::proto::client::ResourceAvailabilityFilter::Unavailable => Some(false),
            },
            sort_by,
            sort_direction,
        };
        let total = self
            .room_service
            .count_client_playlists(&rid, parent_id.as_ref(), &query)
            .await
            .map_err(ApiError::from)? as i32;
        let offset = (page - 1) * page_size;
        let playlists = self
            .room_service
            .list_client_playlists(
                &rid,
                parent_id.as_ref(),
                &query,
                page_size as i64,
                offset as i64,
            )
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch media counts to avoid N+1 queries.
        let playlist_ids: Vec<&str> = playlists
            .iter()
            .map(|entry| entry.playlist.id.as_str())
            .collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .unwrap_or_default();

        let proto_playlists: Vec<_> = playlists
            .iter()
            .map(|entry| {
                let item_count =
                    counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0) as i32;
                playlist_to_proto_with_availability(&entry.playlist, item_count, entry.is_available)
            })
            .collect();

        Ok(crate::proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
            total,
        })
    }
}
