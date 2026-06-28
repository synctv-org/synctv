use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

use super::media::SourceProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MediaSourceConfig {
    DirectUrl(DirectUrlMediaSourceConfig),
    Bilibili(BilibiliMediaSourceConfig),
    Alist(AlistMediaSourceConfig),
    Emby(EmbyMediaSourceConfig),
    Rtmp(RtmpMediaSourceConfig),
    LiveProxy(LiveProxyMediaSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlaylistSourceConfig {
    Alist(AlistPlaylistSourceConfig),
    Emby(EmbyPlaylistSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlMediaSourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_proxy: Option<bool>,
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
            is_live: None,
            duration_seconds: None,
            prefer_proxy: None,
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

    #[must_use]
    pub fn inferred_live_status(&self) -> Option<bool> {
        self.is_live.or_else(|| {
            self.has_positive_duration().then_some(false).or_else(|| {
                self.default_media()
                    .and_then(DirectUrlMediaResourceConfig::is_file_video)
            })
        })
    }

    #[must_use]
    pub fn positive_duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
            .filter(|duration| duration.is_finite() && *duration > 0.0)
    }

    fn has_positive_duration(&self) -> bool {
        self.positive_duration_seconds().is_some()
    }

    fn default_media(&self) -> Option<&DirectUrlMediaResourceConfig> {
        self.default_media_index
            .and_then(|index| self.medias.get(index))
            .or_else(|| self.medias.first())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlMediaResourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

impl DirectUrlMediaResourceConfig {
    #[must_use]
    pub fn inferred_format(&self) -> String {
        if self.format.is_empty() {
            detect_direct_url_format(&self.url).to_string()
        } else {
            self.format.clone()
        }
    }

    fn is_file_video(&self) -> Option<bool> {
        let format = self.inferred_format();
        match format.trim().to_ascii_lowercase().as_str() {
            "mp4" | "mkv" | "webm" | "avi" => Some(false),
            _ => None,
        }
    }
}

#[must_use]
pub fn detect_direct_url_format(url: &str) -> &'static str {
    let path = Url::parse(url).map_or_else(|_| url.to_string(), |url| url.path().to_string());
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "m3u8" => "m3u8",
        "mpd" => "mpd",
        "flv" => "flv",
        "mp4" | "m4v" | "mov" => "mp4",
        "mkv" => "mkv",
        "webm" => "webm",
        "avi" => "avi",
        _ => "video",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BilibiliMediaSourceConfig {
    Video(BilibiliVideoSourceConfig),
    Pgc(BilibiliPgcSourceConfig),
    Live(BilibiliLiveSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliVideoSourceConfig {
    pub bvid: Option<String>,
    pub aid: Option<u64>,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliPgcSourceConfig {
    pub epid: u64,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliLiveSourceConfig {
    pub room_id: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlistMediaSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlistPlaylistSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyMediaSourceConfig {
    pub item_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyPlaylistSourceConfig {
    pub item_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RtmpMediaSourceConfig {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

    pub fn ensure_provider(self, provider: SourceProvider) -> Result<Self, String> {
        if self.provider() == provider {
            Ok(self)
        } else {
            Err(format!(
                "media source_config provider '{}' does not match source_provider '{}'",
                self.provider(),
                provider
            ))
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

    pub fn ensure_provider(self, provider: SourceProvider) -> Result<Self, String> {
        if self.provider() == provider {
            Ok(self)
        } else {
            Err(format!(
                "playlist source_config provider '{}' does not match source_provider '{}'",
                self.provider(),
                provider
            ))
        }
    }
}

macro_rules! impl_source_config_sqlx_jsonb {
    ($ty:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $ty {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <sqlx::types::Json<$ty> as sqlx::Type<sqlx::Postgres>>::type_info()
            }

            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <sqlx::types::Json<$ty> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $ty {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                sqlx::types::Json(self).encode_by_ref(buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $ty {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let sqlx::types::Json(config) =
                    <sqlx::types::Json<Self> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                Ok(config)
            }
        }
    };
}

impl_source_config_sqlx_jsonb!(MediaSourceConfig);
impl_source_config_sqlx_jsonb!(PlaylistSourceConfig);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media_round_trip(config: &MediaSourceConfig, expected: &serde_json::Value) {
        let storage = serde_json::to_value(config).expect("media source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = serde_json::from_value::<MediaSourceConfig>(storage)
            .expect("media source config should deserialize");
        assert_eq!(&decoded, config);
    }

    fn playlist_round_trip(config: &PlaylistSourceConfig, expected: &serde_json::Value) {
        let storage =
            serde_json::to_value(config).expect("playlist source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = serde_json::from_value::<PlaylistSourceConfig>(storage)
            .expect("playlist source config should deserialize");
        assert_eq!(&decoded, config);
    }

    #[test]
    fn direct_url_inferred_live_status_treats_file_video_as_finite() {
        let config = DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4?token=m3u8".to_string(),
            HashMap::new(),
        );

        assert_eq!(config.inferred_live_status(), Some(false));
    }

    #[test]
    fn direct_url_inferred_live_status_keeps_manifest_unknown() {
        let config = DirectUrlMediaSourceConfig::single(
            "https://example.com/live.m3u8".to_string(),
            HashMap::new(),
        );

        assert_eq!(config.inferred_live_status(), None);
    }

    #[test]
    fn direct_url_inferred_live_status_uses_default_media() {
        let config = DirectUrlMediaSourceConfig {
            is_live: None,
            duration_seconds: None,
            prefer_proxy: None,
            medias: vec![
                DirectUrlMediaResourceConfig {
                    name: "manifest".to_string(),
                    url: "https://example.com/live.m3u8".to_string(),
                    headers: HashMap::new(),
                    format: String::new(),
                },
                DirectUrlMediaResourceConfig {
                    name: "file".to_string(),
                    url: "https://example.com/video.mp4".to_string(),
                    headers: HashMap::new(),
                    format: String::new(),
                },
            ],
            default_media_index: Some(1),
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };

        assert_eq!(config.inferred_live_status(), Some(false));
    }

    #[test]
    fn direct_url_inferred_live_status_honors_explicit_live_flag() {
        let mut config = DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::new(),
        );
        config.is_live = Some(true);

        assert_eq!(config.inferred_live_status(), Some(true));
    }

    #[test]
    fn media_source_configs_round_trip_provider_storage() {
        media_round_trip(
            &MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig {
                is_live: Some(false),
                duration_seconds: Some(120.5),
                prefer_proxy: Some(true),
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
                "provider": "directUrl",
                "isLive": false,
                "durationSeconds": 120.5,
                "preferProxy": true,
                "medias": [{
                    "name": "1080p",
                    "url": "https://example.com/video.mp4",
                    "headers": {"User-Agent": "SyncTV"},
                    "format": "mp4"
                }],
                "defaultMediaIndex": 0,
                "subtitles": [{
                    "name": "English",
                    "language": "en",
                    "url": "https://example.com/subtitle.vtt",
                    "format": "vtt"
                }],
                "defaultSubtitleIndex": 0,
                "danmakus": [{
                    "name": "Danmaku",
                    "url": "https://example.com/danmaku.xml",
                    "format": "xml"
                }],
                "defaultDanmakuIndex": 0
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Bilibili(BilibiliMediaSourceConfig::Video(
                BilibiliVideoSourceConfig {
                    bvid: Some("BV1234567890".to_string()),
                    aid: None,
                    cid: 42,
                    shared: true,
                },
            )),
            &json!({
                "provider": "bilibili",
                "kind": "video",
                "bvid": "BV1234567890",
                "aid": null,
                "cid": 42,
                "shared": true
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Alist(AlistMediaSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/movies/demo.mkv".to_string(),
                password: None,
            }),
            &json!({
                "provider": "alist",
                "serverId": "alist-main",
                "path": "/movies/demo.mkv",
                "password": null
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Emby(EmbyMediaSourceConfig {
                server_id: "emby-main".to_string(),
                item_id: "item-1".to_string(),
            }),
            &json!({
                "provider": "emby",
                "serverId": "emby-main",
                "itemId": "item-1"
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Rtmp(RtmpMediaSourceConfig {}),
            &json!({
                "provider": "rtmp"
            }),
        );
        media_round_trip(
            &MediaSourceConfig::LiveProxy(LiveProxyMediaSourceConfig {
                url: "rtmp://example.com/live/room".to_string(),
            }),
            &json!({
                "provider": "liveProxy",
                "url": "rtmp://example.com/live/room"
            }),
        );
    }

    #[test]
    fn media_source_config_storage_rejects_obsolete_shapes() {
        let snake_case_provider_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "direct_url",
            "medias": [{"url": "https://example.com/video.mp4"}]
        }))
        .expect_err("storage uses ProtoJSON lowerCamelCase provider names");
        assert!(
            snake_case_provider_error
                .to_string()
                .contains("unknown variant `direct_url`"),
            "{snake_case_provider_error}"
        );

        let direct_url_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "directUrl",
            "url": "https://example.com/video.mp4",
            "headers": {"User-Agent": "SyncTV"}
        }))
        .expect_err("direct_url storage requires medias[]");
        let direct_url_error = direct_url_error.to_string();
        assert!(
            direct_url_error.contains("unknown field `url`")
                || direct_url_error.contains("unknown field `headers`"),
            "{direct_url_error}"
        );

        let bilibili_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "bilibili",
            "type": "video",
            "bvid": "BV1234567890",
            "cid": 42
        }))
        .expect_err("bilibili storage requires kind");
        assert!(
            bilibili_error.to_string().contains("unknown field `type`")
                || bilibili_error.to_string().contains("missing field `kind`"),
            "{bilibili_error}"
        );

        let alist_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "alist",
            "server_id": "alist-main",
            "path": "/movies/demo.mkv"
        }))
        .expect_err("storage uses ProtoJSON lowerCamelCase field names");
        assert!(
            alist_error
                .to_string()
                .contains("unknown field `server_id`"),
            "{alist_error}"
        );
    }

    #[test]
    fn playlist_source_configs_round_trip_provider_storage() {
        playlist_round_trip(
            &PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/shows".to_string(),
                password: Some("pw".to_string()),
            }),
            &json!({
                "provider": "alist",
                "serverId": "alist-main",
                "path": "/shows",
                "password": "pw"
            }),
        );
        playlist_round_trip(
            &PlaylistSourceConfig::Emby(EmbyPlaylistSourceConfig {
                server_id: "emby-main".to_string(),
                item_id: "folder-1".to_string(),
            }),
            &json!({
                "provider": "emby",
                "serverId": "emby-main",
                "itemId": "folder-1"
            }),
        );
    }
}
