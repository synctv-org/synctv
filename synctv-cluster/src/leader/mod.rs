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
//! let is_sentinel = matches!(redis_deployment_mode, RedisDeploymentMode::Sentinel);
//! let elector = LeaderElector::new(redis_conn, node_id, "synctv:", is_sentinel);
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
            Self::Redis(e) => e.is_leader(),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.is_leader(),
        }
    }

    /// Returns the identity of the current leader if this node is the leader.
    ///
    /// Returns `Some(identity)` if this node currently holds leadership,
    /// `None` otherwise. For querying the identity of a remote leader,
    /// check the distributed lock directly.
    pub fn current_leader_identity(&self) -> Option<String> {
        match self {
            Self::Redis(e) => e.current_leader_identity(),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => {
                if e.is_leader() {
                    Some(e.identity().to_string())
                } else {
                    None
                }
            }
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
            Self::Redis(e) => e.leader_epoch(),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.leader_epoch(),
        }
    }

    /// Subscribe to leadership change events (observer pattern).
    ///
    /// Returns a receiver that will receive `LeadershipEvent::Gained` when this
    /// node becomes leader and `LeadershipEvent::Lost` when it loses leadership.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        match self {
            Self::Redis(e) => e.subscribe(),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.subscribe(),
        }
    }

    /// Start the leader election loop.
    ///
    /// Spawns a background task that continuously tries to acquire or
    /// renew leadership. The task runs until the `cancel_token` is cancelled.
    pub fn start(&self, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
        match self {
            Self::Redis(e) => e.start(cancel_token),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.start(cancel_token),
        }
    }

    /// Gracefully resign leadership by releasing the distributed lock.
    ///
    /// This method is called when:
    /// - The node enters quarantine due to epoch mismatch (split-brain detection)
    /// - The application is shutting down
    /// - Leadership needs to be voluntarily relinquished
    ///
    /// # Behavior
    ///
    /// - If this node is not the leader, this is a no-op
    /// - If this node is the leader, releases the distributed lock (Redis or K8s Lease)
    /// - Sends a `LeadershipEvent::Lost` to all subscribers
    /// - Updates metrics to reflect leadership loss
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example(
    /// #     elector: synctv_cluster::leader::AnyLeaderElector,
    /// #     is_quarantined: bool,
    /// # ) {
    /// // When entering quarantine due to epoch mismatch
    /// if is_quarantined && elector.is_leader() {
    ///     elector.resign().await;
    /// }
    /// # }
    /// ```
    pub async fn resign(&self) {
        match self {
            Self::Redis(e) => e.resign().await,
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.resign_public().await,
        }
    }
}

impl LeaderElect for AnyLeaderElector {
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        match self {
            Self::Redis(e) => e.event_tx.subscribe(),
            #[cfg(feature = "k8s")]
            Self::K8s(e) => e.subscribe(),
        }
    }
}

impl synctv_core::service::LeaderCheck for AnyLeaderElector {
    fn is_leader(&self) -> bool {
        self.is_leader()
    }
}

impl LeaderElect for synctv_core::service::AlwaysLeader {
    fn subscribe(&self) -> broadcast::Receiver<LeadershipEvent> {
        static ALWAYS_LEADER_EVENT_TX: std::sync::OnceLock<broadcast::Sender<LeadershipEvent>> =
            std::sync::OnceLock::new();

        ALWAYS_LEADER_EVENT_TX
            .get_or_init(|| {
                let (tx, _rx) = broadcast::channel(1);
                tx
            })
            .subscribe()
    }
}

#[async_trait]
impl LeaderRuntime for synctv_core::service::AlwaysLeader {
    fn current_leader_identity(&self) -> Option<String> {
        Some("standalone".to_string())
    }

    fn leader_epoch(&self) -> u64 {
        0
    }

    async fn resign(&self) {}
}

#[async_trait]
impl LeaderRuntime for AnyLeaderElector {
    fn current_leader_identity(&self) -> Option<String> {
        Self::current_leader_identity(self)
    }

