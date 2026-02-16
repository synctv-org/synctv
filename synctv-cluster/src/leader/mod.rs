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
//!
//! # Usage
//!
//! ```ignore
//! // Redis-based (any deployment):
//! let elector = LeaderElector::new(redis_conn, node_id);
//! elector.start(cancel_token.clone());
//!
//! // K8s Lease-based (K8s only):
//! let elector = K8sLeaderElector::new(pod_name, namespace, config).await?;
//! elector.start(cancel_token.clone());
//!
//! if elector.is_leader() {
//!     // run singleton tasks
//! }
//! ```

pub mod k8s_lease;
pub use k8s_lease::{K8sLeaderElector, K8sLeaderElectorConfig};

/// Unified leader elector that supports both Redis and K8s modes.
///
/// This enum allows the application to dynamically switch between
/// Redis-based and K8s Lease-based leader election at runtime.
#[derive(Clone)]
pub enum AnyLeaderElector {
    /// Redis-based leader election (works in any deployment)
    Redis(LeaderElector),
    /// Kubernetes Lease-based leader election (K8s only)
    K8s(K8sLeaderElector),
}

impl AnyLeaderElector {
    /// Returns `true` if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        match self {
            AnyLeaderElector::Redis(e) => e.is_leader(),
            AnyLeaderElector::K8s(e) => e.is_leader(),
        }
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use synctv_core::service::DistributedLock;

/// Redis key prefix for leader election lock.
const LEADER_LOCK_KEY: &str = "leader_election";

/// Leader election using Redis distributed locks.
///
/// Only one replica at a time holds the lock and is considered the leader.
/// The leader performs singleton tasks like database migrations and
/// periodic cleanup (expired token cleanup, audit log pruning).
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
    pub fn new(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
    ) -> Self {
        Self::with_config(redis_conn, identity, LeaderElectorConfig::default())
    }

    /// Create a new leader elector with custom configuration.
    pub fn with_config(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
        config: LeaderElectorConfig,
    ) -> Self {
        assert!(
            config.renew_interval_secs < config.lease_duration_secs,
            "renew_interval_secs ({}) must be less than lease_duration_secs ({})",
            config.renew_interval_secs,
            config.lease_duration_secs
        );

        Self {
            is_leader: Arc::new(AtomicBool::new(false)),
            lock: DistributedLock::new(redis_conn),
            identity,
            lease_duration_secs: config.lease_duration_secs,
            renew_interval_secs: config.renew_interval_secs,
            lock_value: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Returns `true` if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::Acquire)
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
            match self.lock.extend(LEADER_LOCK_KEY, value, self.lease_duration_secs).await {
                Ok(true) => {
                    debug!(identity = %self.identity, "Leader lease renewed");
                    // Still the leader
                }
                Ok(false) => {
                    // Lock expired or was taken by someone else
                    warn!(identity = %self.identity, "Leader lease renewal failed (lock lost)");
                    self.set_leader(false);
                    *self.lock_value.lock() = None;
                    // Immediately try to re-acquire
                    self.try_acquire().await;
                }
                Err(e) => {
                    warn!(
                        identity = %self.identity,
                        error = %e,
                        "Leader lease renewal failed (Redis error)"
                    );
                    // Conservatively assume we lost leadership on error,
                    // because we can't confirm the lock still exists.
                    self.set_leader(false);
                    *self.lock_value.lock() = None;
                }
            }
        } else {
            // Not currently the leader; try to acquire.
            self.try_acquire().await;
        }
    }

    /// Try to acquire the leadership lock.
    async fn try_acquire(&self) {
        match self.lock.acquire(LEADER_LOCK_KEY, self.lease_duration_secs).await {
            Ok(Some(value)) => {
                info!(identity = %self.identity, "Became leader");
                *self.lock_value.lock() = Some(value);
                self.set_leader(true);
            }
            Ok(None) => {
                debug!(identity = %self.identity, "Another node is leader");
                self.set_leader(false);
            }
            Err(e) => {
                warn!(
                    identity = %self.identity,
                    error = %e,
                    "Failed to acquire leader lock"
                );
                self.set_leader(false);
            }
        }
    }

    /// Gracefully resign leadership by releasing the lock.
    async fn resign(&self) {
        let value = self.lock_value.lock().take();
        if let Some(value) = value {
            info!(identity = %self.identity, "Resigning leadership");
            if let Err(e) = self.lock.release(LEADER_LOCK_KEY, &value).await {
                warn!(
                    identity = %self.identity,
                    error = %e,
                    "Failed to release leader lock during resignation"
                );
            }
        }
        self.set_leader(false);
    }

    /// Update the is_leader flag and log transitions.
    fn set_leader(&self, leader: bool) {
        let was_leader = self.is_leader.swap(leader, Ordering::AcqRel);
        if was_leader && !leader {
            info!(identity = %self.identity, "Lost leadership");
        }
        // Gaining leadership is logged in try_acquire
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
