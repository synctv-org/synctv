use async_trait::async_trait;
use synctv_core::models::{RoomId, UserId};

use crate::impls::ApiError;

#[async_trait]
pub trait RoomMembersSnapshotService: Send + Sync {
    async fn get_room_members_snapshot(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        req: &crate::proto::client::GetRoomMembersRequest,
    ) -> Result<crate::proto::client::GetRoomMembersResponse, ApiError>;
}
