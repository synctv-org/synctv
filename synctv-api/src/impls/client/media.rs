//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use crate::impls::ApiError;
use std::str::FromStr;
use synctv_core::models::{ProviderType, RoomId, UserId};

use super::convert::{media_to_proto, playlist_to_proto};
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

    // Truncate to max allowed length (byte-safe: find last char boundary)
    if sanitized.len() > MEDIA_TITLE_MAX {
        let mut end = MEDIA_TITLE_MAX;
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized[..end].to_string()
    } else {
        sanitized.into_owned()
    }
}

impl ClientApiImpl {
    /// Validate `source_config` URLs for SSRF protection.
    ///
    /// Checks `url` and `urls` fields in the `source_config` JSON to prevent
    /// attackers from forcing the server to make requests to internal network addresses.
    fn validate_source_config_urls(source_config_bytes: &[u8]) -> Result<(), ApiError> {
        if source_config_bytes.is_empty() {
            return Ok(());
        }
        if let Ok(source_config) = serde_json::from_slice::<serde_json::Value>(source_config_bytes)
        {
            if let Some(url_str) = source_config.get("url").and_then(|u| u.as_str()) {
                crate::http::validation::validate_url(url_str)
                    .map_err(|e| ApiError::InvalidInput(format!("Invalid media URL: {e}")))?;
            }
            if let Some(urls_arr) = source_config.get("urls").and_then(|v| v.as_array()) {
                for url_val in urls_arr {
                    if let Some(url_str) = url_val.as_str() {
                        crate::http::validation::validate_url(url_str).map_err(|e| {
                            ApiError::InvalidInput(format!("Invalid media URL: {e}"))
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn add_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;

        // Validate media URLs to prevent SSRF attacks
        Self::validate_source_config_urls(&req.source_config)?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

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
        let root_playlist = self
            .room_service
            .playlist_service()
            .get_root_playlist(&rid)
            .await
            .map_err(ApiError::from)?;
        let existing_count = self
            .room_service
            .media_service()
            .count_playlist_media(&root_playlist.id)
            .await
            .map_err(ApiError::from)? as usize;
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
        let provider_instance_name = if provider == ProviderType::DirectUrl {
            String::new()
        } else if !req.provider_instance_name.is_empty() {
            req.provider_instance_name
        } else {
            // Fallback: use provider type name for backwards compatibility
            // when client doesn't specify an instance name
            req.provider
        };

        let media = self
            .room_service
            .add_media(
                rid.clone(),
                uid.clone(),
                provider_instance_name,
                source_config,
                title,
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
            crate::impls::try_publish_cluster_event(
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
            );
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
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;
        crate::http::validation::validate_id(&req.media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let media_id_str = req.media_id.clone();
        let mid = synctv_core::models::MediaId::from_string(req.media_id);

        // Fetch media before deletion so we can invalidate its playback cache
        let media = self
            .room_service
            .media_service()
            .get_media(&mid)
            .await
            .ok()
            .flatten();

        self.room_service
            .remove_media(rid.clone(), uid.clone(), mid)
            .await
            .map_err(ApiError::from)?;

        // Broadcast MediaRemoved cluster event for cross-replica propagation
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::MediaRemoved {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid,
                        user_id: uid,
                        username,
                        media_id: synctv_core::models::MediaId::from_string(media_id_str.clone()),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Invalidate playback cache (best-effort)
        if let (Some(media), Some(pm)) = (&media, self.providers_manager.as_ref()) {
            if !media.is_direct() {
                let instance_name = media
                    .provider_instance_name
                    .as_deref()
                    .unwrap_or(&media.source_provider);
                if let Some(provider) = pm.get(instance_name).await {
                    let resolved = self.resolve_redis_conn().await;
                    crate::impls::provider::invalidate_playback_cache(
                        provider.as_ref(),
                        &media.source_config,
                        resolved.as_ref(),
                    )
                    .await;
                }
            }
        }

        // Kick active stream for deleted media (local + cluster-wide)
        self.kick_stream_cluster(room_id, &media_id_str, "media_deleted");

        Ok(crate::proto::client::DeleteMediaResponse { success: true })
    }

    /// Edit media metadata
    pub async fn edit_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::EditMediaRequest,
    ) -> Result<crate::proto::client::EditMediaResponse, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;
        crate::http::validation::validate_id(&req.media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let mid = synctv_core::models::MediaId::from_string(req.media_id);

        let title = if req.title.is_empty() {
            None
        } else {
            // Validate user-provided title for length and security
            let validated = crate::http::validation::validate_media_title(&req.title)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid media title: {e}")))?;
            Some(validated)
        };

        let media = self
            .room_service
            .edit_media(rid.clone(), uid, mid, title)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to edit media: {e}")))?;

        // Invalidate room cache on other replicas so they see updated metadata
        self.publish_room_cache_invalidation(&rid);

        Ok(crate::proto::client::EditMediaResponse {
            media: Some(media_to_proto(&media)),
        })
    }

    /// Clear all media from room's root playlist
    pub async fn clear_playlist(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::ClearPlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::CLEAR_PLAYLIST,
            )
            .await
            .map_err(ApiError::from)?;

        // Fetch all media before deletion for cache invalidation
        let media_items = self
            .room_service
            .get_playlist(&rid)
            .await
            .unwrap_or_default();

        let deleted_count = self
            .room_service
            .clear_playlist(rid.clone(), uid.clone())
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to clear playlist: {e}")))?;

        // Broadcast MediaRemoved for each cleared item so other replicas update playlists
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            for media in &media_items {
                crate::impls::try_publish_cluster_event(
                    tx,
                    synctv_cluster::sync::PublishRequest {
                        event: synctv_cluster::sync::ClusterEvent::MediaRemoved {
                            event_id: nanoid::nanoid!(16),
                            room_id: rid.clone(),
                            user_id: uid.clone(),
                            username: username.clone(),
                            media_id: media.id.clone(),
                            timestamp: chrono::Utc::now(),
                        },
                    },
                );
            }
        }

        // Invalidate playback cache for cleared media (best-effort)
        if let Some(pm) = self.providers_manager.as_ref() {
            let resolved = self.resolve_redis_conn().await;
            crate::impls::provider::invalidate_playback_cache_batch(
                &media_items,
                pm,
                resolved.as_ref(),
            )
            .await;
        }

        Ok(crate::proto::client::ClearPlaylistResponse {
            success: true,
            deleted_count: deleted_count as i32,
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
        let rid = RoomId::from_string(room_id.to_string());

        // Check total playlist size limit to prevent unbounded growth
        let root_playlist = self
            .room_service
            .playlist_service()
            .get_root_playlist(&rid)
            .await
            .map_err(ApiError::from)?;
        let existing_count = self
            .room_service
            .media_service()
            .count_playlist_media(&root_playlist.id)
            .await
            .map_err(ApiError::from)? as usize;
        let new_total = existing_count + req.items.len();
        if new_total > Self::MAX_PLAYLIST_SIZE {
            return Err(ApiError::InvalidInput(format!(
                "Playlist would exceed maximum size of {} items \
                 (current: {}, adding: {})",
                Self::MAX_PLAYLIST_SIZE,
                existing_count,
                req.items.len()
            )));
        }

        // Build batch items for the atomic service call
        let mut items: Vec<(String, serde_json::Value, String)> =
            Vec::with_capacity(req.items.len());
        for item in &req.items {
            // Validate media URLs for SSRF protection (same as add_media)
            Self::validate_source_config_urls(&item.source_config)?;

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
            // Use provider_instance_name from the request item
            items.push((item.provider_instance_name.clone(), source_config, title));
        }

        let media_list = self
            .room_service
            .add_media_batch(rid.clone(), uid.clone(), items)
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
                crate::impls::try_publish_cluster_event(
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
                );
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

    /// Bulk delete multiple media items
    pub async fn delete_media_batch(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::DeleteMediaBatchRequest,
    ) -> Result<crate::proto::client::DeleteMediaBatchResponse, ApiError> {
        if req.media_ids.is_empty() {
            return Err(ApiError::InvalidInput(
                "media_ids array cannot be empty".to_string(),
            ));
        }
        if req.media_ids.len() > 100 {
            return Err(ApiError::InvalidInput(
                "Too many items (max 100 per batch)".to_string(),
            ));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let media_id_strings: Vec<String> = req.media_ids.clone();
        let mids: Vec<synctv_core::models::MediaId> = req
            .media_ids
            .into_iter()
            .map(synctv_core::models::MediaId::from_string)
            .collect();

        // M-12: Batch fetch all media in a single query instead of N+1
        let media_items: Vec<synctv_core::models::Media> = self
            .room_service
            .media_service()
            .get_media_batch(&mids)
            .await
            .unwrap_or_default();

        let deleted_count = self
            .room_service
            .media_service()
            .remove_media_batch(rid.clone(), uid.clone(), mids)
            .await
            .map_err(ApiError::from)?;

        // Broadcast single MediaRemovedBatch event instead of N individual events
        // This reduces Redis pub/sub traffic from O(n) to O(1) messages
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();
            let media_ids: Vec<synctv_core::models::MediaId> = media_id_strings
                .iter()
                .map(|id| synctv_core::models::MediaId::from_string(id.clone()))
                .collect();
            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::MediaRemovedBatch {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: uid.clone(),
                        username,
                        media_ids,
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Invalidate playback cache for deleted media (best-effort)
        if let Some(pm) = self.providers_manager.as_ref() {
            let resolved = self.resolve_redis_conn().await;
            crate::impls::provider::invalidate_playback_cache_batch(
                &media_items,
                pm,
                resolved.as_ref(),
            )
            .await;
        }

        // Kick active streams for deleted media (local + cluster-wide)
        for media_id in &media_id_strings {
            self.kick_stream_cluster(room_id, media_id, "media_deleted");
        }

        Ok(crate::proto::client::DeleteMediaBatchResponse {
            deleted_count: deleted_count as i32,
        })
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

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

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
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

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

        self.room_service
            .media_service()
            .reorder_media_batch(rid.clone(), uid, updates_converted)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas so they refresh playlist order
        self.publish_room_cache_invalidation(&rid);

        Ok(crate::proto::client::ReorderMediaBatchResponse { success: true })
    }

    /// List media items in room's playlist
    pub async fn list_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistRequest,
    ) -> Result<crate::proto::client::ListPlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // M-7: Use paginated query instead of loading all items into memory.
        // Default to first page with 50 items, max 200 items per page.
        let page = req.page.max(1) as u32;
        let page_size = req.page_size.clamp(1, 200) as u32;
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));
        let (media_list, total_count) = self
            .room_service
            .get_playlist_paginated(&rid, pagination)
            .await
            .map_err(ApiError::from)?;

        let media: Vec<_> = media_list.into_iter().map(|m| media_to_proto(&m)).collect();
        let total = total_count as i32;

        let playlist = match self
            .room_service
            .playlist_service()
            .get_root_playlist(&rid)
            .await
        {
            Ok(pl) => Some(crate::proto::client::Playlist {
                id: pl.id.as_str().to_string(),
                room_id: pl.room_id.as_str().to_string(),
                name: pl.name.clone(),
                parent_id: pl
                    .parent_id
                    .as_ref()
                    .map_or(String::new(), |p| p.as_str().to_string()),
                position: pl.position,
                is_folder: true,
                is_dynamic: pl.is_dynamic(),
                item_count: total,
                created_at: pl.created_at.timestamp(),
                updated_at: pl.updated_at.timestamp(),
            }),
            Err(_) => Some(crate::proto::client::Playlist {
                id: String::new(),
                room_id: rid.as_str().to_string(),
                name: String::new(),
                parent_id: String::new(),
                position: 0,
                is_folder: true,
                is_dynamic: false,
                item_count: total,
                created_at: 0,
                updated_at: 0,
            }),
        };

        Ok(crate::proto::client::ListPlaylistResponse {
            playlist,
            media,
            total,
        })
    }

    /// List playlist items (supports both static and dynamic playlists)
    ///
    /// For static playlists: returns child playlists + media from database
    /// For dynamic playlists: returns remote provider items
    pub async fn list_playlist_items(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id.clone());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // Get playlist info to determine if static or dynamic
        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(&playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {} not found", req.playlist_id)))?;

        let page = req.page.max(1) as usize;
        let page_size = req.page_size.clamp(1, 100) as usize;

        if playlist.is_dynamic() {
            // Dynamic playlist: fetch from provider
            let relative_path = if req.relative_path.is_empty() {
                None
            } else {
                Some(req.relative_path.as_str())
            };

            let items = self
                .room_service
                .media_service()
                .list_dynamic_playlist_items(rid, uid, &playlist_id, relative_path, page, page_size)
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

                    crate::proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        path: item.path,
                        size: item.size.map(|s| s as i64),
                        thumbnail: Some(item.thumbnail.unwrap_or_default()),
                        modified_at: Some(item.modified_at.unwrap_or(0)),
                    }
                })
                .collect();

            let total = dynamic_items.len() as i32;

            Ok(crate::proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
            })
        } else {
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
            })
        }
    }

    pub async fn swap_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SwapMediaRequest,
    ) -> Result<crate::proto::client::SwapMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

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
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let media_id1 = synctv_core::models::MediaId::from_string(req.media_id1.clone());
        let media_id2 = synctv_core::models::MediaId::from_string(req.media_id2.clone());

        self.room_service
            .swap_media(rid.clone(), uid, media_id1, media_id2)
            .await
            .map_err(ApiError::from)?;

        // Invalidate room cache on other replicas so they refresh playlist order
        self.publish_room_cache_invalidation(&rid);

        Ok(crate::proto::client::SwapMediaResponse { success: true })
    }

    /// Get a single media record from database
    pub async fn get_media(
        &self,
        user_id: &str,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::Media, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;
        crate::http::validation::validate_id(media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
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
