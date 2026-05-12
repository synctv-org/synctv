use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::models::Media;
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::{
    RealtimeOutboxMediaBatchEventFactory, RealtimeOutboxMediaEventFactory,
    RealtimeOutboxMediaIdsEventFactory,
};
use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

use crate::realtime_fanout::{publish_best_effort, RealtimeFanoutService};
use crate::runtime::RealtimeEventService;

#[derive(Clone)]
pub struct PreparedMediaRemovedFanout {
    pub event: RealtimeEvent,
    pub outbox_event: Option<NewRealtimeOutboxEvent>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl PreparedMediaRemovedFanout {
    pub fn publish_after_outbox_commit(self) {
        self.realtime_fanout.publish_after_outbox_commit(self.event);
    }
}

#[derive(Clone)]
pub struct PreparedMediaOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_builder: Arc<dyn Fn(&Media) -> RealtimeEvent + Send + Sync>,
    event: Arc<std::sync::Mutex<Option<RealtimeEvent>>>,
}

impl PreparedMediaOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<RealtimeOutboxMediaEventFactory> {
        if !self.realtime_fanout.is_distributed_enabled() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |media: &Media| {
            let event = (prepared.event_builder)(media);
            *prepared
                .event
                .lock()
                .expect("media fanout event mutex should not be poisoned") = Some(event.clone());
            prepared.realtime_fanout.outbox_event(&event)
        }))
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self
            .event
            .lock()
            .expect("media fanout event mutex should not be poisoned")
            .take()
        {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedMediaBatchOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    events_builder: Arc<dyn Fn(&[Media]) -> Vec<RealtimeEvent> + Send + Sync>,
    events: Arc<std::sync::Mutex<Vec<RealtimeEvent>>>,
}

impl PreparedMediaBatchOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<RealtimeOutboxMediaBatchEventFactory> {
        if !self.realtime_fanout.is_distributed_enabled() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |media: &[Media]| {
            let events = (prepared.events_builder)(media);
            let outbox_events = events
                .iter()
                .filter_map(|event| prepared.realtime_fanout.outbox_event(event))
                .collect();
            *prepared
                .events
                .lock()
                .expect("media fanout events mutex should not be poisoned") = events;
            outbox_events
        }))
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(
            &mut *self
                .events
                .lock()
                .expect("media fanout events mutex should not be poisoned"),
        );
        for event in events {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedMediaIdsOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    events_builder: Arc<dyn Fn(&[MediaId]) -> Vec<RealtimeEvent> + Send + Sync>,
    events: Arc<std::sync::Mutex<Vec<RealtimeEvent>>>,
}

impl PreparedMediaIdsOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<RealtimeOutboxMediaIdsEventFactory> {
        if !self.realtime_fanout.is_distributed_enabled() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |media_ids: &[MediaId]| {
            let events = (prepared.events_builder)(media_ids);
            let outbox_events = events
                .iter()
                .filter_map(|event| prepared.realtime_fanout.outbox_event(event))
                .collect();
            *prepared
                .events
                .lock()
                .expect("media fanout events mutex should not be poisoned") = events;
            outbox_events
        }))
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(
            &mut *self
                .events
                .lock()
                .expect("media fanout events mutex should not be poisoned"),
        );
        for event in events {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
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

    fn prepare_added_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout;

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout;

    fn prepare_added_batch_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaBatchOutboxFanout;

    fn prepare_move_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        plan: crate::impls::client::media::MoveMediaFanoutPlan,
    ) -> PreparedMediaBatchOutboxFanout;

    fn prepare_removed_batch_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaIdsOutboxFanout;
}

pub struct DefaultMediaFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultMediaFanoutService {
    #[must_use]
    pub fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        _event_service: Option<Arc<dyn RealtimeEventService>>,
    ) -> Self {
        Self { realtime_fanout }
    }
}

