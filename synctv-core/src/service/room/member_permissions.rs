use crate::{
    models::{RoomId, RoomRole, UserId},
    service::optimistic_retry,
    Error, Result,
};

use super::{
    member_role_policy::validate_override_bits_for_role, permission_writes::PermissionWriteParams,
    RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

impl RoomService {
    /// Grant permission to user.
    pub async fn grant_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        permission: u64,
    ) -> Result<crate::models::RoomMember> {
        self.member_service
            .grant_permission(room_id, granter_id, target_user_id, permission)
            .await
    }

    /// Update member permissions using the allow/deny override pattern.
    pub async fn set_member_permission(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
    ) -> Result<crate::models::RoomMember> {
        self.set_member_permission_with_outbox(
            room_id,
            granter_id,
            target_user_id,
            added_permissions,
            removed_permissions,
            None,
        )
        .await
    }

    pub async fn set_member_permission_with_outbox(
        &self,
        room_id: RoomId,
        granter_id: UserId,
        target_user_id: UserId,
        added_permissions: u64,
        removed_permissions: u64,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<crate::models::RoomMember> {
        optimistic_retry::retry_with_optimistic_lock(
            3,
            5,
            "Permission update failed after maximum retry attempts",
            || async {
                let mut tx = self.pool.begin().await?;
                self.ensure_actor_has_room_permission_now_tx(
                    &mut tx,
                    &room_id,
                    &granter_id,
                    crate::models::RoomPermission::MANAGE_MEMBER_PERMISSIONS,
                )
                .await?;
                let member = self
                    .member_repo
                    .get(&room_id, &target_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::NotFound("User is not a member of this room".to_string())
                    })?;
                validate_override_bits_for_role(
                    member.role,
                    added_permissions,
                    removed_permissions,
                )?;
                let fence = self
                    .begin_permission_write(&room_id, &target_user_id, member.version)
                    .await?;
                let updated = self
                    .apply_permission_write_or_abort_reserved_fence(
                        &mut tx,
                        PermissionWriteParams {
                            room_id: &room_id,
                            user_id: &target_user_id,
                            fence: &fence,
                            effective_is_admin: matches!(member.role, RoomRole::Admin),
                            added_permissions,
                            removed_permissions,
                            admin_added_permissions: added_permissions,
                            admin_removed_permissions: removed_permissions,
                            current_version: member.version,
                        },
                    )
                    .await?;
                let snapshot = match self
                    .prepare_and_insert_member_update_outbox(
                        &mut tx,
                        room_id,
                        target_user_id,
                        granter_id,
                        Some(&updated),
                        Self::permission_member_event_scope(),
                        outbox_event_factory.as_ref(),
                    )
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.abort_permission_write(&fence).await;
                        return Err(error);
                    }
                };
                self.commit_member_update_with_outbox(
                    tx,
                    Some(&fence),
                    &snapshot,
                    updated.version,
                    "grant_member_permissions_with_outbox",
                )
                .await?;
                Ok(updated)
            },
        )
        .await
    }
}
