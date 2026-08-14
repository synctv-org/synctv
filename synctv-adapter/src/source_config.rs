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
            alist_media_source_config_from_proto(config)?,
        ),
        Provider::Emby(config) => synctv_core::models::MediaSourceConfig::Emby(
            emby_media_source_config_from_proto(config)?,
        ),
        Provider::Rtmp(config) => synctv_core::models::MediaSourceConfig::Rtmp(
            synctv_core::models::RtmpMediaSourceConfig {
                mode: rtmp_stream_mode_from_proto(config.mode)?,
            },
        ),
        Provider::LiveProxy(config) => synctv_core::models::MediaSourceConfig::LiveProxy(
            live_proxy_media_source_config_from_proto(config)?,
        ),
        Provider::Cloudreve(config) => synctv_core::models::MediaSourceConfig::Cloudreve(
            synctv_core::models::CloudreveMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Twitch(config) => synctv_core::models::MediaSourceConfig::Twitch(
            twitch_media_source_config_from_proto(config)?,
        ),
        Provider::Youtube(config) => synctv_core::models::MediaSourceConfig::Youtube(
            synctv_core::models::YoutubeMediaSourceConfig {
                video_id: config.video_id,
                shared: config.shared,
            },
        ),
        Provider::Huya(config) => synctv_core::models::MediaSourceConfig::Huya(
            huya_media_source_config_from_proto(config)?,
        ),
        Provider::Douyu(config) => synctv_core::models::MediaSourceConfig::Douyu(
            synctv_core::models::DouyuMediaSourceConfig { room: config.room },
        ),
        Provider::Douyin(config) => {
            let source = config
                .source
                .ok_or_else(|| invalid_source_config("Douyin media source is required"))?;
            synctv_core::models::MediaSourceConfig::Douyin(match source {
                source_config_proto::douyin_media_source_config::Source::Video(source) => {
                    synctv_core::models::DouyinMediaSourceConfig::Video {
                        aweme_id: source.aweme_id,
                        shared: source.shared,
                    }
                }
                source_config_proto::douyin_media_source_config::Source::Live(source) => {
                    synctv_core::models::DouyinMediaSourceConfig::Live {
                        web_rid: source.web_rid,
                        shared: source.shared,
                    }
                }
            })
        }
        Provider::Tiktok(config) => {
            let source = config
                .source
                .ok_or_else(|| invalid_source_config("TikTok media source is required"))?;
            synctv_core::models::MediaSourceConfig::TikTok(match source {
                source_config_proto::tik_tok_media_source_config::Source::Video(source) => {
                    synctv_core::models::TikTokMediaSourceConfig::Video {
                        video_id: source.video_id,
                        shared: source.shared,
                    }
                }
                source_config_proto::tik_tok_media_source_config::Source::Live(source) => {
                    synctv_core::models::TikTokMediaSourceConfig::Live {
                        unique_id: source.unique_id,
                        shared: source.shared,
                    }
                }
            })
        }
        Provider::AcFun(config) => synctv_core::models::MediaSourceConfig::AcFun(
            acfun_media_source_config_from_proto(config)?,
        ),
        Provider::Cctv(config) => synctv_core::models::MediaSourceConfig::Cctv(
            synctv_core::models::CctvMediaSourceConfig {
                resource: config.resource,
            },
        ),
        Provider::Fnos(config) => synctv_core::models::MediaSourceConfig::Fnos(
            synctv_core::models::FnosMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("FNOS media source is required"))?
                {
                    source_config_proto::fnos_media_source_config::Source::File(file) => {
                        synctv_core::models::FnosMediaSource::File { path: file.path }
                    }
                    source_config_proto::fnos_media_source_config::Source::LibraryItem(item) => {
                        synctv_core::models::FnosMediaSource::LibraryItem {
                            item_guid: item.item_guid,
                            media_guid: item.media_guid,
                        }
                    }
                },
            },
        ),
        Provider::Qnap(config) => synctv_core::models::MediaSourceConfig::Qnap(
            synctv_core::models::QnapMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Synology(config) => synctv_core::models::MediaSourceConfig::Synology(
            synctv_core::models::SynologyMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("Synology media source is required"))?
                {
                    source_config_proto::synology_media_source_config::Source::File(file) => {
                        synctv_core::models::SynologyMediaSource::File { path: file.path }
                    }
                    source_config_proto::synology_media_source_config::Source::LibraryItem(
                        item,
                    ) => synctv_core::models::SynologyMediaSource::LibraryItem {
                        kind: synology_item_kind_from_proto(item.kind)?,
                        item_id: item.item_id,
                        file_id: item.file_id,
                    },
                },
            },
        ),
        Provider::Nextcloud(config) => synctv_core::models::MediaSourceConfig::Nextcloud(
            synctv_core::models::NextcloudMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                file_id: config.file_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Seafile(config) => synctv_core::models::MediaSourceConfig::Seafile(
            synctv_core::models::SeafileMediaSourceConfig {
                server_id: config.server_id,
                repository_id: config.repository_id,
                path: config.path,
                object_id: config.object_id,
                has_thumbnail: config.has_thumbnail,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Truenas(config) => synctv_core::models::MediaSourceConfig::TrueNas(
            synctv_core::models::TrueNasMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
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
        Provider::Bilibili(config) => synctv_core::models::PlaylistSourceConfig::Bilibili(
            bilibili_playlist_source_config_from_proto(config)?,
        ),
        Provider::Alist(config) => synctv_core::models::PlaylistSourceConfig::Alist(
            alist_playlist_source_config_from_proto(config)?,
        ),
        Provider::Emby(config) => synctv_core::models::PlaylistSourceConfig::Emby(
            emby_playlist_source_config_from_proto(config)?,
        ),
        Provider::Cloudreve(config) => synctv_core::models::PlaylistSourceConfig::Cloudreve(
            synctv_core::models::CloudrevePlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Twitch(config) => {
            use source_config_proto::twitch_playlist_source_config::Source;
            let shared = config.shared;
            let source = config
                .source
                .ok_or_else(|| invalid_source_config("twitch playlist source is required"))?;
            let config = match source {
                Source::Channel(source) => {
                    synctv_core::models::TwitchPlaylistSourceConfig::Channel {
                        channel: source.channel,
                        content: twitch_playlist_content_from_proto(source.content)?,
                        shared,
                    }
                }
                Source::FollowedLive(_) => {
                    synctv_core::models::TwitchPlaylistSourceConfig::FollowedLive { shared }
                }
                Source::CategoryLive(source) => {
                    synctv_core::models::TwitchPlaylistSourceConfig::CategoryLive {
                        category_id: source.category_id,
                        category_name: source.category_name,
                        shared,
                    }
                }
                Source::SearchLive(source) => {
                    synctv_core::models::TwitchPlaylistSourceConfig::SearchLive {
                        query: source.query,
                        shared,
                    }
                }
            };
            synctv_core::models::PlaylistSourceConfig::Twitch(config)
        }
        Provider::Youtube(config) => {
            use source_config_proto::youtube_playlist_source_config::Source;

            let shared = config.shared;
            let source = config
                .source
                .ok_or_else(|| invalid_source_config("youtube playlist source is required"))?;
            synctv_core::models::PlaylistSourceConfig::Youtube(match source {
                Source::Playlist(source) => {
                    synctv_core::models::YoutubePlaylistSourceConfig::Playlist {
                        playlist_id: source.playlist_id,
                        shared,
                    }
                }
                Source::Channel(source) => {
                    let content =
                        match source_config_proto::YoutubeChannelContent::try_from(source.content)
                            .map_err(|_| {
                            invalid_source_config("youtube channel content is invalid")
                        })? {
                            source_config_proto::YoutubeChannelContent::Videos => {
                                synctv_core::models::YoutubeChannelContent::Videos
                            }
                            source_config_proto::YoutubeChannelContent::Shorts => {
                                synctv_core::models::YoutubeChannelContent::Shorts
                            }
                            source_config_proto::YoutubeChannelContent::Live => {
                                synctv_core::models::YoutubeChannelContent::Live
                            }
                            source_config_proto::YoutubeChannelContent::Unspecified => {
                                return Err(invalid_source_config(
                                    "youtube channel content is required",
                                ));
                            }
                        };
                    synctv_core::models::YoutubePlaylistSourceConfig::Channel {
                        channel_id: source.channel_id,
                        content,
                        shared,
                    }
                }
                Source::Search(source) => {
                    synctv_core::models::YoutubePlaylistSourceConfig::Search {
                        query: source.query,
                        shared,
                    }
                }
                Source::Subscriptions(_) => {
                    synctv_core::models::YoutubePlaylistSourceConfig::Subscriptions { shared }
                }
                Source::LikedVideos(_) => {
                    synctv_core::models::YoutubePlaylistSourceConfig::LikedVideos { shared }
                }
                Source::WatchLater(_) => {
                    synctv_core::models::YoutubePlaylistSourceConfig::WatchLater { shared }
                }
            })
        }
        Provider::Douyin(config) => synctv_core::models::PlaylistSourceConfig::Douyin(
            synctv_core::models::DouyinPlaylistSourceConfig {
                sec_uid: config.sec_uid,
                shared: config.shared,
            },
        ),
        Provider::Tiktok(config) => synctv_core::models::PlaylistSourceConfig::TikTok(
            synctv_core::models::TikTokPlaylistSourceConfig {
                sec_uid: config.sec_uid,
                shared: config.shared,
            },
        ),
        Provider::Fnos(config) => synctv_core::models::PlaylistSourceConfig::Fnos(
            synctv_core::models::FnosPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("FNOS playlist source is required"))?
                {
                    source_config_proto::fnos_playlist_source_config::Source::Files(files) => {
                        synctv_core::models::FnosPlaylistSource::Files { path: files.path }
                    }
                    source_config_proto::fnos_playlist_source_config::Source::MediaLibrary(
                        library,
                    ) => synctv_core::models::FnosPlaylistSource::MediaLibrary {
                        library_guid: library.library_guid,
                        media_types: library.media_types,
                        parent_guid: library.parent_guid,
                    },
                    source_config_proto::fnos_playlist_source_config::Source::Favorites(
                        favorites,
                    ) => synctv_core::models::FnosPlaylistSource::Favorites {
                        media_types: favorites.media_types,
                    },
                    source_config_proto::fnos_playlist_source_config::Source::History(_) => {
                        synctv_core::models::FnosPlaylistSource::History
                    }
                },
            },
        ),
        Provider::Qnap(config) => synctv_core::models::PlaylistSourceConfig::Qnap(
            synctv_core::models::QnapPlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
            },
        ),
        Provider::Synology(config) => synctv_core::models::PlaylistSourceConfig::Synology(
            synctv_core::models::SynologyPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("Synology playlist source is required"))?
                {
                    source_config_proto::synology_playlist_source_config::Source::Files(value) => {
                        synctv_core::models::SynologyPlaylistSource::Files { path: value.path }
                    }
                    source_config_proto::synology_playlist_source_config::Source::Movies(value) => {
                        synctv_core::models::SynologyPlaylistSource::Movies {
                            library_id: value.library_id,
                        }
                    }
                    source_config_proto::synology_playlist_source_config::Source::TvShows(
                        value,
                    ) => synctv_core::models::SynologyPlaylistSource::TvShows {
                        library_id: value.library_id,
                    },
                    source_config_proto::synology_playlist_source_config::Source::Episodes(
                        value,
                    ) => synctv_core::models::SynologyPlaylistSource::Episodes {
                        library_id: value.library_id,
                        tv_show_id: value.tv_show_id,
                    },
                    source_config_proto::synology_playlist_source_config::Source::HomeVideos(
                        value,
                    ) => synctv_core::models::SynologyPlaylistSource::HomeVideos {
                        library_id: value.library_id,
                    },
                    source_config_proto::synology_playlist_source_config::Source::TvRecordings(
                        value,
                    ) => synctv_core::models::SynologyPlaylistSource::TvRecordings {
                        library_id: value.library_id,
                    },
                },
            },
        ),
        Provider::Nextcloud(config) => synctv_core::models::PlaylistSourceConfig::Nextcloud(
            synctv_core::models::NextcloudPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("Nextcloud playlist source is required"))?
                {
                    source_config_proto::nextcloud_playlist_source_config::Source::Folder(
                        value,
                    ) => synctv_core::models::NextcloudPlaylistSource::Folder { path: value.path },
                    source_config_proto::nextcloud_playlist_source_config::Source::Favorites(_) => {
                        synctv_core::models::NextcloudPlaylistSource::Favorites
                    }
                    source_config_proto::nextcloud_playlist_source_config::Source::Search(
                        value,
                    ) => synctv_core::models::NextcloudPlaylistSource::Search {
                        path: value.path,
                        query: value.query,
                    },
                },
            },
        ),
        Provider::Seafile(config) => synctv_core::models::PlaylistSourceConfig::Seafile(
            synctv_core::models::SeafilePlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("Seafile playlist source is required"))?
                {
                    source_config_proto::seafile_playlist_source_config::Source::Folder(value) => {
                        synctv_core::models::SeafilePlaylistSource::Folder {
                            repository_id: value.repository_id,
                            path: value.path,
                        }
                    }
                    source_config_proto::seafile_playlist_source_config::Source::Starred(_) => {
                        synctv_core::models::SeafilePlaylistSource::Starred
                    }
                    source_config_proto::seafile_playlist_source_config::Source::Search(value) => {
                        synctv_core::models::SeafilePlaylistSource::Search {
                            repository_id: value.repository_id,
                            query: value.query,
                        }
                    }
                },
            },
        ),
        Provider::Truenas(config) => synctv_core::models::PlaylistSourceConfig::TrueNas(
            synctv_core::models::TrueNasPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
                source: match config
                    .source
                    .ok_or_else(|| invalid_source_config("TrueNAS playlist source is required"))?
                {
                    source_config_proto::true_nas_playlist_source_config::Source::Folder(value) => {
                        synctv_core::models::TrueNasPlaylistSource::Folder { path: value.path }
                    }
                    source_config_proto::true_nas_playlist_source_config::Source::Search(value) => {
                        synctv_core::models::TrueNasPlaylistSource::Search {
                            path: value.path,
                            query: value.query,
                        }
                    }
                },
            },
        ),
    };

    Ok((config.provider(), config))
}

