use async_trait::async_trait;
use std::sync::Arc;
use synctv_cluster::sync::{ClusterEvent, PublishRequest};
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::repository::cluster_outbox::NewClusterOutboxEvent;

use crate::cluster_fanout::{publish_best_effort, ClusterFanoutService};
use crate::runtime::RealtimeEventService;

#[derive(Clone)]
pub struct PreparedMediaRemovedFanout {
    pub event: ClusterEvent,
    pub outbox_event: Option<NewClusterOutboxEvent>,
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl PreparedMediaRemovedFanout {
    pub fn publish_after_outbox_commit(self) {
        self.cluster_fanout.publish_after_outbox_commit(self.event);
    }
}

#[async_trait]
pub trait MediaFanoutService: Send + Sync {
    fn publish_added(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    );

    fn publish_removed(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    );

    fn publish_updated(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    );

    fn publish_removed_batch(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    );

    fn publish_reordered(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    );

    fn prepare_removed_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) -> PreparedMediaRemovedFanout;
}

pub struct DefaultMediaFanoutService {
    cluster_fanout: Arc<dyn ClusterFanoutService>,
}

impl DefaultMediaFanoutService {
    #[must_use]
    pub fn new(
        cluster_fanout: Arc<dyn ClusterFanoutService>,
        _event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self { cluster_fanout }
    }
}

impl std::fmt::Debug for DefaultMediaFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMediaFanoutService")
            .field(
                "cluster_fanout_distributed",
                &self.cluster_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl MediaFanoutService for DefaultMediaFanoutService {
    fn publish_added(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        let event = ClusterEvent::MediaAdded {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            media_title: media_title.to_string(),
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.cluster_fanout.clone(), PublishRequest { event });
    }

    fn publish_removed(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) {
        let event = ClusterEvent::MediaRemoved {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.cluster_fanout.clone(), PublishRequest { event });
    }

    fn publish_updated(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        let event = ClusterEvent::MediaUpdated {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            media_title: media_title.to_string(),
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.cluster_fanout.clone(), PublishRequest { event });
    }

    fn publish_removed_batch(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    ) {
        let event = ClusterEvent::MediaRemovedBatch {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_ids,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.cluster_fanout.clone(), PublishRequest { event });
    }

    fn publish_reordered(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    ) {
        let event = ClusterEvent::PlaylistReordered {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_ids,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.cluster_fanout.clone(), PublishRequest { event });
    }

    fn prepare_removed_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) -> PreparedMediaRemovedFanout {
        let event = media_removed_event(room_id, user_id, username, media_id);
        let outbox_event = self.cluster_fanout.outbox_event(&event);
        PreparedMediaRemovedFanout {
            event,
            outbox_event,
            cluster_fanout: self.cluster_fanout.clone(),
        }
    }
}

fn media_removed_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    media_id: &MediaId,
) -> ClusterEvent {
    ClusterEvent::MediaRemoved {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        media_id: *media_id,
        timestamp: chrono::Utc::now(),
    }
}

#[must_use]
pub fn default_media_fanout_service(
    cluster_fanout: Arc<dyn ClusterFanoutService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn MediaFanoutService> {
    Arc::new(DefaultMediaFanoutService::new(
        cluster_fanout,
        event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_media_fanout_service;
    use crate::cluster_fanout::default_cluster_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use crate::test_support::channel_cluster_fanout_service;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use synctv_cluster::sync::{BroadcastResult, ClusterEvent, ConnectionId};
    use synctv_core::models::{MediaId, RoomId, UserId};
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
            panic!("subscribe_with_id should not be called in media fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in media fanout tests");
        }

        fn broadcast(&self, _event: ClusterEvent) -> BroadcastResult {
            self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
            BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: ClusterEvent) -> bool {
            panic!("publish_only should not be called in media fanout tests");
        }

        fn broadcast_local(&self, room_id: &RoomId, event: &ClusterEvent) -> usize {
            self.broadcast_local_calls.fetch_add(1, Ordering::SeqCst);
            self.local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .push((room_id.to_string(), event.clone()));
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
            panic!("subscribe_admin_events should not be called in media fanout tests");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: true,
            }
        }

        fn node_id(&self) -> &'static str {
            "media-fanout-test-node"
        }

        async fn shutdown(&self) {}
    }

    fn room_id() -> RoomId {
        RoomId::from(106_001)
    }

    fn user_id() -> UserId {
        UserId::from(106_002)
    }

    fn media_id() -> MediaId {
        MediaId::from(106_003)
    }

    #[tokio::test]
    async fn test_media_fanout_publishes_media_added_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(channel_cluster_fanout_service(tx), None);
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            ClusterEvent::MediaAdded {
                room_id,
                user_id,
                username,
                media_id,
                media_title,
                ..
            } => {
                assert_eq!(room_id, RoomId::from(106_001));
                assert_eq!(user_id, UserId::from(106_002));
                assert_eq!(username, "tester");
                assert_eq!(media_id, MediaId::from(106_003));
                assert_eq!(media_title, "demo");
            }
            other => panic!("expected MediaAdded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cluster_media_fanout_does_not_broadcast_locally_and_publishes_once() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(
            channel_cluster_fanout_service(tx),
            Some(event_service.clone()),
        );
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );

        let request = rx.recv().await.expect("publish request should be queued");
        assert!(matches!(request.event, ClusterEvent::MediaAdded { .. }));
        assert!(
            rx.try_recv().is_err(),
            "cluster media add should publish exactly one Redis event"
        );
    }

    #[tokio::test]
    async fn test_standalone_media_fanout_does_not_broadcast_locally() {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(
            default_cluster_fanout_service(None, false),
            Some(event_service.clone()),
        );
        service.publish_reordered(&room_id(), &user_id(), "tester", vec![media_id()]);

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
            "standalone media fanout must rely on the room notification bridge instead of rebroadcasting locally"
        );
    }

    #[tokio::test]
    async fn test_cluster_media_fanout_publishes_playlist_reordered_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(
            channel_cluster_fanout_service(tx),
            Some(event_service.clone()),
        );
        service.publish_reordered(&room_id(), &user_id(), "tester", vec![media_id()]);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );

        let request = rx.recv().await.expect("publish request should be queued");
        assert!(matches!(
            request.event,
            ClusterEvent::PlaylistReordered { .. }
        ));
    }
}
