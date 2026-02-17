//! Emby/Jellyfin `MediaProvider` Adapter
//!
//! Adapter that calls `EmbyClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_emby_client, load_local_emby_client, EmbyClientArc},
    DirectoryItem, DynamicFolder, ItemType, MediaProvider, NextPlayItem, PlaybackInfo,
    PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
};
use crate::service::RemoteProviderManager;
use crate::validation::{validate_url_for_ssrf, ValidationError};
use async_trait::async_trait;
use chrono::Utc;
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Emby `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct EmbyProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
}

impl EmbyProvider {
    /// Create a new `EmbyProvider` with `RemoteProviderManager`
    #[must_use] 
    pub const fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
        }
    }

    /// Get Emby client for the given instance name (remote if available, local fallback)
    async fn get_client(&self, instance_name: Option<&str>) -> EmbyClientArc {
        self.provider_instance_manager
            .resolve_client(instance_name, create_remote_emby_client, load_local_emby_client)
            .await
    }

    // ========== Provider API Methods ==========

    /// Login to Emby/Jellyfin (validate API key)
    pub async fn login(
        &self,
        host: String,
        api_key: String,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::MeResp, ProviderError> {
        let client = self.get_client(instance_name).await;

        let me_req = synctv_media_providers::grpc::emby::MeReq {
            host,
            token: api_key,
            user_id: String::new(), // Empty = get current user
        };

        client.me(me_req).await.map_err(std::convert::Into::into)
    }

    /// List Emby library items
    pub async fn fs_list(
        &self,
        req: synctv_media_providers::grpc::emby::FsListReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::FsListResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.fs_list(req).await.map_err(std::convert::Into::into)
    }

    /// Get Emby user info
    pub async fn me(
        &self,
        req: synctv_media_providers::grpc::emby::MeReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::MeResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.me(req).await.map_err(std::convert::Into::into)
    }
}

/// Emby source configuration
#[derive(Debug, Deserialize, Serialize)]
struct EmbySourceConfig {
    host: String,
    token: String,
    user_id: String,
    item_id: String,
    #[serde(default)]
    provider_instance_name: Option<String>,
}

impl TryFrom<&Value> for EmbySourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "Emby")
    }
}

// Note: Default implementation removed as it requires RemoteProviderManager

#[async_trait]
impl MediaProvider for EmbyProvider {
    fn name(&self) -> &'static str {
        "emby"
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;

        // Validate host URL format
        if config.host.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby host must not be empty".to_string(),
            ));
        }
        if !config.host.starts_with("http://") && !config.host.starts_with("https://") {
            return Err(ProviderError::InvalidConfig(format!(
                "Emby host must start with http:// or https://, got: {}",
                config.host
            )));
        }

        // SSRF protection: reject private/internal network addresses
        validate_url_for_ssrf(&config.host).map_err(|e| match e {
            ValidationError::SSRF(msg) => {
                ProviderError::InvalidUrl(format!("SSRF protection: {msg}"))
            }
            _ => ProviderError::InvalidUrl(e.to_string()),
        })?;

        // Validate item_id is non-empty
        if config.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby item_id must not be empty".to_string(),
            ));
        }

        // Validate token (API key) is non-empty
        if config.token.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby token (API key) must not be empty".to_string(),
            ));
        }

        // Validate user_id is non-empty
        if config.user_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby user_id must not be empty".to_string(),
            ));
        }

        Ok(())
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        // Parse source_config first
        let config = EmbySourceConfig::try_from(source_config)?;

        // Get appropriate client based on instance_name from config
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Get item details first
        let item_request = synctv_media_providers::grpc::emby::GetItemReq {
            host: config.host.clone(),
            token: config.token.clone(),
            item_id: config.item_id.clone(),
            user_id: config.user_id.clone(),
        };

        let item = client.get_item(item_request).await?;

        let mut metadata = HashMap::new();
        metadata.insert("name".to_string(), json!(item.name));
        metadata.insert("type".to_string(), json!(item.r#type));
        if !item.series_name.is_empty() {
            metadata.insert("series_name".to_string(), json!(item.series_name));
        }
        if !item.season_name.is_empty() {
            metadata.insert("season_name".to_string(), json!(item.season_name));
        }

        // Get playback info
        let playback_request = synctv_media_providers::grpc::emby::PlaybackInfoReq {
            host: config.host.clone(),
            token: config.token.clone(),
            user_id: config.user_id.clone(),
            item_id: config.item_id.clone(),
            media_source_id: String::new(), // Use default media source
            audio_stream_index: 0,
            subtitle_stream_index: 0,
            max_streaming_bitrate: 0, // No limit
        };

        let playback_info = client.playback_info(playback_request).await?;

        // Store play_session_id in metadata for lifecycle hooks
        metadata.insert(
            "emby_play_session_id".to_string(),
            json!(playback_info.play_session_id),
        );

        let mut playback_infos = HashMap::new();

        // Emby session-based URLs: default to 30 minutes
        let emby_expires_at = Some(Utc::now().timestamp() + 30 * 60);

        // Auth headers for Emby: use X-Emby-Token header instead of
        // embedding api_key in query strings to avoid credential exposure
        // in URLs (which end up in logs, browser history, Referer headers).
        let emby_auth_headers = {
            let mut h = HashMap::new();
            h.insert("X-Emby-Token".to_string(), config.token.clone());
            h
        };

        // Process media sources
        for (idx, source) in playback_info.media_source_info.iter().enumerate() {
            let mode_name = if source.name.is_empty() {
                format!("source_{idx}")
            } else {
                source.name.clone()
            };

            // Get direct stream URL (no transcoding) -- no credentials in URL
            let direct_url = if !source.direct_play_url.is_empty() {
                format!(
                    "{}{}",
                    config.host.trim_end_matches('/'),
                    source.direct_play_url
                )
            } else if !source.path.is_empty() {
                format!(
                    "{}/Items/{}/Download",
                    config.host.trim_end_matches('/'),
                    config.item_id
                )
            } else {
                continue;
            };

            // Extract subtitles -- no credentials in subtitle URLs
            let subtitles: Vec<SubtitleTrack> = source
                .media_stream_info
                .iter()
                .filter(|stream| stream.r#type == "Subtitle")
                .map(|stream| {
                    let subtitle_url = format!(
                        "{}/Videos/{}/{}/Subtitles/{}/Stream.{}",
                        config.host.trim_end_matches('/'),
                        config.item_id,
                        source.id,
                        stream.index,
                        stream.codec.to_lowercase(),
                    );

                    SubtitleTrack {
                        language: stream.language.clone(),
                        name: stream.display_title.clone(),
                        url: subtitle_url,
                        format: stream.codec.to_lowercase(),
                    }
                })
                .collect();

            // Detect format from container
            let format = source.container.to_lowercase();
            let format = if format.contains("mp4") || format == "m4v" {
                "mp4"
            } else if format.contains("mkv") {
                "mkv"
            } else if format.contains("webm") {
                "webm"
            } else if format.contains("m3u8") || format == "hls" {
                "hls"
            } else {
                "video"
            }
            .to_string();

            playback_infos.insert(
                mode_name.clone(),
                PlaybackInfo {
                    urls: vec![direct_url],
                    format,
                    headers: emby_auth_headers.clone(),
                    subtitles,
                    expires_at: emby_expires_at,
                },
            );

            // Also add transcode URLs if available
            if !source.transcoding_url.is_empty() {
                let transcode_url = format!(
                    "{}{}",
                    config.host.trim_end_matches('/'),
                    source.transcoding_url
                );

                playback_infos.insert(
                    format!("{mode_name}_transcode"),
                    PlaybackInfo {
                        urls: vec![transcode_url],
                        format: "hls".to_string(), // Emby transcodes to HLS
                        headers: emby_auth_headers.clone(),
                        subtitles: Vec::new(), // Subtitles burned in for transcode
                        expires_at: emby_expires_at,
                    },
                );
            }
        }

        // Default to first media source
        let default_mode = playback_infos
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "direct".to_string());

        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            metadata,
            dash: None,
            hevc_dash: None,
        })
    }

    fn cache_key(&self, _ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        // Cache key must include token hash to prevent cross-user data leakage.
        // Different users have different tokens and may see different content.
        if let Ok(config) = EmbySourceConfig::try_from(source_config) {
            use sha2::{Sha256, Digest};
            let identifier = format!("{}:{}:{}", config.host, config.token, config.item_id);
            format!("emby:{:x}", Sha256::digest(identifier.as_bytes()))
        } else {
            "emby:unknown".to_string()
        }
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    async fn on_playback_start(
        &self,
        _ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        let req = synctv_media_providers::grpc::emby::ReportPlaybackStartReq {
            host: config.host,
            token: config.token,
            item_id: config.item_id,
            play_session_id: session_id.to_string(),
            media_source_id: String::new(),
            position_ticks: 0,
        };

        if let Err(e) = client.report_playback_start(req).await {
            tracing::warn!(error = %e, "Failed to report Emby playback start");
        }

        Ok(())
    }

    async fn on_playback_stop(
        &self,
        _ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &Value,
        position: f64,
    ) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = (position * 10_000_000.0) as i64;

        // Report playback stopped
        let stop_req = synctv_media_providers::grpc::emby::ReportPlaybackStopReq {
            host: config.host.clone(),
            token: config.token.clone(),
            item_id: config.item_id.clone(),
            play_session_id: session_id.to_string(),
            position_ticks,
        };

        if let Err(e) = client.report_playback_stop(stop_req).await {
            tracing::warn!(error = %e, "Failed to report Emby playback stop");
        }

        // Also clean up active encodings
        let delete_req = synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq {
            host: config.host,
            token: config.token,
            play_session_id: session_id.to_string(),
        };

        if let Err(e) = client.delete_active_encodings(delete_req).await {
            tracing::warn!(error = %e, "Failed to delete Emby active encodings");
        }

        Ok(())
    }

    async fn on_playback_progress(
        &self,
        _ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &Value,
        position: f64,
    ) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = (position * 10_000_000.0) as i64;

        let req = synctv_media_providers::grpc::emby::ReportPlaybackProgressReq {
            host: config.host,
            token: config.token,
            item_id: config.item_id,
            play_session_id: session_id.to_string(),
            media_source_id: String::new(),
            position_ticks,
            is_paused: false,
        };

        if let Err(e) = client.report_playback_progress(req).await {
            tracing::warn!(error = %e, "Failed to report Emby playback progress");
        }

        Ok(())
    }
}

#[async_trait]
impl DynamicFolder for EmbyProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        relative_path: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        // Parse base config from playlist.source_config
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config: EmbySourceConfig = serde_json::from_value(config.clone()).map_err(|e| {
            ProviderError::InvalidConfig(format!("Failed to parse Emby playlist config: {e}"))
        })?;

        // Determine path to list
        // If relative_path is provided, use it as the item_id to list that folder's contents
        // Otherwise, use the base config's item_id
        let target_path = relative_path
            .filter(|s| !s.is_empty() && *s != "/")
            .unwrap_or(&base_config.item_id);

        // Call fs_list to get items
        let client = self
            .get_client(base_config.provider_instance_name.as_deref())
            .await;

        let list_req = synctv_media_providers::grpc::emby::FsListReq {
            host: base_config.host.clone(),
            token: base_config.token.clone(),
            path: target_path.to_string(),
            start_index: (page * page_size) as u64,
            limit: page_size as u64,
            search_term: String::new(),
            user_id: base_config.user_id.clone(),
        };

        let response = client.fs_list(list_req).await?;

        // Convert Item to DirectoryItem
        let items: Vec<DirectoryItem> = response
            .items
            .into_iter()
            .filter_map(|item| {
                // Determine item type
                let item_type = if item.is_folder {
                    ItemType::Folder
                } else {
                    match item.r#type.as_str() {
                        "Movie" | "Episode" | "Video" => ItemType::Video,
                        "Audio" | "MusicAlbum" => ItemType::Audio,
                        _ => return None, // Skip other types
                    }
                };

                let thumbnail_url = format!(
                    "{}/emby/Items/{}/Images/Primary?maxHeight=300",
                    base_config.host.trim_end_matches('/'),
                    item.id,
                );

                Some(DirectoryItem {
                    name: item.name,
                    path: item.id,
                    item_type,
                    size: None,
                    thumbnail: Some(thumbnail_url),
                    modified_at: None,
                })
            })
            .collect();

        Ok(items)
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        _playing_media: &crate::models::Media,
        relative_path: &str,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        use crate::models::PlayMode;

        // Parse base playlist config
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config: EmbySourceConfig = serde_json::from_value(config.clone()).map_err(|e| {
            ProviderError::InvalidConfig(format!("Failed to parse Emby playlist config: {e}"))
        })?;

        match play_mode {
            PlayMode::RepeatOne => {
                // Repeat current: return None to signal player to replay current
                Ok(None)
            }
            PlayMode::Sequential | PlayMode::RepeatAll => {
                // Get directory listing
                // Extract parent path from relative_path (item_id in Emby)
                // For Emby, we need to query the parent folder
                // Since relative_path is the item_id, we need to get items from the base config's path
                let items = self
                    .list_playlist(_ctx, playlist, Some(&base_config.item_id), 0, 1000)
                    .await?;

                // Find current item index
                let current_idx = items.iter().position(|item| item.path == relative_path);

                if let Some(idx) = current_idx {
                    // Get next video/audio item
                    let next_item = items.iter().skip(idx + 1).find(|item| {
                        matches!(item.item_type, ItemType::Video | ItemType::Audio)
                    });

                    if let Some(next) = next_item {
                        // Construct source_config for next item
                        let source_config = json!({
                            "host": base_config.host,
                            "token": base_config.token,
                            "user_id": base_config.user_id,
                            "item_id": next.path,
                            "provider_instance_name": base_config.provider_instance_name,
                        });

                        return Ok(Some(NextPlayItem {
                            name: next.name.clone(),
                            item_type: next.item_type,
                            source_config,
                            metadata: json!({}),
                            provider_data: json!({}),
                            relative_path: next.path.clone(),
                        }.strip_credentials()));
                    } else if play_mode == PlayMode::RepeatAll {
                        // Wrap around to first video/audio
                        let first_item = items.iter().find(|item| {
                            matches!(item.item_type, ItemType::Video | ItemType::Audio)
                        });

                        if let Some(first) = first_item {
                            let source_config = json!({
                                "host": base_config.host,
                                "token": base_config.token,
                                "user_id": base_config.user_id,
                                "item_id": first.path,
                                "provider_instance_name": base_config.provider_instance_name,
                            });

                            return Ok(Some(NextPlayItem {
                                name: first.name.clone(),
                                item_type: first.item_type,
                                source_config,
                                metadata: json!({}),
                                provider_data: json!({}),
                                relative_path: first.path.clone(),
                            }.strip_credentials()));
                        }
                    }
                }

                // No next item found
                Ok(None)
            }
            PlayMode::Shuffle => {
                // Get all video/audio items and pick random
                let items = self
                    .list_playlist(_ctx, playlist, Some(&base_config.item_id), 0, 1000)
                    .await?;

                let playable_items: Vec<_> = items
                    .iter()
                    .filter(|item| matches!(item.item_type, ItemType::Video | ItemType::Audio))
                    .collect();

                if playable_items.is_empty() {
                    return Ok(None);
                }

                // Pick random item (excluding current)
                let mut rng = rand::rng();
                let candidates: Vec<_> = playable_items
                    .iter()
                    .filter(|item| item.path != relative_path)
                    .collect();

                let random_item = if candidates.is_empty() {
                    // Only one item, pick it
                    playable_items.choose(&mut rng).copied()
                } else {
                    candidates.choose(&mut rng).copied().copied()
                };

                if let Some(random) = random_item {
                    let source_config = json!({
                        "host": base_config.host,
                        "token": base_config.token,
                        "user_id": base_config.user_id,
                        "item_id": random.path,
                        "provider_instance_name": base_config.provider_instance_name,
                    });

                    Ok(Some(NextPlayItem {
                        name: random.name.clone(),
                        item_type: random.item_type,
                        source_config,
                        metadata: json!({}),
                        provider_data: json!({}),
                        relative_path: random.path.clone(),
                    }.strip_credentials()))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_emby(config: Value) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(&config)?;

        if config.host.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby host must not be empty".to_string(),
            ));
        }
        if !config.host.starts_with("http://") && !config.host.starts_with("https://") {
            return Err(ProviderError::InvalidConfig(format!(
                "Emby host must start with http:// or https://, got: {}",
                config.host
            )));
        }
        if config.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby item_id must not be empty".to_string(),
            ));
        }
        if config.token.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby token (API key) must not be empty".to_string(),
            ));
        }
        if config.user_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby user_id must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_valid_emby_config() {
        let config = json!({
            "host": "https://emby.example.com",
            "token": "my-api-key",
            "user_id": "abc123",
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_ok());
    }

    #[test]
    fn test_emby_config_empty_host() {
        let config = json!({
            "host": "",
            "token": "my-api-key",
            "user_id": "abc123",
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_invalid_host_scheme() {
        let config = json!({
            "host": "ftp://emby.example.com",
            "token": "my-api-key",
            "user_id": "abc123",
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_empty_item_id() {
        let config = json!({
            "host": "https://emby.example.com",
            "token": "my-api-key",
            "user_id": "abc123",
            "item_id": ""
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_empty_token() {
        let config = json!({
            "host": "https://emby.example.com",
            "token": "",
            "user_id": "abc123",
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_empty_user_id() {
        let config = json!({
            "host": "https://emby.example.com",
            "token": "my-api-key",
            "user_id": "",
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_missing_required_fields() {
        let config = json!({
            "host": "https://emby.example.com"
        });
        assert!(validate_emby(config).is_err());
    }
}
