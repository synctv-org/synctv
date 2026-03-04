//! Room operations: list, create, get, join, leave, delete, settings, chat, hot rooms, public settings

use crate::impls::ApiError;
use synctv_core::models::{RoomId, UserId};

use super::convert::{
    media_to_proto, members_to_proto, playback_state_to_proto, room_role_to_proto,
    room_to_proto_basic,
};
use super::ClientApiImpl;
use super::{validate_password_for_set, validate_password_for_verify};

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
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning playing media
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

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
        // Clamp negative i32 values to valid u32 range before passing to PageParams.
        // Without this, -1i32 as u32 wraps to u32::MAX, causing absurd page numbers.
        let page = u32::try_from(req.page).unwrap_or(1);
        let page_size = u32::try_from(req.page_size).unwrap_or(1);

        let mut query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(Some(page), Some(page_size)),
            ..Default::default()
        };
        if !req.search.is_empty() {
            query.search = Some(req.search);
        }
        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch distributed connection counts (single Redis MGET) to avoid N+1 queries
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let counts = self
            .connection_manager
            .room_connection_count_distributed_batch(&room_id_refs)
            .await;

        let mut room_list = Vec::with_capacity(rooms.len());
        for (r, count) in rooms.iter().zip(counts) {
            let member_count: Option<i32> = count.try_into().ok();
            room_list.push(room_to_proto_basic(r, None, member_count));
        }

        Ok(crate::proto::client::ListRoomsResponse {
            rooms: room_list,
            total: total as i32,
        })
    }

    pub async fn get_joined_rooms(
        &self,
        user_id: &str,
        page: i32,
        page_size: i32,
    ) -> Result<crate::proto::client::ListParticipatedRoomsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        // Clamp negative i32 values to valid u32 range before passing to PageParams.
        let page = u32::try_from(page).unwrap_or(1);
        let page_size = u32::try_from(page_size).unwrap_or(1);
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));
        let (rooms, total) = self
            .room_service
            .list_joined_rooms_with_details(&uid, pagination)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch distributed connection counts (single Redis MGET) to avoid N+1 queries
        let room_id_refs: Vec<&synctv_core::models::RoomId> =
            rooms.iter().map(|(r, _, _, _)| &r.id).collect();
        let counts = self
            .connection_manager
            .room_connection_count_distributed_batch(&room_id_refs)
            .await;

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
            room_list.push(crate::proto::client::RoomWithRole {
                room: Some(room_to_proto_basic(room, None, member_count)),
                permissions,
                role: room_role_to_proto(*role),
            });
        }

        Ok(crate::proto::client::ListParticipatedRoomsResponse {
            rooms: room_list,
            total: total as i32,
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

        let uid = UserId::from_string(user_id.to_string());

        let settings = if req.settings.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&req.settings)?)
        };

        let password = if req.password.is_empty() {
            None
        } else {
            Some(req.password)
        };

        let (room, _member) = self
            .room_service
            .create_room(req.name, req.description, uid.clone(), password, settings)
            .await
            .map_err(ApiError::from)?;

        // Publish RoomCreated cluster event for cross-replica propagation (non-blocking)
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::RoomCreated {
                        event_id: nanoid::nanoid!(16),
                        room_id: room.id.clone(),
                        room_name: room.name.clone(),
                        creator_id: uid,
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        let member_count = self
            .connection_manager
            .room_connection_count_distributed(&room.id)
            .await
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
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

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
            .connection_manager
            .room_connection_count_distributed(&rid)
            .await
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
    ) -> Result<crate::proto::client::JoinRoomResponse, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        let password = if req.password.is_empty() {
            None
        } else {
            Some(req.password)
        };

        let (_room, _member, members) = self
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
            .connection_manager
            .room_connection_count_distributed(&rid)
            .await
            .try_into()
            .ok();

        Ok(crate::proto::client::JoinRoomResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
            members: proto_members,
            playback_state,
        })
    }

    pub async fn leave_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::LeaveRoomResponse, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Resolve username for the UserLeft event before performing the leave
        let username = self
            .user_service
            .get_user(&uid)
            .await
            .map(|u| u.username)
            .unwrap_or_default();

        self.room_service
            .leave_room(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the user's connections from this room (local)
        self.connection_manager
            .disconnect_user_from_room(&uid, &rid);

        // Publish UserLeft cluster event so other replicas also disconnect this user.
        // Using UserLeft (not KickUserFromRoom) to correctly distinguish voluntary
        // departure from administrative kicks in audit logs. (non-blocking)
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::UserLeft {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid,
                        user_id: uid,
                        username,
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        Ok(crate::proto::client::LeaveRoomResponse { success: true })
    }

    pub async fn delete_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::DeleteRoomResponse, ApiError> {
        crate::http::validation::validate_id(room_id, "room_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid room_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // 1. Delete the DB record first. If this fails, no cluster event is
        //    published and no connections are dropped -- the room remains intact.
        self.room_service
            .delete_room(rid.clone(), uid.clone())
            .await
            .map_err(ApiError::from)?;

        // 2. Publish RoomDeleted cluster event so other replicas disconnect
        //    their users. Only reached after successful DB deletion. (non-blocking)
        if let Some(ref tx) = self.redis_publish_tx {
            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::RoomDeleted {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        deleted_by: uid,
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // 3. Force disconnect all local connections in the deleted room
        self.connection_manager.disconnect_room(&rid);

        Ok(crate::proto::client::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_settings(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdateRoomSettingsRequest,
    ) -> Result<crate::proto::client::UpdateRoomSettingsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        if req.settings.is_empty() {
            // SECURITY: Return success with None room instead of room details.
            // Previously this returned room data after only checking membership,
            // which allowed any room member to bypass UPDATE_ROOM_SETTINGS permission.
            // Users should use get_room or get_room_settings endpoints to fetch room info.
            return Ok(crate::proto::client::UpdateRoomSettingsResponse { room: None });
        }

        let settings: synctv_core::models::RoomSettings = serde_json::from_slice(&req.settings)?;

        self.room_service
            .set_settings(rid.clone(), uid.clone(), settings)
            .await
            .map_err(ApiError::from)?;

        // Publish RoomSettingsChanged cluster event for cross-replica propagation (non-blocking)
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();

            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::RoomSettingsChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: uid,
                        username,
                        settings_json: req.settings.clone(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        // Get updated room
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        let member_count = self
            .connection_manager
            .room_connection_count_distributed(&rid)
            .await
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
        let rid = RoomId::from_string(room_id.to_string());

        // Validate password length
        if !req.password.is_empty() {
            validate_password_for_set(&req.password)?;
        }

        // Check permission
        self.room_service
            .check_permission(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::UPDATE_ROOM_SETTINGS,
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

        self.room_service
            .update_room_password(&rid, password_hash)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to update password: {e}")))?;

        // Invalidate room cache on other replicas so password check uses fresh data
        self.publish_room_cache_invalidation(&rid);

        Ok(crate::proto::client::SetRoomPasswordResponse { success: true })
    }

    /// Check room password validity
    ///
    /// Enforces per-IP per-room rate limiting to prevent brute-force attacks.
    /// After 5 failed attempts within a 5-minute window, further attempts are
    /// temporarily blocked.
    ///
    /// # Timing Attack Protection
    ///
    /// This method includes a fixed delay to prevent timing attacks that could
    /// leak information about password validity through response time differences.
    /// The delay ensures that both successful and failed password checks take
    /// approximately the same amount of time.
    pub async fn check_room_password(
        &self,
        room_id: &str,
        req: crate::proto::client::CheckRoomPasswordRequest,
        client_ip: &str,
    ) -> Result<crate::proto::client::CheckRoomPasswordResponse, ApiError> {
        let rid = RoomId::from_string(room_id.to_string());

        // --- Rate limit: per-IP per-room password check (5 attempts / 300s window) ---
        const MAX_PASSWORD_ATTEMPTS: u32 = 5;
        const PASSWORD_WINDOW_SECONDS: u64 = 300; // 5 minutes
        if let Some(ref rate_limiter) = self.rate_limiter {
            let rate_key = format!("room_password_check:{client_ip}:{room_id}");
            if let Err(e) = rate_limiter
                .check_rate_limit(&rate_key, MAX_PASSWORD_ATTEMPTS, PASSWORD_WINDOW_SECONDS)
                .await
            {
                tracing::warn!(
                    client_ip = %client_ip,
                    room_id = %room_id,
                    "Room password check rate limit exceeded"
                );
                return Err(ApiError::from(synctv_core::Error::from(e)));
            }
        }

        // Validate password length
        validate_password_for_verify(&req.password)?;

        // Verify room exists
        self.room_service
            .get_room(&rid)
            .await
            .map_err(|e| ApiError::NotFound(format!("Room not found: {e}")))?;

        // Record start time for timing attack protection
        let start = std::time::Instant::now();

        let valid = self
            .room_service
            .check_room_password(&rid, &req.password)
            .await
            .map_err(|e| ApiError::Internal(format!("Password verification failed: {e}")))?;

        if !valid {
            tracing::info!(
                client_ip = %client_ip,
                room_id = %room_id,
                "Failed room password check attempt"
            );
        }

        // --- Timing attack protection: fixed minimum delay ---
        // Ensure the total time is at least MIN_PASSWORD_CHECK_DELAY_MS regardless of
        // whether the password was correct or not. This prevents attackers from
        // distinguishing between valid and invalid passwords based on response time.
        // Using 250ms to provide robust protection against network-based timing attacks
        // while keeping response times acceptable for legitimate users.
        const MIN_PASSWORD_CHECK_DELAY_MS: u64 = 250;
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

        Ok(crate::proto::client::CheckRoomPasswordResponse { valid })
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
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning settings
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

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
        let rid = RoomId::from_string(room_id.to_string());

        let settings_json = self
            .room_service
            .reset_room_settings(&rid, &uid)
            .await
            .map_err(ApiError::from)?;

        // Broadcast RoomSettingsChanged cluster event for cross-replica propagation (non-blocking)
        if let Some(ref tx) = self.redis_publish_tx {
            let username = self
                .user_service
                .get_user(&uid)
                .await
                .map(|u| u.username)
                .unwrap_or_default();

            crate::impls::try_publish_cluster_event(
                tx,
                synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::RoomSettingsChanged {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid,
                        user_id: uid,
                        username,
                        settings_json: settings_json.as_bytes().to_vec(),
                        timestamp: chrono::Utc::now(),
                    },
                },
            );
        }

        Ok(crate::proto::client::ResetRoomSettingsResponse {
            settings: settings_json.into_bytes(),
        })
    }

    /// List rooms created by a user
    pub async fn list_created_rooms(
        &self,
        user_id: &str,
        req: crate::proto::client::ListCreatedRoomsRequest,
    ) -> Result<crate::proto::client::ListCreatedRoomsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());

        let pagination = synctv_core::models::PageParams::new(
            Some(u32::try_from(req.page).unwrap_or(1)),
            Some(
                u32::try_from(req.page_size)
                    .ok()
                    .filter(|&ps| ps > 0 && ps <= 50)
                    .unwrap_or(10),
            ),
        );

        let (rooms_with_count, total) = self
            .room_service
            .list_rooms_by_creator_with_count(&uid, pagination)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch settings for all rooms
        let room_ids: Vec<&str> = rooms_with_count
            .iter()
            .map(|rwc| rwc.room.id.as_str())
            .collect();
        let settings_map = self
            .room_service
            .get_room_settings_batch(&room_ids)
            .await
            .unwrap_or_default();

        let rooms = rooms_with_count
            .into_iter()
            .map(|rwc| {
                let settings = settings_map.get(rwc.room.id.as_str()).cloned();
                room_to_proto_basic(&rwc.room, settings.as_ref(), Some(rwc.member_count))
            })
            .collect();

        Ok(crate::proto::client::ListCreatedRoomsResponse {
            rooms,
            total: total as i32,
        })
    }

    /// Get public settings
    pub fn get_public_settings(
        &self,
    ) -> Result<crate::proto::client::GetPublicSettingsResponse, ApiError> {
        let reg = self
            .settings_registry
            .as_ref()
            .ok_or_else(|| ApiError::Internal("Settings registry not configured".to_string()))?;

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
        let rid = RoomId::from_string(req.room_id);

        match self.room_service.get_room(&rid).await {
            Ok(_room) => {
                let settings = self
                    .room_service
                    .get_room_settings(&rid)
                    .await
                    .unwrap_or_default();
                Ok(crate::proto::client::CheckRoomResponse {
                    exists: true,
                    requires_password: settings.require_password.0,
                    name: String::new(),
                })
            }
            Err(_) => Ok(crate::proto::client::CheckRoomResponse {
                exists: false,
                requires_password: false,
                name: String::new(),
            }),
        }
    }

    pub async fn get_hot_rooms(
        &self,
        req: crate::proto::client::GetHotRoomsRequest,
    ) -> Result<crate::proto::client::GetHotRoomsResponse, ApiError> {
        let limit = if req.limit <= 0 || req.limit > 50 {
            10
        } else {
            i64::from(req.limit)
        };

        // Query for active, non-banned rooms.
        // Fetch a bounded set (4x the requested limit, capped at 200) to reduce DB
        // and memory overhead while still providing a reasonable candidate pool for
        // sorting by online count.
        let fetch_limit = ((limit as u32) * 4).min(200);
        let query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(Some(1), Some(fetch_limit)),
            search: None,
            status: Some(synctv_core::models::RoomStatus::Active),
            is_banned: Some(false),
            creator_id: None,
        };

        let (rooms, _total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to list rooms: {e}")))?;

        // Fetch distributed connection counts for all candidate rooms (single Redis MGET),
        // then sort by distributed count to get a globally correct ranking.
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let distributed_counts = self
            .connection_manager
            .room_connection_count_distributed_batch(&room_id_refs)
            .await;

        let mut room_online: Vec<(synctv_core::models::Room, i32)> = rooms
            .into_iter()
            .zip(distributed_counts)
            .map(|(room, count)| (room, count as i32))
            .collect();
        room_online.sort_by_key(|item| std::cmp::Reverse(item.1));
        let top_rooms: Vec<_> = room_online.into_iter().take(limit as usize).collect();

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
                let member_count = member_counts.get(room.id.as_str()).copied().unwrap_or(0);
                let settings = settings_map.get(room.id.as_str());

                let room_proto = room_to_proto_basic(&room, settings, Some(member_count));

                crate::proto::client::RoomWithStats {
                    room: Some(room_proto),
                    online_count,
                    total_members: member_count,
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
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership before returning chat history
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        // Enforce a hard max page size of 100 to prevent OOM on large rooms
        let limit = req.limit.clamp(1, 100);

        // Cursor takes precedence over the legacy timestamp field.
        // If cursor is non-empty, use (created_at, id) composite keyset pagination
        // (avoids O(N) scans and nanoid ordering issues).
        // Cursor format: "<rfc3339_created_at>|<id>"
        // Otherwise fall back to the legacy timestamp-based pagination for backward compat.
        let (messages, next_cursor_str) = if req.cursor.is_empty() {
            // Legacy path: timestamp-based pagination
            let before = if req.before > 0 {
                chrono::DateTime::from_timestamp(req.before, 0)
            } else {
                None
            };
            let msgs = self
                .room_service
                .get_chat_history(&rid, before, limit)
                .await
                .map_err(ApiError::from)?;
            // Derive a cursor from the oldest message so callers can switch to cursor pagination
            let cursor = msgs
                .last()
                .map(|m| format!("{}|{}", m.created_at.to_rfc3339(), m.id));
            (msgs, cursor)
        } else {
            let cursor = if let Some((ts_str, id)) = req.cursor.split_once('|') {
                let ts = chrono::DateTime::parse_from_rfc3339(ts_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|_| ApiError::InvalidInput("Invalid cursor format".to_string()))?;
                Some((ts, id))
            } else {
                None
            };
            let (msgs, next) = self
                .room_service
                .get_chat_history_cursor(&rid, cursor.as_ref().map(|(ts, id)| (*ts, *id)), limit)
                .await
                .map_err(ApiError::from)?;
            let next_str = next.map(|(ts, id)| format!("{}|{}", ts.to_rfc3339(), id));
            (msgs, next_str)
        };

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
