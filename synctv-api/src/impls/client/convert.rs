//! Proto conversion helper functions
use rayon::prelude::*;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::Serialize;

use synctv_core::service::room::ClientResourceAvailability;

const PARALLEL_PROTO_MAP_THRESHOLD: usize = 128;
const REDACTED_SOURCE_CONFIG_VALUE: &str = "[REDACTED]";
const SOURCE_CONFIG_CREDENTIAL_FIELDS: &[&str] = &[
    "token",
    "api_key",
    "password",
    "cookies",
    "secret",
    "access_token",
];

fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

pub(crate) fn map_slice_preserve_order<T, U, F>(items: &[T], map: F) -> Vec<U>
where
    T: Sync,
    U: Send,
    F: Fn(&T) -> U + Sync + Send,
{
    items
        .par_iter()
        .with_min_len(PARALLEL_PROTO_MAP_THRESHOLD)
        .map(&map)
        .collect()
}

struct SanitizedSourceConfig<'a>(&'a serde_json::Value);

impl Serialize for SanitizedSourceConfig<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            serde_json::Value::Null => serializer.serialize_unit(),
            serde_json::Value::Bool(value) => serializer.serialize_bool(*value),
            serde_json::Value::Number(value) => value.serialize(serializer),
            serde_json::Value::String(value) => serializer.serialize_str(value),
            serde_json::Value::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&SanitizedSourceConfig(value))?;
                }
                seq.end()
            }
            serde_json::Value::Object(values) => {
                let mut map = serializer.serialize_map(Some(values.len()))?;
                for (key, value) in values {
                    map.serialize_key(key)?;
                    if SOURCE_CONFIG_CREDENTIAL_FIELDS.contains(&key.as_str()) {
                        map.serialize_value(REDACTED_SOURCE_CONFIG_VALUE)?;
                    } else {
                        map.serialize_value(&SanitizedSourceConfig(value))?;
                    }
                }
                map.end()
            }
        }
    }
}

fn serialize_sanitized_source_config(source_config: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&SanitizedSourceConfig(source_config)).unwrap_or_default()
}

pub(super) const fn user_role_to_proto(role: synctv_core::models::UserRole) -> i32 {
    match role {
        synctv_core::models::UserRole::Root => synctv_proto::common::UserRole::Root as i32,
        synctv_core::models::UserRole::Admin => synctv_proto::common::UserRole::Admin as i32,
        synctv_core::models::UserRole::User => synctv_proto::common::UserRole::User as i32,
    }
}

pub(crate) const fn user_status_to_proto(status: synctv_core::models::UserStatus) -> i32 {
    match status {
        synctv_core::models::UserStatus::Active => synctv_proto::common::UserStatus::Active as i32,
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    }
}

pub(crate) const fn member_status_to_proto(status: synctv_core::models::MemberStatus) -> i32 {
    match status {
        synctv_core::models::MemberStatus::Active => {
            synctv_proto::common::MemberStatus::Active as i32
        }
        synctv_core::models::MemberStatus::Left => synctv_proto::common::MemberStatus::Left as i32,
    }
}

pub(crate) const fn resource_availability_to_proto(is_available: bool) -> i32 {
    if is_available {
        crate::proto::client::ResourceAvailability::Available as i32
    } else {
        crate::proto::client::ResourceAvailability::CreatorInactive as i32
    }
}

pub(crate) const fn resource_availability_enum_to_proto(
    availability: ClientResourceAvailability,
) -> i32 {
    match availability {
        ClientResourceAvailability::Available => {
            crate::proto::client::ResourceAvailability::Available as i32
        }
        ClientResourceAvailability::CreatorInactive => {
            crate::proto::client::ResourceAvailability::CreatorInactive as i32
        }
    }
}

