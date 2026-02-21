//! Kubernetes Lease-based leader election.
//!
//! Uses the coordination.k8s.io/v1 Lease resource for leader election
//! when running in a Kubernetes cluster. This is the recommended approach
//! for K8s deployments as it uses native K8s primitives.
//!
//! For non-K8s deployments, use [`super::LeaderElector`] which uses Redis.
//!
//! # How it works
//!
//! 1. Each pod tries to create/update a Lease resource with its identity
//! 2. The holder renews the lease periodically (before `lease_duration` expires)
//! 3. If the holder crashes, the lease expires and another pod acquires it
//! 4. Uses optimistic concurrency (resourceVersion) to prevent split-brain

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::coordination::v1::Lease;
use kube::api::{Api, PostParams};
use kube::Client;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::LeadershipEvent;

/// K8s Lease-based leader election.
///
/// Creates or acquires a Lease resource in the pod's namespace.
/// The leader periodically renews the lease; if it fails, another
/// pod can take over after the lease expires.
///
/// Supports **observer pattern**: use `subscribe()` to receive leadership
/// change notifications (gained/lost events). Observers can use this to
/// start/stop singleton tasks when leadership changes.
#[derive(Clone)]
pub struct K8sLeaderElector {
    /// Whether this instance is currently the leader
    is_leader: Arc<AtomicBool>,
    /// K8s API client (in-cluster)
    client: Client,
    /// Lease name (e.g., "synctv-leader")
    lease_name: String,
    /// Namespace (from downward API or in-cluster config)
    namespace: String,
    /// This pod's identity (pod name)
    identity: String,
    /// Lease duration in seconds
    lease_duration_secs: i32,
    /// How often to attempt renewal in seconds
    renew_interval_secs: u64,
    /// Monotonically increasing epoch (fencing token) incremented on each
    /// leadership acquisition. Used for split-brain protection.
    leader_epoch: Arc<AtomicU64>,
    /// Timestamp at which leadership was lost. Used to enforce a grace period
    /// before re-acquisition attempts.
    leadership_lost_at: Arc<parking_lot::Mutex<Option<tokio::time::Instant>>>,
    /// Broadcast channel for leadership change events (observer pattern)
    event_tx: Arc<broadcast::Sender<LeadershipEvent>>,
}

/// Configuration for K8s lease-based leader election.
pub struct K8sLeaderElectorConfig {
    /// Lease name in Kubernetes (default: "synctv-leader")
    pub lease_name: String,
    /// Lease duration in seconds (default: 30)
    pub lease_duration_secs: i32,
    /// Renewal interval in seconds (default: 10, must be < lease_duration_secs)
    pub renew_interval_secs: u64,
}

impl Default for K8sLeaderElectorConfig {
    fn default() -> Self {
        Self {
            lease_name: "synctv-leader".to_string(),
            lease_duration_secs: 30,
            renew_interval_secs: 10,
        }
    }
}

impl K8sLeaderElector {
    /// Create a new K8s leader elector using in-cluster configuration.
    ///
    /// # Arguments
    /// * `identity` - This pod's unique identity (typically POD_NAME from downward API)
    /// * `namespace` - Kubernetes namespace (typically POD_NAMESPACE from downward API)
    /// * `config` - Leader election configuration
    pub async fn new(
        identity: String,
        namespace: String,
        config: K8sLeaderElectorConfig,
    ) -> anyhow::Result<Self> {
        let client = Client::try_default()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create in-cluster K8s client: {e}"))?;

        // Probe RBAC permissions by attempting to read the lease we will manage.
        // 404 (not found) is fine — the lease just doesn't exist yet.
        // 403 (forbidden) means the service account lacks the required permissions.
        let leases: Api<Lease> = Api::namespaced(client.clone(), &namespace);
        match leases.get_opt(&config.lease_name).await {
            Ok(_) => {} // Permission verified (lease exists or doesn't — both OK)
            Err(kube::Error::Api(err)) if err.code == 403 => {
                return Err(anyhow::anyhow!(
                    "K8s service account lacks permission to manage Leases in namespace {namespace}. \
                     Required RBAC: verbs [get,create,update] on resource leases in group coordination.k8s.io"
                ));
            }
            Err(_) => {} // Other errors (e.g. transient network) are acceptable at startup
        }

        let (event_tx, _) = broadcast::channel(16);

        Ok(Self {
            is_leader: Arc::new(AtomicBool::new(false)),
            client,
            lease_name: config.lease_name,
            namespace,
            identity,
            lease_duration_secs: config.lease_duration_secs,
            renew_interval_secs: config.renew_interval_secs,
            leader_epoch: Arc::new(AtomicU64::new(0)),
            leadership_lost_at: Arc::new(parking_lot::Mutex::new(None)),
            event_tx: Arc::new(event_tx),
        })
    }

