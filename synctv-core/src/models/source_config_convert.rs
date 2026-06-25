//! Infallible conversions from the core source-config model types into their
//! `synctv-proto` counterparts.
//!
//! The proto message family mirrors the core model family field-for-field, so
//! these `From` impls are the single place that bridges the two. Both the HTTP
//! API (`synctv-api`) and the CLI (`synctv`) reuse them instead of carrying
//! their own copies.

use synctv_proto::source_config as proto;

use super::media::SourceProvider;
use super::source_config::{
    AlistMediaSourceConfig, AlistPlaylistSourceConfig, BilibiliMediaSourceConfig,
    DirectUrlDanmakuSourceConfig, DirectUrlMediaResourceConfig, DirectUrlMediaSourceConfig,
    DirectUrlSubtitleSourceConfig, EmbyMediaSourceConfig, EmbyPlaylistSourceConfig,
    LiveProxyMediaSourceConfig, MediaSourceConfig, PlaylistSourceConfig, RtmpMediaSourceConfig,
};

impl From<SourceProvider> for proto::SourceProvider {
    fn from(provider: SourceProvider) -> Self {
        match provider {
            SourceProvider::DirectUrl => Self::DirectUrl,
            SourceProvider::Bilibili => Self::Bilibili,
            SourceProvider::Alist => Self::Alist,
            SourceProvider::Emby => Self::Emby,
            SourceProvider::Rtmp => Self::Rtmp,
            SourceProvider::LiveProxy => Self::LiveProxy,
        }
    }
}

impl From<DirectUrlMediaResourceConfig> for proto::DirectUrlMediaResourceConfig {
    fn from(media: DirectUrlMediaResourceConfig) -> Self {
        Self {
            name: media.name,
            url: media.url,
            headers: media.headers,
            format: media.format,
        }
    }
}

impl From<DirectUrlSubtitleSourceConfig> for proto::DirectUrlSubtitleSourceConfig {
    fn from(subtitle: DirectUrlSubtitleSourceConfig) -> Self {
        Self {
            name: subtitle.name,
            language: subtitle.language,
            url: subtitle.url,
            headers: subtitle.headers,
            format: subtitle.format,
        }
    }
}

impl From<DirectUrlDanmakuSourceConfig> for proto::DirectUrlDanmakuSourceConfig {
    fn from(danmaku: DirectUrlDanmakuSourceConfig) -> Self {
        Self {
            name: danmaku.name,
            url: danmaku.url,
            headers: danmaku.headers,
            format: danmaku.format,
        }
    }
}

/// Map an optional `usize` index into the proto `Option<u32>` representation.
/// An index beyond `u32::MAX` (unreachable for any real resource list) is
/// dropped to `None` rather than truncated.
fn index_to_proto(index: Option<usize>) -> Option<u32> {
    index.and_then(|index| u32::try_from(index).ok())
}

impl From<DirectUrlMediaSourceConfig> for proto::DirectUrlMediaSourceConfig {
    fn from(config: DirectUrlMediaSourceConfig) -> Self {
        Self {
            is_live: config.is_live,
            duration_seconds: config.duration_seconds.and_then(|value| value.as_f64()),
            prefer_proxy: config.prefer_proxy,
            medias: config.medias.into_iter().map(Into::into).collect(),
            default_media_index: index_to_proto(config.default_media_index),
            subtitles: config.subtitles.into_iter().map(Into::into).collect(),
            default_subtitle_index: index_to_proto(config.default_subtitle_index),
            danmakus: config.danmakus.into_iter().map(Into::into).collect(),
            default_danmaku_index: index_to_proto(config.default_danmaku_index),
        }
    }
}

impl From<BilibiliMediaSourceConfig> for proto::BilibiliMediaSourceConfig {
    fn from(config: BilibiliMediaSourceConfig) -> Self {
        use proto::bilibili_media_source_config::Source;

        let source = match config {
            BilibiliMediaSourceConfig::Video(config) => {
                Source::Video(proto::BilibiliVideoSourceConfig {
                    bvid: config.bvid.unwrap_or_default(),
                    aid: config.aid,
                    cid: config.cid,
                    shared: config.shared,
                })
            }
            BilibiliMediaSourceConfig::Pgc(config) => Source::Pgc(proto::BilibiliPgcSourceConfig {
                epid: config.epid,
                cid: config.cid,
                shared: config.shared,
            }),
            BilibiliMediaSourceConfig::Live(config) => {
                Source::Live(proto::BilibiliLiveSourceConfig {
                    room_id: config.room_id,
                    shared: config.shared,
                })
            }
        };
        Self {
            source: Some(source),
        }
    }
}

impl From<AlistMediaSourceConfig> for proto::AlistMediaSourceConfig {
    fn from(config: AlistMediaSourceConfig) -> Self {
        Self {
            server_id: config.server_id,
            path: config.path,
            password: config.password,
        }
    }
}

impl From<AlistPlaylistSourceConfig> for proto::AlistPlaylistSourceConfig {
    fn from(config: AlistPlaylistSourceConfig) -> Self {
        Self {
            server_id: config.server_id,
            path: config.path,
            password: config.password,
        }
    }
}

impl From<EmbyMediaSourceConfig> for proto::EmbyMediaSourceConfig {
    fn from(config: EmbyMediaSourceConfig) -> Self {
        Self {
            server_id: config.server_id,
            item_id: config.item_id,
        }
    }
}

impl From<EmbyPlaylistSourceConfig> for proto::EmbyPlaylistSourceConfig {
    fn from(config: EmbyPlaylistSourceConfig) -> Self {
        Self {
            server_id: config.server_id,
            item_id: config.item_id,
        }
    }
}

impl From<RtmpMediaSourceConfig> for proto::RtmpMediaSourceConfig {
    fn from(_config: RtmpMediaSourceConfig) -> Self {
        Self {}
    }
}

impl From<LiveProxyMediaSourceConfig> for proto::LiveProxyMediaSourceConfig {
    fn from(config: LiveProxyMediaSourceConfig) -> Self {
        Self { url: config.url }
    }
}

impl From<MediaSourceConfig> for proto::MediaSourceConfig {
    fn from(config: MediaSourceConfig) -> Self {
        use proto::media_source_config::Provider;

        let provider = match config {
            MediaSourceConfig::DirectUrl(config) => Provider::DirectUrl(config.into()),
            MediaSourceConfig::Bilibili(config) => Provider::Bilibili(config.into()),
            MediaSourceConfig::Alist(config) => Provider::Alist(config.into()),
            MediaSourceConfig::Emby(config) => Provider::Emby(config.into()),
            MediaSourceConfig::Rtmp(config) => Provider::Rtmp(config.into()),
            MediaSourceConfig::LiveProxy(config) => Provider::LiveProxy(config.into()),
        };
        Self {
            provider: Some(provider),
        }
    }
}

impl From<PlaylistSourceConfig> for proto::PlaylistSourceConfig {
    fn from(config: PlaylistSourceConfig) -> Self {
        use proto::playlist_source_config::Provider;

        let provider = match config {
            PlaylistSourceConfig::Alist(config) => Provider::Alist(config.into()),
            PlaylistSourceConfig::Emby(config) => Provider::Emby(config.into()),
        };
        Self {
            provider: Some(provider),
        }
    }
}
