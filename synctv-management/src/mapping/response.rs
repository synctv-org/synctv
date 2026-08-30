use tonic::Status;

use synctv_proto::{client as client_proto, source_config as source_config_proto};

fn source_provider_to_proto(provider: synctv_core::models::SourceProvider) -> i32 {
    match provider {
        synctv_core::models::SourceProvider::DirectUrl => {
            source_config_proto::SourceProvider::DirectUrl as i32
        }
        synctv_core::models::SourceProvider::Bilibili => {
            source_config_proto::SourceProvider::Bilibili as i32
        }
        synctv_core::models::SourceProvider::Alist => {
            source_config_proto::SourceProvider::Alist as i32
        }
        synctv_core::models::SourceProvider::Emby => {
            source_config_proto::SourceProvider::Emby as i32
        }
        synctv_core::models::SourceProvider::Rtmp => {
            source_config_proto::SourceProvider::Rtmp as i32
        }
        synctv_core::models::SourceProvider::LiveProxy => {
            source_config_proto::SourceProvider::LiveProxy as i32
        }
        synctv_core::models::SourceProvider::Cloudreve => {
            source_config_proto::SourceProvider::Cloudreve as i32
        }
        synctv_core::models::SourceProvider::Twitch => {
            source_config_proto::SourceProvider::Twitch as i32
        }
        synctv_core::models::SourceProvider::Huya => {
            source_config_proto::SourceProvider::Huya as i32
        }
        synctv_core::models::SourceProvider::Douyu => {
            source_config_proto::SourceProvider::Douyu as i32
        }
        synctv_core::models::SourceProvider::Douyin => {
            source_config_proto::SourceProvider::Douyin as i32
        }
        synctv_core::models::SourceProvider::TikTok => {
            source_config_proto::SourceProvider::Tiktok as i32
        }
        synctv_core::models::SourceProvider::AcFun => {
            source_config_proto::SourceProvider::Acfun as i32
        }
        synctv_core::models::SourceProvider::Cctv => {
            source_config_proto::SourceProvider::Cctv as i32
        }
        synctv_core::models::SourceProvider::Fnos => {
            source_config_proto::SourceProvider::Fnos as i32
        }
        synctv_core::models::SourceProvider::Qnap => {
            source_config_proto::SourceProvider::Qnap as i32
        }
        synctv_core::models::SourceProvider::Synology => {
            source_config_proto::SourceProvider::Synology as i32
        }
        synctv_core::models::SourceProvider::Nextcloud => {
            source_config_proto::SourceProvider::Nextcloud as i32
        }
        synctv_core::models::SourceProvider::Seafile => {
            source_config_proto::SourceProvider::Seafile as i32
        }
        synctv_core::models::SourceProvider::TrueNas => {
            source_config_proto::SourceProvider::Truenas as i32
        }
        synctv_core::models::SourceProvider::Youtube => {
            source_config_proto::SourceProvider::Youtube as i32
        }
    }
}

const fn playback_proxy_mode_to_proto(mode: synctv_core::models::PlaybackProxyMode) -> i32 {
    match mode {
        synctv_core::models::PlaybackProxyMode::Auto => {
            source_config_proto::PlaybackProxyMode::Auto as i32
        }
        synctv_core::models::PlaybackProxyMode::Prefer => {
            source_config_proto::PlaybackProxyMode::Prefer as i32
        }
        synctv_core::models::PlaybackProxyMode::Only => {
            source_config_proto::PlaybackProxyMode::Only as i32
        }
        synctv_core::models::PlaybackProxyMode::DirectPrefer => {
            source_config_proto::PlaybackProxyMode::DirectPrefer as i32
        }
        synctv_core::models::PlaybackProxyMode::DirectOnly => {
            source_config_proto::PlaybackProxyMode::DirectOnly as i32
        }
    }
}

