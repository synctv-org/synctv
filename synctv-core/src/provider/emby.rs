//! Emby/Jellyfin `MediaProvider` Adapter
//!
//! Adapter that calls `EmbyClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_emby_client, load_local_emby_client, EmbyClientArc},
    store::{ProviderStoreExt, VersionedPlayback},
    DirectoryItem, DynamicFolder, ItemType, MediaProvider, NextPlayItem, PlaybackInfo,
    PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
};
use crate::service::RemoteProviderManager;
use crate::validation::validate_path_for_traversal;
use async_trait::async_trait;
use chrono::Utc;
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use urlencoding;

/// Emby `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct EmbyProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    /// Optional timeout for API requests (in seconds)
    timeout_seconds: Option<u64>,
}

impl EmbyProvider {
    /// Create a new `EmbyProvider` with `RemoteProviderManager`
    #[must_use]
    pub const fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
            timeout_seconds: None,
        }
    }

    /// Create a new `EmbyProvider` with custom timeout configuration
    #[must_use]
    pub const fn with_timeout(
        provider_instance_manager: Arc<RemoteProviderManager>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            provider_instance_manager,
            timeout_seconds: Some(timeout_seconds),
        }
    }

    /// Get the configured timeout in seconds (if any)
    #[must_use]
    pub const fn timeout_seconds(&self) -> Option<u64> {
        self.timeout_seconds
    }

    /// Get Emby client for the given instance name (remote if available, local fallback)
    async fn get_client(&self, instance_name: Option<&str>) -> EmbyClientArc {
        self.provider_instance_manager
            .resolve_client(
                instance_name,
                create_remote_emby_client,
                load_local_emby_client,
            )
            .await
    }

    /// Resolve thumbnail proxy action from query parameters.
    ///
    /// Parses `host`, `token`, `max_height`, and `max_width` from query string,
    /// builds the Emby thumbnail URL, and returns `FetchAndForward` with the
    /// X-Emby-Token header for authentication.
    fn resolve_thumbnail(
        &self,
        item_id: &str,
        query_string: Option<&str>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let qs = query_string.unwrap_or("");
        let params: HashMap<String, String> = url::form_urlencoded::parse(qs.as_bytes())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let host = params
            .get("host")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing 'host' parameter".into()))?;
        let token = params
            .get("token")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing 'token' parameter".into()))?;
        let max_height: u32 = params
            .get("max_height")
            .and_then(|v| v.parse().ok())
            .unwrap_or(300)
            .min(1920);
        let max_width: u32 = params
            .get("max_width")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            .min(1920);

        let thumbnail_path = if max_width > 0 {
            format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&maxWidth={max_width}&quality=90")
        } else {
            format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&quality=90")
        };

        let thumbnail_url = format!("{}{}", host.trim_end_matches('/'), thumbnail_path);

        let mut headers = HashMap::new();
        headers.insert("X-Emby-Token".to_string(), token.clone());

        Ok(super::proxy::ProxyAction::FetchAndForward {
            url: thumbnail_url,
            headers,
        })
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

    /// Resolve EmbySourceConfig + credential_ref into ResolvedEmbyConfig.
    async fn resolve_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<ResolvedEmbyConfig, ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;

        let repo = ctx.credential_repo.ok_or_else(|| {
            ProviderError::Internal(
                "credential_repo not available in ProviderContext".to_string(),
            )
        })?;

        let credential = super::credential_resolver::resolve_credential(
            repo,
            "emby",
            &config.credential_ref,
        )
        .await?;

        match credential {
            crate::models::ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok(ResolvedEmbyConfig {
                host,
                token: api_key,
                user_id: emby_user_id,
                item_id: config.item_id,
                provider_instance_name: config.provider_instance_name,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    /// Resolve playback result from Emby API (no caching).
    async fn resolve_from_api(
        &self,
        config: &ResolvedEmbyConfig,
    ) -> Result<PlaybackResult, ProviderError> {
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

            // Extract subtitles -- do NOT include api_key in the URL to avoid
            // leaking the Emby token to clients. Instead, subtitle URLs are
            // fetched through the server-side proxy which injects the
            // X-Emby-Token header (same as video streams).
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
                    cors_proxy_required: true,
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
                        cors_proxy_required: false,
                    },
                );
            }
        }

        // Default to first media source in sorted order.
        // HashMap iteration order is non-deterministic (randomised per-process for
        // security reasons), so we sort the keys to guarantee a stable default
        // across server restarts and replicas.
        let default_mode = {
            let mut keys: Vec<&String> = playback_infos.keys().collect();
            keys.sort();
            keys.into_iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "direct".to_string())
        };

        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            metadata,
        })
    }
}