fn synology_item_kind_from_proto(
    value: i32,
) -> AdapterResult<synctv_core::models::SynologyLibraryItemKind> {
    match source_config_proto::SynologyLibraryItemKind::try_from(value)
        .map_err(|_| invalid_source_config("Synology library item kind is invalid"))?
    {
        source_config_proto::SynologyLibraryItemKind::Movie => {
            Ok(synctv_core::models::SynologyLibraryItemKind::Movie)
        }
        source_config_proto::SynologyLibraryItemKind::Episode => {
            Ok(synctv_core::models::SynologyLibraryItemKind::Episode)
        }
        source_config_proto::SynologyLibraryItemKind::HomeVideo => {
            Ok(synctv_core::models::SynologyLibraryItemKind::HomeVideo)
        }
        source_config_proto::SynologyLibraryItemKind::TvRecording => {
            Ok(synctv_core::models::SynologyLibraryItemKind::TvRecording)
        }
        source_config_proto::SynologyLibraryItemKind::Unspecified => Err(invalid_source_config(
            "Synology library item kind is required",
        )),
    }
}

fn twitch_media_source_config_from_proto(
    config: source_config_proto::TwitchMediaSourceConfig,
) -> AdapterResult<synctv_core::models::TwitchMediaSourceConfig> {
    use source_config_proto::twitch_media_source_config::Source;

    match config
        .source
        .ok_or_else(|| invalid_source_config("twitch source_config source is required"))?
    {
        Source::Live(config) => Ok(synctv_core::models::TwitchMediaSourceConfig::Live {
            channel: config.channel,
            shared: config.shared,
        }),
        Source::Video(config) => Ok(synctv_core::models::TwitchMediaSourceConfig::Video {
            video_id: config.video_id,
            shared: config.shared,
        }),
        Source::Clip(config) => Ok(synctv_core::models::TwitchMediaSourceConfig::Clip {
            slug: config.slug,
            shared: config.shared,
        }),
    }
}

