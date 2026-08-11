use std::collections::HashMap;
use std::hash::BuildHasher;

use synctv_core::models::{
    AlistMediaSourceConfig, AlistPlaylistSourceConfig, BilibiliMediaSourceConfig,
    BilibiliVideoSourceConfig, DirectUrlMediaSourceConfig, LiveProxyMediaSourceConfig,
    MediaSourceConfig, PlaylistSourceConfig, RtmpMediaSourceConfig,
};

#[must_use]
pub fn media_source_config_json(config: MediaSourceConfig) -> serde_json::Value {
    match serde_json::to_value(config) {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!(
            "test media source_config should serialize: {error}"
        )),
    }
}

#[must_use]
pub fn playlist_source_config_json(config: PlaylistSourceConfig) -> serde_json::Value {
    match serde_json::to_value(config) {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!(
            "test playlist source_config should serialize: {error}"
        )),
    }
}

#[must_use]
pub fn direct_url_media_source_config(url: impl Into<String>) -> MediaSourceConfig {
    direct_url_media_source_config_with_headers(url, HashMap::new())
}

#[must_use]
pub fn direct_url_media_source_config_with_headers<S: BuildHasher>(
    url: impl Into<String>,
    headers: HashMap<String, String, S>,
) -> MediaSourceConfig {
    MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig::single(
        url.into(),
        headers.into_iter().collect(),
    ))
}

#[must_use]
pub fn rtmp_managed_live_media_source_config() -> MediaSourceConfig {
    MediaSourceConfig::Rtmp(RtmpMediaSourceConfig {
        mode: synctv_core::models::RtmpStreamMode::Default,
    })
}

#[must_use]
pub fn live_proxy_pull_live_media_source_config(url: impl Into<String>) -> MediaSourceConfig {
    let url = url.into();
    let source = if url.starts_with("rtsp://") {
        synctv_core::models::ExternalLiveSourceConfig::Rtsp {
            url,
            transport: synctv_core::models::RtspTransport::Tcp,
            video_track: synctv_core::models::RtspTrackSelection::FirstCompatible,
            audio_track: synctv_core::models::RtspTrackSelection::FirstCompatible,
        }
    } else if url.starts_with("rtmp://") {
        synctv_core::models::ExternalLiveSourceConfig::Rtmp {
            url,
            mode: synctv_core::models::RtmpStreamMode::Default,
        }
    } else {
        synctv_core::models::ExternalLiveSourceConfig::HttpFlv { url }
    };
    MediaSourceConfig::LiveProxy(LiveProxyMediaSourceConfig { source })
}

#[must_use]
pub fn alist_file_media_source_config(
    server_id: impl Into<String>,
    path: impl Into<String>,
) -> MediaSourceConfig {
    MediaSourceConfig::Alist(AlistMediaSourceConfig {
        server_id: server_id.into(),
        path: path.into(),
        password: None,
        proxy_mode: synctv_core::models::PlaybackProxyMode::Auto,
    })
}

#[must_use]
pub fn bilibili_video_media_source_config(
    bvid: impl Into<String>,
    cid: u64,
    shared: bool,
) -> MediaSourceConfig {
    MediaSourceConfig::Bilibili(BilibiliMediaSourceConfig::Video(
        BilibiliVideoSourceConfig {
            bvid: Some(bvid.into()),
            aid: None,
            cid,
            shared,
            proxy_mode: synctv_core::models::PlaybackProxyMode::Auto,
        },
    ))
}

#[must_use]
pub fn alist_directory_playlist_source_config(
    server_id: impl Into<String>,
    path: impl Into<String>,
) -> PlaylistSourceConfig {
    PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
        server_id: server_id.into(),
        path: path.into(),
        password: None,
        proxy_mode: synctv_core::models::PlaybackProxyMode::Auto,
    })
}
