//! Emby `MediaProvider` Adapter
//!
//! Adapter that calls `EmbyClient` to implement `MediaProvider` trait

use super::{
    access::EmbyAccess,
    provider_client::{create_remote_emby_client, EmbyClientArc, ProviderClientManager},
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistItemSourceConfig, DynamicPlaylistItemThumbnail,
    DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem, PlaybackClientProfile,
    PlaybackInfo, PlaybackResult, PreparedSourceConfig, ProviderContext,
    ProviderCredentialDependency, ProviderError, ProviderPlaybackSessionLifecycle, SourceConfig,
    SourceCover,
};
use crate::models::media::{
    EmbyPlaybackKind, EmbyPlaybackMetadata, PlaybackEmbyMedia, PlaybackEmbySubtitle, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{
    normalize_provider_instance_name, validate_provider_instance_name, EmbyMediaSourceConfig,
    EmbyPlaybackSession, EmbyPlaylistSource, EmbyPlaylistSourceConfig, EmbyTarget,
    MediaSourceConfig, PlaylistSourceConfig, ProviderCredential, ProviderPlaybackSession, RoomId,
    UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use crate::service::RemoteProviderManager;
use async_trait::async_trait;
use rand::prelude::IndexedRandom;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::upstream_transport::emby as emby_upstream;

const EMBY_TICKS_PER_SECOND: u128 = 10_000_000;

#[derive(Debug, Clone, Copy)]
pub struct EmbyHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

fn emby_item_types(item_types: &[String]) -> Vec<String> {
    if item_types.is_empty() {
        return vec![
            "Movie".to_string(),
            "Episode".to_string(),
            "Video".to_string(),
        ];
    }
    item_types
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

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

fn emby_play_session_id(result: &PlaybackResult) -> Option<String> {
    result
        .metadata
        .as_ref()
        .and_then(PlaybackMetadata::as_emby)
        .and_then(|metadata| metadata.play_session_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn emby_playback_resource_version(result: &PlaybackResult) -> Option<String> {
    result.playback_infos.values().find_map(|info| {
        info.medias.iter().find_map(|media| match &media.provider {
            PlaybackMediaProvider::Emby(
                PlaybackEmbyMedia::ProxyMediaStream { version, .. }
                | PlaybackEmbyMedia::ProxyHlsManifest { version, .. },
            ) => Some(version.clone()),
            _ => None,
        })
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

fn optional_i64_to_upstream_absent_zero(value: Option<i64>) -> i64 {
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
        p2p_swarm_id: None,
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
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(url_index, media)| {
                let url = media.upstream_url()?.to_string();
                let mut proxy = playback_media(
                    media.name.clone(),
                    media.format.clone(),
                    media.expire_at.map(|dt| dt.timestamp()),
                    PlaybackMediaProvider::Emby(
                        if super::playback_media_is_hls(&mode_name, media) {
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
                        },
                    ),
                );
                proxy.p2p_swarm_id.clone_from(&media.p2p_swarm_id);
                Some(proxy)
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
                p2p_swarm_id: subtitle.p2p_swarm_id.clone(),
                provider: PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Proxy {
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

/// Build an absolute Emby upstream URL from a configured server URL and item path.
///
/// `host` may point at either a root deployment (`https://media.example.com`) or
/// a reverse-proxy base path (`https://media.example.com/emby`). Provider
/// responses may include paths with or without that base path, so this helper
/// preserves the configured base path without duplicating it.
pub(crate) fn emby_server_url(host: &str, path_or_url: &str) -> Result<String, ProviderError> {
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

struct EmbyPlaybackRequestHints {
    max_audio_channels: Option<i32>,
    enable_direct_play: Option<bool>,
    enable_direct_stream: Option<bool>,
    enable_transcoding: Option<bool>,
    device_profile: Option<EmbyPlaybackDeviceProfile>,
}

struct EmbyPlaybackDeviceProfile {
    direct_play_profiles: Vec<EmbyDirectPlayProfileHint>,
    transcoding_container: String,
    transcoding_protocol: String,
    transcoding_video_codec: String,
    transcoding_audio_codec: String,
    subtitle_profiles: Vec<EmbySubtitleProfileHint>,
}

struct EmbyDirectPlayProfileHint {
    container: String,
    video_codecs: Vec<String>,
    audio_codecs: Vec<String>,
}

struct EmbySubtitleProfileHint {
    format: String,
    method: EmbySubtitleDeliveryMethod,
}

#[derive(Clone, Copy)]
enum EmbySubtitleDeliveryMethod {
    External,
    Embed,
}

fn emby_playback_request_hints(
    profile: Option<&PlaybackClientProfile>,
) -> EmbyPlaybackRequestHints {
    profile.map_or(
        EmbyPlaybackRequestHints {
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
                    vec![EmbySubtitleDeliveryMethod::External]
                }
                super::PlaybackSubtitlePreference::EmbeddedOrExternal => vec![
                    EmbySubtitleDeliveryMethod::External,
                    EmbySubtitleDeliveryMethod::Embed,
                ],
                super::PlaybackSubtitlePreference::None => Vec::new(),
            };

            EmbyPlaybackRequestHints {
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
                device_profile: Some(EmbyPlaybackDeviceProfile {
                    direct_play_profiles: profile
                        .supported_containers
                        .iter()
                        .map(|container| match container {
                            super::PlaybackContainer::Mp4 => EmbyDirectPlayProfileHint {
                                container: "mp4,m4v".to_string(),
                                video_codecs: profile
                                    .supported_video_codecs
                                    .iter()
                                    .map(|codec| match codec {
                                        super::PlaybackVideoCodec::H264 => "h264".to_string(),
                                        super::PlaybackVideoCodec::Hevc => "hevc".to_string(),
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
                            },
                            super::PlaybackContainer::Mkv => EmbyDirectPlayProfileHint {
                                container: "mkv".to_string(),
                                video_codecs: profile
                                    .supported_video_codecs
                                    .iter()
                                    .map(|codec| match codec {
                                        super::PlaybackVideoCodec::H264 => "h264".to_string(),
                                        super::PlaybackVideoCodec::Hevc => "hevc".to_string(),
                                        super::PlaybackVideoCodec::Vp9 => "vp9".to_string(),
                                        super::PlaybackVideoCodec::Av1 => "av1".to_string(),
                                    })
                                    .collect(),
                                audio_codecs: direct_play_audio_codecs
                                    .iter()
                                    .map(|codec| (*codec).to_string())
                                    .collect(),
                            },
                            super::PlaybackContainer::Webm => EmbyDirectPlayProfileHint {
                                container: "webm".to_string(),
                                video_codecs: profile
                                    .supported_video_codecs
                                    .iter()
                                    .filter_map(|codec| match codec {
                                        super::PlaybackVideoCodec::H264
                                        | super::PlaybackVideoCodec::Hevc => None,
                                        super::PlaybackVideoCodec::Vp9 => Some("vp9".to_string()),
                                        super::PlaybackVideoCodec::Av1 => Some("av1".to_string()),
                                    })
                                    .collect(),
                                audio_codecs: vec!["vorbis".to_string(), "opus".to_string()],
                            },
                        })
                        .collect(),
                    transcoding_container: "ts".to_string(),
                    transcoding_protocol: "hls".to_string(),
                    transcoding_video_codec: "h264".to_string(),
                    transcoding_audio_codec: "aac".to_string(),
                    subtitle_profiles: match profile.subtitle_preference {
                        super::PlaybackSubtitlePreference::None => Vec::new(),
                        super::PlaybackSubtitlePreference::External
                        | super::PlaybackSubtitlePreference::EmbeddedOrExternal => subtitle_methods
                            .into_iter()
                            .flat_map(|method| {
                                ["srt", "vtt", "ass"].into_iter().map(move |format| {
                                    EmbySubtitleProfileHint {
                                        format: format.to_string(),
                                        method,
                                    }
                                })
                            })
                            .collect(),
                    },
                }),
            }
        },
    )
}

fn emby_auth_headers(token: &str) -> HashMap<String, String> {
    HashMap::from([("X-Emby-Token".to_string(), token.to_string())])
}

fn emby_get_item_request(
    resolved: &ResolvedEmbyConfig,
    item_id: &str,
) -> emby_upstream::GetItemReq {
    emby_upstream::GetItemReq {
        host: resolved.host.clone(),
        token: resolved.token.clone(),
        item_id: item_id.to_string(),
        user_id: resolved.user_id.clone(),
    }
}

fn emby_playback_info_request(
    config: &ResolvedEmbyConfig,
    playback_client_profile: Option<&PlaybackClientProfile>,
    playback_hints: EmbyPlaybackRequestHints,
) -> emby_upstream::PlaybackInfoReq {
    emby_upstream::PlaybackInfoReq {
        host: config.host.clone(),
        token: config.token.clone(),
        user_id: config.user_id.clone(),
        item_id: config.item_id.clone(),
        media_source_id: String::new(),
        audio_stream_index: 0,
        subtitle_stream_index: 0,
        max_streaming_bitrate: optional_i64_to_upstream_absent_zero(
            playback_client_profile.and_then(|profile| profile.max_streaming_bitrate),
        ),
        max_audio_channels: playback_hints.max_audio_channels,
        enable_direct_play: playback_hints.enable_direct_play,
        enable_direct_stream: playback_hints.enable_direct_stream,
        enable_transcoding: playback_hints.enable_transcoding,
        device_profile: playback_hints
            .device_profile
            .map(emby_device_profile_to_upstream),
    }
}

fn emby_device_profile_to_upstream(
    profile: EmbyPlaybackDeviceProfile,
) -> emby_upstream::PlaybackInfoDeviceProfile {
    emby_upstream::PlaybackInfoDeviceProfile {
        direct_play_profiles: profile
            .direct_play_profiles
            .into_iter()
            .map(|hint| emby_upstream::DirectPlayProfileHint {
                container: hint.container,
                video_codecs: hint.video_codecs,
                audio_codecs: hint.audio_codecs,
            })
            .collect(),
        transcoding_container: profile.transcoding_container,
        transcoding_protocol: profile.transcoding_protocol,
        transcoding_video_codec: profile.transcoding_video_codec,
        transcoding_audio_codec: profile.transcoding_audio_codec,
        subtitle_profiles: profile
            .subtitle_profiles
            .into_iter()
            .map(|hint| emby_upstream::SubtitleProfileHint {
                format: hint.format,
                method: emby_subtitle_delivery_method_to_upstream(hint.method),
            })
            .collect(),
    }
}

fn emby_subtitle_delivery_method_to_upstream(method: EmbySubtitleDeliveryMethod) -> i32 {
    match method {
        EmbySubtitleDeliveryMethod::External => {
            emby_upstream::SubtitleDeliveryMethod::External as i32
        }
        EmbySubtitleDeliveryMethod::Embed => emby_upstream::SubtitleDeliveryMethod::Embed as i32,
    }
}

fn emby_dynamic_list_request(
    resolved: &ResolvedEmbyConfig,
    source: emby_upstream::fs_list_req::Source,
    page: usize,
    page_size: usize,
    search_term: String,
) -> Result<emby_upstream::FsListReq, ProviderError> {
    Ok(emby_upstream::FsListReq {
        host: resolved.host.clone(),
        token: resolved.token.clone(),
        user_id: resolved.user_id.clone(),
        source: Some(source),
        start_index: dynamic_list_start_index(page, page_size)?,
        limit: usize_to_u64(page_size, "Emby page size")?,
        search_term,
    })
}

fn emby_fs_list_request(req: EmbyListRequest) -> Result<emby_upstream::FsListReq, ProviderError> {
    let source = match req.source {
        EmbyPlaylistSource::Folder { item_id } => {
            emby_upstream::fs_list_req::Source::Folder(emby_upstream::EmbyFolderListSource {
                parent_id: item_id,
            })
        }
        EmbyPlaylistSource::FavoriteItems { item_types } => {
            emby_upstream::fs_list_req::Source::FavoriteItems(
                emby_upstream::EmbyFavoriteItemsListSource {
                    item_types: emby_item_types(&item_types),
                },
            )
        }
        EmbyPlaylistSource::FavoritePeople => emby_upstream::fs_list_req::Source::FavoritePeople(
            emby_upstream::EmbyFavoritePeopleListSource {},
        ),
        EmbyPlaylistSource::PersonItems {
            person_id,
            item_types,
        } => {
            if person_id.trim().is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Emby person_id must not be empty".to_string(),
                ));
            }
            emby_upstream::fs_list_req::Source::PersonItems(
                emby_upstream::EmbyPersonItemsListSource {
                    person_id,
                    item_types: emby_item_types(&item_types),
                },
            )
        }
        EmbyPlaylistSource::ContinueWatching => {
            emby_upstream::fs_list_req::Source::ContinueWatching(
                emby_upstream::EmbyContinueWatchingListSource {},
            )
        }
        EmbyPlaylistSource::NextUp => {
            emby_upstream::fs_list_req::Source::NextUp(emby_upstream::EmbyNextUpListSource {})
        }
        EmbyPlaylistSource::RecentlyAdded { item_types } => {
            emby_upstream::fs_list_req::Source::RecentlyAdded(
                emby_upstream::EmbyRecentlyAddedListSource {
                    item_types: emby_item_types(&item_types),
                },
            )
        }
        EmbyPlaylistSource::Playlists => {
            emby_upstream::fs_list_req::Source::Playlists(emby_upstream::EmbyPlaylistsListSource {})
        }
        EmbyPlaylistSource::Collections => emby_upstream::fs_list_req::Source::Collections(
            emby_upstream::EmbyCollectionsListSource {},
        ),
        EmbyPlaylistSource::Genres { item_types } => {
            emby_upstream::fs_list_req::Source::Genres(emby_upstream::EmbyGenresListSource {
                item_types: emby_item_types(&item_types),
            })
        }
        EmbyPlaylistSource::GenreItems {
            genre_id,
            item_types,
        } => {
            if genre_id.trim().is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Emby genre_id must not be empty".to_string(),
                ));
            }
            emby_upstream::fs_list_req::Source::GenreItems(
                emby_upstream::EmbyGenreItemsListSource {
                    genre_id,
                    item_types: emby_item_types(&item_types),
                },
            )
        }
    };
    Ok(emby_upstream::FsListReq {
        host: req.host,
        token: req.token,
        user_id: req.user_id,
        source: Some(source),
        start_index: req.start_index,
        limit: req.limit,
        search_term: req.search_term,
    })
}

fn emby_report_playback_start_request(
    config: &ResolvedEmbyConfig,
    session_id: &str,
) -> emby_upstream::ReportPlaybackStartReq {
    emby_upstream::ReportPlaybackStartReq {
        host: config.host.clone(),
        token: config.token.clone(),
        item_id: config.item_id.clone(),
        play_session_id: session_id.to_string(),
        media_source_id: String::new(),
        position_ticks: 0,
    }
}

fn emby_report_playback_stop_request(
    config: &ResolvedEmbyConfig,
    session_id: &str,
    position_ticks: i64,
) -> emby_upstream::ReportPlaybackStopReq {
    emby_upstream::ReportPlaybackStopReq {
        host: config.host.clone(),
        token: config.token.clone(),
        item_id: config.item_id.clone(),
        play_session_id: session_id.to_string(),
        position_ticks,
    }
}

fn emby_delete_active_encodings_request(
    config: &ResolvedEmbyConfig,
    session_id: &str,
) -> emby_upstream::DeleteActiveEncodingsReq {
    emby_upstream::DeleteActiveEncodingsReq {
        host: config.host.clone(),
        token: config.token.clone(),
        play_session_id: session_id.to_string(),
    }
}

fn emby_report_playback_progress_request(
    config: &ResolvedEmbyConfig,
    session_id: &str,
    position_ticks: i64,
    is_paused: bool,
) -> emby_upstream::ReportPlaybackProgressReq {
    emby_upstream::ReportPlaybackProgressReq {
        host: config.host.clone(),
        token: config.token.clone(),
        item_id: config.item_id.clone(),
        play_session_id: session_id.to_string(),
        media_source_id: String::new(),
        position_ticks,
        is_paused,
    }
}

/// Emby `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct EmbyProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub enum EmbyLoginCredential {
    Password(String),
    ApiKey(String),
}

#[derive(Debug, Clone)]
pub struct EmbyLoginRequest {
    pub host: String,
    pub username: String,
    pub credential: EmbyLoginCredential,
}

#[derive(Debug, Clone)]
pub struct EmbyUserPolicy {
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub is_disabled: bool,
    pub enable_all_folders: bool,
}

#[derive(Debug, Clone)]
pub struct EmbyLoginResponse {
    pub token: String,
    pub user_id: String,
    pub username: String,
    pub server_id: String,
    pub policy: Option<EmbyUserPolicy>,
}

#[derive(Debug, Clone)]
pub struct EmbyPersistedLoginResponse {
    pub login: EmbyLoginResponse,
    pub server_id: String,
}

#[derive(Debug, Clone)]
pub struct EmbyLoginAndPersistRequest {
    pub user_id: UserId,
    pub host: String,
    pub username: String,
    pub password: Option<String>,
    pub api_key: Option<String>,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EmbyBind {
    pub id: i64,
    pub server_id: String,
    pub host: String,
    pub emby_user_id: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EmbyListRequest {
    pub host: String,
    pub token: String,
    pub source: EmbyPlaylistSource,
    pub start_index: u64,
    pub limit: u64,
    pub search_term: String,
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct EmbyListItem {
    pub name: String,
    pub id: String,
    pub item_type: String,
    pub parent_id: String,
    pub series_name: String,
    pub series_id: String,
    pub season_name: String,
    pub season_id: String,
    pub is_folder: bool,
    pub collection_type: String,
    pub has_thumbnail: bool,
    pub description: String,
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct EmbyListResponse {
    pub items: Vec<EmbyListItem>,
    pub total: u64,
}

#[derive(Debug, Clone)]
struct EmbyItem {
    name: String,
    id: String,
    item_type: String,
    parent_id: String,
    series_name: String,
    series_id: String,
    season_name: String,
    season_id: String,
    is_folder: bool,
    collection_type: String,
    has_thumbnail: bool,
    description: String,
    duration_seconds: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct EmbyMeRequest {
    pub host: String,
    pub token: String,
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct EmbyMeResponse {
    pub id: String,
    pub name: String,
    pub server_id: String,
    pub policy: Option<EmbyUserPolicy>,
}

impl EmbyProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "emby";

    #[must_use]
    pub fn credential_server_id_for_instance(
        host: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        match normalize_provider_instance_name(provider_instance_name) {
            Some(instance_name) => hex::encode(Sha256::digest(
                format!("{host}\n{instance_name}").as_bytes(),
            )),
            None => hex::encode(Sha256::digest(host.as_bytes())),
        }
    }

    /// Create a new `EmbyProvider` with `RemoteProviderManager`
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()?),
            credential_repo: None,
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
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            provider_instance_manager: self.provider_instance_manager.clone(),
            client_manager: self.client_manager.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    fn credential_repo(&self) -> Result<&UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("Emby credential repository is not configured".to_string())
        })
    }

    #[cfg(test)]
    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager:
                crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager: Arc::new(ProviderClientManager::new()?),
            credential_repo: None,
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

    /// Login to Emby and return a validated provider credential session.
    pub fn resolve_login_request(
        host: String,
        username: String,
        password: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<EmbyLoginRequest, ProviderError> {
        if username.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby username must not be empty".to_string(),
            ));
        }

        let credential = match (password, api_key) {
            (Some(password), None) => EmbyLoginCredential::Password(password.to_string()),
            (None, Some(api_key)) => EmbyLoginCredential::ApiKey(api_key.to_string()),
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Emby login requires exactly one credential".to_string(),
                ));
            }
        };

        Ok(EmbyLoginRequest {
            host,
            username,
            credential,
        })
    }

    /// Login to Emby and return a validated provider credential session.
    pub async fn login_with_context(
        &self,
        req: EmbyLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyLoginResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let credential = match req.credential {
            EmbyLoginCredential::Password(password) => {
                emby_upstream::login_req::Credential::Password(password)
            }
            EmbyLoginCredential::ApiKey(api_key) => {
                emby_upstream::login_req::Credential::ApiKey(api_key)
            }
        };
        let resp = client
            .login(emby_upstream::LoginReq {
                host: req.host,
                username: req.username,
                credential: Some(credential),
            })
            .await
            .map_err(ProviderError::from)?;
        Ok(EmbyLoginResponse {
            token: resp.token,
            user_id: resp.user_id,
            username: resp.username,
            server_id: resp.server_id,
            policy: resp.policy.map(Self::emby_user_policy_from_provider),
        })
    }

    pub async fn persist_login_credential(
        &self,
        user_id: UserId,
        host: String,
        api_key: String,
        emby_user_id: String,
        provider_instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        let server_id = Self::credential_server_id_for_instance(&host, provider_instance_name);
        let credential_data = ProviderCredential::Emby {
            host,
            api_key,
            emby_user_id,
        };
        let now = crate::SystemClock.now();
        let credential = UserProviderCredential {
            id: 0,
            user_id,
            provider: Self::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name: provider_instance_name.map(ToString::to_string),
            credential_data,
            expires_at: None,
            created_at: now,
            updated_at: now,
        };

        self.credential_repo()?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to persist emby credential: {error}"))
            })?;

        Ok(server_id)
    }

    pub async fn login_and_persist_with_context(
        &self,
        request: EmbyLoginAndPersistRequest,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyPersistedLoginResponse, ProviderError> {
        let provider_instance_name = request.provider_instance_name.as_deref();
        let login_req = Self::resolve_login_request(
            request.host.clone(),
            request.username,
            request.password.as_deref(),
            request.api_key.as_deref(),
        )?;
        let login = self
            .login_with_context(login_req, provider_instance_name, request_context)
            .await?;
        let server_id = self
            .persist_login_credential(
                request.user_id,
                request.host,
                login.token.clone(),
                login.user_id.clone(),
                provider_instance_name,
            )
            .await?;

        Ok(EmbyPersistedLoginResponse { login, server_id })
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(existing) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query emby credential: {error}"))
            })?
        else {
            return Ok(false);
        };

        self.credential_repo()?
            .delete(existing.id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to delete emby credential: {error}"))
            })?;
        Ok(true)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<EmbyBind>, ProviderError> {
        let requested_instance_name = match normalize_provider_instance_name(provider_instance_name)
        {
            Some(instance_name) => {
                validate_provider_instance_name(instance_name)
                    .map_err(ProviderError::InvalidConfig)?;
                Some(instance_name)
            }
            None => None,
        };
        let credentials = self
            .credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query emby credentials: {error}"))
            })?;

        credentials
            .into_iter()
            .filter(|credential| {
                requested_instance_name.is_none_or(|requested| {
                    normalize_provider_instance_name(credential.provider_instance_name.as_deref())
                        == Some(requested)
                })
            })
            .map(|credential| {
                let ProviderCredential::Emby {
                    host, emby_user_id, ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                let host = host.trim();
                let emby_user_id = emby_user_id.trim();
                if host.is_empty() || emby_user_id.is_empty() {
                    return Err(ProviderError::InvalidConfig(format!(
                        "Emby credential {} has empty bind fields",
                        credential.id
                    )));
                }

                Ok(EmbyBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    host: host.to_string(),
                    emby_user_id: emby_user_id.to_string(),
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub fn access_from_stored_credential(
        user_id: UserId,
        server_id: &str,
        credential: ProviderCredential,
        credential_revision: String,
        stored_provider_instance_name: Option<String>,
        requested_provider_instance_name: Option<&str>,
    ) -> Result<EmbyAccess, ProviderError> {
        let provider_instance_name = requested_provider_instance_name
            .map(std::string::ToString::to_string)
            .or(stored_provider_instance_name);
        match credential {
            ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok(EmbyAccess {
                host,
                api_key,
                emby_user_id,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision,
                provider_instance_name,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    /// List Emby library items
    pub async fn fs_list_with_context(
        &self,
        req: EmbyListRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyListResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .fs_list(emby_fs_list_request(req)?)
            .await
            .map_err(ProviderError::from)?;
        Ok(EmbyListResponse {
            items: resp
                .items
                .into_iter()
                .map(Self::emby_list_item_from_provider)
                .collect(),
            total: resp.total,
        })
    }

    /// Get Emby user info
    pub async fn me_with_context(
        &self,
        req: EmbyMeRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyMeResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .me(emby_upstream::MeReq {
                host: req.host,
                token: req.token,
                user_id: req.user_id,
            })
            .await
            .map_err(ProviderError::from)?;
        Ok(EmbyMeResponse {
            id: resp.id,
            name: resp.name,
            server_id: resp.server_id,
            policy: resp.policy.map(Self::emby_user_policy_from_provider),
        })
    }

    fn emby_user_policy_from_provider(policy: emby_upstream::UserPolicy) -> EmbyUserPolicy {
        EmbyUserPolicy {
            is_administrator: policy.is_administrator,
            is_hidden: policy.is_hidden,
            is_disabled: policy.is_disabled,
            enable_all_folders: policy.enable_all_folders,
        }
    }

    fn emby_item_from_provider(item: emby_upstream::Item) -> EmbyItem {
        EmbyItem {
            name: item.name,
            id: item.id,
            item_type: item.r#type,
            parent_id: item.parent_id,
            series_name: item.series_name,
            series_id: item.series_id,
            season_name: item.season_name,
            season_id: item.season_id,
            is_folder: item.is_folder,
            collection_type: item.collection_type,
            has_thumbnail: item.has_thumbnail,
            description: item.description,
            duration_seconds: item.duration_seconds,
        }
    }

    fn emby_list_item_from_provider(item: emby_upstream::Item) -> EmbyListItem {
        Self::emby_list_item_from_item(Self::emby_item_from_provider(item))
    }

    fn emby_list_item_from_item(item: EmbyItem) -> EmbyListItem {
        let is_folder = Self::item_is_container(&item);
        EmbyListItem {
            name: item.name,
            id: item.id,
            item_type: item.item_type,
            parent_id: item.parent_id,
            series_name: item.series_name,
            series_id: item.series_id,
            season_name: item.season_name,
            season_id: item.season_id,
            is_folder,
            collection_type: item.collection_type,
            has_thumbnail: item.has_thumbnail,
            description: item.description,
            duration_seconds: item.duration_seconds,
        }
    }

    fn encode_target(item_id: &str) -> Result<crate::models::ProviderTarget, ProviderError> {
        if item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby target item_id cannot be empty".to_string(),
            ));
        }

        Ok(crate::models::ProviderTarget::emby(item_id.to_string()))
    }

    fn decode_target(
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        let crate::models::ProviderTarget::Emby(session) = target else {
            return Err(ProviderError::InvalidConfig(
                "Emby target must use emby session".to_string(),
            ));
        };
        let item_id = match session {
            EmbyTarget::Item { item_id } | EmbyTarget::PersonItem { item_id, .. } => item_id,
            EmbyTarget::Person { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Emby person target does not identify playable media".to_string(),
                ));
            }
        };
        if item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby target item_id cannot be empty".to_string(),
            ));
        }

        Ok(Some(item_id.clone()))
    }

    async fn fetch_item(
        &self,
        resolved: &ResolvedEmbyConfig,
        item_id: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<EmbyItem, ProviderError> {
        let client = self
            .get_client_with_context(resolved.provider_instance_name.as_deref(), request_context)
            .await?;
        let request = emby_get_item_request(resolved, item_id);
        client
            .get_item(request)
            .await
            .map(Self::emby_item_from_provider)
            .map_err(Into::into)
    }

    fn item_type_from_listing(item: &EmbyItem) -> Option<ItemType> {
        if Self::item_is_container(item) {
            Some(ItemType::Playlist)
        } else {
            Self::playback_kind(&item.item_type).map(|_| ItemType::Media)
        }
    }

    fn item_is_container(item: &EmbyItem) -> bool {
        item.is_folder || Self::is_container_type(&item.item_type)
    }

    fn is_container_type(item_type: &str) -> bool {
        matches!(
            item_type.trim().to_ascii_lowercase().as_str(),
            "aggregatefolder"
                | "basepluginfolder"
                | "boxset"
                | "collectionfolder"
                | "folder"
                | "manualplaylistsfolder"
                | "playlist"
                | "playlistsfolder"
                | "season"
                | "series"
                | "userrootfolder"
                | "userview"
        )
    }

    fn playback_kind(item_type: &str) -> Option<EmbyPlaybackKind> {
        match item_type {
            "Movie" => Some(EmbyPlaybackKind::Movie),
            "Episode" => Some(EmbyPlaybackKind::Episode),
            "Video" => Some(EmbyPlaybackKind::Video),
            "Audio" => Some(EmbyPlaybackKind::Audio),
            "MusicAlbum" => Some(EmbyPlaybackKind::MusicAlbum),
            _ => None,
        }
    }

    fn build_next_source_config(
        server_id: &str,
        item_id: &str,
        proxy_mode: crate::models::PlaybackProxyMode,
    ) -> MediaSourceConfig {
        MediaSourceConfig::Emby(EmbyMediaSourceConfig {
            item_id: item_id.to_string(),
            server_id: server_id.to_string(),
            proxy_mode,
        })
    }

    fn playback_cache_key(
        server_id: &str,
        credential_owner_id: &str,
        credential_revision: &str,
        room_id: Option<RoomId>,
        item_id: &str,
        playback_profile_cache_key: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(credential_owner_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(credential_revision.as_bytes());
        hasher.update(b"\0");
        hasher.update(
            room_id
                .map_or_else(|| "unscoped".to_string(), |room_id| room_id.to_string())
                .as_bytes(),
        );
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
        config: EmbySourceConfig,
    ) -> Result<ResolvedEmbyConfig, ProviderError> {
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

    async fn resolve_playlist_config(
        &self,
        ctx: &ProviderContext<'_>,
        config: &EmbyPlaylistConfig,
    ) -> Result<ResolvedEmbyConfig, ProviderError> {
        let item_id = match &config.source {
            EmbyPlaylistSource::Folder { item_id } => item_id.clone(),
            _ => String::new(),
        };
        self.resolve_config(
            ctx,
            EmbySourceConfig {
                item_id,
                server_id: config.server_id.clone(),
                proxy_mode: config.proxy_mode,
            },
        )
        .await
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
        let item_request = emby_get_item_request(config, &config.item_id);

        let item = client
            .get_item(item_request)
            .await
            .map(Self::emby_item_from_provider)?;
        let kind = Self::playback_kind(&item.item_type).ok_or_else(|| {
            ProviderError::InvalidConfig(format!(
                "Unsupported Emby playback item type: {}",
                item.item_type
            ))
        })?;

        let mut metadata = EmbyPlaybackMetadata {
            kind,
            series_name: (!item.series_name.is_empty()).then_some(item.series_name.clone()),
            season_name: (!item.season_name.is_empty()).then_some(item.season_name.clone()),
            play_session_id: None,
        };

        let playback_hints = emby_playback_request_hints(playback_client_profile);

        // Get playback info
        let playback_request =
            emby_playback_info_request(config, playback_client_profile, playback_hints);

        let playback_info = client.playback_info(playback_request).await?;

        metadata.play_session_id = Some(playback_info.play_session_id.clone());

        let mut playback_infos = HashMap::new();

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
                        p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                            Self::NAME,
                            config.provider_instance_name.as_deref(),
                            "subtitle",
                            &format!(
                                "item:{}:source:{}:stream:{}",
                                config.item_id, source.id, stream.index
                            ),
                        )),
                        provider: PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Direct {
                            url: subtitle_url,
                            headers: emby_auth_headers.clone(),
                            expire_at: None,
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
                    thumbnail: None,
                    medias: vec![playback_media(
                        source.name.clone(),
                        format,
                        None,
                        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                            url: direct_url,
                            headers: emby_auth_headers.clone(),
                        }),
                    )
                    .with_p2p_swarm_id(super::provider_p2p_swarm_id(
                        Self::NAME,
                        config.provider_instance_name.as_deref(),
                        "media",
                        &format!("item:{}:source:{}:direct", config.item_id, source.id),
                    ))],
                    default_media_index: Some(0),
                    subtitles,
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            );

            // Also add transcode URLs if available
            if !source.transcoding_url.is_empty() {
                let transcode_url = emby_server_url(&config.host, &source.transcoding_url)?;
                let info = playback_infos
                    .get_mut(&mode_name)
                    .expect("media source mode was inserted above");
                let transcode_index = info.medias.len();
                info.medias.push(
                    playback_media(
                        "Transcode".to_string(),
                        "hls".to_string(),
                        None,
                        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                            url: transcode_url,
                            headers: emby_auth_headers.clone(),
                        }),
                    )
                    .with_p2p_swarm_id(super::provider_p2p_swarm_id(
                        Self::NAME,
                        config.provider_instance_name.as_deref(),
                        "media",
                        &format!("item:{}:source:{}:transcode", config.item_id, source.id),
                    )),
                );
                if playback_client_profile.is_some_and(|profile| {
                    matches!(
                        profile.stream_preference,
                        super::PlaybackStreamPreference::Transcode
                    )
                }) {
                    info.default_media_index = Some(transcode_index);
                }
            }
        }

        // Default to first media source in sorted order.
        // HashMap iteration order is non-deterministic (randomised per-process for
        // security reasons), so we sort the keys to guarantee a stable default
        // across server restarts and replicas.
        let default_mode = playback_infos
            .keys()
            .min()
            .cloned()
            .unwrap_or_else(|| "direct".to_string());

        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            provider: crate::models::SourceProvider::Emby,
            provider_instance_name: config.provider_instance_name.clone(),
            duration_seconds: item
                .duration_seconds
                .filter(|duration| duration.is_finite() && *duration > 0.0),
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Emby(metadata)),
        })
    }
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