#[must_use]
pub(crate) fn playback_client_profile_from_proto(
    profile: Option<&crate::proto::client::PlaybackClientProfile>,
) -> Option<synctv_core::provider::PlaybackClientProfile> {
    profile.map(|profile| {
        let default_profile = synctv_core::provider::PlaybackClientProfile::default();
        synctv_core::provider::PlaybackClientProfile {
            delivery_preference: match crate::proto::client::PlaybackDeliveryPreference::try_from(
                profile.delivery_preference,
            )
            .unwrap_or(crate::proto::client::PlaybackDeliveryPreference::Unspecified)
            {
                crate::proto::client::PlaybackDeliveryPreference::Unspecified
                | crate::proto::client::PlaybackDeliveryPreference::Auto => {
                    synctv_core::provider::PlaybackDeliveryPreference::Auto
                }
                crate::proto::client::PlaybackDeliveryPreference::DirectPlay => {
                    synctv_core::provider::PlaybackDeliveryPreference::DirectPlay
                }
                crate::proto::client::PlaybackDeliveryPreference::Transcode => {
                    synctv_core::provider::PlaybackDeliveryPreference::Transcode
                }
            },
            max_streaming_bitrate: profile.max_streaming_bitrate,
            max_audio_channels: profile.max_audio_channels,
            supported_video_codecs: if profile.supported_video_codecs.is_empty() {
                default_profile.supported_video_codecs.clone()
            } else {
                profile
                    .supported_video_codecs
                    .iter()
                    .filter_map(|codec| {
                        match crate::proto::client::PlaybackVideoCodec::try_from(*codec)
                            .unwrap_or(crate::proto::client::PlaybackVideoCodec::Unspecified)
                        {
                            crate::proto::client::PlaybackVideoCodec::Unspecified => None,
                            crate::proto::client::PlaybackVideoCodec::H264 => {
                                Some(synctv_core::provider::PlaybackVideoCodec::H264)
                            }
                            crate::proto::client::PlaybackVideoCodec::Hevc => {
                                Some(synctv_core::provider::PlaybackVideoCodec::Hevc)
                            }
                            crate::proto::client::PlaybackVideoCodec::Vp9 => {
                                Some(synctv_core::provider::PlaybackVideoCodec::Vp9)
                            }
                            crate::proto::client::PlaybackVideoCodec::Av1 => {
                                Some(synctv_core::provider::PlaybackVideoCodec::Av1)
                            }
                        }
                    })
                    .collect()
            },
            supported_containers: if profile.supported_containers.is_empty() {
                default_profile.supported_containers.clone()
            } else {
                profile
                    .supported_containers
                    .iter()
                    .filter_map(
                        |container| match crate::proto::client::PlaybackContainer::try_from(
                            *container,
                        )
                        .unwrap_or(crate::proto::client::PlaybackContainer::Unspecified)
                        {
                            crate::proto::client::PlaybackContainer::Unspecified => None,
                            crate::proto::client::PlaybackContainer::Mp4 => {
                                Some(synctv_core::provider::PlaybackContainer::Mp4)
                            }
                            crate::proto::client::PlaybackContainer::Mkv => {
                                Some(synctv_core::provider::PlaybackContainer::Mkv)
                            }
                            crate::proto::client::PlaybackContainer::Webm => {
                                Some(synctv_core::provider::PlaybackContainer::Webm)
                            }
                        },
                    )
                    .collect()
            },
            audio_capability: match crate::proto::client::PlaybackAudioCapability::try_from(
                profile.audio_capability,
            )
            .unwrap_or(crate::proto::client::PlaybackAudioCapability::Unspecified)
            {
                crate::proto::client::PlaybackAudioCapability::Unspecified => {
                    default_profile.audio_capability
                }
                crate::proto::client::PlaybackAudioCapability::Stereo => {
                    synctv_core::provider::PlaybackAudioCapability::Stereo
                }
                crate::proto::client::PlaybackAudioCapability::Surround => {
                    synctv_core::provider::PlaybackAudioCapability::Surround
                }
                crate::proto::client::PlaybackAudioCapability::LosslessSurround => {
                    synctv_core::provider::PlaybackAudioCapability::LosslessSurround
                }
            },
            subtitle_preference: match crate::proto::client::PlaybackSubtitlePreference::try_from(
                profile.subtitle_preference,
            )
            .unwrap_or(crate::proto::client::PlaybackSubtitlePreference::Unspecified)
            {
                crate::proto::client::PlaybackSubtitlePreference::Unspecified
                | crate::proto::client::PlaybackSubtitlePreference::External => {
                    synctv_core::provider::PlaybackSubtitlePreference::External
                }
                crate::proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal => {
                    synctv_core::provider::PlaybackSubtitlePreference::EmbeddedOrExternal
                }
                crate::proto::client::PlaybackSubtitlePreference::None => {
                    synctv_core::provider::PlaybackSubtitlePreference::None
                }
            },
        }
    })
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

pub(crate) fn user_to_proto(
    user: &synctv_core::models::User,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::User {
    crate::proto::client::User {
        id: public_id_codec
            .encode_user_id(user.id)
            .expect("positive user ID must encode"),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        role: user_role_to_proto(user.role),
        status: user_status_to_proto(user.status),
        created_at: user.created_at.timestamp(),
        email_verified: user.email_verified,
        is_banned: user.is_banned,
    }
}

pub(crate) fn room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Room {
    room_to_proto_with_availability(
        room,
        settings,
        member_count,
        ClientResourceAvailability::Available,
        public_id_codec,
    )
}

pub(crate) fn room_to_proto_with_availability(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Room {
    let room_settings = settings.cloned().unwrap_or_default();
    crate::proto::client::Room {
        id: public_id_codec
            .encode_room_id(room.id)
            .expect("positive room ID must encode"),
        name: room.name.clone(),
        description: room.description.clone(),
        created_by: public_id_codec
            .encode_user_id(room.created_by)
            .expect("positive user ID must encode"),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: serde_json::to_vec(&room_settings).unwrap_or_default(),
        created_at: room.created_at.timestamp(),
        member_count: member_count.unwrap_or(0),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        availability: resource_availability_enum_to_proto(availability),
        version: i64::from(room.version),
    }
}

#[cfg(test)]
#[must_use]
pub(super) fn hot_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    online_count: i32,
    total_members: i32,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::RoomWithStats {
    crate::proto::client::RoomWithStats {
        room: Some(room_to_proto_basic(
            room,
            settings,
            Some(total_members),
            public_id_codec,
        )),
        online_count,
        total_members,
    }
}

#[must_use]
pub(crate) fn normalize_created_room_settings(
    settings: Option<&synctv_core::models::RoomSettings>,
    has_password: bool,
) -> synctv_core::models::RoomSettings {
    let mut room_settings = settings.cloned().unwrap_or_default();
    room_settings.require_password =
        synctv_core::models::room_settings::RequirePassword(has_password);
    room_settings
}

#[must_use]
pub fn media_to_proto(
    media: &synctv_core::models::Media,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Media {
    media_to_proto_with_availability(media, true, public_id_codec)
}

pub fn media_to_proto_with_availability(
    media: &synctv_core::models::Media,
    is_available: bool,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Media {
    // Extract metadata from source_config if present (any provider may store it)
    let metadata_bytes = media
        .source_config
        .get("metadata")
        .map(|m| serde_json::to_vec(m).unwrap_or_default())
        .unwrap_or_default();

    crate::proto::client::Media {
        id: public_id_codec
            .encode_media_id(media.id)
            .expect("positive media ID must encode"),
        room_id: public_id_codec
            .encode_room_id(media.room_id)
            .expect("positive room ID must encode"),
        provider: media.source_provider.clone(),
        title: media.name.clone(),
        metadata: metadata_bytes,
        position: media.position,
        added_at: media.added_at.timestamp(),
        added_by: media.creator_id.as_ref().map_or_else(String::new, |id| {
            public_id_codec
                .encode_user_id(*id)
                .expect("positive user ID must encode")
        }),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: serialize_sanitized_source_config(&media.source_config),
        availability: resource_availability_to_proto(is_available),
        version: i64::from(media.version),
    }
}

pub(crate) fn playlist_to_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Playlist {
    playlist_to_proto_with_availability(playlist, item_count, true, public_id_codec)
}

pub(crate) fn playlist_to_proto_with_availability(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::Playlist {
    crate::proto::client::Playlist {
        id: public_id_codec
            .encode_playlist_id(playlist.id)
            .expect("positive playlist ID must encode"),
        room_id: public_id_codec
            .encode_room_id(playlist.room_id)
            .expect("positive room ID must encode"),
        name: playlist.name.clone(),
        parent_id: playlist.parent_id.as_ref().map_or_else(String::new, |id| {
            public_id_codec
                .encode_playlist_id(*id)
                .expect("positive playlist ID must encode")
        }),
        position: playlist.position,
        is_dynamic: playlist.is_dynamic(),
        item_count,
        created_at: playlist.created_at.timestamp(),
        updated_at: playlist.updated_at.timestamp(),
        availability: resource_availability_to_proto(is_available),
        version: i64::from(playlist.version),
    }
}

pub(crate) fn playlist_path_node_to_proto(
    playlist: &synctv_core::models::Playlist,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::PlaylistBrowsePathNode {
    crate::proto::client::PlaylistBrowsePathNode {
        playlist_id: public_id_codec
            .encode_playlist_id(playlist.id)
            .expect("positive playlist ID must encode"),
        name: playlist.name.clone(),
        target: Vec::new(),
    }
}

pub(crate) fn playback_state_to_proto(
    state: &synctv_core::models::RoomPlaybackState,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::PlaybackState {
    crate::proto::client::PlaybackState {
        room_id: public_id_codec
            .encode_room_id(state.room_id)
            .expect("positive room ID must encode"),
        playing_media_id: state
            .playing_media_id
            .as_ref()
            .map_or_else(String::new, |id| {
                public_id_codec
                    .encode_media_id(*id)
                    .expect("positive media ID must encode")
            }),
        current_time: state.computed_current_time(),
        speed: state.speed,
        is_playing: state.is_playing,
        updated_at: state.updated_at.timestamp(),
        version: state.version,
        playing_playlist_id: state
            .playing_playlist_id
            .as_ref()
            .map_or_else(String::new, |id| {
                public_id_codec
                    .encode_playlist_id(*id)
                    .expect("positive playlist ID must encode")
            }),
        target: state.target.clone(),
    }
}

pub(super) fn room_member_to_proto(
    member: &synctv_core::models::RoomMemberWithUser,
    role_default: synctv_core::models::PermissionBits,
    public_id_codec: &crate::PublicIdCodec,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: public_id_codec
            .encode_room_id(member.room_id)
            .expect("positive room ID must encode"),
        user_id: public_id_codec
            .encode_user_id(member.user_id)
            .expect("positive user ID must encode"),
        username: member.username.clone(),
        role: room_role_to_proto(member.role),
        permissions: member.effective_permissions(role_default).0,
        status: member_status_to_proto(member.status),
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
        is_banned: member.is_banned,
        banned_at: member.banned_at.map_or(0, |value| value.timestamp()),
        banned_reason: member.banned_reason.clone().unwrap_or_default(),
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
    members: &[synctv_core::models::RoomMemberWithUser],
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
    public_id_codec: &crate::PublicIdCodec,
) -> Vec<synctv_proto::common::RoomMember> {
    map_slice_preserve_order(members, |m| {
        let role_default =
            permission_service.calculate_role_default_permissions(&m.role, room_settings);
        room_member_to_proto(m, role_default, public_id_codec)
    })
}

pub(crate) fn media_list_to_proto(
    media: &[synctv_core::models::Media],
    public_id_codec: &crate::PublicIdCodec,
) -> Vec<crate::proto::client::Media> {
    map_slice_preserve_order(media, |media| media_to_proto(media, public_id_codec))
}

pub(crate) fn media_list_to_proto_with_availability<T, F>(
    items: &[T],
    map: F,
) -> Vec<crate::proto::client::Media>
where
    T: Sync,
    F: Fn(&T) -> crate::proto::client::Media + Sync + Send,
{
    map_slice_preserve_order(items, map)
}

pub(crate) fn playlist_list_to_proto<T, F>(
    items: &[T],
    map: F,
) -> Vec<crate::proto::client::Playlist>
where
    T: Sync,
    F: Fn(&T) -> crate::proto::client::Playlist + Sync + Send,
{
    map_slice_preserve_order(items, map)
}

/// Convert provider `PlaybackInfo` to models `PlaybackInfo`
#[must_use]
pub(crate) fn provider_playback_info_to_model(
    info: &synctv_core::provider::traits::PlaybackInfo,
) -> synctv_core::models::media::PlaybackInfo {
    use synctv_core::models::media::{PlaybackInfo, PlaybackUrl, Subtitle, SubtitleUrl};

    let urls = map_slice_preserve_order(&info.urls, |url| PlaybackUrl {
        name: String::new(),
        url: url.clone(),
        headers: info.headers.clone(),
        expire_at: info
            .expires_at
            .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
    });

    let subtitles = map_slice_preserve_order(&info.subtitles, |sub| {
        let url = SubtitleUrl {
            name: String::new(),
            url: sub.url.clone(),
            headers: sub.headers.clone(),
            format: sub.format.clone(),
        };
        Subtitle {
            name: sub.name.clone(),
            language: sub.language.clone(),
            urls: vec![url],
            default_url_index: 0,
        }
    });

    PlaybackInfo {
        urls,
        default_url_index: 0,
        subtitles,
        default_subtitle_index: None,
        danmakus: Vec::new(),
        format: info.format.clone(),
    }
}

pub(crate) fn direct_url_embedded_playback_result_to_model(
    media: &synctv_core::models::Media,
) -> Result<Option<synctv_core::models::media::PlaybackResult>, crate::impls::ApiError> {
    if media.source_provider.trim() != synctv_core::provider::DirectUrlProvider::NAME {
        return Ok(None);
    }

    let Some(playback_infos_value) = media.source_config.get("playback_infos") else {
        return Ok(None);
    };

    let playback_infos: std::collections::HashMap<
        String,
        synctv_core::models::media::PlaybackInfo,
    > = serde_json::from_value(playback_infos_value.clone()).map_err(|error| {
        crate::impls::ApiError::Internal(format!(
            "Failed to parse embedded DirectUrl playback_infos: {error}"
        ))
    })?;
    let default_mode = media
        .source_config
        .get("default_mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            crate::impls::ApiError::Internal(
                "Embedded DirectUrl playback result missing default_mode".to_string(),
            )
        })?
        .to_string();

    if !playback_infos.contains_key(&default_mode) {
        return Err(crate::impls::ApiError::Internal(format!(
            "Embedded DirectUrl playback result default_mode '{default_mode}' is missing from playback_infos"
        )));
    }

    let metadata = media
        .source_config
        .get("metadata")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            crate::impls::ApiError::Internal(format!(
                "Failed to parse embedded DirectUrl metadata: {error}"
            ))
        })?
        .unwrap_or_default();

    Ok(Some(synctv_core::models::media::PlaybackResult {
        id: Some(media.id),
        playlist_id: media.playlist_id,
        room_id: media.room_id,
        name: media.name.clone(),
        position: media.position,
        playback_infos,
        default_mode,
        metadata,
    }))
}

#[must_use]
pub(crate) fn bilibili_live_danmaku_for_static_media(
    media: &synctv_core::models::Media,
    user_id: &str,
    public_id_codec: &crate::PublicIdCodec,
    signing_key: Option<&synctv_core::service::ProxySigningKey>,
    expires_at: Option<i64>,
) -> Option<synctv_core::models::media::Danmaku> {
    let signing_key = signing_key?;
    if media.source_provider.trim() != synctv_core::provider::BilibiliProvider::NAME {
        return None;
    }
    if media
        .source_config
        .get("type")
        .and_then(serde_json::Value::as_str)
        != Some("live")
    {
        return None;
    }

    let expires_at = expires_at.unwrap_or_else(|| {
        chrono::Utc::now().timestamp()
            + synctv_core::service::ProxySigningKey::default_expiry_secs()
    });
    let room_id = public_id_codec
        .encode_room_id(media.room_id)
        .expect("positive room id must encode as public ID");
    let media_id = public_id_codec
        .encode_media_id(media.id)
        .expect("positive media id must encode as public ID");
    let url = synctv_core::service::proxy_signature::build_signed_proxy_url(
        synctv_core::provider::BilibiliProvider::NAME,
        &room_id,
        &format!("{media_id}/danmu"),
        signing_key,
        &room_id,
        user_id,
        expires_at,
    );

    Some(synctv_core::models::media::Danmaku {
        name: "Bilibili Danmaku".to_string(),
        url,
        format: Some("bilibili".to_string()),
        headers: std::collections::HashMap::new(),
    })
}

pub(crate) fn sign_local_bilibili_danmaku_urls(
    result: &mut synctv_core::models::media::PlaybackResult,
    user_id: &str,
    signing_key: Option<&synctv_core::service::ProxySigningKey>,
    expires_at: Option<i64>,
) {
    let Some(signing_key) = signing_key else {
        return;
    };
    let expires_at = expires_at.unwrap_or_else(|| {
        chrono::Utc::now().timestamp()
            + synctv_core::service::ProxySigningKey::default_expiry_secs()
    });

    for info in result.playback_infos.values_mut() {
        for danmaku in &mut info.danmakus {
            if danmaku.url.contains('?') {
                continue;
            }
            let Some(sub_path) = danmaku.url.strip_prefix("/api/providers/proxy/bilibili/") else {
                continue;
            };
            let Some((room_id, action)) = sub_path.split_once('/') else {
                continue;
            };
            if !action.ends_with("/danmu") {
                continue;
            }

            danmaku.url = synctv_core::service::proxy_signature::build_signed_proxy_url(
                synctv_core::provider::BilibiliProvider::NAME,
                room_id,
                action,
                signing_key,
                room_id,
                user_id,
                expires_at,
            );
        }
    }
}

/// Convert models `PlaybackResult` to proto `PlaybackSnapshot`
#[must_use]
pub(crate) fn playback_snapshot_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
    public_id_codec: &crate::PublicIdCodec,
) -> crate::proto::client::PlaybackSnapshot {
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

    crate::proto::client::PlaybackSnapshot {
        media_id: result
            .id
            .as_ref()
            .map(|id| {
                public_id_codec
                    .encode_media_id(*id)
                    .expect("positive media ID must encode")
            })
            .unwrap_or_default(),
        playlist_id: result
            .playlist_id
            .as_ref()
            .map(|id| {
                public_id_codec
                    .encode_playlist_id(*id)
                    .expect("positive playlist ID must encode")
            })
            .unwrap_or_default(),
        room_id: public_id_codec
            .encode_room_id(result.room_id)
            .expect("positive room ID must encode"),
        name: result.name.clone(),
        position: result.position,
        playback_infos,
        default_mode: result.default_mode.clone(),
        metadata,
        version: String::new(),
        expires_at: None,
    }
}

/// Convert models `PlaybackInfo` to proto `PlaybackInfo`
fn playback_info_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
) -> crate::proto::client::PlaybackInfo {
    crate::proto::client::PlaybackInfo {
        urls: map_slice_preserve_order(&info.urls, playback_url_to_proto),
        default_url_index: usize_to_i32_saturating(info.default_url_index),
        subtitles: map_slice_preserve_order(&info.subtitles, subtitle_to_proto),
        default_subtitle_index: info.default_subtitle_index.map(usize_to_i32_saturating),
        danmakus: map_slice_preserve_order(&info.danmakus, danmaku_to_proto),
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
        urls: map_slice_preserve_order(&subtitle.urls, subtitle_url_to_proto),
        default_url_index: usize_to_i32_saturating(subtitle.default_url_index),
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

#[cfg(test)]
mod tests {
    use super::{
        bilibili_live_danmaku_for_static_media, direct_url_embedded_playback_result_to_model,
        media_to_proto, normalize_created_room_settings, playback_client_profile_from_proto,
        playlist_to_proto, provider_playback_info_to_model, room_to_proto_basic,
        sign_local_bilibili_danmaku_urls, REDACTED_SOURCE_CONFIG_VALUE,
    };
    use std::collections::HashMap;
    use synctv_core::models::{Media, MediaId, PlaylistId, Room, RoomId, UserId};

    #[test]
    fn provider_playback_info_to_model_preserves_transport_fields() {
        let info = synctv_core::provider::traits::PlaybackInfo {
            urls: vec!["https://cdn.example.com/video.mpd".to_string()],
            format: "mpd".to_string(),
            headers: HashMap::from([
                ("Authorization".to_string(), "Bearer token".to_string()),
                ("User-Agent".to_string(), "SyncTVNative/1.0".to_string()),
            ]),
            subtitles: vec![synctv_core::provider::traits::SubtitleTrack {
                language: "zh-CN".to_string(),
                name: "Chinese".to_string(),
                url: "https://cdn.example.com/subtitle.ass".to_string(),
                headers: HashMap::from([(
                    "X-Subtitle-Token".to_string(),
                    "subtitle-header".to_string(),
                )]),
                format: "ass".to_string(),
            }],
            expires_at: Some(1_700_000_000),
            cors_proxy_required: false,
        };

        let converted = provider_playback_info_to_model(&info);

        assert_eq!(converted.format, "mpd");
        assert_eq!(
            converted.urls[0]
                .headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(
            converted.urls[0]
                .headers
                .get("User-Agent")
                .map(String::as_str),
            Some("SyncTVNative/1.0")
        );
        assert_eq!(
            converted.urls[0].expire_at.map(|dt| dt.timestamp()),
            Some(1_700_000_000)
        );
        assert_eq!(converted.subtitles[0].urls[0].format, "ass");
        assert_eq!(
            converted.subtitles[0].urls[0]
                .headers
                .get("X-Subtitle-Token")
                .map(String::as_str),
            Some("subtitle-header")
        );
    }

    #[test]
    fn playback_client_profile_from_proto_returns_none_when_absent() {
        assert_eq!(playback_client_profile_from_proto(None), None);
    }

    #[test]
    fn playback_client_profile_from_proto_applies_defaults_for_omitted_repeated_capabilities() {
        let proto = crate::proto::client::PlaybackClientProfile {
            delivery_preference: crate::proto::client::PlaybackDeliveryPreference::Unspecified
                as i32,
            max_streaming_bitrate: Some(12_000_000),
            max_audio_channels: Some(2),
            supported_video_codecs: Vec::new(),
            supported_containers: Vec::new(),
            audio_capability: crate::proto::client::PlaybackAudioCapability::Unspecified as i32,
            subtitle_preference: crate::proto::client::PlaybackSubtitlePreference::Unspecified
                as i32,
        };

        let converted = playback_client_profile_from_proto(Some(&proto))
            .expect("present proto profile should convert");
        let default_profile = synctv_core::provider::PlaybackClientProfile::default();

        assert_eq!(
            converted.delivery_preference,
            synctv_core::provider::PlaybackDeliveryPreference::Auto
        );
        assert_eq!(converted.max_streaming_bitrate, Some(12_000_000));
        assert_eq!(converted.max_audio_channels, Some(2));
        assert_eq!(
            converted.supported_video_codecs,
            default_profile.supported_video_codecs
        );
        assert_eq!(
            converted.supported_containers,
            default_profile.supported_containers
        );
        assert_eq!(converted.audio_capability, default_profile.audio_capability);
        assert_eq!(
            converted.subtitle_preference,
            synctv_core::provider::PlaybackSubtitlePreference::External
        );
    }

    #[test]
    fn playback_client_profile_from_proto_maps_explicit_capabilities() {
        let proto = crate::proto::client::PlaybackClientProfile {
            delivery_preference: crate::proto::client::PlaybackDeliveryPreference::DirectPlay
                as i32,
            max_streaming_bitrate: None,
            max_audio_channels: Some(6),
            supported_video_codecs: vec![
                crate::proto::client::PlaybackVideoCodec::H264 as i32,
                crate::proto::client::PlaybackVideoCodec::Vp9 as i32,
            ],
            supported_containers: vec![
                crate::proto::client::PlaybackContainer::Mp4 as i32,
                crate::proto::client::PlaybackContainer::Webm as i32,
            ],
            audio_capability: crate::proto::client::PlaybackAudioCapability::Surround as i32,
            subtitle_preference: crate::proto::client::PlaybackSubtitlePreference::None as i32,
        };

        let converted = playback_client_profile_from_proto(Some(&proto))
            .expect("present proto profile should convert");

        assert_eq!(
            converted.delivery_preference,
            synctv_core::provider::PlaybackDeliveryPreference::DirectPlay
        );
        assert_eq!(converted.max_streaming_bitrate, None);
        assert_eq!(converted.max_audio_channels, Some(6));
        assert_eq!(
            converted.supported_video_codecs,
            vec![
                synctv_core::provider::PlaybackVideoCodec::H264,
                synctv_core::provider::PlaybackVideoCodec::Vp9,
            ]
        );
        assert_eq!(
            converted.supported_containers,
            vec![
                synctv_core::provider::PlaybackContainer::Mp4,
                synctv_core::provider::PlaybackContainer::Webm,
            ]
        );
        assert_eq!(
            converted.audio_capability,
            synctv_core::provider::PlaybackAudioCapability::Surround
        );
        assert_eq!(
            converted.subtitle_preference,
            synctv_core::provider::PlaybackSubtitlePreference::None
        );
    }

    #[test]
    fn direct_url_embedded_playback_result_preserves_rich_playback_fields() {
        let expire_at = chrono::DateTime::from_timestamp(1_700_000_100, 0)
            .expect("test timestamp should be valid");
        let media = Media {
            id: MediaId::from(1101),
            playlist_id: None,
            room_id: RoomId::from(1102),
            creator_id: None,
            name: "Embedded Direct Playback".to_string(),
            position: 7.5,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({
                "playback_infos": {
                    "direct": synctv_core::models::media::PlaybackInfo {
                        urls: vec![
                            synctv_core::models::media::PlaybackUrl {
                                name: "primary".to_string(),
                                url: "https://cdn.example.com/video.mpd".to_string(),
                                headers: HashMap::from([
                                    ("Authorization".to_string(), "Bearer direct".to_string()),
                                ]),
                                expire_at: Some(expire_at),
                                metadata: None,
                            }
                        ],
                        default_url_index: 0,
                        subtitles: vec![
                            synctv_core::models::media::Subtitle {
                                name: "Chinese".to_string(),
                                language: "zh-CN".to_string(),
                                urls: vec![
                                    synctv_core::models::media::SubtitleUrl {
                                        name: "orig".to_string(),
                                        url: "https://cdn.example.com/subtitle.ass".to_string(),
                                        headers: HashMap::from([(
                                            "X-Subtitle-Token".to_string(),
                                            "subtitle-secret".to_string(),
                                        )]),
                                        format: "ass".to_string(),
                                    }
                                ],
                                default_url_index: 0,
                            }
                        ],
                        default_subtitle_index: Some(0),
                        danmakus: vec![
                            synctv_core::models::media::Danmaku {
                                name: "Bilibili Danmaku".to_string(),
                                url: "/api/providers/proxy/bilibili/room-1/media-1/danmu".to_string(),
                                format: Some("bilibili".to_string()),
                                headers: HashMap::from([(
                                    "X-Danmaku-Token".to_string(),
                                    "dm-secret".to_string(),
                                )]),
                            }
                        ],
                        format: "mpd".to_string(),
                    }
                },
                "default_mode": "direct",
                "metadata": {
                    "provider": "direct_url"
                }
            }),
            provider_instance_name: Some("direct_url".to_string()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let mut result = direct_url_embedded_playback_result_to_model(&media)
            .expect("embedded direct playback should parse")
            .expect("embedded direct playback should be detected");
        let signing_key = synctv_core::service::ProxySigningKey::derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        );
        let signed_expiry = chrono::Utc::now().timestamp() + 600;
        sign_local_bilibili_danmaku_urls(
            &mut result,
            "user-embedded",
            Some(&signing_key),
            Some(signed_expiry),
        );

        let direct = result
            .playback_infos
            .get("direct")
            .expect("direct mode should be preserved");
        assert_eq!(result.id, Some(media.id));
        assert_eq!(result.name, "Embedded Direct Playback");
        assert_eq!(
            direct.urls[0]
                .headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer direct")
        );
        assert_eq!(
            direct.urls[0].expire_at.map(|dt| dt.timestamp()),
            Some(1_700_000_100)
        );
        assert_eq!(direct.subtitles[0].urls[0].format, "ass");
        assert_eq!(
            direct.subtitles[0].urls[0]
                .headers
                .get("X-Subtitle-Token")
                .map(String::as_str),
            Some("subtitle-secret")
        );
        assert_eq!(direct.danmakus.len(), 1);
        assert_eq!(direct.danmakus[0].format.as_deref(), Some("bilibili"));
        let signed_query = direct.danmakus[0]
            .url
            .split('?')
            .nth(1)
            .expect("embedded bilibili danmaku URL should be signed");
        let claims = signing_key
            .parse_and_verify_query(signed_query, "bilibili", "room-1")
            .expect("signed embedded danmaku query should verify");
        assert_eq!(claims.user_id, "user-embedded");
        assert_eq!(claims.expires_at, signed_expiry);
        assert_eq!(
            direct.danmakus[0]
                .headers
                .get("X-Danmaku-Token")
                .map(String::as_str),
            Some("dm-secret")
        );
    }

    #[test]
    fn normalize_created_room_settings_sets_require_password_when_password_present() {
        let settings = normalize_created_room_settings(None, true);
        assert!(settings.require_password.0);
    }

    #[test]
    fn normalize_created_room_settings_preserves_other_fields() {
        let source = synctv_core::models::RoomSettings {
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };

        let settings = normalize_created_room_settings(Some(&source), false);

        assert!(settings.allow_guest_join.0);
        assert!(!settings.require_password.0);
    }

    #[test]
    fn bilibili_live_danmaku_for_static_media_builds_signed_proxy_url() {
        let media = Media {
            id: MediaId::from(1201),
            playlist_id: None,
            room_id: RoomId::from(1202),
            creator_id: None,
            name: "Bilibili Live".to_string(),
            position: 0.0,
            source_provider: synctv_core::provider::BilibiliProvider::NAME.to_string(),
            source_config: serde_json::json!({
                "type": "live",
                "room_id": 12345_u64
            }),
            provider_instance_name: Some("bilibili".to_string()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };
        let signing_key = synctv_core::service::ProxySigningKey::derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        );

        let expires_at = chrono::Utc::now().timestamp() + 600;
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let public_room_id = public_id_codec
            .encode_room_id(media.room_id)
            .expect("room id should encode");
        let public_user_id = public_id_codec
            .encode_user_id(UserId::from(301))
            .expect("user id should encode");
        let danmaku = bilibili_live_danmaku_for_static_media(
            &media,
            &public_user_id,
            &public_id_codec,
            Some(&signing_key),
            Some(expires_at),
        )
        .expect("bilibili live should expose danmaku");

        assert_eq!(danmaku.name, "Bilibili Danmaku");
        assert_eq!(danmaku.format.as_deref(), Some("bilibili"));
        assert!(danmaku
            .url
            .starts_with(&format!("/api/providers/proxy/bilibili/{public_room_id}/")));
        let query = danmaku
            .url
            .split('?')
            .nth(1)
            .expect("signed danmaku URL should include query");
        let claims = signing_key
            .parse_and_verify_query(query, "bilibili", &public_room_id)
            .expect("signed danmaku query should verify");
        assert_eq!(claims.room_id, public_room_id);
        assert_eq!(claims.user_id, public_user_id);
        assert_eq!(claims.expires_at, expires_at);
    }

    #[test]
    fn media_to_proto_includes_resource_version() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let media = Media {
            id: MediaId::from(101),
            playlist_id: None,
            room_id: RoomId::from(102),
            creator_id: Some(UserId::from(103)),
            name: "Proto Media".to_string(),
            position: 3.5,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({ "url": "https://example.com/video.mp4" }),
            provider_instance_name: Some("direct_url".to_string()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 42,
        };

        let proto = media_to_proto(&media, &public_id_codec);
        assert_eq!(proto.version, 42);
    }

    #[test]
    fn media_to_proto_redacts_nested_credentials_without_cloning_sanitized_value() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let media = Media {
            id: MediaId::from(104),
            playlist_id: None,
            room_id: RoomId::from(102),
            creator_id: Some(UserId::from(103)),
            name: "Secret Media".to_string(),
            position: 1.0,
            source_provider: "alist".to_string(),
            source_config: serde_json::json!({
                "url": "https://example.com/video.mp4",
                "token": "top-level-token",
                "nested": {
                    "password": "nested-password",
                    "safe": true
                },
                "items": [
                    {
                        "api_key": "nested-api-key",
                        "path": "/tv"
                    }
                ],
                "metadata": {
                    "title": "Secret Media"
                }
            }),
            provider_instance_name: Some("alist-main".to_string()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        };

        let proto = media_to_proto(&media, &public_id_codec);
        let source_config: serde_json::Value = serde_json::from_slice(&proto.source_config)
            .expect("proto source config should be JSON");

        assert_eq!(source_config["token"], REDACTED_SOURCE_CONFIG_VALUE);
        assert_eq!(
            source_config["nested"]["password"],
            REDACTED_SOURCE_CONFIG_VALUE
        );
        assert_eq!(
            source_config["items"][0]["api_key"],
            REDACTED_SOURCE_CONFIG_VALUE
        );
        assert_eq!(source_config["nested"]["safe"], serde_json::json!(true));
        assert_eq!(
            source_config["metadata"]["title"],
            serde_json::json!("Secret Media")
        );
    }

    #[test]
    fn playlist_to_proto_includes_resource_version() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let playlist = synctv_core::models::Playlist {
            id: PlaylistId::from(105),
            room_id: RoomId::from(102),
            creator_id: Some(UserId::from(103)),
            name: "Proto Playlist".to_string(),
            parent_id: None,
            position: 1.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 7,
        };

        let proto = playlist_to_proto(&playlist, 3, &public_id_codec);
        assert_eq!(proto.version, 7);
    }

    #[test]
    fn room_to_proto_includes_resource_version() {
        let public_id_codec = crate::PublicIdCodec::default_for_tests();
        let now = chrono::Utc::now();
        let room = Room {
            id: RoomId::from(102),
            name: "Proto Room".to_string(),
            description: "Room description".to_string(),
            created_by: UserId::from(103),
            status: synctv_core::models::RoomStatus::Active,
            is_banned: false,
            closed_at: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 9,
            last_activity_at: now,
        };

        let proto = room_to_proto_basic(&room, None, Some(0), &public_id_codec);
        assert_eq!(proto.version, 9);
    }
}
