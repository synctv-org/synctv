use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, MemberStatus, Room, RoomId, RoomMember,
        RoomRole, UserId,
    },
    Error, Result,
};

use super::{
    outbox::MemberJoinedEffectsRequest, RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

struct AddActiveMemberTxRequest<'a> {
    room_id: &'a RoomId,
    target_user_id: &'a UserId,
    role: RoomRole,
    remark_name: String,
    display_tag: String,
    reviewed_by: Option<&'a UserId>,
    require_pending_review: bool,
}

pub struct AdminAddMemberWithOutboxRequest<'a> {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub actor_username: &'a str,
    pub target_user_id: UserId,
    pub role: RoomRole,
    pub remark_name: String,
    pub display_tag: String,
    pub notify: bool,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

pub struct AddMemberWithOutboxRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub role: RoomRole,
    pub remark_name: String,
    pub display_tag: String,
    pub notify: bool,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

impl RoomService {
    async fn add_active_member_and_resolve_join_review_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        request: AddActiveMemberTxRequest<'_>,
    ) -> Result<RoomMember> {
        let AddActiveMemberTxRequest {
            room_id,
            target_user_id,
            role,
            remark_name,
            display_tag,
            reviewed_by,
            require_pending_review,
        } = request;
        self.ensure_target_user_can_join_now_tx(tx, target_user_id)
            .await?;
        Self::ensure_room_can_admit_member_now_tx(tx, room_id, target_user_id).await?;

        let mut member = RoomMember::new(*room_id, *target_user_id, role);
        member.status = MemberStatus::Active;
        member.remark_name = remark_name;
        member.display_tag = display_tag;
        let options = self.active_member_add_options(room_id).await?;
        let created = self
            .member_repo
            .add_with_options_tx(&member, &options, tx)
            .await?;
        let resolved = Self::resolve_pending_join_request_as_approved_tx(
            tx,
            room_id,
            target_user_id,
            reviewed_by,
        )
        .await?;
        if require_pending_review && resolved == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        Ok(created)
    }

    async fn notify_room_invitation_best_effort(
        &self,
        target_user_id: &UserId,
        room: &Room,
        actor_username: &str,
    ) {
        let Some(ref notif_service) = self.user_notification_service else {
            return;
        };

        if let Err(error) = notif_service
            .create_room_invitation(
                *target_user_id,
                room.id.to_string(),
                room.name.clone(),
                actor_username.to_string(),
            )
            .await
        {
            tracing::warn!(
                room_id = %room.id,
                user_id = %target_user_id,
                error = %error,
                "Failed to create room invitation notification"
            );
        }
    }

    /// Explicitly add a user as an active member.
    ///
    /// This is the manager-side admission path used when `allow_auto_join=false`.
    pub async fn add_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        role: RoomRole,
        notify: bool,
    ) -> Result<RoomMember> {
        self.add_member_with_outbox(AddMemberWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            role,
            remark_name: String::new(),
            display_tag: String::new(),
            notify,
            outbox_event_factory: None,
        })
        .await
    }

    pub async fn add_member_with_outbox(
        &self,
        request: AddMemberWithOutboxRequest,
    ) -> Result<RoomMember> {
        let AddMemberWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            role,
            remark_name,
            display_tag,
            notify,
            outbox_event_factory,
        } = request;
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_room_creator_is_active_for_access(&room, &actor_id)
            .await?;
        self.permission_service
            .check_permission_no_cache(
                &room_id,
                &actor_id,
                crate::models::RoomPermission::ADD_MEMBER,
            )
            .await?;

        self.ensure_target_user_can_join(&target_user_id).await?;

        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::ADD_MEMBER,
        )
        .await?;
        let created = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                AddActiveMemberTxRequest {
                    room_id: &room_id,
                    target_user_id: &target_user_id,
                    role,
                    remark_name,
                    display_tag,
                    reviewed_by: Some(&actor_id),
                    require_pending_review: false,
                },
            )
            .await?;
        self.apply_member_joined_effects_and_commit(
            tx,
            MemberJoinedEffectsRequest {
                room_id,
                target_user_id,
                actor_id,
                member: &created,
                outbox_event_factory: outbox_event_factory.as_ref(),
            },
        )
        .await?;

        let actor_username = self.actor_username_required(&actor_id).await?;

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                new_status: Some("active".to_string()),
                role: Some(role.to_string()),
                source: Some("explicit_add_member".to_string()),
                ..Default::default()
            },
        )
        .await;

        if notify {
            self.notify_room_invitation_best_effort(&target_user_id, &room, &actor_username)
                .await;
        }

        Ok(created)
    }

    /// Administrative override: add a room member without requiring room-local membership.
    pub async fn admin_add_member(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        actor_username: &str,
        target_user_id: UserId,
        role: RoomRole,
        notify: bool,
    ) -> Result<RoomMember> {
        self.admin_add_member_with_outbox(AdminAddMemberWithOutboxRequest {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            remark_name: String::new(),
            display_tag: String::new(),
            notify,
            outbox_event_factory: None,
        })
        .await
    }

    pub async fn admin_add_member_with_outbox(
        &self,
        request: AdminAddMemberWithOutboxRequest<'_>,
    ) -> Result<RoomMember> {
        let AdminAddMemberWithOutboxRequest {
            room_id,
            actor_id,
            actor_username,
            target_user_id,
            role,
            remark_name,
            display_tag,
            notify,
            outbox_event_factory,
        } = request;
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        self.ensure_target_user_can_join(&target_user_id).await?;

        let mut tx = self.pool.begin().await?;
        let created = self
            .add_active_member_and_resolve_join_review_tx(
                &mut tx,
                AddActiveMemberTxRequest {
                    room_id: &room_id,
                    target_user_id: &target_user_id,
                    role,
                    remark_name,
                    display_tag,
                    reviewed_by: Some(&actor_id),
                    require_pending_review: false,
                },
            )
            .await?;
        self.apply_member_joined_effects_and_commit(
            tx,
            MemberJoinedEffectsRequest {
                room_id,
                target_user_id,
                actor_id,
                member: &created,
                outbox_event_factory: outbox_event_factory.as_ref(),
            },
        )
        .await?;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                new_status: Some("active".to_string()),
                role: Some(role.to_string()),
                source: Some("admin_add_member".to_string()),
                ..Default::default()
            },
        )
        .await;

        if notify {
            self.notify_room_invitation_best_effort(&target_user_id, &room, actor_username)
                .await;
        }

        Ok(created)
    }
}