impl std::fmt::Debug for DefaultMediaFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultMediaFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
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
        let event = RealtimeEvent::MediaAdded {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            media_title: media_title.to_string(),
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest { event });
    }

    fn publish_removed(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) {
        let event = RealtimeEvent::MediaRemoved {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest { event });
    }

    fn publish_updated(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        let event = RealtimeEvent::MediaUpdated {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_id: *media_id,
            media_title: media_title.to_string(),
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest { event });
    }

    fn publish_removed_batch(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    ) {
        let event = RealtimeEvent::MediaRemovedBatch {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_ids,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest { event });
    }

    fn publish_reordered(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_ids: Vec<MediaId>,
    ) {
        let event = RealtimeEvent::PlaylistReordered {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.to_string(),
            media_ids,
            timestamp: chrono::Utc::now(),
        };
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest { event });
    }

    fn prepare_removed_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) -> PreparedMediaRemovedFanout {
        let event = media_removed_event(room_id, user_id, username, media_id);
        let outbox_event = self.realtime_fanout.outbox_event(&event);
        PreparedMediaRemovedFanout {
            event,
            outbox_event,
            realtime_fanout: self.realtime_fanout.clone(),
        }
    }

    fn prepare_added_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout {
        PreparedMediaOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            event_builder: Arc::new(move |media: &Media| {
                media_added_event(&room_id, &user_id, &username, &media.id, &media.name)
            }),
            event: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout {
        PreparedMediaOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            event_builder: Arc::new(move |media: &Media| {
                media_updated_event(&room_id, &user_id, &username, &media.id, &media.name)
            }),
            event: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn prepare_added_batch_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaBatchOutboxFanout {
        PreparedMediaBatchOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            events_builder: Arc::new(move |media: &[Media]| {
                media
                    .iter()
                    .map(|media| {
                        media_added_event(&room_id, &user_id, &username, &media.id, &media.name)
                    })
                    .collect()
            }),
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn prepare_move_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        plan: crate::impls::client::media::MoveMediaFanoutPlan,
    ) -> PreparedMediaBatchOutboxFanout {
        PreparedMediaBatchOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            events_builder: Arc::new(move |moved_media: &[Media]| {
                move_media_events(&plan, &room_id, &user_id, &username, moved_media)
            }),
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn prepare_removed_batch_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaIdsOutboxFanout {
        PreparedMediaIdsOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            events_builder: Arc::new(move |media_ids: &[MediaId]| {
                if media_ids.is_empty() {
                    Vec::new()
                } else {
                    vec![media_removed_batch_event(
                        &room_id,
                        &user_id,
                        &username,
                        media_ids.to_vec(),
                    )]
                }
            }),
            events: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

fn media_added_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    media_id: &MediaId,
    media_title: &str,
) -> RealtimeEvent {
    RealtimeEvent::MediaAdded {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        media_id: *media_id,
        media_title: media_title.to_string(),
        timestamp: chrono::Utc::now(),
    }
}

fn media_updated_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    media_id: &MediaId,
    media_title: &str,
) -> RealtimeEvent {
    RealtimeEvent::MediaUpdated {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        media_id: *media_id,
        media_title: media_title.to_string(),
        timestamp: chrono::Utc::now(),
    }
}

fn media_removed_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    media_id: &MediaId,
) -> RealtimeEvent {
    RealtimeEvent::MediaRemoved {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        media_id: *media_id,
        timestamp: chrono::Utc::now(),
    }
}

fn media_removed_batch_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    media_ids: Vec<MediaId>,
) -> RealtimeEvent {
    RealtimeEvent::MediaRemovedBatch {
        event_id: synctv_common::snanoid!(16),
        room_id: *room_id,
        user_id: *user_id,
        username: username.to_string(),
        media_ids,
        timestamp: chrono::Utc::now(),
    }
}

fn move_media_events(
    plan: &crate::impls::client::media::MoveMediaFanoutPlan,
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    moved_media: &[Media],
) -> Vec<RealtimeEvent> {
    match plan {
        crate::impls::client::media::MoveMediaFanoutPlan::None => Vec::new(),
        crate::impls::client::media::MoveMediaFanoutPlan::Reordered => {
            vec![RealtimeEvent::PlaylistReordered {
                event_id: synctv_common::snanoid!(16),
                room_id: *room_id,
                user_id: *user_id,
                username: username.to_string(),
                media_ids: moved_media.iter().map(|media| media.id).collect(),
                timestamp: chrono::Utc::now(),
            }]
        }
        crate::impls::client::media::MoveMediaFanoutPlan::PerMedia(steps) => {
            let moved_by_id: std::collections::HashMap<MediaId, &Media> =
                moved_media.iter().map(|media| (media.id, media)).collect();
            let mut events = Vec::new();
            for step in steps {
                match step {
                    crate::impls::client::media::MoveMediaFanoutStep::Updated { media_id } => {
                        if let Some(media) = moved_by_id.get(media_id) {
                            events.push(media_updated_event(
                                room_id,
                                user_id,
                                username,
                                &media.id,
                                &media.name,
                            ));
                        }
                    }
                    crate::impls::client::media::MoveMediaFanoutStep::RemovedAndAdded {
                        media_id,
                    } => {
                        events.push(media_removed_event(room_id, user_id, username, media_id));
                        if let Some(media) = moved_by_id.get(media_id) {
                            events.push(media_added_event(
                                room_id,
                                user_id,
                                username,
                                &media.id,
                                &media.name,
                            ));
                        }
                    }
                }
            }
            events
        }
    }
}

