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
    }
}

fn playlist_source_config_to_proto(
    config: &synctv_core::models::PlaylistSourceConfig,
) -> source_config_proto::PlaylistSourceConfig {
    use source_config_proto::playlist_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::PlaylistSourceConfig::Alist(config) => {
            Provider::Alist(source_config_proto::AlistPlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
                password: config.password,
            })
        }
        synctv_core::models::PlaylistSourceConfig::Emby(config) => {
            Provider::Emby(source_config_proto::EmbyPlaylistSourceConfig {
                server_id: config.server_id,
                item_id: config.item_id,
            })
        }
        synctv_core::models::PlaylistSourceConfig::Cloudreve(config) => {
            Provider::Cloudreve(source_config_proto::CloudrevePlaylistSourceConfig {
                server_id: config.server_id,
                path: config.path,
            })
        }
    };

    source_config_proto::PlaylistSourceConfig {
        provider: Some(provider),
    }
}

fn media_source_config_to_proto(
    config: &synctv_core::models::MediaSourceConfig,
) -> source_config_proto::MediaSourceConfig {
    use source_config_proto::media_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::MediaSourceConfig::DirectUrl(config) => {
            Provider::DirectUrl(source_config_proto::DirectUrlMediaSourceConfig {
                is_live: config.is_live,
                duration_seconds: config.duration_seconds,
                prefer_proxy: config.prefer_proxy,
                medias: config
                    .medias
                    .into_iter()
                    .map(|media| source_config_proto::DirectUrlMediaResourceConfig {
                        name: media.name,
                        url: media.url,
                        headers: media.headers,
                        format: media.format,
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
                        },
                    )
                    .collect(),
                default_danmaku_index: optional_index_to_proto(config.default_danmaku_index),
            })
        }
        synctv_core::models::MediaSourceConfig::Bilibili(config) => {
            use source_config_proto::bilibili_media_source_config::Source;
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
            })
        }
        synctv_core::models::MediaSourceConfig::Alist(config) => {
            Provider::Alist(source_config_proto::AlistMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                password: config.password,
            })
        }
        synctv_core::models::MediaSourceConfig::Emby(config) => {
            Provider::Emby(source_config_proto::EmbyMediaSourceConfig {
                server_id: config.server_id,
                item_id: config.item_id,
            })
        }
        synctv_core::models::MediaSourceConfig::Rtmp(_) => {
            Provider::Rtmp(source_config_proto::RtmpMediaSourceConfig {})
        }
        synctv_core::models::MediaSourceConfig::LiveProxy(config) => {
            Provider::LiveProxy(source_config_proto::LiveProxyMediaSourceConfig { url: config.url })
        }
        synctv_core::models::MediaSourceConfig::Cloudreve(config) => {
            Provider::Cloudreve(source_config_proto::CloudreveMediaSourceConfig {
                server_id: config.server_id,
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
        synctv_core::models::MediaSourceConfig::Alist(config) => {
            format!(
                "alist://{}/{}",
                config.server_id,
                config.path.trim_start_matches('/')
            )
        }
        synctv_core::models::MediaSourceConfig::Emby(config) => {
            format!("emby://{}/{}", config.server_id, config.item_id)
        }
        synctv_core::models::MediaSourceConfig::Rtmp(_) => "rtmp".to_string(),
        synctv_core::models::MediaSourceConfig::LiveProxy(config) => config.url.clone(),
        synctv_core::models::MediaSourceConfig::Cloudreve(config) => config.path.clone(),
    };

    client_proto::ResourceMetadata {
        source: Some(source),
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
        favorited: false,
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
        creator_id: playlist
            .creator_id
            .map(|id| encode_user_id(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
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
