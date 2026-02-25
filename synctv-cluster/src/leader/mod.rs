//! Leader election for singleton operations in multi-replica deployments.
//!
//! Two implementations are provided:
//!
//! - **Redis-based** ([`LeaderElector`]): Uses Redis distributed locks.
//!   Works in any deployment (bare-metal, Docker Compose, VM, K8s).
//!
//! - **K8s Lease-based** ([`K8sLeaderElector`]): Uses the native
//!   `coordination.k8s.io/v1` Lease resource via in-cluster kube client.
//!   Preferred for Kubernetes deployments as it integrates with RBAC
//!   and doesn't require Redis for leader election.
//!   Requires the `k8s` feature flag.
//!
//! # Usage
//!
//! ```text
//! // Redis-based (any deployment):
//! let elector = LeaderElector::new(redis_conn, node_id, "synctv:");
//! elector.start(cancel_token.clone());
//!
//! // K8s Lease-based (K8s only, requires "k8s" feature):
//! let elector = K8sLeaderElector::new(pod_name, namespace, config).await?;
//! elector.start(cancel_token.clone());
//!
//! if elector.is_leader() {
//!     // run singleton tasks
//! }
//! ```

#[cfg(feature = "k8s")]
pub mod k8s_lease;
#[cfg(feature = "k8s")]
pub use k8s_lease::{K8sLeaderElector, K8sLeaderElectorConfig};

/// Unified leader elector that supports both Redis and K8s modes.
///
/// This enum allows the application to dynamically switch between
/// Redis-based and K8s Lease-based leader election at runtime.
#[derive(Clone)]
pub enum AnyLeaderElector {
    /// Redis-based leader election (works in any deployment)
    Redis(LeaderElector),
    /// Kubernetes Lease-based leader election (K8s only, requires `k8s` feature)
    #[cfg(feature = "k8s")]
    K8s(K8sLeaderElector),
    /// Disabled leader election (non-cluster mode).
    ///
    /// Used when running in standalone mode without Redis.
    /// `is_leader()` always returns `false`, meaning this node never runs
    /// cluster-wide singleton tasks. In standalone mode, each node runs
    /// its own local tasks without coordination.
    Disabled,
}

impl AnyLeaderElector {
    /// Returns `true` if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        match self {
            AnyLeaderElector::Redis(e) => e.is_leader(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.is_leader(),
            AnyLeaderElector::Disabled => false,
        }
    }

    /// Returns the identity of the current leader if this node is the leader.
    ///
    /// Returns `Some(identity)` if this node currently holds leadership,
    /// `None` otherwise. For querying the identity of a remote leader,
    /// check the distributed lock directly.
    pub fn current_leader_identity(&self) -> Option<String> {
        match self {
            AnyLeaderElector::Redis(e) => e.current_leader_identity(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => {
                if e.is_leader() {
                    Some(e.identity().to_string())
                } else {
                    None
                }
            }
            AnyLeaderElector::Disabled => None,
        }
    }

    /// Returns the current leader epoch (fencing token).
    ///
    /// The epoch is monotonically increasing and changes each time this node
    /// acquires leadership. Callers should capture the epoch before starting a
    /// singleton task and re-check it after to detect leadership loss.
    ///
    /// Returns 0 if this node has never been leader.
    pub fn leader_epoch(&self) -> u64 {
        match self {
            AnyLeaderElector::Redis(e) => e.leader_epoch(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.leader_epoch(),
            AnyLeaderElector::Disabled => 0,
        }
    }

    /// Subscribe to leadership change events (observer pattern).
    ///
    /// Returns a receiver that will receive `LeadershipEvent::Gained` when this
    /// node becomes leader and `LeadershipEvent::Lost` when it loses leadership.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        match self {
            AnyLeaderElector::Redis(e) => e.subscribe(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.subscribe(),
            AnyLeaderElector::Disabled => {
                // Create a closed channel that never sends events.
                // Since the Disabled elector never becomes leader,
                // subscribers will only see channel closure when dropped.
                let (tx, rx) = broadcast::channel(1);
                drop(tx); // Close immediately
                rx
            }
        }
    }

    /// Start the leader election loop.
    ///
    /// Spawns a background task that continuously tries to acquire or
    /// renew leadership. The task runs until the `cancel_token` is cancelled.
    pub fn start(&self, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        match self {
            AnyLeaderElector::Redis(e) => e.start(cancel_token),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.start(cancel_token),
            AnyLeaderElector::Disabled => {
                // No-op: Disabled elector doesn't run any background task.
                // Return a handle that completes immediately when cancelled.
                tokio::spawn(async move {
                    cancel_token.cancelled().await;
                })
            }
        }
    }

}

