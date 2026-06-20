use std::sync::Arc;
use synctv_core::models::Media;
use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::{RealtimeOutboxMediaBatchEventFactory, RealtimeOutboxMediaEventFactory};
use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

use crate::realtime_fanout::{
    publish_best_effort, PreparedOutboxFanout, PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};

type MediaBatchEventsBuilder = Arc<dyn Fn(&[Media]) -> Vec<RealtimeEvent> + Send + Sync>;

#[derive(Clone)]
pub struct PreparedMediaRemovedFanout {
    plan: PreparedRealtimeFanoutPlan,
}

impl PreparedMediaRemovedFanout {
    #[must_use]
    pub fn event(&self) -> &RealtimeEvent {
        self.plan.event()
    }

    #[must_use]
    pub fn cloned_outbox_event(&self) -> NewRealtimeOutboxEvent {
        self.plan.cloned_outbox_event()
    }

    pub fn publish_after_outbox_commit(self) {
        self.plan.publish_after_outbox_commit();
    }
}

#[derive(Clone)]
pub struct PreparedMediaOutboxFanout {
    prepared: PreparedOutboxFanout<Media>,
}

impl PreparedMediaOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxMediaEventFactory {
        self.prepared.outbox_factory()
    }

    pub fn publish_after_outbox_commit(&self) {
        self.prepared.publish_after_outbox_commit();
    }
}

#[derive(Clone)]
pub struct PreparedMediaBatchOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    events_builder: MediaBatchEventsBuilder,
    events: Arc<parking_lot::Mutex<Vec<RealtimeEvent>>>,
}

