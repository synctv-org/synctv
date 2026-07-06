use std::sync::Arc;
use synctv_core::models::{Playlist, PlaylistId, RoomId, UserId};
use synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent;
use synctv_core::service::RealtimeOutboxPlaylistEventFactory;
use synctv_realtime::sync::RealtimeEvent;

use crate::realtime_fanout::{
    PreparedOutboxFanout, PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};

#[derive(Clone)]
pub struct PreparedPlaylistOutboxFanout {
    prepared: PreparedOutboxFanout<Playlist>,
}

impl PreparedPlaylistOutboxFanout {
    #[must_use]
    pub fn outbox_factory(&self) -> RealtimeOutboxPlaylistEventFactory {
        self.prepared.outbox_factory()
    }

    pub fn publish_after_outbox_commit(&self) {
        self.prepared.publish_after_outbox_commit();
    }
}

#[derive(Clone)]
pub struct PreparedPlaylistDeletedFanout {
    plan: PreparedRealtimeFanoutPlan,
}

impl PreparedPlaylistDeletedFanout {
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
    ) -> synctv_core::Result<PreparedPlaylistDeletedFanout>;
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

impl PlaylistFanoutService for DefaultPlaylistFanoutService {
    fn prepare_created_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout {
        PreparedPlaylistOutboxFanout {
            prepared: PreparedOutboxFanout::new(
                self.realtime_fanout.clone(),
                move |playlist: &Playlist| RealtimeEvent::PlaylistCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    user_id,
                    username: username.clone(),
                    playlist: playlist.clone(),
                    timestamp: synctv_core::SystemClock.now(),
                },
            ),
        }
    }

    fn prepare_updated_outbox_fanout(
        &self,
        room_id: RoomId,
        user_id: UserId,
        username: String,
    ) -> PreparedPlaylistOutboxFanout {
        PreparedPlaylistOutboxFanout {
            prepared: PreparedOutboxFanout::new(
                self.realtime_fanout.clone(),
                move |playlist: &Playlist| RealtimeEvent::PlaylistUpdated {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    user_id,
                    username: username.clone(),
                    playlist: playlist.clone(),
                    timestamp: synctv_core::SystemClock.now(),
                },
            ),
        }
    }

    fn prepare_deleted_outbox_fanout(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        playlist_id: &PlaylistId,
    ) -> synctv_core::Result<PreparedPlaylistDeletedFanout> {
        let event = playlist_deleted_event(room_id, user_id, username, playlist_id);
        Ok(PreparedPlaylistDeletedFanout {
            plan: PreparedRealtimeFanoutPlan::new(self.realtime_fanout.clone(), event)
                .map_err(synctv_core::Error::Internal)?,
        })
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
        timestamp: synctv_core::SystemClock.now(),
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

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    #[derive(Default)]
    struct RecordingRealtimeFanout {
        committed_publish_count: AtomicUsize,
    }

    #[async_trait]
    impl RealtimeFanoutService for RecordingRealtimeFanout {
        async fn try_publish(&self, _request: PublishRequest) -> bool {
            false
        }

        fn outbox_event(&self, event: &RealtimeEvent) -> Result<NewRealtimeOutboxEvent, String> {
            Ok(NewRealtimeOutboxEvent {
                id: event.event_id().to_string(),
                enqueue_outbox: true,
                aggregate_type: "playlist".to_string(),
                aggregate_id: event
                    .room_id()
                    .map_or_else(|| "global".to_string(), std::string::ToString::to_string),
                event_type: event.event_type().to_string(),
                event_version: 1,
                aggregate_version: None,
                payload: event.clone(),
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
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: synctv_core::SystemClock.now(),
            updated_at: synctv_core::SystemClock.now(),
            version: 0,
        }
    }

    #[tokio::test]
    async fn test_playlist_fanout_prepares_created_outbox_without_local_publish() -> TestResult {
        let realtime_fanout = Arc::new(RecordingRealtimeFanout::default());
        let service = default_playlist_fanout_service(realtime_fanout.clone());

        let playlist = playlist();
        let prepared =
            service.prepare_created_outbox_fanout(room_id(), user_id(), "tester".to_string());
        let factory = prepared.outbox_factory();
        let outbox_event = core_ok(factory(&playlist))?;

        assert_eq!(outbox_event.event_type.as_str(), "playlist_created");
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

        match outbox_event.payload {
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
            other => {
                return Err(test_error(format!(
                    "expected PlaylistCreated, got {other:?}"
                )))
            }
        }
        Ok(())
    }
}
