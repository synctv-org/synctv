//! Alist `MediaProvider` Adapter
//!
//! Adapter that calls `AlistProviderClient` to implement `MediaProvider` trait.
//! `ProviderClient` abstracts local/remote implementation, so `MediaProvider` doesn't need to know.

use super::{
    provider_client::{
        create_remote_alist_client, load_local_alist_client, AlistClientArc, AlistClientExt,
        AlistFileInfo,
    },
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
    DynamicFolder, DirectoryItem, ItemType, NextPlayItem,
};
use crate::service::RemoteProviderManager;
use crate::validation::{validate_url_for_ssrf, ValidationError};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Alist `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct AlistProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
}

impl AlistProvider {
    /// Create a new `AlistProvider` with `RemoteProviderManager`
    #[must_use] 
    pub const fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
        }
    }

    /// Get Alist client for the given instance name (remote if available, local fallback)
    async fn get_client(&self, instance_name: Option<&str>) -> AlistClientArc {
        self.provider_instance_manager
            .resolve_client(instance_name, create_remote_alist_client, load_local_alist_client)
            .await
    }

    /// Detect file format from extension
    fn detect_format(filename: &str) -> String {
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "mp4" | "m4v" | "mov" => "mp4",
            "mkv" => "mkv",
            "avi" => "avi",
            "flv" => "flv",
            "webm" => "webm",
            "m3u8" => "hls",
            _ => "video",
        }
        .to_string()
    }

    // ========== Provider API Methods ==========

    /// Login to Alist
    ///
    /// Takes grpc-generated `LoginReq` and returns token string
    pub async fn login(
        &self,
        req: synctv_media_providers::grpc::alist::LoginReq,
        instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.login(req).await.map_err(std::convert::Into::into)
    }

    /// List Alist directory
    ///
    /// Takes grpc-generated `FsListReq` and returns `FsListResp`
    pub async fn fs_list(
        &self,
        req: synctv_media_providers::grpc::alist::FsListReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::alist::FsListResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.fs_list(req).await.map_err(std::convert::Into::into)
    }

    /// Get Alist user info
    ///
    /// Takes grpc-generated `MeReq` and returns `MeResp`
    pub async fn me(
        &self,
        req: synctv_media_providers::grpc::alist::MeReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::alist::MeResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.me(req).await.map_err(std::convert::Into::into)
    }
}

/// Alist source configuration
#[derive(Debug, Deserialize, Serialize)]
struct AlistSourceConfig {
    host: String,
    token: String,
    path: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    provider_instance_name: Option<String>,
}

impl TryFrom<&Value> for AlistSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "Alist")
    }
}

// Note: Default implementation removed as it requires RemoteProviderManager

#[async_trait]
impl MediaProvider for AlistProvider {
    fn name(&self) -> &'static str {
        "alist"
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;

        // Validate host URL format
        if config.host.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist host must not be empty".to_string(),
            ));
        }
        if !config.host.starts_with("http://") && !config.host.starts_with("https://") {
            return Err(ProviderError::InvalidConfig(format!(
                "Alist host must start with http:// or https://, got: {}",
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

        // Validate path is not empty and doesn't contain path traversal
        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        if config.path.contains("..") {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not contain path traversal (..)".to_string(),
            ));
        }

        // Validate token is non-empty
        if config.token.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist token must not be empty".to_string(),
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
        let config = AlistSourceConfig::try_from(source_config)?;

        // Re-validate host URL at request time to protect against DNS rebinding.
        // The hostname may have been safe at config time but could resolve to a
        // private IP now.
        validate_url_for_ssrf(&config.host).map_err(|e| match e {
            ValidationError::SSRF(msg) => {
                ProviderError::InvalidUrl(format!("SSRF protection: {msg}"))
            }
            _ => ProviderError::InvalidUrl(e.to_string()),
        })?;

        // Get appropriate client based on instance_name from config
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Build proto request
        let request = synctv_media_providers::grpc::alist::FsGetReq {
            host: config.host.clone(),
            token: config.token.clone(),
            path: config.path.clone(),
            password: config.password.clone().unwrap_or_default(),
            user_agent: String::new(),
        };

        // Call client (trait method - implementation handles local/remote)
        let fs_get_data = client.fs_get(request).await?;

        let file_info: AlistFileInfo = fs_get_data.into();

        if file_info.is_dir {
            return Err(ProviderError::UnsupportedFormat(
                "Cannot play directory, use browse() instead".to_string(),
            ));
        }

        let mut playback_infos = HashMap::new();
        let mut metadata = HashMap::new();

        // Add basic metadata
        metadata.insert("name".to_string(), json!(file_info.name));
        metadata.insert("size".to_string(), json!(file_info.size));
        metadata.insert("provider".to_string(), json!(file_info.provider));
        if !file_info.thumb.is_empty() {
            metadata.insert("thumbnail".to_string(), json!(file_info.thumb));
        }

        // Try to get video preview info for transcoded URLs (optional)
        let has_video_preview = if let Some(preview) = client
            .get_video_preview(
                &config.host,
                &config.token,
                &config.path,
                config.password.as_deref(),
            )
            .await?
        {
            // Add transcoding quality options
            for (idx, task) in preview.transcoding_tasks.iter().enumerate() {
                if !task.url.is_empty() {
                    let quality_name = if task.template_name.is_empty() {
                        format!("quality_{idx}")
                    } else {
                        task.template_name.clone()
                    };

                    // Alist transcoded URLs typically valid for ~15 minutes
                    let task_expires_at = Some(Utc::now().timestamp() + 15 * 60);

                    playback_infos.insert(
                        format!("transcoded_{quality_name}"),
                        PlaybackInfo {
                            urls: vec![task.url.clone()],
                            format: "hls".to_string(),
                            headers: HashMap::new(),
                            subtitles: preview
                                .subtitle_tasks
                                .iter()
                                .map(|sub| SubtitleTrack {
                                    language: sub.language.clone(),
                                    name: sub.language.clone(),
                                    url: sub.url.clone(),
                                    format: "srt".to_string(),
                                })
                                .collect(),
                            expires_at: task_expires_at,
                            cors_proxy_required: false,
                        },
                    );
                }
            }

            // Add video metadata
            metadata.insert("duration".to_string(), json!(preview.duration));
            metadata.insert("width".to_string(), json!(preview.width));
            metadata.insert("height".to_string(), json!(preview.height));

            true
        } else {
            false
        };

        // Always add direct URL (raw_url) as fallback
        if !file_info.raw_url.is_empty() {
            // Alist direct URLs typically valid for ~15 minutes
            let direct_expires_at = Some(Utc::now().timestamp() + 15 * 60);

            playback_infos.insert(
                "direct".to_string(),
                PlaybackInfo {
                    urls: vec![file_info.raw_url.clone()],
                    format: Self::detect_format(&file_info.name),
                    headers: HashMap::new(),
                    subtitles: Vec::new(),
                    expires_at: direct_expires_at,
                    cors_proxy_required: false,
                },
            );
        }

        // Determine default mode
        let default_mode = if has_video_preview && !playback_infos.is_empty() {
            playback_infos
                .keys()
                .find(|k| k.starts_with("transcoded_"))
                .cloned()
                .unwrap_or_else(|| "direct".to_string())
        } else {
            "direct".to_string()
        };

        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            metadata,
        })
    }

    fn cache_key(&self, ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        // Cache key must include token hash to prevent cross-user data leakage.
        // Different users have different tokens and may see different files.
        if let Ok(config) = AlistSourceConfig::try_from(source_config) {
            use sha2::{Sha256, Digest};
            let identifier = format!("{}:{}:{}", config.host, config.token, config.path);
            format!("{}:playback:alist:{:x}", ctx.key_prefix, Sha256::digest(identifier.as_bytes()))
        } else {
            format!("{}:playback:alist:unknown", ctx.key_prefix)
        }
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }
}

/// Implement `DynamicFolder` trait for Alist
///
/// Allows browsing Alist directories and getting next item for auto-play
#[async_trait]
impl DynamicFolder for AlistProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        relative_path: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        // Parse playlist's source_config to get base path
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;

        let base_config = AlistSourceConfig::try_from(config)?;

        // Validate relative_path to prevent path traversal attacks
        if let Some(rel) = relative_path {
            if rel.contains("..") || rel.contains('\0') {
                return Err(ProviderError::InvalidConfig(
                    "Relative path must not contain path traversal (..) or null bytes".to_string(),
                ));
            }
        }

        // Construct full path: base_path + relative_path
        let full_path = if let Some(rel) = relative_path {
            if rel.starts_with('/') {
                format!("{}{}", base_config.path.trim_end_matches('/'), rel)
            } else {
                format!("{}/{}", base_config.path.trim_end_matches('/'), rel)
            }
        } else {
            base_config.path.clone()
        };

        // Get appropriate client
        let client = self
            .get_client(base_config.provider_instance_name.as_deref())
            .await;

        // Build list request
        let list_req = synctv_media_providers::grpc::alist::FsListReq {
            host: base_config.host.clone(),
            token: base_config.token.clone(),
            path: full_path.clone(),
            password: base_config.password.clone().unwrap_or_default(),
            page: page as u64,
            per_page: page_size as u64,
            refresh: false,
        };

        // Call fs_list
        let list_resp = client.fs_list(list_req).await?;

        // Convert to DirectoryItem list
        let items: Vec<DirectoryItem> = list_resp
            .content
            .into_iter()
            .filter_map(|file_item| {
                // Determine item type
                let item_type = if file_item.is_dir {
                    ItemType::Playlist
                } else {
                    // Check if it's a media file (video or audio)
                    let ext = file_item
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_lowercase();
                    match ext.as_str() {
                        "mp4" | "mkv" | "avi" | "mov" | "flv" | "webm" | "m4v" | "wmv" | "m3u8" |
                        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" => ItemType::Media,
                        _ => return None, // Skip non-media files
                    }
                };

                // Construct relative path for this item
                let item_relative_path = if let Some(rel) = relative_path {
                    format!("{}/{}", rel.trim_end_matches('/'), file_item.name)
                } else {
                    format!("/{}", file_item.name)
                };

                Some(DirectoryItem {
                    name: file_item.name,
                    item_type,
                    path: item_relative_path,
                    size: Some(file_item.size),
                    thumbnail: if file_item.thumb.is_empty() {
                        None
                    } else {
                        Some(file_item.thumb)
                    },
                    modified_at: Some(file_item.modified as i64),
                })
            })
            .collect();

        Ok(items)
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        _playing_media: &crate::models::Media,
        relative_path: &str,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        use crate::models::PlayMode;

        // Validate relative_path to prevent path traversal attacks
        if relative_path.contains("..") || relative_path.contains('\0') {
            return Err(ProviderError::InvalidConfig(
                "Relative path must not contain path traversal (..) or null bytes".to_string(),
            ));
        }

        match play_mode {
            PlayMode::RepeatOne => {
                // Repeat current: return None to signal player to replay current
                Ok(None)
            }
            PlayMode::Sequential | PlayMode::RepeatAll => {
                // Get directory listing with pagination instead of fetching all at once.
                // Use a reasonable page size and iterate pages to find the current and next items.
                // Cap at MAX_PAGES to prevent memory exhaustion on huge folders.
                let parent_path = relative_path.rsplit_once('/').map(|x| x.0)
                    .and_then(|s| if s.is_empty() { None } else { Some(s) });

                const PAGE_SIZE: usize = 50;
                const MAX_PAGES: usize = 20;
                let mut all_items = Vec::new();
                let mut page = 0;
                loop {
                    let page_items = self
                        .list_playlist(ctx, playlist, parent_path, page, PAGE_SIZE)
                        .await?;
                    let is_last_page = page_items.len() < PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page {
                        break;
                    }
                    page += 1;
                    if page >= MAX_PAGES {
                        tracing::warn!(
                            "Alist DynamicFolder next(): hit MAX_PAGES limit ({MAX_PAGES}), \
                             folder may have more items"
                        );
                        break;
                    }
                }
                let items = all_items;

                // Find current item index
                let current_idx = items
                    .iter()
                    .position(|item| item.path == relative_path);

                if let Some(idx) = current_idx {
                    // Get next video item
                    let next_item = items
                        .iter()
                        .skip(idx + 1)
                        .find(|item| item.item_type == ItemType::Media);

                    if let Some(next) = next_item {
                        // Parse base config to construct source_config
                        let config = playlist
                            .source_config
                            .as_ref()
                            .ok_or_else(|| {
                                ProviderError::InvalidConfig("Missing source_config".to_string())
                            })?;
                        let base_config = AlistSourceConfig::try_from(config)?;

                        // Construct full path for next item
                        let full_path = format!(
                            "{}{}",
                            base_config.path.trim_end_matches('/'),
                            next.path
                        );

                        let source_config = json!({
                            "host": base_config.host,
                            "token": base_config.token,
                            "path": full_path,
                            "password": base_config.password,
                            "provider_instance_name": base_config.provider_instance_name,
                        });

                        return Ok(Some(NextPlayItem {
                            name: next.name.clone(),
                            item_type: next.item_type,
                            source_config,
                            metadata: json!({
                                "size": next.size,
                                "thumbnail": next.thumbnail,
                                "modified_at": next.modified_at,
                            }),
                            provider_data: json!({}),
                            relative_path: next.path.clone(),
                        }.strip_credentials()));
                    } else if play_mode == PlayMode::RepeatAll {
                        // Wrap around to first video
                        let first_video = items
                            .iter()
                            .find(|item| item.item_type == ItemType::Media);

                        if let Some(first) = first_video {
                            let config = playlist.source_config.as_ref().ok_or_else(|| {
                                ProviderError::InvalidConfig("Missing source_config".to_string())
                            })?;
                            let base_config = AlistSourceConfig::try_from(config)?;

                            let full_path = format!(
                                "{}{}",
                                base_config.path.trim_end_matches('/'),
                                first.path
                            );

                            let source_config = json!({
                                "host": base_config.host,
                                "token": base_config.token,
                                "path": full_path,
                                "password": base_config.password,
                                "provider_instance_name": base_config.provider_instance_name,
                            });

                            return Ok(Some(NextPlayItem {
                                name: first.name.clone(),
                                item_type: first.item_type,
                                source_config,
                                metadata: json!({
                                    "size": first.size,
                                    "thumbnail": first.thumbnail,
                                    "modified_at": first.modified_at,
                                }),
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
                // Get all video items and pick random, using paginated fetching.
                // Cap at MAX_PAGES to prevent memory exhaustion.
                let parent_path = relative_path.rsplit_once('/').map(|x| x.0)
                    .and_then(|s| if s.is_empty() { None } else { Some(s) });

                const PAGE_SIZE: usize = 50;
                const MAX_PAGES: usize = 20;
                let mut all_items = Vec::new();
                let mut page = 0;
                loop {
                    let page_items = self
                        .list_playlist(ctx, playlist, parent_path, page, PAGE_SIZE)
                        .await?;
                    let is_last_page = page_items.len() < PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page {
                        break;
                    }
                    page += 1;
                    if page >= MAX_PAGES {
                        tracing::warn!(
                            "Alist DynamicFolder next() shuffle: hit MAX_PAGES limit ({MAX_PAGES})"
                        );
                        break;
                    }
                }
                let items = all_items;

                let videos: Vec<_> = items
                    .iter()
                    .filter(|item| item.item_type == ItemType::Media)
                    .collect();

                if videos.is_empty() {
                    return Ok(None);
                }

                let random_idx = rand::random_range(0..videos.len());
                let random_item = videos[random_idx];

                let config = playlist
                    .source_config
                    .as_ref()
                    .ok_or_else(|| {
                        ProviderError::InvalidConfig("Missing source_config".to_string())
                    })?;
                let base_config = AlistSourceConfig::try_from(config)?;

                let full_path = format!(
                    "{}{}",
                    base_config.path.trim_end_matches('/'),
                    random_item.path
                );

                let source_config = json!({
                    "host": base_config.host,
                    "token": base_config.token,
                    "path": full_path,
                    "password": base_config.password,
                    "provider_instance_name": base_config.provider_instance_name,
                });

                Ok(Some(NextPlayItem {
                    name: random_item.name.clone(),
                    item_type: random_item.item_type,
                    source_config,
                    metadata: json!({
                        "size": random_item.size,
                        "thumbnail": random_item.thumbnail,
                        "modified_at": random_item.modified_at,
                    }),
                    provider_data: json!({}),
                    relative_path: random_item.path.clone(),
                }.strip_credentials()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Provider creation tests removed as they require ProviderClient setup

    #[test]
    fn test_detect_format() {
        assert_eq!(AlistProvider::detect_format("video.mp4"), "mp4");
        assert_eq!(AlistProvider::detect_format("video.mkv"), "mkv");
        assert_eq!(AlistProvider::detect_format("video.m3u8"), "hls");
        assert_eq!(AlistProvider::detect_format("video.unknown"), "video");
    }

    fn validate_alist(config: Value) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(&config)?;

        if config.host.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist host must not be empty".to_string(),
            ));
        }
        if !config.host.starts_with("http://") && !config.host.starts_with("https://") {
            return Err(ProviderError::InvalidConfig(format!(
                "Alist host must start with http:// or https://, got: {}",
                config.host
            )));
        }
        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        if config.path.contains("..") {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not contain path traversal (..)".to_string(),
            ));
        }
        if config.token.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist token must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_valid_alist_config() {
        let config = json!({
            "host": "https://alist.example.com",
            "token": "my-token",
            "path": "/media/movies/test.mp4"
        });
        assert!(validate_alist(config).is_ok());
    }

    #[test]
    fn test_alist_config_missing_host() {
        let config = json!({
            "host": "",
            "token": "my-token",
            "path": "/media/movies"
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_invalid_host_scheme() {
        let config = json!({
            "host": "ftp://alist.example.com",
            "token": "my-token",
            "path": "/media/movies"
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_path_traversal() {
        let config = json!({
            "host": "https://alist.example.com",
            "token": "my-token",
            "path": "/media/../../../etc/passwd"
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_empty_token() {
        let config = json!({
            "host": "https://alist.example.com",
            "token": "",
            "path": "/media/movies"
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_empty_path() {
        let config = json!({
            "host": "https://alist.example.com",
            "token": "my-token",
            "path": ""
        });
        assert!(validate_alist(config).is_err());
    }
}