impl PreparedMediaBatchOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxMediaBatchEventFactory {
        let prepared = self.clone();
        Arc::new(move |media: &[Media]| {
            let events = (prepared.events_builder)(media);
            prepared.events.lock().clone_from(&events);
            if !prepared.realtime_fanout.is_distributed_enabled() {
                return Ok(Vec::new());
            }
            events
                .iter()
                .map(|event| {
                    prepared
                        .realtime_fanout
                        .outbox_event(event)
                        .map_err(synctv_core::Error::Internal)
                })
                .collect::<synctv_core::Result<Vec<_>>>()
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(&mut *self.events.lock());
        for event in events {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

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
    ) -> synctv_core::Result<PreparedMediaRemovedFanout>;

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
}

pub struct DefaultMediaFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultMediaFanoutService {
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
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

impl MediaFanoutService for DefaultMediaFanoutService {
    fn publish_added(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        let event = media_added_event(room_id, user_id, username, media_id, media_title);
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest::new(event));
    }

    fn publish_removed(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) {
        let event = media_removed_event(room_id, user_id, username, media_id);
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest::new(event));
    }

    fn publish_updated(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
        media_title: &str,
    ) {
        let event = media_updated_event(room_id, user_id, username, media_id, media_title);
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest::new(event));
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
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest::new(event));
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
        publish_best_effort(self.realtime_fanout.clone(), PublishRequest::new(event));
    }

    fn prepare_removed_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: &MediaId,
    ) -> synctv_core::Result<PreparedMediaRemovedFanout> {
        let event = media_removed_event(room_id, user_id, username, media_id);
        Ok(PreparedMediaRemovedFanout {
            plan: PreparedRealtimeFanoutPlan::new(self.realtime_fanout.clone(), event)
                .map_err(synctv_core::Error::Internal)?,
        })
    }

    fn prepare_added_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout {
        PreparedMediaOutboxFanout {
            prepared: PreparedOutboxFanout::new(
                self.realtime_fanout.clone(),
                move |media: &Media| {
                    media_added_event(&room_id, &user_id, &username, &media.id, &media.name)
                },
            ),
        }
    }

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedMediaOutboxFanout {
        PreparedMediaOutboxFanout {
            prepared: PreparedOutboxFanout::new(
                self.realtime_fanout.clone(),
                move |media: &Media| {
                    media_updated_event(&room_id, &user_id, &username, &media.id, &media.name)
                },
            ),
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
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
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
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
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
) -> Arc<dyn MediaFanoutService> {
    Arc::new(DefaultMediaFanoutService::new(realtime_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_media_fanout_service;
    use crate::realtime_fanout::local_realtime_fanout_service;
    use crate::test_support::{channel_realtime_fanout_service, RecordingRealtimeEventService};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use synctv_core::models::{Media, MediaId, RoomId, UserId};
    use synctv_realtime::sync::RealtimeEvent;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    async fn recv_request(
        rx: &mut tokio::sync::mpsc::Receiver<synctv_realtime::sync::PublishRequest>,
    ) -> TestResult<synctv_realtime::sync::PublishRequest> {
        rx.recv()
            .await
            .ok_or_else(|| test_error("publish request should be queued"))
    }

    fn recorded_room_events(
        service: &RecordingRealtimeEventService,
    ) -> TestResult<Vec<(String, RealtimeEvent)>> {
        service
            .room_events
            .lock()
            .map(|events| events.clone())
            .map_err(|_| test_error("recorded room events mutex was poisoned"))
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
            description: String::new(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({ "url": "https://example.test/video.mp4" }),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        }
    }

    #[tokio::test]
    async fn test_media_fanout_publishes_media_added_event() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx));
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        let request = recv_request(&mut rx).await?;
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
            other => return Err(test_error(format!("expected MediaAdded, got {other:?}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_cluster_media_fanout_does_not_broadcast_locally_and_publishes_once() -> TestResult
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx));
        service.publish_added(&room_id(), &user_id(), "tester", &media_id(), "demo");

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 0);

        let request = recv_request(&mut rx).await?;
        assert!(matches!(request.event, RealtimeEvent::MediaAdded { .. }));
        assert!(
            rx.try_recv().is_err(),
            "cluster media add should publish exactly one Redis event"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_standalone_media_fanout_broadcasts_locally() -> TestResult {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service =
            default_media_fanout_service(local_realtime_fanout_service(event_service.clone()));
        service.publish_reordered(&room_id(), &user_id(), "tester", vec![media_id()]);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while event_service.room_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| test_error("standalone media fanout should broadcast locally"))?;

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        assert_eq!(event_service.room_event_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_cluster_media_fanout_publishes_playlist_reordered_event() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx));
        service.publish_reordered(&room_id(), &user_id(), "tester", vec![media_id()]);

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 0);

        let request = recv_request(&mut rx).await?;
        assert!(matches!(
            request.event,
            RealtimeEvent::PlaylistReordered { .. }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_prepared_media_added_fanout_publishes_committed_event() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let service = default_media_fanout_service(channel_realtime_fanout_service(tx));
        let prepared =
            service.prepare_added_outbox_fanout(room_id(), user_id(), "tester".to_string());
        let factory = prepared.outbox_factory();

        let event = core_ok(factory(&media()))?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        let request = recv_request(&mut rx).await?;
        assert!(matches!(request.event, RealtimeEvent::MediaAdded { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn test_standalone_prepared_media_added_fanout_broadcasts_committed_event() -> TestResult
    {
        let event_service = Arc::new(RecordingRealtimeEventService::default());
        let service =
            default_media_fanout_service(local_realtime_fanout_service(event_service.clone()));
        let prepared =
            service.prepare_added_outbox_fanout(room_id(), user_id(), "tester".to_string());
        let factory = prepared.outbox_factory();

        let event = core_ok(factory(&media()))?;
        assert!(!event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        assert_eq!(event_service.broadcast_calls.load(Ordering::SeqCst), 0);
        assert_eq!(event_service.room_calls.load(Ordering::SeqCst), 1);
        let events = recorded_room_events(&event_service)?;
        assert!(matches!(events[0].1, RealtimeEvent::MediaAdded { .. }));
        Ok(())
    }
}
