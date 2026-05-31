//! Room operations: list, create, get, join, leave, delete, settings, chat, hot rooms, public settings

use crate::impls::ApiError;
use std::collections::HashMap;
use synctv_core::models::{
    ChatImageBlob, ChatMessageEvent, ChatMessageType, ChatMessageWithImages,
    ChatPlaybackMessagesQuery, CreateChatImageUploadSession, DeleteChatMessage, EditChatMessage,
    FileUploadSession, MarkChatRead, NewChatImage, SendChatMessage, UserId,
};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::room::ClientResourceAvailability;

use super::convert::{
    member_status_to_proto, members_to_proto, playback_state_to_proto,
    resource_availability_enum_to_proto, room_role_to_proto,
};
use super::media::{
    file_cover_proto_to_stored_file, file_upload_session_to_room_cover_proto,
    prepare_delete_entries_outbox_fanout, room_cover_object_to_proto,
};
use super::{validate_password_for_set, validate_password_for_verify};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};

fn settings_registry_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Public settings are not available on this server.".to_string())
}

fn chat_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Chat service is not available on this server.".to_string())
}

const DEFAULT_ROOM_PAGE: u32 = 1;
const DEFAULT_ROOM_PAGE_SIZE: u32 = 20;
const MAX_ROOM_PAGE_SIZE: u32 = 100;
const DEFAULT_HOT_ROOM_LIMIT: i64 = 10;
const DEFAULT_HOT_ROOM_LIMIT_USIZE: usize = 10;
const MIN_PASSWORD_CHECK_DELAY_MS: u64 = 250;

fn positive_i32_to_u32(value: i32, default: u32) -> u32 {
    if value > 0 {
        value.cast_unsigned()
    } else {
        default
    }
}

fn positive_i32(value: i32, default: i32) -> i32 {
    if value > 0 {
        value
    } else {
        default
    }
}

fn positive_i64_to_usize(value: i64, default: usize) -> usize {
    usize::try_from(value).unwrap_or(default)
}

fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn chat_status_to_proto(
    status: synctv_core::models::ChatMessageStatus,
) -> crate::proto::client::ChatMessageStatus {
    match status {
        synctv_core::models::ChatMessageStatus::Active => {
            crate::proto::client::ChatMessageStatus::Active
        }
        synctv_core::models::ChatMessageStatus::Edited => {
            crate::proto::client::ChatMessageStatus::Edited
        }
        synctv_core::models::ChatMessageStatus::Deleted => {
            crate::proto::client::ChatMessageStatus::Deleted
        }
    }
}

async fn username_for_chat_message(
    api: &ClientApiImpl,
    message: &synctv_core::models::ChatMessage,
) -> String {
    let Some(user_id) = message.user_id else {
        return "[deleted]".to_string();
    };
    api.user_service
        .get_username(&user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn chat_image_to_proto(image: &synctv_core::models::ChatImage) -> crate::proto::client::ChatImage {
    crate::impls::messaging::core_chat_image_to_proto(image)
}

fn new_chat_image_to_proto(image: &NewChatImage) -> crate::proto::client::ChatImage {
    crate::proto::client::ChatImage {
        id: image.id.clone(),
        storage_backend: image.storage_backend.clone(),
        object_key: image.object_key.clone(),
        url: image.url.clone().unwrap_or_default(),
        mime_type: image.mime_type.clone().unwrap_or_default(),
        size_bytes: image.size_bytes.unwrap_or_default(),
        width: image.width.unwrap_or_default(),
        height: image.height.unwrap_or_default(),
        metadata: serde_json::to_vec(&image.metadata).unwrap_or_default(),
    }
}

fn chat_message_to_proto(
    api: &ClientApiImpl,
    message: ChatMessageWithImages,
    username: String,
) -> crate::proto::client::ChatMessageReceive {
    let msg = message.message;
    let room_id = api
        .public_id_codec
        .encode_room_id(msg.room_id)
        .expect("chat message room id must be encodable");
    let user_id = msg
        .user_id
        .and_then(|id| api.public_id_codec.encode_user_id(id).ok())
        .unwrap_or_default();
    let deleted_by_user_id = msg
        .deleted_by
        .and_then(|id| api.public_id_codec.encode_user_id(id).ok())
        .unwrap_or_default();
    crate::proto::client::ChatMessageReceive {
        id: msg.id.to_string(),
        room_id,
        user_id,
        username,
        content: msg.content,
        timestamp: msg.created_at.timestamp(),
        display_position: crate::impls::messaging::chat_display_position_from_metadata(
            &msg.metadata,
        ),
        display_color: crate::impls::messaging::chat_display_color_from_metadata(&msg.metadata),
        client_message_id: msg.client_message_id.unwrap_or_default(),
        status: chat_status_to_proto(msg.status) as i32,
        version: msg.version,
        edited_at: msg.edited_at.map_or(0, |ts| ts.timestamp()),
        deleted_at: msg.deleted_at.map_or(0, |ts| ts.timestamp()),
        reply_to_message_id: msg
            .reply_to_message_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        images: message.images.iter().map(chat_image_to_proto).collect(),
        deleted_by_user_id,
        delete_reason: msg.delete_reason.unwrap_or_default(),
        playback_media_id: crate::impls::messaging::chat_playback_media_id_from_metadata(
            &msg.metadata,
            &api.public_id_codec,
        ),
        playback_playlist_id: crate::impls::messaging::chat_playback_playlist_id_from_metadata(
            &msg.metadata,
            &api.public_id_codec,
        ),
        playback_target: crate::impls::messaging::chat_playback_target_from_metadata(&msg.metadata),
        playback_target_hash: crate::impls::messaging::chat_playback_target_hash_from_metadata(
            &msg.metadata,
        ),
        playback_position_seconds:
            crate::impls::messaging::chat_playback_position_seconds_from_metadata(&msg.metadata),
    }
}

async fn chat_event_to_proto(
    api: &ClientApiImpl,
    event: ChatMessageEvent,
) -> crate::proto::client::ChatMessageEvent {
    let username = username_for_chat_message(api, &event.message.message).await;
    let room_id = api
        .public_id_codec
        .encode_room_id(event.room_id)
        .expect("chat event room id must be encodable");
    crate::proto::client::ChatMessageEvent {
        event_id: event.event_id,
        room_id,
        kind: crate::impls::messaging::chat_event_kind_to_proto(event.kind) as i32,
        message: Some(chat_message_to_proto(api, event.message, username)),
        occurred_at: event.occurred_at.timestamp(),
    }
}

fn expected_chat_version(raw: i64) -> Option<i64> {
    (raw > 0).then_some(raw)
}

fn optional_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_chat_message_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidInput("Invalid chat message id".to_string()))
}

fn parse_json_metadata(bytes: &[u8]) -> Result<serde_json::Value, ApiError> {
    if bytes.is_empty() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    let metadata: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ApiError::InvalidInput(format!("Invalid metadata JSON: {error}")))?;
    if !metadata.is_object() {
        return Err(ApiError::InvalidInput(
            "metadata must be a JSON object".to_string(),
        ));
    }
    Ok(metadata)
}

fn parse_proto_chat_images(
    images: &[crate::proto::client::ChatImage],
) -> Result<Vec<synctv_core::models::NewChatImage>, ApiError> {
    images
        .iter()
        .map(crate::impls::messaging::proto_chat_image_to_core)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError::InvalidInput(format!("Invalid chat image: {error}")))
}

fn upload_session_to_proto(
    session: FileUploadSession,
) -> crate::proto::client::ChatImageUploadSession {
    crate::proto::client::ChatImageUploadSession {
        image: Some(new_chat_image_to_proto(&session.file)),
        upload_required: session.upload_required,
        upload_url: session.upload_url.unwrap_or_default(),
        upload_method: session.upload_method.unwrap_or_default(),
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: session.expires_at.map_or(0, |ts| ts.timestamp()),
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: session.ownership_proof_nonce.unwrap_or_default(),
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(|range| crate::proto::client::ChatImageOwnershipProofRange {
                offset: range.offset,
                length: range.length,
            })
            .collect(),
        ownership_proof_metadata_key: session.ownership_proof_metadata_key.unwrap_or_default(),
    }
}

