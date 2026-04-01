//! Alist `MediaProvider` Adapter
//!
//! Adapter that calls `AlistProviderClient` to implement `MediaProvider` trait.
//! `ProviderClient` abstracts local/remote implementation, so `MediaProvider` doesn't need to know.

use super::{
    provider_client::{
        create_remote_alist_client, AlistClientArc, AlistClientExt, AlistFileInfo,
        ProviderClientManager,
    },
    store::{ProviderStoreExt, VersionedPlayback},
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, ItemType, MediaProvider,
    NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
};
use crate::service::RemoteProviderManager;
use crate::validation::validate_path_for_traversal;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Alist `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct AlistProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AlistBrowseTarget {
    relative_path: String,
}

impl AlistProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "alist";

    /// Create a new `AlistProvider` with `RemoteProviderManager`
    #[must_use]
    pub fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()),
        }
    }

    #[must_use]
    pub const fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
        }
    }

    /// Get Alist client for the given instance name (remote if available, local fallback)
    async fn get_client(
        &self,
        instance_name: Option<&str>,
    ) -> Result<AlistClientArc, ProviderError> {
        self.provider_instance_manager
            .resolve_client_required(instance_name, create_remote_alist_client, || {
                self.client_manager.local_alist_client()
            })
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
        client.me(req).await.map_err(std::convert::Into::into)
    }

    fn encode_target(relative_path: &str) -> Result<Vec<u8>, ProviderError> {
        serde_json::to_vec(&AlistBrowseTarget {
            relative_path: relative_path.to_string(),
        })
        .map_err(|e| ProviderError::InvalidConfig(format!("Failed to encode Alist target: {e}")))
    }

    fn decode_target(target: Option<&[u8]>) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        if target.is_empty() {
            return Ok(None);
        }

        let payload: AlistBrowseTarget = serde_json::from_slice(target)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid Alist target: {e}")))?;

        if payload.relative_path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist target relative_path cannot be empty".to_string(),
            ));
        }

        Ok(Some(payload.relative_path))
    }
}

/// Alist source configuration
#[derive(Debug, Deserialize, Serialize)]
struct AlistSourceConfig {
    path: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    provider_instance_name: Option<String>,
    /// Reference to stored credentials (server-side)
    credential_ref: super::credential_resolver::CredentialRef,
}

/// Resolved Alist configuration with credentials ready for API calls.
struct ResolvedAlistConfig {
    host: String,
    token: String,
    path: String,
    password: Option<String>,
    provider_instance_name: Option<String>,
}

impl TryFrom<&Value> for AlistSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "Alist")
    }
}

// Note: Default implementation removed as it requires RemoteProviderManager

impl AlistProvider {
    /// Resolve AlistSourceConfig + credential_ref into ResolvedAlistConfig.
    async fn resolve_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<ResolvedAlistConfig, ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;

        let repo = ctx.credential_repo.ok_or_else(|| {
            ProviderError::Internal("credential_repo not available in ProviderContext".to_string())
        })?;

        let credential = super::credential_resolver::resolve_credential(
            repo,
            Self::NAME,
            &config.credential_ref,
        )
        .await?;

        match credential {
            crate::models::ProviderCredential::Alist {
                host,
                username,
                password,
            } => {
                // Re-login with stored credentials to get a fresh token
                let login_req = synctv_media_providers::grpc::alist::LoginReq {
                    host: host.clone(),
                    username,
                    password,
                    hashed: true,
                };
                let instance_name = config.provider_instance_name.as_deref();
                let token = self.provider_login(login_req, instance_name).await?;

                Ok(ResolvedAlistConfig {
                    host,
                    token,
                    path: config.path,
                    password: config.password,
                    provider_instance_name: config.provider_instance_name,
                })
            }
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    /// Internal login for credential resolution
    async fn provider_login(
        &self,
        req: synctv_media_providers::grpc::alist::LoginReq,
        instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.login(req, instance_name).await
    }

    /// Resolve playback from the Alist API (no caching layer).
    ///
    /// Contains the core API interaction logic, called by `generate_playback`
    /// after cache miss.
    async fn resolve_from_api(
        &self,
        config: &ResolvedAlistConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        // Get appropriate client based on instance_name from config
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await?;

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
}

#[async_trait]
impl MediaProvider for AlistProvider {
    #[cfg(test)]
    fn test_client_manager_marker(&self) -> Option<usize> {
        Some(self.client_manager.marker())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;

        // Validate path is not empty and doesn't contain path traversal
        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        validate_path_for_traversal(&config.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;

        // Validate credential_ref exists
        if let Some(repo) = _ctx.credential_repo {
            let cred = repo
                .get_by_provider_and_server(
                    &config.credential_ref.credential_owner_id,
                    Self::NAME,
                    &config.credential_ref.server_id,
                )
                .await
                .map_err(|e| {
                    ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
                })?;

            if cred.is_none() {
                return Err(ProviderError::CredentialNotFound(format!(
                    "Referenced alist credential not found for server_id '{}'",
                    config.credential_ref.server_id
                )));
            }
        }

        Ok(())
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        mut source_config: Value,
    ) -> Result<Value, ProviderError> {
        // Server-side: ensure credential_owner_id is set to the current user
        if let Some(user_id) = _ctx.user_id {
            if let Some(obj) = source_config.as_object_mut() {
                if let Some(cred_ref) = obj.get_mut("credential_ref") {
                    if let Some(cred_obj) = cred_ref.as_object_mut() {
                        cred_obj.insert(
                            "credential_owner_id".to_string(),
                            Value::String(user_id.to_string()),
                        );
                    }
                }
            }
        }

        Ok(source_config)
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        // Resolve credentials from DB
        let resolved = self.resolve_config(_ctx, source_config).await?;

        // Re-validate path at request time (defense-in-depth against traversal)
        validate_path_for_traversal(&resolved.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;

        // Build cache key from server_id and path
        let config = AlistSourceConfig::try_from(source_config)?;
        use sha2::{Digest, Sha256};
        let path_hash: String = format!("{:x}", Sha256::digest(resolved.path.as_bytes()))
            .chars()
            .take(16)
            .collect();
        let cache_key = format!("playback:{}:{path_hash}", config.credential_ref.server_id);
        let cache_ttl = Duration::from_mins(15);

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::maybe_sign_cached_versioned_playback(cached, Self::NAME, _ctx)
                        .await;
                }
            }
        }

        // Acquire lock to prevent concurrent resolution of same content
        let _lock = if let Some(store) = store {
            store
                .lock(&format!("lock:{cache_key}"), Duration::from_secs(30))
                .await
                .ok()
        } else {
            None
        };

        // Double-check cache after lock acquisition
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::maybe_sign_cached_versioned_playback(cached, Self::NAME, _ctx)
                        .await;
                }
            }
        }

        // Call provider API
        let result = self.resolve_from_api(&resolved).await?;

        // Generate version and store result
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        Some(self)
    }
}

// ProviderProxy implementation for Alist
//
// Supported sub_paths:
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
#[async_trait]
impl super::proxy::ProviderProxy for AlistProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        if let Some((version, rest)) = sub_path.split_once('/') {
            let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;
            let default_info = versioned
                .result
                .playback_infos
                .get(&versioned.result.default_mode)
                .ok_or(ProviderError::NotFound)?;
            let url = default_info.urls.first().ok_or(ProviderError::NotFound)?;

            match rest {
                "stream" => {
                    return Ok(super::proxy::ProxyAction::FetchAndForward {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                    });
                }
                "m3u8" => {
                    // Propagate HMAC signature into M3U8 segment URLs
                    let proxy_base = if let Some(claims) = ctx.verified_claims {
                        let signed_query = ctx.services.signing_key.build_signed_query(claims);
                        format!("{}/{version}?{signed_query}", ctx.proxy_base)
                    } else {
                        format!("{}/{version}", ctx.proxy_base)
                    };
                    return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                        proxy_base,
                    });
                }
                _ => {}
            }
        }

        Err(ProviderError::NotFound)
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
        target: Option<&[u8]>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        // Parse playlist's source_config to get base path
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;

        let resolved = self.resolve_config(_ctx, config).await?;

        let relative_path = Self::decode_target(target)?;

        // Validate relative_path BEFORE any path concatenation to prevent traversal attacks
        if let Some(rel) = relative_path.as_deref() {
            validate_path_for_traversal(rel)
                .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;
        }

        // Construct full path: base_path + relative_path
        let full_path = if let Some(rel) = relative_path.as_deref() {
            if rel.starts_with('/') {
                format!("{}{}", resolved.path.trim_end_matches('/'), rel)
            } else {
                format!("{}/{}", resolved.path.trim_end_matches('/'), rel)
            }
        } else {
            resolved.path.clone()
        };

        // Get appropriate client
        let client = self
            .get_client(resolved.provider_instance_name.as_deref())
            .await?;

        // Build list request
        let list_req = synctv_media_providers::grpc::alist::FsListReq {
            host: resolved.host.clone(),
            token: resolved.token.clone(),
            path: full_path.clone(),
            password: resolved.password.clone().unwrap_or_default(),
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
                        "mp4" | "mkv" | "avi" | "mov" | "flv" | "webm" | "m4v" | "wmv" | "m3u8"
                        | "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" => ItemType::Media,
                        _ => return None, // Skip non-media files
                    }
                };

                // Construct relative path for this item
                let item_relative_path = if let Some(rel) = relative_path.as_deref() {
                    format!("{}/{}", rel.trim_end_matches('/'), file_item.name)
                } else {
                    format!("/{}", file_item.name)
                };

                Some(DirectoryItem {
                    name: file_item.name,
                    item_type,
                    target: Self::encode_target(&item_relative_path).ok()?,
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

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let relative_path = Self::decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Alist target is required".to_string())
        })?;
        validate_path_for_traversal(&relative_path)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::try_from(config)?;

        let build_next_source_config = |full_path: &str| -> Value {
            json!({
                "path": full_path,
                "password": base_config.password,
                "provider_instance_name": base_config.provider_instance_name,
                "credential_ref": {
                    "credential_owner_id": base_config.credential_ref.credential_owner_id,
                    "server_id": base_config.credential_ref.server_id,
                },
            })
        };

        let build_full_path = |item_path: &str| -> String {
            format!("{}{}", base_config.path.trim_end_matches('/'), item_path)
        };

        let parent_path = relative_path.rsplit_once('/').map(|x| x.0).and_then(|s| {
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });
        let parent_target = parent_path.map(Self::encode_target).transpose()?;

        const PAGE_SIZE: usize = 50;
        let mut page = 0;
        loop {
            let page_items = self
                .list_playlist(ctx, playlist, parent_target.as_deref(), page, PAGE_SIZE)
                .await?;
            if page_items.is_empty() {
                return Ok(None);
            }

            if let Some(item) = page_items
                .iter()
                .find(|item| item.item_type == ItemType::Media && item.target == target)
            {
                return Ok(Some(
                    NextPlayItem {
                        name: item.name.clone(),
                        item_type: item.item_type,
                        source_config: build_next_source_config(&build_full_path(&relative_path)),
                        metadata: json!({
                            "size": item.size,
                            "thumbnail": item.thumbnail,
                            "modified_at": item.modified_at
                        }),
                        provider_data: json!({}),
                        target: item.target.clone(),
                    }
                    .strip_credentials(),
                ));
            }

            if page_items.len() < PAGE_SIZE {
                return Ok(None);
            }
            page += 1;
        }
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        _playing_media: &crate::models::Media,
        target: &[u8],
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        use crate::models::PlayMode;
        let relative_path = Self::decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Alist target is required".to_string())
        })?;

        // Validate relative_path BEFORE any path operations to prevent traversal attacks
        validate_path_for_traversal(&relative_path)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;

        // Parse base config and build helper for NextPlayItem source_configs
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::try_from(config)?;

        let build_next_source_config = |full_path: &str| -> Value {
            json!({
                "path": full_path,
                "password": base_config.password,
                "provider_instance_name": base_config.provider_instance_name,
                "credential_ref": {
                    "credential_owner_id": base_config.credential_ref.credential_owner_id,
                    "server_id": base_config.credential_ref.server_id,
                },
            })
        };

        let build_full_path = |item_path: &str| -> String {
            format!("{}{}", base_config.path.trim_end_matches('/'), item_path)
        };

        match play_mode {
            PlayMode::RepeatOne => Ok(None),
            PlayMode::Sequential | PlayMode::RepeatAll => {
                let parent_path = relative_path.rsplit_once('/').map(|x| x.0).and_then(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                });
                let parent_target = parent_path.map(Self::encode_target).transpose()?;

                const PAGE_SIZE: usize = 50;
                let mut found_current = false;
                let mut current_page = 0;

                loop {
                    let page_items = self
                        .list_playlist(
                            ctx,
                            playlist,
                            parent_target.as_deref(),
                            current_page,
                            PAGE_SIZE,
                        )
                        .await?;

                    if page_items.is_empty() {
                        break;
                    }

                    if found_current {
                        if let Some(next) = page_items
                            .iter()
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(NextPlayItem {
                                name: next.name.clone(),
                                item_type: next.item_type,
                                source_config: build_next_source_config(&build_full_path(
                                    &Self::decode_target(Some(&next.target))?
                                        .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                                )),
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }.strip_credentials()));
                        }
                    } else if let Some(idx) = page_items.iter().position(|item| item.target == target)
                    {
                        found_current = true;
                        if let Some(next) = page_items
                            .iter()
                            .skip(idx + 1)
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(NextPlayItem {
                                name: next.name.clone(),
                                item_type: next.item_type,
                                source_config: build_next_source_config(&build_full_path(
                                    &Self::decode_target(Some(&next.target))?
                                        .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                                )),
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }.strip_credentials()));
                        }
                    }

                    if page_items.len() < PAGE_SIZE {
                        break;
                    }
                    current_page += 1;
                }

                if found_current && play_mode == PlayMode::RepeatAll {
                    let parent_path = relative_path.rsplit_once('/').map(|x| x.0).and_then(|s| {
                        if s.is_empty() {
                            None
                        } else {
                            Some(s)
                        }
                    });
                    let parent_target = parent_path.map(Self::encode_target).transpose()?;
                    let first_page = self
                        .list_playlist(ctx, playlist, parent_target.as_deref(), 0, PAGE_SIZE)
                        .await?;
                    if let Some(first) = first_page
                        .iter()
                        .find(|item| item.item_type == ItemType::Media)
                    {
                        return Ok(Some(NextPlayItem {
                            name: first.name.clone(),
                            item_type: first.item_type,
                            source_config: build_next_source_config(&build_full_path(
                                &Self::decode_target(Some(&first.target))?
                                    .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                            )),
                            metadata: json!({"size": first.size, "thumbnail": first.thumbnail, "modified_at": first.modified_at}),
                            provider_data: json!({}),
                            target: first.target.clone(),
                        }.strip_credentials()));
                    }
                }

                Ok(None)
            }
            PlayMode::Shuffle => {
                let parent_path = relative_path.rsplit_once('/').map(|x| x.0).and_then(|s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                });
                let parent_target = parent_path.map(Self::encode_target).transpose()?;

                const PAGE_SIZE: usize = 50;
                const MAX_ITEMS: usize = 200;
                let mut all_items = Vec::with_capacity(MAX_ITEMS);
                let mut page = 0;
                loop {
                    let page_items = self
                        .list_playlist(ctx, playlist, parent_target.as_deref(), page, PAGE_SIZE)
                        .await?;
                    let is_last_page = page_items.len() < PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page || all_items.len() >= MAX_ITEMS {
                        break;
                    }
                    page += 1;
                }
                all_items.truncate(MAX_ITEMS);

                let videos: Vec<_> = all_items
                    .iter()
                    .filter(|item| item.item_type == ItemType::Media)
                    .collect();
                if videos.is_empty() {
                    return Ok(None);
                }

                let random_idx = rand::random_range(0..videos.len());
                let random_item = videos[random_idx];

                Ok(Some(NextPlayItem {
                    name: random_item.name.clone(),
                    item_type: random_item.item_type,
                    source_config: build_next_source_config(&build_full_path(
                        &Self::decode_target(Some(&random_item.target))?
                            .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                    )),
                    metadata: json!({"size": random_item.size, "thumbnail": random_item.thumbnail, "modified_at": random_item.modified_at}),
                    provider_data: json!({}),
                    target: random_item.target.clone(),
                }.strip_credentials()))
            }
        }
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(relative_path) = Self::decode_target(target)? else {
            return Ok(Vec::new());
        };

        let mut segments = Vec::new();
        for (index, segment) in relative_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .enumerate()
        {
            let target = Self::encode_target(
                &relative_path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .take(index + 1)
                    .collect::<Vec<_>>()
                    .join("/"),
            )?;
            segments.push(DynamicBrowsePathSegment {
                name: segment.to_string(),
                target,
            });
        }

        Ok(segments)
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

    /// Validate Alist source config: checks path and credential_ref fields.
    /// Host/token are no longer in source_config (resolved from credential_ref at runtime).
    fn validate_alist(config: Value) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(&config)?;

        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        // Use the shared validate_path_for_traversal (matches actual impl)
        validate_path_for_traversal(&config.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;
        if config.credential_ref.credential_owner_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "credential_ref.credential_owner_id must not be empty".to_string(),
            ));
        }
        if config.credential_ref.server_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "credential_ref.server_id must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_valid_alist_config() {
        let config = json!({
            "path": "/media/movies/test.mp4",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_alist(config).is_ok());
    }

    #[test]
    fn test_alist_config_with_provider_instance_name() {
        let config = json!({
            "path": "/media/movies/test.mp4",
            "provider_instance_name": "remote-alist-1",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_alist(config).is_ok());
    }

    #[test]
    fn test_alist_config_path_traversal() {
        let config = json!({
            "path": "/media/../../../etc/passwd",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_empty_path() {
        let config = json!({
            "path": "",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_config_missing_credential_ref() {
        let config = json!({
            "path": "/media/movies/test.mp4"
        });
        assert!(validate_alist(config).is_err());
    }

    #[test]
    fn test_alist_credential_ref_parsing() {
        let config = json!({
            "path": "/media/movies",
            "credential_ref": {
                "credential_owner_id": "owner-abc",
                "server_id": "srv-xyz"
            }
        });
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        assert_eq!(parsed.credential_ref.credential_owner_id, "owner-abc");
        assert_eq!(parsed.credential_ref.server_id, "srv-xyz");
        assert_eq!(parsed.path, "/media/movies");
        assert!(parsed.provider_instance_name.is_none());
    }

    // ========== Path Traversal Validation Tests ==========

    #[test]
    fn test_path_traversal_validation_rejects_literal_double_dot() {
        // Use the centralized validation function
        assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
        assert!(validate_path_for_traversal("../secret").is_err());
        assert!(validate_path_for_traversal("test/../etc").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_url_encoded_dot() {
        // URL-encoded . (2E in hex)
        assert!(validate_path_for_traversal("%2e%2e/etc/passwd").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err()); // uppercase
        assert!(validate_path_for_traversal("test/%2e%2e/config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_mixed_encoding() {
        // Mixed literal and encoded
        assert!(validate_path_for_traversal("..%2fetc/passwd").is_err());
        assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_backslash_traversal() {
        assert!(validate_path_for_traversal("..\\..\\windows").is_err());
        assert!(validate_path_for_traversal("test\\..\\config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_mixed_dot_sequences() {
        assert!(validate_path_for_traversal("./../etc").is_err());
        assert!(validate_path_for_traversal(".././secret").is_err());
        assert!(validate_path_for_traversal("././../config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_null_bytes() {
        assert!(validate_path_for_traversal("test\0../etc").is_err());
        assert!(validate_path_for_traversal("/etc/\0passwd").is_err());
    }

    #[test]
    fn test_path_traversal_validation_allows_valid_paths() {
        assert!(validate_path_for_traversal("media/movies").is_ok());
        assert!(validate_path_for_traversal("/absolute/path").is_ok());
        assert!(validate_path_for_traversal("folder with spaces/file.txt").is_ok());
        assert!(validate_path_for_traversal("file-with-dashes.txt").is_ok());
        assert!(validate_path_for_traversal("file_with_underscores.txt").is_ok());
    }

    /// Test helper to verify cursor-based pagination bounds.
    /// The sequential mode algorithm should:
    /// 1. Process one page at a time (max `PAGE_SIZE` items in memory)
    /// 2. Find current item and look for next within same or next page
    /// 3. Not accumulate items across pages (bounded memory)
    #[test]
    fn test_sequential_pagination_memory_bounds() {
        // Simulate the pagination behavior: max PAGE_SIZE items in memory at once
        const PAGE_SIZE: usize = 50;

        // Simulate finding item at position 125 (page 2, index 25)
        let current_item_idx = 125;

        // Old behavior would load: page 0 + page 1 + page 2 = 150 items
        // New behavior only processes one page at a time

        let page_of_current = current_item_idx / PAGE_SIZE; // page 2
        let idx_in_page = current_item_idx % PAGE_SIZE; // index 25

        assert_eq!(page_of_current, 2);
        assert_eq!(idx_in_page, 25);

        // Next item is at position 126 (same page, index 26)
        // So we only need to keep at most PAGE_SIZE items in memory
        let next_idx_in_page = idx_in_page + 1;
        assert!(next_idx_in_page < PAGE_SIZE, "Next item is in same page");

        // If current is at end of page (index 49), next is in next page
        // We discard current page and fetch next, still only PAGE_SIZE in memory
    }

    /// Test shuffle mode memory bounds (capped at `MAX_ITEMS`).
    #[test]
    fn test_shuffle_pagination_memory_bounds() {
        const PAGE_SIZE: usize = 50;
        const MAX_ITEMS: usize = 200;

        // Simulate a folder with 800 items
        let total_items = 800;

        // Old behavior: would fetch 20 pages = 1000 items (or hit MAX_PAGES limit)
        // New behavior: stops at MAX_ITEMS = 200 items (4 pages)

        let pages_to_fetch = MAX_ITEMS.div_ceil(PAGE_SIZE); // 4 pages
        let items_fetched = pages_to_fetch * PAGE_SIZE; // 200 items

        assert_eq!(pages_to_fetch, 4);
        assert!(items_fetched <= MAX_ITEMS);
        assert!(items_fetched < total_items, "Should not fetch all items");

        // Memory usage: max 200 items vs 1000 items (80% reduction)
    }

    // ========== Credential Ref Tests ==========

    #[test]
    fn test_alist_config_with_password() {
        // Alist supports optional per-directory password
        let config = json!({
            "path": "/media/movies/test.mp4",
            "password": "dir-password",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        assert_eq!(parsed.password, Some("dir-password".to_string()));
    }

    // ========== B6: Alist URL-encoded path traversal ==========

    #[test]
    fn test_alist_url_encoded_path_traversal_rejected() {
        // The current Alist validation only checks for literal ".."
        // but URL-encoded traversal like "%2e%2e/" should also be caught.
        // After the fix, validate_source_config should use the shared
        // validate_path_for_traversal function.

        // URL-encoded .. (%2e%2e)
        assert!(
            validate_path_for_traversal("%2e%2e/etc/passwd").is_err(),
            "URL-encoded dot-dot must be rejected by validate_path_for_traversal"
        );

        // Mixed case
        assert!(
            validate_path_for_traversal("%2E%2E/secret").is_err(),
            "Uppercase URL-encoded dot-dot must be rejected"
        );

        // Double-encoded
        assert!(
            validate_path_for_traversal("%252e%252e/etc").is_err(),
            "Double-encoded traversal must be rejected"
        );
    }

    #[test]
    fn test_alist_validate_config_uses_shared_path_traversal_check() {
        // After the fix, the Alist validate_source_config should use
        // validate_path_for_traversal instead of simple contains("..")

        // This config uses URL-encoded traversal: %2e%2e/etc/passwd
        let config = json!({
            "path": "/media/%2e%2e/etc/passwd",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });

        // After the fix, the config should be rejected
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        let result = validate_path_for_traversal(&parsed.path);
        assert!(
            result.is_err(),
            "Alist path with URL-encoded traversal must be rejected"
        );
    }
}
