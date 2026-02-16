// Publisher Manager - Handles RTMP publisher registration to Redis
//
// Listens to StreamHub events and manages publisher lifecycle:
// 1. On Publish event: Register publisher to Redis (atomic HSETNX)
// 2. Maintain heartbeat to keep registration alive
// 3. On UnPublish event: Remove publisher from Redis
//
// Based on design doc 17-数据流设计.md § 11.1

use super::registry::HEARTBEAT_INTERVAL_SECS;
use super::registry_trait::StreamRegistryTrait;
use anyhow::anyhow;
use synctv_xiu::streamhub::{
    define::BroadcastEventReceiver,
    stream::StreamIdentifier,
};
use std::sync::Arc;
use tokio::time::{interval, sleep, Duration};
use tracing::{debug, error, info, trace, warn};
use dashmap::DashMap;

/// Maximum number of retry attempts for heartbeat failures
const MAX_HEARTBEAT_RETRIES: u32 = 3;
/// Delay between heartbeat retries (exponential backoff base)
const HEARTBEAT_RETRY_BASE_DELAY_MS: u64 = 100;

/// Publisher manager that listens to `StreamHub` events
pub struct PublisherManager {
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    /// Active publishers (`stream_key` -> `media_id`)
    /// Live streaming is media-level, not room-level
    active_publishers: Arc<DashMap<String, String>>,
}

impl PublisherManager {
    pub fn new(registry: Arc<dyn StreamRegistryTrait>, local_node_id: String) -> Self {
        Self {
            registry,
            local_node_id,
            active_publishers: Arc::new(DashMap::new()),
        }
    }

    /// Start listening to `StreamHub` broadcast events
    pub async fn start(self: Arc<Self>, mut event_receiver: BroadcastEventReceiver) {
        info!("Publisher manager started");

        // Start heartbeat maintenance task and track its handle
        let heartbeat_manager = Arc::clone(&self);
        let heartbeat_handle = tokio::spawn(async move {
            heartbeat_manager.maintain_heartbeats().await;
        });

        // Listen to broadcast events
        loop {
            match event_receiver.recv().await {
                Ok(event) => {
                    if let Err(e) = self.handle_broadcast_event(event).await {
                        error!("Failed to handle broadcast event: {}", e);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "Publisher manager lagged behind by {n} broadcast events; \
                         some publish/unpublish events may have been missed. \
                         Active publishers may be stale."
                    );
                    // Continue processing -- stale state will be corrected by
                    // heartbeat failures or next publish/unpublish event.
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    error!("Broadcast channel closed");
                    break;
                }
            }
        }

        // Abort heartbeat task on exit to prevent leaked background work
        heartbeat_handle.abort();
        let _ = heartbeat_handle.await;
        warn!("Publisher manager stopped");
    }

    /// Handle `StreamHub` broadcast events
    ///
    /// Note: If publisher registration fails (e.g., Redis error or another
    /// node already publishing), we return an error but the RTMP session
    /// may continue. The stream will be published locally but not registered
    /// globally. This is a known limitation of the broadcast event pattern.
    /// Future improvement: Use request-response pattern for Publish events
    /// to allow rejecting RTMP connections before they start.
    async fn handle_broadcast_event(&self, event: synctv_xiu::streamhub::define::BroadcastEvent) -> anyhow::Result<()> {
        match event {
            synctv_xiu::streamhub::define::BroadcastEvent::Publish { identifier, .. } => {
                if let Err(e) = self.handle_publish(identifier.clone()).await {
                    // Log detailed error for monitoring
                    error!(
                        error = %e,
                        identifier = ?identifier,
                        "Publisher registration failed - stream will continue locally but not be globally registered. \
                         This may cause issues with cross-node pull streams."
                    );
                    return Err(e);
                }
            }
            synctv_xiu::streamhub::define::BroadcastEvent::UnPublish { identifier } => {
                self.handle_unpublish(identifier).await?;
            }
        }
        Ok(())
    }