fn chat_image_object_to_proto(
    room_id: &str,
    blob: &ChatImageBlob,
) -> crate::proto::client::ChatImageObjectResponse {
    crate::proto::client::ChatImageObjectResponse {
        room_id: room_id.to_string(),
        object_key: blob.object_key.clone(),
        mime_type: blob.mime_type.clone(),
        checksum_sha256: blob.checksum_sha256.clone(),
        data: blob.data.clone(),
    }
}

fn edit_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: crate::proto::client::EditChatMessageRequest,
) -> Result<EditChatMessage, ApiError> {
    Ok(EditChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
        content: req.content,
        metadata: parse_json_metadata(&req.metadata)?,
        expected_version: expected_chat_version(req.expected_version),
    })
}

fn delete_chat_message_request_to_core(
    room_id: synctv_core::models::RoomId,
    user_id: synctv_core::models::UserId,
    req: &crate::proto::client::DeleteChatMessageRequest,
) -> Result<DeleteChatMessage, ApiError> {
    Ok(DeleteChatMessage {
        room_id,
        message_id: parse_chat_message_id(&req.message_id)?,
        user_id,
        client_operation_id: optional_trimmed_string(&req.client_operation_id),
        reason: optional_trimmed_string(&req.reason),
        expected_version: expected_chat_version(req.expected_version),
    })
}

fn chat_read_state_to_proto(
    api: &ClientApiImpl,
    state: synctv_core::models::ChatReadStateWithUnread,
) -> crate::proto::client::ChatReadStateResponse {
    let room_id = api
        .public_id_codec
        .encode_room_id(state.state.room_id)
        .expect("chat read state room id must be encodable");
    let user_id = api
        .public_id_codec
        .encode_user_id(state.state.user_id)
        .expect("chat read state user id must be encodable");
    crate::proto::client::ChatReadStateResponse {
        state: Some(crate::proto::client::ChatReadState {
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
    }
}

fn build_public_room_list_query(
    req: crate::proto::client::ListRoomsRequest,
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
        sort_by: match crate::proto::client::RoomListSortBy::try_from(req.sort_by) {
            Ok(crate::proto::client::RoomListSortBy::Name) => {
                synctv_core::models::RoomListSortBy::Name
            }
            Ok(crate::proto::client::RoomListSortBy::UpdatedAt) => {
                synctv_core::models::RoomListSortBy::UpdatedAt
            }
            Ok(crate::proto::client::RoomListSortBy::LastActivityAt) => {
                synctv_core::models::RoomListSortBy::LastActivityAt
            }
            _ => synctv_core::models::RoomListSortBy::CreatedAt,
        },
        sort_direction: match crate::proto::client::SortDirection::try_from(req.sort_direction) {
            Ok(crate::proto::client::SortDirection::Asc) => synctv_core::models::SortDirection::Asc,
            _ => synctv_core::models::SortDirection::Desc,
        },
        ..Default::default()
    })
}

fn build_my_room_list_query(
    req: crate::proto::client::ListMyRoomsRequest,
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
        status: synctv_core::models::RoomStatus::try_from(req.status).ok(),
        is_banned: req.is_banned,
        relation: match crate::proto::client::MyRoomRelation::try_from(req.relation) {
            Ok(crate::proto::client::MyRoomRelation::Created) => {
                synctv_core::models::MyRoomRelation::Created
            }
            Ok(crate::proto::client::MyRoomRelation::Participating) => {
                synctv_core::models::MyRoomRelation::Participating
            }
            _ => synctv_core::models::MyRoomRelation::All,
        },
        sort_by: match crate::proto::client::MyRoomListSortBy::try_from(req.sort_by) {
            Ok(crate::proto::client::MyRoomListSortBy::Name) => {
                synctv_core::models::MyRoomListSortBy::Name
            }
            Ok(crate::proto::client::MyRoomListSortBy::CreatedAt) => {
                synctv_core::models::MyRoomListSortBy::CreatedAt
            }
            Ok(crate::proto::client::MyRoomListSortBy::UpdatedAt) => {
                synctv_core::models::MyRoomListSortBy::UpdatedAt
            }
            Ok(crate::proto::client::MyRoomListSortBy::LastActivityAt) => {
                synctv_core::models::MyRoomListSortBy::LastActivityAt
            }
            _ => synctv_core::models::MyRoomListSortBy::JoinedAt,
        },
        sort_direction: match crate::proto::client::SortDirection::try_from(req.sort_direction) {
            Ok(crate::proto::client::SortDirection::Asc) => synctv_core::models::SortDirection::Asc,
            _ => synctv_core::models::SortDirection::Desc,
        },
    })
}

fn build_transfer_room_ownership_request(
    req: crate::proto::client::TransferRoomOwnershipRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<UserId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    crate::impls::proto_validated_user_id(req.new_owner_user_id, public_id_codec)
}

fn build_check_room_request(
    req: crate::proto::client::CheckRoomRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    crate::impls::proto_validated_room_id(req.room_id, public_id_codec)
}

pub(crate) fn build_create_websocket_ticket_request(
    req: &crate::proto::client::CreateWebSocketTicketRequest,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(req)?;
    crate::impls::proto_validated_room_id(req.room_id.clone(), public_id_codec)
}

fn websocket_ticket_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("WebSocket ticket service is not available.".to_string())
}

type ChatHistoryCursor = (chrono::DateTime<chrono::Utc>, i64);

fn build_get_chat_history_request(
    req: &crate::proto::client::GetChatHistoryRequest,
) -> Result<(i32, Option<ChatHistoryCursor>), ApiError> {
    crate::impls::validate_proto_request(req)?;

    let limit = if req.limit > 0 { req.limit } else { 50 };
    let cursor = if req.cursor.is_empty() {
        None
    } else if let Some((ts_str, id)) = req.cursor.split_once('|') {
        let ts = synctv_common::time::parse_datetime_to_utc(ts_str)
            .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
        let id = id
            .parse::<i64>()
            .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
        Some((ts, id))
    } else {
        return Err(ApiError::InvalidInput("Invalid cursor format".to_string()));
    };

    Ok((limit, cursor))
}

impl ClientApiImpl {
    async fn load_room_member_count(
        &self,
        room_id: &synctv_core::models::RoomId,
    ) -> Result<Option<i32>, ApiError> {
        self.room_service
            .get_member_count(room_id)
            .await
            .map(Some)
            .map_err(ApiError::from)
    }

    /// Get the currently playing media for a room.
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_playing_media(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<Option<crate::proto::client::Media>, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Check membership before returning playing media
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let media = self
            .room_service
            .get_playing_media(&rid)
            .await
            .map_err(ApiError::from)?;
        match media {
            Some(media) => Ok(Some(
                self.media_to_proto_for_viewer_with_loaded_cover(&media, true, Some(uid))
                    .await?,
            )),
            None => Ok(None),
        }
    }

    pub async fn list_rooms(
        &self,
        req: crate::proto::client::ListRoomsRequest,
    ) -> Result<crate::proto::client::ListRoomsResponse, ApiError> {
        let query = build_public_room_list_query(req)?;
        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;
        let availability_map = self
            .room_service
            .room_availability_batch(&rooms)
            .await
            .map_err(ApiError::from)?;

        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let room_ids: Vec<synctv_core::models::RoomId> = rooms.iter().map(|room| room.id).collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&room_id_refs)
            .await
            .map_err(ApiError::from)?;
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let mut room_list = Vec::with_capacity(rooms.len());
        for r in &rooms {
            let member_count = member_counts.get(&r.id).copied();
            let availability = *availability_map
                .get(&r.id)
                .unwrap_or(&ClientResourceAvailability::Available);
            let settings = room_settings_map.get(&r.id);
            room_list.push(
                self.room_to_proto_with_availability_and_loaded_cover(
                    r,
                    settings,
                    member_count,
                    availability,
                )
                .await?,
            );
        }

