use parking_lot::Mutex;
use std::sync::Arc;
use synctv_core::models::{RoomPlaybackState, UserId};
use synctv_core::service::RealtimeOutboxPlaybackStateEventFactory;
use synctv_realtime::fanout::RealtimeFanoutService;
use synctv_realtime::sync::RealtimeEvent;

pub trait PlaybackFanoutService: Send + Sync {
    fn prepare_state_changed_outbox_fanout(
        &self,
        actor: PlaybackFanoutActor<'_>,
    ) -> PreparedPlaybackStateFanout;

    fn prepare_system_state_changed_outbox_fanout(&self) -> PreparedPlaybackStateFanout {
        self.prepare_state_changed_outbox_fanout(PlaybackFanoutActor::system())
    }

    fn prepare_system_state_changed_batch_outbox_fanout(&self) -> PreparedPlaybackStateBatchFanout;
}

#[derive(Debug, Clone, Copy)]
pub struct PlaybackFanoutActor<'a> {
    user_id: UserId,
    username: &'a str,
}

impl<'a> PlaybackFanoutActor<'a> {
    #[must_use]
    pub const fn new(user_id: UserId, username: &'a str) -> Self {
        Self { user_id, username }
    }

    #[must_use]
    pub fn system() -> Self {
        Self {
            user_id: system_user_id(),
            username: synctv_common::reserved::SYSTEM_USERNAME,
        }
    }
}

pub struct DefaultPlaybackFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultPlaybackFanoutService {
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self { realtime_fanout }
    }
}

impl std::fmt::Debug for DefaultPlaybackFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultPlaybackFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

impl PlaybackFanoutService for DefaultPlaybackFanoutService {
    fn prepare_state_changed_outbox_fanout(
        &self,
        actor: PlaybackFanoutActor<'_>,
    ) -> PreparedPlaybackStateFanout {
        let actor = OwnedPlaybackFanoutActor {
            user_id: actor.user_id,
            username: actor.username.to_string(),
        };
        PreparedPlaybackStateFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            actor,
            event: Arc::new(Mutex::new(None)),
        }
    }

    fn prepare_system_state_changed_batch_outbox_fanout(&self) -> PreparedPlaybackStateBatchFanout {
        PreparedPlaybackStateBatchFanout::new(
            self.realtime_fanout.clone(),
            OwnedPlaybackFanoutActor {
                user_id: system_user_id(),
                username: synctv_common::reserved::SYSTEM_USERNAME.to_string(),
            },
        )
    }
}

#[derive(Debug, Clone)]
struct OwnedPlaybackFanoutActor {
    user_id: UserId,
    username: String,
}

impl OwnedPlaybackFanoutActor {
    fn as_borrowed(&self) -> PlaybackFanoutActor<'_> {
        PlaybackFanoutActor::new(self.user_id, &self.username)
    }
}

#[derive(Clone)]
pub struct PreparedPlaybackStateFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    actor: OwnedPlaybackFanoutActor,
    event: Arc<Mutex<Option<RealtimeEvent>>>,
}