#[must_use]
pub fn default_media_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
) -> Arc<dyn MediaFanoutService> {
    Arc::new(DefaultMediaFanoutService::new(
        realtime_fanout,
        event_service,
    ))
}

#[cfg(test)]
mod tests {
    use super::default_media_fanout_service;
    use crate::realtime_fanout::default_realtime_fanout_service;
    use crate::runtime::{RealtimeEventService, RealtimeMetrics};
    use crate::test_support::channel_realtime_fanout_service;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use synctv_core::models::{Media, MediaId, RoomId, UserId};
    use synctv_realtime::sync::{BroadcastResult, ConnectionId, RealtimeEvent};
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        broadcast_calls: AtomicUsize,
        broadcast_local_calls: AtomicUsize,
        local_events: Mutex<Vec<(String, RealtimeEvent)>>,
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
            panic!("subscribe_with_id should not be called in media fanout tests");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in media fanout tests");
        }

        fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
            self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
            BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: RealtimeEvent) -> bool {
            panic!("publish_only should not be called in media fanout tests");
        }

        fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
            self.broadcast_local_calls.fetch_add(1, Ordering::SeqCst);
            self.local_events
                .lock()
                .expect("recorded local events mutex should not be poisoned")
                .push((room_id.to_string(), event.clone()));
            1
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
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
        RoomId::expect_positive(106_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(106_002)
    }

    fn media_id() -> MediaId {
        MediaId::expect_positive(106_003)
    }

    fn media() -> Media {
        Media {
            id: media_id(),
            playlist_id: None,
            room_id: room_id(),
            creator_id: Some(user_id()),
            name: "demo".to_string(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({ "url": "https://example.test/video.mp4" }),
            provider_instance_name: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        }
    }

    #[tokio::test]
    async fn test_media_fanout_publishes_media_added_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx), None);
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        let request = rx.recv().await.expect("publish request should be queued");
        match request.event {
            RealtimeEvent::MediaAdded {
                room_id,
                user_id,
                username,
                media_id,
                media_title,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(106_001));
                assert_eq!(user_id, UserId::expect_positive(106_002));
                assert_eq!(username, "tester");
                assert_eq!(media_id, MediaId::expect_positive(106_003));
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
            channel_realtime_fanout_service(tx),
            Some(event_service.clone()),
        );
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            event_service.broadcast_local_calls.load(Ordering::SeqCst),
            0
        );

        let request = rx.recv().await.expect("publish request should be queued");
        assert!(matches!(request.event, RealtimeEvent::MediaAdded { .. }));
        assert!(
            rx.try_recv().is_err(),
            "cluster media add should publish exactly one Redis event"
        );
    }

    #[tokio::test]
    async fn test_standalone_media_fanout_does_not_broadcast_locally() {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(
            default_realtime_fanout_service(None, false),
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
            channel_realtime_fanout_service(tx),
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
            RealtimeEvent::PlaylistReordered { .. }
        ));
    }

    #[tokio::test]
    async fn test_prepared_media_added_fanout_publishes_committed_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx), None);
        let prepared =
            service.prepare_added_outbox_fanout(room_id(), user_id(), "tester".to_string());
        let factory = prepared
            .outbox_factory()
            .expect("distributed realtime fanout should prepare an outbox factory");

        assert!(
            factory(&media()).is_none(),
            "test channel fanout does not provide persistent outbox rows"
        );
        prepared.publish_after_outbox_commit();

        let request = rx
            .recv()
            .await
            .expect("committed media add event should be published");
        assert!(matches!(request.event, RealtimeEvent::MediaAdded { .. }));
    }
}
