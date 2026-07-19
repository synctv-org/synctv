use async_trait::async_trait;

use crate::impls::client::RoomActor;
use crate::impls::ApiError;

#[async_trait]
pub trait RoomMembersSnapshotService: Send + Sync {
    async fn get_room_members_snapshot(
        &self,
        actor: &RoomActor,
        req: &synctv_proto::client::GetRoomMembersRequest,
    ) -> Result<synctv_proto::client::GetRoomMembersResponse, ApiError>;
}
