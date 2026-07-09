use crate::{
    models::{MemberStatus, ReviewRequestId, Room, RoomId, RoomMember, RoomRole, UserId},
    Error, Result,
};

use super::RoomService;

impl RoomService {
    pub(in crate::service::room) async fn approve_pending_join_request_tx(
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

    fn validate_join_request_role(role: RoomRole) -> Result<RoomRole> {
        match role {
            RoomRole::Guest | RoomRole::Member => Ok(role),
            RoomRole::Admin | RoomRole::Creator => Err(Error::InvalidInput(
                "Join requests cannot grant elevated room roles".to_string(),
            )),
        }
    }

    pub(in crate::service::room) async fn notify_membership_event_best_effort(
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
}
