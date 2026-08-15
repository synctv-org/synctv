use synctv_core::service::ClientResourceAvailability;
use synctv_proto::client as client_proto;
use synctv_proto::source_config as source_config_proto;

use crate::proxy_signature::ProxySigningKeyQueryExt;

pub use crate::impls::source_provider::{
    core_source_provider_to_proto,
    proto_source_provider_filter as optional_proto_source_provider_to_core,
};

fn playback_kind_to_proto(kind: synctv_core::models::PlaybackKind) -> i32 {
    match kind {
        synctv_core::models::PlaybackKind::Regular => {
            source_config_proto::PlaybackKind::Regular as i32
        }
        synctv_core::models::PlaybackKind::Live => source_config_proto::PlaybackKind::Live as i32,
    }
}

fn bilibili_playback_kind_to_proto(kind: synctv_core::models::BilibiliPlaybackKind) -> i32 {
    match kind {
        synctv_core::models::BilibiliPlaybackKind::Video => {
            client_proto::BilibiliPlaybackKind::Video as i32
        }
        synctv_core::models::BilibiliPlaybackKind::Pgc => {
            client_proto::BilibiliPlaybackKind::Pgc as i32
        }
        synctv_core::models::BilibiliPlaybackKind::Live => {
            client_proto::BilibiliPlaybackKind::Live as i32
        }
    }
}

fn douyin_playback_kind_to_proto(kind: synctv_core::models::DouyinPlaybackKind) -> i32 {
    match kind {
        synctv_core::models::DouyinPlaybackKind::Video => {
            client_proto::DouyinPlaybackKind::Video as i32
        }
        synctv_core::models::DouyinPlaybackKind::Live => {
            client_proto::DouyinPlaybackKind::Live as i32
        }
    }
}

fn emby_playback_kind_to_proto(kind: synctv_core::models::EmbyPlaybackKind) -> i32 {
    match kind {
        synctv_core::models::EmbyPlaybackKind::Movie => {
            client_proto::EmbyPlaybackKind::Movie as i32
        }
        synctv_core::models::EmbyPlaybackKind::Episode => {
            client_proto::EmbyPlaybackKind::Episode as i32
        }
        synctv_core::models::EmbyPlaybackKind::Video => {
            client_proto::EmbyPlaybackKind::Video as i32
        }
        synctv_core::models::EmbyPlaybackKind::Audio => {
            client_proto::EmbyPlaybackKind::Audio as i32
        }
        synctv_core::models::EmbyPlaybackKind::MusicAlbum => {
            client_proto::EmbyPlaybackKind::MusicAlbum as i32
        }
    }
}

fn tiktok_playback_kind_to_proto(kind: synctv_core::models::TikTokPlaybackKind) -> i32 {
    match kind {
        synctv_core::models::TikTokPlaybackKind::Video => {
            client_proto::TikTokPlaybackKind::Video as i32
        }
        synctv_core::models::TikTokPlaybackKind::Live => {
            client_proto::TikTokPlaybackKind::Live as i32
        }
    }
}

pub struct PlaybackHttpSigningContext<'a> {
    pub signing_key: &'a crate::proxy_signature::ProxySigningKey,
    pub media_swarm_signing_key: &'a crate::proxy_signature::MediaSwarmSigningKey,
    pub room_id: &'a str,
    pub proxy_authorizer_id: &'a str,
    pub actor_id: &'a str,
}

fn proto_encode_error(kind: &str, error: &str) -> crate::impls::ApiError {
    crate::impls::ApiError::Internal(format!("Failed to encode {kind} public id: {error}"))
}

fn encode_room_id_for_proto(
    id: synctv_core::models::RoomId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_room_id(id)
        .map_err(|error| proto_encode_error("room", &error))
}

pub fn room_category_to_proto(
    category: &synctv_core::models::RoomCategory,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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

pub fn room_label_to_proto(
    label: &synctv_core::models::RoomLabel,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| proto_encode_error("media", &error))
}

fn encode_playlist_id_for_proto(
    id: synctv_core::models::PlaylistId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| proto_encode_error("playlist", &error))
}

fn encode_user_id_for_proto(
    id: synctv_core::models::UserId,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_user_id(id)
        .map_err(|error| proto_encode_error("user", &error))
}

pub fn room_settings_to_proto(
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
        voice_chat_enabled: settings.voice_chat_enabled.0,
        p2p_media_enabled: settings.p2p_media_enabled.0,
    }
}

pub fn room_settings_from_proto(
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
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin::new(
            settings.allow_guest_join,
        ),
        max_members: synctv_core::models::room_settings::MaxMembers::new(settings.max_members),
        require_approval: synctv_core::models::room_settings::RequireApproval::new(
            settings.require_approval,
        ),
        allow_auto_join: synctv_core::models::room_settings::AllowAutoJoin::new(
            settings.allow_auto_join,
        ),
        chat_enabled: synctv_core::models::room_settings::ChatEnabled::new(settings.chat_enabled),
        voice_chat_enabled: synctv_core::models::room_settings::VoiceChatEnabled::new(
            settings.voice_chat_enabled,
        ),
        p2p_media_enabled: synctv_core::models::room_settings::P2pMediaEnabled::new(
            settings.p2p_media_enabled,
        ),
        auto_play: synctv_core::models::room_settings::AutoPlay::new(
            synctv_core::models::AutoPlaySettings {
                enabled: auto_play.enabled,
                mode: play_mode,
                delay: auto_play.delay,
            },
        ),
        admin_added_permissions: synctv_core::models::room_settings::AdminAddedPermissions::new(
            settings.admin_added_permissions,
        ),
        admin_removed_permissions: synctv_core::models::room_settings::AdminRemovedPermissions::new(
            settings.admin_removed_permissions,
        ),
        member_added_permissions: synctv_core::models::room_settings::MemberAddedPermissions::new(
            settings.member_added_permissions,
        ),
        member_removed_permissions:
            synctv_core::models::room_settings::MemberRemovedPermissions::new(
                settings.member_removed_permissions,
            ),
        guest_added_permissions: synctv_core::models::room_settings::GuestAddedPermissions::new(
            settings.guest_added_permissions,
        ),
        guest_removed_permissions: synctv_core::models::room_settings::GuestRemovedPermissions::new(
            settings.guest_removed_permissions,
        ),
    };
    Ok(settings)
}

pub fn apply_room_settings_patch_from_proto(
    mut settings: synctv_core::models::RoomSettings,
    request: client_proto::UpdateRoomSettingsRequest,
) -> Result<synctv_core::models::RoomSettings, crate::impls::ApiError> {
    use synctv_core::models::room_settings::{
        AdminAddedPermissions, AdminRemovedPermissions, AllowAutoJoin, AllowGuestJoin, AutoPlay,
        ChatEnabled, GuestAddedPermissions, GuestRemovedPermissions, MaxMembers,
        MemberAddedPermissions, MemberRemovedPermissions, P2pMediaEnabled, RequireApproval,
        RoomSettingsPatch, VoiceChatEnabled,
    };

    let patch = request
        .settings
        .ok_or_else(|| crate::impls::ApiError::InvalidInput("settings is required".to_string()))?;
    let paths = request
        .update_mask
        .ok_or_else(|| crate::impls::ApiError::InvalidInput("update_mask is required".to_string()))?
        .paths;
    let patch = crate::room_settings_mapping::select_room_settings_patch(patch, &paths)?;
    let mut typed_patch = RoomSettingsPatch::default();
    if let Some(value) = patch.allow_guest_join {
        typed_patch.allow_guest_join = Some(AllowGuestJoin::new(value));
    }
    if let Some(value) = patch.max_members {
        typed_patch.max_members = Some(MaxMembers::new(value));
    }
    if let Some(value) = patch.require_approval {
        typed_patch.require_approval = Some(RequireApproval::new(value));
    }
    if let Some(value) = patch.allow_auto_join {
        typed_patch.allow_auto_join = Some(AllowAutoJoin::new(value));
    }
    if let Some(value) = patch.chat_enabled {
        typed_patch.chat_enabled = Some(ChatEnabled::new(value));
    }
    if let Some(value) = patch.voice_chat_enabled {
        typed_patch.voice_chat_enabled = Some(VoiceChatEnabled::new(value));
    }
    if let Some(value) = patch.p2p_media_enabled {
        typed_patch.p2p_media_enabled = Some(P2pMediaEnabled::new(value));
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
        typed_patch.auto_play = Some(AutoPlay::new(value));
    }
    if let Some(value) = patch.admin_added_permissions {
        typed_patch.admin_added_permissions = Some(AdminAddedPermissions::new(value));
    }
    if let Some(value) = patch.admin_removed_permissions {
        typed_patch.admin_removed_permissions = Some(AdminRemovedPermissions::new(value));
    }
    if let Some(value) = patch.member_added_permissions {
        typed_patch.member_added_permissions = Some(MemberAddedPermissions::new(value));
    }
    if let Some(value) = patch.member_removed_permissions {
        typed_patch.member_removed_permissions = Some(MemberRemovedPermissions::new(value));
    }
    if let Some(value) = patch.guest_added_permissions {
        typed_patch.guest_added_permissions = Some(GuestAddedPermissions::new(value));
    }
    if let Some(value) = patch.guest_removed_permissions {
        typed_patch.guest_removed_permissions = Some(GuestRemovedPermissions::new(value));
    }
    settings.merge_patch(typed_patch);
    Ok(settings)
}

