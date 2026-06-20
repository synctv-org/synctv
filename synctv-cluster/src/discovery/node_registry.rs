//! Node registry for cluster member discovery
//!
//! Uses Redis to track active nodes in the cluster.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use failsafe::{backoff, failure_policy, Config as CbConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use synctv_core::RedisCoordinationRuntime;
use tokio::sync::RwLock;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use super::runtime::{ClusterNodeDirectory, ClusterNodeDirectoryFactory};
use crate::error::{Error, Result};

/// Staleness threshold in seconds. If `last_refreshed` is older than this,
/// `is_nodes_stale()` returns `true`.
const NODES_STALE_THRESHOLD_SECS: u64 = 30;

/// Interval between cached Redis connection health checks.
const HEALTH_CHECK_INTERVAL_SECS: u64 = 30;

/// TTL for the `get_all_nodes()` moka cache, in seconds.
///
/// This controls the maximum staleness of node discovery queries used by health
/// checks and load balancer. A 2-second window means that a node that registers
/// or deregisters may not appear/disappear from `get_all_nodes()` for up to 2s.
///
/// Trade-offs:
/// - **Lower value** (1-2s): fresher view of the cluster, but more Redis index
///   reads under high query rates (health probes run every ~5-15s, so this is
///   usually negligible).
/// - **Higher value** (5-10s): fewer Redis round-trips, but stale membership
///   data may cause load balancer to route to a recently-departed node or miss
///   a newly-joined node for longer.
///
/// The value of 2s was chosen as a balance: it is well below the heartbeat
/// interval (typically 10s) so membership changes are reflected promptly, while
/// still coalescing bursts of `get_all_nodes()` calls within the same tick.
const NODES_CACHE_TTL_SECS: u64 = 2;

/// Maximum number of SSCAN iterations when listing node IDs from the index set.
const MAX_INDEX_SCAN_ITERATIONS: usize = 1000;
const NODE_METADATA_DISCOVERY_KEY: &str = "discovery";
const NODE_DISCOVERY_SOURCE_K8S_DNS: &str = "k8s_dns";

static REGISTER_NODE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local key = KEYS[1]
        local index_key = KEYS[2]
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
        redis.call('SADD', index_key, node_id)
        redis.call('EXPIRE', index_key, ttl)

        return new_epoch
        ",
    )
});

static HEARTBEAT_NODE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local key = KEYS[1]
        local index_key = KEYS[2]
        local expected_epoch = tonumber(ARGV[1])
        local new_node_json = ARGV[2]
        local ttl = tonumber(ARGV[3])
        local now_str = ARGV[4]
        local node_id = ARGV[5]

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
        redis.call('SADD', index_key, node_id)
        redis.call('EXPIRE', index_key, ttl)
        return expected_epoch
        ",
    )
});

static UNREGISTER_NODE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local key = KEYS[1]
        local index_key = KEYS[2]
        local expected_epoch = tonumber(ARGV[1])
        local node_id = ARGV[2]

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
        redis.call('SREM', index_key, node_id)
        return 1
        ",
    )
});

static REGISTER_REMOTE_NODE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local key = KEYS[1]
        local index_key = KEYS[2]
        local new_json = ARGV[1]
        local ttl = tonumber(ARGV[2])
        local incoming_epoch = tonumber(ARGV[3])
        local node_id = ARGV[4]

        local existing = redis.call('GET', key)
        if existing then
            local existing_info = cjson.decode(existing)
            local existing_epoch = existing_info.epoch or 0
            if existing_epoch > incoming_epoch then
                return 0
            end
        end

        redis.call('SETEX', key, ttl, new_json)
        redis.call('SADD', index_key, node_id)
        redis.call('EXPIRE', index_key, ttl)
        return 1
        ",
    )
});

/// Create a failsafe circuit breaker for Redis operations.
///
/// Opens after 3 consecutive failures. Uses exponential backoff starting at
/// 10 seconds up to 60 seconds before allowing probe requests in half-open state.
fn create_redis_circuit_breaker(
) -> failsafe::StateMachine<failure_policy::ConsecutiveFailures<backoff::Exponential>, ()> {
    let backoff = backoff::exponential(Duration::from_secs(10), Duration::from_mins(1));
    let policy = failure_policy::consecutive_failures(3, backoff);
    CbConfig::new().failure_policy(policy).build()
}

fn unix_epoch_elapsed() -> Duration {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "System clock is before UNIX_EPOCH while reading cluster node registry time"
            );
            Duration::ZERO
        }
    }
}

