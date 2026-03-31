//! Proto conversion helper functions

pub(super) const fn user_role_to_proto(role: synctv_core::models::UserRole) -> i32 {
    match role {
        synctv_core::models::UserRole::Root => synctv_proto::common::UserRole::Root as i32,
        synctv_core::models::UserRole::Admin => synctv_proto::common::UserRole::Admin as i32,
        synctv_core::models::UserRole::User => synctv_proto::common::UserRole::User as i32,
    }
}

pub(super) const fn user_status_to_proto(status: synctv_core::models::UserStatus) -> i32 {
    match status {
        synctv_core::models::UserStatus::Active => synctv_proto::common::UserStatus::Active as i32,
        synctv_core::models::UserStatus::Pending => {
            synctv_proto::common::UserStatus::Pending as i32
        }
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    }
}

pub fn proto_role_to_room_role(
    role_i32: i32,
) -> Result<synctv_core::models::RoomRole, crate::impls::ApiError> {
    match synctv_proto::common::RoomMemberRole::try_from(role_i32) {
        Ok(synctv_proto::common::RoomMemberRole::Creator) => {
            Ok(synctv_core::models::RoomRole::Creator)
        }
        Ok(synctv_proto::common::RoomMemberRole::Admin) => Ok(synctv_core::models::RoomRole::Admin),
        Ok(synctv_proto::common::RoomMemberRole::Member) => {
            Ok(synctv_core::models::RoomRole::Member)
        }
        Ok(synctv_proto::common::RoomMemberRole::Guest) => Ok(synctv_core::models::RoomRole::Guest),
        _ => Err(crate::impls::ApiError::InvalidInput(format!(
            "Unknown room member role: {role_i32}"
        ))),
    }
}

pub fn proto_role_to_user_role(
    role_i32: i32,
) -> Result<synctv_core::models::UserRole, crate::impls::ApiError> {
    match synctv_proto::common::UserRole::try_from(role_i32) {
        Ok(synctv_proto::common::UserRole::Root) => Ok(synctv_core::models::UserRole::Root),
        Ok(synctv_proto::common::UserRole::Admin) => Ok(synctv_core::models::UserRole::Admin),
        Ok(synctv_proto::common::UserRole::User) => Ok(synctv_core::models::UserRole::User),
        _ => Err(crate::impls::ApiError::InvalidInput(format!(
            "Unknown user role: {role_i32}"
        ))),
    }
}

#[must_use]
pub const fn room_role_to_proto(role: synctv_core::models::RoomRole) -> i32 {
    match role {
        synctv_core::models::RoomRole::Creator => {
            synctv_proto::common::RoomMemberRole::Creator as i32
        }
        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
        synctv_core::models::RoomRole::Member => {
            synctv_proto::common::RoomMemberRole::Member as i32
        }
        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
    }
}

pub(super) fn user_to_proto(user: &synctv_core::models::User) -> crate::proto::client::User {
    crate::proto::client::User {
        id: user.id.as_str().to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        role: user_role_to_proto(user.role),
        status: user_status_to_proto(user.status),
        created_at: user.created_at.timestamp(),
        email_verified: user.email_verified,
    }
}

pub(super) fn room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
) -> crate::proto::client::Room {
    let room_settings = settings.cloned().unwrap_or_default();
    crate::proto::client::Room {
        id: room.id.as_str().to_string(),
        name: room.name.clone(),
        description: room.description.clone(),
        created_by: room.created_by.as_str().to_string(),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: serde_json::to_vec(&room_settings).unwrap_or_default(),
        created_at: room.created_at.timestamp(),
        member_count: member_count.unwrap_or(0),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
    }
}

#[must_use]
pub(super) fn hot_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    online_count: i32,
    total_members: i32,
) -> crate::proto::client::RoomWithStats {
    crate::proto::client::RoomWithStats {
        room: Some(room_to_proto_basic(room, settings, Some(online_count))),
        online_count,
        total_members,
    }
}

