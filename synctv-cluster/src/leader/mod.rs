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
//! ```ignore
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
}

impl AnyLeaderElector {
    /// Returns `true` if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        match self {
            AnyLeaderElector::Redis(e) => e.is_leader(),
            #[cfg(feature = "k8s")]
            AnyLeaderElector::K8s(e) => e.is_leader(),
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
        }
    }

    /// Create a fencing guard that is automatically cancelled when leadership is lost.
    ///
    /// Returns a `CancellationToken` that singleton tasks should use as their
    /// cancellation signal. When this node loses leadership (receives a `Lost`
    /// event), the token is cancelled, causing the singleton task to stop.
    ///
    /// This prevents split-brain scenarios where a demoted leader continues
    /// running singleton tasks that should only execute on the current leader.
    #[must_use]
    pub fn leader_guard(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let mut rx = self.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(LeadershipEvent::Lost) => {
                        token_clone.cancel();
                        break;
                    }
                    Ok(LeadershipEvent::Gained { .. }) => {
                        // Still leader or re-elected, continue watching
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Missed events; check current state is ambiguous,
                        // so cancel to be safe (singleton will restart on next election)
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

impl synctv_core::service::LeaderCheck for AnyLeaderElector {
    fn is_leader(&self) -> bool {
        self.is_leader()
    }
}

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
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
}

/// Default Redis key for leader election lock (used when no prefix is configured).
const DEFAULT_LEADER_LOCK_KEY: &str = "leader_election";

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
    lock_value: Arc<parking_lot::Mutex<Option<String>>>,
    /// Redis key used for the leader election lock (includes configured prefix)
    lock_key: String,
    /// Monotonically increasing epoch (fencing token) incremented on each
    /// leadership acquisition. Used for split-brain protection.
    leader_epoch: Arc<AtomicU64>,
    /// Timestamp (Instant) at which leadership was lost. Used to enforce a
    /// grace period before re-acquisition attempts.
    leadership_lost_at: Arc<parking_lot::Mutex<Option<tokio::time::Instant>>>,
    /// Broadcast channel for leadership change events (observer pattern)
    event_tx: Arc<broadcast::Sender<LeadershipEvent>>,
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
            lock_value: Arc::new(parking_lot::Mutex::new(None)),
            lock_key: format!("{}{}", key_prefix, DEFAULT_LEADER_LOCK_KEY),
            leader_epoch: Arc::new(AtomicU64::new(0)),
            leadership_lost_at: Arc::new(parking_lot::Mutex::new(None)),
            event_tx: Arc::new(event_tx),
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

    /// Create a fencing guard that is automatically cancelled when leadership is lost.
    ///
    /// Returns a `CancellationToken` that singleton tasks should use as their
    /// cancellation signal. When this node loses leadership (receives a `Lost`
    /// event), the token is cancelled, causing the singleton task to stop.
    ///
    /// This prevents split-brain scenarios where a demoted leader continues
    /// running singleton tasks that should only execute on the current leader.
    #[must_use]
    pub fn leader_guard(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let mut rx = self.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(LeadershipEvent::Lost) => {
                        token_clone.cancel();
                        break;
                    }
                    Ok(LeadershipEvent::Gained { .. }) => {
                        // Still leader or re-elected, continue watching
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        token_clone.cancel();
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        token_clone.cancel();
                        break;
                    }
                }
            }
        });
        token
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
        let interval = Duration::from_secs(self.renew_interval_secs);

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
                () = tokio::time::sleep(interval) => {
                    self.try_acquire_or_renew().await;
                }
            }
        }
    }

    /// Try to acquire the lock or renew it if we already hold it.
    async fn try_acquire_or_renew(&self) {
        let current_value = self.lock_value.lock().clone();

        if let Some(ref value) = current_value {
            // We think we're the leader; try to extend the lock.
            match self.lock.extend(&self.lock_key, value, self.lease_duration_secs).await {
                Ok(true) => {
                    debug!(identity = %self.identity, "Leader lease renewed");
                    // Still the leader
                }
                Ok(false) => {
                    // Lock expired or was taken by someone else
                    warn!(identity = %self.identity, "Leader lease renewal failed (lock lost)");
                    self.lose_leadership();
                }
                Err(e) => {
                    warn!(
                        identity = %self.identity,
                        error = %e,
                        "Leader lease renewal failed (Redis error)"
                    );
                    // Immediately assume we lost leadership on error,
                    // because we can't confirm the lock still exists.
                    self.lose_leadership();
                }
            }
        } else {
            // Not currently the leader; check grace period before attempting re-acquire.
            if self.in_grace_period() {
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
    fn lose_leadership(&self) {
        self.set_leader(false, None);
        *self.lock_value.lock() = None;
        *self.leadership_lost_at.lock() = Some(tokio::time::Instant::now());
    }

    /// Returns `true` if we recently lost leadership and should wait before
    /// attempting to re-acquire. The grace period equals `renew_interval_secs`
    /// to avoid rapid flip-flopping during transient Redis issues.
    fn in_grace_period(&self) -> bool {
        let guard = self.leadership_lost_at.lock();
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
                *self.lock_value.lock() = Some(value);
                // Clear grace period since we successfully acquired
                *self.leadership_lost_at.lock() = None;
                self.set_leader(true, Some(epoch));
            }
            Ok(None) => {
                debug!(identity = %self.identity, "Another node is leader");
                self.set_leader(false, None);
            }
            Err(e) => {
                warn!(
                    identity = %self.identity,
                    error = %e,
                    "Failed to acquire leader lock"
                );
                self.set_leader(false, None);
            }
        }
    }

    /// Gracefully resign leadership by releasing the lock.
    async fn resign(&self) {
        let value = self.lock_value.lock().take();
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
    /// Notifies observers of leadership changes.
    fn set_leader(&self, leader: bool, gained_epoch: Option<u64>) {
        let was_leader = self.is_leader.swap(leader, Ordering::AcqRel);
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
}
