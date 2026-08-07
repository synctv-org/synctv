use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, Room, RoomId, RoomMember, RoomRole, UserId,
    },
    service::room::{RealtimeOutboxPermissionChangedEventFactory, RoomService},
    service::PermissionWriteFence,
    Error, Result,
};
use sqlx::{Postgres, Transaction};

use super::permission_writes::MemberRoleWriteParams;

struct CompleteOwnershipTransferRequest<'a> {
    tx: Transaction<'a, Postgres>,
    current_owner_fence: &'a PermissionWriteFence,
    new_owner_fence: &'a PermissionWriteFence,
    room_id: RoomId,
    current_owner_id: UserId,
    new_owner_id: UserId,
    current_owner_username: String,
    current_owner_previous_role: RoomRole,
    new_owner_previous_role: RoomRole,
    updated_room: Room,
    updated_current_owner: RoomMember,
    updated_new_owner: RoomMember,
    outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

impl RoomService {
    async fn abort_ownership_transfer_fences(
        &self,
        current_owner_fence: &PermissionWriteFence,
        new_owner_fence: &PermissionWriteFence,
    ) {
        self.abort_permission_write(current_owner_fence).await;
        self.abort_permission_write(new_owner_fence).await;
    }

    async fn complete_ownership_transfer(
        &self,
        request: CompleteOwnershipTransferRequest<'_>,
    ) -> Result<Room> {
        let CompleteOwnershipTransferRequest {
            mut tx,
            current_owner_fence,
            new_owner_fence,
            room_id,
            current_owner_id,
            new_owner_id,
            current_owner_username,
            current_owner_previous_role,
            new_owner_previous_role,
            updated_room,
            updated_current_owner,
            updated_new_owner,
            outbox_event_factory,
        } = request;

        if let Err(error) = self
            .prepare_and_insert_member_update_outbox(
                &mut tx,
                room_id,
                current_owner_id,
                current_owner_id,
                Some(&updated_current_owner),
                Self::role_member_event_scope(),
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_ownership_transfer_fences(current_owner_fence, new_owner_fence)
                .await;
            return Err(error);
        }
        if let Err(error) = self
            .prepare_and_insert_member_update_outbox(
                &mut tx,
                room_id,
                new_owner_id,
                current_owner_id,
                Some(&updated_new_owner),
                Self::role_member_event_scope(),
                outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_ownership_transfer_fences(current_owner_fence, new_owner_fence)
                .await;
            return Err(error);
        }

        if let Err(error) = tx.commit().await {
            self.abort_ownership_transfer_fences(current_owner_fence, new_owner_fence)
                .await;
            return Err(error.into());
        }

        self.finalize_committed_permission_write_best_effort(
            current_owner_fence,
            &room_id,
            &current_owner_id,
            updated_current_owner.version,
            "transfer_room_ownership_with_outbox:current_owner",
        )
        .await;
        self.finalize_committed_permission_write_best_effort(
            new_owner_fence,
            &room_id,
            &new_owner_id,
            updated_new_owner.version,
            "transfer_room_ownership_with_outbox:new_owner",
        )
        .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &current_owner_id)
            .await;
        self.permission_service
            .invalidate_committed_member_write_cache(&room_id, &new_owner_id)
            .await;

        self.invalidate_room_caches(&room_id).await;
        self.notify_room_settings_invalidation(&room_id).await;