impl LeaderElect for AnyLeaderElector {
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        match self {
            AnyLeaderElector::Redis(e) => e.event_tx.subscribe(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.subscribe(),
            AnyLeaderElector::Disabled => {
                // Create a closed channel that never sends events.
                let (tx, rx) = broadcast::channel(1);
                drop(tx);
                rx
            }
        }
    }
}

impl synctv_core::service::LeaderCheck for AnyLeaderElector {
    fn is_leader(&self) -> bool {
        self.is_leader()
    }
}

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex as TokioMutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use synctv_core::service::DistributedLock;

/// Leadership change event for observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadershipEvent {
    /// This node gained leadership (includes the new epoch).
    Gained { epoch: u64 },
    /// This node lost leadership.
    Lost,
    /// No node holds leadership after multiple consecutive failures.
    /// Observers should enter a safe "no leader" state.
    Vacancy,
}

/// Trait for types that participate in leader election.
///
/// Provides a default [`leader_guard`](Self::leader_guard) implementation
/// built on top of [`subscribe`](Self::subscribe), eliminating duplicated
/// fencing-guard logic across each concrete elector.
pub trait LeaderElect {
    /// Subscribe to leadership change events.
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent>;

    /// Create a fencing guard that is automatically cancelled when leadership is lost.
    ///
    /// Returns a `CancellationToken` that singleton tasks should use as their
    /// cancellation signal. When this node loses leadership (receives a `Lost`
    /// event), the token is cancelled, causing the singleton task to stop.
    ///
    /// This prevents split-brain scenarios where a demoted leader continues
    /// running singleton tasks that should only execute on the current leader.
    #[must_use]
    fn leader_guard(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let mut rx = self.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(LeadershipEvent::Lost) | Ok(LeadershipEvent::Vacancy) => {
                        token_clone.cancel();
                        break;
                    }
                    Ok(LeadershipEvent::Gained { .. }) => {
                        // Still leader or re-elected, continue watching
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Missed events; cancel to be safe (singleton will restart
                        // on next election).
                        token_clone.cancel();
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Channel closed (elector dropped), cancel the guard
                        token_clone.cancel();
                        break;
                    }
                }
            }
        });
        token
    }
}

/// Default Redis key for leader election lock (used when no prefix is configured).
const DEFAULT_LEADER_LOCK_KEY: &str = "leader_election";

/// Number of consecutive election failures before declaring a leader vacancy.
/// At the default renew interval of 10s, 3 failures = ~30s of no leader.
const LEADER_VACANCY_THRESHOLD: u64 = 3;

