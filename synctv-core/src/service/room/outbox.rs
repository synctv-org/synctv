use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        AuditAction, AuditTargetType, RoomId, RoomMember, RoomPermissionSet, RoomRole, UserId,
    },
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    service::{
        audit::AuditEventParams,
        room::{
            MemberResourceCleanupResult, PermissionChangedOutboxSnapshot,
            RealtimeOutboxMemberResourceCleanupEventFactory,
            RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxUserLeftEventFactory,
            RoomService, UserLeftOutboxSnapshot,
        },
    },
    Error, Result,
};

impl RoomService {
    pub(super) async fn audit_log(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) {
        if let Some(ref audit) = self.audit_service {
            if let Err(error) = audit
                .log(AuditEventParams {
                    actor_id: actor_id.to_string(),
                    actor_username: actor_username.to_string(),
                    action,
                    target_type,
                    target_id,
                    details,
                    ip_address: None,
                    user_agent: None,
                })
                .await
            {
                tracing::warn!(error = %error, "Failed to write audit log from RoomService");
            }
        }
    }

    pub(super) async fn write_audit_event(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
    ) -> Result<()> {
        let Some(ref audit) = self.audit_service else {
            return Ok(());
        };
        audit
            .log(AuditEventParams {
                actor_id: actor_id.to_string(),
                actor_username: actor_username.to_string(),
                action,
                target_type,
                target_id,
                details,
                ip_address: None,
                user_agent: None,
            })
            .await
    }

    pub(super) async fn membership_snapshot_username(&self, user_id: &UserId) -> Result<String> {
        if *user_id == UserId::MAX {
            return Ok("local-management".to_string());
        }

        self.user_service
            .get_username(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Membership snapshot user not found".to_string()))
    }

    pub(super) async fn actor_username_required(&self, user_id: &UserId) -> Result<String> {
        self.user_service
            .get_username(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("Actor user not found".to_string()))
    }

    pub(super) async fn membership_snapshot_username_tx(
        tx: &mut Transaction<'_, Postgres>,
        user_id: &UserId,
    ) -> Result<String> {
        if *user_id == UserId::MAX {
            return Ok("local-management".to_string());
        }

        sqlx::query_scalar!(
            "SELECT username FROM users WHERE id = $1 AND deleted_at IS NULL",
            user_id as &UserId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| Error::NotFound("Membership snapshot user not found".to_string()))
    }

    pub(super) async fn permission_changed_snapshot_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: RoomId,
        target_user_id: UserId,
        changed_by: UserId,
        member: Option<&RoomMember>,
    ) -> Result<PermissionChangedOutboxSnapshot> {
        let target_username = Self::membership_snapshot_username_tx(tx, &target_user_id).await?;
        let changed_by_username = Self::membership_snapshot_username_tx(tx, &changed_by).await?;
        let room_settings = self
            .room_settings_repo
            .get_for_update(&room_id, &mut **tx)
            .await?;

        let (
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if let Some(member) = member.filter(|member| member.is_active()) {
            (
                self.permission_service
                    .effective_member_permissions(member, &room_settings),
                i32::from(member.role),
                RoomPermissionSet(member.added_permissions),
                RoomPermissionSet(member.removed_permissions),
                RoomPermissionSet(member.admin_added_permissions),
                RoomPermissionSet(member.admin_removed_permissions),
            )
        } else {
            (
                RoomPermissionSet::empty(),
                i32::from(RoomRole::Member),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
                RoomPermissionSet::empty(),
            )
        };

        Ok(PermissionChangedOutboxSnapshot {
            room_id,
            target_user_id,
            target_username,
            changed_by,
            changed_by_username,
            new_permissions,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        })
    }

    pub(super) async fn user_left_snapshot(
        &self,
        room_id: RoomId,
        user_id: UserId,
    ) -> Result<UserLeftOutboxSnapshot> {
        Ok(UserLeftOutboxSnapshot {
            room_id,
            user_id,
            username: self.membership_snapshot_username(&user_id).await?,
        })
    }

    pub(super) async fn insert_permission_changed_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        snapshot: &PermissionChangedOutboxSnapshot,
        outbox_event_factory: Option<&RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(snapshot))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn insert_realtime_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outbox_event: Option<&NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        if let (Some(outbox), Some(event)) = (&self.realtime_outbox, outbox_event) {
            outbox.insert_with_executor(event, &mut **tx).await?;
        }
        Ok(())
    }

    pub(super) async fn insert_realtime_outbox_events_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outbox_events: &[NewRealtimeOutboxEvent],
    ) -> Result<()> {
        if let Some(outbox) = &self.realtime_outbox {
            for event in outbox_events {
                outbox.insert_with_executor(event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn insert_member_resource_cleanup_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        cleanup: &MemberResourceCleanupResult,
        outbox_event_factory: Option<&RealtimeOutboxMemberResourceCleanupEventFactory>,
    ) -> Result<()> {
        if let Some(events) = outbox_event_factory
            .map(|factory| factory(cleanup))
            .transpose()?
        {
            self.insert_realtime_outbox_events_tx(tx, &events).await?;
        }
        Ok(())
    }

    pub(super) async fn insert_user_left_outbox_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        snapshot: &UserLeftOutboxSnapshot,
        outbox_event_factory: Option<&RealtimeOutboxUserLeftEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(snapshot))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }
}