    fn leader_epoch(&self) -> u64 {
        Self::leader_epoch(self)
    }

    async fn resign(&self) {
        Self::resign(self).await;
    }
}

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
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
#[async_trait]
pub trait LeaderRuntime: LeaderElect + synctv_core::service::LeaderCheck + Send + Sync {
    /// Returns the identity of this node if it currently holds leadership.
    fn current_leader_identity(&self) -> Option<String>;

    /// Returns the current leader epoch (fencing token).
    fn leader_epoch(&self) -> u64;

    /// Gracefully resign leadership.
    async fn resign(&self);
}

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
    ///
    /// # Synchronization Guarantee
    ///
    /// The returned `broadcast::Receiver` is already subscribed before this
    /// method spawns the listener task, so `Lost`/`Vacancy` events sent after
    /// `subscribe()` cannot be missed even if the task has not yet polled
    /// `recv()`. Avoiding any blocking handshake here keeps this method safe
    /// on both multi-threaded and current-thread Tokio runtimes.
    #[must_use]
    fn leader_guard(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let mut rx = self.subscribe();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(LeadershipEvent::Lost | LeadershipEvent::Vacancy) => {
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

/// Startup inputs for constructing the configured leader election backend.
pub struct LeaderRuntimeBuilder<'a> {
    pub cluster_enabled: bool,
    pub leader_mode: &'a str,
    pub node_id: &'a str,
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    pub redis_key_prefix: &'a str,
    pub redis_is_sentinel: bool,
}

impl<'a> LeaderRuntimeBuilder<'a> {
    #[must_use]
    pub const fn new(
        cluster_enabled: bool,
        leader_mode: &'a str,
        node_id: &'a str,
        redis_conn: Option<redis::aio::ConnectionManager>,
        redis_key_prefix: &'a str,
        redis_is_sentinel: bool,
    ) -> Self {
        Self {
            cluster_enabled,
            leader_mode,
            node_id,
            redis_conn,
            redis_key_prefix,
            redis_is_sentinel,
        }
    }

    pub async fn build(self) -> anyhow::Result<AnyLeaderElector> {
        if !self.cluster_enabled {
            return Err(anyhow::anyhow!(
                "LeaderRuntimeBuilder is cluster-only; standalone mode must use AlwaysLeader directly"
            ));
        }

        match self.leader_mode {
            #[cfg(feature = "k8s")]
            "k8s_lease" => {
                let pod_name = std::env::var("POD_NAME").map_err(|_| {
                    anyhow::anyhow!(
                        "cluster.leader_election_mode='k8s_lease' requires POD_NAME; \
                         this should have been caught by configuration validation"
                    )
                })?;
                let namespace = std::env::var("POD_NAMESPACE").map_err(|_| {
                    anyhow::anyhow!(
                        "cluster.leader_election_mode='k8s_lease' requires POD_NAMESPACE; \
                         this should have been caught by configuration validation"
                    )
                })?;

                let elector = K8sLeaderElector::new(
                    pod_name,
                    namespace,
                    K8sLeaderElectorConfig::default(),
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "K8s leader election initialization failed: {e}. \
                         Cannot safely continue in cluster mode. \
                         Either fix K8s RBAC/env vars or set cluster.leader_election_mode='redis'"
                    )
                })?;

                Ok(AnyLeaderElector::K8s(elector))
            }
            #[cfg(not(feature = "k8s"))]
            "k8s_lease" => Err(anyhow::anyhow!(
                "K8s leader election mode 'k8s_lease' requires the 'k8s' feature. \
                 Rebuild with: cargo build --features k8s, or set cluster.leader_election_mode='redis'"
            )),
            "redis" => {
                if self.redis_is_sentinel {
                    return Err(anyhow::anyhow!(
                        "cluster.leader_election_mode='redis' is not supported with Redis Sentinel. \
                         Sentinel failover can create split-brain leader windows; use \
                         cluster.leader_election_mode='k8s_lease' or a non-Sentinel Redis deployment."
                    ));
                }
                let redis_conn = self.redis_conn.ok_or_else(|| {
                    anyhow::anyhow!(
                        "cluster.enabled=true requires Redis-backed leader election wiring"
                    )
                })?;
                Ok(AnyLeaderElector::Redis(LeaderElector::new(
                    redis_conn,
                    self.node_id.to_string(),
                    self.redis_key_prefix,
                    self.redis_is_sentinel,
                )))
            }
            other => Err(anyhow::anyhow!(
                "cluster.leader_election_mode is validated before startup: {other}"
            )),
        }
    }
}

