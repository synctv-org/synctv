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
    "credential_ref",
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
        synctv_core::models::UserStatus::Pending => {
            synctv_proto::common::UserStatus::Pending as i32
        }
        synctv_core::models::UserStatus::Rejected => {
            synctv_proto::common::UserStatus::Rejected as i32
        }
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    }
}

pub(crate) const fn member_status_to_proto(status: synctv_core::models::MemberStatus) -> i32 {
    match status {
        synctv_core::models::MemberStatus::Active => {
            synctv_proto::common::MemberStatus::Active as i32
        }
        synctv_core::models::MemberStatus::Pending => {
            synctv_proto::common::MemberStatus::Pending as i32
        }
        synctv_core::models::MemberStatus::Rejected => {
            synctv_proto::common::MemberStatus::Rejected as i32
        }
        synctv_core::models::MemberStatus::Banned => {
            synctv_proto::common::MemberStatus::Banned as i32
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

pub(crate) fn user_to_proto(user: &synctv_core::models::User) -> crate::proto::client::User {
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

pub(crate) fn room_to_proto_basic(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
) -> crate::proto::client::Room {
    room_to_proto_with_availability(
        room,
        settings,
        member_count,
        ClientResourceAvailability::Available,
    )
}

pub(crate) fn room_to_proto_with_availability(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    availability: ClientResourceAvailability,
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
) -> crate::proto::client::RoomWithStats {
    crate::proto::client::RoomWithStats {
        room: Some(room_to_proto_basic(room, settings, Some(online_count))),
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
pub fn media_to_proto(media: &synctv_core::models::Media) -> crate::proto::client::Media {
    media_to_proto_with_availability(media, true)
}

pub fn media_to_proto_with_availability(
    media: &synctv_core::models::Media,
    is_available: bool,
) -> crate::proto::client::Media {
    // Extract metadata from source_config if present (any provider may store it)
    let metadata_bytes = media
        .source_config
        .get("metadata")
        .map(|m| serde_json::to_vec(m).unwrap_or_default())
        .unwrap_or_default();

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
        provider_instance_name: media.provider_instance_name.clone(),
        source_config: serialize_sanitized_source_config(&media.source_config),
        availability: resource_availability_to_proto(is_available),
        version: i64::from(media.version),
    }
}

pub(crate) fn playlist_to_proto(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
) -> crate::proto::client::Playlist {
    playlist_to_proto_with_availability(playlist, item_count, true)
}

pub(crate) fn playlist_to_proto_with_availability(
    playlist: &synctv_core::models::Playlist,
    item_count: i32,
    is_available: bool,
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
) -> crate::proto::client::PlaylistBrowsePathNode {
    crate::proto::client::PlaylistBrowsePathNode {
        playlist_id: playlist.id.as_str().to_string(),
        name: playlist.name.clone(),
        target: Vec::new(),
    }
}

pub(crate) fn playback_state_to_proto(
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
        target: state.target.clone(),
    }
}

pub(super) fn room_member_to_proto(
    member: &synctv_core::models::RoomMemberWithUser,
    role_default: synctv_core::models::PermissionBits,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: member.room_id.as_str().to_string(),
        user_id: member.user_id.as_str().to_string(),
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
) -> Vec<synctv_proto::common::RoomMember> {
    map_slice_preserve_order(members, |m| {
        let role_default =
            permission_service.calculate_role_default_permissions(&m.role, room_settings);
        room_member_to_proto(m, role_default)
    })
}

pub(crate) fn media_list_to_proto(
    media: &[synctv_core::models::Media],
) -> Vec<crate::proto::client::Media> {
    map_slice_preserve_order(media, media_to_proto)
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
        id: Some(media.id.clone()),
        playlist_id: media.playlist_id.clone(),
        room_id: media.room_id.clone(),
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
    let url = synctv_core::service::proxy_signature::build_signed_proxy_url(
        synctv_core::provider::BilibiliProvider::NAME,
        media.room_id.as_str(),
        &format!("{}/danmu", media.id.as_str()),
        signing_key,
        media.room_id.as_str(),
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
        media_to_proto, normalize_created_room_settings, playlist_to_proto,
        provider_playback_info_to_model, room_to_proto_basic, sign_local_bilibili_danmaku_urls,
        REDACTED_SOURCE_CONFIG_VALUE,
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
    fn direct_url_embedded_playback_result_preserves_rich_playback_fields() {
        let expire_at = chrono::DateTime::from_timestamp(1_700_000_100, 0)
            .expect("test timestamp should be valid");
        let media = Media {
            id: MediaId::from_string("media_embedded".to_string()),
            playlist_id: None,
            room_id: RoomId::from_string("room_embedded".to_string()),
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
            provider_instance_name: "direct_url".to_string(),
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
        assert_eq!(
            result.id.as_ref().map(synctv_core::models::MediaId::as_str),
            Some("media_embedded")
        );
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
            id: MediaId::from_string("media_live".to_string()),
            playlist_id: None,
            room_id: RoomId::from_string("room_live".to_string()),
            creator_id: None,
            name: "Bilibili Live".to_string(),
            position: 0.0,
            source_provider: synctv_core::provider::BilibiliProvider::NAME.to_string(),
            source_config: serde_json::json!({
                "type": "live",
                "room_id": 12345_u64,
                "credential_ref": {
                    "server_id": "srv-1",
                    "credential_owner_id": "owner-1"
                }
            }),
            provider_instance_name: "bilibili".to_string(),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };
        let signing_key = synctv_core::service::ProxySigningKey::derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        );

        let expires_at = chrono::Utc::now().timestamp() + 600;
        let danmaku = bilibili_live_danmaku_for_static_media(
            &media,
            "user-live",
            Some(&signing_key),
            Some(expires_at),
        )
        .expect("bilibili live should expose danmaku");

        assert_eq!(danmaku.name, "Bilibili Danmaku");
        assert_eq!(danmaku.format.as_deref(), Some("bilibili"));
        assert!(danmaku
            .url
            .starts_with("/api/providers/proxy/bilibili/room_live/"));
        let query = danmaku
            .url
            .split('?')
            .nth(1)
            .expect("signed danmaku URL should include query");
        let claims = signing_key
            .parse_and_verify_query(query, "bilibili", "room_live")
            .expect("signed danmaku query should verify");
        assert_eq!(claims.room_id, "room_live");
        assert_eq!(claims.user_id, "user-live");
        assert_eq!(claims.expires_at, expires_at);
    }

    #[test]
    fn media_to_proto_includes_resource_version() {
        let media = Media {
            id: MediaId::from_string("media_proto".to_string()),
            playlist_id: None,
            room_id: RoomId::from_string("room_proto".to_string()),
            creator_id: Some(UserId::from_string("user_proto".to_string())),
            name: "Proto Media".to_string(),
            position: 3.5,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({ "url": "https://example.com/video.mp4" }),
            provider_instance_name: "direct_url".to_string(),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 42,
        };

        let proto = media_to_proto(&media);
        assert_eq!(proto.version, 42);
    }

    #[test]
    fn media_to_proto_redacts_nested_credentials_without_cloning_sanitized_value() {
        let media = Media {
            id: MediaId::from_string("media_secret".to_string()),
            playlist_id: None,
            room_id: RoomId::from_string("room_proto".to_string()),
            creator_id: Some(UserId::from_string("user_proto".to_string())),
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
            provider_instance_name: "alist-main".to_string(),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        };

        let proto = media_to_proto(&media);
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
        let playlist = synctv_core::models::Playlist {
            id: PlaylistId::from_string("playlist_proto".to_string()),
            room_id: RoomId::from_string("room_proto".to_string()),
            creator_id: Some(UserId::from_string("user_proto".to_string())),
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

        let proto = playlist_to_proto(&playlist, 3);
        assert_eq!(proto.version, 7);
    }

    #[test]
    fn room_to_proto_includes_resource_version() {
        let now = chrono::Utc::now();
        let room = Room {
            id: RoomId::from_string("room_proto".to_string()),
            name: "Proto Room".to_string(),
            description: "Room description".to_string(),
            created_by: UserId::from_string("user_proto".to_string()),
            status: synctv_core::models::RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 9,
            last_activity_at: now,
        };

        let proto = room_to_proto_basic(&room, None, Some(0));
        assert_eq!(proto.version, 9);
    }
}
