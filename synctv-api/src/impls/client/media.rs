//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use synctv_core::models::{
    MediaListQuery as CoreMediaListQuery, MediaListSortBy as CoreMediaListSortBy, Playlist,
    PlaylistListQuery as CorePlaylistListQuery, PlaylistListSortBy as CorePlaylistListSortBy,
    SortDirection as CoreSortDirection, UserId,
};
use synctv_core::service::media::AddMediaRequest as CoreAddMediaRequest;
use synctv_core::service::media::MoveMediaRequest as CoreMoveMediaRequest;
use synctv_core::service::room::DeleteEntriesRequest as CoreDeleteEntriesRequest;

use super::convert::{
    media_to_proto, media_to_proto_with_availability, playlist_path_node_to_proto,
    playlist_to_proto_with_availability,
};
use super::ClientApiImpl;

#[derive(Debug)]
struct AddMediaBatchBuildResult {
    items: Vec<synctv_core::service::media::AddMediaRequest>,
    playlist_id: Option<synctv_core::models::PlaylistId>,
}
const DEFAULT_MEDIA_TITLE: &str = "Unknown";

pub(crate) fn resolve_add_media_provider_instance(
    provider_instance_name: String,
) -> Result<String, ApiError> {
    let trimmed = provider_instance_name.trim();
    Ok(trimmed.to_string())
}

fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn map_sort_direction(sort_direction: i32) -> CoreSortDirection {
    match crate::proto::client::SortDirection::try_from(sort_direction) {
        Ok(crate::proto::client::SortDirection::Desc) => CoreSortDirection::Desc,
        _ => CoreSortDirection::Asc,
    }
}

fn map_playlist_sort_from_media_sort(sort_by: i32) -> CorePlaylistListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CorePlaylistListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CorePlaylistListSortBy::CreatedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CorePlaylistListSortBy::UpdatedAt,
        _ => CorePlaylistListSortBy::Position,
    }
}

fn map_media_sort(sort_by: i32) -> CoreMediaListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CoreMediaListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CoreMediaListSortBy::AddedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CoreMediaListSortBy::UpdatedAt,
        crate::proto::client::MediaListSortBy::SourceProvider => {
            CoreMediaListSortBy::SourceProvider
        }
        crate::proto::client::MediaListSortBy::ProviderInstanceName => {
            CoreMediaListSortBy::ProviderInstanceName
        }
        _ => CoreMediaListSortBy::Position,
    }
}

fn map_availability_filter(filter: i32) -> Option<bool> {
    match crate::proto::client::ResourceAvailabilityFilter::try_from(filter)
        .unwrap_or(crate::proto::client::ResourceAvailabilityFilter::All)
    {
        crate::proto::client::ResourceAvailabilityFilter::All => None,
        crate::proto::client::ResourceAvailabilityFilter::Available => Some(true),
        crate::proto::client::ResourceAvailabilityFilter::Unavailable => Some(false),
    }
}

pub(crate) fn validate_dynamic_playlist_query_support(
    playlist: &Playlist,
    req: &crate::proto::client::ListPlaylistItemsRequest,
) -> Result<bool, ApiError> {
    if let Some(source_provider) = normalize_non_empty_filter(&req.source_provider) {
        if playlist.source_provider.as_deref() != Some(source_provider.as_str()) {
            return Ok(false);
        }
    }
    if let Some(provider_instance_name) = normalize_non_empty_filter(&req.provider_instance_name) {
        if playlist.provider_instance_name.as_deref() != Some(provider_instance_name.as_str()) {
            return Ok(false);
        }
    }

    if normalize_non_empty_filter(&req.search).is_some() {
        return Err(ApiError::InvalidInput(
            "dynamic playlist browsing does not support search yet".to_string(),
        ));
    }

    let sort_by = crate::proto::client::MediaListSortBy::try_from(req.sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position);
    let sort_direction = crate::proto::client::SortDirection::try_from(req.sort_direction)
        .unwrap_or(crate::proto::client::SortDirection::Asc);
    let allows_default_sort = matches!(
        sort_by,
        crate::proto::client::MediaListSortBy::Position
            | crate::proto::client::MediaListSortBy::Unspecified
    ) && matches!(
        sort_direction,
        crate::proto::client::SortDirection::Asc | crate::proto::client::SortDirection::Unspecified
    );
    if !allows_default_sort {
        return Err(ApiError::InvalidInput(
            "dynamic playlist browsing does not support custom sorting yet".to_string(),
        ));
    }

    Ok(true)
}

