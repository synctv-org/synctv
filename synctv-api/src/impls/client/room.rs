//! Room operations: list, create, get, join, leave, delete, settings, chat, hot rooms, public settings

use crate::impls::ApiError;
use std::collections::HashMap;
use synctv_core::models::{PermissionBits, UserId};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::room::ClientResourceAvailability;

use super::convert::{
    media_to_proto_for_viewer, member_status_to_proto, members_to_proto, playback_state_to_proto,
    resource_availability_enum_to_proto, room_role_to_proto, room_to_proto_basic,
    room_to_proto_with_availability,
};
use super::media::prepare_delete_entries_outbox_fanout;
use super::{validate_password_for_set, validate_password_for_verify};
use super::{ClientApiImpl, GuestRoomAccess, RoomActor};

fn settings_registry_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Public settings are not available on this server.".to_string())
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

fn positive_i64_to_usize(value: i64, default: usize) -> usize {
    usize::try_from(value).unwrap_or(default)
}

fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
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
        Ok(media.map(|m| media_to_proto_for_viewer(&m, true, Some(uid), &self.public_id_codec)))
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
            room_list.push(room_to_proto_with_availability(
                r,
                settings,
                member_count,
                availability,
                &self.public_id_codec,
            ));
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
                room: Some(room_to_proto_basic(
                    room,
                    Some(&settings),
                    Some(*member_count),
                    &self.public_id_codec,
                )),
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
            room: Some(room_to_proto_basic(
                &room,
                Some(&response_settings),
                self.load_room_member_count(&room.id).await?,
                &self.public_id_codec,
            )),
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
            room: Some(room_to_proto_basic(
                &room,
                Some(&settings),
                self.load_room_member_count(&rid).await?,
                &self.public_id_codec,
            )),
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
            room: Some(room_to_proto_basic(
                &room,
                Some(&room_settings),
                self.load_room_member_count(&rid).await?,
                &self.public_id_codec,
            )),
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
            room: Some(room_to_proto_basic(
                &room,
                Some(&snapshot.settings),
                self.load_room_member_count(&rid).await?,
                &self.public_id_codec,
            )),
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
                synctv_core::models::PermissionBits::SET_ROOM_SETTINGS,
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
            room: Some(room_to_proto_basic(
                &room,
                Some(&settings),
                self.load_room_member_count(&rid).await?,
                &self.public_id_codec,
            )),
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
            room_ttl: s.room_ttl,
            room_must_need_pwd: s.room_must_need_pwd,
            enable_password_signup: s.enable_password_signup,
            password_signup_need_review: s.password_signup_need_review,
            enable_email_signup: s.enable_email_signup,
            email_signup_need_review: s.email_signup_need_review,
            enable_webauthn_signup: s.enable_webauthn_signup,
            webauthn_signup_need_review: s.webauthn_signup_need_review,
            enable_guest: s.enable_guest,
            movie_proxy: s.movie_proxy,
            live_proxy: s.live_proxy,
            ts_disguised_as_png: s.ts_disguised_as_png,
            custom_publish_host: s.custom_publish_host,
            email_whitelist_enabled: s.email_whitelist_enabled,
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

        let hot_rooms: Vec<crate::proto::client::RoomWithStats> = top_rooms
            .into_iter()
            .map(|(room, online_count)| {
                let total_members = member_counts.get(&room.id).copied().unwrap_or(0);
                let settings = settings_map.get(&room.id);
                let availability = *availability_map
                    .get(&room.id)
                    .unwrap_or(&ClientResourceAvailability::Available);

                crate::proto::client::RoomWithStats {
                    room: Some(room_to_proto_with_availability(
                        &room,
                        settings,
                        Some(total_members),
                        availability,
                        &self.public_id_codec,
                    )),
                    online_count,
                    total_members,
                }
            })
            .collect();

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
        let (messages, next) = self
            .room_service
            .get_chat_history_cursor(rid, cursor, limit)
            .await
            .map_err(ApiError::from)?;
        let next_cursor_str = next.map(|(ts, id)| {
            format!(
                "{}|{}",
                synctv_common::time::format_datetime_rfc3339(ts),
                id
            )
        });

        // Collect unique user IDs to batch fetch usernames
        let user_ids: Vec<synctv_core::models::UserId> = messages
            .iter()
            .filter_map(|m| m.user_id)
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
                let (user_id_str, username) = match &m.user_id {
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

                crate::proto::client::ChatMessageReceive {
                    id: m.id.to_string(),
                    room_id: self
                        .public_id_codec
                        .encode_room_id(m.room_id)
                        .expect("chat message room id must be encodable"),
                    user_id: user_id_str,
                    username,
                    content: m.content,
                    timestamp: m.created_at.timestamp(),
                    position: None, // History messages don't show as danmaku
                    color: None,
                }
            })
            .collect();

        Ok(crate::proto::client::GetChatHistoryResponse {
            messages: proto_messages,
            next_cursor: next_cursor_str.unwrap_or_default(),
        })
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
        self.require_room_permission(actor, PermissionBits::VIEW_CHAT_HISTORY)
            .await?;
        self.get_chat_history_for_room_id(&actor.room_id(), req)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_check_room_request, build_create_websocket_ticket_request,
        build_get_chat_history_request, build_my_room_list_query, build_public_room_list_query,
        build_transfer_room_ownership_request, settings_registry_unavailable_error,
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
    fn get_public_settings_missing_registry_is_service_unavailable() {
        let err = settings_registry_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Public settings are not available on this server."
        );
    }
}
