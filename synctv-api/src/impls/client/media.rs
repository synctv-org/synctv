//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use std::cmp::Ordering;
use std::str::FromStr;
use synctv_core::models::{
    Media, Playlist, ProviderType, SortDirection as CoreSortDirection, UserId,
};

use super::convert::{media_to_proto, playlist_path_node_to_proto, playlist_to_proto};
use super::ClientApiImpl;

/// Sanitize and truncate a title extracted from a URL path segment.
///
/// Unlike user-provided titles (which are validated and rejected if invalid),
/// URL-derived titles are best-effort: we sanitize control characters and
/// truncate to [`MEDIA_TITLE_MAX`] to ensure they fit in the database.
pub(crate) fn sanitize_url_derived_title(raw: &str) -> String {
    use crate::http::validation::limits::MEDIA_TITLE_MAX;

    // URL-decode percent-encoded characters (e.g., "my%20video.mp4" -> "my video.mp4")
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .unwrap_or(std::borrow::Cow::Borrowed(raw));
    // Sanitize control characters and trim whitespace
    let sanitized = crate::http::validation::sanitize_string(&decoded);

    // Truncate to max allowed character count
    if sanitized.chars().count() > MEDIA_TITLE_MAX {
        sanitized.chars().take(MEDIA_TITLE_MAX).collect()
    } else {
        sanitized.into_owned()
    }
}

pub(crate) fn resolve_add_media_provider_instance(
    provider: ProviderType,
    provider_name: &str,
    provider_instance_name: String,
) -> Result<String, ApiError> {
    if provider == ProviderType::DirectUrl {
        return Ok("direct_url".to_string());
    }

    let trimmed = provider_instance_name.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }

    Err(ApiError::InvalidInput(format!(
        "provider_instance_name is required for provider '{provider_name}'"
    )))
}

pub(crate) fn parse_add_media_provider(provider_name: &str) -> Result<ProviderType, ApiError> {
    if provider_name.is_empty() {
        return Ok(ProviderType::DirectUrl);
    }

    ProviderType::from_str(provider_name)
        .map_err(|_| ApiError::InvalidInput(format!("Unknown provider type: '{provider_name}'")))
}

fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

#[derive(Debug, Clone)]
struct StaticPlaylistItemsQuery {
    page: usize,
    page_size: usize,
    search: Option<String>,
    source_provider: Option<String>,
    provider_instance_name: Option<String>,
    sort_by: crate::proto::client::MediaListSortBy,
    sort_direction: CoreSortDirection,
}

impl StaticPlaylistItemsQuery {
    fn from_request(req: &crate::proto::client::ListPlaylistItemsRequest) -> Self {
        Self {
            page: req.page.max(1) as usize,
            page_size: req.page_size.clamp(1, 100) as usize,
            search: normalize_non_empty_filter(&req.search).map(|value| value.to_ascii_lowercase()),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            sort_by: crate::proto::client::MediaListSortBy::try_from(req.sort_by)
                .unwrap_or(crate::proto::client::MediaListSortBy::Position),
            sort_direction: match crate::proto::client::SortDirection::try_from(req.sort_direction)
            {
                Ok(crate::proto::client::SortDirection::Desc) => CoreSortDirection::Desc,
                _ => CoreSortDirection::Asc,
            },
        }
    }
}

pub(crate) struct StaticPlaylistItemsPage {
    pub playlists: Vec<Playlist>,
    pub media: Vec<Media>,
    pub total: usize,
    pub folder_count: usize,
    pub file_count: usize,
}

fn compare_playlists_for_media_query(
    left: &Playlist,
    right: &Playlist,
    sort_by: crate::proto::client::MediaListSortBy,
    sort_direction: CoreSortDirection,
) -> Ordering {
    let ordering = match sort_by {
        crate::proto::client::MediaListSortBy::Name => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::AddedAt => left
            .created_at
            .cmp(&right.created_at)
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::UpdatedAt => left
            .updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::SourceProvider => left
            .source_provider
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .source_provider
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
        crate::proto::client::MediaListSortBy::ProviderInstanceName => left
            .provider_instance_name
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .provider_instance_name
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
        crate::proto::client::MediaListSortBy::Position
        | crate::proto::client::MediaListSortBy::Unspecified => {
            left.position.cmp(&right.position).then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
        }
    };

    match sort_direction {
        CoreSortDirection::Asc => ordering,
        CoreSortDirection::Desc => ordering.reverse(),
    }
}

