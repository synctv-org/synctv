use crate::{
    models::{
        AddMemberOptions, AuditAction, AuditDetails, AuditTargetType, MemberStatus,
        ReviewRequestId, Room, RoomId, RoomMember, RoomRole, UserId,
    },
    repository::ReviewRepository,
    Error, Result,
};

use super::{
    cleanup_member_resources_in_tx, AddMemberWithOutboxRequest, AdminAddMemberWithOutboxRequest,
    AdminRejectJoinRequestWithOutbox, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxPermissionChangedEventFactory, RealtimeOutboxUserLeftEventFactory, RoomService,
};

fn normalized_rejection_reason(reason: Option<&str>) -> Option<&str> {
    reason.map(str::trim).filter(|value| !value.is_empty())
}

fn rejection_reason(reason: Option<&str>) -> Option<String> {
    normalized_rejection_reason(reason).map(ToOwned::to_owned)
}

fn rejection_notification(reason: Option<&str>) -> String {
    match normalized_rejection_reason(reason) {
        Some(reason) => format!("Your join request was rejected: {reason}"),
        None => "Your join request was rejected".to_string(),
    }
}

struct AddActiveMemberTxRequest<'a> {
    room_id: &'a RoomId,
    target_user_id: &'a UserId,
    role: RoomRole,
    remark_name: String,
    display_tag: String,
    reviewed_by: Option<&'a UserId>,
    require_pending_review: bool,
}

impl RoomService {
    async fn active_member_add_options(&self, room_id: &RoomId) -> Result<AddMemberOptions> {
        let room_settings = self.room_settings_repo.get(room_id).await?;
        Ok(AddMemberOptions::new().with_max_members(room_settings.max_members.0))
    }

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