    /// Handle Publish event - Register publisher to Redis
    async fn handle_publish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        // Extract app_name and stream_name from RTMP identifier
        let (app_name, stream_name) = if let StreamIdentifier::Rtmp { app_name, stream_name } = identifier { (app_name, stream_name) } else {
            warn!("Ignoring non-RTMP publish event: {:?}", identifier);
            return Ok(());
        };

        info!(
            "RTMP Publish event: app_name={}, stream_name={}",
            app_name,
            stream_name
        );

        // Parse room_id and media_id from stream_name
        // Expected format: "{room_id}/{media_id}" (e.g., "room123/media456")
        // Live streaming granularity is media-level within a room context
        let (room_id, media_id) = if let Some((r, m)) = stream_name.split_once('/') {
            (r.to_string(), m.to_string())
        } else {
            error!("Invalid stream_name format, expected 'room_id/media_id': {}", stream_name);
            return Err(anyhow!("Invalid stream_name format, expected 'room_id/media_id'"));
        };

        // Try to register as publisher (atomic HSETNX)
        match self.registry.try_register_publisher(&room_id, &media_id, &self.local_node_id, "").await {
            Ok(true) => {
                info!(
                    "Successfully registered as publisher for room {} / media {} (stream: {})",
                    room_id,
                    media_id,
                    stream_name
                );
                // Track active publisher with composite key
                let publisher_key = format!("{room_id}:{media_id}");
                self.active_publishers.insert(stream_name.clone(), publisher_key);
            }
            Ok(false) => {
                warn!(
                    "Publisher already exists for room {} / media {} (stream: {})",
                    room_id,
                    media_id,
                    stream_name
                );
                // Another node is already publisher, reject this push
                return Err(anyhow!("Publisher already exists for room {room_id} / media {media_id}"));
            }
            Err(e) => {
                error!("Failed to register publisher: {}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// Handle `UnPublish` event - Remove publisher from Redis
    async fn handle_unpublish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        let (app_name, stream_name) = match identifier {
            StreamIdentifier::Rtmp { app_name, stream_name } => (app_name, stream_name),
            _ => {
                return Ok(());
            }
        };

        info!(
            "RTMP UnPublish event: app_name={}, stream_name={}",
            app_name,
            stream_name
        );

        // Get composite key (room_id:media_id) from active publishers
        if let Some((_, publisher_key)) = self.active_publishers.remove(&stream_name) {
            // Parse room_id and media_id from the composite key
            if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                // Unregister from Redis
                if let Err(e) = self.registry.unregister_publisher(room_id, media_id).await {
                    error!("Failed to unregister publisher for room {} / media {}: {}", room_id, media_id, e);
                } else {
                    info!("Unregistered publisher for room {} / media {}", room_id, media_id);
                }
            }
        }

        Ok(())
    }

    /// Force re-registration of all tracked active publishers in Redis.
    ///
    /// Called after StreamHub restart to ensure Redis state is consistent
    /// with the local `active_publishers` map. Without this, publishers
    /// that were cleaned up from Redis would remain stale until TTL expiry.
    pub async fn reregister_all_publishers(&self) {
        let snapshot: Vec<(String, String)> = self
            .active_publishers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        if snapshot.is_empty() {
            debug!("No active publishers to re-register after StreamHub restart");
            return;
        }

        info!(
            "Re-registering {} active publishers after StreamHub restart",
            snapshot.len()
        );

        for (stream_name, publisher_key) in &snapshot {
            if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                match self
                    .registry
                    .try_register_publisher(room_id, media_id, &self.local_node_id, "")
                    .await
                {
                    Ok(true) => {
                        info!(
                            "Re-registered publisher for room {} / media {} (stream: {})",
                            room_id, media_id, stream_name
                        );
                    }
                    Ok(false) => {
                        warn!(
                            "Could not re-register publisher for room {} / media {} (another node took over)",
                            room_id, media_id
                        );
                        // Remove from local tracking since we no longer own it
                        self.active_publishers.remove(stream_name);
                    }
                    Err(e) => {
                        error!(
                            "Failed to re-register publisher for room {} / media {}: {}",
                            room_id, media_id, e
                        );
                    }
                }
            }
        }
    }

