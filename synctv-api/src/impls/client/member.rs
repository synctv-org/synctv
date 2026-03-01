//! Member operations: `get_room_members`, `update_member_permissions`, kick, ban, unban

use crate::impls::ApiError;
use synctv_core::models::{RoomId, UserId};

use super::convert::{proto_role_to_room_role, room_member_to_proto};
use super::ClientApiImpl;

impl ClientApiImpl {
    pub async fn get_room_members(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service
            .check_membership(&rid, &uid)
            .await
            .map_err(|e| ApiError::Authorization(format!("Forbidden: {e}")))?;

        let members = self
            .room_service
            .get_room_members(&rid)
            .await
            .map_err(ApiError::from)?;

        // Fetch room settings for proper three-layer permission calculation
        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .unwrap_or_default();
        let permission_service = self.room_service.permission_service();

        let proto_members: Vec<_> = members
            .into_iter()
            .map(|m| {
                let role_default =
                    permission_service.calculate_role_default_permissions(&m.role, &room_settings);
                room_member_to_proto(m, role_default)
            })
            .collect();

        let total = proto_members.len() as i32;
        Ok(crate::proto::client::GetRoomMembersResponse {
            members: proto_members,
            total,
        })
    }

    pub async fn update_member_permissions(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UpdateMemberPermissionsRequest,
    ) -> Result<crate::proto::client::UpdateMemberPermissionsResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        // Check that the caller has GRANT_PERMISSION before any mutation.
        // Use check_permission_no_cache for security-sensitive operations to
        // ensure we always use fresh permission state from the database.
        self.room_service
            .permission_service()
            .check_permission_no_cache(
                &rid,
                &uid,
                synctv_core::models::PermissionBits::GRANT_PERMISSION,
            )
            .await
            .map_err(ApiError::from)?;

        // Handle role update if provided (non-zero = specified)
        if req.role != synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            let new_role = proto_role_to_room_role(req.role)?;
            // Update the member role
            self.room_service
                .member_service()
                .set_member_role(rid.clone(), uid.clone(), target_uid.clone(), new_role)
                .await
                .map_err(ApiError::from)?;
        }

        // Determine which permission set to use based on the caller's actual role,
        // not based on whether admin fields are populated in the request.
        let caller_member = self
            .room_service
            .member_service()
            .get_member(&rid, &uid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::Authorization("Caller is not a member of this room".to_string())
            })?;

        let caller_is_admin = matches!(
            caller_member.role,
            synctv_core::models::RoomRole::Creator | synctv_core::models::RoomRole::Admin
        );

        // Only callers with admin/creator role can set admin-level permissions
        if !caller_is_admin
            && (req.admin_added_permissions > 0 || req.admin_removed_permissions > 0)
        {
            return Err(ApiError::Authorization(
                "Only admins or creators can modify admin-level permissions".to_string(),
            ));
        }

        let added = if caller_is_admin {
            req.admin_added_permissions
        } else {
            req.added_permissions
        };

        let removed = if caller_is_admin {
            req.admin_removed_permissions
        } else {
            req.removed_permissions
        };

        self.room_service
            .set_member_permission(rid.clone(), uid.clone(), target_uid.clone(), added, removed)
            .await
            .map_err(ApiError::from)?;

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid)
            .await;

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
            is_online: false,
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
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        self.room_service
            .kick_member(rid.clone(), uid.clone(), target_uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the kicked user's connections in this specific room
        self.connection_manager
            .disconnect_user_from_room(&target_uid, &rid);

        // Broadcast KickUserFromRoom cluster event so other replicas also disconnect this user
        if let Some(ref tx) = self.redis_publish_tx {
            if let Err(e) = tx
                .send(synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: target_uid.clone(),
                        reason: "kicked".to_string(),
                        timestamp: chrono::Utc::now(),
                    },
                })
                .await
            {
                tracing::error!(room_id = %rid.as_str(), user_id = %target_uid.as_str(), "Failed to publish KickUserFromRoom cluster event: {e}");
            }
        }

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid)
            .await;

        Ok(crate::proto::client::KickMemberResponse { success: true })
    }

    pub async fn ban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::BanMemberRequest,
    ) -> Result<crate::proto::client::BanMemberResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());
        let reason = if req.reason.is_empty() {
            None
        } else {
            Some(req.reason)
        };

        self.room_service
            .member_service()
            .ban_member(rid.clone(), uid.clone(), target_uid.clone(), reason)
            .await
            .map_err(ApiError::from)?;

        // Force disconnect the banned user's connections in this specific room
        self.connection_manager
            .disconnect_user_from_room(&target_uid, &rid);

        // Broadcast KickUserFromRoom cluster event so other replicas also disconnect this user
        if let Some(ref tx) = self.redis_publish_tx {
            if let Err(e) = tx
                .send(synctv_cluster::sync::PublishRequest {
                    event: synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
                        event_id: nanoid::nanoid!(16),
                        room_id: rid.clone(),
                        user_id: target_uid.clone(),
                        reason: "banned".to_string(),
                        timestamp: chrono::Utc::now(),
                    },
                })
                .await
            {
                tracing::error!(room_id = %rid.as_str(), user_id = %target_uid.as_str(), "Failed to publish KickUserFromRoom cluster event: {e}");
            }
        }

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid)
            .await;

        Ok(crate::proto::client::BanMemberResponse { success: true })
    }

    pub async fn unban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UnbanMemberRequest,
    ) -> Result<crate::proto::client::UnbanMemberResponse, ApiError> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        self.room_service
            .member_service()
            .unban_member(rid.clone(), uid.clone(), target_uid.clone())
            .await
            .map_err(ApiError::from)?;

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid)
            .await;

        Ok(crate::proto::client::UnbanMemberResponse { success: true })
    }
}