        self.audit_log(
            &current_owner_id,
            &current_owner_username,
            AuditAction::RoomOwnershipTransferred,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                operation: Some("transfer_ownership".to_string()),
                previous_owner_id: Some(current_owner_id.to_string()),
                new_owner_id: Some(new_owner_id.to_string()),
                previous_owner_role: Some(format!("{current_owner_previous_role:?}")),
                new_owner_previous_role: Some(format!("{new_owner_previous_role:?}")),
                ..Default::default()
            },
        )
        .await;

        Ok(updated_room)
    }

    /// Transfer room ownership to another active member.
    pub async fn transfer_room_ownership(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
    ) -> Result<Room> {
        self.transfer_room_ownership_with_outbox(room_id, current_owner_id, new_owner_id, None)
            .await
    }

    pub async fn transfer_room_ownership_with_outbox(
        &self,
        room_id: RoomId,
        current_owner_id: UserId,
        new_owner_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<Room> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.created_by != current_owner_id {
            return Err(Error::Authorization(
                "Only the current room owner can transfer ownership".to_string(),
            ));
        }

        if current_owner_id == new_owner_id {
            return Err(Error::InvalidInput(
                "Room ownership is already assigned to this user".to_string(),
            ));
        }

        let new_owner = self.user_service.get_user(&new_owner_id).await?;
        if !new_owner.status.is_active() {
            return Err(Error::Authorization(
                "New room owner must be an active user".to_string(),
            ));
        }

        let new_owner_member = self
            .member_repo
            .get(&room_id, &new_owner_id)
            .await?
            .ok_or_else(|| {
                Error::InvalidInput(
                    "New room owner must already be an active member of this room".to_string(),
                )
            })?;

        if !new_owner_member.status.is_active() {
            return Err(Error::InvalidInput(
                "New room owner must already be an active member of this room".to_string(),
            ));
        }

        let current_owner_member = self
            .member_repo
            .get(&room_id, &current_owner_id)
            .await?
            .ok_or_else(|| {
                Error::Internal(
                    "Current room owner is missing the required creator membership".to_string(),
                )
            })?;

        let mut tx = self.pool.begin().await?;
        let current_owner_username =
            Self::membership_snapshot_username_tx(&mut tx, &current_owner_id).await?;
        self.enforce_room_ownership_limit_tx(&mut tx, &new_owner_id, Some(&room_id))
            .await?;
        self.ensure_room_name_available_for_creator_tx(&mut tx, &new_owner_id, &room.name)
            .await?;

        let current_owner_fence = self
            .begin_permission_write(&room_id, &current_owner_id, current_owner_member.version)
            .await?;
        let new_owner_fence = match self
            .begin_permission_write(&room_id, &new_owner_id, new_owner_member.version)
            .await
        {
            Ok(fence) => fence,
            Err(error) => {
                self.abort_permission_write(&current_owner_fence).await;
                return Err(error);
            }
        };

        let updated_room = self
            .room_repo
            .transfer_ownership_with_executor(&room_id, &new_owner_id, &mut *tx)
            .await;
        let updated_room = match updated_room {
            Ok(room) => room,
            Err(error) => {
                self.abort_ownership_transfer_fences(&current_owner_fence, &new_owner_fence)
                    .await;
                return Err(error);
            }
        };

        let updated_current_owner = match self
            .apply_member_role_write_with_fence(
                &mut tx,
                MemberRoleWriteParams {
                    room_id: &room_id,
                    user_id: &current_owner_id,
                    fence: &current_owner_fence,
                    role: RoomRole::Admin,
                    current_version: current_owner_member.version,
                },
            )
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                self.abort_ownership_transfer_fences(&current_owner_fence, &new_owner_fence)
                    .await;
                return Err(error);
            }
        };
        let updated_new_owner = match self
            .apply_member_role_write_with_fence(
                &mut tx,
                MemberRoleWriteParams {
                    room_id: &room_id,
                    user_id: &new_owner_id,
                    fence: &new_owner_fence,
                    role: RoomRole::Creator,
                    current_version: new_owner_member.version,
                },
            )
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                self.abort_ownership_transfer_fences(&current_owner_fence, &new_owner_fence)
                    .await;
                return Err(error);
            }
        };
        self.complete_ownership_transfer(CompleteOwnershipTransferRequest {
            tx,
            current_owner_fence: &current_owner_fence,
            new_owner_fence: &new_owner_fence,
            room_id,
            current_owner_id,
            new_owner_id,
            current_owner_username,
            current_owner_previous_role: current_owner_member.role,
            new_owner_previous_role: new_owner_member.role,
            updated_room,
            updated_current_owner,
            updated_new_owner,
            outbox_event_factory,
        })
        .await
    }
}
