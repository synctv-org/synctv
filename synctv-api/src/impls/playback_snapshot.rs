use async_trait::async_trait;
use synctv_core::models::{RoomId, RoomPlaybackState, UserId};

use crate::impls::ApiError;

#[async_trait]
pub trait PlaybackSnapshotService: Send + Sync {
    async fn get_playback_snapshot(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError>;
}

pub(crate) fn static_playback_snapshot_version(media: &synctv_core::models::Media) -> String {
    media.version.to_string()
}

pub(crate) fn dynamic_playback_snapshot_version(
    playlist: &synctv_core::models::Playlist,
) -> String {
    playlist.version.to_string()
}

pub(crate) fn playback_snapshot_expires_at(
    snapshot: &crate::proto::client::PlaybackSnapshot,
) -> Option<i64> {
    snapshot
        .playback_infos
        .values()
        .flat_map(|info| info.urls.iter().filter_map(|url| url.expire_at))
        .min()
}