/// Default Redis key for leader election lock (used when no prefix is configured).
const DEFAULT_LEADER_LOCK_KEY: &str = "leader_election";

/// Number of consecutive election failures before declaring a leader vacancy.
/// At the default renew interval of 10s, 3 failures = ~30s of no leader.
const LEADER_VACANCY_THRESHOLD: u64 = 3;

/// Multiplier applied to grace period after prolonged Redis outage.
/// When Redis recovers after being unavailable for an extended period,
/// the grace period is multiplied by this factor to prevent multiple nodes
/// from simultaneously attempting to acquire leadership.
const OUTAGE_RECOVERY_GRACE_MULTIPLIER: u64 = 3;

/// Threshold (in number of consecutive failures) at which we consider
/// Redis to be in a prolonged outage state. Once failures exceed this
/// threshold and Redis recovers, we apply an extended grace period.
const PROLONGED_OUTAGE_FAILURE_THRESHOLD: u64 = 6;

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
/// ## Clock skew handling
///
/// Grace period calculations use Redis TIME (server-side timestamps) instead of
/// local wall-clock time to prevent split-brain during NTP clock skew or VM
/// clock drift. This ensures that multiple nodes cannot simultaneously exit the
/// grace period and claim leadership.
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
    /// Redis timestamp (seconds since Unix epoch) at which leadership was lost.
    /// Used to enforce a grace period before re-acquisition attempts.
    /// Uses Redis TIME instead of local clock to prevent clock skew issues.
    leadership_lost_at_redis_ts: Arc<TokioMutex<Option<u64>>>,
    /// Broadcast channel for leadership change events (observer pattern)
    event_tx: Arc<broadcast::Sender<LeadershipEvent>>,
    /// Number of consecutive election failures (acquire or renew).
    /// Used to detect prolonged leader vacancy.
    consecutive_failures: Arc<AtomicU64>,
    /// Redis connection manager for TIME command
    redis_conn: redis::aio::ConnectionManager,
    /// Tracks whether we've experienced a prolonged Redis outage.
    /// When true and Redis recovers, we apply an extended grace period
    /// to prevent multiple nodes from simultaneously claiming leadership.
    in_prolonged_outage: Arc<AtomicBool>,
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
    ///
    /// # Safety Warning: Sentinel Mode
    ///
    /// When `is_sentinel` is true, a startup warning is logged about the
    /// distributed lock vulnerability during Sentinel failover. For production
    /// deployments with Redis Sentinel, consider using K8s Lease-based leader
    /// election instead (requires `k8s` feature).
    pub fn new(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
        key_prefix: &str,
        is_sentinel: bool,
    ) -> Self {
        Self::with_config(
            redis_conn,
            identity,
            LeaderElectorConfig::default(),
            key_prefix,
            is_sentinel,
        )
    }

    /// Create a new leader elector with custom configuration.
    ///
    /// The `key_prefix` is prepended to the leader election lock key in Redis.
    ///
    /// # Safety Warning: Sentinel Mode
    ///
    /// When `is_sentinel` is true, a startup warning is logged about the
    /// distributed lock vulnerability during Sentinel failover. For production
    /// deployments with Redis Sentinel, consider using K8s Lease-based leader
    /// election instead (requires `k8s` feature).
    pub fn with_config(
        redis_conn: redis::aio::ConnectionManager,
        identity: String,
        config: LeaderElectorConfig,
        key_prefix: &str,
        is_sentinel: bool,
    ) -> Self {
        assert!(
            config.renew_interval_secs < config.lease_duration_secs,
            "renew_interval_secs ({}) must be less than lease_duration_secs ({})",
            config.renew_interval_secs,
            config.lease_duration_secs
        );

        let (event_tx, _) = broadcast::channel(16);
        let redis_conn_clone = redis_conn.clone();

        // Use new_with_mode to log warning if using Sentinel
        let lock = DistributedLock::new_with_mode(redis_conn, is_sentinel);

        Self {
            is_leader: Arc::new(AtomicBool::new(false)),
            lock,
            identity,
            lease_duration_secs: config.lease_duration_secs,
            renew_interval_secs: config.renew_interval_secs,
            lock_value: Arc::new(TokioMutex::new(None)),
            lock_key: format!("{key_prefix}{DEFAULT_LEADER_LOCK_KEY}"),
            leader_epoch: Arc::new(AtomicU64::new(0)),
            leadership_lost_at_redis_ts: Arc::new(TokioMutex::new(None)),
            event_tx: Arc::new(event_tx),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
            redis_conn: redis_conn_clone,
            in_prolonged_outage: Arc::new(AtomicBool::new(false)),
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

    /// Get current time from Redis server (seconds since Unix epoch).
    ///
    /// Uses Redis TIME command to get authoritative server-side timestamp,
    /// avoiding clock skew issues when multiple nodes have NTP drift.
    async fn get_redis_time(&self) -> Result<u64, redis::RedisError> {
        let mut conn = self.redis_conn.clone();
        // TIME returns: [seconds, microseconds]
        let time_result: (u64, u64) = redis::cmd("TIME").query_async(&mut conn).await?;
        Ok(time_result.0)
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
            match self
                .lock
                .extend(&self.lock_key, value, self.lease_duration_secs)
                .await
            {
                Ok(true) => {
                    let was_in_outage = self.in_prolonged_outage.swap(false, Ordering::AcqRel);
                    if was_in_outage {
                        info!(
                            identity = %self.identity,
                            "Leader lease renewed after prolonged outage recovery"
                        );
                    } else {
                        debug!(identity = %self.identity, "Leader lease renewed");
                    }
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
                    let is_failover =
                        error_str.contains("READONLY") || error_str.contains("LOADING");

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
                            consecutive_failures = self.consecutive_failures.load(Ordering::Relaxed),
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
                        // Use Redis timestamp to avoid clock skew.
                        if let Ok(redis_ts) = self.get_redis_time().await {
                            // Set the timestamp such that (current - lost_at) = renew_interval - 2
                            // This means: lost_at = current - (renew_interval - 2)
                            let grace_elapsed = self.renew_interval_secs.saturating_sub(2);
                            *self.leadership_lost_at_redis_ts.lock().await =
                                Some(redis_ts.saturating_sub(grace_elapsed));
                        }
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
    /// value, and record the Redis timestamp of loss for grace period enforcement.
    ///
    /// Logs additional context when leadership is lost during a prolonged outage
    /// to aid in debugging split-brain scenarios.
    async fn lose_leadership(&self) {
        let was_leader = self.is_leader.load(Ordering::Acquire);
        let consecutive_failures = self.consecutive_failures.load(Ordering::Relaxed);

        self.set_leader(false, None);
        *self.lock_value.lock().await = None;

        // Get current Redis timestamp to avoid clock skew issues.
        // Fall back to local time if Redis TIME fails (e.g., during Sentinel
        // failover). Using 0 would make saturating_sub(0) skip the grace
        // period entirely once Redis recovers, while local time preserves the
        // intended grace period duration (minor clock skew is acceptable).
        let redis_ts = self.get_redis_time().await.unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });
        *self.leadership_lost_at_redis_ts.lock().await = Some(redis_ts);

        if was_leader {
            debug!(
                identity = %self.identity,
                consecutive_failures = consecutive_failures,
                redis_ts = redis_ts,
                "Leadership lost, entering grace period"
            );
        }
    }

    /// Returns `true` if we recently lost leadership and should wait before
    /// attempting to re-acquire.
    ///
    /// The grace period is determined as follows:
    /// - Normal case: `renew_interval_secs` to avoid rapid flip-flopping during
    ///   transient Redis issues.
    /// - Prolonged outage recovery: `renew_interval_secs * OUTAGE_RECOVERY_GRACE_MULTIPLIER`
    ///   to prevent multiple nodes from simultaneously claiming leadership when
    ///   Redis recovers after being unavailable for an extended period.
    ///
    /// Uses Redis TIME (server-side timestamp) instead of local clock to prevent
    /// multiple nodes from simultaneously exiting the grace period due to NTP skew.
    async fn in_grace_period(&self) -> bool {
        let guard = self.leadership_lost_at_redis_ts.lock().await;
        if let Some(lost_at_ts) = *guard {
            // Determine the applicable grace period
            let in_prolonged_outage = self.in_prolonged_outage.load(Ordering::Acquire);
            let grace_period = if in_prolonged_outage {
                // Extended grace period after prolonged outage
                self.renew_interval_secs * OUTAGE_RECOVERY_GRACE_MULTIPLIER
            } else {
                // Normal grace period
                self.renew_interval_secs
            };

            // Get current Redis time
            match self.get_redis_time().await {
                Ok(current_ts) => {
                    let elapsed = current_ts.saturating_sub(lost_at_ts);
                    let in_grace = elapsed < grace_period;

                    if in_grace && in_prolonged_outage {
                        debug!(
                            identity = %self.identity,
                            elapsed_secs = elapsed,
                            grace_period_secs = grace_period,
                            "In extended grace period after prolonged outage"
                        );
                    }

                    in_grace
                }
                Err(e) => {
                    // If Redis TIME fails, we're still in an outage.
                    // Stay in grace period (conservative: prevent premature acquisition attempts)
                    debug!(
                        identity = %self.identity,
                        error = %e,
                        "Cannot determine grace period (Redis TIME failed), assuming in grace period"
                    );
                    true
                }
            }
        } else {
            false
        }
    }

    /// Try to acquire the leadership lock.
    ///
    /// When recovering from a prolonged Redis outage, applies an extended
    /// grace period to prevent multiple nodes from simultaneously claiming
    /// leadership. The extended grace period is calculated as:
    /// `renew_interval_secs * OUTAGE_RECOVERY_GRACE_MULTIPLIER`.
    ///
    /// Additionally, when this node was in a prolonged outage and successfully
    /// acquires the lock, it increments the leader epoch to ensure the fencing
    /// token advances, invalidating any stale operations from nodes that may
    /// have incorrectly believed they were leader during the outage.
    async fn try_acquire(&self) {
        // Check if we're recovering from a prolonged outage
        let was_in_prolonged_outage = self.in_prolonged_outage.load(Ordering::Acquire);

        match self
            .lock
            .acquire(&self.lock_key, self.lease_duration_secs)
            .await
        {
            Ok(Some(value)) => {
                let epoch = self.leader_epoch.fetch_add(1, Ordering::AcqRel) + 1;

                // Log recovery from prolonged outage
                if was_in_prolonged_outage {
                    info!(
                        identity = %self.identity,
                        epoch = epoch,
                        "Became leader after prolonged Redis outage (recovery)"
                    );
                    // Clear the prolonged outage flag now that we've recovered
                    self.in_prolonged_outage.store(false, Ordering::Release);
                } else {
                    info!(
                        identity = %self.identity,
                        epoch = epoch,
                        "Became leader"
                    );
                }

                *self.lock_value.lock().await = Some(value);
                // Clear grace period since we successfully acquired
                *self.leadership_lost_at_redis_ts.lock().await = None;
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.set_leader(true, Some(epoch));
            }
            Ok(None) => {
                debug!(identity = %self.identity, "Another node is leader");
                // Another node is leader -- not a failure, reset counter
                // Also clear prolonged outage flag since Redis is clearly working
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.in_prolonged_outage.store(false, Ordering::Release);
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
                    // Use Redis timestamp to avoid clock skew.
                    if let Ok(redis_ts) = self.get_redis_time().await {
                        let grace_elapsed = self.renew_interval_secs.saturating_sub(2);
                        *self.leadership_lost_at_redis_ts.lock().await =
                            Some(redis_ts.saturating_sub(grace_elapsed));
                    }
                } else {
                    warn!(
                        identity = %self.identity,
                        error = %e,
                        consecutive_failures = self.consecutive_failures.load(Ordering::Relaxed),
                        in_prolonged_outage = was_in_prolonged_outage,
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
    ///
    /// When failures exceed [`PROLONGED_OUTAGE_FAILURE_THRESHOLD`], marks
    /// this node as being in a prolonged outage state. When Redis recovers
    /// after a prolonged outage, an extended grace period is applied to
    /// prevent multiple nodes from simultaneously claiming leadership.
    fn record_election_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        // Update metrics for monitoring
        synctv_core::metrics::cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES.set(failures as i64);

        // Check for prolonged outage
        if failures == PROLONGED_OUTAGE_FAILURE_THRESHOLD {
            warn!(
                identity = %self.identity,
                consecutive_failures = failures,
                "Prolonged Redis outage detected: applying extended grace period on recovery"
            );
            self.in_prolonged_outage.store(true, Ordering::Release);
        }

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

    /// Returns the number of consecutive election failures.
    /// Useful for health check endpoints.
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Gracefully resign leadership by releasing the lock.
    ///
    /// This method is called automatically when:
    /// - The election loop is cancelled (shutdown)
    /// - The node enters quarantine due to epoch mismatch (split-brain detection)
    ///
    /// It can also be called manually to voluntarily give up leadership.
    ///
    /// # Behavior
    ///
    /// - If this node is not the leader, this is a no-op
    /// - If this node is the leader, releases the distributed lock
    /// - Sends a `LeadershipEvent::Lost` to all subscribers
    /// - Updates metrics to reflect leadership loss
    pub async fn resign(&self) {
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
        synctv_core::metrics::cluster::LEADER_ELECTION_STATE.set(i64::from(leader));
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

    #[tokio::test]
    async fn test_builder_rejects_standalone_mode() {
        let error =
            match LeaderRuntimeBuilder::new(false, "redis", "node-1", None, "synctv:", false)
                .build()
                .await
            {
                Ok(_) => panic!("standalone mode must use AlwaysLeader directly"),
                Err(error) => error,
            };

        assert!(
            error
                .to_string()
                .contains("LeaderRuntimeBuilder is cluster-only"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_builder_rejects_redis_mode_with_sentinel() {
        let error = match LeaderRuntimeBuilder::new(true, "redis", "node-1", None, "synctv:", true)
            .build()
            .await
        {
            Ok(_) => panic!("sentinel-backed redis leader election must fail closed"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("not supported with Redis Sentinel"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_always_leader_implements_unified_runtime_trait() {
        let leader: std::sync::Arc<dyn LeaderRuntime> =
            std::sync::Arc::new(synctv_core::service::AlwaysLeader);

        assert!(leader.is_leader(), "AlwaysLeader should report leader=true");
        assert_eq!(leader.leader_epoch(), 0, "standalone epoch should be zero");
        assert_eq!(
            leader.current_leader_identity().as_deref(),
            Some("standalone"),
            "standalone leader identity should be stable"
        );

        leader.resign().await;
        assert!(
            leader.is_leader(),
            "resign() on AlwaysLeader should remain a no-op in standalone mode"
        );
    }

    #[test]
    fn test_always_leader_leader_check_trait() {
        use synctv_core::service::LeaderCheck;
        let elector = synctv_core::service::AlwaysLeader;
        assert!(LeaderCheck::is_leader(&elector));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_always_leader_guard_stays_active_without_loss_event() {
        let elector = synctv_core::service::AlwaysLeader;
        let guard = elector.leader_guard();

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert!(
            !guard.is_cancelled(),
            "AlwaysLeader guard should remain active without leadership loss"
        );
    }
}
