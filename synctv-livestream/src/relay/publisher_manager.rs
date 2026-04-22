// Publisher Manager - Maintains heartbeat for RTMP publishers
// Listens to StreamHub events and manages publisher heartbeat:
// 1. On Publish event: Track publisher locally (registration happens in auth phase)
// 2. Maintain heartbeat to keep registration alive
// 3. On UnPublish event: Remove publisher from Redis and local tracking
// NOTE: Publisher registration to Redis happens in the authentication phase
// (SyncTvRtmpAuth::on_publish) before the RTMP session is established.
// This component only maintains heartbeat for already-registered publishers.
// Based on design doc 17-data-flow-design.md §11.1

use super::registry::HEARTBEAT_INTERVAL_SECS;
use super::registry_trait::{PublisherRefreshOutcome, StreamRegistryTrait};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use synctv_xiu::streamhub::{
    define::{BroadcastEventReceiver, StreamHubEvent, StreamHubEventSender},
    stream::StreamIdentifier,
};
use tokio::time::{interval, sleep, Duration};
use tracing::{debug, error, info, trace, warn};

/// Maximum number of retry attempts for heartbeat failures within a single heartbeat cycle
const MAX_HEARTBEAT_RETRIES: u32 = 3;
/// Delay between heartbeat retries (exponential backoff base)
const HEARTBEAT_RETRY_BASE_DELAY_MS: u64 = 100;
/// Cleanup-time unregister uses a longer retry budget because stale fenced keys
/// block rapid republish even after Redis recovers.
const UNREGISTER_RETRY_ATTEMPTS: u32 = 3;
/// Retry envelope matches the previous background cleanup behavior.
const UNREGISTER_RETRY_DELAYS_MS: [u64; 2] = [2_000, 4_000];
/// Number of consecutive heartbeat *cycles* where Redis **confirms the publisher is gone**
/// before cleaning up. This prevents killing active streams during transient Redis timeouts
/// or maintenance windows.
///
/// A Redis timeout (slow response) is NOT the same as the publisher being dead.
/// Only count a heartbeat failure if Redis responds AND the publisher TTL actually expired.
/// If Redis itself is unreachable, use a separate `redis_unreachable` counter (below).
const MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 3;

/// Number of consecutive heartbeat cycles where Redis is completely unreachable before
/// triggering publisher cleanup as a last resort.
///
/// This threshold is intentionally much higher than `MAX_CONSECUTIVE_HEARTBEAT_FAILURES`
/// because Redis timeouts should not cause publisher cleanup — they likely indicate a
/// transient network issue, not a dead publisher.
const MAX_CONSECUTIVE_REDIS_UNREACHABLE: u32 = 10;
/// Maximum time to wait for delivering a critical `UnPublish` control event.
const UNPUBLISH_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Duration after which a publisher that hasn't sent any media data is
/// considered silent and should be cleaned up (LS-5). This is separate from
/// Redis TTL, which only detects node-level failures. A silent publisher may
/// keep its TCP connection alive but stop sending RTMP frames (e.g., crashed
/// encoder, frozen camera).
///
/// The silent timeout must be LONGER than the max broadcast channel
/// lag window (typically several seconds) to avoid false-positive timeouts when
/// frames are still being produced but have not yet been delivered through the
/// broadcast channel. The activity callback is throttled to 10s intervals
/// (see `ACTIVITY_RECORD_INTERVAL`), so the effective minimum useful timeout is
/// ~2× the throttle interval. Set to 5 minutes to comfortably exceed any
/// realistic broadcast lag window while still detecting truly frozen encoders.
/// Reduced from 300s to 60s for faster detection of crashed encoders.
const SILENT_PUBLISHER_TIMEOUT_SECS: u64 = 60;

/// Interval for periodic registry-local consistency sync.
///
/// This sync ensures local tracking state matches the registry, detecting and
/// repairing inconsistencies caused by:
/// - Network partitions where registry state changed but local didn't notice
/// - Missed broadcast events (though these also trigger immediate reconciliation)
/// - Registry entries taken over by other nodes
///
/// Set to 5 minutes as a balance between consistency and registry load.
/// Reconciliation also runs on broadcast lag events, so this is a safety net.
const PERIODIC_SYNC_INTERVAL_SECS: u64 = 300;

#[derive(Clone, Debug)]
struct PublisherCleanupContext {
    room_id: String,
    media_id: String,
    epoch: u64,
}

/// Tracked publisher state including activity timestamp and registration info.
struct PublisherEntry {
    /// Unix timestamp (seconds) of last observed data activity.
    /// Updated via `record_publisher_activity` when media frames arrive.
    last_active_secs: AtomicU64,
    /// Number of consecutive heartbeat cycles where Redis responded AND
    /// the publisher TTL refresh failed (i.e., publisher entry is gone).
    /// Reset to 0 on any successful heartbeat. Only triggers cleanup when
    /// this reaches `MAX_CONSECUTIVE_HEARTBEAT_FAILURES`.
    ///
    /// This counter is NOT incremented when Redis itself is
    /// unreachable (timeout/connection error). See `redis_unreachable_cycles`.
    consecutive_heartbeat_failures: std::sync::atomic::AtomicU32,
    /// Number of consecutive heartbeat cycles where Redis was completely
    /// unreachable (timeout or connection refused), regardless of publisher status.
    ///
    /// Separated from `consecutive_heartbeat_failures` to prevent
    /// slow Redis from triggering false publisher cleanup. Only triggers
    /// cleanup when this reaches `MAX_CONSECUTIVE_REDIS_UNREACHABLE`.
    redis_unreachable_cycles: std::sync::atomic::AtomicU32,
    /// User ID from the publisher registration (L-01: for reverse-index TTL refresh).
    user_id: String,
    /// Publisher epoch captured from the registry when tracking started.
    epoch: u64,
}

impl PublisherEntry {
    // Used in test helpers to insert entries without a real registry lookup.
    #[cfg(test)]
    fn new() -> Self {
        Self {
            last_active_secs: AtomicU64::new(Self::now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            redis_unreachable_cycles: std::sync::atomic::AtomicU32::new(0),
            user_id: String::new(),
            epoch: 0,
        }
    }

    fn with_registration(user_id: String, epoch: u64) -> Self {
        Self {
            last_active_secs: AtomicU64::new(Self::now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            redis_unreachable_cycles: std::sync::atomic::AtomicU32::new(0),
            user_id,
            epoch,
        }
    }

    fn clone_with_registration(&self, user_id: String, epoch: u64) -> Self {
        Self {
            last_active_secs: AtomicU64::new(self.last_active_secs.load(Ordering::Acquire)),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(
                self.consecutive_heartbeat_failures.load(Ordering::Acquire),
            ),
            redis_unreachable_cycles: std::sync::atomic::AtomicU32::new(
                self.redis_unreachable_cycles.load(Ordering::Acquire),
            ),
            user_id,
            epoch,
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn touch(&self) {
        self.last_active_secs
            .store(Self::now_secs(), Ordering::Release);
    }

    fn idle_secs(&self) -> u64 {
        Self::now_secs().saturating_sub(self.last_active_secs.load(Ordering::Acquire))
    }
}

fn is_redis_unreachable_error(error: &anyhow::Error) -> bool {
    if let Some(redis_err) = error.downcast_ref::<redis::RedisError>() {
        return matches!(
            redis_err.kind(),
            redis::ErrorKind::Io | redis::ErrorKind::ClusterConnectionNotFound
        );
    }

    if error.downcast_ref::<std::io::Error>().is_some() {
        return true;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("redis operation timed out") || message.contains("redis timeout")
}

/// Publisher manager that listens to `StreamHub` events
pub struct PublisherManager {
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    /// Advertised shared API address of this node (L-05: used for re-registration after restart).
    local_api_address: String,
    /// Active publishers (composite key -> `PublisherEntry`)
    /// Live streaming is media-level, not room-level
    active_publishers: Arc<DashMap<String, Arc<PublisherEntry>>>,
    /// Sender for `StreamHub` events -- used to trigger unpublish on heartbeat failure
    /// so that subscribers are notified immediately instead of waiting for Redis TTL expiry.
    hub_event_sender: StreamHubEventSender,
    /// Counter for broadcast lag events (for monitoring)
    lag_event_count: AtomicU64,
    /// Duration of inactivity before a publisher is considered silent
    silent_timeout_secs: u64,
    /// Flag to suppress silent-publisher cleanup during `StreamHub` restart.
    /// Set before restart, cleared after re-registration completes.
    is_restarting: Arc<AtomicBool>,
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
            local_api_address: String::new(),
            active_publishers: Arc::new(DashMap::new()),
            hub_event_sender,
            lag_event_count: AtomicU64::new(0),
            silent_timeout_secs: SILENT_PUBLISHER_TIMEOUT_SECS,
            is_restarting: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a new `PublisherManager` with a shared restarting flag.
    ///
    /// This allows external code (e.g., `StreamHub` restart loop) to share the
    /// restarting flag and set it before cleanup operations begin, preventing
    /// false silent-publisher detections during the restart window.
    pub fn with_restarting_flag(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        hub_event_sender: StreamHubEventSender,
        is_restarting: Arc<AtomicBool>,
    ) -> Self {
        Self {
            registry,
            local_node_id,
            local_api_address: String::new(),
            active_publishers: Arc::new(DashMap::new()),
            hub_event_sender,
            lag_event_count: AtomicU64::new(0),
            silent_timeout_secs: SILENT_PUBLISHER_TIMEOUT_SECS,
            is_restarting,
        }
    }

    /// Get a clone of the restarting flag for external coordination.
    ///
    /// This allows external code (e.g., `StreamHub` restart loop) to set the
    /// restarting flag before cleanup operations begin, preventing false
    /// silent-publisher detections during the restart window.
    pub fn restarting_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_restarting)
    }

    /// Set the advertised shared API address for this node.
    /// Used during re-registration after `StreamHub` restart (L-05).
    #[must_use]
    pub fn with_api_address(mut self, api_address: String) -> Self {
        self.local_api_address = api_address;
        self
    }

    /// Mark the manager as restarting to suppress silent-publisher cleanup
    /// during the `StreamHub` restart window.
    pub fn set_restarting(&self) {
        self.is_restarting.store(true, Ordering::Release);
    }

    /// Clear the restarting flag after re-registration completes.
    pub fn clear_restarting(&self) {
        self.is_restarting.store(false, Ordering::Release);
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
                entry
                    .key()
                    .split_once(':')
                    .map(|(room_id, media_id)| (room_id.to_string(), media_id.to_string()))
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
        if self.local_api_address.is_empty() {
            warn!(
                "PublisherManager started with empty api_address. \
                 Re-registration after StreamHub restart will use an empty address, \
                 preventing cross-node HLS proxy from reaching this node. \
                 Set api_address in LivestreamConfig."
            );
        }

        // Clean up stale Redis registrations from a previous process/restart
        // of this node. Without this, stale entries would persist until TTL
        // expiry, causing other nodes to route requests to a node that is no
        // longer publishing those streams.
        if !self.local_node_id.is_empty() {
            match self
                .registry
                .cleanup_all_publishers_for_node(&self.local_node_id)
                .await
            {
                Ok(()) => {
                    info!(
                        "Cleaned up stale publisher registrations for node {}",
                        self.local_node_id
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to cleanup stale publisher registrations for node {}: {}",
                        self.local_node_id, e
                    );
                }
            }
        }

        info!("Publisher manager started");

        let mut heartbeat_interval = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        let mut sync_interval = interval(Duration::from_secs(PERIODIC_SYNC_INTERVAL_SECS));
        let mut sync_started = false;

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    self.run_heartbeat_cycle().await;
                }
                _ = sync_interval.tick() => {
                    if !sync_started {
                        sync_started = true;
                        continue;
                    }

                    debug!(
                        "Running periodic registry-local sync (interval={}s)",
                        PERIODIC_SYNC_INTERVAL_SECS
                    );
                    self.run_periodic_sync_once().await;
                }
                recv = event_receiver.recv() => {
                    match recv {
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
                            self.run_periodic_sync_once().await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            error!("Broadcast channel closed");
                            break;
                        }
                    }
                }
            }
        }

