//! Emby `MediaProvider` Adapter
//!
//! Adapter that calls `EmbyClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_emby_client, EmbyClientArc, ProviderClientManager},
    store::{ProviderStoreExt, VersionedPlayback},
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
    MediaProvider, NextPlayItem, PlaybackClientProfile, PlaybackInfo, PlaybackResult,
    ProviderContext, ProviderCredentialDependency, ProviderError, SourceConfig,
};
use crate::models::media::{
    PlaybackEmbyMedia, PlaybackEmbySubtitle, PlaybackMedia, PlaybackMediaProvider,
    PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::service::RemoteProviderManager;
use async_trait::async_trait;
use chrono::Utc;
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use urlencoding;

const EMBY_TICKS_PER_SECOND: u128 = 10_000_000;

fn seconds_to_emby_ticks(position: f64) -> Result<i64, ProviderError> {
    let Ok(duration) = Duration::try_from_secs_f64(position.max(0.0)) else {
        return Err(ProviderError::InvalidConfig(format!(
            "Invalid Emby playback position: {position}"
        )));
    };
    let ticks = duration.as_nanos() / (1_000_000_000 / EMBY_TICKS_PER_SECOND);
    i64::try_from(ticks).map_err(|_| {
        ProviderError::InvalidConfig(format!(
            "Emby playback position {position} exceeds i64 tick range"
        ))
    })
}

fn usize_to_u64(value: usize, field: &str) -> Result<u64, ProviderError> {
    u64::try_from(value)
        .map_err(|_| ProviderError::InvalidConfig(format!("{field} exceeds u64::MAX")))
}

fn dynamic_list_start_index(page: usize, page_size: usize) -> Result<u64, ProviderError> {
    let zero_based_page = page
        .checked_sub(1)
        .ok_or_else(|| ProviderError::InvalidConfig("Emby page must be at least 1".to_string()))?;
    let start_index = zero_based_page.checked_mul(page_size).ok_or_else(|| {
        ProviderError::InvalidConfig(format!(
            "Emby pagination start overflows for page {page} and page size {page_size}"
        ))
    })?;
    usize_to_u64(start_index, "Emby pagination start")
}

fn optional_i64_to_proto_absent_zero(value: Option<i64>) -> i64 {
    value.unwrap_or(0)
}

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        provider,
    }
}

fn mark_emby_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // Emby exposes upstream modes and SyncTV proxy siblings together.
    // Upstream token headers remain visible by product policy; administrators
    // are warned that direct playback can disclose those credentials.
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let proxy_mode_name = format!("proxy_{mode_name}");
        if result.playback_infos.contains_key(&proxy_mode_name) {
            continue;
        }

        let mut proxy_info = original_info.clone();
        let proxy_is_hls = super::playback_info_is_hls(&mode_name, &original_info);
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(url_index, media)| {
                let url = media.upstream_url()?.to_string();
                Some(playback_media(
                    media.name.clone(),
                    media.format.clone(),
                    media.expire_at.map(|dt| dt.timestamp()),
                    PlaybackMediaProvider::Emby(if proxy_is_hls {
                        PlaybackEmbyMedia::ProxyHlsManifest {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            url_index,
                            url,
                            headers: media.upstream_headers(),
                        }
                    } else {
                        PlaybackEmbyMedia::ProxyMediaStream {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            url_index,
                            url,
                            headers: media.upstream_headers(),
                        }
                    }),
                ))
            })
            .collect();
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                provider: PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
    }
}

/// Build an absolute Emby URL from a configured server URL and an API path.
///
/// `host` may point at either a root deployment (`https://media.example.com`) or
/// a reverse-proxy base path (`https://media.example.com/emby`). Provider
/// responses may include paths with or without that base path, so this helper
/// preserves the configured base path without duplicating it.
pub fn emby_server_url(host: &str, path_or_url: &str) -> Result<String, ProviderError> {
    let path_or_url = path_or_url.trim();
    if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        return Ok(path_or_url.to_string());
    }

    let parsed = url::Url::parse(host)
        .map_err(|error| ProviderError::InvalidConfig(format!("Invalid Emby host URL: {error}")))?;
    let origin = parsed.origin().unicode_serialization();
    let base_path = parsed.path().trim_end_matches('/');
    let base_path = if base_path == "/" { "" } else { base_path };
    let path = if path_or_url.starts_with('/') {
        path_or_url.to_string()
    } else {
        format!("/{path_or_url}")
    };

    let path = if !base_path.is_empty()
        && path != base_path
        && !path
            .strip_prefix(base_path)
            .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with('?'))
    {
        format!("{base_path}{path}")
    } else {
        path
    };

    Ok(format!("{}{}", origin.trim_end_matches('/'), path))
}

