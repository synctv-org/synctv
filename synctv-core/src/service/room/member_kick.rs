use chrono::Duration;
use sqlx::{Postgres, Transaction};

use crate::{
    models::{RoomId, UserId},
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    repository::room_member::KickCooldownInsert,
    service::PermissionWriteFence,
    Error, Result,
};

use super::{
    cleanup_member_resources_in_tx, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

#[derive(Default)]
pub struct KickMemberOutboxOptions {
    pub permission_changed: Option<RealtimeOutboxPermissionChangedEventFactory>,
    pub cleanup: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
    pub lifecycle: Option<NewRealtimeOutboxEvent>,
}

pub const MAX_KICK_COOLDOWN_SECONDS: i64 = 30 * 24 * 60 * 60;

struct CompleteKickedMemberRemovalRequest<'a> {
    tx: Transaction<'a, Postgres>,
    fence: &'a PermissionWriteFence,
    room_id: RoomId,
    actor_id: UserId,
    target_user_id: UserId,
    removed_version: i64,
    cooldown_seconds: i64,
    kicked_by: Option<&'a UserId>,
    outbox: KickMemberOutboxOptions,
    context: &'static str,
    event_label: &'static str,
}

fn validate_kick_cooldown_seconds(cooldown_seconds: i64) -> Result<()> {
    if cooldown_seconds <= 0 {
        return Err(Error::InvalidInput(
            "kick_cooldown_seconds must be greater than 0".to_string(),
        ));
    }
    if cooldown_seconds > MAX_KICK_COOLDOWN_SECONDS {
        return Err(Error::InvalidInput(format!(
            "kick_cooldown_seconds must be at most {MAX_KICK_COOLDOWN_SECONDS}"
        )));
    }
    Ok(())
}

impl RoomService {
    async fn complete_kicked_member_removal(
        &self,
        request: CompleteKickedMemberRemovalRequest<'_>,
    ) -> Result<()> {
        let CompleteKickedMemberRemovalRequest {
            mut tx,
            fence,
            room_id,
            actor_id,
            target_user_id,
            removed_version,
            cooldown_seconds,
            kicked_by,
            outbox,
            context,
            event_label,
        } = request;

        let now = crate::SystemClock.now();
        if let Err(error) = self
            .member_repo
            .add_kick_cooldown_with_executor(
                KickCooldownInsert {
                    room_id: &room_id,
                    user_id: &target_user_id,
                    kicked_by,
                    starts_at: now,
                    ends_at: now + Duration::seconds(cooldown_seconds),
                    reason: Some("kicked"),
                },
                &mut *tx,
            )
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &target_user_id).await
        {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(fence).await;
                return Err(error);
            }
        };
        let snapshot = match self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                None,
                Self::role_member_event_scope(),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.abort_permission_write(fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_permission_changed_outbox_tx(
                &mut tx,
                &snapshot,
                outbox.permission_changed.as_ref(),
            )
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox.lifecycle.as_ref())
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_member_resource_cleanup_outbox_tx(&mut tx, &cleanup, outbox.cleanup.as_ref())
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            fence,
            &room_id,
            &target_user_id,
            removed_version,
            context,
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &target_user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &target_user_id, &cleanup)
            .await;
        let subscriber_count = self
            .notification_service
            .notify_member_kicked(&room_id, &target_user_id);
        super::outbox::log_if_no_local_subscribers(subscriber_count, &room_id, event_label);
        Ok(())
    }

    /// Kick member from room.
    pub async fn kick_member(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
    ) -> Result<()> {
        self.kick_member_with_outbox(
            room_id,
            kicker_id,
            target_user_id,
            cooldown_seconds,
            KickMemberOutboxOptions::default(),
        )
        .await
    }

    pub async fn kick_member_with_outbox(
        &self,
        room_id: RoomId,
        kicker_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if kicker_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &kicker_id,
            crate::models::RoomPermission::REMOVE_MEMBERS,
        )
        .await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .kick_with_role_check_with_executor(&room_id, &kicker_id, &target_user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::Authorization(
                "User is not a member or cannot kick a member with equal or higher role"
                    .to_string(),
            ));
        };
        self.complete_kicked_member_removal(CompleteKickedMemberRemovalRequest {
            tx,
            fence: &fence,
            room_id,
            actor_id: kicker_id,
            target_user_id,
            removed_version,
            cooldown_seconds,
            kicked_by: Some(&kicker_id),
            outbox,
            context: "kick_member_with_outbox",
            event_label: "Member kick",
        })
        .await
    }

    pub async fn admin_kick_member_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        cooldown_seconds: i64,
        persisted_kicked_by: Option<UserId>,
        outbox: KickMemberOutboxOptions,
    ) -> Result<()> {
        validate_kick_cooldown_seconds(cooldown_seconds)?;
        if actor_id == target_user_id {
            return Err(Error::InvalidInput("Cannot kick yourself".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &target_user_id, &mut tx)
            .await?
        else {
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &target_user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &target_user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            ));
        };
        self.complete_kicked_member_removal(CompleteKickedMemberRemovalRequest {
            tx,
            fence: &fence,
            room_id,
            actor_id,
            target_user_id,
            removed_version,
            cooldown_seconds,
            kicked_by: persisted_kicked_by.as_ref(),
            outbox,
            context: "admin_kick_member_with_outbox",
            event_label: "Admin member kick",
        })
        .await
    }
}