#[derive(Debug, Clone)]
struct EmbySourceConfig {
    item_id: String,
    server_id: String,
    proxy_mode: crate::models::PlaybackProxyMode,
}

impl From<EmbyMediaSourceConfig> for EmbySourceConfig {
    fn from(config: EmbyMediaSourceConfig) -> Self {
        Self {
            item_id: config.item_id,
            server_id: config.server_id,
            proxy_mode: config.proxy_mode,
        }
    }
}

#[derive(Debug, Clone)]
struct EmbyPlaylistConfig {
    server_id: String,
    source: EmbyPlaylistSource,
    proxy_mode: crate::models::PlaybackProxyMode,
}

impl EmbySourceConfig {
    fn media_from_config(value: &crate::models::MediaSourceConfig) -> Result<Self, ProviderError> {
        match value {
            crate::models::MediaSourceConfig::Emby(config) => Ok(config.clone().into()),
            _ => Err(ProviderError::InvalidConfig(
                "Emby media requires Emby source_config".to_string(),
            )),
        }
    }
}

impl EmbyPlaylistConfig {
    fn from_config(value: &crate::models::PlaylistSourceConfig) -> Result<Self, ProviderError> {
        match value {
            crate::models::PlaylistSourceConfig::Emby(config) => Ok(Self {
                server_id: config.server_id.clone(),
                source: config.source.clone(),
                proxy_mode: config.proxy_mode,
            }),
            _ => Err(ProviderError::InvalidConfig(
                "Emby playlist requires Emby source_config".to_string(),
            )),
        }
    }