struct GrpcPlaybackRequestHints {
    max_audio_channels: Option<i32>,
    enable_direct_play: Option<bool>,
    enable_direct_stream: Option<bool>,
    enable_transcoding: Option<bool>,
    device_profile: Option<synctv_media_providers::grpc::emby::PlaybackInfoDeviceProfile>,
}

fn grpc_playback_request_hints(
    profile: Option<&PlaybackClientProfile>,
) -> GrpcPlaybackRequestHints {
    profile.map_or(
        GrpcPlaybackRequestHints {
            max_audio_channels: None,
            enable_direct_play: None,
            enable_direct_stream: None,
            enable_transcoding: None,
            device_profile: None,
        },
        |profile| {
            let direct_play_audio_codecs = match profile.audio_capability {
                super::PlaybackAudioCapability::Stereo => vec!["aac", "mp3"],
                super::PlaybackAudioCapability::Surround => {
                    vec!["aac", "mp3", "ac3", "eac3", "dts"]
                }
                super::PlaybackAudioCapability::LosslessSurround => {
                    vec!["aac", "mp3", "ac3", "eac3", "dts", "flac", "alac", "truehd"]
                }
            };
            let subtitle_methods = match profile.subtitle_preference {
                super::PlaybackSubtitlePreference::External => {
                    vec![
                        synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::External as i32,
                    ]
                }
                super::PlaybackSubtitlePreference::EmbeddedOrExternal => vec![
                    synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::External as i32,
                    synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::Embed as i32,
                ],
                super::PlaybackSubtitlePreference::None => Vec::new(),
            };

            GrpcPlaybackRequestHints {
                max_audio_channels: profile.max_audio_channels,
                enable_direct_play: Some(!matches!(
                    profile.stream_preference,
                    super::PlaybackStreamPreference::Transcode
                )),
                enable_direct_stream: Some(!matches!(
                    profile.stream_preference,
                    super::PlaybackStreamPreference::Transcode
                )),
                enable_transcoding: Some(!matches!(
                    profile.stream_preference,
                    super::PlaybackStreamPreference::DirectPlay
                )),
                device_profile: Some(
                    synctv_media_providers::grpc::emby::PlaybackInfoDeviceProfile {
                        direct_play_profiles: profile
                            .supported_containers
                            .iter()
                            .map(|container| match container {
                                super::PlaybackContainer::Mp4 => {
                                    synctv_media_providers::grpc::emby::DirectPlayProfileHint {
                                        container: "mp4,m4v".to_string(),
                                        video_codecs: profile
                                            .supported_video_codecs
                                            .iter()
                                            .map(|codec| match codec {
                                                super::PlaybackVideoCodec::H264 => {
                                                    "h264".to_string()
                                                }
                                                super::PlaybackVideoCodec::Hevc => {
                                                    "hevc".to_string()
                                                }
                                                super::PlaybackVideoCodec::Vp9 => "vp9".to_string(),
                                                super::PlaybackVideoCodec::Av1 => "av1".to_string(),
                                            })
                                            .collect(),
                                        audio_codecs: match profile.audio_capability {
                                            super::PlaybackAudioCapability::Stereo => {
                                                vec!["aac".to_string(), "mp3".to_string()]
                                            }
                                            super::PlaybackAudioCapability::Surround => vec![
                                                "aac".to_string(),
                                                "mp3".to_string(),
                                                "ac3".to_string(),
                                                "eac3".to_string(),
                                            ],
                                            super::PlaybackAudioCapability::LosslessSurround => {
                                                vec![
                                                    "aac".to_string(),
                                                    "mp3".to_string(),
                                                    "ac3".to_string(),
                                                    "eac3".to_string(),
                                                    "flac".to_string(),
                                                    "alac".to_string(),
                                                ]
                                            }
                                        },
                                    }
                                }
                                super::PlaybackContainer::Mkv => {
                                    synctv_media_providers::grpc::emby::DirectPlayProfileHint {
                                        container: "mkv".to_string(),
                                        video_codecs: profile
                                            .supported_video_codecs
                                            .iter()
                                            .map(|codec| match codec {
                                                super::PlaybackVideoCodec::H264 => {
                                                    "h264".to_string()
                                                }
                                                super::PlaybackVideoCodec::Hevc => {
                                                    "hevc".to_string()
                                                }
                                                super::PlaybackVideoCodec::Vp9 => "vp9".to_string(),
                                                super::PlaybackVideoCodec::Av1 => "av1".to_string(),
                                            })
                                            .collect(),
                                        audio_codecs: direct_play_audio_codecs
                                            .iter()
                                            .map(|codec| (*codec).to_string())
                                            .collect(),
                                    }
                                }
                                super::PlaybackContainer::Webm => {
                                    synctv_media_providers::grpc::emby::DirectPlayProfileHint {
                                        container: "webm".to_string(),
                                        video_codecs: profile
                                            .supported_video_codecs
                                            .iter()
                                            .filter_map(|codec| match codec {
                                                super::PlaybackVideoCodec::H264
                                                | super::PlaybackVideoCodec::Hevc => None,
                                                super::PlaybackVideoCodec::Vp9 => {
                                                    Some("vp9".to_string())
                                                }
                                                super::PlaybackVideoCodec::Av1 => {
                                                    Some("av1".to_string())
                                                }
                                            })
                                            .collect(),
                                        audio_codecs: vec![
                                            "vorbis".to_string(),
                                            "opus".to_string(),
                                        ],
                                    }
                                }
                            })
                            .collect(),
                        transcoding_container: "ts".to_string(),
                        transcoding_protocol: "hls".to_string(),
                        transcoding_video_codec: "h264".to_string(),
                        transcoding_audio_codec: "aac".to_string(),
                        subtitle_profiles: match profile.subtitle_preference {
                            super::PlaybackSubtitlePreference::None => Vec::new(),
                            super::PlaybackSubtitlePreference::External
                            | super::PlaybackSubtitlePreference::EmbeddedOrExternal => {
                                subtitle_methods
                                    .into_iter()
                                    .flat_map(|method| {
                                        ["srt", "vtt", "ass"].into_iter().map(move |format| {
                                    synctv_media_providers::grpc::emby::SubtitleProfileHint {
                                        format: format.to_string(),
                                        method,
                                    }
                                })
                                    })
                                    .collect()
                            }
                        },
                    },
                ),
            }
        },
    )
}