pub(crate) fn build_move_media_request(
    req: crate::proto::client::MoveMediaRequest,
) -> Result<CoreMoveMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::MoveMediaRequest {
        media_ids,
        source_playlist_id,
        target_playlist_id,
        all_from_scope,
        before_media_id,
        after_media_id,
    } = req;

    Ok(CoreMoveMediaRequest {
        media_ids: crate::impls::proto_validated_media_ids(media_ids),
        source_playlist_id: source_playlist_id.map(crate::impls::proto_validated_playlist_id),
        target_playlist_id: target_playlist_id.map(crate::impls::proto_validated_playlist_id),
        all_from_scope,
        before_media_id: before_media_id.map(crate::impls::proto_validated_media_id),
        after_media_id: after_media_id.map(crate::impls::proto_validated_media_id),
    })
}

pub(crate) fn build_add_media_request(
    req: crate::proto::client::AddMediaRequest,
) -> Result<CoreAddMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::AddMediaRequest {
        playlist_id,
        provider,
        provider_instance_name,
        source_config,
        title,
    } = req;

    let playlist_id = playlist_id.map(crate::impls::proto_validated_playlist_id);

    let source_config: serde_json::Value = if source_config.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&source_config)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?
    };

    let title = if title.is_empty() {
        DEFAULT_MEDIA_TITLE.to_string()
    } else {
        crate::http::validation::validate_media_title(&title)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?
    };

    let provider_instance_name = resolve_add_media_provider_instance(provider_instance_name)?;

    Ok(CoreAddMediaRequest {
        playlist_id,
        name: title,
        source_provider: provider,
        provider_instance_name,
        source_config,
    })
}

pub(crate) fn build_delete_entries_request(
    req: crate::proto::client::DeleteEntriesRequest,
) -> Result<(CoreDeleteEntriesRequest, Vec<String>), ApiError> {
    crate::impls::validate_proto_request(&req)?;
    let crate::proto::client::DeleteEntriesRequest {
        playlist_ids,
        media_ids,
        force,
    } = req;
    let media_id_strings = media_ids.clone();
    Ok((
        CoreDeleteEntriesRequest {
            playlist_ids: crate::impls::proto_validated_playlist_ids(playlist_ids),
            media_ids: crate::impls::proto_validated_media_ids(media_ids),
            force,
        },
        media_id_strings,
    ))
}

fn build_add_media_batch_request(
    req: crate::proto::client::AddMediaBatchRequest,
) -> Result<AddMediaBatchBuildResult, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    if req.items.is_empty() {
        return Err(ApiError::InvalidInput(
            "items array cannot be empty".to_string(),
        ));
    }

    let mut playlist_targets = std::collections::HashSet::new();
    let mut items = Vec::with_capacity(req.items.len());

    for item in req.items {
        playlist_targets.insert(item.playlist_id.clone());
        items.push(build_add_media_request(item)?);
    }

    if playlist_targets.len() != 1 {
        return Err(ApiError::InvalidInput(
            "Batch add must target exactly one location".to_string(),
        ));
    }

    let playlist_id = playlist_targets
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::InvalidInput("Batch add must target one location".to_string()))?
        .map(crate::impls::proto_validated_playlist_id);

    Ok(AddMediaBatchBuildResult { items, playlist_id })
}

pub(crate) fn build_delete_media_request(
    req: crate::proto::client::DeleteMediaRequest,
) -> Result<crate::proto::client::DeleteEntriesRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    Ok(crate::proto::client::DeleteEntriesRequest {
        playlist_ids: Vec::new(),
        media_ids: vec![req.media_id],
        force: req.force,
    })
}

pub(crate) fn build_edit_media_request(
    req: crate::proto::client::EditMediaRequest,
) -> Result<synctv_core::service::media::EditMediaRequest, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    let title = if req.title.is_empty() {
        None
    } else {
        Some(
            crate::http::validation::validate_media_title(&req.title)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?,
        )
    };

    Ok(synctv_core::service::media::EditMediaRequest {
        media_id: crate::impls::proto_validated_media_id(req.media_id),
        name: title,
    })
}

impl ClientApiImpl {
    pub async fn add_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_add_media_request(req)?;
        let playlist_id = service_req.playlist_id.clone();

