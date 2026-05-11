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
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};
use crate::impls::ApiError;

const DEFAULT_PLAYLIST_PAGE_SIZE: i32 = 50;
const MAX_PLAYLIST_PAGE_SIZE: i32 = 100;

fn i64_to_i32_api(value: i64, field: &str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

fn page_i32_to_usize(value: i32, field: &str) -> Result<usize, ApiError> {
    let non_negative = u32::try_from(value)
        .map_err(|_| ApiError::InvalidInput(format!("{field} must be non-negative")))?;
    usize::try_from(non_negative)
        .map_err(|_| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

fn usize_to_u32_api(value: usize, field: &str) -> Result<u32, ApiError> {
    u32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds u32::MAX")))
}

fn usize_to_i64_api(value: usize, field: &str) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i64::MAX")))
}

fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

pub(crate) fn build_create_playlist_request(
    room_id: &synctv_core::models::RoomId,
    req: crate::proto::client::CreatePlaylistRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<CoreCreatePlaylistRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::CreatePlaylistRequest {
        name,
        parent_id,
        source_provider,
        source_config,
        provider_instance_name,
    } = req;

    let parent_id = crate::impls::proto_validated_optional_playlist_id(parent_id, public_id_codec)?;
    let source_provider = normalize_non_empty_filter(&source_provider);
    let source_config = if source_config.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<JsonValue>(&source_config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?,
        )
    };
    let provider_instance_name = normalize_non_empty_filter(&provider_instance_name);

    Ok(CoreCreatePlaylistRequest {
        room_id: *room_id,
        name,
        parent_id,
        source_provider,
        source_config,
        provider_instance_name,
    })
}

pub(crate) fn build_update_playlist_request(
    req: crate::proto::client::UpdatePlaylistRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<CoreSetPlaylistRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    if req.name.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "playlist update requires at least one changed field".to_string(),
        ));
    }

    Ok(CoreSetPlaylistRequest {
        playlist_id: crate::impls::proto_validated_playlist_id(req.playlist_id, public_id_codec)?,
        name: Some(req.name),
    })
}

pub(crate) fn build_move_playlist_request(
    req: crate::proto::client::MovePlaylistRequest,
    public_id_codec: &crate::PublicIdCodec,
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
            Some(crate::impls::proto_validated_playlist_id(
                anchor_id,
                public_id_codec,
            )?),
            None,
        ),
        crate::proto::client::move_playlist_request::Anchor::AfterPlaylistId(anchor_id) => (
            None,
            Some(crate::impls::proto_validated_playlist_id(
                anchor_id,
                public_id_codec,
            )?),
        ),
    };

    Ok(CoreMovePlaylistRequest {
        playlist_id: crate::impls::proto_validated_playlist_id(playlist_id, public_id_codec)?,
        before_playlist_id,
        after_playlist_id,
    })
}

pub(crate) fn build_delete_playlist_request(
    req: crate::proto::client::DeletePlaylistRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<(synctv_core::models::PlaylistId, bool), ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok((
        crate::impls::proto_validated_playlist_id(req.playlist_id, public_id_codec)?,
        req.force,
    ))
}