fn emby_auth_headers(token: &str) -> HashMap<String, String> {
    HashMap::from([("X-Emby-Token".to_string(), token.to_string())])
}

/// Emby `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct EmbyProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct EmbyBrowseTarget {
    item_id: String,
}

impl EmbyProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "emby";

    /// Create a new `EmbyProvider` with `RemoteProviderManager`
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    #[must_use]
    pub fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
        }
    }

    #[cfg(test)]
    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager:
                crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    async fn get_client_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyClientArc, ProviderError> {
        match instance_name {
            None => Ok(self.client_manager.local_emby_client()),
            Some(_) => {
                self.provider_instance_manager
                    .resolve_client_required_with_context(
                        instance_name,
                        request_context,
                        create_remote_emby_client,
                        || self.client_manager.local_emby_client(),
                    )
                    .await
            }
        }
    }

    /// Login to Emby and return a validated provider credential payload.
    pub async fn login(
        &self,
        req: synctv_media_providers::grpc::emby::LoginReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::LoginResp, ProviderError> {
        self.login_with_context(req, instance_name, None).await
    }

    pub async fn login_with_context(
        &self,
        req: synctv_media_providers::grpc::emby::LoginReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::emby::LoginResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.login(req).await.map_err(std::convert::Into::into)
    }

    /// List Emby library items
    pub async fn fs_list(
        &self,
        req: synctv_media_providers::grpc::emby::FsListReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::FsListResp, ProviderError> {
        self.fs_list_with_context(req, instance_name, None).await
    }

    pub async fn fs_list_with_context(
        &self,
        req: synctv_media_providers::grpc::emby::FsListReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::emby::FsListResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.fs_list(req).await.map_err(std::convert::Into::into)
    }

    /// Get Emby user info
    pub async fn me(
        &self,
        req: synctv_media_providers::grpc::emby::MeReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::emby::MeResp, ProviderError> {
        self.me_with_context(req, instance_name, None).await
    }

    pub async fn me_with_context(
        &self,
        req: synctv_media_providers::grpc::emby::MeReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::emby::MeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.me(req).await.map_err(std::convert::Into::into)
    }

    fn encode_target(item_id: &str) -> Result<Vec<u8>, ProviderError> {
        if item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby target item_id cannot be empty".to_string(),
            ));
        }

        serde_json::to_vec(&EmbyBrowseTarget {
            item_id: item_id.to_string(),
        })
        .map_err(|e| ProviderError::InvalidConfig(format!("Failed to encode Emby target: {e}")))
    }

    fn decode_target(target: Option<&[u8]>) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        if target.is_empty() {
            return Ok(None);
        }

        let payload: EmbyBrowseTarget = serde_json::from_slice(target)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid Emby target: {e}")))?;
        if payload.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby target item_id cannot be empty".to_string(),
            ));
        }

        Ok(Some(payload.item_id))
    }

    async fn fetch_item(
        &self,
        resolved: &ResolvedEmbyConfig,
        item_id: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::emby::Item, ProviderError> {
        let client = self
            .get_client_with_context(resolved.provider_instance_name.as_deref(), request_context)
            .await?;
        let request = synctv_media_providers::grpc::emby::GetItemReq {
            host: resolved.host.clone(),
            token: resolved.token.clone(),
            item_id: item_id.to_string(),
            user_id: resolved.user_id.clone(),
        };
        client.get_item(request).await.map_err(Into::into)
    }

    fn item_type_from_listing(item: &synctv_media_providers::grpc::emby::Item) -> Option<ItemType> {
        if item.is_folder {
            Some(ItemType::Playlist)
        } else {
            match item.r#type.as_str() {
                "Movie" | "Episode" | "Video" | "Audio" | "MusicAlbum" => Some(ItemType::Media),
                _ => None,
            }
        }
    }

    fn build_thumbnail_url(server_id: &str, credential_owner_id: &str, item_id: &str) -> String {
        format!(
            "/api/providers/emby/thumbnail/{item_id}?server_id={server_id}&credential_owner_id={credential_owner_id}&max_height=300",
            server_id = urlencoding::encode(server_id),
            credential_owner_id = urlencoding::encode(credential_owner_id),
        )
    }

    fn build_next_source_config(base_config: &EmbySourceConfig, item_id: &str) -> Value {
        json!({
            "item_id": item_id,
            "server_id": base_config.server_id,
        })
    }

    fn playback_cache_key(
        server_id: &str,
        credential_owner_id: &str,
        credential_revision: &str,
        item_id: &str,
        playback_profile_cache_key: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(credential_owner_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(credential_revision.as_bytes());
        hasher.update(b"\0");
        hasher.update(item_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(playback_profile_cache_key.as_bytes());
        let scoped_hash: String = hex::encode(hasher.finalize()).chars().take(24).collect();
        format!("playback:{server_id}:{scoped_hash}")
    }

    /// Resolve EmbySourceConfig into credentials owned by the media/playlist creator.
    async fn resolve_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<ResolvedEmbyConfig, ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "provider_access_service not available in ProviderContext".to_string(),
            )
        })?;
        let access = access_service
            .emby_access(
                *credential_owner_id,
                &config.server_id,
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        Ok(ResolvedEmbyConfig {
            host: access.host,
            token: access.api_key,
            user_id: access.emby_user_id,
            item_id: config.item_id,
            credential_owner_id: access.credential_owner_id,
            credential_revision: access.credential_revision,
            provider_instance_name: access.provider_instance_name,
        })
    }

    /// Resolve playback result from Emby API (no caching).
    async fn resolve_from_api(
        &self,
        config: &ResolvedEmbyConfig,
        request_context: Option<&super::ExecutionControl>,
        playback_client_profile: Option<&PlaybackClientProfile>,
    ) -> Result<PlaybackResult, ProviderError> {
        // Get appropriate client based on instance_name from config
        let client = self
            .get_client_with_context(config.provider_instance_name.as_deref(), request_context)
            .await?;

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

        let playback_hints = grpc_playback_request_hints(playback_client_profile);

        // Get playback info
        let playback_request = synctv_media_providers::grpc::emby::PlaybackInfoReq {
            host: config.host.clone(),
            token: config.token.clone(),
            user_id: config.user_id.clone(),
            item_id: config.item_id.clone(),
            media_source_id: String::new(), // Use default media source
            audio_stream_index: 0,
            subtitle_stream_index: 0,
            max_streaming_bitrate: optional_i64_to_proto_absent_zero(
                playback_client_profile.and_then(|profile| profile.max_streaming_bitrate),
            ),
            max_audio_channels: playback_hints.max_audio_channels,
            enable_direct_play: playback_hints.enable_direct_play,
            enable_direct_stream: playback_hints.enable_direct_stream,
            enable_transcoding: playback_hints.enable_transcoding,
            device_profile: playback_hints.device_profile,
        };

        let playback_info = client.playback_info(playback_request).await?;

        // Store play_session_id in metadata for lifecycle hooks
        metadata.insert(
            "emby_play_session_id".to_string(),
            json!(playback_info.play_session_id),
        );

        let mut playback_infos = HashMap::new();

        // Emby session-based URLs: default to 30 minutes
        let emby_expires_at = Utc::now().timestamp() + 30 * 60;

        // Auth headers for Emby: use X-Emby-Token header instead of
        // embedding api_key in query strings to avoid credential exposure
        // in URLs (which end up in logs, browser history, Referer headers).
        let emby_auth_headers = emby_auth_headers(&config.token);

        // Process media sources
        for (idx, source) in playback_info.media_source_info.iter().enumerate() {
            let mode_name = if source.name.is_empty() {
                format!("source_{idx}")
            } else {
                source.name.clone()
            };

            // Get direct stream URL (no transcoding) -- no credentials in URL
            let direct_url = if !source.direct_play_url.is_empty() {
                emby_server_url(&config.host, &source.direct_play_url)?
            } else if !source.path.is_empty() {
                emby_server_url(&config.host, &format!("/Items/{}/Download", config.item_id))?
            } else {
                continue;
            };

            // Extract subtitles -- do NOT include api_key in the URL to avoid
            // leaking the Emby token to clients. Direct clients and the server
            // proxy both use X-Emby-Token headers, same as video streams.
            let subtitles: Vec<PlaybackSubtitle> = source
                .media_stream_info
                .iter()
                .filter(|stream| stream.r#type == "Subtitle")
                .map(|stream| {
                    let subtitle_index = usize::try_from(stream.index).map_err(|_| {
                        ProviderError::InvalidConfig(format!(
                            "Invalid Emby subtitle stream index: {}",
                            stream.index
                        ))
                    })?;
                    let subtitle_url = emby_server_url(
                        &config.host,
                        &format!(
                            "/Videos/{}/{}/Subtitles/{}/Stream.{}",
                            config.item_id,
                            source.id,
                            stream.index,
                            stream.codec.to_lowercase(),
                        ),
                    )?;

                    Ok(PlaybackSubtitle {
                        language: stream.language.clone(),
                        name: stream.display_title.clone(),
                        format: stream.codec.to_lowercase(),
                        provider: PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle {
                            version: String::new(),
                            expires_at: emby_expires_at,
                            mode_name: mode_name.clone(),
                            subtitle_index,
                            url: subtitle_url,
                            headers: emby_auth_headers.clone(),
                        }),
                    })
                })
                .collect::<Result<_, ProviderError>>()?;

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
                    medias: vec![playback_media(
                        source.name.clone(),
                        format,
                        Some(emby_expires_at),
                        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                            url: direct_url,
                            headers: emby_auth_headers.clone(),
                        }),
                    )],
                    default_media_index: None,
                    subtitles,
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            );

            // Also add transcode URLs if available
            if !source.transcoding_url.is_empty() {
                let transcode_url = emby_server_url(&config.host, &source.transcoding_url)?;

                playback_infos.insert(
                    format!("{mode_name}_transcode"),
                    PlaybackInfo {
                        medias: vec![playback_media(
                            format!("{mode_name} Transcode"),
                            "hls".to_string(),
                            Some(emby_expires_at),
                            PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                                url: transcode_url,
                                headers: emby_auth_headers.clone(),
                            }),
                        )],
                        default_media_index: None,
                        subtitles: Vec::new(), // Subtitles burned in for transcode
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
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
            provider: Self::NAME.to_string(),
            provider_instance_name: config.provider_instance_name.clone(),
            duration_seconds: item
                .duration_seconds
                .filter(|duration| duration.is_finite() && *duration > 0.0),
            metadata,
        })
    }
}