fn huya_media_source_config_from_proto(
    config: source_config_proto::HuyaMediaSourceConfig,
) -> AdapterResult<synctv_core::models::HuyaMediaSourceConfig> {
    use source_config_proto::huya_media_source_config::Source;

    match config
        .source
        .ok_or_else(|| invalid_source_config("huya source_config source is required"))?
    {
        Source::Live(config) => Ok(synctv_core::models::HuyaMediaSourceConfig::Live {
            room_id: config.room_id,
        }),
        Source::Video(config) => Ok(synctv_core::models::HuyaMediaSourceConfig::Video {
            video_id: config.video_id,
        }),
    }
}

fn acfun_media_source_config_from_proto(
    config: source_config_proto::AcFunMediaSourceConfig,
) -> AdapterResult<synctv_core::models::AcFunMediaSourceConfig> {
    use source_config_proto::ac_fun_media_source_config::Source;

    match config
        .source
        .ok_or_else(|| invalid_source_config("AcFun source_config source is required"))?
    {
        Source::Video(config) => Ok(synctv_core::models::AcFunMediaSourceConfig::Video {
            video_id: config.video_id,
        }),
        Source::Bangumi(config) => Ok(synctv_core::models::AcFunMediaSourceConfig::Bangumi {
            bangumi_id: config.bangumi_id,
            episode_query: config.episode_query,
        }),
        Source::Live(config) => Ok(synctv_core::models::AcFunMediaSourceConfig::Live {
            author_id: config.author_id,
        }),
    }
}

