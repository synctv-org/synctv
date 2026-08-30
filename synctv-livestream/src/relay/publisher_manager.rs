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
    LeaseRefreshOutcome, LeaseRefreshRequest, StreamGenerationRegistration, StreamRegistryTrait,
    PUBLISHER_REFRESH_BATCH_SIZE,
};
use crate::util::{unix_now_secs, validate_stream_ids};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use synctv_xiu::streamhub::{
    define::{BroadcastEventReceiver, StreamHubEvent, StreamHubEventSender},
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

struct AbortOnDrop(AbortHandle);

impl AbortOnDrop {
    fn new(handle: &JoinHandle<()>) -> Self {
        Self(handle.abort_handle())
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Publisher readiness transitions emitted after the shared registry is committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamLifecycleEvent {
    Started {
        room_id: String,
        media_id: String,
        user_id: String,
        generation_id: String,
    },
    Stopped {
        room_id: String,
        media_id: String,
        user_id: String,
        generation_id: String,
    },
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherStopRequest {
    pub room_id: String,
    pub media_id: String,
    pub generation_id: String,
    pub lease_epoch: u64,
}

impl PublisherStopRequest {
    #[must_use]
    pub fn new(
        room_id: impl Into<String>,
        media_id: impl Into<String>,
        generation_id: impl Into<String>,
        lease_epoch: u64,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            media_id: media_id.into(),
            generation_id: generation_id.into(),
            lease_epoch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherStopOutcome {
    Stopped,
    AlreadyStopped,
    Superseded,
}

#[derive(Clone)]
pub struct PublisherControlHandle {
    sender: mpsc::Sender<PublisherMaintenanceCommand>,
}

impl PublisherControlHandle {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<PublisherMaintenanceCommand>) {
        let (sender, receiver) = mpsc::channel(MAINTENANCE_COMMAND_CAPACITY);
        (Self { sender }, receiver)
    }

    pub async fn stop_publisher(
        &self,
        request: PublisherStopRequest,
    ) -> anyhow::Result<PublisherStopOutcome> {
        let (done, completed) = oneshot::channel();
        self.sender
            .send(PublisherMaintenanceCommand::StopPublisher { request, done })
            .await
            .map_err(|_| anyhow::anyhow!("publisher manager control channel is closed"))?;
        completed
            .await
            .map_err(|_| anyhow::anyhow!("publisher manager dropped the stop request"))?
    }
}

pub(crate) enum PublisherMaintenanceCommand {
    Reregister {
        done: oneshot::Sender<()>,
    },
    RecoverPublisher {
        room_id: String,
        media_id: String,
        generation_id: Uuid,
    },
    StopPublisher {
        request: PublisherStopRequest,
        done: oneshot::Sender<anyhow::Result<PublisherStopOutcome>>,
    },
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
    /// Publisher lease_epoch captured from the registry when tracking started.
    lease_epoch: u64,
    /// Whether this publisher can serve raw RTP to WHEP subscribers.
    supports_rtp: bool,
    /// Local StreamHub publication generation. Registry-only reconciliation
    /// leaves this empty until the next media packet binds the current owner.
    generation_id: parking_lot::RwLock<Option<Uuid>>,
}

impl PublisherEntry {
    // Used in test helpers to insert entries without a real registry lookup.
    #[cfg(test)]
    fn new() -> Self {
        Self {
            last_active_secs: AtomicU64::new(unix_now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            user_id: String::new(),
            lease_epoch: 0,
            supports_rtp: false,
            generation_id: parking_lot::RwLock::new(None),
        }
    }

    fn with_registration(user_id: String, lease_epoch: u64, supports_rtp: bool) -> Self {
        Self {
            last_active_secs: AtomicU64::new(unix_now_secs()),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(0),
            user_id,
            lease_epoch,
            supports_rtp,
            generation_id: parking_lot::RwLock::new(None),
        }
    }

    fn clone_with_registration(
        &self,
        user_id: String,
        lease_epoch: u64,
        supports_rtp: bool,
    ) -> Self {
        Self {
            last_active_secs: AtomicU64::new(self.last_active_secs.load(Ordering::Acquire)),
            consecutive_heartbeat_failures: std::sync::atomic::AtomicU32::new(
                self.consecutive_heartbeat_failures.load(Ordering::Acquire),
            ),
            user_id,
            lease_epoch,
            supports_rtp,
            generation_id: parking_lot::RwLock::new(*self.generation_id.read()),
        }
    }

    fn bind_publisher(&self, generation_id: Uuid) -> bool {
        let mut current = self.generation_id.write();
        if let Some(current_id) = *current {
            current_id == generation_id
        } else {
            *current = Some(generation_id);
            true
        }
    }

    fn generation_id(&self) -> Option<Uuid> {
        *self.generation_id.read()
    }

    fn owns_generation(&self, generation_id: &str) -> bool {
        self.generation_id()
            .is_none_or(|current| current.to_string() == generation_id)
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
    maintenance_cancel: CancellationToken,
    maintenance_handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    sync_notify: Notify,
    lifecycle_tx: Option<mpsc::Sender<StreamLifecycleEvent>>,
}

impl PublisherManager {
    /// Create a new `PublisherManager` with a shared restarting flag.
    ///
    /// This allows external code (e.g., `StreamHub` restart loop) to share the
    /// restarting flag and set it before cleanup operations begin, preventing
    /// false silent-publisher detections during the restart window.
    #[cfg(test)]
    pub(crate) fn with_restarting_flag(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        hub_event_sender: StreamHubEventSender,
        is_restarting: Arc<AtomicBool>,
    ) -> Self {
        let (control_handle, maintenance_rx) = PublisherControlHandle::channel();
        Self::with_restarting_flag_and_control(
            registry,
            local_node_id,
            hub_event_sender,
            is_restarting,
            control_handle,
            maintenance_rx,
        )
    }

    pub(crate) fn with_restarting_flag_and_control(
        registry: Arc<dyn StreamRegistryTrait>,
        local_node_id: String,
        hub_event_sender: StreamHubEventSender,
        is_restarting: Arc<AtomicBool>,
        control_handle: PublisherControlHandle,
        maintenance_rx: mpsc::Receiver<PublisherMaintenanceCommand>,
    ) -> Self {
        Self {
            registry,
            local_node_id,
            local_cluster_address: String::new(),
            active_publishers: Arc::new(DashMap::new()),
            hub_event_sender,
            lag_event_count: AtomicU64::new(0),
            silent_timeout_secs: SILENT_PUBLISHER_TIMEOUT_SECS,
            is_restarting,
            maintenance_tx: control_handle.sender,
            maintenance_rx: tokio::sync::Mutex::new(Some(maintenance_rx)),
            maintenance_cancel: CancellationToken::new(),
            maintenance_handle: tokio::sync::Mutex::new(None),
            sync_notify: Notify::new(),
            lifecycle_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn control_handle(&self) -> PublisherControlHandle {
        PublisherControlHandle {
            sender: self.maintenance_tx.clone(),
        }
    }

    /// Set the advertised cluster listener address for this node.
    /// Used during re-registration after `StreamHub` restart (L-05).
    #[must_use]
    pub(crate) fn with_cluster_address(mut self, cluster_address: String) -> Self {
        self.local_cluster_address = cluster_address;
        self
    }

    /// Stop and join the maintenance worker independently of the broadcast
    /// event loop. The worker owns Redis heartbeat/reconciliation resources and
    /// must finish before registry cleanup or runtime shutdown proceeds.
    pub(crate) async fn shutdown_maintenance(&self, budget: Duration) {
        self.maintenance_cancel.cancel();
        if let Some(mut handle) = self.maintenance_handle.lock().await.take() {
            let _abort_on_drop = AbortOnDrop::new(&handle);
            match tokio::time::timeout(budget, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    error!(error = %error, "Publisher maintenance worker failed during shutdown");
                }
                Err(_) => {
                    warn!("Publisher maintenance worker exceeded its shutdown budget; aborting");
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
    }

    pub(crate) fn cancel_maintenance(&self) {
        self.maintenance_cancel.cancel();
    }

    #[must_use]
    pub(crate) fn with_lifecycle_sender(
        mut self,
        lifecycle_tx: mpsc::Sender<StreamLifecycleEvent>,
    ) -> Self {
        self.lifecycle_tx = Some(lifecycle_tx);
        self
    }

    async fn emit_lifecycle_event(&self, event: StreamLifecycleEvent) {
        let Some(sender) = &self.lifecycle_tx else {
            return;
        };
        if let Err(error) = sender.send(event).await {
            warn!(%error, "Livestream lifecycle receiver closed");
        }
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
    pub(crate) fn active_publisher_streams(&self) -> Vec<(String, String, Uuid)> {
        self.active_publishers
            .iter()
            .filter_map(|entry| {
                let generation_id = entry.value().generation_id()?;
                parse_publisher_key(entry.key()).map(|(room_id, media_id)| {
                    (room_id.to_string(), media_id.to_string(), generation_id)
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
    pub(crate) fn record_publisher_activity(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: Uuid,
    ) {
        let Ok(key) = publisher_key(room_id, media_id) else {
            warn!(
                room_id = room_id,
                media_id = media_id,
                "Ignoring publisher activity for invalid stream identifiers"
            );
            return;
        };
        if let Some(entry) = self.active_publishers.get(&key) {
            if entry.bind_publisher(generation_id) {
                entry.touch();
            } else {
                self.schedule_registry_sync();
            }
        } else {
            match self
                .maintenance_tx
                .try_send(PublisherMaintenanceCommand::RecoverPublisher {
                    room_id: room_id.to_string(),
                    media_id: media_id.to_string(),
                    generation_id,
                }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!(
                        room_id,
                        media_id,
                        %generation_id,
                        "Publisher activity recovery queue is full; a later activity sample will retry"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!(
                        room_id,
                        media_id,
                        %generation_id,
                        "Publisher activity recovery worker is closed"
                    );
                }
            }
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
                .cleanup_all_generations_for_node(&self.local_node_id)
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
        if self.maintenance_cancel.is_cancelled() {
            info!("Publisher manager maintenance worker was cancelled before startup");
            return;
        }
        let maintenance_handle = tokio::spawn(
            Arc::clone(&self)
                .run_maintenance_worker(maintenance_rx, self.maintenance_cancel.clone()),
        );
        *self.maintenance_handle.lock().await = Some(maintenance_handle);

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

        self.shutdown_maintenance(Duration::from_secs(2)).await;
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
                        Some(PublisherMaintenanceCommand::StopPublisher { request, done }) => {
                            let result = self.commit_publisher_stop(&request).await;
                            let _ = done.send(result);
                        }
                        Some(PublisherMaintenanceCommand::Reregister { done }) => {
                            if !run_until_cancelled(&cancel, self.reregister_all_publishers_once()).await {
                                break;
                            }
                            let _ = done.send(());
                        }
                        Some(PublisherMaintenanceCommand::RecoverPublisher {
                            room_id,
                            media_id,
                            generation_id,
                        }) => {
                            if self
                                .active_publishers
                                .get(&format!("{room_id}:{media_id}"))
                                .is_some_and(|entry| entry.generation_id() == Some(generation_id))
                            {
                                continue;
                            }
                            let identifier = StreamIdentifier::Rtmp {
                                app_name: room_id,
                                stream_name: media_id,
                            };
                            if let Err(error) = self
                                .handle_publish_with_owner(identifier.clone(), generation_id)
                                .await
                            {
                                error!(
                                    %error,
                                    ?identifier,
                                    %generation_id,
                                    "Failed to recover publisher tracking from media activity"
                                );
                                self.stop_untracked_publish(identifier, generation_id).await;
                            }
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
                generation_id,
            } => {
                // User RTMP and WHIP pushes are tracked here because auth registered
                // them in Redis before StreamHub publish. ExternalPull streams are
                // registered and cleaned up by ExternalPublishManager, while
                // RtmpRelay streams are owned by their origin node.
                if !pub_type.is_user_push() {
                    debug!(
                        pub_type = ?pub_type,
                        identifier = ?identifier,
                        "Ignoring non-user publish event for heartbeat tracking"
                    );
                    return Ok(());
                }
                if let Err(e) = self
                    .handle_publish_with_owner(identifier.clone(), generation_id)
                    .await
                {
                    error!(
                        error = %e,
                        identifier = ?identifier,
                        "Failed to track publisher for heartbeat maintenance"
                    );
                    // The publication has already been admitted by StreamHub at this point.
                    // Stop that exact generation when tracking cannot be established so an
                    // untracked RTMP publisher cannot remain alive after its Redis lease expires.
                    self.stop_untracked_publish(identifier, generation_id).await;
                    return Err(e);
                }
            }
            synctv_xiu::streamhub::define::BroadcastEvent::UnPublish {
                identifier,
                generation_id,
            } => {
                self.handle_unpublish_with_owner(identifier, generation_id)
                    .await?;
            }
        }
        Ok(())
    }

    async fn stop_untracked_publish(&self, identifier: StreamIdentifier, generation_id: Uuid) {
        match tokio::time::timeout(
            UNPUBLISH_SEND_TIMEOUT,
            self.hub_event_sender.send(StreamHubEvent::UnPublish {
                identifier: identifier.clone(),
                generation_id,
            }),
        )
        .await
        {
            Ok(Ok(())) => {
                warn!(
                    identifier = ?identifier,
                    generation_id = %generation_id,
                    "Stopped publisher whose heartbeat tracking could not be established"
                );
            }
            Ok(Err(error)) => {
                error!(
                    identifier = ?identifier,
                    generation_id = %generation_id,
                    %error,
                    "Failed to stop publisher after heartbeat tracking failure"
                );
            }
            Err(_) => {
                error!(
                    identifier = ?identifier,
                    generation_id = %generation_id,
                    timeout_secs = UNPUBLISH_SEND_TIMEOUT.as_secs(),
                    "Timed out stopping publisher after heartbeat tracking failure"
                );
            }
        }
    }

    /// Handle Publish event - Track publisher locally for heartbeat maintenance
    ///
    /// NOTE: This does NOT register the publisher to Redis. Registration happens
    /// in the authentication phase (`SyncTvRtmpAuth::on_publish`) before the RTMP
    /// session is established. This method only tracks publishers that have already
    /// been successfully authenticated and registered.
    async fn handle_publish_with_owner(
        &self,
        identifier: StreamIdentifier,
        generation_id: Uuid,
    ) -> anyhow::Result<()> {
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
        let (entry, user_id, lease_epoch) = match self
            .registry
            .get_active_generation(&room_id, &media_id)
            .await
        {
            Ok(Some(info)) => {
                if info.generation_id != generation_id.to_string() {
                    warn!(
                        room_id = %room_id,
                        media_id = %media_id,
                        registered_generation_id = %info.generation_id,
                        event_generation_id = %generation_id,
                        "Ignoring Publish event from a stale StreamHub generation"
                    );
                    return Ok(());
                }
                debug!(
                    "Retrieved publisher info for heartbeat tracking: room={}, media={}, user_id={}, lease_epoch={}",
                    room_id, media_id, info.user_id, info.lease_epoch
                );
                let user_id = info.user_id;
                let lease_epoch = info.lease_epoch;
                let entry = PublisherEntry::with_registration(
                    user_id.clone(),
                    lease_epoch,
                    info.supports_rtp,
                );
                entry.bind_publisher(generation_id);
                (Arc::new(entry), user_id, lease_epoch)
            }
            Ok(None) => {
                // Publisher not in registry (was never registered or already expired).
                // Fail closed so the caller can stop the already-admitted StreamHub
                // generation instead of leaving an untracked publisher alive.
                warn!(
                    "Publisher not found in registry during tracking (room={}, media={}); rejecting untracked publication",
                    room_id, media_id
                );
                return Err(anyhow::anyhow!(
                    "Publisher registration missing during tracking for room={room_id}, media={media_id}"
                ));
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

        let tracked_entry = Arc::clone(&entry);
        match self.active_publishers.entry(publisher_key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if occupied.get().generation_id() == Some(generation_id) {
                    occupied.get().touch();
                    return Ok(());
                }
                occupied.insert(entry);
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(entry);
            }
        }
        let readiness_error = match self
            .registry
            .mark_generation_ready(&room_id, &media_id, &generation_id.to_string(), lease_epoch)
            .await
        {
            Ok(true) => None,
            Ok(false) => Some(anyhow::anyhow!(
                "Publisher ownership changed before readiness commit for room={room_id}, media={media_id}"
            )),
            Err(error) => Some(error.context(format!(
                "Failed to commit publisher readiness for room={room_id}, media={media_id}"
            ))),
        };
        if let Some(error) = readiness_error {
            self.active_publishers
                .remove_if(&publisher_key, |_, current| {
                    Arc::ptr_eq(current, &tracked_entry)
                });
            if let Err(rollback_error) = self
                .registry
                .deactivate_generation_if_lease_matches(
                    &room_id,
                    &media_id,
                    &generation_id.to_string(),
                    lease_epoch,
                )
                .await
            {
                warn!(
                    room_id,
                    media_id,
                    %generation_id,
                    lease_epoch,
                    error = %rollback_error,
                    "Failed to roll back publisher registration after readiness failure"
                );
            }
            return Err(error);
        }

        self.emit_lifecycle_event(StreamLifecycleEvent::Started {
            room_id,
            media_id,
            user_id,
            generation_id: generation_id.to_string(),
        })
        .await;

        Ok(())
    }

    /// Handle `UnPublish` event - Remove publisher from Redis
    #[cfg(test)]
    async fn handle_unpublish(&self, identifier: StreamIdentifier) -> anyhow::Result<()> {
        let StreamIdentifier::Rtmp {
            ref app_name,
            ref stream_name,
        } = identifier;
        let key = publisher_key(app_name, stream_name)?;
        let Some(generation_id) = self
            .active_publishers
            .get(&key)
            .and_then(|entry| entry.generation_id())
        else {
            return Ok(());
        };
        self.handle_unpublish_with_owner(identifier, generation_id)
            .await
    }

    async fn handle_unpublish_with_owner(
        &self,
        identifier: StreamIdentifier,
        generation_id: Uuid,
    ) -> anyhow::Result<()> {
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

        let publisher_key = publisher_key(&room_id, &media_id)?;
        let Some(entry) = self
            .active_publishers
            .get(&publisher_key)
            .filter(|entry| entry.generation_id() == Some(generation_id))
            .map(|entry| Arc::clone(entry.value()))
        else {
            return Ok(());
        };

        self.commit_publisher_stop(&PublisherStopRequest::new(
            room_id,
            media_id,
            generation_id.to_string(),
            entry.lease_epoch,
        ))
        .await?;

        Ok(())
    }

    fn remove_tracked_publisher(
        &self,
        publisher_key: &str,
        request: &PublisherStopRequest,
    ) -> Option<Arc<PublisherEntry>> {
        self.active_publishers
            .remove_if(publisher_key, |_, entry| {
                entry.lease_epoch == request.lease_epoch
                    && entry.generation_id().is_some_and(|generation_id| {
                        generation_id.to_string() == request.generation_id.as_str()
                    })
            })
            .map(|(_, entry)| entry)
    }

    async fn emit_stopped(&self, request: &PublisherStopRequest, user_id: String) {
        info!(
            room_id = request.room_id,
            media_id = request.media_id,
            generation_id = request.generation_id,
            lease_epoch = request.lease_epoch,
            "Publisher generation stopped"
        );
        self.emit_lifecycle_event(StreamLifecycleEvent::Stopped {
            room_id: request.room_id.clone(),
            media_id: request.media_id.clone(),
            user_id,
            generation_id: request.generation_id.clone(),
        })
        .await;
    }

    async fn commit_publisher_stop(
        &self,
        request: &PublisherStopRequest,
    ) -> anyhow::Result<PublisherStopOutcome> {
        let publisher_key = publisher_key(&request.room_id, &request.media_id)?;
        let tracked_before = self
            .active_publishers
            .get(&publisher_key)
            .filter(|entry| {
                entry.lease_epoch == request.lease_epoch
                    && entry.generation_id().is_some_and(|generation_id| {
                        generation_id.to_string() == request.generation_id.as_str()
                    })
            })
            .map(|entry| Arc::clone(entry.value()));
        let current = self
            .registry
            .get_active_generation(&request.room_id, &request.media_id)
            .await?;

        let Some(current) = current else {
            if let Some(entry) = self.remove_tracked_publisher(&publisher_key, request) {
                self.emit_stopped(request, entry.user_id.clone()).await;
            }
            return Ok(PublisherStopOutcome::AlreadyStopped);
        };

        if current.generation_id != request.generation_id
            || current.lease_epoch != request.lease_epoch
        {
            self.remove_tracked_publisher(&publisher_key, request);
            return Ok(PublisherStopOutcome::Superseded);
        }

        let was_ready = current.ready_at.is_some();
        let deactivated = self
            .registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                &request.room_id,
                &request.media_id,
                &request.generation_id,
                request.lease_epoch,
            )
            .await?;

        if deactivated {
            let removed = self.remove_tracked_publisher(&publisher_key, request);
            if was_ready && (removed.is_some() || tracked_before.is_none()) {
                self.emit_stopped(request, current.user_id).await;
            }
            return Ok(PublisherStopOutcome::Stopped);
        }

        let current_after = self
            .registry
            .get_active_generation(&request.room_id, &request.media_id)
            .await?;
        match current_after {
            None => {
                let removed = self.remove_tracked_publisher(&publisher_key, request);
                if was_ready {
                    if let Some(entry) = removed {
                        self.emit_stopped(request, entry.user_id.clone()).await;
                    }
                }
                Ok(PublisherStopOutcome::AlreadyStopped)
            }
            Some(generation)
                if generation.generation_id != request.generation_id
                    || generation.lease_epoch != request.lease_epoch =>
            {
                self.remove_tracked_publisher(&publisher_key, request);
                Ok(PublisherStopOutcome::Superseded)
            }
            Some(_) => Err(anyhow::anyhow!(
                "fenced publisher stop left the matching generation active"
            )),
        }
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
                match self.registry.get_active_generation(room_id, media_id).await {
                    Ok(Some(info))
                        if info.node_id == self.local_node_id
                            && self
                                .active_publishers
                                .get(publisher_key)
                                .is_some_and(|entry| {
                                    entry.owns_generation(&info.generation_id)
                                }) =>
                    {
                        if let Some(mut current) = self.active_publishers.get_mut(publisher_key) {
                            if current.lease_epoch != info.lease_epoch
                                || current.user_id != info.user_id
                            {
                                *current.value_mut() = Arc::new(current.clone_with_registration(
                                    info.user_id,
                                    info.lease_epoch,
                                    info.supports_rtp,
                                ));
                            }
                        }
                        trace!(
                            "Reconcile: publisher room={} media={} still active on this node",
                            room_id,
                            media_id
                        );
                    }
                    Ok(Some(info)) if info.node_id == self.local_node_id => {
                        warn!(
                            room_id,
                            media_id,
                            registry_generation_id = info.generation_id,
                            "Reconcile: a newer generation owns this stream on the local node; removing the stale local generation"
                        );
                        if let Some(expected_lease_epoch) = self
                            .active_publishers
                            .get(publisher_key)
                            .map(|entry| entry.lease_epoch)
                        {
                            self.cleanup_publisher(
                                room_id,
                                media_id,
                                expected_lease_epoch,
                                "local stream generation changed during reconciliation",
                            )
                            .await;
                        }
                        removed += 1;
                    }
                    Ok(Some(info)) => {
                        warn!(
                            "Reconcile: publisher room={} media={} moved to node {}; cleaning up the stale local generation",
                            room_id, media_id, info.node_id
                        );
                        if let Some(expected_lease_epoch) = self
                            .active_publishers
                            .get(publisher_key)
                            .map(|entry| entry.lease_epoch)
                        {
                            self.cleanup_publisher(
                                room_id,
                                media_id,
                                expected_lease_epoch,
                                "stream ownership moved to another node during reconciliation",
                            )
                            .await;
                        }
                        removed += 1;
                    }
                    Ok(None) => {
                        warn!(
                            "Reconcile: publisher room={} media={} no longer in registry; cleaning up the stale local generation",
                            room_id, media_id
                        );
                        if let Some(expected_lease_epoch) = self
                            .active_publishers
                            .get(publisher_key)
                            .map(|entry| entry.lease_epoch)
                        {
                            self.cleanup_publisher(
                                room_id,
                                media_id,
                                expected_lease_epoch,
                                "registry owner disappeared during reconciliation",
                            )
                            .await;
                        }
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
        let active_publishers = match self.registry.list_active_generations().await {
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
            if publisher.generation.ready_at.is_none() {
                debug!(
                    room_id = publisher.room_id,
                    media_id = publisher.media_id,
                    generation_id = publisher.generation.generation_id,
                    "Skipping starting publisher during reverse reconciliation"
                );
                continue;
            }
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
            if publisher.generation.node_id != self.local_node_id {
                continue;
            }

            let generation_id = match Uuid::parse_str(&publisher.generation.generation_id) {
                Ok(generation_id) => generation_id,
                Err(error) => {
                    warn!(
                        room_id = %publisher.room_id,
                        media_id = %publisher.media_id,
                        generation_id = %publisher.generation.generation_id,
                        %error,
                        "Skipping registry publisher with invalid generation ID"
                    );
                    continue;
                }
            };

            info!(
                "Reconcile (reverse): adding missing publisher room={} media={} to local tracking",
                publisher.room_id, publisher.media_id
            );
            let entry = Arc::new(PublisherEntry::with_registration(
                publisher.generation.user_id,
                publisher.generation.lease_epoch,
                publisher.generation.supports_rtp,
            ));
            entry.bind_publisher(generation_id);
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
        let tracked_generation_id = tracked_entry
            .generation_id()
            .map(|generation_id| generation_id.to_string());
        match self.registry.get_active_generation(room_id, media_id).await {
            Ok(Some(info))
                if info.node_id == self.local_node_id
                    && tracked_generation_id.as_deref() == Some(info.generation_id.as_str()) =>
            {
                let became_ready = if info.ready_at.is_none() {
                    match self
                        .registry
                        .mark_generation_ready(
                            room_id,
                            media_id,
                            &info.generation_id,
                            info.lease_epoch,
                        )
                        .await
                    {
                        Ok(true) => true,
                        Ok(false) => {
                            warn!(
                                room_id,
                                media_id,
                                generation_id = info.generation_id,
                                lease_epoch = info.lease_epoch,
                                "Publisher ownership changed before restart readiness commit"
                            );
                            self.cleanup_publisher(
                                room_id,
                                media_id,
                                tracked_entry.lease_epoch,
                                "restart readiness ownership changed",
                            )
                            .await;
                            return;
                        }
                        Err(error) => {
                            error!(
                                room_id,
                                media_id,
                                generation_id = info.generation_id,
                                lease_epoch = info.lease_epoch,
                                %error,
                                "Failed to restore publisher readiness after restart"
                            );
                            self.cleanup_publisher(
                                room_id,
                                media_id,
                                tracked_entry.lease_epoch,
                                "restart readiness commit failed",
                            )
                            .await;
                            return;
                        }
                    }
                } else {
                    false
                };
                let user_id = info.user_id.clone();
                let lease_epoch = info.lease_epoch;
                let generation_id = info.generation_id.clone();
                let mut local_registration_updated = false;
                if let Some(mut current) = self.active_publishers.get_mut(publisher_key) {
                    if Arc::ptr_eq(current.value(), tracked_entry) {
                        *current.value_mut() = Arc::new(tracked_entry.clone_with_registration(
                            user_id.clone(),
                            lease_epoch,
                            info.supports_rtp,
                        ));
                        local_registration_updated = true;
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
                if became_ready && local_registration_updated {
                    self.emit_lifecycle_event(StreamLifecycleEvent::Started {
                        room_id: room_id.to_string(),
                        media_id: media_id.to_string(),
                        user_id,
                        generation_id,
                    })
                    .await;
                }
            }
            Ok(Some(info)) if info.node_id == self.local_node_id => {
                warn!(
                    room_id,
                    media_id,
                    tracked_generation_id,
                    registry_generation_id = info.generation_id,
                    "Re-registration completed after a newer local generation took ownership; cleaning up only the stale generation"
                );
                self.cleanup_publisher(
                    room_id,
                    media_id,
                    tracked_entry.lease_epoch,
                    "local generation changed during re-registration",
                )
                .await;
            }
            Ok(Some(info)) => {
                warn!(
                    "Re-registered publisher for room {} / media {} but registry ownership moved to node {} before local lease_epoch refresh",
                    room_id, media_id, info.node_id
                );
            }
            Ok(None) => {
                warn!(
                    "Re-registered publisher for room {} / media {} but registry entry disappeared before local lease_epoch refresh",
                    room_id, media_id
                );
            }
            Err(e) => {
                error!(
                    "Re-registered publisher for room {} / media {} but failed to refresh local lease_epoch from registry: {}",
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
        let Some(generation_id) = entry.generation_id() else {
            warn!(
                room_id,
                media_id, "Cannot re-register publisher without a StreamHub generation"
            );
            return;
        };
        let generation_id = generation_id.to_string();
        match self
            .registry
            .try_activate_generation_with_capabilities(
                StreamGenerationRegistration::new(
                    room_id,
                    media_id,
                    &self.local_node_id,
                    &entry.user_id,
                    &self.local_cluster_address,
                    &generation_id,
                )
                .with_rtp_support(entry.supports_rtp),
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
                let Some(generation_id) = entry.generation_id() else {
                    warn!(
                        room_id,
                        media_id,
                        "Skipping publisher re-registration without a StreamHub generation"
                    );
                    continue;
                };
                let generation_id = generation_id.to_string();
                // Try to register the publisher in registry.
                // After reconcile_with_registry, only entries owned by us remain,
                // so we just need to refresh TTL or re-register if expired.
                match self
                    .registry
                    .try_activate_generation_with_capabilities(
                        StreamGenerationRegistration::new(
                            room_id,
                            media_id,
                            &self.local_node_id,
                            &entry.user_id,
                            &self.local_cluster_address,
                            &generation_id,
                        )
                        .with_rtp_support(entry.supports_rtp),
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
                            .refresh_generation_lease(
                                room_id,
                                media_id,
                                &generation_id,
                                &entry.user_id,
                                &self.local_node_id,
                                entry.lease_epoch,
                            )
                            .await
                        {
                            Ok(LeaseRefreshOutcome::Refreshed) => {
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
                            Ok(LeaseRefreshOutcome::Missing) => {
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
                            Ok(LeaseRefreshOutcome::OwnershipChanged) => {
                                warn!(
                                    "Publisher room {} / media {} changed ownership during re-register TTL refresh; cleaning up local publisher",
                                    room_id, media_id
                                );
                                self.cleanup_publisher(
                                    room_id,
                                    media_id,
                                    entry.lease_epoch,
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
        expected_lease_epoch: u64,
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
            "Cleaning up publisher room={} media={} lease_epoch={}: {}",
            room_id, media_id, expected_lease_epoch, reason
        );

        let Some(entry) = self.active_publishers.get(&publisher_key) else {
            debug!(
                "Cleanup skipped for room={} media={} because it is no longer tracked",
                room_id, media_id
            );
            return;
        };
        if entry.lease_epoch != expected_lease_epoch {
            debug!(
                "Cleanup skipped for room={} media={} because local owner advanced from lease_epoch {} to {}",
                room_id, media_id, expected_lease_epoch, entry.lease_epoch
            );
            return;
        }
        let generation_id = entry.generation_id();
        let tracked_entry = Arc::clone(entry.value());
        drop(entry);
        let Some(generation_id) = generation_id else {
            self.active_publishers
                .remove_if(&publisher_key, |_, current| {
                    Arc::ptr_eq(current, &tracked_entry)
                        && current.lease_epoch == expected_lease_epoch
                });
            debug!(
                room_id,
                media_id,
                lease_epoch = expected_lease_epoch,
                "Skipped StreamHub UnPublish because the local publication generation is unknown"
            );
            return;
        };

        let stop_request = PublisherStopRequest::new(
            room_id,
            media_id,
            generation_id.to_string(),
            expected_lease_epoch,
        );
        if let Err(error) = self.commit_publisher_stop(&stop_request).await {
            warn!(
                room_id,
                media_id,
                %generation_id,
                lease_epoch = expected_lease_epoch,
                %error,
                "Failed to commit publisher cleanup; StreamHub UnPublish will trigger a retry"
            );
        }

        // This is a critical control-plane event and must not be silently dropped.
        // Wait briefly for backpressure to clear, then log a hard failure if even
        // the bounded timeout cannot deliver it.
        let identifier = StreamIdentifier::Rtmp {
            app_name: room_id.to_string(),
            stream_name: media_id.to_string(),
        };
        match tokio::time::timeout(
            UNPUBLISH_SEND_TIMEOUT,
            self.hub_event_sender.send(StreamHubEvent::UnPublish {
                identifier: identifier.clone(),
                generation_id,
            }),
        )
        .await
        {
            Ok(Ok(())) => {
                info!(
                    "Sent UnPublish event for room {} / media {} ({})",
                    room_id, media_id, reason
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
        request: &LeaseRefreshRequest,
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
                entry.lease_epoch,
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
                    entry.lease_epoch,
                    &format!("silent publisher timeout ({idle_secs}s idle)"),
                )
                .await;
                continue;
            }

            refresh_requests.push(LeaseRefreshRequest {
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
                generation_id: if let Some(generation_id) = entry.generation_id() {
                    generation_id.to_string()
                } else {
                    warn!(
                        room_id,
                        media_id, "Skipping heartbeat for publisher without a bound generation"
                    );
                    self.schedule_registry_sync();
                    continue;
                },
                user_id: entry.user_id.clone(),
                expected_lease_epoch: entry.lease_epoch,
            });
            refresh_entries.push(entry);
        }

        for (request_batch, entry_batch) in refresh_requests
            .chunks(PUBLISHER_REFRESH_BATCH_SIZE)
            .zip(refresh_entries.chunks(PUBLISHER_REFRESH_BATCH_SIZE))
        {
            let outcomes = match self
                .registry
                .refresh_generation_leases(&self.local_node_id, request_batch)
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
                    LeaseRefreshOutcome::Refreshed => {
                        entry
                            .consecutive_heartbeat_failures
                            .store(0, Ordering::Release);
                        trace!(
                            "Heartbeat cycle succeeded for room {} / media {}",
                            request.room_id,
                            request.media_id
                        );
                    }
                    LeaseRefreshOutcome::Missing => {
                        self.record_heartbeat_failure(
                            request,
                            entry,
                            "publisher missing from registry",
                        )
                        .await;
                    }
                    LeaseRefreshOutcome::OwnershipChanged => {
                        warn!(
                            "Publisher room={} media={} no longer matches local owner/lease_epoch; cleaning up immediately",
                            request.room_id, request.media_id
                        );
                        self.cleanup_publisher(
                            &request.room_id,
                            &request.media_id,
                            entry.lease_epoch,
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
