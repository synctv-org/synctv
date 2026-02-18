// Publisher Manager - Maintains heartbeat for RTMP publishers
//
// Listens to StreamHub events and manages publisher heartbeat:
// 1. On Publish event: Track publisher locally (registration happens in auth phase)
// 2. Maintain heartbeat to keep registration alive
// 3. On UnPublish event: Remove publisher from Redis and local tracking
//
// NOTE: Publisher registration to Redis happens in the authentication phase
// (SyncTvRtmpAuth::on_publish) before the RTMP session is established.
// This component only maintains heartbeat for already-registered publishers.
//
// Based on design doc 17-数据流设计.md § 11.1

use super::registry::HEARTBEAT_INTERVAL_SECS;
use super::registry_trait::StreamRegistryTrait;
use synctv_xiu::streamhub::{
    define::{BroadcastEventReceiver, StreamHubEvent, StreamHubEventSender},
    stream::StreamIdentifier,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{interval, sleep, Duration};
use tracing::{debug, error, info, trace, warn};
use dashmap::DashMap;

/// Maximum number of retry attempts for heartbeat failures within a single heartbeat cycle
const MAX_HEARTBEAT_RETRIES: u32 = 3;
/// Delay between heartbeat retries (exponential backoff base)
const HEARTBEAT_RETRY_BASE_DELAY_MS: u64 = 100;
/// Number of consecutive heartbeat *cycles* that must fail before cleaning up a publisher.
/// This prevents killing active streams during transient Redis maintenance or network issues.
const MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 3;

/// Duration after which a publisher that hasn't sent any media data is
/// considered silent and should be cleaned up (LS-5). This is separate from
/// Redis TTL, which only detects node-level failures. A silent publisher may
/// keep its TCP connection alive but stop sending RTMP frames (e.g., crashed
/// encoder, frozen camera).
const SILENT_PUBLISHER_TIMEOUT_SECS: u64 = 60;

/// Tracked publisher state including activity timestamp and registration info.
struct PublisherEntry {
    /// Unix timestamp (seconds) of last observed data activity.
    /// Updated via `record_publisher_activity` when media frames arrive.
    last_active_secs: AtomicU64,
    /// Number of consecutive heartbeat cycles where all retries failed.
    /// Reset to 0 on any successful heartbeat. Only triggers cleanup when
    /// this reaches `MAX_CONSECUTIVE_HEARTBEAT_FAILURES`.
    consecutive_heartbeat_failures: std::sync::atomic::AtomicU32,
    /// User ID from the publisher registration (L-01: for reverse-index TTL refresh).
    user_id: String,
}

impl PublisherEntry {
    fn new() -> Self {
        Self {
            last_active_secs: AtomicU64::new(Self::now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            user_id: String::new(),
        }
    }

    fn with_user_id(user_id: String) -> Self {
        Self {
            last_active_secs: AtomicU64::new(Self::now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            user_id,
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn touch(&self) {
        self.last_active_secs.store(Self::now_secs(), Ordering::Release);
    }

    fn idle_secs(&self) -> u64 {
        Self::now_secs().saturating_sub(self.last_active_secs.load(Ordering::Acquire))
    }
}

/// Publisher manager that listens to `StreamHub` events
pub struct PublisherManager {
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    /// Advertised gRPC address of this node (L-05: used for re-registration after restart).
    local_grpc_address: String,
    /// Active publishers (composite key -> `PublisherEntry`)
    /// Live streaming is media-level, not room-level
    active_publishers: Arc<DashMap<String, Arc<PublisherEntry>>>,
    /// Sender for StreamHub events -- used to trigger unpublish on heartbeat failure
    /// so that subscribers are notified immediately instead of waiting for Redis TTL expiry.
    hub_event_sender: StreamHubEventSender,
    /// Counter for broadcast lag events (for monitoring)
    lag_event_count: AtomicU64,
    /// Duration of inactivity before a publisher is considered silent
    silent_timeout_secs: u64,
}

impl PublisherManager {
    pub fn new(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        hub_event_sender: StreamHubEventSender,
    ) -> Self {
        Self {
            registry,
            local_node_id,
            local_grpc_address: String::new(),
            active_publishers: Arc::new(DashMap::new()),
            hub_event_sender,
            lag_event_count: AtomicU64::new(0),
            silent_timeout_secs: SILENT_PUBLISHER_TIMEOUT_SECS,
        }
    }

    /// Set the advertised gRPC address for this node.
    /// Used during re-registration after StreamHub restart (L-05).
    #[must_use]
    pub fn with_grpc_address(mut self, grpc_address: String) -> Self {
        self.local_grpc_address = grpc_address;
        self
    }

    /// Returns the number of broadcast lag events observed since startup.
    pub fn lag_event_count(&self) -> u64 {
        self.lag_event_count.load(Ordering::Relaxed)
    }

    /// Returns the list of active publishers as `(app_name, stream_name)` pairs.
    ///
    /// Used by the HLS remuxer for post-lag reconciliation: after a broadcast
    /// lag event, the remuxer queries this list and starts HLS handlers for
    /// any active publishers that don't already have a running handler.
    pub fn active_publisher_streams(&self) -> Vec<(String, String)> {
        self.active_publishers
            .iter()
            .filter_map(|entry| {
                entry.key().split_once(':').map(|(room_id, media_id)| {
                    (room_id.to_string(), media_id.to_string())
                })
            })
            .collect()
    }

    /// Record media data activity for a publisher.
    ///
    /// Call this when media frames are received from a publisher to reset
    /// the silent publisher timeout. Without periodic calls to this method,
    /// the publisher will be considered silent after `SILENT_PUBLISHER_TIMEOUT_SECS`
    /// and automatically cleaned up.
    pub fn record_publisher_activity(&self, room_id: &str, media_id: &str) {
        let key = format!("{room_id}:{media_id}");
        if let Some(entry) = self.active_publishers.get(&key) {
            entry.touch();
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
                    self.lag_event_count.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "Publisher manager lagged behind by {n} broadcast events; \
                         some publish/unpublish events may have been missed. \
                         Reconciling active publishers with registry."
                    );
                    self.reconcile_with_registry().await;
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
    /// Tracks publishers locally for heartbeat maintenance. Publisher registration
    /// to Redis happens in the authentication phase (SyncTvRtmpAuth::on_publish),
    /// which runs BEFORE the RTMP session is established and can reject connections
    /// on registration failures.
    async fn handle_broadcast_event(&self, event: synctv_xiu::streamhub::define::BroadcastEvent) -> anyhow::Result<()> {
        match event {
            synctv_xiu::streamhub::define::BroadcastEvent::Publish { identifier, .. } => {
                if let Err(e) = self.handle_publish(identifier.clone()).await {
                    error!(
                        error = %e,
                        identifier = ?identifier,
                        "Failed to track publisher for heartbeat maintenance"
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

    /// Handle Publish event - Track publisher locally for heartbeat maintenance
    ///
    /// NOTE: This does NOT register the publisher to Redis. Registration happens
    /// in the authentication phase (SyncTvRtmpAuth::on_publish) before the RTMP
    /// session is established. This method only tracks publishers that have already
    /// been successfully authenticated and registered.
    async fn handle_publish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        // Extract app_name and stream_name from RTMP identifier
        let (app_name, stream_name) = if let StreamIdentifier::Rtmp { app_name, stream_name } = identifier { (app_name, stream_name) } else {
            warn!("Ignoring non-RTMP publish event: {:?}", identifier);
            return Ok(());
        };

        // StreamIdentifier format for RTMP:
        // - app_name: room_id (from RTMP connect command)
        // - stream_name: media_id (from RTMP publish command)
        // Live streaming granularity is media-level within a room context
        let room_id = app_name;
        let media_id = stream_name.clone();

        info!(
            "Tracking publisher for heartbeat: room={}, media={}, stream={}",
            room_id,
            media_id,
            stream_name
        );

        // Track active publisher with composite key (room_id:media_id)
        // This publisher has already been registered to Redis in the auth phase.
        // L-01: Query registry to get user_id for heartbeat TTL refresh.
        let publisher_key = format!("{room_id}:{media_id}");
        let entry = match self.registry.get_publisher(&room_id, &media_id).await {
            Ok(Some(info)) => {
                debug!(
                    "Retrieved publisher info for heartbeat tracking: room={}, media={}, user_id={}",
                    room_id, media_id, info.user_id
                );
                Arc::new(PublisherEntry::with_user_id(info.user_id))
            }
            Ok(None) => {
                warn!(
                    "Publisher not found in registry during tracking (room={}, media={}), using empty user_id",
                    room_id, media_id
                );
                Arc::new(PublisherEntry::new())
            }
            Err(e) => {
                warn!(
                    "Failed to query registry for publisher info (room={}, media={}): {}. Using empty user_id",
                    room_id, media_id, e
                );
                Arc::new(PublisherEntry::new())
            }
        };
        self.active_publishers.insert(publisher_key.clone(), entry);

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

        // StreamIdentifier format: app_name=room_id, stream_name=media_id
        let room_id = app_name;
        let media_id = stream_name;

        // Look up by composite key (room_id:media_id)
        let publisher_key = format!("{room_id}:{media_id}");
        if self.active_publishers.remove(&publisher_key).is_some() {
            // Unregister from Redis
            if let Err(e) = self.registry.unregister_publisher(&room_id, &media_id).await {
                error!("Failed to unregister publisher for room {} / media {}: {}", room_id, media_id, e);
            } else {
                info!("Unregistered publisher for room {} / media {}", room_id, media_id);
            }
        }

        Ok(())
    }

    /// Reconcile `active_publishers` with the registry after a broadcast lag event.
    ///
    /// When the broadcast channel lags, we may have missed `UnPublish` events,
    /// leaving stale entries in `active_publishers`. This method queries the
    /// registry for each locally-tracked publisher and removes entries that no
    /// longer exist or have been taken over by another node.
    async fn reconcile_with_registry(&self) {
        let snapshot: Vec<String> = self
            .active_publishers
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        if snapshot.is_empty() {
            debug!("No active publishers to reconcile after lag event");
            return;
        }

        info!(
            "Reconciling {} active publishers with registry after lag event",
            snapshot.len()
        );

        let mut removed = 0u32;
        for publisher_key in &snapshot {
            if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                match self.registry.get_publisher(room_id, media_id).await {
                    Ok(Some(info)) if info.node_id == self.local_node_id => {
                        // Publisher still registered to us -- keep it
                        trace!(
                            "Reconcile: publisher room={} media={} still active on this node",
                            room_id, media_id
                        );
                    }
                    Ok(Some(info)) => {
                        // Publisher was taken over by another node -- remove locally
                        warn!(
                            "Reconcile: publisher room={} media={} moved to node {}; removing local entry",
                            room_id, media_id, info.node_id
                        );
                        self.active_publishers.remove(publisher_key);
                        removed += 1;
                    }
                    Ok(None) => {
                        // Publisher no longer exists in registry -- remove locally
                        warn!(
                            "Reconcile: publisher room={} media={} no longer in registry; removing local entry",
                            room_id, media_id
                        );
                        self.active_publishers.remove(publisher_key);
                        removed += 1;
                    }
                    Err(e) => {
                        // Registry query failed -- keep the entry and let heartbeat handle it
                        error!(
                            "Reconcile: failed to query registry for room={} media={}: {}",
                            room_id, media_id, e
                        );
                    }
                }
            }
        }

        info!(
            "Reconciliation complete: removed {} stale entries, {} active publishers remaining",
            removed,
            self.active_publishers.len()
        );
    }

    /// Force re-registration of all tracked active publishers in Redis.
    ///
    /// Called after StreamHub restart to ensure Redis state is consistent
    /// with the local `active_publishers` map. Without this, publishers
    /// that were cleaned up from Redis would remain stale until TTL expiry.
    pub async fn reregister_all_publishers(&self) {
        // L-05: Snapshot both key and entry to access stored user_id for re-registration
        let snapshot: Vec<(String, Arc<PublisherEntry>)> = self
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

        for (publisher_key, entry) in &snapshot {
            if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                // L-05: Use stored user_id and node's grpc_address instead of empty strings
                match self
                    .registry
                    .try_register_publisher(
                        room_id,
                        media_id,
                        &self.local_node_id,
                        &entry.user_id,
                        &self.local_grpc_address,
                    )
                    .await
                {
                    Ok(true) => {
                        info!(
                            "Re-registered publisher for room {} / media {}",
                            room_id, media_id
                        );
                    }
                    Ok(false) => {
                        warn!(
                            "Could not re-register publisher for room {} / media {} (another node took over)",
                            room_id, media_id
                        );
                        self.active_publishers.remove(publisher_key);
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

    /// Cleanup a publisher: remove from local tracking, unregister from Redis,
    /// and notify StreamHub. Used by both heartbeat failure and silent publisher
    /// timeout paths.
    async fn cleanup_publisher(&self, room_id: &str, media_id: &str, reason: &str) {
        let publisher_key = format!("{room_id}:{media_id}");
        info!(
            "Cleaning up publisher room={} media={}: {}",
            room_id, media_id, reason
        );

        // 1. Remove from local tracking
        self.active_publishers.remove(&publisher_key);

        // 2. Unregister from Redis immediately (don't wait for TTL)
        if let Err(e) = self.registry.unregister_publisher(room_id, media_id).await {
            warn!(
                "Failed to unregister publisher from Redis for room {} / media {}: {}",
                room_id, media_id, e
            );
        }

        // 3. Send UnPublish to StreamHub so subscribers are notified.
        // Use try_send() instead of send().await to avoid blocking the heartbeat
        // loop if the StreamHub event channel is full or slow.
        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.to_string(),
            stream_name: media_id.to_string(),
        };
        match self.hub_event_sender.try_send(StreamHubEvent::UnPublish {
            identifier: identifier.clone(),
        }) {
            Ok(()) => {
                info!(
                    "Sent UnPublish event for room {} / media {} ({})",
                    room_id, media_id, reason
                );
            }
            Err(e) => {
                error!(
                    "Failed to send UnPublish event for {:?}: {}",
                    identifier, e
                );
            }
        }
    }

    /// Maintain heartbeat for all active publishers and detect silent publishers.
    ///
    /// Two checks run on each heartbeat interval:
    /// 1. Redis TTL refresh (detects node-level failures)
    /// 2. Silent publisher detection (LS-5): if no media data has been received
    ///    for `silent_timeout_secs`, the publisher is considered dead even though
    ///    the TCP connection may still be alive.
    async fn maintain_heartbeats(&self) {
        let mut heartbeat_interval = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

        loop {
            heartbeat_interval.tick().await;

            // M-8: Snapshot keys first to avoid holding DashMap read guard during async Redis ops.
            let snapshot: Vec<(String, Arc<PublisherEntry>)> = self
                .active_publishers
                .iter()
                .map(|entry| (entry.key().clone(), entry.value().clone()))
                .collect();

            for (publisher_key, entry) in &snapshot {
                // Parse room_id and media_id from the composite key
                let Some((room_id, media_id)) = publisher_key.split_once(':') else {
                    continue;
                };

                // LS-5: Check for silent publisher (no media data for too long)
                let idle_secs = entry.idle_secs();
                if idle_secs > self.silent_timeout_secs {
                    warn!(
                        "Silent publisher detected: room={} media={} (no data for {}s, threshold={}s)",
                        room_id, media_id, idle_secs, self.silent_timeout_secs
                    );
                    self.cleanup_publisher(
                        room_id,
                        media_id,
                        &format!("silent publisher timeout ({}s idle)", idle_secs),
                    ).await;
                    continue;
                }

                // L-01: Pass stored user_id to refresh both publisher TTL and user reverse-index TTL
                let user_id = &entry.user_id;

                // Try heartbeat with retries (Redis TTL refresh)
                let mut success = false;
                for attempt in 0..MAX_HEARTBEAT_RETRIES {
                    match self.registry.refresh_publisher_ttl(room_id, media_id, user_id).await {
                        Ok(()) => {
                            success = true;
                            break;
                        }
                        Err(e) => {
                            if attempt < MAX_HEARTBEAT_RETRIES - 1 {
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
                    // Reset consecutive failure counter on any successful heartbeat
                    entry.consecutive_heartbeat_failures.store(0, Ordering::Release);
                    trace!("Heartbeat refreshed for room {} / media {}", room_id, media_id);
                } else {
                    synctv_core::metrics::livestream::PUBLISHER_HEARTBEAT_FAILURES.inc();
                    let failures = entry.consecutive_heartbeat_failures.fetch_add(1, Ordering::AcqRel) + 1;
                    if failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
                        error!(
                            "Publisher room={} media={} failed {} consecutive heartbeat cycles, cleaning up",
                            room_id, media_id, failures
                        );
                        self.cleanup_publisher(
                            room_id,
                            media_id,
                            &format!("heartbeat failed {} consecutive cycles", failures),
                        ).await;
                    } else {
                        warn!(
                            "Heartbeat cycle failed for room={} media={} ({}/{} consecutive failures)",
                            room_id, media_id, failures, MAX_CONSECUTIVE_HEARTBEAT_FAILURES
                        );
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

    /// Create a test `PublisherManager` with a dummy `StreamHubEventSender`.
    /// Returns the manager and the corresponding receiver so tests can inspect
    /// events sent on heartbeat failure.
    fn test_manager(registry: Arc<dyn StreamRegistryTrait>, node_id: &str)
        -> (PublisherManager, synctv_xiu::streamhub::define::StreamHubEventReceiver)
    {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        (PublisherManager::new(registry, node_id.to_string(), tx), rx)
    }

    #[tokio::test]
    async fn test_publisher_manager_creation() {
        let registry = Arc::new(MockStreamRegistry::new());

        let (manager, _rx) = test_manager(registry, "test-node-1");
        assert_eq!(manager.local_node_id, "test-node-1");
        assert!(manager.active_publishers.is_empty());
    }

    #[tokio::test]
    async fn test_active_publishers_map() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        // Verify active publishers map is empty
        assert!(manager.active_publishers.is_empty());
        assert_eq!(manager.active_publishers.len(), 0);
    }

    #[tokio::test]
    async fn test_handle_publish_success() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node-1");

        let identifier = StreamIdentifier::Rtmp {
            app_name: "room123".to_string(),
            stream_name: "media456".to_string(),
        };

        // Handle publish event
        let result = manager.handle_publish(identifier).await;
        assert!(result.is_ok());

        // Verify publisher was tracked with composite key
        assert!(manager.active_publishers.contains_key("room123:media456"));
    }

    #[tokio::test]
    async fn test_handle_unpublish_success() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node-1");

        // First, register a publisher
        let identifier = StreamIdentifier::Rtmp {
            app_name: "room123".to_string(),
            stream_name: "media456".to_string(),
        };
        let _ = manager.handle_publish(identifier.clone()).await;

        // Then unpublish
        let result = manager.handle_unpublish(identifier).await;
        assert!(result.is_ok());

        // Verify publisher was removed from tracking (composite key)
        assert!(!manager.active_publishers.contains_key("room123:media456"));
    }

    #[tokio::test]
    async fn test_handle_publish_tracks_any_stream() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node-1");

        let identifier = StreamIdentifier::Rtmp {
            app_name: "room123".to_string(),
            stream_name: "media456".to_string(),
        };

        // PublisherManager just tracks publishers, doesn't validate format
        let result = manager.handle_publish(identifier).await;
        assert!(result.is_ok());

        // Verify tracking uses composite key
        assert!(manager.active_publishers.contains_key("room123:media456"));
    }

    /// Helper to insert a publisher entry into the active_publishers map.
    fn insert_entry(manager: &PublisherManager, key: &str) {
        manager.active_publishers.insert(
            key.to_string(),
            Arc::new(PublisherEntry::new()),
        );
    }

    #[tokio::test]
    async fn test_reconcile_removes_stale_entries() {
        // Registry has room1:media1 on our node, but NOT room2:media2
        let registry = Arc::new(MockStreamRegistry::new());
        registry.try_register_publisher("room1", "media1", "test-node", "", "").await.unwrap();

        let (manager, _rx) = test_manager(registry, "test-node");

        // Simulate local tracking of two publishers
        insert_entry(&manager, "room1:media1");
        insert_entry(&manager, "room2:media2");
        assert_eq!(manager.active_publishers.len(), 2);

        // Reconcile should remove room2:media2 (not in registry)
        manager.reconcile_with_registry().await;

        assert_eq!(manager.active_publishers.len(), 1);
        assert!(manager.active_publishers.contains_key("room1:media1"));
        assert!(!manager.active_publishers.contains_key("room2:media2"));
    }

    #[tokio::test]
    async fn test_reconcile_removes_entries_moved_to_other_node() {
        // Registry has room1:media1 but on a DIFFERENT node
        let registry = Arc::new(MockStreamRegistry::new());
        registry.try_register_publisher("room1", "media1", "other-node", "", "").await.unwrap();

        let (manager, _rx) = test_manager(registry, "test-node");

        // Local tracking thinks we own it
        insert_entry(&manager, "room1:media1");
        assert_eq!(manager.active_publishers.len(), 1);

        // Reconcile should remove it (owned by other-node)
        manager.reconcile_with_registry().await;

        assert!(manager.active_publishers.is_empty());
    }

    #[tokio::test]
    async fn test_reconcile_keeps_valid_entries() {
        // Registry has both publishers on our node
        let registry = Arc::new(MockStreamRegistry::new());
        registry.try_register_publisher("room1", "media1", "test-node", "", "").await.unwrap();
        registry.try_register_publisher("room2", "media2", "test-node", "", "").await.unwrap();

        let (manager, _rx) = test_manager(registry, "test-node");

        insert_entry(&manager, "room1:media1");
        insert_entry(&manager, "room2:media2");

        manager.reconcile_with_registry().await;

        // Both should still be present
        assert_eq!(manager.active_publishers.len(), 2);
    }

    #[tokio::test]
    async fn test_reconcile_with_empty_map() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        // Should not panic with empty active_publishers
        manager.reconcile_with_registry().await;
        assert!(manager.active_publishers.is_empty());
    }

    #[tokio::test]
    async fn test_lag_event_count() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        assert_eq!(manager.lag_event_count(), 0);
    }

    #[tokio::test]
    async fn test_record_publisher_activity() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        // Insert publisher
        let identifier = StreamIdentifier::Rtmp {
            app_name: "room1".to_string(),
            stream_name: "media1".to_string(),
        };
        manager.handle_publish(identifier).await.unwrap();

        // Record activity and verify the entry was touched
        let before = manager
            .active_publishers
            .get("room1:media1")
            .unwrap()
            .idle_secs();
        assert!(before <= 1); // just created

        manager.record_publisher_activity("room1", "media1");

        let after = manager
            .active_publishers
            .get("room1:media1")
            .unwrap()
            .idle_secs();
        assert!(after <= 1); // just touched
    }

    #[tokio::test]
    async fn test_record_activity_nonexistent_publisher() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        // Should not panic when recording activity for a publisher that doesn't exist
        manager.record_publisher_activity("nonexistent", "publisher");
    }
}
