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
use super::registry_trait::{
    PublisherRefreshOutcome, PublisherRefreshRequest, StreamRegistryTrait,
    PUBLISHER_REFRESH_BATCH_SIZE,
};
use crate::util::{unix_now_secs, validate_stream_ids};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use synctv_xiu::streamhub::{
    define::{BroadcastEventReceiver, StreamHubEvent, StreamHubEventSender},
    stream::StreamIdentifier,
};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

/// Number of consecutive heartbeat cycles that can fail before the local
/// publisher is cleaned up.
const MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 3;
/// Maximum time to wait for delivering a critical `UnPublish` control event.
const UNPUBLISH_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Duration after which a publisher that hasn't sent any media data is
/// considered silent and should be cleaned up.
const SILENT_PUBLISHER_TIMEOUT_SECS: u64 = 60;

/// Interval for periodic registry-local consistency sync.
const PERIODIC_SYNC_INTERVAL_SECS: u64 = 300;
const MAINTENANCE_COMMAND_CAPACITY: usize = 16;

enum PublisherMaintenanceCommand {
    Reregister { done: oneshot::Sender<()> },
}

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
    /// Number of consecutive heartbeat cycles where TTL refresh failed.
    /// Reset to 0 on any successful heartbeat. Only triggers cleanup when
    /// this reaches `MAX_CONSECUTIVE_HEARTBEAT_FAILURES`.
    consecutive_heartbeat_failures: std::sync::atomic::AtomicU32,
    /// User ID from the publisher registration, used for reverse-index TTL refresh.
    user_id: String,
    /// Publisher epoch captured from the registry when tracking started.
    epoch: u64,
}

impl PublisherEntry {
    // Used in test helpers to insert entries without a real registry lookup.
    #[cfg(test)]
    fn new() -> Self {
        Self {
            last_active_secs: AtomicU64::new(unix_now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            user_id: String::new(),
            epoch: 0,
        }
    }

    fn with_registration(user_id: String, epoch: u64) -> Self {
        Self {
            last_active_secs: AtomicU64::new(unix_now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
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
            user_id,
            epoch,
        }
    }

    fn touch(&self) {
        self.last_active_secs
            .store(unix_now_secs(), Ordering::Release);
    }

    fn idle_secs(&self) -> u64 {
        unix_now_secs().saturating_sub(self.last_active_secs.load(Ordering::Acquire))
    }
}

fn publisher_key(room_id: &str, media_id: &str) -> anyhow::Result<String> {
    validate_stream_ids(room_id, media_id)?;
    Ok(format!("{room_id}:{media_id}"))
}

fn parse_publisher_key(key: &str) -> Option<(&str, &str)> {
    let (room_id, media_id) = key.split_once(':')?;
    if media_id.contains(':') || validate_stream_ids(room_id, media_id).is_err() {
        return None;
    }
    Some((room_id, media_id))
}

async fn run_until_cancelled<F>(cancel: &CancellationToken, future: F) -> bool
where
    F: std::future::Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = future => true,
    }
}

/// Publisher manager that listens to `StreamHub` events
pub(crate) struct PublisherManager {
    registry: Arc<dyn StreamRegistryTrait>,
    local_node_id: String,
    /// Advertised cluster listener address used for re-registration after restart.
    local_cluster_address: String,
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
    maintenance_tx: mpsc::Sender<PublisherMaintenanceCommand>,
    maintenance_rx: tokio::sync::Mutex<Option<mpsc::Receiver<PublisherMaintenanceCommand>>>,
    sync_notify: Notify,
}

impl PublisherManager {
    /// Create a new `PublisherManager` with a shared restarting flag.
    ///
    /// This allows external code (e.g., `StreamHub` restart loop) to share the
    /// restarting flag and set it before cleanup operations begin, preventing
    /// false silent-publisher detections during the restart window.
    pub(crate) fn with_restarting_flag(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        hub_event_sender: StreamHubEventSender,
        is_restarting: Arc<AtomicBool>,
    ) -> Self {
        let (maintenance_tx, maintenance_rx) = mpsc::channel(MAINTENANCE_COMMAND_CAPACITY);
        Self {
            registry,
            local_node_id,
            local_cluster_address: String::new(),
            active_publishers: Arc::new(DashMap::new()),
            hub_event_sender,
            lag_event_count: AtomicU64::new(0),
            silent_timeout_secs: SILENT_PUBLISHER_TIMEOUT_SECS,
            is_restarting,
            maintenance_tx,
            maintenance_rx: tokio::sync::Mutex::new(Some(maintenance_rx)),
            sync_notify: Notify::new(),
        }
    }