fn compare_media_for_query(
    left: &Media,
    right: &Media,
    sort_by: crate::proto::client::MediaListSortBy,
    sort_direction: CoreSortDirection,
) -> Ordering {
    let ordering = match sort_by {
        crate::proto::client::MediaListSortBy::Name => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::AddedAt => left
            .added_at
            .cmp(&right.added_at)
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::UpdatedAt => left
            .updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.position.cmp(&right.position)),
        crate::proto::client::MediaListSortBy::SourceProvider => left
            .source_provider
            .to_ascii_lowercase()
            .cmp(&right.source_provider.to_ascii_lowercase())
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
        crate::proto::client::MediaListSortBy::ProviderInstanceName => left
            .provider_instance_name
            .to_ascii_lowercase()
            .cmp(&right.provider_instance_name.to_ascii_lowercase())
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            }),
        crate::proto::client::MediaListSortBy::Position
        | crate::proto::client::MediaListSortBy::Unspecified => {
            left.position.cmp(&right.position).then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
        }
    };

    match sort_direction {
        CoreSortDirection::Asc => ordering,
        CoreSortDirection::Desc => ordering.reverse(),
    }
}

fn playlist_matches_static_query(playlist: &Playlist, query: &StaticPlaylistItemsQuery) -> bool {
    if let Some(search) = &query.search {
        if !playlist.name.to_ascii_lowercase().contains(search) {
            return false;
        }
    }
    if let Some(source_provider) = query.source_provider.as_deref() {
        if playlist.source_provider.as_deref() != Some(source_provider) {
            return false;
        }
    }
    if let Some(provider_instance_name) = query.provider_instance_name.as_deref() {
        if playlist.provider_instance_name.as_deref() != Some(provider_instance_name) {
            return false;
        }
    }
    true
}

fn media_matches_static_query(media: &Media, query: &StaticPlaylistItemsQuery) -> bool {
    if let Some(search) = &query.search {
        if !media.name.to_ascii_lowercase().contains(search) {
            return false;
        }
    }
    if let Some(source_provider) = query.source_provider.as_deref() {
        if media.source_provider != source_provider {
            return false;
        }
    }
    if let Some(provider_instance_name) = query.provider_instance_name.as_deref() {
        if media.provider_instance_name != provider_instance_name {
            return false;
        }
    }
    true
}

