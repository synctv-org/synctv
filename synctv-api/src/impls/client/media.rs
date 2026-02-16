//! Media operations: add, remove, edit, swap, clear, batch operations, playlist items

use std::str::FromStr;
use crate::impls::ApiError;
use synctv_core::models::{ProviderType, RoomId, UserId};

use super::ClientApiImpl;
use super::convert::media_to_proto;

impl ClientApiImpl {
    pub async fn add_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        let provider = if req.provider.is_empty() {
            ProviderType::DirectUrl
        } else {
            ProviderType::from_str(&req.provider)
                .map_err(|_| ApiError::InvalidInput(format!("Unknown provider type: '{}'", req.provider)))?
        };

        // Parse source config from request bytes
        let source_config: serde_json::Value = if req.source_config.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(&req.source_config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?
        };

        // Use provided title or default
        let title = if req.title.is_empty() {
            source_config.get("url")
                .and_then(|u| u.as_str())
                .and_then(|u| u.split('/').next_back())
                .unwrap_or("Unknown")
                .to_string()
        } else {
            req.title
        };

        // Use the parsed provider as the instance name. For DirectUrl, this is
        // empty (no remote provider). For other types, use the provider string
        // from the request so the core layer knows which provider to use.
        let provider_instance_name = if provider == ProviderType::DirectUrl {
            String::new()
        } else {
            req.provider
        };

        let media = self.room_service.add_media(
            rid,
            uid,
            provider_instance_name,
            source_config,
            title,
        ).await.map_err(ApiError::from)?;

