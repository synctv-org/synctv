//! Node registry for cluster member discovery
//!
//! Uses Redis to track active nodes in the cluster.

use chrono::{DateTime, Utc};
use failsafe::{backoff, failure_policy, Config as CbConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};

/// Staleness threshold in seconds. If `last_refreshed` is older than this,
/// `is_nodes_stale()` returns `true`.
const NODES_STALE_THRESHOLD_SECS: u64 = 30;

/// Timeout for Redis operations in seconds
const REDIS_TIMEOUT_SECS: u64 = 5;

/// TTL for the `get_all_nodes()` moka cache, in seconds.
///
/// This controls the maximum staleness of node discovery queries used by health
/// checks and load balancer. A 2-second window means that a node that registers
/// or deregisters may not appear/disappear from `get_all_nodes()` for up to 2s.
///
/// Trade-offs:
/// - **Lower value** (1-2s): fresher view of the cluster, but more Redis SCAN
///   calls under high query rates (health probes run every ~5-15s, so this is
///   usually negligible).
/// - **Higher value** (5-10s): fewer Redis round-trips, but stale membership
///   data may cause load balancer to route to a recently-departed node or miss
///   a newly-joined node for longer.
///
/// The value of 2s was chosen as a balance: it is well below the heartbeat
/// interval (typically 10s) so membership changes are reflected promptly, while
/// still coalescing bursts of `get_all_nodes()` calls within the same tick.
const NODES_CACHE_TTL_SECS: u64 = 2;

/// Create a failsafe circuit breaker for Redis operations.
///
/// Opens after 3 consecutive failures. Uses exponential backoff starting at
/// 10 seconds up to 60 seconds before allowing probe requests in half-open state.
fn create_redis_circuit_breaker(
) -> failsafe::StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()> {
    let backoff = backoff::exponential(Duration::from_secs(10), Duration::from_secs(60));
    let policy = failure_policy::consecutive_failures(3, backoff);
    CbConfig::new().failure_policy(policy).build()
}

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub grpc_address: String,
    pub http_address: String,
    pub last_heartbeat: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
    /// Fencing token (epoch) for split-brain protection
    /// Increments on each registration to prevent stale updates
    #[serde(default)]
    pub epoch: u64,
}

impl NodeInfo {
    #[must_use]
    pub fn new(node_id: String, grpc_address: String, http_address: String) -> Self {
        Self {
            node_id,
            grpc_address,
            http_address,
            last_heartbeat: Utc::now(),
            metadata: HashMap::new(),
            epoch: 1, // Start at epoch 1
        }
    }

    /// Create with a specific epoch (for re-registration)
    #[must_use]
    pub const fn with_epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }

    /// Check if node is stale (no recent heartbeat)
    #[must_use]
    pub fn is_stale(&self, timeout_secs: i64) -> bool {
        let now = Utc::now();
        let elapsed = now.signed_duration_since(self.last_heartbeat);
        elapsed.num_seconds() > timeout_secs
    }

    /// Get the fencing token for this node
    #[must_use]
    pub fn fencing_token(&self) -> FencingToken {
        FencingToken::new(self.node_id.clone(), self.epoch)
    }
}

/// Fencing token for split-brain protection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FencingToken {
    pub node_id: String,
    pub epoch: u64,
}

impl FencingToken {
    /// Create a new fencing token
    #[must_use]
    pub const fn new(node_id: String, epoch: u64) -> Self {
        Self { node_id, epoch }
    }

    /// Check if this token is newer than another (same node, higher epoch)
    #[must_use]
    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.node_id == other.node_id && self.epoch > other.epoch
    }
}

/// Result of a heartbeat operation, indicating whether re-registration is needed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatResult {
    /// Heartbeat succeeded normally
    Ok,
    /// Key not found in Redis -- node needs to re-register
    NeedReregistration,
    /// Epoch mismatch detected -- the remote epoch is returned
    EpochMismatch(u64),
    /// Cannot re-register because local cache has empty address(es)
    EmptyAddress,
}

/// Cluster operating mode, reflecting current Redis connectivity.
///
/// Transitions:
/// - `Normal` -> `Degraded`: circuit breaker opens (3 consecutive Redis failures)
/// - `Degraded` -> `Normal`: circuit breaker closes (successful Redis operation)
/// - `Degraded` -> `Standalone`: prolonged Redis unavailability (reserved for future use)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterMode {
    /// Redis is reachable; full cluster functionality available.
    Normal,
    /// Redis is unreachable (circuit breaker open); serving from local cache.
    /// Health checks, load balancing, and gRPC fan-out operate on stale data.
    Degraded,
    /// Redis has been unreachable for an extended period; node operates solo.
    /// Reserved for future use (e.g., automatic recovery logic).
    Standalone,
}

impl std::fmt::Display for ClusterMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClusterMode::Normal => write!(f, "Normal"),
            ClusterMode::Degraded => write!(f, "Degraded"),
            ClusterMode::Standalone => write!(f, "Standalone"),
        }
    }
}

/// Type alias for our failsafe circuit breaker
type RedisCircuitBreaker =
    failsafe::StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()>;

/// Initial backoff duration for re-registration after heartbeat failure
const INITIAL_REREGISTER_BACKOFF_SECS: u64 = 1;
/// Maximum backoff duration for re-registration
const MAX_REREGISTER_BACKOFF_SECS: u64 = 60;
/// Backoff multiplier for exponential growth
const REREGISTER_BACKOFF_MULTIPLIER: u64 = 2;

/// Redis-based node registry
///
/// Tracks active nodes in the cluster using Redis key expiration.
/// Uses epoch-based fencing tokens to prevent split-brain scenarios.
///
/// For non-cluster deployments, use [`new_local_only`] to create a registry
/// that operates without Redis, using only local in-memory node discovery.
pub struct NodeRegistry {
    /// Redis client (None in local-only mode)
    redis_client: Option<redis::Client>,
    /// Cached multiplexed connection, reused across operations
    cached_conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
    /// Timestamp of last successful connection health check (Unix seconds)
    last_health_check: AtomicU64,
    node_id: String,
    pub heartbeat_timeout_secs: i64,
    pub(crate) local_nodes: Arc<RwLock<HashMap<String, NodeInfo>>>,
    /// Current epoch for this node (incremented on each registration)
    current_epoch: Arc<AtomicU64>,
    /// Circuit breaker for Redis operations (failsafe crate)
    circuit_breaker: Option<RedisCircuitBreaker>,
    /// Short-lived cache for `get_all_nodes()`. See [`NODES_CACHE_TTL_SECS`] for
    /// staleness trade-off documentation.
    nodes_cache: moka::future::Cache<(), Vec<NodeInfo>>,
    /// Redis key prefix for cluster node keys (e.g. "synctv:cluster:nodes")
    key_prefix: String,
    /// Guard to ensure only one health probe task runs at a time.
    /// Set to `true` when a probe is spawned, reset to `false` when it exits.
    health_probe_running: Arc<AtomicBool>,
    /// Current cluster operating mode (Normal/Degraded/Standalone).
    cluster_mode: Arc<parking_lot::RwLock<ClusterMode>>,
    /// Unix timestamp (seconds) of the last successful `get_all_nodes()` refresh
    /// from Redis. Used by callers to detect stale local cache data.
    last_refreshed: Arc<AtomicU64>,
    /// Cancellation token for graceful shutdown of background tasks (health probe).
    cancel_token: CancellationToken,
    /// Timestamp of last re-registration attempt (Unix milliseconds)
    last_reregister_attempt: AtomicU64,
    /// Current backoff duration for re-registration (milliseconds)
    reregister_backoff_ms: AtomicU64,
    /// Whether we're in local-only mode (no Redis)
    local_only: bool,
}

