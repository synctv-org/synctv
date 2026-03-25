//! Adapter bridging `synctv-cluster` and `synctv-core` broadcasting traits.
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
        let metrics = self.cluster_manager.metrics();
        let single_node = !metrics.redis_enabled;

        synctv_core::service::BroadcastResult {
            local_sent: result.local_sent,
            redis_sent: result.redis_sent,
            single_node,
        }
    }
}

/// Adapter that implements `MemberEventBroadcaster` by delegating to `ClusterManager`.
pub struct ClusterMemberEventBroadcaster {
    pub cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::MemberEventBroadcaster for ClusterMemberEventBroadcaster {
    fn broadcast_kick_from_room(
        &self,
        room_id: &synctv_core::models::RoomId,
        user_id: &synctv_core::models::UserId,
        reason: &str,
    ) {
        let _ = self
            .cluster_manager
            .broadcast(synctv_cluster::sync::ClusterEvent::KickUserFromRoom {
                event_id: nanoid::nanoid!(16),
                room_id: room_id.clone(),
                user_id: user_id.clone(),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            });
    }

    fn broadcast_kick_user(
        &self,
        user_id: &synctv_core::models::UserId,
        reason: &str,
    ) {
        let _ = self
            .cluster_manager
            .broadcast(synctv_cluster::sync::ClusterEvent::KickUser {
                event_id: nanoid::nanoid!(16),
                user_id: user_id.clone(),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::ClusterPlaybackBroadcaster;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_cluster::sync::{ClusterConfig, ClusterManager};
    use synctv_core::models::{MediaId, PlaylistId, RoomId, RoomPlaybackState};

    fn sample_state() -> RoomPlaybackState {
        RoomPlaybackState {
            room_id: RoomId::from_string("room-1".to_string()),
            playing_media_id: Some(MediaId::from_string("media-1".to_string())),
            playing_playlist_id: Some(PlaylistId::from_string("playlist-1".to_string())),
            relative_path: String::new(),
            is_playing: true,
            current_time: 42.0,
            speed: 1.0,
            version: 1,
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_playback_broadcaster_reports_single_node_without_redis() {
        let cluster_manager = Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    redis_client: None,
                    redis_conn: None,
                    shared_redis_conn: None,
                    cluster_enabled: false,
                    node_id: "node-local".to_string(),
                    dedup_window: Duration::from_secs(30),
                    cleanup_interval: Duration::from_secs(30),
                    critical_channel_capacity: 16,
                    publish_channel_capacity: 16,
                    key_prefix: "test:".to_string(),
                    catchup_window_secs: 30,
                    stream_max_length: 128,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("local cluster manager should build"),
        );
        let broadcaster = ClusterPlaybackBroadcaster { cluster_manager };

        let result = synctv_core::service::PlaybackBroadcaster::broadcast_playback_state(
            &broadcaster,
            &sample_state(),
        );

        assert!(
            result.single_node,
            "local-only cluster manager should map playback broadcasts to single-node success"
        );
        assert!(
            result.is_success(),
            "single-node playback broadcasts should not be reported as failures"
        );
    }
}
