use crate::{
    models::{
        AuditAction, AuditDetails, AuditTargetType, ReviewRequestId, RoomId, RoomMember, UserId,
    },
    Error, Result,
};

use super::{
    outbox::MemberJoinedEffectsRequest, RealtimeOutboxPermissionChangedEventFactory, RoomService,
};

impl RoomService {
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
                crate::models::RoomPermission::REVIEW_JOIN_REQUESTS,
            )
            .await?;

        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &actor_id,
            crate::models::RoomPermission::REVIEW_JOIN_REQUESTS,
        )
        .await?;
        let (target_user_id, updated) = self
            .approve_pending_join_request_tx(&mut tx, &room_id, request_id, Some(&actor_id))
            .await?;
        self.apply_member_joined_effects_and_commit(
            tx,
            MemberJoinedEffectsRequest {
                room_id,
                target_user_id,
                actor_id,
                member: &updated,
                outbox_event_factory: outbox_event_factory.as_ref(),
            },
        )
        .await?;

        self.notify_membership_event_best_effort(
            &target_user_id,
            &room,
            "Your join request was approved".to_string(),
        )
        .await;

        Ok(updated)
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
        self.apply_member_joined_effects_and_commit(
            tx,
            MemberJoinedEffectsRequest {
                room_id,
                target_user_id,
                actor_id,
                member: &updated,
                outbox_event_factory: outbox_event_factory.as_ref(),
            },
        )
        .await?;

        self.audit_log(
            &actor_id,
            actor_username,
            AuditAction::MembershipUpdated,
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
}
