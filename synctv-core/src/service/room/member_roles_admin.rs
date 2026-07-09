use crate::{models::RoomRole, service::AdminMemberUpdate, Error, Result};

use super::{
    member_role_policy::{
        validate_admin_member_role_update, validate_admin_override_bits,
        validate_member_permission_override_channel, validate_override_bits_for_role,
        validate_role_can_be_assigned,
    },
    member_update_execution::MemberUpdateExecutionRequest,
    RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

impl RoomService {
    pub async fn admin_update_member_with_outbox(
        &self,
        update: AdminMemberUpdate,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        let AdminMemberUpdate {
            room_id,
            actor_id,
            actor_username: _,
            target_user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = update;
        validate_admin_override_bits(admin_added_permissions, admin_removed_permissions)?;

        let current = self
            .member_repo
            .get(&room_id, &target_user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User is not a member of this room".to_string()))?;
        let effective_role = role.unwrap_or(current.role);
        let effective_is_admin = matches!(effective_role, RoomRole::Admin);
        validate_override_bits_for_role(effective_role, added_permissions, removed_permissions)?;

        if let Some(new_role) = role {
            validate_role_can_be_assigned(new_role)?;
            let room = self
                .room_repo
                .get_by_id(&room_id)
                .await?
                .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
            validate_admin_member_role_update(&room, &target_user_id)?;
        }

        validate_member_permission_override_channel(
            effective_is_admin,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        )?;

        let has_permission_changes = added_permissions > 0
            || removed_permissions > 0
            || admin_added_permissions > 0
            || admin_removed_permissions > 0;
        self.execute_member_update_with_outbox(MemberUpdateExecutionRequest {
            room_id,
            actor_id,
            target_user_id,
            current,
            role,
            permission_update_required: has_permission_changes || role.is_none(),
            actor_permission_check: None,
            effective_is_admin,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            outbox_event_factory,
            operation: "admin_update_member_with_outbox",
        })
        .await
    }
}