#[must_use]
pub fn media_to_proto(media: &synctv_core::models::Media) -> crate::proto::client::Media {
    // Extract metadata from source_config if present (any provider may store it)
    let metadata_bytes = media
        .source_config
        .get("metadata")
        .map(|m| serde_json::to_vec(m).unwrap_or_default())
        .unwrap_or_default();

    // Strip credentials from source_config before sending to clients
    let sanitized_config =
        synctv_core::provider::strip_source_config_credentials(&media.source_config);

    crate::proto::client::Media {
        id: media.id.as_str().to_string(),
        room_id: media.room_id.as_str().to_string(),
        provider: media.source_provider.clone(),
        title: media.name.clone(),
        metadata: metadata_bytes,
        position: media.position,
        added_at: media.added_at.timestamp(),
        added_by: media
            .creator_id
            .as_ref()
            .map_or(String::new(), |id| id.as_str().to_string()),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: serde_json::to_vec(&sanitized_config).unwrap_or_default(),
    }
}

pub(super) fn playlist_to_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
) -> crate::proto::client::Playlist {
    crate::proto::client::Playlist {
        id: playlist.id.as_str().to_string(),
        room_id: playlist.room_id.as_str().to_string(),
        name: playlist.name.clone(),
        parent_id: playlist
            .parent_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        position: playlist.position,
        is_folder: true, // All playlists are containers for media items
        is_dynamic: playlist.is_dynamic(),
        item_count,
        created_at: playlist.created_at.timestamp(),
        updated_at: playlist.updated_at.timestamp(),
    }
}

pub(super) fn playback_state_to_proto(
    state: &synctv_core::models::RoomPlaybackState,
) -> crate::proto::client::PlaybackState {
    crate::proto::client::PlaybackState {
        room_id: state.room_id.as_str().to_string(),
        playing_media_id: state
            .playing_media_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        current_time: state.computed_current_time(),
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version,
        playing_playlist_id: state
            .playing_playlist_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        relative_path: state.relative_path.clone(),
    }
}

pub(super) fn room_member_to_proto(
    member: synctv_core::models::RoomMemberWithUser,
    role_default: synctv_core::models::PermissionBits,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: member.room_id.as_str().to_string(),
        user_id: member.user_id.as_str().to_string(),
        username: member.username.clone(),
        role: room_role_to_proto(member.role),
        permissions: member.effective_permissions(role_default).0,
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
    }
}

/// Convert a list of `RoomMemberWithUser` into proto `RoomMember` messages,
/// applying the three-layer permission calculation for each member using
/// the given room settings.
///
/// This eliminates the duplicated pattern of:
///   1. Fetch room settings
///   2. For each member: `calculate_role_default_permissions` + `room_member_to_proto`
pub(super) fn members_to_proto(
    members: Vec<synctv_core::models::RoomMemberWithUser>,
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
) -> Vec<synctv_proto::common::RoomMember> {
    members
        .into_iter()
        .map(|m| {
            let role_default =
                permission_service.calculate_role_default_permissions(&m.role, room_settings);
            room_member_to_proto(m, role_default)
        })
        .collect()
}

/// Convert provider `PlaybackInfo` to models `PlaybackInfo`
#[must_use]
pub fn provider_playback_info_to_model(
    info: &synctv_core::provider::traits::PlaybackInfo,
) -> synctv_core::models::media::PlaybackInfo {
    use synctv_core::models::media::{PlaybackInfo, PlaybackUrl, Subtitle, SubtitleUrl};

    let urls = info
        .urls
        .iter()
        .map(|url| PlaybackUrl::simple(String::new(), url.clone()))
        .collect();

    let subtitles = info
        .subtitles
        .iter()
        .map(|sub| {
            let url = SubtitleUrl {
                name: String::new(),
                url: sub.url.clone(),
                headers: std::collections::HashMap::new(),
                format: String::new(),
            };
            Subtitle {
                name: sub.name.clone(),
                language: sub.language.clone(),
                urls: vec![url],
                default_url_index: 0,
            }
        })
        .collect();

    PlaybackInfo {
        urls,
        default_url_index: 0,
        subtitles,
        default_subtitle_index: None,
        danmakus: Vec::new(),
        format: String::new(),
    }
}