/// Emby source configuration
#[derive(Debug, Deserialize, Serialize)]
struct EmbySourceConfig {
    item_id: String,
    /// Saved Emby credential server identifier.
    server_id: String,
}

/// Resolved Emby configuration with credentials ready for API calls.
struct ResolvedEmbyConfig {
    host: String,
    token: String,
    user_id: String,
    item_id: String,
    credential_owner_id: String,
    credential_revision: String,
    provider_instance_name: Option<String>,
}

impl TryFrom<&Value> for EmbySourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::reject_source_config_provider_instance_name(value, "Emby")?;
        super::reject_source_config_credential_ref(value, "Emby")?;
        super::parse_source_config(value, "Emby")
    }
}

#[async_trait]
impl MediaProvider for EmbyProvider {
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
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(source_config.value())?;

        // Validate item_id is non-empty
        if config.item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby item_id must not be empty".to_string(),
            ));
        }

        if config.server_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby server_id must not be empty".to_string(),
            ));
        }

        // Validate creator-owned credential exists
        if let Some(repo) = _ctx.credential_repo {
            let credential_owner_id = _ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?;
            let cred = repo
                .get_by_provider_and_server(*credential_owner_id, Self::NAME, &config.server_id)
                .await
                .map_err(|e| {
                    ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
                })?;

            if cred.is_none() {
                return Err(ProviderError::CredentialNotFound(format!(
                    "Referenced emby credential not found for server_id '{}'",
                    config.server_id
                )));
            }
        }

        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        Ok(vec![ProviderCredentialDependency::new(
            Self::NAME,
            credential_owner_id.to_string(),
            config.server_id,
        )])
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        super::reject_source_config_provider_instance_name(&source_config, "Emby")?;
        super::reject_source_config_credential_ref(&source_config, "Emby")?;

        Ok(source_config)
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = EmbySourceConfig::try_from(source_config)?;
        let resolved = self.resolve_config(_ctx, source_config).await?;
        let playback_client_profile = _ctx.playback_client_profile();
        let playback_profile_cache_key = playback_client_profile.map_or_else(
            || "default".to_string(),
            PlaybackClientProfile::cache_fingerprint,
        );

        // Cache must be partitioned by the effective playback capability
        // profile, otherwise different clients can receive each other's Emby
        // negotiation result.
        let cache_key = Self::playback_cache_key(
            &config.server_id,
            &resolved.credential_owner_id,
            &resolved.credential_revision,
            &config.item_id,
            &playback_profile_cache_key,
        );
        let cache_ttl = Duration::from_mins(30); // 30 minutes

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::build_cached_versioned_playback_response(
                        cached,
                        Self::NAME,
                        _ctx,
                        mark_emby_playback_resources,
                    )
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
                    return super::build_cached_versioned_playback_response(
                        cached,
                        Self::NAME,
                        _ctx,
                        mark_emby_playback_resources,
                    )
                    .await;
                }
            }
        }

        // Call provider API
        let result = self
            .resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
            .await?;

        // Generate version and store result
        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_emby_playback_resources,
        )
        .await
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
            .get_client_with_context(
                config.provider_instance_name.as_deref(),
                _ctx.request_context(),
            )
            .await?;

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
            .get_client_with_context(
                config.provider_instance_name.as_deref(),
                _ctx.request_context(),
            )
            .await?;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = seconds_to_emby_ticks(position)?;

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
        is_paused: bool,
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
            .get_client_with_context(
                config.provider_instance_name.as_deref(),
                _ctx.request_context(),
            )
            .await?;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = seconds_to_emby_ticks(position)?;

        let item_id = config.item_id.clone();
        let req = synctv_media_providers::grpc::emby::ReportPlaybackProgressReq {
            host: config.host,
            token: config.token,
            item_id: config.item_id,
            play_session_id: session_id.to_string(),
            media_source_id: String::new(),
            position_ticks,
            is_paused,
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

    fn playback_lifecycle_session_id(&self, result: &PlaybackResult) -> Option<String> {
        result
            .metadata
            .get("emby_play_session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
    }
}

impl EmbyProvider {
    pub async fn get_media_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: media.upstream_headers(),
                range_header: range_header.map(ToString::to_string),
            },
        )
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: media.upstream_headers(),
            },
        )
    }

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let headers = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .and_then(|info| info.medias.first())
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let subtitle = playback_info
            .subtitles
            .get(subtitle_index)
            .ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: subtitle.upstream_url().to_string(),
                headers: subtitle.upstream_headers(),
                range_header: None,
            },
        )
    }
}