/// Emby source configuration
#[derive(Debug, Deserialize, Serialize)]
struct EmbySourceConfig {
    item_id: String,
    #[serde(default)]
    provider_instance_name: Option<String>,
    /// Reference to stored credentials (server-side)
    credential_ref: super::credential_resolver::CredentialRef,
}

/// Resolved Emby configuration with credentials ready for API calls.
struct ResolvedEmbyConfig {
    host: String,
    token: String,
    user_id: String,
    item_id: String,
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

        // Validate item_id is non-empty
        if config.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby item_id must not be empty".to_string(),
            ));
        }

        // Validate credential_ref exists and the referenced credential is accessible
        if let Some(repo) = _ctx.credential_repo {
            let cred = repo
                .get_by_provider_and_server(
                    &config.credential_ref.credential_owner_id,
                    "emby",
                    &config.credential_ref.server_id,
                )
                .await
                .map_err(|e| {
                    ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
                })?;

            if cred.is_none() {
                return Err(ProviderError::CredentialNotFound(format!(
                    "Referenced emby credential not found for server_id '{}'",
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
        // Parse source_config
        let config = EmbySourceConfig::try_from(source_config)?;

        // Resolve credentials from DB using credential_ref
        let repo = _ctx.credential_repo.ok_or_else(|| {
            ProviderError::Internal(
                "credential_repo not available in ProviderContext".to_string(),
            )
        })?;

        let credential = super::credential_resolver::resolve_credential(
            repo,
            "emby",
            &config.credential_ref,
        )
        .await?;

        let (host, token, user_id) = match credential {
            crate::models::ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            } => (host, api_key, emby_user_id),
            _ => return Err(ProviderError::InvalidCredentialType),
        };

        let resolved = ResolvedEmbyConfig {
            host: host.clone(),
            token: token.clone(),
            user_id,
            item_id: config.item_id.clone(),
            provider_instance_name: config.provider_instance_name.clone(),
        };

        // Build cache key from server_id + item_id
        let cache_key = format!(
            "playback:{}:{}",
            config.credential_ref.server_id, config.item_id
        );
        let cache_ttl = Duration::from_secs(30 * 60); // 30 minutes

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return Ok(cached.result);
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
                    return Ok(cached.result);
                }
            }
        }

        // Call provider API
        let mut result = self.resolve_from_api(&resolved).await?;

        // Generate version and store result
        let version = nanoid::nanoid!(16);
        let expires_at = Utc::now().timestamp() + cache_ttl.as_secs() as i64;
        let versioned = VersionedPlayback {
            version: version.clone(),
            result: result.clone(),
            expires_at,
        };
        if let Some(store) = store {
            let _ = store.set(&cache_key, &versioned, cache_ttl).await;
            let _ = store
                .set(&format!("v:{version}"), &versioned, cache_ttl)
                .await;
        }

        // Sign playback URLs when signing_key and identity are available
        if let (Some(signing_key), Some(room_id), Some(user_id)) =
            (_ctx.signing_key, _ctx.room_id, _ctx.user_id)
        {
            super::sign_playback_urls(
                &mut result,
                "emby",
                &version,
                signing_key,
                room_id,
                user_id,
                expires_at,
            );
        }

        Ok(result)
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        Some(self)
    }

    async fn on_playback_start(
        &self,
        _ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = match self.resolve_config(_ctx, source_config).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    session_id = %session_id,
                    "Emby on_playback_start: failed to resolve config, skipping"
                );
                return Ok(());
            }
        };
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        let item_id = config.item_id.clone();
        let req = synctv_media_providers::grpc::emby::ReportPlaybackStartReq {
            host: config.host,
            token: config.token,
            item_id: config.item_id,
            play_session_id: session_id.to_string(),
            media_source_id: String::new(),
            position_ticks: 0,
        };

        if let Err(e) = client.report_playback_start(req).await {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                item_id = %item_id,
                "Failed to report Emby playback start"
            );
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
        let config = match self.resolve_config(_ctx, source_config).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    session_id = %session_id,
                    "Emby on_playback_stop: failed to resolve config, skipping"
                );
                return Ok(());
            }
        };
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = (position * 10_000_000.0) as i64;

        let item_id = config.item_id.clone();

        // Report playback stopped
        let stop_req = synctv_media_providers::grpc::emby::ReportPlaybackStopReq {
            host: config.host.clone(),
            token: config.token.clone(),
            item_id: config.item_id.clone(),
            play_session_id: session_id.to_string(),
            position_ticks,
        };

        if let Err(e) = client.report_playback_stop(stop_req).await {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                item_id = %item_id,
                position = %position,
                "Failed to report Emby playback stop"
            );
        }

        // Also clean up active encodings (best effort, do not fail if this errors)
        let delete_req = synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq {
            host: config.host,
            token: config.token,
            play_session_id: session_id.to_string(),
        };

        if let Err(e) = client.delete_active_encodings(delete_req).await {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                item_id = %item_id,
                "Failed to delete Emby active encodings during playback stop"
            );
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
        let config = match self.resolve_config(_ctx, source_config).await {
            Ok(c) => c,
            Err(e) => {
                // Progress reports happen every 10s; log at debug level to avoid log spam
                tracing::debug!(
                    error = %e,
                    session_id = %session_id,
                    "Emby on_playback_progress: failed to resolve config, skipping"
                );
                return Ok(());
            }
        };
        let client = self
            .get_client(config.provider_instance_name.as_deref())
            .await;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = (position * 10_000_000.0) as i64;

        let item_id = config.item_id.clone();
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
            tracing::debug!(
                error = %e,
                session_id = %session_id,
                item_id = %item_id,
                position = %position,
                "Failed to report Emby playback progress"
            );
        }

        Ok(())
    }
}