fn unix_time_millis_u64() -> u64 {
    u64::try_from(unix_epoch_elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn unix_time_secs_u64() -> u64 {
    unix_epoch_elapsed().as_secs()
}

#[cfg(any(test, feature = "test-support"))]
fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Returns `true` if the error message indicates a Redis Sentinel failover is in progress.
///
/// During a Sentinel failover, the previous primary transitions to read-only mode
/// (READONLY error) or the new primary may still be loading data (LOADING error).
fn is_sentinel_failover_error(error_msg: &str) -> bool {
    error_msg.contains("READONLY") || error_msg.contains("LOADING")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeDiscoverySource {
    K8sDns,
}

impl NodeDiscoverySource {
    #[must_use]
    const fn metadata_value(self) -> &'static str {
        match self {
            Self::K8sDns => NODE_DISCOVERY_SOURCE_K8S_DNS,
        }
    }
}

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub api_address: String,
    pub last_heartbeat: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
    /// Fencing token (epoch) for split-brain protection
    /// Increments on each registration to prevent stale updates
    #[serde(default)]
    pub epoch: u64,
}

impl NodeInfo {
    #[must_use]
    pub fn new(node_id: String, api_address: String) -> Self {
        Self {
            node_id,
            api_address,
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

    #[must_use]
    pub fn with_discovery_source(mut self, source: NodeDiscoverySource) -> Self {
        self.set_discovery_source(source);
        self
    }

    pub fn set_discovery_source(&mut self, source: NodeDiscoverySource) {
        self.metadata.insert(
            NODE_METADATA_DISCOVERY_KEY.to_string(),
            source.metadata_value().to_string(),
        );
    }

    #[must_use]
    pub fn has_discovery_source(&self, source: NodeDiscoverySource) -> bool {
        self.metadata
            .get(NODE_METADATA_DISCOVERY_KEY)
            .is_some_and(|value| value == source.metadata_value())
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

/// Node view policy for routing and other correctness-sensitive consumers.
///
/// `LoadBalancer` and similar call paths must not silently consume stale local
/// cache entries as if they were authoritative cluster membership data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeViewMode {
    /// Fresh Redis-backed view in normal clustered operation.
    Fresh,
    /// Degraded mode fallback to local cache while the cache is still within
    /// the staleness budget.
    DegradedCache,
    /// Local-only mode without Redis. The local cache is the source of truth.
    LocalOnly,
}

impl std::fmt::Display for ClusterMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Standalone => write!(f, "Standalone"),
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
    /// Redis coordination runtime (None in local-only mode)
    redis_runtime: Option<Arc<dyn RedisCoordinationRuntime>>,
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

#[derive(Clone)]
pub struct RedisClusterNodeDirectoryFactory {
    redis_runtime: Arc<dyn RedisCoordinationRuntime>,
}

impl RedisClusterNodeDirectoryFactory {
    #[must_use]
    pub fn new(redis_runtime: Arc<dyn RedisCoordinationRuntime>) -> Self {
        Self { redis_runtime }
    }
}

impl ClusterNodeDirectoryFactory for RedisClusterNodeDirectoryFactory {
    fn build(
        &self,
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Arc<dyn ClusterNodeDirectory>> {
        Ok(Arc::new(NodeRegistry::new(
            self.redis_runtime.clone(),
            node_id,
            heartbeat_timeout_secs,
            key_prefix,
        )?))
    }
}

#[derive(Clone, Default)]
pub struct LocalClusterNodeDirectoryFactory;

impl ClusterNodeDirectoryFactory for LocalClusterNodeDirectoryFactory {
    fn build(
        &self,
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Arc<dyn ClusterNodeDirectory>> {
        Ok(Arc::new(NodeRegistry::new_local_only(
            node_id,
            heartbeat_timeout_secs,
            key_prefix,
        )?))
    }
}

impl NodeRegistry {
    async fn invalidate_node_view_cache(&self) {
        self.nodes_cache.invalidate(&()).await;
        self.last_refreshed.store(0, Ordering::Relaxed);
    }

    fn redis_operation_timeout(&self) -> Duration {
        self.redis_runtime.as_ref().map_or(
            synctv_core::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            |runtime| runtime.operation_timeout(),
        )
    }

    fn node_index_key(&self) -> String {
        format!("{}:index", self.key_prefix)
    }

    fn filter_routable_nodes(&self, nodes: Vec<NodeInfo>) -> Vec<NodeInfo> {
        if self.local_only {
            return nodes;
        }

        nodes
            .into_iter()
            .filter(|node| !node.has_discovery_source(NodeDiscoverySource::K8sDns))
            .collect()
    }

    fn merge_verified_discovery_nodes(
        &self,
        redis_nodes: Vec<NodeInfo>,
        local_nodes: &HashMap<String, NodeInfo>,
    ) -> Vec<NodeInfo> {
        let redis_node_ids: std::collections::HashSet<String> = redis_nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect();
        let mut merged_nodes = redis_nodes;

        for node in local_nodes.values() {
            if !node.is_stale(self.heartbeat_timeout_secs)
                && node.has_discovery_source(NodeDiscoverySource::K8sDns)
                && !redis_node_ids.contains(&node.node_id)
            {
                merged_nodes.push(node.clone());
            }
        }

        merged_nodes
    }

    /// Create a new node registry backed by Redis.
    ///
    /// Redis is required for all cluster coordination. If the Redis URL is
    /// invalid, returns an error immediately. The caller (typically `main.rs`)
    /// should abort startup if this fails.
    ///
    /// The `key_prefix` is prepended to cluster node keys in Redis (e.g. `"synctv:"` produces
    /// keys like `synctv:cluster:nodes:<node_id>`). Pass an empty string to use unprefixed keys.
    pub fn new(
        redis_runtime: Arc<dyn RedisCoordinationRuntime>,
        node_id: String,
        heartbeat_timeout_secs: i64,
        key_prefix: &str,
    ) -> Result<Self> {
        let nodes_cache = moka::future::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(NODES_CACHE_TTL_SECS))
            .max_capacity(1)
            .build();

        Ok(Self {
            redis_runtime: Some(redis_runtime),
            cached_conn: tokio::sync::Mutex::new(None),
            last_health_check: AtomicU64::new(0),
            node_id,
            heartbeat_timeout_secs,
            local_nodes: Arc::new(RwLock::new(HashMap::new())),
            current_epoch: Arc::new(AtomicU64::new(1)),
            circuit_breaker: Some(create_redis_circuit_breaker()),
            nodes_cache,
            key_prefix: format!("{key_prefix}cluster:nodes"),
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
            redis_runtime: None,
            cached_conn: tokio::sync::Mutex::new(None),
            last_health_check: AtomicU64::new(0),
            node_id,
            heartbeat_timeout_secs,
            local_nodes: Arc::new(RwLock::new(HashMap::new())),
            current_epoch: Arc::new(AtomicU64::new(1)),
            circuit_breaker: None,
            nodes_cache,
            key_prefix: format!("{key_prefix}cluster:nodes"),
            health_probe_running: Arc::new(AtomicBool::new(false)),
            cluster_mode: Arc::new(parking_lot::RwLock::new(ClusterMode::Standalone)),
            last_refreshed: Arc::new(AtomicU64::new(0)),
            cancel_token: CancellationToken::new(),
            last_reregister_attempt: AtomicU64::new(0),
            reregister_backoff_ms: AtomicU64::new(INITIAL_REREGISTER_BACKOFF_SECS * 1000),
            local_only: true,
        })
    }

    async fn fetch_indexed_node_ids(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
    ) -> Result<Vec<String>> {
        let index_key = self.node_index_key();
        let mut node_ids = Vec::new();
        let mut cursor: u64 = 0;
        let mut scan_iterations = 0usize;

        loop {
            if scan_iterations >= MAX_INDEX_SCAN_ITERATIONS {
                tracing::warn!(
                    index_key = %index_key,
                    iterations = scan_iterations,
                    node_ids_found = node_ids.len(),
                    "SSCAN loop reached maximum iteration limit; node index may be larger than expected or cursor is cycling"
                );
                break;
            }
            scan_iterations += 1;

            let op_result: std::result::Result<(u64, Vec<String>), Error> = timeout(
                self.redis_operation_timeout(),
                redis::cmd("SSCAN")
                    .arg(&index_key)
                    .arg(cursor)
                    .arg("COUNT")
                    .arg(100)
                    .query_async(conn),
            )
            .await
            .map_err(|_| Error::Timeout("Redis SSCAN timed out".to_string()))
            .and_then(|r| r.map_err(|e| Error::Database(format!("Redis SSCAN failed: {e}"))));
            self.record_operation_result(&op_result);
            let scan_result = op_result?;

            cursor = scan_result.0;
            node_ids.extend(scan_result.1);

            if cursor == 0 {
                break;
            }
        }

        Ok(node_ids)
    }

    async fn prune_node_index_members(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        node_ids: &[String],
    ) {
        if node_ids.is_empty() {
            return;
        }

        let index_key = self.node_index_key();
        let mut pipe = redis::pipe();
        for node_id in node_ids {
            pipe.cmd("SREM").arg(&index_key).arg(node_id).ignore();
        }

        let op_result: std::result::Result<(), Error> =
            timeout(self.redis_operation_timeout(), pipe.query_async(conn))
                .await
                .map_err(|_| Error::Timeout("Redis node index cleanup timed out".to_string()))
                .and_then(|r| {
                    r.map_err(|e| Error::Database(format!("Redis node index cleanup failed: {e}")))
                });
        self.record_operation_result(&op_result);

        match op_result {
            Ok(()) => {
                tracing::debug!(
                    removed_members = node_ids.len(),
                    "Pruned stale node IDs from Redis node index"
                );
            }
            Err(error) => {
                tracing::warn!(
                    removed_members = node_ids.len(),
                    error = %error,
                    "Failed to prune stale node IDs from Redis node index"
                );
            }
        }
    }

    /// Get or create a cached multiplexed Redis connection with periodic health checks.
    ///
    /// `MultiplexedConnection` handles concurrent requests internally and
    /// reconnects automatically, so we reuse a single instance.
    /// Every 30 seconds, we PING the connection to detect stale connections early.
    ///
    /// Returns an error in local-only mode (no Redis client configured).
    async fn get_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        let Some(runtime) = &self.redis_runtime else {
            return Err(Error::Database(
                "Redis not configured (local-only mode)".to_string(),
            ));
        };

        let mut guard = self.cached_conn.lock().await;

        // Check if we need to verify connection health
        let now = unix_epoch_elapsed().as_secs();
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
            self.redis_operation_timeout(),
            runtime.multiplexed_connection(),
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

        let Some(circuit_breaker) = &self.circuit_breaker else {
            return Err(Error::Database(
                "Circuit breaker not configured (local-only mode)".to_string(),
            ));
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
            if let Some(runtime) = self.redis_runtime.clone() {
                self.maybe_start_health_probe(runtime);
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
    fn maybe_start_health_probe(&self, runtime: Arc<dyn RedisCoordinationRuntime>) {
        let Some(circuit_breaker) = &self.circuit_breaker else {
            return;
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
                        let mut conn = runtime.multiplexed_connection().await?;
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
                if is_sentinel_failover_error(&error_str) {
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
    pub async fn register(&self, api_address: String) -> Result<()> {
        // In local-only mode, just update local cache
        if self.local_only {
            let local_epoch = self.current_epoch.load(Ordering::SeqCst);
            let new_epoch = local_epoch + 1;
            self.current_epoch.store(new_epoch, Ordering::SeqCst);

            let mut node_info = NodeInfo::new(self.node_id.clone(), api_address);
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
            self.invalidate_node_view_cache().await;

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
        let index_key = self.node_index_key();
        let local_epoch = self.current_epoch.load(Ordering::SeqCst);
        let ttl = self.heartbeat_timeout_secs * 2;

        // Create node info template
        let mut node_info = NodeInfo::new(self.node_id.clone(), api_address);
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

        let now_rfc3339 = Utc::now().to_rfc3339();
        let op_result: std::result::Result<u64, Error> = timeout(
            self.redis_operation_timeout(),
            REGISTER_NODE_SCRIPT
                .key(&key)
                .key(&index_key)
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
        self.invalidate_node_view_cache().await;

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
            let index_key = self.node_index_key();
            let current_epoch = self.current_epoch.load(Ordering::SeqCst);
            let now = Utc::now();
            let now_rfc3339 = now.to_rfc3339();
            let ttl = self.heartbeat_timeout_secs * 2;

            // Build updated node info from local cache
            let (node_json, api_addr) = {
                let nodes = self.local_nodes.read().await;
                let info_opt = nodes.get(&self.node_id).cloned();
                drop(nodes);

                let mut info = match info_opt {
                    Some(existing) if !existing.api_address.is_empty() => existing,
                    _ => {
                        // Local cache is missing or has an empty API address (should not
                        // happen after a successful register()). Log a warning so
                        // operators know the heartbeat is running with degraded data.
                        tracing::warn!(
                            node_id = %self.node_id,
                            "Heartbeat: local node cache missing or has an empty api_address, \
                             auto-re-registration may use an empty api_address"
                        );
                        info_opt
                            .unwrap_or_else(|| NodeInfo::new(self.node_id.clone(), String::new()))
                    }
                };
                let api_address = info.api_address.clone();
                info.last_heartbeat = now;
                info.epoch = current_epoch;
                let json = serde_json::to_string(&info).map_err(|e| {
                    Error::Serialization(format!("Failed to serialize node info: {e}"))
                })?;
                (json, api_address)
            };

            // Atomic Lua script: check epoch matches before writing heartbeat
            // Returns:
            //   -1 if key doesn't exist (need re-registration)
            //   -(1000 + remote_epoch) if epoch mismatch (encodes remote epoch)
            //   current_epoch on success
            // We use -(1000 + remote_epoch) instead of -remote_epoch to avoid
            // ambiguity when remote_epoch == 0 (which would return 0, colliding
            // with a successful epoch-0 result).
            let op_result: std::result::Result<i64, Error> = timeout(
                self.redis_operation_timeout(),
                HEARTBEAT_NODE_SCRIPT
                    .key(&key)
                    .key(&index_key)
                    .arg(current_epoch)
                    .arg(&node_json)
                    .arg(ttl)
                    .arg(&now_rfc3339)
                    .arg(&self.node_id)
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
                // Check for empty api_address before attempting re-registration
                if api_addr.is_empty() {
                    tracing::error!(
                        node_id = %self.node_id,
                        api_address = %api_addr,
                        "Heartbeat auto-re-registration skipped: empty api_address; \
                         node will be unreachable by peers until it is recovered"
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
                if let Err(e) = self.register(api_addr).await {
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
                let remote_epoch = ((-result) - 1000).cast_unsigned();
                // Check for empty api_address before attempting re-registration
                if api_addr.is_empty() {
                    tracing::error!(
                        node_id = %self.node_id,
                        api_address = %api_addr,
                        "Heartbeat auto-re-registration skipped: empty api_address; \
                         node will be unreachable by peers until it is recovered"
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
                if let Err(e) = self.register(api_addr).await {
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
        self.invalidate_node_view_cache().await;

        // Reset backoff on successful heartbeat
        self.reset_reregister_backoff();

        Ok(HeartbeatResult::Ok)
    }

    /// Unregister this node with fencing token validation
    ///
    /// Uses an atomic Lua script to check epoch <= `local_epoch` before deleting.
    /// Prevents stale nodes from unregistering newer registrations.
    pub async fn unregister(&self) -> Result<()> {
        if self.local_only {
            let mut nodes = self.local_nodes.write().await;
            nodes.remove(&self.node_id);
            self.invalidate_node_view_cache().await;
            return Ok(());
        }

        {
            let mut conn = self.get_conn_with_breaker().await?;

            let key = self.node_key(&self.node_id);
            let index_key = self.node_index_key();
            let current_epoch = self.current_epoch.load(Ordering::SeqCst);

            let op_result: std::result::Result<i64, Error> = timeout(
                self.redis_operation_timeout(),
                UNREGISTER_NODE_SCRIPT
                    .key(&key)
                    .key(&index_key)
                    .arg(current_epoch)
                    .arg(&self.node_id)
                    .invoke_async(&mut conn),
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
        self.invalidate_node_view_cache().await;

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
            let index_key = self.node_index_key();
            let value = serde_json::to_string(&node_info)
                .map_err(|e| Error::Serialization(format!("Failed to serialize node info: {e}")))?;
            let ttl = self.heartbeat_timeout_secs * 2;

            let op_result: std::result::Result<i64, Error> = timeout(
                self.redis_operation_timeout(),
                REGISTER_REMOTE_NODE_SCRIPT
                    .key(&key)
                    .key(&index_key)
                    .arg(&value)
                    .arg(ttl)
                    .arg(node_info.epoch)
                    .arg(&node_info.node_id)
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
        self.invalidate_node_view_cache().await;

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
            let index_key = self.node_index_key();

            // Use epoch validation if provided, otherwise just delete
            if let Some(epoch) = expected_epoch {
                let op_result: std::result::Result<i64, Error> = timeout(
                    self.redis_operation_timeout(),
                    UNREGISTER_NODE_SCRIPT
                        .key(&key)
                        .key(&index_key)
                        .arg(epoch)
                        .arg(node_id)
                        .invoke_async(&mut conn),
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
                return Err(Error::Configuration(format!(
                    "expected_epoch is required to unregister remote node '{node_id}' safely"
                )));
            }
        }

        let mut nodes = self.local_nodes.write().await;
        nodes.remove(node_id);
        self.invalidate_node_view_cache().await;

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
                self.last_refreshed
                    .store(unix_time_secs_u64(), Ordering::Relaxed);
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
            let node_ids = self.fetch_indexed_node_ids(&mut conn).await?;
            let keys: Vec<String> = node_ids
                .iter()
                .map(|node_id| self.node_key(node_id))
                .collect();

            let mut nodes = Vec::new();
            if !keys.is_empty() {
                let mut cmd = redis::cmd("MGET");
                for key in &keys {
                    cmd.arg(key);
                }
                let mget_result: std::result::Result<Vec<Option<String>>, Error> =
                    timeout(self.redis_operation_timeout(), cmd.query_async(&mut conn))
                        .await
                        .map_err(|_| Error::Timeout("Redis MGET timed out".to_string()))
                        .and_then(|r| {
                            r.map_err(|e| Error::Database(format!("Redis MGET failed: {e}")))
                        });
                self.record_operation_result(&mget_result);
                let values = mget_result?;

                let mut stale_index_members = Vec::new();
                for (node_id, value) in node_ids.into_iter().zip(values) {
                    match value {
                        Some(value) => match serde_json::from_str::<NodeInfo>(&value) {
                            Ok(node_info) if !node_info.is_stale(self.heartbeat_timeout_secs) => {
                                nodes.push(node_info);
                            }
                            _ => stale_index_members.push(node_id),
                        },
                        None => stale_index_members.push(node_id),
                    }
                }

                if !stale_index_members.is_empty() {
                    self.prune_node_index_members(&mut conn, &stale_index_members)
                        .await;
                }
            }

            if keys.is_empty() {
                let mut local_nodes = self.local_nodes.write().await;
                local_nodes.retain(|_, info| !info.is_stale(self.heartbeat_timeout_secs));
                return Ok(self.merge_verified_discovery_nodes(nodes, &local_nodes));
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

            Ok(self.merge_verified_discovery_nodes(nodes, &local_nodes))
        }
    }

    /// Get a specific node by ID
    pub async fn get_node(&self, node_id: &str) -> Result<Option<NodeInfo>> {
        let mut conn = self.get_conn_with_breaker().await?;

        let key = self.node_key(node_id);
        let op_result: std::result::Result<Option<String>, Error> = timeout(
            self.redis_operation_timeout(),
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

    /// Remove a transient discovery-only node from the local cache.
    ///
    /// This only removes the entry when it is still marked with the expected
    /// discovery source, preventing DNS disappearance from evicting a real node
    /// record that has since been refreshed from Redis.
    pub async fn remove_discovered_local_node(
        &self,
        node_id: &str,
        discovery_source: NodeDiscoverySource,
    ) -> bool {
        let mut nodes = self.local_nodes.write().await;
        let should_remove = nodes
            .get(node_id)
            .is_some_and(|node| node.has_discovery_source(discovery_source));

        if should_remove {
            nodes.remove(node_id);
            return true;
        }

        false
    }

    /// Upsert a transient discovery-only node into the local cache.
    ///
    /// Entries from the same discovery source are replaced so refreshed peer
    /// metadata becomes visible immediately. Entries owned by another source are
    /// left untouched to avoid transient discovery data clobbering Redis-backed
    /// state.
    pub async fn upsert_discovered_local_node(
        &self,
        node_info: NodeInfo,
        discovery_source: NodeDiscoverySource,
    ) {
        let mut nodes = self.local_nodes.write().await;
        match nodes.get_mut(&node_info.node_id) {
            Some(existing) if existing.has_discovery_source(discovery_source) => {
                *existing = node_info;
            }
            Some(_) => {}
            None => {
                nodes.insert(node_info.node_id.clone(), node_info);
            }
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
        let now = unix_time_secs_u64();
        now.saturating_sub(last) > NODES_STALE_THRESHOLD_SECS
    }

    /// Returns the Unix timestamp (seconds) of the last successful refresh
    /// from Redis. Returns 0 if no refresh has occurred yet.
    #[must_use]
    pub fn last_refreshed_at(&self) -> u64 {
        self.last_refreshed.load(Ordering::Relaxed)
    }

    /// Return the node set that is safe to use for routing decisions.
    ///
    /// This differs from [`get_all_nodes`] by refusing to silently serve stale
    /// local cache data once the cache has exceeded the staleness budget, and by
    /// excluding transient DNS-only peers that have not been confirmed by Redis.
    pub async fn get_routable_nodes(&self) -> Result<(Vec<NodeInfo>, NodeViewMode)> {
        if self.local_only {
            return Ok((self.get_all_nodes_local().await, NodeViewMode::LocalOnly));
        }

        match self.get_all_nodes().await {
            Ok(nodes) if self.cluster_mode() == ClusterMode::Normal => {
                Ok((self.filter_routable_nodes(nodes), NodeViewMode::Fresh))
            }
            Ok(nodes) => {
                if self.is_nodes_stale() {
                    Err(Error::NotFound(
                        "Cluster node view is stale while Redis is degraded; refusing to route on stale topology"
                            .to_string(),
                    ))
                } else {
                    Ok((
                        self.filter_routable_nodes(nodes),
                        NodeViewMode::DegradedCache,
                    ))
                }
            }
            Err(err) => Err(err),
        }
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

    /// Test-only hook to override the cluster mode.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_set_cluster_mode(&self, mode: ClusterMode) {
        *self.cluster_mode.write() = mode;
    }

    /// Test-only hook to override the last refresh timestamp.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_set_last_refreshed_at(&self, unix_secs: u64) {
        self.last_refreshed.store(unix_secs, Ordering::Relaxed);
    }

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
        let now_ms = unix_time_millis_u64();

        let elapsed_ms = now_ms.saturating_sub(last_attempt);
        elapsed_ms < backoff_ms
    }

    /// Apply exponential backoff after a failed re-registration.
    ///
    /// Updates the last attempt timestamp and increases the backoff duration
    /// by the multiplier (2x), up to the maximum.
    fn apply_reregister_backoff(&self) {
        let now_ms = unix_time_millis_u64();

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
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_in_reregister_backoff(&self) -> bool {
        self.is_in_reregister_backoff_sync()
    }

    /// Get the current re-registration backoff duration.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn current_reregister_backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.reregister_backoff_ms.load(Ordering::Relaxed))
    }

    /// Get the timestamp of the last re-registration attempt (for tests).
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn last_reregister_attempt(&self) -> u64 {
        self.last_reregister_attempt.load(Ordering::Relaxed)
    }

    /// Set a specific backoff duration for testing purposes.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_reregister_backoff_for_test(&self, duration: std::time::Duration) {
        self.reregister_backoff_ms
            .store(duration_millis_u64(duration), Ordering::Relaxed);
        // Set last attempt to now so the backoff is active
        let now_ms = unix_time_millis_u64();
        self.last_reregister_attempt
            .store(now_ms, Ordering::Relaxed);
    }
}

#[async_trait]
impl ClusterNodeDirectory for NodeRegistry {
    async fn register(&self, api_address: String) -> Result<()> {
        Self::register(self, api_address).await
    }

    async fn heartbeat(&self) -> Result<HeartbeatResult> {
        Self::heartbeat(self).await
    }

    async fn unregister(&self) -> Result<()> {
        Self::unregister(self).await
    }

    async fn register_remote(&self, node_info: NodeInfo) -> Result<()> {
        Self::register_remote(self, node_info).await
    }

    async fn unregister_remote(&self, node_id: &str, expected_epoch: Option<u64>) -> Result<()> {
        Self::unregister_remote(self, node_id, expected_epoch).await
    }

    async fn get_all_nodes(&self) -> Result<Vec<NodeInfo>> {
        Self::get_all_nodes(self).await
    }

    async fn get_routable_nodes(&self) -> Result<(Vec<NodeInfo>, NodeViewMode)> {
        Self::get_routable_nodes(self).await
    }

    async fn update_local_metadata(&self, key: &str, value: String) {
        Self::update_local_metadata(self, key, value).await;
    }

    async fn upsert_discovered_local_node(
        &self,
        node_info: NodeInfo,
        discovery_source: NodeDiscoverySource,
    ) {
        Self::upsert_discovered_local_node(self, node_info, discovery_source).await;
    }

    async fn remove_discovered_local_node(
        &self,
        node_id: &str,
        discovery_source: NodeDiscoverySource,
    ) -> bool {
        Self::remove_discovered_local_node(self, node_id, discovery_source).await
    }

    fn heartbeat_timeout_secs(&self) -> i64 {
        self.heartbeat_timeout_secs
    }

    fn cluster_mode(&self) -> ClusterMode {
        Self::cluster_mode(self)
    }

    fn cancel_token(&self) -> CancellationToken {
        Self::cancel_token(self)
    }

    fn is_nodes_stale(&self) -> bool {
        Self::is_nodes_stale(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_registry(node_id: &str) -> Result<NodeRegistry> {
        let client = redis::Client::open("redis://127.0.0.1:1")
            .map_err(|error| crate::Error::Redis(error.to_string()))?;
        NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(client),
            node_id.to_string(),
            30,
            "synctv:",
        )
    }

    #[test]
    fn test_node_info_stale() {
        let mut node = NodeInfo::new("test".to_string(), "localhost:8080".to_string());

        assert!(!node.is_stale(30));

        node.last_heartbeat = Utc::now() - Duration::seconds(60);
        assert!(node.is_stale(30));
    }

    #[test]
    fn test_node_info_epoch_initialization() {
        let node = NodeInfo::new("test".to_string(), "localhost:8080".to_string());

        // New nodes should start with epoch 1
        assert_eq!(node.epoch, 1);
    }

    #[test]
    fn test_node_info_with_epoch() {
        let node = NodeInfo::new("test".to_string(), "localhost:8080".to_string()).with_epoch(5);

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
        let node =
            NodeInfo::new("test_node".to_string(), "localhost:8080".to_string()).with_epoch(10);

        let token = node.fencing_token();
        assert_eq!(token.node_id, "test_node");
        assert_eq!(token.epoch, 10);
    }

    #[test]
    fn test_node_registry_creation_and_fencing_token() -> Result<()> {
        let registry = make_registry("test_node")?;

        let token = registry.current_fencing_token();
        assert_eq!(token.node_id, "test_node");
        assert_eq!(token.epoch, 1);
        Ok(())
    }

    #[test]
    fn test_fencing_token_serialization() -> Result<()> {
        let token = FencingToken::new("node1".to_string(), 42);

        let json = serde_json::to_string(&token)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        assert!(json.contains("node1"));
        assert!(json.contains("42"));

        let deserialized: FencingToken =
            serde_json::from_str(&json).map_err(|error| Error::Serialization(error.to_string()))?;
        assert_eq!(deserialized.node_id, "node1");
        assert_eq!(deserialized.epoch, 42);
        Ok(())
    }

    #[test]
    fn test_node_info_serialization_with_epoch() -> Result<()> {
        let node = NodeInfo::new("test".to_string(), "localhost:8080".to_string()).with_epoch(7);

        let json = serde_json::to_string(&node)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        assert!(json.contains("\"epoch\":7"));

        let deserialized: NodeInfo =
            serde_json::from_str(&json).map_err(|error| Error::Serialization(error.to_string()))?;
        assert_eq!(deserialized.epoch, 7);
        Ok(())
    }

    #[tokio::test]
    async fn test_merge_dns_peers_inserts_new() -> Result<()> {
        let registry = make_registry("self")?;

        let peer = NodeInfo::new("dns-peer-1".to_string(), "10.0.0.2:8080".to_string());

        registry.merge_dns_peers(vec![peer]).await;

        let nodes = registry.local_nodes.read().await;
        assert!(nodes.contains_key("dns-peer-1"));
        assert_eq!(nodes["dns-peer-1"].api_address, "10.0.0.2:8080");
        Ok(())
    }

    #[test]
    fn test_redis_operation_timeout_uses_runtime_budget() -> Result<()> {
        let client = redis::Client::open("redis://127.0.0.1:1")
            .map_err(|error| crate::Error::Redis(error.to_string()))?;
        let registry = NodeRegistry::new(
            synctv_core::coordination_runtime_from_client_with_config_and_operation_timeout(
                client,
                redis::aio::ConnectionManagerConfig::new(),
                std::time::Duration::from_secs(17),
            ),
            "self".to_string(),
            30,
            "synctv:",
        )?;

        assert_eq!(
            registry.redis_operation_timeout(),
            std::time::Duration::from_secs(17)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_merge_dns_peers_does_not_overwrite_existing() -> Result<()> {
        let registry = make_registry("self")?;

        {
            let mut nodes = registry.local_nodes.write().await;
            nodes.insert(
                "self".to_string(),
                NodeInfo::new("self".to_string(), "10.0.0.1:8080".to_string()),
            );
        }

        let dns_peer = NodeInfo::new("self".to_string(), "10.0.0.99:8080".to_string());

        registry.merge_dns_peers(vec![dns_peer]).await;

        let nodes = registry.local_nodes.read().await;
        assert_eq!(nodes["self"].api_address, "10.0.0.1:8080");
        Ok(())
    }

    #[tokio::test]
    async fn test_merge_verified_discovery_nodes_includes_k8s_dns_peers_missing_from_redis(
    ) -> Result<()> {
        let registry = NodeRegistry::new_local_only("self".to_string(), 30, "synctv:")?;

        let redis_nodes = vec![NodeInfo::new(
            "redis-peer-1".to_string(),
            "10.0.0.10:8080".to_string(),
        )];

        let mut local_nodes = HashMap::new();
        let mut dns_peer = NodeInfo::new("dns-peer-1".to_string(), "10.0.0.2:8080".to_string());
        dns_peer.set_discovery_source(NodeDiscoverySource::K8sDns);
        local_nodes.insert(dns_peer.node_id.clone(), dns_peer);

        local_nodes.insert(
            "other-discovery".to_string(),
            NodeInfo::new("other-discovery".to_string(), "10.0.0.3:8080".to_string()),
        );

        let nodes = registry.merge_verified_discovery_nodes(redis_nodes, &local_nodes);
        assert!(
            nodes.iter().any(|node| node.node_id == "dns-peer-1"),
            "verified k8s DNS peers should remain visible in the normal node view"
        );
        assert!(
            nodes.iter().any(|node| node.node_id == "redis-peer-1"),
            "Redis-backed nodes should remain present after the merge"
        );
        assert!(
            nodes.iter().all(|node| node.node_id != "other-discovery"),
            "only verified k8s DNS peers should supplement the Redis-backed node view"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_routable_nodes_excludes_k8s_dns_only_candidates() -> Result<()> {
        let registry = make_registry("self")?;

        let redis_peer = NodeInfo::new("redis-peer-1".to_string(), "10.0.0.10:8080".to_string());
        let mut dns_peer = NodeInfo::new("dns-peer-1".to_string(), "10.0.0.2:8080".to_string());
        dns_peer.set_discovery_source(NodeDiscoverySource::K8sDns);

        registry
            .nodes_cache
            .insert((), vec![redis_peer.clone(), dns_peer])
            .await;

        let (nodes, mode) = registry.get_routable_nodes().await?;

        assert_eq!(mode, NodeViewMode::Fresh);
        assert_eq!(
            nodes.len(),
            1,
            "transient DNS-only peers must not be treated as routable members"
        );
        assert_eq!(nodes[0].node_id, redis_peer.node_id);
        Ok(())
    }

    #[test]
    fn test_heartbeat_result_variants() {
        // Test that HeartbeatResult variants exist and can be matched
        let ok = HeartbeatResult::Ok;
        let need_rereg = HeartbeatResult::NeedReregistration;
        let epoch_mismatch = HeartbeatResult::EpochMismatch(42);
        let empty_addr = HeartbeatResult::EmptyAddress;

        assert!(matches!(ok, HeartbeatResult::Ok));
        assert!(matches!(need_rereg, HeartbeatResult::NeedReregistration));
        assert!(matches!(epoch_mismatch, HeartbeatResult::EpochMismatch(42)));
        assert!(matches!(empty_addr, HeartbeatResult::EmptyAddress));
    }

    #[tokio::test]
    async fn test_node_info_empty_address_detection() {
        let node_with_empty_api = NodeInfo::new("test".to_string(), String::new());
        assert!(node_with_empty_api.api_address.is_empty());

        let node_with_valid_api = NodeInfo::new("test".to_string(), "localhost:8080".to_string());
        assert!(!node_with_valid_api.api_address.is_empty());
    }

    #[tokio::test]
    async fn test_local_cache_empty_address_scenario() -> Result<()> {
        let registry = make_registry("test_node")?;

        {
            let mut nodes = registry.local_nodes.write().await;
            nodes.insert(
                "test_node".to_string(),
                NodeInfo::new("test_node".to_string(), String::new()),
            );
        }

        {
            let nodes = registry.local_nodes.read().await;
            let info = nodes
                .get("test_node")
                .ok_or_else(|| Error::NotFound("test_node".to_string()))?;
            assert!(info.api_address.is_empty());
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_local_cache_missing_scenario() -> Result<()> {
        let registry = make_registry("test_node")?;

        {
            let nodes = registry.local_nodes.read().await;
            assert!(!nodes.contains_key("test_node"));
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn test_unregister_remote_without_expected_epoch_does_not_remove_newer_registration(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (redis_container, redis_url) =
            synctv_core_testing::start_redis_url_with_label("node-registry-unregister-remote")
                .await;
        let redis_client = redis::Client::open(redis_url.as_str())?;

        let registry = NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            "self-node".to_string(),
            30,
            "cl-unregister:",
        )?;

        let original =
            NodeInfo::new("peer-node".to_string(), "10.0.0.1:8080".to_string()).with_epoch(3);
        registry.register_remote(original.clone()).await?;

        let newer =
            NodeInfo::new("peer-node".to_string(), "10.0.0.2:8080".to_string()).with_epoch(9);
        registry.register_remote(newer.clone()).await?;

        let err = registry
            .unregister_remote("peer-node", None)
            .await
            .expect_err("missing epoch must fail closed");
        assert!(
            err.to_string().contains("expected_epoch is required"),
            "unexpected error: {err}"
        );

        let nodes = registry.get_all_nodes_uncached().await?;
        let persisted = nodes
            .into_iter()
            .find(|node| node.node_id == "peer-node")
            .ok_or("newer remote registration must remain present")?;
        assert_eq!(persisted.epoch, 9);
        assert_eq!(persisted.api_address, "10.0.0.2:8080");

        drop(redis_container);
        Ok(())
    }
}
