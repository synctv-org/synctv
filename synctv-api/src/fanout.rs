use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{RoomId, UserId};

use crate::cluster_fanout::ClusterFanoutService;
use crate::impls::{ApiError, ClusterEventPublishReservation};
use crate::runtime::RealtimeEventService;

#[async_trait]
pub trait RoomSettingsFanoutService: Send + Sync {
    async fn reserve_settings_changed(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError>;

    fn publish_settings_changed(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
        version: i64,
    );
}

pub struct DefaultRoomSettingsFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultRoomSettingsFanoutService {
    #[must_use]
    pub fn new(
        cluster_fanout: Arc<dyn ClusterFanoutService>,
        _event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultRoomSettingsFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultRoomSettingsFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl RoomSettingsFanoutService for DefaultRoomSettingsFanoutService {
    async fn reserve_settings_changed(
        &self,
    ) -> Result<Option<ClusterEventPublishReservation>, ApiError> {
        self.cluster_fanout
            .reserve("failed to fan out RoomSettingsChanged to cluster replicas")
            .await
    }

    fn publish_settings_changed(
        &self,
        reservation: Option<ClusterEventPublishReservation>,
        room_id: &RoomId,
        actor_user_id: &UserId,
        actor_username: &str,
        settings_json: Vec<u8>,
        version: i64,
    ) {
        let event = ClusterEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id: room_id.clone(),
            user_id: actor_user_id.clone(),
            username: actor_username.to_string(),
            settings_json,
            version,
            timestamp: chrono::Utc::now(),
        };
        self.cluster_fanout
            .publish(reservation, PublishRequest { event });
    }
}

#[must_use]
pub fn default_room_settings_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn RoomSettingsFanoutService> {
    Arc::new(DefaultRoomSettingsFanoutService::new(
        cluster_fanout,
        event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_room_settings_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use synctv_cluster::sync::{BroadcastResult, ClusterEvent, ConnectionId};
    use synctv_core::models::{RoomId, UserId};
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        broadcast_calls: AtomicUsize,
        broadcast_local_calls: AtomicUsize,
        local_events: Mutex<Vec<(String, ClusterEvent)>>,
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_cluster::Result<(mpsc::Receiver<ClusterEvent>, ConnectionId)> {
            panic!("subscribe_with_id should not be called in room settings fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in room settings fanout tests");
        }

        fn broadcast(&self, _event: ClusterEvent) -> BroadcastResult {
            self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
            BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: ClusterEvent) -> bool {
            panic!("publish_only should not be called in room settings fanout tests");
        }

        fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize {
            self.broadcast_local_calls.fetch_add(1, Ordering::SeqCst);
            self.local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .push((room_id.as_str().to_string(), event.clone()));
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
            panic!("subscribe_admin_events should not be called in room settings fanout tests");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: true,
            }
        }

        fn node_id(&self) -> &'static str {
            "room-settings-fanout-test-node"
        }

        async fn shutdown(&self) {}
    }

    fn room_id() -> RoomId {
        RoomId::from_string("room-fanout".to_string())
    }

    fn user_id() -> UserId {
        UserId::from_string("user-fanout".to_string())
    }

    #[tokio::test]
    async fn test_room_settings_fanout_is_noop_when_cluster_fanout_is_local() {
        let service =
            default_room_settings_fanout_service(default_cluster_fanout_service(None, false), None);
        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("local room settings fanout should not fail");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"allow_guest_join":true}"#.to_vec(),
            1,
        );
    }

    #[tokio::test]
    async fn test_cluster_room_settings_fanout_publishes_when_channel_available() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_room_settings_fanout_service(
            default_cluster_fanout_service(Some(tx), true),
            None,
        );

        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("cluster room settings fanout should reserve");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"require_password":false}"#.to_vec(),
            9,
        );

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::RoomSettingsChanged {
                room_id,
                user_id,
                username,
                settings_json,
                version,
                ..
            } => {
                assert_eq!(room_id.as_str(), "room-fanout");
                assert_eq!(user_id.as_str(), "user-fanout");
                assert_eq!(username, "tester");
                assert_eq!(settings_json, br#"{"require_password":false}"#.to_vec());
                assert_eq!(version, 9);
            }
            other => panic!("expected RoomSettingsChanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cluster_room_settings_fanout_does_not_broadcast_locally_and_publishes_once() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_room_settings_fanout_service(
            default_cluster_fanout_service(Some(tx), true),
            Some(event_service.clone()),
        );

        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("cluster room settings fanout should reserve");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"require_password":true}"#.to_vec(),
            11,
        );

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );

        let request = rx.recv().await.expect("publish request should be queued");
        assert!(matches!(
            request.event,
            ClusterEvent::RoomSettingsChanged { .. }
        ));
    }

    #[tokio::test]
    async fn test_standalone_room_settings_fanout_does_not_broadcast_locally() {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_room_settings_fanout_service(
            default_cluster_fanout_service(None, false),
            Some(event_service.clone()),
        );

        let reservation = service
            .reserve_settings_changed()
            .await
            .expect("standalone room settings fanout should reserve");

        service.publish_settings_changed(
            reservation,
            &room_id(),
            &user_id(),
            "tester",
            br#"{"require_password":true}"#.to_vec(),
            11,
        );

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );
        assert!(
            event_service
                .local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .is_empty(),
            "standalone room settings fanout must rely on the room notification bridge instead of rebroadcasting locally"
        );
    }
}