    /// Set the advertised cluster listener address for this node.
    /// Used during re-registration after `StreamHub` restart (L-05).
    #[must_use]
    pub(crate) fn with_cluster_address(mut self, cluster_address: String) -> Self {
        self.local_cluster_address = cluster_address;
        self
    }

    /// Mark the manager as restarting to suppress silent-publisher cleanup
    /// during the `StreamHub` restart window.
    fn set_restarting(&self) {
        self.is_restarting.store(true, Ordering::Release);
    }

    /// Clear the restarting flag after re-registration completes.
    fn clear_restarting(&self) {
        self.is_restarting.store(false, Ordering::Release);
    }

    /// Returns the list of active publishers as `(app_name, stream_name)` pairs.
    ///
    /// Used by the HLS remuxer for post-lag reconciliation: after a broadcast
    /// lag event, the remuxer queries this list and starts HLS handlers for
    /// any active publishers that don't already have a running handler.
    pub(crate) fn active_publisher_streams(&self) -> Vec<(String, String)> {
        self.active_publishers
            .iter()
            .filter_map(|entry| {
                parse_publisher_key(entry.key())
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
    pub(crate) fn record_publisher_activity(&self, room_id: &str, media_id: &str) {
        let Ok(key) = publisher_key(room_id, media_id) else {
            warn!(
                room_id = room_id,
                media_id = media_id,
                "Ignoring publisher activity for invalid stream identifiers"
            );
            return;
        };
        if let Some(entry) = self.active_publishers.get(&key) {
            entry.touch();
        }
    }

    /// Start listening to `StreamHub` broadcast events
    pub(crate) async fn start(self: Arc<Self>, mut event_receiver: BroadcastEventReceiver) {
        if self.local_cluster_address.is_empty() {
            warn!(
                "PublisherManager started with empty cluster_address. \
                 Re-registration after StreamHub restart will use an empty address, \
                 preventing cross-node HLS proxy from reaching this node. \
                 Set cluster_address in LivestreamConfig."
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

        let maintenance_rx = {
            let mut receiver = self.maintenance_rx.lock().await;
            receiver.take()
        };
        let Some(maintenance_rx) = maintenance_rx else {
            error!("Publisher manager maintenance worker was already started");
            return;
        };
        let maintenance_cancel = CancellationToken::new();
        let maintenance_handle = tokio::spawn(
            Arc::clone(&self).run_maintenance_worker(maintenance_rx, maintenance_cancel.clone()),
        );

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
                         Scheduling active publisher reconciliation."
                    );
                    self.schedule_registry_sync();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    error!("Broadcast channel closed");
                    break;
                }
            }
        }

        maintenance_cancel.cancel();
        if let Err(error) = maintenance_handle.await {
            error!(error = %error, "Publisher maintenance worker failed during shutdown");
        }
        warn!("Publisher manager stopped");
    }

    async fn run_maintenance_worker(
        self: Arc<Self>,
        mut receiver: mpsc::Receiver<PublisherMaintenanceCommand>,
        cancel: CancellationToken,
    ) {
        let mut heartbeat_interval = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sync_interval = interval(Duration::from_secs(PERIODIC_SYNC_INTERVAL_SECS));
        sync_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        sync_interval.tick().await;

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                command = receiver.recv() => {
                    match command {
                        Some(PublisherMaintenanceCommand::Reregister { done }) => {
                            if !run_until_cancelled(&cancel, self.reregister_all_publishers_once()).await {
                                break;
                            }
                            let _ = done.send(());
                        }
                        None => break,
                    }
                }
                () = self.sync_notify.notified() => {
                    if !run_until_cancelled(&cancel, self.run_periodic_sync_once()).await {
                        break;
                    }
                }
                _ = heartbeat_interval.tick() => {
                    if !run_until_cancelled(&cancel, self.run_heartbeat_cycle()).await {
                        break;
                    }
                }
                _ = sync_interval.tick() => {
                    debug!(
                        "Running periodic registry-local sync (interval={}s)",
                        PERIODIC_SYNC_INTERVAL_SECS
                    );
                    if !run_until_cancelled(&cancel, self.run_periodic_sync_once()).await {
                        break;
                    }
                }
            }
        }