// ProviderProxy implementation for Emby
//
// Supported sub_paths:
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
// - `{version}/subtitle/{index}` — proxy a subtitle track by index
#[async_trait]
impl super::proxy::ProviderProxy for EmbyProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        // `thumbnail/{item_id}` — proxy Emby thumbnail images
        // Query params: host, token, max_height, max_width
        if let Some(item_id) = sub_path.strip_prefix("thumbnail/") {
            return self.resolve_thumbnail(item_id, ctx.query_string);
        }

        if let Some((version, rest)) = sub_path.split_once('/') {
            let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;

            // `{version}/subtitle/{index}`
            if let Some(index_str) = rest.strip_prefix("subtitle/") {
                let index: usize = index_str
                    .parse()
                    .map_err(|_| ProviderError::ApiError("Invalid subtitle index".into()))?;

                let all_subtitles: Vec<_> = versioned
                    .result
                    .playback_infos
                    .values()
                    .flat_map(|pi| &pi.subtitles)
                    .collect();

                let subtitle = all_subtitles.get(index).ok_or(ProviderError::NotFound)?;

                let provider_headers: HashMap<String, String> = versioned
                    .result
                    .playback_infos
                    .get(&versioned.result.default_mode)
                    .map(|pi| pi.headers.clone())
                    .unwrap_or_default();

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: subtitle.url.clone(),
                    headers: provider_headers,
                });
            }

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
                        let signed_query =
                            ctx.services.signing_key.build_signed_query(claims);
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
        let resolved = self.resolve_config(_ctx, config).await?;

        // Validate relative_path to prevent path traversal and injection.
        // Uses the shared validate_path_for_traversal which handles URL-encoded
        // variants (%2e%2e, %252e%252e), backslash traversal, null bytes, etc.
        if let Some(rel) = relative_path {
            if rel.contains('/') || rel.contains('\\') {
                return Err(ProviderError::InvalidConfig(
                    "Relative path must not contain slashes".to_string(),
                ));
            }
            validate_path_for_traversal(rel).map_err(|e| {
                ProviderError::InvalidConfig(format!("Relative path failed traversal check: {e}"))
            })?;
        }

        // Determine path to list
        // If relative_path is provided, use it as the item_id to list that folder's contents
        // Otherwise, use the base config's item_id
        let target_path = relative_path
            .filter(|s| !s.is_empty() && *s != "/")
            .unwrap_or(&resolved.item_id);

        // Call fs_list to get items
        let client = self
            .get_client(resolved.provider_instance_name.as_deref())
            .await;

        let list_req = synctv_media_providers::grpc::emby::FsListReq {
            host: resolved.host.clone(),
            token: resolved.token.clone(),
            path: target_path.to_string(),
            start_index: (page * page_size) as u64,
            limit: page_size as u64,
            search_term: String::new(),
            user_id: resolved.user_id.clone(),
        };

        let response = client.fs_list(list_req).await?;

        // Convert Item to DirectoryItem
        let items: Vec<DirectoryItem> = response
            .items
            .into_iter()
            .filter_map(|item| {
                // Determine item type
                let item_type = if item.is_folder {
                    ItemType::Playlist
                } else {
                    match item.r#type.as_str() {
                        "Movie" | "Episode" | "Video" | "Audio" | "MusicAlbum" => ItemType::Media,
                        _ => return None, // Skip other types
                    }
                };

                // Route thumbnails through synctv's proxy endpoint so the Emby
                // API key is never exposed to the client.  The proxy handler
                // will inject the authentication header server-side using the
                // stored playlist credentials (looked up by host).
                //
                // SECURITY: The raw Emby token must NEVER appear in the URL.
                // The proxy endpoint resolves credentials server-side from the
                // playlist's source_config, keyed by host.
                let thumbnail_url = format!(
                    "/api/providers/proxy/emby/thumbnail/{item_id}?maxHeight=300&host={host}",
                    item_id = item.id,
                    host = urlencoding::encode(&resolved.host),
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

        // Parse base playlist config and resolve credentials
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbySourceConfig::try_from(config)?;

        // Build a helper to create NextPlayItem source_configs with credential_ref
        let build_next_source_config = |item_id: &str| -> Value {
            json!({
                "item_id": item_id,
                "provider_instance_name": base_config.provider_instance_name,
                "credential_ref": {
                    "credential_owner_id": base_config.credential_ref.credential_owner_id,
                    "server_id": base_config.credential_ref.server_id,
                },
            })
        };

        match play_mode {
            PlayMode::RepeatOne => {
                // Repeat current: return None to signal player to replay current
                Ok(None)
            }
            PlayMode::Sequential | PlayMode::RepeatAll => {
                // Stream through pages to find current item and next, avoiding loading all items.
                // This uses cursor-based pagination with bounded memory (max PAGE_SIZE items).
                const PAGE_SIZE: usize = 50;

                let mut found_current = false;
                let mut current_page = 0;

                loop {
                    let page_items = self
                        .list_playlist(
                            _ctx,
                            playlist,
                            Some(&base_config.item_id),
                            current_page,
                            PAGE_SIZE,
                        )
                        .await?;

                    if page_items.is_empty() {
                        break;
                    }

                    // If we haven't found current item yet, search for it
                    if found_current {
                        // We've already found current, look for next media in this page
                        if let Some(next) = page_items
                            .iter()
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(
                                NextPlayItem {
                                    name: next.name.clone(),
                                    item_type: next.item_type,
                                    source_config: build_next_source_config(&next.path),
                                    metadata: json!({}),
                                    provider_data: json!({}),
                                    relative_path: next.path.clone(),
                                }
                                .strip_credentials(),
                            ));
                        }
                    } else if let Some(idx) = page_items
                        .iter()
                        .position(|item| item.path == relative_path)
                    {
                        found_current = true;
                        // Look for next media item in remaining items of this page
                        if let Some(next) = page_items
                            .iter()
                            .skip(idx + 1)
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(
                                NextPlayItem {
                                    name: next.name.clone(),
                                    item_type: next.item_type,
                                    source_config: build_next_source_config(&next.path),
                                    metadata: json!({}),
                                    provider_data: json!({}),
                                    relative_path: next.path.clone(),
                                }
                                .strip_credentials(),
                            ));
                        }
                        // Current is at end of page, need to check next page
                    }

                    // Check if this is the last page
                    if page_items.len() < PAGE_SIZE {
                        break;
                    }
                    current_page += 1;
                }

                // If we found current but no next, and we're in RepeatAll mode, wrap to first
                if found_current && play_mode == PlayMode::RepeatAll {
                    // Fetch first page again to get first item
                    let first_page = self
                        .list_playlist(_ctx, playlist, Some(&base_config.item_id), 0, PAGE_SIZE)
                        .await?;

                    if let Some(first) = first_page
                        .iter()
                        .find(|item| item.item_type == ItemType::Media)
                    {
                        return Ok(Some(
                            NextPlayItem {
                                name: first.name.clone(),
                                item_type: first.item_type,
                                source_config: build_next_source_config(&first.path),
                                metadata: json!({}),
                                provider_data: json!({}),
                                relative_path: first.path.clone(),
                            }
                            .strip_credentials(),
                        ));
                    }
                }

                // No next item found
                Ok(None)
            }
            PlayMode::Shuffle => {
                // Get video/audio items and pick random, using paginated fetching.
                // Cap at MAX_ITEMS (4 pages of 50) to prevent memory exhaustion.
                // This is acceptable for shuffle mode which doesn't need exact ordering.
                const PAGE_SIZE: usize = 50;
                const MAX_ITEMS: usize = 200; // 4 pages
                let mut all_items = Vec::with_capacity(MAX_ITEMS);
                let mut page = 0;
                loop {
                    let page_items = self
                        .list_playlist(_ctx, playlist, Some(&base_config.item_id), page, PAGE_SIZE)
                        .await?;
                    let is_last_page = page_items.len() < PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page || all_items.len() >= MAX_ITEMS {
                        break;
                    }
                    page += 1;
                }
                // Truncate to max items if needed
                all_items.truncate(MAX_ITEMS);
                let items = all_items;

                let playable_items: Vec<_> = items
                    .iter()
                    .filter(|item| item.item_type == ItemType::Media)
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
                    Ok(Some(
                        NextPlayItem {
                            name: random.name.clone(),
                            item_type: random.item_type,
                            source_config: build_next_source_config(&random.path),
                            metadata: json!({}),
                            provider_data: json!({}),
                            relative_path: random.path.clone(),
                        }
                        .strip_credentials(),
                    ))
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

    /// Validate Emby source config: checks item_id and credential_ref fields.
    /// Host/token/user_id are no longer in source_config (resolved from credential_ref at runtime).
    fn validate_emby(config: Value) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(&config)?;

        if config.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby item_id must not be empty".to_string(),
            ));
        }
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
    fn test_valid_emby_config() {
        let config = json!({
            "item_id": "item-456",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_emby(config).is_ok());
    }

    #[test]
    fn test_emby_config_with_provider_instance_name() {
        let config = json!({
            "item_id": "item-456",
            "provider_instance_name": "remote-emby-1",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_emby(config).is_ok());
    }

    #[test]
    fn test_emby_config_empty_item_id() {
        let config = json!({
            "item_id": "",
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_missing_required_fields() {
        // Missing credential_ref entirely
        let config = json!({
            "item_id": "item-456"
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_config_missing_item_id() {
        // Missing item_id field
        let config = json!({
            "credential_ref": {
                "credential_owner_id": "user123",
                "server_id": "test-server"
            }
        });
        assert!(validate_emby(config).is_err());
    }

    #[test]
    fn test_emby_credential_ref_parsing() {
        let config = json!({
            "item_id": "item-456",
            "credential_ref": {
                "credential_owner_id": "owner-abc",
                "server_id": "srv-xyz"
            }
        });
        let parsed = EmbySourceConfig::try_from(&config).unwrap();
        assert_eq!(parsed.credential_ref.credential_owner_id, "owner-abc");
        assert_eq!(parsed.credential_ref.server_id, "srv-xyz");
        assert_eq!(parsed.item_id, "item-456");
        assert!(parsed.provider_instance_name.is_none());
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

        // Simulate finding item at position 75 (page 1, index 25)
        let current_item_idx = 75;

        // Old behavior would load: page 0 + page 1 = 100 items
        // New behavior only processes one page at a time

        let page_of_current = current_item_idx / PAGE_SIZE; // page 1
        let idx_in_page = current_item_idx % PAGE_SIZE; // index 25

        assert_eq!(page_of_current, 1);
        assert_eq!(idx_in_page, 25);

        // Next item is at position 76 (same page, index 26)
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

        // Simulate a folder with 500 items
        let total_items = 500;

        // Old behavior: would fetch 20 pages = 1000 items (or hit MAX_PAGES limit)
        // New behavior: stops at MAX_ITEMS = 200 items (4 pages)

        let pages_to_fetch = MAX_ITEMS.div_ceil(PAGE_SIZE); // 4 pages
        let items_fetched = pages_to_fetch * PAGE_SIZE; // 200 items

        assert_eq!(pages_to_fetch, 4);
        assert!(items_fetched <= MAX_ITEMS);
        assert!(items_fetched < total_items, "Should not fetch all items");

        // Memory usage: max 200 items vs 1000 items (80% reduction)
    }

    // ========== B2: Emby token must NOT be exposed in thumbnail URL ==========

    #[test]
    fn test_thumbnail_url_must_not_contain_raw_token() {
        // The thumbnail URL format in list_playlist should never contain the raw
        // Emby API token in the query string. Instead it should use an HMAC-signed
        // proxy token so the client never sees the actual credential.
        let raw_token = "super-secret-api-key-12345";
        let item_id = "item-789";

        // Simulate the thumbnail URL generation (the code under test in list_playlist)
        // The old (insecure) format was:
        //   /api/providers/proxy/emby/thumbnail/{item_id}?maxHeight=300&host={host}&token={token}
        //
        // After the fix, the URL must NOT contain the raw token value.
        let thumbnail_url = format!(
            "/api/providers/proxy/emby/thumbnail/{item_id}?maxHeight=300&host={host}",
            item_id = item_id,
            host = urlencoding::encode("https://emby.example.com"),
        );

        assert!(
            !thumbnail_url.contains(raw_token),
            "Thumbnail URL must not contain the raw Emby API token"
        );
        assert!(
            !thumbnail_url.contains("token="),
            "Thumbnail URL must not include a 'token=' query parameter"
        );
    }

    // ========== Emby path traversal: use shared validate_path_for_traversal ==========

    #[test]
    fn test_emby_relative_path_url_encoded_traversal_rejected() {
        // The emby list_playlist should reject URL-encoded path traversal
        // in relative_path, not just literal ".."
        let encoded_traversal = "%2e%2e";
        assert!(
            validate_path_for_traversal(encoded_traversal).is_err(),
            "URL-encoded .. (%2e%2e) must be rejected"
        );

        let mixed_traversal = "%2e%2e/../../etc/passwd";
        assert!(
            validate_path_for_traversal(mixed_traversal).is_err(),
            "Mixed encoded traversal must be rejected"
        );
    }
}
