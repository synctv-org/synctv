use crate::impls::ApiError;
use std::net::IpAddr;
use synctv_core::models::{
    ChatMessageEvent, ChatMessageSelection, ChatMessageWithAttachments, DeleteChatMessage,
    EditChatMessage, FileBlob, FileUploadSession, NewStoredFile, UserId,
};

use super::super::convert::{chat_message_selection_from_proto_values, chat_metadata_from_proto};
#[cfg(test)]
use super::super::media::required_stored_file_fields;
use super::super::media::upload_session_fields;
use super::super::ClientApiImpl;

pub(super) fn runtime_settings_store_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Public settings are not available on this server.".to_string())
}

pub(super) fn chat_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Chat service is not available on this server.".to_string())
}

pub(super) fn parse_optional_client_ip(
    client_ip: Option<&str>,
) -> Result<Option<IpAddr>, ApiError> {
    client_ip
        .map(|ip| {
            ip.parse::<IpAddr>().map_err(|error| {
                ApiError::InvalidInput(format!("Invalid client IP address '{ip}': {error}"))
            })
        })
        .transpose()
}

pub(super) fn required_room_settings<'a>(
    settings: &'a std::collections::HashMap<
        synctv_core::models::RoomId,
        synctv_core::models::RoomSettings,
    >,
    room_id: &synctv_core::models::RoomId,
) -> Result<&'a synctv_core::models::RoomSettings, ApiError> {
    settings.get(room_id).ok_or_else(|| {
        ApiError::Internal(format!(
            "Missing room settings for room {room_id} in batch response"
        ))
    })
}

pub(super) fn proto_room_status_filter(
    value: i32,
) -> Result<Option<synctv_core::models::RoomStatus>, ApiError> {
    if value == synctv_proto::common::RoomStatus::Unspecified as i32 {
        return Ok(None);
    }
    synctv_core::models::RoomStatus::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::InvalidInput("Unsupported room status".to_string()))
}

pub(super) fn proto_room_list_sort_by(
    value: i32,
) -> Result<synctv_core::models::RoomListSortBy, ApiError> {
    match synctv_proto::client::RoomListSortBy::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported room list sort field".to_string()))?
    {
        synctv_proto::client::RoomListSortBy::Unspecified
        | synctv_proto::client::RoomListSortBy::CreatedAt => {
            Ok(synctv_core::models::RoomListSortBy::CreatedAt)
        }
        synctv_proto::client::RoomListSortBy::Name => Ok(synctv_core::models::RoomListSortBy::Name),
        synctv_proto::client::RoomListSortBy::UpdatedAt => {
            Ok(synctv_core::models::RoomListSortBy::UpdatedAt)
        }
        synctv_proto::client::RoomListSortBy::LastActivityAt => {
            Ok(synctv_core::models::RoomListSortBy::LastActivityAt)
        }
    }
}

pub(super) fn proto_my_room_relation(
    value: i32,
) -> Result<synctv_core::models::MyRoomRelation, ApiError> {
    match synctv_proto::client::MyRoomRelation::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported room relation".to_string()))?
    {
        synctv_proto::client::MyRoomRelation::Unspecified
        | synctv_proto::client::MyRoomRelation::All => Ok(synctv_core::models::MyRoomRelation::All),
        synctv_proto::client::MyRoomRelation::Created => {
            Ok(synctv_core::models::MyRoomRelation::Created)
        }
        synctv_proto::client::MyRoomRelation::Participating => {
            Ok(synctv_core::models::MyRoomRelation::Participating)
        }
    }
}

pub(super) fn proto_my_room_list_sort_by(
    value: i32,
) -> Result<synctv_core::models::MyRoomListSortBy, ApiError> {
    match synctv_proto::client::MyRoomListSortBy::try_from(value).map_err(|_| {
        ApiError::InvalidInput("Unsupported related room list sort field".to_string())
    })? {
        synctv_proto::client::MyRoomListSortBy::Unspecified
        | synctv_proto::client::MyRoomListSortBy::JoinedAt => {
            Ok(synctv_core::models::MyRoomListSortBy::JoinedAt)
        }
        synctv_proto::client::MyRoomListSortBy::Name => {
            Ok(synctv_core::models::MyRoomListSortBy::Name)
        }
        synctv_proto::client::MyRoomListSortBy::CreatedAt => {
            Ok(synctv_core::models::MyRoomListSortBy::CreatedAt)
        }
        synctv_proto::client::MyRoomListSortBy::UpdatedAt => {
            Ok(synctv_core::models::MyRoomListSortBy::UpdatedAt)
        }
        synctv_proto::client::MyRoomListSortBy::LastActivityAt => {
            Ok(synctv_core::models::MyRoomListSortBy::LastActivityAt)
        }
    }
}

