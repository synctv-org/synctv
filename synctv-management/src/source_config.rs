use synctv_proto::source_config::{
    bilibili_media_source_config, BilibiliLiveSourceConfig, BilibiliMediaSourceConfig,
    BilibiliPgcSourceConfig, BilibiliVideoSourceConfig, DirectUrlMediaSourceConfig,
    EmbyMediaSourceConfig, EmbyPlaylistSourceConfig, MediaSourceConfig, PlaylistSourceConfig,
};
use tonic::Status;

fn trimmed_required(field_name: &str, value: &str) -> Result<String, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn alist_media_config(
    server_id: &str,
    path: &str,
    password: &str,
) -> Result<synctv_proto::source_config::AlistMediaSourceConfig, Status> {
    Ok(synctv_proto::source_config::AlistMediaSourceConfig {
        server_id: trimmed_required("server_id", server_id)?,
        path: trimmed_required("path", path)?,
        password: optional_trimmed(password),
    })
}

pub(crate) fn alist_media_source_config(
    server_id: &str,
    path: &str,
    password: &str,
) -> Result<MediaSourceConfig, Status> {
    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::Alist(alist_media_config(
                server_id, path, password,
            )?),
        ),
    })
}

pub(crate) fn alist_playlist_source_config(
    server_id: &str,
    path: &str,
    password: &str,
) -> Result<PlaylistSourceConfig, Status> {
    Ok(PlaylistSourceConfig {
        provider: Some(
            synctv_proto::source_config::playlist_source_config::Provider::Alist(
                synctv_proto::source_config::AlistPlaylistSourceConfig {
                    server_id: trimmed_required("server_id", server_id)?,
                    path: trimmed_required("path", path)?,
                    password: optional_trimmed(password),
                },
            ),
        ),
    })
}

pub(crate) fn emby_media_source_config(
    server_id: &str,
    item_id: &str,
) -> Result<MediaSourceConfig, Status> {
    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::Emby(
                EmbyMediaSourceConfig {
                    server_id: trimmed_required("server_id", server_id)?,
                    item_id: trimmed_required("item_id", item_id)?,
                },
            ),
        ),
    })
}

pub(crate) fn emby_playlist_source_config(
    server_id: &str,
    item_id: &str,
) -> Result<PlaylistSourceConfig, Status> {
    Ok(PlaylistSourceConfig {
        provider: Some(
            synctv_proto::source_config::playlist_source_config::Provider::Emby(
                EmbyPlaylistSourceConfig {
                    server_id: trimmed_required("server_id", server_id)?,
                    source: Some(
                        synctv_proto::source_config::emby_playlist_source_config::Source::Folder(
                            synctv_proto::source_config::EmbyFolderPlaylistSource {
                                item_id: trimmed_required("item_id", item_id)?,
                            },
                        ),
                    ),
                },
            ),
        ),
    })
}

pub(crate) fn bilibili_video_source_config(
    bvid: &str,
    aid: Option<u64>,
    cid: u64,
    shared: bool,
) -> Result<MediaSourceConfig, Status> {
    if bvid.trim().is_empty() && aid.is_none() {
        return Err(Status::invalid_argument("bvid or aid is required"));
    }
    if cid == 0 {
        return Err(Status::invalid_argument("cid must be non-zero"));
    }

    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::Bilibili(
                BilibiliMediaSourceConfig {
                    source: Some(bilibili_media_source_config::Source::Video(
                        BilibiliVideoSourceConfig {
                            bvid: bvid.trim().to_string(),
                            aid,
                            cid,
                            shared,
                        },
                    )),
                },
            ),
        ),
    })
}

pub(crate) fn bilibili_pgc_source_config(
    epid: u64,
    cid: u64,
    shared: bool,
) -> Result<MediaSourceConfig, Status> {
    if epid == 0 {
        return Err(Status::invalid_argument("epid must be non-zero"));
    }
    if cid == 0 {
        return Err(Status::invalid_argument("cid must be non-zero"));
    }
    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::Bilibili(
                BilibiliMediaSourceConfig {
                    source: Some(bilibili_media_source_config::Source::Pgc(
                        BilibiliPgcSourceConfig { epid, cid, shared },
                    )),
                },
            ),
        ),
    })
}

pub(crate) fn bilibili_live_source_config(
    room_live_id: u64,
    shared: bool,
) -> Result<MediaSourceConfig, Status> {
    if room_live_id == 0 {
        return Err(Status::invalid_argument("room_live_id must be non-zero"));
    }
    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::Bilibili(
                BilibiliMediaSourceConfig {
                    source: Some(bilibili_media_source_config::Source::Live(
                        BilibiliLiveSourceConfig {
                            room_id: room_live_id,
                            shared,
                        },
                    )),
                },
            ),
        ),
    })
}

pub(crate) fn direct_url_source_config(
    config: DirectUrlMediaSourceConfig,
) -> Result<MediaSourceConfig, Status> {
    if config.medias.is_empty() {
        return Err(Status::invalid_argument(
            "source_config.medias must contain at least one media",
        ));
    }

    Ok(MediaSourceConfig {
        provider: Some(
            synctv_proto::source_config::media_source_config::Provider::DirectUrl(config),
        ),
    })
}