fn direct_url_media_source_config_from_proto(
    config: source_config_proto::DirectUrlMediaSourceConfig,
) -> AdapterResult<synctv_core::models::DirectUrlMediaSourceConfig> {
    Ok(synctv_core::models::DirectUrlMediaSourceConfig {
        playback_kind: config
            .playback_kind
            .map(|kind| {
                match source_config_proto::PlaybackKind::try_from(kind)
                    .map_err(|_| invalid_source_config("direct_url playback_kind is invalid"))?
                {
                    source_config_proto::PlaybackKind::Regular => {
                        Ok(synctv_core::models::PlaybackKind::Regular)
                    }
                    source_config_proto::PlaybackKind::Live => {
                        Ok(synctv_core::models::PlaybackKind::Live)
                    }
                    source_config_proto::PlaybackKind::Unspecified => Err(invalid_source_config(
                        "direct_url playback_kind must be regular or live",
                    )),
                }
            })
            .transpose()?,
        duration_seconds: config.duration_seconds,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
        medias: config
            .medias
            .into_iter()
            .map(|media| synctv_core::models::DirectUrlMediaResourceConfig {
                name: media.name,
                url: media.url,
                headers: media.headers,
                format: media.format,
                expires_at: media.expires_at,
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
                    expires_at: subtitle.expires_at,
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
                    expires_at: danmaku.expires_at,
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
    let proxy_mode = playback_proxy_mode_from_proto(config.proxy_mode)?;
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
                proxy_mode,
            },
        )),
        Source::Pgc(pgc) => Ok(synctv_core::models::BilibiliMediaSourceConfig::Pgc(
            synctv_core::models::BilibiliPgcSourceConfig {
                epid: pgc.epid,
                cid: pgc.cid,
                shared: pgc.shared,
                proxy_mode,
            },
        )),
        Source::Live(live) => Ok(synctv_core::models::BilibiliMediaSourceConfig::Live(
            synctv_core::models::BilibiliLiveSourceConfig {
                room_id: live.room_id,
                shared: live.shared,
                proxy_mode,
            },
        )),
    }
}