    fn server_id_from_source(value: SourceConfig<'_>) -> Result<String, ProviderError> {
        match value {
            SourceConfig::Media(config) => {
                Ok(EmbySourceConfig::media_from_config(config)?.server_id)
            }
            SourceConfig::DynamicPlaylist(config) => Ok(Self::from_config(config)?.server_id),
        }
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
        let (server_id, required_id) = match source_config {
            SourceConfig::Media(config) => {
                let config = EmbySourceConfig::media_from_config(config)?;
                (config.server_id, Some(config.item_id))
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = EmbyPlaylistConfig::from_config(config)?;
                let required_id = match &config.source {
                    EmbyPlaylistSource::Folder { item_id } => Some(item_id.clone()),
                    EmbyPlaylistSource::PersonItems { person_id, .. } => Some(person_id.clone()),
                    EmbyPlaylistSource::GenreItems { genre_id, .. } => Some(genre_id.clone()),
                    _ => None,
                };
                (config.server_id, required_id)
            }
        };
        if required_id
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ProviderError::InvalidConfig(
                "Emby source identifier must not be empty".to_string(),
            ));
        }

        if server_id.trim().is_empty() {
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
                .get_by_provider_and_server(*credential_owner_id, Self::NAME, &server_id)
                .await
                .map_err(|e| {
                    ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
                })?;

            if cred.is_none() {
                return Err(ProviderError::CredentialNotFound(format!(
                    "Referenced emby credential not found for server_id '{server_id}'"
                )));
            }
        }

        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let server_id = EmbyPlaylistConfig::server_id_from_source(source_config)?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        Ok(vec![ProviderCredentialDependency::new(
            crate::models::SourceProvider::Emby,
            credential_owner_id.to_string(),
            server_id,
        )])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let (server_id, item_id) = match source_config {
            SourceConfig::Media(config) => {
                let config = EmbySourceConfig::media_from_config(config)?;
                (config.server_id, Some(config.item_id))
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = EmbyPlaylistConfig::from_config(config)?;
                let item_id = match config.source {
                    EmbyPlaylistSource::Folder { item_id } => Some(item_id),
                    EmbyPlaylistSource::PersonItems { person_id, .. } => Some(person_id),
                    EmbyPlaylistSource::GenreItems { genre_id, .. } => Some(genre_id),
                    _ => None,
                };
                (config.server_id, item_id)
            }
        };
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;
        Ok(item_id.map(|item_id| SourceCover::Emby {
            server_id,
            credential_owner_id: *credential_owner_id,
            item_id,
        }))
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<PreparedSourceConfig, ProviderError> {
        EmbyPlaylistConfig::server_id_from_source(source_config)?;
        Ok(source_config.into())
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = EmbySourceConfig::media_from_config(source_config)?;
        let resolved = self.resolve_config(_ctx, config.clone()).await?;
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
            _ctx.room_id().copied(),
            &config.item_id,
            &playback_profile_cache_key,
        );
        let cache_ttl = Duration::from_mins(30); // 30 minutes
        let proxy_mode = config.proxy_mode;

        let result = Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            |result, version, expires_at| {
                mark_emby_playback_resources(result, version, expires_at);
                super::apply_provider_playback_policy(result, proxy_mode, true);
            },
            || async {
                self.resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
                    .await
            },
        ))
        .await?;

        let Some(play_session_id) = emby_play_session_id(&result) else {
            return Ok(result);
        };
        let resource_version = emby_playback_resource_version(&result);
        if let Some((repo, session)) = super::playback_session_registration(
            _ctx,
            play_session_id.clone(),
            resource_version,
            ProviderPlaybackSession::Emby(EmbyPlaybackSession {
                server_id: config.server_id.clone(),
                item_id: config.item_id.clone(),
                play_session_id: play_session_id.clone(),
                media_source_id: None,
                playback_cache_key: cache_key.clone(),
                start_reported: false,
            }),
        )? {
            let session_id = match repo.upsert(session).await {
                Ok(session_id) => session_id,
                Err(error) => {
                    let cleanup = self
                        .report_playback_stop(_ctx, &play_session_id, source_config, 0.0)
                        .await;
                    return Err(ProviderError::Internal(match cleanup {
                        Ok(()) => format!(
                            "failed to persist Emby playback session: {error}"
                        ),
                        Err(cleanup_error) => format!(
                            "failed to persist Emby playback session: {error}; compensation={cleanup_error}"
                        ),
                    }));
                }
            };
            if _ctx.playback_is_playing() == Some(true) {
                match self
                    .report_playback_start(_ctx, &play_session_id, source_config)
                    .await
                {
                    Ok(()) => repo
                        .mark_emby_started(session_id)
                        .await
                        .map_err(|error| ProviderError::Internal(error.to_string()))?,
                    Err(error) => tracing::warn!(
                        error = %error,
                        session_id,
                        play_session_id,
                        "Emby playback start report remains pending"
                    ),
                }
            }
        }
        Ok(result)
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn as_playback_session_lifecycle(&self) -> Option<&dyn ProviderPlaybackSessionLifecycle> {
        Some(self)
    }
}

