//! Adapter bridging runtime realtime delivery and `synctv-core` broadcasting traits.
//!
//! `RealtimePlaybackBroadcaster` implements the `PlaybackBroadcaster` trait from
//! `synctv-core` by delegating to `RealtimeManager`, keeping the core crate
//! decoupled from runtime-specific types.

use std::sync::Arc;
use synctv_realtime::sync::{RealtimeEvent, RealtimeManager};

fn system_user_id() -> synctv_core::models::UserId {
    synctv_core::models::UserId::MAX
}

fn bridge_user_id(user_id: Option<&synctv_core::models::UserId>) -> synctv_core::models::UserId {
    user_id.copied().unwrap_or_else(system_user_id)
}

fn playlist_created_event(
    room_id: &synctv_core::models::RoomId,
    playlist: &synctv_core::models::Playlist,
    user_id: &synctv_core::models::UserId,
    username: &str,
) -> RealtimeEvent {
    RealtimeEvent::PlaylistCreated {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
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
) -> RealtimeEvent {
    RealtimeEvent::PlaylistUpdated {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
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
) -> RealtimeEvent {
    RealtimeEvent::PlaylistDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        playlist_id: *playlist_id,
        timestamp: chrono::Utc::now(),
    }
}

#[must_use]
pub fn room_event_to_realtime_event(
    room_id: &synctv_core::models::RoomId,
    event: &synctv_core::service::RoomEvent,
) -> Option<RealtimeEvent> {
    let timestamp = chrono::Utc::now();
    match event {
        synctv_core::service::RoomEvent::MediaAdded {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(RealtimeEvent::MediaAdded {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            media_id: *media_id,
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaRemoved {
            user_id,
            username,
            media_id,
        } => Some(RealtimeEvent::MediaRemoved {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_id: *media_id,
            timestamp,
        }),
        synctv_core::service::RoomEvent::MediaUpdated {
            user_id,
            username,
            media_id,
            title,
            ..
        } => Some(RealtimeEvent::MediaUpdated {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            media_id: *media_id,
            media_title: title.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistReordered {
            user_id,
            username,
            media_ids,
        } => Some(RealtimeEvent::PlaylistReordered {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            media_ids: media_ids.clone(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::PlaylistDeleted {
            user_id,
            username,
            playlist_id,
        } => Some(RealtimeEvent::PlaylistDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            playlist_id: *playlist_id,
            timestamp,
        }),
        synctv_core::service::RoomEvent::UserLeft { user_id, username } => {
            Some(RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                user_id: *user_id,
                username: username.clone(),
                timestamp,
            })
        }
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
        } => Some(RealtimeEvent::PermissionChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            target_user_id: *user_id,
            target_username: String::new(),
            changed_by: *updated_by_user_id,
            changed_by_username: updated_by_username.clone(),
            new_permissions: synctv_core::models::RoomPermissionSet(*effective_permissions),
            role: *role,
            added_permissions: synctv_core::models::RoomPermissionSet(*added_permissions),
            removed_permissions: synctv_core::models::RoomPermissionSet(*removed_permissions),
            admin_added_permissions: synctv_core::models::RoomPermissionSet(
                *admin_added_permissions,
            ),
            admin_removed_permissions: synctv_core::models::RoomPermissionSet(
                *admin_removed_permissions,
            ),
            timestamp,
        }),
        synctv_core::service::RoomEvent::SettingsUpdated {
            settings,
            version,
            user_id,
            username,
        } => Some(RealtimeEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: bridge_user_id(user_id.as_ref()),
            username: username.clone(),
            settings_json: serde_json::to_vec(settings).unwrap_or_default(),
            version: *version,
            timestamp,
        }),
        synctv_core::service::RoomEvent::RoomDeleted => Some(RealtimeEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            deleted_by: system_user_id(),
            timestamp,
        }),
        synctv_core::service::RoomEvent::UserJoined { .. }
        | synctv_core::service::RoomEvent::ChatMessage { .. }
        | synctv_core::service::RoomEvent::PlaybackStateChanged { .. }
        | synctv_core::service::RoomEvent::MemberKicked { .. }
        | synctv_core::service::RoomEvent::GuestKicked { .. }
        | synctv_core::service::RoomEvent::StreamStarted { .. }
        | synctv_core::service::RoomEvent::StreamStopped { .. } => None,
    }
}

/// Adapter that implements `PlaybackBroadcaster` by delegating to `RealtimeManager`.
pub struct RealtimePlaybackBroadcaster {
    pub realtime_manager: Arc<RealtimeManager>,
}

impl synctv_core::service::PlaybackBroadcaster for RealtimePlaybackBroadcaster {
    fn broadcast_playback_state(
        &self,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> synctv_core::service::BroadcastResult {
        let event = RealtimeEvent::PlaybackStateChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: state.room_id,
            // For system-initiated broadcasts (auto-play, reset), use a sentinel user_id
            // with a clearly-invalid prefix that cannot collide with real user IDs.
            // The consumer in messaging.rs only reads the state payload, not the user fields.
            user_id: system_user_id(),
            username: synctv_common::reserved::SYSTEM_USERNAME.to_string(),
            state: state.clone(),
            timestamp: chrono::Utc::now(),
        };

        let result = self.realtime_manager.broadcast(event);
        let metrics = self.realtime_manager.metrics();
        let single_node = !metrics.distributed_enabled;

        synctv_core::service::BroadcastResult {
            local_sent: result.local_sent,
            redis_sent: result.redis_sent,
            single_node,
        }
    }
}

/// Adapter that implements `PlaylistBroadcaster` by delegating to `RealtimeManager`.
pub struct RealtimePlaylistBroadcaster {
    pub realtime_manager: Arc<RealtimeManager>,
}

impl synctv_core::service::PlaylistBroadcaster for RealtimePlaylistBroadcaster {
    fn broadcast_playlist_created(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self
            .realtime_manager
            .broadcast(playlist_created_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_updated(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self
            .realtime_manager
            .broadcast(playlist_updated_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_deleted(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist_id: &synctv_core::models::PlaylistId,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self.realtime_manager.broadcast(playlist_deleted_event(
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
    pub realtime_manager: Arc<RealtimeManager>,
}

impl synctv_core::service::PlaylistBroadcaster for LocalPlaylistBroadcaster {
    fn broadcast_playlist_created(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self
            .realtime_manager
            .broadcast_local(playlist_created_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_updated(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist: &synctv_core::models::Playlist,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self
            .realtime_manager
            .broadcast_local(playlist_updated_event(room_id, playlist, user_id, username));
    }

    fn broadcast_playlist_deleted(
        &self,
        room_id: &synctv_core::models::RoomId,
        playlist_id: &synctv_core::models::PlaylistId,
        user_id: &synctv_core::models::UserId,
        username: &str,
    ) {
        let _ = self
            .realtime_manager
            .broadcast_local(playlist_deleted_event(
                room_id,
                playlist_id,
                user_id,
                username,
            ));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        room_event_to_realtime_event, LocalPlaylistBroadcaster, RealtimePlaybackBroadcaster,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_core::models::{MediaId, Playlist, PlaylistId, RoomId, RoomPlaybackState, UserId};
    use synctv_realtime::sync::{
        PublishRequest, RealtimeConfig, RealtimeEvent, RealtimeManager, RealtimeMessageTransport,
        RealtimeMessageTransportConfig, RealtimeMessageTransportFactory,
        RealtimeMessageTransportRuntime, RoomMessageHub,
    };

    #[derive(Clone)]
    struct CaptureTransportFactory {
        publish_tx: tokio::sync::mpsc::Sender<PublishRequest>,
    }

    struct CaptureTransport {
        publish_tx: tokio::sync::mpsc::Sender<PublishRequest>,
    }

    impl RealtimeMessageTransportFactory for CaptureTransportFactory {
        fn build(
            &self,
            _config: RealtimeMessageTransportConfig,
        ) -> synctv_realtime::Result<Arc<dyn RealtimeMessageTransport>> {
            Ok(Arc::new(CaptureTransport {
                publish_tx: self.publish_tx.clone(),
            }))
        }
    }

    #[async_trait]
    impl RealtimeMessageTransport for CaptureTransport {
        async fn start(
            self: Arc<Self>,
            _publish_channel_capacity: usize,
        ) -> synctv_realtime::Result<RealtimeMessageTransportRuntime> {
            Ok(RealtimeMessageTransportRuntime {
                publish_tx: self.publish_tx.clone(),
                publisher_handle: tokio::spawn(async {}),
            })
        }

        async fn shutdown(&self) {}
    }

    fn sample_state() -> RoomPlaybackState {
        RoomPlaybackState {
            room_id: RoomId::expect_positive(120_001),
            playing_media_id: Some(MediaId::expect_positive(120_002)),
            playing_playlist_id: Some(PlaylistId::expect_positive(120_003)),
            target: Vec::new(),
            current_progress_id: None,
            is_playing: true,
            position: 42.0,
            speed: 1.0,
            version: 1,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_playlist(room_id: &RoomId) -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(120_003),
            room_id: *room_id,
            creator_id: Some(UserId::expect_positive(120_004)),
            name: "playlist".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
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
        let realtime_manager = Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "node-local".to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 16,
                publish_channel_capacity: 16,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 30,
                stream_max_length: 128,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("local realtime manager should build"),
        );
        let broadcaster = RealtimePlaybackBroadcaster { realtime_manager };

        let result = synctv_core::service::PlaybackBroadcaster::broadcast_playback_state(
            &broadcaster,
            &sample_state(),
        );

        assert!(
            result.single_node,
            "local-only realtime manager should map playback broadcasts to single-node success"
        );
        assert!(
            result.is_success(),
            "single-node playback broadcasts should not be reported as failures"
        );
    }

    #[tokio::test]
    async fn test_local_playlist_broadcaster_keeps_origin_delivery_without_redis_publish() {
        let room_id = RoomId::expect_positive(120_005);
        let user_id = UserId::expect_positive(120_006);
        let playlist = sample_playlist(&room_id);
        let (publish_tx, mut publish_rx) = tokio::sync::mpsc::channel(4);
        let realtime_manager = Arc::new(
            RealtimeManager::new(RealtimeConfig {
                distributed_transport_factory: Some(Arc::new(CaptureTransportFactory {
                    publish_tx,
                })),
                message_runtime: Arc::new(RoomMessageHub::new()),
                distributed_enabled: true,
                node_id: "node-cluster".to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 16,
                publish_channel_capacity: 16,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 30,
                stream_max_length: 128,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("realtime manager should build"),
        );
        let mut room_rx = realtime_manager
            .message_hub()
            .subscribe(room_id, user_id, "conn-playlist".to_string())
            .await
            .expect("room subscription should succeed");
        let broadcaster = LocalPlaylistBroadcaster {
            realtime_manager: realtime_manager.clone(),
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
            RealtimeEvent::PlaylistCreated {
                room_id, playlist, ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_005));
                assert_eq!(playlist.id, PlaylistId::expect_positive(120_003));
            }
            other => panic!("expected PlaylistCreated, got {other:?}"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), publish_rx.recv())
                .await
                .is_err(),
            "local playlist broadcaster must not enqueue a distributed publish"
        );

        realtime_manager.shutdown().await;
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_room_deleted() {
        let room_id = RoomId::expect_positive(120_007);

        let event =
            room_event_to_realtime_event(&room_id, &synctv_core::service::RoomEvent::RoomDeleted)
                .expect("RoomDeleted should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::RoomDeleted { room_id, .. } => {
                assert_eq!(room_id, RoomId::expect_positive(120_007));
            }
            other => panic!("expected RoomDeleted, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_user_left() {
        let room_id = RoomId::expect_positive(120_008);
        let user_id = UserId::expect_positive(120_009);

        let event = room_event_to_realtime_event(
            &room_id,
            &synctv_core::service::RoomEvent::UserLeft {
                user_id,
                username: "left-user".to_string(),
            },
        )
        .expect("UserLeft should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::UserLeft {
                room_id,
                user_id,
                username,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_008));
                assert_eq!(user_id, UserId::expect_positive(120_009));
                assert_eq!(username, "left-user");
            }
            other => panic!("expected UserLeft, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_to_realtime_event_maps_playlist_deleted() {
        let room_id = RoomId::expect_positive(120_010);
        let user_id = UserId::expect_positive(120_011);

        let event = room_event_to_realtime_event(
            &room_id,
            &synctv_core::service::RoomEvent::PlaylistDeleted {
                user_id: Some(user_id),
                username: "tester".to_string(),
                playlist_id: PlaylistId::expect_positive(120_012),
            },
        )
        .expect("PlaylistDeleted should bridge to RealtimeEvent");

        match event {
            RealtimeEvent::PlaylistDeleted {
                room_id,
                user_id,
                username,
                playlist_id,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(120_010));
                assert_eq!(user_id, UserId::expect_positive(120_011));
                assert_eq!(username, "tester");
                assert_eq!(playlist_id, PlaylistId::expect_positive(120_012));
            }
            other => panic!("expected PlaylistDeleted, got {other:?}"),
        }
    }

    #[test]
    fn test_room_event_bridge_keeps_direct_realtime_events_explicitly_unmapped() {
        let room_id = RoomId::expect_positive(120_013);
        let user_id = UserId::expect_positive(120_014);

        let events = [
            synctv_core::service::RoomEvent::UserJoined {
                user_id,
                username: "joiner".to_string(),
            },
            synctv_core::service::RoomEvent::ChatMessage {
                message_id: "chat-1".to_string(),
                user_id,
                username: "chat-user".to_string(),
                content: "hello".to_string(),
                timestamp: chrono::Utc::now(),
            },
            synctv_core::service::RoomEvent::PlaybackStateChanged {
                playing: true,
                position: 12.0,
                speed: 1.0,
                media_id: Some(MediaId::expect_positive(120_015)),
            },
        ];

        for event in events {
            assert!(
                room_event_to_realtime_event(&room_id, &event).is_none(),
                "{} has a direct realtime broadcaster and must not be bridged twice",
                event.event_type()
            );
        }
    }

    #[test]
    fn test_room_event_bridge_keeps_non_protocol_events_explicitly_unmapped() {
        let room_id = RoomId::expect_positive(120_016);
        let user_id = UserId::expect_positive(120_017);

        let events = [
            synctv_core::service::RoomEvent::MemberKicked { user_id },
            synctv_core::service::RoomEvent::GuestKicked {
                reason: synctv_core::service::notification::GuestKickReason::AdminKick,
                message: "guest removed".to_string(),
            },
            synctv_core::service::RoomEvent::StreamStarted {
                media_id: MediaId::expect_positive(120_018),
                user_id,
            },
            synctv_core::service::RoomEvent::StreamStopped {
                media_id: MediaId::expect_positive(120_019),
                user_id,
            },
        ];

        for event in events {
            assert!(
                room_event_to_realtime_event(&room_id, &event).is_none(),
                "{} has no stable ServerMessage protocol mapping yet",
                event.event_type()
            );
        }
    }
}
