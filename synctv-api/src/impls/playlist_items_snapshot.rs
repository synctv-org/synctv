use async_trait::async_trait;

use crate::impls::client::RoomActor;
use crate::impls::ApiError;

#[async_trait]
pub trait PlaylistItemsSnapshotService: Send + Sync {
    async fn get_playlist_items_snapshot(
        &self,
        actor: &RoomActor,
        req: &synctv_proto::client::ListPlaylistItemsRequest,
    ) -> Result<synctv_proto::client::ListPlaylistItemsResponse, ApiError>;
}
