//! Member operations: get_room_members, update_member_permissions, kick, ban, unban

use synctv_core::models::{RoomId, UserId};

use super::ClientApiImpl;
use super::convert::{proto_role_to_room_role, room_member_to_proto};

impl ClientApiImpl {
    pub async fn get_room_members(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());

        // Check membership
        self.room_service.check_membership(&rid, &uid).await
            .map_err(|e| format!("Forbidden: {e}"))?;

        let members = self.room_service.get_room_members(&rid).await
            .map_err(|e| e.to_string())?;

        let proto_members: Vec<_> = members.into_iter()
            .map(room_member_to_proto)
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
    ) -> Result<crate::proto::client::UpdateMemberPermissionsResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        // Handle role update if provided (non-zero = specified)
        if req.role != synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            let new_role = proto_role_to_room_role(req.role)?;
            // Update the member role
            self.room_service.member_service().set_member_role(
                rid.clone(),
                uid.clone(),
                target_uid.clone(),
                new_role,
            ).await.map_err(|e| e.to_string())?;
        }

        // Determine which permission set to use based on the caller's actual role,
        // not based on whether admin fields are populated in the request.
        let caller_member = self.room_service.member_service()
            .get_member(&rid, &uid)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Caller is not a member of this room".to_string())?;

        let caller_is_admin = matches!(
            caller_member.role,
            synctv_core::models::RoomRole::Creator | synctv_core::models::RoomRole::Admin
        );

        // Only callers with admin/creator role can set admin-level permissions
        if !caller_is_admin && (req.admin_added_permissions > 0 || req.admin_removed_permissions > 0) {
            return Err("Only admins or creators can modify admin-level permissions".to_string());
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

        self.room_service.set_member_permission(
            rid.clone(),
            uid.clone(),
            target_uid.clone(),
            added,
            removed,
        ).await
            .map_err(|e| e.to_string())?;

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid);

        // Get updated member
        let members = self.room_service.get_room_members(&rid).await
            .map_err(|e| e.to_string())?;
        let member = members.into_iter()
            .find(|m| m.user_id == target_uid)
            .ok_or_else(|| "Member not found".to_string())?;

        Ok(crate::proto::client::UpdateMemberPermissionsResponse {
            member: Some(room_member_to_proto(member)),
        })
    }

    pub async fn kick_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::KickMemberRequest,
    ) -> Result<crate::proto::client::KickMemberResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        self.room_service.kick_member(rid.clone(), uid.clone(), target_uid.clone()).await
            .map_err(|e| e.to_string())?;

        // Force disconnect the kicked user's connections in this specific room
        self.connection_manager.disconnect_user_from_room(&target_uid, &rid);

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid);

        Ok(crate::proto::client::KickMemberResponse {
            success: true,
        })
    }

    pub async fn ban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::BanMemberRequest,
    ) -> Result<crate::proto::client::BanMemberResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());
        let reason = if req.reason.is_empty() { None } else { Some(req.reason) };

        self.room_service.member_service()
            .ban_member(rid.clone(), uid.clone(), target_uid.clone(), reason)
            .await
            .map_err(|e| e.to_string())?;

        // Force disconnect the banned user's connections in this specific room
        self.connection_manager.disconnect_user_from_room(&target_uid, &rid);

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid);

        Ok(crate::proto::client::BanMemberResponse { success: true })
    }

    pub async fn unban_member(
        &self,
        user_id: &str,
        room_id: &str,
        req: crate::proto::client::UnbanMemberRequest,
    ) -> Result<crate::proto::client::UnbanMemberResponse, String> {
        let uid = UserId::from_string(user_id.to_string());
        let rid = RoomId::from_string(room_id.to_string());
        let target_uid = UserId::from_string(req.user_id.clone());

        self.room_service.member_service()
            .unban_member(rid.clone(), uid.clone(), target_uid.clone())
            .await
            .map_err(|e| e.to_string())?;

        // Notify other replicas to invalidate permission cache
        self.publish_permission_changed(&rid, &target_uid, &uid);

        Ok(crate::proto::client::UnbanMemberResponse { success: true })
    }
}