fn playlist_source_config_to_proto(
    config: &synctv_core::models::PlaylistSourceConfig,
) -> source_config_proto::PlaylistSourceConfig {
    use source_config_proto::playlist_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::PlaylistSourceConfig::Bilibili(config) => {
            use source_config_proto::bilibili_playlist_source_config::Source;
            let source = match config.source {
                synctv_core::models::BilibiliPlaylistSource::VideoParts { bvid, aid } => {
                    Source::VideoParts(source_config_proto::BilibiliVideoPartsPlaylistSource {
                        bvid,
                        aid,
                    })
                }
                synctv_core::models::BilibiliPlaylistSource::Popular => {
                    Source::Popular(source_config_proto::BilibiliPopularPlaylistSource {})
                }
                synctv_core::models::BilibiliPlaylistSource::Recommended => {
                    Source::Recommended(source_config_proto::BilibiliRecommendedPlaylistSource {})
                }
                synctv_core::models::BilibiliPlaylistSource::UpVideos { mid, keyword } => {
                    Source::UpVideos(source_config_proto::BilibiliUpVideosPlaylistSource {
                        mid,
                        keyword,
                    })
                }
                synctv_core::models::BilibiliPlaylistSource::FavoriteVideos { media_id } => {
                    Source::FavoriteVideos(
                        source_config_proto::BilibiliFavoriteVideosPlaylistSource { media_id },
                    )
                }
                synctv_core::models::BilibiliPlaylistSource::CollectionVideos {
                    mid,
                    season_id,
                } => Source::CollectionVideos(
                    source_config_proto::BilibiliCollectionVideosPlaylistSource { mid, season_id },
                ),
                synctv_core::models::BilibiliPlaylistSource::SeriesVideos { mid, series_id } => {
                    Source::SeriesVideos(source_config_proto::BilibiliSeriesVideosPlaylistSource {
                        mid,
                        series_id,
                    })
                }
                synctv_core::models::BilibiliPlaylistSource::WatchLater => {
                    Source::WatchLater(source_config_proto::BilibiliWatchLaterPlaylistSource {})
                }
                synctv_core::models::BilibiliPlaylistSource::PgcSeason { season_id } => {
                    Source::PgcSeason(source_config_proto::BilibiliPgcSeasonPlaylistSource {
                        season_id,
                    })
                }
                synctv_core::models::BilibiliPlaylistSource::LiveRecommended => {
                    Source::LiveRecommended(
                        source_config_proto::BilibiliLiveRecommendedPlaylistSource {},
                    )
                }
                synctv_core::models::BilibiliPlaylistSource::LiveFollowed => {
                    Source::LiveFollowed(source_config_proto::BilibiliLiveFollowedPlaylistSource {})
                }
                synctv_core::models::BilibiliPlaylistSource::LiveArea {
                    parent_area_id,
                    area_id,
                } => Source::LiveArea(source_config_proto::BilibiliLiveAreaPlaylistSource {
                    parent_area_id,
                    area_id,
                }),
                synctv_core::models::BilibiliPlaylistSource::History { history_type } => {
                    Source::History(source_config_proto::BilibiliHistoryPlaylistSource {
                        r#type: match history_type {
                            synctv_core::models::BilibiliHistoryType::All => {
                                source_config_proto::BilibiliHistoryType::All as i32
                            }
                            synctv_core::models::BilibiliHistoryType::Archive => {
                                source_config_proto::BilibiliHistoryType::Archive as i32
                            }
                            synctv_core::models::BilibiliHistoryType::Live => {
                                source_config_proto::BilibiliHistoryType::Live as i32
                            }
                        },
                    })
                }
                synctv_core::models::BilibiliPlaylistSource::PgcTimeline {
                    timeline_type,
                    before_days,
                    after_days,
                } => Source::PgcTimeline(source_config_proto::BilibiliPgcTimelinePlaylistSource {
                    r#type: match timeline_type {
                        synctv_core::models::BilibiliPgcTimelineType::Anime => {
                            source_config_proto::BilibiliPgcTimelineType::Anime as i32
                        }
                        synctv_core::models::BilibiliPgcTimelineType::Cinema => {
                            source_config_proto::BilibiliPgcTimelineType::Cinema as i32
                        }
                        synctv_core::models::BilibiliPgcTimelineType::Guochuang => {
                            source_config_proto::BilibiliPgcTimelineType::Guochuang as i32
                        }
                    },
                    before_days,
                    after_days,
                }),
            };
            Provider::Bilibili(source_config_proto::BilibiliPlaylistSourceConfig {
                source: Some(source),
                shared: config.shared,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Alist(config) => {
            Provider::Alist(source_config_proto::AlistPlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
                password: config.password,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Emby(config) => {
            use source_config_proto::emby_playlist_source_config::Source;
            let source = match config.source {
                synctv_core::models::EmbyPlaylistSource::Folder { item_id } => {
                    Source::Folder(source_config_proto::EmbyFolderPlaylistSource { item_id })
                }
                synctv_core::models::EmbyPlaylistSource::FavoriteItems { item_types } => {
                    Source::FavoriteItems(source_config_proto::EmbyFavoriteItemsPlaylistSource {
                        item_types,
                    })
                }
                synctv_core::models::EmbyPlaylistSource::FavoritePeople => {
                    Source::FavoritePeople(source_config_proto::EmbyFavoritePeoplePlaylistSource {})
                }
                synctv_core::models::EmbyPlaylistSource::PersonItems {
                    person_id,
                    item_types,
                } => Source::PersonItems(source_config_proto::EmbyPersonItemsPlaylistSource {
                    person_id,
                    item_types,
                }),
                synctv_core::models::EmbyPlaylistSource::ContinueWatching => {
                    Source::ContinueWatching(
                        source_config_proto::EmbyContinueWatchingPlaylistSource {},
                    )
                }
                synctv_core::models::EmbyPlaylistSource::NextUp => {
                    Source::NextUp(source_config_proto::EmbyNextUpPlaylistSource {})
                }
                synctv_core::models::EmbyPlaylistSource::RecentlyAdded { item_types } => {
                    Source::RecentlyAdded(source_config_proto::EmbyRecentlyAddedPlaylistSource {
                        item_types,
                    })
                }
                synctv_core::models::EmbyPlaylistSource::Playlists => {
                    Source::Playlists(source_config_proto::EmbyPlaylistsPlaylistSource {})
                }
                synctv_core::models::EmbyPlaylistSource::Collections => {
                    Source::Collections(source_config_proto::EmbyCollectionsPlaylistSource {})
                }
                synctv_core::models::EmbyPlaylistSource::Genres { item_types } => {
                    Source::Genres(source_config_proto::EmbyGenresPlaylistSource { item_types })
                }
                synctv_core::models::EmbyPlaylistSource::GenreItems {
                    genre_id,
                    item_types,
                } => Source::GenreItems(source_config_proto::EmbyGenreItemsPlaylistSource {
                    genre_id,
                    item_types,
                }),
            };
            Provider::Emby(source_config_proto::EmbyPlaylistSourceConfig {
                server_id: config.server_id,
                source: Some(source),
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Cloudreve(config) => {
            Provider::Cloudreve(source_config_proto::CloudrevePlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Twitch(config) => {
            use source_config_proto::twitch_playlist_source_config::{
                CategoryLive, Channel, FollowedLive, SearchLive, Source,
            };
            let (shared, source) = match config {
                synctv_core::models::TwitchPlaylistSourceConfig::Channel {
                    channel,
                    content,
                    shared,
                } => (
                    shared,
                    Source::Channel(Channel {
                        channel,
                        content: match content {
                            synctv_core::models::TwitchPlaylistContent::Videos => {
                                source_config_proto::TwitchPlaylistContent::Videos as i32
                            }
                            synctv_core::models::TwitchPlaylistContent::Highlights => {
                                source_config_proto::TwitchPlaylistContent::Highlights as i32
                            }
                            synctv_core::models::TwitchPlaylistContent::Uploads => {
                                source_config_proto::TwitchPlaylistContent::Uploads as i32
                            }
                            synctv_core::models::TwitchPlaylistContent::Clips => {
                                source_config_proto::TwitchPlaylistContent::Clips as i32
                            }
                        },
                    }),
                ),
                synctv_core::models::TwitchPlaylistSourceConfig::FollowedLive { shared } => {
                    (shared, Source::FollowedLive(FollowedLive {}))
                }
                synctv_core::models::TwitchPlaylistSourceConfig::CategoryLive {
                    category_id,
                    category_name,
                    shared,
                } => (
                    shared,
                    Source::CategoryLive(CategoryLive {
                        category_id,
                        category_name,
                    }),
                ),
                synctv_core::models::TwitchPlaylistSourceConfig::SearchLive { query, shared } => {
                    (shared, Source::SearchLive(SearchLive { query }))
                }
            };
            Provider::Twitch(source_config_proto::TwitchPlaylistSourceConfig {
                shared,
                source: Some(source),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Youtube(config) => {
            use source_config_proto::youtube_playlist_source_config::{
                Channel, LikedVideos, Playlist, Search, Source, Subscriptions, WatchLater,
            };
            let (shared, source) = match config {
                synctv_core::models::YoutubePlaylistSourceConfig::Playlist {
                    playlist_id,
                    shared,
                } => (shared, Source::Playlist(Playlist { playlist_id })),
                synctv_core::models::YoutubePlaylistSourceConfig::Channel {
                    channel_id,
                    content,
                    shared,
                } => (
                    shared,
                    Source::Channel(Channel {
                        channel_id,
                        content: match content {
                            synctv_core::models::YoutubeChannelContent::Videos => {
                                source_config_proto::YoutubeChannelContent::Videos as i32
                            }
                            synctv_core::models::YoutubeChannelContent::Shorts => {
                                source_config_proto::YoutubeChannelContent::Shorts as i32
                            }
                            synctv_core::models::YoutubeChannelContent::Live => {
                                source_config_proto::YoutubeChannelContent::Live as i32
                            }
                        },
                    }),
                ),
                synctv_core::models::YoutubePlaylistSourceConfig::Search { query, shared } => {
                    (shared, Source::Search(Search { query }))
                }
                synctv_core::models::YoutubePlaylistSourceConfig::Subscriptions { shared } => {
                    (shared, Source::Subscriptions(Subscriptions {}))
                }
                synctv_core::models::YoutubePlaylistSourceConfig::LikedVideos { shared } => {
                    (shared, Source::LikedVideos(LikedVideos {}))
                }
                synctv_core::models::YoutubePlaylistSourceConfig::WatchLater { shared } => {
                    (shared, Source::WatchLater(WatchLater {}))
                }
            };
            Provider::Youtube(source_config_proto::YoutubePlaylistSourceConfig {
                shared,
                source: Some(source),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Douyin(config) => {
            Provider::Douyin(source_config_proto::DouyinPlaylistSourceConfig {
                sec_uid: config.sec_uid,
                shared: config.shared,
            })
        }
        synctv_core::models::PlaylistSourceConfig::TikTok(config) => {
            Provider::Tiktok(source_config_proto::TikTokPlaylistSourceConfig {
                sec_uid: config.sec_uid,
                shared: config.shared,
            })
        }
        synctv_core::models::PlaylistSourceConfig::Fnos(config) => {
            Provider::Fnos(source_config_proto::FnosPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::FnosPlaylistSource::Files { path } => {
                        source_config_proto::fnos_playlist_source_config::Source::Files(
                            source_config_proto::FnosFilesPlaylistSourceConfig { path },
                        )
                    }
                    synctv_core::models::FnosPlaylistSource::MediaLibrary {
                        library_guid,
                        media_types,
                        parent_guid,
                    } => source_config_proto::fnos_playlist_source_config::Source::MediaLibrary(
                        source_config_proto::FnosMediaLibraryPlaylistSourceConfig {
                            library_guid,
                            media_types,
                            parent_guid,
                        },
                    ),
                    synctv_core::models::FnosPlaylistSource::Favorites { media_types } => {
                        source_config_proto::fnos_playlist_source_config::Source::Favorites(
                            source_config_proto::FnosFavoritesPlaylistSourceConfig { media_types },
                        )
                    }
                    synctv_core::models::FnosPlaylistSource::History => {
                        source_config_proto::fnos_playlist_source_config::Source::History(
                            source_config_proto::FnosHistoryPlaylistSourceConfig {},
                        )
                    }
                }),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Qnap(config) => {
            Provider::Qnap(source_config_proto::QnapPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                path: config.path,
            })
        }
        synctv_core::models::PlaylistSourceConfig::Synology(config) => {
            Provider::Synology(source_config_proto::SynologyPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::SynologyPlaylistSource::Files { path } => {
                        source_config_proto::synology_playlist_source_config::Source::Files(
                            source_config_proto::SynologyFilesPlaylistSourceConfig { path },
                        )
                    }
                    synctv_core::models::SynologyPlaylistSource::Movies { library_id } => {
                        source_config_proto::synology_playlist_source_config::Source::Movies(
                            source_config_proto::SynologyMoviesPlaylistSourceConfig { library_id },
                        )
                    }
                    synctv_core::models::SynologyPlaylistSource::TvShows { library_id } => {
                        source_config_proto::synology_playlist_source_config::Source::TvShows(
                            source_config_proto::SynologyTvShowsPlaylistSourceConfig { library_id },
                        )
                    }
                    synctv_core::models::SynologyPlaylistSource::Episodes {
                        library_id,
                        tv_show_id,
                    } => source_config_proto::synology_playlist_source_config::Source::Episodes(
                        source_config_proto::SynologyEpisodesPlaylistSourceConfig {
                            library_id,
                            tv_show_id,
                        },
                    ),
                    synctv_core::models::SynologyPlaylistSource::HomeVideos { library_id } => {
                        source_config_proto::synology_playlist_source_config::Source::HomeVideos(
                            source_config_proto::SynologyHomeVideosPlaylistSourceConfig {
                                library_id,
                            },
                        )
                    }
                    synctv_core::models::SynologyPlaylistSource::TvRecordings { library_id } => {
                        source_config_proto::synology_playlist_source_config::Source::TvRecordings(
                            source_config_proto::SynologyTvRecordingsPlaylistSourceConfig {
                                library_id,
                            },
                        )
                    }
                }),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Nextcloud(config) => {
            Provider::Nextcloud(source_config_proto::NextcloudPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::NextcloudPlaylistSource::Folder { path } => {
                        source_config_proto::nextcloud_playlist_source_config::Source::Folder(
                            source_config_proto::NextcloudFolderPlaylistSourceConfig { path },
                        )
                    }
                    synctv_core::models::NextcloudPlaylistSource::Favorites => {
                        source_config_proto::nextcloud_playlist_source_config::Source::Favorites(
                            source_config_proto::NextcloudFavoritesPlaylistSourceConfig {},
                        )
                    }
                    synctv_core::models::NextcloudPlaylistSource::Search { path, query } => {
                        source_config_proto::nextcloud_playlist_source_config::Source::Search(
                            source_config_proto::NextcloudSearchPlaylistSourceConfig {
                                path,
                                query,
                            },
                        )
                    }
                }),
            })
        }
        synctv_core::models::PlaylistSourceConfig::Seafile(config) => {
            Provider::Seafile(source_config_proto::SeafilePlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::SeafilePlaylistSource::Folder {
                        repository_id,
                        path,
                    } => source_config_proto::seafile_playlist_source_config::Source::Folder(
                        source_config_proto::SeafileFolderPlaylistSourceConfig {
                            repository_id,
                            path,
                        },
                    ),
                    synctv_core::models::SeafilePlaylistSource::Starred => {
                        source_config_proto::seafile_playlist_source_config::Source::Starred(
                            source_config_proto::SeafileStarredPlaylistSourceConfig {},
                        )
                    }
                    synctv_core::models::SeafilePlaylistSource::Search {
                        repository_id,
                        query,
                    } => source_config_proto::seafile_playlist_source_config::Source::Search(
                        source_config_proto::SeafileSearchPlaylistSourceConfig {
                            repository_id,
                            query,
                        },
                    ),
                }),
            })
        }
        synctv_core::models::PlaylistSourceConfig::TrueNas(config) => {
            Provider::Truenas(source_config_proto::TrueNasPlaylistSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::TrueNasPlaylistSource::Folder { path } => {
                        source_config_proto::true_nas_playlist_source_config::Source::Folder(
                            source_config_proto::TrueNasFolderPlaylistSourceConfig { path },
                        )
                    }
                    synctv_core::models::TrueNasPlaylistSource::Search { path, query } => {
                        source_config_proto::true_nas_playlist_source_config::Source::Search(
                            source_config_proto::TrueNasSearchPlaylistSourceConfig { path, query },
                        )
                    }
                }),
            })
        }
    };

    source_config_proto::PlaylistSourceConfig {
        provider: Some(provider),
    }
}

fn rtsp_track_selection_to_proto(
    selection: synctv_core::models::RtspTrackSelection,
) -> source_config_proto::RtspTrackSelection {
    use source_config_proto::rtsp_track_selection::Mode;

    let mode = match selection {
        synctv_core::models::RtspTrackSelection::FirstCompatible => Mode::FirstCompatible(true),
        synctv_core::models::RtspTrackSelection::Index(index) => Mode::Index(index),
        synctv_core::models::RtspTrackSelection::Disabled => Mode::Disabled(true),
    };
    source_config_proto::RtspTrackSelection { mode: Some(mode) }
}

fn media_source_config_to_proto(
    config: &synctv_core::models::MediaSourceConfig,
) -> source_config_proto::MediaSourceConfig {
    use source_config_proto::media_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::MediaSourceConfig::DirectUrl(config) => {
            Provider::DirectUrl(source_config_proto::DirectUrlMediaSourceConfig {
                playback_kind: config.playback_kind.map(|kind| match kind {
                    synctv_core::models::PlaybackKind::Regular => {
                        source_config_proto::PlaybackKind::Regular as i32
                    }
                    synctv_core::models::PlaybackKind::Live => {
                        source_config_proto::PlaybackKind::Live as i32
                    }
                }),
                duration_seconds: config.duration_seconds,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                medias: config
                    .medias
                    .into_iter()
                    .map(|media| source_config_proto::DirectUrlMediaResourceConfig {
                        name: media.name,
                        url: media.url,
                        headers: media.headers,
                        format: media.format,
                        expires_at: media.expires_at,
                    })
                    .collect(),
                default_media_index: optional_index_to_proto(config.default_media_index),
                subtitles: config
                    .subtitles
                    .into_iter()
                    .map(
                        |subtitle| source_config_proto::DirectUrlSubtitleSourceConfig {
                            name: subtitle.name,
                            language: subtitle.language,
                            url: subtitle.url,
                            headers: subtitle.headers,
                            format: subtitle.format,
                            expires_at: subtitle.expires_at,
                        },
                    )
                    .collect(),
                default_subtitle_index: optional_index_to_proto(config.default_subtitle_index),
                danmakus: config
                    .danmakus
                    .into_iter()
                    .map(
                        |danmaku| source_config_proto::DirectUrlDanmakuSourceConfig {
                            name: danmaku.name,
                            url: danmaku.url,
                            headers: danmaku.headers,
                            format: danmaku.format,
                            expires_at: danmaku.expires_at,
                        },
                    )
                    .collect(),
                default_danmaku_index: optional_index_to_proto(config.default_danmaku_index),
            })
        }
        synctv_core::models::MediaSourceConfig::Bilibili(config) => {
            use source_config_proto::bilibili_media_source_config::Source;
            let proxy_mode = config.proxy_mode();
            let source = match config {
                synctv_core::models::BilibiliMediaSourceConfig::Video(config) => {
                    Source::Video(source_config_proto::BilibiliVideoSourceConfig {
                        bvid: config.bvid.unwrap_or_default(),
                        aid: config.aid,
                        cid: config.cid,
                        shared: config.shared,
                    })
                }
                synctv_core::models::BilibiliMediaSourceConfig::Pgc(config) => {
                    Source::Pgc(source_config_proto::BilibiliPgcSourceConfig {
                        epid: config.epid,
                        cid: config.cid,
                        shared: config.shared,
                    })
                }
                synctv_core::models::BilibiliMediaSourceConfig::Live(config) => {
                    Source::Live(source_config_proto::BilibiliLiveSourceConfig {
                        room_id: config.room_id,
                        shared: config.shared,
                    })
                }
            };
            Provider::Bilibili(source_config_proto::BilibiliMediaSourceConfig {
                source: Some(source),
                proxy_mode: playback_proxy_mode_to_proto(proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Alist(config) => {
            Provider::Alist(source_config_proto::AlistMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                password: config.password,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Emby(config) => {
            Provider::Emby(source_config_proto::EmbyMediaSourceConfig {
                server_id: config.server_id,
                item_id: config.item_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Rtmp(config) => {
            Provider::Rtmp(source_config_proto::RtmpMediaSourceConfig {
                mode: match config.mode {
                    synctv_core::models::RtmpStreamMode::Default => {
                        source_config_proto::RtmpStreamMode::Default as i32
                    }
                    synctv_core::models::RtmpStreamMode::VideoOnly => {
                        source_config_proto::RtmpStreamMode::VideoOnly as i32
                    }
                    synctv_core::models::RtmpStreamMode::AudioOnly => {
                        source_config_proto::RtmpStreamMode::AudioOnly as i32
                    }
                },
            })
        }
        synctv_core::models::MediaSourceConfig::LiveProxy(config) => {
            use source_config_proto::live_proxy_media_source_config::Source;
            let source = match config.source {
                synctv_core::models::ExternalLiveSourceConfig::Rtmp { url, mode } => {
                    Source::Rtmp(source_config_proto::RtmpPullSourceConfig {
                        url,
                        mode: match mode {
                            synctv_core::models::RtmpStreamMode::Default => {
                                source_config_proto::RtmpStreamMode::Default as i32
                            }
                            synctv_core::models::RtmpStreamMode::VideoOnly => {
                                source_config_proto::RtmpStreamMode::VideoOnly as i32
                            }
                            synctv_core::models::RtmpStreamMode::AudioOnly => {
                                source_config_proto::RtmpStreamMode::AudioOnly as i32
                            }
                        },
                    })
                }
                synctv_core::models::ExternalLiveSourceConfig::Rtsp {
                    url,
                    transport,
                    video_track,
                    audio_track,
                } => Source::Rtsp(source_config_proto::RtspPullSourceConfig {
                    url,
                    transport: match transport {
                        synctv_core::models::RtspTransport::Tcp => {
                            source_config_proto::RtspTransport::Tcp as i32
                        }
                        synctv_core::models::RtspTransport::Udp => {
                            source_config_proto::RtspTransport::Udp as i32
                        }
                    },
                    video_track: Some(rtsp_track_selection_to_proto(video_track)),
                    audio_track: Some(rtsp_track_selection_to_proto(audio_track)),
                }),
                synctv_core::models::ExternalLiveSourceConfig::HttpFlv { url } => {
                    Source::HttpFlv(source_config_proto::HttpFlvPullSourceConfig { url })
                }
                synctv_core::models::ExternalLiveSourceConfig::Whep { url, .. } => {
                    Source::Whep(source_config_proto::WhepPullSourceConfig {
                        url,
                        authorization: None,
                    })
                }
            };
            Provider::LiveProxy(source_config_proto::LiveProxyMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::Cloudreve(config) => {
            Provider::Cloudreve(source_config_proto::CloudreveMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Twitch(config) => {
            use source_config_proto::twitch_media_source_config::Source;
            let source = match config {
                synctv_core::models::TwitchMediaSourceConfig::Live { channel, shared } => {
                    Source::Live(source_config_proto::TwitchLiveSourceConfig { channel, shared })
                }
                synctv_core::models::TwitchMediaSourceConfig::Video { video_id, shared } => {
                    Source::Video(source_config_proto::TwitchVideoSourceConfig { video_id, shared })
                }
                synctv_core::models::TwitchMediaSourceConfig::Clip { slug, shared } => {
                    Source::Clip(source_config_proto::TwitchClipSourceConfig { slug, shared })
                }
            };
            Provider::Twitch(source_config_proto::TwitchMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::Youtube(config) => {
            Provider::Youtube(source_config_proto::YoutubeMediaSourceConfig {
                video_id: config.video_id,
                shared: config.shared,
            })
        }
        synctv_core::models::MediaSourceConfig::Douyin(config) => {
            use source_config_proto::douyin_media_source_config::Source;
            let source = match config {
                synctv_core::models::DouyinMediaSourceConfig::Video { aweme_id, shared } => {
                    Source::Video(source_config_proto::DouyinVideoSourceConfig { aweme_id, shared })
                }
                synctv_core::models::DouyinMediaSourceConfig::Live { web_rid, shared } => {
                    Source::Live(source_config_proto::DouyinLiveSourceConfig { web_rid, shared })
                }
            };
            Provider::Douyin(source_config_proto::DouyinMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::TikTok(config) => {
            use source_config_proto::tik_tok_media_source_config::Source;
            let source = match config {
                synctv_core::models::TikTokMediaSourceConfig::Video { video_id, shared } => {
                    Source::Video(source_config_proto::TikTokVideoSourceConfig { video_id, shared })
                }
                synctv_core::models::TikTokMediaSourceConfig::Live { unique_id, shared } => {
                    Source::Live(source_config_proto::TikTokLiveSourceConfig { unique_id, shared })
                }
            };
            Provider::Tiktok(source_config_proto::TikTokMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::Huya(config) => {
            use source_config_proto::huya_media_source_config::Source;
            let source = match config {
                synctv_core::models::HuyaMediaSourceConfig::Live { room_id } => {
                    Source::Live(source_config_proto::HuyaLiveSourceConfig { room_id })
                }
                synctv_core::models::HuyaMediaSourceConfig::Video { video_id } => {
                    Source::Video(source_config_proto::HuyaVideoSourceConfig { video_id })
                }
            };
            Provider::Huya(source_config_proto::HuyaMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::Douyu(config) => {
            Provider::Douyu(source_config_proto::DouyuMediaSourceConfig { room: config.room })
        }
        synctv_core::models::MediaSourceConfig::AcFun(config) => {
            use source_config_proto::ac_fun_media_source_config::Source;
            let source = match config {
                synctv_core::models::AcFunMediaSourceConfig::Video { video_id } => {
                    Source::Video(source_config_proto::AcFunVideoSourceConfig { video_id })
                }
                synctv_core::models::AcFunMediaSourceConfig::Bangumi {
                    bangumi_id,
                    episode_query,
                } => Source::Bangumi(source_config_proto::AcFunBangumiSourceConfig {
                    bangumi_id,
                    episode_query,
                }),
                synctv_core::models::AcFunMediaSourceConfig::Live { author_id } => {
                    Source::Live(source_config_proto::AcFunLiveSourceConfig { author_id })
                }
            };
            Provider::AcFun(source_config_proto::AcFunMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::Cctv(config) => {
            Provider::Cctv(source_config_proto::CctvMediaSourceConfig {
                resource: config.resource,
            })
        }
        synctv_core::models::MediaSourceConfig::Fnos(config) => {
            Provider::Fnos(source_config_proto::FnosMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::FnosMediaSource::File { path } => {
                        source_config_proto::fnos_media_source_config::Source::File(
                            source_config_proto::FnosFileSourceConfig { path },
                        )
                    }
                    synctv_core::models::FnosMediaSource::LibraryItem {
                        item_guid,
                        media_guid,
                    } => source_config_proto::fnos_media_source_config::Source::LibraryItem(
                        source_config_proto::FnosLibraryItemSourceConfig {
                            item_guid,
                            media_guid,
                        },
                    ),
                }),
            })
        }
        synctv_core::models::MediaSourceConfig::Qnap(config) => {
            Provider::Qnap(source_config_proto::QnapMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                path: config.path,
            })
        }
        synctv_core::models::MediaSourceConfig::Synology(config) => {
            Provider::Synology(source_config_proto::SynologyMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                source: Some(match config.source {
                    synctv_core::models::SynologyMediaSource::File { path } => {
                        source_config_proto::synology_media_source_config::Source::File(
                            source_config_proto::SynologyFileSourceConfig { path },
                        )
                    }
                    synctv_core::models::SynologyMediaSource::LibraryItem {
                        kind,
                        item_id,
                        file_id,
                    } => source_config_proto::synology_media_source_config::Source::LibraryItem(
                        source_config_proto::SynologyLibraryItemSourceConfig {
                            kind: match kind {
                                synctv_core::models::SynologyLibraryItemKind::Movie => {
                                    source_config_proto::SynologyLibraryItemKind::Movie as i32
                                }
                                synctv_core::models::SynologyLibraryItemKind::Episode => {
                                    source_config_proto::SynologyLibraryItemKind::Episode as i32
                                }
                                synctv_core::models::SynologyLibraryItemKind::HomeVideo => {
                                    source_config_proto::SynologyLibraryItemKind::HomeVideo as i32
                                }
                                synctv_core::models::SynologyLibraryItemKind::TvRecording => {
                                    source_config_proto::SynologyLibraryItemKind::TvRecording as i32
                                }
                            },
                            item_id,
                            file_id,
                        },
                    ),
                }),
            })
        }
        synctv_core::models::MediaSourceConfig::Nextcloud(config) => {
            Provider::Nextcloud(source_config_proto::NextcloudMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                path: config.path,
                file_id: config.file_id,
            })
        }
        synctv_core::models::MediaSourceConfig::Seafile(config) => {
            Provider::Seafile(source_config_proto::SeafileMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                repository_id: config.repository_id,
                path: config.path,
                object_id: config.object_id,
                has_thumbnail: config.has_thumbnail,
            })
        }
        synctv_core::models::MediaSourceConfig::TrueNas(config) => {
            Provider::Truenas(source_config_proto::TrueNasMediaSourceConfig {
                server_id: config.server_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
                path: config.path,
            })
        }
    };

    source_config_proto::MediaSourceConfig {
        provider: Some(provider),
    }
}

fn media_resource_metadata_to_proto(
    source_config: &synctv_core::models::MediaSourceConfig,
) -> client_proto::ResourceMetadata {
    let source = match source_config {
        synctv_core::models::MediaSourceConfig::DirectUrl(config) => config
            .medias
            .first()
            .map_or_else(|| "direct_url".to_string(), |media| media.url.clone()),
        synctv_core::models::MediaSourceConfig::Bilibili(config) => match config {
            synctv_core::models::BilibiliMediaSourceConfig::Video(video) => video
                .bvid
                .clone()
                .or_else(|| video.aid.map(|aid| format!("av{aid}")))
                .unwrap_or_else(|| format!("cid:{}", video.cid)),
            synctv_core::models::BilibiliMediaSourceConfig::Pgc(pgc) => {
                format!("ep{}", pgc.epid)
            }
            synctv_core::models::BilibiliMediaSourceConfig::Live(live) => {
                format!("live:{}", live.room_id)
            }
        },
        synctv_core::models::MediaSourceConfig::Alist(config) => config.path.clone(),
        synctv_core::models::MediaSourceConfig::Emby(config) => config.item_id.clone(),
        synctv_core::models::MediaSourceConfig::Rtmp(_) => "rtmp".to_string(),
        synctv_core::models::MediaSourceConfig::LiveProxy(config) => {
            config.source.url().to_string()
        }
        synctv_core::models::MediaSourceConfig::Cloudreve(config) => config.path.clone(),
        synctv_core::models::MediaSourceConfig::Twitch(config) => match config {
            synctv_core::models::TwitchMediaSourceConfig::Live { channel, .. } => {
                format!("https://www.twitch.tv/{channel}")
            }
            synctv_core::models::TwitchMediaSourceConfig::Video { video_id, .. } => {
                format!("https://www.twitch.tv/videos/{video_id}")
            }
            synctv_core::models::TwitchMediaSourceConfig::Clip { slug, .. } => {
                format!("https://clips.twitch.tv/{slug}")
            }
        },
        synctv_core::models::MediaSourceConfig::Youtube(config) => {
            format!("https://www.youtube.com/watch?v={}", config.video_id)
        }
        synctv_core::models::MediaSourceConfig::Douyin(config) => match config {
            synctv_core::models::DouyinMediaSourceConfig::Video { aweme_id, .. } => {
                format!("https://www.douyin.com/video/{aweme_id}")
            }
            synctv_core::models::DouyinMediaSourceConfig::Live { web_rid, .. } => {
                format!("https://live.douyin.com/{web_rid}")
            }
        },
        synctv_core::models::MediaSourceConfig::TikTok(config) => match config {
            synctv_core::models::TikTokMediaSourceConfig::Video { video_id, .. } => {
                format!("https://www.tiktok.com/@_/video/{video_id}")
            }
            synctv_core::models::TikTokMediaSourceConfig::Live { unique_id, .. } => {
                format!("https://www.tiktok.com/@{unique_id}/live")
            }
        },
        synctv_core::models::MediaSourceConfig::Huya(config) => match config {
            synctv_core::models::HuyaMediaSourceConfig::Live { room_id } => {
                format!("https://www.huya.com/{room_id}")
            }
            synctv_core::models::HuyaMediaSourceConfig::Video { video_id } => {
                format!("https://www.huya.com/video/play/{video_id}.html")
            }
        },
        synctv_core::models::MediaSourceConfig::Douyu(config) => {
            format!("https://www.douyu.com/{}", config.room)
        }
        synctv_core::models::MediaSourceConfig::AcFun(config) => match config {
            synctv_core::models::AcFunMediaSourceConfig::Video { video_id } => {
                format!("https://www.acfun.cn/v/{video_id}")
            }
            synctv_core::models::AcFunMediaSourceConfig::Bangumi {
                bangumi_id,
                episode_query,
            } => format!(
                "https://www.acfun.cn/bangumi/{bangumi_id}{}",
                episode_query
                    .as_deref()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            ),
            synctv_core::models::AcFunMediaSourceConfig::Live { author_id } => {
                format!("https://live.acfun.cn/live/{author_id}")
            }
        },
        synctv_core::models::MediaSourceConfig::Cctv(config) => config.resource.clone(),
        synctv_core::models::MediaSourceConfig::Fnos(config) => match &config.source {
            synctv_core::models::FnosMediaSource::File { path } => path.clone(),
            synctv_core::models::FnosMediaSource::LibraryItem { item_guid, .. } => {
                item_guid.clone()
            }
        },
        synctv_core::models::MediaSourceConfig::Qnap(config) => config.path.clone(),
        synctv_core::models::MediaSourceConfig::Synology(config) => match &config.source {
            synctv_core::models::SynologyMediaSource::File { path } => path.clone(),
            synctv_core::models::SynologyMediaSource::LibraryItem { item_id, .. } => {
                item_id.to_string()
            }
        },
        synctv_core::models::MediaSourceConfig::Nextcloud(config) => config.path.clone(),
        synctv_core::models::MediaSourceConfig::Seafile(config) => config.path.clone(),
        synctv_core::models::MediaSourceConfig::TrueNas(config) => config.path.clone(),
    };

    client_proto::ResourceMetadata {
        source: Some(source),
        provider: None,
    }
}

fn i64_to_i32(value: i64, field: &'static str) -> Result<i32, Status> {
    i32::try_from(value).map_err(|_| Status::internal(format!("{field} exceeds i32::MAX")))
}

fn optional_index_to_proto(index: Option<usize>) -> Option<u32> {
    index.and_then(|index| u32::try_from(index).ok())
}

fn encode_room_id(
    id: synctv_core::models::RoomId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, Status> {
    public_id_codec
        .encode_room_id(id)
        .map_err(|error| Status::internal(format!("failed to encode room id: {error}")))
}

fn encode_media_id(
    id: synctv_core::models::MediaId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, Status> {
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| Status::internal(format!("failed to encode media id: {error}")))
}

fn encode_user_id(
    id: synctv_core::models::UserId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, Status> {
    public_id_codec
        .encode_user_id(id)
        .map_err(|error| Status::internal(format!("failed to encode user id: {error}")))
}

fn room_settings_to_client_proto(
    settings: &synctv_core::models::RoomSettings,
) -> client_proto::RoomSettings {
    client_proto::RoomSettings {
        allow_guest_join: settings.allow_guest_join.0,
        max_members: settings.max_members.0,
        require_approval: settings.require_approval.0,
        allow_auto_join: settings.allow_auto_join.0,
        chat_enabled: settings.chat_enabled.0,
        voice_chat_enabled: settings.voice_chat_enabled.0,
        p2p_media_enabled: settings.p2p_media_enabled.0,
        auto_play: Some(client_proto::AutoPlaySettings {
            enabled: settings.auto_play.value.enabled,
            mode: match settings.auto_play.value.mode {
                synctv_core::models::PlayMode::Sequential => client_proto::PlayMode::Sequential,
                synctv_core::models::PlayMode::RepeatOne => client_proto::PlayMode::RepeatOne,
                synctv_core::models::PlayMode::RepeatAll => client_proto::PlayMode::RepeatAll,
                synctv_core::models::PlayMode::Shuffle => client_proto::PlayMode::Shuffle,
            } as i32,
            delay: settings.auto_play.value.delay,
        }),
        admin_added_permissions: settings.admin_added_permissions.0,
        admin_removed_permissions: settings.admin_removed_permissions.0,
        member_added_permissions: settings.member_added_permissions.0,
        member_removed_permissions: settings.member_removed_permissions.0,
        guest_added_permissions: settings.guest_added_permissions.0,
        guest_removed_permissions: settings.guest_removed_permissions.0,
    }
}

fn room_category_to_client_proto(
    category: &synctv_core::models::RoomCategory,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::RoomCategory, Status> {
    Ok(client_proto::RoomCategory {
        id: public_id_codec
            .encode_room_category_id(category.id)
            .map_err(|error| {
                Status::internal(format!("failed to encode room category id: {error}"))
            })?,
        key: category.key.clone(),
        name: category.name.clone(),
        description: category.description.clone(),
        sort_order: category.sort_order,
        is_enabled: category.is_enabled,
    })
}

fn room_label_to_client_proto(
    label: &synctv_core::models::RoomLabel,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::RoomLabel, Status> {
    Ok(client_proto::RoomLabel {
        id: public_id_codec
            .encode_room_label_id(label.id)
            .map_err(|error| {
                Status::internal(format!("failed to encode room label id: {error}"))
            })?,
        key: label.key.clone(),
        name: label.name.clone(),
        description: label.description.clone(),
        color: label.color.clone(),
        category_id: label
            .category_id
            .map(|id| public_id_codec.encode_room_category_id(id))
            .transpose()
            .map_err(|error| {
                Status::internal(format!("failed to encode room label category id: {error}"))
            })?
            .unwrap_or_default(),
        sort_order: label.sort_order,
        is_enabled: label.is_enabled,
    })
}

fn user_public_view_to_client_proto(
    user: &synctv_core::models::User,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::UserPublicView, Status> {
    Ok(client_proto::UserPublicView {
        id: encode_user_id(user.id, public_id_codec)?,
        username: user.username.clone(),
        role: i32::from(user.role),
        created_at: user.created_at.timestamp(),
        avatar_url: String::new(),
        avatar: None,
        avatar_access: None,
    })
}

pub(crate) fn created_room_to_client_proto(
    room: &synctv_core::models::Room,
    settings: &synctv_core::models::RoomSettings,
    member_count: i32,
    creator: &synctv_core::models::User,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::Room, Status> {
    Ok(client_proto::Room {
        id: encode_room_id(room.id, public_id_codec)?,
        name: room.name.clone(),
        created_by: encode_user_id(room.created_by, public_id_codec)?,
        status: i32::from(room.status),
        settings: Some(room_settings_to_client_proto(settings)),
        created_at: room.created_at.timestamp(),
        member_count,
        description: room.description.clone(),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        availability: client_proto::ResourceAvailability::Available as i32,
        version: i64::from(room.version),
        cover: None,
        presence: None,
        creator: Some(user_public_view_to_client_proto(creator, public_id_codec)?),
        creator_blocked: false,
        category: room
            .category
            .as_ref()
            .map(|category| room_category_to_client_proto(category, public_id_codec))
            .transpose()?,
        labels: room
            .labels
            .iter()
            .map(|label| room_label_to_client_proto(label, public_id_codec))
            .collect::<Result<Vec<_>, _>>()?,
        is_public: Some(room.is_public),
    })
}

pub(crate) fn created_playlist_to_client_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i64,
    viewer_id: synctv_core::models::UserId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::Playlist, Status> {
    if playlist.source_provider.is_some() && playlist.source_config.is_none() {
        return Err(Status::internal(format!(
            "Dynamic playlist {} missing source_config",
            playlist.id
        )));
    }
    if playlist.source_provider.is_none() && playlist.source_config.is_some() {
        return Err(Status::internal(
            "playlist source_config is present without source_provider",
        ));
    }

    Ok(client_proto::Playlist {
        id: public_id_codec
            .encode_playlist_id(playlist.id)
            .map_err(|error| Status::internal(format!("failed to encode playlist id: {error}")))?,
        room_id: encode_room_id(playlist.room_id, public_id_codec)?,
        name: playlist.name.clone(),
        parent_id: playlist
            .parent_id
            .map(|id| public_id_codec.encode_playlist_id(id))
            .transpose()
            .map_err(|error| {
                Status::internal(format!("failed to encode parent playlist id: {error}"))
            })?
            .unwrap_or_default(),
        position: playlist.position,
        is_dynamic: playlist.is_dynamic(),
        item_count: i64_to_i32(item_count, "playlist item count")?,
        created_at: playlist.created_at.timestamp(),
        updated_at: playlist.updated_at.timestamp(),
        availability: client_proto::ResourceAvailability::Available as i32,
        version: i64::from(playlist.version),
        source_config: match (
            &playlist.source_config,
            playlist.creator_id == Some(viewer_id),
        ) {
            (Some(config), true) => Some(playlist_source_config_to_proto(config)),
            _ => None,
        },
        source_provider: playlist.source_provider.map_or(
            source_config_proto::SourceProvider::Unspecified as i32,
            source_provider_to_proto,
        ),
        provider_instance_name: playlist.provider_instance_name.clone().unwrap_or_default(),
        description: playlist.description.clone(),
        cover: None,
        metadata: None,
        creator_id: playlist
            .creator_id
            .map(|id| encode_user_id(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        browse_access_mode: match playlist.browse_access_mode {
            synctv_core::models::PlaylistBrowseAccessMode::Default => {
                client_proto::PlaylistBrowseAccessMode::Default as i32
            }
            synctv_core::models::PlaylistBrowseAccessMode::RoomMembers => {
                client_proto::PlaylistBrowseAccessMode::RoomMembers as i32
            }
            synctv_core::models::PlaylistBrowseAccessMode::CreatorOnly => {
                client_proto::PlaylistBrowseAccessMode::CreatorOnly as i32
            }
        },
    })
}

pub(crate) fn created_media_to_client_proto(
    media: &synctv_core::models::Media,
    viewer_id: synctv_core::models::UserId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<client_proto::Media, Status> {
    let can_view_source = media.creator_id == Some(viewer_id);

    Ok(client_proto::Media {
        id: encode_media_id(media.id, public_id_codec)?,
        room_id: encode_room_id(media.room_id, public_id_codec)?,
        source_provider: source_provider_to_proto(media.source_provider),
        name: media.name.clone(),
        metadata: can_view_source.then(|| media_resource_metadata_to_proto(&media.source_config)),
        position: media.position,
        added_at: media.added_at.timestamp(),
        creator_id: media
            .creator_id
            .map(|id| encode_user_id(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: can_view_source.then(|| media_source_config_to_proto(&media.source_config)),
        availability: client_proto::ResourceAvailability::Available as i32,
        version: i64::from(media.version),
        cover: None,
        description: media.description.clone(),
        thumbnail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{media_source_config_to_proto, playback_proxy_mode_to_proto};
    use synctv_core::models::{
        ExternalLiveSourceConfig, LiveProxyMediaSourceConfig, MediaSourceConfig, PlaybackProxyMode,
    };
    use synctv_proto::source_config::PlaybackProxyMode as ProtoPlaybackProxyMode;

    #[test]
    fn maps_direct_playback_proxy_modes() {
        assert_eq!(
            playback_proxy_mode_to_proto(PlaybackProxyMode::DirectPrefer),
            ProtoPlaybackProxyMode::DirectPrefer as i32,
        );
        assert_eq!(
            playback_proxy_mode_to_proto(PlaybackProxyMode::DirectOnly),
            ProtoPlaybackProxyMode::DirectOnly as i32,
        );
    }

    #[test]
    fn management_response_redacts_whep_authorization() {
        use synctv_proto::source_config::{
            live_proxy_media_source_config::Source, media_source_config::Provider,
        };

        let converted = media_source_config_to_proto(&MediaSourceConfig::LiveProxy(
            LiveProxyMediaSourceConfig {
                source: ExternalLiveSourceConfig::Whep {
                    url: "https://media.example.com/whep/channel".to_string(),
                    authorization: Some("Bearer upstream-secret".to_string()),
                },
            },
        ));
        let Some(Provider::LiveProxy(proxy)) = converted.provider else {
            panic!("expected live proxy source config");
        };
        let Some(Source::Whep(whep)) = proxy.source else {
            panic!("expected WHEP source config");
        };

        assert_eq!(whep.url, "https://media.example.com/whep/channel");
        assert!(whep.authorization.is_none());
    }
}