fn bilibili_playlist_source_config_from_proto(
    config: source_config_proto::BilibiliPlaylistSourceConfig,
) -> AdapterResult<synctv_core::models::BilibiliPlaylistSourceConfig> {
    use source_config_proto::bilibili_playlist_source_config::Source;

    let source = match config
        .source
        .ok_or_else(|| invalid_source_config("bilibili playlist source is required"))?
    {
        Source::VideoParts(source) => synctv_core::models::BilibiliPlaylistSource::VideoParts {
            bvid: source.bvid,
            aid: source.aid,
        },
        Source::Popular(_) => synctv_core::models::BilibiliPlaylistSource::Popular,
        Source::Recommended(_) => synctv_core::models::BilibiliPlaylistSource::Recommended,
        Source::UpVideos(source) => synctv_core::models::BilibiliPlaylistSource::UpVideos {
            mid: source.mid,
            keyword: source.keyword,
        },
        Source::FavoriteVideos(source) => {
            synctv_core::models::BilibiliPlaylistSource::FavoriteVideos {
                media_id: source.media_id,
            }
        }
        Source::CollectionVideos(source) => {
            synctv_core::models::BilibiliPlaylistSource::CollectionVideos {
                mid: source.mid,
                season_id: source.season_id,
            }
        }
        Source::SeriesVideos(source) => synctv_core::models::BilibiliPlaylistSource::SeriesVideos {
            mid: source.mid,
            series_id: source.series_id,
        },
        Source::WatchLater(_) => synctv_core::models::BilibiliPlaylistSource::WatchLater,
        Source::PgcSeason(source) => synctv_core::models::BilibiliPlaylistSource::PgcSeason {
            season_id: source.season_id,
        },
        Source::LiveRecommended(_) => synctv_core::models::BilibiliPlaylistSource::LiveRecommended,
        Source::LiveFollowed(_) => synctv_core::models::BilibiliPlaylistSource::LiveFollowed,
        Source::LiveArea(source) => synctv_core::models::BilibiliPlaylistSource::LiveArea {
            parent_area_id: source.parent_area_id,
            area_id: source.area_id,
        },
        Source::History(source) => synctv_core::models::BilibiliPlaylistSource::History {
            history_type: match source_config_proto::BilibiliHistoryType::try_from(source.r#type)
                .map_err(|_| invalid_source_config("bilibili history type is invalid"))?
            {
                source_config_proto::BilibiliHistoryType::All => {
                    synctv_core::models::BilibiliHistoryType::All
                }
                source_config_proto::BilibiliHistoryType::Archive => {
                    synctv_core::models::BilibiliHistoryType::Archive
                }
                source_config_proto::BilibiliHistoryType::Live => {
                    synctv_core::models::BilibiliHistoryType::Live
                }
            },
        },
        Source::PgcTimeline(source) => synctv_core::models::BilibiliPlaylistSource::PgcTimeline {
            timeline_type: match source_config_proto::BilibiliPgcTimelineType::try_from(
                source.r#type,
            )
            .map_err(|_| invalid_source_config("bilibili PGC timeline type is invalid"))?
            {
                source_config_proto::BilibiliPgcTimelineType::Anime => {
                    synctv_core::models::BilibiliPgcTimelineType::Anime
                }
                source_config_proto::BilibiliPgcTimelineType::Cinema => {
                    synctv_core::models::BilibiliPgcTimelineType::Cinema
                }
                source_config_proto::BilibiliPgcTimelineType::Guochuang => {
                    synctv_core::models::BilibiliPgcTimelineType::Guochuang
                }
                source_config_proto::BilibiliPgcTimelineType::Unspecified => {
                    return Err(invalid_source_config(
                        "bilibili PGC timeline type is required",
                    ));
                }
            },
            before_days: source.before_days,
            after_days: source.after_days,
        },
    };
    Ok(synctv_core::models::BilibiliPlaylistSourceConfig {
        source,
        shared: config.shared,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
    })
}

fn alist_media_source_config_from_proto(
    config: source_config_proto::AlistMediaSourceConfig,
) -> AdapterResult<synctv_core::models::AlistMediaSourceConfig> {
    Ok(synctv_core::models::AlistMediaSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
    })
}