        Ok(crate::proto::client::ListRoomsResponse {
            rooms: room_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn list_my_rooms(
        &self,
        user_id: &UserId,
        req: crate::proto::client::ListMyRoomsRequest,
    ) -> Result<crate::proto::client::ListMyRoomsResponse, ApiError> {
        let uid = *user_id;
        let query = build_my_room_list_query(req)?;
        let (rooms, total) = self
            .room_service
            .list_accessible_joined_rooms_with_query(&uid, &query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch room settings for full permission calculation.
        let room_ids: Vec<synctv_core::models::RoomId> =
            rooms.iter().map(|(room, _, _, _)| room.id).collect();
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let mut room_list = Vec::with_capacity(rooms.len());
        for (room, role, _status, member_count) in &rooms {
            // Use the full permission calculation instead of role.permissions(),
            // which only gives role-level defaults. calculate_role_default_permissions applies:
            //   1. Global default permissions (from SettingsRegistry)
            //   2. Room-level overrides (room_added / room_removed)
            let settings = room_settings_map.get(&room.id).cloned().unwrap_or_default();
            let permissions = self
                .room_service
                .permission_service()
                .calculate_role_default_permissions(role, &settings)
                .0;
            let relation = if room.created_by == uid {
                crate::proto::client::MyRoomRelation::Created as i32
            } else {
                crate::proto::client::MyRoomRelation::Participating as i32
            };
            room_list.push(crate::proto::client::MyRoom {
                room: Some(
                    self.room_to_proto_basic_with_loaded_cover(
                        room,
                        Some(&settings),
                        Some(*member_count),
                    )
                    .await?,
                ),
                permissions,
                role: room_role_to_proto(*role),
                relation,
            });
        }

        Ok(crate::proto::client::ListMyRoomsResponse {
            rooms: room_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn create_room(
        &self,
        user_id: &UserId,
        mut req: crate::proto::client::CreateRoomRequest,
    ) -> Result<crate::proto::client::CreateRoomResponse, ApiError> {
        // Validate and sanitize room name
        req.name = crate::impls::validation::validate_room_name(&req.name)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Validate and sanitize room description against ROOM_DESCRIPTION_MAX
        if !req.description.is_empty() {
            req.description = crate::impls::validation::validate_room_description(&req.description)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        crate::impls::validate_proto_request(&req)?;

        let uid = *user_id;

        let settings = if req.settings.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&req.settings)?)
        };

        let password = if req.password.is_empty() {
            None
        } else {
            validate_password_for_set(&req.password)?;
            Some(req.password)
        };
        let response_settings = crate::impls::client::convert::normalize_created_room_settings(
            settings.as_ref(),
            password.is_some(),
        );
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_created_outbox_fanout(uid);
        let (room, _member) = self
            .room_service
            .create_room_with_outbox(
                req.name,
                req.description,
                uid,
                password,
                settings,
                prepared_outbox_fanout.outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        Ok(crate::proto::client::CreateRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&response_settings),
                    self.load_room_member_count(&room.id).await?,
                )
                .await?,
            ),
        })
    }

    pub async fn get_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_for_actor(&actor).await
    }

