use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use super::dedup::MessageDeduplicator;
use super::events::ClusterEvent;
use super::redis_pubsub::PublishRequest;
use super::runtime::RoomMessageRuntime;
use crate::error::Result;
use synctv_core::cache::CacheInvalidationRuntime;
use synctv_core::service::PermissionService;

pub struct ClusterMessageTransportConfig {
    pub message_runtime: Arc<dyn RoomMessageRuntime>,
    pub node_id: String,
    pub key_prefix: String,
    pub admin_event_tx: broadcast::Sender<ClusterEvent>,
    pub permission_service: Option<PermissionService>,
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub deduplicator: Arc<MessageDeduplicator>,
    pub catchup_window_secs: u64,
    pub stream_max_length: usize,
}

pub struct ClusterMessageTransportRuntime {
    pub publish_tx: mpsc::Sender<PublishRequest>,
    pub publisher_handle: tokio::task::JoinHandle<()>,
}

#[async_trait]
pub trait ClusterMessageTransport: Send + Sync {
    async fn start(
        self: Arc<Self>,
        publish_channel_capacity: usize,
    ) -> Result<ClusterMessageTransportRuntime>;

    async fn shutdown(&self);
}

pub trait ClusterMessageTransportFactory: Send + Sync {
    fn build(
        &self,
        config: ClusterMessageTransportConfig,
    ) -> Result<Arc<dyn ClusterMessageTransport>>;
}
