//! Adapter bridging `synctv-cluster` and `synctv-core` playback broadcasting.
//!
//! `ClusterPlaybackBroadcaster` implements the `PlaybackBroadcaster` trait from
//! `synctv-core` by delegating to `ClusterManager`, keeping the core crate
//! decoupled from cluster-specific types.

use std::sync::Arc;
use synctv_cluster::sync::ClusterManager;

/// Adapter that implements `PlaybackBroadcaster` by delegating to `ClusterManager`.
pub struct ClusterPlaybackBroadcaster {
    pub cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::PlaybackBroadcaster for ClusterPlaybackBroadcaster {
    fn broadcast_playback_state(
        &self,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> synctv_core::service::BroadcastResult {
        let event = synctv_cluster::sync::ClusterEvent::PlaybackStateChanged {
            event_id: nanoid::nanoid!(16),
            room_id: state.room_id.clone(),
            // For system-initiated broadcasts (auto-play, reset), use a sentinel user_id
            // with a clearly-invalid prefix that cannot collide with real user IDs.
            // The consumer in messaging.rs only reads the state payload, not the user fields.
            user_id: synctv_core::models::UserId::from_string("__system__".to_string()),
            username: "__system__".to_string(),
            state: state.clone(),
            timestamp: chrono::Utc::now(),
        };

        let result = self.cluster_manager.broadcast(event);

        synctv_core::service::BroadcastResult {
            local_sent: result.local_sent,
            redis_sent: result.redis_sent,
        }
    }
}