pub(super) fn proto_sort_direction(
    value: i32,
    default: synctv_core::models::SortDirection,
) -> Result<synctv_core::models::SortDirection, ApiError> {
    match synctv_proto::client::SortDirection::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::client::SortDirection::Unspecified => Ok(default),
        synctv_proto::client::SortDirection::Asc => Ok(synctv_core::models::SortDirection::Asc),
        synctv_proto::client::SortDirection::Desc => Ok(synctv_core::models::SortDirection::Desc),
    }
}

const DEFAULT_ROOM_PAGE: u32 = 1;
const DEFAULT_ROOM_PAGE_SIZE: u32 = 20;
const MAX_ROOM_PAGE_SIZE: u32 = 100;
pub(super) const DEFAULT_HOT_ROOM_LIMIT: i64 = 10;
pub(super) const DEFAULT_HOT_ROOM_LIMIT_USIZE: usize = 10;

pub(super) fn validate_room_password_for_set(password: &str) -> Result<(), ApiError> {
    let char_count = password.trim().chars().count();
    if char_count < synctv_core::validation::ROOM_PASSWORD_MIN {
        return Err(ApiError::InvalidInput(format!(
            "Room password must be at least {} characters",
            synctv_core::validation::ROOM_PASSWORD_MIN
        )));
    }
    if char_count > synctv_core::validation::ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput(format!(
            "Room password must not exceed {} characters",
            synctv_core::validation::ROOM_PASSWORD_MAX
        )));
    }
    Ok(())
}

pub(super) fn validate_room_password_for_verify(password: &str) -> Result<(), ApiError> {
    let char_count = password.chars().count();
    if char_count == 0 || char_count > synctv_core::validation::ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput("Invalid room password".to_string()));
    }
    Ok(())
}

pub(super) fn positive_i32_to_u32(value: i32, default: u32) -> u32 {
    if value > 0 {
        value.cast_unsigned()
    } else {
        default
    }
}

pub(super) fn positive_i32(value: i32, default: i32) -> i32 {
    if value > 0 {
        value
    } else {
        default
    }
}

pub(super) fn optional_positive_window_seconds(
    value: f64,
    default: f64,
    field: &str,
) -> Result<f64, ApiError> {
    if value == 0.0 {
        return Ok(default);
    }
    if !value.is_finite() || value < 0.0 {
        return Err(ApiError::InvalidInput(format!(
            "{field} must be a finite non-negative number"
        )));
    }
    Ok(value)
}

pub(super) fn optional_positive_limit(
    value: i32,
    default: i32,
    max: i32,
    field: &str,
) -> Result<i32, ApiError> {
    if value == 0 {
        return Ok(default);
    }
    if value < 0 {
        return Err(ApiError::InvalidInput(format!(
            "{field} must be a positive integer"
        )));
    }
    if value > max {
        return Err(ApiError::InvalidInput(format!(
            "{field} must be at most {max}"
        )));
    }
    Ok(value)
}

pub(super) fn required_playback_position_seconds(value: f64) -> Result<f64, ApiError> {
    if !value.is_finite() || value < 0.0 {
        return Err(ApiError::InvalidInput(
            "position_seconds must be a finite non-negative number".to_string(),
        ));
    }
    Ok(value)
}