    async fn approve_pending_join_request_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        request_id: ReviewRequestId,
        reviewed_by: Option<&UserId>,
    ) -> Result<(UserId, RoomMember)> {
        let (target_user_id, requested_role) =
            Self::load_pending_join_request_by_id_for_update(tx, room_id, request_id).await?;
        self.ensure_target_user_can_join_now_tx(tx, &target_user_id)
            .await?;
        Self::ensure_room_can_admit_member_now_tx(tx, room_id, &target_user_id).await?;
        let role = Self::validate_join_request_role(requested_role)?;

        let mut member = RoomMember::new(*room_id, target_user_id, role);
        member.status = MemberStatus::Active;
        let options = self.active_member_add_options(room_id).await?;
        let created = self
            .member_repo
            .add_with_options_tx(&member, &options, tx)
            .await?;
        let resolved = Self::resolve_pending_join_request_by_id_as_approved_tx(
            tx,
            request_id,
            room_id,
            reviewed_by,
        )
        .await?;
        if resolved == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }

        Ok((target_user_id, created))
    }

    pub(super) fn validate_user_can_join(user: &crate::models::User) -> Result<()> {
        if user.is_banned {
            return Err(Error::Authorization(
                "Target user cannot be added while banned".to_string(),
            ));
        }
        if !user.status.can_join_room() {
            return Err(Error::Authorization(format!(
                "Target user cannot be added while account status is {}",
                user.status
            )));
        }
        Ok(())
    }

    async fn ensure_target_user_can_join(&self, target_user_id: &UserId) -> Result<()> {
        let target_user = self.user_service.get_user(target_user_id).await?;
        Self::validate_user_can_join(&target_user)
    }

    fn validate_join_request_role(role: RoomRole) -> Result<RoomRole> {
        match role {
            RoomRole::Guest | RoomRole::Member => Ok(role),
            RoomRole::Admin | RoomRole::Creator => Err(Error::InvalidInput(
                "Join requests cannot grant elevated room roles".to_string(),
            )),
        }
    }

    async fn notify_membership_event_best_effort(
        &self,
        target_user_id: &UserId,
        room: &Room,
        event: String,
    ) {
        let Some(ref notif_service) = self.user_notification_service else {
            return;
        };

        if let Err(error) = notif_service
            .create_room_event(
                *target_user_id,
                room.id.to_string(),
                room.name.clone(),
                event,
            )
            .await
        {
            tracing::warn!(
                room_id = %room.id,
                user_id = %target_user_id,
                error = %error,
                "Failed to create room membership notification"
            );
        }
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
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&created),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        self.insert_member_joined_system_chat_tx(
            &mut tx,
            room_id,
            target_user_id,
            snapshot.target_username.clone(),
            actor_id,
            snapshot.changed_by_username.clone(),
            created.role,
        )
        .await?;
        tx.commit().await?;
        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, created.version)
            .await;

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

    /// Approve a specific pending join request and promote it to an active membership.
    pub async fn approve_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
    ) -> Result<RoomMember> {
        self.approve_join_request_with_outbox(room_id, actor_id, request_id, None)
            .await
    }

    pub async fn approve_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<RoomMember> {
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
                crate::models::RoomPermission::APPROVE_MEMBER,
            )
            .await?;

        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::APPROVE_MEMBER,
        )
        .await?;
        let (target_user_id, updated) = self
            .approve_pending_join_request_tx(&mut tx, &room_id, request_id, Some(&actor_id))
            .await?;
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        self.insert_member_joined_system_chat_tx(
            &mut tx,
            room_id,
            target_user_id,
            snapshot.target_username.clone(),
            actor_id,
            snapshot.changed_by_username.clone(),
            updated.role,
        )
        .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, updated.version)
            .await;

        self.notify_membership_event_best_effort(
            &target_user_id,
            &room,
            "Your join request was approved".to_string(),
        )
        .await;

        Ok(updated)
    }

    /// Reject a specific pending join request without banning the user from the room.
    pub async fn reject_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        reason: Option<&str>,
    ) -> Result<UserId> {
        self.reject_join_request_with_outbox(room_id, actor_id, request_id, reason, None)
            .await
    }

    pub async fn reject_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        request_id: ReviewRequestId,
        reason: Option<&str>,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<UserId> {
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
                crate::models::RoomPermission::APPROVE_MEMBER,
            )
            .await?;

        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::APPROVE_MEMBER,
        )
        .await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        let rejected = ReviewRepository::reject_room_join_with_executor(
            &mut *tx,
            request_id,
            room_id,
            Some(actor_id),
            reason,
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                None,
                Self::permission_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        let actor_username = self.actor_username_required(&actor_id).await?;

        self.audit_log(
            &actor_id,
            &actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("rejected".to_string()),
                source: Some("reject_join_request".to_string()),
                reason: rejection_reason(reason),
                ..Default::default()
            },
        )
        .await;

        let event = rejection_notification(reason);
        self.notify_membership_event_best_effort(&target_user_id, &room, event)
            .await;

        Ok(target_user_id)
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
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&created),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        self.insert_member_joined_system_chat_tx(
            &mut tx,
            room_id,
            target_user_id,
            snapshot.target_username.clone(),
            actor_id,
            snapshot.changed_by_username.clone(),
            created.role,
        )
        .await?;
        tx.commit().await?;
        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, created.version)
            .await;

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

    /// Administrative override: approve a specific pending join request.
    pub async fn admin_approve_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
    ) -> Result<RoomMember> {
        self.admin_approve_join_request_with_outbox(
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            None,
        )
        .await
    }

    pub async fn admin_approve_join_request_with_outbox(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
    ) -> Result<RoomMember> {
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, updated) = self
            .approve_pending_join_request_tx(&mut tx, &room_id, request_id, reviewed_by)
            .await?;
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
                Self::role_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        self.insert_member_joined_system_chat_tx(
            &mut tx,
            room_id,
            target_user_id,
            snapshot.target_username.clone(),
            actor_id,
            snapshot.changed_by_username.clone(),
            updated.role,
        )
        .await?;
        tx.commit().await?;

        self.permission_service
            .seed_added_member_cache(&room_id, &target_user_id, updated.version)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("approved".to_string()),
                source: Some("admin_approve_join_request".to_string()),
                ..Default::default()
            },
        )
        .await;

        self.notify_membership_event_best_effort(
            &target_user_id,
            &room,
            "Your join request was approved".to_string(),
        )
        .await;

        Ok(updated)
    }

    /// Administrative override: reject a specific pending join request without banning the user.
    pub async fn admin_reject_join_request(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        reviewed_by: Option<&UserId>,
        actor_username: &str,
        request_id: ReviewRequestId,
        reason: Option<&str>,
    ) -> Result<UserId> {
        self.admin_reject_join_request_with_outbox(AdminRejectJoinRequestWithOutbox {
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            reason,
            outbox_event_factory: None,
        })
        .await
    }

    pub async fn admin_reject_join_request_with_outbox(
        &self,
        request: AdminRejectJoinRequestWithOutbox<'_>,
    ) -> Result<UserId> {
        let AdminRejectJoinRequestWithOutbox {
            room_id,
            actor_id,
            reviewed_by,
            actor_username,
            request_id,
            reason,
            outbox_event_factory,
        } = request;
        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let mut tx = self.pool.begin().await?;
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        let rejected = ReviewRepository::reject_room_join_with_executor(
            &mut *tx,
            request_id,
            room_id,
            reviewed_by.copied(),
            reason,
        )
        .await?;
        if rejected == 0 {
            return Err(Error::NotFound(
                "Pending join request not found".to_string(),
            ));
        }
        let snapshot = self
            .permission_changed_snapshot_tx(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                None,
                Self::permission_member_event_scope(),
            )
            .await?;
        self.insert_permission_changed_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;

        self.permission_service
            .invalidate_cache(&room_id, &target_user_id)
            .await;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MemberStatusUpdated,
            AuditTargetType::Member,
            Some(target_user_id.to_string()),
            AuditDetails {
                room_id: Some(room_id.to_string()),
                request_id: Some(request_id.to_string()),
                previous_review_status: Some("pending".to_string()),
                new_review_status: Some("rejected".to_string()),
                source: Some("admin_reject_join_request".to_string()),
                reason: rejection_reason(reason),
                ..Default::default()
            },
        )
        .await;

        let event = rejection_notification(reason);
        self.notify_membership_event_best_effort(&target_user_id, &room, event)
            .await;

        Ok(target_user_id)
    }

    /// Leave a room.
    ///
    /// Lifecycle rules:
    /// - the actor must currently be an active member of the room
    /// - the creator cannot leave and must transfer ownership or delete the room
    ///
    /// **Important for callers**: This method only removes the membership record
    /// and sends an in-app notification. It does NOT disconnect active room
    /// connections or fan out cluster disconnect events.
    pub async fn leave_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        self.leave_room_with_outbox(room_id, user_id, None, None)
            .await
    }

    pub async fn leave_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxUserLeftEventFactory>,
        cleanup_outbox_event_factory: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "User leaving room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let membership = self
            .member_repo
            .get(&room_id, &user_id)
            .await?
            .ok_or_else(|| Error::Authorization("You are not a member of this room".to_string()))?;

        if room.created_by == user_id {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        if membership.role == RoomRole::Creator {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        let snapshot = self.user_left_snapshot(room_id, user_id).await?;
        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = (match self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => return Err(error),
        }) else {
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &user_id, &mut tx)
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
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &user_id).await {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_user_left_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_member_resource_cleanup_outbox_tx(
                &mut tx,
                &cleanup,
                cleanup_outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(&fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(&fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            &fence,
            &room_id,
            &user_id,
            removed_version,
            "leave_room_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &user_id, &cleanup)
            .await;

        // Notify room members with username
        let username = snapshot.username;
        let subscriber_count = self
            .notification_service
            .notify_user_left(&room_id, &user_id, &username);
        tracing::debug!(
            room_id = %room_id,
            user_id = %user_id,
            subscriber_count,
            "User left notification dispatched"
        );

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            deleted_playlists = cleanup.deleted_playlist_ids.len(),
            deleted_media = cleanup.deleted_media_ids.len(),
            "User left room"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{rejection_notification, rejection_reason};

    #[test]
    fn rejection_reason_preserves_missing_and_trims_present_reason() {
        assert_eq!(rejection_reason(None), None);
        assert_eq!(rejection_reason(Some("   ")), None);
        assert_eq!(
            rejection_reason(Some("  duplicate  ")).as_deref(),
            Some("duplicate")
        );
    }

    #[test]
    fn rejection_notification_uses_trimmed_reason_when_present() {
        assert_eq!(
            rejection_notification(Some("  duplicate  ")),
            "Your join request was rejected: duplicate"
        );
        assert_eq!(
            rejection_notification(Some("   ")),
            "Your join request was rejected"
        );
    }
}
