use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use super::media::SourceProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaSourceConfig {
    DirectUrl(DirectUrlMediaSourceConfig),
    Bilibili(BilibiliMediaSourceConfig),
    Alist(AlistMediaSourceConfig),
    Emby(EmbyMediaSourceConfig),
    Rtmp(RtmpMediaSourceConfig),
    LiveProxy(LiveProxyMediaSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlaylistSourceConfig {
    Alist(AlistPlaylistSourceConfig),
    Emby(EmbyPlaylistSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectUrlMediaSourceConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub medias: Vec<DirectUrlMediaResourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_media_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<DirectUrlSubtitleSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_subtitle_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub danmakus: Vec<DirectUrlDanmakuSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_danmaku_index: Option<usize>,
}

impl DirectUrlMediaSourceConfig {
    /// Build a config wrapping a single unnamed media resource, with no
    /// subtitles or danmaku tracks.
    #[must_use]
    pub fn single(url: String, headers: HashMap<String, String>) -> Self {
        Self {
            medias: vec![DirectUrlMediaResourceConfig {
                name: String::new(),
                url,
                headers,
                format: String::new(),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectUrlMediaResourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectUrlSubtitleSourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectUrlDanmakuSourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BilibiliMediaSourceConfig {
    Video(BilibiliVideoSourceConfig),
    Pgc(BilibiliPgcSourceConfig),
    Live(BilibiliLiveSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BilibiliVideoSourceConfig {
    pub bvid: Option<String>,
    pub aid: Option<u64>,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BilibiliPgcSourceConfig {
    pub epid: u64,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BilibiliLiveSourceConfig {
    pub room_id: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AlistMediaSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AlistPlaylistSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmbyMediaSourceConfig {
    pub item_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmbyPlaylistSourceConfig {
    pub item_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct RtmpMediaSourceConfig {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveProxyMediaSourceConfig {
    pub url: String,
}

impl MediaSourceConfig {
    #[must_use]
    pub const fn provider(&self) -> SourceProvider {
        match self {
            Self::DirectUrl(_) => SourceProvider::DirectUrl,
            Self::Bilibili(_) => SourceProvider::Bilibili,
            Self::Alist(_) => SourceProvider::Alist,
            Self::Emby(_) => SourceProvider::Emby,
            Self::Rtmp(_) => SourceProvider::Rtmp,
            Self::LiveProxy(_) => SourceProvider::LiveProxy,
        }
    }

    pub fn into_provider_json(self) -> Result<JsonValue, serde_json::Error> {
        match self {
            Self::DirectUrl(config) => serde_json::to_value(config),
            Self::Bilibili(config) => serde_json::to_value(config),
            Self::Alist(config) => serde_json::to_value(config),
            Self::Emby(config) => serde_json::to_value(config),
            Self::Rtmp(config) => serde_json::to_value(config),
            Self::LiveProxy(config) => serde_json::to_value(config),
        }
    }

    pub fn from_provider_json(
        provider: SourceProvider,
        value: &JsonValue,
    ) -> Result<Self, serde_json::Error> {
        match provider {
            SourceProvider::DirectUrl => Deserialize::deserialize(value).map(Self::DirectUrl),
            SourceProvider::Bilibili => Deserialize::deserialize(value).map(Self::Bilibili),
            SourceProvider::Alist => Deserialize::deserialize(value).map(Self::Alist),
            SourceProvider::Emby => Deserialize::deserialize(value).map(Self::Emby),
            SourceProvider::Rtmp => Deserialize::deserialize(value).map(Self::Rtmp),
            SourceProvider::LiveProxy => Deserialize::deserialize(value).map(Self::LiveProxy),
        }
    }
}

impl PlaylistSourceConfig {
    #[must_use]
    pub const fn provider(&self) -> SourceProvider {
        match self {
            Self::Alist(_) => SourceProvider::Alist,
            Self::Emby(_) => SourceProvider::Emby,
        }
    }

    pub fn into_provider_json(self) -> Result<JsonValue, serde_json::Error> {
        match self {
            Self::Alist(config) => serde_json::to_value(config),
            Self::Emby(config) => serde_json::to_value(config),
        }
    }

    pub fn from_provider_json(
        provider: SourceProvider,
        value: &JsonValue,
    ) -> Result<Self, serde_json::Error> {
        match provider {
            SourceProvider::Alist => Deserialize::deserialize(value).map(Self::Alist),
            SourceProvider::Emby => Deserialize::deserialize(value).map(Self::Emby),
            other => Err(serde_json::Error::custom(format!(
                "{other} does not support playlist source_config"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media_round_trip(
        provider: SourceProvider,
        config: &MediaSourceConfig,
        expected: &JsonValue,
    ) {
        let storage = config
            .clone()
            .into_provider_json()
            .expect("media source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = MediaSourceConfig::from_provider_json(provider, &storage)
            .expect("media source config should deserialize");
        assert_eq!(&decoded, config);
    }

    fn playlist_round_trip(
        provider: SourceProvider,
        config: &PlaylistSourceConfig,
        expected: &JsonValue,
    ) {
        let storage = config
            .clone()
            .into_provider_json()
            .expect("playlist source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = PlaylistSourceConfig::from_provider_json(provider, &storage)
            .expect("playlist source config should deserialize");
        assert_eq!(&decoded, config);
    }

    #[test]
    fn media_source_configs_round_trip_provider_storage() {
        media_round_trip(
            SourceProvider::DirectUrl,
            &MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig {
                medias: vec![DirectUrlMediaResourceConfig {
                    name: "1080p".to_string(),
                    url: "https://example.com/video.mp4".to_string(),
                    headers: HashMap::from([("User-Agent".to_string(), "SyncTV".to_string())]),
                    format: "mp4".to_string(),
                }],
                default_media_index: Some(0),
                subtitles: vec![DirectUrlSubtitleSourceConfig {
                    name: "English".to_string(),
                    language: "en".to_string(),
                    url: "https://example.com/subtitle.vtt".to_string(),
                    headers: HashMap::new(),
                    format: "vtt".to_string(),
                }],
                default_subtitle_index: Some(0),
                danmakus: vec![DirectUrlDanmakuSourceConfig {
                    name: "Danmaku".to_string(),
                    url: "https://example.com/danmaku.xml".to_string(),
                    headers: HashMap::new(),
                    format: Some("xml".to_string()),
                }],
                default_danmaku_index: Some(0),
            }),
            &json!({
                "medias": [{
                    "name": "1080p",
                    "url": "https://example.com/video.mp4",
                    "headers": {"User-Agent": "SyncTV"},
                    "format": "mp4"
                }],
                "default_media_index": 0,
                "subtitles": [{
                    "name": "English",
                    "language": "en",
                    "url": "https://example.com/subtitle.vtt",
                    "format": "vtt"
                }],
                "default_subtitle_index": 0,
                "danmakus": [{
                    "name": "Danmaku",
                    "url": "https://example.com/danmaku.xml",
                    "format": "xml"
                }],
                "default_danmaku_index": 0
            }),
        );
        media_round_trip(
            SourceProvider::Bilibili,
            &MediaSourceConfig::Bilibili(BilibiliMediaSourceConfig::Video(
                BilibiliVideoSourceConfig {
                    bvid: Some("BV1234567890".to_string()),
                    aid: None,
                    cid: 42,
                    shared: true,
                },
            )),
            &json!({
                "kind": "video",
                "bvid": "BV1234567890",
                "aid": null,
                "cid": 42,
                "shared": true
            }),
        );
        media_round_trip(
            SourceProvider::Alist,
            &MediaSourceConfig::Alist(AlistMediaSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/movies/demo.mkv".to_string(),
                password: None,
            }),
            &json!({
                "server_id": "alist-main",
                "path": "/movies/demo.mkv",
                "password": null
            }),
        );
        media_round_trip(
            SourceProvider::Emby,
            &MediaSourceConfig::Emby(EmbyMediaSourceConfig {
                server_id: "emby-main".to_string(),
                item_id: "item-1".to_string(),
            }),
            &json!({
                "server_id": "emby-main",
                "item_id": "item-1"
            }),
        );
        media_round_trip(
            SourceProvider::Rtmp,
            &MediaSourceConfig::Rtmp(RtmpMediaSourceConfig {}),
            &json!({}),
        );
        media_round_trip(
            SourceProvider::LiveProxy,
            &MediaSourceConfig::LiveProxy(LiveProxyMediaSourceConfig {
                url: "rtmp://example.com/live/room".to_string(),
            }),
            &json!({
                "url": "rtmp://example.com/live/room"
            }),
        );
    }

    #[test]
    fn media_source_config_storage_rejects_obsolete_shapes() {
        let direct_url_error = MediaSourceConfig::from_provider_json(
            SourceProvider::DirectUrl,
            &json!({
                "url": "https://example.com/video.mp4",
                "headers": {"User-Agent": "SyncTV"}
            }),
        )
        .expect_err("direct_url storage requires medias[]");
        let direct_url_error = direct_url_error.to_string();
        assert!(
            direct_url_error.contains("unknown field `url`")
                || direct_url_error.contains("unknown field `headers`"),
            "{direct_url_error}"
        );

        let bilibili_error = MediaSourceConfig::from_provider_json(
            SourceProvider::Bilibili,
            &json!({
                "type": "video",
                "bvid": "BV1234567890",
                "cid": 42
            }),
        )
        .expect_err("bilibili storage requires kind");
        assert!(
            bilibili_error.to_string().contains("unknown field `type`")
                || bilibili_error.to_string().contains("missing field `kind`"),
            "{bilibili_error}"
        );
    }

    #[test]
    fn playlist_source_configs_round_trip_provider_storage() {
        playlist_round_trip(
            SourceProvider::Alist,
            &PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/shows".to_string(),
                password: Some("pw".to_string()),
            }),
            &json!({
                "server_id": "alist-main",
                "path": "/shows",
                "password": "pw"
            }),
        );
        playlist_round_trip(
            SourceProvider::Emby,
            &PlaylistSourceConfig::Emby(EmbyPlaylistSourceConfig {
                server_id: "emby-main".to_string(),
                item_id: "folder-1".to_string(),
            }),
            &json!({
                "server_id": "emby-main",
                "item_id": "folder-1"
            }),
        );
    }
}