/// Leader election using Redis distributed locks.
///
/// Only one replica at a time holds the lock and is considered the leader.
/// The leader performs singleton tasks like database migrations and
/// periodic cleanup (expired token cleanup, audit log pruning).
///
/// ## Split-brain protection
///
/// Each leadership acquisition increments a monotonic epoch (fencing token).
/// Callers can use [`leader_epoch()`](Self::leader_epoch) to validate that
/// leadership has not changed between the start and end of a singleton task.
///
/// On any Redis operation failure, `is_leader` is set to `false` immediately
/// (no waiting for the next tick). After losing leadership, a grace period
/// equal to `renew_interval_secs` is enforced before attempting re-acquisition
/// to prevent rapid flip-flopping during transient network issues.
///
/// ## Observer pattern
///
/// Use `subscribe()` to receive leadership change notifications (gained/lost
/// events). Observers can use this to start/stop singleton tasks when
/// leadership changes.
#[derive(Clone)]
pub struct LeaderElector {
    /// Whether this instance is currently the leader
    is_leader: Arc<AtomicBool>,
    /// Distributed lock service
    lock: DistributedLock,
    /// Identity of this node (pod name or generated node ID)
    identity: String,
    /// Lease duration in seconds (how long the lock is held before expiry)
    lease_duration_secs: u64,
    /// How often to attempt renewal, in seconds (must be < lease_duration_secs)
    renew_interval_secs: u64,
    /// Current lock value (used for renewal and release)
    lock_value: Arc<TokioMutex<Option<String>>>,
    /// Redis key used for the leader election lock (includes configured prefix)
    lock_key: String,
    /// Monotonically increasing epoch (fencing token) incremented on each
    /// leadership acquisition. Used for split-brain protection.
    leader_epoch: Arc<AtomicU64>,
    /// Timestamp (Instant) at which leadership was lost. Used to enforce a
    /// grace period before re-acquisition attempts.
    leadership_lost_at: Arc<TokioMutex<Option<tokio::time::Instant>>>,
    /// Broadcast channel for leadership change events (observer pattern)
    event_tx: Arc<broadcast::Sender<LeadershipEvent>>,
    /// Number of consecutive election failures (acquire or renew).
    /// Used to detect prolonged leader vacancy.
    consecutive_failures: Arc<AtomicU64>,
}

/// Configuration for leader election.
pub struct LeaderElectorConfig {
    /// Lease duration in seconds (default: 30)
    pub lease_duration_secs: u64,
    /// Renewal interval in seconds (default: 10, must be < lease_duration_secs)
    pub renew_interval_secs: u64,
}

impl Default for LeaderElectorConfig {
    fn default() -> Self {
        Self {
            lease_duration_secs: 30,
            renew_interval_secs: 10,
        }
    }
}

impl LeaderElector {
    /// Create a new leader elector with default configuration.
    ///
    /// The `key_prefix` is prepended to the leader election lock key in Redis
    /// (e.g. `"synctv:"` produces the lock key `synctv:leader_election`).
    /// Pass an empty string to use the unprefixed default key.
    pub fn new(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
        key_prefix: &str,
    ) -> Self {
        Self::with_config(redis_conn, identity, LeaderElectorConfig::default(), key_prefix)
    }

