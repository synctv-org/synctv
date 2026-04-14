//! Adapter bridging `synctv-cluster` and `synctv-core` broadcasting traits.
//!
//! `ClusterPlaybackBroadcaster` implements the `PlaybackBroadcaster` trait from
//! `synctv-core` by delegating to `ClusterManager`, keeping the core crate
//! decoupled from cluster-specific types.

use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, ClusterManager};

fn bridge_user_id(user_id: Option<&synctv_core::models::UserId>) -> synctv_core::models::UserId {
    user_id
        .cloned()
        .unwrap_or_else(|| synctv_core::models::UserId::from_string("__system__".to_string()))
}

fn playlist_created_event(
    room_id: &synctv_core::models::RoomId,
    playlist: &synctv_core::models::Playlist,
    user_id: &synctv_core::models::UserId,
    username: &str,
) -> ClusterEvent {
    ClusterEvent::PlaylistCreated {
        event_id: synctv_common::snanoid!(16),
        room_id: room_id.clone(),
        user_id: user_id.clone(),
        username: username.to_string(),
        playlist: playlist.clone(),
        timestamp: chrono::Utc::now(),
    }
}

fn playlist_updated_event(
    room_id: &synctv_core::models::RoomId,
    playlist: &synctv_core::models::Playlist,
    user_id: &synctv_core::models::UserId,
    username: &str,
) -> ClusterEvent {
    ClusterEvent::PlaylistUpdated {
        event_id: synctv_common::snanoid!(16),
        room_id: room_id.clone(),
        user_id: user_id.clone(),
        username: username.to_string(),
        playlist: playlist.clone(),
        timestamp: chrono::Utc::now(),
    }
}

fn playlist_deleted_event(
    room_id: &synctv_core::models::RoomId,
    playlist_id: &synctv_core::models::PlaylistId,
    user_id: &synctv_core::models::UserId,
    username: &str,
) -> ClusterEvent {
    ClusterEvent::PlaylistDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: room_id.clone(),
        user_id: user_id.clone(),
        username: username.to_string(),
        playlist_id: playlist_id.clone(),
        timestamp: chrono::Utc::now(),
    }
}

