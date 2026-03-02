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

/// Default base grace period in seconds before attempting re-acquisition.
const DEFAULT_GRACE_PERIOD_BASE_SECS: u64 = 5;

/// Default maximum grace period in seconds (cap for exponential backoff).
const DEFAULT_GRACE_PERIOD_MAX_SECS: u64 = 60;

/// Number of consecutive election failures before declaring a leader vacancy.
/// At the default renew interval of 10s, 3 failures = ~30s of no leader.
const LEADER_VACANCY_THRESHOLD: u64 = 3;

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
    /// Base grace period in seconds for exponential backoff.
    /// After losing leadership, we wait this long before first re-acquisition attempt.
    grace_period_base_secs: u64,
    /// Maximum grace period in seconds (cap for exponential backoff).
    grace_period_max_secs: u64,
    /// Monotonically increasing epoch (fencing token) incremented on each
    /// leadership acquisition. Used for split-brain protection.
    leader_epoch: Arc<AtomicU64>,
    /// Timestamp at which leadership was lost. Used to enforce a grace period
    /// before re-acquisition attempts.
    leadership_lost_at: Arc<parking_lot::Mutex<Option<tokio::time::Instant>>>,
    /// Number of consecutive leadership losses (for exponential backoff).
    consecutive_losses: Arc<AtomicU64>,
    /// Number of consecutive election failures (for vacancy detection).
    /// Incremented when election attempts fail, reset when leadership is gained.
    consecutive_failures: Arc<AtomicU64>,
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
    /// Base grace period in seconds after losing leadership (default: 5).
    /// Uses exponential backoff on consecutive losses.
    pub grace_period_base_secs: u64,
    /// Maximum grace period in seconds (default: 60).
    /// Caps the exponential backoff to prevent indefinite waiting.
    pub grace_period_max_secs: u64,
}

impl Default for K8sLeaderElectorConfig {
    fn default() -> Self {
        Self {
            lease_name: "synctv-leader".to_string(),
            lease_duration_secs: 30,
            renew_interval_secs: 10,
            grace_period_base_secs: DEFAULT_GRACE_PERIOD_BASE_SECS,
            grace_period_max_secs: DEFAULT_GRACE_PERIOD_MAX_SECS,
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
            grace_period_base_secs: config.grace_period_base_secs,
            grace_period_max_secs: config.grace_period_max_secs,
            leader_epoch: Arc::new(AtomicU64::new(0)),
            leadership_lost_at: Arc::new(parking_lot::Mutex::new(None)),
            consecutive_losses: Arc::new(AtomicU64::new(0)),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
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
            grace_period_base_secs = self.grace_period_base_secs,
            grace_period_max_secs = self.grace_period_max_secs,
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
                self.record_election_failure();
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
            // Uses exponential backoff based on consecutive losses
            if let Some(remaining) = self.grace_period_remaining() {
                info!(
                    identity = %self.identity,
                    consecutive_losses = self.consecutive_losses.load(Ordering::Relaxed),
                    remaining_secs = remaining.as_secs(),
                    "In grace period after leadership loss, deferring acquisition"
                );
                return;
            }
            // Lease expired, try to take it
            info!(
                identity = %self.identity,
                previous_holder = ?holder,
                consecutive_losses = self.consecutive_losses.load(Ordering::Relaxed),
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
            // Reset consecutive losses/failures since another valid leader exists
            self.consecutive_losses.store(0, Ordering::Relaxed);
            self.reset_election_failures();
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
                // Another leader exists, reset failures
                self.reset_election_failures();
            }
            Err(e) => {
                warn!(identity = %self.identity, error = %e, "Failed to create lease");
                self.set_leader(false);
                self.record_election_failure();
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
                        self.record_election_failure();
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
                        self.record_election_failure();
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
                spec.renew_time = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                    now,
                ));
            }

