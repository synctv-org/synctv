use synctv_proto::source_config as source_config_proto;

use crate::{AdapterError, AdapterResult};

fn invalid_source_config(message: impl Into<String>) -> AdapterError {
    AdapterError::invalid_input(message)
}

pub fn media_source_config_from_proto(
    config: Option<source_config_proto::MediaSourceConfig>,
) -> AdapterResult<(
    synctv_core::models::SourceProvider,
    synctv_core::models::MediaSourceConfig,
)> {
    use source_config_proto::media_source_config::Provider;

    let provider = config
        .and_then(|config| config.provider)
        .ok_or_else(|| invalid_source_config("source_config is required"))?;
    let config = match provider {
        Provider::DirectUrl(config) => synctv_core::models::MediaSourceConfig::DirectUrl(
            direct_url_media_source_config_from_proto(config)?,
        ),
        Provider::Bilibili(config) => synctv_core::models::MediaSourceConfig::Bilibili(
            bilibili_media_source_config_from_proto(config)?,
        ),
        Provider::Alist(config) => synctv_core::models::MediaSourceConfig::Alist(
            alist_media_source_config_from_proto(config),
        ),
        Provider::Emby(config) => synctv_core::models::MediaSourceConfig::Emby(
            emby_media_source_config_from_proto(config),
        ),
        Provider::Rtmp(_) => synctv_core::models::MediaSourceConfig::Rtmp(
            synctv_core::models::RtmpMediaSourceConfig {},
        ),
        Provider::LiveProxy(config) => synctv_core::models::MediaSourceConfig::LiveProxy(
            live_proxy_media_source_config_from_proto(config),
        ),
    };

    Ok((config.provider(), config))
}

pub fn playlist_source_config_from_proto(
    config: Option<source_config_proto::PlaylistSourceConfig>,
) -> AdapterResult<(
    synctv_core::models::SourceProvider,
    synctv_core::models::PlaylistSourceConfig,
)> {
    use source_config_proto::playlist_source_config::Provider;

    let provider = config
        .and_then(|config| config.provider)
        .ok_or_else(|| invalid_source_config("source_config is required"))?;
    let config = match provider {
        Provider::Alist(config) => synctv_core::models::PlaylistSourceConfig::Alist(
            alist_playlist_source_config_from_proto(config),
        ),
        Provider::Emby(config) => synctv_core::models::PlaylistSourceConfig::Emby(
            emby_playlist_source_config_from_proto(config),
        ),
    };

    Ok((config.provider(), config))
}

fn direct_url_media_source_config_from_proto(
    config: source_config_proto::DirectUrlMediaSourceConfig,
) -> AdapterResult<synctv_core::models::DirectUrlMediaSourceConfig> {
    Ok(synctv_core::models::DirectUrlMediaSourceConfig {
        is_live: config.is_live,
        duration_seconds: config.duration_seconds,
        prefer_proxy: config.prefer_proxy,
        medias: config
            .medias
            .into_iter()
            .map(|media| synctv_core::models::DirectUrlMediaResourceConfig {
                name: media.name,
                url: media.url,
                headers: media.headers,
                format: media.format,
            })
            .collect(),
        default_media_index: config
            .default_media_index
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid_source_config("direct_url default_media_index is too large"))?,
        subtitles: config
            .subtitles
            .into_iter()
            .map(
                |subtitle| synctv_core::models::DirectUrlSubtitleSourceConfig {
                    name: subtitle.name,
                    language: subtitle.language,
                    url: subtitle.url,
                    headers: subtitle.headers,
                    format: subtitle.format,
                },
            )
            .collect(),
        default_subtitle_index: config
            .default_subtitle_index
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid_source_config("direct_url default_subtitle_index is too large"))?,
        danmakus: config
            .danmakus
            .into_iter()
            .map(
                |danmaku| synctv_core::models::DirectUrlDanmakuSourceConfig {
                    name: danmaku.name,
                    url: danmaku.url,
                    headers: danmaku.headers,
                    format: danmaku.format,
                },
            )
            .collect(),
        default_danmaku_index: config
            .default_danmaku_index
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid_source_config("direct_url default_danmaku_index is too large"))?,
    })
}

fn bilibili_media_source_config_from_proto(
    config: source_config_proto::BilibiliMediaSourceConfig,
) -> AdapterResult<synctv_core::models::BilibiliMediaSourceConfig> {
    use source_config_proto::bilibili_media_source_config::Source;
    match config
        .source
        .ok_or_else(|| invalid_source_config("bilibili source_config source is required"))?
    {
        Source::Video(video) => Ok(synctv_core::models::BilibiliMediaSourceConfig::Video(
            synctv_core::models::BilibiliVideoSourceConfig {
                bvid: (!video.bvid.trim().is_empty()).then_some(video.bvid),
                aid: video.aid,
                cid: video.cid,
                shared: video.shared,
            },
        )),
        Source::Pgc(pgc) => Ok(synctv_core::models::BilibiliMediaSourceConfig::Pgc(
            synctv_core::models::BilibiliPgcSourceConfig {
                epid: pgc.epid,
                cid: pgc.cid,
                shared: pgc.shared,
            },
        )),
        Source::Live(live) => Ok(synctv_core::models::BilibiliMediaSourceConfig::Live(
            synctv_core::models::BilibiliLiveSourceConfig {
                room_id: live.room_id,
                shared: live.shared,
            },
        )),
    }
}

fn alist_media_source_config_from_proto(
    config: source_config_proto::AlistMediaSourceConfig,
) -> synctv_core::models::AlistMediaSourceConfig {
    synctv_core::models::AlistMediaSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
    }
}

fn alist_playlist_source_config_from_proto(
    config: source_config_proto::AlistPlaylistSourceConfig,
) -> synctv_core::models::AlistPlaylistSourceConfig {
    synctv_core::models::AlistPlaylistSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
    }
}

fn emby_media_source_config_from_proto(
    config: source_config_proto::EmbyMediaSourceConfig,
) -> synctv_core::models::EmbyMediaSourceConfig {
    synctv_core::models::EmbyMediaSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
    }
}

fn emby_playlist_source_config_from_proto(
    config: source_config_proto::EmbyPlaylistSourceConfig,
) -> synctv_core::models::EmbyPlaylistSourceConfig {
    synctv_core::models::EmbyPlaylistSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
    }
}

fn live_proxy_media_source_config_from_proto(
    config: source_config_proto::LiveProxyMediaSourceConfig,
) -> synctv_core::models::LiveProxyMediaSourceConfig {
    synctv_core::models::LiveProxyMediaSourceConfig { url: config.url }
}