    /// Create a new leader elector with custom configuration.
    ///
    /// The `key_prefix` is prepended to the leader election lock key in Redis.
    pub fn with_config(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
        config: LeaderElectorConfig,
        key_prefix: &str,
    ) -> Self {
        assert!(
            config.renew_interval_secs < config.lease_duration_secs,
            "renew_interval_secs ({}) must be less than lease_duration_secs ({})",
            config.renew_interval_secs,
            config.lease_duration_secs
        );

        let (event_tx, _) = broadcast::channel(16);

        Self {
            is_leader: Arc::new(AtomicBool::new(false)),
            lock: DistributedLock::new(redis_conn),
            identity,
            lease_duration_secs: config.lease_duration_secs,
            renew_interval_secs: config.renew_interval_secs,
            lock_value: Arc::new(TokioMutex::new(None)),
            lock_key: format!("{}{}", key_prefix, DEFAULT_LEADER_LOCK_KEY),
            leader_epoch: Arc::new(AtomicU64::new(0)),
            leadership_lost_at: Arc::new(TokioMutex::new(None)),
            event_tx: Arc::new(event_tx),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
        }
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
    /// The epoch is monotonically increasing and increments each time this node
    /// acquires leadership. Callers should capture the epoch before starting a
    /// singleton task and compare it afterwards to detect if leadership was lost
    /// and re-acquired during the task.
    ///
    /// Returns 0 if this node has never been leader.
    pub fn leader_epoch(&self) -> u64 {
        self.leader_epoch.load(Ordering::Acquire)
    }

    /// Returns the identity of this node if it is currently the leader.
    ///
    /// Returns `Some(identity)` when this node holds the leader lock,
    /// `None` otherwise.
    pub fn current_leader_identity(&self) -> Option<String> {
        if self.is_leader() {
            Some(self.identity.clone())
        } else {
            None
        }
    }

    /// Returns this node's identity string.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Start the leader election loop.
    ///
    /// This spawns a background task that continuously tries to acquire or
    /// renew leadership. The task runs until the `cancel_token` is cancelled.
    ///
    /// Returns a `JoinHandle` for the background task.
    pub fn start(&self, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        let elector = self.clone();
        tokio::spawn(async move {
            elector.run_loop(cancel_token).await;
        })
    }

    /// Main election loop: try to acquire or renew the lock periodically.
    async fn run_loop(&self, cancel_token: CancellationToken) {
        let mut ticker = tokio::time::interval(Duration::from_secs(self.renew_interval_secs));
        // The first tick fires immediately, ensuring the first election attempt
        // happens without waiting for `renew_interval_secs`.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!(
            identity = %self.identity,
            lease_duration_secs = self.lease_duration_secs,
            renew_interval_secs = self.renew_interval_secs,
            "Leader election started"
        );

        loop {
            tokio::select! {
                () = cancel_token.cancelled() => {
                    info!(identity = %self.identity, "Leader election shutting down");
                    self.resign().await;
                    break;
                }
                _ = ticker.tick() => {
                    self.try_acquire_or_renew().await;
                }
            }
        }
    }

    /// Try to acquire the lock or renew it if we already hold it.
    ///
    /// Detects Sentinel failover errors (READONLY/LOADING) and immediately
    /// resets local state for a fast retry on the next tick. Tracks consecutive
    /// failures to detect prolonged leader vacancy.
    async fn try_acquire_or_renew(&self) {
        let current_value = self.lock_value.lock().await.clone();

        if let Some(ref value) = current_value {
            // We think we're the leader; try to extend the lock.
            match self.lock.extend(&self.lock_key, value, self.lease_duration_secs).await {
                Ok(true) => {
                    debug!(identity = %self.identity, "Leader lease renewed");
                    self.consecutive_failures.store(0, Ordering::Relaxed);
                    // Still the leader
                }
                Ok(false) => {
                    // Lock expired or was taken by someone else
                    warn!(identity = %self.identity, "Leader lease renewal failed (lock lost)");
                    self.lose_leadership().await;
                    self.record_election_failure();
                }
                Err(e) => {
                    let error_str = e.to_string();
                    let is_failover = error_str.contains("READONLY") || error_str.contains("LOADING");

                    if is_failover {
                        warn!(
                            identity = %self.identity,
                            error = %e,
                            "Sentinel failover detected during lease renewal, resetting leader state for immediate retry"
                        );
                    } else {
                        warn!(
                            identity = %self.identity,
                            error = %e,
                            "Leader lease renewal failed (Redis error)"
                        );
                    }

                    // Immediately assume we lost leadership on error,
                    // because we can't confirm the lock still exists.
                    self.lose_leadership().await;

                    if is_failover {
                        // On failover, set a short grace period (2s) before retrying
                        // instead of clearing entirely. This prevents rapid flip-flopping
                        // if the new primary is not yet ready to accept writes.
                        *self.leadership_lost_at.lock().await = Some(
                            tokio::time::Instant::now() - Duration::from_secs(self.renew_interval_secs) + Duration::from_secs(2)
                        );
                    }

                    self.record_election_failure();
                }
            }
        } else {
            // Not currently the leader; check grace period before attempting re-acquire.
            if self.in_grace_period().await {
                debug!(
                    identity = %self.identity,
                    "In grace period after leadership loss, deferring acquisition"
                );
                return;
            }
            self.try_acquire().await;
        }
    }

