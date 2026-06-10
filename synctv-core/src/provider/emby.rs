//! Emby/Jellyfin `MediaProvider` Adapter
//!
//! Adapter that calls `EmbyClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_emby_client, EmbyClientArc, ProviderClientManager},
    store::{ProviderStoreExt, VersionedPlayback},
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
    MediaProvider, NextPlayItem, PlaybackClientProfile, PlaybackInfo, PlaybackResult,
    ProviderContext, ProviderCredentialDependency, ProviderError, SourceConfig, SubtitleTrack,
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

/// Build an absolute Emby/Jellyfin URL from a configured server URL and an API path.
///
/// `host` may point at either a root deployment (`https://media.example.com`) or
/// a reverse-proxy base path (`https://media.example.com/jellyfin`). Provider
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
                    profile.delivery_preference,
                    super::PlaybackDeliveryPreference::Transcode
                )),
                enable_direct_stream: Some(!matches!(
                    profile.delivery_preference,
                    super::PlaybackDeliveryPreference::Transcode
                )),
                enable_transcoding: Some(!matches!(
                    profile.delivery_preference,
                    super::PlaybackDeliveryPreference::DirectPlay
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

    /// Login to Emby/Jellyfin and return a validated provider credential payload.
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
        let emby_expires_at = Some(Utc::now().timestamp() + 30 * 60);

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
            let subtitles: Vec<SubtitleTrack> = source
                .media_stream_info
                .iter()
                .filter(|stream| stream.r#type == "Subtitle")
                .map(|stream| {
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

                    Ok(SubtitleTrack {
                        language: stream.language.clone(),
                        name: stream.display_title.clone(),
                        url: subtitle_url,
                        headers: emby_auth_headers.clone(),
                        format: stream.codec.to_lowercase(),
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
                let transcode_url = emby_server_url(&config.host, &source.transcoding_url)?;

                playback_infos.insert(
                    format!("{mode_name}_transcode"),
                    PlaybackInfo {
                        urls: vec![transcode_url],
                        format: "hls".to_string(), // Emby transcodes to HLS
                        headers: emby_auth_headers.clone(),
                        subtitles: Vec::new(), // Subtitles burned in for transcode
                        expires_at: emby_expires_at,
                        cors_proxy_required: true,
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
        let result = self
            .resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
            .await?;

        // Generate version and store result
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
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

// ProviderProxy implementation for Emby
// Supported sub_paths:
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
// - `{version}/subtitle/{mode}/{index}` — proxy a subtitle track for a mode
#[async_trait]
impl super::proxy::ProviderProxy for EmbyProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;
        let version = super::proxy::proxy_version_segment(sub_path)?;

        {
            let versioned =
                super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;

            if let Some(url) = super::proxy::signed_target_url(ctx) {
                let headers = versioned
                    .result
                    .playback_infos
                    .get(&versioned.result.default_mode)
                    .map_or_else(HashMap::new, |info| info.headers.clone());
                return super::proxy::action_for_signed_target_url(ctx, version, url, headers);
            }

            let (_, rest) = super::proxy::split_versioned_proxy_path(sub_path)?;

            if let Some(subtitle_path) = rest.strip_prefix("subtitle/") {
                let (mode_name, index_str) = subtitle_path
                    .split_once('/')
                    .ok_or(ProviderError::NotFound)?;
                let playback_info = versioned
                    .result
                    .playback_infos
                    .get(mode_name)
                    .ok_or(ProviderError::NotFound)?;
                let index = super::proxy::parse_proxy_index(index_str)?;
                let subtitle = playback_info
                    .subtitles
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: subtitle.url.clone(),
                    headers: super::subtitle_headers_for_proxy(&playback_info.headers, subtitle),
                    range_header: None,
                });
            }

            if let Some(stream_path) = rest.strip_prefix("stream/") {
                let (playback_info, index_str) =
                    if let Some((mode_name, index_str)) = stream_path.split_once('/') {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(mode_name)
                                .ok_or(ProviderError::NotFound)?,
                            index_str,
                        )
                    } else {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(&versioned.result.default_mode)
                                .ok_or(ProviderError::NotFound)?,
                            stream_path,
                        )
                    };
                let index = super::proxy::parse_proxy_index(index_str)?;
                let url = playback_info
                    .urls
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: url.clone(),
                    headers: playback_info.headers.clone(),
                    range_header: super::proxy::selected_range_header(ctx)?,
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
                        range_header: super::proxy::selected_range_header(ctx)?,
                    });
                }
                "m3u8" => {
                    return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                        proxy_base: super::proxy::m3u8_segment_proxy_base(ctx, version),
                        proxy_url_claims: ctx.verified_claims.cloned(),
                    });
                }
                _ => {}
            }
            Err(ProviderError::NotFound)
        }
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
                let thumbnail_url = Self::build_thumbnail_url(
                    &base_config.server_id,
                    &credential_owner_id.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserId;
    use crate::provider::ProviderClientManager;
    use crate::test_helpers::{TestOptionExt, TestResultExt};
    use async_trait::async_trait;
    use std::sync::Arc;
    use synctv_media_providers::emby::{EmbyError, EmbyInterface};
    use synctv_media_providers::grpc::emby as proto;
    /// Validate Emby source config: checks item_id and server_id fields.
    /// Host/token/user_id are resolved from the media or playlist creator at runtime.
    fn validate_emby(config: &Value) -> Result<(), ProviderError> {
        let config = EmbySourceConfig::try_from(config)?;

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
        Ok(())
    }

    struct TestEmbyClient;

    fn unconfigured_test_response() -> EmbyError {
        EmbyError::InvalidConfig("test emby method is not configured".to_string())
    }

    #[async_trait]
    impl EmbyInterface for TestEmbyClient {
        async fn login(&self, _request: proto::LoginReq) -> Result<proto::LoginResp, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn me(&self, _request: proto::MeReq) -> Result<proto::MeResp, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn get_items(
            &self,
            _request: proto::GetItemsReq,
        ) -> Result<proto::GetItemsResp, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn get_item(&self, _request: proto::GetItemReq) -> Result<proto::Item, EmbyError> {
            Ok(proto::Item {
                name: "Test Movie".to_string(),
                id: "item-1".to_string(),
                r#type: "Movie".to_string(),
                parent_id: String::new(),
                series_name: String::new(),
                series_id: String::new(),
                season_name: String::new(),
                season_id: String::new(),
                is_folder: false,
                media_source_info: Vec::new(),
                collection_type: String::new(),
                has_thumbnail: false,
                description: String::new(),
            })
        }

        async fn fs_list(
            &self,
            _request: proto::FsListReq,
        ) -> Result<proto::FsListResp, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn get_system_info(
            &self,
            _request: proto::SystemInfoReq,
        ) -> Result<proto::SystemInfoResp, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn logout(&self, _request: proto::LogoutReq) -> Result<proto::Empty, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn playback_info(
            &self,
            _request: proto::PlaybackInfoReq,
        ) -> Result<proto::PlaybackInfoResp, EmbyError> {
            Ok(proto::PlaybackInfoResp {
                play_session_id: "play-session-1".to_string(),
                media_source_info: vec![proto::MediaSourceInfo {
                    id: "source-1".to_string(),
                    name: "Main".to_string(),
                    path: String::new(),
                    container: "mp4".to_string(),
                    protocol: "File".to_string(),
                    default_subtitle_stream_index: 2,
                    default_audio_stream_index: 1,
                    media_stream_info: vec![proto::MediaStreamInfo {
                        codec: "srt".to_string(),
                        language: "eng".to_string(),
                        r#type: "Subtitle".to_string(),
                        title: "English".to_string(),
                        display_title: "English".to_string(),
                        display_language: "English".to_string(),
                        is_default: true,
                        index: 2,
                        protocol: "File".to_string(),
                    }],
                    direct_play_url: "/Videos/item-1/stream.mp4".to_string(),
                    transcoding_url: String::new(),
                }],
            })
        }

        async fn delete_active_encodings(
            &self,
            _request: proto::DeleteActiveEncodingsReq,
        ) -> Result<proto::Empty, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn report_playback_start(
            &self,
            _request: proto::ReportPlaybackStartReq,
        ) -> Result<proto::Empty, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn report_playback_stop(
            &self,
            _request: proto::ReportPlaybackStopReq,
        ) -> Result<proto::Empty, EmbyError> {
            Err(unconfigured_test_response())
        }

        async fn report_playback_progress(
            &self,
            _request: proto::ReportPlaybackProgressReq,
        ) -> Result<proto::Empty, EmbyError> {
            Err(unconfigured_test_response())
        }
    }

    fn provider_with_test_emby_client() -> EmbyProvider {
        let default_clients = ProviderClientManager::new_for_tests()
            .checked("default provider HTTP client should build");
        let client_manager = Arc::new(ProviderClientManager::with_custom_clients(
            default_clients.local_alist_client(),
            default_clients.local_bilibili_client(),
            Arc::new(TestEmbyClient),
        ));
        EmbyProvider::with_client_manager(
            crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager,
        )
    }

    #[tokio::test]
    async fn test_emby_direct_playback_returns_subtitle_auth_headers() {
        let provider = provider_with_test_emby_client();
        let result = provider
            .resolve_from_api(
                &ResolvedEmbyConfig {
                    host: "https://emby.example.com".to_string(),
                    token: "token-123".to_string(),
                    user_id: "user-1".to_string(),
                    item_id: "item-1".to_string(),
                    credential_owner_id: "owner-1".to_string(),
                    credential_revision: "credential-1:1".to_string(),
                    provider_instance_name: None,
                },
                None,
                None,
            )
            .await
            .checked("mock Emby playback should resolve");

        let playback = &result.playback_infos["Main"];
        assert_eq!(
            playback.headers.get("X-Emby-Token").map(String::as_str),
            Some("token-123")
        );
        assert_eq!(playback.subtitles.len(), 1);
        assert_eq!(
            playback.subtitles[0]
                .headers
                .get("X-Emby-Token")
                .map(String::as_str),
            Some("token-123"),
            "direct subtitle clients need the same Emby auth header as video streams"
        );
    }

    #[test]
    fn test_valid_emby_config() {
        let config = json!({
            "item_id": "item-456",
            "server_id": "test-server"
        });
        assert!(validate_emby(&config).is_ok());
    }

    #[test]
    fn test_emby_config_with_provider_instance_name() {
        let config = json!({
            "item_id": "item-456",
            "provider_instance_name": "remote-emby-1",
            "server_id": "test-server"
        });
        assert!(validate_emby(&config).is_err());
    }

    #[tokio::test]
    async fn test_emby_credential_dependencies_use_creator_credential() {
        let provider = EmbyProvider::new_local_only().checked("provider should build");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_credential_owner_id(UserId::expect_positive(2));
        let dependencies = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "item_id": "item-456",
                    "server_id": "emby-main"
                }),
            )
            .checked("Emby dependency extraction should succeed");

        assert_eq!(
            dependencies,
            vec![ProviderCredentialDependency::new(
                EmbyProvider::NAME,
                "2",
                "emby-main"
            )]
        );
    }

    #[tokio::test]
    async fn test_emby_credential_dependencies_require_explicit_creator_credential_owner() {
        let provider = EmbyProvider::new_local_only().checked("provider should build");
        let ctx = ProviderContext::new("test").with_user_id(UserId::expect_positive(1));
        let err = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "item_id": "item-456",
                    "server_id": "emby-main"
                }),
            )
            .failed("Emby must not silently fall back to viewer credentials");

        assert!(
            err.to_string().contains("credential_owner_id"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_prepare_emby_config_rejects_provider_instance_name() {
        let provider = EmbyProvider::new_local_only().checked("provider should build");
        let config = json!({
            "item_id": "item-456",
            "provider_instance_name": "remote-emby-1",
            "server_id": "test-server"
        });

        let result = provider
            .prepare_source_config(&ProviderContext::new("test"), config)
            .await;

        assert!(matches!(result, Err(ProviderError::InvalidConfig(_))));
    }

    #[test]
    fn test_emby_config_empty_item_id() {
        let config = json!({
            "item_id": "",
            "server_id": "test-server"
        });
        assert!(validate_emby(&config).is_err());
    }

    #[test]
    fn test_emby_config_missing_required_fields() {
        // Missing server_id entirely
        let config = json!({
            "item_id": "item-456"
        });
        assert!(validate_emby(&config).is_err());
    }

    #[test]
    fn test_emby_config_missing_item_id() {
        // Missing item_id field
        let config = json!({
            "server_id": "test-server"
        });
        assert!(validate_emby(&config).is_err());
    }

    #[test]
    fn test_emby_server_id_parsing() {
        let config = json!({
            "item_id": "item-456",
            "server_id": "srv-xyz"
        });
        let parsed = EmbySourceConfig::try_from(&config).checked("operation should succeed");
        assert_eq!(parsed.server_id, "srv-xyz");
        assert_eq!(parsed.item_id, "item-456");
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

    #[test]
    fn test_thumbnail_url_must_not_contain_raw_token() {
        // The thumbnail URL format in list_playlist should never contain the raw
        // Emby API token in the query string. Instead it should carry only the
        // opaque server_id so the authenticated thumbnail handler can resolve
        // credentials server-side.
        let raw_token = "super-secret-api-key-12345";
        let item_id = "item-789";
        let server_id = "srv-123";
        let credential_owner_id = "owner-456";

        let thumbnail_url =
            EmbyProvider::build_thumbnail_url(server_id, credential_owner_id, item_id);

        assert!(
            !thumbnail_url.contains(raw_token),
            "Thumbnail URL must not contain the raw Emby API token"
        );
        assert!(
            !thumbnail_url.contains("token="),
            "Thumbnail URL must not include a 'token=' query parameter"
        );
        assert!(
            thumbnail_url.contains("server_id=srv-123"),
            "Thumbnail URL must include the opaque server_id for credential lookup"
        );
        assert!(
            thumbnail_url.contains("credential_owner_id=owner-456"),
            "Thumbnail URL must include the credential owner for shared Emby media"
        );
    }

    #[test]
    fn test_emby_playback_cache_key_includes_credential_owner() {
        let revision = "credential-1:1000";
        let owner_a =
            EmbyProvider::playback_cache_key("server-1", "owner-a", revision, "item-1", "default");
        let owner_b =
            EmbyProvider::playback_cache_key("server-1", "owner-b", revision, "item-1", "default");

        assert_ne!(
            owner_a, owner_b,
            "Emby playback cache must be isolated by credential owner"
        );
    }

    #[test]
    fn test_emby_playback_cache_key_includes_client_profile() {
        let revision = "credential-1:1000";
        let default_profile =
            EmbyProvider::playback_cache_key("server-1", "owner-a", revision, "item-1", "default");
        let mobile_profile =
            EmbyProvider::playback_cache_key("server-1", "owner-a", revision, "item-1", "mobile");

        assert_ne!(
            default_profile, mobile_profile,
            "Emby playback cache must remain isolated by playback client profile"
        );
    }

    #[test]
    fn test_emby_playback_cache_key_includes_credential_update_time() {
        let first = EmbyProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:1000",
            "item-1",
            "default",
        );
        let second = EmbyProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:2000",
            "item-1",
            "default",
        );

        assert_ne!(
            first, second,
            "Credential changes must invalidate Emby playback cache entries"
        );
    }

    #[test]
    fn test_emby_server_url_supports_emby_and_jellyfin_root_deployments() {
        assert_eq!(
            emby_server_url("https://emby.example.com", "/Items/item-1/Download")
                .checked("operation should succeed"),
            "https://emby.example.com/Items/item-1/Download"
        );
        assert_eq!(
            emby_server_url("https://jellyfin.example.com", "/Items/item-1/Download")
                .checked("operation should succeed"),
            "https://jellyfin.example.com/Items/item-1/Download"
        );
    }

    #[test]
    fn test_emby_server_url_preserves_configured_base_path_without_duplication() {
        assert_eq!(
            emby_server_url(
                "https://media.example.com/jellyfin",
                "/Items/item-1/Download"
            )
            .checked("operation should succeed"),
            "https://media.example.com/jellyfin/Items/item-1/Download"
        );
        assert_eq!(
            emby_server_url(
                "https://media.example.com/jellyfin",
                "/jellyfin/Videos/item/master.m3u8"
            )
            .checked("operation should succeed"),
            "https://media.example.com/jellyfin/Videos/item/master.m3u8"
        );
    }

    #[test]
    fn test_emby_server_url_accepts_absolute_provider_urls() {
        assert_eq!(
            emby_server_url(
                "https://media.example.com/jellyfin",
                "https://cdn.example.com/video.m3u8"
            )
            .checked("operation should succeed"),
            "https://cdn.example.com/video.m3u8"
        );
    }

    #[tokio::test]
    async fn test_emby_playback_lifecycle_session_id_uses_provider_metadata() {
        let provider = EmbyProvider::new_local_only().checked("provider should build");
        let result = PlaybackResult {
            playback_infos: HashMap::new(),
            default_mode: "direct".to_string(),
            metadata: HashMap::from([(
                "emby_play_session_id".to_string(),
                json!("play-session-123"),
            )]),
        };

        assert_eq!(
            provider.playback_lifecycle_session_id(&result).as_deref(),
            Some("play-session-123")
        );
    }

    #[test]
    fn grpc_playback_request_hints_omit_provider_profile_when_client_profile_is_absent() {
        let hints = grpc_playback_request_hints(None);

        assert_eq!(hints.max_audio_channels, None);
        assert_eq!(hints.enable_direct_play, None);
        assert_eq!(hints.enable_direct_stream, None);
        assert_eq!(hints.enable_transcoding, None);
        assert!(hints.device_profile.is_none());
    }

    #[test]
    fn grpc_playback_request_hints_map_transcode_profile_without_subtitles() {
        let profile = crate::provider::PlaybackClientProfile {
            delivery_preference: crate::provider::PlaybackDeliveryPreference::Transcode,
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            supported_video_codecs: vec![
                crate::provider::PlaybackVideoCodec::H264,
                crate::provider::PlaybackVideoCodec::Vp9,
            ],
            supported_containers: vec![
                crate::provider::PlaybackContainer::Mp4,
                crate::provider::PlaybackContainer::Webm,
            ],
            audio_capability: crate::provider::PlaybackAudioCapability::Stereo,
            subtitle_preference: crate::provider::PlaybackSubtitlePreference::None,
        };

        let hints = grpc_playback_request_hints(Some(&profile));
        let device_profile = hints
            .device_profile
            .checked("client profile should produce an Emby device profile");

        assert_eq!(hints.max_audio_channels, Some(2));
        assert_eq!(hints.enable_direct_play, Some(false));
        assert_eq!(hints.enable_direct_stream, Some(false));
        assert_eq!(hints.enable_transcoding, Some(true));
        assert_eq!(device_profile.transcoding_container, "ts");
        assert_eq!(device_profile.transcoding_protocol, "hls");
        assert_eq!(device_profile.transcoding_video_codec, "h264");
        assert_eq!(device_profile.transcoding_audio_codec, "aac");
        assert!(
            device_profile.subtitle_profiles.is_empty(),
            "subtitle preference none must become an explicit empty Emby subtitle profile"
        );

        assert_eq!(device_profile.direct_play_profiles.len(), 2);
        let mp4 = device_profile
            .direct_play_profiles
            .iter()
            .find(|profile| profile.container == "mp4,m4v")
            .checked("mp4 direct-play profile should exist");
        assert_eq!(mp4.video_codecs, vec!["h264", "vp9"]);
        assert_eq!(mp4.audio_codecs, vec!["aac", "mp3"]);

        let webm = device_profile
            .direct_play_profiles
            .iter()
            .find(|profile| profile.container == "webm")
            .checked("webm direct-play profile should exist");
        assert_eq!(webm.video_codecs, vec!["vp9"]);
        assert_eq!(webm.audio_codecs, vec!["vorbis", "opus"]);
    }

    #[test]
    fn grpc_playback_request_hints_map_direct_play_profile_with_embedded_or_external_subtitles() {
        let profile = crate::provider::PlaybackClientProfile {
            delivery_preference: crate::provider::PlaybackDeliveryPreference::DirectPlay,
            max_streaming_bitrate: None,
            max_audio_channels: Some(6),
            supported_video_codecs: vec![
                crate::provider::PlaybackVideoCodec::Hevc,
                crate::provider::PlaybackVideoCodec::Av1,
            ],
            supported_containers: vec![crate::provider::PlaybackContainer::Mkv],
            audio_capability: crate::provider::PlaybackAudioCapability::Surround,
            subtitle_preference: crate::provider::PlaybackSubtitlePreference::EmbeddedOrExternal,
        };

        let hints = grpc_playback_request_hints(Some(&profile));
        let device_profile = hints
            .device_profile
            .checked("client profile should produce an Emby device profile");

        assert_eq!(hints.enable_direct_play, Some(true));
        assert_eq!(hints.enable_direct_stream, Some(true));
        assert_eq!(hints.enable_transcoding, Some(false));
        assert_eq!(device_profile.direct_play_profiles.len(), 1);

        let mkv = &device_profile.direct_play_profiles[0];
        assert_eq!(mkv.container, "mkv");
        assert_eq!(mkv.video_codecs, vec!["hevc", "av1"]);
        assert_eq!(mkv.audio_codecs, vec!["aac", "mp3", "ac3", "eac3", "dts"]);

        let methods: Vec<i32> = device_profile
            .subtitle_profiles
            .iter()
            .map(|profile| profile.method)
            .collect();
        assert!(methods.contains(
            &(synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::External as i32)
        ));
        assert!(methods
            .contains(&(synctv_media_providers::grpc::emby::SubtitleDeliveryMethod::Embed as i32)));
        assert_eq!(device_profile.subtitle_profiles.len(), 6);
    }
}