/// Convert models `PlaybackResult` to proto `PlaybackResult`
#[must_use]
pub(super) fn playback_result_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
) -> crate::proto::client::PlaybackResult {
    let playback_infos = result
        .playback_infos
        .iter()
        .map(|(mode, info)| (mode.clone(), playback_info_to_proto(info)))
        .collect();

    let metadata = result
        .metadata
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_string(v).unwrap_or_default()))
        .collect();

    crate::proto::client::PlaybackResult {
        media_id: result
            .id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        playlist_id: result
            .playlist_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        room_id: result.room_id.as_str().to_string(),
        name: result.name.clone(),
        position: result.position,
        playback_infos,
        default_mode: result.default_mode.clone(),
        metadata,
    }
}

/// Convert models `PlaybackInfo` to proto `PlaybackInfo`
fn playback_info_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
) -> crate::proto::client::PlaybackInfo {
    crate::proto::client::PlaybackInfo {
        urls: info.urls.iter().map(playback_url_to_proto).collect(),
        default_url_index: info.default_url_index as i32,
        subtitles: info.subtitles.iter().map(subtitle_to_proto).collect(),
        default_subtitle_index: info.default_subtitle_index.map(|idx| idx as i32),
        danmakus: info.danmakus.iter().map(danmaku_to_proto).collect(),
        format: info.format.clone(),
    }
}

/// Convert models `PlaybackUrl` to proto `PlaybackUrl`
fn playback_url_to_proto(
    url: &synctv_core::models::media::PlaybackUrl,
) -> crate::proto::client::PlaybackUrl {
    crate::proto::client::PlaybackUrl {
        name: url.name.clone(),
        url: url.url.clone(),
        headers: url.headers.clone(),
        expire_at: url.expire_at.map(|dt| dt.timestamp()),
        metadata: url.metadata.as_ref().map(playback_url_metadata_to_proto),
    }
}

/// Convert models `PlaybackUrlMetadata` to proto `PlaybackUrlMetadata`
fn playback_url_metadata_to_proto(
    metadata: &synctv_core::models::media::PlaybackUrlMetadata,
) -> crate::proto::client::PlaybackUrlMetadata {
    let extra = metadata
        .extra
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_string(v).unwrap_or_default()))
        .collect();

    crate::proto::client::PlaybackUrlMetadata {
        resolution: metadata.resolution.clone(),
        bitrate: metadata.bitrate,
        codec: metadata.codec.clone(),
        fps: metadata.fps,
        extra,
    }
}

/// Convert models Subtitle to proto Subtitle
fn subtitle_to_proto(
    subtitle: &synctv_core::models::media::Subtitle,
) -> crate::proto::client::Subtitle {
    crate::proto::client::Subtitle {
        name: subtitle.name.clone(),
        language: subtitle.language.clone(),
        urls: subtitle.urls.iter().map(subtitle_url_to_proto).collect(),
        default_url_index: subtitle.default_url_index as i32,
    }
}

/// Convert models `SubtitleUrl` to proto `SubtitleUrl`
fn subtitle_url_to_proto(
    url: &synctv_core::models::media::SubtitleUrl,
) -> crate::proto::client::SubtitleUrl {
    crate::proto::client::SubtitleUrl {
        name: url.name.clone(),
        url: url.url.clone(),
        headers: url.headers.clone(),
        format: url.format.clone(),
    }
}

/// Convert models Danmaku to proto Danmaku
fn danmaku_to_proto(
    danmaku: &synctv_core::models::media::Danmaku,
) -> crate::proto::client::Danmaku {
    crate::proto::client::Danmaku {
        name: danmaku.name.clone(),
        url: danmaku.url.clone(),
        format: danmaku.format.clone(),
        headers: danmaku.headers.clone(),
    }
}