    /// Maintain heartbeat for all active publishers
    async fn maintain_heartbeats(&self) {
        let mut heartbeat_interval = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

        loop {
            heartbeat_interval.tick().await;

            // M-8: Snapshot keys first to avoid holding DashMap read guard during async Redis ops.
            let snapshot: Vec<String> = self
                .active_publishers
                .iter()
                .map(|entry| entry.value().clone())
                .collect();

            for publisher_key in &snapshot {
                // Parse room_id and media_id from the composite key
                if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                    // Try heartbeat with retries
                    let mut success = false;
                    for attempt in 0..MAX_HEARTBEAT_RETRIES {
                        match self.registry.refresh_publisher_ttl(room_id, media_id, "").await {
                            Ok(()) => {
                                success = true;
                                break;
                            }
                            Err(e) => {
                                if attempt < MAX_HEARTBEAT_RETRIES - 1 {
                                    // Exponential backoff: 100ms, 200ms, 400ms...
                                    let delay_ms = HEARTBEAT_RETRY_BASE_DELAY_MS * (1 << attempt);
                                    warn!(
                                        "Heartbeat attempt {} failed for room {} / media {}: {}. Retrying in {}ms",
                                        attempt + 1, room_id, media_id, e, delay_ms
                                    );
                                    sleep(Duration::from_millis(delay_ms)).await;
                                } else {
                                    error!(
                                        "All {} heartbeat attempts failed for room {} / media {}: {}. Publisher may be lost.",
                                        MAX_HEARTBEAT_RETRIES, room_id, media_id, e
                                    );
                                }
                            }
                        }
                    }

                    if success {
                        trace!("Heartbeat refreshed for room {} / media {}", room_id, media_id);
                    }
                }
            }

            if !self.active_publishers.is_empty() {
                debug!(
                    "Heartbeat: {} active publishers",
                    self.active_publishers.len()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::MockStreamRegistry;

    #[tokio::test]
    async fn test_publisher_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new());
        let local_node_id = "test-node-1".to_string();

        let manager = PublisherManager::new(registry, local_node_id);
        assert_eq!(manager.local_node_id, "test-node-1");
        assert!(manager.active_publishers.is_empty());
    }

    #[tokio::test]
    async fn test_active_publishers_map() {
        let (_event_sender, _) = tokio::sync::mpsc::channel::<synctv_xiu::streamhub::define::StreamHubEvent>(64);

        let registry = Arc::new(MockStreamRegistry::new());
        let manager = PublisherManager::new(registry, "test-node".to_string());

        // Verify active publishers map is empty
        assert!(manager.active_publishers.is_empty());
        assert_eq!(manager.active_publishers.len(), 0);
    }

    #[tokio::test]
    async fn test_handle_publish_success() {
        let registry = Arc::new(MockStreamRegistry::new());
        let manager = PublisherManager::new(registry, "test-node-1".to_string());

        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
        };

        // Handle publish event
        let result = manager.handle_publish(identifier).await;
        assert!(result.is_ok());

        // Verify publisher was tracked
        assert!(manager.active_publishers.contains_key("room123/media456"));
    }

    #[tokio::test]
    async fn test_handle_unpublish_success() {
        let registry = Arc::new(MockStreamRegistry::new());
        let manager = PublisherManager::new(registry, "test-node-1".to_string());

        // First, register a publisher
        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: "room123/media456".to_string(),
        };
        let _ = manager.handle_publish(identifier.clone()).await;

        // Then unpublish
        let result = manager.handle_unpublish(identifier).await;
        assert!(result.is_ok());

        // Verify publisher was removed from tracking
        assert!(!manager.active_publishers.contains_key("room123/media456"));
    }

    #[tokio::test]
    async fn test_handle_publish_invalid_format() {
        let registry = Arc::new(MockStreamRegistry::new());
        let manager = PublisherManager::new(registry, "test-node-1".to_string());

        let identifier = StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: "invalid_format".to_string(), // Missing room_id/media_id separator
        };

        let result = manager.handle_publish(identifier).await;
        assert!(result.is_err());
    }
}