pub(super) fn positive_i64_to_usize(
    value: i64,
    default: usize,
    field: &'static str,
) -> Result<usize, ApiError> {
    let value = if value <= 0 {
        i64::try_from(default)
            .map_err(|_| ApiError::Internal(format!("{field} default exceeds i64::MAX")))?
    } else {
        value
    };
    usize::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

pub(super) fn usize_to_i32_api(value: usize, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub(super) fn i64_to_i32_api(value: i64, field: &'static str) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub(super) async fn username_for_chat_message(
    api: &ClientApiImpl,
    message: &synctv_core::models::ChatMessage,
) -> Result<String, ApiError> {
    let Some(user_id) = message.user_id else {
        return Ok("[deleted]".to_string());
    };
    api.user_service
        .get_username(&user_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::NotFound("Chat message author not found".to_string()))
}

#[cfg(test)]
pub(super) fn new_chat_attachment_to_proto(
    attachment: &NewStoredFile,
) -> Result<synctv_proto::client::ChatAttachment, ApiError> {
    let fields = required_stored_file_fields(attachment, "chat attachment metadata")?;
    Ok(synctv_proto::client::ChatAttachment {
        id: attachment.id.clone(),
        url: fields.url,
        object_access: fields.object_access,
        mime_type: fields.mime_type,
        size_bytes: fields.size_bytes,
        width: fields.width,
        height: fields.height,
        metadata: fields.metadata,
        filename: attachment.filename.clone().unwrap_or_default(),
        kind: if attachment
            .mime_type
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .starts_with("image/")
        {
            synctv_proto::client::ChatAttachmentKind::Image
        } else if attachment
            .mime_type
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .starts_with("audio/")
        {
            synctv_proto::client::ChatAttachmentKind::Audio
        } else {
            synctv_proto::client::ChatAttachmentKind::File
        } as i32,
        reuse_token: String::new(),
        reuse_expires_at: None,
        variants: crate::impls::client::convert::file_object_variants_from_metadata(
            &attachment.metadata,
            "chat attachment",
        )?,
    })
}

pub(super) fn upload_session_chat_attachment_to_proto(
    attachment: &NewStoredFile,
) -> Result<synctv_proto::client::ChatAttachmentReference, ApiError> {
    let reference = synctv_core::service::submitted_file_reference_from_session_file(attachment)
        .map_err(ApiError::from)?;
    Ok(synctv_proto::client::ChatAttachmentReference {
        id: reference.id,
        kind: synctv_proto::client::ChatAttachmentReferenceKind::Upload as i32,
    })
}

pub(super) fn chat_message_to_proto(
    api: &ClientApiImpl,
    message: &ChatMessageWithAttachments,
    username: String,
) -> Result<synctv_proto::client::ChatMessageReceive, ApiError> {
    crate::impls::messaging::chat_message_receive_to_proto(message, &api.public_id_codec, username)
        .map_err(ApiError::Internal)
}

pub(crate) fn chat_reaction_summary_to_proto(
    reaction: &synctv_core::models::ChatReactionSummary,
) -> Result<synctv_proto::client::ChatReactionSummary, ApiError> {
    let key = reaction.key.trim();
    if key.is_empty() {
        return Err(ApiError::Internal(
            "chat reaction summary key is empty".to_string(),
        ));
    }
    if reaction.count < 0 {
        return Err(ApiError::Internal(format!(
            "chat reaction summary '{}' has negative count",
            reaction.key
        )));
    }
    Ok(synctv_proto::client::ChatReactionSummary {
        key: key.to_string(),
        count: reaction.count,
        reacted_by_me: reaction.reacted_by_me,
    })
}

pub(crate) fn chat_reaction_count(
    reactions: &[synctv_proto::client::ChatReactionSummary],
) -> Result<i32, ApiError> {
    reactions
        .iter()
        .try_fold(0_i64, |total, reaction| {
            if reaction.count < 0 {
                return Err(ApiError::Internal(format!(
                    "chat reaction summary '{}' has negative count",
                    reaction.key
                )));
            }
            total.checked_add(reaction.count).ok_or_else(|| {
                ApiError::Internal("chat reaction count exceeds i64::MAX".to_string())
            })
        })?
        .try_into()
        .map_err(|_| ApiError::Internal("chat reaction count exceeds i32::MAX".to_string()))
}

pub(super) async fn chat_event_to_proto(
    api: &ClientApiImpl,
    event: ChatMessageEvent,
) -> Result<synctv_proto::client::ChatMessageEvent, ApiError> {
    let username = username_for_chat_message(api, &event.message.message).await?;
    let room_id = api
        .public_id_codec
        .encode_room_id(event.room_id)
        .map_err(|error| {
            ApiError::Internal(format!("Failed to encode chat event room id: {error}"))
        })?;
    Ok(synctv_proto::client::ChatMessageEvent {
        event_id: event.event_id,
        room_id,
        kind: crate::impls::messaging::chat_event_kind_to_proto(event.kind) as i32,
        message: Some(chat_message_to_proto(api, &event.message, username)?),
        occurred_at: event.occurred_at.timestamp(),
        sequence: event.sequence,
    })
}

pub(super) async fn chat_pin_event_to_proto(
    api: &ClientApiImpl,
    event: synctv_core::models::ChatPinEvent,
) -> Result<synctv_proto::client::ChatPinEvent, ApiError> {
    let username = username_for_chat_message(api, &event.message.message).await?;
    let mut proto = crate::impls::messaging::chat_pin_event_to_proto(&event, &api.public_id_codec)
        .map_err(ApiError::Internal)?;
    if let Some(message) = proto.message.as_mut() {
        message.username = username;
    }
    Ok(proto)
}

pub(super) fn optional_chat_expected_version(raw: i64) -> Result<Option<i64>, ApiError> {
    if raw < 0 {
        return Err(ApiError::InvalidInput(
            "expected_version must be non-negative".to_string(),
        ));
    }
    Ok((raw > 0).then_some(raw))
}

pub(super) fn optional_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(super) fn parse_chat_message_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidInput("Invalid chat message id".to_string()))
}