impl EmbyProvider {
    async fn report_playback_start(
        &self,
        ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &MediaSourceConfig,
    ) -> Result<(), ProviderError> {
        let source_config = match EmbySourceConfig::media_from_config(source_config) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    session_id = %session_id,
                    "Emby on_playback_start: failed to read source config, skipping"
                );
                return Ok(());
            }
        };
        let config = match self.resolve_config(ctx, source_config).await {
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
                ctx.request_context(),
            )
            .await?;

        let item_id = config.item_id.clone();
        let req = emby_report_playback_start_request(&config, session_id);

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

    async fn report_playback_stop(
        &self,
        ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &MediaSourceConfig,
        position: f64,
    ) -> Result<(), ProviderError> {
        let source_config = EmbySourceConfig::media_from_config(source_config)?;
        let config = self.resolve_config(ctx, source_config).await?;
        let client = self
            .get_client_with_context(
                config.provider_instance_name.as_deref(),
                ctx.request_context(),
            )
            .await?;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = seconds_to_emby_ticks(position)?;

        let item_id = config.item_id.clone();

        // Report playback stopped
        let stop_req = emby_report_playback_stop_request(&config, session_id, position_ticks);

        let stop_result = client
            .report_playback_stop(stop_req)
            .await
            .map(|_| ())
            .map_err(ProviderError::from);

        // Also clean up active encodings (best effort, do not fail if this errors)
        let delete_req = emby_delete_active_encodings_request(&config, session_id);

        if let Err(error) = client.delete_active_encodings(delete_req).await {
            tracing::debug!(
                error = %error,
                session_id = %session_id,
                item_id = %item_id,
                "Emby active encoding cleanup was unavailable"
            );
        }

        stop_result
    }

    async fn report_playback_progress(
        &self,
        ctx: &ProviderContext<'_>,
        session_id: &str,
        source_config: &MediaSourceConfig,
        position: f64,
        is_paused: bool,
    ) -> Result<(), ProviderError> {
        let source_config = match EmbySourceConfig::media_from_config(source_config) {
            Ok(config) => config,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    session_id = %session_id,
                    "Emby on_playback_progress: failed to read source config, skipping"
                );
                return Ok(());
            }
        };
        let config = match self.resolve_config(ctx, source_config).await {
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
                ctx.request_context(),
            )
            .await?;

        // Convert seconds to Emby ticks (1 tick = 100 nanoseconds = 10^-7 seconds)
        let position_ticks = seconds_to_emby_ticks(position)?;

        let item_id = config.item_id.clone();
        let req =
            emby_report_playback_progress_request(&config, session_id, position_ticks, is_paused);

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

