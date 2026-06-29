use synctv_core::service::room::ClientResourceAvailability;
use synctv_proto::client as client_proto;
use synctv_proto::source_config as source_config_proto;

pub(crate) use crate::impls::source_provider::{
    core_source_provider_to_proto,
    proto_source_provider_filter as optional_proto_source_provider_to_core,
    proto_source_provider_required as proto_source_provider_to_core,
};

pub(crate) struct PlaybackHttpSigningContext<'a> {
    pub signing_key: &'a synctv_core::proxy_signature::ProxySigningKey,
    pub room_id: &'a str,
    pub user_id: &'a str,
}

fn proto_encode_error(kind: &str, error: &str) -> crate::impls::ApiError {
    crate::impls::ApiError::Internal(format!("Failed to encode {kind} public id: {error}"))
}

fn encode_room_id_for_proto(
    id: synctv_core::models::RoomId,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_room_id(id)
        .map_err(|error| proto_encode_error("room", &error))
}

pub(crate) fn room_category_to_proto(
    category: &synctv_core::models::RoomCategory,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::RoomCategory, crate::impls::ApiError> {
    Ok(synctv_proto::client::RoomCategory {
        id: public_id_codec
            .encode_room_category_id(category.id)
            .map_err(|error| proto_encode_error("room category", &error))?,
        key: category.key.clone(),
        name: category.name.clone(),
        description: category.description.clone(),
        sort_order: category.sort_order,
        is_enabled: category.is_enabled,
    })
}

pub(crate) fn room_label_to_proto(
    label: &synctv_core::models::RoomLabel,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::RoomLabel, crate::impls::ApiError> {
    Ok(synctv_proto::client::RoomLabel {
        id: public_id_codec
            .encode_room_label_id(label.id)
            .map_err(|error| proto_encode_error("room label", &error))?,
        key: label.key.clone(),
        name: label.name.clone(),
        description: label.description.clone(),
        color: label.color.clone(),
        category_id: label
            .category_id
            .map(|id| {
                public_id_codec
                    .encode_room_category_id(id)
                    .map_err(|error| proto_encode_error("room category", &error))
            })
            .transpose()?
            .unwrap_or_default(),
        sort_order: label.sort_order,
        is_enabled: label.is_enabled,
    })
}

fn encode_media_id_for_proto(
    id: synctv_core::models::MediaId,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| proto_encode_error("media", &error))
}

fn encode_playlist_id_for_proto(
    id: synctv_core::models::PlaylistId,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| proto_encode_error("playlist", &error))
}

fn encode_user_id_for_proto(
    id: synctv_core::models::UserId,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_user_id(id)
        .map_err(|error| proto_encode_error("user", &error))
}

pub(crate) fn room_settings_to_proto(
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

pub(crate) fn room_settings_from_proto(
    settings: Option<client_proto::RoomSettings>,
) -> Result<synctv_core::models::RoomSettings, crate::impls::ApiError> {
    let settings = settings
        .ok_or_else(|| crate::impls::ApiError::InvalidInput("settings are required".to_string()))?;
    let auto_play = settings.auto_play.unwrap_or_default();
    let play_mode = match client_proto::PlayMode::try_from(auto_play.mode).map_err(|_| {
        crate::impls::ApiError::InvalidInput("Unsupported auto_play.mode".to_string())
    })? {
        client_proto::PlayMode::Unspecified | client_proto::PlayMode::Sequential => {
            synctv_core::models::PlayMode::Sequential
        }
        client_proto::PlayMode::RepeatOne => synctv_core::models::PlayMode::RepeatOne,
        client_proto::PlayMode::RepeatAll => synctv_core::models::PlayMode::RepeatAll,
        client_proto::PlayMode::Shuffle => synctv_core::models::PlayMode::Shuffle,
    };
    let settings = synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(
            settings.allow_guest_join,
        ),
        max_members: synctv_core::models::room_settings::MaxMembers(settings.max_members),
        require_approval: synctv_core::models::room_settings::RequireApproval(
            settings.require_approval,
        ),
        allow_auto_join: synctv_core::models::room_settings::AllowAutoJoin(
            settings.allow_auto_join,
        ),
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(settings.chat_enabled),
        auto_play: synctv_core::models::room_settings::AutoPlay::new(
            synctv_core::models::AutoPlaySettings {
                enabled: auto_play.enabled,
                mode: play_mode,
                delay: auto_play.delay,
            },
        ),
        admin_added_permissions: synctv_core::models::room_settings::AdminAddedPermissions(
            settings.admin_added_permissions,
        ),
        admin_removed_permissions: synctv_core::models::room_settings::AdminRemovedPermissions(
            settings.admin_removed_permissions,
        ),
        member_added_permissions: synctv_core::models::room_settings::MemberAddedPermissions(
            settings.member_added_permissions,
        ),
        member_removed_permissions: synctv_core::models::room_settings::MemberRemovedPermissions(
            settings.member_removed_permissions,
        ),
        guest_added_permissions: synctv_core::models::room_settings::GuestAddedPermissions(
            settings.guest_added_permissions,
        ),
        guest_removed_permissions: synctv_core::models::room_settings::GuestRemovedPermissions(
            settings.guest_removed_permissions,
        ),
    };
    settings.validate().map_err(crate::impls::ApiError::from)?;
    Ok(settings)
}

pub(crate) fn apply_room_settings_patch_from_proto(
    mut settings: synctv_core::models::RoomSettings,
    patch: Option<client_proto::RoomSettingsPatch>,
) -> Result<synctv_core::models::RoomSettings, crate::impls::ApiError> {
    let patch = patch.ok_or_else(|| {
        crate::impls::ApiError::InvalidInput("settings patch is required".to_string())
    })?;
    if let Some(value) = patch.allow_guest_join {
        settings.allow_guest_join = synctv_core::models::room_settings::AllowGuestJoin(value);
    }
    if let Some(value) = patch.max_members {
        settings.max_members = synctv_core::models::room_settings::MaxMembers(value);
    }
    if let Some(value) = patch.require_approval {
        settings.require_approval = synctv_core::models::room_settings::RequireApproval(value);
    }
    if let Some(value) = patch.allow_auto_join {
        settings.allow_auto_join = synctv_core::models::room_settings::AllowAutoJoin(value);
    }
    if let Some(value) = patch.chat_enabled {
        settings.chat_enabled = synctv_core::models::room_settings::ChatEnabled(value);
    }
    if let Some(auto_play) = patch.auto_play {
        let mut value = settings.auto_play.value.clone();
        if let Some(enabled) = auto_play.enabled {
            value.enabled = enabled;
        }
        if let Some(mode) = auto_play.mode {
            value.mode = match client_proto::PlayMode::try_from(mode).map_err(|_| {
                crate::impls::ApiError::InvalidInput("Unsupported auto_play.mode".to_string())
            })? {
                client_proto::PlayMode::Unspecified | client_proto::PlayMode::Sequential => {
                    synctv_core::models::PlayMode::Sequential
                }
                client_proto::PlayMode::RepeatOne => synctv_core::models::PlayMode::RepeatOne,
                client_proto::PlayMode::RepeatAll => synctv_core::models::PlayMode::RepeatAll,
                client_proto::PlayMode::Shuffle => synctv_core::models::PlayMode::Shuffle,
            };
        }
        if let Some(delay) = auto_play.delay {
            value.delay = delay;
        }
        settings.auto_play = synctv_core::models::room_settings::AutoPlay::new(value);
    }
    if let Some(value) = patch.admin_added_permissions {
        settings.admin_added_permissions =
            synctv_core::models::room_settings::AdminAddedPermissions(value);
    }
    if let Some(value) = patch.admin_removed_permissions {
        settings.admin_removed_permissions =
            synctv_core::models::room_settings::AdminRemovedPermissions(value);
    }
    if let Some(value) = patch.member_added_permissions {
        settings.member_added_permissions =
            synctv_core::models::room_settings::MemberAddedPermissions(value);
    }
    if let Some(value) = patch.member_removed_permissions {
        settings.member_removed_permissions =
            synctv_core::models::room_settings::MemberRemovedPermissions(value);
    }
    if let Some(value) = patch.guest_added_permissions {
        settings.guest_added_permissions =
            synctv_core::models::room_settings::GuestAddedPermissions(value);
    }
    if let Some(value) = patch.guest_removed_permissions {
        settings.guest_removed_permissions =
            synctv_core::models::room_settings::GuestRemovedPermissions(value);
    }
    settings.validate().map_err(crate::impls::ApiError::from)?;
    Ok(settings)
}

pub(crate) fn file_metadata_to_proto(
    metadata: &synctv_core::models::FileMetadata,
) -> Result<Option<client_proto::FileMetadata>, crate::impls::ApiError> {
    let public = metadata.public();
    if public == synctv_core::models::FileMetadata::default() {
        return Ok(None);
    }
    Ok(Some(client_proto::FileMetadata {
        width: public.width,
        height: public.height,
        duration_seconds: public.audio.as_ref().map(|audio| audio.duration_seconds),
        bitrate_bps: public.audio.as_ref().map(|audio| audio.bitrate_bps),
        blurhash: public.blurhash,
    }))
}

pub(crate) fn file_metadata_from_proto(
    metadata: Option<&client_proto::FileMetadata>,
) -> Result<synctv_core::models::FileMetadata, crate::impls::ApiError> {
    match metadata {
        Some(metadata) => Ok(synctv_core::models::FileMetadata {
            width: metadata.width,
            height: metadata.height,
            blurhash: metadata.blurhash.clone(),
            audio: match (metadata.duration_seconds, metadata.bitrate_bps) {
                (Some(duration_seconds), Some(bitrate_bps)) => {
                    Some(synctv_core::models::FileAudioMetadata {
                        duration_seconds,
                        bitrate_bps,
                        sample_rate_hz: None,
                        channels: None,
                    })
                }
                _ => None,
            },
            ..Default::default()
        }),
        None => Ok(Default::default()),
    }
}

