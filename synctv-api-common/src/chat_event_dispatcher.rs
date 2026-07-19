use std::sync::Arc;

use synctv_core::models::{ChatMessageEvent, ChatPinEvent};
use synctv_realtime::fanout::{RealtimeDeliveryOutcome, RealtimeEventService};
use synctv_realtime::sync::RealtimeEvent;

pub trait ChatEventDispatcher: Send + Sync {
    fn dispatch(&self, event: &ChatMessageEvent) -> RealtimeDeliveryOutcome;
    fn dispatch_pin(&self, event: &ChatPinEvent) -> RealtimeDeliveryOutcome;
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

    fn dispatch_pin(&self, event: &ChatPinEvent) -> RealtimeDeliveryOutcome {
        let outcome = self
            .event_service
            .broadcast_outcome(chat_pin_event_to_realtime(event));
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %event.room_id,
                event_id = %event.event_id,
                "ChatPinEvent broadcast missed the distributed fan-out path"
            );
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["chat_pin_event_no_redis"])
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

#[must_use]
pub fn chat_pin_event_to_realtime(event: &ChatPinEvent) -> RealtimeEvent {
    RealtimeEvent::ChatPinEvent {
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
    use crate::test_support::RecordingRealtimeEventService;
    use synctv_core::models::{
        ChatEventKind, ChatMessage, ChatMessageWithAttachments, RoomId, UserId,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn chat_event() -> ChatMessageEvent {
        let occurred_at = synctv_core::SystemClock.now();
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
            message: ChatMessageWithAttachments {
                message,
                attachments: Vec::new(),
                reactions: Vec::new(),
                mentions: Vec::new(),
                pin: None,
            },
            occurred_at,
        }
    }

    #[test]
    fn realtime_dispatcher_maps_chat_event_to_realtime_event() -> TestResult {
        let recorder = Arc::new(RecordingRealtimeEventService::default());
        let dispatcher = RealtimeChatEventDispatcher::new(recorder.clone());
        let event = chat_event();

        let outcome = dispatcher.dispatch(&event);

        assert!(outcome.local_delivered());
        let events = recorder.broadcast_events();
        match events.first().ok_or_else(|| test_error("recorded event"))? {
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
            other => return Err(test_error(format!("unexpected realtime event: {other:?}"))),
        }
        Ok(())
    }
}
