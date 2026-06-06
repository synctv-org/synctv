//! Playlist operations: create, update, delete, list playlists

use serde_json::Value as JsonValue;
use synctv_core::models::{
    PlaylistListSortBy as CorePlaylistListSortBy, SortDirection as CoreSortDirection, UserId,
};
use synctv_core::service::playlist::{
    CreatePlaylistRequest as CoreCreatePlaylistRequest,
    MovePlaylistRequest as CoreMovePlaylistRequest, SetPlaylistRequest as CoreSetPlaylistRequest,
};

use super::media::{
    file_cover_proto_to_stored_file, file_upload_session_to_playlist_cover_proto,
    parse_json_metadata, playlist_cover_object_to_proto,
};
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

fn proto_playlist_list_sort_by(value: i32) -> Result<CorePlaylistListSortBy, ApiError> {
    match crate::proto::client::PlaylistListSortBy::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported playlist list sort field".to_string()))?
    {
        crate::proto::client::PlaylistListSortBy::Unspecified
        | crate::proto::client::PlaylistListSortBy::Position => {
            Ok(CorePlaylistListSortBy::Position)
        }
        crate::proto::client::PlaylistListSortBy::Name => Ok(CorePlaylistListSortBy::Name),
        crate::proto::client::PlaylistListSortBy::CreatedAt => {
            Ok(CorePlaylistListSortBy::CreatedAt)
        }
        crate::proto::client::PlaylistListSortBy::UpdatedAt => {
            Ok(CorePlaylistListSortBy::UpdatedAt)
        }
    }
}

fn proto_sort_direction(value: i32) -> Result<CoreSortDirection, ApiError> {
    match crate::proto::client::SortDirection::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        crate::proto::client::SortDirection::Unspecified
        | crate::proto::client::SortDirection::Asc => Ok(CoreSortDirection::Asc),
        crate::proto::client::SortDirection::Desc => Ok(CoreSortDirection::Desc),
    }
}

fn proto_resource_availability_filter(value: i32) -> Result<Option<bool>, ApiError> {
    match crate::proto::client::ResourceAvailabilityFilter::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported availability filter".to_string()))?
    {
        crate::proto::client::ResourceAvailabilityFilter::All => Ok(None),
        crate::proto::client::ResourceAvailabilityFilter::Available => Ok(Some(true)),
        crate::proto::client::ResourceAvailabilityFilter::Unavailable => Ok(Some(false)),
    }
}