pub(crate) fn chat_metadata_to_proto(
    metadata: &synctv_core::models::ChatMetadata,
) -> Result<Option<client_proto::ChatMetadata>, crate::impls::ApiError> {
    if metadata.is_empty() {
        return Ok(None);
    }
    Ok(Some(client_proto::ChatMetadata {
        presentation: metadata.presentation.as_ref().map(|presentation| {
            client_proto::ChatPresentationMetadata {
                display_position: presentation.display_position.clone(),
                display_color: presentation.display_color.clone(),
            }
        }),
        playback: metadata
            .playback
            .as_ref()
            .map(|playback| client_proto::ChatPlaybackMetadata {
                media_id: playback
                    .media_id
                    .map(|id| id.as_i64().to_string())
                    .unwrap_or_default(),
                playlist_id: playback
                    .playlist_id
                    .map(|id| id.as_i64().to_string())
                    .unwrap_or_default(),
                target: playback.target.as_ref().map(provider_target_to_proto),
                position_seconds: playback.position_seconds,
            }),
    }))
}

pub(crate) fn chat_metadata_from_proto(
    metadata: Option<&client_proto::ChatMetadata>,
) -> Result<synctv_core::models::ChatMetadata, crate::impls::ApiError> {
    let Some(metadata) = metadata else {
        return Ok(synctv_core::models::ChatMetadata::default());
    };
    Ok(synctv_core::models::ChatMetadata {
        presentation: metadata.presentation.as_ref().map(|presentation| {
            synctv_core::models::ChatPresentationMetadata {
                display_position: presentation.display_position.clone(),
                display_color: presentation.display_color.clone(),
            }
        }),
        playback: None,
    })
}

pub(crate) fn content_report_metadata_to_proto(
    metadata: &synctv_core::models::ContentReportMetadata,
) -> Result<Option<client_proto::ContentReportMetadata>, crate::impls::ApiError> {
    if metadata.is_empty() {
        return Ok(None);
    }
    Ok(Some(client_proto::ContentReportMetadata {
        client_reason: metadata.client_reason.clone(),
    }))
}

pub(crate) fn content_report_metadata_from_proto(
    metadata: Option<&client_proto::ContentReportMetadata>,
) -> Result<synctv_core::models::ContentReportMetadata, crate::impls::ApiError> {
    Ok(match metadata {
        Some(metadata) => synctv_core::models::ContentReportMetadata {
            client_reason: metadata.client_reason.clone(),
        },
        None => synctv_core::models::ContentReportMetadata::default(),
    })
}

pub(crate) fn provider_target_to_proto(
    target: &synctv_core::models::ProviderTarget,
) -> client_proto::ProviderTarget {
    match target {
        synctv_core::models::ProviderTarget::Alist(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Alist(
                client_proto::AlistTarget {
                    relative_path: target.relative_path.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Emby(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Emby(
                client_proto::EmbyTarget {
                    item_id: target.item_id.clone(),
                },
            )),
        },
    }
}

pub(crate) fn optional_provider_target_to_proto(
    target: Option<&synctv_core::models::ProviderTarget>,
) -> Option<client_proto::ProviderTarget> {
    target.map(provider_target_to_proto)
}

pub(crate) fn provider_target_from_proto(
    target: Option<client_proto::ProviderTarget>,
) -> Result<Option<synctv_core::models::ProviderTarget>, crate::impls::ApiError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let target = target.target.ok_or_else(|| {
        crate::impls::ApiError::InvalidInput("provider target oneof is required".to_string())
    })?;
    Ok(Some(match target {
        client_proto::provider_target::Target::Alist(target) => {
            synctv_core::models::ProviderTarget::alist(target.relative_path)
        }
        client_proto::provider_target::Target::Emby(target) => {
            synctv_core::models::ProviderTarget::emby(target.item_id)
        }
    }))
}

pub(crate) fn file_object_variant_to_proto(
    variant: &synctv_core::models::FileObjectVariant,
) -> Result<synctv_proto::client::FileObjectVariant, crate::impls::ApiError> {
    let metadata = synctv_core::models::FileMetadata {
        width: variant.metadata.width,
        height: variant.metadata.height,
        blurhash: variant.metadata.blurhash.clone(),
        ..Default::default()
    };
    Ok(synctv_proto::client::FileObjectVariant {
        key: variant.variant_key.clone(),
        label: variant.label.clone(),
        url: variant.url.clone().unwrap_or_default(),
        mime_type: variant.mime_type.clone(),
        size_bytes: variant.size_bytes,
        width: variant.width.unwrap_or_default(),
        height: variant.height.unwrap_or_default(),
        is_original: variant.is_original,
        lossy: variant.lossy,
        quality: variant.quality,
        metadata: file_metadata_to_proto(&metadata)?,
    })
}

pub(crate) fn file_object_variants_from_metadata(
    metadata: &synctv_core::models::FileMetadata,
    context: &'static str,
) -> Result<Vec<synctv_proto::client::FileObjectVariant>, crate::impls::ApiError> {
    let _ = context;
    metadata
        .variants
        .iter()
        .map(file_object_variant_to_proto)
        .collect()
}

#[cfg(test)]
mod file_variant_metadata_tests {
    use super::*;

    #[test]
    fn client_variants_metadata_field_is_ignored() {
        let metadata = synctv_core::models::FileMetadata::default();

        let variants = file_object_variants_from_metadata(&metadata, "test metadata")
            .expect("client metadata should parse");

        assert!(variants.is_empty());
    }

    #[test]
    fn generated_variants_metadata_uses_reserved_key() {
        let variant = synctv_core::models::FileObjectVariant {
            storage_backend: "database".to_string(),
            object_key: "objects/file-small.jpg".to_string(),
            original_storage_backend: "database".to_string(),
            original_object_key: "objects/file.jpg".to_string(),
            group_id: "fg_test".to_string(),
            variant_key: "small".to_string(),
            label: "Small".to_string(),
            url: Some("/files/small".to_string()),
            mime_type: "image/jpeg".to_string(),
            size_bytes: 1024,
            width: Some(320),
            height: Some(180),
            is_original: false,
            lossy: true,
            quality: Some(78),
            sort_order: 20,
            metadata: synctv_core::models::FileVariantMetadata::default(),
            created_at: chrono::Utc::now(),
        };
        let metadata = synctv_core::models::FileMetadata {
            variants: vec![variant],
            ..Default::default()
        };

        let variants = file_object_variants_from_metadata(&metadata, "test metadata")
            .expect("generated metadata should parse");

        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].key, "small");
        assert_eq!(variants[0].url, "/files/small");
        assert_eq!(variants[0].width, 320);
        assert_eq!(variants[0].height, 180);
    }
}

fn invalid_source_config(message: impl Into<String>) -> crate::impls::ApiError {
    crate::impls::ApiError::InvalidInput(message.into())
}

pub(crate) fn proto_media_source_config_to_core(
    config: Option<source_config_proto::MediaSourceConfig>,
) -> Result<
    (
        synctv_core::models::SourceProvider,
        synctv_core::models::MediaSourceConfig,
    ),
    crate::impls::ApiError,
> {
    use source_config_proto::media_source_config::Provider;

    let provider = config
        .and_then(|config| config.provider)
        .ok_or_else(|| invalid_source_config("source_config is required"))?;
    let config = match provider {
        Provider::DirectUrl(config) => synctv_core::models::MediaSourceConfig::DirectUrl(
            proto_direct_url_media_source_config_to_core(config)?,
        ),
        Provider::Bilibili(config) => synctv_core::models::MediaSourceConfig::Bilibili(
            proto_bilibili_media_source_config_to_core(config)?,
        ),
        Provider::Alist(config) => synctv_core::models::MediaSourceConfig::Alist(
            proto_alist_media_source_config_to_core(config)?,
        ),
        Provider::Emby(config) => synctv_core::models::MediaSourceConfig::Emby(
            proto_emby_media_source_config_to_core(config)?,
        ),
        Provider::Rtmp(config) => synctv_core::models::MediaSourceConfig::Rtmp(
            proto_rtmp_media_source_config_to_core(config)?,
        ),
        Provider::LiveProxy(config) => synctv_core::models::MediaSourceConfig::LiveProxy(
            proto_live_proxy_media_source_config_to_core(config)?,
        ),
    };

    Ok((config.provider(), config))
}

pub(crate) fn proto_playlist_source_config_to_core(
    config: Option<source_config_proto::PlaylistSourceConfig>,
) -> Result<
    (
        synctv_core::models::SourceProvider,
        synctv_core::models::PlaylistSourceConfig,
    ),
    crate::impls::ApiError,
> {
    use source_config_proto::playlist_source_config::Provider;

    let provider = config
        .and_then(|config| config.provider)
        .ok_or_else(|| invalid_source_config("source_config is required"))?;
    let config = match provider {
        Provider::Alist(config) => synctv_core::models::PlaylistSourceConfig::Alist(
            proto_alist_playlist_source_config_to_core(config)?,
        ),
        Provider::Emby(config) => synctv_core::models::PlaylistSourceConfig::Emby(
            proto_emby_playlist_source_config_to_core(config)?,
        ),
    };

    Ok((config.provider(), config))
}

fn proto_direct_url_media_source_config_to_core(
    config: source_config_proto::DirectUrlMediaSourceConfig,
) -> Result<synctv_core::models::DirectUrlMediaSourceConfig, crate::impls::ApiError> {
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

fn proto_bilibili_media_source_config_to_core(
    config: source_config_proto::BilibiliMediaSourceConfig,
) -> Result<synctv_core::models::BilibiliMediaSourceConfig, crate::impls::ApiError> {
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

fn proto_alist_media_source_config_to_core(
    config: source_config_proto::AlistMediaSourceConfig,
) -> Result<synctv_core::models::AlistMediaSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::AlistMediaSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
    })
}