pub fn file_metadata_to_proto(
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

pub fn file_metadata_from_proto(
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

pub fn chat_message_selection_from_proto_values(
    include_message_types: &[i32],
) -> Result<synctv_core::models::ChatMessageSelection, String> {
    if include_message_types.is_empty() {
        return Ok(synctv_core::models::ChatMessageSelection::user_default());
    }

    let include_message_types = include_message_types
        .iter()
        .map(|value| {
            let message_type = client_proto::ChatMessageType::try_from(*value)
                .map_err(|_| format!("Invalid chat message type: {value}"))?;
            match message_type {
                client_proto::ChatMessageType::Unspecified => {
                    Err("Chat message type must be specified".to_string())
                }
                client_proto::ChatMessageType::User => {
                    Ok(synctv_core::models::ChatMessageType::User)
                }
                client_proto::ChatMessageType::SystemMemberJoined => {
                    Ok(synctv_core::models::ChatMessageType::SystemMemberJoined)
                }
                client_proto::ChatMessageType::SystemPlaybackChanged => {
                    Ok(synctv_core::models::ChatMessageType::SystemPlaybackChanged)
                }
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(synctv_core::models::ChatMessageSelection {
        include_message_types,
    })
}

pub fn chat_metadata_from_proto(
    metadata: Option<&client_proto::ChatMetadata>,
) -> Result<Option<synctv_core::models::ChatMetadata>, crate::impls::ApiError> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let Some(client_proto::chat_metadata::Metadata::User(user)) = metadata.metadata.as_ref() else {
        return Err(crate::impls::ApiError::InvalidInput(
            "Client chat metadata must use user metadata".to_string(),
        ));
    };
    let metadata = synctv_core::models::ChatMetadata::User(synctv_core::models::ChatUserMetadata {
        presentation: user.presentation.as_ref().map(|presentation| {
            synctv_core::models::ChatPresentationMetadata {
                display_position: presentation.display_position.clone(),
                display_color: presentation.display_color.clone(),
            }
        }),
        playback: None,
    });
    Ok((!metadata.is_empty()).then_some(metadata))
}

pub fn content_report_metadata_to_proto(
    metadata: Option<&synctv_core::models::ContentReportMetadata>,
) -> Result<Option<client_proto::ContentReportMetadata>, crate::impls::ApiError> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    if metadata.is_empty() {
        return Ok(None);
    }
    Ok(Some(client_proto::ContentReportMetadata {
        client_reason: metadata.client_reason.clone(),
    }))
}

pub fn content_report_metadata_from_proto(
    metadata: Option<&client_proto::ContentReportMetadata>,
) -> Result<Option<synctv_core::models::ContentReportMetadata>, crate::impls::ApiError> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let metadata = synctv_core::models::ContentReportMetadata {
        client_reason: metadata.client_reason.clone(),
    };
    Ok((!metadata.is_empty()).then_some(metadata))
}
pub fn provider_target_to_proto(
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
        synctv_core::models::ProviderTarget::Bilibili(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Bilibili(
                client_proto::BilibiliTarget {
                    target: Some(match target {
                        synctv_core::models::BilibiliTarget::Video { bvid, aid } => {
                            client_proto::bilibili_target::Target::Video(
                                client_proto::BilibiliVideoTarget {
                                    bvid: bvid.clone(),
                                    aid: *aid,
                                },
                            )
                        }
                        synctv_core::models::BilibiliTarget::VideoPart {
                            bvid,
                            aid,
                            cid,
                            page,
                        } => client_proto::bilibili_target::Target::VideoPart(
                            client_proto::BilibiliVideoPartTarget {
                                bvid: bvid.clone(),
                                aid: *aid,
                                cid: *cid,
                                page: *page,
                            },
                        ),
                        synctv_core::models::BilibiliTarget::PgcEpisode { epid, cid } => {
                            client_proto::bilibili_target::Target::PgcEpisode(
                                client_proto::BilibiliPgcEpisodeTarget {
                                    epid: *epid,
                                    cid: *cid,
                                },
                            )
                        }
                        synctv_core::models::BilibiliTarget::Live { room_id } => {
                            client_proto::bilibili_target::Target::Live(
                                client_proto::BilibiliLiveTarget { room_id: *room_id },
                            )
                        }
                    }),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Emby(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Emby(
                client_proto::EmbyTarget {
                    target: Some(match target {
                        synctv_core::models::EmbyTarget::Item { item_id } => {
                            client_proto::emby_target::Target::Item(client_proto::EmbyItemTarget {
                                item_id: item_id.clone(),
                            })
                        }
                        synctv_core::models::EmbyTarget::Person { person_id } => {
                            client_proto::emby_target::Target::Person(
                                client_proto::EmbyPersonTarget {
                                    person_id: person_id.clone(),
                                },
                            )
                        }
                        synctv_core::models::EmbyTarget::PersonItem { person_id, item_id } => {
                            client_proto::emby_target::Target::PersonItem(
                                client_proto::EmbyPersonItemTarget {
                                    person_id: person_id.clone(),
                                    item_id: item_id.clone(),
                                },
                            )
                        }
                    }),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Cloudreve(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Cloudreve(
                client_proto::CloudreveTarget {
                    relative_path: target.relative_path.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Twitch(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Twitch(
                client_proto::TwitchTarget {
                    kind: match target.kind {
                        synctv_core::models::TwitchTargetKind::Video => {
                            client_proto::TwitchTargetKind::Video as i32
                        }
                        synctv_core::models::TwitchTargetKind::Clip => {
                            client_proto::TwitchTargetKind::Clip as i32
                        }
                        synctv_core::models::TwitchTargetKind::Live => {
                            client_proto::TwitchTargetKind::Live as i32
                        }
                    },
                    id: target.id.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Youtube(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Youtube(
                client_proto::YoutubeTarget {
                    video_id: target.video_id.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Douyin(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Douyin(
                client_proto::DouyinTarget {
                    aweme_id: target.aweme_id.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::TikTok(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Tiktok(
                client_proto::TikTokTarget {
                    video_id: target.video_id.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Fnos(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Fnos(
                client_proto::FnosTarget {
                    target: Some(match &target.target {
                        synctv_core::models::FnosTargetKind::File { relative_path } => {
                            client_proto::fnos_target::Target::File(client_proto::FnosFileTarget {
                                relative_path: relative_path.clone(),
                            })
                        }
                        synctv_core::models::FnosTargetKind::MediaItem {
                            item_guid,
                            media_guid,
                            library_guid,
                        } => client_proto::fnos_target::Target::MediaItem(
                            client_proto::FnosMediaItemTarget {
                                item_guid: item_guid.clone(),
                                media_guid: media_guid.clone(),
                                library_guid: library_guid.clone(),
                            },
                        ),
                    }),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Qnap(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Qnap(
                client_proto::QnapTarget {
                    relative_path: target.relative_path.clone(),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Synology(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Synology(
                client_proto::SynologyTarget {
                    target: Some(match target {
                        synctv_core::models::SynologyTarget::File { relative_path } => {
                            client_proto::synology_target::Target::File(
                                client_proto::SynologyFileTarget {
                                    relative_path: relative_path.clone(),
                                },
                            )
                        }
                        synctv_core::models::SynologyTarget::LibraryItem {
                            kind,
                            item_id,
                            file_id,
                            parent_id,
                        } => client_proto::synology_target::Target::LibraryItem(
                            client_proto::SynologyLibraryItemTarget {
                                kind: synology_kind_to_proto(*kind),
                                item_id: *item_id,
                                file_id: *file_id,
                                parent_id: *parent_id,
                            },
                        ),
                        synctv_core::models::SynologyTarget::TvShow {
                            library_id,
                            tv_show_id,
                        } => client_proto::synology_target::Target::TvShow(
                            client_proto::SynologyTvShowTarget {
                                library_id: *library_id,
                                tv_show_id: *tv_show_id,
                            },
                        ),
                    }),
                },
            )),
        },
        synctv_core::models::ProviderTarget::Nextcloud(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Nextcloud(
                client_proto::NextcloudTarget {
                    path: target.path.clone(),
                    file_id: target.file_id,
                },
            )),
        },
        synctv_core::models::ProviderTarget::Seafile(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Seafile(
                client_proto::SeafileTarget {
                    repository_id: target.repository_id.clone(),
                    path: target.path.clone(),
                    object_id: target.object_id.clone(),
                    has_thumbnail: target.has_thumbnail,
                },
            )),
        },
        synctv_core::models::ProviderTarget::TrueNas(target) => client_proto::ProviderTarget {
            target: Some(client_proto::provider_target::Target::Truenas(
                client_proto::TrueNasTarget {
                    path: target.path.clone(),
                },
            )),
        },
    }
}

pub fn optional_provider_target_to_proto(
    target: Option<&synctv_core::models::ProviderTarget>,
) -> Option<client_proto::ProviderTarget> {
    target.map(provider_target_to_proto)
}

fn synology_kind_to_proto(kind: synctv_core::models::SynologyLibraryItemKind) -> i32 {
    match kind {
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
    }
}

fn synology_kind_from_proto(
    kind: i32,
) -> Result<synctv_core::models::SynologyLibraryItemKind, crate::impls::ApiError> {
    match source_config_proto::SynologyLibraryItemKind::try_from(kind) {
        Ok(source_config_proto::SynologyLibraryItemKind::Movie) => {
            Ok(synctv_core::models::SynologyLibraryItemKind::Movie)
        }
        Ok(source_config_proto::SynologyLibraryItemKind::Episode) => {
            Ok(synctv_core::models::SynologyLibraryItemKind::Episode)
        }
        Ok(source_config_proto::SynologyLibraryItemKind::HomeVideo) => {
            Ok(synctv_core::models::SynologyLibraryItemKind::HomeVideo)
        }
        Ok(source_config_proto::SynologyLibraryItemKind::TvRecording) => {
            Ok(synctv_core::models::SynologyLibraryItemKind::TvRecording)
        }
        Ok(source_config_proto::SynologyLibraryItemKind::Unspecified) | Err(_) => {
            Err(crate::impls::ApiError::InvalidInput(
                "Synology library item kind is required".to_string(),
            ))
        }
    }
}

pub fn provider_target_from_proto(
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
        client_proto::provider_target::Target::Bilibili(target) => {
            match target.target.ok_or_else(|| {
                crate::impls::ApiError::InvalidInput(
                    "Bilibili target oneof is required".to_string(),
                )
            })? {
                client_proto::bilibili_target::Target::Video(target) => {
                    synctv_core::models::ProviderTarget::bilibili_video(target.bvid, target.aid)
                }
                client_proto::bilibili_target::Target::VideoPart(target) => {
                    synctv_core::models::ProviderTarget::bilibili_video_part(
                        target.bvid,
                        target.aid,
                        target.cid,
                        target.page,
                    )
                }
                client_proto::bilibili_target::Target::PgcEpisode(target) => {
                    synctv_core::models::ProviderTarget::bilibili_pgc_episode(
                        target.epid,
                        target.cid,
                    )
                }
                client_proto::bilibili_target::Target::Live(target) => {
                    synctv_core::models::ProviderTarget::bilibili_live(target.room_id)
                }
            }
        }
        client_proto::provider_target::Target::Emby(target) => {
            match target.target.ok_or_else(|| {
                crate::impls::ApiError::InvalidInput("Emby target oneof is required".to_string())
            })? {
                client_proto::emby_target::Target::Item(target) => {
                    synctv_core::models::ProviderTarget::emby(target.item_id)
                }
                client_proto::emby_target::Target::Person(target) => {
                    synctv_core::models::ProviderTarget::emby_person(target.person_id)
                }
                client_proto::emby_target::Target::PersonItem(target) => {
                    synctv_core::models::ProviderTarget::emby_person_item(
                        target.person_id,
                        target.item_id,
                    )
                }
            }
        }
        client_proto::provider_target::Target::Cloudreve(target) => {
            synctv_core::models::ProviderTarget::cloudreve(target.relative_path)
        }
        client_proto::provider_target::Target::Twitch(target) => {
            let kind = match client_proto::TwitchTargetKind::try_from(target.kind) {
                Ok(client_proto::TwitchTargetKind::Video) => {
                    synctv_core::models::TwitchTargetKind::Video
                }
                Ok(client_proto::TwitchTargetKind::Clip) => {
                    synctv_core::models::TwitchTargetKind::Clip
                }
                Ok(client_proto::TwitchTargetKind::Live) => {
                    synctv_core::models::TwitchTargetKind::Live
                }
                Ok(client_proto::TwitchTargetKind::Unspecified) | Err(_) => {
                    return Err(crate::impls::ApiError::InvalidInput(
                        "Twitch target kind is required".to_string(),
                    ));
                }
            };
            synctv_core::models::ProviderTarget::twitch(kind, target.id)
        }
        client_proto::provider_target::Target::Youtube(target) => {
            synctv_core::models::ProviderTarget::youtube(target.video_id)
        }
        client_proto::provider_target::Target::Douyin(target) => {
            synctv_core::models::ProviderTarget::douyin(target.aweme_id)
        }
        client_proto::provider_target::Target::Tiktok(target) => {
            synctv_core::models::ProviderTarget::tiktok(target.video_id)
        }
        client_proto::provider_target::Target::Fnos(target) => {
            match target.target.ok_or_else(|| {
                crate::impls::ApiError::InvalidInput("FNOS target oneof is required".to_string())
            })? {
                client_proto::fnos_target::Target::File(file) => {
                    synctv_core::models::ProviderTarget::fnos(file.relative_path)
                }
                client_proto::fnos_target::Target::MediaItem(item) => {
                    synctv_core::models::ProviderTarget::fnos_media(
                        item.item_guid,
                        item.media_guid,
                        item.library_guid,
                    )
                }
            }
        }
        client_proto::provider_target::Target::Qnap(target) => {
            synctv_core::models::ProviderTarget::qnap(target.relative_path)
        }
        client_proto::provider_target::Target::Synology(target) => {
            match target.target.ok_or_else(|| {
                crate::impls::ApiError::InvalidInput(
                    "Synology target oneof is required".to_string(),
                )
            })? {
                client_proto::synology_target::Target::File(value) => {
                    synctv_core::models::ProviderTarget::synology_file(value.relative_path)
                }
                client_proto::synology_target::Target::LibraryItem(value) => {
                    synctv_core::models::ProviderTarget::synology_library_item(
                        synology_kind_from_proto(value.kind)?,
                        value.item_id,
                        value.file_id,
                        value.parent_id,
                    )
                }
                client_proto::synology_target::Target::TvShow(value) => {
                    synctv_core::models::ProviderTarget::synology_tv_show(
                        value.library_id,
                        value.tv_show_id,
                    )
                }
            }
        }
        client_proto::provider_target::Target::Nextcloud(target) => {
            synctv_core::models::ProviderTarget::nextcloud(target.path, target.file_id)
        }
        client_proto::provider_target::Target::Seafile(target) => {
            synctv_core::models::ProviderTarget::seafile(
                target.repository_id,
                target.path,
                target.object_id,
                target.has_thumbnail,
            )
        }
        client_proto::provider_target::Target::Truenas(target) => {
            synctv_core::models::ProviderTarget::truenas(target.path)
        }
    }))
}

pub fn file_object_variant_to_proto(
    variant: &synctv_core::models::FileObjectVariant,
) -> Result<synctv_proto::client::FileObjectVariant, crate::impls::ApiError> {
    let metadata = synctv_core::models::FileMetadata {
        width: variant.metadata.width,
        height: variant.metadata.height,
        blurhash: variant.metadata.blurhash.clone(),
        ..Default::default()
    };
    let object_access = variant
        .object_access
        .as_ref()
        .map(crate::impls::stored_files::file_object_access_to_proto);
    let url = variant
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            variant
                .object_access
                .as_ref()
                .and_then(crate::impls::stored_files::render_file_object_access_url)
        })
        .unwrap_or_default();
    Ok(synctv_proto::client::FileObjectVariant {
        key: variant.variant_key.clone(),
        label: variant.label.clone(),
        url,
        mime_type: variant.mime_type.clone(),
        size_bytes: variant.size_bytes,
        width: variant.width.unwrap_or_default(),
        height: variant.height.unwrap_or_default(),
        is_original: variant.is_original,
        lossy: variant.lossy,
        quality: variant.quality,
        metadata: file_metadata_to_proto(&metadata)?,
        object_access,
    })
}

pub fn file_object_variants_from_metadata(
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
            object_access: None,
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
            created_at: synctv_core::SystemClock.now(),
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

    #[test]
    fn generated_variant_metadata_preserves_object_access() {
        let variant = synctv_core::models::FileObjectVariant {
            storage_backend: "database".to_string(),
            object_key: "objects/file-small.jpg".to_string(),
            original_storage_backend: "database".to_string(),
            original_object_key: "objects/file.jpg".to_string(),
            group_id: "fg_test".to_string(),
            variant_key: "small".to_string(),
            label: "Small".to_string(),
            object_access: Some(synctv_core::models::FileObjectAccess {
                object_kind: synctv_core::models::FileObjectKind::MediaCover,
                encoded_object_key: "encoded-small".to_string(),
                read_token: "read-small".to_string(),
            }),
            url: None,
            mime_type: "image/jpeg".to_string(),
            size_bytes: 1024,
            width: Some(320),
            height: Some(180),
            is_original: false,
            lossy: true,
            quality: Some(78),
            sort_order: 20,
            metadata: synctv_core::models::FileVariantMetadata::default(),
            created_at: synctv_core::SystemClock.now(),
        };

        let proto = file_object_variant_to_proto(&variant).expect("variant should convert");
        let access = proto
            .object_access
            .expect("object access should be present");

        assert_eq!(
            proto.url,
            "/api/media/cover-objects/encoded-small?token=read-small"
        );
        assert_eq!(
            access.object_kind,
            synctv_proto::client::FileObjectAccessKind::MediaCover as i32
        );
        assert_eq!(access.encoded_object_key, "encoded-small");
        assert_eq!(access.read_token, "read-small");
    }
}

pub fn media_source_config_to_proto(
    config: &synctv_core::models::MediaSourceConfig,
) -> Result<source_config_proto::MediaSourceConfig, crate::impls::ApiError> {
    use source_config_proto::media_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::MediaSourceConfig::DirectUrl(config) => {
            Provider::DirectUrl(direct_url_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Bilibili(config) => {
            Provider::Bilibili(bilibili_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Alist(config) => {
            Provider::Alist(alist_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Emby(config) => {
            Provider::Emby(emby_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Rtmp(config) => {
            Provider::Rtmp(rtmp_media_source_config_to_proto(&config))
        }
        synctv_core::models::MediaSourceConfig::LiveProxy(config) => {
            Provider::LiveProxy(live_proxy_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Cloudreve(config) => {
            Provider::Cloudreve(source_config_proto::CloudreveMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Twitch(config) => {
            Provider::Twitch(twitch_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Youtube(config) => {
            Provider::Youtube(source_config_proto::YoutubeMediaSourceConfig {
                video_id: config.video_id,
                shared: config.shared,
            })
        }
        synctv_core::models::MediaSourceConfig::Huya(config) => {
            Provider::Huya(huya_media_source_config_to_proto(config))
        }
        synctv_core::models::MediaSourceConfig::Douyu(config) => {
            Provider::Douyu(source_config_proto::DouyuMediaSourceConfig { room: config.room })
        }
        synctv_core::models::MediaSourceConfig::Douyin(config) => {
            let source = match config {
                synctv_core::models::DouyinMediaSourceConfig::Video { aweme_id, shared } => {
                    source_config_proto::douyin_media_source_config::Source::Video(
                        source_config_proto::DouyinVideoSourceConfig { aweme_id, shared },
                    )
                }
                synctv_core::models::DouyinMediaSourceConfig::Live { web_rid, shared } => {
                    source_config_proto::douyin_media_source_config::Source::Live(
                        source_config_proto::DouyinLiveSourceConfig { web_rid, shared },
                    )
                }
            };
            Provider::Douyin(source_config_proto::DouyinMediaSourceConfig {
                source: Some(source),
            })
        }
        synctv_core::models::MediaSourceConfig::TikTok(config) => {
            let source = match config {
                synctv_core::models::TikTokMediaSourceConfig::Video { video_id, shared } => {
                    source_config_proto::tik_tok_media_source_config::Source::Video(
                        source_config_proto::TikTokVideoSourceConfig { video_id, shared },
                    )
                }
                synctv_core::models::TikTokMediaSourceConfig::Live { unique_id, shared } => {
                    source_config_proto::tik_tok_media_source_config::Source::Live(
                        source_config_proto::TikTokLiveSourceConfig { unique_id, shared },
                    )
                }
            };
            Provider::Tiktok(source_config_proto::TikTokMediaSourceConfig {
                source: Some(source),
            })
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
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
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
                            kind: synology_kind_to_proto(kind),
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
                path: config.path,
                file_id: config.file_id,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::Seafile(config) => {
            Provider::Seafile(source_config_proto::SeafileMediaSourceConfig {
                server_id: config.server_id,
                repository_id: config.repository_id,
                path: config.path,
                object_id: config.object_id,
                has_thumbnail: config.has_thumbnail,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
        synctv_core::models::MediaSourceConfig::TrueNas(config) => {
            Provider::Truenas(source_config_proto::TrueNasMediaSourceConfig {
                server_id: config.server_id,
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
            })
        }
    };

    Ok(source_config_proto::MediaSourceConfig {
        provider: Some(provider),
    })
}

pub fn playlist_source_config_to_proto(
    config: &synctv_core::models::PlaylistSourceConfig,
) -> Result<source_config_proto::PlaylistSourceConfig, crate::impls::ApiError> {
    use source_config_proto::playlist_source_config::Provider;

    let provider = match config.clone() {
        synctv_core::models::PlaylistSourceConfig::Bilibili(config) => {
            Provider::Bilibili(bilibili_playlist_source_config_to_proto(config))
        }
        synctv_core::models::PlaylistSourceConfig::Alist(config) => {
            Provider::Alist(alist_playlist_source_config_to_proto(config))
        }
        synctv_core::models::PlaylistSourceConfig::Emby(config) => {
            Provider::Emby(emby_playlist_source_config_to_proto(config))
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
                path: config.path,
                proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
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

    Ok(source_config_proto::PlaylistSourceConfig {
        provider: Some(provider),
    })
}

fn twitch_media_source_config_to_proto(
    config: synctv_core::models::TwitchMediaSourceConfig,
) -> source_config_proto::TwitchMediaSourceConfig {
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
    source_config_proto::TwitchMediaSourceConfig {
        source: Some(source),
    }
}

fn huya_media_source_config_to_proto(
    config: synctv_core::models::HuyaMediaSourceConfig,
) -> source_config_proto::HuyaMediaSourceConfig {
    use source_config_proto::huya_media_source_config::Source;

    let source = match config {
        synctv_core::models::HuyaMediaSourceConfig::Live { room_id } => {
            Source::Live(source_config_proto::HuyaLiveSourceConfig { room_id })
        }
        synctv_core::models::HuyaMediaSourceConfig::Video { video_id } => {
            Source::Video(source_config_proto::HuyaVideoSourceConfig { video_id })
        }
    };
    source_config_proto::HuyaMediaSourceConfig {
        source: Some(source),
    }
}

fn optional_index_to_proto(index: Option<usize>) -> Option<u32> {
    index.and_then(|index| u32::try_from(index).ok())
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

fn direct_url_media_source_config_to_proto(
    config: synctv_core::models::DirectUrlMediaSourceConfig,
) -> source_config_proto::DirectUrlMediaSourceConfig {
    source_config_proto::DirectUrlMediaSourceConfig {
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
    }
}

pub(crate) fn bilibili_media_source_config_to_proto(
    config: synctv_core::models::BilibiliMediaSourceConfig,
) -> source_config_proto::BilibiliMediaSourceConfig {
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

    source_config_proto::BilibiliMediaSourceConfig {
        source: Some(source),
        proxy_mode: playback_proxy_mode_to_proto(proxy_mode),
    }
}

pub(crate) fn bilibili_playlist_source_config_to_proto(
    config: synctv_core::models::BilibiliPlaylistSourceConfig,
) -> source_config_proto::BilibiliPlaylistSourceConfig {
    use source_config_proto::bilibili_playlist_source_config::Source;

    let source = match config.source {
        synctv_core::models::BilibiliPlaylistSource::VideoParts { bvid, aid } => {
            Source::VideoParts(source_config_proto::BilibiliVideoPartsPlaylistSource { bvid, aid })
        }
        synctv_core::models::BilibiliPlaylistSource::Popular => {
            Source::Popular(source_config_proto::BilibiliPopularPlaylistSource {})
        }
        synctv_core::models::BilibiliPlaylistSource::Recommended => {
            Source::Recommended(source_config_proto::BilibiliRecommendedPlaylistSource {})
        }
        synctv_core::models::BilibiliPlaylistSource::UpVideos { mid, keyword } => {
            Source::UpVideos(source_config_proto::BilibiliUpVideosPlaylistSource { mid, keyword })
        }
        synctv_core::models::BilibiliPlaylistSource::FavoriteVideos { media_id } => {
            Source::FavoriteVideos(source_config_proto::BilibiliFavoriteVideosPlaylistSource {
                media_id,
            })
        }
        synctv_core::models::BilibiliPlaylistSource::CollectionVideos { mid, season_id } => {
            Source::CollectionVideos(
                source_config_proto::BilibiliCollectionVideosPlaylistSource { mid, season_id },
            )
        }
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
            Source::PgcSeason(source_config_proto::BilibiliPgcSeasonPlaylistSource { season_id })
        }
        synctv_core::models::BilibiliPlaylistSource::LiveRecommended => {
            Source::LiveRecommended(source_config_proto::BilibiliLiveRecommendedPlaylistSource {})
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
    source_config_proto::BilibiliPlaylistSourceConfig {
        source: Some(source),
        shared: config.shared,
        proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
    }
}

fn alist_media_source_config_to_proto(
    config: synctv_core::models::AlistMediaSourceConfig,
) -> source_config_proto::AlistMediaSourceConfig {
    source_config_proto::AlistMediaSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
        proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
    }
}

fn alist_playlist_source_config_to_proto(
    config: synctv_core::models::AlistPlaylistSourceConfig,
) -> source_config_proto::AlistPlaylistSourceConfig {
    source_config_proto::AlistPlaylistSourceConfig {
        server_id: config.server_id,
        path: config.path,
        password: config.password,
        proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
    }
}

fn emby_media_source_config_to_proto(
    config: synctv_core::models::EmbyMediaSourceConfig,
) -> source_config_proto::EmbyMediaSourceConfig {
    source_config_proto::EmbyMediaSourceConfig {
        server_id: config.server_id,
        item_id: config.item_id,
        proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
    }
}

fn emby_playlist_source_config_to_proto(
    config: synctv_core::models::EmbyPlaylistSourceConfig,
) -> source_config_proto::EmbyPlaylistSourceConfig {
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
            Source::ContinueWatching(source_config_proto::EmbyContinueWatchingPlaylistSource {})
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
    source_config_proto::EmbyPlaylistSourceConfig {
        server_id: config.server_id,
        source: Some(source),
        proxy_mode: playback_proxy_mode_to_proto(config.proxy_mode),
    }
}

fn rtmp_media_source_config_to_proto(
    config: &synctv_core::models::RtmpMediaSourceConfig,
) -> source_config_proto::RtmpMediaSourceConfig {
    source_config_proto::RtmpMediaSourceConfig {
        mode: rtmp_stream_mode_to_proto(config.mode) as i32,
    }
}

fn live_proxy_media_source_config_to_proto(
    config: synctv_core::models::LiveProxyMediaSourceConfig,
) -> source_config_proto::LiveProxyMediaSourceConfig {
    use source_config_proto::live_proxy_media_source_config::Source;

    let source = match config.source {
        synctv_core::models::ExternalLiveSourceConfig::Rtmp { url, mode } => {
            Source::Rtmp(source_config_proto::RtmpPullSourceConfig {
                url,
                mode: rtmp_stream_mode_to_proto(mode) as i32,
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
    };
    source_config_proto::LiveProxyMediaSourceConfig {
        source: Some(source),
    }
}

fn rtmp_stream_mode_to_proto(
    mode: synctv_core::models::RtmpStreamMode,
) -> source_config_proto::RtmpStreamMode {
    match mode {
        synctv_core::models::RtmpStreamMode::Default => {
            source_config_proto::RtmpStreamMode::Default
        }
        synctv_core::models::RtmpStreamMode::VideoOnly => {
            source_config_proto::RtmpStreamMode::VideoOnly
        }
        synctv_core::models::RtmpStreamMode::AudioOnly => {
            source_config_proto::RtmpStreamMode::AudioOnly
        }
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

fn usize_to_i32(value: usize, field: &'static str) -> Result<i32, crate::impls::ApiError> {
    i32::try_from(value)
        .map_err(|_| crate::impls::ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub fn room_presence_stats_to_proto(
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

pub fn user_presence_stats_to_proto(
    stats: &synctv_core::service::OnlineUserStats,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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

pub fn node_presence_stats_to_proto(
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

pub fn presence_overview_to_proto(
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

fn can_browse_library_source_config(
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

fn access_url_field(
    access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
    field: &'static str,
) -> Result<String, crate::impls::ApiError> {
    let url = access
        .and_then(crate::impls::stored_files::stored_file_object_access_url)
        .ok_or_else(|| crate::impls::ApiError::Internal(format!("{field} url is missing")))?;
    if url.is_empty() {
        return Err(crate::impls::ApiError::Internal(format!(
            "{field} url is empty"
        )));
    }
    Ok(url)
}

fn proto_object_access(
    access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
) -> Option<synctv_proto::client::FileObjectAccess> {
    access
        .and_then(|access| access.object_access.as_ref())
        .map(crate::impls::stored_files::file_object_access_to_proto)
}

pub fn stored_file_reference_to_resource_cover(
    file: &synctv_core::models::StoredFileReference,
    access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
) -> Result<synctv_proto::client::ResourceCover, crate::impls::ApiError> {
    Ok(synctv_proto::client::ResourceCover {
        url: access_url_field(access, "resource cover")?,
        object_access: proto_object_access(access),
        metadata: file_metadata_to_proto(&file.metadata)?,
        variants: file_object_variants_from_metadata(&file.metadata, "resource cover")?,
    })
}

pub fn stored_file_reference_to_media_cover(
    file: &synctv_core::models::StoredFileReference,
    access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
) -> Result<synctv_proto::client::MediaCover, crate::impls::ApiError> {
    Ok(synctv_proto::client::MediaCover {
        id: file.file_reference_id.to_string(),
        url: access_url_field(access, "media cover")?,
        object_access: proto_object_access(access),
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        width: file.metadata.width.unwrap_or_default(),
        height: file.metadata.height.unwrap_or_default(),
        metadata: file_metadata_to_proto(&file.metadata)?,
        variants: file_object_variants_from_metadata(&file.metadata, "media cover")?,
    })
}

pub fn stored_file_reference_to_media_thumbnail(
    file: &synctv_core::models::StoredFileReference,
    access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
) -> Result<synctv_proto::client::MediaThumbnail, crate::impls::ApiError> {
    Ok(synctv_proto::client::MediaThumbnail {
        id: file.file_reference_id.to_string(),
        url: access_url_field(access, "media thumbnail")?,
        object_access: proto_object_access(access),
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        width: file.metadata.width.unwrap_or_default(),
        height: file.metadata.height.unwrap_or_default(),
        metadata: file_metadata_to_proto(&file.metadata)?,
        variants: file_object_variants_from_metadata(&file.metadata, "media thumbnail")?,
    })
}

pub fn source_url_to_resource_cover(url: String) -> synctv_proto::client::ResourceCover {
    synctv_proto::client::ResourceCover {
        url,
        object_access: None,
        metadata: None,
        variants: Vec::new(),
    }
}

pub fn source_url_to_media_cover(url: String) -> synctv_proto::client::MediaCover {
    synctv_proto::client::MediaCover {
        id: String::new(),
        url,
        object_access: None,
        mime_type: String::new(),
        size_bytes: 0,
        width: 0,
        height: 0,
        metadata: None,
        variants: Vec::new(),
    }
}

fn empty_media_cover() -> Option<synctv_proto::client::MediaCover> {
    None
}

fn empty_media_thumbnail() -> Option<synctv_proto::client::MediaThumbnail> {
    None
}

fn empty_resource_cover() -> Option<synctv_proto::client::ResourceCover> {
    None
}

pub(super) fn user_role_to_proto(role: synctv_core::models::UserRole) -> i32 {
    i32::from(role)
}

pub fn user_status_to_proto(status: synctv_core::models::UserStatus) -> i32 {
    i32::from(status)
}

pub fn member_status_to_proto(status: synctv_core::models::MemberStatus) -> i32 {
    i32::from(status)
}

pub const fn resource_availability_to_proto(is_available: bool) -> i32 {
    if is_available {
        synctv_proto::client::ResourceAvailability::Available as i32
    } else {
        synctv_proto::client::ResourceAvailability::CreatorInactive as i32
    }
}

pub const fn resource_availability_enum_to_proto(availability: ClientResourceAvailability) -> i32 {
    match availability {
        ClientResourceAvailability::Available => {
            synctv_proto::client::ResourceAvailability::Available as i32
        }
        ClientResourceAvailability::CreatorInactive => {
            synctv_proto::client::ResourceAvailability::CreatorInactive as i32
        }
    }
}

pub fn playback_client_profile_from_proto(
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

    let supported_live_transports = if profile.supported_live_transports.is_empty() {
        default_profile.supported_live_transports.clone()
    } else {
        profile
            .supported_live_transports
            .iter()
            .filter_map(|transport| {
                Some(
                    match synctv_proto::client::PlaybackLiveTransport::try_from(*transport) {
                        Ok(synctv_proto::client::PlaybackLiveTransport::Unspecified) => {
                            return None
                        }
                        Ok(synctv_proto::client::PlaybackLiveTransport::Hls) => {
                            Ok(synctv_core::provider::PlaybackLiveTransport::Hls)
                        }
                        Ok(synctv_proto::client::PlaybackLiveTransport::Flv) => {
                            Ok(synctv_core::provider::PlaybackLiveTransport::Flv)
                        }
                        Err(_) => Err(crate::impls::ApiError::InvalidInput(
                            "Unsupported playback live transport".to_string(),
                        )),
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?
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
        supported_live_transports,
    }))
}

pub fn proto_role_to_room_role(
    role_i32: i32,
) -> Result<synctv_core::models::RoomRole, crate::impls::ApiError> {
    synctv_core::models::RoomRole::try_from(role_i32).map_err(crate::impls::ApiError::InvalidInput)
}

pub fn proto_role_to_assignable_room_role(
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

pub fn proto_role_filter_to_room_role(
    role_i32: i32,
) -> Result<Option<synctv_core::models::RoomRole>, crate::impls::ApiError> {
    if role_i32 == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
        return Ok(None);
    }
    synctv_core::models::RoomRole::try_from(role_i32)
        .map(Some)
        .map_err(crate::impls::ApiError::InvalidInput)
}

pub fn proto_role_to_user_role(
    role_i32: i32,
) -> Result<synctv_core::models::UserRole, crate::impls::ApiError> {
    synctv_core::models::UserRole::try_from(role_i32).map_err(crate::impls::ApiError::InvalidInput)
}

#[must_use]
pub fn room_role_to_proto(role: synctv_core::models::RoomRole) -> i32 {
    i32::from(role)
}

pub fn try_user_to_proto(
    user: &synctv_core::models::User,
    email: Option<&str>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
        avatar_access: None,
        avatar: None,
    })
}

pub fn try_user_public_view_to_proto(
    user: &synctv_core::models::User,
    avatar_access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::UserPublicView, crate::impls::ApiError> {
    Ok(synctv_proto::client::UserPublicView {
        id: public_id_codec
            .encode_user_id(user.id)
            .map_err(|error| proto_encode_error("user", &error))?,
        username: user.username.clone(),
        role: user_role_to_proto(user.role),
        created_at: user.created_at.timestamp(),
        avatar_url: avatar_access
            .and_then(crate::impls::stored_files::stored_file_object_access_url)
            .unwrap_or_default(),
        avatar_access: proto_object_access(avatar_access),
        avatar: None,
    })
}

#[cfg(test)]
pub fn try_room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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

pub fn try_room_to_proto_basic_with_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    creator: Option<synctv_proto::client::UserPublicView>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
        .map(|file| stored_file_reference_to_resource_cover(file, cover_access))
        .transpose()?;
    Ok(proto)
}

pub fn try_room_to_proto_with_availability_and_presence(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    presence: Option<&synctv_core::service::OnlineRoomStats>,
    creator: Option<synctv_proto::client::UserPublicView>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
        status: i32::from(room.status),
        settings: Some(room_settings_to_proto(room_settings)),
        created_at: room.created_at.timestamp(),
        member_count,
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        availability: resource_availability_enum_to_proto(availability),
        version: i64::from(room.version),
        cover: empty_resource_cover(),
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
pub fn try_room_to_proto_with_availability_presence_and_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    presence: Option<&synctv_core::service::OnlineRoomStats>,
    creator: Option<synctv_proto::client::UserPublicView>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
        .map(|file| stored_file_reference_to_resource_cover(file, cover_access))
        .transpose()?;
    Ok(proto)
}

#[must_use]
pub fn normalize_created_room_settings(
    settings: Option<&synctv_core::models::RoomSettings>,
) -> synctv_core::models::RoomSettings {
    settings.cloned().unwrap_or_default()
}

pub fn try_media_to_proto_for_viewer_without_cover(
    media: &synctv_core::models::Media,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    provider_metadata: Option<&synctv_core::provider::ProviderResourceMetadata>,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let can_view_source = can_browse_library_source_config(media, viewer_id);
    let metadata = (can_view_source || provider_metadata.is_some())
        .then(|| {
            media_resource_metadata_to_proto(
                &media.source_config,
                can_view_source,
                provider_metadata,
                public_id_codec,
            )
        })
        .transpose()?;
    Ok(synctv_proto::client::Media {
        id: encode_media_id_for_proto(media.id, public_id_codec)?,
        room_id: encode_room_id_for_proto(media.room_id, public_id_codec)?,
        source_provider: core_source_provider_to_proto(media.source_provider),
        name: media.name.clone(),
        metadata,
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
        cover: empty_media_cover(),
        thumbnail: empty_media_thumbnail(),
    })
}

fn media_resource_metadata_to_proto(
    source_config: &synctv_core::models::MediaSourceConfig,
    include_source: bool,
    provider_metadata: Option<&synctv_core::provider::ProviderResourceMetadata>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::ResourceMetadata, crate::impls::ApiError> {
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

    Ok(synctv_proto::client::ResourceMetadata {
        source: include_source.then_some(source),
        provider: provider_metadata
            .map(|metadata| provider_resource_metadata_to_proto(metadata, public_id_codec))
            .transpose()?,
    })
}

#[derive(Clone, Copy)]
pub struct MediaProtoView<'a> {
    pub is_available: bool,
    pub viewer_id: Option<synctv_core::models::UserId>,
    pub cover: Option<&'a synctv_core::models::StoredFileReference>,
    pub cover_access: Option<&'a crate::impls::stored_files::StoredFileObjectAccess>,
    pub thumbnail: Option<&'a synctv_core::models::StoredFileReference>,
    pub thumbnail_access: Option<&'a crate::impls::stored_files::StoredFileObjectAccess>,
    pub public_id_codec: &'a synctv_adapter::PublicIdCodec,
}

pub fn try_media_to_proto_for_viewer_with_cover(
    media: &synctv_core::models::Media,
    view: MediaProtoView<'_>,
    provider_metadata: Option<&synctv_core::provider::ProviderResourceMetadata>,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let mut proto = try_media_to_proto_for_viewer_without_cover(
        media,
        view.is_available,
        view.viewer_id,
        view.public_id_codec,
        provider_metadata,
    )?;
    proto.cover = view
        .cover
        .map(|file| stored_file_reference_to_media_cover(file, view.cover_access))
        .transpose()?;
    proto.thumbnail = view
        .thumbnail
        .map(|file| stored_file_reference_to_media_thumbnail(file, view.thumbnail_access))
        .transpose()?;
    Ok(proto)
}

pub fn try_playlist_to_proto_for_viewer_without_cover(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    provider_metadata: Option<&synctv_core::provider::ProviderResourceMetadata>,
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
    let metadata = provider_metadata
        .map(|metadata| provider_resource_metadata_to_proto(metadata, public_id_codec))
        .transpose()?
        .map(|provider| synctv_proto::client::ResourceMetadata {
            source: None,
            provider: Some(provider),
        });

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
        cover: empty_resource_cover(),
        metadata,
        creator_id: playlist
            .creator_id
            .map(|creator_id| public_id_codec.encode_user_id(creator_id))
            .transpose()
            .map_err(|error| crate::impls::ApiError::Internal(error.clone()))?
            .unwrap_or_default(),
    })
}

pub struct DynamicPlaylistSourceFields<'a> {
    pub provider: synctv_core::models::SourceProvider,
    pub source_config: &'a synctv_core::models::PlaylistSourceConfig,
    pub provider_instance_name: Option<&'a str>,
}

pub fn dynamic_playlist_source_fields(
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

// This entry point deliberately mirrors the independent fields needed to
// authorize, enrich, and render a playlist resource in one conversion.
#[allow(clippy::too_many_arguments)]
pub fn try_playlist_to_proto_for_viewer_with_cover(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_access: Option<&crate::impls::stored_files::StoredFileObjectAccess>,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    provider_metadata: Option<&synctv_core::provider::ProviderResourceMetadata>,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    let mut proto = try_playlist_to_proto_for_viewer_without_cover(
        playlist,
        item_count,
        is_available,
        viewer_id,
        public_id_codec,
        provider_metadata,
    )?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_resource_cover(file, cover_access))
        .transpose()?;
    Ok(proto)
}

pub fn try_playlist_path_node_to_proto(
    playlist: &synctv_core::models::Playlist,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::PlaylistBrowsePathNode, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaylistBrowsePathNode {
        playlist_id: encode_playlist_id_for_proto(playlist.id, public_id_codec)?,
        name: playlist.name.clone(),
        target: None,
    })
}

pub fn try_playback_state_to_proto(
    state: &synctv_core::models::RoomPlaybackState,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::PlaybackState, crate::impls::ApiError> {
    let generated_at = synctv_core::SystemClock.now();
    let generated_at_millis = generated_at.timestamp_millis();
    let generated_at_for_position =
        synctv_core::clock::utc_from_millis(generated_at_millis).unwrap_or(generated_at);
    Ok(synctv_proto::client::PlaybackState {
        room_id: encode_room_id_for_proto(state.room_id, public_id_codec)?,
        playing_media_id: state
            .playing_media_id
            .map(|id| encode_media_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        position: state.computed_position_at(generated_at_for_position),
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
        generated_at_millis,
        history_cursor_id: state
            .history_cursor_id
            .map(|id| public_id_codec.encode_playback_history_entry_id(id))
            .transpose()
            .map_err(crate::impls::ApiError::InvalidInput)?
            .unwrap_or_default(),
        client_operation_id: String::new(),
    })
}

pub fn playback_history_page_to_proto(
    page: synctv_core::models::PlaybackHistoryPage,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::ListPlaybackHistoryResponse, crate::impls::ApiError> {
    let entries = page
        .entries
        .into_iter()
        .map(|entry| {
            Ok(synctv_proto::client::PlaybackHistoryEntry {
                id: public_id_codec
                    .encode_playback_history_entry_id(entry.id)
                    .map_err(|error| proto_encode_error("playback history entry", &error))?,
                media_id: entry
                    .media_id
                    .map(|id| encode_media_id_for_proto(id, public_id_codec))
                    .transpose()?
                    .unwrap_or_default(),
                playlist_id: entry
                    .playlist_id
                    .map(|id| encode_playlist_id_for_proto(id, public_id_codec))
                    .transpose()?
                    .unwrap_or_default(),
                target: entry.target.as_ref().map(provider_target_to_proto),
                position_seconds: entry.position_seconds,
                selected_by_user_id: entry
                    .selected_by_user_id
                    .map(|id| encode_user_id_for_proto(id, public_id_codec))
                    .transpose()?
                    .unwrap_or_default(),
                created_at: entry.created_at.timestamp(),
                updated_at: entry.updated_at.timestamp(),
                media_name: entry.media_name.unwrap_or_default(),
                playlist_name: entry.playlist_name.unwrap_or_default(),
                source_provider: entry
                    .source_provider
                    .map(core_source_provider_to_proto)
                    .unwrap_or_default(),
                provider_instance_name: entry.provider_instance_name.unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>, crate::impls::ApiError>>()?;
    Ok(synctv_proto::client::ListPlaybackHistoryResponse {
        entries,
        history_cursor_id: page
            .history_cursor_id
            .map(|id| public_id_codec.encode_playback_history_entry_id(id))
            .transpose()
            .map_err(|error| proto_encode_error("playback history entry", &error))?
            .unwrap_or_default(),
        next_before_entry_id: page
            .next_before_entry_id
            .map(|id| public_id_codec.encode_playback_history_entry_id(id))
            .transpose()
            .map_err(|error| proto_encode_error("playback history entry", &error))?
            .unwrap_or_default(),
    })
}

pub fn try_room_member_to_proto_with_permissions(
    member: &synctv_core::models::RoomMemberWithUser,
    permissions: synctv_core::models::RoomPermissionSet,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::common::RoomMember, crate::impls::ApiError> {
    Ok(synctv_proto::common::RoomMember {
        room_id: encode_room_id_for_proto(member.room_id, public_id_codec)?,
        user_id: encode_user_id_for_proto(member.user_id, public_id_codec)?,
        username: member.username.clone(),
        remark_name: member.remark_name.clone(),
        display_tag: member.display_tag.clone(),
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

pub fn try_members_to_proto(
    members: &[synctv_core::models::RoomMemberWithUser],
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
pub fn provider_playback_info_to_model(
    info: &synctv_core::provider::PlaybackInfo,
) -> synctv_core::models::media::PlaybackInfo {
    synctv_core::models::media::PlaybackInfo {
        thumbnail: info.thumbnail.clone(),
        medias: info.medias.clone(),
        default_media_index: info.default_media_index,
        subtitles: info.subtitles.clone(),
        default_subtitle_index: info.default_subtitle_index,
        danmakus: info.danmakus.clone(),
        default_danmaku_index: info.default_danmaku_index,
    }
}

/// Convert models `PlaybackResult` to proto `Playback`
pub fn try_playback_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
    validate_playback_result_shape(result)?;

    let playback_infos: std::collections::HashMap<String, synctv_proto::client::PlaybackInfo> =
        result
            .playback_infos
            .iter()
            .map(|(mode, info)| {
                Ok((
                    mode.clone(),
                    playback_info_to_proto(info, public_id_codec, signing)?,
                ))
            })
            .collect::<Result<_, crate::impls::ApiError>>()?;

    let metadata = playback_metadata_to_proto(result, public_id_codec)?;
    let expires_at = playback_infos
        .values()
        .flat_map(|info| {
            info.medias
                .iter()
                .filter_map(|media| media.expire_at)
                .chain(
                    info.subtitles
                        .iter()
                        .filter_map(|subtitle| subtitle.expire_at),
                )
                .chain(info.danmakus.iter().filter_map(|danmaku| danmaku.expire_at))
        })
        .min();

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
        provider: core_source_provider_to_proto(result.provider),
        provider_instance_name: result.provider_instance_name.clone().unwrap_or_default(),
        playback_infos,
        default_mode: result.default_mode.clone(),
        metadata,
        expires_at,
        duration_seconds: result.duration_seconds,
        playback_kind: playback_kind_to_proto(result.playback_kind),
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
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_proto::client::PlaybackMetadata>, crate::impls::ApiError> {
    result
        .metadata
        .as_ref()
        .map(|metadata| provider_resource_metadata_to_proto(metadata, public_id_codec))
        .transpose()
}

pub(crate) fn provider_resource_metadata_to_proto(
    metadata: &synctv_core::models::media::PlaybackMetadata,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<synctv_proto::client::PlaybackMetadata, crate::impls::ApiError> {
    use synctv_core::models::media::PlaybackMetadata;
    use synctv_proto::client::playback_metadata;

    let metadata = match metadata {
        PlaybackMetadata::Alist(metadata) => {
            playback_metadata::Metadata::Alist(synctv_proto::client::AlistPlaybackMetadata {
                name: metadata.name.clone(),
                size: metadata.size,
                provider: metadata.provider.clone(),
                external_subtitle_count: metadata
                    .external_subtitle_count
                    .map(|count| usize_to_i32(count, "external subtitle count"))
                    .transpose()?,
                video_preview_error: metadata.video_preview_error.clone(),
                transcoding_tasks: metadata
                    .transcoding_tasks
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
                video_preview: metadata
                    .video_preview
                    .as_ref()
                    .map(|preview| {
                        Ok::<_, crate::impls::ApiError>(
                            synctv_proto::client::AlistVideoPreviewMetadata {
                                drive_id: preview.drive_id.clone(),
                                file_id: preview.file_id.clone(),
                                provider: preview.provider.clone(),
                                category: preview.category.clone(),
                                transcoding_count: u64::try_from(preview.transcoding_count)
                                    .map_err(|_| {
                                        crate::impls::ApiError::Internal(
                                            "video preview transcoding count exceeds u64::MAX"
                                                .to_string(),
                                        )
                                    })?,
                                subtitle_count: u64::try_from(preview.subtitle_count).map_err(
                                    |_| {
                                        crate::impls::ApiError::Internal(
                                            "video preview subtitle count exceeds u64::MAX"
                                                .to_string(),
                                        )
                                    },
                                )?,
                            },
                        )
                    })
                    .transpose()?,
                width: metadata.width,
                height: metadata.height,
            })
        }
        PlaybackMetadata::Bilibili(metadata) => {
            playback_metadata::Metadata::Bilibili(synctv_proto::client::BilibiliPlaybackMetadata {
                kind: bilibili_playback_kind_to_proto(metadata.kind),
                bvid: metadata.bvid.clone(),
                aid: metadata.aid,
                epid: metadata.epid,
                cid: metadata.cid,
                min_buffer_time: metadata.min_buffer_time,
                fallback_format: metadata.fallback_format.clone(),
                quality: metadata.quality,
                room_id: metadata.room_id,
                live_started_at: metadata.live_started_at,
                is_live: metadata.is_live,
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Emby(metadata) => {
            playback_metadata::Metadata::Emby(synctv_proto::client::EmbyPlaybackMetadata {
                kind: emby_playback_kind_to_proto(metadata.kind),
                series_name: metadata.series_name.clone(),
                season_name: metadata.season_name.clone(),
                play_session_id: metadata.play_session_id.clone(),
            })
        }
        PlaybackMetadata::DirectUrl(metadata) => playback_metadata::Metadata::DirectUrl(
            synctv_proto::client::DirectUrlPlaybackMetadata {
                format: metadata.format.clone(),
                filename: metadata.filename.clone(),
            },
        ),
        PlaybackMetadata::LiveProxy(metadata) => playback_metadata::Metadata::LiveProxy(
            synctv_proto::client::LiveProxyPlaybackMetadata {
                media_id: encode_media_id_for_proto(metadata.media_id, public_id_codec)?,
                room_id: encode_room_id_for_proto(metadata.room_id, public_id_codec)?,
                source_host: metadata.source_host.clone(),
            },
        ),
        PlaybackMetadata::Live(metadata) => {
            playback_metadata::Metadata::Live(synctv_proto::client::LivePlaybackMetadata {
                media_id: encode_media_id_for_proto(metadata.media_id, public_id_codec)?,
                room_id: encode_room_id_for_proto(metadata.room_id, public_id_codec)?,
                availability: synctv_proto::client::LiveStreamAvailability::Unspecified as i32,
                stream_generation_id: String::new(),
            })
        }
        PlaybackMetadata::Twitch(metadata) => {
            playback_metadata::Metadata::Twitch(synctv_proto::client::TwitchPlaybackMetadata {
                resource_id: metadata.resource_id.clone(),
                title: metadata.title.clone(),
                author: metadata.author.clone(),
                category: metadata.category.clone(),
                thumbnail_url: metadata.thumbnail_url.clone(),
                description: metadata.description.clone(),
                view_count: metadata.view_count,
                published_at: metadata.published_at.clone(),
                chapters: metadata
                    .chapters
                    .iter()
                    .map(|chapter| synctv_proto::client::TwitchChapterMetadata {
                        title: chapter.title.clone(),
                        start_seconds: chapter.start_seconds,
                        end_seconds: chapter.end_seconds,
                    })
                    .collect(),
                storyboard_url: metadata.storyboard_url.clone(),
                is_live: metadata.is_live,
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Youtube(metadata) => {
            playback_metadata::Metadata::Youtube(synctv_proto::client::YoutubePlaybackMetadata {
                video_id: metadata.video_id.clone(),
                channel_id: metadata.channel_id.clone(),
                channel_name: metadata.channel_name.clone(),
                description: metadata.description.clone(),
                view_count: metadata.view_count,
                publish_date: metadata.publish_date.clone(),
                upload_date: metadata.upload_date.clone(),
                category: metadata.category.clone(),
                is_live: metadata.is_live,
                live_start: metadata.live_start.clone(),
                live_end: metadata.live_end.clone(),
                storyboard_spec: metadata.storyboard_spec.clone(),
                automatic_caption_count: u32::try_from(metadata.automatic_caption_count).map_err(
                    |_| {
                        crate::impls::ApiError::Internal(
                            "YouTube automatic caption count exceeds u32::MAX".to_string(),
                        )
                    },
                )?,
                manual_caption_count: u32::try_from(metadata.manual_caption_count).map_err(
                    |_| {
                        crate::impls::ApiError::Internal(
                            "YouTube manual caption count exceeds u32::MAX".to_string(),
                        )
                    },
                )?,
                translation_languages: metadata.translation_languages.clone(),
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Huya(metadata) => {
            playback_metadata::Metadata::Huya(synctv_proto::client::HuyaPlaybackMetadata {
                resource_id: metadata.resource_id.clone(),
                title: metadata.title.clone(),
                author: metadata.author.clone(),
                author_id: metadata.author_id.clone(),
                category: metadata.category.clone(),
                thumbnail_url: metadata.thumbnail_url.clone(),
                avatar_url: metadata.avatar_url.clone(),
                description: metadata.description.clone(),
                view_count: metadata.view_count,
                comment_count: metadata.comment_count,
                like_count: metadata.like_count,
                published_at: metadata.published_at,
                is_live: metadata.is_live,
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Douyu(metadata) => {
            playback_metadata::Metadata::Douyu(synctv_proto::client::DouyuPlaybackMetadata {
                room_id: metadata.room_id.clone(),
                title: metadata.title.clone(),
                author: metadata.author.clone(),
                category: metadata.category.clone(),
                thumbnail_url: metadata.thumbnail_url.clone(),
                avatar_url: metadata.avatar_url.clone(),
                is_replay: metadata.is_replay,
                is_vip: metadata.is_vip,
                viewer_count: metadata.viewer_count,
                started_at: metadata.started_at.clone(),
                is_live: metadata.is_live,
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Douyin(metadata) => {
            playback_metadata::Metadata::Douyin(synctv_proto::client::DouyinPlaybackMetadata {
                id: metadata.id.clone(),
                kind: douyin_playback_kind_to_proto(metadata.kind),
                author_id: metadata.author_id.clone(),
                author_sec_uid: metadata.author_sec_uid.clone(),
                author_name: metadata.author_name.clone(),
                description: metadata.description.clone(),
                view_count: metadata.view_count,
                like_count: metadata.like_count,
                comment_count: metadata.comment_count,
                share_count: metadata.share_count,
                collect_count: metadata.collect_count,
                created_at: metadata.created_at,
                music_title: metadata.music_title.clone(),
                music_author: metadata.music_author.clone(),
                is_live: metadata.is_live,
                room_id: metadata.room_id.clone(),
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::TikTok(metadata) => {
            playback_metadata::Metadata::Tiktok(synctv_proto::client::TikTokPlaybackMetadata {
                id: metadata.id.clone(),
                kind: tiktok_playback_kind_to_proto(metadata.kind),
                author_id: metadata.author_id.clone(),
                author_sec_uid: metadata.author_sec_uid.clone(),
                author_unique_id: metadata.author_unique_id.clone(),
                author_name: metadata.author_name.clone(),
                description: metadata.description.clone(),
                view_count: metadata.view_count,
                like_count: metadata.like_count,
                comment_count: metadata.comment_count,
                share_count: metadata.share_count,
                collect_count: metadata.collect_count,
                concurrent_viewers: metadata.concurrent_viewers,
                created_at: metadata.created_at,
                music_title: metadata.music_title.clone(),
                music_author: metadata.music_author.clone(),
                subtitle_count: u32::try_from(metadata.subtitle_count).unwrap_or(u32::MAX),
                is_live: metadata.is_live,
                room_id: metadata.room_id.clone(),
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::AcFun(metadata) => {
            playback_metadata::Metadata::AcFun(synctv_proto::client::AcFunPlaybackMetadata {
                resource_id: metadata.resource_id.clone(),
                title: metadata.title.clone(),
                author: metadata.author.clone(),
                author_id: metadata.author_id.clone(),
                category: metadata.category.clone(),
                thumbnail_url: metadata.thumbnail_url.clone(),
                avatar_url: metadata.avatar_url.clone(),
                description: metadata.description.clone(),
                tags: metadata.tags.clone(),
                view_count: metadata.view_count,
                like_count: metadata.like_count,
                comment_count: metadata.comment_count,
                published_at: metadata.published_at,
                started_at: metadata.started_at,
                is_live: metadata.is_live,
                is_currently_live: metadata.is_currently_live,
            })
        }
        PlaybackMetadata::Cctv(metadata) => {
            playback_metadata::Metadata::Cctv(synctv_proto::client::CctvPlaybackMetadata {
                video_id: metadata.video_id.clone(),
                title: metadata.title.clone(),
                description: metadata.description.clone(),
                uploader: metadata.uploader.clone(),
                producer: metadata.producer.clone(),
                channel: metadata.channel.clone(),
                column: metadata.column.clone(),
                tags: metadata.tags.clone(),
                thumbnail_url: metadata.thumbnail_url.clone(),
                published_at: metadata.published_at,
                chapters: metadata
                    .chapters
                    .iter()
                    .map(|chapter| synctv_proto::client::CctvChapterMetadata {
                        id: chapter.id.clone(),
                        title: chapter.title.clone(),
                        start_ms: chapter.start_ms,
                        end_ms: chapter.end_ms,
                    })
                    .collect(),
                protected: metadata.protected,
            })
        }
        PlaybackMetadata::Fnos(metadata) => {
            playback_metadata::Metadata::Fnos(synctv_proto::client::FnosPlaybackMetadata {
                kind: Some(match metadata {
                    synctv_core::models::FnosPlaybackMetadata::File(metadata) => {
                        synctv_proto::client::fnos_playback_metadata::Kind::File(
                            synctv_proto::client::FnosFilePlaybackMetadata {
                                name: metadata.name.clone(),
                                path: metadata.path.clone(),
                                size: metadata.size,
                                modified_at: metadata.modified_at,
                            },
                        )
                    }
                    synctv_core::models::FnosPlaybackMetadata::Media(metadata) => {
                        synctv_proto::client::fnos_playback_metadata::Kind::Media(
                            synctv_proto::client::FnosMediaPlaybackMetadata {
                                item_guid: metadata.item_guid.clone(),
                                media_guid: metadata.media_guid.clone(),
                                title: metadata.title.clone(),
                                overview: metadata.overview.clone(),
                                poster_url: metadata.poster_url.clone(),
                                backdrop_url: metadata.backdrop_url.clone(),
                                width: metadata.width,
                                height: metadata.height,
                                video_codec: metadata.video_codec.clone(),
                                video_profile: metadata.video_profile.clone(),
                                bit_depth: metadata.bit_depth,
                                dolby_vision_profile: metadata.dolby_vision_profile,
                                frame_rate: metadata.frame_rate.clone(),
                                season_number: metadata.season_number,
                                episode_number: metadata.episode_number,
                                progress_seconds: metadata.progress_seconds,
                                duration_seconds: metadata.duration_seconds,
                                watched: metadata.watched,
                                audio_tracks: metadata
                                    .audio_tracks
                                    .iter()
                                    .map(|track| synctv_proto::client::FnosAudioTrackMetadata {
                                        guid: track.guid.clone(),
                                        title: track.title.clone(),
                                        language: track.language.clone(),
                                        codec: track.codec.clone(),
                                        channels: track.channels,
                                        bitrate: track.bitrate,
                                        is_default: track.default,
                                    })
                                    .collect(),
                                subtitle_tracks: metadata
                                    .subtitle_tracks
                                    .iter()
                                    .map(|track| synctv_proto::client::FnosSubtitleTrackMetadata {
                                        guid: track.guid.clone(),
                                        title: track.title.clone(),
                                        language: track.language.clone(),
                                        codec: track.codec.clone(),
                                        format: track.format.clone(),
                                        external: track.external,
                                        is_default: track.default,
                                        forced: track.forced,
                                    })
                                    .collect(),
                            },
                        )
                    }
                }),
            })
        }
        PlaybackMetadata::Qnap(metadata) => {
            playback_metadata::Metadata::Qnap(synctv_proto::client::QnapPlaybackMetadata {
                name: metadata.name.clone(),
                path: metadata.path.clone(),
                size: metadata.size,
                modified_at: metadata.modified_at,
                file_type: metadata.file_type,
                realtime_transcode: metadata.realtime_transcode,
                hardware_transcode: metadata.hardware_transcode,
                multimedia_codec: metadata.multimedia_codec,
                pre_transcoded_heights: metadata.pre_transcoded_heights.clone(),
                realtime_heights: metadata.realtime_heights.clone(),
            })
        }
        PlaybackMetadata::Synology(metadata) => {
            playback_metadata::Metadata::Synology(synctv_proto::client::SynologyPlaybackMetadata {
                title: metadata.title.clone(),
                summary: metadata.summary.clone(),
                tagline: metadata.tagline.clone(),
                certificate: metadata.certificate.clone(),
                rating: metadata.rating,
                actors: metadata.actors.clone(),
                directors: metadata.directors.clone(),
                writers: metadata.writers.clone(),
                genres: metadata.genres.clone(),
                item_id: metadata.item_id,
                file_id: metadata.file_id,
                kind: synology_kind_to_proto(metadata.kind),
                path: metadata.path.clone(),
                size: metadata.size,
                duration_seconds: metadata.duration_seconds,
                progress_seconds: metadata.progress_seconds,
                width: metadata.width,
                height: metadata.height,
                video_codec: metadata.video_codec.clone(),
                audio_codec: metadata.audio_codec.clone(),
                container: metadata.container.clone(),
                video_bitrate: metadata.video_bitrate,
                audio_bitrate: metadata.audio_bitrate,
                frame_rate_numerator: metadata.frame_rate_numerator,
                frame_rate_denominator: metadata.frame_rate_denominator,
                audio_channels: metadata.audio_channels,
                audio_frequency_hz: metadata.audio_frequency_hz,
                poster_url: metadata.poster_url.clone(),
                backdrop_url: metadata.backdrop_url.clone(),
                watched: metadata.watched,
                watched_ratio: metadata.watched_ratio,
                parental_controlled: metadata.parental_controlled,
                create_time: metadata.create_time,
                last_watched: metadata.last_watched,
                audio_tracks: metadata
                    .audio_tracks
                    .iter()
                    .map(|track| synctv_proto::client::SynologyAudioTrackMetadata {
                        id: track.id,
                        language: track.language.clone(),
                        codec: track.codec.clone(),
                        channels: track.channels,
                        bitrate: track.bitrate,
                        is_default: track.default,
                    })
                    .collect(),
                subtitles: metadata
                    .subtitles
                    .iter()
                    .map(|subtitle| synctv_proto::client::SynologySubtitleMetadata {
                        id: subtitle.id.clone(),
                        language: subtitle.language.clone(),
                        title: subtitle.title.clone(),
                        format: subtitle.format.clone(),
                        embedded: subtitle.embedded,
                    })
                    .collect(),
            })
        }
        PlaybackMetadata::Nextcloud(metadata) => playback_metadata::Metadata::Nextcloud(
            synctv_proto::client::NextcloudPlaybackMetadata {
                file_id: metadata.file_id,
                name: metadata.name.clone(),
                path: metadata.path.clone(),
                size: metadata.size,
                modified_at: metadata.modified_at.clone(),
                content_type: metadata.content_type.clone(),
                etag: metadata.etag.clone(),
                permissions: metadata.permissions.clone(),
                owner_id: metadata.owner_id.clone(),
                owner_display_name: metadata.owner_display_name.clone(),
                favorite: metadata.favorite,
                has_preview: metadata.has_preview,
                blurhash: metadata.blurhash.clone(),
                width: metadata.width,
                height: metadata.height,
                duration_millis: metadata.duration_millis,
            },
        ),
        PlaybackMetadata::Seafile(metadata) => {
            playback_metadata::Metadata::Seafile(synctv_proto::client::SeafilePlaybackMetadata {
                repository_id: metadata.repository_id.clone(),
                object_id: metadata.object_id.clone(),
                name: metadata.name.clone(),
                path: metadata.path.clone(),
                size: metadata.size,
                modified_at: metadata.modified_at.clone(),
                is_locked: metadata.is_locked,
                can_preview: metadata.can_preview,
                can_edit: metadata.can_edit,
                has_thumbnail: metadata.has_thumbnail,
            })
        }
        PlaybackMetadata::TrueNas(metadata) => {
            playback_metadata::Metadata::Truenas(synctv_proto::client::TrueNasPlaybackMetadata {
                realpath: metadata.realpath.clone(),
                size: metadata.size,
                allocation_size: metadata.allocation_size,
                mode: metadata.mode,
                mount_id: metadata.mount_id,
                uid: metadata.uid,
                gid: metadata.gid,
                atime: metadata.atime,
                mtime: metadata.mtime,
                ctime: metadata.ctime,
                btime: metadata.btime,
                dev: metadata.dev,
                inode: metadata.inode,
                nlink: metadata.nlink,
                acl: metadata.acl,
                is_mountpoint: metadata.is_mountpoint,
                is_ctldir: metadata.is_ctldir,
                attributes: metadata.attributes.clone(),
                user: metadata.user.clone(),
                group: metadata.group.clone(),
            })
        }
    };

    Ok(synctv_proto::client::PlaybackMetadata {
        metadata: Some(metadata),
    })
}

fn signed_provider_thumbnail_url(
    provider: &str,
    version: &str,
    expires_at: i64,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    let version = version.trim();

    let signing = require_provider_signing(signing, "Alist thumbnail URL")?;
    let query = signed_provider_query(
        provider,
        version,
        expires_at,
        "thumbnail".to_string(),
        signing,
    );
    let version = path_segment_encode(version);
    Ok(format!(
        "/api/playback-providers/{provider}/{version}/thumbnail?{query}"
    ))
}

fn playback_info_thumbnail_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<Option<String>, crate::impls::ApiError> {
    let Some(thumbnail) = info
        .thumbnail
        .as_deref()
        .map(str::trim)
        .filter(|thumbnail| !thumbnail.is_empty())
    else {
        return Ok(None);
    };

    let Some((provider, version, expires_at)) = proxy_resource_for_thumbnail(info) else {
        return Ok(Some(thumbnail.to_string()));
    };

    signed_provider_thumbnail_url(provider, version, expires_at, signing).map(Some)
}

fn proxy_resource_for_thumbnail(
    info: &synctv_core::models::media::PlaybackInfo,
) -> Option<(&'static str, &str, i64)> {
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackFnosMedia, PlaybackMediaProvider, PlaybackQnapMedia,
    };

    info.medias.iter().find_map(|media| match &media.provider {
        PlaybackMediaProvider::Alist(PlaybackAlistMedia::ProxyFile {
            version,
            expires_at,
            ..
        }) => Some(("alist", version.as_str(), *expires_at)),
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
            version,
            expires_at,
            ..
        }) => Some(("fnos", version.as_str(), *expires_at)),
        PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Proxy {
            version,
            expires_at,
            ..
        }) => Some(("qnap", version.as_str(), *expires_at)),
        _ => None,
    })
}

/// Convert models `PlaybackInfo` to proto `PlaybackInfo`
fn playback_info_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
    public_id_codec: &synctv_adapter::PublicIdCodec,
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
    let thumbnail = playback_info_thumbnail_to_proto(info, signing)?;
    Ok(synctv_proto::client::PlaybackInfo {
        thumbnail,
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
    let (url_value, signed_expires_at) = playback_media_url(media, signing)?;
    let expire_at = [
        media.expire_at.map(|expires_at| expires_at.timestamp()),
        signed_expires_at,
    ]
    .into_iter()
    .flatten()
    .min();
    let p2p_delivery = synctv_core::provider::playback_media_p2p_delivery(media)
        .map(|delivery| p2p_resource_delivery_to_proto(delivery, signing))
        .transpose()?;
    Ok(synctv_proto::client::PlaybackMedia {
        name: media.name.clone(),
        url: require_non_empty_url(&url_value, "playback")?,
        headers: playback_media_headers_for_proto(media),
        format: media.format.clone(),
        expire_at,
        metadata: media
            .metadata
            .as_ref()
            .map(playback_media_metadata_to_proto)
            .transpose()?,
        p2p_delivery,
    })
}

fn playback_media_headers_for_proto(
    media: &synctv_core::models::media::PlaybackMedia,
) -> std::collections::HashMap<String, String> {
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackBilibiliMedia, PlaybackCloudreveMedia, PlaybackDirectUrlMedia,
        PlaybackEmbyMedia, PlaybackFnosMedia, PlaybackMediaProvider, PlaybackNextcloudMedia,
        PlaybackQnapMedia, PlaybackSeafileMedia, PlaybackSynologyMedia, PlaybackTrueNasMedia,
    };

    match &media.provider {
        PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Bilibili(
            PlaybackBilibiliMedia::Direct { headers, .. }
            | PlaybackBilibiliMedia::DirectDashManifest { headers, .. }
            | PlaybackBilibiliMedia::DirectDurlManifest { headers, .. }
            | PlaybackBilibiliMedia::DurlManifest { headers, .. },
        )
        | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Direct { headers, .. })
        | PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct { headers, .. })
        | PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct { headers, .. }) => {
            headers.clone()
        }
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
    let p2p_delivery = synctv_core::provider::playback_subtitle_p2p_delivery(subtitle)
        .map(|delivery| p2p_resource_delivery_to_proto(delivery, signing))
        .transpose()?;
    Ok(synctv_proto::client::PlaybackSubtitle {
        name: subtitle.name.clone(),
        language: subtitle.language.clone(),
        url: require_non_empty_url(&url_value, "subtitle")?,
        headers: client_visible_headers(&url_value, &subtitle.upstream_headers()),
        format: subtitle.format.clone(),
        expire_at: subtitle.expiration_timestamp(),
        p2p_delivery,
    })
}

fn danmaku_to_proto(
    danmaku: &synctv_core::models::media::PlaybackDanmaku,
    public_id_codec: &synctv_adapter::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::PlaybackDanmaku, crate::impls::ApiError> {
    let url_value = playback_danmaku_url(danmaku, public_id_codec, signing)?;
    let p2p_delivery = synctv_core::provider::playback_danmaku_p2p_delivery(danmaku)
        .map(|delivery| p2p_resource_delivery_to_proto(delivery, signing))
        .transpose()?;
    Ok(synctv_proto::client::PlaybackDanmaku {
        name: danmaku.name.clone(),
        url: require_non_empty_url(&url_value, "danmaku")?,
        format: danmaku.format.clone(),
        headers: client_visible_headers(&url_value, &danmaku.upstream_headers()),
        expire_at: danmaku.expiration_timestamp(),
        p2p_delivery,
    })
}

fn p2p_resource_delivery_to_proto(
    delivery: synctv_core::provider::P2pResourceDelivery,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<synctv_proto::client::P2pResourceDelivery, crate::impls::ApiError> {
    let signing = signing.ok_or_else(|| {
        crate::impls::ApiError::Internal(
            "P2P resource delivery requires playback signing context".to_string(),
        )
    })?;
    let swarm_ticket = signing.media_swarm_signing_key.sign_media_swarm_ticket(
        signing.room_id,
        signing.actor_id,
        &delivery.swarm_id,
    );
    Ok(synctv_proto::client::P2pResourceDelivery {
        swarm_id: delivery.swarm_id,
        swarm_ticket,
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
        .build_signed_query(&crate::proxy_signature::ProxyUrlClaims {
            provider: provider.to_string(),
            version: version.to_string(),
            resource,
            room_id: signing.room_id.to_string(),
            user_id: signing.proxy_authorizer_id.to_string(),
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
) -> Result<(String, Option<i64>), crate::impls::ApiError> {
    use synctv_core::models::media::{
        PlaybackAcFunMedia, PlaybackAlistMedia, PlaybackBilibiliMedia, PlaybackCctvMedia,
        PlaybackCloudreveMedia, PlaybackDirectUrlMedia, PlaybackDouyinMedia, PlaybackDouyuMedia,
        PlaybackEmbyMedia, PlaybackFnosMedia, PlaybackHuyaMedia, PlaybackLiveProxyMedia,
        PlaybackMediaProvider, PlaybackNextcloudMedia, PlaybackQnapMedia, PlaybackRtmpMedia,
        PlaybackSeafileMedia, PlaybackSynologyMedia, PlaybackTikTokMedia, PlaybackTrueNasMedia,
        PlaybackTwitchMedia, PlaybackYoutubeMedia,
    };

    let (provider, version, expires_at, path, resource) = match &media.provider {
        PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, .. })
        | PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { url, .. })
        | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { url, .. })
        | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { url, .. })
        | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { url, .. })
        | PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Direct { url, .. })
        | PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Direct { url, .. })
        | PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Direct { url, .. })
        | PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Direct { url, .. })
        | PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct { url, .. })
        | PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct { url, .. }) => {
            return Ok((url.clone(), None));
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
        PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::ProxyStream {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::CloudreveProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::ProxyHlsManifest {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::CloudreveProvider::NAME,
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            *media_index,
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
        PlaybackMediaProvider::Bilibili(
            PlaybackBilibiliMedia::DirectDurlManifest {
                version,
                expires_at,
                mode_name,
                ..
            }
            | PlaybackBilibiliMedia::DurlManifest {
                version,
                expires_at,
                mode_name,
                ..
            },
        ) => versioned_indexed_resource(
            "bilibili",
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            0,
        ),
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
            synctv_core::provider::DirectUrlProvider::NAME,
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
            synctv_core::provider::DirectUrlProvider::NAME,
            version,
            *expires_at,
            "hls-manifests",
            mode_name,
            *url_index,
        ),
        PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyDashManifest {
            version,
            expires_at,
            mode_name,
            url_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::DirectUrlProvider::NAME,
            version,
            *expires_at,
            "dash-manifests",
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
        PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsMaster {
            version,
            expires_at,
            ..
        }) => (
            "rtmp",
            version.clone(),
            *expires_at,
            "hls-master".to_string(),
            "hls-master".to_string(),
        ),
        PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::FlvStream {
            version,
            expires_at,
            ..
        }) => (
            synctv_core::provider::LiveProxyProvider::NAME,
            version.clone(),
            *expires_at,
            "flv-stream".to_string(),
            "flv-stream".to_string(),
        ),
        PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsMaster {
            version,
            expires_at,
            ..
        }) => (
            synctv_core::provider::LiveProxyProvider::NAME,
            version.clone(),
            *expires_at,
            "hls-master".to_string(),
            "hls-master".to_string(),
        ),
        PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::TwitchProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Twitch(PlaybackTwitchMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Twitch playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::YoutubeProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked YouTube playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::HuyaProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Huya playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::DouyuProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Douyu playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::DouyinProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Douyin playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::TikTokProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked TikTok playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::AcFunProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked AcFun playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => versioned_indexed_resource(
            synctv_core::provider::CctvProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked CCTV playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::FnosProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Fnos(
            PlaybackFnosMedia::FileRefresh { .. }
            | PlaybackFnosMedia::MediaRefresh { .. }
            | PlaybackFnosMedia::MediaOriginalRefresh { .. }
            | PlaybackFnosMedia::TranscodeRefresh { .. },
        ) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked FNOS playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            "qnap",
            version,
            *expires_at,
            storage_media_resource_prefix(media),
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked QNAP playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::SynologyProvider::NAME,
            version,
            *expires_at,
            "resources",
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Synology playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::NextcloudProvider::NAME,
            version,
            *expires_at,
            storage_media_resource_prefix(media),
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Nextcloud playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::SeafileProvider::NAME,
            version,
            *expires_at,
            storage_media_resource_prefix(media),
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked Seafile playback resource".to_string(),
            ));
        }
        PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
            ..
        }) => versioned_indexed_resource(
            synctv_core::provider::TrueNasProvider::NAME,
            version,
            *expires_at,
            storage_media_resource_prefix(media),
            mode_name,
            *media_index,
        ),
        PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked TrueNAS playback resource".to_string(),
            ));
        }
    };
    let signing = require_provider_signing(signing, "playback provider URL")?;
    let encoded_version = path_segment_encode(&version);
    let query = signed_provider_query(provider, &version, expires_at, resource, signing);
    let route_provider = playback_provider_route_slug(provider);
    let separator = if path.contains('?') { '&' } else { '?' };
    Ok((
        format!(
            "/api/playback-providers/{route_provider}/{encoded_version}/{path}{separator}{query}"
        ),
        Some(expires_at),
    ))
}

fn playback_provider_route_slug(provider: &str) -> &str {
    match provider {
        synctv_core::provider::DirectUrlProvider::NAME => "direct-url",
        synctv_core::provider::LiveProxyProvider::NAME => "live-proxy",
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

fn storage_media_resource_prefix(
    media: &synctv_core::models::media::PlaybackMedia,
) -> &'static str {
    if matches!(
        media.format.trim().to_ascii_lowercase().as_str(),
        "m3u8" | "hls"
    ) {
        "hls-manifests"
    } else {
        "resources"
    }
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
        PlaybackAlistSubtitle, PlaybackBilibiliSubtitle, PlaybackCloudreveSubtitle,
        PlaybackDirectUrlSubtitle, PlaybackEmbySubtitle, PlaybackFnosSubtitle,
        PlaybackNextcloudSubtitle, PlaybackQnapSubtitle, PlaybackSeafileSubtitle,
        PlaybackSubtitleProvider, PlaybackSynologySubtitle, PlaybackTikTokSubtitle,
        PlaybackTrueNasSubtitle, PlaybackYoutubeSubtitle,
    };
    let (provider, version, expires_at, mode_name, subtitle_index) = match &subtitle.provider {
        PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct { url, .. })
        | PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle::Refresh { url, .. })
        | PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Direct { url, .. })
        | PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct { url, .. })
        | PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Direct { url, .. })
        | PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Direct { url, .. }) => {
            return Ok(url.clone());
        }
        PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("alist", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
        }) => (
            synctv_core::provider::CloudreveProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("bilibili", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => (
            synctv_core::provider::DirectUrlProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("emby", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("fnos", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::Qnap(PlaybackQnapSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => ("qnap", version, *expires_at, mode_name, *subtitle_index),
        PlaybackSubtitleProvider::Nextcloud(PlaybackNextcloudSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => (
            synctv_core::provider::NextcloudProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Seafile(PlaybackSeafileSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => (
            synctv_core::provider::SeafileProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::TrueNas(PlaybackTrueNasSubtitle {
            version,
            expires_at,
            mode_name,
            subtitle_index,
            ..
        }) => (
            synctv_core::provider::TrueNasProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Synology(
            PlaybackSynologySubtitle::File {
                version,
                expires_at,
                mode_name,
                subtitle_index,
                ..
            }
            | PlaybackSynologySubtitle::VideoStation {
                version,
                expires_at,
                mode_name,
                subtitle_index,
                ..
            },
        ) => (
            synctv_core::provider::SynologyProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
        }) => (
            synctv_core::provider::YoutubeProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked YouTube subtitle resource".to_string(),
            ));
        }
        PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Proxy {
            version,
            expires_at,
            mode_name,
            subtitle_index,
        }) => (
            synctv_core::provider::TikTokProvider::NAME,
            version,
            *expires_at,
            mode_name,
            *subtitle_index,
        ),
        PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh { .. }) => {
            return Err(crate::impls::ApiError::Internal(
                "unmarked TikTok subtitle resource".to_string(),
            ));
        }
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
    public_id_codec: &synctv_adapter::PublicIdCodec,
    signing: Option<&PlaybackHttpSigningContext<'_>>,
) -> Result<String, crate::impls::ApiError> {
    use synctv_core::models::media::{
        PlaybackAcFunDanmaku, PlaybackBilibiliDanmaku, PlaybackDanmakuProvider,
        PlaybackDouyinDanmaku, PlaybackDouyuDanmaku, PlaybackHuyaDanmaku, PlaybackTwitchDanmaku,
    };
    match &danmaku.provider {
        PlaybackDanmakuProvider::DirectUrl(danmaku) => Ok(danmaku.url.clone()),
        PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileDirect { url, .. }) => {
            Ok(url.clone())
        }
        PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileProxy {
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
        PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "Twitch chat URL")?;
            let resource = format!("chats/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::TwitchProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/twitch/{}/chats/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Refresh { .. }) => Err(
            crate::impls::ApiError::Internal("unmarked Twitch chat resource".to_string()),
        ),
        PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "Huya danmaku URL")?;
            let resource = format!("danmakus/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::HuyaProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/huya/{}/danmakus/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Refresh { .. }) => Err(
            crate::impls::ApiError::Internal("unmarked Huya danmaku resource".to_string()),
        ),
        PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "Douyu danmaku URL")?;
            let resource = format!("danmakus/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::DouyuProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/douyu/{}/danmakus/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Refresh { .. }) => Err(
            crate::impls::ApiError::Internal("unmarked Douyu danmaku resource".to_string()),
        ),
        PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Proxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "Douyin danmaku URL")?;
            let resource = format!("danmakus/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::DouyinProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/douyin/{}/danmakus/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Refresh { .. }) => Err(
            crate::impls::ApiError::Internal("unmarked Douyin danmaku resource".to_string()),
        ),
        PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::FileProxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "AcFun VOD danmaku URL")?;
            let resource = format!("danmaku-files/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::AcFunProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/acfun/{}/danmaku-files/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::LiveProxy {
            version,
            expires_at,
            mode_name,
            media_index,
        }) => {
            let signing = require_provider_signing(signing, "AcFun live danmaku URL")?;
            let resource = format!("danmakus/{mode_name}/{media_index}");
            let query = signed_provider_query(
                synctv_core::provider::AcFunProvider::NAME,
                version,
                *expires_at,
                resource,
                signing,
            );
            Ok(format!(
                "/api/playback-providers/acfun/{}/danmakus/{}/{}?{query}",
                path_segment_encode(version),
                path_segment_encode(mode_name),
                media_index,
            ))
        }
        PlaybackDanmakuProvider::AcFun(
            PlaybackAcFunDanmaku::FileRefresh { .. } | PlaybackAcFunDanmaku::LiveRefresh { .. },
        ) => Err(crate::impls::ApiError::Internal(
            "unmarked AcFun danmaku resource".to_string(),
        )),
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
    use std::collections::HashMap;
    use synctv_core::models::media::{
        PlaybackAlistMedia, PlaybackBilibiliDanmaku, PlaybackBilibiliMedia, PlaybackDanmaku,
        PlaybackDanmakuProvider, PlaybackDirectUrlMedia, PlaybackDirectUrlSubtitle,
        PlaybackDouyuDanmaku, PlaybackDouyuMedia, PlaybackHuyaDanmaku, PlaybackHuyaMedia,
        PlaybackInfo, PlaybackLiveProxyMedia, PlaybackMedia, PlaybackMediaProvider,
        PlaybackNextcloudMedia, PlaybackQnapMedia, PlaybackResult, PlaybackSeafileMedia,
        PlaybackSubtitle, PlaybackSubtitleProvider, PlaybackTrueNasMedia, QnapPlaybackMode,
        QnapPlaybackResource,
    };

    fn direct_url_media(name: &str, url: &str) -> PlaybackMedia {
        PlaybackMedia {
            name: name.to_string(),
            format: "mp4".to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: None,
            provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                url: url.to_string(),
                headers: HashMap::new(),
            }),
        }
    }

    fn signing_key() -> crate::proxy_signature::ProxySigningKey {
        crate::proxy_signature::ProxySigningKey::try_derive_from(
            b"test-proxy-signing-secret-that-is-long-enough",
        )
        .expect("test signing key should derive")
    }

    fn signing_context(
        key: &crate::proxy_signature::ProxySigningKey,
    ) -> PlaybackHttpSigningContext<'_> {
        static SWARM_KEY: std::sync::OnceLock<crate::proxy_signature::MediaSwarmSigningKey> =
            std::sync::OnceLock::new();
        PlaybackHttpSigningContext {
            signing_key: key,
            media_swarm_signing_key: SWARM_KEY.get_or_init(|| {
                crate::proxy_signature::MediaSwarmSigningKey::try_derive_from(
                    b"test-media-swarm-signing-key-for-playback-convert",
                )
                .expect("test media swarm signing key should derive")
            }),
            room_id: "room-1",
            proxy_authorizer_id: "user-1",
            actor_id: "user-1",
        }
    }

    fn codec() -> synctv_adapter::PublicIdCodec {
        synctv_adapter::PublicIdCodec::plain()
    }

    #[test]
    fn provider_playback_kinds_map_to_typed_proto_enums() {
        assert_eq!(
            bilibili_playback_kind_to_proto(synctv_core::models::BilibiliPlaybackKind::Pgc),
            synctv_proto::client::BilibiliPlaybackKind::Pgc as i32
        );
        assert_eq!(
            douyin_playback_kind_to_proto(synctv_core::models::DouyinPlaybackKind::Live),
            synctv_proto::client::DouyinPlaybackKind::Live as i32
        );
        assert_eq!(
            emby_playback_kind_to_proto(synctv_core::models::EmbyPlaybackKind::MusicAlbum),
            synctv_proto::client::EmbyPlaybackKind::MusicAlbum as i32
        );
        assert_eq!(
            tiktok_playback_kind_to_proto(synctv_core::models::TikTokPlaybackKind::Video),
            synctv_proto::client::TikTokPlaybackKind::Video as i32
        );
        assert_eq!(
            synology_kind_to_proto(synctv_core::models::SynologyLibraryItemKind::TvRecording),
            synctv_proto::source_config::SynologyLibraryItemKind::TvRecording as i32
        );
    }

    fn playback_result_with_mode(mode: &str, info: PlaybackInfo) -> PlaybackResult {
        let mut playback_infos = HashMap::new();
        playback_infos.insert(mode.to_string(), info);
        PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: synctv_core::models::RoomId::new(),
            name: "media".to_string(),
            provider: synctv_core::models::SourceProvider::DirectUrl,
            provider_instance_name: None,
            position: 0.0,
            playback_infos,
            default_mode: mode.to_string(),
            duration_seconds: None,
            playback_kind: synctv_core::models::PlaybackKind::Regular,
            target: None,
            metadata: None,
        }
    }

    fn signed_query(url: &str) -> &str {
        url.split_once('?')
            .map(|(_, query)| query)
            .expect("signed provider URL should include query")
    }

    fn storage_proxy_provider(provider: &str, expires_at: i64) -> PlaybackMediaProvider {
        match provider {
            "nextcloud" => PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Proxy {
                version: "storage v1".to_string(),
                expires_at,
                mode_name: "proxy mode".to_string(),
                media_index: 0,
                credential_owner_id: "42".to_string(),
                server_id: "nextcloud-main".to_string(),
                path: "/Videos/Movie.m3u8".to_string(),
                file_id: 7,
            }),
            "qnap" => PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Proxy {
                version: "storage v1".to_string(),
                expires_at,
                mode_name: "proxy mode".to_string(),
                media_index: 0,
                credential_owner_id: "42".to_string(),
                server_id: "qnap-main".to_string(),
                resource: QnapPlaybackResource {
                    path: "/Videos/Movie.m3u8".to_string(),
                    mode: QnapPlaybackMode::Original,
                },
            }),
            "seafile" => PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Proxy {
                version: "storage v1".to_string(),
                expires_at,
                mode_name: "proxy mode".to_string(),
                media_index: 0,
                credential_owner_id: "42".to_string(),
                server_id: "seafile-main".to_string(),
                repository_id: "library".to_string(),
                path: "/Videos/Movie.m3u8".to_string(),
                object_id: "object".to_string(),
            }),
            "truenas" => PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Proxy {
                version: "storage v1".to_string(),
                expires_at,
                mode_name: "proxy mode".to_string(),
                media_index: 0,
                credential_owner_id: "42".to_string(),
                server_id: "truenas-main".to_string(),
                path: "/mnt/tank/Videos/Movie.m3u8".to_string(),
            }),
            _ => panic!("unsupported storage provider test case: {provider}"),
        }
    }

    #[test]
    fn storage_provider_media_routes_match_the_declared_format() {
        let key = signing_key();
        let signing = signing_context(&key);
        let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;

        for provider in [
            synctv_core::models::SourceProvider::Nextcloud,
            synctv_core::models::SourceProvider::Qnap,
            synctv_core::models::SourceProvider::Seafile,
            synctv_core::models::SourceProvider::TrueNas,
        ] {
            let provider_name = provider.as_str();
            for (format, route) in [(" HLS ", "hls-manifests"), ("mp4", "resources")] {
                let info = PlaybackInfo::builder()
                    .add_media(PlaybackMedia {
                        name: "Storage media".to_string(),
                        format: format.to_string(),
                        expire_at: None,
                        metadata: None,
                        p2p_swarm_id: None,
                        provider: storage_proxy_provider(provider_name, expires_at),
                    })
                    .build();
                let mut result = playback_result_with_mode("proxy mode", info);
                result.provider = provider;
                let proto = try_playback_to_proto(&result, &codec(), Some(&signing))
                    .expect("storage playback should convert");
                let media = &proto.playback_infos["proxy mode"].medias[0];
                let expected_resource = format!("{route}/proxy mode/0");

                assert_eq!(media.expire_at, Some(expires_at));
                assert!(
                    media.url.starts_with(&format!(
                        "/api/playback-providers/{provider_name}/storage%20v1/{route}/proxy%20mode/0?"
                    )),
                    "unexpected {provider_name} {format} URL: {}",
                    media.url
                );
                key.parse_and_verify_query(
                    signed_query(&media.url),
                    provider_name,
                    "storage v1",
                    &expected_resource,
                )
                .expect("storage signature should bind the selected route");
            }
        }
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
                expire_at: synctv_core::SystemClock
                    .now()
                    .checked_add_signed(chrono::Duration::minutes(30)),
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Bilibili(
                    PlaybackBilibiliMedia::DirectDashManifest {
                        version: "v1".to_string(),
                        expires_at: synctv_core::SystemClock.now().timestamp() + 1800,
                        mode_name: "h264".to_string(),
                        headers: headers.clone(),
                    },
                ),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("h264", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let media = &proto.playback_infos["h264"].medias[0];
        assert!(
            media
                .url
                .starts_with("/api/playback-providers/bilibili/v1/dash-manifests/h264/direct?"),
            "unexpected direct DASH URL: {}",
            media.url
        );
        assert_eq!(media.headers, headers);
    }

    #[test]
    fn direct_durl_manifest_preserves_bilibili_headers_for_clients() {
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
                name: "MP4".to_string(),
                format: "m3u8".to_string(),
                expire_at: synctv_core::SystemClock
                    .now()
                    .checked_add_signed(chrono::Duration::minutes(30)),
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Bilibili(
                    PlaybackBilibiliMedia::DirectDurlManifest {
                        version: "v1".to_string(),
                        expires_at: synctv_core::SystemClock.now().timestamp() + 1800,
                        mode_name: "mp4".to_string(),
                        segments: vec![synctv_core::models::media::BilibiliDurlSegment {
                            url: "https://cdn.example/video.mp4".to_string(),
                            backup_urls: Vec::new(),
                            duration_millis: 1_000,
                        }],
                        headers: headers.clone(),
                    },
                ),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("mp4", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let media = &proto.playback_infos["mp4"].medias[0];
        assert!(
            media
                .url
                .starts_with("/api/playback-providers/bilibili/v1/hls-manifests/mp4/0?"),
            "unexpected direct DURL URL: {}",
            media.url
        );
        assert_eq!(media.headers, headers);
    }

    #[test]
    fn live_danmaku_provider_converts_to_live_endpoint() {
        let key = signing_key();
        let signing = signing_context(&key);
        let room_id = synctv_core::models::RoomId::new();
        let media_id = synctv_core::models::MediaId::new();
        let codec = codec();
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "Live HLS".to_string(),
                format: "hls".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                    url: "https://example.com/live.m3u8".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Bilibili Live Danmaku".to_string(),
                format: Some("synctv-bilibili-live".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                    room_id,
                    media_id,
                }),
            })
            .default_danmaku_index(0)
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("hls", info),
            &codec,
            Some(&signing),
        )
        .expect("playback should convert");
        let danmaku = &proto.playback_infos["hls"].danmakus[0];
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
        let provider_info = synctv_core::provider::PlaybackInfo {
            thumbnail: None,
            medias: vec![
                direct_url_media("primary", "https://example.com/1.mp4"),
                direct_url_media("selected", "https://example.com/2.mp4"),
            ],
            default_media_index: Some(1),
            subtitles: vec![
                PlaybackSubtitle {
                    name: "English".to_string(),
                    language: "en".to_string(),
                    format: "vtt".to_string(),
                    p2p_swarm_id: None,
                    provider: PlaybackSubtitleProvider::DirectUrl(
                        PlaybackDirectUrlSubtitle::Direct {
                            url: "https://example.com/en.vtt".to_string(),
                            headers: HashMap::new(),
                            expire_at: None,
                        },
                    ),
                },
                PlaybackSubtitle {
                    name: "Japanese".to_string(),
                    language: "ja".to_string(),
                    format: "vtt".to_string(),
                    p2p_swarm_id: None,
                    provider: PlaybackSubtitleProvider::DirectUrl(
                        PlaybackDirectUrlSubtitle::Direct {
                            url: "https://example.com/ja.vtt".to_string(),
                            headers: HashMap::new(),
                            expire_at: None,
                        },
                    ),
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
        let key = signing_key();
        let signing = signing_context(&key);
        let info = PlaybackInfo::builder()
            .add_media(direct_url_media("first", "https://example.com/1.mp4"))
            .add_media(direct_url_media("second", "https://example.com/2.mp4"))
            .default_media_index(1)
            .add_subtitle(PlaybackSubtitle {
                name: "English".to_string(),
                language: "en".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: None,
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                    url: "https://example.com/en.vtt".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            })
            .add_subtitle(PlaybackSubtitle {
                name: "Japanese".to_string(),
                language: "ja".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: None,
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                    url: "https://example.com/ja.vtt".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            })
            .default_subtitle_index(1)
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("direct", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let info = &proto.playback_infos["direct"];

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
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
                    version: "v 1".to_string(),
                    expires_at: synctv_core::SystemClock.now().timestamp() + 1800,
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
                synctv_core::provider::DirectUrlProvider::NAME,
                "v 1",
                "streams/My Source+Main/0",
            )
            .expect("signature should bind decoded resource");
        assert_eq!(claims.resource, "streams/My Source+Main/0");
    }

    #[test]
    fn huya_proxy_urls_bind_media_and_danmaku_resources() {
        let key = signing_key();
        let signing = signing_context(&key);
        let mode_name = "蓝光 HLS";
        let version = "huya v1";
        let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "蓝光".to_string(),
                format: "m3u8".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.to_string(),
                    media_index: 0,
                }),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Huya live danmaku".to_string(),
                format: Some("synctv-huya-live".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.to_string(),
                    media_index: 0,
                }),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode(mode_name, info),
            &codec(),
            Some(&signing),
        )
        .expect("Huya playback should convert");
        let info = &proto.playback_infos[mode_name];
        let media = &info.medias[0];
        let danmaku = &info.danmakus[0];

        assert_eq!(media.expire_at, Some(expires_at));

        assert!(media.url.starts_with(
            "/api/playback-providers/huya/huya%20v1/resources/%E8%93%9D%E5%85%89%20HLS/0?"
        ));
        key.parse_and_verify_query(
            signed_query(&media.url),
            synctv_core::provider::HuyaProvider::NAME,
            version,
            "resources/蓝光 HLS/0",
        )
        .expect("media signature should bind its decoded resource");

        assert!(danmaku.url.starts_with(
            "/api/playback-providers/huya/huya%20v1/danmakus/%E8%93%9D%E5%85%89%20HLS/0?"
        ));
        key.parse_and_verify_query(
            signed_query(&danmaku.url),
            synctv_core::provider::HuyaProvider::NAME,
            version,
            "danmakus/蓝光 HLS/0",
        )
        .expect("danmaku signature should bind its decoded resource");
    }

    #[test]
    fn proxy_media_exposes_the_earliest_upstream_or_signature_expiry() {
        let key = signing_key();
        let signing = signing_context(&key);
        let signed_expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
        let upstream_expires_at = signed_expires_at - 600;
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "Live".to_string(),
                format: "m3u8".to_string(),
                expire_at: chrono::DateTime::from_timestamp(upstream_expires_at, 0),
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Proxy {
                    version: "v1".to_string(),
                    expires_at: signed_expires_at,
                    mode_name: "main".to_string(),
                    media_index: 0,
                }),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("main", info),
            &codec(),
            Some(&signing),
        )
        .expect("Huya playback should convert");

        assert_eq!(
            proto.playback_infos["main"].medias[0].expire_at,
            Some(upstream_expires_at)
        );
    }

    #[test]
    fn douyu_proxy_urls_bind_media_and_danmaku_resources() {
        let key = signing_key();
        let signing = signing_context(&key);
        let mode_name = "original_tct_hevc";
        let version = "douyu v1";
        let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "Original".to_string(),
                format: "flv".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.to_string(),
                    media_index: 0,
                }),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Douyu live danmaku".to_string(),
                format: Some("synctv-douyu-live".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.to_string(),
                    media_index: 0,
                }),
            })
            .build();
        let proto = try_playback_to_proto(
            &playback_result_with_mode(mode_name, info),
            &codec(),
            Some(&signing),
        )
        .expect("Douyu playback should convert");
        let info = &proto.playback_infos[mode_name];
        assert_eq!(info.medias[0].expire_at, Some(expires_at));
        for (url, resource) in [
            (&info.medias[0].url, "resources/original_tct_hevc/0"),
            (&info.danmakus[0].url, "danmakus/original_tct_hevc/0"),
        ] {
            assert!(url.starts_with("/api/playback-providers/douyu/douyu%20v1/"));
            key.parse_and_verify_query(
                signed_query(url),
                synctv_core::provider::DouyuProvider::NAME,
                version,
                resource,
            )
            .expect("Douyu signature should bind its resource");
        }
    }

    #[test]
    fn live_proxy_url_uses_route_slug_and_internal_signature_provider() {
        let key = signing_key();
        let signing = signing_context(&key);
        let room_id = synctv_core::models::RoomId::new();
        let media_id = synctv_core::models::MediaId::new();
        let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "live".to_string(),
                format: "m3u8".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsMaster {
                    version: "live v1".to_string(),
                    expires_at,
                    room_id,
                    media_id,
                }),
            })
            .build();

        let proto = try_playback_to_proto(
            &playback_result_with_mode("hls", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let media = &proto.playback_infos["hls"].medias[0];

        assert_eq!(media.expire_at, Some(expires_at));

        assert!(
            media
                .url
                .starts_with("/api/playback-providers/live-proxy/live%20v1/hls-master?"),
            "unexpected live-proxy URL: {}",
            media.url
        );
        let claims = key
            .parse_and_verify_query(
                signed_query(&media.url),
                synctv_core::provider::LiveProxyProvider::NAME,
                "live v1",
                "hls-master",
            )
            .expect("signature should use internal provider name");
        assert_eq!(
            claims.provider,
            synctv_core::provider::LiveProxyProvider::NAME
        );
    }

    #[test]
    fn alist_playback_info_thumbnail_exposes_signed_proxy_url() {
        let key = signing_key();
        let signing = signing_context(&key);
        let expires_at = synctv_core::SystemClock.now().timestamp() + 1800;
        let mut result = playback_result_with_mode(
            "default",
            PlaybackInfo::builder()
                .thumbnail(Some("https://alist.example.com/thumb.jpg".to_string()))
                .add_media(PlaybackMedia {
                    name: "proxied".to_string(),
                    format: "mp4".to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: None,
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
        result.provider = synctv_core::models::SourceProvider::Alist;

        let proto = try_playback_to_proto(&result, &codec(), Some(&signing))
            .expect("playback should convert");
        let thumbnail = proto
            .playback_infos
            .get("default")
            .as_ref()
            .and_then(|info| info.thumbnail.as_deref())
            .expect("playback info thumbnail should exist");

        assert!(
            thumbnail.starts_with("/api/playback-providers/alist/v%201/thumbnail?"),
            "unexpected thumbnail URL: {thumbnail}"
        );
        let claims = key
            .parse_and_verify_query(signed_query(thumbnail), "alist", "v 1", "thumbnail")
            .expect("thumbnail signature should verify");
        assert_eq!(claims.resource, "thumbnail");
    }

    #[test]
    fn static_playback_exposes_server_approved_p2p_delivery() {
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "1080p".to_string(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: Some("sm3_static_media".to_string()),
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                    url: "https://cdn.example.com/movie.mp4?token=private".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .build();

        let key = signing_key();
        let signing = signing_context(&key);
        let proto = try_playback_to_proto(
            &playback_result_with_mode("direct", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let delivery = proto.playback_infos["direct"].medias[0]
            .p2p_delivery
            .as_ref()
            .expect("static media should expose P2P delivery");

        assert_eq!(delivery.swarm_id, "sm3_static_media");
        assert!(!delivery.swarm_ticket.is_empty());
    }

    #[test]
    fn live_playback_omits_p2p_delivery() {
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "live".to_string(),
                format: "hls".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                    url: "https://live.example.com/index.m3u8".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .build();
        let mut result = playback_result_with_mode("hls", info);
        result.playback_kind = synctv_core::models::PlaybackKind::Live;

        let key = signing_key();
        let signing = signing_context(&key);
        let proto = try_playback_to_proto(&result, &codec(), Some(&signing))
            .expect("playback should convert");

        assert!(proto.playback_infos["hls"].medias[0].p2p_delivery.is_none());
    }

    #[test]
    fn static_attachments_receive_independent_signed_deliveries() {
        let info = PlaybackInfo::builder()
            .add_media(PlaybackMedia {
                name: "1080p".to_string(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: Some("sm3_media".to_string()),
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                    url: "https://cdn.example.com/movie.mp4".to_string(),
                    headers: HashMap::new(),
                }),
            })
            .add_subtitle(PlaybackSubtitle {
                name: "Chinese".to_string(),
                language: "zh-CN".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: Some("sm3_subtitle".to_string()),
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                    url: "https://cdn.example.com/subtitle.vtt".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Bilibili danmaku".to_string(),
                format: Some("xml".to_string()),
                p2p_swarm_id: Some("sm3_danmaku".to_string()),
                provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileDirect {
                    url: "https://api.bilibili.com/x/v1/dm/list.so?oid=123".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            })
            .add_danmaku(PlaybackDanmaku {
                name: "Bilibili live danmaku".to_string(),
                format: Some("synctv-bilibili-live".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                    room_id: synctv_core::models::RoomId::new(),
                    media_id: synctv_core::models::MediaId::new(),
                }),
            })
            .build();
        let key = signing_key();
        let signing = signing_context(&key);
        let proto = try_playback_to_proto(
            &playback_result_with_mode("direct", info),
            &codec(),
            Some(&signing),
        )
        .expect("playback should convert");
        let info = &proto.playback_infos["direct"];
        let media = info.medias[0]
            .p2p_delivery
            .as_ref()
            .expect("static media should have delivery");
        let subtitle = info.subtitles[0]
            .p2p_delivery
            .as_ref()
            .expect("static subtitle should have delivery");
        let danmaku = info.danmakus[0]
            .p2p_delivery
            .as_ref()
            .expect("static danmaku should have delivery");

        assert_ne!(media.swarm_id, subtitle.swarm_id);
        assert_ne!(subtitle.swarm_id, danmaku.swarm_id);
        assert!(info.danmakus[1].p2p_delivery.is_none());
        for delivery in [media, subtitle, danmaku] {
            signing
                .media_swarm_signing_key
                .verify_media_swarm_ticket(
                    signing.room_id,
                    signing.actor_id,
                    &delivery.swarm_id,
                    &delivery.swarm_ticket,
                )
                .expect("attachment ticket should bind the current room, user, and swarm");
        }
    }
}