#[async_trait]
impl ProviderPlaybackSessionLifecycle for EmbyProvider {
    async fn progress(
        &self,
        ctx: &ProviderContext<'_>,
        record: &crate::models::ProviderPlaybackSessionRecord,
        position: f64,
        paused: bool,
    ) -> Result<(), ProviderError> {
        let db = ctx.db.ok_or_else(|| {
            ProviderError::Internal("Emby lifecycle requires database context".to_string())
        })?;
        let repo = crate::repository::ProviderPlaybackSessionRepository::new(db.clone());
        let ProviderPlaybackSession::Emby(session) = &record.session else {
            return Err(ProviderError::InvalidConfig(
                "Emby lifecycle received another provider's session".to_string(),
            ));
        };
        let source_config = MediaSourceConfig::Emby(EmbyMediaSourceConfig {
            item_id: session.item_id.clone(),
            server_id: session.server_id.clone(),
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        });
        if !paused && !session.start_reported {
            self.report_playback_start(ctx, &session.play_session_id, &source_config)
                .await?;
            repo.mark_emby_started(record.id)
                .await
                .map_err(|error| ProviderError::Internal(error.to_string()))?;
        }
        self.report_playback_progress(
            ctx,
            &session.play_session_id,
            &source_config,
            position,
            paused,
        )
        .await
    }

