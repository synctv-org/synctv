use crate::{
    models::{
        PageParams, Room, RoomId, RoomMember, RoomMemberListQuery, RoomMemberWithUser, UserId,
    },
    Error, Result,
};

use super::RoomService;

#[derive(Debug, Clone)]
pub enum RealtimeMembershipAccess {
    Allowed(RoomMember),
    Denied(String),
}

impl RoomService {
    /// Update the room's `last_activity_at` timestamp.
    ///
    /// Call this after chat messages, playback state changes, or member
    /// joins/leaves to prevent active rooms from being expired by the TTL
    /// cleanup.
    pub async fn touch_room_activity(&self, room_id: RoomId) {
        if let Err(e) = self.room_repo.touch_activity(&room_id).await {
            tracing::debug!(error = %e, room_id = %room_id, "Failed to touch room activity");
        }
    }

    /// Get room members with user info.
    pub async fn get_room_members(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        self.member_service.list_members(room_id).await
    }

    /// Get room members with database-level pagination.
    pub async fn get_room_members_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        self.member_service
            .list_members_paginated(room_id, pagination)
            .await
    }

    pub async fn get_room_members_query(
        &self,
        room_id: &RoomId,
        query: RoomMemberListQuery,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        self.member_service.list_members_query(room_id, query).await
    }

    /// Get member count for a room.
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        self.member_service.count_members(room_id).await
    }

    /// Get member counts for multiple rooms in a single query.
    pub async fn get_member_count_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        self.member_service.count_members_batch(room_ids).await
    }

    pub async fn get_member_count_batch_eventually_consistent(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        self.member_repo
            .count_by_rooms_batch_eventually_consistent(room_ids)
            .await
    }

    /// Get a specific room member record.
    pub async fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMember>> {
        self.member_service.get_member(room_id, user_id).await
    }

    /// Check if user is a member of the room.
    pub async fn check_membership(&self, room_id: &RoomId, user_id: &UserId) -> Result<()> {
        let room = self.get_room(room_id).await?;
        self.check_membership_with_room(&room, user_id).await
    }

    pub async fn check_membership_with_room(&self, room: &Room, user_id: &UserId) -> Result<()> {
        self.ensure_room_creator_is_active_for_access(room, user_id)
            .await?;

        if self.member_service.is_member(&room.id, user_id).await? {
            Ok(())
        } else {
            Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ))
        }
    }

    pub async fn realtime_membership_access_with_room(
        &self,
        room: &Room,
        user_id: &UserId,
    ) -> Result<RealtimeMembershipAccess> {
        self.ensure_room_creator_is_active_for_access(room, user_id)
            .await?;

        if let Some(member) = self.member_service.get_member(&room.id, user_id).await? {
            return Ok(RealtimeMembershipAccess::Allowed(member));
        }

        if self
            .member_service
            .is_in_kick_cooldown(&room.id, user_id)
            .await?
        {
            return Ok(RealtimeMembershipAccess::Denied(
                Error::kick_cooldown_denied_message().to_string(),
            ));
        }

        Ok(RealtimeMembershipAccess::Denied(
            synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
        ))
    }
}