    /// Immediately mark this node as no longer the leader, clear the lock
    /// value, and record the time of loss for grace period enforcement.
    async fn lose_leadership(&self) {
        self.set_leader(false, None);
        *self.lock_value.lock().await = None;
        *self.leadership_lost_at.lock().await = Some(tokio::time::Instant::now());
    }

    /// Returns `true` if we recently lost leadership and should wait before
    /// attempting to re-acquire. The grace period equals `renew_interval_secs`
    /// to avoid rapid flip-flopping during transient Redis issues.
    async fn in_grace_period(&self) -> bool {
        let guard = self.leadership_lost_at.lock().await;
        if let Some(lost_at) = *guard {
            lost_at.elapsed() < Duration::from_secs(self.renew_interval_secs)
        } else {
            false
        }
    }

    /// Try to acquire the leadership lock.
    async fn try_acquire(&self) {
        match self.lock.acquire(&self.lock_key, self.lease_duration_secs).await {
            Ok(Some(value)) => {
                let epoch = self.leader_epoch.fetch_add(1, Ordering::AcqRel) + 1;
                info!(
                    identity = %self.identity,
                    epoch = epoch,
                    "Became leader"
                );
                *self.lock_value.lock().await = Some(value);
                // Clear grace period since we successfully acquired
                *self.leadership_lost_at.lock().await = None;
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.set_leader(true, Some(epoch));
            }
            Ok(None) => {
                debug!(identity = %self.identity, "Another node is leader");
                // Another node is leader -- not a failure, reset counter
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.set_leader(false, None);
            }
            Err(e) => {
                let error_str = e.to_string();
                let is_failover = error_str.contains("READONLY") || error_str.contains("LOADING");

                if is_failover {
                    warn!(
                        identity = %self.identity,
                        error = %e,
                        "Sentinel failover detected during lock acquisition, will retry after short grace period"
                    );
                    // Set a short grace period (2s) before retrying instead of clearing
                    // entirely. This prevents rapid flip-flopping if the new primary is
                    // not yet ready to accept writes.
                    *self.leadership_lost_at.lock().await = Some(
                        tokio::time::Instant::now() - Duration::from_secs(self.renew_interval_secs) + Duration::from_secs(2)
                    );
                } else {
                    warn!(
                        identity = %self.identity,
                        error = %e,
                        "Failed to acquire leader lock"
                    );
                }

                self.record_election_failure();
                self.set_leader(false, None);
            }
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
        } else if failures > LEADER_VACANCY_THRESHOLD && failures % LEADER_VACANCY_THRESHOLD == 0 {
            // Periodic reminder at every N failures
            warn!(
                identity = %self.identity,
                consecutive_failures = failures,
                "Leader vacancy persists"
            );
        }
    }

    /// Returns the number of consecutive election failures.
    /// Useful for health check endpoints.
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Gracefully resign leadership by releasing the lock.
    async fn resign(&self) {
        let value = self.lock_value.lock().await.take();
        if let Some(value) = value {
            info!(identity = %self.identity, "Resigning leadership");
            if let Err(e) = self.lock.release(&self.lock_key, &value).await {
                warn!(
                    identity = %self.identity,
                    error = %e,
                    "Failed to release leader lock during resignation"
                );
            }
        }
        self.set_leader(false, None);
    }

