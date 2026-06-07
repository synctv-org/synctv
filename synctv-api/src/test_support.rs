use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{BroadcastResult, ConnectionId, PublishRequest, RealtimeEvent};
use tokio::sync::{broadcast, mpsc};

use crate::realtime_fanout::{ChannelRealtimeFanoutService, RealtimeFanoutService};
use crate::runtime::{RealtimeEventService, RealtimeMetrics};

pub fn channel_realtime_fanout_service(
    sender: mpsc::Sender<PublishRequest>,
) -> std::sync::Arc<dyn RealtimeFanoutService> {
    std::sync::Arc::new(ChannelRealtimeFanoutService { sender })
}

#[derive(Default)]
pub struct RecordingRealtimeEventService {
    pub broadcast_calls: AtomicUsize,
    pub publish_only_calls: AtomicUsize,
    pub broadcast_events: Mutex<Vec<RealtimeEvent>>,
    pub room_calls: AtomicUsize,
    pub admin_calls: AtomicUsize,
    pub room_events: Mutex<Vec<(String, RealtimeEvent)>>,
    pub admin_events: Mutex<Vec<RealtimeEvent>>,
    pub distributed_enabled: bool,
    node_id: String,
}

impl RecordingRealtimeEventService {
    pub fn with_node(node_id: impl Into<String>, distributed_enabled: bool) -> Self {
        Self {
            node_id: node_id.into(),
            distributed_enabled,
            ..Self::default()
        }
    }

    pub fn room_event_count(&self) -> usize {
        self.room_events
            .lock()
            .expect("recorded room events mutex should not be poisoned")
            .len()
    }

    pub fn room_events(&self) -> Vec<RealtimeEvent> {
        self.room_events
            .lock()
            .expect("recorded room events mutex should not be poisoned")
            .iter()
            .map(|(_, event)| event.clone())
            .collect()
    }

    pub fn admin_events(&self) -> Vec<RealtimeEvent> {
        self.admin_events
            .lock()
            .expect("recorded admin events mutex should not be poisoned")
            .clone()
    }

    pub fn broadcast_events(&self) -> Vec<RealtimeEvent> {
        self.broadcast_events
            .lock()
            .expect("recorded broadcast events mutex should not be poisoned")
            .clone()
    }
}

#[async_trait]
impl RealtimeEventService for RecordingRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
        let (_tx, rx) = mpsc::channel(16);
        Ok((rx, connection_id))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
        self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
        self.broadcast_events
            .lock()
            .expect("recorded broadcast events mutex should not be poisoned")
            .push(event);
        BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        }
    }

    fn publish_only(&self, _event: RealtimeEvent) -> bool {
        self.publish_only_calls.fetch_add(1, Ordering::SeqCst);
        false
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        self.room_calls.fetch_add(1, Ordering::SeqCst);
        self.room_events
            .lock()
            .expect("recorded room events mutex should not be poisoned")
            .push((room_id.to_string(), event.clone()));
        1
    }

    fn broadcast_admin_local(&self, event: &RealtimeEvent) -> usize {
        self.admin_calls.fetch_add(1, Ordering::SeqCst);
        self.admin_events
            .lock()
            .expect("recorded admin events mutex should not be poisoned")
            .push(event.clone());
        1
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        let (tx, rx) = broadcast::channel(16);
        drop(tx);
        rx
    }

    fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            distributed_enabled: self.distributed_enabled,
        }
    }

    fn node_id(&self) -> &str {
        if self.node_id.is_empty() {
            "recording-realtime-event-service"
        } else {
            &self.node_id
        }
    }

    async fn shutdown(&self) {}
}
