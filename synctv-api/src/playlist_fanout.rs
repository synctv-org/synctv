use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::RealtimeOutboxPlaylistEventFactory;
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::RealtimeFanoutService;

#[derive(Clone)]
pub struct PreparedPlaylistOutboxFanout {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    event_builder: Arc<dyn Fn(&Playlist) -> RealtimeEvent + Send + Sync>,
    event: Arc<std::sync::Mutex<Option<RealtimeEvent>>>,
}

impl PreparedPlaylistOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> Option<RealtimeOutboxPlaylistEventFactory> {
        if !self.realtime_fanout.is_distributed_enabled() {
            return None;
        }

        let prepared = self.clone();
        Some(Arc::new(move |playlist: &Playlist| {
            let event = (prepared.event_builder)(playlist);
            *prepared
                .event
                .lock()
                .expect("playlist fanout event mutex should not be poisoned") = Some(event.clone());
            prepared.realtime_fanout.outbox_event(&event)
        }))
    }

    pub fn publish_after_outbox_commit(&self) {
        if let Some(event) = self
            .event
            .lock()
            .expect("playlist fanout event mutex should not be poisoned")
            .take()
        {
            self.realtime_fanout.publish_after_outbox_commit(event);
        }
    }
}

#[derive(Clone)]
pub struct PreparedPlaylistDeletedFanout {
    pub event: RealtimeEvent,
    pub outbox_event: Option<NewRealtimeOutboxEvent>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl PreparedPlaylistDeletedFanout {
    pub fn publish_after_outbox_commit(self) {
        self.realtime_fanout.publish_after_outbox_commit(self.event);
    }
}

#[async_trait]
pub trait PlaylistFanoutService: Send + Sync {
    fn prepare_created_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout;

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout;

    fn prepare_deleted_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist_id: &PlaylistId,
    ) -> PreparedPlaylistDeletedFanout;
}

pub struct DefaultPlaylistFanoutService {
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
}

impl DefaultPlaylistFanoutService {
    #[must_use]
    pub fn new(realtime_fanout: Arc<dyn RealtimeFanoutService>) -> Self {
        Self { realtime_fanout }
    }
}

impl std::fmt::Debug for DefaultPlaylistFanoutService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultPlaylistFanoutService")
            .field(
                "realtime_fanout_distributed",
                &self.realtime_fanout.is_distributed_enabled(),
            )
            .finish()
    }
}

#[async_trait]
impl PlaylistFanoutService for DefaultPlaylistFanoutService {
    fn prepare_created_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout {
        PreparedPlaylistOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            event_builder: Arc::new(move |playlist: &Playlist| RealtimeEvent::PlaylistCreated {
                event_id: synctv_common::snanoid!(16),
                room_id,
                user_id,
                username: username.clone(),
                playlist: playlist.clone(),
                timestamp: chrono::Utc::now(),
            }),
            event: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout {
        PreparedPlaylistOutboxFanout {
            realtime_fanout: self.realtime_fanout.clone(),
            event_builder: Arc::new(move |playlist: &Playlist| RealtimeEvent::PlaylistUpdated {
                event_id: synctv_common::snanoid!(16),
                room_id,
                user_id,
                username: username.clone(),
                playlist: playlist.clone(),
                timestamp: chrono::Utc::now(),
            }),
            event: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn prepare_deleted_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist_id: &PlaylistId,
    ) -> PreparedPlaylistDeletedFanout {
        let event = playlist_deleted_event(room_id, user_id, username, playlist_id);
        let outbox_event = self.realtime_fanout.outbox_event(&event);
        PreparedPlaylistDeletedFanout {
            event,
            outbox_event,
            realtime_fanout: self.realtime_fanout.clone(),
        }
    }
}

fn playlist_deleted_event(
    room_id: &RoomId,
    user_id: &UserId,
    username: &str,
    playlist_id: &PlaylistId,
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
pub fn default_playlist_fanout_service(
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
) -> Arc<dyn PlaylistFanoutService> {
    Arc::new(DefaultPlaylistFanoutService::new(realtime_fanout))
}

#[cfg(test)]
mod tests {
    use super::default_playlist_fanout_service;
    use crate::realtime_fanout::RealtimeFanoutService;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};
    use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
    use synctv_realtime::sync::{PublishRequest, RealtimeEvent};

    #[derive(Default)]
    struct RecordingRealtimeFanout {
        committed_publish_count: AtomicUsize,
    }

    #[async_trait]
    impl RealtimeFanoutService for RecordingRealtimeFanout {
        async fn try_publish(&self, _request: PublishRequest) -> bool {
            false
        }

        fn outbox_event(&self, event: &RealtimeEvent) -> Option<NewRealtimeOutboxEvent> {
            Some(NewRealtimeOutboxEvent {
                id: event.event_id().to_string(),
                aggregate_type: "playlist".to_string(),
                aggregate_id: event
                    .room_id()
                    .map_or_else(|| "global".to_string(), std::string::ToString::to_string),
                event_type: event.event_type().to_string(),
                event_version: 1,
                aggregate_version: None,
                payload: serde_json::to_value(event)
                    .expect("realtime event serialization should not fail"),
            })
        }

        fn publish_after_outbox_commit(&self, _event: RealtimeEvent) {
            self.committed_publish_count.fetch_add(1, Ordering::SeqCst);
        }

        fn is_distributed_enabled(&self) -> bool {
            true
        }
    }

    fn room_id() -> RoomId {
        RoomId::expect_positive(105_001)
    }

    fn user_id() -> UserId {
        UserId::expect_positive(105_002)
    }

    fn playlist() -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(105_003),
            room_id: room_id(),
            creator_id: Some(user_id()),
            name: "fanout playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        }
    }

    #[tokio::test]
    async fn test_playlist_fanout_prepares_created_outbox_without_local_publish() {
        let realtime_fanout = Arc::new(RecordingRealtimeFanout::default());
        let service = default_playlist_fanout_service(realtime_fanout.clone());

        let playlist = playlist();
        let prepared =
            service.prepare_created_outbox_fanout(room_id(), user_id(), "tester".to_string());
        let factory = prepared.outbox_factory();
        assert!(factory.is_some());
        let outbox_event =
            factory.expect("realtime fanout should provide playlist outbox factory")(&playlist);

        assert_eq!(
            outbox_event.as_ref().map(|event| event.event_type.as_str()),
            Some("playlist_created")
        );
        assert_eq!(
            realtime_fanout
                .committed_publish_count
                .load(Ordering::SeqCst),
            0,
            "playlist outbox preparation must not locally publish before commit"
        );

        prepared.publish_after_outbox_commit();
        assert_eq!(
            realtime_fanout
                .committed_publish_count
                .load(Ordering::SeqCst),
            1,
            "playlist fanout should publish the same prepared event after commit"
        );

        let event: RealtimeEvent = serde_json::from_value(
            outbox_event
                .expect("outbox event should be generated")
                .payload,
        )
        .expect("playlist outbox payload should deserialize");
        match event {
            RealtimeEvent::PlaylistCreated {
                room_id,
                user_id,
                username,
                playlist,
                ..
            } => {
                assert_eq!(room_id, RoomId::expect_positive(105_001));
                assert_eq!(user_id, UserId::expect_positive(105_002));
                assert_eq!(username, "tester");
                assert_eq!(playlist.id, PlaylistId::expect_positive(105_003));
            }
            other => panic!("expected PlaylistCreated, got {other:?}"),
        }
    }
}