        warn!("Publisher manager stopped");
    }

    /// Handle `StreamHub` broadcast events
    ///
    /// Tracks publishers locally for heartbeat maintenance. Publisher registration
    /// to Redis happens in the authentication phase (`SyncTvRtmpAuth::on_publish`),
    /// which runs BEFORE the RTMP session is established and can reject connections
    /// on registration failures.
    async fn handle_broadcast_event(
        &self,
        event: synctv_xiu::streamhub::define::BroadcastEvent,
    ) -> anyhow::Result<()> {
        match event {
            synctv_xiu::streamhub::define::BroadcastEvent::Publish {
                identifier,
                pub_type,
            } => {
                // Only track local RTMP push publishers for heartbeat management.
                // Remote relay streams (RtmpRelay) are managed by their origin node;
                // tracking them here would create duplicate heartbeats and incorrect
                // cleanup on unpublish.
                if pub_type == synctv_xiu::streamhub::define::PublishType::RtmpRelay {
                    debug!(
                        identifier = ?identifier,
                        "Ignoring relay publish event for heartbeat tracking"
                    );
                    return Ok(());
                }
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
    /// in the authentication phase (`SyncTvRtmpAuth::on_publish`) before the RTMP
    /// session is established. This method only tracks publishers that have already
    /// been successfully authenticated and registered.
    async fn handle_publish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        // Extract app_name and stream_name from RTMP identifier
        let identifier_debug = format!("{identifier:?}");
        let StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        } = identifier
        else {
            warn!("Ignoring non-RTMP publish event: {identifier_debug}");
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
            room_id, media_id, stream_name
        );

        // Track active publisher with composite key (room_id:media_id)
        // This publisher has already been registered to Redis in the auth phase.
        // L-01: Query registry to get user_id for heartbeat TTL refresh.
        let publisher_key = format!("{room_id}:{media_id}");
        let entry = match self.registry.get_publisher(&room_id, &media_id).await {
            Ok(Some(info)) => {
                debug!(
                    "Retrieved publisher info for heartbeat tracking: room={}, media={}, user_id={}, epoch={}",
                    room_id, media_id, info.user_id, info.epoch
                );
                Arc::new(PublisherEntry::with_registration(info.user_id, info.epoch))
            }
            Ok(None) => {
                // Publisher not in registry (was never registered or already expired).
                // Skip tracking to avoid sending heartbeats with incorrect ownership.
                warn!(
                    "Publisher not found in registry during tracking (room={}, media={}); skipping heartbeat tracking",
                    room_id, media_id
                );
                return Ok(());
            }
            Err(e) => {
                // Fail-closed on Redis failure. If we cannot verify the publisher
                // in the registry, we must reject the stream rather than allowing it to
                // continue without heartbeat tracking. An untracked publisher would stay
                // active indefinitely with an empty user_id, bypassing ownership checks.
                error!(
                    "Failed to query registry for publisher info (room={}, media={}): {}. \
                     Rejecting stream to prevent untracked publisher (fail-closed)",
                    room_id, media_id, e
                );
                return Err(e.context(format!(
                    "Redis failure during publish tracking for room={room_id}, media={media_id}"
                )));
            }
        };
        self.active_publishers.insert(publisher_key.clone(), entry);

        Ok(())
    }

    /// Handle `UnPublish` event - Remove publisher from Redis
    async fn handle_unpublish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        let StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        } = identifier
        else {
            return Ok(());
        };

        info!(
            "RTMP UnPublish event: app_name={}, stream_name={}",
            app_name, stream_name
        );

        // StreamIdentifier format: app_name=room_id, stream_name=media_id
        let room_id = app_name;
        let media_id = stream_name;

        // Look up by composite key (room_id:media_id)
        let publisher_key = format!("{room_id}:{media_id}");
        if self.active_publishers.remove(&publisher_key).is_some() {
            // Unregister from Redis
            if let Err(e) = self
                .registry
                .unregister_publisher(&room_id, &media_id)
                .await
            {
                error!(
                    "Failed to unregister publisher for room {} / media {}: {}",
                    room_id, media_id, e
                );
            } else {
                info!(
                    "Unregistered publisher for room {} / media {}",
                    room_id, media_id
                );
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
                            room_id,
                            media_id
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

    /// Bidirectional reconciliation: query Redis for all publishers on this node
    /// and add any missing entries to `active_publishers`.
    ///
    /// After a broadcast channel lag, we may have missed `Publish` events,
    /// causing publishers that exist in Redis to be absent from
    /// `active_publishers`. Without heartbeat maintenance, these publishers
    /// would silently expire from Redis when their TTL runs out.
    async fn reconcile_missing_from_registry(&self) {
        let active_publishers = match self.registry.list_active_publishers().await {
            Ok(streams) => streams,
            Err(e) => {
                error!(
                    "Reconcile (reverse): failed to list active publishers from registry: {}",
                    e
                );
                return;
            }
        };

        let mut added = 0u32;
        for publisher in active_publishers {
            let publisher_key = format!("{}:{}", publisher.room_id, publisher.media_id);
            // Skip if already tracked locally
            if self.active_publishers.contains_key(&publisher_key) {
                continue;
            }
            if publisher.publisher.node_id != self.local_node_id {
                continue;
            }

            info!(
                "Reconcile (reverse): adding missing publisher room={} media={} to local tracking",
                publisher.room_id, publisher.media_id
            );
            let entry = Arc::new(PublisherEntry::with_registration(
                publisher.publisher.user_id,
                publisher.publisher.epoch,
            ));
            self.active_publishers.insert(publisher_key, entry);
            added += 1;
        }

        if added > 0 {
            info!(
                "Reconcile (reverse): added {} missing publishers, {} total active",
                added,
                self.active_publishers.len()
            );
        }
    }

    /// Run periodic synchronization between local tracking and registry.
    ///
    /// This is a background task that runs at `PERIODIC_SYNC_INTERVAL_SECS` intervals
    /// to ensure local-registry consistency. It catches inconsistencies that may
    /// occur due to:
    /// - Network partitions where registry state changed
    /// - Missed broadcast events that didn't trigger immediate reconciliation
    /// - Registry entries taken over by other nodes
    ///
    /// The sync is bidirectional:
    /// 1. Remove local entries that no longer exist in registry or changed ownership
    /// 2. Add registry entries that belong to this node but are missing locally
    async fn run_periodic_sync_once(&self) {
        self.reconcile_with_registry().await;
        self.reconcile_missing_from_registry().await;
    }

    async fn refresh_tracked_registration_after_reregister(
        &self,
        publisher_key: &str,
        tracked_entry: &Arc<PublisherEntry>,
        room_id: &str,
        media_id: &str,
    ) {
        match self.registry.get_publisher(room_id, media_id).await {
            Ok(Some(info)) if info.node_id == self.local_node_id => {
                if let Some(mut current) = self.active_publishers.get_mut(publisher_key) {
                    if Arc::ptr_eq(current.value(), tracked_entry) {
                        *current.value_mut() = Arc::new(
                            tracked_entry.clone_with_registration(info.user_id, info.epoch),
                        );
                    } else {
                        debug!(
                            "Skipped tracked registration refresh for room={} media={} because local entry changed during re-registration",
                            room_id, media_id
                        );
                    }
                } else {
                    debug!(
                        "Skipped tracked registration refresh for room={} media={} because it is no longer tracked",
                        room_id, media_id
                    );
                }
            }
            Ok(Some(info)) => {
                warn!(
                    "Re-registered publisher for room {} / media {} but registry ownership moved to node {} before local epoch refresh",
                    room_id, media_id, info.node_id
                );
            }
            Ok(None) => {
                warn!(
                    "Re-registered publisher for room {} / media {} but registry entry disappeared before local epoch refresh",
                    room_id, media_id
                );
            }
            Err(e) => {
                error!(
                    "Re-registered publisher for room {} / media {} but failed to refresh local epoch from registry: {}",
                    room_id, media_id, e
                );
            }
        }
    }

    async fn try_reregister_after_missing_refresh(
        &self,
        publisher_key: &str,
        entry: &Arc<PublisherEntry>,
        room_id: &str,
        media_id: &str,
    ) {
        match self
            .registry
            .try_register_publisher(
                room_id,
                media_id,
                &self.local_node_id,
                &entry.user_id,
                &self.local_api_address,
            )
            .await
        {
            Ok(true) => {
                self.refresh_tracked_registration_after_reregister(
                    publisher_key,
                    entry,
                    room_id,
                    media_id,
                )
                .await;
                info!(
                    "Re-created missing publisher registration for room {} / media {} after TTL-refresh race",
                    room_id, media_id
                );
            }
            Ok(false) => {
                self.refresh_tracked_registration_after_reregister(
                    publisher_key,
                    entry,
                    room_id,
                    media_id,
                )
                .await;
                info!(
                    "Publisher room {} / media {} was re-created by another flow during restart recovery",
                    room_id, media_id
                );
            }
            Err(e) => {
                error!(
                    "Failed to re-create missing publisher registration for room {} / media {} after TTL-refresh race: {}",
                    room_id, media_id, e
                );
            }
        }
    }

    fn spawn_cleanup_publisher(
        self: &Arc<Self>,
        room_id: String,
        media_id: String,
        expected_epoch: u64,
        reason: String,
    ) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager
                .cleanup_publisher(&room_id, &media_id, expected_epoch, &reason)
                .await;
        });
    }

    /// Force re-registration of all tracked active publishers in Redis.
    ///
    /// Called after `StreamHub` restart to ensure Redis state is consistent
    /// with the local `active_publishers` map. Without this, publishers
    /// that were cleaned up from Redis would remain stale until TTL expiry.
    ///
    /// This method also cleans up zombie entries (local entries that no longer
    /// exist in the registry) to prevent memory leaks when `UnPublish` events are lost.
    ///
    /// Sets `is_restarting` before re-registration and clears it after,
    /// suppressing silent-publisher cleanup during the restart window.
    pub async fn reregister_all_publishers(&self) {
        self.set_restarting();

        // Memory leak fix: First reconcile with registry to remove zombie entries
        // (local entries that no longer exist in registry or were taken over).
        // This prevents memory leaks when UnPublish events are lost.
        self.reconcile_with_registry().await;

        // L-05: Snapshot both key and entry to access stored user_id for re-registration
        let snapshot: Vec<(String, Arc<PublisherEntry>)> = self
            .active_publishers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        if snapshot.is_empty() {
            debug!("No active publishers to re-register after StreamHub restart");
            self.clear_restarting();
            return;
        }

        if self.local_api_address.is_empty() {
            warn!(
                "Re-registering {} publishers with empty local_api_address. \
                 Cross-node HLS proxying will fail until api_address is set in LivestreamConfig. \
                 Proceeding with re-registration to restore local publisher ownership in Redis.",
                snapshot.len()
            );
        }

        info!(
            "Re-registering {} active publishers after StreamHub restart",
            snapshot.len()
        );

        for (publisher_key, entry) in &snapshot {
            if let Some((room_id, media_id)) = publisher_key.split_once(':') {
                // Try to register the publisher in registry.
                // After reconcile_with_registry, only entries owned by us remain,
                // so we just need to refresh TTL or re-register if expired.
                match self
                    .registry
                    .try_register_publisher(
                        room_id,
                        media_id,
                        &self.local_node_id,
                        &entry.user_id,
                        &self.local_api_address,
                    )
                    .await
                {
                    Ok(true) => {
                        self.refresh_tracked_registration_after_reregister(
                            publisher_key,
                            entry,
                            room_id,
                            media_id,
                        )
                        .await;
                        info!(
                            "Re-registered publisher for room {} / media {}",
                            room_id, media_id
                        );
                    }
                    Ok(false) => {
                        // Entry exists in registry - refresh TTL instead
                        match self
                            .registry
                            .refresh_publisher_ttl(
                                room_id,
                                media_id,
                                &entry.user_id,
                                &self.local_node_id,
                                entry.epoch,
                            )
                            .await
                        {
                            Ok(PublisherRefreshOutcome::Refreshed) => {
                                self.refresh_tracked_registration_after_reregister(
                                    publisher_key,
                                    entry,
                                    room_id,
                                    media_id,
                                )
                                .await;
                                info!(
                                    "Refreshed TTL for publisher room {} / media {}",
                                    room_id, media_id
                                );
                            }
                            Ok(PublisherRefreshOutcome::Missing) => {
                                warn!(
                                    "Publisher room {} / media {} disappeared from registry during re-register TTL refresh",
                                    room_id, media_id
                                );
                                self.try_reregister_after_missing_refresh(
                                    publisher_key,
                                    entry,
                                    room_id,
                                    media_id,
                                )
                                .await;
                            }
                            Ok(PublisherRefreshOutcome::OwnershipChanged) => {
                                warn!(
                                    "Publisher room {} / media {} changed ownership during re-register TTL refresh; cleaning up local publisher",
                                    room_id, media_id
                                );
                                self.cleanup_publisher(
                                    room_id,
                                    media_id,
                                    entry.epoch,
                                    "publisher ownership changed during restart recovery",
                                )
                                .await;
                            }
                            Err(e) => {
                                error!(
                                    "Failed to refresh TTL for publisher room {} / media {}: {}",
                                    room_id, media_id, e
                                );
                            }
                        }
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
        self.clear_restarting();
    }

    /// Cleanup a publisher: remove from local tracking, unregister from Redis,
    /// and notify `StreamHub`. Used by both heartbeat failure and silent publisher
    /// timeout paths.
    async fn cleanup_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
        reason: &str,
    ) {
        let publisher_key = format!("{room_id}:{media_id}");
        info!(
            "Cleaning up publisher room={} media={} epoch={}: {}",
            room_id, media_id, expected_epoch, reason
        );

        // 1. Remove from local tracking only if the current entry is still the
        // same epoch observed when cleanup was scheduled.
        let Some(entry) = self.active_publishers.get(&publisher_key) else {
            debug!(
                "Cleanup skipped for room={} media={} because it is no longer tracked",
                room_id, media_id
            );
            return;
        };
        if entry.epoch != expected_epoch {
            debug!(
                "Cleanup skipped for room={} media={} because local owner advanced from epoch {} to {}",
                room_id, media_id, expected_epoch, entry.epoch
            );
            return;
        }
        let tracked_entry = Arc::clone(entry.value());
        drop(entry);
        let Some((_, removed_entry)) = self
            .active_publishers
            .remove_if(&publisher_key, |_, current| {
                Arc::ptr_eq(current, &tracked_entry) && current.epoch == expected_epoch
            })
        else {
            debug!(
                "Cleanup skipped for room={} media={} because local owner changed before removal",
                room_id, media_id
            );
            return;
        };
        let cleanup = PublisherCleanupContext {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            epoch: removed_entry.epoch,
        };

        // 2. Unregister from Redis immediately (don't wait for TTL), retrying
        // transient failures to avoid leaving a stale epoch fenced key that
        // blocks rapid republish.
        for attempt in 0..UNREGISTER_RETRY_ATTEMPTS {
            match self
                .registry
                .unregister_publisher_if_epoch_matches(
                    &cleanup.room_id,
                    &cleanup.media_id,
                    cleanup.epoch,
                )
                .await
            {
                Ok(()) => {
                    break;
                }
                Err(e) if attempt < UNREGISTER_RETRY_ATTEMPTS - 1 => {
                    let delay_ms = UNREGISTER_RETRY_DELAYS_MS[attempt as usize];
                    warn!(
                        "Failed to unregister publisher from Redis for room {} / media {} with epoch {} on attempt {}/{}: {}. Retrying in {}ms",
                        cleanup.room_id,
                        cleanup.media_id,
                        cleanup.epoch,
                        attempt + 1,
                        UNREGISTER_RETRY_ATTEMPTS,
                        e,
                        delay_ms
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(e) => {
                    warn!(
                        "Failed to unregister publisher from Redis for room {} / media {} with epoch {} after {} attempts: {}. \
                         Leaving Redis cleanup to later reconciliation/TTL expiry.",
                        cleanup.room_id,
                        cleanup.media_id,
                        cleanup.epoch,
                        UNREGISTER_RETRY_ATTEMPTS,
                        e
                    );
                    break;
                }
            }
        }

        // 3. Send UnPublish to StreamHub so subscribers are notified.
        // This is a critical control-plane event and must not be silently dropped.
        // Wait briefly for backpressure to clear, then log a hard failure if even
        // the bounded timeout cannot deliver it.
        let identifier = StreamIdentifier::Rtmp {
            app_name: cleanup.room_id.clone(),
            stream_name: cleanup.media_id.clone(),
        };
        match tokio::time::timeout(
            UNPUBLISH_SEND_TIMEOUT,
            self.hub_event_sender.send(StreamHubEvent::UnPublish {
                identifier: identifier.clone(),
            }),
        )
        .await
        {
            Ok(Ok(())) => {
                info!(
                    "Sent UnPublish event for room {} / media {} ({})",
                    cleanup.room_id, cleanup.media_id, reason
                );
            }
            Ok(Err(e)) => {
                error!("Failed to send UnPublish event for {:?}: {}", identifier, e);
            }
            Err(_) => {
                error!(
                    "Timed out after {}s sending UnPublish event for {:?}",
                    UNPUBLISH_SEND_TIMEOUT.as_secs(),
                    identifier
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
    async fn run_heartbeat_cycle(self: &Arc<Self>) {
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

            // LS-5: Check for silent publisher (no media data for too long).
            // Skip during StreamHub restart to avoid false cleanups while
            // publishers are reconnecting to the new hub instance.
            let idle_secs = entry.idle_secs();
            if idle_secs > self.silent_timeout_secs && !self.is_restarting.load(Ordering::Acquire) {
                warn!(
                    "Silent publisher detected: room={} media={} (no data for {}s, threshold={}s)",
                    room_id, media_id, idle_secs, self.silent_timeout_secs
                );
                self.spawn_cleanup_publisher(
                    room_id.to_string(),
                    media_id.to_string(),
                    entry.epoch,
                    format!("silent publisher timeout ({idle_secs}s idle)"),
                );
                continue;
            }

            // L-01: Pass stored user_id to refresh both publisher TTL and user reverse-index TTL
            let user_id = &entry.user_id;

            // Per-cycle heartbeat failure counting semantics:
            // Within a single cycle, we retry up to MAX_HEARTBEAT_RETRIES times
            // with exponential backoff. The cycle is considered:
            // - SUCCESS: at least one retry returned Ok(())
            // - FAILURE: ALL retries returned Err(...)
            // `consecutive_heartbeat_failures` counts consecutive *cycles* with
            // a FAILURE outcome, not individual retry attempts within a cycle.
            // Reset semantics: reset to 0 when a cycle succeeds (even partially).
            // This means a transient Redis error that resolves within the retry
            // window does NOT accumulate toward the cleanup threshold.
            // Redis timeout (slow response) counts as a cycle failure.
            // To prevent false publisher cleanup due to slow Redis, we separate the
            // publisher-dead threshold (MAX_CONSECUTIVE_HEARTBEAT_FAILURES cycles)
            // from Redis reachability issues. Slow Redis increments this counter;
            // only when it reaches the max do we conclude the publisher is dead.
            let mut cycle_succeeded = false;
            let mut publisher_missing = false;
            let mut last_error: Option<anyhow::Error> = None;
            for attempt in 0..MAX_HEARTBEAT_RETRIES {
                match self
                    .registry
                    .refresh_publisher_ttl(
                        room_id,
                        media_id,
                        user_id,
                        &self.local_node_id,
                        entry.epoch,
                    )
                    .await
                {
                    Ok(PublisherRefreshOutcome::Refreshed) => {
                        cycle_succeeded = true;
                        break;
                    }
                    Ok(PublisherRefreshOutcome::Missing) => {
                        publisher_missing = true;
                        last_error = Some(anyhow::anyhow!(
                            "publisher registration missing from registry"
                        ));
                        break;
                    }
                    Ok(PublisherRefreshOutcome::OwnershipChanged) => {
                        warn!(
                            "Publisher room={} media={} no longer matches local owner/epoch; cleaning up immediately",
                            room_id, media_id
                        );
                        self.spawn_cleanup_publisher(
                            room_id.to_string(),
                            media_id.to_string(),
                            entry.epoch,
                            "publisher ownership changed in registry".to_string(),
                        );
                        cycle_succeeded = true;
                        break;
                    }
                    Err(e) => {
                        if attempt < MAX_HEARTBEAT_RETRIES - 1 {
                            let delay_ms = HEARTBEAT_RETRY_BASE_DELAY_MS * (1 << attempt);
                            warn!(
                                "Heartbeat attempt {} failed for room {} / media {}: {}. Retrying in {}ms",
                                attempt + 1, room_id, media_id, e, delay_ms
                            );
                            last_error = Some(e);
                            sleep(Duration::from_millis(delay_ms)).await;
                        } else {
                            error!(
                                "All {} heartbeat attempts failed for room {} / media {}: {}. \
                                 Incrementing consecutive failure counter.",
                                MAX_HEARTBEAT_RETRIES, room_id, media_id, e
                            );
                            last_error = Some(e);
                        }
                    }
                }
            }

            if cycle_succeeded {
                // Cycle succeeded: reset BOTH failure counters.
                // Any previous partial failures within this cycle are discarded.
                entry
                    .consecutive_heartbeat_failures
                    .store(0, Ordering::Release);
                entry.redis_unreachable_cycles.store(0, Ordering::Release);
                trace!(
                    "Heartbeat cycle succeeded for room {} / media {}",
                    room_id,
                    media_id
                );
            } else {
                // Cycle failed: ALL retries exhausted with errors.
                synctv_core::metrics::livestream::PUBLISHER_HEARTBEAT_FAILURES.inc();

                // Distinguish Redis-unreachable errors from publisher-missing errors.
                // A slow/unreachable Redis should NOT immediately trigger publisher cleanup.
                // Only escalate to cleanup if:
                // (a) Redis is reachable but publisher TTL refresh actually fails
                // (key not found = publisher expired) → uses heartbeat_failures counter
                // (b) Redis is completely unreachable for an extended period
                // → uses redis_unreachable_cycles counter (higher threshold)
                // Use structured redis::ErrorKind matching instead of string comparison
                // to avoid brittle matching against error message text.
                let is_redis_unreachable =
                    last_error.as_ref().is_some_and(is_redis_unreachable_error);

                if is_redis_unreachable {
                    // Redis itself is unreachable — do NOT count toward publisher cleanup threshold.
                    // Use a separate (higher) counter to eventually clean up if Redis stays down.
                    entry
                        .consecutive_heartbeat_failures
                        .store(0, Ordering::Release);
                    let redis_failures = entry
                        .redis_unreachable_cycles
                        .fetch_add(1, Ordering::AcqRel)
                        + 1;
                    if redis_failures >= MAX_CONSECUTIVE_REDIS_UNREACHABLE {
                        error!(
                            "Redis unreachable for {} consecutive heartbeat cycles for room={} media={}. \
                             Cleaning up publisher as last resort (Redis may be permanently down).",
                            redis_failures, room_id, media_id
                        );
                        self.spawn_cleanup_publisher(
                            room_id.to_string(),
                            media_id.to_string(),
                            entry.epoch,
                            format!("Redis unreachable for {redis_failures} consecutive cycles"),
                        );
                    } else {
                        warn!(
                            "Redis unreachable for heartbeat room={} media={} ({}/{} consecutive). \
                             NOT counting toward publisher cleanup threshold.",
                            room_id, media_id, redis_failures, MAX_CONSECUTIVE_REDIS_UNREACHABLE
                        );
                    }
                } else if publisher_missing {
                    entry.redis_unreachable_cycles.store(0, Ordering::Release);
                    let failures = entry
                        .consecutive_heartbeat_failures
                        .fetch_add(1, Ordering::AcqRel)
                        + 1;
                    if failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
                        error!(
                            "Publisher room={} media={} missing from registry for {} consecutive heartbeat cycles, cleaning up",
                            room_id, media_id, failures
                        );
                        self.spawn_cleanup_publisher(
                            room_id.to_string(),
                            media_id.to_string(),
                            entry.epoch,
                            format!(
                                "publisher missing from registry for {failures} consecutive cycles"
                            ),
                        );
                    } else {
                        warn!(
                            "Publisher room={} media={} missing from registry ({}/{} consecutive cycles)",
                            room_id, media_id, failures, MAX_CONSECUTIVE_HEARTBEAT_FAILURES
                        );
                    }
                } else {
                    let failures = entry
                        .consecutive_heartbeat_failures
                        .fetch_add(1, Ordering::AcqRel)
                        + 1;
                    entry.redis_unreachable_cycles.store(0, Ordering::Release);
                    warn!(
                        "Heartbeat cycle failed for room={} media={} due to persistent registry error (counting toward cleanup threshold) ({}/{} consecutive): {}",
                        room_id,
                        media_id,
                        failures,
                        MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
                        last_error
                            .as_ref()
                            .map_or_else(|| "unknown error".to_string(), ToString::to_string)
                    );
                    if failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
                        error!(
                            "Publisher room={} media={} failed heartbeat with persistent registry error for {} consecutive cycles, cleaning up",
                            room_id, media_id, failures
                        );
                        self.spawn_cleanup_publisher(
                            room_id.to_string(),
                            media_id.to_string(),
                            entry.epoch,
                            format!(
                                "persistent registry heartbeat failure for {failures} consecutive cycles"
                            ),
                        );
                    }
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

#[cfg(test)]
mod tests {
    use super::super::MockStreamRegistry;
    use super::*;
    use crate::relay::{ActivePublisherEntry, PublisherInfo};
    use anyhow::Result;
    use chrono::Utc;

    /// Create a test `PublisherManager` with a dummy `StreamHubEventSender`.
    /// Returns the manager and the corresponding receiver so tests can inspect
    /// events sent on heartbeat failure.
    fn test_manager(
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: &str,
    ) -> (
        Arc<PublisherManager>,
        synctv_xiu::streamhub::define::StreamHubEventReceiver,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        (
            Arc::new(PublisherManager::new(registry, node_id.to_string(), tx)),
            rx,
        )
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
        // Pre-register publisher so handle_publish can look up the entry
        registry
            .try_register_publisher("room123", "media456", "test-node-1", "", "")
            .await
            .unwrap();
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
        // Pre-register publisher so handle_publish can look up the entry
        registry
            .try_register_publisher("room123", "media456", "test-node-1", "", "")
            .await
            .unwrap();
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

    /// Helper to insert a publisher entry into the `active_publishers` map.
    fn insert_entry(manager: &PublisherManager, key: &str) {
        manager
            .active_publishers
            .insert(key.to_string(), Arc::new(PublisherEntry::new()));
    }

    async fn insert_registered_entry(
        manager: &PublisherManager,
        registry: &dyn StreamRegistryTrait,
        room_id: &str,
        media_id: &str,
    ) {
        let info = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("registry lookup should succeed")
            .expect("publisher should exist in registry");
        manager.active_publishers.insert(
            format!("{room_id}:{media_id}"),
            Arc::new(PublisherEntry::with_registration(info.user_id, info.epoch)),
        );
    }

    struct RecreateOnReregisterRegistry {
        publisher: tokio::sync::Mutex<Option<PublisherInfo>>,
        next_epoch: AtomicU64,
        expire_before_next_try_register: AtomicBool,
        replace_before_next_try_register: AtomicBool,
        expire_before_next_refresh: AtomicBool,
    }

    impl RecreateOnReregisterRegistry {
        fn new() -> Self {
            Self {
                publisher: tokio::sync::Mutex::new(Some(PublisherInfo {
                    node_id: "test-node".to_string(),
                    api_address: "addr1".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: Utc::now(),
                    epoch: 1,
                })),
                next_epoch: AtomicU64::new(2),
                expire_before_next_try_register: AtomicBool::new(false),
                replace_before_next_try_register: AtomicBool::new(false),
                expire_before_next_refresh: AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl StreamRegistryTrait for RecreateOnReregisterRegistry {
        async fn register_publisher(
            &self,
            room_id: &str,
            media_id: &str,
            node_id: &str,
            _app_name: &str,
            api_address: &str,
        ) -> Result<bool> {
            self.try_register_publisher(room_id, media_id, node_id, "", api_address)
                .await
        }

        async fn try_register_publisher(
            &self,
            _room_id: &str,
            _media_id: &str,
            node_id: &str,
            user_id: &str,
            api_address: &str,
        ) -> Result<bool> {
            let mut publisher = self.publisher.lock().await;
            if self
                .expire_before_next_try_register
                .swap(false, Ordering::AcqRel)
            {
                publisher.take();
            }
            if self
                .replace_before_next_try_register
                .swap(false, Ordering::AcqRel)
            {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
                *publisher = Some(PublisherInfo {
                    node_id: node_id.to_string(),
                    api_address: api_address.to_string(),
                    app_name: "live".to_string(),
                    user_id: user_id.to_string(),
                    started_at: Utc::now(),
                    epoch,
                });
                return Ok(false);
            }

            if publisher.is_some() {
                return Ok(false);
            }

            let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
            *publisher = Some(PublisherInfo {
                node_id: node_id.to_string(),
                api_address: api_address.to_string(),
                app_name: "live".to_string(),
                user_id: user_id.to_string(),
                started_at: Utc::now(),
                epoch,
            });
            Ok(true)
        }

        async fn refresh_publisher_ttl(
            &self,
            _room_id: &str,
            _media_id: &str,
            _user_id: &str,
            _node_id: &str,
            _expected_epoch: u64,
        ) -> Result<PublisherRefreshOutcome> {
            let mut publisher = self.publisher.lock().await;
            if self
                .expire_before_next_refresh
                .swap(false, Ordering::AcqRel)
            {
                publisher.take();
            }
            Ok(if publisher.is_some() {
                PublisherRefreshOutcome::Refreshed
            } else {
                PublisherRefreshOutcome::Missing
            })
        }

        async fn unregister_publisher(&self, _room_id: &str, _media_id: &str) -> Result<()> {
            let mut publisher = self.publisher.lock().await;
            publisher.take();
            Ok(())
        }

        async fn unregister_publisher_if_epoch_matches(
            &self,
            _room_id: &str,
            _media_id: &str,
            expected_epoch: u64,
        ) -> Result<()> {
            let mut publisher = self.publisher.lock().await;
            if publisher
                .as_ref()
                .is_some_and(|current| current.epoch == expected_epoch)
            {
                publisher.take();
            }
            Ok(())
        }

        async fn get_publisher(
            &self,
            _room_id: &str,
            _media_id: &str,
        ) -> Result<Option<PublisherInfo>> {
            let publisher = self.publisher.lock().await;
            if publisher.is_some() {
                self.expire_before_next_try_register
                    .store(true, Ordering::Release);
            }
            Ok(publisher.clone())
        }

        async fn is_stream_active(&self, _room_id: &str, _media_id: &str) -> Result<bool> {
            Ok(self.publisher.lock().await.is_some())
        }

        async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
            Ok(self
                .publisher
                .lock()
                .await
                .clone()
                .into_iter()
                .map(|publisher| ActivePublisherEntry {
                    room_id: "room-reregister".to_string(),
                    media_id: "media-reregister".to_string(),
                    publisher,
                })
                .collect())
        }

        async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
            Ok(if self.publisher.lock().await.is_some() {
                vec![(
                    "room-reregister".to_string(),
                    "media-reregister".to_string(),
                )]
            } else {
                Vec::new()
            })
        }

        async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
            let publisher = self.publisher.lock().await;
            Ok(
                if publisher
                    .as_ref()
                    .is_some_and(|current| current.user_id == user_id)
                {
                    vec![(
                        "room-reregister".to_string(),
                        "media-reregister".to_string(),
                    )]
                } else {
                    Vec::new()
                },
            )
        }

        async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
            let mut publisher = self.publisher.lock().await;
            if publisher
                .as_ref()
                .is_some_and(|current| current.user_id == user_id)
            {
                publisher.take();
            }
            Ok(())
        }

        async fn validate_epoch(
            &self,
            _room_id: &str,
            _media_id: &str,
            epoch: u64,
        ) -> Result<bool> {
            Ok(self
                .publisher
                .lock()
                .await
                .as_ref()
                .is_some_and(|current| current.epoch == epoch))
        }

        async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
            let mut publisher = self.publisher.lock().await;
            if publisher
                .as_ref()
                .is_some_and(|current| current.node_id == node_id)
            {
                publisher.take();
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_reconcile_removes_stale_entries() {
        // Registry has room1:media1 on our node, but NOT room2:media2
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room1", "media1", "test-node", "", "")
            .await
            .unwrap();

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
        registry
            .try_register_publisher("room1", "media1", "other-node", "", "")
            .await
            .unwrap();

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
        registry
            .try_register_publisher("room1", "media1", "test-node", "", "")
            .await
            .unwrap();
        registry
            .try_register_publisher("room2", "media2", "test-node", "", "")
            .await
            .unwrap();

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
    async fn test_reregister_refreshes_local_epoch_after_registry_recreate() {
        let registry = Arc::new(RecreateOnReregisterRegistry::new());
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
            .with_api_address("addr1".to_string());
        manager.active_publishers.insert(
            "room-reregister:media-reregister".to_string(),
            Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
        );

        manager.reregister_all_publishers().await;

        let active_entry = manager
            .active_publishers
            .get("room-reregister:media-reregister")
            .expect("publisher should still be tracked after re-registration");
        assert_eq!(
            active_entry.epoch, 2,
            "successful re-registration should refresh the locally tracked epoch"
        );
        drop(active_entry);

        manager
            .cleanup_publisher("room-reregister", "media-reregister", 2, "test cleanup")
            .await;

        assert!(
            registry
                .get_publisher("room-reregister", "media-reregister")
                .await
                .unwrap()
                .is_none(),
            "cleanup should remove the recreated registry entry using the refreshed epoch"
        );
    }

    #[tokio::test]
    async fn test_reregister_refreshes_local_epoch_after_ttl_only_recovery() {
        let registry = Arc::new(RecreateOnReregisterRegistry::new());
        registry
            .replace_before_next_try_register
            .store(true, Ordering::Release);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
            .with_api_address("addr1".to_string());
        manager.active_publishers.insert(
            "room-reregister:media-reregister".to_string(),
            Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
        );

        manager.reregister_all_publishers().await;

        let active_entry = manager
            .active_publishers
            .get("room-reregister:media-reregister")
            .expect("publisher should still be tracked after TTL-only re-registration");
        assert_eq!(
            active_entry.epoch, 2,
            "TTL-only recovery should still refresh the locally tracked epoch"
        );
        drop(active_entry);

        manager
            .cleanup_publisher("room-reregister", "media-reregister", 2, "test cleanup")
            .await;

        assert!(
            registry
                .get_publisher("room-reregister", "media-reregister")
                .await
                .unwrap()
                .is_none(),
            "cleanup should remove the live registry entry after TTL-only recovery"
        );
    }

    #[tokio::test]
    async fn test_reregister_recreates_entry_when_ttl_refresh_reports_missing() {
        let registry = Arc::new(RecreateOnReregisterRegistry::new());
        registry
            .expire_before_next_refresh
            .store(true, Ordering::Release);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
            .with_api_address("addr1".to_string());
        manager.active_publishers.insert(
            "room-reregister:media-reregister".to_string(),
            Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
        );

        manager.reregister_all_publishers().await;

        let recreated = registry
            .get_publisher("room-reregister", "media-reregister")
            .await
            .unwrap()
            .expect("restart recovery should recreate the missing registry entry");
        assert_eq!(recreated.node_id, "test-node");
        assert_eq!(recreated.epoch, 2);

        let active_entry = manager
            .active_publishers
            .get("room-reregister:media-reregister")
            .expect("publisher should still be tracked after recovery");
        assert_eq!(
            active_entry.epoch, 2,
            "recovery should refresh the tracked epoch after recreating a missing entry"
        );
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
        // Pre-register publisher so handle_publish can look up the entry
        registry
            .try_register_publisher("room1", "media1", "test-node", "", "")
            .await
            .unwrap();
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

    /// Verify that handle_publish returns an error when Redis fails,
    /// ensuring the stream is rejected (fail-closed behavior).
    #[tokio::test]
    async fn test_handle_publish_fails_closed_on_redis_failure() {
        let registry = Arc::new(MockStreamRegistry::new());
        // Pre-register publisher so it exists
        registry
            .try_register_publisher("room123", "media456", "test-node-1", "user1", "")
            .await
            .unwrap();

        // Enable Redis failure simulation
        registry.set_fail_get_publisher(true);

        let (manager, _rx) = test_manager(registry, "test-node-1");

        let identifier = StreamIdentifier::Rtmp {
            app_name: "room123".to_string(),
            stream_name: "media456".to_string(),
        };

        // handle_publish should return Err when Redis fails
        let result = manager.handle_publish(identifier).await;
        assert!(
            result.is_err(),
            "Stream should be rejected on Redis failure"
        );

        // Verify publisher was NOT tracked (fail-closed: no untracked publisher)
        assert!(
            !manager.active_publishers.contains_key("room123:media456"),
            "Publisher should not be tracked when Redis fails"
        );
    }

    /// Verify that the broadcast event handler propagates the error from
    /// handle_publish when Redis fails, so the stream hub can reject the connection.
    #[tokio::test]
    async fn test_broadcast_event_propagates_redis_failure() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room1", "media1", "test-node", "user1", "")
            .await
            .unwrap();

        // Enable Redis failure simulation
        registry.set_fail_get_publisher(true);

        let (manager, _rx) = test_manager(registry, "test-node");

        let event = synctv_xiu::streamhub::define::BroadcastEvent::Publish {
            identifier: StreamIdentifier::Rtmp {
                app_name: "room1".to_string(),
                stream_name: "media1".to_string(),
            },
            pub_type: synctv_xiu::streamhub::define::PublishType::RtmpPush,
        };

        let result = manager.handle_broadcast_event(event).await;
        assert!(
            result.is_err(),
            "Broadcast event handler should propagate Redis failure"
        );
    }

    #[tokio::test]
    async fn test_record_activity_nonexistent_publisher() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry, "test-node");

        // Should not panic when recording activity for a publisher that doesn't exist
        manager.record_publisher_activity("nonexistent", "publisher");
    }

    #[tokio::test]
    async fn test_cleanup_publisher_waits_for_unpublish_backpressure() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx);

        registry
            .try_register_publisher(
                "room-backpressure",
                "media-backpressure",
                "test-node",
                "user1",
                "",
            )
            .await
            .unwrap();
        manager.active_publishers.insert(
            "room-backpressure:media-backpressure".to_string(),
            Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
        );

        manager
            .hub_event_sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: StreamIdentifier::Rtmp {
                    app_name: "occupied".to_string(),
                    stream_name: "occupied".to_string(),
                },
            })
            .expect("fill channel to create backpressure");

        let cleanup =
            manager.cleanup_publisher("room-backpressure", "media-backpressure", 1, "test");
        let delayed_recv = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = rx.recv().await;
            rx.recv().await
        };

        let ((), received) = tokio::join!(cleanup, delayed_recv);
        let Some(StreamHubEvent::UnPublish { identifier }) = received else {
            panic!("expected an UnPublish event after backpressure clears");
        };

        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-backpressure".to_string(),
                stream_name: "media-backpressure".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn test_cleanup_publisher_uses_epoch_fenced_unregister() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, mut rx) = test_manager(registry.clone(), "test-node");

        registry
            .try_register_publisher("room-fence", "media-fence", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let original = registry
            .get_publisher("room-fence", "media-fence")
            .await
            .unwrap()
            .unwrap();

        manager.active_publishers.insert(
            "room-fence:media-fence".to_string(),
            Arc::new(PublisherEntry::with_registration(
                "user1".to_string(),
                original.epoch,
            )),
        );

        registry
            .unregister_publisher("room-fence", "media-fence")
            .await
            .unwrap();
        registry
            .try_register_publisher("room-fence", "media-fence", "other-node", "user2", "addr2")
            .await
            .unwrap();
        let replacement = registry
            .get_publisher("room-fence", "media-fence")
            .await
            .unwrap()
            .expect("replacement owner should exist");
        manager.active_publishers.insert(
            "room-fence:media-fence".to_string(),
            Arc::new(PublisherEntry::with_registration(
                "user2".to_string(),
                replacement.epoch,
            )),
        );

        manager
            .cleanup_publisher(
                "room-fence",
                "media-fence",
                original.epoch,
                "stale local owner",
            )
            .await;

        let current = registry
            .get_publisher("room-fence", "media-fence")
            .await
            .unwrap()
            .expect("new owner should still exist");
        assert_eq!(current.node_id, "other-node");
        assert_eq!(current.epoch, replacement.epoch);
        let active_entry = manager
            .active_publishers
            .get("room-fence:media-fence")
            .expect("replacement publisher should still be tracked");
        assert_eq!(active_entry.epoch, replacement.epoch);

        assert!(
            rx.try_recv().is_err(),
            "cleanup must not unpublish a replacement publisher"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_cleanup_publisher_retries_epoch_fenced_unregister() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, mut rx) = test_manager(registry.clone(), "test-node");

        registry
            .try_register_publisher("room-retry", "media-retry", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let original = registry
            .get_publisher("room-retry", "media-retry")
            .await
            .unwrap()
            .unwrap();
        manager.active_publishers.insert(
            "room-retry:media-retry".to_string(),
            Arc::new(PublisherEntry::with_registration(
                "user1".to_string(),
                original.epoch,
            )),
        );
        registry.set_fail_unregister_if_epoch_matches_times(2);

        manager
            .cleanup_publisher("room-retry", "media-retry", original.epoch, "retry cleanup")
            .await;

        assert!(
            !manager
                .active_publishers
                .contains_key("room-retry:media-retry"),
            "cleanup should remove the local publisher entry"
        );

        let event = rx.recv().await.expect("cleanup should emit unpublish");
        let StreamHubEvent::UnPublish { identifier } = event else {
            panic!("expected unpublish event");
        };
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-retry".to_string(),
                stream_name: "media-retry".to_string(),
            }
        );

        assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
        assert!(
            registry
                .get_publisher("room-retry", "media-retry")
                .await
                .unwrap()
                .is_none(),
            "cleanup should clear the stale registry entry after retrying"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_cleanup_publisher_survives_brief_extended_redis_outage() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, mut rx) = test_manager(registry.clone(), "test-node");

        registry
            .try_register_publisher(
                "room-recover",
                "media-recover",
                "test-node",
                "user1",
                "addr1",
            )
            .await
            .unwrap();
        let original = registry
            .get_publisher("room-recover", "media-recover")
            .await
            .unwrap()
            .unwrap();
        manager.active_publishers.insert(
            "room-recover:media-recover".to_string(),
            Arc::new(PublisherEntry::with_registration(
                "user1".to_string(),
                original.epoch,
            )),
        );
        registry.set_fail_unregister_if_epoch_matches_times(2);

        manager
            .cleanup_publisher(
                "room-recover",
                "media-recover",
                original.epoch,
                "temporary redis outage",
            )
            .await;

        let event = rx.recv().await.expect("cleanup should emit unpublish");
        let StreamHubEvent::UnPublish { identifier } = event else {
            panic!("expected unpublish event");
        };
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-recover".to_string(),
                stream_name: "media-recover".to_string(),
            }
        );

        assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
        assert!(
            registry
                .get_publisher("room-recover", "media-recover")
                .await
                .unwrap()
                .is_none(),
            "cleanup should eventually clear the stale registry entry after Redis recovers"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_heartbeat_cleanup_runs_in_background_without_blocking_cycle() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room-bg", "media-bg", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-bg", "media-bg")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        let entry = Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        ));
        entry
            .consecutive_heartbeat_failures
            .store(MAX_CONSECUTIVE_HEARTBEAT_FAILURES - 1, Ordering::Release);
        manager
            .active_publishers
            .insert("room-bg:media-bg".to_string(), Arc::clone(&entry));

        registry.set_fail_refresh_publisher_ttl_with_response_error(true);
        registry.set_fail_unregister_if_epoch_matches_times(2);

        let heartbeat = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager.run_heartbeat_cycle().await;
            }
        });
        let observe = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(HEARTBEAT_RETRY_BASE_DELAY_MS + 1)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(
                (HEARTBEAT_RETRY_BASE_DELAY_MS * 2) + 1,
            ))
            .await;
            tokio::task::yield_now().await;
            assert!(
                heartbeat.is_finished(),
                "heartbeat cycle should finish after the small heartbeat retry budget without waiting for cleanup retry backoff"
            );
            tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[0] + 1)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[1] + 1)).await;
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            let event = rx
                .recv()
                .await
                .expect("heartbeat cleanup should emit unpublish");
            let StreamHubEvent::UnPublish { identifier } = event else {
                panic!("expected unpublish event");
            };
            assert_eq!(
                identifier,
                StreamIdentifier::Rtmp {
                    app_name: "room-bg".to_string(),
                    stream_name: "media-bg".to_string(),
                }
            );
            assert!(
                !manager.active_publishers.contains_key("room-bg:media-bg"),
                "background cleanup should eventually remove local state"
            );
            assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
        };

        observe.await;
        heartbeat
            .await
            .expect("heartbeat task should complete successfully");
    }

    #[tokio::test(start_paused = true)]
    async fn test_redis_unreachable_does_not_cleanup_active_publisher() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room-redis", "media-redis", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-redis", "media-redis")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        manager.active_publishers.insert(
            "room-redis:media-redis".to_string(),
            Arc::new(PublisherEntry::with_registration(
                "user1".to_string(),
                current.epoch,
            )),
        );

        registry.set_fail_refresh_publisher_ttl(true);

        for _ in 0..MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
            manager.run_heartbeat_cycle().await;
            tokio::task::yield_now().await;
        }

        assert!(
            manager
                .active_publishers
                .contains_key("room-redis:media-redis"),
            "Redis connectivity failures must not clear active publishers"
        );
        assert!(
            rx.try_recv().is_err(),
            "Redis connectivity failures must not emit unpublish events"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_wrapped_redis_timeout_counts_as_unreachable_cycle() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher(
                "room-timeout",
                "media-timeout",
                "test-node",
                "user1",
                "addr1",
            )
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-timeout", "media-timeout")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        let entry = Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        ));
        manager
            .active_publishers
            .insert("room-timeout:media-timeout".to_string(), Arc::clone(&entry));

        let timeout_error = anyhow::anyhow!("Redis operation timed out after 5s");
        assert!(
            is_redis_unreachable_error(&timeout_error),
            "wrapped Redis timeout errors should classify as registry-unreachable"
        );

        registry.set_fail_refresh_publisher_ttl_with_wrapped_timeout(true);
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert_eq!(
            entry.redis_unreachable_cycles.load(Ordering::Acquire),
            1,
            "wrapped Redis timeouts should increment redis_unreachable_cycles"
        );
        assert_eq!(
            entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
            0,
            "wrapped Redis timeouts must not count as publisher-missing failures"
        );
        assert!(
            manager
                .active_publishers
                .contains_key("room-timeout:media-timeout"),
            "last-resort cleanup must not trigger before the redis-unreachable threshold"
        );
        assert!(
            rx.try_recv().is_err(),
            "wrapped Redis timeouts must not emit unpublish before threshold"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_redis_outage_last_resort_cleanup_runs_in_background() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room-outage", "media-outage", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-outage", "media-outage")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        let entry = Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        ));
        manager
            .active_publishers
            .insert("room-outage:media-outage".to_string(), Arc::clone(&entry));

        registry.set_fail_refresh_publisher_ttl(true);
        registry.set_fail_unregister_if_epoch_matches_times(2);

        for _ in 1..MAX_CONSECUTIVE_REDIS_UNREACHABLE {
            manager.run_heartbeat_cycle().await;
            tokio::task::yield_now().await;
        }

        let heartbeat = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager.run_heartbeat_cycle().await;
            }
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(HEARTBEAT_RETRY_BASE_DELAY_MS + 1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(
            (HEARTBEAT_RETRY_BASE_DELAY_MS * 2) + 1,
        ))
        .await;
        tokio::task::yield_now().await;

        assert!(
            heartbeat.is_finished(),
            "redis-outage last-resort cleanup should not block the heartbeat loop on unregister retry backoff"
        );
        heartbeat
            .await
            .expect("heartbeat task should complete successfully");

        tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[0] + 1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[1] + 1)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let event = rx
            .recv()
            .await
            .expect("background cleanup should eventually emit unpublish");
        let StreamHubEvent::UnPublish { identifier } = event else {
            panic!("expected unpublish event");
        };
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-outage".to_string(),
                stream_name: "media-outage".to_string(),
            }
        );
        assert!(
            !manager
                .active_publishers
                .contains_key("room-outage:media-outage"),
            "background cleanup should eventually remove local state after redis outage threshold"
        );
        assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn test_persistent_non_io_registry_failures_still_trigger_cleanup() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher(
                "room-response",
                "media-response",
                "test-node",
                "user1",
                "addr1",
            )
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-response", "media-response")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        let entry = Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        ));
        manager.active_publishers.insert(
            "room-response:media-response".to_string(),
            Arc::clone(&entry),
        );

        registry.set_fail_refresh_publisher_ttl_with_response_error(true);

        for expected_failures in 1..MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
            manager.run_heartbeat_cycle().await;
            tokio::task::yield_now().await;

            assert_eq!(
                entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
                expected_failures,
                "persistent non-I/O registry failures should count toward cleanup threshold"
            );
            assert_eq!(
                entry.redis_unreachable_cycles.load(Ordering::Acquire),
                0,
                "persistent non-I/O registry failures must not increment redis_unreachable_cycles"
            );
            assert!(
                manager
                    .active_publishers
                    .contains_key("room-response:media-response"),
                "cleanup should wait until the heartbeat-failure threshold is reached"
            );
            assert!(
                rx.try_recv().is_err(),
                "cleanup must not emit unpublish before the threshold is reached"
            );
        }

        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert!(
            !manager
                .active_publishers
                .contains_key("room-response:media-response"),
            "persistent non-I/O registry failures should eventually trigger cleanup"
        );
        let event = rx
            .recv()
            .await
            .expect("cleanup should emit unpublish at threshold");
        let StreamHubEvent::UnPublish { identifier } = event else {
            panic!("expected unpublish event");
        };
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-response".to_string(),
                stream_name: "media-response".to_string(),
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_switching_between_failure_classes_resets_consecutive_counters() {
        let registry = Arc::new(MockStreamRegistry::new());
        registry
            .try_register_publisher("room-switch", "media-switch", "test-node", "user1", "addr1")
            .await
            .unwrap();
        let current = registry
            .get_publisher("room-switch", "media-switch")
            .await
            .unwrap()
            .unwrap();

        let (manager, mut rx) = test_manager(registry.clone(), "test-node");
        let entry = Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        ));
        manager
            .active_publishers
            .insert("room-switch:media-switch".to_string(), Arc::clone(&entry));

        registry
            .unregister_publisher("room-switch", "media-switch")
            .await
            .unwrap();
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert_eq!(
            entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
            2,
            "publisher-missing failures should count consecutively"
        );

        registry.set_fail_refresh_publisher_ttl(true);
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert_eq!(
            entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
            0,
            "redis-unreachable failure must reset publisher-missing streak"
        );
        assert_eq!(
            entry.redis_unreachable_cycles.load(Ordering::Acquire),
            1,
            "redis-unreachable failure should start its own consecutive streak"
        );

        registry.set_fail_refresh_publisher_ttl(false);
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert_eq!(
            entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
            1,
            "publisher-missing streak should restart after a different failure class"
        );
        assert_eq!(
            entry.redis_unreachable_cycles.load(Ordering::Acquire),
            0,
            "publisher-missing failure must reset redis-unreachable streak"
        );
        assert!(
            manager
                .active_publishers
                .contains_key("room-switch:media-switch"),
            "mixed failure classes must not trigger premature cleanup"
        );
        assert!(
            rx.try_recv().is_err(),
            "mixed failure classes must not emit unpublish"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_start_stops_heartbeat_and_sync_when_broadcast_channel_closes() {
        let registry = Arc::new(MockStreamRegistry::with_publishers(
            std::collections::HashMap::from([(
                ("room1".to_string(), "media1".to_string()),
                PublisherInfo {
                    node_id: "test-node".to_string(),
                    api_address: "127.0.0.1:50051".to_string(),
                    app_name: "live".to_string(),
                    user_id: "user1".to_string(),
                    started_at: Utc::now(),
                    epoch: 1,
                },
            )]),
        ));
        let (hub_tx, _hub_rx) = tokio::sync::mpsc::channel(16);
        let manager = Arc::new(PublisherManager::new(
            registry.clone(),
            "test-node".to_string(),
            hub_tx,
        ));

        manager.active_publishers.insert(
            "room1:media1".to_string(),
            Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
        );

        let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel(16);
        let start_handle = tokio::spawn(Arc::clone(&manager).start(broadcast_rx));

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(HEARTBEAT_INTERVAL_SECS + 1)).await;
        tokio::task::yield_now().await;

        let refresh_before_shutdown = registry.refresh_call_count();
        assert!(
            refresh_before_shutdown >= 1,
            "heartbeat loop should refresh tracked publishers while manager is running"
        );

        drop(broadcast_tx);
        start_handle.await.unwrap();

        let sync_before_shutdown = registry.list_active_publishers_call_count();

        tokio::time::advance(Duration::from_secs(
            PERIODIC_SYNC_INTERVAL_SECS + HEARTBEAT_INTERVAL_SECS + 5,
        ))
        .await;
        tokio::task::yield_now().await;

        assert_eq!(
            registry.refresh_call_count(),
            refresh_before_shutdown,
            "heartbeat task must stop when publisher manager exits"
        );
        assert_eq!(
            registry.list_active_publishers_call_count(),
            sync_before_shutdown,
            "periodic sync task must stop when publisher manager exits"
        );
    }

    // Memory leak tests: reregister_all_publishers should clean up zombie entries

    /// Test that `reregister_all_publishers` cleans up stale entries from local `DashMap`
    /// when the registry entry no longer exists.
    ///
    /// Scenario:
    /// 1. Publisher is tracked locally (entry in `DashMap`)
    /// 2. Registry entry expires or is removed (e.g., TTL, external cleanup)
    /// 3. `UnPublish` event is lost - `handle_unpublish` never called
    /// 4. `reregister_all_publishers` is called
    ///
    /// Expected: The stale entry should be removed from `DashMap`.
    #[tokio::test]
    async fn test_reregister_removes_stale_entry_when_registry_entry_gone() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry.clone(), "test-node");

        // 1. Register a publisher
        registry
            .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();

        // 2. Track it locally (simulating Publish event)
        insert_entry(&manager, "room1:media1");

        // 3. Verify the entry is in DashMap
        assert_eq!(manager.active_publishers.len(), 1);

        // 4. Simulate registry entry being removed (TTL expiry, external cleanup)
        // but UnPublish event is lost
        registry
            .unregister_publisher("room1", "media1")
            .await
            .unwrap();

        // 5. Call reregister_all_publishers - this should remove the stale entry
        manager.reregister_all_publishers().await;

        // 6. Verify the stale entry is removed
        assert!(
            manager.active_publishers.is_empty(),
            "Stale entry should be removed from DashMap after reregister"
        );
    }

    /// Test that `reregister_all_publishers` removes local entry when
    /// the publisher is now owned by another node.
    #[tokio::test]
    async fn test_reregister_removes_entry_taken_over_by_other_node() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry.clone(), "test-node");

        // 1. Register a publisher
        registry
            .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();

        // 2. Track it locally
        insert_entry(&manager, "room1:media1");

        // 3. Verify tracking
        assert_eq!(manager.active_publishers.len(), 1);

        // 4. Simulate takeover by another node (ownership change)
        registry
            .unregister_publisher("room1", "media1")
            .await
            .unwrap();
        registry
            .try_register_publisher("room1", "media1", "other-node", "user1", "other:50051")
            .await
            .unwrap();

        // 5. reregister should remove our local entry since we no longer own it
        manager.reregister_all_publishers().await;

        // 6. Local tracking should be empty
        assert!(
            manager.active_publishers.is_empty(),
            "Entry should be removed since other node took over"
        );
    }

    /// Test that `reregister_all_publishers` keeps entries that are still
    /// owned by this node in the registry.
    #[tokio::test]
    async fn test_reregister_keeps_entries_owned_by_this_node() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry.clone(), "test-node");

        // 1. Register a publisher
        registry
            .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();

        // 2. Track it locally
        insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await;

        // 3. Verify tracking
        assert_eq!(manager.active_publishers.len(), 1);

        // 4. reregister should keep this entry since we still own it
        manager.reregister_all_publishers().await;

        // 5. Entry should still be tracked
        assert_eq!(
            manager.active_publishers.len(),
            1,
            "Entry should still be tracked since we own it"
        );
    }

    /// Test that reregister correctly handles a mix of:
    /// - Publishers still owned by this node (keep)
    /// - Publishers taken over by other nodes (remove)
    /// - Publishers no longer in registry (remove)
    #[tokio::test]
    async fn test_reregister_partial_cleanup() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry.clone(), "test-node");

        // 1. Register three publishers
        registry
            .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();
        registry
            .try_register_publisher("room2", "media2", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();
        registry
            .try_register_publisher("room3", "media3", "test-node", "user1", "localhost:50051")
            .await
            .unwrap();

        // 2. Track all three locally
        insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await;
        insert_registered_entry(&manager, registry.as_ref(), "room2", "media2").await;
        insert_registered_entry(&manager, registry.as_ref(), "room3", "media3").await;

        // 3. Verify all tracked
        assert_eq!(manager.active_publishers.len(), 3);

        // 4. Remove room2 from registry (TTL expired)
        registry
            .unregister_publisher("room2", "media2")
            .await
            .unwrap();

        // 5. Transfer room3 to another node
        registry
            .unregister_publisher("room3", "media3")
            .await
            .unwrap();
        registry
            .try_register_publisher("room3", "media3", "other-node", "user1", "other:50051")
            .await
            .unwrap();

        // 6. reregister should:
        // - Keep room1 (we still own it)
        // - Remove room2 (not in registry)
        // - Remove room3 (owned by other node)
        manager.reregister_all_publishers().await;

        // 7. Only room1 should remain
        assert_eq!(
            manager.active_publishers.len(),
            1,
            "Only room1 should remain"
        );
        assert!(
            manager.active_publishers.contains_key("room1:media1"),
            "room1:media1 should still be tracked"
        );
    }

    /// Regression test for the `DashMap` memory leak.
    ///
    /// This test simulates the exact scenario that causes the memory leak:
    /// 1. Multiple publishers are tracked locally
    /// 2. All registry entries expire or are removed
    /// 3. `UnPublish` events are lost (e.g., broadcast channel lag)
    /// 4. Without the fix, entries would remain in `DashMap` forever
    /// 5. With the fix, `reregister_all_publishers` cleans them up
    #[tokio::test]
    async fn test_memory_leak_regression_zombie_cleanup() {
        let registry = Arc::new(MockStreamRegistry::new());
        let (manager, _rx) = test_manager(registry.clone(), "test-node");

        // 1. Create 10 publishers
        for i in 0..10 {
            let room = format!("room{i}");
            let media = format!("media{i}");
            registry
                .try_register_publisher(&room, &media, "test-node", "user1", "localhost:50051")
                .await
                .unwrap();
            insert_entry(&manager, &format!("{room}:{media}"));
        }

        // 2. Verify all 10 are tracked
        assert_eq!(manager.active_publishers.len(), 10);

        // 3. Remove all from registry (simulating mass TTL expiry)
        for i in 0..10 {
            let room = format!("room{i}");
            let media = format!("media{i}");
            registry.unregister_publisher(&room, &media).await.unwrap();
        }

        // 4. reregister should clean up all zombie entries
        manager.reregister_all_publishers().await;

        // 5. Verify DashMap is empty (no memory leak)
        assert!(
            manager.active_publishers.is_empty(),
            "All zombie entries should be cleaned up - no memory leak"
        );
    }
}
