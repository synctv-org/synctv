//! Member operations: `get_room_members`, `update_member_permissions`, kick, ban, unban

use crate::impls::ApiError;
use synctv_core::models::UserId;

use super::convert::{members_to_proto, proto_role_to_room_role, room_member_to_proto};
use super::ClientApiImpl;

impl ClientApiImpl {
    /// Get room members with pagination (E8 fix).
    ///
    /// Uses `get_room_members_paginated` to avoid loading ALL members into memory,
    /// matching the pattern used by the admin endpoint.
    pub async fn get_room_members(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(Self::map_room_access_error)?;

        // E8 fix: Use database-level pagination instead of loading all members
        // Safely convert i32 to u32 (negative values default to safe values)
        let page = u32::try_from(req.page).unwrap_or(1);
        let page_size = u32::try_from(req.page_size)
            .ok()
            .filter(|&ps| ps > 0)
            .map_or(50, |ps| ps.min(100));
        let pagination = synctv_core::models::PageParams::new(Some(page), Some(page_size));

        let (members, total) = self
            .room_service
            .get_room_members_paginated(&rid, pagination)
            .await
            .map_err(ApiError::from)?;

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

        Ok(crate::proto::client::GetRoomMembersResponse {
            members: proto_members,
            total: total as i32,
        })
    }

    pub async fn update_member_permissions(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdateMemberPermissionsRequest,
    ) -> Result<crate::proto::client::UpdateMemberPermissionsResponse, ApiError> {
        crate::http::validation::validate_id(&req.user_id, "user_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid user_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let target_uid = UserId::from_string(req.user_id.clone());
        let permission_fanout = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out permission changes to cluster replicas",
        )
        .await?;

        // Fetch current target member state BEFORE any mutations.
        // This prevents partial mutation when role change + permission update
        // are requested together and validation fails after the role commit.
        let has_permission_changes = req.added_permissions > 0
            || req.removed_permissions > 0
            || req.admin_added_permissions > 0
            || req.admin_removed_permissions > 0;

        if has_permission_changes || req.role != synctv_proto::common::RoomMemberRole::Unspecified as i32
        {
            let target_member = self
                .room_service
                .member_service()
                .get_member(&rid, &target_uid)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Target member not found".to_string()))?;

            // Determine the effective role AFTER potential mutation.
            // When the request changes the role, we must use the NEW role
            // (not the current role) to decide which permission columns
            // to validate against and write to.
            let role_is_changing =
                req.role != synctv_proto::common::RoomMemberRole::Unspecified as i32;
            let effective_is_admin = if role_is_changing {
                let new_role = proto_role_to_room_role(req.role)?;
                matches!(new_role, synctv_core::models::RoomRole::Admin)
            } else {
                matches!(target_member.role, synctv_core::models::RoomRole::Admin)
            };

            if has_permission_changes {
                if effective_is_admin
                    && (req.added_permissions > 0 || req.removed_permissions > 0)
                {
                    return Err(ApiError::Authorization(
                        "Admin members must use admin_added_permissions/admin_removed_permissions"
                            .to_string(),
                    ));
                }
                if !effective_is_admin
                    && (req.admin_added_permissions > 0 || req.admin_removed_permissions > 0)
                {
                    return Err(ApiError::Authorization(
                        "Only admin members use admin_added_permissions/admin_removed_permissions"
                            .to_string(),
                    ));
                }
            }

            // All validation passed — apply mutations now.

            // Handle role update if provided (non-zero = specified).
            if role_is_changing {
                let new_role = proto_role_to_room_role(req.role)?;
                self.room_service
                    .member_service()
                    .set_member_role(rid.clone(), uid.clone(), target_uid.clone(), new_role)
                    .await
                    .map_err(ApiError::from)?;
            }

            // Handle permission updates
            if has_permission_changes {
                // The service layer (set_member_permissions) already enforces
                // GRANT_PERMISSION as the single source of truth via
                // check_permission_no_cache.
                let added = if effective_is_admin {
                    req.admin_added_permissions
                } else {
                    req.added_permissions
                };

                let removed = if effective_is_admin {
                    req.admin_removed_permissions
                } else {
                    req.removed_permissions
                };

                self.room_service
                    .set_member_permission(rid.clone(), uid.clone(), target_uid.clone(), added, removed)
                    .await
                    .map_err(ApiError::from)?;
            }
        }

        self.publish_permission_changed_with_reservation(
            &rid,
            &target_uid,
            &uid,
            permission_fanout,
        )
        .await?;

        // Get updated member directly instead of fetching all members
        let member = self
            .room_service
            .get_member(&rid, &target_uid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Member not found".to_string()))?;

        // Fetch username for the target user
        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);

        // Query ConnectionManager for actual online status instead of hardcoding false
        let is_online = self
            .connection_manager
            .get_connection_id(&rid, &target_uid)
            .is_some();

        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online,
            is_active: true,
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };

        // Fetch room settings for proper three-layer permission calculation
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let role_default = self
            .room_service
            .permission_service()
            .calculate_role_default_permissions(&member_with_user.role, &room_settings);

        Ok(crate::proto::client::UpdateMemberPermissionsResponse {
            member: Some(room_member_to_proto(member_with_user, role_default)),
        })
    }

    pub async fn kick_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::KickMemberRequest,
    ) -> Result<crate::proto::client::KickMemberResponse, ApiError> {
        crate::http::validation::validate_id(&req.user_id, "user_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid user_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let target_uid = UserId::from_string(req.user_id.clone());
        let cluster_event = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out KickUserFromRoom to cluster replicas",
        )
        .await?;
        let permission_fanout = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out permission changes to cluster replicas",
        )
        .await?;

        self.room_service
            .kick_member(rid.clone(), uid.clone(), target_uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the kicked user's connections in this specific room
        self.connection_manager
            .disconnect_user_from_room(&target_uid, &rid);

        if let Some(cluster_event) = cluster_event {
            cluster_event.publish(synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
                    event_id: nanoid::nanoid!(16),
                    room_id: rid.clone(),
                    user_id: target_uid.clone(),
                    reason: "kicked".to_string(),
                    timestamp: chrono::Utc::now(),
                },
            });
        }

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed_with_reservation(
            &rid,
            &target_uid,
            &uid,
            permission_fanout,
        )
        .await?;

        Ok(crate::proto::client::KickMemberResponse { success: true })
    }

    /// Maximum length for ban reason text
    const BAN_REASON_MAX: usize = 500;

    pub async fn ban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::BanMemberRequest,
    ) -> Result<crate::proto::client::BanMemberResponse, ApiError> {
        crate::http::validation::validate_id(&req.user_id, "user_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid user_id: {e}")))?;

        if req.reason.chars().count() > Self::BAN_REASON_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Ban reason too long (maximum {} characters)",
                Self::BAN_REASON_MAX
            )));
        }

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let target_uid = UserId::from_string(req.user_id.clone());
        let reason = if req.reason.is_empty() {
            None
        } else {
            Some(req.reason)
        };
        let cluster_event = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out room ban to cluster replicas",
        )
        .await?;
        let permission_fanout = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out permission changes to cluster replicas",
        )
        .await?;

        self.room_service
            .member_service()
            .ban_member(rid.clone(), uid.clone(), target_uid.clone(), reason)
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the banned user's connections in this specific room
        self.connection_manager
            .disconnect_user_from_room(&target_uid, &rid);

        if let Some(cluster_event) = cluster_event {
            cluster_event.publish(synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
                    event_id: nanoid::nanoid!(16),
                    room_id: rid.clone(),
                    user_id: target_uid.clone(),
                    reason: "banned".to_string(),
                    timestamp: chrono::Utc::now(),
                },
            });
        }

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed_with_reservation(
            &rid,
            &target_uid,
            &uid,
            permission_fanout,
        )
        .await?;

        Ok(crate::proto::client::BanMemberResponse { success: true })
    }

    pub async fn unban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UnbanMemberRequest,
    ) -> Result<crate::proto::client::UnbanMemberResponse, ApiError> {
        crate::http::validation::validate_id(&req.user_id, "user_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid user_id: {e}")))?;

        let uid = UserId::from_string(user_id.to_string());
        let rid = self.parse_room_id(room_id)?;
        let target_uid = UserId::from_string(req.user_id.clone());
        let permission_fanout = crate::impls::reserve_cluster_event_publish(
            self.redis_publish_tx.as_ref(),
            self.config.cluster_runtime_enabled(),
            "failed to fan out permission changes to cluster replicas",
        )
        .await?;

        self.room_service
            .member_service()
            .unban_member(rid.clone(), uid.clone(), target_uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed_with_reservation(
            &rid,
            &target_uid,
            &uid,
            permission_fanout,
        )
        .await?;

        Ok(crate::proto::client::UnbanMemberResponse { success: true })
    }
}