fn proto_alist_playlist_source_config_to_core(
    config: source_config_proto::AlistPlaylistSourceConfig,
) -> Result<synctv_core::models::AlistPlaylistSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::AlistPlaylistSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
    })
}

fn proto_emby_media_source_config_to_core(
    config: source_config_proto::EmbyMediaSourceConfig,
) -> Result<synctv_core::models::EmbyMediaSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::EmbyMediaSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
    })
}

fn proto_emby_playlist_source_config_to_core(
    config: source_config_proto::EmbyPlaylistSourceConfig,
) -> Result<synctv_core::models::EmbyPlaylistSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::EmbyPlaylistSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
    })
}

fn proto_rtmp_media_source_config_to_core(
    _config: source_config_proto::RtmpMediaSourceConfig,
) -> Result<synctv_core::models::RtmpMediaSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::RtmpMediaSourceConfig {})
}

fn proto_live_proxy_media_source_config_to_core(
    config: source_config_proto::LiveProxyMediaSourceConfig,
) -> Result<synctv_core::models::LiveProxyMediaSourceConfig, crate::impls::ApiError> {
    Ok(synctv_core::models::LiveProxyMediaSourceConfig { url: config.url })
}

pub(crate) fn media_source_config_to_proto(
    config: &synctv_core::models::MediaSourceConfig,
) -> Result<source_config_proto::MediaSourceConfig, crate::impls::ApiError> {
    Ok(config.clone().into())
}

pub(crate) fn playlist_source_config_to_proto(
    config: &synctv_core::models::PlaylistSourceConfig,
) -> Result<source_config_proto::PlaylistSourceConfig, crate::impls::ApiError> {
    Ok(config.clone().into())
}

fn usize_to_i32(value: usize, field: &'static str) -> Result<i32, crate::impls::ApiError> {
    i32::try_from(value)
        .map_err(|_| crate::impls::ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub(crate) fn room_presence_stats_to_proto(
    stats: &synctv_core::service::OnlineRoomStats,
) -> Result<synctv_proto::common::RoomPresenceStats, crate::impls::ApiError> {
    Ok(synctv_proto::common::RoomPresenceStats {
        online_user_count: usize_to_i32(stats.online_user_count, "online user count")?,
        connection_count: usize_to_i32(stats.connection_count, "room connection count")?,
        node_connection_counts: node_connection_counts_to_proto(&stats.node_connection_counts)?,
        sampled_at: stats.sampled_at_ms / 1000,
        version: stats.version,
    })
}

fn node_connection_counts_to_proto(
    counts: &std::collections::BTreeMap<String, usize>,
) -> Result<Vec<synctv_proto::common::NodeConnectionCount>, crate::impls::ApiError> {
    counts
        .iter()
        .map(|(node_id, count)| {
            Ok(synctv_proto::common::NodeConnectionCount {
                node_id: node_id.clone(),
                connection_count: usize_to_i32(*count, "node connection count")?,
            })
        })
        .collect::<Result<Vec<_>, crate::impls::ApiError>>()
}

pub(crate) fn user_presence_stats_to_proto(
    stats: &synctv_core::service::OnlineUserStats,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::common::UserPresenceStats, crate::impls::ApiError> {
    Ok(synctv_proto::common::UserPresenceStats {
        connection_count: usize_to_i32(stats.connection_count, "user connection count")?,
        node_connection_counts: node_connection_counts_to_proto(&stats.node_connection_counts)?,
        room_count: usize_to_i32(stats.room_count, "user room count")?,
        room_ids: stats
            .rooms
            .iter()
            .copied()
            .map(|room_id| public_id_codec.encode_room_id(room_id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::impls::ApiError::InvalidInput)?,
        sampled_at: stats.sampled_at_ms / 1000,
        version: stats.version,
    })
}

pub(crate) fn node_presence_stats_to_proto(
    stats: &synctv_core::service::OnlineNodeStats,
) -> Result<synctv_proto::common::NodePresenceStats, crate::impls::ApiError> {
    Ok(synctv_proto::common::NodePresenceStats {
        node_id: stats.node_id.clone(),
        connection_count: usize_to_i32(stats.connection_count, "node connection count")?,
        online_user_count: usize_to_i32(stats.online_user_count, "node online user count")?,
        room_count: usize_to_i32(stats.room_count, "node room count")?,
        sampled_at: stats.sampled_at_ms / 1000,
        version: stats.version,
    })
}

pub(crate) fn presence_overview_to_proto(
    stats: &synctv_core::service::PresenceOverview,
) -> Result<synctv_proto::common::PresenceOverview, crate::impls::ApiError> {
    Ok(synctv_proto::common::PresenceOverview {
        online_user_count: usize_to_i32(stats.online_user_count, "online user count")?,
        connection_count: usize_to_i32(stats.connection_count, "connection count")?,
        active_room_count: usize_to_i32(stats.active_room_count, "active room count")?,
        nodes: stats
            .nodes
            .iter()
            .map(node_presence_stats_to_proto)
            .collect::<Result<Vec<_>, crate::impls::ApiError>>()?,
        sampled_at: stats.sampled_at_ms / 1000,
        version: stats.version,
    })
}

fn require_non_empty_url(url: &str, field: &'static str) -> Result<String, crate::impls::ApiError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(crate::impls::ApiError::Internal(format!(
            "{field} url is empty"
        )));
    }
    Ok(trimmed.to_string())
}

fn checked_index_i32(
    index: usize,
    len: usize,
    field: &'static str,
) -> Result<i32, crate::impls::ApiError> {
    if index >= len {
        return Err(crate::impls::ApiError::Internal(format!(
            "{field} {index} is outside item count {len}"
        )));
    }
    usize_to_i32(index, field)
}

fn can_view_media_source_config(
    media: &synctv_core::models::Media,
    viewer_id: Option<synctv_core::models::UserId>,
) -> bool {
    media
        .creator_id
        .is_some_and(|creator_id| Some(creator_id) == viewer_id)
}

fn serialize_source_config_for_viewer(
    media: &synctv_core::models::Media,
    can_view_source: bool,
) -> Result<Option<source_config_proto::MediaSourceConfig>, crate::impls::ApiError> {
    if can_view_source {
        media_source_config_to_proto(&media.source_config).map(Some)
    } else {
        Ok(None)
    }
}

fn can_view_playlist_source_config(
    playlist: &synctv_core::models::Playlist,
    viewer_id: Option<synctv_core::models::UserId>,
) -> bool {
    playlist
        .creator_id
        .is_some_and(|creator_id| Some(creator_id) == viewer_id)
}

fn serialize_playlist_source_config_for_viewer(
    playlist: &synctv_core::models::Playlist,
    viewer_id: Option<synctv_core::models::UserId>,
) -> Result<Option<source_config_proto::PlaylistSourceConfig>, crate::impls::ApiError> {
    if can_view_playlist_source_config(playlist, viewer_id) {
        match (playlist.source_provider, playlist.source_config.as_ref()) {
            (Some(_provider), Some(source_config)) => {
                playlist_source_config_to_proto(source_config).map(Some)
            }
            (Some(_), None) => Err(crate::impls::ApiError::Internal(format!(
                "Dynamic playlist {} missing source_config",
                playlist.id
            ))),
            (None, Some(_)) => Err(crate::impls::ApiError::Internal(
                "playlist source_config is present without source_provider".to_string(),
            )),
            (None, None) => Ok(None),
        }
    } else {
        Ok(None)
    }
}

fn required_cover_url(
    url: Option<&str>,
    field: &'static str,
) -> Result<String, crate::impls::ApiError> {
    let url = url
        .map(str::trim)
        .ok_or_else(|| crate::impls::ApiError::Internal(format!("{field} url is missing")))?;
    if url.is_empty() {
        return Err(crate::impls::ApiError::Internal(format!(
            "{field} url is empty"
        )));
    }
    Ok(url.to_string())
}

pub(crate) fn stored_file_reference_to_resource_cover(
    file: &synctv_core::models::StoredFileReference,
    url: Option<&str>,
) -> Result<synctv_proto::client::ResourceCover, crate::impls::ApiError> {
    Ok(synctv_proto::client::ResourceCover {
        url: required_cover_url(url, "resource cover")?,
        metadata: file_metadata_to_proto(&file.metadata)?,
        variants: file_object_variants_from_metadata(&file.metadata, "resource cover")?,
    })
}

pub(crate) fn stored_file_reference_to_media_cover(
    file: &synctv_core::models::StoredFileReference,
    url: Option<&str>,
) -> Result<synctv_proto::client::MediaCover, crate::impls::ApiError> {
    Ok(synctv_proto::client::MediaCover {
        id: file.file_reference_id.to_string(),
        url: required_cover_url(url, "media cover")?,
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        width: file.metadata.width.unwrap_or_default(),
        height: file.metadata.height.unwrap_or_default(),
        metadata: file_metadata_to_proto(&file.metadata)?,
        variants: file_object_variants_from_metadata(&file.metadata, "media cover")?,
    })
}

pub(super) fn user_role_to_proto(role: synctv_core::models::UserRole) -> i32 {
    i32::from(role)
}

pub(crate) fn user_status_to_proto(status: synctv_core::models::UserStatus) -> i32 {
    i32::from(status)
}

pub(crate) fn member_status_to_proto(status: synctv_core::models::MemberStatus) -> i32 {
    i32::from(status)
}

pub(crate) const fn resource_availability_to_proto(is_available: bool) -> i32 {
    if is_available {
        synctv_proto::client::ResourceAvailability::Available as i32
    } else {
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    }
}

pub(crate) const fn resource_availability_enum_to_proto(
    availability: ClientResourceAvailability,
) -> i32 {
    match availability {
        ClientResourceAvailability::Available => {
            synctv_proto::client::ResourceAvailability::Available as i32
        }
        ClientResourceAvailability::CreatorInactive => {
            synctv_proto::client::ResourceAvailability::CreatorInactive as i32
        }
    }
}

