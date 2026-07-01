use synctv_core::{
    models::{
        MediaListQuery as CoreMediaListQuery, PlaylistListQuery as CorePlaylistListQuery, UserId,
    },
    provider::DynamicListQuery,
};

use crate::impls::client::convert::{
    optional_provider_target_to_proto, provider_target_from_proto,
    try_media_to_proto_for_viewer_with_cover, try_playlist_path_node_to_proto,
    try_playlist_to_proto_for_viewer_with_cover,
};
use crate::impls::client::media::{
    build_move_media_fanout_plan, prepare_delete_entries_outbox_fanout,
};
use crate::impls::source_provider::proto_source_provider_filter;

use super::{
    i64_count_to_usize, i64_to_i32_api, map_admin_media_sort, map_admin_playlist_sort,
    map_admin_playlist_sort_from_media_sort, map_client_sort_direction,
    map_resource_availability_filter, normalize_non_empty_filter, page_i32_to_usize,
    page_offset_usize, page_size_i32_to_usize, u64_to_i64_api, usize_to_i32_api, usize_to_i64_api,
    AdminApiImpl, ApiError,
};

impl AdminApiImpl {
    async fn media_to_proto_for_admin_with_loaded_cover(
        &self,
        media: &synctv_core::models::Media,
        is_available: bool,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let cover = self
            .load_admin_file_reference(media.cover_file_reference_id)
            .await?;
        let cover_url = cover
            .as_ref()
            .map(|file| {
                self.admin_stored_file_reference_url(
                    file,
                    &synctv_core::service::media_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        try_media_to_proto_for_viewer_with_cover(
            media,
            is_available,
            media.creator_id,
            cover.as_ref(),
            cover_url.as_deref(),
            &self.public_id_codec,
        )
    }

    async fn playlist_to_proto_for_admin_with_loaded_cover(
        &self,
        playlist: &synctv_core::models::Playlist,
        item_count: i32,
        is_available: bool,
    ) -> Result<synctv_proto::client::Playlist, ApiError> {
        let cover = self
            .load_admin_file_reference(playlist.cover_file_reference_id)
            .await?;
        let cover_url = cover
            .as_ref()
            .map(|file| {
                self.admin_stored_file_reference_url(
                    file,
                    &synctv_core::service::playlist_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        try_playlist_to_proto_for_viewer_with_cover(
            playlist,
            item_count,
            is_available,
            playlist.creator_id,
            cover.as_ref(),
            cover_url.as_deref(),
            &self.public_id_codec,
        )
    }

    async fn load_admin_file_reference(
        &self,
        file_reference_id: Option<i64>,
    ) -> Result<Option<synctv_core::models::StoredFileReference>, ApiError> {
        let Some(file_reference_id) = file_reference_id else {
            return Ok(None);
        };
        synctv_core::repository::FileStorageRepository::new(self.user_service.pool().clone())
            .get_reference_by_id(file_reference_id)
            .await
            .map_err(ApiError::from)
    }

    fn admin_stored_file_reference_url(
        &self,
        file: &synctv_core::models::StoredFileReference,
        policy: &synctv_core::models::FileUploadPolicy,
    ) -> Result<Option<String>, ApiError> {
        let Some(storage) = self
            .user_service
            .file_storage_service()
            .or_else(|| self.room_service.file_storage_service())
            .or_else(|| self.room_service.playlist_service().file_storage_service())
            .or_else(|| self.room_service.media_service().file_storage_service())
        else {
            return Ok(None);
        };
        storage
            .object_url(
                &file.storage_backend,
                &file.object_key,
                &policy.database_object_route_prefix,
            )
            .map_err(ApiError::from)
    }

    pub async fn get_playlist(
        &self,
        room_id: &str,
        playlist_id: &str,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::GetPlaylistResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let pid = crate::impls::parse_playlist_id_param(
            playlist_id,
            "playlist_id",
            &self.public_id_codec,
        )?;

        self.require_admin_actor(admin_user_id).await?;

        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(&rid, &pid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {playlist_id} not found")))?;

        let child_folder_count = i64_to_i32_api(
            self.room_service
                .playlist_service()
                .count_room_children(&rid, &pid)
                .await
                .map_err(ApiError::from)?,
            "child playlist count",
        )?;
        let media_count = i64_to_i32_api(
            self.room_service
                .media_service()
                .count_room_playlist_media(&rid, &pid)
                .await
                .map_err(ApiError::from)?,
            "playlist media count",
        )?;

        Ok(synctv_proto::client::GetPlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_admin_with_loaded_cover(&playlist, media_count, true)
                    .await?,
            ),
            child_folder_count,
            media_count,
        })
    }

    pub async fn list_playlists(
        &self,
        room_id: &str,
        req: synctv_proto::client::ListPlaylistsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::ListPlaylistsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;

        self.require_admin_actor(admin_user_id).await?;

        let page = page_i32_to_usize(req.page)?;
        let page_size = if req.page_size <= 0 {
            page_size_i32_to_usize(50, 100)?
        } else {
            page_size_i32_to_usize(req.page_size, 100)?
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
        let query = CorePlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(u32::try_from(page).map_err(|_| {
                    ApiError::Internal("playlist page exceeds u32::MAX".to_string())
                })?),
                Some(u32::try_from(page_size).map_err(|_| {
                    ApiError::Internal("playlist page_size exceeds u32::MAX".to_string())
                })?),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: proto_source_provider_filter(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: req.dynamic_only,
            availability: map_resource_availability_filter(req.availability)?,
            sort_by: map_admin_playlist_sort(req.sort_by)?,
            sort_direction: map_client_sort_direction(req.sort_direction)?,
        };
        let total = i64_to_i32_api(
            self.room_service
                .count_client_playlists(&rid, parent_id.as_ref(), &query)
                .await
                .map_err(ApiError::from)?,
            "playlist total",
        )?;
        let offset = (page - 1) * page_size;
        let limit = usize_to_i64_api(page_size, "playlist page size")?;
        let offset = usize_to_i64_api(offset, "playlist offset")?;
        let playlists = self
            .room_service
            .list_client_playlists(&rid, parent_id.as_ref(), &query, limit, offset)
            .await
            .map_err(ApiError::from)?;

        let playlist_ids: Vec<_> = playlists.iter().map(|pl| pl.playlist.id).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .map_err(ApiError::from)?;

        let mut proto_playlists = Vec::with_capacity(playlists.len());
        for entry in &playlists {
            let item_count = i64_to_i32_api(
                crate::impls::playlist_media_count_or_zero(&counts, &entry.playlist.id),
                "playlist item count",
            )?;
            proto_playlists.push(
                self.playlist_to_proto_for_admin_with_loaded_cover(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                )
                .await?,
            );
        }

        Ok(synctv_proto::client::ListPlaylistsResponse {
            playlists: proto_playlists,
            total,
        })
    }

    pub async fn update_playlist(
        &self,
        room_id: &str,
        req: synctv_proto::client::UpdatePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::UpdatePlaylistResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let service_req = crate::impls::client::playlist::build_update_playlist_request(
            req,
            &self.public_id_codec,
        )?;
        let actor_username = admin_actor.username;

        let prepared_outbox_fanout = self.playlist_fanout.prepare_updated_outbox_fanout(
            rid,
            *admin_user_id,
            actor_username.clone(),
        );
        let playlist = self
            .room_service
            .playlist_service()
            .admin_set_playlist_with_outbox(
                rid,
                *admin_user_id,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &playlist.id)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_to_i32_api(count, "playlist media count"))?;

        Ok(synctv_proto::client::UpdatePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_admin_with_loaded_cover(&playlist, item_count, true)
                    .await?,
            ),
        })
    }

    pub async fn move_playlist(
        &self,
        room_id: &str,
        req: synctv_proto::client::MovePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::MovePlaylistResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let admin_actor = self.require_admin_actor(admin_user_id).await?;
        let service_req = crate::impls::client::playlist::build_move_playlist_request(
            req,
            &self.public_id_codec,
        )?;
        let actor_username = admin_actor.username;

        let prepared_outbox_fanout = self.playlist_fanout.prepare_updated_outbox_fanout(
            rid,
            *admin_user_id,
            actor_username.clone(),
        );
        let playlist = self
            .room_service
            .playlist_service()
            .admin_move_playlist_with_outbox(
                rid,
                *admin_user_id,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&rid, &playlist.id)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_to_i32_api(count, "playlist media count"))?;

        Ok(synctv_proto::client::MovePlaylistResponse {
            playlist: Some(
                self.playlist_to_proto_for_admin_with_loaded_cover(&playlist, item_count, true)
                    .await?,
            ),
        })
    }

    pub async fn delete_playlist(
        &self,
        room_id: &str,
        req: synctv_proto::client::DeletePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::DeletePlaylistResponse, ApiError> {
        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;

        let (playlist_id, force) = crate::impls::client::playlist::build_delete_playlist_request(
            req,
            &self.public_id_codec,
        )?;
        let prepared_outbox_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.playback_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            *admin_user_id,
            actor.username().to_string(),
        );
        let result = self
            .room_service
            .admin_delete_entries_as_with_outbox(
                rid,
                &actor,
                synctv_core::service::room::DeleteEntriesRequest {
                    playlist_ids: vec![playlist_id],
                    media_ids: Vec::new(),
                    force,
                },
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        for media_id in &result.deleted_media_ids {
            if let Err(error) = self
                .realtime_lifecycle
                .kick_local_stream(&rid, media_id)
                .await
            {
                tracing::warn!(
                    room_id = %rid,
                    media_id = %media_id,
                    error = %error,
                    "Failed to kick local stream after playlist deletion"
                );
            }
        }

        Ok(synctv_proto::client::DeletePlaylistResponse { success: true })
    }

    pub async fn list_media(
        &self,
        room_id: &str,
        req: synctv_proto::client::ListPlaylistItemsRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid = crate::impls::parse_room_id_param(room_id, "room_id", &self.public_id_codec)?;

        self.require_admin_actor(admin_user_id).await?;
        let target = provider_target_from_proto(req.target.clone())?;

        let Some(playlist_id) = (if req.playlist_id.is_empty() {
            None
        } else {
            Some(crate::impls::proto_validated_playlist_id(
                req.playlist_id.clone(),
                &self.public_id_codec,
            )?)
        }) else {
            if target.is_some() {
                return Err(ApiError::InvalidInput(
                    "target must be omitted when browsing the room root".to_string(),
                ));
            }
            let playlist_query = CorePlaylistListQuery {
                pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
                search: normalize_non_empty_filter(&req.search),
                source_provider: proto_source_provider_filter(req.source_provider)?,
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability: map_resource_availability_filter(req.availability)?,
                sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by)?,
                sort_direction: map_client_sort_direction(req.sort_direction)?,
            };
            let media_query = CoreMediaListQuery {
                pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
                search: normalize_non_empty_filter(&req.search),
                source_provider: proto_source_provider_filter(req.source_provider)?,
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                availability: map_resource_availability_filter(req.availability)?,
                sort_by: map_admin_media_sort(req.sort_by)?,
                sort_direction: map_client_sort_direction(req.sort_direction)?,
            };
            let folder_count = self
                .room_service
                .count_client_playlists(&rid, None, &playlist_query)
                .await
                .map_err(ApiError::from)
                .and_then(|count| i64_count_to_usize(count, "root playlist count"))?;
            let file_count = self
                .room_service
                .count_client_media(&rid, None, &media_query)
                .await
                .map_err(ApiError::from)
                .and_then(|count| i64_count_to_usize(count, "root media count"))?;
            let total = folder_count.checked_add(file_count).ok_or_else(|| {
                ApiError::Internal("root playlist/media count exceeds usize::MAX".to_string())
            })?;
            let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100)?;
            let page = page_i32_to_usize(req.page)?;
            let skip = page_offset_usize(page, page_size, "root playlist/media offset")?;
            let (playlists, media) = if skip < folder_count {
                let playlist_limit = usize_to_i64_api(page_size, "playlist page size")?;
                let playlist_offset = usize_to_i64_api(skip, "playlist offset")?;
                let playlists = self
                    .room_service
                    .list_client_playlists(
                        &rid,
                        None,
                        &playlist_query,
                        playlist_limit,
                        playlist_offset,
                    )
                    .await
                    .map_err(ApiError::from)?;
                let remaining = page_size.saturating_sub(playlists.len());
                let media = if remaining > 0 {
                    let media_limit = usize_to_i64_api(remaining, "media page size")?;
                    self.room_service
                        .list_client_media(&rid, None, &media_query, media_limit, 0)
                        .await
                        .map_err(ApiError::from)?
                } else {
                    Vec::new()
                };
                (playlists, media)
            } else {
                let media_skip = skip - folder_count;
                let media_limit = usize_to_i64_api(page_size, "media page size")?;
                let media_offset = usize_to_i64_api(media_skip, "media offset")?;
                let media = self
                    .room_service
                    .list_client_media(&rid, None, &media_query, media_limit, media_offset)
                    .await
                    .map_err(ApiError::from)?;
                (Vec::new(), media)
            };
            let folder_ids: Vec<_> = playlists.iter().map(|pl| pl.playlist.id).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&folder_ids)
                .await
                .map_err(ApiError::from)?;
            let mut proto_playlists = Vec::with_capacity(playlists.len());
            for entry in &playlists {
                let item_count = i64_to_i32_api(
                    crate::impls::playlist_media_count_or_zero(&counts, &entry.playlist.id),
                    "playlist item count",
                )?;
                proto_playlists.push(
                    self.playlist_to_proto_for_admin_with_loaded_cover(
                        &entry.playlist,
                        item_count,
                        entry.is_available,
                    )
                    .await?,
                );
            }
            let mut proto_media = Vec::with_capacity(media.len());
            for entry in &media {
                proto_media.push(
                    self.media_to_proto_for_admin_with_loaded_cover(
                        &entry.media,
                        entry.is_available,
                    )
                    .await?,
                );
            }

            let mut response = synctv_proto::client::ListPlaylistItemsResponse {
                playlists: proto_playlists,
                media: proto_media,
                total: usize_to_i32_api(total, "playlist item total")?,
                folder_count: usize_to_i32_api(folder_count, "playlist folder count")?,
                file_count: usize_to_i32_api(file_count, "playlist file count")?,
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
                version: String::new(),
            };
            response.version =
                crate::impls::client::media::compute_playlist_items_response_version(&response)?;
            return Ok(response);
        };

        let playlist = self
            .room_service
            .playlist_service()
            .get_room_playlist(&rid, &playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {} not found", req.playlist_id)))?;

        let static_path = self
            .room_service
            .playlist_service()
            .get_room_playlist_path(&rid, &playlist_id)
            .await
            .map_err(ApiError::from)?;
        let mut current_path = static_path
            .iter()
            .map(|playlist| try_playlist_path_node_to_proto(playlist, &self.public_id_codec))
            .collect::<Result<Vec<_>, ApiError>>()?;

        if playlist.is_dynamic() {
            if !crate::impls::client::media::validate_dynamic_playlist_query_support(
                &playlist, &req,
            )? {
                let mut response = synctv_proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total: 0,
                    folder_count: 0,
                    file_count: 0,
                    dynamic_items: Vec::new(),
                    current_path,
                    version: String::new(),
                };
                response.version =
                    crate::impls::client::media::compute_playlist_items_response_version(
                        &response,
                    )?;
                return Ok(response);
            }

            let page = page_i32_to_usize(req.page)?;
            let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100)?;
            let public_credential_owner_id = playlist
                .creator_id
                .map(|creator_id| self.public_id_codec.encode_user_id(creator_id))
                .transpose()
                .map_err(|error| {
                    ApiError::Internal(format!(
                        "Failed to encode credential owner public id: {error}"
                    ))
                })?;
            let items = self
                .room_service
                .media_service()
                .admin_list_dynamic_playlist_items(
                    rid,
                    *admin_user_id,
                    &playlist_id,
                    target.as_ref(),
                    DynamicListQuery {
                        page,
                        page_size,
                        search: crate::impls::client::media::normalize_non_empty_filter(
                            &req.search,
                        ),
                        refresh: req.refresh,
                    },
                    public_credential_owner_id.as_deref(),
                )
                .await
                .map_err(ApiError::from)?;

            let dynamic_items = items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::ItemType;
                    let item_type = match item.item_type {
                        ItemType::Playlist => synctv_proto::client::ItemType::Playlist as i32,
                        ItemType::Media => synctv_proto::client::ItemType::Media as i32,
                    };

                    let thumbnail = match item.thumbnail {
                        Some(thumbnail) => {
                            let public_room_id =
                                self.public_id_codec.encode_room_id(rid).map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode room public id: {error}"
                                    ))
                                })?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode user public id: {error}"
                                    ))
                                })?;
                            Some(
                                crate::http::providers::emby::sign_emby_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        None => None,
                    };

                    Ok(synctv_proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        target: optional_provider_target_to_proto(Some(&item.target)),
                        size: item
                            .size
                            .map(|size| u64_to_i64_api(size, "dynamic playlist item size"))
                            .transpose()?,
                        thumbnail,
                        modified_at: item.modified_at,
                        description: item.description.unwrap_or_default(),
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;

            let browse_path = self
                .room_service
                .media_service()
                .admin_get_dynamic_playlist_browse_path(
                    rid,
                    *admin_user_id,
                    &playlist_id,
                    target.as_ref(),
                )
                .await
                .map_err(ApiError::from)?;
            current_path.extend(browse_path.into_iter().map(|segment| {
                synctv_proto::client::PlaylistBrowsePathNode {
                    playlist_id: String::new(),
                    name: segment.name,
                    target: optional_provider_target_to_proto(Some(&segment.target)),
                }
            }));

            let mut response = synctv_proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total: -1,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
                current_path,
                version: String::new(),
            };
            response.version =
                crate::impls::client::media::compute_playlist_items_response_version(&response)?;
            return Ok(response);
        }

        if target.is_some() {
            return Err(ApiError::InvalidInput(
                "target must be omitted when browsing a static playlist".to_string(),
            ));
        }

        let playlist_query = CorePlaylistListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: normalize_non_empty_filter(&req.search),
            source_provider: proto_source_provider_filter(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability: map_resource_availability_filter(req.availability)?,
            sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by)?,
            sort_direction: map_client_sort_direction(req.sort_direction)?,
        };
        let media_query = CoreMediaListQuery {
            pagination: crate::impls::proto_page_params(req.page, req.page_size, 50, 100),
            search: normalize_non_empty_filter(&req.search),
            source_provider: proto_source_provider_filter(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            availability: map_resource_availability_filter(req.availability)?,
            sort_by: map_admin_media_sort(req.sort_by)?,
            sort_direction: map_client_sort_direction(req.sort_direction)?,
        };
        let folder_count = self
            .room_service
            .count_client_playlists(&rid, Some(&playlist_id), &playlist_query)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_count_to_usize(count, "playlist child playlist count"))?;
        let file_count = self
            .room_service
            .count_client_media(&rid, Some(&playlist_id), &media_query)
            .await
            .map_err(ApiError::from)
            .and_then(|count| i64_count_to_usize(count, "playlist child media count"))?;
        let total = folder_count.checked_add(file_count).ok_or_else(|| {
            ApiError::Internal("playlist child count exceeds usize::MAX".to_string())
        })?;
        let page_size = crate::impls::proto_page_size_usize(req.page_size, 50, 100)?;
        let page = page_i32_to_usize(req.page)?;
        let skip = page_offset_usize(page, page_size, "playlist child offset")?;
        let (playlists, media) = if skip < folder_count {
            let playlist_limit = usize_to_i64_api(page_size, "playlist page size")?;
            let playlist_offset = usize_to_i64_api(skip, "playlist offset")?;
            let playlists = self
                .room_service
                .list_client_playlists(
                    &rid,
                    Some(&playlist_id),
                    &playlist_query,
                    playlist_limit,
                    playlist_offset,
                )
                .await
                .map_err(ApiError::from)?;
            let remaining = page_size.saturating_sub(playlists.len());
            let media = if remaining > 0 {
                let media_limit = usize_to_i64_api(remaining, "media page size")?;
                self.room_service
                    .list_client_media(&rid, Some(&playlist_id), &media_query, media_limit, 0)
                    .await
                    .map_err(ApiError::from)?
            } else {
                Vec::new()
            };
            (playlists, media)
        } else {
            let media_skip = skip - folder_count;
            let media_limit = usize_to_i64_api(page_size, "media page size")?;
            let media_offset = usize_to_i64_api(media_skip, "media offset")?;
            let media = self
                .room_service
                .list_client_media(
                    &rid,
                    Some(&playlist_id),
                    &media_query,
                    media_limit,
                    media_offset,
                )
                .await
                .map_err(ApiError::from)?;
            (Vec::new(), media)
        };
        let folder_ids: Vec<_> = playlists.iter().map(|pl| pl.playlist.id).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&folder_ids)
            .await
            .map_err(ApiError::from)?;
        let mut proto_playlists = Vec::with_capacity(playlists.len());
        for entry in &playlists {
            let item_count = i64_to_i32_api(
                crate::impls::playlist_media_count_or_zero(&counts, &entry.playlist.id),
                "playlist item count",
            )?;
            proto_playlists.push(
                self.playlist_to_proto_for_admin_with_loaded_cover(
                    &entry.playlist,
                    item_count,
                    entry.is_available,
                )
                .await?,
            );
        }
        let mut proto_media = Vec::with_capacity(media.len());
        for entry in &media {
            proto_media.push(
                self.media_to_proto_for_admin_with_loaded_cover(&entry.media, entry.is_available)
                    .await?,
            );
        }

        let mut response = synctv_proto::client::ListPlaylistItemsResponse {
            playlists: proto_playlists,
            media: proto_media,
            total: usize_to_i32_api(total, "playlist item total")?,
            folder_count: usize_to_i32_api(folder_count, "playlist folder count")?,
            file_count: usize_to_i32_api(file_count, "playlist file count")?,
            dynamic_items: Vec::new(),
            current_path,
            version: String::new(),
        };
        response.version =
            crate::impls::client::media::compute_playlist_items_response_version(&response)?;
        Ok(response)
    }

    pub async fn edit_media(
        &self,
        room_id: &str,
        req: synctv_proto::client::EditMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::EditMediaResponse, ApiError> {
        let rid = self
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.clone()))?;
        let service_req =
            crate::impls::client::media::build_edit_media_request(req, &self.public_id_codec)?;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let prepared_outbox_fanout = self.media_fanout.prepare_updated_outbox_fanout(
            rid,
            *admin_user_id,
            actor.username.clone(),
        );
        let media = self
            .room_service
            .media_service()
            .admin_edit_media_with_outbox(
                rid,
                *admin_user_id,
                &actor.username,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        Ok(synctv_proto::client::EditMediaResponse {
            media: Some(
                self.media_to_proto_for_admin_with_loaded_cover(&media, true)
                    .await?,
            ),
        })
    }

    pub async fn delete_media(
        &self,
        room_id: &str,
        req: synctv_proto::client::DeleteMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::DeleteMediaResponse, ApiError> {
        let rid = self
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.clone()))?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let prepared_outbox_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.playback_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            *admin_user_id,
            actor.username().to_string(),
        );
        let result = self
            .room_service
            .admin_delete_entries_as_with_outbox(
                rid,
                &actor,
                crate::impls::client::media::build_delete_entries_request(
                    crate::impls::client::media::build_delete_media_request(req)?,
                    &self.public_id_codec,
                )?
                .0,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        for media_id in &result.deleted_media_ids {
            if let Err(error) = self
                .realtime_lifecycle
                .kick_local_stream(&rid, media_id)
                .await
            {
                tracing::warn!(
                    room_id = %rid,
                    media_id = %media_id,
                    error = %error,
                    "Failed to kick local stream after media deletion"
                );
            }
        }

        Ok(synctv_proto::client::DeleteMediaResponse { success: true })
    }

    pub async fn move_media(
        &self,
        room_id: &str,
        req: synctv_proto::client::MoveMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::MoveMediaResponse, ApiError> {
        let rid = self
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.clone()))?;
        let service_req =
            crate::impls::client::media::build_move_media_request(req, &self.public_id_codec)?;
        let actor = self.require_admin_actor(admin_user_id).await?;

        let media_fanout_plan =
            build_move_media_fanout_plan(self.room_service.media_service(), &rid, &service_req)
                .await?;

        let prepared_outbox_fanout = self.media_fanout.prepare_move_outbox_fanout(
            rid,
            *admin_user_id,
            actor.username.clone(),
            media_fanout_plan,
        );
        let media = self
            .room_service
            .media_service()
            .admin_move_media_with_outbox(
                rid,
                *admin_user_id,
                &actor.username,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        self.publish_room_cache_invalidation(&rid);

        let mut proto_media = Vec::with_capacity(media.len());
        for media in &media {
            proto_media.push(
                self.media_to_proto_for_admin_with_loaded_cover(media, true)
                    .await?,
            );
        }

        Ok(synctv_proto::client::MoveMediaResponse {
            moved_count: usize_to_i32_api(media.len(), "moved media count")?,
            media: proto_media,
        })
    }
}