    pub async fn get_room_for_actor(
        &self,
        actor: &RoomActor,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        let rid = actor.room_id();
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        let playback_state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .ok()
            .map(|s| playback_state_to_proto(&s, &self.public_id_codec));
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            playback_state,
        })
    }

    pub async fn get_room_as_guest(
        &self,
        access: &GuestRoomAccess,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        self.get_room_for_actor(&RoomActor::Guest(access.clone()))
            .await
    }

    async fn room_response_after_room_update(
        &self,
        room: &synctv_core::models::Room,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        let rid = room.id;
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::GetRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    room,
                    Some(&settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            playback_state: self
                .room_service
                .get_playback_state(&rid)
                .await
                .ok()
                .map(|s| playback_state_to_proto(&s, &self.public_id_codec)),
        })
    }

    pub async fn create_room_cover_upload_session(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::CreateRoomCoverUploadSessionRequest,
    ) -> Result<crate::proto::client::CreateRoomCoverUploadSessionResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let session = self
            .room_service
            .create_room_cover_upload_session(
                rid,
                *user_id,
                synctv_core::service::room::CreateRoomCoverUploadSession {
                    client_cover_id: optional_trimmed_string(&req.client_cover_id),
                    mime_type: req.mime_type,
                    size_bytes: req.size_bytes,
                    width: (req.width > 0).then_some(req.width),
                    height: (req.height > 0).then_some(req.height),
                    checksum_sha256: optional_trimmed_string(&req.checksum_sha256),
                    metadata: parse_json_metadata(&req.metadata)?,
                },
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::CreateRoomCoverUploadSessionResponse {
            session: Some(file_upload_session_to_room_cover_proto(session)),
        })
    }

    pub async fn upload_room_cover_object(
        &self,
        req: crate::proto::client::UploadRoomCoverObjectRequest,
    ) -> Result<crate::proto::client::UploadRoomCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .store_room_cover_upload_object(
                &req.encoded_object_key,
                &req.token,
                (!req.content_type.trim().is_empty()).then_some(req.content_type.as_str()),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::UploadRoomCoverObjectResponse {
            object: Some(room_cover_object_to_proto(&blob)),
        })
    }

    pub async fn get_room_cover_object(
        &self,
        req: crate::proto::client::GetRoomCoverObjectRequest,
    ) -> Result<crate::proto::client::RoomCoverObjectResponse, ApiError> {
        let blob = self
            .room_service
            .get_room_cover_object(&req.encoded_object_key, &req.token)
            .await
            .map_err(ApiError::from)?;
        Ok(room_cover_object_to_proto(&blob))
    }

    pub async fn update_room_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdateRoomCoverRequest,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let cover = req
            .cover
            .ok_or_else(|| ApiError::InvalidInput("cover is required".to_string()))?;
        let room = self
            .room_service
            .update_room_cover(rid, *user_id, file_cover_proto_to_stored_file(cover)?)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.room_response_after_room_update(&room).await
    }

    pub async fn clear_room_cover(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::ClearRoomCoverRequest,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = self.parse_room_id(room_id)?;
        let room = self
            .room_service
            .clear_room_cover(rid, *user_id)
            .await
            .map_err(ApiError::from)?;
        self.room_cache_fanout.publish_invalidation(&rid);
        self.room_response_after_room_update(&room).await
    }

    pub async fn join_room(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::JoinRoomRequest,
        client_ip: Option<&str>,
    ) -> Result<crate::proto::client::JoinRoomResponse, ApiError> {
        self.join_room_with_control(user_id, room_id, req, client_ip, None)
            .await
    }

    pub async fn join_room_with_control(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::JoinRoomRequest,
        client_ip: Option<&str>,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::JoinRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        let password = if req.password.is_empty() {
            None
        } else {
            validate_password_for_verify(&req.password)?;
            Some(req.password)
        };

        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        if room_settings.require_password.0 {
            let password = password.as_ref().ok_or_else(|| {
                ApiError::Authorization("Forbidden: Password required".to_string())
            })?;
            let start = std::time::Instant::now();
            let parsed_client_ip = client_ip.and_then(|ip| ip.parse().ok());

            let valid = self
                .room_service
                .check_room_password_with_rate_limit_with_control(
                    &rid,
                    password,
                    parsed_client_ip,
                    request_control,
                )
                .await
                .map_err(ApiError::from)?;

            let elapsed = start.elapsed();
            let min_delay = std::time::Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS);
            if elapsed < min_delay {
                let delay = min_delay
                    .checked_sub(elapsed)
                    .expect("elapsed < min_delay guaranteed by if-check above");
                if let Some(request_control) = request_control {
                    request_control
                        .run(tokio::time::sleep(delay))
                        .await
                        .map_err(|error| ApiError::from(synctv_core::Error::from(error)))?;
                } else {
                    tokio::time::sleep(delay).await;
                }
            }

            if !valid {
                return Err(ApiError::Authorization(
                    "Forbidden: Invalid password".to_string(),
                ));
            }
        }

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(uid, uid);
        let (_room, member, members) = self
            .room_service
            .join_room_with_outbox(
                rid,
                uid,
                password,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;
        prepared_membership_fanout.publish_after_outbox_commit();

        // Get updated room and playback state
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback_state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .ok()
            .map(|s| playback_state_to_proto(&s, &self.public_id_codec));

        let proto_members = members_to_proto(
            &members,
            &room_settings,
            self.room_service.permission_service(),
            &self.public_id_codec,
        );

        let requires_approval = proto_members.is_empty();
        Ok(crate::proto::client::JoinRoomResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&room_settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
            members: proto_members,
            playback_state,
            membership_status: member_status_to_proto(member.status),
            requires_approval,
        })
    }

    pub async fn create_websocket_ticket_with_control(
        &self,
        user_id: &UserId,
        password_version: i32,
        req: crate::proto::client::CreateWebSocketTicketRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::CreateWebSocketTicketResponse, ApiError> {
        let room_id = build_create_websocket_ticket_request(&req, &self.public_id_codec)?;
        let requested_room_id = req.room_id;
        let ws_ticket_service = self
            .ws_ticket_service
            .as_ref()
            .ok_or_else(websocket_ticket_service_unavailable_error)?;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|err| match err {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound(format!("Room {requested_room_id} not found"))
                }
                other => ApiError::from(other),
            })?;

        if room.is_banned {
            return Err(ApiError::Authorization("Room is banned".to_string()));
        }

        let is_member = self
            .room_service
            .member_service()
            .is_member(&room_id, user_id)
            .await
            .map_err(ApiError::from)?;

        if !is_member {
            return Err(ApiError::Authorization(
                "Not a member of this room. Join the room first.".to_string(),
            ));
        }

        let ticket = ws_ticket_service
            .create_ticket_with_control(user_id, &room_id, password_version, request_control)
            .await
            .map_err(ApiError::from)?;

        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::Internal)?;

        Ok(crate::proto::client::CreateWebSocketTicketResponse {
            ticket,
            room_id: public_room_id.clone(),
            expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
            usage: format!("Use in WebSocket URL: ws://host/ws/rooms/{public_room_id}?ticket=xxx"),
        })
    }

    pub async fn create_websocket_ticket_for_actor_with_control(
        &self,
        actor: RoomActor,
        req: crate::proto::client::CreateWebSocketTicketRequest,
        request_control: Option<&ExecutionControl>,
    ) -> Result<crate::proto::client::CreateWebSocketTicketResponse, ApiError> {
        let room_id = build_create_websocket_ticket_request(&req, &self.public_id_codec)?;
        let requested_room_id = req.room_id;
        if actor.room_id() != room_id {
            return Err(ApiError::Authorization(
                "Cannot create a WebSocket ticket for a different room".to_string(),
            ));
        }

        let ws_ticket_service = self
            .ws_ticket_service
            .as_ref()
            .ok_or_else(websocket_ticket_service_unavailable_error)?;

        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|err| match err {
                synctv_core::Error::NotFound(_) => {
                    ApiError::NotFound(format!("Room {requested_room_id} not found"))
                }
                other => ApiError::from(other),
            })?;

        if room.is_banned {
            return Err(ApiError::Authorization("Room is banned".to_string()));
        }

        let ticket = match actor {
            RoomActor::User { user_id, .. } => {
                let current_user = self
                    .user_service
                    .get_user(&user_id)
                    .await
                    .map_err(ApiError::from)?;
                ws_ticket_service
                    .create_ticket_with_control(
                        &user_id,
                        &room_id,
                        current_user.password_version,
                        request_control,
                    )
                    .await
                    .map_err(ApiError::from)?
            }
            RoomActor::Guest(access) => ws_ticket_service
                .create_guest_ticket_with_control(
                    synctv_core::service::CreateGuestTicketRequest {
                        room_id,
                        guest_id: access.guest_id,
                        display_name: access.display_name,
                        session_id: access.session_id,
                        token_jti: access.token_jti,
                        room_guest_version: access.room_guest_version,
                        permissions: access.permissions,
                    },
                    request_control,
                )
                .await
                .map_err(ApiError::from)?,
        };

        let public_room_id = self
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(ApiError::Internal)?;

        Ok(crate::proto::client::CreateWebSocketTicketResponse {
            ticket,
            room_id: public_room_id.clone(),
            expires_in_secs: ws_ticket_service.ticket_ttl_secs(),
            usage: format!("Use in WebSocket URL: ws://host/ws/rooms/{public_room_id}?ticket=xxx"),
        })
    }

    pub async fn leave_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::LeaveRoomResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_user_left_outbox_fanout();
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map_or_else(|_| uid.to_string(), |user| user.username);
        let prepared_cleanup_fanout = prepare_delete_entries_outbox_fanout(
            self.media_fanout.clone(),
            self.playlist_fanout.clone(),
            self.realtime_fanout.clone(),
            rid,
            uid,
            username,
        );
        self.room_service
            .leave_room_with_outbox(
                rid,
                uid,
                Some(prepared_membership_fanout.outbox_factory()),
                Some(prepared_cleanup_fanout.member_cleanup_outbox_factory()),
            )
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the user's room-scoped connections and any local
        // publishers tied to the room they just left.
        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &uid)
            .await;

        prepared_membership_fanout.publish_after_outbox_commit();
        prepared_cleanup_fanout.publish_after_outbox_commit();

        Ok(crate::proto::client::LeaveRoomResponse { success: true })
    }

    pub async fn delete_room(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::DeleteRoomResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let prepared_outbox_fanout = self
            .room_lifecycle_fanout
            .prepare_room_deleted_outbox_fanout(&rid, &uid);

        // 1. Delete the DB record first. If this fails, no realtime event is
        //    published and no connections are dropped -- the room remains intact.
        self.room_service
            .delete_room_with_outbox(rid, uid, prepared_outbox_fanout.cloned_outbox_event())
            .await
            .map_err(ApiError::from)?;

        prepared_outbox_fanout.publish_after_outbox_commit();

        // Force disconnect room members and any active publishers tied to this room.
        self.realtime_lifecycle
            .disconnect_room(&rid, "room_deleted")
            .await;

        Ok(crate::proto::client::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::UpdateRoomSettingsRequest,
    ) -> Result<crate::proto::client::UpdateRoomSettingsResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        if req.settings.is_empty() {
            // SECURITY: Return success with None room instead of room details.
            // Previously this returned room data after only checking membership,
            // which allowed any room member to bypass UPDATE_ROOM_SETTINGS permission.
            // Users should use get_room or get_room_settings endpoints to fetch room info.
            return Ok(crate::proto::client::UpdateRoomSettingsResponse { room: None });
        }

        let settings_patch: serde_json::Value = serde_json::from_slice(&req.settings)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid settings JSON: {e}")))?;
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            &uid,
            &username,
            Vec::new(),
            0,
        );
        let snapshot = self
            .room_service
            .patch_settings_with_outbox(
                rid,
                uid,
                settings_patch,
                prepared_settings_fanout.settings_outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;
        let prepared_settings_fanout = prepared_settings_fanout
            .with_settings_and_version(&snapshot.settings, snapshot.version)
            .ok_or_else(|| {
                ApiError::Internal("Failed to serialize updated room settings".to_string())
            })?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(prepared_settings_fanout);
        self.room_cache_fanout.publish_invalidation(&rid);

        // Get updated room
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::UpdateRoomSettingsResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&snapshot.settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
        })
    }

    /// Set or remove room password
    pub async fn set_room_password(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::SetRoomPasswordRequest,
    ) -> Result<crate::proto::client::SetRoomPasswordResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;

        // Validate password length
        if !req.password.is_empty() {
            validate_password_for_set(&req.password)?;
        }

        // Check permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::RoomPermission::SET_ROOM_SETTINGS,
            )
            .await
            .map_err(ApiError::from)?;

        // Hash password if provided, or None to remove
        let password_hash = if req.password.is_empty() {
            None
        } else {
            let hash = synctv_core::service::auth::password::hash_password(&req.password)
                .await
                .map_err(|e| ApiError::Internal(format!("Failed to hash password: {e}")))?;
            Some(hash)
        };
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            &uid,
            &username,
            Vec::new(),
            0,
        );

        let snapshot = self
            .room_service
            .update_room_password_as_with_outbox(
                &rid,
                Some(&uid),
                &username,
                password_hash,
                prepared_settings_fanout.settings_outbox_factory(),
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to update password: {e}")))?;

        // Invalidate room cache on other replicas so password check uses fresh data
        self.room_cache_fanout.publish_invalidation(&rid);
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout
                    .with_settings_and_version(&snapshot.settings, snapshot.version)
                    .ok_or_else(|| {
                        ApiError::Internal("Failed to serialize room settings".to_string())
                    })?,
            );

        Ok(crate::proto::client::SetRoomPasswordResponse { success: true })
    }

    /// Get room settings
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomSettingsResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_room_settings_for_actor(&actor).await
    }

    pub async fn get_room_settings_for_actor(
        &self,
        actor: &RoomActor,
    ) -> Result<crate::proto::client::GetRoomSettingsResponse, ApiError> {
        let rid = actor.room_id();
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;

        let settings_bytes = serde_json::to_vec(&settings)
            .map_err(|e| ApiError::Internal(format!("Failed to serialize settings: {e}")))?;

        Ok(crate::proto::client::GetRoomSettingsResponse {
            settings: settings_bytes,
            version,
        })
    }

    pub async fn get_room_settings_as_guest(
        &self,
        access: &GuestRoomAccess,
    ) -> Result<crate::proto::client::GetRoomSettingsResponse, ApiError> {
        self.get_room_settings_for_actor(&RoomActor::Guest(access.clone()))
            .await
    }

    /// Reset room settings to defaults
    pub async fn reset_room_settings(
        &self,
        user_id: &UserId,
        room_id: &str,
    ) -> Result<crate::proto::client::ResetRoomSettingsResponse, ApiError> {
        let uid = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let default_settings = synctv_core::models::RoomSettings::default();
        let (_, current_version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&default_settings).map_err(ApiError::from)?;
        let prepared_settings_fanout = self.room_settings_fanout.prepare_settings_changed(
            &rid,
            &uid,
            &username,
            settings_json.clone(),
            current_version + 1,
        );
        let snapshot = self
            .room_service
            .reset_room_settings_with_outbox(
                &rid,
                &uid,
                prepared_settings_fanout.settings_outbox_factory(),
            )
            .await
            .map_err(ApiError::from)?;
        self.room_settings_fanout
            .publish_prepared_after_outbox_commit(
                prepared_settings_fanout.with_version(snapshot.version),
            );
        self.room_cache_fanout.publish_invalidation(&rid);

        Ok(crate::proto::client::ResetRoomSettingsResponse {
            settings: settings_json,
        })
    }

    pub async fn transfer_room_ownership(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::TransferRoomOwnershipRequest,
    ) -> Result<crate::proto::client::TransferRoomOwnershipResponse, ApiError> {
        let current_owner_id = *user_id;
        let rid = self.parse_room_id(room_id)?;
        let new_owner_id = build_transfer_room_ownership_request(req, &self.public_id_codec)?;

        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(current_owner_id, current_owner_id);
        let room = self
            .room_service
            .transfer_room_ownership_with_outbox(
                rid,
                current_owner_id,
                new_owner_id,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(Self::map_room_access_error)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&rid);
        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::TransferRoomOwnershipResponse {
            room: Some(
                self.room_to_proto_basic_with_loaded_cover(
                    &room,
                    Some(&settings),
                    self.load_room_member_count(&rid).await?,
                )
                .await?,
            ),
        })
    }

    /// Get public settings
    pub fn get_public_settings(
        &self,
    ) -> Result<crate::proto::client::GetPublicSettingsResponse, ApiError> {
        let reg = self
            .settings_registry
            .as_ref()
            .ok_or_else(settings_registry_unavailable_error)?;

        let s = reg.to_public_settings();
        Ok(crate::proto::client::GetPublicSettingsResponse {
            allow_room_creation: s.allow_room_creation,
            max_rooms_per_user: s.max_rooms_per_user,
            max_members_per_room: s.max_members_per_room,
            disable_create_room: s.disable_create_room,
            create_room_need_review: s.create_room_need_review,
            room_password_policy: s.room_password_policy.to_string(),
            enable_password_signup: s.enable_password_signup,
            password_signup_need_review: s.password_signup_need_review,
            enable_email_signup: s.enable_email_signup,
            email_signup_need_review: s.email_signup_need_review,
            enable_email: s.enable_email && self.email_api.is_some(),
            enable_webauthn: self.passkey_service.is_some(),
            enable_webauthn_signup: s.enable_webauthn_signup,
            webauthn_signup_need_review: s.webauthn_signup_need_review,
            enable_guest: s.enable_guest,
            movie_proxy: s.movie_proxy,
            live_proxy: s.live_proxy,
            ts_disguised_as_png: s.ts_disguised_as_png,
            custom_publish_host: s.custom_publish_host,
            email_whitelist_enabled: s.email_whitelist_enabled,
            email_whitelist_domains: s.email_whitelist_domains,
        })
    }

    pub async fn get_server_info(
        &self,
    ) -> Result<crate::proto::client::GetServerInfoResponse, ApiError> {
        let reg = self
            .settings_registry
            .as_ref()
            .ok_or_else(settings_registry_unavailable_error)?;
        let server_id = reg
            .get_or_initialize_server_id()
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetServerInfoResponse {
            server_id,
            server_name: self.config.webauthn.rp_name.clone(),
        })
    }

    /// Check if a room exists and whether it requires a password (public endpoint).
    ///
    /// Only returns whether the room requires a password -- the room name is
    /// intentionally omitted to avoid leaking room metadata to unauthenticated
    /// users (room enumeration / information disclosure).
    pub async fn check_room(
        &self,
        req: crate::proto::client::CheckRoomRequest,
    ) -> Result<crate::proto::client::CheckRoomResponse, ApiError> {
        let rid = build_check_room_request(req, &self.public_id_codec)?;

        match self.room_service.get_room(&rid).await {
            Ok(room) => {
                let settings = self
                    .room_service
                    .get_room_settings(&rid)
                    .await
                    .map_err(ApiError::from)?;
                let availability = self
                    .room_service
                    .room_availability(&room)
                    .await
                    .map_err(ApiError::from)?;
                Ok(crate::proto::client::CheckRoomResponse {
                    exists: true,
                    requires_password: settings.require_password.0,
                    name: String::new(),
                    availability: resource_availability_enum_to_proto(availability),
                })
            }
            Err(_) => Ok(crate::proto::client::CheckRoomResponse {
                exists: false,
                requires_password: false,
                name: String::new(),
                availability: crate::proto::client::ResourceAvailability::Unspecified as i32,
            }),
        }
    }

    pub async fn get_hot_rooms(
        &self,
        req: crate::proto::client::GetHotRoomsRequest,
    ) -> Result<crate::proto::client::GetHotRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let limit = if req.limit == 0 {
            DEFAULT_HOT_ROOM_LIMIT
        } else {
            i64::from(req.limit)
        };
        let limit_usize = positive_i64_to_usize(limit, DEFAULT_HOT_ROOM_LIMIT_USIZE);

        let room_online_counts = self
            .connection_service
            .hot_room_online_user_counts_distributed()
            .await
            .map_err(ApiError::Internal)?;
        let room_ids: Vec<synctv_core::models::RoomId> = room_online_counts
            .iter()
            .map(|(room_id, _)| *room_id)
            .collect();
        let rooms = self
            .room_service
            .list_active_unbanned_rooms_by_ids(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let mut online_by_room: HashMap<synctv_core::models::RoomId, usize> =
            room_online_counts.into_iter().collect();
        let mut room_online: Vec<(synctv_core::models::Room, i32)> = rooms
            .into_iter()
            .filter_map(|room| {
                let count = online_by_room.remove(&room.id).unwrap_or(0);
                (count > 0).then_some((room, usize_to_i32_saturating(count)))
            })
            .collect();
        room_online.sort_by_key(|(room, count)| (std::cmp::Reverse(*count), room.id));
        let mut top_rooms: Vec<_> = room_online.into_iter().take(limit_usize).collect();

        if top_rooms.len() < limit_usize {
            let fallback_query = synctv_core::models::RoomListQuery {
                pagination: synctv_core::models::PageParams::new(
                    Some(1),
                    Some(u32::try_from(limit_usize).unwrap_or(u32::MAX)),
                ),
                search: None,
                status: Some(synctv_core::models::RoomStatus::Active),
                is_banned: Some(false),
                creator_id: None,
                sort_by: synctv_core::models::RoomListSortBy::CreatedAt,
                sort_direction: synctv_core::models::SortDirection::Desc,
            };
            let (fallback_rooms, _) = self
                .room_service
                .list_rooms(&fallback_query)
                .await
                .map_err(ApiError::from)?;
            for room in fallback_rooms {
                if top_rooms.iter().all(|(existing, _)| existing.id != room.id) {
                    top_rooms.push((room, 0));
                }
                if top_rooms.len() >= limit_usize {
                    break;
                }
            }
        }

        let selected_rooms: Vec<synctv_core::models::Room> =
            top_rooms.iter().map(|(room, _)| room.clone()).collect();
        let availability_map = self
            .room_service
            .room_availability_batch(&selected_rooms)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch member counts for the top N rooms (single SQL query instead of N+1)
        let top_room_id_refs: Vec<&synctv_core::models::RoomId> =
            top_rooms.iter().map(|(r, _)| &r.id).collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&top_room_id_refs)
            .await
            .unwrap_or_default();

        // Batch-fetch settings for the top N rooms
        let room_ids: Vec<synctv_core::models::RoomId> =
            top_rooms.iter().map(|(room, _)| room.id).collect();
        let settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .map_err(ApiError::from)?;

        let mut hot_rooms = Vec::with_capacity(top_rooms.len());
        for (room, online_count) in top_rooms {
            let total_members = member_counts.get(&room.id).copied().unwrap_or(0);
            let settings = settings_map.get(&room.id);
            let availability = *availability_map
                .get(&room.id)
                .unwrap_or(&ClientResourceAvailability::Available);

            hot_rooms.push(crate::proto::client::RoomWithStats {
                room: Some(
                    self.room_to_proto_with_availability_and_loaded_cover(
                        &room,
                        settings,
                        Some(total_members),
                        availability,
                    )
                    .await?,
                ),
                online_count,
                total_members,
            });
        }

        Ok(crate::proto::client::GetHotRoomsResponse { rooms: hot_rooms })
    }

    pub async fn get_chat_history(
        &self,
        user_id: &UserId,
        room_id: &str,
        req: crate::proto::client::GetChatHistoryRequest,
    ) -> Result<crate::proto::client::GetChatHistoryResponse, ApiError> {
        let actor = self.room_actor_for_user(user_id, room_id).await?;
        self.get_chat_history_for_actor(&actor, req).await
    }

    async fn get_chat_history_for_room_id(
        &self,
        rid: &synctv_core::models::RoomId,
        req: crate::proto::client::GetChatHistoryRequest,
    ) -> Result<crate::proto::client::GetChatHistoryResponse, ApiError> {
        let (limit, cursor) = build_get_chat_history_request(&req)?;
        let cursor = cursor
            .map(|(created_at, id)| synctv_core::models::ChatHistoryCursor { created_at, id });
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let (messages, next) = chat_service
            .get_history_with_images(rid, cursor, limit, true)
            .await
            .map_err(ApiError::from)?;
        let next_cursor_str = next.map(|cursor| {
            format!(
                "{}|{}",
                synctv_common::time::format_datetime_rfc3339(cursor.created_at),
                cursor.id
            )
        });

        // Collect unique user IDs to batch fetch usernames
        let user_ids: Vec<synctv_core::models::UserId> = messages
            .iter()
            .filter_map(|m| m.message.user_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Batch fetch usernames (single query instead of N+1)
        let username_map: std::collections::HashMap<synctv_core::models::UserId, String> = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .unwrap_or_default();

        // Convert to proto format
        let proto_messages = messages
            .into_iter()
            .map(|m| {
                let (user_id_str, username) = match &m.message.user_id {
                    Some(uid) => {
                        let uid_str = self
                            .public_id_codec
                            .encode_user_id(*uid)
                            .expect("chat message user id must be encodable");
                        let name = username_map
                            .get(uid)
                            .cloned()
                            .unwrap_or_else(|| format!("user_{uid_str}"));
                        (uid_str, name)
                    }
                    None => (String::new(), "[deleted]".to_string()),
                };

                let mut proto = chat_message_to_proto(self, m, username);
                proto.user_id = user_id_str;
                proto
            })
            .collect();

        Ok(crate::proto::client::GetChatHistoryResponse {
            messages: proto_messages,
            next_cursor: next_cursor_str.unwrap_or_default(),
        })
    }

    pub async fn send_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::SendChatMessageRequest,
    ) -> Result<crate::proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let room_id = actor.room_id();
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let images = parse_proto_chat_images(&req.images)?;
        let playback_state = self
            .room_service
            .playback_service()
            .get_state(&room_id)
            .await
            .ok();
        let metadata = crate::impls::messaging::chat_metadata_for_send(
            parse_json_metadata(&req.metadata)?,
            &req.display_position,
            &req.display_color,
            playback_state.as_ref(),
        )
        .map_err(ApiError::InvalidInput)?;
        let outcome = chat_service
            .send_message_event_outcome(SendChatMessage {
                room_id,
                user_id,
                client_message_id: optional_trimmed_string(&req.client_message_id),
                content: req.content,
                message_type: if images.is_empty() {
                    ChatMessageType::Text
                } else {
                    ChatMessageType::Image
                },
                reply_to_message_id: if req.reply_to_message_id.trim().is_empty() {
                    None
                } else {
                    Some(parse_chat_message_id(&req.reply_to_message_id)?)
                },
                metadata,
                images,
            })
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
        }
        Ok(crate::proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await),
        })
    }

    pub async fn create_chat_image_upload_session_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::CreateChatImageUploadSessionRequest,
    ) -> Result<crate::proto::client::CreateChatImageUploadSessionResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let session = chat_service
            .create_image_upload_session(CreateChatImageUploadSession {
                room_id: actor.room_id(),
                user_id,
                client_image_id: optional_trimmed_string(&req.client_image_id),
                mime_type: req.mime_type,
                size_bytes: req.size_bytes,
                width: (req.width > 0).then_some(req.width),
                height: (req.height > 0).then_some(req.height),
                checksum_sha256: optional_trimmed_string(&req.checksum_sha256),
                metadata: parse_json_metadata(&req.metadata)?,
            })
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::CreateChatImageUploadSessionResponse {
            session: Some(upload_session_to_proto(session)),
        })
    }

    pub async fn upload_chat_image_object(
        &self,
        req: crate::proto::client::UploadChatImageObjectRequest,
    ) -> Result<crate::proto::client::UploadChatImageObjectResponse, ApiError> {
        let _room_id = self.parse_room_id(&req.room_id)?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let blob = chat_service
            .store_image_upload_object(
                &req.encoded_object_key,
                &req.token,
                (!req.content_type.trim().is_empty()).then_some(req.content_type.as_str()),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::UploadChatImageObjectResponse {
            object: Some(chat_image_object_to_proto(&req.room_id, &blob)),
        })
    }

    pub async fn get_chat_image_object(
        &self,
        req: crate::proto::client::GetChatImageObjectRequest,
    ) -> Result<crate::proto::client::ChatImageObjectResponse, ApiError> {
        let _room_id = self.parse_room_id(&req.room_id)?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let blob = chat_service
            .get_image_object(&req.encoded_object_key, &req.token)
            .await
            .map_err(ApiError::from)?;
        Ok(chat_image_object_to_proto(&req.room_id, &blob))
    }

    pub async fn edit_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::EditChatMessageRequest,
    ) -> Result<crate::proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .edit_message_outcome(edit_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
        }
        Ok(crate::proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await),
        })
    }

    pub async fn delete_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::DeleteChatMessageRequest,
    ) -> Result<crate::proto::client::ChatMessageEventResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let outcome = chat_service
            .delete_message_event_outcome(delete_chat_message_request_to_core(
                actor.room_id(),
                user_id,
                &req,
            )?)
            .await
            .map_err(ApiError::from)?;
        if outcome.inserted {
            self.broadcast_chat_event(&outcome.event);
        }
        Ok(crate::proto::client::ChatMessageEventResponse {
            event: Some(chat_event_to_proto(self, outcome.event).await),
        })
    }

    pub async fn mark_chat_read_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::MarkChatReadRequest,
    ) -> Result<crate::proto::client::ChatReadStateResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let state = chat_service
            .mark_read(MarkChatRead {
                room_id: actor.room_id(),
                user_id,
                message_id: parse_chat_message_id(&req.message_id)?,
            })
            .await
            .map_err(ApiError::from)?;
        Ok(chat_read_state_to_proto(self, state))
    }

    pub async fn get_chat_read_state_for_actor(
        &self,
        actor: &RoomActor,
        _req: crate::proto::client::GetChatReadStateRequest,
    ) -> Result<crate::proto::client::ChatReadStateResponse, ApiError> {
        let user_id = actor.require_user_id()?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let state = chat_service
            .get_read_state(&actor.room_id(), &user_id)
            .await
            .map_err(ApiError::from)?;
        Ok(chat_read_state_to_proto(self, state))
    }

    fn broadcast_chat_event(&self, event: &ChatMessageEvent) {
        self.chat_event_dispatcher.dispatch(event);
    }

    pub async fn get_chat_history_as_guest(
        &self,
        access: &GuestRoomAccess,
        req: crate::proto::client::GetChatHistoryRequest,
    ) -> Result<crate::proto::client::GetChatHistoryResponse, ApiError> {
        self.get_chat_history_for_actor(&RoomActor::Guest(access.clone()), req)
            .await
    }

    pub async fn get_chat_history_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::GetChatHistoryRequest,
    ) -> Result<crate::proto::client::GetChatHistoryResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        self.get_chat_history_for_room_id(&actor.room_id(), req)
            .await
    }

    pub async fn get_chat_message_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::GetChatMessageRequest,
    ) -> Result<crate::proto::client::GetChatMessageResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let message = chat_service
            .get_message_with_images(
                &actor.room_id(),
                parse_chat_message_id(&req.message_id)?,
                req.include_deleted,
            )
            .await
            .map_err(ApiError::from)?;
        let username = username_for_chat_message(self, &message.message).await;
        Ok(crate::proto::client::GetChatMessageResponse {
            message: Some(chat_message_to_proto(self, message, username)),
        })
    }

    pub async fn get_chat_message_context_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::GetChatMessageContextRequest,
    ) -> Result<crate::proto::client::GetChatMessageContextResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let context = chat_service
            .get_message_context(
                &actor.room_id(),
                parse_chat_message_id(&req.message_id)?,
                positive_i32(req.before_limit, 20).min(50),
                positive_i32(req.after_limit, 20).min(50),
                req.include_deleted,
            )
            .await
            .map_err(ApiError::from)?;
        let before = self.chat_messages_to_proto(context.before).await;
        let username = username_for_chat_message(self, &context.anchor.message).await;
        let message = chat_message_to_proto(self, context.anchor, username);
        let after = self.chat_messages_to_proto(context.after).await;
        Ok(crate::proto::client::GetChatMessageContextResponse {
            before,
            message: Some(message),
            after,
        })
    }

    pub async fn get_chat_playback_messages_for_actor(
        &self,
        actor: &RoomActor,
        req: crate::proto::client::GetChatPlaybackMessagesRequest,
    ) -> Result<crate::proto::client::GetChatPlaybackMessagesResponse, ApiError> {
        self.require_room_permission(
            actor,
            synctv_core::models::RoomPermission::VIEW_CHAT_HISTORY,
        )
        .await?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(chat_service_unavailable_error)?;
        let media_id = optional_trimmed_string(&req.playback_media_id)
            .map(|id| crate::impls::proto_validated_media_id(id, &self.public_id_codec))
            .transpose()?;
        let playlist_id = optional_trimmed_string(&req.playback_playlist_id)
            .map(|id| crate::impls::proto_validated_playlist_id(id, &self.public_id_codec))
            .transpose()?;
        let target_hash = (!req.playback_target.is_empty())
            .then(|| crate::impls::messaging::chat_playback_target_hash(&req.playback_target));
        let before_seconds = if req.before_seconds.is_finite() && req.before_seconds > 0.0 {
            req.before_seconds
        } else {
            0.0
        };
        let after_seconds = if req.after_seconds.is_finite() && req.after_seconds > 0.0 {
            req.after_seconds
        } else {
            30.0
        };
        let limit = positive_i32(req.limit, 200).min(500);
        let messages = chat_service
            .get_playback_messages_with_images(ChatPlaybackMessagesQuery {
                room_id: actor.room_id(),
                media_id,
                playlist_id,
                target_hash,
                position_seconds: req.position_seconds,
                before_seconds,
                after_seconds,
                limit,
                include_deleted: req.include_deleted,
            })
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetChatPlaybackMessagesResponse {
            messages: self.chat_messages_to_proto(messages).await,
        })
    }

    async fn chat_messages_to_proto(
        &self,
        messages: Vec<ChatMessageWithImages>,
    ) -> Vec<crate::proto::client::ChatMessageReceive> {
        let mut converted = Vec::with_capacity(messages.len());
        for message in messages {
            let username = username_for_chat_message(self, &message.message).await;
            converted.push(chat_message_to_proto(self, message, username));
        }
        converted
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_check_room_request, build_create_websocket_ticket_request,
        build_get_chat_history_request, build_my_room_list_query, build_public_room_list_query,
        build_transfer_room_ownership_request, delete_chat_message_request_to_core,
        edit_chat_message_request_to_core, optional_trimmed_string, parse_json_metadata,
        parse_proto_chat_images, settings_registry_unavailable_error,
        websocket_ticket_service_unavailable_error,
    };
    use crate::impls::ErrorKind;

    #[test]
    fn build_public_room_list_query_maps_sorting_and_defaults() {
        let query = build_public_room_list_query(crate::proto::client::ListRoomsRequest {
            page: 0,
            page_size: 0,
            search: "alpha".to_string(),
            sort_by: crate::proto::client::RoomListSortBy::Name as i32,
            sort_direction: crate::proto::client::SortDirection::Asc as i32,
        })
        .unwrap();

        assert_eq!(query.pagination.page, 1);
        assert_eq!(query.pagination.page_size, 20);
        assert_eq!(query.search.as_deref(), Some("alpha"));
        assert_eq!(query.status, Some(synctv_core::models::RoomStatus::Active));
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.sort_by, synctv_core::models::RoomListSortBy::Name);
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Asc
        );
    }

    #[test]
    fn build_my_room_list_query_maps_filters_sorting_and_defaults() {
        let query = build_my_room_list_query(crate::proto::client::ListMyRoomsRequest {
            page: 0,
            page_size: 0,
            search: "alpha".to_string(),
            status: synctv_proto::common::RoomStatus::Closed as i32,
            is_banned: Some(false),
            relation: crate::proto::client::MyRoomRelation::Participating as i32,
            sort_by: crate::proto::client::MyRoomListSortBy::Name as i32,
            sort_direction: crate::proto::client::SortDirection::Asc as i32,
        })
        .unwrap();

        assert_eq!(query.pagination.page, 1);
        assert_eq!(query.pagination.page_size, 20);
        assert_eq!(query.search.as_deref(), Some("alpha"));
        assert_eq!(query.status, Some(synctv_core::models::RoomStatus::Closed));
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(
            query.relation,
            synctv_core::models::MyRoomRelation::Participating
        );
        assert_eq!(query.sort_by, synctv_core::models::MyRoomListSortBy::Name);
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Asc
        );
    }

    #[test]
    fn build_my_room_list_query_defaults_relation_to_all() {
        let query = build_my_room_list_query(crate::proto::client::ListMyRoomsRequest {
            page: 1,
            page_size: 20,
            search: String::new(),
            status: synctv_proto::common::RoomStatus::Unspecified as i32,
            is_banned: None,
            relation: crate::proto::client::MyRoomRelation::Unspecified as i32,
            sort_by: crate::proto::client::MyRoomListSortBy::Unspecified as i32,
            sort_direction: crate::proto::client::SortDirection::Unspecified as i32,
        })
        .unwrap();

        assert_eq!(query.relation, synctv_core::models::MyRoomRelation::All);
        assert_eq!(
            query.sort_by,
            synctv_core::models::MyRoomListSortBy::JoinedAt
        );
        assert_eq!(
            query.sort_direction,
            synctv_core::models::SortDirection::Desc
        );
    }

    #[test]
    fn build_my_room_list_query_rejects_too_long_search() {
        let error = build_my_room_list_query(crate::proto::client::ListMyRoomsRequest {
            page: 1,
            page_size: 20,
            search: "a".repeat(101),
            status: synctv_proto::common::RoomStatus::Unspecified as i32,
            is_banned: None,
            relation: crate::proto::client::MyRoomRelation::Unspecified as i32,
            sort_by: crate::proto::client::MyRoomListSortBy::Unspecified as i32,
            sort_direction: crate::proto::client::SortDirection::Unspecified as i32,
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("search"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_public_room_list_query_rejects_invalid_proto_request() {
        let error = build_public_room_list_query(crate::proto::client::ListRoomsRequest {
            page: -1,
            page_size: 101,
            search: "a".repeat(101),
            sort_by: 99,
            sort_direction: 99,
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("page"), "{message}");
                assert!(message.contains("page_size"), "{message}");
                assert!(message.contains("search"), "{message}");
                assert!(message.contains("sort_by"), "{message}");
                assert!(message.contains("sort_direction"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_transfer_room_ownership_request_rejects_invalid_new_owner_user_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_transfer_room_ownership_request(
            crate::proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: "bad-id".to_string(),
            },
            &codec,
        )
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("new_owner_user_id"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_check_room_request_rejects_invalid_room_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_check_room_request(
            crate::proto::client::CheckRoomRequest {
                room_id: "bad-room".to_string(),
            },
            &codec,
        )
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("room_id"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_create_websocket_ticket_request_rejects_invalid_room_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_create_websocket_ticket_request(
            &crate::proto::client::CreateWebSocketTicketRequest {
                room_id: "bad-room".to_string(),
            },
            &codec,
        )
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("room_id"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn build_create_websocket_ticket_request_parses_proto_validated_room_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let room_id = synctv_core::models::RoomId::expect_positive(123);
        let room_public_id = codec.encode_room_id(room_id).unwrap();
        let parsed = build_create_websocket_ticket_request(
            &crate::proto::client::CreateWebSocketTicketRequest {
                room_id: room_public_id,
            },
            &codec,
        )
        .expect("valid room id");

        assert_eq!(parsed, room_id);
    }

    #[test]
    fn build_create_websocket_ticket_request_rejects_proto_valid_but_undecodable_room_id() {
        let codec = crate::PublicIdCodec::default_for_tests();
        let error = build_create_websocket_ticket_request(
            &crate::proto::client::CreateWebSocketTicketRequest {
                room_id: "room_abc".to_string(),
            },
            &codec,
        )
        .expect_err("plain public ID body must decode");

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("RoomId"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn websocket_ticket_service_unavailable_maps_to_service_unavailable() {
        let err = websocket_ticket_service_unavailable_error();

        assert!(matches!(
            err,
            crate::impls::ApiError::ServiceUnavailable(ref message)
                if message == "WebSocket ticket service is not available."
        ));
    }

    #[test]
    fn build_get_chat_history_request_rejects_invalid_limit() {
        let error = build_get_chat_history_request(&crate::proto::client::GetChatHistoryRequest {
            limit: 101,
            cursor: String::new(),
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("limit"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn hot_rooms_validation_rejects_out_of_range_limit() {
        let error =
            crate::impls::validate_proto_request(&crate::proto::client::GetHotRoomsRequest {
                limit: 51,
            })
            .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("limit"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn hot_rooms_validation_allows_default_limit_sentinel() {
        crate::impls::validate_proto_request(&crate::proto::client::GetHotRoomsRequest {
            limit: 0,
        })
        .expect("zero should request the default hot-room limit");
    }

    #[test]
    fn build_get_chat_history_request_rejects_invalid_cursor() {
        let error = build_get_chat_history_request(&crate::proto::client::GetChatHistoryRequest {
            limit: 50,
            cursor: "not-a-cursor".to_string(),
        })
        .unwrap_err();

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(message.contains("Invalid cursor format"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn optional_trimmed_string_normalizes_idempotency_keys() {
        assert_eq!(
            optional_trimmed_string("  client-key  ").as_deref(),
            Some("client-key")
        );
        assert!(optional_trimmed_string(" \n\t ").is_none());
    }

    #[test]
    fn parse_json_metadata_rejects_non_object_values() {
        let error = parse_json_metadata(br#"["tag"]"#).expect_err("metadata should be object");

        match error {
            crate::impls::ApiError::InvalidInput(message) => {
                assert!(
                    message.contains("metadata must be a JSON object"),
                    "{message}"
                );
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn chat_image_proto_roundtrip_preserves_upload_token_metadata() {
        let metadata = serde_json::json!({
            "_synctv_upload_token": "v1.payload.signature",
            "blurhash": "abc"
        });
        let image = synctv_core::models::NewChatImage {
            id: "image-1".to_string(),
            storage_backend: "database".to_string(),
            object_key: "rooms/1/chat/2/image-1".to_string(),
            url: None,
            mime_type: Some("image/webp".to_string()),
            size_bytes: Some(1024),
            width: Some(640),
            height: Some(480),
            metadata: metadata.clone(),
        };

        let proto = super::new_chat_image_to_proto(&image);
        let parsed = parse_proto_chat_images(&[proto]).expect("image should parse");

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].metadata, metadata);
    }

    #[test]
    fn edit_chat_message_request_maps_client_operation_id() {
        let request = crate::proto::client::EditChatMessageRequest {
            message_id: "42".to_string(),
            content: "hello".to_string(),
            expected_version: 7,
            metadata: serde_json::to_vec(&serde_json::json!({"edited": true})).unwrap(),
            client_operation_id: " edit-op-42 ".to_string(),
        };
        let core = edit_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            request,
        )
        .expect("request should map");

        assert_eq!(
            core.room_id,
            synctv_core::models::RoomId::expect_positive(9)
        );
        assert_eq!(
            core.user_id,
            synctv_core::models::UserId::expect_positive(11)
        );
        assert_eq!(core.message_id, 42);
        assert_eq!(core.client_operation_id.as_deref(), Some("edit-op-42"));
        assert_eq!(core.expected_version, Some(7));
    }

    #[test]
    fn delete_chat_message_request_maps_client_operation_id() {
        let request = crate::proto::client::DeleteChatMessageRequest {
            message_id: "42".to_string(),
            expected_version: 7,
            reason: " cleanup ".to_string(),
            client_operation_id: " delete-op-42 ".to_string(),
        };
        let core = delete_chat_message_request_to_core(
            synctv_core::models::RoomId::expect_positive(9),
            synctv_core::models::UserId::expect_positive(11),
            &request,
        )
        .expect("request should map");

        assert_eq!(
            core.room_id,
            synctv_core::models::RoomId::expect_positive(9)
        );
        assert_eq!(
            core.user_id,
            synctv_core::models::UserId::expect_positive(11)
        );
        assert_eq!(core.message_id, 42);
        assert_eq!(core.client_operation_id.as_deref(), Some("delete-op-42"));
        assert_eq!(core.reason.as_deref(), Some("cleanup"));
        assert_eq!(core.expected_version, Some(7));
    }

    #[test]
    fn get_public_settings_missing_registry_is_service_unavailable() {
        let err = settings_registry_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Public settings are not available on this server."
        );
    }
}
