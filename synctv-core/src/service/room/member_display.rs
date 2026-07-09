use crate::{
    models::{RoomId, UserId},
    Result,
};

use super::{RealtimeOutboxPermissionChangedEventFactory, RoomService};

pub struct UpdateMemberRemarkNameWithOutboxRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub remark_name: String,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

pub struct UpdateMemberDisplayTagWithOutboxRequest {
    pub room_id: RoomId,
    pub actor_id: UserId,
    pub target_user_id: UserId,
    pub display_tag: String,
    pub outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
}

enum MemberDisplayFieldUpdate {
    RemarkName(String),
    DisplayTag(String),
}

impl RoomService {
    pub async fn admin_update_member_remark_name_with_outbox(
        &self,
        request: UpdateMemberRemarkNameWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberRemarkNameWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            remark_name,
            outbox_event_factory,
        } = request;
        self.update_member_display_field_with_outbox_inner(
            room_id,
            actor_id,
            target_user_id,
            MemberDisplayFieldUpdate::RemarkName(remark_name),
            outbox_event_factory,
            false,
        )
        .await
    }

    pub async fn update_member_remark_name_with_outbox(
        &self,
        request: UpdateMemberRemarkNameWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberRemarkNameWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            remark_name,
            outbox_event_factory,
        } = request;
        self.update_member_display_field_with_outbox_inner(
            room_id,
            actor_id,
            target_user_id,
            MemberDisplayFieldUpdate::RemarkName(remark_name),
            outbox_event_factory,
            true,
        )
        .await
    }

    pub async fn admin_update_member_display_tag_with_outbox(
        &self,
        request: UpdateMemberDisplayTagWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberDisplayTagWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            display_tag,
            outbox_event_factory,
        } = request;
        self.update_member_display_field_with_outbox_inner(
            room_id,
            actor_id,
            target_user_id,
            MemberDisplayFieldUpdate::DisplayTag(display_tag),
            outbox_event_factory,
            false,
        )
        .await
    }

    pub async fn update_member_display_tag_with_outbox(
        &self,
        request: UpdateMemberDisplayTagWithOutboxRequest,
    ) -> Result<crate::models::RoomMember> {
        let UpdateMemberDisplayTagWithOutboxRequest {
            room_id,
            actor_id,
            target_user_id,
            display_tag,
            outbox_event_factory,
        } = request;
        self.update_member_display_field_with_outbox_inner(
            room_id,
            actor_id,
            target_user_id,
            MemberDisplayFieldUpdate::DisplayTag(display_tag),
            outbox_event_factory,
            true,
        )
        .await
    }

    async fn update_member_display_field_with_outbox_inner(
        &self,
        room_id: RoomId,
        actor_id: UserId,
        target_user_id: UserId,
        update: MemberDisplayFieldUpdate,
        outbox_event_factory: Option<RealtimeOutboxPermissionChangedEventFactory>,
        require_room_permission: bool,
    ) -> Result<crate::models::RoomMember> {
        let mut tx = self.pool.begin().await?;
        if require_room_permission {
            self.ensure_actor_has_room_permission_now_tx(
                &mut tx,
                &room_id,
                &actor_id,
                crate::models::RoomPermission::SET_MEMBER_PERMISSIONS,
            )
            .await?;
        }

        let (remark_name, display_tag) = match update {
            MemberDisplayFieldUpdate::RemarkName(remark_name) => (Some(remark_name), None),
            MemberDisplayFieldUpdate::DisplayTag(display_tag) => (None, Some(display_tag)),
        };
        let updated = self
            .member_repo
            .update_display_info_with_executor(
                &room_id,
                &target_user_id,
                remark_name.as_deref(),
                display_tag.as_deref(),
                &mut *tx,
            )
            .await?;

        let snapshot = self
            .prepare_and_insert_member_update_outbox(
                &mut tx,
                room_id,
                target_user_id,
                actor_id,
                Some(&updated),
                false,
                outbox_event_factory.as_ref(),
            )
            .await?;

        self.commit_member_update_with_outbox(
            tx,
            None,
            &snapshot,
            updated.version,
            "update_member_display_field_with_outbox",
        )
        .await?;
        Ok(updated)
    }
}