        debug!("Publisher maintenance worker stopped");
    }

    fn schedule_registry_sync(&self) {
        self.sync_notify.notify_one();
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
                // User RTMP pushes are tracked here because RTMP auth registered
                // them in Redis before StreamHub publish. ExternalPull streams are
                // registered and cleaned up by ExternalPublishManager, while
                // RtmpRelay streams are owned by their origin node.
                if pub_type != synctv_xiu::streamhub::define::PublishType::RtmpPush {
                    debug!(
                        pub_type = ?pub_type,
                        identifier = ?identifier,
                        "Ignoring non-user publish event for heartbeat tracking"
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
        let StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        } = identifier;

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
        // Query registry to get user_id for heartbeat TTL refresh.
        let publisher_key = publisher_key(&room_id, &media_id)?;
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
        } = identifier;

        info!(
            "RTMP UnPublish event: app_name={}, stream_name={}",
            app_name, stream_name
        );

        // StreamIdentifier format: app_name=room_id, stream_name=media_id
        let room_id = app_name;
        let media_id = stream_name;

        // Look up by composite key (room_id:media_id)
        let publisher_key = publisher_key(&room_id, &media_id)?;
        if let Some((_, entry)) = self.active_publishers.remove(&publisher_key) {
            // Unregister from Redis only if this event still matches the tracked
            // publisher epoch. A delayed UnPublish for a previous RTMP session
            // must not remove a newer publisher that already replaced it.
            if let Err(e) = self
                .registry
                .unregister_publisher_if_epoch_matches(&room_id, &media_id, entry.epoch)
                .await
            {
                error!(
                    "Failed to unregister publisher for room {} / media {} with epoch {}: {}",
                    room_id, media_id, entry.epoch, e
                );
            } else {
                info!(
                    "Unregistered publisher for room {} / media {} with epoch {}",
                    room_id, media_id, entry.epoch
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
            if let Some((room_id, media_id)) = parse_publisher_key(publisher_key) {
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
            } else {
                warn!(
                    publisher_key = publisher_key,
                    "Removing invalid publisher tracking key during reconciliation"
                );
                self.active_publishers.remove(publisher_key);
                removed += 1;
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
            let publisher_key = match publisher_key(&publisher.room_id, &publisher.media_id) {
                Ok(key) => key,
                Err(error) => {
                    warn!(
                        room_id = %publisher.room_id,
                        media_id = %publisher.media_id,
                        error = %error,
                        "Skipping invalid publisher entry returned by registry"
                    );
                    continue;
                }
            };
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
                &self.local_cluster_address,
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
    pub(crate) async fn reregister_all_publishers(&self) {
        let (done, completed) = oneshot::channel();
        if self
            .maintenance_tx
            .send(PublisherMaintenanceCommand::Reregister { done })
            .await
            .is_err()
        {
            error!("Publisher maintenance worker stopped before re-registration");
            return;
        }
        if completed.await.is_err() {
            error!("Publisher maintenance worker stopped during re-registration");
        }
    }

    async fn reregister_all_publishers_once(&self) {
        self.set_restarting();

        // First reconcile with registry to remove zombie entries, meaning local
        // entries that no longer exist in registry or were taken over.
        // This prevents memory leaks when UnPublish events are lost.
        self.reconcile_with_registry().await;

        // Snapshot both key and entry to access stored user_id for re-registration.
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

        if self.local_cluster_address.is_empty() {
            warn!(
                "Re-registering {} publishers with empty local_cluster_address. \
                 Cross-node HLS proxying will fail until cluster_address is set in LivestreamConfig. \
                 Proceeding with re-registration to restore local publisher ownership in Redis.",
                snapshot.len()
            );
        }

        info!(
            "Re-registering {} active publishers after StreamHub restart",
            snapshot.len()
        );

        for (publisher_key, entry) in &snapshot {
            if let Some((room_id, media_id)) = parse_publisher_key(publisher_key) {
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
                        &self.local_cluster_address,
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
            } else {
                warn!(
                    publisher_key = publisher_key,
                    "Removing invalid publisher tracking key during re-registration"
                );
                self.active_publishers.remove(publisher_key);
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
        let publisher_key = match publisher_key(room_id, media_id) {
            Ok(key) => key,
            Err(error) => {
                warn!(
                    room_id = room_id,
                    media_id = media_id,
                    error = %error,
                    "Skipping cleanup for invalid publisher identifiers"
                );
                return;
            }
        };
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

        // 2. Unregister from Redis immediately. The epoch fence protects newer
        // publishers from delayed cleanup events.
        if let Err(e) = self
            .registry
            .unregister_publisher_if_epoch_matches(
                &cleanup.room_id,
                &cleanup.media_id,
                cleanup.epoch,
            )
            .await
        {
            warn!(
                "Failed to unregister publisher from Redis for room {} / media {} with epoch {}: {}. \
                 Leaving Redis cleanup to later reconciliation/TTL expiry.",
                cleanup.room_id, cleanup.media_id, cleanup.epoch, e
            );
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

    async fn record_heartbeat_failure(
        &self,
        request: &PublisherRefreshRequest,
        entry: &Arc<PublisherEntry>,
        failure_reason: &str,
    ) {
        synctv_core::metrics::livestream::PUBLISHER_HEARTBEAT_FAILURES.inc();
        let failures = entry
            .consecutive_heartbeat_failures
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        if failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
            error!(
                "Publisher room={} media={} failed heartbeat for {} consecutive cycles, cleaning up: {}",
                request.room_id, request.media_id, failures, failure_reason
            );
            self.cleanup_publisher(
                &request.room_id,
                &request.media_id,
                entry.epoch,
                &format!("heartbeat failed for {failures} consecutive cycles"),
            )
            .await;
        } else {
            warn!(
                "Heartbeat failed for room={} media={} ({}/{} consecutive): {}",
                request.room_id,
                request.media_id,
                failures,
                MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
                failure_reason
            );
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
        // Snapshot keys first to avoid holding DashMap read guard during async Redis ops.
        let snapshot: Vec<(String, Arc<PublisherEntry>)> = self
            .active_publishers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        let mut refresh_requests = Vec::with_capacity(snapshot.len());
        let mut refresh_entries = Vec::with_capacity(snapshot.len());

        for (publisher_key, entry) in snapshot {
            // Parse room_id and media_id from the composite key
            let Some((room_id, media_id)) = parse_publisher_key(&publisher_key) else {
                warn!(
                    publisher_key = publisher_key,
                    "Removing invalid publisher tracking key during heartbeat"
                );
                self.active_publishers.remove(&publisher_key);
                continue;
            };

            // Skip during StreamHub restart to avoid false cleanups while
            // publishers are reconnecting to the new hub instance.
            let idle_secs = entry.idle_secs();
            if idle_secs > self.silent_timeout_secs && !self.is_restarting.load(Ordering::Acquire) {
                warn!(
                    "Silent publisher detected: room={} media={} (no data for {}s, threshold={}s)",
                    room_id, media_id, idle_secs, self.silent_timeout_secs
                );
                self.cleanup_publisher(
                    room_id,
                    media_id,
                    entry.epoch,
                    &format!("silent publisher timeout ({idle_secs}s idle)"),
                )
                .await;
                continue;
            }

            refresh_requests.push(PublisherRefreshRequest {
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
                user_id: entry.user_id.clone(),
                expected_epoch: entry.epoch,
            });
            refresh_entries.push(entry);
        }

        for (request_batch, entry_batch) in refresh_requests
            .chunks(PUBLISHER_REFRESH_BATCH_SIZE)
            .zip(refresh_entries.chunks(PUBLISHER_REFRESH_BATCH_SIZE))
        {
            let outcomes = match self
                .registry
                .refresh_publishers_ttl(&self.local_node_id, request_batch)
                .await
            {
                Ok(outcomes) if outcomes.len() == request_batch.len() => outcomes,
                Ok(outcomes) => {
                    let failure_reason = format!(
                        "registry heartbeat returned {} outcomes for {} requests",
                        outcomes.len(),
                        request_batch.len()
                    );
                    for (request, entry) in request_batch.iter().zip(entry_batch) {
                        self.record_heartbeat_failure(request, entry, &failure_reason)
                            .await;
                    }
                    continue;
                }
                Err(error) => {
                    let failure_reason = format!("registry heartbeat error: {error}");
                    for (request, entry) in request_batch.iter().zip(entry_batch) {
                        self.record_heartbeat_failure(request, entry, &failure_reason)
                            .await;
                    }
                    continue;
                }
            };

            for ((request, entry), outcome) in request_batch.iter().zip(entry_batch).zip(outcomes) {
                match outcome {
                    PublisherRefreshOutcome::Refreshed => {
                        entry
                            .consecutive_heartbeat_failures
                            .store(0, Ordering::Release);
                        trace!(
                            "Heartbeat cycle succeeded for room {} / media {}",
                            request.room_id,
                            request.media_id
                        );
                    }
                    PublisherRefreshOutcome::Missing => {
                        self.record_heartbeat_failure(
                            request,
                            entry,
                            "publisher missing from registry",
                        )
                        .await;
                    }
                    PublisherRefreshOutcome::OwnershipChanged => {
                        warn!(
                            "Publisher room={} media={} no longer matches local owner/epoch; cleaning up immediately",
                            request.room_id, request.media_id
                        );
                        self.cleanup_publisher(
                            &request.room_id,
                            &request.media_id,
                            entry.epoch,
                            "publisher ownership changed in registry",
                        )
                        .await;
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
mod tests;
