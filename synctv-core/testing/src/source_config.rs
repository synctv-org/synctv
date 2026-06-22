use std::collections::HashMap;
use std::hash::BuildHasher;

use serde_json::Value;
use synctv_core::models::{
    AlistMediaSourceConfig, AlistPlaylistSourceConfig, BilibiliMediaSourceConfig,
    BilibiliVideoSourceConfig, DirectUrlMediaResourceConfig, DirectUrlMediaSourceConfig,
    LiveProxyMediaSourceConfig, MediaSourceConfig, PlaylistSourceConfig, RtmpMediaSourceConfig,
};

fn media_storage(config: MediaSourceConfig) -> Value {
    match config.into_provider_json() {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!(
            "test media source_config should serialize: {error}"
        )),
    }
}

fn playlist_storage(config: PlaylistSourceConfig) -> Value {
    match config.into_provider_json() {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!(
            "test playlist source_config should serialize: {error}"
        )),
    }
}

#[must_use]
pub fn direct_url_media_source_config(url: impl Into<String>) -> Value {
    direct_url_media_source_config_with_headers(url, HashMap::new())
}

#[must_use]
pub fn direct_url_media_source_config_with_headers<S: BuildHasher>(
    url: impl Into<String>,
    headers: HashMap<String, String, S>,
) -> Value {
    media_storage(MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig {
        is_live: None,
        duration_seconds: None,
        medias: vec![DirectUrlMediaResourceConfig {
            name: String::new(),
            url: url.into(),
            headers: headers.into_iter().collect(),
            format: String::new(),
        }],
        default_media_index: None,
        subtitles: Vec::new(),
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    }))
}

#[must_use]
pub fn rtmp_managed_live_media_source_config() -> Value {
    media_storage(MediaSourceConfig::Rtmp(RtmpMediaSourceConfig {}))
}

#[must_use]
pub fn live_proxy_pull_live_media_source_config(url: impl Into<String>) -> Value {
    media_storage(MediaSourceConfig::LiveProxy(LiveProxyMediaSourceConfig {
        url: url.into(),
    }))
}

#[must_use]
pub fn alist_file_media_source_config(
    server_id: impl Into<String>,
    path: impl Into<String>,
) -> Value {
    media_storage(MediaSourceConfig::Alist(AlistMediaSourceConfig {
        server_id: server_id.into(),
        path: path.into(),
        password: None,
    }))
}

#[must_use]
pub fn bilibili_video_media_source_config(
    bvid: impl Into<String>,
    cid: u64,
    shared: bool,
) -> Value {
    media_storage(MediaSourceConfig::Bilibili(
        BilibiliMediaSourceConfig::Video(BilibiliVideoSourceConfig {
            bvid: Some(bvid.into()),
            aid: None,
            cid,
            shared,
        }),
    ))
}

#[must_use]
pub fn alist_directory_playlist_source_config(
    server_id: impl Into<String>,
    path: impl Into<String>,
) -> Value {
    playlist_storage(PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
        server_id: server_id.into(),
        path: path.into(),
        password: None,
    }))
}