pub(crate) fn parse_proto_chat_attachments(
    attachments: &[synctv_proto::client::ChatAttachmentReference],
) -> Result<Vec<synctv_core::models::SubmittedFileReference>, ApiError> {
    attachments
        .iter()
        .map(|attachment| {
            let kind =
                match synctv_proto::client::ChatAttachmentReferenceKind::try_from(attachment.kind)
                    .map_err(|_| {
                    ApiError::InvalidInput("Unsupported chat attachment reference kind".to_string())
                })? {
                    synctv_proto::client::ChatAttachmentReferenceKind::Unspecified
                    | synctv_proto::client::ChatAttachmentReferenceKind::Upload => {
                        synctv_core::models::SubmittedFileReferenceKind::Upload
                    }
                    synctv_proto::client::ChatAttachmentReferenceKind::Reuse => {
                        synctv_core::models::SubmittedFileReferenceKind::Reuse
                    }
                };
            Ok(synctv_core::models::SubmittedFileReference {
                id: attachment.id.clone(),
                kind,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()
}

pub(super) fn upload_session_to_proto(
    session: FileUploadSession,
) -> Result<synctv_proto::client::ChatAttachmentUploadSession, ApiError> {
    let fields = upload_session_fields(&session)?;
    Ok(synctv_proto::client::ChatAttachmentUploadSession {
        attachment_reference: Some(upload_session_chat_attachment_to_proto(&session.file)?),
        upload_required: session.upload_required,
        upload_url: fields.upload_url,
        upload_object_access: fields.upload_object_access,
        upload_method: fields.upload_method,
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: fields.expires_at,
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: fields.ownership_proof_nonce,
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(
                |range| synctv_proto::client::ChatAttachmentOwnershipProofRange {
                    offset: range.offset,
                    length: range.length,
                },
            )
            .collect(),
        resumable: session.resumable,
        part_size_bytes: session.part_size_bytes,
        uploaded_size_bytes: session.uploaded_size_bytes,
        uploaded_parts: session.uploaded_parts,
        upload_id: session.upload_id,
        part_urls: session
            .part_urls
            .into_iter()
            .map(|part_url| synctv_proto::client::FileUploadPartUrl {
                part_number: part_url.part_number,
                offset_bytes: part_url.offset_bytes,
                size_bytes: part_url.size_bytes,
                upload_url: part_url.upload_url,
                upload_method: part_url.upload_method,
                upload_headers: part_url.upload_headers.into_iter().collect(),
                expires_at: part_url.expires_at.map(|expires_at| expires_at.timestamp()),
            })
            .collect(),
        upload_token: fields.upload_token,
        encoded_object_key: session.encoded_object_key,
    })
}

pub(super) fn chat_attachment_upload_create_result_to_proto(
    result: synctv_core::models::FileUploadSessionCreateResult,
) -> Result<synctv_proto::client::CreateChatAttachmentUploadSessionResponse, ApiError> {
    use synctv_proto::client::create_chat_attachment_upload_session_response::Result as ProtoResult;
    Ok(
        synctv_proto::client::CreateChatAttachmentUploadSessionResponse {
            result: Some(match result {
                synctv_core::models::FileUploadSessionCreateResult::Plan(plan) => {
                    ProtoResult::Plan(super::super::media::file_upload_plan_to_proto(plan))
                }
                synctv_core::models::FileUploadSessionCreateResult::Session(session) => {
                    ProtoResult::Session(upload_session_to_proto(session)?)
                }
            }),
        },
    )
}

pub(super) fn chat_attachment_object_to_proto(
    room_id: &str,
    blob: &FileBlob,
) -> synctv_proto::client::ChatAttachmentObjectResponse {
    synctv_proto::client::ChatAttachmentObjectResponse {
        room_id: room_id.to_string(),
        mime_type: blob.mime_type.clone(),
        content_manifest_sha256: blob.content_manifest_sha256.clone(),
        data: blob.data.clone(),
        content_range: blob
            .range
            .map(super::super::media::file_byte_range_to_proto),
        total_size_bytes: blob.total_size_bytes,
    }
}

pub(super) fn edit_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: synctv_proto::client::EditChatMessageRequest,
) -> Result<EditChatMessage, ApiError> {
    Ok(EditChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
        content: req.content,
        metadata: chat_metadata_from_proto(req.metadata.as_ref())?,
        expected_version: optional_chat_expected_version(req.expected_version)?,
    })
}

pub(super) fn delete_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: &synctv_proto::client::DeleteChatMessageRequest,
) -> Result<DeleteChatMessage, ApiError> {
    Ok(DeleteChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
        reason: optional_trimmed_string(&req.reason),
        expected_version: optional_chat_expected_version(req.expected_version)?,
    })
}