#[must_use]
pub fn room_event_to_cluster_event(
    room_id: &synctv_core::models::RoomId,
    event: &synctv_core::service::RoomEvent,
) -> Option<ClusterEvent> {
    let timestamp = chrono::Utc::now();
    match event {
        synctv_core::service::RoomEvent::MediaAdded {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(ClusterEvent::MediaAdded {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: username.clone(),
            media_id: synctv_core::models::MediaId::from_string(media_id.clone()),
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaRemoved {
            user_id,
            username,
            media_id,
        } => Some(ClusterEvent::MediaRemoved {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_id: synctv_core::models::MediaId::from_string(media_id.clone()),
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaUpdated {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(ClusterEvent::MediaUpdated {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: username.clone(),
            media_id: synctv_core::models::MediaId::from_string(media_id.clone()),
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistReordered {
            user_id,
            username,
            media_ids,
        } => Some(ClusterEvent::PlaylistReordered {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_ids: media_ids
                .iter()
                .cloned()
                .map(synctv_core::models::MediaId::from_string)
                .collect(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistDeleted {
            user_id,
            username,
            playlist_id,
        } => Some(ClusterEvent::PlaylistDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            playlist_id: synctv_core::models::PlaylistId::from_string(playlist_id.clone()),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PermissionChanged {
            user_id,
            role,
            effective_permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            updated_by_user_id,
            updated_by_username,
        } => Some(ClusterEvent::PermissionChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            target_user_id: user_id.clone(),
            target_username: String::new(),
            changed_by: updated_by_user_id.clone(),
            changed_by_username: updated_by_username.clone(),
            new_permissions: synctv_core::models::PermissionBits(*effective_permissions),
            role: *role,
            added_permissions: synctv_core::models::PermissionBits(*added_permissions),
            removed_permissions: synctv_core::models::PermissionBits(*removed_permissions),
            admin_added_permissions: synctv_core::models::PermissionBits(*admin_added_permissions),
            admin_removed_permissions: synctv_core::models::PermissionBits(
                *admin_removed_permissions,
            ),
            timestamp,
        }),
        synctv_core::service::RoomEvent::SettingsUpdated {
            settings,
            user_id,
            username,
        } => Some(ClusterEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            settings_json: serde_json::to_vec(settings).unwrap_or_default(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::RoomDeleted => Some(ClusterEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            deleted_by: synctv_core::models::UserId::from_string("__system__".to_string()),
            timestamp,
        }),
        _ => None,
    }
}

/// Adapter that implements `PlaybackBroadcaster` by delegating to `ClusterManager`.
pub struct ClusterPlaybackBroadcaster {
    pub cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::PlaybackBroadcaster for ClusterPlaybackBroadcaster {
    fn broadcast_playback_state(
        &self,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> synctv_core::service::BroadcastResult {
        let event = ClusterEvent::PlaybackStateChanged {
            event_id: synctv_common::snanoid!(16),
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
        let single_node = !metrics.distributed_enabled;

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
        let _ =
            self.cluster_manager.broadcast(ClusterEvent::KickUserFromRoom {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    reason: reason.to_string(),
                    timestamp: chrono::Utc::now(),
                });
    }

    fn broadcast_kick_user(&self, user_id: &synctv_core::models::UserId, reason: &str) {
        let _ = self
            .cluster_manager
            .broadcast(ClusterEvent::KickUser {
                event_id: synctv_common::snanoid!(16),
                user_id: user_id.clone(),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            });
    }
}

/// Adapter that implements `PlaylistBroadcaster` by delegating to `ClusterManager`.
pub struct ClusterPlaylistBroadcaster {
    pub cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::PlaylistBroadcaster for ClusterPlaylistBroadcaster {
    fn broadcast_playlist_created(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ =
            self.cluster_manager
                .broadcast(playlist_created_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_updated(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ =
            self.cluster_manager
                .broadcast(playlist_updated_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_deleted(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist_id: &synctv_core::models::PlaylistId,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self.cluster_manager.broadcast(playlist_deleted_event(
            room_id,
            playlist_id,
            user_id,
            username,
        ));
    }
}

/// Adapter that emits playlist lifecycle events to local subscribers only.
///
/// In clustered mode the API layer already handles fail-closed distributed
/// fan-out. This broadcaster exists solely to preserve origin-node realtime
/// delivery without double-publishing the same mutation to Redis.
pub struct LocalPlaylistBroadcaster {
    pub cluster_manager: Arc<ClusterManager>,
}

impl synctv_core::service::PlaylistBroadcaster for LocalPlaylistBroadcaster {
    fn broadcast_playlist_created(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self.cluster_manager.broadcast_local(playlist_created_event(
            room_id,
            playlist,
            user_id,
            username,
        ));
    }

    fn broadcast_playlist_updated(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self.cluster_manager.broadcast_local(playlist_updated_event(
            room_id,
            playlist,
            user_id,
            username,
        ));
    }

    fn broadcast_playlist_deleted(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist_id: &synctv_core::models::PlaylistId,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self.cluster_manager.broadcast_local(playlist_deleted_event(
            room_id,
            playlist_id,
            user_id,
            username,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{room_event_to_cluster_event, ClusterPlaybackBroadcaster, LocalPlaylistBroadcaster};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_cluster::sync::{
        ClusterConfig, ClusterManager, ClusterMessageTransport, ClusterMessageTransportConfig,
        ClusterMessageTransportFactory, ClusterMessageTransportRuntime, ClusterEvent,
        PublishRequest, RoomMessageHub,
    };
    use synctv_core::models::{MediaId, Playlist, PlaylistId, RoomId, RoomPlaybackState, UserId};

    #[derive(Clone)]
    struct CaptureTransportFactory {
        publish_tx: tokio::sync::mpsc::Sender<PublishRequest>,
    }

    struct CaptureTransport {
        publish_tx: tokio::sync::mpsc::Sender<PublishRequest>,
    }

    impl ClusterMessageTransportFactory for CaptureTransportFactory {
        fn build(
            &self,
            _config: ClusterMessageTransportConfig,
        ) -> synctv_cluster::error::Result<Arc<dyn ClusterMessageTransport>> {
            Ok(Arc::new(CaptureTransport {
                publish_tx: self.publish_tx.clone(),
            }))
        }
    }

    #[async_trait]
    impl ClusterMessageTransport for CaptureTransport {
        async fn start(
            self: Arc<Self>,
            _publish_channel_capacity: usize,
        ) -> synctv_cluster::error::Result<ClusterMessageTransportRuntime> {
            Ok(ClusterMessageTransportRuntime {
                publish_tx: self.publish_tx.clone(),
                publisher_handle: tokio::spawn(async {}),
            })
        }

        async fn shutdown(&self) {}
    }

    fn sample_state() -> RoomPlaybackState {
        RoomPlaybackState {
            room_id: RoomId::from_string("room-1".to_string()),
            playing_media_id: Some(MediaId::from_string("media-1".to_string())),
            playing_playlist_id: Some(PlaylistId::from_string("playlist-1".to_string())),
            target: Vec::new(),
            is_playing: true,
            current_time: 42.0,
            speed: 1.0,
            version: 1,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_playlist(room_id: &RoomId) -> Playlist {
        Playlist {
            id: PlaylistId::from_string("playlist-1".to_string()),
            room_id: room_id.clone(),
            creator_id: Some(UserId::from_string("user-1".to_string())),
            name: "playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        }
    }

    #[tokio::test]
    async fn test_playback_broadcaster_reports_single_node_without_redis() {
        let cluster_manager = Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    distributed_transport_factory: None,
                    message_runtime: Arc::new(RoomMessageHub::new()),
                    cluster_enabled: false,
                    node_id: "node-local".to_string(),
                    dedup_window: Duration::from_secs(30),
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

    #[tokio::test]
    async fn test_local_playlist_broadcaster_keeps_origin_delivery_without_redis_publish() {
        let room_id = RoomId::from_string("room-playlist".to_string());
        let user_id = UserId::from_string("user-playlist".to_string());
        let playlist = sample_playlist(&room_id);
        let (publish_tx, mut publish_rx) = tokio::sync::mpsc::channel(4);
        let cluster_manager = Arc::new(
            ClusterManager::new(
                ClusterConfig {
                    distributed_transport_factory: Some(Arc::new(CaptureTransportFactory {
                        publish_tx,
                    })),
                    message_runtime: Arc::new(RoomMessageHub::new()),
                    cluster_enabled: true,
                    node_id: "node-cluster".to_string(),
                    dedup_window: Duration::from_secs(30),
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
            .expect("cluster manager should build"),
        );
        let mut room_rx = cluster_manager
            .message_hub()
            .subscribe(
                room_id.clone(),
                user_id.clone(),
                "conn-playlist".to_string(),
            )
            .await
            .expect("room subscription should succeed");
        let broadcaster = LocalPlaylistBroadcaster {
            cluster_manager: cluster_manager.clone(),
        };

        synctv_core::service::PlaylistBroadcaster::broadcast_playlist_created(
            &broadcaster,
            &room_id,
            &playlist,
            &user_id,
            "tester",
        );

        let delivered = tokio::time::timeout(Duration::from_millis(200), room_rx.recv())
            .await
            .expect("playlist event should be delivered locally")
            .expect("local subscriber should stay open");
        match delivered {
            ClusterEvent::PlaylistCreated { room_id, playlist, .. } => {
                assert_eq!(room_id.as_str(), "room-playlist");
                assert_eq!(playlist.id.as_str(), "playlist-1");
            }
            other => panic!("expected PlaylistCreated, got {other:?}"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), publish_rx.recv())
                .await
                .is_err(),
            "local playlist broadcaster must not enqueue a distributed publish"
        );

        cluster_manager.shutdown().await;
    }

    #[test]
    fn test_room_event_to_cluster_event_maps_room_deleted() {
        let room_id = RoomId::from_string("room-deleted".to_string());

        let event = room_event_to_cluster_event(&room_id, &synctv_core::service::RoomEvent::RoomDeleted)
            .expect("RoomDeleted should bridge to ClusterEvent");

        match event {
            ClusterEvent::RoomDeleted { room_id, .. } => {
                assert_eq!(room_id.as_str(), "room-deleted");
            }
            other => panic!("expected RoomDeleted, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_cluster_event_maps_playlist_deleted() {
        let room_id = RoomId::from_string("room-playlist-deleted".to_string());
        let user_id = UserId::from_string("user-playlist-deleted".to_string());

        let event = room_event_to_cluster_event(
            &room_id,
            &synctv_core::service::RoomEvent::PlaylistDeleted {
                user_id: Some(user_id.clone()),
                username: "tester".to_string(),
                playlist_id: "playlist-123".to_string(),
            },
        )
        .expect("PlaylistDeleted should bridge to ClusterEvent");

        match event {
            ClusterEvent::PlaylistDeleted {
                room_id,
                user_id,
                username,
                playlist_id,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-playlist-deleted");
                assert_eq!(user_id.as_str(), "user-playlist-deleted");
                assert_eq!(username, "tester");
                assert_eq!(playlist_id.as_str(), "playlist-123");
            }
            other => panic!("expected PlaylistDeleted, got {other:?}"),
        }
    }
}
