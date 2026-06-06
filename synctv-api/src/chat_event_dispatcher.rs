use std::sync::Arc;

use synctv_core::models::ChatMessageEvent;
use synctv_realtime::sync::RealtimeEvent;

use crate::runtime::{RealtimeDeliveryOutcome, RealtimeEventService};

pub trait ChatEventDispatcher: Send + Sync {
    fn dispatch(&self, event: &ChatMessageEvent) -> RealtimeDeliveryOutcome;
}

pub struct RealtimeChatEventDispatcher {
    event_service: Arc<dyn RealtimeEventService>,
}

impl RealtimeChatEventDispatcher {
    #[must_use]
    pub fn new(event_service: Arc<dyn RealtimeEventService>) -> Self {
        Self { event_service }
    }
}

impl ChatEventDispatcher for RealtimeChatEventDispatcher {
    fn dispatch(&self, event: &ChatMessageEvent) -> RealtimeDeliveryOutcome {
        let outcome = self
            .event_service
            .broadcast_outcome(chat_message_event_to_realtime(event));
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %event.room_id,
                event_id = %event.event_id,
                "ChatMessageEvent broadcast missed the distributed fan-out path"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["chat_event_no_redis"])
                .inc();
        }
        outcome
    }
}

#[must_use]
pub fn default_chat_event_dispatcher(
    event_service: Arc<dyn RealtimeEventService>,
) -> Arc<dyn ChatEventDispatcher> {
    Arc::new(RealtimeChatEventDispatcher::new(event_service))
}

#[must_use]
pub fn chat_message_event_to_realtime(event: &ChatMessageEvent) -> RealtimeEvent {
    RealtimeEvent::ChatMessageEvent {
        event_id: event.event_id.clone(),
        room_id: event.room_id,
        actor_user_id: event.actor_user_id,
        event: event.clone(),
        timestamp: event.occurred_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RealtimeMetrics;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::Mutex;
    use synctv_core::models::{ChatEventKind, ChatMessage, ChatMessageWithImages, RoomId, UserId};
    use synctv_realtime::sync::{BroadcastResult, ConnectionId};
    use tokio::sync::{broadcast, mpsc};

    #[derive(Default)]
    struct RecordingRealtimeEventService {
        events: Mutex<Vec<RealtimeEvent>>,
    }

    #[async_trait]
    impl RealtimeEventService for RecordingRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
            let (_tx, rx) = mpsc::channel(1);
            Ok((rx, "test-connection".to_string()))
        }

        fn unsubscribe(&self, _connection_id: &str) {}

        fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
            self.events.lock().expect("lock events").push(event);
            BroadcastResult {
                local_sent: 1,
                redis_sent: false,
            }
        }

        fn publish_only(&self, _event: RealtimeEvent) -> bool {
            false
        }

        fn broadcast_local(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
            0
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
            let (_tx, rx) = broadcast::channel(1);
            rx
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: false,
            }
        }

        fn node_id(&self) -> &'static str {
            "test-node"
        }

        async fn shutdown(&self) {}
    }

    fn chat_event() -> ChatMessageEvent {
        let occurred_at = Utc::now();
        let room_id = RoomId::expect_positive(7);
        let user_id = UserId::expect_positive(11);
        let mut message = ChatMessage::new(room_id, user_id, "hello".to_string());
        message.id = 19;
        message.client_message_id = Some("client-1".to_string());
        message.created_at = occurred_at;
        ChatMessageEvent {
            event_id: "evt_test".to_string(),
            sequence: 1,
            room_id,
            actor_user_id: user_id,
            kind: ChatEventKind::Created,
            message: ChatMessageWithImages {
                message,
                images: Vec::new(),
                reactions: Vec::new(),
            },
            occurred_at,
        }
    }

    #[test]
    fn realtime_dispatcher_maps_chat_event_to_realtime_event() {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let dispatcher = RealtimeChatEventDispatcher::new(recorder.clone());
        let event = chat_event();

        let outcome = dispatcher.dispatch(&event);

        assert!(outcome.local_delivered());
        let events = recorder.events.lock().expect("lock events");
        match events.first().expect("recorded event") {
            RealtimeEvent::ChatMessageEvent {
                event_id,
                room_id,
                actor_user_id,
                event: recorded,
                ..
            } => {
                assert_eq!(event_id, "evt_test");
                assert_eq!(*room_id, RoomId::expect_positive(7));
                assert_eq!(*actor_user_id, UserId::expect_positive(11));
                assert_eq!(recorded.event_id, event.event_id);
            }
            other => panic!("unexpected realtime event: {other:?}"),
        }
    }
}