#[async_trait]
impl DynamicFolder for EmbyProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
        query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbySourceConfig::try_from(config)?;
        let resolved = self.resolve_config(ctx, config).await?;
        let target_item_id =
            Self::decode_target(target)?.unwrap_or_else(|| resolved.item_id.clone());
        let client = self
            .get_client_with_context(
                resolved.provider_instance_name.as_deref(),
                ctx.request_context(),
            )
            .await?;

        let page = query.page.max(1);
        let page_size = query.page_size.max(1);
        let list_req = synctv_media_providers::grpc::emby::FsListReq {
            host: resolved.host.clone(),
            token: resolved.token.clone(),
            path: target_item_id,
            start_index: dynamic_list_start_index(page, page_size)?,
            limit: usize_to_u64(page_size, "Emby page size")?,
            search_term: query.search.unwrap_or_default(),
            user_id: resolved.user_id.clone(),
        };

        let response = client.fs_list(list_req).await?;
        let items = response
            .items
            .into_iter()
            .filter_map(|item| {
                let item_type = Self::item_type_from_listing(&item)?;
                Some((item, item_type))
            })
            .map(|(item, item_type)| {
                let credential_owner_id = ctx
                    .credential_owner_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let public_credential_owner_id = ctx
                    .public_credential_owner_id()
                    .map_or_else(|| credential_owner_id.to_string(), str::to_owned);
                let thumbnail_url = Self::build_thumbnail_url(
                    &base_config.server_id,
                    &public_credential_owner_id,
                    &item.id,
                );

                Ok(DirectoryItem {
                    name: item.name,
                    target: Self::encode_target(&item.id)?,
                    item_type,
                    size: None,
                    thumbnail: Some(thumbnail_url),
                    description: (!item.description.trim().is_empty()).then_some(item.description),
                    modified_at: None,
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;

        Ok(items)
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let item_id = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Emby target is required".to_string()))?;
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbySourceConfig::try_from(config)?;
        let resolved = self.resolve_config(ctx, config).await?;
        let item = self
            .fetch_item(&resolved, &item_id, ctx.request_context())
            .await?;
        let Some(item_type) = Self::item_type_from_listing(&item) else {
            return Ok(None);
        };
        if item_type != ItemType::Media {
            return Ok(None);
        }

        Ok(Some(NextPlayItem {
            name: item.name,
            item_type,
            source_config: Self::build_next_source_config(&base_config, &item_id),
            metadata: json!({}),
            provider_data: json!({}),
            target: Self::encode_target(&item_id)?,
        }))
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
        let item_id = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Emby target is required".to_string()))?;

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbySourceConfig::try_from(config)?;
        let resolved = self.resolve_config(ctx, config).await?;
        let current_item = self
            .fetch_item(&resolved, &item_id, ctx.request_context())
            .await?;
        let sibling_parent_id = if current_item.parent_id.is_empty() {
            base_config.item_id.clone()
        } else {
            current_item.parent_id.clone()
        };
        let sibling_target = Self::encode_target(&sibling_parent_id)?;

        match play_mode {
            PlayMode::RepeatOne => Ok(None),
            PlayMode::Sequential | PlayMode::RepeatAll => {
                const PAGE_SIZE: usize = 50;
                let mut found_current = false;
                let mut current_page = 1;

                loop {
                    let page_items = self
                        .list_playlist(
                            ctx,
                            playlist,
                            Some(&sibling_target),
                            DynamicListQuery {
                                page: current_page,
                                page_size: PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
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
                                source_config: Self::build_next_source_config(
                                    &base_config,
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Emby item target".to_string(),
                                        )
                                    })?,
                                ),
                                metadata: json!({}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }));
                        }
                    } else if let Some(idx) =
                        page_items.iter().position(|item| item.target == target)
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
                                source_config: Self::build_next_source_config(
                                    &base_config,
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Emby item target".to_string(),
                                        )
                                    })?,
                                ),
                                metadata: json!({}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }));
                        }
                    }

                    if page_items.len() < PAGE_SIZE {
                        break;
                    }
                    current_page += 1;
                }

                if found_current && play_mode == PlayMode::RepeatAll {
                    let first_page = self
                        .list_playlist(
                            ctx,
                            playlist,
                            Some(&sibling_target),
                            DynamicListQuery {
                                page: 1,
                                page_size: PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
                        )
                        .await?;

                    if let Some(first) = first_page
                        .iter()
                        .find(|item| item.item_type == ItemType::Media)
                    {
                        return Ok(Some(NextPlayItem {
                            name: first.name.clone(),
                            item_type: first.item_type,
                            source_config: Self::build_next_source_config(
                                &base_config,
                                &Self::decode_target(Some(&first.target))?.ok_or_else(|| {
                                    ProviderError::InvalidConfig(
                                        "Missing Emby item target".to_string(),
                                    )
                                })?,
                            ),
                            metadata: json!({}),
                            provider_data: json!({}),
                            target: first.target.clone(),
                        }));
                    }
                }

                Ok(None)
            }
            PlayMode::Shuffle => {
                const PAGE_SIZE: usize = 50;
                const MAX_ITEMS: usize = 200;
                let mut all_items = Vec::with_capacity(MAX_ITEMS);
                let mut page = 1;
                loop {
                    let page_items = self
                        .list_playlist(
                            ctx,
                            playlist,
                            Some(&sibling_target),
                            DynamicListQuery {
                                page,
                                page_size: PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
                        )
                        .await?;
                    let is_last_page = page_items.len() < PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page || all_items.len() >= MAX_ITEMS {
                        break;
                    }
                    page += 1;
                }
                all_items.truncate(MAX_ITEMS);
                let playable_items: Vec<_> = all_items
                    .iter()
                    .filter(|item| item.item_type == ItemType::Media)
                    .collect();

                if playable_items.is_empty() {
                    return Ok(None);
                }

                let mut rng = rand::rng();
                let candidates: Vec<_> = playable_items
                    .iter()
                    .filter(|item| item.target != target)
                    .collect();

                let random_item = if candidates.is_empty() {
                    playable_items.choose(&mut rng).copied()
                } else {
                    candidates.choose(&mut rng).copied().copied()
                };

                if let Some(random) = random_item {
                    Ok(Some(NextPlayItem {
                        name: random.name.clone(),
                        item_type: random.item_type,
                        source_config: Self::build_next_source_config(
                            &base_config,
                            &Self::decode_target(Some(&random.target))?.ok_or_else(|| {
                                ProviderError::InvalidConfig("Missing Emby item target".to_string())
                            })?,
                        ),
                        metadata: json!({}),
                        provider_data: json!({}),
                        target: random.target.clone(),
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    }

    async fn browse_path(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(mut current_id) = Self::decode_target(target)? else {
            return Ok(Vec::new());
        };

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbySourceConfig::try_from(config)?;
        let resolved = self.resolve_config(ctx, config).await?;

        let mut segments = Vec::new();
        for _ in 0..32 {
            if current_id == base_config.item_id {
                break;
            }

            let item = self
                .fetch_item(&resolved, &current_id, ctx.request_context())
                .await?;
            segments.push(DynamicBrowsePathSegment {
                name: item.name,
                target: Self::encode_target(&current_id)?,
            });

            if item.parent_id.is_empty() {
                break;
            }
            current_id = item.parent_id;
        }

        segments.reverse();
        Ok(segments)
    }
}