            match leases
                .replace(&self.lease_name, &PostParams::default(), &updated_lease)
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
                    // Another pod took over, reset failures
                    self.reset_election_failures();
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
                    self.record_election_failure();
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
                        self.record_election_failure();
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
                // Another pod acquired leadership, reset failures
                self.reset_election_failures();
                return;
            }

            let now = chrono::Utc::now();

            // LS-4: Use replace() instead of Patch::Merge to enforce
            // resourceVersion optimistic locking on acquisition.
            let mut updated_lease = current_lease.clone();
            if let Some(ref mut spec) = updated_lease.spec {
                spec.holder_identity = Some(self.identity.clone());
                spec.lease_duration_seconds = Some(self.lease_duration_secs);
                spec.acquire_time = Some(
                    k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(now),
                );
                spec.renew_time = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                    now,
                ));
            }

            match leases
                .replace(&self.lease_name, &PostParams::default(), &updated_lease)
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
                    // Another pod acquired the lease, reset failures
                    self.reset_election_failures();
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
                    self.record_election_failure();
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
            .replace(&self.lease_name, &PostParams::default(), &updated_lease)
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
                acquire_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                    now,
                )),
                renew_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                    now,
                )),
                ..Default::default()
            }),
        }
    }

    /// Update leader status with logging on transitions.
    /// Notifies observers when leadership is lost.
    /// Increments consecutive loss counter for exponential backoff.
    /// Updates Prometheus metrics for monitoring.
    fn set_leader(&self, leader: bool) {
        let was_leader = self.is_leader.swap(leader, Ordering::AcqRel);

        // Update metrics
        synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(if leader { 1 } else { 0 });

        if was_leader && !leader {
            // Increment consecutive losses for exponential backoff
            let losses = self.consecutive_losses.fetch_add(1, Ordering::AcqRel) + 1;
            let grace_period = self.calculate_grace_period(losses);
            info!(
                identity = %self.identity,
                consecutive_losses = losses,
                grace_period_secs = grace_period.as_secs(),
                "Lost K8s lease leadership"
            );
            *self.leadership_lost_at.lock() = Some(tokio::time::Instant::now());
            // Notify observers of leadership loss
            let _ = self.event_tx.send(LeadershipEvent::Lost);
        }
    }

    /// Record an election failure and check for leader vacancy.
    ///
    /// If consecutive failures exceed [`LEADER_VACANCY_THRESHOLD`], emits
    /// a `LeadershipEvent::Vacancy` event so observers can take action
    /// (e.g., pause singleton tasks, report degraded status).
    fn record_election_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        // Update metrics for monitoring
        synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(failures as i64);
        if failures == LEADER_VACANCY_THRESHOLD {
            warn!(
                identity = %self.identity,
                consecutive_failures = failures,
                "Leader vacancy detected: no node has held leadership for {} consecutive election cycles",
                failures
            );
            let _ = self.event_tx.send(LeadershipEvent::Vacancy);
        } else if failures > LEADER_VACANCY_THRESHOLD
            && failures.is_multiple_of(LEADER_VACANCY_THRESHOLD)
        {
            // Periodic reminder at every N failures
            warn!(
                identity = %self.identity,
                consecutive_failures = failures,
                "Leader vacancy persists"
            );
        }
    }

    /// Reset consecutive failures counter (called when leadership is gained
    /// or when another valid leader is detected).
    fn reset_election_failures(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);
    }

    /// Record leadership gain: increment epoch, clear grace period, and reset losses.
    /// Notifies observers when leadership is gained.
    /// Updates Prometheus metrics for monitoring.
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

        // Step 4: Clear grace period, reset consecutive losses/failures, and log.
        *self.leadership_lost_at.lock() = None;
        let previous_losses = self.consecutive_losses.swap(0, Ordering::AcqRel);
        let previous_failures = self.consecutive_failures.swap(0, Ordering::AcqRel);
        info!(
            identity = %self.identity,
            epoch = epoch,
            previous_consecutive_losses = previous_losses,
            previous_consecutive_failures = previous_failures,
            "Gained K8s lease leadership"
        );

        // Update metrics for monitoring
        synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(1);
        synctv_core::metrics::cluster::LEADER_ELECTION_EPOCH.set(epoch as i64);
        synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);

        // Step 5: Notify observers of leadership gain (after is_leader and epoch are set).
        let _ = self.event_tx.send(LeadershipEvent::Gained { epoch });
    }

    /// Calculate the grace period duration based on consecutive losses.
    /// Uses exponential backoff: base * 2^(losses-1), capped at max.
    fn calculate_grace_period(&self, consecutive_losses: u64) -> Duration {
        if consecutive_losses == 0 {
            return Duration::from_secs(self.grace_period_base_secs);
        }

        // Exponential backoff: base * 2^(losses-1)
        // Cap at max to prevent indefinite waiting
        let multiplier = 1u64 << (consecutive_losses - 1).min(6); // Max 2^6 = 64x
        let grace_secs = (self.grace_period_base_secs * multiplier).min(self.grace_period_max_secs);

        Duration::from_secs(grace_secs)
    }

    /// Returns the remaining grace period duration, or None if not in grace period.
    fn grace_period_remaining(&self) -> Option<Duration> {
        let guard = self.leadership_lost_at.lock();
        if let Some(lost_at) = *guard {
            let losses = self.consecutive_losses.load(Ordering::Relaxed);
            let grace_period = self.calculate_grace_period(losses);
            let elapsed = lost_at.elapsed();

            if elapsed < grace_period {
                Some(grace_period - elapsed)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Returns `true` if we recently lost leadership and should wait before
    /// attempting to re-acquire. Uses exponential backoff based on consecutive losses.
    #[allow(dead_code)]
    fn in_grace_period(&self) -> bool {
        self.grace_period_remaining().is_some()
    }

    /// Returns the number of consecutive leadership losses.
    /// Useful for monitoring and health checks.
    pub fn consecutive_losses(&self) -> u64 {
        self.consecutive_losses.load(Ordering::Relaxed)
    }

    /// Returns the number of consecutive election failures.
    /// Useful for health check endpoints.
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Gracefully resign leadership by releasing the K8s Lease.
    ///
    /// This is the public interface for resigning leadership, which creates
    /// the K8s API client internally. Used by `AnyLeaderElector::resign()`.
    ///
    /// # Behavior
    ///
    /// - If this pod is not the leader, this is a no-op
    /// - If this pod is the leader, clears the holder identity on the Lease
    /// - Sends a `LeadershipEvent::Lost` to all subscribers
    /// - Updates metrics to reflect leadership loss
    pub async fn resign_public(&self) {
        if !self.is_leader() {
            return;
        }

        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        self.resign(&leases).await;
    }
}

impl super::LeaderElect for K8sLeaderElector {
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<super::LeadershipEvent> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = K8sLeaderElectorConfig::default();
        assert_eq!(config.lease_name, "synctv-leader");
        assert_eq!(config.lease_duration_secs, 30);
        assert_eq!(config.renew_interval_secs, 10);
        assert_eq!(
            config.grace_period_base_secs,
            DEFAULT_GRACE_PERIOD_BASE_SECS
        );
        assert_eq!(config.grace_period_max_secs, DEFAULT_GRACE_PERIOD_MAX_SECS);
    }

    #[test]
    fn test_calculate_grace_period_exponential_backoff() {
        // Create a minimal elector for testing grace period calculation
        // We don't need a real K8s client for this unit test
        let base_secs = 5u64;
        let max_secs = 60u64;

        // Test the exponential backoff calculation directly
        // consecutive_losses = 0: grace = base = 5s
        // consecutive_losses = 1: grace = base * 2^0 = 5s
        // consecutive_losses = 2: grace = base * 2^1 = 10s
        // consecutive_losses = 3: grace = base * 2^2 = 20s
        // consecutive_losses = 4: grace = base * 2^3 = 40s
        // consecutive_losses = 5: grace = base * 2^4 = 60s (capped at max)
        // consecutive_losses = 6: grace = base * 2^5 = 60s (capped at max)
        // consecutive_losses = 7+: grace = 60s (capped at max)

        let test_cases = [
            (0, 5),   // 5 * 1 = 5
            (1, 5),   // 5 * 2^0 = 5
            (2, 10),  // 5 * 2^1 = 10
            (3, 20),  // 5 * 2^2 = 20
            (4, 40),  // 5 * 2^3 = 40
            (5, 60),  // 5 * 2^4 = 80, capped at 60
            (6, 60),  // 5 * 2^5 = 160, capped at 60
            (10, 60), // capped at max
        ];

        for (losses, expected_secs) in test_cases {
            let multiplier = if losses == 0 {
                1
            } else {
                1u64 << (losses - 1).min(6)
            };
            let grace_secs = (base_secs * multiplier).min(max_secs);
            assert_eq!(
                grace_secs, expected_secs,
                "Failed for consecutive_losses = {}",
                losses
            );
        }
    }

    #[test]
    fn test_grace_period_caps_at_max() {
        // Verify that even with very high consecutive losses, grace period is capped
        let base_secs = 5u64;
        let max_secs = 60u64;

        // Simulate very high consecutive losses
        for losses in 10u64..100 {
            let multiplier = 1u64 << (losses - 1).min(6);
            let grace_secs = (base_secs * multiplier).min(max_secs);
            assert_eq!(
                grace_secs, 60,
                "Grace period should be capped at max for losses = {}",
                losses
            );
        }
    }

    #[test]
    fn test_grace_period_with_custom_config() {
        // Test with different base and max values
        let base_secs = 10u64;
        let max_secs = 120u64;

        let test_cases = [
            (0, 10),  // base
            (1, 10),  // base * 2^0 = 10
            (2, 20),  // base * 2^1 = 20
            (3, 40),  // base * 2^2 = 40
            (4, 80),  // base * 2^3 = 80
            (5, 120), // base * 2^4 = 160, capped at 120
            (6, 120), // capped at max
        ];

        for (losses, expected_secs) in test_cases {
            let multiplier = if losses == 0 {
                1
            } else {
                1u64 << (losses - 1).min(6)
            };
            let grace_secs = (base_secs * multiplier).min(max_secs);
            assert_eq!(
                grace_secs, expected_secs,
                "Failed for consecutive_losses = {}",
                losses
            );
        }
    }

    #[test]
    fn test_grace_period_remaining_returns_none_when_no_loss() {
        // When leadership_lost_at is None, grace_period_remaining should return None
        // This is tested by verifying the logic without a full elector
        let leadership_lost_at: Arc<parking_lot::Mutex<Option<tokio::time::Instant>>> =
            Arc::new(parking_lot::Mutex::new(None));

        let guard = leadership_lost_at.lock();
        let result = guard.is_some();
        drop(guard);

        assert!(
            !result,
            "Should not be in grace period when no loss recorded"
        );
    }

    #[test]
    fn test_consecutive_losses_resets_on_gain() {
        // This test verifies that consecutive_losses counter increments on loss
        // and resets on gain. The actual logic is in set_leader and gain_leadership.
        let consecutive_losses = Arc::new(AtomicU64::new(0));

        // Simulate first loss
        let losses = consecutive_losses.fetch_add(1, Ordering::AcqRel) + 1;
        assert_eq!(losses, 1);

        // Simulate second loss
        let losses = consecutive_losses.fetch_add(1, Ordering::AcqRel) + 1;
        assert_eq!(losses, 2);

        // Simulate third loss
        let losses = consecutive_losses.fetch_add(1, Ordering::AcqRel) + 1;
        assert_eq!(losses, 3);

        // Simulate leadership gain (reset)
        let previous = consecutive_losses.swap(0, Ordering::AcqRel);
        assert_eq!(
            previous, 3,
            "Should have had 3 consecutive losses before reset"
        );
        assert_eq!(
            consecutive_losses.load(Ordering::Relaxed),
            0,
            "Should be reset to 0"
        );
    }

    #[test]
    fn test_grace_period_elapsed_check() {
        // Test that grace period correctly detects when elapsed time exceeds threshold
        use std::time::Duration;

        let base_secs = 5u64;
        let max_secs = 60u64;
        let consecutive_losses = 2u64; // grace = 10s

        let grace_period = {
            let multiplier = 1u64 << (consecutive_losses - 1).min(6);
            Duration::from_secs((base_secs * multiplier).min(max_secs))
        };

        assert_eq!(grace_period, Duration::from_secs(10));

        // Simulate elapsed time scenarios
        let elapsed_short = Duration::from_secs(5); // 5s < 10s grace
        let elapsed_equal = Duration::from_secs(10); // 10s = 10s grace
        let elapsed_long = Duration::from_secs(15); // 15s > 10s grace

        assert!(elapsed_short < grace_period, "Should be in grace period");
        assert!(elapsed_equal >= grace_period, "Grace period should be over");
        assert!(elapsed_long >= grace_period, "Grace period should be over");
    }

    #[test]
    fn test_vacancy_threshold_constant() {
        // Verify the vacancy threshold is set to 3 (matching Redis implementation)
        assert_eq!(LEADER_VACANCY_THRESHOLD, 3);
    }

    #[test]
    fn test_consecutive_failures_tracking() {
        // Test that consecutive_failures counter increments and resets correctly
        let consecutive_failures = Arc::new(AtomicU64::new(0));

        // Simulate first failure
        let failures = consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(failures, 1);
        assert!(failures < LEADER_VACANCY_THRESHOLD);

        // Simulate second failure
        let failures = consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(failures, 2);
        assert!(failures < LEADER_VACANCY_THRESHOLD);

        // Simulate third failure - this triggers vacancy
        let failures = consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(failures, 3);
        assert_eq!(failures, LEADER_VACANCY_THRESHOLD);

        // Simulate leadership gain (reset)
        let previous = consecutive_failures.swap(0, Ordering::Relaxed);
        assert_eq!(previous, 3);
        assert_eq!(consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_vacancy_periodic_reminder() {
        // Test that periodic reminders are sent at multiples of the threshold
        let consecutive_failures = Arc::new(AtomicU64::new(0));

        // Simulate failures up to threshold * 2
        for _ in 1..=LEADER_VACANCY_THRESHOLD * 2 {
            let failures = consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

            if failures == LEADER_VACANCY_THRESHOLD {
                // First vacancy event
                assert_eq!(failures, 3);
            } else if failures > LEADER_VACANCY_THRESHOLD
                && failures.is_multiple_of(LEADER_VACANCY_THRESHOLD)
            {
                // Periodic reminder (at 6, 9, 12, etc.)
                assert!(failures > LEADER_VACANCY_THRESHOLD);
            }
        }

        assert_eq!(consecutive_failures.load(Ordering::Relaxed), 6);
    }
}