    /// Subscribe to leadership change events (observer pattern).
    ///
    /// Returns a receiver that will receive `LeadershipEvent::Gained` when this
    /// node becomes leader and `LeadershipEvent::Lost` when it loses leadership.
    ///
    /// Observers can use this to start/stop singleton tasks (database migrations,
    /// periodic cleanup, etc.) when leadership changes.
    ///
    /// The channel has a capacity of 16 events. If observers lag behind, they
    /// will receive a `RecvError::Lagged` error and can resync by checking
    /// `is_leader()` and `leader_epoch()`.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        self.event_tx.subscribe()
    }

    /// Returns `true` if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::Acquire)
    }

    /// Returns the current leader epoch (fencing token).
    ///
    /// The epoch is monotonically increasing and increments each time this pod
    /// acquires leadership. Returns 0 if this pod has never been leader.
    pub fn leader_epoch(&self) -> u64 {
        self.leader_epoch.load(Ordering::Acquire)
    }

    /// Returns this pod's identity string (typically POD_NAME).
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Start the leader election loop.
    ///
    /// Returns a `JoinHandle` for the background task.
    pub fn start(&self, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        let elector = self.clone();
        tokio::spawn(async move {
            elector.run_loop(cancel_token).await;
        })
    }

    /// Main election loop.
    async fn run_loop(&self, cancel_token: CancellationToken) {
        let interval = Duration::from_secs(self.renew_interval_secs);
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);

        info!(
            identity = %self.identity,
            lease_name = %self.lease_name,
            namespace = %self.namespace,
            lease_duration_secs = self.lease_duration_secs,
            renew_interval_secs = self.renew_interval_secs,
            "K8s Lease leader election started"
        );

        loop {
            tokio::select! {
                () = cancel_token.cancelled() => {
                    info!(identity = %self.identity, "K8s leader election shutting down");
                    self.resign(&leases).await;
                    break;
                }
                () = tokio::time::sleep(interval) => {
                    self.try_acquire_or_renew(&leases).await;
                }
            }
        }
    }

    /// Try to acquire or renew the lease.
    async fn try_acquire_or_renew(&self, leases: &Api<Lease>) {
        match leases.get_opt(&self.lease_name).await {
            Ok(Some(existing)) => {
                self.handle_existing_lease(leases, existing).await;
            }
            Ok(None) => {
                // Lease doesn't exist, try to create it
                self.try_create_lease(leases).await;
            }
            Err(e) => {
                warn!(
                    identity = %self.identity,
                    error = %e,
                    "Failed to get lease, assuming not leader"
                );
                self.set_leader(false);
            }
        }
    }

    /// Handle an existing lease - renew if we hold it, or try to acquire if expired.
    async fn handle_existing_lease(&self, leases: &Api<Lease>, lease: Lease) {
        let spec = lease.spec.as_ref();
        let holder = spec.and_then(|s| s.holder_identity.as_deref());
        let renew_time = spec.and_then(|s| s.renew_time.as_ref());
        let duration = spec
            .and_then(|s| s.lease_duration_seconds)
            .unwrap_or(self.lease_duration_secs);

        let is_our_lease = holder == Some(self.identity.as_str());

        // Check if the lease has expired
        let lease_expired = if let Some(renew) = renew_time {
            let renew_time = renew.0;
            let now = chrono::Utc::now();
            let elapsed = now.signed_duration_since(renew_time);
            elapsed.num_seconds() > i64::from(duration)
        } else {
            true // No renew time means expired
        };

        if is_our_lease {
            // We hold the lease, renew it
            self.renew_lease(leases, &lease).await;
        } else if lease_expired {
            // Grace period: don't try to re-acquire immediately after losing leadership
            if self.in_grace_period() {
                debug!(
                    identity = %self.identity,
                    "In grace period after leadership loss, deferring acquisition"
                );
                return;
            }
            // Lease expired, try to take it
            info!(
                identity = %self.identity,
                previous_holder = ?holder,
                "Lease expired, attempting to acquire"
            );
            self.update_lease(leases, &lease).await;
        } else {
            // Another pod holds a valid lease
            debug!(
                identity = %self.identity,
                holder = ?holder,
                "Another pod holds the lease"
            );
            self.set_leader(false);
        }
    }

    /// Try to create a new lease (first time).
    async fn try_create_lease(&self, leases: &Api<Lease>) {
        let lease = self.build_lease();

        match leases.create(&PostParams::default(), &lease).await {
            Ok(_) => {
                self.gain_leadership();
            }
            Err(kube::Error::Api(err)) if err.code == 409 => {
                // Conflict - another pod created it first
                debug!(identity = %self.identity, "Lease creation conflict, another pod is leader");
                self.set_leader(false);
            }
            Err(e) => {
                warn!(identity = %self.identity, error = %e, "Failed to create lease");
                self.set_leader(false);
            }
        }
    }

    /// Renew our existing lease.
    ///
    /// Uses the `resourceVersion` from the existing lease to detect concurrent
    /// modifications (optimistic concurrency). If another pod modified the lease
    /// between our GET and this PATCH, the API server returns 409 Conflict.
    ///
    /// **Conflict retry**: On 409 Conflict, retries up to 3 times with exponential
    /// backoff (100ms, 200ms, 400ms). Each retry re-fetches the lease to get the
    /// latest resourceVersion. This handles transient conflicts during rolling
    /// restarts or network hiccups.
    async fn renew_lease(&self, leases: &Api<Lease>, existing: &Lease) {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_BACKOFF_MS: u64 = 100;

        for attempt in 1..=MAX_RETRIES {
            let current_lease = if attempt == 1 {
                // First attempt: use the lease we already fetched
                existing.clone()
            } else {
                // Retry: re-fetch the lease to get the latest resourceVersion
                match leases.get_opt(&self.lease_name).await {
                    Ok(Some(lease)) => lease,
                    Ok(None) => {
                        warn!(
                            identity = %self.identity,
                            attempt = attempt,
                            "Lease disappeared during renewal retry"
                        );
                        self.set_leader(false);
                        return;
                    }
                    Err(e) => {
                        warn!(
                            identity = %self.identity,
                            attempt = attempt,
                            error = %e,
                            "Failed to fetch lease for retry"
                        );
                        self.set_leader(false);
                        return;
                    }
                }
            };

            let now = chrono::Utc::now();

            // LS-4: Use replace() instead of Patch::Merge to enforce
            // resourceVersion optimistic locking. The API server rejects
            // the request with 409 if the resourceVersion doesn't match.
            let mut updated_lease = current_lease.clone();
            if let Some(ref mut spec) = updated_lease.spec {
                spec.renew_time = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now));
            }

            match leases
                .replace(
                    &self.lease_name,
                    &PostParams::default(),
                    &updated_lease,
                )
                .await
            {
                Ok(_) => {
                    if attempt > 1 {
                        debug!(
                            identity = %self.identity,
                            attempt = attempt,
                            "Lease renewed after retry"
                        );
                    } else {
                        debug!(identity = %self.identity, "Lease renewed");
                    }
                    self.set_leader(true);
                    return;
                }
                Err(kube::Error::Api(err)) if err.code == 409 && attempt < MAX_RETRIES => {
                    let backoff_ms = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                    debug!(
                        identity = %self.identity,
                        attempt = attempt,
                        backoff_ms = backoff_ms,
                        "Lease renewal conflict, retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(kube::Error::Api(err)) if err.code == 409 => {
                    warn!(
                        identity = %self.identity,
                        attempt = attempt,
                        "Lease renewal conflict after {} retries, lost leadership",
                        MAX_RETRIES
                    );
                    self.set_leader(false);
                    return;
                }
                Err(e) => {
                    warn!(
                        identity = %self.identity,
                        attempt = attempt,
                        error = %e,
                        "Failed to renew lease"
                    );
                    self.set_leader(false);
                    return;
                }
            }
        }
    }

    /// Update an expired lease to claim leadership.
    ///
    /// Uses the `resourceVersion` from the existing lease so that two pods racing
    /// to acquire an expired lease will not both succeed: the second PATCH gets a
    /// 409 Conflict from the API server.
    ///
    /// **Conflict retry**: On 409 Conflict, retries up to 3 times with exponential
    /// backoff (100ms, 200ms, 400ms). Each retry re-fetches the lease to check if
    /// it's still expired and get the latest resourceVersion. This increases the
    /// chance of acquiring leadership during rolling restarts.
    async fn update_lease(&self, leases: &Api<Lease>, existing: &Lease) {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_BACKOFF_MS: u64 = 100;

        for attempt in 1..=MAX_RETRIES {
            let current_lease = if attempt == 1 {
                // First attempt: use the lease we already fetched
                existing.clone()
            } else {
                // Retry: re-fetch the lease
                match leases.get_opt(&self.lease_name).await {
                    Ok(Some(lease)) => lease,
                    Ok(None) => {
                        debug!(
                            identity = %self.identity,
                            attempt = attempt,
                            "Lease disappeared during acquisition retry, will try to create"
                        );
                        self.try_create_lease(leases).await;
                        return;
                    }
                    Err(e) => {
                        warn!(
                            identity = %self.identity,
                            attempt = attempt,
                            error = %e,
                            "Failed to fetch lease for acquisition retry"
                        );
                        self.set_leader(false);
                        return;
                    }
                }
            };

            // Check if lease is still expired before attempting to acquire
            let spec = current_lease.spec.as_ref();
            let renew_time = spec.and_then(|s| s.renew_time.as_ref());
            let duration = spec
                .and_then(|s| s.lease_duration_seconds)
                .unwrap_or(self.lease_duration_secs);
            let lease_expired = if let Some(renew) = renew_time {
                let renew_time = renew.0;
                let now = chrono::Utc::now();
                let elapsed = now.signed_duration_since(renew_time);
                elapsed.num_seconds() > i64::from(duration)
            } else {
                true // No renew time means expired
            };

            if !lease_expired && attempt > 1 {
                debug!(
                    identity = %self.identity,
                    attempt = attempt,
                    "Lease is no longer expired during retry, another pod acquired it"
                );
                self.set_leader(false);
                return;
            }

            let now = chrono::Utc::now();

            // LS-4: Use replace() instead of Patch::Merge to enforce
            // resourceVersion optimistic locking on acquisition.
            let mut updated_lease = current_lease.clone();
            if let Some(ref mut spec) = updated_lease.spec {
                spec.holder_identity = Some(self.identity.clone());
                spec.lease_duration_seconds = Some(self.lease_duration_secs);
                spec.acquire_time = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now));
                spec.renew_time = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now));
            }

            match leases
                .replace(
                    &self.lease_name,
                    &PostParams::default(),
                    &updated_lease,
                )
                .await
            {
                Ok(_) => {
                    if attempt > 1 {
                        info!(
                            identity = %self.identity,
                            attempt = attempt,
                            "Acquired lease after retry"
                        );
                    }
                    self.gain_leadership();
                    return;
                }
                Err(kube::Error::Api(err)) if err.code == 409 && attempt < MAX_RETRIES => {
                    let backoff_ms = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                    debug!(
                        identity = %self.identity,
                        attempt = attempt,
                        backoff_ms = backoff_ms,
                        "Lease acquisition conflict, retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(kube::Error::Api(err)) if err.code == 409 => {
                    debug!(
                        identity = %self.identity,
                        attempt = attempt,
                        "Lease acquisition conflict after {} retries",
                        MAX_RETRIES
                    );
                    self.set_leader(false);
                    return;
                }
                Err(e) => {
                    warn!(
                        identity = %self.identity,
                        attempt = attempt,
                        error = %e,
                        "Failed to acquire lease"
                    );
                    self.set_leader(false);
                    return;
                }
            }
        }
    }

    /// Resign leadership by clearing the holder.
    ///
    /// Fetches the current Lease to verify that we still hold it (holderIdentity
    /// matches our identity) and uses the `resourceVersion` for optimistic
    /// concurrency. This prevents accidentally clearing another pod's lease if
    /// leadership was already transferred before the resign call.
    async fn resign(&self, leases: &Api<Lease>) {
        if !self.is_leader() {
            return;
        }

        info!(identity = %self.identity, "Resigning K8s lease leadership");

        // Fetch the current lease to get resourceVersion and verify we still hold it
        let current_lease = match leases.get_opt(&self.lease_name).await {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                debug!(identity = %self.identity, "Lease does not exist during resign, nothing to do");
                self.set_leader(false);
                return;
            }
            Err(e) => {
                warn!(identity = %self.identity, error = %e, "Failed to fetch lease for resign");
                self.set_leader(false);
                return;
            }
        };

        // Verify that we still hold the lease before clearing it
        let holder = current_lease
            .spec
            .as_ref()
            .and_then(|s| s.holder_identity.as_deref());
        if holder != Some(self.identity.as_str()) {
            debug!(
                identity = %self.identity,
                holder = ?holder,
                "Lease is no longer held by us, skipping resign"
            );
            self.set_leader(false);
            return;
        }

        // LS-4: Use replace() for resign as well, to enforce resourceVersion.
        let mut updated_lease = current_lease;
        if let Some(ref mut spec) = updated_lease.spec {
            spec.holder_identity = None;
        }

        if let Err(e) = leases
            .replace(
                &self.lease_name,
                &PostParams::default(),
                &updated_lease,
            )
            .await
        {
            warn!(identity = %self.identity, error = %e, "Failed to resign lease");
        }

        self.set_leader(false);
    }

    /// Build a new Lease resource.
    fn build_lease(&self) -> Lease {
        let now = chrono::Utc::now();
        Lease {
            metadata: kube::api::ObjectMeta {
                name: Some(self.lease_name.clone()),
                namespace: Some(self.namespace.clone()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                holder_identity: Some(self.identity.clone()),
                lease_duration_seconds: Some(self.lease_duration_secs),
                acquire_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now)),
                renew_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now)),
                ..Default::default()
            }),
        }
    }

    /// Update leader status with logging on transitions.
    /// Notifies observers when leadership is lost.
    fn set_leader(&self, leader: bool) {
        let was_leader = self.is_leader.swap(leader, Ordering::AcqRel);
        if was_leader && !leader {
            info!(identity = %self.identity, "Lost K8s lease leadership");
            *self.leadership_lost_at.lock() = Some(tokio::time::Instant::now());
            // Notify observers of leadership loss
            let _ = self.event_tx.send(LeadershipEvent::Lost);
        }
    }

    /// Record leadership gain: increment epoch and clear grace period.
    /// Notifies observers when leadership is gained.
    ///
    /// Ordering: (1) store is_leader=true, (2) SeqCst fence, (3) increment epoch,
    /// (4) log, (5) send Gained event. This ensures observers always see
    /// is_leader=true before the epoch changes and before the event arrives.
    fn gain_leadership(&self) {
        // Step 1: Set is_leader first so observers never see epoch change
        // while is_leader() still returns false.
        self.is_leader.store(true, Ordering::Release);

        // Step 2: Full fence ensures the is_leader store is visible to all
        // threads before the epoch increment below.
        std::sync::atomic::fence(Ordering::SeqCst);

        // Step 3: Increment epoch after is_leader is visible.
        let epoch = self.leader_epoch.fetch_add(1, Ordering::AcqRel) + 1;

        // Step 4: Clear grace period and log.
        *self.leadership_lost_at.lock() = None;
        info!(
            identity = %self.identity,
            epoch = epoch,
            "Gained K8s lease leadership"
        );

        // Step 5: Notify observers of leadership gain (after is_leader and epoch are set).
        let _ = self.event_tx.send(LeadershipEvent::Gained { epoch });
    }

    /// Returns `true` if we recently lost leadership and should wait before
    /// attempting to re-acquire.
    fn in_grace_period(&self) -> bool {
        let guard = self.leadership_lost_at.lock();
        if let Some(lost_at) = *guard {
            lost_at.elapsed() < Duration::from_secs(self.renew_interval_secs)
        } else {
            false
        }
    }
}

impl super::LeaderElect for K8sLeaderElector {
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<super::LeadershipEvent> {
        self.event_tx.subscribe()
    }
}