fn alist_playlist_source_config_from_proto(
    config: source_config_proto::AlistPlaylistSourceConfig,
) -> AdapterResult<synctv_core::models::AlistPlaylistSourceConfig> {
    Ok(synctv_core::models::AlistPlaylistSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
    })
}

fn emby_media_source_config_from_proto(
    config: source_config_proto::EmbyMediaSourceConfig,
) -> AdapterResult<synctv_core::models::EmbyMediaSourceConfig> {
    Ok(synctv_core::models::EmbyMediaSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
    })
}

fn emby_playlist_source_config_from_proto(
    config: source_config_proto::EmbyPlaylistSourceConfig,
) -> AdapterResult<synctv_core::models::EmbyPlaylistSourceConfig> {
    use source_config_proto::emby_playlist_source_config::Source;
    let source = match config.source {
        Some(Source::Folder(source)) => synctv_core::models::EmbyPlaylistSource::Folder {
            item_id: source.item_id,
        },
        Some(Source::FavoriteItems(source)) => {
            synctv_core::models::EmbyPlaylistSource::FavoriteItems {
                item_types: source.item_types,
            }
        }
        Some(Source::FavoritePeople(_)) => synctv_core::models::EmbyPlaylistSource::FavoritePeople,
        Some(Source::PersonItems(source)) => synctv_core::models::EmbyPlaylistSource::PersonItems {
            person_id: source.person_id,
            item_types: source.item_types,
        },
        Some(Source::ContinueWatching(_)) => {
            synctv_core::models::EmbyPlaylistSource::ContinueWatching
        }
        Some(Source::NextUp(_)) => synctv_core::models::EmbyPlaylistSource::NextUp,
        Some(Source::RecentlyAdded(source)) => {
            synctv_core::models::EmbyPlaylistSource::RecentlyAdded {
                item_types: source.item_types,
            }
        }
        Some(Source::Playlists(_)) => synctv_core::models::EmbyPlaylistSource::Playlists,
        Some(Source::Collections(_)) => synctv_core::models::EmbyPlaylistSource::Collections,
        Some(Source::Genres(source)) => synctv_core::models::EmbyPlaylistSource::Genres {
            item_types: source.item_types,
        },
        Some(Source::GenreItems(source)) => synctv_core::models::EmbyPlaylistSource::GenreItems {
            genre_id: source.genre_id,
            item_types: source.item_types,
        },
        None => return Err(invalid_source_config("Emby playlist source is required")),
    };
    Ok(synctv_core::models::EmbyPlaylistSourceConfig {
        server_id: config.server_id,
        source,
        proxy_mode: playback_proxy_mode_from_proto(config.proxy_mode)?,
    })
}