        // Check total playlist size limit before adding
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            self.room_service
                .media_service()
                .count_playlist_media(playlist_id)
                .await
                .map_err(ApiError::from)? as usize
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)? as usize
        };
        if existing_count >= Self::MAX_PLAYLIST_SIZE {
            return Err(ApiError::InvalidInput(format!(
                "Playlist has reached maximum size of {} items",
                Self::MAX_PLAYLIST_SIZE
            )));
        }

        let media = self
            .room_service
            .media_service()
            .add_media(rid.clone(), uid.clone(), service_req)
            .await
            .map_err(ApiError::from)?;

        // Broadcast MediaAdded cluster event for cross-replica propagation
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            let _ = crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::MediaAdded {
                        event_id: synctv_common::snanoid!(16),
                        room_id: rid,
                        user_id: uid,
                        username,
                        media_id: media.id.clone(),
                        media_title: media.name.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            )
            .await;
        }

        Ok(crate::proto::client::AddMediaResponse {
            media: Some(media_to_proto(&media)),
        })
    }

    pub async fn delete_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::DeleteMediaRequest,
    ) -> Result<crate::proto::client::DeleteMediaResponse, ApiError> {
        self.delete_entries(user_id, room_id, build_delete_media_request(req)?)
            .await?;

        Ok(crate::proto::client::DeleteMediaResponse { success: true })
    }

    /// Delete a mixed set of playlist and media entries.
    pub async fn delete_entries(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::DeleteEntriesRequest,
    ) -> Result<crate::proto::client::DeleteEntriesResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let (service_req, media_id_strings) = build_delete_entries_request(req)?;
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let result = self
            .room_service
            .delete_entries(rid.clone(), uid.clone(), service_req)
            .await
            .map_err(ApiError::from)?;

        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();

            for media_id in &media_id_strings {
                let _ = crate::impls::try_publish_cluster_event(
                    tx,
                    synctv_cluster::sync::PublishRequest {
                        event: synctv_cluster::sync::ClusterEvent::MediaRemoved {
                            event_id: synctv_common::snanoid!(16),
                            room_id: rid.clone(),
                            user_id: uid.clone(),
                            username: username.clone(),
                            media_id: crate::impls::proto_validated_media_id(media_id.clone()),
                            timestamp: chrono::Utc::now(),
                        },
                    },
                )
                .await;
            }
        }

        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        for media_id in &media_id_strings {
            self.kick_stream_cluster(room_id, media_id, "media_deleted")
                .await;
        }

        Ok(crate::proto::client::DeleteEntriesResponse {
            deleted_playlists: result.deleted_playlists as i32,
            deleted_media: result.deleted_media as i32,
        })
    }

    /// Edit media metadata
    pub async fn edit_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::EditMediaRequest,
    ) -> Result<crate::proto::client::EditMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_edit_media_request(req)?;
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let media = self
            .room_service
            .edit_media(
                rid.clone(),
                uid.clone(),
                service_req.media_id,
                service_req.name,
            )
            .await
            .map_err(ApiError::from)?;

        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            let _ = crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::MediaUpdated {
                        event_id: synctv_common::snanoid!(16),
                        room_id: rid.clone(),
                        user_id: uid.clone(),
                        username,
                        media_id: media.id.clone(),
                        media_title: media.name.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            )
            .await;
        }

        // Invalidate room cache on other replicas so they see updated metadata
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::EditMediaResponse {
            media: Some(media_to_proto(&media)),
        })
    }

    /// Clear all media directly under the room root
    pub async fn clear_playlist(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::ClearPlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        // Check permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::CLEAR_PLAYLIST,
            )
            .await
            .map_err(ApiError::from)?;

        let result = self
            .room_service
            .clear_playlist(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Broadcast a single MediaRemovedBatch event instead of N individual events.
        // This reduces Redis pub/sub traffic from O(n) to O(1) messages.
        if !result.deleted_media_ids.is_empty() {
            if let Some(ref tx) = self.redis_publish_tx {
                let username = self
                    .user_service
                    .get_user(&uid)
                    .await
                    .map(|u| u.username)
                    .unwrap_or_default();
                let _ = crate::impls::try_publish_cluster_event(
                    tx,
                    synctv_cluster::sync::PublishRequest {
                        event: synctv_cluster::sync::ClusterEvent::MediaRemovedBatch {
                            event_id: synctv_common::snanoid!(16),
                            room_id: rid.clone(),
                            user_id: uid.clone(),
                            username,
                            media_ids: result.deleted_media_ids.clone(),
                            timestamp: chrono::Utc::now(),
                        },
                    },
                )
                .await;
            }
        }

        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::ClearPlaylistResponse {
            success: true,
            deleted_count: result.deleted_count as i32,
        })
    }

    /// Maximum total media items allowed in a single room's playlist.
    ///
    /// Prevents unbounded playlist growth which could degrade database
    /// performance and client rendering.
    pub const MAX_PLAYLIST_SIZE: usize = 1000;

    /// Add multiple media items in a batch (atomic - all succeed or all fail)
    pub async fn add_media_batch(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaBatchRequest,
    ) -> Result<crate::proto::client::AddMediaBatchResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let AddMediaBatchBuildResult { items, playlist_id } = build_add_media_batch_request(req)?;
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            self.room_service
                .media_service()
                .count_playlist_media(playlist_id)
                .await
                .map_err(ApiError::from)? as usize
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)? as usize
        };
        let new_total = existing_count + items.len();
        if new_total > Self::MAX_PLAYLIST_SIZE {
            let target = if playlist_id.is_some() {
                "playlist"
            } else {
                "room root"
            };
            return Err(ApiError::InvalidInput(format!(
                "{} would exceed maximum size of {} items \
                 (current: {}, adding: {})",
                target,
                Self::MAX_PLAYLIST_SIZE,
                existing_count,
                items.len()
            )));
        }

        let media_list = self
            .room_service
            .media_service()
            .add_media_batch(rid.clone(), uid.clone(), playlist_id, items)
            .await
            .map_err(ApiError::from)?;

        // Broadcast MediaAdded for each item in the batch
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            for media in &media_list {
                let _ = crate::impls::try_publish_cluster_event(
                    tx,
                    synctv_cluster::sync::PublishRequest {
                        event: synctv_cluster::sync::ClusterEvent::MediaAdded {
                            event_id: synctv_common::snanoid!(16),
                            room_id: rid.clone(),
                            user_id: uid.clone(),
                            username: username.clone(),
                            media_id: media.id.clone(),
                            media_title: media.name.clone(),
                            timestamp: chrono::Utc::now(),
                        },
                    },
                )
                .await;
            }
        }

        let results = media_list
            .into_iter()
            .map(|media| crate::proto::client::AddMediaResponse {
                media: Some(media_to_proto(&media)),
            })
            .collect();

        Ok(crate::proto::client::AddMediaBatchResponse { results })
    }

    pub async fn move_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::MoveMediaRequest,
    ) -> Result<crate::proto::client::MoveMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let service_req = build_move_media_request(req)?;

        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::REORDER_PLAYLIST,
            )
            .await
            .map_err(Self::map_room_access_error)?;
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let media = self
            .room_service
            .media_service()
            .move_media(rid.clone(), uid, service_req)
            .await
            .map_err(ApiError::from)?;

        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::MoveMediaResponse {
            moved_count: media.len() as i32,
            media: media.iter().map(media_to_proto).collect(),
        })
    }

    /// List playlist items (supports both static and dynamic playlists)
    ///
    /// Empty `playlist_id` means the room root.
    ///
    /// For room root and static playlists: returns child playlists + media from database
    /// For dynamic playlists: returns remote provider items
    pub async fn list_playlist_items(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let Some(playlist_id) = (!req.playlist_id.is_empty())
            .then(|| crate::impls::proto_validated_playlist_id(req.playlist_id.clone()))
        else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing the room root".to_string(),
                ));
            }
            let playlist_query = CorePlaylistListQuery {
                pagination: synctv_core::models::PageParams::new(
                    Some(req.page.max(1) as u32),
                    Some(req.page_size.clamp(1, 100) as u32),
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability: map_availability_filter(req.availability),
                sort_by: map_playlist_sort_from_media_sort(req.sort_by),
                sort_direction: map_sort_direction(req.sort_direction),
            };
            let media_query = CoreMediaListQuery {
                pagination: synctv_core::models::PageParams::new(
                    Some(req.page.max(1) as u32),
                    Some(req.page_size.clamp(1, 100) as u32),
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                availability: map_availability_filter(req.availability),
                sort_by: map_media_sort(req.sort_by),
                sort_direction: map_sort_direction(req.sort_direction),
            };
            let folder_count = self
                .room_service
                .count_client_playlists(&rid, None, &playlist_query)
                .await
                .map_err(ApiError::from)? as usize;
            let file_count = self
                .room_service
                .count_client_media(&rid, None, &media_query)
                .await
                .map_err(ApiError::from)? as usize;
            let total = folder_count + file_count;
            let page_size = req.page_size.clamp(1, 100) as usize;
            let skip = (req.page.max(1) as usize - 1) * page_size;
            let (playlists, media) = if skip < folder_count {
                let playlists = self
                    .room_service
                    .list_client_playlists(
                        &rid,
                        None,
                        &playlist_query,
                        page_size as i64,
                        skip as i64,
                    )
                    .await
                    .map_err(ApiError::from)?;
                let remaining = page_size.saturating_sub(playlists.len());
                let media = if remaining > 0 {
                    self.room_service
                        .list_client_media(&rid, None, &media_query, remaining as i64, 0)
                        .await
                        .map_err(ApiError::from)?
                } else {
                    Vec::new()
                };
                (playlists, media)
            } else {
                let media_skip = skip - folder_count;
                let media = self
                    .room_service
                    .list_client_media(
                        &rid,
                        None,
                        &media_query,
                        page_size as i64,
                        media_skip as i64,
                    )
                    .await
                    .map_err(ApiError::from)?;
                (Vec::new(), media)
            };
            let folder_ids: Vec<&str> =
                playlists.iter().map(|pl| pl.playlist.id.as_str()).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&folder_ids)
                .await
                .unwrap_or_default();
            let proto_playlists = playlists
                .iter()
                .map(|entry| {
                    let item_count =
                        counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0) as i32;
                    playlist_to_proto_with_availability(
                        &entry.playlist,
                        item_count,
                        entry.is_available,
                    )
                })
                .collect();
            let proto_media = media
                .iter()
                .map(|entry| media_to_proto_with_availability(&entry.media, entry.is_available))
                .collect();

            return Ok(crate::proto::client::ListPlaylistItemsResponse {
                playlists: proto_playlists,
                media: proto_media,
                total: total as i32,
                folder_count: folder_count as i32,
                file_count: file_count as i32,
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
            });
        };

        // Get playlist info to determine if static or dynamic
        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(&playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {} not found", req.playlist_id)))?;
        let static_path = self
            .room_service
            .playlist_service()
            .get_playlist_path(&playlist_id)
            .await
            .map_err(ApiError::from)?;
        let mut current_path: Vec<crate::proto::client::PlaylistBrowsePathNode> = static_path
            .iter()
            .map(playlist_path_node_to_proto)
            .collect();

        if playlist.is_dynamic() {
            self.room_service
                .ensure_client_usable_playlist(&playlist)
                .await
                .map_err(ApiError::from)?;
            if !validate_dynamic_playlist_query_support(&playlist, &req)? {
                return Ok(crate::proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total: 0,
                    folder_count: 0,
                    file_count: 0,
                    dynamic_items: Vec::new(),
                    current_path,
                });
            }

            let page = req.page.max(1) as usize;
            let page_size = req.page_size.clamp(1, 100) as usize;
            let items = self
                .room_service
                .media_service()
                .list_dynamic_playlist_items(
                    rid.clone(),
                    uid.clone(),
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                    page,
                    page_size,
                )
                .await
                .map_err(ApiError::from)?;

            // Convert provider DirectoryItem to proto PlaylistItem
            let dynamic_items: Vec<_> = items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::ItemType;
                    let item_type = match item.item_type {
                        ItemType::Playlist => crate::proto::client::ItemType::Playlist as i32,
                        ItemType::Media => crate::proto::client::ItemType::Media as i32,
                    };

                    Ok(crate::proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        target: item.target,
                        size: item.size.map(|s| s as i64),
                        thumbnail: Some(item.thumbnail.unwrap_or_default()),
                        modified_at: Some(item.modified_at.unwrap_or(0)),
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;

            let browse_path = self
                .room_service
                .media_service()
                .get_dynamic_playlist_browse_path(
                    rid,
                    uid,
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                )
                .await
                .map_err(ApiError::from)?;
            current_path.extend(browse_path.into_iter().map(|segment| {
                crate::proto::client::PlaylistBrowsePathNode {
                    playlist_id: String::new(),
                    name: segment.name,
                    target: segment.target,
                }
            }));

            // Dynamic playlists don't provide a reliable total count since the
            // provider may paginate server-side.  Use -1 to signal "unknown total"
            // so the client knows to use has_more / next-page semantics.
            let total: i32 = -1;

            return Ok(crate::proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
                current_path,
            });
        }

        if !req.target.is_empty() {
            return Err(ApiError::InvalidInput(
                "target must be empty when browsing a static playlist".to_string(),
            ));
        }

        let playlist_query = CorePlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(req.page.max(1) as u32),
                Some(req.page_size.clamp(1, 100) as u32),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability: map_availability_filter(req.availability),
            sort_by: map_playlist_sort_from_media_sort(req.sort_by),
            sort_direction: map_sort_direction(req.sort_direction),
        };
        let media_query = CoreMediaListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(req.page.max(1) as u32),
                Some(req.page_size.clamp(1, 100) as u32),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            availability: map_availability_filter(req.availability),
            sort_by: map_media_sort(req.sort_by),
            sort_direction: map_sort_direction(req.sort_direction),
        };
        let folder_count = self
            .room_service
            .count_client_playlists(&rid, Some(&playlist_id), &playlist_query)
            .await
            .map_err(ApiError::from)? as usize;
        let file_count = self
            .room_service
            .count_client_media(&rid, Some(&playlist_id), &media_query)
            .await
            .map_err(ApiError::from)? as usize;
        let total = folder_count + file_count;
        let page_size = req.page_size.clamp(1, 100) as usize;
        let skip = (req.page.max(1) as usize - 1) * page_size;
        let (playlists, media) = if skip < folder_count {
            let playlists = self
                .room_service
                .list_client_playlists(
                    &rid,
                    Some(&playlist_id),
                    &playlist_query,
                    page_size as i64,
                    skip as i64,
                )
                .await
                .map_err(ApiError::from)?;
            let remaining = page_size.saturating_sub(playlists.len());
            let media = if remaining > 0 {
                self.room_service
                    .list_client_media(&rid, Some(&playlist_id), &media_query, remaining as i64, 0)
                    .await
                    .map_err(ApiError::from)?
            } else {
                Vec::new()
            };
            (playlists, media)
        } else {
            let media_skip = skip - folder_count;
            let media = self
                .room_service
                .list_client_media(
                    &rid,
                    Some(&playlist_id),
                    &media_query,
                    page_size as i64,
                    media_skip as i64,
                )
                .await
                .map_err(ApiError::from)?;
            (Vec::new(), media)
        };
        let folder_ids: Vec<&str> = playlists.iter().map(|pl| pl.playlist.id.as_str()).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&folder_ids)
            .await
            .unwrap_or_default();
        let proto_playlists = playlists
            .iter()
            .map(|entry| {
                let item_count =
                    counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0) as i32;
                playlist_to_proto_with_availability(&entry.playlist, item_count, entry.is_available)
            })
            .collect();
        let proto_media = media
            .iter()
            .map(|entry| media_to_proto_with_availability(&entry.media, entry.is_available))
            .collect();

        Ok(crate::proto::client::ListPlaylistItemsResponse {
            playlists: proto_playlists,
            media: proto_media,
            total: total as i32,
            folder_count: folder_count as i32,
            file_count: file_count as i32,
            dynamic_items: Vec::new(),
            current_path,
        })
    }

    /// Get a single media record from database
    pub async fn get_media(
        &self,
        user_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let mid = crate::impls::parse_media_id_param(media_id, "media_id")?;

        // Check VIEW_PLAYLIST permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::VIEW_PLAYLIST,
            )
            .await
            .map_err(ApiError::from)?;

        // M-5: Direct lookup by ID instead of loading the entire playlist
        let media = self
            .room_service
            .media_service()
            .get_media(&mid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Media {media_id} not found")))?;
        let availability = self
            .room_service
            .media_availability(&media)
            .await
            .map_err(ApiError::from)?;

        Ok(media_to_proto_with_availability(
            &media,
            availability.is_available(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_add_media_batch_request, build_add_media_request, build_delete_entries_request,
        build_delete_media_request, build_edit_media_request, build_move_media_request,
        resolve_add_media_provider_instance, validate_dynamic_playlist_query_support,
        DEFAULT_MEDIA_TITLE,
    };
    use chrono::Utc;
    use serde_json::json;
    use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};

    fn make_playlist(
        name: &str,
        source_provider: Option<&str>,
        provider_instance_name: Option<&str>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: name.to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: source_provider.map(str::to_string),
            source_config: Some(json!({})),
            provider_instance_name: provider_instance_name.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_resolve_add_media_provider_instance_preserves_empty_binding_for_default_provider() {
        let resolved = resolve_add_media_provider_instance(String::new()).unwrap();
        assert_eq!(resolved, "");
    }

    #[test]
    fn test_resolve_add_media_provider_instance_uses_explicit_binding() {
        let resolved = resolve_add_media_provider_instance("alist_main".to_string()).unwrap();
        assert_eq!(resolved, "alist_main");
    }

    #[test]
    fn test_build_add_media_request_requires_source_provider() {
        let err = build_add_media_request(crate::proto::client::AddMediaRequest {
            playlist_id: None,
            provider: String::new(),
            provider_instance_name: String::new(),
            source_config: br#"{"path":"/tv"}"#.to_vec(),
            title: String::new(),
        })
        .unwrap_err();

        assert!(err.to_string().contains("provider"));
    }

    #[test]
    fn test_build_add_media_request_parses_dynamic_payload() {
        let playlist_id = synctv_common::snanoid!(12);
        let request = build_add_media_request(crate::proto::client::AddMediaRequest {
            playlist_id: Some(playlist_id.clone()),
            provider: "alist".into(),
            provider_instance_name: "alist-main".into(),
            source_config: br#"{"path":"/tv"}"#.to_vec(),
            title: "Episode 1".into(),
        })
        .unwrap();

        assert_eq!(
            request.playlist_id.as_ref().map(|id| id.as_str()),
            Some(playlist_id.as_str())
        );
        assert_eq!(request.name, "Episode 1");
        assert_eq!(request.source_provider, "alist");
        assert_eq!(request.provider_instance_name, "alist-main");
        assert_eq!(request.source_config, serde_json::json!({"path":"/tv"}));
    }

    #[test]
    fn test_build_add_media_request_preserves_empty_provider_instance_for_default_resolution() {
        let request = build_add_media_request(crate::proto::client::AddMediaRequest {
            playlist_id: None,
            provider: "direct_url".into(),
            provider_instance_name: String::new(),
            source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
            title: "Example".into(),
        })
        .unwrap();

        assert_eq!(request.source_provider, "direct_url");
        assert!(request.provider_instance_name.is_empty());
    }

    #[test]
    fn test_build_add_media_request_does_not_infer_title_from_source_config() {
        let request = build_add_media_request(crate::proto::client::AddMediaRequest {
            playlist_id: None,
            provider: "alist".into(),
            provider_instance_name: "alist-main".into(),
            source_config: br#"{"url":"https://example.com/video.mp4","path":"/tv"}"#.to_vec(),
            title: String::new(),
        })
        .unwrap();

        assert_eq!(request.name, DEFAULT_MEDIA_TITLE);
    }

    #[test]
    fn test_build_add_media_batch_request_rejects_invalid_nested_playlist_id() {
        let err = build_add_media_batch_request(crate::proto::client::AddMediaBatchRequest {
            items: vec![crate::proto::client::AddMediaRequest {
                playlist_id: Some("bad-playlist".into()),
                provider: "alist".into(),
                provider_instance_name: "alist-main".into(),
                source_config: br#"{"path":"/tv"}"#.to_vec(),
                title: "Episode 1".into(),
            }],
        })
        .unwrap_err();

        assert!(err.to_string().contains("playlist_id"));
    }

    #[test]
    fn test_build_add_media_batch_request_reuses_single_item_builder_semantics() {
        let playlist_id = synctv_common::snanoid!(12);
        let result = build_add_media_batch_request(crate::proto::client::AddMediaBatchRequest {
            items: vec![crate::proto::client::AddMediaRequest {
                playlist_id: Some(playlist_id.clone()),
                provider: "alist".into(),
                provider_instance_name: "alist-main".into(),
                source_config: br#"{"url":"https://example.com/video.mp4","path":"/tv"}"#.to_vec(),
                title: String::new(),
            }],
        })
        .unwrap();

        assert_eq!(
            result.playlist_id.as_ref().map(|id| id.as_str()),
            Some(playlist_id.as_str())
        );
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, DEFAULT_MEDIA_TITLE);
        assert_eq!(
            result.items[0].source_config,
            serde_json::json!({"url":"https://example.com/video.mp4","path":"/tv"})
        );
    }

    #[test]
    fn test_build_delete_entries_request_rejects_empty_target_set() {
        let err = build_delete_entries_request(crate::proto::client::DeleteEntriesRequest {
            playlist_ids: Vec::new(),
            media_ids: Vec::new(),
            force: false,
        })
        .unwrap_err();

        assert!(err.to_string().contains("delete_entries"));
    }

    #[test]
    fn test_build_delete_entries_request_parses_ids() {
        let playlist_id = synctv_common::snanoid!(12);
        let media_id = synctv_common::snanoid!(12);
        let (request, media_id_strings) =
            build_delete_entries_request(crate::proto::client::DeleteEntriesRequest {
                playlist_ids: vec![playlist_id.clone()],
                media_ids: vec![media_id.clone()],
                force: true,
            })
            .unwrap();

        assert_eq!(request.playlist_ids.len(), 1);
        assert_eq!(request.playlist_ids[0].as_str(), playlist_id);
        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0].as_str(), media_id);
        assert!(request.force);
        assert_eq!(media_id_strings, vec![media_id]);
    }

    #[test]
    fn test_build_delete_media_request_rejects_invalid_media_id() {
        let err = build_delete_media_request(crate::proto::client::DeleteMediaRequest {
            media_id: "bad-media".to_string(),
            force: false,
        })
        .unwrap_err();

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_delete_media_request_maps_to_delete_entries_request() {
        let media_id = synctv_common::snanoid!(12);
        let request = build_delete_media_request(crate::proto::client::DeleteMediaRequest {
            media_id: media_id.clone(),
            force: true,
        })
        .unwrap();

        assert!(request.playlist_ids.is_empty());
        assert_eq!(request.media_ids, vec![media_id]);
        assert!(request.force);
    }

    #[test]
    fn test_build_edit_media_request_rejects_invalid_media_id() {
        let err = build_edit_media_request(crate::proto::client::EditMediaRequest {
            media_id: "bad-media".to_string(),
            title: "Episode 1".to_string(),
        })
        .unwrap_err();

        assert!(err.to_string().contains("media_id"));
    }

    #[test]
    fn test_build_edit_media_request_parses_title_and_id() {
        let media_id = synctv_common::snanoid!(12);
        let request = build_edit_media_request(crate::proto::client::EditMediaRequest {
            media_id: media_id.clone(),
            title: "Episode 1".to_string(),
        })
        .unwrap();

        assert_eq!(request.media_id.as_str(), media_id);
        assert_eq!(request.name.as_deref(), Some("Episode 1"));
    }

    #[test]
    fn test_validate_dynamic_playlist_query_support_rejects_search() {
        let playlist = make_playlist("Dynamic Folder", Some("alist"), Some("alist-main"));
        let err = validate_dynamic_playlist_query_support(
            &playlist,
            &crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist.id.as_str().to_string(),
                target: Vec::new(),
                page: 1,
                page_size: 20,
                search: "alpha".to_string(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
                availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("does not support search"));
    }

    #[test]
    fn test_build_move_media_request_rejects_invalid_proto_payload() {
        let err = build_move_media_request(crate::proto::client::MoveMediaRequest {
            media_ids: Vec::new(),
            source_playlist_id: Some("playlist-1".into()),
            target_playlist_id: None,
            all_from_scope: false,
            before_media_id: Some("media-before".into()),
            after_media_id: Some("media-after".into()),
        })
        .unwrap_err();

        assert!(
            err.to_string().contains("source_playlist_id")
                || err.to_string().contains("before_media_id")
        );
    }

    #[test]
    fn test_build_move_media_request_parses_ids() {
        let media_id = synctv_common::snanoid!(12);
        let playlist_id = synctv_common::snanoid!(12);
        let before_media_id = synctv_common::snanoid!(12);
        let request = build_move_media_request(crate::proto::client::MoveMediaRequest {
            media_ids: vec![media_id.clone()],
            source_playlist_id: None,
            target_playlist_id: Some(playlist_id.clone()),
            all_from_scope: false,
            before_media_id: None,
            after_media_id: Some(before_media_id.clone()),
        })
        .unwrap();

        assert_eq!(request.media_ids.len(), 1);
        assert_eq!(request.media_ids[0].as_str(), media_id);
        assert_eq!(
            request.target_playlist_id.as_ref().map(|id| id.as_str()),
            Some(playlist_id.as_str())
        );
        assert_eq!(
            request.after_media_id.as_ref().map(|id| id.as_str()),
            Some(before_media_id.as_str())
        );
    }
}