impl ClientApiImpl {
    pub async fn create_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, ApiError> {
        let uid = *user_id;
        let actor_id = uid;
        let rid = self.parse_room_id(room_id)?;

        // Check membership and add media permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::ADD_MEDIA)
            .await
            .map_err(Self::map_room_access_error)?;
        let service_req = build_create_playlist_request(&rid, req, &self.public_id_codec)?;
        let actor_username = self
            .user_service
            .get_user(&actor_id)
            .await
            .map(|user| user.username)
            .unwrap_or_default();

        let prepared_outbox_fanout = self.playlist_fanout.prepare_created_outbox_fanout(
            rid,
            actor_id,
            actor_username.clone(),
        );
        let playlist = self
            .room_service
            .playlist_service()
            .create_playlist_with_outbox(
                rid,
                actor_id,
                service_req,
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist structure change
        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &playlist.id)
            .await
            .unwrap_or(0);
        let item_count = i64_to_i32_api(item_count, "playlist item count")?;

        Ok(crate::proto::client::CreatePlaylistResponse {
            playlist: Some(playlist_to_proto_with_availability(
                &playlist,
                item_count,
                true,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn update_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdatePlaylistRequest,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        let uid = *user_id;
        let actor_id = uid;
        let rid = self.parse_room_id(room_id)?;

        // Check membership and playlist management permission
        self.room_service
            .check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)
            .await
            .map_err(Self::map_room_access_error)?;
        let service_req = build_update_playlist_request(req, &self.public_id_codec)?;
        let actor_username = self
            .user_service
            .get_user(&actor_id)
            .await
            .map(|user| user.username)
            .unwrap_or_default();

        let prepared_outbox_fanout = self.playlist_fanout.prepare_updated_outbox_fanout(
            rid,
            actor_id,
            actor_username.clone(),
        );
        let playlist = self
            .room_service
            .playlist_service()
            .set_playlist_with_outbox(
                rid,
                actor_id,
                service_req,
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas for playlist update
        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &playlist.id)
            .await
            .unwrap_or(0);
        let item_count = i64_to_i32_api(item_count, "playlist item count")?;

        Ok(crate::proto::client::UpdatePlaylistResponse {
            playlist: Some(playlist_to_proto_with_availability(
                &playlist,
                item_count,
                true,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn move_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::MovePlaylistRequest,
    ) -> Result<crate::proto::client::MovePlaylistResponse, ApiError> {
        let uid = *user_id;
        let actor_id = uid;
        let rid = self.parse_room_id(room_id)?;

        self.room_service
            .check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)
            .await
            .map_err(Self::map_room_access_error)?;
        let service_req = build_move_playlist_request(req, &self.public_id_codec)?;
        let actor_username = self
            .user_service
            .get_user(&actor_id)
            .await
            .map(|user| user.username)
            .unwrap_or_default();

        let prepared_outbox_fanout = self.playlist_fanout.prepare_updated_outbox_fanout(
            rid,
            actor_id,
            actor_username.clone(),
        );
        let playlist = self
            .room_service
            .playlist_service()
            .move_playlist_with_outbox(
                rid,
                actor_id,
                service_req,
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;

        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &playlist.id)
            .await
            .unwrap_or(0);
        let item_count = i64_to_i32_api(item_count, "playlist item count")?;

        Ok(crate::proto::client::MovePlaylistResponse {
            playlist: Some(playlist_to_proto_with_availability(
                &playlist,
                item_count,
                true,
                &self.public_id_codec,
            )),
        })
    }

    pub async fn delete_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::DeletePlaylistRequest,
    ) -> Result<crate::proto::client::DeletePlaylistResponse, ApiError> {
        let public_playlist_id = req.playlist_id.clone();
        let (_playlist_id, force) = build_delete_playlist_request(req, &self.public_id_codec)?;
        let _ = self
            .delete_entries(
                user_id,
                room_id,
                crate::proto::client::DeleteEntriesRequest {
                    playlist_ids: vec![public_playlist_id],
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
        user_id: &UserId,
        room_id: &str,
        playlist_id: &str,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_playlist_for_actor(&actor, playlist_id).await
    }

    pub async fn get_playlist_as_guest(
        &self,
        access: &GuestRoomAccess,
        playlist_id: &str,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        self.get_playlist_for_actor(&RoomActor::Guest(access.clone()), playlist_id)
            .await
    }

    pub async fn get_playlist_for_actor(
        &self,
        actor: &RoomActor,
        playlist_id: &str,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        self.require_room_permission(actor, PermissionBits::VIEW_PLAYLIST)
            .await?;
        let rid = actor.room_id();
        let pid = crate::impls::parse_playlist_id_param(
            playlist_id,
            "playlist_id",
            &self.public_id_codec,
        )?;

        // Get playlist
        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(&rid, &pid)
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
            .count_room_children(&rid, &pid)
            .await
            .map_err(ApiError::from)?;
        let child_folder_count = i64_to_i32_api(child_folder_count, "child folder count")?;

        let media_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &pid)
            .await
            .unwrap_or(0);
        let media_count = i64_to_i32_api(media_count, "playlist media count")?;

        Ok(crate::proto::client::GetPlaylistResponse {
            playlist: Some(playlist_to_proto_with_availability(
                &playlist,
                media_count,
                playlist_availability.is_available(),
                &self.public_id_codec,
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
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.list_playlists_for_actor(&actor, req).await
    }

    pub async fn list_playlists_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        self.list_playlists_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn list_playlists_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::ListPlaylistsRequest,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        self.require_room_permission(actor, PermissionBits::VIEW_PLAYLIST)
            .await?;
        let rid = actor.room_id();
        let page = page_i32_to_usize(req.page.max(1), "page")?;
        let page_size = if req.page_size <= 0 {
            page_i32_to_usize(DEFAULT_PLAYLIST_PAGE_SIZE, "page_size")?
        } else {
            page_i32_to_usize(req.page_size.min(MAX_PLAYLIST_PAGE_SIZE), "page_size")?
        };
        let search =
            normalize_non_empty_filter(&req.search).map(|value| value.to_ascii_lowercase());
        let source_provider = normalize_non_empty_filter(&req.source_provider);
        let provider_instance_name = normalize_non_empty_filter(&req.provider_instance_name);
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
            let parent_id = crate::impls::proto_validated_playlist_id(
                req.parent_id.clone(),
                &self.public_id_codec,
            )?;
            let parent = self
                .room_service
                .playlist_service()
                .get_room_playlist(&rid, &parent_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Parent playlist not found".to_string()))?;
            debug_assert_eq!(parent.room_id, rid);
            Some(parent_id)
        };
        let query = synctv_core::models::PlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(usize_to_u32_api(page, "page")?),
                Some(usize_to_u32_api(page_size, "page_size")?),
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
            .map_err(ApiError::from)?;
        let total = i64_to_i32_api(total, "playlist total")?;
        let offset = (page - 1) * page_size;
        let playlists = self
            .room_service
            .list_client_playlists(
                &rid,
                parent_id.as_ref(),
                &query,
                usize_to_i64_api(page_size, "page_size")?,
                usize_to_i64_api(offset, "offset")?,
            )
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch media counts to avoid N+1 queries.
        let playlist_ids: Vec<synctv_core::models::PlaylistId> =
            playlists.iter().map(|entry| entry.playlist.id).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .unwrap_or_default();

        let proto_playlists: Vec<_> = playlists
            .iter()
            .map(|entry| {
                let item_count = i64_to_i32_api(
                    counts.get(&entry.playlist.id).copied().unwrap_or(0),
                    "playlist item count",
                )?;
                Ok(playlist_to_proto_with_availability(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                    &self.public_id_codec,
                ))
            })
            .collect::<std::result::Result<Vec<_>, ApiError>>()?;

        Ok(crate::proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
            total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_create_playlist_request, build_delete_playlist_request, build_move_playlist_request,
        build_update_playlist_request,
    };
    use synctv_core::models::{PlaylistId, RoomId};

    #[test]
    fn build_create_playlist_request_rejects_invalid_proto_payload() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_create_playlist_request(
            &RoomId::new(),
            crate::proto::client::CreatePlaylistRequest {
                name: "a".repeat(256),
                parent_id: String::new(),
                source_provider: "alist".into(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
            },
            &codec,
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
        let codec = crate::PublicIdCodec::default_for_tests();
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
            &codec,
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
        let codec = crate::PublicIdCodec::default_for_tests();
        let room_id = RoomId::new();
        let parent_id = PlaylistId::expect_positive(123);
        let parent_public_id = codec.encode_playlist_id(parent_id).unwrap();
        let request = build_create_playlist_request(
            &room_id,
            crate::proto::client::CreatePlaylistRequest {
                name: "Child".into(),
                parent_id: parent_public_id,
                source_provider: String::new(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
            },
            &codec,
        )
        .expect("valid request");

        assert_eq!(request.parent_id, Some(parent_id));
    }

    #[test]
    fn build_update_playlist_request_rejects_long_name() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_update_playlist_request(
            crate::proto::client::UpdatePlaylistRequest {
                playlist_id: codec
                    .encode_playlist_id(PlaylistId::expect_positive(1))
                    .unwrap(),
                name: "a".repeat(256),
            },
            &codec,
        )
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
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_move_playlist_request(
            crate::proto::client::MovePlaylistRequest {
                playlist_id: codec
                    .encode_playlist_id(PlaylistId::expect_positive(1))
                    .unwrap(),
                anchor: None,
            },
            &codec,
        )
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
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_delete_playlist_request(
            crate::proto::client::DeletePlaylistRequest {
                playlist_id: "bad-playlist".into(),
                force: false,
            },
            &codec,
        )
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
        let codec = crate::PublicIdCodec::default_for_tests();
        let playlist_id = PlaylistId::expect_positive(123);
        let request = build_delete_playlist_request(
            crate::proto::client::DeletePlaylistRequest {
                playlist_id: codec.encode_playlist_id(playlist_id).unwrap(),
                force: true,
            },
            &codec,
        )
        .unwrap();

        assert_eq!(request.0, playlist_id);
        assert!(request.1);
    }
}