fn playback_proxy_mode_from_proto(
    mode: i32,
) -> AdapterResult<synctv_core::models::PlaybackProxyMode> {
    match source_config_proto::PlaybackProxyMode::try_from(mode)
        .map_err(|_| invalid_source_config("playback proxy mode is invalid"))?
    {
        source_config_proto::PlaybackProxyMode::Auto => {
            Ok(synctv_core::models::PlaybackProxyMode::Auto)
        }
        source_config_proto::PlaybackProxyMode::Prefer => {
            Ok(synctv_core::models::PlaybackProxyMode::Prefer)
        }
        source_config_proto::PlaybackProxyMode::Only => {
            Ok(synctv_core::models::PlaybackProxyMode::Only)
        }
        source_config_proto::PlaybackProxyMode::DirectPrefer => {
            Ok(synctv_core::models::PlaybackProxyMode::DirectPrefer)
        }
        source_config_proto::PlaybackProxyMode::DirectOnly => {
            Ok(synctv_core::models::PlaybackProxyMode::DirectOnly)
        }
    }
}

fn live_proxy_media_source_config_from_proto(
    config: source_config_proto::LiveProxyMediaSourceConfig,
) -> AdapterResult<synctv_core::models::LiveProxyMediaSourceConfig> {
    use source_config_proto::live_proxy_media_source_config::Source;

    let source = match config.source {
        Some(Source::Rtmp(config)) => {
            let mode = match source_config_proto::RtmpStreamMode::try_from(config.mode)
                .map_err(|_| invalid_source_config("RTMP stream mode is invalid"))?
            {
                source_config_proto::RtmpStreamMode::Unspecified
                | source_config_proto::RtmpStreamMode::Default => {
                    synctv_core::models::RtmpStreamMode::Default
                }
                source_config_proto::RtmpStreamMode::VideoOnly => {
                    synctv_core::models::RtmpStreamMode::VideoOnly
                }
                source_config_proto::RtmpStreamMode::AudioOnly => {
                    synctv_core::models::RtmpStreamMode::AudioOnly
                }
            };
            synctv_core::models::ExternalLiveSourceConfig::Rtmp {
                url: config.url,
                mode,
            }
        }
        Some(Source::Rtsp(config)) => {
            let transport = match source_config_proto::RtspTransport::try_from(config.transport)
                .map_err(|_| invalid_source_config("RTSP transport is invalid"))?
            {
                source_config_proto::RtspTransport::Tcp => synctv_core::models::RtspTransport::Tcp,
                source_config_proto::RtspTransport::Udp => synctv_core::models::RtspTransport::Udp,
                source_config_proto::RtspTransport::Unspecified => {
                    return Err(invalid_source_config("RTSP transport is required"));
                }
            };
            synctv_core::models::ExternalLiveSourceConfig::Rtsp {
                url: config.url,
                transport,
                video_track: rtsp_track_selection_from_proto(
                    config.video_track,
                    "RTSP video track selection is required",
                )?,
                audio_track: rtsp_track_selection_from_proto(
                    config.audio_track,
                    "RTSP audio track selection is required",
                )?,
            }
        }
        Some(Source::HttpFlv(config)) => {
            synctv_core::models::ExternalLiveSourceConfig::HttpFlv { url: config.url }
        }
        None => return Err(invalid_source_config("external live source is required")),
    };
    Ok(synctv_core::models::LiveProxyMediaSourceConfig { source })
}

