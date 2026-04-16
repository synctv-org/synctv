use async_trait::async_trait;
use synctv_core::models::{RoomId, UserId};

use crate::impls::ApiError;

#[async_trait]
pub trait PlaylistItemsSnapshotService: Send + Sync {
    async fn get_playlist_items_snapshot(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        req: &crate::proto::client::ListPlaylistItemsRequest,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError>;
}