    async fn cleanup(
        &self,
        ctx: &ProviderContext<'_>,
        record: &crate::models::ProviderPlaybackSessionRecord,
    ) -> Result<(), ProviderError> {
        let ProviderPlaybackSession::Emby(session) = &record.session else {
            return Err(ProviderError::InvalidConfig(
                "Emby lifecycle received another provider's session".to_string(),
            ));
        };
        let source_config = MediaSourceConfig::Emby(EmbyMediaSourceConfig {
            item_id: session.item_id.clone(),
            server_id: session.server_id.clone(),
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        });
        self.report_playback_stop(
            ctx,
            &session.play_session_id,
            &source_config,
            record.stop_position.unwrap_or(0.0),
        )
        .await
    }
}

impl EmbyProvider {
    pub fn thumbnail_action(
        item_id: &str,
        host: &str,
        api_key: &str,
        max_height: u32,
        max_width: u32,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let item_id = item_id.trim();
        if item_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Emby thumbnail item_id must not be empty".to_string(),
            ));
        }
        let mut url = url::Url::parse(host).map_err(|error| {
            ProviderError::InvalidUrl(format!("Invalid Emby host URL: {error}"))
        })?;
        url.set_query(None);
        url.set_fragment(None);
        {
            let mut segments = url.path_segments_mut().map_err(|()| {
                ProviderError::InvalidUrl("Invalid Emby host URL path".to_string())
            })?;
            segments
                .push("Items")
                .push(item_id)
                .push("Images")
                .push("Primary");
        }
        {
            let mut query = url.query_pairs_mut();
            if max_height > 0 {
                query.append_pair("maxHeight", &max_height.to_string());
            }
            if max_width > 0 {
                query.append_pair("maxWidth", &max_width.to_string());
            }
            query.append_pair("quality", "90");
        }
        let url = url.to_string();

        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url,
                headers: HashMap::from([("X-Emby-Token".to_string(), api_key.to_string())]),
                range_header: None,
            },
        )
    }

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
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(url_index))
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: media.upstream_headers(),
            },
        )
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: EmbyHlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.media_index))
            .ok_or(ProviderError::NotFound)?;
        if request.is_manifest {
            Ok(
                super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                    url: request.target_url.to_string(),
                    headers: media.upstream_headers(),
                },
            )
        } else {
            Ok(
                super::playback_transport::PlaybackTransportAction::FetchAndForward {
                    url: request.target_url.to_string(),
                    headers: media.upstream_headers(),
                    range_header: request.range_header.map(ToString::to_string),
                },
            )
        }
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
impl DynamicPlaylistProvider for EmbyProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&crate::models::ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbyPlaylistConfig::from_config(config)?;
        let resolved = self.resolve_playlist_config(ctx, &base_config).await?;
        let client = self
            .get_client_with_context(
                resolved.provider_instance_name.as_deref(),
                ctx.request_context(),
            )
            .await?;

        let page = query.page().max(1);
        let page_size = query.page_size.max(1);
        let genre_listing =
            matches!(&base_config.source, EmbyPlaylistSource::Genres { .. }) && target.is_none();
        let (source, person_context, people_listing, container_listing) =
            match (&base_config.source, target) {
                (EmbyPlaylistSource::Folder { item_id }, None)
                | (
                    EmbyPlaylistSource::Folder { .. }
                    | EmbyPlaylistSource::Playlists
                    | EmbyPlaylistSource::Collections,
                    Some(crate::models::ProviderTarget::Emby(EmbyTarget::Item { item_id })),
                ) => (
                    emby_upstream::fs_list_req::Source::Folder(
                        emby_upstream::EmbyFolderListSource {
                            parent_id: item_id.clone(),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                (EmbyPlaylistSource::FavoriteItems { item_types }, None) => (
                    emby_upstream::fs_list_req::Source::FavoriteItems(
                        emby_upstream::EmbyFavoriteItemsListSource {
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                (EmbyPlaylistSource::FavoritePeople, None) => (
                    emby_upstream::fs_list_req::Source::FavoritePeople(
                        emby_upstream::EmbyFavoritePeopleListSource {},
                    ),
                    None,
                    true,
                    false,
                ),
                (
                    EmbyPlaylistSource::FavoritePeople,
                    Some(crate::models::ProviderTarget::Emby(EmbyTarget::Person { person_id })),
                ) => (
                    emby_upstream::fs_list_req::Source::PersonItems(
                        emby_upstream::EmbyPersonItemsListSource {
                            person_id: person_id.clone(),
                            item_types: emby_item_types(&[]),
                        },
                    ),
                    Some(person_id.clone()),
                    false,
                    false,
                ),
                (
                    EmbyPlaylistSource::PersonItems {
                        person_id,
                        item_types,
                    },
                    None,
                ) => (
                    emby_upstream::fs_list_req::Source::PersonItems(
                        emby_upstream::EmbyPersonItemsListSource {
                            person_id: person_id.clone(),
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    Some(person_id.clone()),
                    false,
                    false,
                ),
                (EmbyPlaylistSource::ContinueWatching, None) => (
                    emby_upstream::fs_list_req::Source::ContinueWatching(
                        emby_upstream::EmbyContinueWatchingListSource {},
                    ),
                    None,
                    false,
                    false,
                ),
                (EmbyPlaylistSource::NextUp, None) => (
                    emby_upstream::fs_list_req::Source::NextUp(
                        emby_upstream::EmbyNextUpListSource {},
                    ),
                    None,
                    false,
                    false,
                ),
                (EmbyPlaylistSource::RecentlyAdded { item_types }, None) => (
                    emby_upstream::fs_list_req::Source::RecentlyAdded(
                        emby_upstream::EmbyRecentlyAddedListSource {
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                (EmbyPlaylistSource::Playlists, None) => (
                    emby_upstream::fs_list_req::Source::Playlists(
                        emby_upstream::EmbyPlaylistsListSource {},
                    ),
                    None,
                    false,
                    true,
                ),
                (EmbyPlaylistSource::Collections, None) => (
                    emby_upstream::fs_list_req::Source::Collections(
                        emby_upstream::EmbyCollectionsListSource {},
                    ),
                    None,
                    false,
                    true,
                ),
                (EmbyPlaylistSource::Genres { item_types }, None) => (
                    emby_upstream::fs_list_req::Source::Genres(
                        emby_upstream::EmbyGenresListSource {
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                (
                    EmbyPlaylistSource::Genres { item_types },
                    Some(crate::models::ProviderTarget::Emby(EmbyTarget::Item { item_id })),
                ) => (
                    emby_upstream::fs_list_req::Source::GenreItems(
                        emby_upstream::EmbyGenreItemsListSource {
                            genre_id: item_id.clone(),
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                (
                    EmbyPlaylistSource::GenreItems {
                        genre_id,
                        item_types,
                    },
                    None,
                ) => (
                    emby_upstream::fs_list_req::Source::GenreItems(
                        emby_upstream::EmbyGenreItemsListSource {
                            genre_id: genre_id.clone(),
                            item_types: emby_item_types(item_types),
                        },
                    ),
                    None,
                    false,
                    false,
                ),
                _ => {
                    return Err(ProviderError::InvalidConfig(
                        "Emby target is invalid for this playlist source".to_string(),
                    ));
                }
            };
        let list_req = emby_dynamic_list_request(
            &resolved,
            source,
            page,
            page_size,
            query.search.unwrap_or_default(),
        )?;

        let response = client.fs_list(list_req).await?;
        let items = response
            .items
            .into_iter()
            .map(Self::emby_item_from_provider)
            .filter_map(|item| {
                let item_type = if (people_listing && item.item_type == "Person")
                    || container_listing
                    || genre_listing
                {
                    ItemType::Playlist
                } else {
                    Self::item_type_from_listing(&item)?
                };
                Some((item, item_type))
            })
            .map(|(item, item_type)| {
                let credential_owner_id = ctx
                    .credential_owner_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let item_id = item.id.clone();
                let source_config = if item_type == ItemType::Playlist {
                    let source = if people_listing {
                        EmbyPlaylistSource::PersonItems {
                            person_id: item_id.clone(),
                            item_types: emby_item_types(&[]),
                        }
                    } else if genre_listing {
                        let item_types = match &base_config.source {
                            EmbyPlaylistSource::Genres { item_types } => {
                                emby_item_types(item_types)
                            }
                            _ => Vec::new(),
                        };
                        EmbyPlaylistSource::GenreItems {
                            genre_id: item_id.clone(),
                            item_types,
                        }
                    } else {
                        EmbyPlaylistSource::Folder {
                            item_id: item_id.clone(),
                        }
                    };
                    DynamicPlaylistItemSourceConfig::Playlist(PlaylistSourceConfig::Emby(
                        EmbyPlaylistSourceConfig {
                            server_id: base_config.server_id.clone(),
                            source,
                            proxy_mode: base_config.proxy_mode,
                        },
                    ))
                } else {
                    DynamicPlaylistItemSourceConfig::Media(MediaSourceConfig::Emby(
                        EmbyMediaSourceConfig {
                            item_id: item_id.clone(),
                            server_id: base_config.server_id.clone(),
                            proxy_mode: base_config.proxy_mode,
                        },
                    ))
                };
                Ok(DynamicPlaylistItem {
                    name: item.name,
                    target: if people_listing {
                        crate::models::ProviderTarget::emby_person(item.id.clone())
                    } else if let Some(person_id) = person_context.as_ref() {
                        crate::models::ProviderTarget::emby_person_item(
                            person_id.clone(),
                            item.id.clone(),
                        )
                    } else {
                        Self::encode_target(&item.id)?
                    },
                    item_type,
                    size: None,
                    thumbnail: Some(DynamicPlaylistItemThumbnail::Emby {
                        server_id: base_config.server_id.clone(),
                        credential_owner_id: *credential_owner_id,
                        item_id: item.id,
                    }),
                    description: (!item.description.trim().is_empty()).then_some(item.description),
                    modified_at: None,
                    source_config: Some(source_config),
                    metadata: None,
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;

        Ok(DynamicListResult {
            has_more: items.len() >= query.page_size.max(1),
            items,
            pagination: DynamicPagination::Page { page },
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &crate::models::ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let item_id = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Emby target is required".to_string()))?;
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbyPlaylistConfig::from_config(config)?;
        let resolved = self.resolve_playlist_config(ctx, &base_config).await?;
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
            source_config: Self::build_next_source_config(
                &base_config.server_id,
                &item_id,
                base_config.proxy_mode,
            ),
            target: target.clone(),
        }))
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &crate::models::ProviderTarget,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        use crate::models::PlayMode;
        let item_id = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Emby target is required".to_string()))?;

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbyPlaylistConfig::from_config(config)?;
        let browse_target = match target {
            crate::models::ProviderTarget::Emby(EmbyTarget::PersonItem { person_id, .. }) => Some(
                crate::models::ProviderTarget::emby_person(person_id.clone()),
            ),
            crate::models::ProviderTarget::Emby(EmbyTarget::Item { .. })
                if matches!(base_config.source, EmbyPlaylistSource::Folder { .. }) =>
            {
                let resolved = self.resolve_playlist_config(ctx, &base_config).await?;
                let current_item = self
                    .fetch_item(&resolved, &item_id, ctx.request_context())
                    .await?;
                (!current_item.parent_id.is_empty())
                    .then(|| crate::models::ProviderTarget::emby(current_item.parent_id))
            }
            crate::models::ProviderTarget::Emby(EmbyTarget::Item { .. }) => None,
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Emby target does not identify playable media".to_string(),
                ));
            }
        };

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
                            browse_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page: current_page },
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
                                    &base_config.server_id,
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Emby item target".to_string(),
                                        )
                                    })?,
                                    base_config.proxy_mode,
                                ),
                                target: next.target.clone(),
                            }));
                        }
                    } else if let Some(idx) =
                        page_items.iter().position(|item| &item.target == target)
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
                                    &base_config.server_id,
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Emby item target".to_string(),
                                        )
                                    })?,
                                    base_config.proxy_mode,
                                ),
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
                            browse_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page: 1 },
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
                                &base_config.server_id,
                                &Self::decode_target(Some(&first.target))?.ok_or_else(|| {
                                    ProviderError::InvalidConfig(
                                        "Missing Emby item target".to_string(),
                                    )
                                })?,
                                base_config.proxy_mode,
                            ),
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
                            browse_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page },
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
                    .filter(|item| &item.target != target)
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
                            &base_config.server_id,
                            &Self::decode_target(Some(&random.target))?.ok_or_else(|| {
                                ProviderError::InvalidConfig("Missing Emby item target".to_string())
                            })?,
                            base_config.proxy_mode,
                        ),
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
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(target) = target else {
            return Ok(Vec::new());
        };

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = EmbyPlaylistConfig::from_config(config)?;
        let resolved = self.resolve_playlist_config(ctx, &base_config).await?;
        let crate::models::ProviderTarget::Emby(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Emby target must use emby session".to_string(),
            ));
        };
        if let EmbyTarget::Person { person_id } = target {
            let person = self
                .fetch_item(&resolved, person_id, ctx.request_context())
                .await?;
            return Ok(vec![DynamicBrowsePathSegment {
                name: person.name,
                target: crate::models::ProviderTarget::emby_person(person_id.clone()),
            }]);
        }
        let mut current_id = match target {
            EmbyTarget::Item { item_id } | EmbyTarget::PersonItem { item_id, .. } => {
                item_id.clone()
            }
            EmbyTarget::Person { .. } => unreachable!(),
        };
        let base_item_id = match &base_config.source {
            EmbyPlaylistSource::Folder { item_id } => Some(item_id.as_str()),
            _ => None,
        };

        let mut segments = Vec::new();
        for _ in 0..32 {
            if base_item_id.is_some_and(|base| current_id == base) {
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

    #[test]
    fn playable_item_types_share_the_metadata_kind_parser() {
        for (item_type, expected) in [
            ("Movie", EmbyPlaybackKind::Movie),
            ("Episode", EmbyPlaybackKind::Episode),
            ("Video", EmbyPlaybackKind::Video),
            ("Audio", EmbyPlaybackKind::Audio),
            ("MusicAlbum", EmbyPlaybackKind::MusicAlbum),
        ] {
            assert_eq!(EmbyProvider::playback_kind(item_type), Some(expected));
        }
        assert_eq!(EmbyProvider::playback_kind("Person"), None);
    }

    #[test]
    fn collection_folder_is_always_exposed_as_a_playlist() {
        let item = EmbyItem {
            name: "SyncTV Dev Media".to_string(),
            id: "collection-folder".to_string(),
            item_type: "CollectionFolder".to_string(),
            parent_id: String::new(),
            series_name: String::new(),
            series_id: String::new(),
            season_name: String::new(),
            season_id: String::new(),
            is_folder: false,
            collection_type: "homevideos".to_string(),
            has_thumbnail: false,
            description: String::new(),
            duration_seconds: None,
        };

        assert_eq!(
            EmbyProvider::item_type_from_listing(&item),
            Some(ItemType::Playlist)
        );
        assert!(EmbyProvider::emby_list_item_from_item(item).is_folder);
    }

    #[test]
    fn playback_cache_key_is_room_scoped() {
        let room_a = RoomId::expect_positive(1);
        let room_b = RoomId::expect_positive(2);
        let first = EmbyProvider::playback_cache_key(
            "server",
            "owner",
            "revision",
            Some(room_a),
            "item",
            "profile",
        );
        let repeated = EmbyProvider::playback_cache_key(
            "server",
            "owner",
            "revision",
            Some(room_a),
            "item",
            "profile",
        );
        let other_room = EmbyProvider::playback_cache_key(
            "server",
            "owner",
            "revision",
            Some(room_b),
            "item",
            "profile",
        );

        assert_eq!(first, repeated);
        assert_ne!(first, other_room);
    }

    #[test]
    fn proxy_marker_signs_subtitles_without_changing_direct_tracks() {
        let direct_subtitle = PlaybackSubtitle {
            name: "English".to_string(),
            language: "en".to_string(),
            format: "vtt".to_string(),
            p2p_swarm_id: None,
            provider: PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Direct {
                url: "https://emby.example/subtitle.vtt".to_string(),
                headers: HashMap::from([("X-Emby-Token".to_string(), "secret".to_string())]),
                expire_at: None,
            }),
        };
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "source".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![playback_media(
                        "Direct".to_string(),
                        "mp4".to_string(),
                        None,
                        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct {
                            url: "https://emby.example/video.mp4".to_string(),
                            headers: HashMap::new(),
                        }),
                    )],
                    default_media_index: Some(0),
                    subtitles: vec![direct_subtitle],
                    default_subtitle_index: Some(0),
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "source".to_string(),
            provider: crate::models::SourceProvider::Emby,
            provider_instance_name: None,
            duration_seconds: Some(60.0),
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        };

        mark_emby_playback_resources(&mut result, "version-1", 1234);

        assert!(matches!(
            result.playback_infos["source"].subtitles[0].provider,
            PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Direct { .. })
        ));
        assert!(matches!(
            &result.playback_infos["proxy_source"].subtitles[0].provider,
            PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Proxy {
                version,
                expires_at: 1234,
                mode_name,
                subtitle_index: 0,
                ..
            }) if version == "version-1" && mode_name == "source"
        ));
    }
}