fn rtsp_track_selection_from_proto(
    selection: Option<source_config_proto::RtspTrackSelection>,
    missing_message: &'static str,
) -> AdapterResult<synctv_core::models::RtspTrackSelection> {
    use source_config_proto::rtsp_track_selection::Mode;

    match selection.and_then(|selection| selection.mode) {
        Some(Mode::FirstCompatible(_)) => {
            Ok(synctv_core::models::RtspTrackSelection::FirstCompatible)
        }
        Some(Mode::Index(index)) => Ok(synctv_core::models::RtspTrackSelection::Index(index)),
        Some(Mode::Disabled(_)) => Ok(synctv_core::models::RtspTrackSelection::Disabled),
        None => Err(invalid_source_config(missing_message)),
    }
}

fn rtmp_stream_mode_from_proto(mode: i32) -> AdapterResult<synctv_core::models::RtmpStreamMode> {
    match source_config_proto::RtmpStreamMode::try_from(mode)
        .map_err(|_| invalid_source_config("RTMP stream mode is invalid"))?
    {
        source_config_proto::RtmpStreamMode::Unspecified
        | source_config_proto::RtmpStreamMode::Default => {
            Ok(synctv_core::models::RtmpStreamMode::Default)
        }
        source_config_proto::RtmpStreamMode::VideoOnly => {
            Ok(synctv_core::models::RtmpStreamMode::VideoOnly)
        }
        source_config_proto::RtmpStreamMode::AudioOnly => {
            Ok(synctv_core::models::RtmpStreamMode::AudioOnly)
        }
    }
}

fn twitch_playlist_content_from_proto(
    content: i32,
) -> AdapterResult<synctv_core::models::TwitchPlaylistContent> {
    Ok(
        match source_config_proto::TwitchPlaylistContent::try_from(content)
            .map_err(|_| invalid_source_config("twitch playlist content is invalid"))?
        {
            source_config_proto::TwitchPlaylistContent::Videos => {
                synctv_core::models::TwitchPlaylistContent::Videos
            }
            source_config_proto::TwitchPlaylistContent::Highlights => {
                synctv_core::models::TwitchPlaylistContent::Highlights
            }
            source_config_proto::TwitchPlaylistContent::Uploads => {
                synctv_core::models::TwitchPlaylistContent::Uploads
            }
            source_config_proto::TwitchPlaylistContent::Clips => {
                synctv_core::models::TwitchPlaylistContent::Clips
            }
            source_config_proto::TwitchPlaylistContent::Unspecified => {
                return Err(invalid_source_config("twitch playlist content is required"));
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_direct_playback_proxy_modes() {
        assert_eq!(
            playback_proxy_mode_from_proto(
                source_config_proto::PlaybackProxyMode::DirectPrefer as i32
            )
            .unwrap(),
            synctv_core::models::PlaybackProxyMode::DirectPrefer,
        );
        assert_eq!(
            playback_proxy_mode_from_proto(
                source_config_proto::PlaybackProxyMode::DirectOnly as i32
            )
            .unwrap(),
            synctv_core::models::PlaybackProxyMode::DirectOnly,
        );
    }
}
