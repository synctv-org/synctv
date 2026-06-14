use synctv_core::models::{
    ContentReportAdminRow, ContentReportStatus, ContentReportTargetType, RoomId, UserId, UserStatus,
};
use synctv_core::service::{
    BanRecordRow, RoomCreationReviewRecord, RoomJoinReviewRecord, UserRegistrationReviewRecord,
};

use super::{json_to_vec, user_status_to_proto, ApiError};

pub(in crate::impls::admin) fn public_id_encode_error(kind: &str, error: &str) -> ApiError {
    ApiError::Internal(format!("Failed to encode {kind} public id: {error}"))
}

pub(in crate::impls::admin) fn required_room_settings<'a>(
    settings: &'a std::collections::HashMap<RoomId, synctv_core::models::RoomSettings>,
    room_id: &RoomId,
) -> Result<&'a synctv_core::models::RoomSettings, ApiError> {
    settings.get(room_id).ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing room settings for room {room_id} in batch response"
        ))
    })
}

fn optional_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> i64 {
    value.map_or(0, |timestamp| timestamp.timestamp())
}

fn required_banned_at(user: &synctv_core::models::User) -> Result<i64, ApiError> {
    if !user.is_banned {
        return Ok(0);
    }

    user.banned_at
        .map(|value| value.timestamp())
        .ok_or_else(|| ApiError::Internal(format!("Banned user {} is missing banned_at", user.id)))
}

fn encode_optional_user_id(
    public_id_codec: &synctv_core::PublicIdCodec,
    id: Option<UserId>,
) -> Result<String, ApiError> {
    encode_optional_user_id_option(public_id_codec, id).map(std::option::Option::unwrap_or_default)
}

fn encode_optional_user_id_option(
    public_id_codec: &synctv_core::PublicIdCodec,
    id: Option<UserId>,
) -> Result<Option<String>, ApiError> {
    id.map(|id| {
        public_id_codec
            .encode_user_id(id)
            .map_err(ApiError::InvalidInput)
    })
    .transpose()
}

fn encode_optional_room_id(
    public_id_codec: &synctv_core::PublicIdCodec,
    id: Option<RoomId>,
) -> Result<String, ApiError> {
    id.map(|id| {
        public_id_codec
            .encode_room_id(id)
            .map_err(ApiError::InvalidInput)
    })
    .transpose()
    .map(std::option::Option::unwrap_or_default)
}

fn optional_i64_timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> i64 {
    value.map_or(0, |timestamp| timestamp.timestamp())
}

fn content_report_target_type_to_proto(value: ContentReportTargetType) -> i32 {
    match value {
        ContentReportTargetType::Room => synctv_proto::admin::ContentReportTargetType::Room as i32,
        ContentReportTargetType::User => synctv_proto::admin::ContentReportTargetType::User as i32,
        ContentReportTargetType::RoomMember => {
            synctv_proto::admin::ContentReportTargetType::RoomMember as i32
        }
        ContentReportTargetType::ChatMessage => {
            synctv_proto::admin::ContentReportTargetType::ChatMessage as i32
        }
    }
}

fn content_report_status_to_proto(value: ContentReportStatus) -> i32 {
    match value {
        ContentReportStatus::Open => synctv_proto::admin::ContentReportStatus::Open as i32,
        ContentReportStatus::Reviewing => {
            synctv_proto::admin::ContentReportStatus::Reviewing as i32
        }
        ContentReportStatus::Resolved => synctv_proto::admin::ContentReportStatus::Resolved as i32,
        ContentReportStatus::Dismissed => {
            synctv_proto::admin::ContentReportStatus::Dismissed as i32
        }
    }
}

