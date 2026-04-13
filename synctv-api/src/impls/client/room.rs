//! Room operations: list, create, get, join, leave, delete, settings, chat, hot rooms, public settings

use crate::impls::ApiError;
use synctv_core::models::UserId;
use synctv_core::service::room::ClientResourceAvailability;

use super::convert::{
    media_to_proto, member_status_to_proto, members_to_proto, playback_state_to_proto,
    resource_availability_enum_to_proto, room_role_to_proto, room_to_proto_basic,
    room_to_proto_with_availability,
};
use super::ClientApiImpl;
use super::{validate_password_for_set, validate_password_for_verify};

fn settings_registry_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Public settings are not available on this server.".to_string())
}

const DEFAULT_ROOM_PAGE: u32 = 1;
const DEFAULT_ROOM_PAGE_SIZE: u32 = 20;
const MAX_ROOM_PAGE_SIZE: u32 = 100;
const DEFAULT_HOT_ROOM_LIMIT: i64 = 10;
const DEFAULT_HOT_ROOM_LIMIT_U32: u32 = 10;
const DEFAULT_HOT_ROOM_LIMIT_USIZE: usize = 10;
const MAX_HOT_ROOM_LIMIT_I32: i32 = 50;
const HOT_ROOM_FETCH_MULTIPLIER: u32 = 4;
const HOT_ROOM_FETCH_LIMIT_CAP: u32 = 200;
const MIN_PASSWORD_CHECK_DELAY_MS: u64 = 250;

fn positive_i32_to_u32(value: i32, default: u32) -> u32 {
    if value > 0 {
        value.cast_unsigned()
    } else {
        default
    }
}

fn positive_i64_to_u32(value: i64, default: u32) -> u32 {
    u32::try_from(value).unwrap_or(default)
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
        status: match synctv_proto::common::RoomStatus::try_from(req.status) {
            Ok(synctv_proto::common::RoomStatus::Active) => {
                Some(synctv_core::models::RoomStatus::Active)
            }
            Ok(synctv_proto::common::RoomStatus::Pending) => {
                Some(synctv_core::models::RoomStatus::Pending)
            }
            Ok(synctv_proto::common::RoomStatus::Rejected) => {
                Some(synctv_core::models::RoomStatus::Rejected)
            }
            Ok(synctv_proto::common::RoomStatus::Closed) => {
                Some(synctv_core::models::RoomStatus::Closed)
            }
            _ => None,
        },
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
) -> Result<UserId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    Ok(crate::impls::proto_validated_user_id(req.new_owner_user_id))
}

fn build_check_room_request(
    req: crate::proto::client::CheckRoomRequest,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(&req)?;
    Ok(crate::impls::proto_validated_room_id(req.room_id))
}

pub(crate) fn build_create_websocket_ticket_request(
    req: &crate::proto::client::CreateWebSocketTicketRequest,
) -> Result<synctv_core::models::RoomId, ApiError> {
    crate::impls::validate_proto_request(req)?;
    Ok(crate::impls::proto_validated_room_id(req.room_id.clone()))
}

type ChatHistoryCursor = (chrono::DateTime<chrono::Utc>, String);

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
        Some((ts, id.to_string()))
    } else {
        return Err(ApiError::InvalidInput("Invalid cursor format".to_string()));
    };

    Ok((limit, cursor))
}

