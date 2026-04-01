//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use std::str::FromStr;
use synctv_core::models::{ProviderType, UserId};

use super::convert::{media_to_proto, playlist_path_node_to_proto, playlist_to_proto};
use super::ClientApiImpl;

/// Sanitize and truncate a title extracted from a URL path segment.
///
/// Unlike user-provided titles (which are validated and rejected if invalid),
/// URL-derived titles are best-effort: we sanitize control characters and
/// truncate to [`MEDIA_TITLE_MAX`] to ensure they fit in the database.
pub(super) fn sanitize_url_derived_title(raw: &str) -> String {
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

fn resolve_add_media_provider_instance(
    provider: ProviderType,
    provider_name: &str,
    provider_instance_name: String,
) -> Result<String, ApiError> {
    if provider == ProviderType::DirectUrl {
        return Ok(String::new());
    }

    let trimmed = provider_instance_name.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }

    Err(ApiError::InvalidInput(format!(
        "provider_instance_name is required for provider '{provider_name}'"
    )))
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

        let provider = if req.provider.is_empty() {
            ProviderType::DirectUrl
        } else {
            ProviderType::from_str(&req.provider).map_err(|_| {
                ApiError::InvalidInput(format!("Unknown provider type: '{}'", req.provider))
            })?
        };

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

        // Use the explicit provider_instance_name from the request for registry lookup.
        // For DirectUrl, this is empty (no remote provider).
        // For other provider types, prefer provider_instance_name (e.g., "bilibili_main")
        // over provider type name (e.g., "bilibili") since the registry stores instances
        // by their instance ID, not by type name.
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
                        event_id: nanoid::nanoid!(16),
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
                            event_id: nanoid::nanoid!(16),
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
                        event_id: nanoid::nanoid!(16),
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
                            event_id: nanoid::nanoid!(16),
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
            items.push(synctv_core::service::media::AddMediaRequest {
                playlist_id,
                name: title,
                provider_instance_name: item.provider_instance_name.clone(),
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
            .ok_or_else(|| ApiError::InvalidInput("Batch add must target one location".to_string()))?
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
                            event_id: nanoid::nanoid!(16),
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

        let page = req.page.max(1) as usize;
        let page_size = req.page_size.clamp(1, 100) as usize;
        let Some(playlist_id) = (!req.playlist_id.is_empty())
            .then(|| synctv_core::models::PlaylistId::from_string(req.playlist_id.clone()))
        else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing the room root".to_string(),
                ));
            }

            let folder_count = self
                .room_service
                .playlist_service()
                .count_top_level_playlists(&rid)
                .await
                .map_err(ApiError::from)? as usize;
            let file_count = self
                .room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)? as usize;
            let total = folder_count + file_count;
            let skip = (page - 1) * page_size;
            let mut proto_playlists = Vec::new();
            let mut proto_media = Vec::new();

            if skip < folder_count {
                let folder_take = (folder_count - skip).min(page_size);
                let folders_page = self
                    .room_service
                    .playlist_service()
                    .get_top_level_playlists_paginated(&rid, folder_take as i64, skip as i64)
                    .await
                    .map_err(ApiError::from)?;
                let folder_ids: Vec<&str> = folders_page.iter().map(|pl| pl.id.as_str()).collect();
                let counts = self
                    .room_service
                    .media_service()
                    .count_playlist_media_batch(&folder_ids)
                    .await
                    .unwrap_or_default();
                proto_playlists = folders_page
                    .iter()
                    .map(|pl| {
                        let item_count = counts.get(pl.id.as_str()).copied().unwrap_or(0) as i32;
                        playlist_to_proto(pl, item_count)
                    })
                    .collect();

                let remaining = page_size - folder_take;
                if remaining > 0 && file_count > 0 {
                    let media_page = self
                        .room_service
                        .media_service()
                        .get_room_root_media_offset_limit(&rid, remaining as i64, 0)
                        .await
                        .map_err(ApiError::from)?;
                    proto_media = media_page.into_iter().map(|m| media_to_proto(&m)).collect();
                }
            } else {
                let media_skip = skip - folder_count;
                if file_count > media_skip {
                    let media_page = self
                        .room_service
                        .media_service()
                        .get_room_root_media_offset_limit(&rid, page_size as i64, media_skip as i64)
                        .await
                        .map_err(ApiError::from)?;
                    proto_media = media_page.into_iter().map(|m| media_to_proto(&m)).collect();
                }
            }

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

            Ok(crate::proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
                current_path,
            })
        } else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing a static playlist".to_string(),
                ));
            }

            // Static playlist: list child playlists and media from database
            // Use database pagination to avoid loading all items into memory.

            // Get counts first (2 small queries instead of loading all data)
            let folder_count = self
                .room_service
                .playlist_service()
                .count_children(&playlist_id)
                .await
                .map_err(ApiError::from)? as usize;

            let file_count = self
                .room_service
                .media_service()
                .count_playlist_media(&playlist_id)
                .await
                .map_err(ApiError::from)? as usize;

            let total = folder_count + file_count;

            // Pagination: folders first, then files
            let skip = (page - 1) * page_size;

            let (mut proto_playlists, mut proto_media) = (Vec::new(), Vec::new());

            if skip < folder_count {
                // Current page includes folders - fetch paginated folders from DB
                let folder_take = (folder_count - skip).min(page_size);
                let folders_page = self
                    .room_service
                    .playlist_service()
                    .get_children_paginated(&playlist_id, folder_take as i64, skip as i64)
                    .await
                    .map_err(ApiError::from)?;

                // Batch fetch media counts for folders
                let folder_ids: Vec<&str> = folders_page.iter().map(|pl| pl.id.as_str()).collect();
                let counts = self
                    .room_service
                    .media_service()
                    .count_playlist_media_batch(&folder_ids)
                    .await
                    .unwrap_or_default();

                // Convert to proto Playlist
                proto_playlists = folders_page
                    .iter()
                    .map(|pl| {
                        let item_count = counts.get(pl.id.as_str()).copied().unwrap_or(0) as i32;
                        playlist_to_proto(pl, item_count)
                    })
                    .collect();

                // If there's room left on this page, add media
                let remaining = page_size - folder_take;
                if remaining > 0 && file_count > 0 {
                    // Fetch only the needed media items from DB
                    let media_page = self
                        .room_service
                        .media_service()
                        .get_playlist_media_offset_limit(&playlist_id, remaining as i64, 0)
                        .await
                        .map_err(ApiError::from)?;
                    proto_media = media_page.into_iter().map(|m| media_to_proto(&m)).collect();
                }
            } else {
                // Current page only has media (all folders already shown)
                let media_skip = skip - folder_count;
                if file_count > media_skip {
                    // Fetch only the needed media items from DB
                    let media_page = self
                        .room_service
                        .media_service()
                        .get_playlist_media_offset_limit(
                            &playlist_id,
                            page_size as i64,
                            media_skip as i64,
                        )
                        .await
                        .map_err(ApiError::from)?;
                    proto_media = media_page.into_iter().map(|m| media_to_proto(&m)).collect();
                }
            }

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
    use super::resolve_add_media_provider_instance;
    use synctv_core::models::ProviderType;

    #[test]
    fn test_resolve_add_media_provider_instance_allows_direct_without_binding() {
        let resolved =
            resolve_add_media_provider_instance(ProviderType::DirectUrl, "", String::new())
                .unwrap();
        assert!(resolved.is_empty());
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
        let err =
            resolve_add_media_provider_instance(ProviderType::Alist, "alist", String::new())
                .unwrap_err();
        assert!(
            err.to_string().contains("provider_instance_name"),
            "non-direct providers must require explicit provider_instance_name"
        );
    }
}
