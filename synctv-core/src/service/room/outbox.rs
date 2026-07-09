use std::sync::Arc;

use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, Room, RoomId, RoomMember, RoomPermissionSet,
        RoomRole, RoomSettings, UserId, LOCAL_MANAGEMENT_ACTOR_USER_ID,
    },
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    service::{
        audit::AuditEventParams,
        room::{MemberResourceCleanupResult, RoomService},
    },
    Error, Result,
};

pub type RealtimeOutboxSettingsEventFactory =
    Arc<dyn Fn(&RoomSettings, i64) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxRoomEventFactory =
    Arc<dyn Fn(&Room) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxPermissionChangedEventFactory =
    Arc<dyn Fn(&PermissionChangedOutboxSnapshot) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxUserLeftEventFactory =
    Arc<dyn Fn(&UserLeftOutboxSnapshot) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;
pub type RealtimeOutboxMemberResourceCleanupEventFactory =
    Arc<dyn Fn(&MemberResourceCleanupResult) -> Result<Vec<NewRealtimeOutboxEvent>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct PermissionChangedOutboxSnapshot {
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub target_username: String,
    pub target_remark_name: String,
    pub target_display_tag: String,
    pub changed_by: UserId,
    pub changed_by_username: String,
    pub role_changed: bool,
    pub new_permissions: RoomPermissionSet,
    pub role: i32,
    pub added_permissions: RoomPermissionSet,
    pub removed_permissions: RoomPermissionSet,
    pub admin_added_permissions: RoomPermissionSet,
    pub admin_removed_permissions: RoomPermissionSet,
}

#[derive(Debug, Clone)]
pub struct UserLeftOutboxSnapshot {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub username: String,
    pub remark_name: String,
    pub display_tag: String,
    pub role: i32,
}

pub(super) fn log_if_no_local_subscribers(
    subscriber_count: usize,
    room_id: &RoomId,
    event_label: &str,
) {
    if subscriber_count == 0 {
        tracing::debug!(
            room_id = %room_id,
            "{} event had no local subscribers",
            event_label
        );
    }
}

pub(super) struct MemberJoinedSystemChatInsert {
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub target_username: String,
    pub actor_user_id: UserId,
    pub actor_username: String,
    pub role: RoomRole,
}

pub(super) struct MemberJoinedEffectsRequest<'a> {
    pub room_id: RoomId,
    pub target_user_id: UserId,
    pub actor_id: UserId,
    pub member: &'a RoomMember,
    pub outbox_event_factory: Option<&'a RealtimeOutboxPermissionChangedEventFactory>,
}

impl RoomService {
    pub(super) const fn role_member_event_scope() -> bool {
        true
    }

    pub(super) const fn permission_member_event_scope() -> bool {
        false
    }