impl ClientApiImpl {
    /// Get the currently playing media for a room.
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_playing_media(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<Option<crate::proto::client::Media>, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

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
        Ok(media.map(|m| media_to_proto(&m)))
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

        // Batch-fetch distributed online user counts (single Redis-backed lookup) to avoid N+1 queries
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let counts = self
            .connection_service
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;

        let mut room_list = Vec::with_capacity(rooms.len());
        for (r, count) in rooms.iter().zip(counts) {
            let member_count: Option<i32> = count.try_into().ok();
            let availability = *availability_map
                .get(&r.id)
                .unwrap_or(&ClientResourceAvailability::Available);
            room_list.push(room_to_proto_with_availability(
                r,
                None,
                member_count,
                availability,
            ));
        }

        Ok(crate::proto::client::ListRoomsResponse {
            rooms: room_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn list_my_rooms(
        &self,
        user_id: &str,
        req: crate::proto::client::ListMyRoomsRequest,
    ) -> Result<crate::proto::client::ListMyRoomsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let query = build_my_room_list_query(req)?;
        let (rooms, total) = self
            .room_service
            .list_accessible_joined_rooms_with_query(&uid, &query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch distributed online user counts to avoid N+1 queries
        let room_id_refs: Vec<&synctv_core::models::RoomId> =
            rooms.iter().map(|(r, _, _, _)| &r.id).collect();
        let counts = self
            .connection_service
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;

        // Batch-fetch room settings for three-layer permission calculation (A6 fix)
        let room_id_strs: Vec<&str> = rooms.iter().map(|(r, _, _, _)| r.id.as_str()).collect();
        let room_settings_map = self
            .room_service
            .get_room_settings_batch(&room_id_strs)
            .await
            .unwrap_or_default();

        let mut room_list = Vec::with_capacity(rooms.len());
        for ((room, role, _status, _member_count), count) in rooms.iter().zip(counts) {
            // A6 fix: Use proper three-layer permission calculation instead of
            // role.permissions() which only gives role-level defaults.
            // calculate_role_default_permissions applies:
            //   1. Global default permissions (from SettingsRegistry)
            //   2. Room-level overrides (room_added / room_removed)
            let settings = room_settings_map
                .get(room.id.as_str())
                .cloned()
                .unwrap_or_default();
            let permissions = self
                .room_service
                .permission_service()
                .calculate_role_default_permissions(role, &settings)
                .0;
            let member_count: Option<i32> = count.try_into().ok();
            let relation = if room.created_by == uid {
                crate::proto::client::MyRoomRelation::Created as i32
            } else {
                crate::proto::client::MyRoomRelation::Participating as i32
            };
            room_list.push(crate::proto::client::MyRoom {
                room: Some(room_to_proto_basic(room, None, member_count)),
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
        user_id: &str,
        mut req: crate::proto::client::CreateRoomRequest,
    ) -> Result<crate::proto::client::CreateRoomResponse, ApiError> {
        // Validate and sanitize room name
        req.name = crate::http::validation::validate_room_name(&req.name)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        // Validate and sanitize room description against ROOM_DESCRIPTION_MAX
        if !req.description.is_empty() {
            req.description = crate::http::validation::validate_room_description(&req.description)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        crate::impls::validate_proto_request(&req)?;

        let uid = UserId::from_string(user_id.to_string());

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
        let cluster_event = self
            .cluster_fanout
            .reserve("failed to fan out RoomCreated to cluster replicas")
            .await?;

        let (room, _member) = self
            .room_service
            .create_room(req.name, req.description, uid.clone(), password, settings)
            .await
            .map_err(ApiError::from)?;

        self.cluster_fanout.publish(
            cluster_event,
            synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::RoomCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room.id.clone(),
                    room_name: room.name.clone(),
                    creator_id: uid,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&room.id)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::CreateRoomResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
        })
    }

    pub async fn get_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

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
            .map(|s| playback_state_to_proto(&s));

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&rid)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::GetRoomResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
            playback_state,
        })
    }

    pub async fn join_room(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::JoinRoomRequest,
        client_ip: Option<&str>,
    ) -> Result<crate::proto::client::JoinRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        let password = if req.password.is_empty() {
            None
        } else {
            validate_password_for_verify(&req.password)?;
            Some(req.password)
        };

        if let Some(password) = password.as_ref() {
            let start = std::time::Instant::now();
            let parsed_client_ip = client_ip.and_then(|ip| ip.parse().ok());

            let valid = self
                .room_service
                .check_room_password_with_rate_limit(&rid, password, parsed_client_ip)
                .await
                .map_err(ApiError::from)?;

            let elapsed = start.elapsed();
            let min_delay = std::time::Duration::from_millis(MIN_PASSWORD_CHECK_DELAY_MS);
            if elapsed < min_delay {
                tokio::time::sleep(
                    min_delay
                        .checked_sub(elapsed)
                        .expect("elapsed < min_delay guaranteed by if-check above"),
                )
                .await;
            }

            if !valid {
                return Err(ApiError::Authorization(
                    "Forbidden: Invalid password".to_string(),
                ));
            }
        }

        let (_room, member, members) = self
            .room_service
            .join_room(rid.clone(), uid, password)
            .await
            .map_err(ApiError::from)?;

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
            .map(|s| playback_state_to_proto(&s));

        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let proto_members = members_to_proto(
            members,
            &room_settings,
            self.room_service.permission_service(),
        );

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&rid)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::JoinRoomResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
            members: proto_members,
            playback_state,
            membership_status: member_status_to_proto(member.status),
            requires_approval: member.status.is_pending(),
        })
    }

    pub async fn leave_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::LeaveRoomResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Resolve username for the UserLeft event before performing the leave
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        let cluster_event = self
            .cluster_fanout
            .reserve("failed to fan out UserLeft to cluster replicas")
            .await?;

        self.room_service
            .leave_room(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the user's connections from this room (local)
        self.connection_service
            .disconnect_user_from_room(&uid, &rid);

        self.cluster_fanout.publish(
            cluster_event,
            synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::UserLeft {
                    event_id: synctv_common::snanoid!(16),
                    room_id: rid,
                    user_id: uid,
                    username,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        Ok(crate::proto::client::LeaveRoomResponse { success: true })
    }

    pub async fn delete_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::DeleteRoomResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let cluster_event = self
            .cluster_fanout
            .reserve("failed to fan out RoomDeleted to cluster replicas")
            .await?;

        // 1. Delete the DB record first. If this fails, no cluster event is
        //    published and no connections are dropped -- the room remains intact.
        self.room_service
            .delete_room(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        self.cluster_fanout.publish(
            cluster_event,
            synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::RoomDeleted {
                    event_id: synctv_common::snanoid!(16),
                    room_id: rid.clone(),
                    deleted_by: uid,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        // 3. Force disconnect all local connections in the deleted room
        self.connection_service.disconnect_room(&rid);

        Ok(crate::proto::client::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_settings(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdateRoomSettingsRequest,
    ) -> Result<crate::proto::client::UpdateRoomSettingsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        if req.settings.is_empty() {
            // SECURITY: Return success with None room instead of room details.
            // Previously this returned room data after only checking membership,
            // which allowed any room member to bypass UPDATE_ROOM_SETTINGS permission.
            // Users should use get_room or get_room_settings endpoints to fetch room info.
            return Ok(crate::proto::client::UpdateRoomSettingsResponse { room: None });
        }

        let settings: synctv_core::models::RoomSettings = serde_json::from_slice(&req.settings)?;
        let room_settings_fanout = self
            .room_settings_fanout
            .reserve_settings_changed(self.cluster_fanout.as_ref())
            .await?;
        self.room_service
            .set_settings(rid.clone(), uid.clone(), settings)
            .await
            .map_err(ApiError::from)?;

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();

        self.room_settings_fanout
            .publish_settings_changed(
                room_settings_fanout,
                &rid,
                &uid,
                &username,
                req.settings.clone(),
            );

        // Get updated room
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&rid)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::UpdateRoomSettingsResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
        })
    }

    // === Room Password Operations ===

    /// Set or remove room password
    pub async fn set_room_password(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::SetRoomPasswordRequest,
    ) -> Result<crate::proto::client::SetRoomPasswordResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

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
        let cache_invalidation = self.reserve_room_cache_invalidation(&rid).await?;

        self.room_service
            .update_room_password(&rid, password_hash)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to update password: {e}")))?;

        // Invalidate room cache on other replicas so password check uses fresh data
        if let Some(cache_invalidation) = cache_invalidation {
            cache_invalidation.publish(Self::build_room_cache_invalidation_request(&rid));
        }

        Ok(crate::proto::client::SetRoomPasswordResponse { success: true })
    }

    // === Room Settings Operations ===

    /// Get room settings
    ///
    /// Requires the caller to be a member of the room.
    pub async fn get_room_settings(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomSettingsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Check membership before returning settings
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;

        let settings_bytes = serde_json::to_vec(&settings)
            .map_err(|e| ApiError::Internal(format!("Failed to serialize settings: {e}")))?;

        Ok(crate::proto::client::GetRoomSettingsResponse {
            settings: settings_bytes,
        })
    }

    /// Reset room settings to defaults
    pub async fn reset_room_settings(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::ResetRoomSettingsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let room_settings_fanout = self
            .room_settings_fanout
            .reserve_settings_changed(self.cluster_fanout.as_ref())
            .await?;
        let settings_json = self
            .room_service
            .reset_room_settings(&rid, &uid)
            .await
            .map_err(ApiError::from)?;

        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();
        self.room_settings_fanout
            .publish_settings_changed(
                room_settings_fanout,
                &rid,
                &uid,
                &username,
                settings_json.as_bytes().to_vec(),
            );

        Ok(crate::proto::client::ResetRoomSettingsResponse {
            settings: settings_json.into_bytes(),
        })
    }

    pub async fn transfer_room_ownership(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::TransferRoomOwnershipRequest,
    ) -> Result<crate::proto::client::TransferRoomOwnershipResponse, ApiError> {
        let current_owner_id = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;
        let new_owner_id = build_transfer_room_ownership_request(req)?;

        let room = self
            .room_service
            .transfer_room_ownership(rid.clone(), current_owner_id, new_owner_id)
            .await
            .map_err(Self::map_room_access_error)?;

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&rid)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::TransferRoomOwnershipResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
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
            signup_enabled: s.signup_enabled,
            allow_room_creation: s.allow_room_creation,
            max_rooms_per_user: s.max_rooms_per_user,
            max_members_per_room: s.max_members_per_room,
            disable_create_room: s.disable_create_room,
            create_room_need_review: s.create_room_need_review,
            room_ttl: s.room_ttl,
            room_must_need_pwd: s.room_must_need_pwd,
            signup_need_review: s.signup_need_review,
            enable_password_signup: s.enable_password_signup,
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
        let rid = build_check_room_request(req)?;

        match self.room_service.get_room(&rid).await {
            Ok(room) => {
                let settings = self
                    .room_service
                    .get_room_settings(&rid)
                    .await
                    .unwrap_or_default();
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

        let limit = if req.limit <= 0 || req.limit > MAX_HOT_ROOM_LIMIT_I32 {
            DEFAULT_HOT_ROOM_LIMIT
        } else {
            i64::from(req.limit)
        };

        // Query for active, non-banned rooms.
        // Fetch a bounded set (4x the requested limit, capped at 200) to reduce DB
        // and memory overhead while still providing a reasonable candidate pool for
        // sorting by online count.
        let fetch_limit = positive_i64_to_u32(limit, DEFAULT_HOT_ROOM_LIMIT_U32)
            .saturating_mul(HOT_ROOM_FETCH_MULTIPLIER)
            .min(HOT_ROOM_FETCH_LIMIT_CAP);
        let query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(Some(1), Some(fetch_limit)),
            search: None,
            status: Some(synctv_core::models::RoomStatus::Active),
            is_banned: Some(false),
            creator_id: None,
            ..Default::default()
        };

        let (rooms, _total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;
        let availability_map = self
            .room_service
            .room_availability_batch(&rooms)
            .await
            .map_err(ApiError::from)?;

        // Fetch distributed connection counts for all candidate rooms (single Redis MGET),
        // then sort by distributed count to get a globally correct ranking.
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let distributed_counts = self
            .connection_service
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;

        let mut room_online: Vec<(synctv_core::models::Room, i32)> = rooms
            .into_iter()
            .zip(distributed_counts)
            .map(|(room, count)| (room, usize_to_i32_saturating(count)))
            .collect();
        room_online.sort_by_key(|item| std::cmp::Reverse(item.1));
        let top_rooms: Vec<_> = room_online
            .into_iter()
            .take(positive_i64_to_usize(limit, DEFAULT_HOT_ROOM_LIMIT_USIZE))
            .collect();

        // Batch-fetch member counts for the top N rooms (single SQL query instead of N+1)
        let top_room_id_refs: Vec<&synctv_core::models::RoomId> =
            top_rooms.iter().map(|(r, _)| &r.id).collect();
        let member_counts = self
            .room_service
            .get_member_count_batch(&top_room_id_refs)
            .await
            .unwrap_or_default();

        // Batch-fetch settings for the top N rooms
        let room_ids: Vec<&str> = top_rooms.iter().map(|(r, _)| r.id.as_str()).collect();
        let settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .unwrap_or_default();

        let hot_rooms: Vec<crate::proto::client::RoomWithStats> = top_rooms
            .into_iter()
            .map(|(room, online_count)| {
                let total_members = member_counts.get(room.id.as_str()).copied().unwrap_or(0);
                let settings = settings_map.get(room.id.as_str());
                let availability = *availability_map
                    .get(&room.id)
                    .unwrap_or(&ClientResourceAvailability::Available);

                crate::proto::client::RoomWithStats {
                    room: Some(room_to_proto_with_availability(
                        &room,
                        settings,
                        Some(online_count),
                        availability,
                    )),
                    online_count,
                    total_members,
                }
            })
            .collect();

        Ok(crate::proto::client::GetHotRoomsResponse { rooms: hot_rooms })
    }

    // === Chat Operations ===

    pub async fn get_chat_history(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetChatHistoryRequest,
    ) -> Result<crate::proto::client::GetChatHistoryResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = Self::parse_room_id(room_id)?;

        // Check membership before returning chat history
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        let (limit, cursor) = build_get_chat_history_request(&req)?;
        let (messages, next) = self
            .room_service
            .get_chat_history_cursor(
                &rid,
                cursor.as_ref().map(|(ts, id)| (*ts, id.as_str())),
                limit,
            )
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
            .filter_map(|m| m.user_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Batch fetch usernames (single query instead of N+1)
        let username_map: std::collections::HashMap<String, String> = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, name)| (id.to_string(), name))
            .collect();

        // Convert to proto format
        let proto_messages = messages
            .into_iter()
            .map(|m| {
                let (user_id_str, username) = match &m.user_id {
                    Some(uid) => {
                        let uid_str = uid.as_str().to_string();
                        let name = username_map
                            .get(&uid_str)
                            .cloned()
                            .unwrap_or_else(|| format!("user_{uid_str}"));
                        (uid_str, name)
                    }
                    None => (String::new(), "[deleted]".to_string()),
                };

                crate::proto::client::ChatMessageReceive {
                    id: m.id.clone(),
                    room_id: m.room_id.as_str().to_string(),
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
}

#[cfg(test)]
mod tests {
    use super::{
        build_check_room_request, build_create_websocket_ticket_request,
        build_get_chat_history_request, build_my_room_list_query, build_public_room_list_query,
        build_transfer_room_ownership_request, settings_registry_unavailable_error,
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
        let error = build_transfer_room_ownership_request(
            crate::proto::client::TransferRoomOwnershipRequest {
                new_owner_user_id: "bad-id".to_string(),
            },
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
        let error = build_check_room_request(crate::proto::client::CheckRoomRequest {
            room_id: "bad-room".to_string(),
        })
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
        let error = build_create_websocket_ticket_request(
            &crate::proto::client::CreateWebSocketTicketRequest {
                room_id: "bad-room".to_string(),
            },
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
        let room_id = synctv_common::snanoid!(12);
        let parsed = build_create_websocket_ticket_request(
            &crate::proto::client::CreateWebSocketTicketRequest {
                room_id: room_id.clone(),
            },
        )
        .expect("valid room id");

        assert_eq!(parsed.as_str(), room_id);
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
