use synctv_core::{
    models::{
        MediaListQuery as CoreMediaListQuery, PlaylistListQuery as CorePlaylistListQuery, UserId,
    },
    provider::{DynamicListQuery, DynamicPagination},
};

use crate::impls::client::convert::{
    optional_provider_target_to_proto, provider_target_from_proto,
    try_media_to_proto_for_viewer_with_cover, try_playlist_path_node_to_proto,
    try_playlist_to_proto_for_viewer_with_cover, MediaProtoView,
};
use crate::impls::client::media::{
    prepare_delete_entries_outbox_fanout, MoveMediaFanoutPlanner, PrepareDeleteEntriesOutboxFanout,
};
use crate::impls::source_provider::proto_source_provider_filter;

use super::{
    i64_count_to_usize, i64_to_i32_api, map_admin_media_sort, map_admin_playlist_sort,
    map_admin_playlist_sort_from_media_sort, map_client_sort_direction,
    map_resource_availability_filter, normalize_non_empty_filter, page_i32_to_usize,
    page_offset_usize, page_size_i32_to_usize, page_u32_to_usize, usize_to_i32_api,
    usize_to_i64_api, usize_to_u32_api, usize_to_u64_api, AdminApiImpl, ApiError,
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
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.admin_stored_file_reference_access(
                    file,
                    &synctv_core::service::media_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        let thumbnail = self
            .load_admin_file_reference(media.thumbnail_file_reference_id)
            .await?;
        let thumbnail_access = thumbnail
            .as_ref()
            .map(|file| {
                self.admin_stored_file_reference_access(
                    file,
                    &synctv_core::service::media_thumbnail_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        try_media_to_proto_for_viewer_with_cover(
            media,
            MediaProtoView {
                is_available,
                viewer_id: media.creator_id,
                cover: cover.as_ref(),
                cover_access: cover_access.as_ref(),
                thumbnail: thumbnail.as_ref(),
                thumbnail_access: thumbnail_access.as_ref(),
                public_id_codec: &self.public_id_codec,
            },
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
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.admin_stored_file_reference_access(
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
            cover_access.as_ref(),
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
        self.user_service
            .get_stored_file_reference(file_reference_id)
            .await
            .map_err(ApiError::from)
    }

    fn admin_stored_file_reference_access(
        &self,
        file: &synctv_core::models::StoredFileReference,
        policy: &synctv_core::models::FileUploadPolicy,
    ) -> Result<Option<crate::impls::stored_files::StoredFileObjectAccess>, ApiError> {
        let Some(storage) = crate::impls::stored_files::first_file_storage([
            self.user_service.file_storage_service(),
            self.room_service.file_storage_service(),
            self.room_service.playlist_service().file_storage_service(),
            self.room_service.media_service().file_storage_service(),
        ]) else {
            return Ok(None);
        };
        crate::impls::stored_files::stored_file_reference_access(storage.as_ref(), file, policy)
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
    ) -> Result<synctv_proto::client::Playlist, ApiError> {
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

        self.playlist_to_proto_for_admin_with_loaded_cover(&playlist, item_count, true)
            .await
    }

    pub async fn move_playlist(
        &self,
        room_id: &str,
        req: synctv_proto::client::MovePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<synctv_proto::client::Playlist, ApiError> {
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

        self.playlist_to_proto_for_admin_with_loaded_cover(&playlist, item_count, true)
            .await
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
        let prepared_outbox_fanout =
            prepare_delete_entries_outbox_fanout(PrepareDeleteEntriesOutboxFanout {
                clock: self.clock.clone(),
                media_fanout: self.media_fanout.clone(),
                playlist_fanout: self.playlist_fanout.clone(),
                playback_fanout: self.playback_fanout.clone(),
                realtime_fanout: self.realtime_fanout.clone(),
                room_id: rid,
                user_id: *admin_user_id,
                username: actor.username().to_string(),
            });
        let result = self
            .room_service
            .admin_delete_entries_as_with_outbox(
                rid,
                &actor,
                synctv_core::service::DeleteEntriesRequest {
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
                pagination: crate::impls::proto_page_params_u32(
                    crate::impls::client::media::playlist_items_page(&req)?,
                    req.page_size,
                    50,
                    100,
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: proto_source_provider_filter(req.source_provider)?,
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability: map_resource_availability_filter(req.availability)?,
                sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by)?,
                sort_direction: map_client_sort_direction(req.sort_direction)?,
            };
            let media_query = CoreMediaListQuery {
                pagination: crate::impls::proto_page_params_u32(
                    crate::impls::client::media::playlist_items_page(&req)?,
                    req.page_size,
                    50,
                    100,
                ),
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
            let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
            let page = page_u32_to_usize(crate::impls::client::media::playlist_items_page(&req)?)?;
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
                total: Some(usize_to_u64_api(total, "playlist item total")?),
                folder_count: usize_to_u64_api(folder_count, "playlist folder count")?,
                file_count: usize_to_u64_api(file_count, "playlist file count")?,
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
                version: String::new(),
                pagination: None,
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
                    total: Some(0),
                    folder_count: 0,
                    file_count: 0,
                    dynamic_items: Vec::new(),
                    current_path,
                    version: String::new(),
                    pagination: None,
                };
                response.version =
                    crate::impls::client::media::compute_playlist_items_response_version(
                        &response,
                    )?;
                return Ok(response);
            }

            let page = match req.pagination.as_ref() {
                Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(page)) => {
                    page_u32_to_usize(page.page)?
                }
                _ => 1,
            };
            let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
            let search = crate::impls::client::media::normalize_non_empty_filter(&req.search);
            let pagination = if playlist.source_provider
                == Some(synctv_core::models::SourceProvider::Cloudreve)
                && search.is_none()
            {
                match req.pagination.as_ref() {
                    Some(synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        pagination,
                    )) => DynamicPagination::Page {
                        page: page_u32_to_usize(pagination.page)?,
                    },
                    Some(
                        synctv_proto::client::list_playlist_items_request::Pagination::Cursor(
                            pagination,
                        ),
                    ) => DynamicPagination::Cursor {
                        cursor: Some(pagination.cursor.clone()).filter(|value| !value.is_empty()),
                    },
                    None => DynamicPagination::Cursor { cursor: None },
                }
            } else {
                DynamicPagination::Page { page }
            };
            let result = self
                .room_service
                .media_service()
                .admin_list_dynamic_playlist_items(
                    rid,
                    *admin_user_id,
                    &playlist_id,
                    target.as_ref(),
                    DynamicListQuery {
                        pagination,
                        page_size,
                        search,
                        refresh: req.refresh,
                    },
                )
                .await
                .map_err(ApiError::from)?;
            let response_pagination = result.pagination.clone();

            let dynamic_items = result
                .items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::{DirectoryItemThumbnail, ItemType};
                    let item_type = match item.item_type {
                        ItemType::Playlist => synctv_proto::client::ItemType::Playlist as i32,
                        ItemType::Media => synctv_proto::client::ItemType::Media as i32,
                    };

                    let thumbnail = match item.thumbnail {
                        Some(DirectoryItemThumbnail::Url(thumbnail)) => Some(thumbnail),
                        Some(DirectoryItemThumbnail::Emby {
                            server_id,
                            credential_owner_id,
                            item_id,
                        }) => {
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
                            let public_credential_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(|error| {
                                    ApiError::Internal(format!(
                                        "Failed to encode credential owner public id: {error}"
                                    ))
                                })?;
                            let thumbnail = crate::emby_thumbnail_urls::emby_thumbnail_url(
                                &server_id,
                                &public_credential_owner_id,
                                &item_id,
                            );
                            Some(
                                crate::emby_thumbnail_urls::sign_emby_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::Fnos {
                            server_id,
                            credential_owner_id,
                            image_path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::fnos_thumbnail_urls::fnos_thumbnail_url(
                                &server_id,
                                &public_owner_id,
                                &image_path,
                                400,
                            );
                            Some(
                                crate::fnos_thumbnail_urls::sign_fnos_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::Qnap {
                            server_id,
                            credential_owner_id,
                            path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::qnap_thumbnail_urls::qnap_thumbnail_url(
                                &server_id,
                                &public_owner_id,
                                &path,
                                320,
                            );
                            Some(
                                crate::qnap_thumbnail_urls::sign_qnap_thumbnail_url(
                                    &thumbnail,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::Nextcloud {
                            server_id,
                            credential_owner_id,
                            file_id,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let preview = crate::nextcloud_preview_urls::nextcloud_preview_url(
                                &server_id,
                                &public_owner_id,
                                file_id,
                                320,
                                320,
                                true,
                            );
                            Some(
                                crate::nextcloud_preview_urls::sign_nextcloud_preview_url(
                                    &preview,
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::Seafile {
                            server_id,
                            credential_owner_id,
                            repository_id,
                            path,
                        }) => {
                            let room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let thumbnail = crate::seafile_thumbnail_urls::seafile_thumbnail_url(
                                &server_id,
                                &owner_id,
                                &repository_id,
                                &path,
                                320,
                            );
                            Some(
                                crate::seafile_thumbnail_urls::sign_seafile_thumbnail_url(
                                    &thumbnail,
                                    &room_id,
                                    &user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::SynologyFile {
                            server_id,
                            credential_owner_id,
                            path,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let image = crate::synology_image_urls::synology_file_image_url(
                                &server_id,
                                &public_owner_id,
                                &path,
                                "medium",
                            );
                            Some(
                                crate::synology_image_urls::sign_synology_image_url(
                                    &image,
                                    crate::synology_image_urls::SynologyImageScope::File {
                                        server_id: &server_id,
                                        credential_owner_id: &public_owner_id,
                                        path: &path,
                                        size: "medium",
                                    },
                                    &public_room_id,
                                    &public_user_id,
                                    self.signing_key.as_ref(),
                                )
                                .map_err(ApiError::Internal)?,
                            )
                        }
                        Some(DirectoryItemThumbnail::SynologyPoster {
                            server_id,
                            credential_owner_id,
                            item_id,
                            media_type,
                            poster_mtime,
                        }) => {
                            let public_room_id = self
                                .public_id_codec
                                .encode_room_id(rid)
                                .map_err(ApiError::Internal)?;
                            let public_user_id = self
                                .public_id_codec
                                .encode_user_id(*admin_user_id)
                                .map_err(ApiError::Internal)?;
                            let public_owner_id = self
                                .public_id_codec
                                .encode_user_id(credential_owner_id)
                                .map_err(ApiError::Internal)?;
                            let image = crate::synology_image_urls::synology_poster_url(
                                &server_id,
                                &public_owner_id,
                                item_id,
                                &media_type,
                                poster_mtime.as_deref(),
                            );
                            Some(
                                crate::synology_image_urls::sign_synology_image_url(
                                    &image,
                                    crate::synology_image_urls::SynologyImageScope::Poster {
                                        server_id: &server_id,
                                        credential_owner_id: &public_owner_id,
                                        item_id,
                                        media_type: &media_type,
                                        poster_mtime: poster_mtime.as_deref(),
                                    },
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
                        size: item.size,
                        thumbnail,
                        modified_at: item.modified_at,
                        description: item.description.unwrap_or_default(),
                        source_config: None,
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
                total: None,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
                current_path,
                version: String::new(),
                pagination: Some(match response_pagination {
                    DynamicPagination::Page { page } => {
                        synctv_proto::client::list_playlist_items_response::Pagination::Page(
                            synctv_proto::client::PagePagination {
                                page: usize_to_u32_api(page, "playlist page")?,
                            },
                        )
                    }
                    DynamicPagination::Cursor { cursor } => {
                        synctv_proto::client::list_playlist_items_response::Pagination::Cursor(
                            synctv_proto::client::CursorPagination {
                                cursor: cursor.unwrap_or_default(),
                            },
                        )
                    }
                }),
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
            pagination: crate::impls::proto_page_params_u32(
                crate::impls::client::media::playlist_items_page(&req)?,
                req.page_size,
                50,
                100,
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: proto_source_provider_filter(req.source_provider)?,
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability: map_resource_availability_filter(req.availability)?,
            sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by)?,
            sort_direction: map_client_sort_direction(req.sort_direction)?,
        };
        let media_query = CoreMediaListQuery {
            pagination: crate::impls::proto_page_params_u32(
                crate::impls::client::media::playlist_items_page(&req)?,
                req.page_size,
                50,
                100,
            ),
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
        let page_size = crate::impls::proto_page_size_u32_usize(req.page_size, 50, 100)?;
        let page = page_u32_to_usize(crate::impls::client::media::playlist_items_page(&req)?)?;
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
            total: Some(usize_to_u64_api(total, "playlist item total")?),
            folder_count: usize_to_u64_api(folder_count, "playlist folder count")?,
            file_count: usize_to_u64_api(file_count, "playlist file count")?,
            dynamic_items: Vec::new(),
            current_path,
            version: String::new(),
            pagination: None,
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
    ) -> Result<synctv_proto::client::Media, ApiError> {
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

        self.media_to_proto_for_admin_with_loaded_cover(&media, true)
            .await
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
        let prepared_outbox_fanout =
            prepare_delete_entries_outbox_fanout(PrepareDeleteEntriesOutboxFanout {
                clock: self.clock.clone(),
                media_fanout: self.media_fanout.clone(),
                playlist_fanout: self.playlist_fanout.clone(),
                playback_fanout: self.playback_fanout.clone(),
                realtime_fanout: self.realtime_fanout.clone(),
                room_id: rid,
                user_id: *admin_user_id,
                username: actor.username().to_string(),
            });
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

        let media_fanout_plan = MoveMediaFanoutPlanner::new(self.room_service.media_service())
            .build(&rid, &service_req)
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