pub(crate) fn build_static_playlist_items_page(
    mut playlists: Vec<Playlist>,
    mut media: Vec<Media>,
    req: &crate::proto::client::ListPlaylistItemsRequest,
) -> StaticPlaylistItemsPage {
    let query = StaticPlaylistItemsQuery::from_request(req);
    playlists.retain(|playlist| playlist_matches_static_query(playlist, &query));
    media.retain(|item| media_matches_static_query(item, &query));

    playlists.sort_by(|left, right| {
        compare_playlists_for_media_query(left, right, query.sort_by, query.sort_direction)
    });
    media.sort_by(|left, right| {
        compare_media_for_query(left, right, query.sort_by, query.sort_direction)
    });

    let folder_count = playlists.len();
    let file_count = media.len();
    let total = folder_count + file_count;
    let skip = (query.page - 1) * query.page_size;
    let (playlists, media) = if skip < folder_count {
        let page_playlists: Vec<Playlist> = playlists
            .into_iter()
            .skip(skip)
            .take(query.page_size)
            .collect();
        let remaining = query.page_size.saturating_sub(page_playlists.len());
        let page_media = if remaining > 0 {
            media.into_iter().take(remaining).collect()
        } else {
            Vec::new()
        };
        (page_playlists, page_media)
    } else {
        let media_skip = skip - folder_count;
        (
            Vec::new(),
            media
                .into_iter()
                .skip(media_skip)
                .take(query.page_size)
                .collect(),
        )
    };

    StaticPlaylistItemsPage {
        playlists,
        media,
        total,
        folder_count,
        file_count,
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

impl ClientApiImpl {
    pub async fn add_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let playlist_id = if let Some(playlist_id) = req.playlist_id.as_ref() {
            crate::http::validation::validate_id(playlist_id, "playlist_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
            Some(synctv_core::models::PlaylistId::from_string(
                playlist_id.clone(),
            ))
        } else {
            None
        };

        let provider = parse_add_media_provider(&req.provider)?;

        // Parse source config from request bytes
        let source_config: serde_json::Value = if req.source_config.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&req.source_config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?
        };

        // Use provided title or extract from URL (with validation/truncation)
        let title = if req.title.is_empty() {
            let raw = source_config
                .get("url")
                .and_then(|u| u.as_str())
                .and_then(|u| u.split('/').next_back())
                .unwrap_or("Unknown");
            sanitize_url_derived_title(raw)
        } else {
            // Validate user-provided title for length and security
            crate::http::validation::validate_media_title(&req.title)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?
        };

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

        // Normalize the request into the concrete provider instance name used by the registry.
        // DirectUrl maps to the built-in "direct_url" instance; other providers must bind
        // explicitly to a configured provider instance.
        let provider_instance_name = resolve_add_media_provider_instance(
            provider,
            &req.provider,
            req.provider_instance_name,
        )?;

        let media = self
            .room_service
            .media_service()
            .add_media(
                rid.clone(),
                uid.clone(),
                synctv_core::service::media::AddMediaRequest {
                    playlist_id,
                    name: title,
                    provider_instance_name,
                    source_config,
                },
            )
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
        self.delete_entries(
            user_id,
            room_id,
            crate::proto::client::DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: vec![req.media_id],
                force: req.force,
            },
        )
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
        let total_targets = req.playlist_ids.len() + req.media_ids.len();
        if total_targets == 0 {
            return Err(ApiError::InvalidInput(
                "delete request cannot be empty".to_string(),
            ));
        }
        if total_targets > 100 {
            return Err(ApiError::InvalidInput(
                "Too many items (max 100 per delete request)".to_string(),
            ));
        }

        for playlist_id in &req.playlist_ids {
            crate::http::validation::validate_id(playlist_id, "playlist_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
        }
        for media_id in &req.media_ids {
            crate::http::validation::validate_id(media_id, "media_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let playlist_ids = req
            .playlist_ids
            .iter()
            .cloned()
            .map(synctv_core::models::PlaylistId::from_string)
            .collect();
        let media_id_strings = req.media_ids.clone();
        let media_ids = req
            .media_ids
            .iter()
            .cloned()
            .map(synctv_core::models::MediaId::from_string)
            .collect();
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let result = self
            .room_service
            .delete_entries(
                rid.clone(),
                uid.clone(),
                synctv_core::service::room::DeleteEntriesRequest {
                    playlist_ids,
                    media_ids,
                    force: req.force,
                },
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

            for media_id in &media_id_strings {
                let _ = crate::impls::try_publish_cluster_event(
                    tx,
                    synctv_cluster::sync::PublishRequest {
                        event: synctv_cluster::sync::ClusterEvent::MediaRemoved {
                            event_id: synctv_common::snanoid!(16),
                            room_id: rid.clone(),
                            user_id: uid.clone(),
                            username: username.clone(),
                            media_id: synctv_core::models::MediaId::from_string(media_id.clone()),
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
        crate::http::validation::validate_id(&req.media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let mid = synctv_core::models::MediaId::from_string(req.media_id);

        let title = if req.title.is_empty() {
            None
        } else {
            // Validate user-provided title for length and security
            let validated = crate::http::validation::validate_media_title(&req.title)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?;
            Some(validated)
        };
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        let media = self
            .room_service
            .edit_media(rid.clone(), uid.clone(), mid, title)
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
        if req.items.is_empty() {
            return Err(ApiError::InvalidInput(
                "items array cannot be empty".to_string(),
            ));
        }
        if req.items.len() > 100 {
            return Err(ApiError::InvalidInput(
                "Too many items (max 100 per batch)".to_string(),
            ));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let mut playlist_targets = std::collections::HashSet::new();

        // Build batch items for the atomic service call
        let mut items: Vec<synctv_core::service::media::AddMediaRequest> =
            Vec::with_capacity(req.items.len());
        for item in &req.items {
            let playlist_id = if let Some(playlist_id) = item.playlist_id.as_ref() {
                crate::http::validation::validate_id(playlist_id, "playlist_id")
                    .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
                Some(synctv_core::models::PlaylistId::from_string(
                    playlist_id.clone(),
                ))
            } else {
                None
            };
            playlist_targets.insert(item.playlist_id.clone());
            let source_config: serde_json::Value = if item.source_config.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&item.source_config).map_err(|e| {
                    ApiError::InvalidInput(format!("Invalid source_config JSON: {e}"))
                })?
            };
            let title = if item.title.is_empty() {
                let raw = source_config
                    .get("url")
                    .and_then(|u| u.as_str())
                    .and_then(|u| u.split('/').next_back())
                    .unwrap_or("Unknown");
                sanitize_url_derived_title(raw)
            } else {
                // Validate user-provided title for length and security
                crate::http::validation::validate_media_title(&item.title)
                    .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?
            };
            let provider = parse_add_media_provider(&item.provider)?;
            let provider_instance_name = resolve_add_media_provider_instance(
                provider,
                &item.provider,
                item.provider_instance_name.clone(),
            )?;
            items.push(synctv_core::service::media::AddMediaRequest {
                playlist_id,
                name: title,
                provider_instance_name,
                source_config,
            });
        }

        if playlist_targets.len() != 1 {
            return Err(ApiError::InvalidInput(
                "Batch add must target exactly one location".to_string(),
            ));
        }

        let playlist_id = playlist_targets
            .into_iter()
            .next()
            .ok_or_else(|| {
                ApiError::InvalidInput("Batch add must target one location".to_string())
            })?
            .map(synctv_core::models::PlaylistId::from_string);
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

    /// Bulk reorder multiple media items
    pub async fn reorder_media_batch(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ReorderMediaBatchRequest,
    ) -> Result<crate::proto::client::ReorderMediaBatchResponse, ApiError> {
        if req.updates.is_empty() {
            return Err(ApiError::InvalidInput(
                "updates array cannot be empty".to_string(),
            ));
        }
        if req.updates.len() > 100 {
            return Err(ApiError::InvalidInput(
                "Too many items (max 100 per batch)".to_string(),
            ));
        }

        for update in &req.updates {
            crate::http::validation::validate_id(&update.media_id, "media_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Defense-in-depth: check REORDER_PLAYLIST permission at the API layer
        // (the service layer also checks, but checking here provides early rejection
        // and consistent error handling with other API methods like clear_playlist)
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::REORDER_PLAYLIST,
            )
            .await
            .map_err(Self::map_room_access_error)?;

        let updates_converted: Vec<(synctv_core::models::MediaId, i32)> = req
            .updates
            .into_iter()
            .map(|u| {
                (
                    synctv_core::models::MediaId::from_string(u.media_id),
                    u.position,
                )
            })
            .collect();
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        self.room_service
            .media_service()
            .reorder_media_batch(rid.clone(), uid, updates_converted)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas so they refresh playlist order
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::ReorderMediaBatchResponse { success: true })
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
        if !req.playlist_id.is_empty() {
            crate::http::validation::validate_id(&req.playlist_id, "playlist_id")
                .map_err(|e| ApiError::InvalidInput(format!("Invalid playlist_id: {e}")))?;
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let Some(playlist_id) = (!req.playlist_id.is_empty())
            .then(|| synctv_core::models::PlaylistId::from_string(req.playlist_id.clone()))
        else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing the room root".to_string(),
                ));
            }

            let playlists = self
                .room_service
                .playlist_service()
                .get_top_level_playlists(&rid)
                .await
                .map_err(ApiError::from)?;
            let media = self
                .room_service
                .media_service()
                .get_room_root_media(&rid)
                .await
                .map_err(ApiError::from)?;
            let page = build_static_playlist_items_page(playlists, media, &req);
            let folder_ids: Vec<&str> = page.playlists.iter().map(|pl| pl.id.as_str()).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&folder_ids)
                .await
                .unwrap_or_default();
            let proto_playlists = page
                .playlists
                .iter()
                .map(|pl| {
                    let item_count = counts.get(pl.id.as_str()).copied().unwrap_or(0) as i32;
                    playlist_to_proto(pl, item_count)
                })
                .collect();
            let proto_media = page.media.iter().map(media_to_proto).collect();

            return Ok(crate::proto::client::ListPlaylistItemsResponse {
                playlists: proto_playlists,
                media: proto_media,
                total: page.total as i32,
                folder_count: page.folder_count as i32,
                file_count: page.file_count as i32,
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

        let playlists = self
            .room_service
            .playlist_service()
            .get_children(&playlist_id)
            .await
            .map_err(ApiError::from)?;
        let media = self
            .room_service
            .media_service()
            .get_playlist_media(&playlist_id)
            .await
            .map_err(ApiError::from)?;
        let page = build_static_playlist_items_page(playlists, media, &req);
        let folder_ids: Vec<&str> = page.playlists.iter().map(|pl| pl.id.as_str()).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&folder_ids)
            .await
            .unwrap_or_default();
        let proto_playlists = page
            .playlists
            .iter()
            .map(|pl| {
                let item_count = counts.get(pl.id.as_str()).copied().unwrap_or(0) as i32;
                playlist_to_proto(pl, item_count)
            })
            .collect();
        let proto_media = page.media.iter().map(media_to_proto).collect();

        Ok(crate::proto::client::ListPlaylistItemsResponse {
            playlists: proto_playlists,
            media: proto_media,
            total: page.total as i32,
            folder_count: page.folder_count as i32,
            file_count: page.file_count as i32,
            dynamic_items: Vec::new(),
            current_path,
        })
    }

    pub async fn swap_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SwapMediaRequest,
    ) -> Result<crate::proto::client::SwapMediaResponse, ApiError> {
        crate::http::validation::validate_id(&req.media_id1, "media_id1")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id1: {e}")))?;
        crate::http::validation::validate_id(&req.media_id2, "media_id2")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id2: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Defense-in-depth: check REORDER_PLAYLIST permission at the API layer
        // (the service layer also checks, but checking here provides early rejection
        // and consistent error handling with other API methods like clear_playlist)
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::REORDER_PLAYLIST,
            )
            .await
            .map_err(Self::map_room_access_error)?;

        let media_id1 = synctv_core::models::MediaId::from_string(req.media_id1.clone());
        let media_id2 = synctv_core::models::MediaId::from_string(req.media_id2.clone());
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        self.room_service
            .swap_media(rid.clone(), uid, media_id1, media_id2)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas so they refresh playlist order
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::SwapMediaResponse { success: true })
    }

    /// Get a single media record from database
    pub async fn get_media(
        &self,
        user_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        crate::http::validation::validate_id(media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let mid = synctv_core::models::MediaId::from_string(media_id.to_string());

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

        Ok(media_to_proto(&media))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_static_playlist_items_page, parse_add_media_provider,
        resolve_add_media_provider_instance, validate_dynamic_playlist_query_support,
    };
    use chrono::Utc;
    use serde_json::json;
    use synctv_core::models::{Media, Playlist, PlaylistId, ProviderType, RoomId, UserId};

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
            position: 0,
            source_provider: source_provider.map(str::to_string),
            source_config: Some(json!({})),
            provider_instance_name: provider_instance_name.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    fn make_media(
        name: &str,
        source_provider: &str,
        provider_instance_name: &str,
        position: i32,
    ) -> Media {
        Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: None,
            room_id: RoomId::new(),
            creator_id: Some(UserId::new()),
            name: name.to_string(),
            position,
            source_provider: source_provider.to_string(),
            source_config: json!({}),
            provider_instance_name: provider_instance_name.to_string(),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_resolve_add_media_provider_instance_maps_direct_to_builtin_instance() {
        let resolved =
            resolve_add_media_provider_instance(ProviderType::DirectUrl, "", String::new())
                .unwrap();
        assert_eq!(resolved, "direct_url");
    }

    #[test]
    fn test_resolve_add_media_provider_instance_uses_explicit_binding() {
        let resolved = resolve_add_media_provider_instance(
            ProviderType::Alist,
            "alist",
            "alist_main".to_string(),
        )
        .unwrap();
        assert_eq!(resolved, "alist_main");
    }

    #[test]
    fn test_resolve_add_media_provider_instance_rejects_missing_binding_for_remote_provider() {
        let err = resolve_add_media_provider_instance(ProviderType::Alist, "alist", String::new())
            .unwrap_err();
        assert!(
            err.to_string().contains("provider_instance_name"),
            "non-direct providers must require explicit provider_instance_name"
        );
    }

    #[test]
    fn test_parse_add_media_provider_defaults_empty_to_direct_url() {
        let provider = parse_add_media_provider("").unwrap();
        assert_eq!(provider, ProviderType::DirectUrl);
    }

    #[test]
    fn test_parse_add_media_provider_rejects_unknown_type() {
        let err = parse_add_media_provider("unknown-provider").unwrap_err();
        assert!(err.to_string().contains("Unknown provider type"));
    }

    #[test]
    fn test_build_static_playlist_items_page_applies_search_filter_and_media_provider_filters() {
        let playlists = vec![
            make_playlist("Alpha Folder", Some("alist"), Some("alist-main")),
            make_playlist("Beta Folder", None, None),
        ];
        let media = vec![
            make_media("Alpha Media", "direct_url", "direct_url", 1),
            make_media("Beta Media", "direct_url", "direct_url", 2),
        ];

        let page = build_static_playlist_items_page(
            playlists,
            media,
            &crate::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 10,
                search: "alpha".to_string(),
                source_provider: "direct_url".to_string(),
                provider_instance_name: "direct_url".to_string(),
                sort_by: crate::proto::client::MediaListSortBy::Name as i32,
                sort_direction: crate::proto::client::SortDirection::Asc as i32,
            },
        );

        assert_eq!(page.total, 1);
        assert_eq!(page.folder_count, 0);
        assert_eq!(page.file_count, 1);
        assert!(page.playlists.is_empty());
        assert_eq!(page.media.len(), 1);
        assert_eq!(page.media[0].name, "Alpha Media");
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
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("does not support search"));
    }
}