pub(crate) fn playback_client_profile_from_proto(
    profile: Option<&synctv_proto::client::PlaybackClientProfile>,
) -> Result<Option<synctv_core::provider::PlaybackClientProfile>, crate::impls::ApiError> {
    let Some(profile) = profile else {
        return Ok(None);
    };

    let default_profile = synctv_core::provider::PlaybackClientProfile::default();
    let stream_preference =
        match synctv_proto::client::PlaybackStreamPreference::try_from(profile.stream_preference)
            .map_err(|_| {
                crate::impls::ApiError::InvalidInput(
                    "Unsupported playback stream preference".to_string(),
                )
            })? {
            synctv_proto::client::PlaybackStreamPreference::Unspecified
            | synctv_proto::client::PlaybackStreamPreference::Auto => {
                synctv_core::provider::PlaybackStreamPreference::Auto
            }
            synctv_proto::client::PlaybackStreamPreference::DirectPlay => {
                synctv_core::provider::PlaybackStreamPreference::DirectPlay
            }
            synctv_proto::client::PlaybackStreamPreference::Transcode => {
                synctv_core::provider::PlaybackStreamPreference::Transcode
            }
        };

    let supported_video_codecs = if profile.supported_video_codecs.is_empty() {
        default_profile.supported_video_codecs.clone()
    } else {
        profile
            .supported_video_codecs
            .iter()
            .filter_map(|codec| {
                Some(
                    match synctv_proto::client::PlaybackVideoCodec::try_from(*codec) {
                        Ok(synctv_proto::client::PlaybackVideoCodec::Unspecified) => return None,
                        Ok(synctv_proto::client::PlaybackVideoCodec::H264) => {
                            Ok(synctv_core::provider::PlaybackVideoCodec::H264)
                        }
                        Ok(synctv_proto::client::PlaybackVideoCodec::Hevc) => {
                            Ok(synctv_core::provider::PlaybackVideoCodec::Hevc)
                        }
                        Ok(synctv_proto::client::PlaybackVideoCodec::Vp9) => {
                            Ok(synctv_core::provider::PlaybackVideoCodec::Vp9)
                        }
                        Ok(synctv_proto::client::PlaybackVideoCodec::Av1) => {
                            Ok(synctv_core::provider::PlaybackVideoCodec::Av1)
                        }
                        Err(_) => Err(crate::impls::ApiError::InvalidInput(
                            "Unsupported playback video codec".to_string(),
                        )),
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let supported_containers = if profile.supported_containers.is_empty() {
        default_profile.supported_containers.clone()
    } else {
        profile
            .supported_containers
            .iter()
            .filter_map(|container| {
                Some(
                    match synctv_proto::client::PlaybackContainer::try_from(*container) {
                        Ok(synctv_proto::client::PlaybackContainer::Unspecified) => return None,
                        Ok(synctv_proto::client::PlaybackContainer::Mp4) => {
                            Ok(synctv_core::provider::PlaybackContainer::Mp4)
                        }
                        Ok(synctv_proto::client::PlaybackContainer::Mkv) => {
                            Ok(synctv_core::provider::PlaybackContainer::Mkv)
                        }
                        Ok(synctv_proto::client::PlaybackContainer::Webm) => {
                            Ok(synctv_core::provider::PlaybackContainer::Webm)
                        }
                        Err(_) => Err(crate::impls::ApiError::InvalidInput(
                            "Unsupported playback container".to_string(),
                        )),
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let audio_capability =
        match synctv_proto::client::PlaybackAudioCapability::try_from(profile.audio_capability)
            .map_err(|_| {
                crate::impls::ApiError::InvalidInput(
                    "Unsupported playback audio capability".to_string(),
                )
            })? {
            synctv_proto::client::PlaybackAudioCapability::Unspecified => {
                default_profile.audio_capability
            }
            synctv_proto::client::PlaybackAudioCapability::Stereo => {
                synctv_core::provider::PlaybackAudioCapability::Stereo
            }
            synctv_proto::client::PlaybackAudioCapability::Surround => {
                synctv_core::provider::PlaybackAudioCapability::Surround
            }
            synctv_proto::client::PlaybackAudioCapability::LosslessSurround => {
                synctv_core::provider::PlaybackAudioCapability::LosslessSurround
            }
        };

    let subtitle_preference = match synctv_proto::client::PlaybackSubtitlePreference::try_from(
        profile.subtitle_preference,
    )
    .map_err(|_| {
        crate::impls::ApiError::InvalidInput("Unsupported playback subtitle preference".to_string())
    })? {
        synctv_proto::client::PlaybackSubtitlePreference::Unspecified
        | synctv_proto::client::PlaybackSubtitlePreference::External => {
            synctv_core::provider::PlaybackSubtitlePreference::External
        }
        synctv_proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal => {
            synctv_core::provider::PlaybackSubtitlePreference::EmbeddedOrExternal
        }
        synctv_proto::client::PlaybackSubtitlePreference::None => {
            synctv_core::provider::PlaybackSubtitlePreference::None
        }
    };

    Ok(Some(synctv_core::provider::PlaybackClientProfile {
        stream_preference,
        max_streaming_bitrate: profile.max_streaming_bitrate,
        max_audio_channels: profile
            .max_audio_channels
            .or(default_profile.max_audio_channels),
        supported_video_codecs,
        supported_containers,
        audio_capability,
        subtitle_preference,
    }))
}

pub(crate) fn proto_role_to_room_role(
    role_i32: i32,
) -> Result<synctv_core::models::RoomRole, crate::impls::ApiError> {
    synctv_core::models::RoomRole::try_from(role_i32).map_err(crate::impls::ApiError::InvalidInput)
}

pub(crate) fn proto_role_to_assignable_room_role(
    role_i32: i32,
) -> Result<synctv_core::models::RoomRole, crate::impls::ApiError> {
    let role = proto_role_to_room_role(role_i32)?;
    if role == synctv_core::models::RoomRole::Creator {
        return Err(crate::impls::ApiError::InvalidInput(
            "Creator role is bound to room ownership and cannot be assigned via add_member"
                .to_string(),
        ));
    }
    Ok(role)
}

pub(crate) fn proto_role_filter_to_room_role(
    role_i32: i32,
) -> Result<Option<synctv_core::models::RoomRole>, crate::impls::ApiError> {
    if role_i32 == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
        return Ok(None);
    }
    synctv_core::models::RoomRole::try_from(role_i32)
        .map(Some)
        .map_err(crate::impls::ApiError::InvalidInput)
}

pub(crate) fn proto_role_to_user_role(
    role_i32: i32,
) -> Result<synctv_core::models::UserRole, crate::impls::ApiError> {
    synctv_core::models::UserRole::try_from(role_i32).map_err(crate::impls::ApiError::InvalidInput)
}

#[must_use]
pub(crate) fn room_role_to_proto(role: synctv_core::models::RoomRole) -> i32 {
    i32::from(role)
}

pub(crate) fn try_user_to_proto(
    user: &synctv_core::models::User,
    email: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::User, crate::impls::ApiError> {
    Ok(synctv_proto::client::User {
        id: public_id_codec
            .encode_user_id(user.id)
            .map_err(|error| proto_encode_error("user", &error))?,
        username: user.username.clone(),
        email: email.unwrap_or_default().to_string(),
        role: user_role_to_proto(user.role),
        status: user_status_to_proto(user.status),
        created_at: user.created_at.timestamp(),
        is_banned: user.is_banned,
        avatar_url: String::new(),
        avatar: None,
    })
}

pub(crate) fn try_user_public_view_to_proto(
    user: &synctv_core::models::User,
    avatar_url: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::UserPublicView, crate::impls::ApiError> {
    Ok(synctv_proto::client::UserPublicView {
        id: public_id_codec
            .encode_user_id(user.id)
            .map_err(|error| proto_encode_error("user", &error))?,
        username: user.username.clone(),
        role: user_role_to_proto(user.role),
        created_at: user.created_at.timestamp(),
        avatar_url: avatar_url.unwrap_or_default().to_string(),
        avatar: None,
    })
}

#[cfg(test)]
pub(crate) fn try_room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    try_room_to_proto_with_availability_and_presence(
        room,
        settings,
        member_count,
        ClientResourceAvailability::Available,
        None,
        None,
        public_id_codec,
    )
}

pub(crate) fn try_room_to_proto_basic_with_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    creator: Option<synctv_proto::client::UserPublicView>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let mut proto = try_room_to_proto_with_availability_and_presence(
        room,
        settings,
        member_count,
        ClientResourceAvailability::Available,
        None,
        creator,
        public_id_codec,
    )?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_resource_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

pub(crate) fn try_room_to_proto_with_availability_and_presence(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    presence: Option<&synctv_core::service::OnlineRoomStats>,
    creator: Option<synctv_proto::client::UserPublicView>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let room_settings = settings.ok_or_else(|| {
        crate::impls::ApiError::Internal(format!(
            "Missing room settings for client room {}",
            room.id
        ))
    })?;
    let member_count = member_count.ok_or_else(|| {
        crate::impls::ApiError::Internal(format!(
            "Missing member count for client room {}",
            room.id
        ))
    })?;
    Ok(synctv_proto::client::Room {
        id: encode_room_id_for_proto(room.id, public_id_codec)?,
        name: room.name.clone(),
        description: room.description.clone(),
        created_by: encode_user_id_for_proto(room.created_by, public_id_codec)?,
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: Some(room_settings_to_proto(room_settings)),
        created_at: room.created_at.timestamp(),
        member_count,
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        availability: resource_availability_enum_to_proto(availability),
        version: i64::from(room.version),
        cover: None,
        presence: presence.map(room_presence_stats_to_proto).transpose()?,
        creator,
        category: room
            .category
            .as_ref()
            .map(|category| room_category_to_proto(category, public_id_codec))
            .transpose()?,
        labels: room
            .labels
            .iter()
            .map(|label| room_label_to_proto(label, public_id_codec))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_room_to_proto_with_availability_presence_and_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    presence: Option<&synctv_core::service::OnlineRoomStats>,
    creator: Option<synctv_proto::client::UserPublicView>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let mut proto = try_room_to_proto_with_availability_and_presence(
        room,
        settings,
        member_count,
        availability,
        presence,
        creator,
        public_id_codec,
    )?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_resource_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

#[cfg(test)]
pub(super) fn hot_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    online_count: i32,
    total_members: i32,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::RoomWithStats, crate::impls::ApiError> {
    Ok(synctv_proto::client::RoomWithStats {
        room: Some(try_room_to_proto_basic(
            room,
            settings,
            Some(total_members),
            public_id_codec,
        )?),
        online_count,
        total_members,
    })
}

#[must_use]
pub(crate) fn normalize_created_room_settings(
    settings: Option<&synctv_core::models::RoomSettings>,
) -> synctv_core::models::RoomSettings {
    settings.cloned().unwrap_or_default()
}

pub(crate) fn try_media_to_proto(
    media: &synctv_core::models::Media,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    try_media_to_proto_for_viewer(media, true, None, public_id_codec)
}

pub(crate) fn try_media_to_proto_with_availability(
    media: &synctv_core::models::Media,
    is_available: bool,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    try_media_to_proto_for_viewer(media, is_available, None, public_id_codec)
}

pub(crate) fn try_media_to_proto_for_viewer(
    media: &synctv_core::models::Media,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let can_view_source = can_view_media_source_config(media, viewer_id);
    Ok(synctv_proto::client::Media {
        id: encode_media_id_for_proto(media.id, public_id_codec)?,
        room_id: encode_room_id_for_proto(media.room_id, public_id_codec)?,
        source_provider: core_source_provider_to_proto(media.source_provider),
        name: media.name.clone(),
        metadata: can_view_source.then(|| media_resource_metadata_to_proto(&media.source_config)),
        position: media.position,
        added_at: media.added_at.timestamp(),
        creator_id: media
            .creator_id
            .map(|id| encode_user_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: serialize_source_config_for_viewer(media, can_view_source)?,
        availability: resource_availability_to_proto(is_available),
        version: i64::from(media.version),
        description: media.description.clone(),
        cover: None,
    })
}

fn media_resource_metadata_to_proto(
    source_config: &synctv_core::models::MediaSourceConfig,
) -> synctv_proto::client::ResourceMetadata {
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
    };

    synctv_proto::client::ResourceMetadata {
        source: Some(source),
    }
}

pub(crate) fn try_media_to_proto_for_viewer_with_cover(
    media: &synctv_core::models::Media,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let mut proto = try_media_to_proto_for_viewer(media, is_available, viewer_id, public_id_codec)?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_media_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

pub(crate) fn try_playlist_to_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    try_playlist_to_proto_with_availability(playlist, item_count, true, public_id_codec)
}

pub(crate) fn try_playlist_to_proto_with_availability(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    try_playlist_to_proto_for_viewer(playlist, item_count, is_available, None, public_id_codec)
}

pub(crate) fn try_playlist_to_proto_for_viewer(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    if playlist.source_provider.is_some() && playlist.source_config.is_none() {
        return Err(crate::impls::ApiError::Internal(format!(
            "Dynamic playlist {} missing source_config",
            playlist.id
        )));
    }
    if playlist.source_provider.is_none() && playlist.source_config.is_some() {
        return Err(crate::impls::ApiError::Internal(
            "playlist source_config is present without source_provider".to_string(),
        ));
    }

    let source_provider = match playlist.source_provider {
        Some(provider) => core_source_provider_to_proto(provider),
        None => source_config_proto::SourceProvider::Unspecified as i32,
    };

    Ok(synctv_proto::client::Playlist {
        id: encode_playlist_id_for_proto(playlist.id, public_id_codec)?,
        room_id: encode_room_id_for_proto(playlist.room_id, public_id_codec)?,
        name: playlist.name.clone(),
        parent_id: playlist
            .parent_id
            .map(|id| encode_playlist_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        position: playlist.position,
        is_dynamic: playlist.is_dynamic(),
        item_count,
        created_at: playlist.created_at.timestamp(),
        updated_at: playlist.updated_at.timestamp(),
        availability: resource_availability_to_proto(is_available),
        version: i64::from(playlist.version),
        source_config: serialize_playlist_source_config_for_viewer(playlist, viewer_id)?,
        source_provider,
        provider_instance_name: playlist.provider_instance_name.clone().unwrap_or_default(),
        description: playlist.description.clone(),
        cover: None,
    })
}

pub(crate) struct DynamicPlaylistSourceFields<'a> {
    pub provider: synctv_core::models::SourceProvider,
    pub source_config: &'a synctv_core::models::PlaylistSourceConfig,
    pub provider_instance_name: Option<&'a str>,
}

pub(crate) fn dynamic_playlist_source_fields(
    playlist: &synctv_core::models::Playlist,
) -> Result<DynamicPlaylistSourceFields<'_>, crate::impls::ApiError> {
    let provider = playlist.source_provider.ok_or_else(|| {
        crate::impls::ApiError::Internal("Dynamic playlist missing provider".to_string())
    })?;
    let source_config = playlist.source_config.as_ref().ok_or_else(|| {
        crate::impls::ApiError::Internal(format!(
            "Dynamic playlist {} missing source_config",
            playlist.id
        ))
    })?;
    let provider_instance_name = playlist.provider_instance_name.as_deref().and_then(|name| {
        let trimmed = name.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    });

    Ok(DynamicPlaylistSourceFields {
        provider,
        source_config,
        provider_instance_name,
    })
}

pub(crate) fn try_playlist_to_proto_for_viewer_with_cover(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    let mut proto = try_playlist_to_proto_for_viewer(
        playlist,
        item_count,
        is_available,
        viewer_id,
        public_id_codec,
    )?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_resource_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

pub(crate) fn try_playlist_path_node_to_proto(
    playlist: &synctv_core::models::Playlist,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::PlaylistBrowsePathNode, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaylistBrowsePathNode {
        playlist_id: encode_playlist_id_for_proto(playlist.id, public_id_codec)?,
        name: playlist.name.clone(),
        target: None,
    })
}

pub(crate) fn try_playback_state_to_proto(
    state: &synctv_core::models::RoomPlaybackState,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::client::PlaybackState, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaybackState {
        room_id: encode_room_id_for_proto(state.room_id, public_id_codec)?,
        playing_media_id: state
            .playing_media_id
            .map(|id| encode_media_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        position: state.computed_position(),
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version,
        playing_playlist_id: state
            .playing_playlist_id
            .map(|id| encode_playlist_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        target: optional_provider_target_to_proto(state.target.as_ref()),
        target_hash: state.target_hash()?,
    })
}

pub(crate) fn try_room_member_to_proto_with_permissions(
    member: &synctv_core::models::RoomMemberWithUser,
    permissions: synctv_core::models::RoomPermissionSet,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::common::RoomMember, crate::impls::ApiError> {
    Ok(synctv_proto::common::RoomMember {
        room_id: encode_room_id_for_proto(member.room_id, public_id_codec)?,
        user_id: encode_user_id_for_proto(member.user_id, public_id_codec)?,
        username: member.username.clone(),
        role: room_role_to_proto(member.role),
        permissions: permissions.0,
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
        connection_count: 0,
    })
}

pub(crate) fn try_members_to_proto(
    members: &[synctv_core::models::RoomMemberWithUser],
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<Vec<synctv_proto::common::RoomMember>, crate::impls::ApiError> {
    members
        .iter()
        .map(|m| {
            let permissions =
                permission_service.effective_member_with_user_permissions(m, room_settings);
            try_room_member_to_proto_with_permissions(m, permissions, public_id_codec)
        })
        .collect()
}

/// Convert provider `PlaybackInfo` to models `PlaybackInfo`
#[must_use]
pub(crate) fn provider_playback_info_to_model(
    info: &synctv_core::provider::traits::PlaybackInfo,
) -> synctv_core::models::media::PlaybackInfo {
    synctv_core::models::media::PlaybackInfo {
        medias: info.medias.clone(),
        default_media_index: info.default_media_index,
        subtitles: info.subtitles.clone(),
        default_subtitle_index: info.default_subtitle_index,
        danmakus: info.danmakus.clone(),
        default_danmaku_index: info.default_danmaku_index,
    }
}

/// Convert models `PlaybackResult` to proto `Playback`
pub(crate) fn try_playback_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
    public_id_codec: &synctv_core::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
    validate_playback_result_shape(result)?;

    let playback_infos = result
        .playback_infos
        .iter()
        .map(|(mode, info)| {
            Ok((
                mode.clone(),
                playback_info_to_proto(info, public_id_codec, signing)?,
            ))
        })
        .collect::<Result<_, crate::impls::ApiError>>()?;

    let metadata = playback_metadata_to_proto(result, signing)?;

    Ok(synctv_proto::client::Playback {
        media_id: result
            .id
            .map(|id| encode_media_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        playlist_id: result
            .playlist_id
            .map(|id| encode_playlist_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        room_id: encode_room_id_for_proto(result.room_id, public_id_codec)?,
        name: result.name.clone(),
        playlist_position: result.position,
        provider: result
            .provider
            .parse::<synctv_core::models::SourceProvider>()
            .map(core_source_provider_to_proto)
            .map_err(|error| {
                crate::impls::ApiError::Internal(format!(
                    "Playback result contains invalid provider: {error}"
                ))
            })?,
        provider_instance_name: result.provider_instance_name.clone().unwrap_or_default(),
        playback_infos,
        default_mode: result.default_mode.clone(),
        metadata,
        expires_at: None,
        duration_seconds: result.duration_seconds,
        is_live: result.is_live,
        target: optional_provider_target_to_proto(result.target.as_ref()),
    })
}

fn validate_playback_result_shape(
    result: &synctv_core::models::media::PlaybackResult,
) -> Result<(), crate::impls::ApiError> {
    if result.playback_infos.is_empty() {
        return Err(crate::impls::ApiError::Internal(
            "playback has no playback modes".to_string(),
        ));
    }

    if result.default_mode.trim().is_empty() {
        return Err(crate::impls::ApiError::Internal(
            "playback default mode is empty".to_string(),
        ));
    }

    if !result.playback_infos.contains_key(&result.default_mode) {
        return Err(crate::impls::ApiError::Internal(format!(
            "playback default mode '{}' is missing",
            result.default_mode
        )));
    }

    for mode in result.playback_infos.keys() {
        if mode.trim().is_empty() {
            return Err(crate::impls::ApiError::Internal(
                "playback contains an empty mode name".to_string(),
            ));
        }
    }

    Ok(())
}

fn playback_metadata_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<Option<synctv_proto::client::PlaybackMetadata>, crate::impls::ApiError> {
    use synctv_core::models::media::PlaybackMetadata;

    let PlaybackMetadata {
        name,
        size,
        provider,
        source_host,
        thumbnail,
        proxy_thumbnail,
        external_subtitle_count,
        video_preview_error,
        transcoding_tasks,
        video_preview,
        duration,
        width,
        height,
        format,
        filename,
        is_live,
        media_id,
        room_id,
        content_type,
        bilibili,
        emby,
    } = &result.metadata;

    let thumbnail_url = proxy_thumbnail
        .as_ref()
        .map(|resource| signed_alist_thumbnail_url(resource, signing))
        .transpose()?;

    let metadata = synctv_proto::client::PlaybackMetadata {
        name: name.clone(),
        size: *size,
        provider: provider.clone(),
        source_host: source_host.clone(),
        thumbnail: thumbnail_url.or_else(|| thumbnail.clone()),
        external_subtitle_count: external_subtitle_count
            .map(|count| usize_to_i32(count, "external subtitle count"))
            .transpose()?,
        video_preview_error: video_preview_error.clone(),
        transcoding_tasks: transcoding_tasks
            .iter()
            .map(|task| {
                Ok(synctv_proto::client::AlistTranscodingTaskMetadata {
                    mode_name: task.mode_name.clone(),
                    template_id: task.template_id.clone(),
                    template_name: task.template_name.clone(),
                    template_width: task.template_width,
                    template_height: task.template_height,
                    stage: task.stage.clone(),
                    status: task.status.clone(),
                })
            })
            .collect::<Result<_, crate::impls::ApiError>>()?,
        video_preview: video_preview
            .as_ref()
            .map(|preview| {
                Ok::<_, crate::impls::ApiError>(synctv_proto::client::AlistVideoPreviewMetadata {
                    drive_id: preview.drive_id.clone(),
                    file_id: preview.file_id.clone(),
                    provider: preview.provider.clone(),
                    category: preview.category.clone(),
                    transcoding_count: u64::try_from(preview.transcoding_count).map_err(|_| {
                        crate::impls::ApiError::Internal(
                            "video preview transcoding count exceeds u64::MAX".to_string(),
                        )
                    })?,
                    subtitle_count: u64::try_from(preview.subtitle_count).map_err(|_| {
                        crate::impls::ApiError::Internal(
                            "video preview subtitle count exceeds u64::MAX".to_string(),
                        )
                    })?,
                })
            })
            .transpose()?,
        duration: *duration,
        width: *width,
        height: *height,
        format: format.clone(),
        filename: filename.clone(),
        is_live: *is_live,
        media_id: media_id.map(|id| id.to_string()),
        room_id: room_id.map(|id| id.to_string()),
        content_type: content_type.clone(),
        bilibili: bilibili.as_ref().map(|metadata| {
            synctv_proto::client::BilibiliPlaybackMetadata {
                bvid: metadata.bvid.clone(),
                aid: metadata.aid,
                epid: metadata.epid,
                cid: metadata.cid,
                min_buffer_time: metadata.min_buffer_time,
                fallback_format: metadata.fallback_format.clone(),
                quality: metadata.quality,
                room_id: metadata.room_id,
            }
        }),
        emby: emby
            .as_ref()
            .map(|metadata| synctv_proto::client::EmbyPlaybackMetadata {
                item_type: metadata.item_type.clone(),
                series_name: metadata.series_name.clone(),
                season_name: metadata.season_name.clone(),
                play_session_id: metadata.play_session_id.clone(),
            }),
    };

    let has_metadata = metadata.name.is_some()
        || metadata.size.is_some()
        || metadata.provider.is_some()
        || metadata.source_host.is_some()
        || metadata.thumbnail.is_some()
        || metadata.external_subtitle_count.is_some()
        || metadata.video_preview_error.is_some()
        || !metadata.transcoding_tasks.is_empty()
        || metadata.video_preview.is_some()
        || metadata.duration.is_some()
        || metadata.width.is_some()
        || metadata.height.is_some()
        || metadata.format.is_some()
        || metadata.filename.is_some()
        || metadata.is_live.is_some()
        || metadata.media_id.is_some()
        || metadata.room_id.is_some()
        || metadata.content_type.is_some()
        || metadata.bilibili.is_some()
        || metadata.emby.is_some();
    Ok(has_metadata.then_some(metadata))
}

fn signed_alist_thumbnail_url(
    resource: &synctv_core::models::media::PlaybackProxyResourceMetadata,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    let version = resource.version.trim();
    let resource_name = resource.resource.as_str();

    let signing = require_provider_signing(signing, "Alist thumbnail URL")?;
    let query = signed_provider_query(
        "alist",
        version,
        resource.expires_at,
        resource_name.to_string(),
        signing,
    );
    let version = path_segment_encode(version);
    Ok(format!(
        "/api/playback-providers/alist/{version}/thumbnail?{query}"
    ))
}

/// Convert models `PlaybackInfo` to proto `PlaybackInfo`
fn playback_info_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
    public_id_codec: &synctv_core::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::PlaybackInfo, crate::impls::ApiError> {
    if info.medias.is_empty() {
        return Err(crate::impls::ApiError::Internal(
            "playback mode has no media resources".to_string(),
        ));
    }
    let default_media_index = info
        .default_media_index
        .map(|index| checked_index_i32(index, info.medias.len(), "default playback media index"))
        .transpose()?;
    Ok(synctv_proto::client::PlaybackInfo {
        medias: info
            .medias
            .iter()
            .map(|media| playback_media_to_proto(media, signing))
            .collect::<Result<_, _>>()?,
        default_media_index,
        subtitles: info
            .subtitles
            .iter()
            .map(|subtitle| subtitle_to_proto(subtitle, signing))
            .collect::<Result<_, _>>()?,
        default_subtitle_index: info
            .default_subtitle_index
            .map(|index| checked_index_i32(index, info.subtitles.len(), "default subtitle index"))
            .transpose()?,
        danmakus: info
            .danmakus
            .iter()
            .map(|danmaku| danmaku_to_proto(danmaku, public_id_codec, signing))
            .collect::<Result<_, _>>()?,
        default_danmaku_index: info
            .default_danmaku_index
            .map(|index| checked_index_i32(index, info.danmakus.len(), "default danmaku index"))
            .transpose()?,
    })
}

/// Convert models `PlaybackMedia` to proto `PlaybackMedia`.
fn playback_media_to_proto(
    media: &synctv_core::models::media::PlaybackMedia,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::PlaybackMedia, crate::impls::ApiError> {
    let url_value = playback_media_url(media, signing)?;
    Ok(synctv_proto::client::PlaybackMedia {
        name: media.name.clone(),
        url: require_non_empty_url(&url_value, "playback")?,
        headers: playback_media_headers_for_proto(media),
        format: media.format.clone(),
        expire_at: media.expire_at.map(|dt| dt.timestamp()),
        metadata: media
            .metadata
            .as_ref()
            .map(playback_media_metadata_to_proto)
            .transpose()?,
    })
}

fn playback_media_headers_for_proto(
    media: &synctv_core::models::media::PlaybackMedia,
) -> std::collections::HashMap<String, String> {
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackBilibiliMedia, PlaybackDirectUrlMedia, PlaybackEmbyMedia,
        PlaybackExternalMedia, PlaybackMediaProvider,
    };

    match &media.provider {
        PlaybackMediaProvider::External(PlaybackExternalMedia { headers, .. })
        | PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Bilibili(
            PlaybackBilibiliMedia::Direct { headers, .. }
            | PlaybackBilibiliMedia::DirectDashManifest { headers, .. },
        )
        | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { headers, .. }) => headers.clone(),
        _ => std::collections::HashMap::new(),
    }
}

/// Convert models `PlaybackMediaMetadata` to proto `PlaybackMediaMetadata`
fn playback_media_metadata_to_proto(
    metadata: &synctv_core::models::media::PlaybackMediaMetadata,
) -> Result<synctv_proto::client::PlaybackMediaMetadata, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaybackMediaMetadata {
        resolution: metadata.resolution.clone(),
        bitrate: metadata.bitrate,
        codec: metadata.codec.clone(),
        fps: metadata.fps,
    })
}

fn subtitle_to_proto(
    subtitle: &synctv_core::models::media::PlaybackSubtitle,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::PlaybackSubtitle, crate::impls::ApiError> {
    let url_value = playback_subtitle_url(subtitle, signing)?;
    Ok(synctv_proto::client::PlaybackSubtitle {
        name: subtitle.name.clone(),
        language: subtitle.language.clone(),
        url: require_non_empty_url(&url_value, "subtitle")?,
        headers: client_visible_headers(&url_value, &subtitle.upstream_headers()),
        format: subtitle.format.clone(),
    })
}

fn danmaku_to_proto(
    danmaku: &synctv_core::models::media::PlaybackDanmaku,
    public_id_codec: &synctv_core::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::PlaybackDanmaku, crate::impls::ApiError> {
    let url_value = playback_danmaku_url(danmaku, public_id_codec, signing)?;
    Ok(synctv_proto::client::PlaybackDanmaku {
        name: danmaku.name.clone(),
        url: require_non_empty_url(&url_value, "danmaku")?,
        format: danmaku.format.clone(),
        headers: client_visible_headers(&url_value, &danmaku.upstream_headers()),
    })
}

fn require_provider_signing<'a>(
    signing: Option<&'a PlaybackHttpSigningContext<'_>>,
    context: &'static str,
) -> Result<&'a PlaybackHttpSigningContext<'a>, crate::impls::ApiError> {
    signing.ok_or_else(|| {
        crate::impls::ApiError::Internal(format!(
            "{context} requires playback provider signing context"
        ))
    })
}

fn signed_provider_query(
    provider: &str,
    version: &str,
    expires_at: i64,
    resource: String,
    signing: &PlaybackHttpSigningContext<'_>,
) -> String {
    signing
        .signing_key
        .build_signed_query(&synctv_core::proxy_signature::ProxyUrlClaims {
            provider: provider.to_string(),
            version: version.to_string(),
            resource,
            room_id: signing.room_id.to_string(),
            user_id: signing.user_id.to_string(),
            expires_at,
            target_url: None,
        })
}

fn path_segment_encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn playback_media_url(
    media: &synctv_core::models::media::PlaybackMedia,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackBilibiliMedia, PlaybackDirectUrlMedia, PlaybackEmbyMedia,
        PlaybackExternalMedia, PlaybackLiveProxyMedia, PlaybackMediaProvider, PlaybackRtmpMedia,
    };

    let direct = |media: &PlaybackExternalMedia| Ok(media.url.clone());
    let (provider, version, expires_at, path, resource) = match &media.provider {
        PlaybackMediaProvider::External(media) => return direct(media),
        PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { url, .. })
        | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { url, .. })
        | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { url, .. })
        | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { url, .. }) => {
            return Ok(url.clone());
        }
        PlaybackMediaProvider::Alist(PlaybackAlistMedia::ProxyFile {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "alist",
            version,
            *expires_at,
            "files",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Alist(PlaybackAlistMedia::ProxyTranscodedHlsManifest {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "alist",
            version,
            *expires_at,
            "transcoded-hls-manifests",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
            version,
            expires_at,
            mode_name,
            ..
        }) => dash_manifest_resource(version, *expires_at, mode_name, "direct"),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyDashManifest {
            version,
            expires_at,
            mode_name,
        }) => dash_manifest_resource(version, *expires_at, mode_name, "proxy"),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyMediaStream {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "bilibili",
            version,
            *expires_at,
            "media-streams",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyHlsManifest {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "bilibili",
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::direct_url::DirectUrlProvider::NAME,
            version,
            *expires_at,
            "streams",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyHlsManifest {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::direct_url::DirectUrlProvider::NAME,
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::ProxyMediaStream {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "emby",
            version,
            *expires_at,
            "media-streams",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Emby(PlaybackEmbyMedia::ProxyHlsManifest {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            "emby",
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::FlvStream {
            version,
            expires_at,
            ..
        }) => (
            "rtmp",
            version.clone(),
            *expires_at,
            "flv-stream".to_string(),
            "flv-stream".to_string(),
        ),
        PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsPlaylist {
            version,
            expires_at,
            ..
        }) => (
            "rtmp",
            version.clone(),
            *expires_at,
            "hls-playlist".to_string(),
            "hls-playlist".to_string(),
        ),
        PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::FlvStream {
            version,
            expires_at,
            ..
        }) => (
            synctv_core::provider::live_proxy::LiveProxyProvider::NAME,
            version.clone(),
            *expires_at,
            "flv-stream".to_string(),
            "flv-stream".to_string(),
        ),
        PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsPlaylist {
            version,
            expires_at,
            ..
        }) => (
            synctv_core::provider::live_proxy::LiveProxyProvider::NAME,
            version.clone(),
            *expires_at,
            "hls-playlist".to_string(),
            "hls-playlist".to_string(),
        ),
    };
    let signing = require_provider_signing(signing, "playback provider URL")?;
    let encoded_version = path_segment_encode(&version);
    let query = signed_provider_query(provider, &version, expires_at, resource, signing);
    let route_provider = playback_provider_route_slug(provider);
    let separator = if path.contains('?') { '&' } else { '?' };
    Ok(format!(
        "/api/playback-providers/{route_provider}/{encoded_version}/{path}{separator}{query}"
    ))
}

fn playback_provider_route_slug(provider: &str) -> &str {
    match provider {
        synctv_core::provider::direct_url::DirectUrlProvider::NAME => "direct-url",
        synctv_core::provider::live_proxy::LiveProxyProvider::NAME => "live-proxy",
        _ => provider,
    }
}

fn versioned_indexed_resource(
    provider: &'static str,
    version: &str,
    expires_at: i64,
    resource_prefix: &'static str,
    mode_name: &str,
    url_index: usize,
) -> (&'static str, String, i64, String, String) {
    let mode = path_segment_encode(mode_name);
    (
        provider,
        version.to_string(),
        expires_at,
        format!("{resource_prefix}/{mode}/{url_index}"),
        format!("{resource_prefix}/{mode_name}/{url_index}"),
    )
}

fn dash_manifest_resource(
    version: &str,
    expires_at: i64,
    mode_name: &str,
    manifest_mode: &'static str,
) -> (&'static str, String, i64, String, String) {
    // Use unencoded mode_name in both path and signature resource for consistency
    // The path will be percent-encoded by the HTTP client/browser automatically
    (
        "bilibili",
        version.to_string(),
        expires_at,
        format!("dash-manifests/{mode_name}/{manifest_mode}"),
        format!("dash-manifests/{mode_name}/{manifest_mode}"),
    )
}

fn playback_subtitle_url(
    subtitle: &synctv_core::models::media::PlaybackSubtitle,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    use synctv_core::models::media::{
        PlaybackAlistSubtitle, PlaybackBilibiliSubtitle, PlaybackDirectUrlSubtitle,
        PlaybackEmbySubtitle, PlaybackSubtitleProvider,
    };
    let (provider, version, expires_at, mode_name, subtitle_index) = match &subtitle.provider {
        PlaybackSubtitleProvider::External(subtitle) => return Ok(subtitle.url.clone()),
        PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("alist", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("bilibili", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => (
            synctv_core::provider::direct_url::DirectUrlProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("emby", version, *expires_at, mode_name, *subtitle_index),
    };
    let signing = require_provider_signing(signing, "playback provider subtitle URL")?;
    let mode = path_segment_encode(mode_name);
    let resource = format!("subtitles/{mode_name}/{subtitle_index}");
    let query = signed_provider_query(provider, version, expires_at, resource, signing);
    let version = path_segment_encode(version);
    let route_provider = playback_provider_route_slug(provider);
    Ok(format!(
        "/api/playback-providers/{route_provider}/{version}/subtitles/{mode}/{subtitle_index}?{query}"
    ))
}

fn playback_danmaku_url(
    danmaku: &synctv_core::models::media::PlaybackDanmaku,
    public_id_codec: &synctv_core::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    use synctv_core::models::media::{PlaybackBilibiliDanmaku, PlaybackDanmakuProvider};
    match &danmaku.provider {
        PlaybackDanmakuProvider::External(danmaku) => Ok(danmaku.url.clone()),
        PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::File {
            version,
            expires_at,
            danmaku_index,
            ..
        }) => {
            let signing = require_provider_signing(signing, "playback provider danmaku URL")?;
            let resource_name = format!("danmaku-files/{danmaku_index}");
            let query =
                signed_provider_query("bilibili", version, *expires_at, resource_name, signing);
            let version = path_segment_encode(version);
            Ok(format!(
                "/api/playback-providers/bilibili/{version}/danmaku-files/{danmaku_index}?{query}"
            ))
        }
        PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { media_id, .. }) => {
            let media_id = encode_media_id_for_proto(*media_id, public_id_codec)?;
            Ok(format!(
                "/api/playback-providers/bilibili/live-danmaku/{media_id}"
            ))
        }
    }
}

fn client_visible_headers(
    url: &str,
    headers: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    if is_provider_proxy_url(url) {
        std::collections::HashMap::new()
    } else {
        headers.clone()
    }
}

fn is_provider_proxy_url(url: &str) -> bool {
    url.starts_with("/api/playback-providers/")
}

#[cfg(test)]
mod playback_conversion_tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackBilibiliDanmaku, PlaybackBilibiliMedia, PlaybackDanmaku,
        PlaybackDanmakuProvider, PlaybackDirectUrlMedia, PlaybackExternalSubtitle, PlaybackInfo,
        PlaybackLiveProxyMedia, PlaybackMedia, PlaybackMediaProvider, PlaybackResult,
        PlaybackSubtitle, PlaybackSubtitleProvider,
    };

    fn signing_key() -> synctv_core::proxy_signature::ProxySigningKey {
        synctv_core::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-jwt-secret-that-is-long-enough",
        )
        .expect("test signing key should derive")
    }

    fn signing_context(
        key: &synctv_core::proxy_signature::ProxySigningKey,
    ) -> PlaybackHttpSigningContext<'_> {
        PlaybackHttpSigningContext {
            signing_key: key,
            room_id: "room-1",
            user_id: "user-1",
        }
    }

    fn codec() -> synctv_core::PublicIdCodec {
        synctv_core::PublicIdCodec::plain()
    }

    fn playback_result(info: PlaybackInfo) -> PlaybackResult {
        let mut playback_infos = HashMap::new();
        playback_infos.insert("dash".to_string(), info);
        PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: synctv_core::models::RoomId::new(),
            name: "media".to_string(),
            provider: "bilibili".to_string(),
            provider_instance_name: None,
            position: 0.0,
            playback_infos,
            default_mode: "dash".to_string(),
            duration_seconds: None,
            is_live: false,
            target: None,
            metadata: synctv_core::models::PlaybackMetadata::default(),
        }
    }

    fn playback_result_with_mode(mode: &str, info: PlaybackInfo) -> PlaybackResult {
        let mut playback_infos = HashMap::new();
        playback_infos.insert(mode.to_string(), info);
        PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: synctv_core::models::RoomId::new(),
            name: "media".to_string(),
            provider: "direct_url".to_string(),
            provider_instance_name: None,
            position: 0.0,
            playback_infos,
            default_mode: mode.to_string(),
            duration_seconds: None,
            is_live: false,
            target: None,
            metadata: synctv_core::models::PlaybackMetadata::default(),
        }
    }

    fn signed_query(url: &str) -> &str {
        url.split_once('?')
            .map(|(_, query)| query)
            .expect("signed provider URL should include query")
    }

    #[test]
    fn direct_dash_manifest_preserves_bilibili_headers_for_clients() {
        let key = signing_key();
        let signing = signing_context(&key);
        let mut headers = HashMap::new();
        headers.insert(
            "Referer".to_string(),
            "https://www.bilibili.com".to_string(),
        );
        headers.insert("User-Agent".to_string(), "SyncTV".to_string());
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "DASH".to_string(),
                format: "mpd".to_string(),
                expire_at: Utc::now().checked_add_signed(chrono::Duration::minutes(30)),
                metadata: None,
                provider: PlaybackMediaProvider::Bilibili(
                    PlaybackBilibiliMedia::DirectDashManifest {
                        version: "v1".to_string(),
                        expires_at: Utc::now().timestamp() + 1800,
                        mode_name: "dash".to_string(),
                        headers: headers.clone(),
                    },
                ),
            })
            .build();

        let proto = try_playback_to_proto(&playback_result(info), &codec(), Some(&signing))
            .expect("playback should convert");
        let media = &proto.playback_infos["dash"].medias[0];
        assert!(
            media.url.starts_with(
                "/api/playback-providers/bilibili/v1/dash-manifests/dash/direct?"
            ),
            "unexpected direct DASH URL: {}",
            media.url
        );
        assert_eq!(media.headers, headers);
    }

    #[test]
    fn live_danmaku_provider_converts_to_live_endpoint() {
        let room_id = synctv_core::models::RoomId::new();
        let media_id = synctv_core::models::MediaId::new();
        let codec = codec();
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "Live HLS".to_string(),
                format: "hls".to_string(),
                expire_at: None,
                metadata: None,
                provider: PlaybackMediaProvider::External(
                    synctv_core::models::media::PlaybackExternalMedia {
                        url: "https://example.com/live.m3u8".to_string(),
                        headers: HashMap::new(),
                    },
                ),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Bilibili Live Danmaku".to_string(),
                format: Some("synctv-bilibili-live".to_string()),
                provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                    room_id,
                    media_id,
                }),
            })
            .default_danmaku_index(0)
            .build();

        let proto = try_playback_to_proto(&playback_result(info), &codec, None)
            .expect("playback should convert");
        let danmaku = &proto.playback_infos["dash"].danmakus[0];
        let public_media_id = codec
            .encode_media_id(media_id)
            .expect("media id should encode");
        assert_eq!(
            danmaku.url,
            format!("/api/playback-providers/bilibili/live-danmaku/{public_media_id}")
        );
        assert!(danmaku.headers.is_empty());
    }

    #[test]
    fn provider_playback_info_to_model_preserves_default_indices() {
        let provider_info = synctv_core::provider::traits::PlaybackInfo {
            medias: vec![
                PlaybackMedia::simple(
                    "primary".to_string(),
                    "https://example.com/1.mp4".to_string(),
                ),
                PlaybackMedia::simple(
                    "selected".to_string(),
                    "https://example.com/2.mp4".to_string(),
                ),
            ],
            default_media_index: Some(1),
            subtitles: vec![
                PlaybackSubtitle {
                    name: "English".to_string(),
                    language: "en".to_string(),
                    format: "vtt".to_string(),
                    provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
                        url: "https://example.com/en.vtt".to_string(),
                        headers: HashMap::new(),
                    }),
                },
                PlaybackSubtitle {
                    name: "Japanese".to_string(),
                    language: "ja".to_string(),
                    format: "vtt".to_string(),
                    provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
                        url: "https://example.com/ja.vtt".to_string(),
                        headers: HashMap::new(),
                    }),
                },
            ],
            default_subtitle_index: Some(1),
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };

        let model = provider_playback_info_to_model(&provider_info);

        assert_eq!(model.default_media_index, Some(1));
        assert_eq!(model.default_subtitle_index, Some(1));
    }

    #[test]
    fn playback_to_proto_serializes_provider_selected_default_indices() {
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia::simple(
                "first".to_string(),
                "https://example.com/1.mp4".to_string(),
            ))
            .add_media(PlaybackMedia::simple(
                "second".to_string(),
                "https://example.com/2.mp4".to_string(),
            ))
            .default_media_index(1)
            .add_subtitle(PlaybackSubtitle {
                name: "English".to_string(),
                language: "en".to_string(),
                format: "vtt".to_string(),
                provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
                    url: "https://example.com/en.vtt".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .add_subtitle(PlaybackSubtitle {
                name: "Japanese".to_string(),
                language: "ja".to_string(),
                format: "vtt".to_string(),
                provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
                    url: "https://example.com/ja.vtt".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .default_subtitle_index(1)
            .build();

        let proto = try_playback_to_proto(&playback_result(info), &codec(), None)
            .expect("playback should convert");
        let info = &proto.playback_infos["dash"];

        assert_eq!(info.default_media_index, Some(1));
        assert_eq!(info.default_subtitle_index, Some(1));
    }

    #[test]
    fn provider_proxy_url_uses_path_segment_encoding_for_mode_names() {
        let key = signing_key();
        let signing = signing_context(&key);
        let mode_name = "My Source+Main";
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "proxied".to_string(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
                    version: "v 1".to_string(),
                    expires_at: Utc::now().timestamp() + 1800,
                    mode_name: mode_name.to_string(),
                    url_index: 0,
                    url: "https://example.com/video.mp4".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode(mode_name, info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let media = &proto.playback_infos[mode_name].medias[0];

        assert!(
            media.url.starts_with(
                "/api/playback-providers/direct-url/v%201/streams/My%20Source%2BMain/0?"
            ),
            "unexpected proxy URL: {}",
            media.url
        );
        let claims = key
            .parse_and_verify_query(
                signed_query(&media.url),
                synctv_core::provider::direct_url::DirectUrlProvider::NAME,
                "v 1",
                "streams/My Source+Main/0",
            )
            .expect("signature should bind decoded resource");
        assert_eq!(claims.resource, "streams/My Source+Main/0");
    }

    #[test]
    fn live_proxy_url_uses_route_slug_and_internal_signature_provider() {
        let key = signing_key();
        let signing = signing_context(&key);
        let room_id = synctv_core::models::RoomId::new();
        let media_id = synctv_core::models::MediaId::new();
        let expires_at = Utc::now().timestamp() + 1800;
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "live".to_string(),
                format: "m3u8".to_string(),
                expire_at: None,
                metadata: None,
                provider: PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsPlaylist {
                    version: "live v1".to_string(),
                    expires_at,
                    room_id,
                    media_id,
                }),
            })
            .build();

        let proto = try_playback_to_proto(&playback_result(info), &codec(), Some(&signing))
            .expect("playback should convert");
        let media = &proto.playback_infos["dash"].medias[0];

        assert!(
            media
                .url
                .starts_with("/api/playback-providers/live-proxy/live%20v1/hls-playlist?"),
            "unexpected live-proxy URL: {}",
            media.url
        );
        let claims = key
            .parse_and_verify_query(
                signed_query(&media.url),
                synctv_core::provider::live_proxy::LiveProxyProvider::NAME,
                "live v1",
                "hls-playlist",
            )
            .expect("signature should use internal provider name");
        assert_eq!(
            claims.provider,
            synctv_core::provider::live_proxy::LiveProxyProvider::NAME
        );
    }

    #[test]
    fn alist_thumbnail_metadata_exposes_signed_proxy_url() {
        let key = signing_key();
        let signing = signing_context(&key);
        let expires_at = Utc::now().timestamp() + 1800;
        let mut result = playback_result(
            PlaybackInfo::builder()
                .add_media(PlaybackMedia {
                    name: "proxied".to_string(),
                    format: "mp4".to_string(),
                    expire_at: None,
                    metadata: None,
                    provider: PlaybackMediaProvider::Alist(PlaybackAlistMedia::ProxyFile {
                        version: "v 1".to_string(),
                        expires_at,
                        mode_name: "default".to_string(),
                        url_index: 0,
                        url: "https://example.com/video.mp4".to_string(),
                        headers: HashMap::new(),
                    }),
                })
                .build(),
        );
        result.provider = "alist".to_string();
        result.metadata.thumbnail = Some("https://alist.example.com/thumb.jpg".to_string());
        result.metadata.proxy_thumbnail =
            Some(synctv_core::models::media::PlaybackProxyResourceMetadata {
                version: "v 1".to_string(),
                expires_at,
                resource: synctv_core::models::media::PlaybackProxyResource::Thumbnail,
            });

        let proto = try_playback_to_proto(&result, &codec(), Some(&signing))
            .expect("playback should convert");
        let thumbnail = proto
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.thumbnail.as_deref())
            .expect("thumbnail metadata should exist");

        assert!(
            thumbnail.starts_with("/api/playback-providers/alist/v%201/thumbnail?"),
            "unexpected thumbnail URL: {thumbnail}"
        );
        let claims = key
            .parse_and_verify_query(signed_query(thumbnail), "alist", "v 1", "thumbnail")
            .expect("thumbnail signature should verify");
        assert_eq!(claims.resource, "thumbnail");
    }
}