    pub(super) async fn audit_log(
        &self,
        actor_id: &UserId,
        actor_username: &str,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: AuditDetails,
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
        details: AuditDetails,
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
        if *user_id == LOCAL_MANAGEMENT_ACTOR_USER_ID {
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
        if *user_id == LOCAL_MANAGEMENT_ACTOR_USER_ID {
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
        role_changed: bool,
    ) -> Result<PermissionChangedOutboxSnapshot> {
        let target_username = Self::membership_snapshot_username_tx(tx, &target_user_id).await?;
        let changed_by_username = Self::membership_snapshot_username_tx(tx, &changed_by).await?;
        let target_remark_name = member
            .map(|member| member.remark_name.clone())
            .unwrap_or_default();
        let target_display_tag = member
            .map(|member| member.display_tag.clone())
            .unwrap_or_default();
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
            target_remark_name,
            target_display_tag,
            changed_by,
            changed_by_username,
            role_changed,
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
        let member = self.get_member(&room_id, &user_id).await?;
        let role = member.as_ref().map_or_else(
            || i32::from(RoomRole::Member),
            |member| i32::from(member.role),
        );
        let (remark_name, display_tag) = member
            .map(|member| (member.remark_name, member.display_tag))
            .unwrap_or_default();
        Ok(UserLeftOutboxSnapshot {
            room_id,
            user_id,
            username: self.membership_snapshot_username(&user_id).await?,
            remark_name,
            display_tag,
            role,
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

    #[allow(clippy::too_many_arguments)] // Shared outbox preparation keeps mutation modules lean.
    pub(super) async fn prepare_and_insert_member_update_outbox(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        room_id: RoomId,
        target_user_id: UserId,
        actor_id: UserId,
        updated: Option<&RoomMember>,
        role_changed: bool,
        outbox_event_factory: Option<&RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<PermissionChangedOutboxSnapshot> {
        let snapshot = self
            .permission_changed_snapshot_tx(
                tx,
                room_id,
                target_user_id,
                actor_id,
                updated,
                role_changed,
            )
            .await?;
        self.insert_permission_changed_outbox_tx(tx, &snapshot, outbox_event_factory)
            .await?;
        Ok(snapshot)
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

    pub(super) async fn commit_member_update_with_outbox(
        &self,
        tx: Transaction<'_, Postgres>,
        fence: Option<&crate::service::PermissionWriteFence>,
        snapshot: &PermissionChangedOutboxSnapshot,
        updated_version: i64,
        context: &'static str,
    ) -> Result<()> {
        if let Err(error) = tx.commit().await {
            if let Some(fence) = fence {
                self.abort_permission_write(fence).await;
            }
            return Err(error.into());
        }
        if let Some(fence) = fence {
            self.finalize_committed_permission_write_best_effort(
                fence,
                &snapshot.room_id,
                &snapshot.target_user_id,
                updated_version,
                context,
            )
            .await;
        }

        self.permission_service
            .invalidate_committed_member_write_cache(&snapshot.room_id, &snapshot.target_user_id)
            .await;
        if snapshot.role_changed {
            self.notify_room_settings_invalidation(&snapshot.room_id)
                .await;
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

    pub(super) async fn insert_member_joined_system_chat_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: MemberJoinedSystemChatInsert,
    ) -> Result<()> {
        let MemberJoinedSystemChatInsert {
            room_id,
            target_user_id,
            target_username,
            actor_user_id,
            actor_username,
            role,
        } = request;
        let content = format!("{target_username} joined the room");
        let mut message = crate::models::ChatMessage::new(room_id, target_user_id, content);
        message.message_type = crate::models::ChatMessageType::SystemMemberJoined;
        message.metadata = Some(crate::models::ChatMetadata::MemberJoined(
            crate::models::ChatMemberJoinedMetadata {
                user_id: target_user_id,
                username: target_username,
                actor_user_id: Some(actor_user_id),
                actor_username: Some(actor_username),
                role,
            },
        ));

        let occurred_at = self.clock.now();
        let event_id = synctv_common::snanoid!(16);
        let logged = self
            .chat_repo
            .insert_message_event_in_tx(
                tx,
                crate::repository::chat::InsertChatMessageEvent {
                    message: &message,
                    attachments: &[],
                    mentions: &[],
                    actor_user_id,
                    event_id: &event_id,
                    occurred_at,
                },
            )
            .await?;

        let event = crate::models::RealtimeEvent::ChatMessageEvent {
            event_id: logged.event.event_id.clone(),
            room_id,
            actor_user_id,
            event: logged.event,
            timestamp: occurred_at,
        };
        self.insert_realtime_outbox_tx(
            tx,
            Some(&NewRealtimeOutboxEvent {
                id: event.event_id().to_string(),
                enqueue_outbox: true,
                aggregate_type: "room".to_string(),
                aggregate_id: room_id.to_string(),
                event_type: event.event_type().to_string(),
                event_version: 1,
                aggregate_version: None,
                payload: event,
            }),
        )
        .await
    }

    pub(super) async fn apply_member_joined_effects_and_commit(
        &self,
        tx: Transaction<'_, Postgres>,
        request: MemberJoinedEffectsRequest<'_>,
    ) -> Result<()> {
        let MemberJoinedEffectsRequest {
            room_id,
            target_user_id,
            actor_id,
            member,
            outbox_event_factory,
        } = request;
        let mut tx = tx;
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(member),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory)
            .await?;
        self.insert_member_joined_system_chat_tx(
            &mut tx,
            MemberJoinedSystemChatInsert {
                room_id,
                target_user_id,
                target_username: snapshot.target_username.clone(),
                actor_user_id: actor_id,
                actor_username: snapshot.changed_by_username.clone(),
                role: member.role,
            },
        )
        .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, member.version)
            .await;

        Ok(())
    }
}