impl PreparedPlaybackStateFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxPlaybackStateEventFactory {
        self.outbox_factory_with_source_changed(false)
    }

    #[must_use]
    pub fn outbox_factory_with_source_changed(
        &self,
        source_changed: bool,
    ) -> RealtimeOutboxPlaybackStateEventFactory {
        let realtime_fanout = self.realtime_fanout.clone();
        let actor = self.actor.clone();
        let event_slot = self.event.clone();
        Arc::new(move |state: &RoomPlaybackState| {
            let event = playback_state_changed_event(actor.as_borrowed(), state, source_changed);
            *event_slot.lock() = Some(event.clone());
            realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self.event.lock().take() {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedPlaybackStateBatchFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    actor: OwnedPlaybackFanoutActor,
    events: Arc<Mutex<Vec<RealtimeEvent>>>,
}

impl PreparedPlaybackStateBatchFanout {
    fn new(
        realtime_fanout: Arc<dyn RealtimeFanoutService>,
        actor: OwnedPlaybackFanoutActor,
    ) -> Self {
        Self {
            realtime_fanout,
            actor,
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxPlaybackStateEventFactory {
        self.outbox_factory_with_source_changed(false)
    }

    #[must_use]
    pub fn outbox_factory_with_source_changed(
        &self,
        source_changed: bool,
    ) -> RealtimeOutboxPlaybackStateEventFactory {
        let realtime_fanout = self.realtime_fanout.clone();
        let actor = self.actor.clone();
        let events = self.events.clone();
        Arc::new(move |state: &RoomPlaybackState| {
            let event = playback_state_changed_event(actor.as_borrowed(), state, source_changed);
            events.lock().push(event.clone());
            realtime_fanout
                .outbox_event(&event)
                .map_err(synctv_core::Error::Internal)
        })
    }

    pub fn publish_after_outbox_commit(&self) {
        let events = std::mem::take(&mut *self.events.lock());
        for event in events {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

fn playback_state_changed_event(
    actor: PlaybackFanoutActor<'_>,
    state: &RoomPlaybackState,
    source_changed: bool,
) -> RealtimeEvent {
    RealtimeEvent::PlaybackStateChanged {
        event_id: synctv_common::snanoid!(16),
        room_id: state.room_id,
        user_id: actor.user_id,
        username: actor.username.to_string(),
        state: state.clone(),
        source_changed,
        timestamp: synctv_core::SystemClock.now(),
    }
}

fn system_user_id() -> UserId {
    UserId::MAX
}

#[must_use]
pub fn default_playback_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn PlaybackFanoutService> {
    Arc::new(DefaultPlaybackFanoutService::new(realtime_fanout))
}

#[cfg(test)]
mod tests {
    use super::{default_playback_fanout_service, PlaybackFanoutActor};
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use synctv_core::models::{MediaId, RoomId, RoomPlaybackState, UserId};
    use synctv_core::service::NewRealtimeOutboxEvent;
    use synctv_realtime::fanout::RealtimeFanoutService;
    use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[derive(Default)]
    struct RecordingRealtimeFanout {
        published: AtomicUsize,
        events: Mutex<Vec<RealtimeEvent>>,
    }

    #[async_trait]
    impl RealtimeFanoutService for RecordingRealtimeFanout {
        async fn try_publish(&self, request: PublishRequest) -> bool {
            self.published.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut events) = self.events.lock() {
                *events = vec![request.event];
            }
            true
        }

        fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
            Ok(NewRealtimeOutboxEvent {
                id: event.event_id().to_string(),
                enqueue_outbox: false,
                aggregate_type: "room_playback_state".to_string(),
                aggregate_id: "160001".to_string(),
                event_type: "playback_state_changed".to_string(),
                event_version: 1,
                aggregate_version: Some(7),
                payload: event.clone(),
            })
        }

        fn publish_after_outbox_commit(&self, event: RealtimeEvent) {
            self.published.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut events) = self.events.lock() {
                events.push(event);
            }
        }

        fn is_distributed_enabled(&self) -> bool {
            false
        }

        fn accepts_immediate_publish(&self) -> bool {
            true
        }
    }

    fn playback_state_with_version(version: i64) -> RoomPlaybackState {
        RoomPlaybackState {
            room_id: RoomId::expect_positive(160_001),
            playing_media_id: Some(MediaId::expect_positive(160_002)),
            playing_playlist_id: None,
            target: None,
            current_progress_id: None,
            position: 12.5,
            speed: 1.0,
            is_playing: true,
            playback_generation: 0,
            updated_at: synctv_core::SystemClock.now(),
            version,
        }
    }

    fn playback_state() -> RoomPlaybackState {
        playback_state_with_version(7)
    }

    #[tokio::test]
    async fn test_playback_fanout_publishes_prepared_state_changed_event_after_commit() -> TestResult
    {
        let realtime = Arc::new(RecordingRealtimeFanout::default());
        let service = default_playback_fanout_service(realtime.clone());
        let actor = PlaybackFanoutActor::new(UserId::expect_positive(160_003), "alice");
        let prepared = service.prepare_state_changed_outbox_fanout(actor);
        let factory = prepared.outbox_factory();

        let outbox_event = factory(&playback_state())?;
        assert!(!outbox_event.enqueue_outbox);
        prepared.publish_after_outbox_commit();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if realtime.published.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;

        let event = realtime
            .events
            .lock()
            .map_err(|_| test_error("event mutex should not be poisoned"))?
            .first()
            .cloned()
            .ok_or_else(|| test_error("published event should be recorded"))?;
        match event {
            RealtimeEvent::PlaybackStateChanged {
                user_id,
                username,
                state,
                ..
            } => {
                assert_eq!(user_id, UserId::expect_positive(160_003));
                assert_eq!(username, "alice");
                assert_eq!(state.version, 7);
            }
            other => {
                return Err(test_error(format!(
                    "expected PlaybackStateChanged, got {other:?}"
                )))
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_playback_batch_fanout_publishes_all_prepared_events_after_commit() -> TestResult {
        let realtime = Arc::new(RecordingRealtimeFanout::default());
        let service = default_playback_fanout_service(realtime.clone());
        let prepared = service.prepare_system_state_changed_batch_outbox_fanout();
        let factory = prepared.outbox_factory();

        let first_event = factory(&playback_state_with_version(7))?;
        let second_event = factory(&playback_state_with_version(8))?;
        assert_eq!(first_event.event_type, "playback_state_changed");
        assert_eq!(second_event.event_type, "playback_state_changed");
        assert_eq!(realtime.published.load(Ordering::SeqCst), 0);

        prepared.publish_after_outbox_commit();

        assert_eq!(realtime.published.load(Ordering::SeqCst), 2);
        let versions = realtime
            .events
            .lock()
            .map_err(|_| test_error("event mutex should not be poisoned"))?
            .iter()
            .map(|event| match event {
                RealtimeEvent::PlaybackStateChanged { state, .. } => Ok(state.version),
                other => Err(test_error(format!(
                    "expected PlaybackStateChanged, got {other:?}"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(versions, vec![7, 8]);
        Ok(())
    }
}