        Ok(crate::proto::client::AddMediaResponse {
            media: Some(media_to_proto(&media)),
        })
    }

    pub async fn remove_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::RemoveMediaRequest,
    ) -> Result<crate::proto::client::RemoveMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let media_id_str = req.media_id.clone();
        let mid = synctv_core::models::MediaId::from_string(req.media_id);

        // Fetch media before deletion so we can invalidate its playback cache
        let media = self.room_service.media_service()
            .get_media(&mid).await
            .ok()
            .flatten();

        self.room_service.remove_media(rid, uid, mid).await
            .map_err(ApiError::from)?;

        // Invalidate playback cache (best-effort)
        if let (Some(media), Some(pm)) = (&media, self.providers_manager.as_ref()) {
            if !media.is_direct() {
                let instance_name = media.provider_instance_name
                    .as_deref()
                    .unwrap_or(&media.source_provider);
                if let Some(provider) = pm.get(instance_name).await {
                    crate::http::provider_common::invalidate_playback_cache(
                        provider.as_ref(),
                        &media.source_config,
                        self.redis_conn.as_ref(),
                    ).await;
                }
            }
        }

        // Kick active stream for deleted media (local + cluster-wide)
        self.kick_stream_cluster(room_id, &media_id_str, "media_deleted");

        Ok(crate::proto::client::RemoveMediaResponse {
            success: true,
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
        let rid = RoomId::from_string(room_id.to_string());
        let mid = synctv_core::models::MediaId::from_string(req.media_id);

        let title = if req.title.is_empty() { None } else { Some(req.title) };

        let media = self.room_service
            .edit_media(rid, uid, mid, title)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to edit media: {e}")))?;

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
            .check_permission(&rid, &uid, synctv_core::models::PermissionBits::CLEAR_PLAYLIST)
            .await
            .map_err(ApiError::from)?;

        // Fetch all media before deletion for cache invalidation
        let media_items = self.room_service
            .get_playlist(&rid)
            .await
            .unwrap_or_default();

        let deleted_count = self.room_service
            .clear_playlist(rid, uid)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to clear playlist: {e}")))?;

        // Invalidate playback cache for cleared media (best-effort)
        if let Some(pm) = self.providers_manager.as_ref() {
            crate::http::provider_common::invalidate_playback_cache_batch(
                &media_items,
                pm,
                self.redis_conn.as_ref(),
            ).await;
        }

        Ok(crate::proto::client::ClearPlaylistResponse {
            success: true,
            deleted_count: deleted_count as i32,
        })
    }

    /// Add multiple media items in a batch (atomic - all succeed or all fail)
    pub async fn add_media_batch(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::AddMediaBatchRequest,
    ) -> Result<crate::proto::client::AddMediaBatchResponse, ApiError> {
        if req.items.is_empty() {
            return Err(ApiError::InvalidInput("items array cannot be empty".to_string()));
        }
        if req.items.len() > 100 {
            return Err(ApiError::InvalidInput("Too many items (max 100 per batch)".to_string()));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Build batch items for the atomic service call
        let mut items: Vec<(String, serde_json::Value, String)> = Vec::with_capacity(req.items.len());
        for item in &req.items {
            let source_config: serde_json::Value = if item.source_config.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&item.source_config)
                    .map_err(|e| ApiError::InvalidInput(format!("Invalid source_config JSON: {e}")))?
            };
            let title = if item.title.is_empty() {
                source_config.get("url")
                    .and_then(|u| u.as_str())
                    .and_then(|u| u.split('/').next_back())
                    .unwrap_or("Unknown")
                    .to_string()
            } else {
                item.title.clone()
            };
            items.push((String::new(), source_config, title));
        }

        let media_list = self.room_service.add_media_batch(rid, uid, items)
            .await
            .map_err(ApiError::from)?;

        let results = media_list.into_iter()
            .map(|media| crate::proto::client::AddMediaResponse {
                media: Some(media_to_proto(&media)),
            })
            .collect();

        Ok(crate::proto::client::AddMediaBatchResponse { results })
    }

    /// Bulk remove multiple media items
    pub async fn remove_media_batch(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::RemoveMediaBatchRequest,
    ) -> Result<crate::proto::client::RemoveMediaBatchResponse, ApiError> {
        if req.media_ids.is_empty() {
            return Err(ApiError::InvalidInput("media_ids array cannot be empty".to_string()));
        }
        if req.media_ids.len() > 100 {
            return Err(ApiError::InvalidInput("Too many items (max 100 per batch)".to_string()));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let media_id_strings: Vec<String> = req.media_ids.clone();
        let mids: Vec<synctv_core::models::MediaId> = req.media_ids
            .into_iter()
            .map(synctv_core::models::MediaId::from_string)
            .collect();

        // Fetch media items before deletion for cache invalidation
        let media_items: Vec<synctv_core::models::Media> = {
            let mut items = Vec::with_capacity(mids.len());
            for mid in &mids {
                if let Ok(Some(m)) = self.room_service.media_service().get_media(mid).await {
                    items.push(m);
                }
            }
            items
        };

        let deleted_count = self.room_service
            .media_service()
            .remove_media_batch(rid, uid, mids)
            .await
            .map_err(ApiError::from)?;

        // Invalidate playback cache for deleted media (best-effort)
        if let Some(pm) = self.providers_manager.as_ref() {
            crate::http::provider_common::invalidate_playback_cache_batch(
                &media_items,
                pm,
                self.redis_conn.as_ref(),
            ).await;
        }

        // Kick active streams for deleted media (local + cluster-wide)
        for media_id in &media_id_strings {
            self.kick_stream_cluster(room_id, media_id, "media_deleted");
        }

        Ok(crate::proto::client::RemoveMediaBatchResponse {
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
            return Err(ApiError::InvalidInput("updates array cannot be empty".to_string()));
        }
        if req.updates.len() > 100 {
            return Err(ApiError::InvalidInput("Too many items (max 100 per batch)".to_string()));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let updates_converted: Vec<(synctv_core::models::MediaId, i32)> = req.updates
            .into_iter()
            .map(|u| (synctv_core::models::MediaId::from_string(u.media_id), u.position))
            .collect();

        self.room_service
            .media_service()
            .reorder_media_batch(rid, uid, updates_converted)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::ReorderMediaBatchResponse { success: true })
    }

    pub async fn get_playlist(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::ListPlaylistResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;
        let media_list = self.room_service.get_playlist(&rid).await
            .map_err(ApiError::from)?;

        let media: Vec<_> = media_list.into_iter().map(|m| media_to_proto(&m)).collect();
        let total = media.len() as i32;

        let playlist = match self.room_service.playlist_service().get_root_playlist(&rid).await {
            Ok(pl) => Some(crate::proto::client::Playlist {
                id: pl.id.as_str().to_string(),
                room_id: pl.room_id.as_str().to_string(),
                name: pl.name.clone(),
                parent_id: pl.parent_id.as_ref().map_or(String::new(), |p| p.as_str().to_string()),
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

    pub async fn list_playlist_items(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let playlist_id = synctv_core::models::PlaylistId::from_string(req.playlist_id.clone());

        let relative_path = if req.relative_path.is_empty() {
            None
        } else {
            Some(req.relative_path.as_str())
        };

        let page = req.page.max(0) as usize;
        let page_size = req.page_size.clamp(1, 100) as usize;

        let items = self
            .room_service
            .media_service()
            .list_dynamic_playlist_items(rid, uid, &playlist_id, relative_path, page, page_size)
            .await
            .map_err(ApiError::from)?;

        // Convert DirectoryItem to proto DirectoryItem
        let proto_items: Vec<_> = items
            .into_iter()
            .map(|item| {
                use synctv_core::provider::ItemType;
                let item_type = match item.item_type {
                    ItemType::Video => crate::proto::client::ItemType::Video as i32,
                    ItemType::Audio => crate::proto::client::ItemType::Audio as i32,
                    ItemType::Folder => crate::proto::client::ItemType::Folder as i32,
                    ItemType::Live => crate::proto::client::ItemType::Live as i32,
                    ItemType::File => crate::proto::client::ItemType::File as i32,
                };

                crate::proto::client::DirectoryItem {
                    name: item.name,
                    item_type,
                    path: item.path,
                    size: item.size.map(|s| s as i64),
                    thumbnail: item.thumbnail,
                    modified_at: item.modified_at,
                }
            })
            .collect();

        let total = proto_items.len() as i32;

        Ok(crate::proto::client::ListPlaylistItemsResponse {
            items: proto_items,
            total,
        })
    }

    pub async fn swap_media(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SwapMediaRequest,
    ) -> Result<crate::proto::client::SwapMediaResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let media_id1 = synctv_core::models::MediaId::from_string(req.media_id1.clone());
        let media_id2 = synctv_core::models::MediaId::from_string(req.media_id2.clone());

        self.room_service.swap_media(rid, uid, media_id1, media_id2).await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::SwapMediaResponse {
            success: true,
        })
    }

    /// Get movie info for a media item (resolves provider playback + proxy/direct decision)
    pub async fn get_movie_info(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetMovieInfoRequest,
    ) -> Result<crate::proto::client::GetMovieInfoResponse, ApiError> {
        use std::collections::HashMap;
        use synctv_core::provider::ProviderContext;

        let rid = RoomId::from_string(room_id.to_string());
        let uid = UserId::from_string(user_id.to_string());
        let mid = synctv_core::models::MediaId::from_string(req.media_id);
        let media_id = mid.as_str();

        // Check VIEW_PLAYLIST permission before exposing playback URLs
        self.room_service
            .check_permission(&rid, &uid, synctv_core::models::PermissionBits::VIEW_PLAYLIST)
            .await
            .map_err(ApiError::from)?;

        // 1. Get media from playlist
        let playlist = self
            .room_service
            .get_playlist(&rid)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to get playlist: {e}")))?;

        let media = playlist
            .iter()
            .find(|m| m.id == mid)
            .ok_or_else(|| ApiError::NotFound("Media not found in playlist".to_string()))?;

        // 2. For direct URL media, return playback info directly
        if media.is_direct() {
            let playback = media
                .get_playback_result()
                .ok_or_else(|| ApiError::Internal("Failed to parse direct media playback info".to_string()))?;
            let default_info = playback.get_default_playback_info();
            let (url, media_type) = if let Some(info) = default_info {
                let first_url = info.urls.first().map(|u| u.url.clone()).unwrap_or_default();
                let fmt = info
                    .urls
                    .first()
                    .and_then(|u| u.metadata.as_ref())
                    .and_then(|m| m.codec.clone())
                    .unwrap_or_else(|| "mp4".to_string());
                (first_url, fmt)
            } else {
                (String::new(), "unknown".to_string())
            };

            return Ok(crate::proto::client::GetMovieInfoResponse {
                movie: Some(crate::proto::client::MovieInfo {
                    r#type: media_type,
                    url,
                    headers: HashMap::new(),
                    more_sources: Vec::new(),
                    subtitles: Vec::new(),
                    is_live: false,
                    duration: playback
                        .metadata
                        .get("duration")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                }),
            });
        }

        // 3. For provider-backed media, use ProvidersManager
        let providers_manager = self.providers_manager.as_ref()
            .ok_or_else(|| ApiError::Internal("Providers manager not configured".to_string()))?;

        let instance_name = media
            .provider_instance_name
            .as_deref()
            .unwrap_or(&media.source_provider);

        let provider = providers_manager
            .get(instance_name)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("Provider instance '{instance_name}' not found")))?;

        let ctx = ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_room_id(room_id);

        let result = crate::http::provider_common::cached_generate_playback(
            provider.as_ref(),
            &ctx,
            &media.source_config,
            self.redis_conn.as_ref(),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("generate_playback failed: {e}")))?;

        // 4. Check movie_proxy setting
        let movie_proxy = self.settings_registry.as_ref()
            .is_none_or(|r| r.to_public_settings().movie_proxy);

        // 5. Build MovieInfo
        let is_live = result
            .metadata
            .get("is_live")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let duration = result
            .metadata
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);

        let subtitles: Vec<crate::proto::client::MovieSubtitle> = result
            .playback_infos
            .values()
            .flat_map(|pi| &pi.subtitles)
            .map(|s| crate::proto::client::MovieSubtitle {
                name: s.name.clone(),
                language: s.language.clone(),
                url: if movie_proxy {
                    format!(
                        "/api/providers/{}/proxy/{room_id}/{media_id}/subtitle/{}",
                        media.source_provider,
                        url::form_urlencoded::byte_serialize(s.name.as_bytes())
                            .collect::<String>()
                    )
                } else {
                    s.url.clone()
                },
                format: s.format.clone(),
            })
            .collect();

        // For DASH providers (bilibili Video/Pgc): generate MPD URLs
        if result.dash.is_some() {
            let (url, media_type, headers) = if movie_proxy {
                (
                    format!("/api/providers/{}/proxy/{room_id}/{media_id}/mpd", media.source_provider),
                    "mpd".to_string(),
                    HashMap::new(),
                )
            } else {
                // Strip credential headers before exposing to client
                let safe_headers: HashMap<String, String> = result
                    .playback_infos
                    .get("dash")
                    .map(|pi| {
                        pi.headers.iter()
                            .filter(|(k, _)| {
                                let lower = k.to_lowercase();
                                !lower.contains("token") && !lower.contains("authorization")
                                    && !lower.contains("cookie") && !lower.contains("api_key")
                                    && !lower.contains("x-emby")
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    format!("/api/providers/{}/proxy/{room_id}/{media_id}/mpd?direct=1", media.source_provider),
                    "mpd".to_string(),
                    safe_headers,
                )
            };

            let mut more_sources = Vec::new();
            if result.hevc_dash.is_some() {
                let hevc_url = if movie_proxy {
                    format!("/api/providers/{}/proxy/{room_id}/{media_id}/mpd?codec=hevc", media.source_provider)
                } else {
                    format!("/api/providers/{}/proxy/{room_id}/{media_id}/mpd?codec=hevc&direct=1", media.source_provider)
                };
                more_sources.push(crate::proto::client::MovieSource {
                    name: "HEVC".to_string(),
                    r#type: "mpd".to_string(),
                    url: hevc_url,
                    headers: HashMap::new(),
                });
            }

            return Ok(crate::proto::client::GetMovieInfoResponse {
                movie: Some(crate::proto::client::MovieInfo {
                    r#type: media_type, url, headers, more_sources, subtitles, is_live, duration,
                }),
            });
        }

        // For HLS/direct providers: use first URL from default playback mode
        let default_info = result.playback_infos.get(&result.default_mode);
        let (url, media_type, headers) = if let Some(info) = default_info {
            let first_url = info.urls.first().cloned().unwrap_or_default();
            if movie_proxy && !first_url.is_empty() {
                (
                    format!("/api/providers/{}/proxy/{room_id}/{media_id}", media.source_provider),
                    info.format.clone(),
                    HashMap::new(),
                )
            } else {
                // Strip credential headers before exposing to client
                let safe_headers: HashMap<String, String> = info.headers.iter()
                    .filter(|(k, _)| {
                        let lower = k.to_lowercase();
                        !lower.contains("token") && !lower.contains("authorization")
                            && !lower.contains("cookie") && !lower.contains("api_key")
                            && !lower.contains("x-emby")
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                (first_url, info.format.clone(), safe_headers)
            }
        } else {
            (String::new(), "unknown".to_string(), HashMap::new())
        };

        Ok(crate::proto::client::GetMovieInfoResponse {
            movie: Some(crate::proto::client::MovieInfo {
                r#type: media_type, url, headers, more_sources: Vec::new(), subtitles, is_live, duration,
            }),
        })
    }
}