pub(super) fn pin_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: &synctv_proto::client::PinChatMessageRequest,
) -> Result<synctv_core::models::PinChatMessage, ApiError> {
    Ok(synctv_core::models::PinChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
        note: optional_trimmed_string(&req.note),
    })
}

pub(super) fn unpin_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: &synctv_proto::client::UnpinChatMessageRequest,
) -> Result<synctv_core::models::UnpinChatMessage, ApiError> {
    Ok(synctv_core::models::UnpinChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
    })
}

pub(super) async fn chat_pinned_message_to_proto(
    api: &ClientApiImpl,
    pinned: synctv_core::models::ChatPinnedMessage,
) -> Result<synctv_proto::client::ChatPinnedMessage, ApiError> {
    let username = username_for_chat_message(api, &pinned.message.message).await?;
    let pinned_by_user_id = pinned
        .pin
        .pinned_by
        .map(|id| {
            api.public_id_codec.encode_user_id(id).map_err(|error| {
                ApiError::Internal(format!("Failed to encode chat pinned_by user id: {error}"))
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(synctv_proto::client::ChatPinnedMessage {
        message: Some(chat_message_to_proto(api, &pinned.message, username)?),
        pinned_by_user_id,
        pinned_by_username: pinned.pin.pinned_by_username.unwrap_or_default(),
        note: pinned.pin.note.unwrap_or_default(),
        pinned_at: pinned.pin.pinned_at.timestamp(),
    })
}

pub(super) fn chat_read_state_to_proto(
    api: &ClientApiImpl,
    state: synctv_core::models::ChatReadStateWithUnread,
) -> Result<synctv_proto::client::ChatReadStateResponse, ApiError> {
    let room_id = api
        .public_id_codec
        .encode_room_id(state.state.room_id)
        .map_err(|error| {
            ApiError::Internal(format!("Failed to encode chat read state room id: {error}"))
        })?;
    let user_id = api
        .public_id_codec
        .encode_user_id(state.state.user_id)
        .map_err(|error| {
            ApiError::Internal(format!("Failed to encode chat read state user id: {error}"))
        })?;
    Ok(synctv_proto::client::ChatReadStateResponse {
        state: Some(synctv_proto::client::ChatReadState {
            room_id,
            user_id,
            last_read_message_id: state
                .state
                .last_read_message_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            last_read_event_id: state.state.last_read_event_id.unwrap_or_default(),
            last_read_event_sequence: state.state.last_read_event_sequence.unwrap_or_default(),
            updated_at: state.state.updated_at.timestamp(),
        }),
        unread_count: state.unread_count,
    })
}

pub(super) async fn chat_message_read_receipts_to_proto(
    api: &ClientApiImpl,
    page: synctv_core::models::ChatMessageReadReceiptsPage,
) -> Result<synctv_proto::client::GetChatMessageReadReceiptsResponse, ApiError> {
    let mut readers = Vec::with_capacity(page.readers.len());
    for reader in page.readers {
        readers.push(synctv_proto::client::ChatMessageReadReceiptUser {
            user: Some(
                api.user_public_view_with_loaded_avatar(&reader.user)
                    .await?,
            ),
            read_at: reader.read_at.timestamp(),
        });
    }
    let mut unread_members = Vec::with_capacity(page.unread_members.len());
    for member in page.unread_members {
        unread_members.push(synctv_proto::client::ChatMessageUnreadMember {
            user: Some(
                api.user_public_view_with_loaded_avatar(&member.user)
                    .await?,
            ),
        });
    }
    Ok(synctv_proto::client::GetChatMessageReadReceiptsResponse {
        readers,
        unread_members,
        reader_total: page.reader_total,
        unread_total: page.unread_total,
    })
}

pub(super) fn build_public_room_list_query(
    req: synctv_proto::client::ListRoomsRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_core::models::RoomListQuery, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    let page = positive_i32_to_u32(req.page, DEFAULT_ROOM_PAGE);
    let page_size = if req.page_size > 0 {
        req.page_size.cast_unsigned().min(MAX_ROOM_PAGE_SIZE)
    } else {
        DEFAULT_ROOM_PAGE_SIZE
    };

    Ok(synctv_core::models::RoomListQuery {
        pagination: synctv_core::models::PageParams::new(Some(page), Some(page_size)),
        search: (!req.search.is_empty()).then_some(req.search),
        status: Some(synctv_core::models::RoomStatus::Active),
        is_banned: Some(false),
        category_id: parse_optional_room_category_id(&req.category_id, public_id_codec)?,
        label_ids: parse_room_label_ids(&req.label_ids, public_id_codec)?,
        sort_by: proto_room_list_sort_by(req.sort_by)?,
        sort_direction: proto_sort_direction(
            req.sort_direction,
            synctv_core::models::SortDirection::Desc,
        )?,
        ..Default::default()
    })
}

pub(crate) fn parse_optional_room_category_id(
    value: &str,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<Option<synctv_core::models::RoomCategoryId>, ApiError> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    public_id_codec
        .decode_room_category_id(value)
        .map(Some)
        .map_err(ApiError::InvalidInput)
}

pub(crate) fn parse_required_room_category_id(
    value: &str,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_core::models::RoomCategoryId, ApiError> {
    parse_optional_room_category_id(value, public_id_codec)?
        .ok_or_else(|| ApiError::InvalidInput("category_id is required".to_string()))
}

pub(crate) fn parse_room_label_ids(
    values: &[String],
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<Vec<synctv_core::models::RoomLabelId>, ApiError> {
    values
        .iter()
        .map(|value| {
            public_id_codec
                .decode_room_label_id(value)
                .map_err(ApiError::InvalidInput)
        })
        .collect()
}

pub(super) fn build_my_room_list_query(
    req: synctv_proto::client::ListMyRoomsRequest,
) -> Result<synctv_core::models::MyRoomListQuery, ApiError> {
    crate::impls::validate_proto_request(&req)?;

    let page = positive_i32_to_u32(req.page, DEFAULT_ROOM_PAGE);
    let page_size = if req.page_size > 0 {
        req.page_size.cast_unsigned().min(MAX_ROOM_PAGE_SIZE)
    } else {
        DEFAULT_ROOM_PAGE_SIZE
    };

    Ok(synctv_core::models::MyRoomListQuery {
        pagination: synctv_core::models::PageParams::new(Some(page), Some(page_size)),
        search: (!req.search.is_empty()).then_some(req.search),
        status: proto_room_status_filter(req.status)?,
        is_banned: req.is_banned,
        relation: proto_my_room_relation(req.relation)?,
        sort_by: proto_my_room_list_sort_by(req.sort_by)?,
        sort_direction: proto_sort_direction(
            req.sort_direction,
            synctv_core::models::SortDirection::Desc,
        )?,
    })
}

pub(super) fn build_transfer_room_ownership_request(
    req: synctv_proto::client::TransferRoomOwnershipRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<UserId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    crate::impls::proto_validated_user_id(req.new_owner_user_id, public_id_codec)
}

pub(super) fn build_check_room_request(
    req: synctv_proto::client::CheckRoomRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    crate::impls::proto_validated_room_id(req.room_id, public_id_codec)
}

pub(crate) fn build_create_websocket_ticket_request(
    req: &synctv_proto::client::CreateWebSocketTicketRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(req)?;
    crate::impls::proto_validated_room_id(req.room_id.clone(), public_id_codec)
}

pub(super) type ChatHistoryCursor = (chrono::DateTime<chrono::Utc>, i64);
pub(super) type ChatReactionUsersCursor = (chrono::DateTime<chrono::Utc>, UserId);

fn parse_chat_history_cursor(cursor: &str) -> Result<Option<ChatHistoryCursor>, ApiError> {
    if cursor.is_empty() {
        return Ok(None);
    }
    let Some((ts_str, id)) = cursor.split_once('|') else {
        return Err(ApiError::InvalidInput("Invalid cursor format".to_string()));
    };
    let ts = synctv_common::time::parse_datetime_to_utc(ts_str)
        .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
    let id = id
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
    Ok(Some((ts, id)))
}

pub(super) fn build_get_chat_history_request(
    req: &synctv_proto::client::GetChatHistoryRequest,
) -> Result<(i32, Option<ChatHistoryCursor>, ChatMessageSelection), ApiError> {
    crate::impls::validate_proto_request(req)?;

    let limit = if req.limit > 0 { req.limit } else { 50 };
    let cursor = parse_chat_history_cursor(&req.cursor)?;
    let selection = chat_message_selection_from_proto(&req.include_message_types)?;

    Ok((limit, cursor, selection))
}

pub(crate) fn chat_message_selection_from_proto(
    include_message_types: &[i32],
) -> Result<ChatMessageSelection, ApiError> {
    chat_message_selection_from_proto_values(include_message_types).map_err(ApiError::InvalidInput)
}

pub(super) fn build_search_chat_messages_query(
    room_id: synctv_core::models::RoomId,
    req: &synctv_proto::client::SearchChatMessagesRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<synctv_core::models::ChatSearchMessagesQuery, ApiError> {
    crate::impls::validate_proto_request(req)?;
    let limit = if req.limit > 0 { req.limit } else { 50 };
    let cursor = parse_chat_history_cursor(&req.cursor)?
        .map(|(created_at, id)| synctv_core::models::ChatHistoryCursor { created_at, id });
    let user_id = if req.user_id.trim().is_empty() {
        None
    } else {
        Some(
            public_id_codec
                .decode_user_id(&req.user_id)
                .map_err(ApiError::InvalidInput)?,
        )
    };

    Ok(synctv_core::models::ChatSearchMessagesQuery {
        room_id,
        query: req.query.clone(),
        cursor,
        limit,
        include_deleted: req.include_deleted,
        user_id,
    })
}

pub(super) fn build_list_chat_reaction_users_request(
    req: &synctv_proto::client::ListChatReactionUsersRequest,
    public_id_codec: &crate::public_id::PublicIdCodec,
) -> Result<(i32, Option<ChatReactionUsersCursor>), ApiError> {
    crate::impls::validate_proto_request(req)?;

    let limit = if req.limit > 0 { req.limit } else { 50 };
    let cursor = if req.cursor.is_empty() {
        None
    } else if let Some((ts_str, user_id)) = req.cursor.split_once('|') {
        let ts = synctv_common::time::parse_datetime_to_utc(ts_str)
            .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
        let user_id = public_id_codec
            .decode_user_id(user_id)
            .map_err(ApiError::InvalidInput)?;
        Some((ts, user_id))
    } else {
        return Err(ApiError::InvalidInput("Invalid cursor format".to_string()));
    };

    Ok((limit, cursor))
}