pub(crate) fn content_report_row_to_proto(
    row: &ContentReportAdminRow,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::ContentReport, ApiError> {
    Ok(synctv_proto::admin::ContentReport {
        id: public_id_codec
            .encode_content_report_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        reporter_user_id: public_id_codec
            .encode_user_id(row.reporter_user_id)
            .map_err(ApiError::InvalidInput)?,
        reporter_username: row.reporter_username.clone(),
        room_id: encode_optional_room_id(public_id_codec, row.room_id)?,
        room_name: row.room_name.clone(),
        target_type: content_report_target_type_to_proto(row.target_type),
        target_room_id: encode_optional_room_id(public_id_codec, row.target_room_id)?,
        target_room_name: row.target_room_name.clone(),
        target_user_id: encode_optional_user_id(public_id_codec, row.target_user_id)?,
        target_username: row.target_username.clone(),
        target_member_room_id: encode_optional_room_id(public_id_codec, row.target_member_room_id)?,
        target_member_room_name: row.target_member_room_name.clone(),
        target_member_user_id: encode_optional_user_id(public_id_codec, row.target_member_user_id)?,
        target_member_username: row.target_member_username.clone(),
        target_chat_message_id: row.target_chat_message_id.unwrap_or_default(),
        target_chat_message_created_at: optional_i64_timestamp(row.target_chat_message_created_at),
        target_chat_message_preview: row.target_chat_message_preview.clone(),
        reason_code: row.reason_code.clone(),
        reason: row.reason.clone(),
        metadata: json_to_vec(&row.metadata, "content report metadata")?,
        status: content_report_status_to_proto(row.status),
        reviewed_by: encode_optional_user_id(public_id_codec, row.reviewed_by)?,
        reviewed_by_username: row.reviewed_by_username.clone(),
        reviewed_at: optional_timestamp(row.reviewed_at),
        resolution_note: row.resolution_note.clone(),
        created_at: row.created_at.timestamp(),
        updated_at: row.updated_at.timestamp(),
    })
}

pub(in crate::impls::admin) fn user_registration_review_row_to_proto(
    row: &UserRegistrationReviewRecord,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::UserRegistrationReview, ApiError> {
    Ok(synctv_proto::admin::UserRegistrationReview {
        id: public_id_codec
            .encode_user_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        username: row.username.clone(),
        email: row.email.clone(),
        signup_method: i32::from(i16::from(row.signup_method)),
        status: i32::from(row.status),
        requested_at: row.requested_at.timestamp(),
        reviewed_at: optional_timestamp(row.reviewed_at),
        reviewed_by: encode_optional_user_id_option(public_id_codec, row.reviewed_by)?,
        rejection_reason: row.rejection_reason.clone(),
        oauth2_provider: row.oauth2_provider.clone(),
        oauth2_provider_instance_name: row.oauth2_provider_instance_name.clone(),
        oauth2_provider_issuer: row.oauth2_provider_issuer.clone(),
        oauth2_provider_user_id: row.oauth2_provider_user_id.clone(),
        oauth2_provider_username: row.oauth2_provider_username.clone(),
        oauth2_avatar_url: row.oauth2_avatar_url.clone(),
        oauth2_email_trusted: row.oauth2_email_trusted,
        webauthn_credential_id: row
            .webauthn_credential_id
            .as_deref()
            .map(synctv_core::service::PasskeyService::encode_credential_id),
        webauthn_credential_name: row.webauthn_credential_name.clone(),
    })
}

pub(in crate::impls::admin) fn room_creation_review_row_to_proto(
    row: &RoomCreationReviewRecord,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::RoomCreationReview, ApiError> {
    Ok(synctv_proto::admin::RoomCreationReview {
        id: public_id_codec
            .encode_room_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        requested_by: public_id_codec
            .encode_user_id(row.requested_by)
            .map_err(ApiError::InvalidInput)?,
        requested_by_username: row.requested_by_username.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        status: i32::from(row.status),
        requested_at: row.requested_at.timestamp(),
        reviewed_at: optional_timestamp(row.reviewed_at),
        reviewed_by: encode_optional_user_id_option(public_id_codec, row.reviewed_by)?,
        rejection_reason: row.rejection_reason.clone(),
    })
}

pub(in crate::impls::admin) fn room_join_review_row_to_proto(
    row: &RoomJoinReviewRecord,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::RoomJoinReview, ApiError> {
    Ok(synctv_proto::admin::RoomJoinReview {
        id: public_id_codec
            .encode_review_request_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        room_id: public_id_codec
            .encode_room_id(row.room_id)
            .map_err(ApiError::InvalidInput)?,
        room_name: row.room_name.clone(),
        user_id: public_id_codec
            .encode_user_id(row.user_id)
            .map_err(ApiError::InvalidInput)?,
        username: row.username.clone(),
        requested_role: row.requested_role,
        status: i32::from(row.status),
        requested_at: row.requested_at.timestamp(),
        reviewed_at: optional_timestamp(row.reviewed_at),
        reviewed_by: encode_optional_user_id_option(public_id_codec, row.reviewed_by)?,
        rejection_reason: row.rejection_reason.clone(),
    })
}

pub(in crate::impls::admin) fn ban_row_to_proto(
    row: &BanRecordRow,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::BanRecord, ApiError> {
    Ok(synctv_proto::admin::BanRecord {
        id: public_id_codec
            .encode_ban_record_id(row.id)
            .map_err(ApiError::InvalidInput)?,
        target_type: row.target_type,
        user_id: encode_optional_user_id(public_id_codec, row.user_id)?,
        username: row.username.clone(),
        room_id: encode_optional_room_id(public_id_codec, row.room_id)?,
        room_name: row.room_name.clone(),
        banned_by: encode_optional_user_id(public_id_codec, row.banned_by)?,
        banned_by_username: row.banned_by_username.clone(),
        reason: row.reason.clone(),
        starts_at: row.starts_at.timestamp(),
        ends_at: optional_timestamp(row.ends_at),
        revoked_at: optional_timestamp(row.revoked_at),
        revoked_by: encode_optional_user_id(public_id_codec, row.revoked_by)?,
        is_active: row.is_active,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::impls::admin) fn try_admin_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    creator_username: Option<&str>,
    creator_status: UserStatus,
    creator_avatar_url: Option<&str>,
    cover: Option<&synctv_core::models::StoredFileReference>,
    cover_url: Option<&str>,
    presence: Option<&synctv_core::service::OnlineRoomStats>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::AdminRoom, ApiError> {
    let room_settings = settings.ok_or_else(|| {
        ApiError::Internal(format!("Missing room settings for admin room {}", room.id))
    })?;
    let creator_username = creator_username.ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing creator username for room {} created_by {}",
            room.id, room.created_by
        ))
    })?;
    let member_count = member_count.ok_or_else(|| {
        ApiError::Internal(format!("Missing member count for admin room {}", room.id))
    })?;
    Ok(synctv_proto::admin::AdminRoom {
        id: public_id_codec
            .encode_room_id(room.id)
            .map_err(|error| ApiError::Internal(format!("Failed to encode room id: {error}")))?,
        name: room.name.clone(),
        description: room.description.clone(),
        creator_id: public_id_codec
            .encode_user_id(room.created_by)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room creator id: {error}"))
            })?,
        creator_username: creator_username.to_string(),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: json_to_vec(room_settings, "admin room settings")?,
        member_count,
        created_at: room.created_at.timestamp(),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        creator_status: user_status_to_proto(creator_status),
        version: i64::from(room.version),
        presence: presence
            .map(crate::impls::client::convert::room_presence_stats_to_proto)
            .transpose()?,
        creator_avatar_url: creator_avatar_url.unwrap_or_default().to_string(),
        cover: cover
            .map(|file| {
                crate::impls::client::convert::stored_file_reference_to_resource_cover(
                    file, cover_url,
                )
            })
            .transpose()?,
    })
}

pub(in crate::impls::admin) fn try_admin_room_member_to_proto_with_settings(
    member: &synctv_core::models::RoomMemberWithUser,
    room_settings: &synctv_core::models::RoomSettings,
    permission_service: &synctv_core::service::PermissionService,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::common::RoomMember, ApiError> {
    let permissions =
        permission_service.effective_member_with_user_permissions(member, room_settings);
    try_admin_room_member_to_proto_with_permissions(member, permissions, public_id_codec)
}

pub(in crate::impls::admin) fn try_admin_room_member_to_proto_with_permissions(
    member: &synctv_core::models::RoomMemberWithUser,
    permissions: synctv_core::models::RoomPermissionSet,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::common::RoomMember, ApiError> {
    Ok(synctv_proto::common::RoomMember {
        room_id: public_id_codec
            .encode_room_id(member.room_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room member room id: {error}"))
            })?,
        user_id: public_id_codec
            .encode_user_id(member.user_id)
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode room member user id: {error}"))
            })?,
        username: member.username.clone(),
        role: crate::impls::client::room_role_to_proto(member.role),
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

pub(in crate::impls::admin) fn try_admin_user_to_proto(
    user: &synctv_core::models::User,
    email: Option<&str>,
    presence: Option<&synctv_core::service::OnlineUserStats>,
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<synctv_proto::admin::AdminUser, ApiError> {
    Ok(synctv_proto::admin::AdminUser {
        id: public_id_codec
            .encode_user_id(user.id)
            .map_err(|error| ApiError::Internal(format!("Failed to encode user id: {error}")))?,
        username: user.username.clone(),
        email: email.unwrap_or_default().to_string(),
        role: i32::from(user.role),
        status: i32::from(user.status),
        created_at: user.created_at.timestamp(),
        updated_at: user.updated_at.timestamp(),
        is_banned: user.is_banned,
        banned_at: required_banned_at(user)?,
        banned_by: user
            .banned_by
            .map(|id| public_id_codec.encode_user_id(id))
            .transpose()
            .map_err(|error| {
                ApiError::Internal(format!("Failed to encode banned_by user id: {error}"))
            })?
            .unwrap_or_default(),
        banned_reason: user.banned_reason.clone().unwrap_or_default(),
        avatar_url: String::new(),
        presence: presence
            .map(|stats| {
                crate::impls::client::convert::user_presence_stats_to_proto(stats, public_id_codec)
            })
            .transpose()?,
    })
}