impl NodeRegistry {
    /// Create a new node registry backed by Redis.
    ///
    /// Redis is required for all cluster coordination. If the Redis URL is
    /// invalid, returns an error immediately. The caller (typically `main.rs`)
    /// should abort startup if this fails.
    ///
    /// The `key_prefix` is prepended to cluster node keys in Redis (e.g. `"synctv:"` produces
    /// keys like `synctv:cluster:nodes:<node_id>`). Pass an empty string to use unprefixed keys.
    pub fn new(
        redis_client: redis::Client,
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Self> {
        let nodes_cache = moka::future::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(NODES_CACHE_TTL_SECS))
            .max_capacity(1)
            .build();

        Ok(Self {
            redis_client: Some(redis_client),
            cached_conn: tokio::sync::Mutex::new(None),
            last_health_check: AtomicU64::new(0),
            node_id,
            heartbeat_timeout_secs,
            local_nodes: Arc::new(RwLock::new(HashMap::new())),
            current_epoch: Arc::new(AtomicU64::new(1)),
            circuit_breaker: Some(create_redis_circuit_breaker()),
            nodes_cache,
            key_prefix: format!("{}cluster:nodes", key_prefix),
            health_probe_running: Arc::new(AtomicBool::new(false)),
            cluster_mode: Arc::new(parking_lot::RwLock::new(ClusterMode::Normal)),
            last_refreshed: Arc::new(AtomicU64::new(0)),
            cancel_token: CancellationToken::new(),
            last_reregister_attempt: AtomicU64::new(0),
            reregister_backoff_ms: AtomicU64::new(INITIAL_REREGISTER_BACKOFF_SECS * 1000),
            local_only: false,
        })
    }

    /// Create a new node registry in local-only mode without Redis.
    ///
    /// This is useful for non-cluster deployments where Redis is not available
    /// or not needed. In local-only mode:
    /// - Node discovery operates purely from local in-memory cache
    /// - The registry starts in `ClusterMode::Standalone` mode
    /// - Operations that require Redis (register, heartbeat to Redis, etc.) will
    ///   work with local cache only
    /// - Use `merge_dns_peers` or `test_insert_local` to populate the local cache
    ///
    /// This supports the architecture where "non-cluster mode can work without Redis".
    pub fn new_local_only(
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Self> {
        let nodes_cache = moka::future::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(NODES_CACHE_TTL_SECS))
            .max_capacity(1)
            .build();

        Ok(Self {
            redis_client: None,
            cached_conn: tokio::sync::Mutex::new(None),
            last_health_check: AtomicU64::new(0),
            node_id,
            heartbeat_timeout_secs,
            local_nodes: Arc::new(RwLock::new(HashMap::new())),
            current_epoch: Arc::new(AtomicU64::new(1)),
            circuit_breaker: None,
            nodes_cache,
            key_prefix: format!("{}cluster:nodes", key_prefix),
            health_probe_running: Arc::new(AtomicBool::new(false)),
            cluster_mode: Arc::new(parking_lot::RwLock::new(ClusterMode::Standalone)),
            last_refreshed: Arc::new(AtomicU64::new(0)),
            cancel_token: CancellationToken::new(),
            last_reregister_attempt: AtomicU64::new(0),
            reregister_backoff_ms: AtomicU64::new(INITIAL_REREGISTER_BACKOFF_SECS * 1000),
            local_only: true,
        })
    }

    /// Get or create a cached multiplexed Redis connection with periodic health checks.
    ///
    /// `MultiplexedConnection` handles concurrent requests internally and
    /// reconnects automatically, so we reuse a single instance.
    /// Every 30 seconds, we PING the connection to detect stale connections early.
    ///
    /// Returns an error in local-only mode (no Redis client configured).
    async fn get_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        let client = match &self.redis_client {
            Some(c) => c,
            None => {
                return Err(Error::Database(
                    "Redis not configured (local-only mode)".to_string(),
                ));
            }
        };
        const HEALTH_CHECK_INTERVAL_SECS: u64 = 30;

        let mut guard = self.cached_conn.lock().await;

        // Check if we need to verify connection health
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_check = self.last_health_check.load(Ordering::Relaxed);
        let needs_health_check = now.saturating_sub(last_check) >= HEALTH_CHECK_INTERVAL_SECS;

        if let Some(ref conn) = *guard {
            // If we have a cached connection and don't need health check, return it
            if !needs_health_check {
                return Ok(conn.clone());
            }

            // Perform health check with PING command
            let mut conn_clone = conn.clone();
            drop(guard); // Release lock during PING to avoid blocking others

            let ping_result = timeout(
                Duration::from_secs(2),
                redis::cmd("PING").query_async::<String>(&mut conn_clone),
            )
            .await;

            guard = self.cached_conn.lock().await; // Re-acquire lock

            match ping_result {
                Ok(Ok(_)) => {
                    // PING succeeded, update health check timestamp
                    self.last_health_check.store(now, Ordering::Relaxed);
                    // Connection might have been replaced while we released the lock
                    if let Some(ref current_conn) = *guard {
                        return Ok(current_conn.clone());
                    }
                    // Connection was cleared, fall through to create new one
                }
                Ok(Err(ref e)) => {
                    // PING failed, clear cache and create new connection
                    tracing::debug!(
                        "Redis connection health check PING failed: {}, reconnecting",
                        e
                    );
                    *guard = None;
                }
                Err(_) => {
                    // PING timeout, clear cache and create new connection
                    tracing::debug!("Redis connection health check PING timeout, reconnecting");
                    *guard = None;
                }
            }
        }

