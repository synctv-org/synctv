use crate::{
    models::{RoomId, RoomRole, UserId},
    Error, Result,
};

use super::{
    member_role_policy::{
        validate_admin_override_bits, validate_creator_member_role_update,
        validate_member_permission_override_channel, validate_override_bits_for_role,
        validate_role_can_be_assigned,
    },
    member_update_execution::MemberUpdateExecutionRequest,
    RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct MemberPermissionPatch {
    pub apply_permission_update: bool,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
}

pub struct UpdateMemberWithOutboxRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub role: Option<RoomRole>,
    pub permissions: MemberPermissionPatch,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

impl RoomService {
    pub async fn update_member_with_outbox(
        &self,
        request: UpdateMemberWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            role,
            permissions,
            outbox_event_factory,
        } = request;
        let MemberPermissionPatch {
            apply_permission_update,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = permissions;

        validate_admin_override_bits(admin_added_permissions, admin_removed_permissions)?;

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        if apply_permission_update {
            validate_override_bits_for_role(
                effective_role,
                added_permissions,
                removed_permissions,
            )?;
        }

        if let Some(new_role) = role {
            validate_role_can_be_assigned(new_role)?;
            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
            validate_creator_member_role_update(&room, &actor_id, &target_user_id)?;
        }

        if apply_permission_update {
            validate_member_permission_override_channel(
                effective_is_admin,
                added_permissions,
                removed_permissions,
                admin_added_permissions,
                admin_removed_permissions,
            )?;
        }

        self.execute_member_update_with_outbox(MemberUpdateExecutionRequest {
            room_id,
            actor_id,
            target_user_id,
            current,
            role,
            permission_update_required: apply_permission_update,
            actor_permission_check: apply_permission_update
                .then_some(crate::models::RoomPermission::MANAGE_MEMBER_PERMISSIONS),
            effective_is_admin,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            outbox_event_factory,
            operation: "update_member_with_outbox",
        })
        .await
    }
}