    /// Update the is_leader flag and log transitions.
    /// Notifies observers of leadership changes and updates metrics.
    fn set_leader(&self, leader: bool, gained_epoch: Option<u64>) {
        let was_leader = self.is_leader.swap(leader, Ordering::AcqRel);

        // Update metrics
        synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(if leader { 1 } else { 0 });
        if let Some(epoch) = gained_epoch {
            synctv_core::metrics::cluster::LEADER_ELECTION_EPOCH.set(epoch as i64);
        }
        // Reset consecutive failures on successful leadership (gain or maintained)
        if leader {
            synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(0);
        }

        if was_leader && !leader {
            info!(identity = %self.identity, "Lost leadership");
            // Notify observers of leadership loss
            let _ = self.event_tx.send(LeadershipEvent::Lost);
        } else if !was_leader && leader {
            // Notify observers of leadership gain (epoch provided by caller)
            if let Some(epoch) = gained_epoch {
                let _ = self.event_tx.send(LeadershipEvent::Gained { epoch });
            }
        }
    }
}

impl LeaderElect for LeaderElector {
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LeaderElectorConfig::default();
        assert_eq!(config.lease_duration_secs, 30);
        assert_eq!(config.renew_interval_secs, 10);
    }

    #[test]
    #[should_panic(expected = "renew_interval_secs")]
    fn test_invalid_config_panics() {
        // We can't construct a real ConnectionManager synchronously, so we
        // replicate the assertion logic from `with_config` directly.
        let config = LeaderElectorConfig {
            lease_duration_secs: 10,
            renew_interval_secs: 20, // > lease_duration_secs, should panic
        };
        assert!(
            config.renew_interval_secs < config.lease_duration_secs,
            "renew_interval_secs ({}) must be less than lease_duration_secs ({})",
            config.renew_interval_secs,
            config.lease_duration_secs
        );
    }

    // ============================================================================
    // Tests for Disabled variant
    // ============================================================================

    #[test]
    fn test_disabled_is_leader_returns_false() {
        let elector = AnyLeaderElector::Disabled;
        assert!(
            !elector.is_leader(),
            "Disabled elector should never be leader"
        );
    }

    #[test]
    fn test_disabled_current_leader_identity_returns_none() {
        let elector = AnyLeaderElector::Disabled;
        assert_eq!(
            elector.current_leader_identity(),
            None,
            "Disabled elector should have no leader identity"
        );
    }

    #[test]
    fn test_disabled_leader_epoch_returns_zero() {
        let elector = AnyLeaderElector::Disabled;
        assert_eq!(
            elector.leader_epoch(),
            0,
            "Disabled elector should have epoch 0"
        );
    }

    #[tokio::test]
    async fn test_disabled_subscribe_returns_closed_channel() {
        let elector = AnyLeaderElector::Disabled;
        let mut rx = elector.subscribe();

        // Channel should be closed (no sender)
        use broadcast::error::TryRecvError;
        let result = rx.try_recv();
        assert!(
            matches!(result, Err(TryRecvError::Closed)),
            "Disabled elector should have a closed subscription channel"
        );
    }

    #[tokio::test]
    async fn test_disabled_start_returns_task_that_completes_on_cancel() {
        let elector = AnyLeaderElector::Disabled;
        let cancel_token = CancellationToken::new();

        let handle = elector.start(cancel_token.clone());

        // Task should be running (waiting for cancel)
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(!handle.is_finished(), "Task should be running while not cancelled");

        // Cancel and wait for completion
        cancel_token.cancel();
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        assert!(handle.is_finished(), "Task should complete after cancellation");

        // Clean up
        handle.abort();
    }

    #[test]
    fn test_disabled_leader_check_trait() {
        use synctv_core::service::LeaderCheck;
        let elector = AnyLeaderElector::Disabled;
        assert!(!LeaderCheck::is_leader(&elector));
    }

    #[tokio::test]
    async fn test_disabled_leader_guard_cancelled_on_channel_close() {
        // The leader_guard creates a CancellationToken and spawns a task
        // that watches the subscription channel. When the channel closes,
        // the guard should be cancelled.
        let elector = AnyLeaderElector::Disabled;
        let guard = elector.leader_guard();

        // The subscription channel is already closed, so the spawned task
        // should cancel the guard almost immediately.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert!(
            guard.is_cancelled(),
            "Guard should be cancelled because Disabled elector has closed channel"
        );
    }
}