        // Create new connection
        let conn = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| Error::Timeout("Redis connection timed out".to_string()))?
        .map_err(|e| Error::Database(format!("Redis connection failed: {e}")))?;
        *guard = Some(conn.clone());
        self.last_health_check.store(now, Ordering::Relaxed);
        Ok(conn)
    }

    /// Check the circuit breaker and get a Redis connection.
    /// Returns `Err` if the circuit breaker is open or in local-only mode.
    /// Records connection failure in the circuit breaker, but does NOT record
    /// success -- callers must call `record_operation_result()` after the full
    /// operation (connection + command) completes.
    ///
    /// **Background health probe**: When the circuit breaker is open (after 3
    /// consecutive failures), starts a background task that periodically probes
    /// Redis with PING commands. If a PING succeeds, the circuit is transitioned
    /// to half-open (allowing the next operation to attempt). The probe task
    /// stops when the circuit closes or when the NodeRegistry is dropped.
    async fn get_conn_with_breaker(&self) -> Result<redis::aio::MultiplexedConnection> {
        // In local-only mode, return error
        if self.local_only {
            return Err(Error::Database(
                "Redis not configured (local-only mode)".to_string(),
            ));
        }

        let circuit_breaker = match &self.circuit_breaker {
            Some(cb) => cb,
            None => {
                return Err(Error::Database(
                    "Circuit breaker not configured (local-only mode)".to_string(),
                ));
            }
        };

        if !circuit_breaker.is_call_permitted() {
            // Circuit is open - switch to Degraded mode and spawn background health probe
            {
                let mut mode = self.cluster_mode.write();
                if *mode == ClusterMode::Normal {
                    tracing::warn!("Circuit breaker open, switching to Degraded cluster mode");
                    *mode = ClusterMode::Degraded;
                }
            }
            if let Some(client) = self.redis_client.clone() {
                self.maybe_start_health_probe(client);
            }
            return Err(Error::Database(
                "Redis circuit breaker is open, request rejected".to_string(),
            ));
        }
        let result = self.get_conn().await;
        if result.is_err() {
            *self.cached_conn.lock().await = None;
            if let Some(ref cb) = self.circuit_breaker {
                cb.on_error();
            }
        }
        result
    }

    /// Start a background health probe task (if not already running) when the
    /// circuit breaker opens. The task PINGs Redis every 5 seconds. On success,
    /// the circuit transitions to half-open, allowing the next operation to try.
    ///
    /// The probe task automatically stops when the circuit closes, the
    /// `CancellationToken` is cancelled, or the NodeRegistry is dropped.
    fn maybe_start_health_probe(&self, client: redis::Client) {
        let circuit_breaker = match &self.circuit_breaker {
            Some(cb) => cb,
            None => return, // No circuit breaker in local-only mode
        };

        // Check if circuit is open before spawning
        if circuit_breaker.is_call_permitted() {
            return; // Circuit is not open, no need for probe
        }

        // Atomically check-and-set the probe guard.
        // compare_exchange ensures only one task is spawned even under concurrent calls.
        if self
            .health_probe_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // Another probe task is already running
        }

        // Spawn a detached health probe task.
        // The guard is reset to `false` when the task exits (success, circuit close, or drop).
        let breaker = circuit_breaker.clone();
        let probe_guard = self.health_probe_running.clone();
        let cancel = self.cancel_token.clone();
        tokio::spawn(async move {
            // Ensure the guard is reset when the task exits, regardless of path.
            struct ProbeGuard(Arc<AtomicBool>);
            impl Drop for ProbeGuard {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _guard = ProbeGuard(probe_guard);

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                // Use tokio::select! to check cancellation alongside the interval tick
                tokio::select! {
                    () = cancel.cancelled() => {
                        tracing::debug!("Circuit breaker health probe stopping (cancelled)");
                        break;
                    }
                    _ = interval.tick() => {}
                }

                // Stop probing if circuit is no longer open
                if breaker.is_call_permitted() {
                    tracing::debug!("Circuit breaker health probe stopping (circuit closed)");
                    break;
                }

                // Attempt to connect and PING
                let probe_result =
                    tokio::time::timeout(tokio::time::Duration::from_secs(3), async {
                        let mut conn = client.get_multiplexed_async_connection().await?;
                        redis::cmd("PING").query_async::<String>(&mut conn).await
                    })
                    .await;

                match probe_result {
                    Ok(Ok(_)) => {
                        tracing::info!(
                            "Circuit breaker health probe succeeded, transitioning to half-open"
                        );
                        breaker.on_success();
                        break; // Allow next operation to attempt
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "Circuit breaker health probe failed (Redis error)");
                    }
                    Err(_) => {
                        tracing::debug!("Circuit breaker health probe failed (timeout)");
                    }
                }
            }
        });
    }

    /// Record the result of a complete Redis operation (connection + command).
    /// Also detects Sentinel failover errors and clears the cached connection.
    fn record_operation_result<T: std::fmt::Debug>(
        &self,
        result: &std::result::Result<T, impl std::fmt::Display>,
    ) {
        // Skip circuit breaker operations in local-only mode
        if self.local_only {
            return;
        }

        match result {
            Ok(_) => {
                if let Some(ref cb) = self.circuit_breaker {
                    cb.on_success();
                }
                // Transition back to Normal mode on successful operation
                let mut mode = self.cluster_mode.write();
                if *mode != ClusterMode::Normal {
                    tracing::info!(
                        previous_mode = %*mode,
                        "Redis operation succeeded, switching back to Normal cluster mode"
                    );
                    *mode = ClusterMode::Normal;
                }
            }
            Err(ref error) => {
                let error_str = error.to_string();
                if error_str.contains("READONLY") || error_str.contains("LOADING") {
                    tracing::warn!(
                        error = %error_str,
                        "Redis Sentinel failover detected in operation result, will reconnect"
                    );
                    // Clear cached connection synchronously (best effort via try_lock)
                    if let Ok(mut guard) = self.cached_conn.try_lock() {
                        *guard = None;
                    }
                    self.last_health_check.store(0, Ordering::Relaxed);
                }
                if let Some(ref cb) = self.circuit_breaker {
                    cb.on_error();
                }
            }
        }
    }

    /// Get the current fencing token for this node
    #[must_use]
    pub fn current_fencing_token(&self) -> FencingToken {
        FencingToken::new(
            self.node_id.clone(),
            self.current_epoch.load(Ordering::SeqCst),
        )
    }

    /// Register this node in the registry with epoch-based fencing
    ///
    /// This operation is atomic - it uses a Lua script to atomically:
    /// 1. Read existing epoch
    /// 2. Increment epoch
    /// 3. Write new registration with TTL
    ///
    /// This prevents race conditions when multiple instances register concurrently.
    ///
    /// In local-only mode, this only updates the local cache without Redis.
    pub async fn register(&self, grpc_address: String, http_address: String) -> Result<()> {
        // In local-only mode, just update local cache
        if self.local_only {
            let local_epoch = self.current_epoch.load(Ordering::SeqCst);
            let new_epoch = local_epoch + 1;
            self.current_epoch.store(new_epoch, Ordering::SeqCst);

            let mut node_info = NodeInfo::new(self.node_id.clone(), grpc_address, http_address);
            node_info.epoch = new_epoch;
            node_info.last_heartbeat = Utc::now();
            node_info
                .metadata
                .insert("local_epoch".to_string(), local_epoch.to_string());
            node_info.metadata.insert(
                "registered_at".to_string(),
                chrono::Utc::now().timestamp().to_string(),
            );

            let mut nodes = self.local_nodes.write().await;
            nodes.insert(self.node_id.clone(), node_info);

            // Reset backoff on successful registration
            self.reset_reregister_backoff();

            tracing::debug!(
                node_id = %self.node_id,
                epoch = new_epoch,
                "Node registered in local-only mode"
            );

            return Ok(());
        }

        let mut conn = self.get_conn_with_breaker().await?;

        let key = self.node_key(&self.node_id);
        let local_epoch = self.current_epoch.load(Ordering::SeqCst);
        let ttl = self.heartbeat_timeout_secs * 2;

        // Create node info template
        let mut node_info = NodeInfo::new(self.node_id.clone(), grpc_address, http_address);
        node_info
            .metadata
            .insert("local_epoch".to_string(), local_epoch.to_string());
        // Record registration timestamp for load balancer warmup logic
        node_info.metadata.insert(
            "registered_at".to_string(),
            chrono::Utc::now().timestamp().to_string(),
        );
        let node_json = serde_json::to_string(&node_info)
            .map_err(|e| Error::Serialization(format!("Failed to serialize node info: {e}")))?;

        // Atomic Lua script: read epoch, increment, write with TTL
        // Returns the new epoch assigned
        let script = redis::Script::new(
            r"
            local key = KEYS[1]
            local new_node_json = ARGV[1]
            local ttl = tonumber(ARGV[2])
            local local_epoch = tonumber(ARGV[3])
            local node_id = ARGV[4]

            -- Parse incoming node info
            local new_node = cjson.decode(new_node_json)

            -- Read existing value
            local existing = redis.call('GET', key)
            local existing_epoch = 0

            if existing then
                local existing_info = cjson.decode(existing)
                -- Only use existing epoch if it's the same node
                if existing_info.node_id == node_id then
                    existing_epoch = existing_info.epoch or 0
                end
            end

            -- Calculate new epoch: max(existing + 1, local_epoch + 1, 1)
            local new_epoch = math.max(existing_epoch + 1, local_epoch + 1, 1)

            -- Update node info with new epoch and current timestamp
            new_node['epoch'] = new_epoch
            new_node['last_heartbeat'] = ARGV[5]

            -- Write with TTL
            local final_json = cjson.encode(new_node)
            redis.call('SETEX', key, ttl, final_json)

            return new_epoch
            ",
        );

        let now_rfc3339 = Utc::now().to_rfc3339();
        let op_result: std::result::Result<u64, Error> = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            script
                .key(&key)
                .arg(&node_json)
                .arg(ttl)
                .arg(local_epoch)
                .arg(&self.node_id)
                .arg(&now_rfc3339)
                .invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis register script timed out".to_string()))
        .and_then(|r| r.map_err(|e| Error::Database(format!("Redis register script failed: {e}"))));
        self.record_operation_result(&op_result);
        let new_epoch = op_result?;

        // Update local epoch
        self.current_epoch.store(new_epoch, Ordering::SeqCst);

        // Update local cache
        node_info.epoch = new_epoch;
        node_info.last_heartbeat = Utc::now();
        let mut nodes = self.local_nodes.write().await;
        nodes.insert(self.node_id.clone(), node_info);

        // Reset backoff on successful registration
        self.reset_reregister_backoff();

        tracing::debug!(
            node_id = %self.node_id,
            epoch = new_epoch,
            "Node registered with fencing token (atomic)"
        );

        Ok(())
    }

    /// Send heartbeat to keep this node alive with fencing token validation
    ///
    /// Uses an atomic Lua script to check epoch == `expected_epoch` before writing,
    /// preventing stale heartbeats from overwriting newer registrations.
    ///
    /// Returns `HeartbeatResult` indicating whether re-registration is needed.
    ///
    /// **Auto-retry on failure**: If `NeedReregistration` or `EpochMismatch` is returned,
    /// automatically attempts re-registration once (to recover from transient Redis issues
    /// or key expiry). Subsequent heartbeat calls will detect if the auto-registration
    /// succeeded.
    ///
    /// **Backoff**: When re-registration fails, an exponential backoff is applied to
    /// prevent hammering Redis during outages. The backoff starts at 1s and doubles
    /// with each consecutive failure, up to a maximum of 60s. A successful heartbeat
    /// or registration resets the backoff.
    pub async fn heartbeat(&self) -> Result<HeartbeatResult> {
        // In local-only mode, just update local cache and return success
        if self.local_only {
            let mut nodes = self.local_nodes.write().await;
            if let Some(node) = nodes.get_mut(&self.node_id) {
                node.last_heartbeat = Utc::now();
            }
            return Ok(HeartbeatResult::Ok);
        }

        {
            let mut conn = self.get_conn_with_breaker().await?;

            let key = self.node_key(&self.node_id);
            let current_epoch = self.current_epoch.load(Ordering::SeqCst);
            let now = Utc::now();
            let now_rfc3339 = now.to_rfc3339();
            let ttl = self.heartbeat_timeout_secs * 2;

            // Build updated node info from local cache
            let (node_json, grpc_addr, http_addr) = {
                let nodes = self.local_nodes.read().await;
                let info_opt = nodes.get(&self.node_id).cloned();
                drop(nodes);

                let mut info = match info_opt {
                    Some(existing) if !existing.grpc_address.is_empty() => existing,
                    _ => {
                        // Local cache is missing or has empty addresses (should not happen
                        // after a successful register()). Log a warning so operators know
                        // the heartbeat is running with degraded data.
                        tracing::warn!(
                            node_id = %self.node_id,
                            "Heartbeat: local node cache missing or has empty addresses, \
                             auto-re-registration may use empty addresses"
                        );
                        info_opt.unwrap_or_else(|| {
                            NodeInfo::new(self.node_id.clone(), String::new(), String::new())
                        })
                    }
                };
                let grpc = info.grpc_address.clone();
                let http = info.http_address.clone();
                info.last_heartbeat = now;
                info.epoch = current_epoch;
                let json = serde_json::to_string(&info).map_err(|e| {
                    Error::Serialization(format!("Failed to serialize node info: {e}"))
                })?;
                (json, grpc, http)
            };

            // Atomic Lua script: check epoch matches before writing heartbeat
            // Returns:
            //   -1 if key doesn't exist (need re-registration)
            //   -(1000 + remote_epoch) if epoch mismatch (encodes remote epoch)
            //   current_epoch on success
            //
            // We use -(1000 + remote_epoch) instead of -remote_epoch to avoid
            // ambiguity when remote_epoch == 0 (which would return 0, colliding
            // with a successful epoch-0 result).
            let script = redis::Script::new(
                r"
                local key = KEYS[1]
                local expected_epoch = tonumber(ARGV[1])
                local new_node_json = ARGV[2]
                local ttl = tonumber(ARGV[3])
                local now_str = ARGV[4]

                local existing = redis.call('GET', key)
                if not existing then
                    return -1
                end

                local existing_info = cjson.decode(existing)
                local remote_epoch = existing_info.epoch or 0

                if remote_epoch ~= expected_epoch then
                    -- Epoch mismatch: encode remote_epoch with offset to avoid 0-ambiguity
                    return -(1000 + remote_epoch)
                end

                -- Epoch matches: update heartbeat and refresh TTL
                local node = cjson.decode(new_node_json)
                node['last_heartbeat'] = now_str
                node['epoch'] = expected_epoch
                local final_json = cjson.encode(node)
                redis.call('SETEX', key, ttl, final_json)
                return expected_epoch
                ",
            );

            let op_result: std::result::Result<i64, Error> = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                script
                    .key(&key)
                    .arg(current_epoch)
                    .arg(&node_json)
                    .arg(ttl)
                    .arg(&now_rfc3339)
                    .invoke_async(&mut conn),
            )
            .await
            .map_err(|_| Error::Timeout("Redis heartbeat script timed out".to_string()))
            .and_then(|r| {
                r.map_err(|e| Error::Database(format!("Redis heartbeat script failed: {e}")))
            });
            self.record_operation_result(&op_result);
            let result = op_result?;

            if result == -1 {
                // Check for empty addresses before attempting re-registration
                if grpc_addr.is_empty() || http_addr.is_empty() {
                    tracing::error!(
                        node_id = %self.node_id,
                        grpc_address = %grpc_addr,
                        http_address = %http_addr,
                        "Heartbeat auto-re-registration skipped: empty address(es); \
                         node will be unreachable by peers until addresses are recovered"
                    );
                    return Ok(HeartbeatResult::EmptyAddress);
                }

                // Check if we're in backoff period
                if self.is_in_reregister_backoff_sync() {
                    tracing::debug!(
                        node_id = %self.node_id,
                        backoff_ms = self.reregister_backoff_ms.load(Ordering::Relaxed),
                        "Heartbeat: skipping re-registration due to backoff"
                    );
                    return Ok(HeartbeatResult::NeedReregistration);
                }

                tracing::warn!(
                    node_id = %self.node_id,
                    "Heartbeat failed: key not found, auto-registering"
                );
                // Auto-retry: attempt re-registration once
                if let Err(e) = self.register(grpc_addr, http_addr).await {
                    tracing::error!(
                        node_id = %self.node_id,
                        error = %e,
                        "Auto-registration after heartbeat failure failed"
                    );
                    // Apply backoff on failure
                    self.apply_reregister_backoff();
                    return Ok(HeartbeatResult::NeedReregistration);
                }
                // Reset backoff on success
                self.reset_reregister_backoff();
                tracing::info!(
                    node_id = %self.node_id,
                    "Auto-registration after heartbeat failure succeeded"
                );
                return Ok(HeartbeatResult::Ok);
            } else if result <= -1000 {
                // Lua returns -(1000 + remote_epoch) on epoch mismatch
                let remote_epoch = ((-result) - 1000) as u64;
                // Check for empty addresses before attempting re-registration
                if grpc_addr.is_empty() || http_addr.is_empty() {
                    tracing::error!(
                        node_id = %self.node_id,
                        grpc_address = %grpc_addr,
                        http_address = %http_addr,
                        "Heartbeat auto-re-registration skipped: empty address(es); \
                         node will be unreachable by peers until addresses are recovered"
                    );
                    return Ok(HeartbeatResult::EmptyAddress);
                }

                // Check if we're in backoff period
                if self.is_in_reregister_backoff_sync() {
                    tracing::debug!(
                        node_id = %self.node_id,
                        backoff_ms = self.reregister_backoff_ms.load(Ordering::Relaxed),
                        "Heartbeat: skipping re-registration due to backoff (epoch mismatch)"
                    );
                    return Ok(HeartbeatResult::EpochMismatch(remote_epoch));
                }

                tracing::warn!(
                    node_id = %self.node_id,
                    local_epoch = current_epoch,
                    remote_epoch = remote_epoch,
                    "Epoch mismatch during heartbeat, auto-registering"
                );
                // Auto-retry: attempt re-registration once to resolve epoch conflict
                if let Err(e) = self.register(grpc_addr, http_addr).await {
                    tracing::error!(
                        node_id = %self.node_id,
                        error = %e,
                        "Auto-registration after epoch mismatch failed"
                    );
                    // Apply backoff on failure
                    self.apply_reregister_backoff();
                    return Ok(HeartbeatResult::EpochMismatch(remote_epoch));
                }
                // Reset backoff on success
                self.reset_reregister_backoff();
                tracing::info!(
                    node_id = %self.node_id,
                    new_epoch = self.current_epoch.load(Ordering::SeqCst),
                    "Auto-registration after epoch mismatch succeeded"
                );
                return Ok(HeartbeatResult::Ok);
            }
        }

        // Update local heartbeat time
        let mut nodes = self.local_nodes.write().await;
        if let Some(node) = nodes.get_mut(&self.node_id) {
            node.last_heartbeat = Utc::now();
        }

        // Reset backoff on successful heartbeat
        self.reset_reregister_backoff();

        Ok(HeartbeatResult::Ok)
    }

    /// Unregister this node with fencing token validation
    ///
    /// Uses an atomic Lua script to check epoch <= `local_epoch` before deleting.
    /// Prevents stale nodes from unregistering newer registrations.
    pub async fn unregister(&self) -> Result<()> {
        {
            let mut conn = self.get_conn_with_breaker().await?;

            let key = self.node_key(&self.node_id);
            let current_epoch = self.current_epoch.load(Ordering::SeqCst);

            // Atomic Lua script: only delete if existing epoch <= our epoch
            // Returns 1 if deleted, 0 if skipped (newer epoch exists), -1 if key not found
            let script = redis::Script::new(
                r"
                local key = KEYS[1]
                local local_epoch = tonumber(ARGV[1])

                local existing = redis.call('GET', key)
                if not existing then
                    return -1
                end

                local existing_info = cjson.decode(existing)
                local remote_epoch = existing_info.epoch or 0

                if remote_epoch > local_epoch then
                    -- Newer registration exists, don't delete
                    return 0
                end

                redis.call('DEL', key)
                return 1
                ",
            );

            let op_result: std::result::Result<i64, Error> = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                script.key(&key).arg(current_epoch).invoke_async(&mut conn),
            )
            .await
            .map_err(|_| Error::Timeout("Redis unregister script timed out".to_string()))
            .and_then(|r| {
                r.map_err(|e| Error::Database(format!("Redis unregister script failed: {e}")))
            });
            self.record_operation_result(&op_result);
            let result = op_result?;

            if result == 0 {
                tracing::warn!(
                    node_id = %self.node_id,
                    local_epoch = current_epoch,
                    "Skipping unregister: newer registration exists in Redis"
                );
            }
        }

        // Remove from local cache
        let mut nodes = self.local_nodes.write().await;
        nodes.remove(&self.node_id);

        Ok(())
    }

    /// Register a remote node (called by gRPC handler when another node joins)
    ///
    /// Uses an atomic Lua script that only allows registration if the incoming
    /// epoch >= existing epoch, preventing stale registrations from overwriting newer ones.
    pub async fn register_remote(&self, node_info: NodeInfo) -> Result<()> {
        {
            let mut conn = self.get_conn_with_breaker().await?;

            let key = self.node_key(&node_info.node_id);
            let value = serde_json::to_string(&node_info)
                .map_err(|e| Error::Serialization(format!("Failed to serialize node info: {e}")))?;
            let ttl = self.heartbeat_timeout_secs * 2;

            // Atomic Lua script: only register if incoming epoch >= existing epoch
            // Returns 1 if written, 0 if rejected (existing epoch is higher)
            let script = redis::Script::new(
                r"
                local key = KEYS[1]
                local new_json = ARGV[1]
                local ttl = tonumber(ARGV[2])
                local incoming_epoch = tonumber(ARGV[3])

                local existing = redis.call('GET', key)
                if existing then
                    local existing_info = cjson.decode(existing)
                    local existing_epoch = existing_info.epoch or 0
                    if existing_epoch > incoming_epoch then
                        return 0
                    end
                end

                redis.call('SETEX', key, ttl, new_json)
                return 1
                ",
            );

            let op_result: std::result::Result<i64, Error> = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                script
                    .key(&key)
                    .arg(&value)
                    .arg(ttl)
                    .arg(node_info.epoch)
                    .invoke_async(&mut conn),
            )
            .await
            .map_err(|_| Error::Timeout("Redis register_remote script timed out".to_string()))
            .and_then(|r| {
                r.map_err(|e| Error::Database(format!("Redis register_remote script failed: {e}")))
            });
            self.record_operation_result(&op_result);
            let result = op_result?;

            if result == 0 {
                tracing::warn!(
                    node_id = %node_info.node_id,
                    incoming_epoch = node_info.epoch,
                    "Remote registration rejected: existing node has higher epoch"
                );
                return Ok(());
            }
        }

        let mut nodes = self.local_nodes.write().await;
        nodes.insert(node_info.node_id.clone(), node_info);

        Ok(())
    }

    /// Update heartbeat for a remote node (atomic via Lua script)
    pub async fn heartbeat_remote(&self, node_id: &str) -> Result<()> {
        let mut conn = self.get_conn_with_breaker().await?;

        let key = self.node_key(node_id);
        let now = Utc::now().to_rfc3339();
        let ttl = self.heartbeat_timeout_secs * 2;

        // Atomic Lua: read → update last_heartbeat → write back with fresh TTL
        let script = redis::Script::new(
            r"
            local val = redis.call('GET', KEYS[1])
            if not val then return nil end
            local obj = cjson.decode(val)
            obj['last_heartbeat'] = ARGV[1]
            local updated = cjson.encode(obj)
            redis.call('SETEX', KEYS[1], ARGV[2], updated)
            return updated
            ",
        );

        let op_result: std::result::Result<Option<String>, Error> = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            script.key(&key).arg(&now).arg(ttl).invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis heartbeat script timed out".to_string()))
        .and_then(|r| {
            r.map_err(|e| Error::Database(format!("Redis heartbeat script failed: {e}")))
        });
        self.record_operation_result(&op_result);
        let result = op_result?;

        // Update local cache from the returned value
        if let Some(updated_json) = result {
            if let Ok(node_info) = serde_json::from_str::<NodeInfo>(&updated_json) {
                let mut nodes = self.local_nodes.write().await;
                nodes.insert(node_id.to_string(), node_info);
            }
        }

        Ok(())
    }

    /// Unregister a remote node with epoch validation
    ///
    /// Uses the same atomic Lua script pattern as `unregister()` to validate
    /// that the existing epoch is not newer than what we expect, preventing
    /// stale deregister requests from removing re-registered nodes.
    pub async fn unregister_remote(
        &self,
        node_id: &str,
        expected_epoch: Option<u64>,
    ) -> Result<()> {
        {
            let mut conn = self.get_conn_with_breaker().await?;

            let key = self.node_key(node_id);

            // Use epoch validation if provided, otherwise just delete
            if let Some(epoch) = expected_epoch {
                // Atomic Lua script: only delete if existing epoch <= expected epoch
                let script = redis::Script::new(
                    r"
                    local key = KEYS[1]
                    local expected_epoch = tonumber(ARGV[1])

                    local existing = redis.call('GET', key)
                    if not existing then
                        return -1
                    end

                    local existing_info = cjson.decode(existing)
                    local remote_epoch = existing_info.epoch or 0

                    if remote_epoch > expected_epoch then
                        return 0
                    end

                    redis.call('DEL', key)
                    return 1
                    ",
                );

                let op_result: std::result::Result<i64, Error> = timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    script.key(&key).arg(epoch).invoke_async(&mut conn),
                )
                .await
                .map_err(|_| Error::Timeout("Redis unregister_remote script timed out".to_string()))
                .and_then(|r| {
                    r.map_err(|e| {
                        Error::Database(format!("Redis unregister_remote script failed: {e}"))
                    })
                });
                self.record_operation_result(&op_result);
                let result = op_result?;

                if result == 0 {
                    tracing::warn!(
                        node_id = %node_id,
                        expected_epoch = epoch,
                        "Skipping remote unregister: newer registration exists in Redis"
                    );
                    return Ok(());
                }
            } else {
                // No epoch provided: fetch current epoch from Redis, then use
                // epoch-validated deletion. This prevents a stale deregister
                // request from removing a re-registered node.
                tracing::warn!(
                    node_id = %node_id,
                    "unregister_remote called without expected_epoch, fetching current epoch from Redis"
                );
                // Atomic Lua: GET the current epoch, then DEL only if the epoch hasn't changed
                // between the read and the delete (single script = atomic).
                let script = redis::Script::new(
                    r"
                    local key = KEYS[1]
                    local existing = redis.call('GET', key)
                    if not existing then
                        return -1
                    end
                    local existing_info = cjson.decode(existing)
                    local current_epoch = existing_info.epoch or 0
                    redis.call('DEL', key)
                    return current_epoch
                    ",
                );
                let op_result: std::result::Result<i64, Error> = timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    script.key(&key).invoke_async(&mut conn),
                )
                .await
                .map_err(|_| {
                    Error::Timeout(
                        "Redis unregister_remote (no epoch) script timed out".to_string(),
                    )
                })
                .and_then(|r| {
                    r.map_err(|e| {
                        Error::Database(format!(
                            "Redis unregister_remote (no epoch) script failed: {e}"
                        ))
                    })
                });
                self.record_operation_result(&op_result);
                let result = op_result?;

                if result == -1 {
                    tracing::debug!(
                        node_id = %node_id,
                        "unregister_remote (no epoch): key not found in Redis"
                    );
                } else {
                    tracing::info!(
                        node_id = %node_id,
                        deleted_epoch = result,
                        "unregister_remote (no epoch): atomically read and deleted node"
                    );
                }
            }
        }

        let mut nodes = self.local_nodes.write().await;
        nodes.remove(node_id);

        Ok(())
    }

    /// Get all active nodes (cached for [`NODES_CACHE_TTL_SECS`] to avoid
    /// hammering Redis). See the constant's documentation for staleness trade-offs.
    ///
    /// In `Degraded` mode (circuit breaker open), falls back to the local cache
    /// instead of returning an error, so that health checks and load balancing
    /// can continue operating on stale data.
    pub async fn get_all_nodes(&self) -> Result<Vec<NodeInfo>> {
        // Check the short-lived cache first
        if let Some(cached) = self.nodes_cache.get(&()).await {
            return Ok(cached);
        }

        match self.get_all_nodes_uncached().await {
            Ok(result) => {
                // Record successful refresh timestamp
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.last_refreshed.store(now, Ordering::Relaxed);
                self.nodes_cache.insert((), result.clone()).await;
                Ok(result)
            }
            Err(e) => {
                // In Degraded mode, fall back to local cache instead of propagating error
                if self.cluster_mode() != ClusterMode::Normal {
                    tracing::debug!(
                        mode = %self.cluster_mode(),
                        error = %e,
                        "get_all_nodes falling back to local cache in degraded mode"
                    );
                    return Ok(self.get_all_nodes_local().await);
                }
                Err(e)
            }
        }
    }

    /// Uncached implementation of get_all_nodes for internal use.
    async fn get_all_nodes_uncached(&self) -> Result<Vec<NodeInfo>> {
        {
            let mut conn = self.get_conn_with_breaker().await?;

            // Use SCAN instead of KEYS for better performance on large datasets
            // SCAN is non-blocking and returns results incrementally
            let pattern = format!("{}:*", self.key_prefix);
            let mut keys = Vec::new();
            let mut cursor: u64 = 0;
            /// Maximum number of SCAN iterations to prevent an infinite loop on
            /// large or unexpectedly-growing keyspaces.  Each iteration requests
            /// up to 100 keys (COUNT hint), so 1 000 iterations covers ~100 000
            /// cluster-node keys — far beyond any realistic deployment.
            const MAX_SCAN_ITERATIONS: usize = 1000;
            let mut scan_iterations: usize = 0;

            loop {
                if scan_iterations >= MAX_SCAN_ITERATIONS {
                    tracing::warn!(
                        pattern = %pattern,
                        iterations = scan_iterations,
                        keys_found = keys.len(),
                        "SCAN loop reached maximum iteration limit; \
                         keyspace may be larger than expected or cursor is cycling"
                    );
                    break;
                }
                scan_iterations += 1;

                let op_result: std::result::Result<(u64, Vec<String>), Error> = timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    redis::cmd("SCAN")
                        .arg(cursor)
                        .arg("MATCH")
                        .arg(&pattern)
                        .arg("COUNT")
                        .arg(100) // Scan 100 keys at a time
                        .query_async(&mut conn),
                )
                .await
                .map_err(|_| Error::Timeout("Redis SCAN timed out".to_string()))
                .and_then(|r| r.map_err(|e| Error::Database(format!("Redis SCAN failed: {e}"))));
                self.record_operation_result(&op_result);
                let scan_result = op_result?;

                cursor = scan_result.0;
                keys.extend(scan_result.1);

                // cursor 0 means iteration complete
                if cursor == 0 {
                    break;
                }
            }

            let mut nodes = Vec::new();
            if !keys.is_empty() {
                // Use MGET to fetch all values in one round trip instead of N individual GETs
                let mut cmd = redis::cmd("MGET");
                for key in &keys {
                    cmd.arg(key);
                }
                let mget_result: std::result::Result<Vec<Option<String>>, Error> = timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    cmd.query_async(&mut conn),
                )
                .await
                .map_err(|_| Error::Timeout("Redis MGET timed out".to_string()))
                .and_then(|r| r.map_err(|e| Error::Database(format!("Redis MGET failed: {e}"))));
                self.record_operation_result(&mget_result);
                let values = mget_result?;

                for value in values.into_iter().flatten() {
                    if let Ok(node_info) = serde_json::from_str::<NodeInfo>(&value) {
                        if !node_info.is_stale(self.heartbeat_timeout_secs) {
                            nodes.push(node_info);
                        }
                    }
                }
            }

            // Merge Redis results into local cache instead of destructively clearing.
            // This preserves locally-known nodes that may be transiently absent from
            // Redis (e.g., during a partial outage). Nodes confirmed absent from Redis
            // AND stale are pruned.
            let mut local_nodes = self.local_nodes.write().await;
            let redis_node_ids: std::collections::HashSet<String> =
                nodes.iter().map(|n| n.node_id.clone()).collect();

            // Update/insert nodes found in Redis
            for node in &nodes {
                local_nodes.insert(node.node_id.clone(), node.clone());
            }

            // Remove local nodes that are absent from Redis AND stale
            local_nodes.retain(|node_id, info| {
                redis_node_ids.contains(node_id) || !info.is_stale(self.heartbeat_timeout_secs)
            });

            Ok(nodes)
        }
    }

    /// Get a specific node by ID
    pub async fn get_node(&self, node_id: &str) -> Result<Option<NodeInfo>> {
        let mut conn = self.get_conn_with_breaker().await?;

        let key = self.node_key(node_id);
        let op_result: std::result::Result<Option<String>, Error> = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            redis::cmd("GET").arg(&key).query_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Timeout("Redis GET timed out".to_string()))
        .and_then(|r| r.map_err(|e| Error::Database(format!("Redis GET failed: {e}"))));
        self.record_operation_result(&op_result);
        let value = op_result?;

        if let Some(value) = value {
            let node_info: NodeInfo = serde_json::from_str(&value).map_err(|e| {
                Error::Serialization(format!("Failed to deserialize node info: {e}"))
            })?;

            if node_info.is_stale(self.heartbeat_timeout_secs) {
                return Ok(None);
            }

            Ok(Some(node_info))
        } else {
            Ok(None)
        }
    }

    /// Update metadata for this node in the local cache
    ///
    /// This should be called periodically by the heartbeat loop to include
    /// connection counts and other metrics. The metadata will be persisted
    /// to Redis on the next heartbeat.
    pub async fn update_local_metadata(&self, key: &str, value: String) {
        let mut nodes = self.local_nodes.write().await;
        if let Some(node) = nodes.get_mut(&self.node_id) {
            node.metadata.insert(key.to_string(), value);
        }
    }

    /// Merge externally discovered peers (e.g. from K8s DNS) into the local
    /// cache so that HealthMonitor and LoadBalancer can see them before they
    /// self-register via Redis.
    ///
    /// Only inserts nodes that are not already present in the local cache.
    /// Existing entries (which may have richer metadata from Redis heartbeats)
    /// are left untouched.
    pub async fn merge_dns_peers(&self, peers: Vec<NodeInfo>) {
        let mut nodes = self.local_nodes.write().await;
        for peer in peers {
            nodes.entry(peer.node_id.clone()).or_insert(peer);
        }
    }

    /// Read all non-stale nodes from the local in-memory cache.
    ///
    /// Unlike [`get_all_nodes`], this does NOT query Redis. It returns whatever
    /// nodes are currently in the local cache, which is kept up-to-date by
    /// heartbeat, registration, and `get_all_nodes()` calls. Useful when a
    /// Redis round-trip is not desired (e.g., in hot-path load balancing or
    /// when Redis is temporarily unavailable).
    pub async fn get_all_nodes_local(&self) -> Vec<NodeInfo> {
        let timeout = self.heartbeat_timeout_secs;
        let nodes = self.local_nodes.read().await;
        nodes
            .values()
            .filter(|n| !n.is_stale(timeout))
            .cloned()
            .collect()
    }

    /// Read a single node from the local in-memory cache.
    ///
    /// Unlike [`get_node`], this does NOT query Redis.
    pub async fn get_node_local(&self, node_id: &str) -> Option<NodeInfo> {
        let nodes = self.local_nodes.read().await;
        nodes.get(node_id).cloned()
    }

    /// Returns the current cluster operating mode.
    ///
    /// - `Normal`: Redis is reachable, full cluster functionality.
    /// - `Degraded`: Redis circuit breaker is open, serving from local cache.
    /// - `Standalone`: prolonged Redis unavailability (reserved).
    #[must_use]
    pub fn cluster_mode(&self) -> ClusterMode {
        *self.cluster_mode.read()
    }

    /// Get the cancellation token for graceful shutdown signaling.
    ///
    /// Cancel this token to stop background tasks (e.g., health probe).
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Returns `true` if the local node cache is stale (i.e., has not been
    /// successfully refreshed from Redis within [`NODES_STALE_THRESHOLD_SECS`]).
    ///
    /// Callers (e.g., health check endpoints) can use this to include a
    /// "data may be stale" warning in their responses.
    #[must_use]
    pub fn is_nodes_stale(&self) -> bool {
        let last = self.last_refreshed.load(Ordering::Relaxed);
        if last == 0 {
            // Never refreshed — consider stale
            return true;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(last) > NODES_STALE_THRESHOLD_SECS
    }

    /// Returns the Unix timestamp (seconds) of the last successful refresh
    /// from Redis. Returns 0 if no refresh has occurred yet.
    #[must_use]
    pub fn last_refreshed_at(&self) -> u64 {
        self.last_refreshed.load(Ordering::Relaxed)
    }

    fn node_key(&self, node_id: &str) -> String {
        format!("{}:{}", self.key_prefix, node_id)
    }

    /// Insert a node directly into the local cache, bypassing Redis.
    ///
    /// This is intended for unit tests that need to populate node state
    /// without a running Redis instance. Production code should use
    /// [`register`] or [`register_remote`].
    #[doc(hidden)]
    pub async fn test_insert_local(&self, node_info: NodeInfo) {
        let mut nodes = self.local_nodes.write().await;
        nodes.insert(node_info.node_id.clone(), node_info);
    }

    /// Remove a node directly from the local cache, bypassing Redis.
    ///
    /// Test-only counterpart to [`test_insert_local`].
    #[doc(hidden)]
    pub async fn test_remove_local(&self, node_id: &str) {
        let mut nodes = self.local_nodes.write().await;
        nodes.remove(node_id);
    }

    /// Alias for [`get_all_nodes_local`] in test context.
    #[doc(hidden)]
    pub async fn test_get_all_local(&self) -> Vec<NodeInfo> {
        self.get_all_nodes_local().await
    }

    /// Alias for [`get_node_local`] in test context.
    #[doc(hidden)]
    pub async fn test_get_local(&self, node_id: &str) -> Option<NodeInfo> {
        self.get_node_local(node_id).await
    }

    // ============ Re-registration backoff methods ============

    /// Check if we're currently in a backoff period for re-registration.
    ///
    /// Returns `true` if the time since the last re-registration attempt is
    /// less than the current backoff duration.
    fn is_in_reregister_backoff_sync(&self) -> bool {
        let last_attempt = self.last_reregister_attempt.load(Ordering::Relaxed);
        if last_attempt == 0 {
            return false; // No previous attempt, not in backoff
        }

        let backoff_ms = self.reregister_backoff_ms.load(Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let elapsed_ms = now_ms.saturating_sub(last_attempt);
        elapsed_ms < backoff_ms
    }

    /// Apply exponential backoff after a failed re-registration.
    ///
    /// Updates the last attempt timestamp and increases the backoff duration
    /// by the multiplier (2x), up to the maximum.
    fn apply_reregister_backoff(&self) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.last_reregister_attempt
            .store(now_ms, Ordering::Relaxed);

        // Increase backoff exponentially, up to max
        let current_backoff = self.reregister_backoff_ms.load(Ordering::Relaxed);
        let new_backoff = std::cmp::min(
            current_backoff * REREGISTER_BACKOFF_MULTIPLIER,
            MAX_REREGISTER_BACKOFF_SECS * 1000,
        );
        self.reregister_backoff_ms
            .store(new_backoff, Ordering::Relaxed);

        tracing::debug!(
            previous_backoff_ms = current_backoff,
            new_backoff_ms = new_backoff,
            "Increased re-registration backoff"
        );
    }

    /// Reset the re-registration backoff after a successful operation.
    ///
    /// Called after successful heartbeat or registration to clear the backoff
    /// state, allowing immediate re-registration on the next failure.
    fn reset_reregister_backoff(&self) {
        let current_backoff = self.reregister_backoff_ms.load(Ordering::Relaxed);
        if current_backoff > INITIAL_REREGISTER_BACKOFF_SECS * 1000 {
            tracing::debug!(
                previous_backoff_ms = current_backoff,
                "Resetting re-registration backoff to initial value"
            );
        }
        self.reregister_backoff_ms
            .store(INITIAL_REREGISTER_BACKOFF_SECS * 1000, Ordering::Relaxed);
        // Reset last_reregister_attempt so next failure won't be in backoff
        self.last_reregister_attempt.store(0, Ordering::Relaxed);
    }

    /// Check if currently in re-registration backoff period (async wrapper for tests).
    #[doc(hidden)]
    pub async fn is_in_reregister_backoff(&self) -> bool {
        self.is_in_reregister_backoff_sync()
    }

    /// Get the current re-registration backoff duration.
    #[doc(hidden)]
    pub async fn current_reregister_backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.reregister_backoff_ms.load(Ordering::Relaxed))
    }

    /// Get the timestamp of the last re-registration attempt (for tests).
    #[doc(hidden)]
    pub async fn last_reregister_attempt(&self) -> u64 {
        self.last_reregister_attempt.load(Ordering::Relaxed)
    }

    /// Set a specific backoff duration for testing purposes.
    #[doc(hidden)]
    pub async fn set_reregister_backoff_for_test(&self, duration: std::time::Duration) {
        self.reregister_backoff_ms
            .store(duration.as_millis() as u64, Ordering::Relaxed);
        // Set last attempt to now so the backoff is active
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_reregister_attempt
            .store(now_ms, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_node_info_stale() {
        let mut node = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        );

        // Fresh node should not be stale
        assert!(!node.is_stale(30));

        // Simulate old heartbeat
        node.last_heartbeat = Utc::now() - Duration::seconds(60);
        assert!(node.is_stale(30));
    }

    #[test]
    fn test_node_info_epoch_initialization() {
        let node = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        );

        // New nodes should start with epoch 1
        assert_eq!(node.epoch, 1);
    }

    #[test]
    fn test_node_info_with_epoch() {
        let node = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        )
        .with_epoch(5);

        assert_eq!(node.epoch, 5);
    }

    #[test]
    fn test_fencing_token_new() {
        let token = FencingToken::new("node1".to_string(), 3);
        assert_eq!(token.node_id, "node1");
        assert_eq!(token.epoch, 3);
    }

    #[test]
    fn test_fencing_token_is_newer_than() {
        let token1 = FencingToken::new("node1".to_string(), 3);
        let token2 = FencingToken::new("node1".to_string(), 5);
        let token3 = FencingToken::new("node2".to_string(), 5);

        // Same node, higher epoch is newer
        assert!(token2.is_newer_than(&token1));
        assert!(!token1.is_newer_than(&token2));

        // Different nodes - not newer even with higher epoch
        assert!(!token3.is_newer_than(&token1));

        // Same token is not newer than itself
        assert!(!token1.is_newer_than(&token1));
    }

    #[test]
    fn test_node_info_fencing_token() {
        let node = NodeInfo::new(
            "test_node".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        )
        .with_epoch(10);

        let token = node.fencing_token();
        assert_eq!(token.node_id, "test_node");
        assert_eq!(token.epoch, 10);
    }

    #[test]
    fn test_node_registry_creation_and_fencing_token() {
        // redis::Client::open succeeds even without a running Redis server
        let registry = NodeRegistry::new(
            redis::Client::open("redis://localhost:6379").unwrap(),
            "test_node".to_string(),
            30,
            "synctv:",
        )
        .unwrap();

        // Get fencing token
        let token = registry.current_fencing_token();
        assert_eq!(token.node_id, "test_node");
        assert_eq!(token.epoch, 1);
    }

    #[test]
    fn test_fencing_token_serialization() {
        let token = FencingToken::new("node1".to_string(), 42);

        // Serialize to JSON
        let json = serde_json::to_string(&token).unwrap();
        assert!(json.contains("node1"));
        assert!(json.contains("42"));

        // Deserialize back
        let deserialized: FencingToken = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.node_id, "node1");
        assert_eq!(deserialized.epoch, 42);
    }

    #[test]
    fn test_node_info_serialization_with_epoch() {
        let node = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        )
        .with_epoch(7);

        // Serialize to JSON
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"epoch\":7"));

        // Deserialize back
        let deserialized: NodeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.epoch, 7);
    }

    #[tokio::test]
    async fn test_merge_dns_peers_inserts_new() {
        let registry = NodeRegistry::new(
            redis::Client::open("redis://localhost:6379").unwrap(),
            "self".to_string(),
            30,
            "synctv:",
        )
        .unwrap();

        let peer = NodeInfo::new(
            "dns-peer-1".to_string(),
            "10.0.0.2:50051".to_string(),
            "10.0.0.2:8080".to_string(),
        );

        registry.merge_dns_peers(vec![peer]).await;

        let nodes = registry.local_nodes.read().await;
        assert!(nodes.contains_key("dns-peer-1"));
        assert_eq!(nodes["dns-peer-1"].grpc_address, "10.0.0.2:50051");
    }

    #[tokio::test]
    async fn test_merge_dns_peers_does_not_overwrite_existing() {
        let registry = NodeRegistry::new(
            redis::Client::open("redis://localhost:6379").unwrap(),
            "self".to_string(),
            30,
            "synctv:",
        )
        .unwrap();

        // Pre-populate local cache directly (simulating a prior registration)
        {
            let mut nodes = registry.local_nodes.write().await;
            nodes.insert(
                "self".to_string(),
                NodeInfo::new(
                    "self".to_string(),
                    "10.0.0.1:50051".to_string(),
                    "10.0.0.1:8080".to_string(),
                ),
            );
        }

        // Try to merge a DNS peer with the same node_id ("self")
        let dns_peer = NodeInfo::new(
            "self".to_string(),
            "10.0.0.99:50051".to_string(),
            "10.0.0.99:8080".to_string(),
        );

        registry.merge_dns_peers(vec![dns_peer]).await;

        // Original registration should be preserved (not overwritten)
        let nodes = registry.local_nodes.read().await;
        assert_eq!(nodes["self"].grpc_address, "10.0.0.1:50051");
    }

    #[test]
    fn test_heartbeat_result_variants() {
        // Test that HeartbeatResult variants exist and can be matched
        let ok = HeartbeatResult::Ok;
        let need_rereg = HeartbeatResult::NeedReregistration;
        let epoch_mismatch = HeartbeatResult::EpochMismatch(42);
        let empty_addr = HeartbeatResult::EmptyAddress;

        match ok {
            HeartbeatResult::Ok => {}
            HeartbeatResult::NeedReregistration => panic!("wrong variant"),
            HeartbeatResult::EpochMismatch(_) => panic!("wrong variant"),
            HeartbeatResult::EmptyAddress => panic!("wrong variant"),
        }

        match need_rereg {
            HeartbeatResult::Ok => panic!("wrong variant"),
            HeartbeatResult::NeedReregistration => {}
            HeartbeatResult::EpochMismatch(_) => panic!("wrong variant"),
            HeartbeatResult::EmptyAddress => panic!("wrong variant"),
        }

        match epoch_mismatch {
            HeartbeatResult::Ok => panic!("wrong variant"),
            HeartbeatResult::NeedReregistration => panic!("wrong variant"),
            HeartbeatResult::EpochMismatch(e) => assert_eq!(e, 42),
            HeartbeatResult::EmptyAddress => panic!("wrong variant"),
        }

        match empty_addr {
            HeartbeatResult::Ok => panic!("wrong variant"),
            HeartbeatResult::NeedReregistration => panic!("wrong variant"),
            HeartbeatResult::EpochMismatch(_) => panic!("wrong variant"),
            HeartbeatResult::EmptyAddress => {}
        }
    }

    #[tokio::test]
    async fn test_node_info_empty_address_detection() {
        // Test that we can detect empty addresses in NodeInfo
        let node_with_empty_grpc = NodeInfo::new(
            "test".to_string(),
            String::new(), // empty grpc_address
            "localhost:8080".to_string(),
        );
        assert!(node_with_empty_grpc.grpc_address.is_empty());
        assert!(!node_with_empty_grpc.http_address.is_empty());

        let node_with_empty_http = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            String::new(), // empty http_address
        );
        assert!(!node_with_empty_http.grpc_address.is_empty());
        assert!(node_with_empty_http.http_address.is_empty());

        let node_with_both_empty = NodeInfo::new("test".to_string(), String::new(), String::new());
        assert!(node_with_both_empty.grpc_address.is_empty());
        assert!(node_with_both_empty.http_address.is_empty());

        let node_with_both_valid = NodeInfo::new(
            "test".to_string(),
            "localhost:50051".to_string(),
            "localhost:8080".to_string(),
        );
        assert!(!node_with_both_valid.grpc_address.is_empty());
        assert!(!node_with_both_valid.http_address.is_empty());
    }

    #[tokio::test]
    async fn test_local_cache_empty_address_scenario() {
        // Test that when local cache has empty addresses, we can detect it
        let registry = NodeRegistry::new(
            redis::Client::open("redis://localhost:6379").unwrap(),
            "test_node".to_string(),
            30,
            "synctv:",
        )
        .unwrap();

        // Simulate a scenario where local cache has empty addresses
        // (e.g., node was registered with empty addresses due to a bug)
        {
            let mut nodes = registry.local_nodes.write().await;
            nodes.insert(
                "test_node".to_string(),
                NodeInfo::new(
                    "test_node".to_string(),
                    String::new(), // empty grpc_address
                    String::new(), // empty http_address
                ),
            );
        }

        // Verify we can read back and detect the empty addresses
        {
            let nodes = registry.local_nodes.read().await;
            let info = nodes.get("test_node").unwrap();
            assert!(info.grpc_address.is_empty());
            assert!(info.http_address.is_empty());
        }
    }

    #[tokio::test]
    async fn test_local_cache_missing_scenario() {
        // Test that when local cache is missing, we can detect it
        let registry = NodeRegistry::new(
            redis::Client::open("redis://localhost:6379").unwrap(),
            "test_node".to_string(),
            30,
            "synctv:",
        )
        .unwrap();

        // Don't insert anything into local cache, simulating a missing entry
        {
            let nodes = registry.local_nodes.read().await;
            assert!(!nodes.contains_key("test_node"));
        }
    }
}
