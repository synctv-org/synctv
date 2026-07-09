use crate::{
    models::{AuditAction, AuditDetails, AuditTargetType, ReviewRequestId, RoomId, UserId},
    repository::ReviewRepository,
    Error, Result,
};

use super::{RealtimeOutboxPermissionChangedEventFactory, RoomService};

pub struct AdminRejectJoinRequestWithOutbox<'a> {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub reviewed_by: Option<&'a UserId>,
    pub actor_username: &'a str,
    pub request_id: ReviewRequestId,
    pub reason: Option<&'a str>,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

struct RejectJoinRequestExecution<'a> {
    room_id: RoomId,
    actor_id: UserId,
    reviewed_by: Option<UserId>,
    actor_permission_check: Option<crate::models::RoomPermission>,
    request_id: ReviewRequestId,
    reason: Option<&'a str>,
    outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

struct RejectedJoinRequest {
    target_user_id: UserId,
}

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

impl RoomService {
    async fn reject_join_request_execution(
        &self,
        request: RejectJoinRequestExecution<'_>,
    ) -> Result<RejectedJoinRequest> {
        let RejectJoinRequestExecution {
            room_id,
            actor_id,
            reviewed_by,
            actor_permission_check,
            request_id,
            reason,
            outbox_event_factory,
        } = request;

        let mut tx = self.pool.begin().await?;
        if let Some(permission) = actor_permission_check {
            self.ensure_actor_has_room_permission_now_tx(&mut tx, &room_id, &actor_id, permission)
                .await?;
        }
        let (target_user_id, _) =
            Self::load_pending_join_request_by_id_for_update(&mut tx, &room_id, request_id).await?;
        let rejected = ReviewRepository::reject_room_join_with_executor(
            &mut *tx,
            request_id,
            room_id,
            reviewed_by,
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

        Ok(RejectedJoinRequest { target_user_id })
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

        let rejected = self
            .reject_join_request_execution(RejectJoinRequestExecution {
                room_id,
                actor_id,
                reviewed_by: Some(actor_id),
                actor_permission_check: Some(crate::models::RoomPermission::APPROVE_MEMBER),
                request_id,
                reason,
                outbox_event_factory,
            })
            .await?;
        let target_user_id = rejected.target_user_id;

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

        let rejected = self
            .reject_join_request_execution(RejectJoinRequestExecution {
                room_id,
                actor_id,
                reviewed_by: reviewed_by.copied(),
                actor_permission_check: None,
                request_id,
                reason,
                outbox_event_factory,
            })
            .await?;
        let target_user_id = rejected.target_user_id;

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
