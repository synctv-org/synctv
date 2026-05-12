use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use super::dedup::MessageDeduplicator;
use super::events::RealtimeEvent;
use super::redis_pubsub::PublishRequest;
use super::runtime::RoomMessageRuntime;
use crate::error::Result;
use synctv_core::models::RoomId;

#[async_trait]
pub trait RealtimeEventHandler: Send + Sync {
    async fn handle_remote_event(&self, room_id: Option<RoomId>, event: &RealtimeEvent);
}

pub struct RealtimeMessageTransportConfig {
    pub message_runtime: Arc<dyn RoomMessageRuntime>,
    pub node_id: String,
    pub key_prefix: String,
    pub admin_event_tx: broadcast::Sender<RealtimeEvent>,
    pub event_handler: Option<Arc<dyn RealtimeEventHandler>>,
    pub deduplicator: Arc<MessageDeduplicator>,
    pub catchup_window_secs: u64,
    pub stream_max_length: usize,
}

pub struct RealtimeMessageTransportRuntime {
    pub publish_tx: mpsc::Sender<PublishRequest>,
    pub publisher_handle: tokio::task::JoinHandle<()>,
}

#[async_trait]
pub trait RealtimeMessageTransport: Send + Sync {
    async fn start(
        self: Arc<Self>,
        publish_channel_capacity: usize,
    ) -> Result<RealtimeMessageTransportRuntime>;

    async fn shutdown(&self);
}

pub trait RealtimeMessageTransportFactory: Send + Sync {
    fn build(
        &self,
        config: RealtimeMessageTransportConfig,
    ) -> Result<Arc<dyn RealtimeMessageTransport>>;
}
