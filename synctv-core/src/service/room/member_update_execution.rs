use crate::{
    models::{RoomId, RoomMember, RoomPermission, RoomRole, UserId},
    service::PermissionWriteFence,
    Error, Result,
};

use super::{
    permission_writes::{
        MemberRolePermissionWriteParams, MemberRoleWriteParams, PermissionWriteParams,
    },
    RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

pub(super) struct MemberUpdateExecutionRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub current: RoomMember,
    pub role: Option<RoomRole>,
    pub permission_update_required: bool,
    pub actor_permission_check: Option<RoomPermission>,
    pub effective_is_admin: bool,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    pub operation: &'static str,
}

impl RoomService {
    pub(super) async fn execute_member_update_with_outbox(
        &self,
        request: MemberUpdateExecutionRequest,
    ) -> Result<RoomMember> {
        let MemberUpdateExecutionRequest {
            room_id,
            actor_id,
            target_user_id,
            current,
            role,
            permission_update_required,
            actor_permission_check,
            effective_is_admin,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            outbox_event_factory,
            operation,
        } = request;

        let mut tx = self.pool.begin().await?;
        if let Some(permission) = actor_permission_check {
            self.ensure_actor_has_room_permission_now_tx(&mut tx, &room_id, &actor_id, permission)
                .await?;
        }

        let mut updated = current;
        let mut fence: Option<PermissionWriteFence> = None;
        let combine_role_and_permissions = role.is_some() && permission_update_required;

        if let Some(new_role) = role.filter(|_| combine_role_and_permissions) {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = self
                .apply_member_role_permission_write_or_abort_reserved_fence(
                    &mut tx,
                    MemberRolePermissionWriteParams {
                        room_id: &room_id,
                        user_id: &target_user_id,
                        fence: &write_fence,
                        role: new_role,
                        effective_is_admin,
                        added_permissions,
                        removed_permissions,
                        admin_added_permissions,
                        admin_removed_permissions,
                        current_version: updated.version,
                    },
                )
                .await?;
            fence = Some(write_fence);
        } else if let Some(new_role) = role {
            let write_fence = self
                .begin_permission_write(&room_id, &target_user_id, updated.version)
                .await?;
            updated = self
                .apply_member_role_write_or_abort_reserved_fence(
                    &mut tx,
                    MemberRoleWriteParams {
                        room_id: &room_id,
                        user_id: &target_user_id,
                        fence: &write_fence,
                        role: new_role,
                        current_version: updated.version,
                    },
                )
                .await?;
            fence = Some(write_fence);
        }

        if permission_update_required && !combine_role_and_permissions {
            if fence.is_none() {
                fence = Some(
                    self.begin_permission_write(&room_id, &target_user_id, updated.version)
                        .await?,
                );
            }
            let Some(write_fence) = fence.as_ref() else {
                return Err(Error::Internal(
                    "Permission update missing write fence".to_string(),
                ));
            };
            updated = match self
                .apply_permission_write_with_fence(
                    &mut tx,
                    PermissionWriteParams {
                        room_id: &room_id,
                        user_id: &target_user_id,
                        fence: write_fence,
                        effective_is_admin,
                        added_permissions,
                        removed_permissions,
                        admin_added_permissions,
                        admin_removed_permissions,
                        current_version: updated.version,
                    },
                )
                .await
            {
                Ok(updated) => updated,
                Err(error) => {
                    if let Some(fence) = &fence {
                        self.abort_permission_write(fence).await;
                    }
                    return Err(error);
                }
            };
        }

        let snapshot = match self
            .prepare_and_insert_member_update_outbox(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
                role.is_some(),
                outbox_event_factory.as_ref(),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(fence) = &fence {
                    self.abort_permission_write(fence).await;
                }
                return Err(error);
            }
        };

        self.commit_member_update_with_outbox(
            tx,
            fence.as_ref(),
            &snapshot,
            updated.version,
            operation,
        )
        .await?;

        Ok(updated)
    }
}
