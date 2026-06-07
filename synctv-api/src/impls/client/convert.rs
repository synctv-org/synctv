use rayon::prelude::*;

use synctv_core::proxy_signature::SignedProxyUrlRequest;
use synctv_core::service::room::ClientResourceAvailability;

const PARALLEL_PROTO_MAP_THRESHOLD: usize = 128;

fn proto_encode_error(kind: &str, error: &str) -> crate::impls::ApiError {
    crate::impls::ApiError::Internal(format!("Failed to encode {kind} public id: {error}"))
}

fn encode_room_id_for_proto(
    id: synctv_core::models::RoomId,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_room_id(id)
        .map_err(|error| proto_encode_error("room", &error))
}

fn encode_media_id_for_proto(
    id: synctv_core::models::MediaId,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_media_id(id)
        .map_err(|error| proto_encode_error("media", &error))
}

fn encode_playlist_id_for_proto(
    id: synctv_core::models::PlaylistId,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_playlist_id(id)
        .map_err(|error| proto_encode_error("playlist", &error))
}

fn encode_user_id_for_proto(
    id: synctv_core::models::UserId,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<String, crate::impls::ApiError> {
    public_id_codec
        .encode_user_id(id)
        .map_err(|error| proto_encode_error("user", &error))
}

pub(crate) fn json_to_vec<T: serde::Serialize + ?Sized>(
    value: &T,
    context: &'static str,
) -> Result<Vec<u8>, crate::impls::ApiError> {
    serde_json::to_vec(value).map_err(|error| {
        crate::impls::ApiError::Internal(format!(
            "Failed to serialize {context} as JSON bytes: {error}"
        ))
    })
}

pub(crate) fn json_to_string<T: serde::Serialize + ?Sized>(
    value: &T,
    context: &'static str,
) -> Result<String, crate::impls::ApiError> {
    serde_json::to_string(value).map_err(|error| {
        crate::impls::ApiError::Internal(format!(
            "Failed to serialize {context} as JSON string: {error}"
        ))
    })
}

fn usize_to_i32(value: usize, field: &'static str) -> Result<i32, crate::impls::ApiError> {
    i32::try_from(value)
        .map_err(|_| crate::impls::ApiError::Internal(format!("{field} exceeds i32::MAX")))
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

pub(crate) fn try_map_slice_preserve_order<T, U, F>(
    items: &[T],
    map: F,
) -> Result<Vec<U>, crate::impls::ApiError>
where
    T: Sync,
    U: Send,
    F: Fn(&T) -> Result<U, crate::impls::ApiError> + Sync + Send,
{
    items
        .par_iter()
        .with_min_len(PARALLEL_PROTO_MAP_THRESHOLD)
        .map(&map)
        .collect()
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
    viewer_id: Option<synctv_core::models::UserId>,
) -> Result<Vec<u8>, crate::impls::ApiError> {
    if can_view_media_source_config(media, viewer_id) {
        json_to_vec(&media.source_config, "media source config")
    } else {
        Ok(Vec::new())
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
) -> Result<Vec<u8>, crate::impls::ApiError> {
    if can_view_playlist_source_config(playlist, viewer_id) {
        playlist
            .source_config
            .as_ref()
            .map(|source_config| json_to_vec(source_config, "playlist source config"))
            .transpose()
            .map(std::option::Option::unwrap_or_default)
    } else {
        Ok(Vec::new())
    }
}

fn metadata_i32(
    metadata: &serde_json::Value,
    key: &'static str,
    context: &'static str,
) -> Result<i32, crate::impls::ApiError> {
    let Some(value) = metadata.get(key) else {
        return Ok(0);
    };
    let raw = value.as_i64().ok_or_else(|| {
        crate::impls::ApiError::Internal(format!("{context} metadata field '{key}' must be i32"))
    })?;
    i32::try_from(raw).map_err(|_| {
        crate::impls::ApiError::Internal(format!("{context} metadata field '{key}' exceeds i32"))
    })
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
        storage_backend: file.storage_backend.clone(),
        object_key: file.object_key.clone(),
        url: required_cover_url(url, "resource cover")?,
        metadata: json_to_vec(&file.metadata, "resource cover metadata")?,
    })
}

pub(crate) fn stored_file_reference_to_video_cover(
    file: &synctv_core::models::StoredFileReference,
    url: Option<&str>,
) -> Result<synctv_proto::client::VideoCover, crate::impls::ApiError> {
    Ok(synctv_proto::client::VideoCover {
        id: file.file_reference_id.to_string(),
        storage_backend: file.storage_backend.clone(),
        object_key: file.object_key.clone(),
        url: required_cover_url(url, "video cover")?,
        mime_type: file.mime_type.clone(),
        size_bytes: file.size_bytes,
        width: metadata_i32(&file.metadata, "width", "video cover")?,
        height: metadata_i32(&file.metadata, "height", "video cover")?,
        metadata: json_to_vec(&file.metadata, "video cover metadata")?,
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
    let delivery_preference = match synctv_proto::client::PlaybackDeliveryPreference::try_from(
        profile.delivery_preference,
    )
    .map_err(|_| {
        crate::impls::ApiError::InvalidInput("Unsupported playback delivery preference".to_string())
    })? {
        synctv_proto::client::PlaybackDeliveryPreference::Unspecified
        | synctv_proto::client::PlaybackDeliveryPreference::Auto => {
            synctv_core::provider::PlaybackDeliveryPreference::Auto
        }
        synctv_proto::client::PlaybackDeliveryPreference::DirectPlay => {
            synctv_core::provider::PlaybackDeliveryPreference::DirectPlay
        }
        synctv_proto::client::PlaybackDeliveryPreference::Transcode => {
            synctv_core::provider::PlaybackDeliveryPreference::Transcode
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
        delivery_preference,
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

pub(crate) fn try_user_to_proto(
    user: &synctv_core::models::User,
    email: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
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

pub(crate) fn try_room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    try_room_to_proto_with_availability(
        room,
        settings,
        member_count,
        ClientResourceAvailability::Available,
        public_id_codec,
    )
}

pub(crate) fn try_room_to_proto_basic_with_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let mut proto = try_room_to_proto_basic(room, settings, member_count, public_id_codec)?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_resource_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

pub(crate) fn try_room_to_proto_with_availability(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let room_settings = settings.cloned().unwrap_or_default();
    Ok(synctv_proto::client::Room {
        id: encode_room_id_for_proto(room.id, public_id_codec)?,
        name: room.name.clone(),
        description: room.description.clone(),
        created_by: encode_user_id_for_proto(room.created_by, public_id_codec)?,
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: json_to_vec(&room_settings, "room settings")?,
        created_at: room.created_at.timestamp(),
        member_count: member_count.unwrap_or(0),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        availability: resource_availability_enum_to_proto(availability),
        version: i64::from(room.version),
        cover: None,
    })
}

pub(crate) fn try_room_to_proto_with_availability_and_cover(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Room, crate::impls::ApiError> {
    let mut proto = try_room_to_proto_with_availability(
        room,
        settings,
        member_count,
        availability,
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
    public_id_codec: &crate::PublicIdCodec,
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

pub fn try_media_to_proto(
    media: &synctv_core::models::Media,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    try_media_to_proto_for_viewer(media, true, None, public_id_codec)
}

pub fn try_media_to_proto_with_availability(
    media: &synctv_core::models::Media,
    is_available: bool,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    try_media_to_proto_for_viewer(media, is_available, None, public_id_codec)
}

pub fn try_media_to_proto_for_viewer(
    media: &synctv_core::models::Media,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let metadata_bytes = if can_view_media_source_config(media, viewer_id) {
        media
            .source_config
            .get("metadata")
            .map(|metadata| json_to_vec(metadata, "media metadata"))
            .transpose()?
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(synctv_proto::client::Media {
        id: encode_media_id_for_proto(media.id, public_id_codec)?,
        room_id: encode_room_id_for_proto(media.room_id, public_id_codec)?,
        source_provider: media.source_provider.clone(),
        name: media.name.clone(),
        metadata: metadata_bytes,
        position: media.position,
        added_at: media.added_at.timestamp(),
        creator_id: media
            .creator_id
            .map(|id| encode_user_id_for_proto(id, public_id_codec))
            .transpose()?
            .unwrap_or_default(),
        provider_instance_name: media.provider_instance_name.clone().unwrap_or_default(),
        source_config: serialize_source_config_for_viewer(media, viewer_id)?,
        availability: resource_availability_to_proto(is_available),
        version: i64::from(media.version),
        description: media.description.clone(),
        cover: None,
    })
}

pub fn try_media_to_proto_for_viewer_with_cover(
    media: &synctv_core::models::Media,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Media, crate::impls::ApiError> {
    let mut proto = try_media_to_proto_for_viewer(media, is_available, viewer_id, public_id_codec)?;
    proto.cover = cover
        .map(|file| stored_file_reference_to_video_cover(file, cover_url))
        .transpose()?;
    Ok(proto)
}

pub(crate) fn try_playlist_to_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    try_playlist_to_proto_with_availability(playlist, item_count, true, public_id_codec)
}

pub(crate) fn try_playlist_to_proto_with_availability(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
    try_playlist_to_proto_for_viewer(playlist, item_count, is_available, None, public_id_codec)
}

pub(crate) fn try_playlist_to_proto_for_viewer(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> {
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
        source_provider: playlist.source_provider.clone().unwrap_or_default(),
        provider_instance_name: playlist.provider_instance_name.clone().unwrap_or_default(),
        description: playlist.description.clone(),
        cover: None,
    })
}

pub(crate) fn try_playlist_to_proto_for_viewer_with_cover(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
    viewer_id: Option<synctv_core::models::UserId>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    public_id_codec: &crate::PublicIdCodec,
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
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::PlaylistBrowsePathNode, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaylistBrowsePathNode {
        playlist_id: encode_playlist_id_for_proto(playlist.id, public_id_codec)?,
        name: playlist.name.clone(),
        target: Vec::new(),
    })
}

pub(crate) fn try_playback_state_to_proto(
    state: &synctv_core::models::RoomPlaybackState,
    public_id_codec: &crate::PublicIdCodec,
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
        target: state.target.clone(),
        target_hash: state.target_hash(),
    })
}

pub(crate) fn try_room_member_to_proto_with_permissions(
    member: &synctv_core::models::RoomMemberWithUser,
    permissions: synctv_core::models::RoomPermissionSet,
    public_id_codec: &crate::PublicIdCodec,
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
    })
}

pub(crate) fn try_members_to_proto(
    members: &[synctv_core::models::RoomMemberWithUser],
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Vec<synctv_proto::common::RoomMember>, crate::impls::ApiError> {
    try_map_slice_preserve_order(members, |m| {
        let permissions =
            permission_service.effective_member_with_user_permissions(m, room_settings);
        try_room_member_to_proto_with_permissions(m, permissions, public_id_codec)
    })
}

pub(crate) fn try_media_list_to_proto(
    media: &[synctv_core::models::Media],
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Vec<synctv_proto::client::Media>, crate::impls::ApiError> {
    try_map_slice_preserve_order(media, |media| try_media_to_proto(media, public_id_codec))
}

pub(crate) fn try_playlist_list_to_proto<T, F>(
    items: &[T],
    map: F,
) -> Result<Vec<synctv_proto::client::Playlist>, crate::impls::ApiError>
where
    T: Sync,
    F: Fn(&T) -> Result<synctv_proto::client::Playlist, crate::impls::ApiError> + Sync + Send,
{
    try_map_slice_preserve_order(items, map)
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

pub(crate) fn try_bilibili_live_danmaku_for_static_media(
    media: &synctv_core::models::Media,
    user_id: &str,
    public_id_codec: &crate::PublicIdCodec,
    signing_key: Option<&synctv_core::proxy_signature::ProxySigningKey>,
    expires_at: Option<i64>,
) -> Result<Option<synctv_core::models::media::Danmaku>, crate::impls::ApiError> {
    let Some(signing_key) = signing_key else {
        return Ok(None);
    };
    if media.source_provider.trim() != synctv_core::provider::BilibiliProvider::NAME {
        return Ok(None);
    }
    if media
        .source_config
        .get("type")
        .and_then(serde_json::Value::as_str)
        != Some("live")
    {
        return Ok(None);
    }

    let expires_at = expires_at.unwrap_or_else(|| {
        chrono::Utc::now().timestamp()
            + synctv_core::proxy_signature::ProxySigningKey::default_expiry_secs()
    });
    let room_id = encode_room_id_for_proto(media.room_id, public_id_codec)?;
    let media_id = encode_media_id_for_proto(media.id, public_id_codec)?;
    let url = synctv_core::proxy_signature::build_signed_proxy_url(SignedProxyUrlRequest {
        provider: synctv_core::provider::BilibiliProvider::NAME,
        version: &room_id,
        action: &format!("{media_id}/danmu"),
        signing_key,
        room_id: &room_id,
        user_id,
        expires_at,
    });

    Ok(Some(synctv_core::models::media::Danmaku {
        name: "Bilibili Danmaku".to_string(),
        url,
        format: Some("bilibili".to_string()),
        headers: std::collections::HashMap::new(),
    }))
}

pub(crate) fn sign_local_bilibili_danmaku_urls(
    result: &mut synctv_core::models::media::PlaybackResult,
    user_id: &str,
    signing_key: Option<&synctv_core::proxy_signature::ProxySigningKey>,
    expires_at: Option<i64>,
) {
    let Some(signing_key) = signing_key else {
        return;
    };
    let expires_at = expires_at.unwrap_or_else(|| {
        chrono::Utc::now().timestamp()
            + synctv_core::proxy_signature::ProxySigningKey::default_expiry_secs()
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

            danmaku.url =
                synctv_core::proxy_signature::build_signed_proxy_url(SignedProxyUrlRequest {
                    provider: synctv_core::provider::BilibiliProvider::NAME,
                    version: room_id,
                    action,
                    signing_key,
                    room_id,
                    user_id,
                    expires_at,
                });
        }
    }
}

/// Convert models `PlaybackResult` to proto `Playback`
pub(crate) fn try_playback_to_proto(
    result: &synctv_core::models::media::PlaybackResult,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_proto::client::Playback, crate::impls::ApiError> {
    validate_playback_result_shape(result)?;

    let playback_infos = result
        .playback_infos
        .iter()
        .map(|(mode, info)| Ok((mode.clone(), playback_info_to_proto(info)?)))
        .collect::<Result<_, crate::impls::ApiError>>()?;

    let metadata = result
        .metadata
        .iter()
        .map(|(key, value)| Ok((key.clone(), json_to_string(value, "playback metadata")?)))
        .collect::<Result<_, crate::impls::ApiError>>()?;

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
        playback_infos,
        default_mode: result.default_mode.clone(),
        metadata,
        expires_at: None,
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

/// Convert models `PlaybackInfo` to proto `PlaybackInfo`
fn playback_info_to_proto(
    info: &synctv_core::models::media::PlaybackInfo,
) -> Result<synctv_proto::client::PlaybackInfo, crate::impls::ApiError> {
    if info.urls.is_empty() {
        return Err(crate::impls::ApiError::Internal(
            "playback mode has no URLs".to_string(),
        ));
    }
    Ok(synctv_proto::client::PlaybackInfo {
        urls: try_map_slice_preserve_order(&info.urls, playback_url_to_proto)?,
        default_url_index: checked_index_i32(
            info.default_url_index,
            info.urls.len(),
            "default playback URL index",
        )?,
        subtitles: try_map_slice_preserve_order(&info.subtitles, subtitle_to_proto)?,
        default_subtitle_index: info
            .default_subtitle_index
            .map(|index| checked_index_i32(index, info.subtitles.len(), "default subtitle index"))
            .transpose()?,
        danmakus: try_map_slice_preserve_order(&info.danmakus, danmaku_to_proto)?,
        format: info.format.clone(),
    })
}

/// Convert models `PlaybackUrl` to proto `PlaybackUrl`
fn playback_url_to_proto(
    url: &synctv_core::models::media::PlaybackUrl,
) -> Result<synctv_proto::client::PlaybackUrl, crate::impls::ApiError> {
    Ok(synctv_proto::client::PlaybackUrl {
        name: url.name.clone(),
        url: require_non_empty_url(&url.url, "playback")?,
        headers: client_visible_headers(&url.url, &url.headers),
        expire_at: url.expire_at.map(|dt| dt.timestamp()),
        metadata: url
            .metadata
            .as_ref()
            .map(playback_url_metadata_to_proto)
            .transpose()?,
    })
}

/// Convert models `PlaybackUrlMetadata` to proto `PlaybackUrlMetadata`
fn playback_url_metadata_to_proto(
    metadata: &synctv_core::models::media::PlaybackUrlMetadata,
) -> Result<synctv_proto::client::PlaybackUrlMetadata, crate::impls::ApiError> {
    let extra = metadata
        .extra
        .iter()
        .map(|(key, value)| Ok((key.clone(), json_to_string(value, "playback URL metadata")?)))
        .collect::<Result<_, crate::impls::ApiError>>()?;

    Ok(synctv_proto::client::PlaybackUrlMetadata {
        resolution: metadata.resolution.clone(),
        bitrate: metadata.bitrate,
        codec: metadata.codec.clone(),
        fps: metadata.fps,
        extra,
    })
}

/// Convert models Subtitle to proto Subtitle
fn subtitle_to_proto(
    subtitle: &synctv_core::models::media::Subtitle,
) -> Result<synctv_proto::client::Subtitle, crate::impls::ApiError> {
    if subtitle.urls.is_empty() {
        return Err(crate::impls::ApiError::Internal(
            "subtitle has no URLs".to_string(),
        ));
    }
    Ok(synctv_proto::client::Subtitle {
        name: subtitle.name.clone(),
        language: subtitle.language.clone(),
        urls: try_map_slice_preserve_order(&subtitle.urls, subtitle_url_to_proto)?,
        default_url_index: checked_index_i32(
            subtitle.default_url_index,
            subtitle.urls.len(),
            "subtitle default URL index",
        )?,
    })
}

/// Convert models `SubtitleUrl` to proto `SubtitleUrl`
fn subtitle_url_to_proto(
    url: &synctv_core::models::media::SubtitleUrl,
) -> Result<synctv_proto::client::SubtitleUrl, crate::impls::ApiError> {
    Ok(synctv_proto::client::SubtitleUrl {
        name: url.name.clone(),
        url: require_non_empty_url(&url.url, "subtitle")?,
        headers: client_visible_headers(&url.url, &url.headers),
        format: url.format.clone(),
    })
}

/// Convert models Danmaku to proto Danmaku
fn danmaku_to_proto(
    danmaku: &synctv_core::models::media::Danmaku,
) -> Result<synctv_proto::client::Danmaku, crate::impls::ApiError> {
    Ok(synctv_proto::client::Danmaku {
        name: danmaku.name.clone(),
        url: require_non_empty_url(&danmaku.url, "danmaku")?,
        format: danmaku.format.clone(),
        headers: client_visible_headers(&danmaku.url, &danmaku.headers),
    })
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
    url.starts_with("/api/providers/proxy/")
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_created_room_settings, playback_client_profile_from_proto,
        proto_role_filter_to_room_role, provider_playback_info_to_model,
        stored_file_reference_to_video_cover, try_bilibili_live_danmaku_for_static_media,
        try_media_to_proto, try_media_to_proto_for_viewer, try_playback_to_proto,
        try_playlist_to_proto, try_playlist_to_proto_for_viewer, try_room_to_proto_basic,
    };
    use chrono::Utc;
    use std::collections::HashMap;
    use synctv_core::models::{
        Media, MediaId, PlaylistId, Room, RoomId, StoredFileReference, UserId,
    };

    fn stored_file_reference_with_metadata(metadata: serde_json::Value) -> StoredFileReference {
        StoredFileReference {
            file_reference_id: 7,
            storage_backend: "database".to_string(),
            object_key: "covers/video.png".to_string(),
            mime_type: "image/png".to_string(),
            size_bytes: 1234,
            checksum_sha256: "checksum".to_string(),
            metadata,
            created_at: Utc::now(),
            validated_at: None,
        }
    }

    #[test]
    fn video_cover_metadata_dimensions_convert_to_proto() {
        let file = stored_file_reference_with_metadata(serde_json::json!({
            "width": 1920,
            "height": 1080
        }));

        let cover = stored_file_reference_to_video_cover(
            &file,
            Some("https://cdn.example.com/covers/video.png"),
        )
        .expect("valid video cover should convert");

        assert_eq!(cover.width, 1920);
        assert_eq!(cover.height, 1080);
    }

    #[test]
    fn video_cover_metadata_rejects_invalid_dimensions() {
        let file = stored_file_reference_with_metadata(serde_json::json!({
            "width": "1920",
            "height": 1080
        }));

        let error = stored_file_reference_to_video_cover(
            &file,
            Some("https://cdn.example.com/covers/video.png"),
        )
        .expect_err("invalid video cover dimensions should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("video cover metadata field 'width'")
        ));
    }

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
        assert_eq!(
            playback_client_profile_from_proto(None).expect("absent profile should convert"),
            None
        );
    }

    #[test]
    fn playback_to_proto_clears_proxy_headers_but_preserves_raw_headers() {
        use synctv_core::models::media::{
            Danmaku, PlaybackInfo, PlaybackResult, PlaybackUrl, Subtitle, SubtitleUrl,
        };

        let result = PlaybackResult::builder(
            None,
            RoomId::expect_positive(1),
            "Headered media".to_string(),
            0.0,
        )
        .default_mode("direct".to_string())
        .add_mode(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec![PlaybackUrl {
                    name: "main".to_string(),
                    url: "https://cdn.example.com/video.mp4".to_string(),
                    headers: HashMap::from([(
                        "Authorization".to_string(),
                        "Bearer stream-token".to_string(),
                    )]),
                    expire_at: None,
                    metadata: None,
                }, PlaybackUrl {
                    name: "proxy".to_string(),
                    url: "/api/providers/proxy/direct_url/ver-1/stream?sig=s&uid=u&rid=r&exp=1".to_string(),
                    headers: HashMap::from([(
                        "Authorization".to_string(),
                        "Bearer proxy-owned-token".to_string(),
                    )]),
                    expire_at: None,
                    metadata: None,
                }],
                default_url_index: 0,
                subtitles: vec![Subtitle {
                    name: "Chinese".to_string(),
                    language: "zh-CN".to_string(),
                    urls: vec![SubtitleUrl {
                        name: "main".to_string(),
                        url: "https://cdn.example.com/subtitle.ass".to_string(),
                        headers: HashMap::from([(
                            "X-Subtitle-Token".to_string(),
                            "subtitle-token".to_string(),
                        )]),
                        format: "ass".to_string(),
                    }, SubtitleUrl {
                        name: "proxy".to_string(),
                        url: "/api/providers/proxy/direct_url/ver-1/subtitle%2Fdirect%2F0?sig=s&uid=u&rid=r&exp=1".to_string(),
                        headers: HashMap::from([(
                            "X-Subtitle-Token".to_string(),
                            "proxy-subtitle-token".to_string(),
                        )]),
                        format: "ass".to_string(),
                    }],
                    default_url_index: 0,
                }],
                default_subtitle_index: Some(0),
                danmakus: vec![Danmaku {
                    name: "Danmaku".to_string(),
                    url: "https://cdn.example.com/danmaku.xml".to_string(),
                    format: Some("xml".to_string()),
                    headers: HashMap::from([("Cookie".to_string(), "sid=secret".to_string())]),
                }, Danmaku {
                    name: "Proxy Danmaku".to_string(),
                    url: "/api/providers/proxy/bilibili/room_1/media_1/danmaku?sig=s&uid=u&rid=r&exp=1".to_string(),
                    format: Some("xml".to_string()),
                    headers: HashMap::from([("Cookie".to_string(), "proxy-owned".to_string())]),
                }],
                format: "mp4".to_string(),
            },
        )
        .build()
        .expect("playback result should build");

        let proto = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect("playback should convert");

        let direct = proto
            .playback_infos
            .get("direct")
            .expect("direct mode should be converted");

        assert_eq!(
            direct.urls[0]
                .headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer stream-token")
        );
        assert!(direct.urls[1].headers.is_empty());
        assert_eq!(
            direct.subtitles[0].urls[0]
                .headers
                .get("X-Subtitle-Token")
                .map(String::as_str),
            Some("subtitle-token")
        );
        assert!(direct.subtitles[0].urls[1].headers.is_empty());
        assert_eq!(
            direct.danmakus[0].headers.get("Cookie").map(String::as_str),
            Some("sid=secret")
        );
        assert!(direct.danmakus[1].headers.is_empty());
    }

    #[test]
    fn playback_to_proto_rejects_empty_playback_url() {
        use synctv_core::models::media::{PlaybackInfo, PlaybackResult, PlaybackUrl};

        let result = PlaybackResult::builder(
            None,
            RoomId::expect_positive(1),
            "Broken media".to_string(),
            0.0,
        )
        .default_mode("direct".to_string())
        .add_mode(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec![PlaybackUrl::simple("main".to_string(), " ".to_string())],
                default_url_index: 0,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                format: "mp4".to_string(),
            },
        )
        .build()
        .expect("playback result should build");

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("empty playback URL should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("playback url is empty")
        ));
    }

    #[test]
    fn playback_to_proto_rejects_missing_default_mode() {
        use synctv_core::models::media::{PlaybackInfo, PlaybackResult, PlaybackUrl};

        let result = PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: RoomId::expect_positive(1),
            name: "Broken media".to_string(),
            position: 0.0,
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    urls: vec![PlaybackUrl::simple(
                        "main".to_string(),
                        "https://cdn.example.com/video.mp4".to_string(),
                    )],
                    default_url_index: 0,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    format: "mp4".to_string(),
                },
            )]),
            default_mode: "missing".to_string(),
            metadata: HashMap::new(),
        };

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("missing default playback mode should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("default mode 'missing' is missing")
        ));
    }

    #[test]
    fn playback_to_proto_rejects_empty_default_mode() {
        use synctv_core::models::media::{PlaybackInfo, PlaybackResult, PlaybackUrl};

        let result = PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: RoomId::expect_positive(1),
            name: "Broken media".to_string(),
            position: 0.0,
            playback_infos: HashMap::from([(
                String::new(),
                PlaybackInfo {
                    urls: vec![PlaybackUrl::simple(
                        "main".to_string(),
                        "https://cdn.example.com/video.mp4".to_string(),
                    )],
                    default_url_index: 0,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    format: "mp4".to_string(),
                },
            )]),
            default_mode: String::new(),
            metadata: HashMap::new(),
        };

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("empty default playback mode should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("default mode is empty")
        ));
    }

    #[test]
    fn playback_to_proto_rejects_empty_mode_name() {
        use synctv_core::models::media::{PlaybackInfo, PlaybackResult, PlaybackUrl};

        let result = PlaybackResult {
            id: None,
            playlist_id: None,
            room_id: RoomId::expect_positive(1),
            name: "Broken media".to_string(),
            position: 0.0,
            playback_infos: HashMap::from([
                (
                    "direct".to_string(),
                    PlaybackInfo {
                        urls: vec![PlaybackUrl::simple(
                            "main".to_string(),
                            "https://cdn.example.com/video.mp4".to_string(),
                        )],
                        default_url_index: 0,
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        format: "mp4".to_string(),
                    },
                ),
                (
                    " ".to_string(),
                    PlaybackInfo {
                        urls: vec![PlaybackUrl::simple(
                            "main".to_string(),
                            "https://cdn.example.com/video-alt.mp4".to_string(),
                        )],
                        default_url_index: 0,
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        format: "mp4".to_string(),
                    },
                ),
            ]),
            default_mode: "direct".to_string(),
            metadata: HashMap::new(),
        };

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("empty playback mode name should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("empty mode name")
        ));
    }

    #[test]
    fn playback_to_proto_rejects_default_url_index_out_of_range() {
        use synctv_core::models::media::{PlaybackInfo, PlaybackResult, PlaybackUrl};

        let result = PlaybackResult::builder(
            None,
            RoomId::expect_positive(1),
            "Broken media".to_string(),
            0.0,
        )
        .default_mode("direct".to_string())
        .add_mode(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec![PlaybackUrl::simple(
                    "main".to_string(),
                    "https://cdn.example.com/video.mp4".to_string(),
                )],
                default_url_index: 1,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                format: "mp4".to_string(),
            },
        )
        .build()
        .expect("playback result should build");

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("out-of-range playback URL index should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("default playback URL index")
        ));
    }

    #[test]
    fn playback_to_proto_rejects_subtitle_default_url_index_out_of_range() {
        use synctv_core::models::media::{
            PlaybackInfo, PlaybackResult, PlaybackUrl, Subtitle, SubtitleUrl,
        };

        let result = PlaybackResult::builder(
            None,
            RoomId::expect_positive(1),
            "Broken media".to_string(),
            0.0,
        )
        .default_mode("direct".to_string())
        .add_mode(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec![PlaybackUrl::simple(
                    "main".to_string(),
                    "https://cdn.example.com/video.mp4".to_string(),
                )],
                default_url_index: 0,
                subtitles: vec![Subtitle {
                    name: "Chinese".to_string(),
                    language: "zh-CN".to_string(),
                    urls: vec![SubtitleUrl {
                        name: "main".to_string(),
                        url: "https://cdn.example.com/subtitle.ass".to_string(),
                        headers: HashMap::new(),
                        format: "ass".to_string(),
                    }],
                    default_url_index: 1,
                }],
                default_subtitle_index: Some(0),
                danmakus: Vec::new(),
                format: "mp4".to_string(),
            },
        )
        .build()
        .expect("playback result should build");

        let error = try_playback_to_proto(&result, &crate::PublicIdCodec::plain())
            .expect_err("out-of-range subtitle URL index should fail");

        assert!(matches!(
            error,
            crate::impls::ApiError::Internal(message)
                if message.contains("subtitle default URL index")
        ));
    }

    #[test]
    fn playback_client_profile_from_proto_applies_defaults_for_omitted_repeated_capabilities() {
        let proto = synctv_proto::client::PlaybackClientProfile {
            delivery_preference: synctv_proto::client::PlaybackDeliveryPreference::Unspecified
                as i32,
            max_streaming_bitrate: Some(12_000_000),
            max_audio_channels: Some(2),
            supported_video_codecs: Vec::new(),
            supported_containers: Vec::new(),
            audio_capability: synctv_proto::client::PlaybackAudioCapability::Unspecified as i32,
            subtitle_preference: synctv_proto::client::PlaybackSubtitlePreference::Unspecified
                as i32,
        };

        let converted = playback_client_profile_from_proto(Some(&proto))
            .expect("present proto profile should convert")
            .expect("present profile should be returned");
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
        let proto = synctv_proto::client::PlaybackClientProfile {
            delivery_preference: synctv_proto::client::PlaybackDeliveryPreference::DirectPlay
                as i32,
            max_streaming_bitrate: None,
            max_audio_channels: Some(6),
            supported_video_codecs: vec![
                synctv_proto::client::PlaybackVideoCodec::H264 as i32,
                synctv_proto::client::PlaybackVideoCodec::Vp9 as i32,
            ],
            supported_containers: vec![
                synctv_proto::client::PlaybackContainer::Mp4 as i32,
                synctv_proto::client::PlaybackContainer::Webm as i32,
            ],
            audio_capability: synctv_proto::client::PlaybackAudioCapability::Surround as i32,
            subtitle_preference: synctv_proto::client::PlaybackSubtitlePreference::None as i32,
        };

        let converted = playback_client_profile_from_proto(Some(&proto))
            .expect("present proto profile should convert")
            .expect("present profile should be returned");

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
    fn playback_client_profile_from_proto_rejects_unknown_enums() {
        let mut proto = synctv_proto::client::PlaybackClientProfile {
            delivery_preference: synctv_proto::client::PlaybackDeliveryPreference::Auto as i32,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            supported_video_codecs: Vec::new(),
            supported_containers: Vec::new(),
            audio_capability: synctv_proto::client::PlaybackAudioCapability::Unspecified as i32,
            subtitle_preference: synctv_proto::client::PlaybackSubtitlePreference::Unspecified
                as i32,
        };

        proto.delivery_preference = 99;
        assert!(matches!(
            playback_client_profile_from_proto(Some(&proto)),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("delivery preference")
        ));

        proto.delivery_preference = synctv_proto::client::PlaybackDeliveryPreference::Auto as i32;
        proto.supported_video_codecs = vec![99];
        assert!(matches!(
            playback_client_profile_from_proto(Some(&proto)),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("video codec")
        ));

        proto.supported_video_codecs.clear();
        proto.supported_containers = vec![99];
        assert!(matches!(
            playback_client_profile_from_proto(Some(&proto)),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("container")
        ));

        proto.supported_containers.clear();
        proto.audio_capability = 99;
        assert!(matches!(
            playback_client_profile_from_proto(Some(&proto)),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("audio capability")
        ));

        proto.audio_capability = synctv_proto::client::PlaybackAudioCapability::Unspecified as i32;
        proto.subtitle_preference = 99;
        assert!(matches!(
            playback_client_profile_from_proto(Some(&proto)),
            Err(crate::impls::ApiError::InvalidInput(message))
                if message.contains("subtitle preference")
        ));
    }

    #[test]
    fn normalize_created_room_settings_defaults_when_missing() {
        let settings = normalize_created_room_settings(None);
        assert!(!settings.allow_guest_join.0);
    }

    #[test]
    fn normalize_created_room_settings_preserves_other_fields() {
        let source = synctv_core::models::RoomSettings {
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };

        let settings = normalize_created_room_settings(Some(&source));

        assert!(settings.allow_guest_join.0);
    }

    #[test]
    fn bilibili_live_danmaku_for_static_media_builds_signed_proxy_url() {
        let media = Media {
            id: MediaId::expect_positive(1201),
            playlist_id: None,
            room_id: RoomId::expect_positive(1202),
            creator_id: None,
            name: "Bilibili Live".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: synctv_core::provider::BilibiliProvider::NAME.to_string(),
            source_config: serde_json::json!({
                "type": "live",
                "room_id": 12345_u64
            }),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };
        let signing_key = synctv_core::proxy_signature::ProxySigningKey::try_derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        )
        .expect("test proxy signing key should derive");

        let expires_at = chrono::Utc::now().timestamp() + 600;
        let public_id_codec = crate::PublicIdCodec::plain();
        let public_room_id = public_id_codec
            .encode_room_id(media.room_id)
            .expect("room id should encode");
        let public_user_id = public_id_codec
            .encode_user_id(UserId::expect_positive(301))
            .expect("user id should encode");
        let danmaku = try_bilibili_live_danmaku_for_static_media(
            &media,
            &public_user_id,
            &public_id_codec,
            Some(&signing_key),
            Some(expires_at),
        )
        .expect("bilibili live danmaku should encode")
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
        let public_id_codec = crate::PublicIdCodec::plain();
        let media = Media {
            id: MediaId::expect_positive(101),
            playlist_id: None,
            room_id: RoomId::expect_positive(102),
            creator_id: Some(UserId::expect_positive(103)),
            name: "Proto Media".to_string(),
            description: String::new(),
            position: 3.5,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({ "url": "https://example.com/video.mp4" }),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 42,
        };

        let proto =
            try_media_to_proto(&media, &public_id_codec).expect("media proto should encode");
        assert_eq!(proto.version, 42);
    }

    #[test]
    fn media_to_proto_only_includes_source_config_for_creator_viewer() {
        let public_id_codec = crate::PublicIdCodec::plain();
        let creator_id = UserId::expect_positive(103);
        let media = Media {
            id: MediaId::expect_positive(104),
            playlist_id: None,
            room_id: RoomId::expect_positive(102),
            creator_id: Some(creator_id),
            name: "Secret Media".to_string(),
            description: String::new(),
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
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        };

        let proto =
            try_media_to_proto(&media, &public_id_codec).expect("media proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "default media conversion must not expose source_config"
        );
        assert!(
            proto.metadata.is_empty(),
            "default media conversion must not expose metadata extracted from source_config"
        );

        let proto = try_media_to_proto_for_viewer(
            &media,
            true,
            Some(UserId::expect_positive(999)),
            &public_id_codec,
        )
        .expect("media proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "non-creator viewers must not receive source_config"
        );
        assert!(
            proto.metadata.is_empty(),
            "non-creator viewers must not receive metadata extracted from source_config"
        );

        let mut unowned_media = media.clone();
        unowned_media.creator_id = None;
        let proto = try_media_to_proto_for_viewer(&unowned_media, true, None, &public_id_codec)
            .expect("media proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "media without a creator must not expose source_config"
        );
        assert!(
            proto.metadata.is_empty(),
            "media without a creator must not expose metadata extracted from source_config"
        );

        let proto = try_media_to_proto_for_viewer(&media, true, Some(creator_id), &public_id_codec)
            .expect("media proto should encode");
        let source_config: serde_json::Value = serde_json::from_slice(&proto.source_config)
            .expect("proto source config should be JSON");
        let metadata: serde_json::Value =
            serde_json::from_slice(&proto.metadata).expect("proto metadata should be JSON");

        assert_eq!(source_config["token"], serde_json::json!("top-level-token"));
        assert_eq!(
            source_config["nested"]["password"],
            serde_json::json!("nested-password")
        );
        assert_eq!(
            source_config["items"][0]["api_key"],
            serde_json::json!("nested-api-key")
        );
        assert_eq!(source_config["nested"]["safe"], serde_json::json!(true));
        assert_eq!(
            source_config["metadata"]["title"],
            serde_json::json!("Secret Media")
        );
        assert_eq!(metadata["title"], serde_json::json!("Secret Media"));
    }

    #[test]
    fn playlist_to_proto_includes_resource_version() {
        let public_id_codec = crate::PublicIdCodec::plain();
        let playlist = synctv_core::models::Playlist {
            id: PlaylistId::expect_positive(105),
            room_id: RoomId::expect_positive(102),
            creator_id: Some(UserId::expect_positive(103)),
            name: "Proto Playlist".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 1.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 7,
        };

        let proto = try_playlist_to_proto(&playlist, 3, &public_id_codec)
            .expect("playlist proto should encode");
        assert_eq!(proto.version, 7);
    }

    #[test]
    fn proto_role_filter_rejects_unknown_values_and_preserves_unspecified_filter() {
        assert_eq!(
            proto_role_filter_to_room_role(
                synctv_proto::common::RoomMemberRole::Unspecified as i32
            )
            .expect("unspecified role filter should be accepted"),
            None
        );
        assert_eq!(
            proto_role_filter_to_room_role(synctv_proto::common::RoomMemberRole::Admin as i32)
                .expect("admin role filter should be accepted"),
            Some(synctv_core::models::RoomRole::Admin)
        );
        assert!(matches!(
            proto_role_filter_to_room_role(99),
            Err(crate::impls::ApiError::InvalidInput(message)) if message.contains("room member role")
        ));
    }

    #[test]
    fn playlist_to_proto_only_includes_source_config_for_creator_viewer() {
        let public_id_codec = crate::PublicIdCodec::plain();
        let creator_id = UserId::expect_positive(103);
        let playlist = synctv_core::models::Playlist {
            id: PlaylistId::expect_positive(106),
            room_id: RoomId::expect_positive(102),
            creator_id: Some(creator_id),
            name: "Secret Playlist".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 1.0,
            source_provider: Some("alist".to_string()),
            source_config: Some(serde_json::json!({
                "path": "/secret",
                "token": "playlist-token",
                "nested": {
                    "password": "nested-password"
                }
            })),
            provider_instance_name: Some("alist-main".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        };

        let proto = try_playlist_to_proto(&playlist, 3, &public_id_codec)
            .expect("playlist proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "default playlist conversion must not expose source_config"
        );
        assert_eq!(proto.source_provider, "alist");
        assert_eq!(proto.provider_instance_name, "alist-main");

        let proto = try_playlist_to_proto_for_viewer(
            &playlist,
            3,
            true,
            Some(UserId::expect_positive(999)),
            &public_id_codec,
        )
        .expect("playlist proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "non-creator viewers must not receive playlist source_config"
        );

        let mut unowned_playlist = playlist.clone();
        unowned_playlist.creator_id = None;
        let proto =
            try_playlist_to_proto_for_viewer(&unowned_playlist, 3, true, None, &public_id_codec)
                .expect("playlist proto should encode");
        assert!(
            proto.source_config.is_empty(),
            "playlist without a creator must not expose source_config"
        );

        let proto = try_playlist_to_proto_for_viewer(
            &playlist,
            3,
            true,
            Some(creator_id),
            &public_id_codec,
        )
        .expect("playlist proto should encode");
        let source_config: serde_json::Value = serde_json::from_slice(&proto.source_config)
            .expect("proto source config should be JSON");
        assert_eq!(source_config["token"], serde_json::json!("playlist-token"));
        assert_eq!(
            source_config["nested"]["password"],
            serde_json::json!("nested-password")
        );
    }

    #[test]
    fn room_to_proto_includes_resource_version() {
        let public_id_codec = crate::PublicIdCodec::plain();
        let now = chrono::Utc::now();
        let room = Room {
            id: RoomId::expect_positive(102),
            name: "Proto Room".to_string(),
            description: "Room description".to_string(),
            cover_file_reference_id: None,
            created_by: UserId::expect_positive(103),
            status: synctv_core::models::RoomStatus::Active,
            is_banned: false,
            closed_at: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 9,
            last_activity_at: now,
        };

        let proto = try_room_to_proto_basic(&room, None, Some(0), &public_id_codec)
            .expect("room proto should encode");
        assert_eq!(proto.version, 9);
    }
}