fn optional_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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
        description,
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
        description,
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
    if req.name.trim().is_empty() && req.description.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "playlist update requires at least one changed field".to_string(),
        ));
    }

    Ok(CoreSetPlaylistRequest {
        playlist_id: crate::impls::proto_validated_playlist_id(req.playlist_id, public_id_codec)?,
        name: (!req.name.trim().is_empty()).then_some(req.name),
        description: (!req.description.trim().is_empty()).then_some(req.description),
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
    async fn playlist_actor_username_for_event(
        &self,
        user_id: &UserId,
    ) -> Result<String, ApiError> {
        self.user_service
            .get_user(user_id)
            .await
            .map(|user| user.username)
            .map_err(ApiError::from)
    }

    async fn playlist_media_count_i32(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist_id: &synctv_core::models::PlaylistId,
    ) -> Result<i32, ApiError> {
        let count = self
            .room_service
            .media_service()
            .count_room_playlist_media(room_id, playlist_id)
            .await
            .map_err(ApiError::from)?;
        i64_to_i32_api(count, "playlist item count")
    }

    pub async fn create_playlist(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, ApiError> {
        let uid = *user_id;
        let actor_id = uid;
        let rid = self.parse_room_id(room_id)?;

        // Playlists/folders and media are both media resources; creating either
        // requires the shared resource creation permission.
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::CREATE_MEDIA_RESOURCE,
            )
            .await
            .map_err(Self::map_room_access_error)?;
        let service_req = build_create_playlist_request(&rid, req, &self.public_id_codec)?;
        let actor_username = self.playlist_actor_username_for_event(&actor_id).await?;

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
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        // Invalidate room cache on other replicas for playlist structure change
        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self.playlist_media_count_i32(&rid, &playlist.id).await?;

        Ok(crate::proto::client::CreatePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    &playlist,
                    item_count,
                    true,
                    Some(uid),
                )
                .await?,
            ),
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

        let service_req = build_update_playlist_request(req, &self.public_id_codec)?;
        let actor_username = self.playlist_actor_username_for_event(&actor_id).await?;

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
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        // Invalidate room cache on other replicas for playlist update
        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self.playlist_media_count_i32(&rid, &playlist.id).await?;

        Ok(crate::proto::client::UpdatePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    &playlist,
                    item_count,
                    true,
                    Some(uid),
                )
                .await?,
            ),
        })
    }

    async fn playlist_update_response(
        &self,
        playlist: &synctv_core::models::Playlist,
        user_id: UserId,
        is_available: bool,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        let item_count = self
            .playlist_media_count_i32(&playlist.room_id, &playlist.id)
            .await?;
        Ok(crate::proto::client::UpdatePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    playlist,
                    item_count,
                    is_available,
                    Some(user_id),
                )
                .await?,
            ),
        })
    }

    pub async fn create_playlist_cover_upload_session(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistCoverUploadSessionRequest,
    ) -> Result<crate::proto::client::CreatePlaylistCoverUploadSessionResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let playlist_id = crate::impls::parse_playlist_id_param(
            &req.playlist_id,
            "playlist_id",
            &self.public_id_codec,
        )?;
        let session = self
            .room_service
            .playlist_service()
            .create_cover_upload_session(
                rid,
                playlist_id,
                *user_id,
                synctv_core::service::playlist::CreatePlaylistCoverUploadSession {
                    client_cover_id: optional_trimmed_string(&req.client_cover_id),
                    mime_type: req.mime_type,
                    size_bytes: req.size_bytes,
                    width: (req.width > 0).then_some(req.width),
                    height: (req.height > 0).then_some(req.height),
                    checksum_sha256: optional_trimmed_string(&req.checksum_sha256),
                    metadata: parse_json_metadata(&req.metadata)?,
                },
            )
            .await
            .map_err(ApiError::from)?;
        Ok(
            crate::proto::client::CreatePlaylistCoverUploadSessionResponse {
                session: Some(file_upload_session_to_playlist_cover_proto(session)?),
            },
        )
    }

    pub async fn upload_playlist_cover_object(
        &self,
        req: crate::proto::client::UploadPlaylistCoverObjectRequest,
    ) -> Result<crate::proto::client::UploadPlaylistCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .playlist_service()
            .store_cover_upload_object(
                &req.encoded_object_key,
                &req.token,
                req.content_type.as_deref(),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::UploadPlaylistCoverObjectResponse {
            object: Some(playlist_cover_object_to_proto(&blob)),
        })
    }

    pub async fn get_playlist_cover_object(
        &self,
        req: crate::proto::client::GetPlaylistCoverObjectRequest,
    ) -> Result<crate::proto::client::PlaylistCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .playlist_service()
            .get_cover_object(&req.encoded_object_key, &req.token)
            .await
            .map_err(ApiError::from)?;
        Ok(playlist_cover_object_to_proto(&blob))
    }

    pub async fn update_playlist_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdatePlaylistCoverRequest,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let playlist_id = crate::impls::parse_playlist_id_param(
            &req.playlist_id,
            "playlist_id",
            &self.public_id_codec,
        )?;
        let cover = req
            .cover
            .ok_or_else(|| ApiError::InvalidInput("cover is required".to_string()))?;
        let playlist = self
            .room_service
            .playlist_service()
            .update_cover(
                rid,
                playlist_id,
                *user_id,
                file_cover_proto_to_stored_file(cover)?,
            )
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.playlist_update_response(&playlist, *user_id, true)
            .await
    }

    pub async fn clear_playlist_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::ClearPlaylistCoverRequest,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let playlist_id = crate::impls::parse_playlist_id_param(
            &req.playlist_id,
            "playlist_id",
            &self.public_id_codec,
        )?;
        let playlist = self
            .room_service
            .playlist_service()
            .clear_cover(rid, playlist_id, *user_id)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.playlist_update_response(&playlist, *user_id, true)
            .await
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
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::REORDER_MEDIA_RESOURCES,
            )
            .await
            .map_err(Self::map_room_access_error)?;
        let service_req = build_move_playlist_request(req, &self.public_id_codec)?;
        let actor_username = self.playlist_actor_username_for_event(&actor_id).await?;

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
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.room_cache_fanout.publish_invalidation(&rid);

        let item_count = self.playlist_media_count_i32(&rid, &playlist.id).await?;

        Ok(crate::proto::client::MovePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    &playlist,
                    item_count,
                    true,
                    Some(uid),
                )
                .await?,
            ),
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
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_MEDIA_RESOURCES,
        )
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

        let media_count = self.playlist_media_count_i32(&rid, &pid).await?;

        Ok(crate::proto::client::GetPlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    &playlist,
                    media_count,
                    playlist_availability.is_available(),
                    actor.user_id(),
                )
                .await?,
            ),
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
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_MEDIA_RESOURCES,
        )
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
        let sort_by = proto_playlist_list_sort_by(req.sort_by)?;
        let sort_direction = proto_sort_direction(req.sort_direction)?;

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
            availability: proto_resource_availability_filter(req.availability)?,
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
            .map_err(ApiError::from)?;

        let mut proto_playlists = Vec::with_capacity(playlists.len());
        for entry in &playlists {
            let item_count = i64_to_i32_api(
                counts.get(&entry.playlist.id).copied().unwrap_or(0),
                "playlist item count",
            )?;
            proto_playlists.push(
                self.playlist_to_proto_for_viewer_with_loaded_cover(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                    actor.user_id(),
                )
                .await?,
            );
        }

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
        build_update_playlist_request, proto_playlist_list_sort_by,
        proto_resource_availability_filter, proto_sort_direction,
    };
    use synctv_core::models::{PlaylistId, RoomId};

    #[test]
    fn playlist_query_enum_mappers_reject_unknown_values_and_preserve_defaults() {
        assert_eq!(
            proto_playlist_list_sort_by(
                crate::proto::client::PlaylistListSortBy::Unspecified as i32
            )
            .expect("unspecified playlist sort should be accepted"),
            synctv_core::models::PlaylistListSortBy::Position
        );
        assert_eq!(
            proto_sort_direction(crate::proto::client::SortDirection::Unspecified as i32)
                .expect("unspecified sort direction should be accepted"),
            synctv_core::models::SortDirection::Asc
        );
        assert_eq!(
            proto_resource_availability_filter(
                crate::proto::client::ResourceAvailabilityFilter::All as i32
            )
            .expect("all availability filter should be accepted"),
            None
        );

        assert!(matches!(
            proto_playlist_list_sort_by(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("playlist list sort")
        ));
        assert!(matches!(
            proto_sort_direction(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("sort direction")
        ));
        assert!(matches!(
            proto_resource_availability_filter(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("availability")
        ));
    }

    #[test]
    fn build_create_playlist_request_rejects_invalid_proto_payload() {
        let codec = crate::PublicIdCodec::plain();
        let error = build_create_playlist_request(
            &RoomId::new(),
            crate::proto::client::CreatePlaylistRequest {
                name: "a".repeat(256),
                parent_id: String::new(),
                source_provider: "alist".into(),
                source_config: Vec::new(),
                provider_instance_name: String::new(),
                description: String::new(),
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
        let codec = crate::PublicIdCodec::plain();
        let room_id = RoomId::new();
        let request = build_create_playlist_request(
            &room_id,
            crate::proto::client::CreatePlaylistRequest {
                name: "Dynamic".into(),
                parent_id: String::new(),
                source_provider: "alist".into(),
                source_config: serde_json::to_vec(&serde_json::json!({"path":"/tv"})).unwrap(),
                provider_instance_name: "alist-main".into(),
                description: String::new(),
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
        let codec = crate::PublicIdCodec::plain();
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
                description: String::new(),
            },
            &codec,
        )
        .expect("valid request");

        assert_eq!(request.parent_id, Some(parent_id));
    }

    #[test]
    fn build_update_playlist_request_rejects_long_name() {
        let codec = crate::PublicIdCodec::plain();
        let error = build_update_playlist_request(
            crate::proto::client::UpdatePlaylistRequest {
                playlist_id: codec
                    .encode_playlist_id(PlaylistId::expect_positive(1))
                    .unwrap(),
                name: "a".repeat(256),
                description: String::new(),
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
        let codec = crate::PublicIdCodec::plain();
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
        let codec = crate::PublicIdCodec::plain();
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
        let codec = crate::PublicIdCodec::plain();
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
