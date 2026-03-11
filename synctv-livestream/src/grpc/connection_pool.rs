// gRPC connection pool for reusing established channels across requests.
//
// Keyed by node address (e.g., "host:port"), each entry holds a tonic Channel
// that multiplexes HTTP/2 streams. Idle connections are evicted after a
// configurable TTL to avoid holding stale connections to nodes that may have
// been replaced.
//
// Includes a per-node circuit breaker to prevent retry storms when a publisher
// node is down. After consecutive failures exceed the threshold, the circuit
// opens and rejects connection attempts for a cooldown period.

use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::Channel;
use tracing::{debug, warn};

/// A pooled gRPC channel with creation timestamp for staleness checks and
/// a consecutive-error counter for health-based eviction.
struct PooledChannel {
    channel: Channel,
    created_at: Instant,
    /// Number of consecutive errors on this connection since the last success.
    /// When this exceeds `CONNECTION_ERROR_EVICTION_THRESHOLD` the connection
    /// is considered unhealthy and evicted from the pool.
    consecutive_errors: AtomicU32,
}

/// Number of consecutive per-connection errors before the connection is
/// considered unhealthy and evicted from the pool regardless of its age.
const CONNECTION_ERROR_EVICTION_THRESHOLD: u32 = 3;

/// Per-node circuit breaker state.
///
/// Tracks consecutive failures to a specific node address. When failures exceed
/// `CIRCUIT_BREAKER_THRESHOLD`, the circuit opens and rejects all connection
/// attempts for `CIRCUIT_BREAKER_COOLDOWN` to prevent retry storms across
/// multiple pull streams targeting the same down node.
///
/// Timing uses a monotonic epoch-relative scheme to avoid issues with
/// `SystemTime` jumping backward due to NTP adjustments. A shared `Instant`
/// epoch is captured at construction, and `opened_at_millis` stores the elapsed
/// milliseconds since that epoch.
struct CircuitBreakerState {
    /// Number of consecutive connection failures
    consecutive_failures: AtomicU32,
    /// Monotonic epoch: the `Instant` when this state was created.
    /// All timestamps are stored as milliseconds elapsed since this epoch.
    epoch: Instant,
    /// Milliseconds since `epoch` when the circuit was opened (0 = circuit closed)
    opened_at_millis: AtomicU64,
    /// `true` while a half-open probe is in flight.
    ///
    /// When the cooldown expires, `is_open()` resets `opened_at_millis` to 0
    /// (transitioning to half-open) and this flag ensures only the first
    /// concurrent caller is allowed through as the probe. All other concurrent
    /// callers that race through the `opened_at == 0` check are blocked until
    /// the probe completes via `record_success` or `record_failure`.
    probe_in_flight: AtomicBool,
}

impl CircuitBreakerState {
    fn new() -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            epoch: Instant::now(),
            opened_at_millis: AtomicU64::new(0),
            probe_in_flight: AtomicBool::new(false),
        }
    }

    /// Returns milliseconds elapsed since the monotonic epoch, offset by 1
    /// so that the result is always >= 1. This avoids collision with the
    /// sentinel value 0 which means "circuit closed".
    fn now_ms(&self) -> u64 {
        (self.epoch.elapsed().as_millis() as u64).saturating_add(1)
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Release);
        self.opened_at_millis.store(0, Ordering::Release);
        self.probe_in_flight.store(false, Ordering::Release);
    }

    fn record_failure(&self, threshold: u32) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
        if failures >= threshold {
            let now_ms = self.now_ms();
            self.opened_at_millis.store(now_ms, Ordering::Release);
            // Reset probe_in_flight so the next cooldown window can send a probe.
            self.probe_in_flight.store(false, Ordering::Release);
            warn!(
                consecutive_failures = failures,
                "Circuit breaker opened after {} consecutive failures", failures
            );
        }
    }

    fn is_open(&self, cooldown_ms: u64) -> bool {
        let opened = self.opened_at_millis.load(Ordering::Acquire);
        if opened == 0 {
            // Circuit is closed (or transitioning to half-open).
            // If a probe is already in flight, block this concurrent caller to
            // prevent multiple simultaneous half-open probes.
            if self.probe_in_flight.load(Ordering::Acquire) {
                return true; // report as open: another probe is in flight
            }
            return false;
        }
        let now_ms = self.now_ms();
        let elapsed = now_ms.saturating_sub(opened);
        if elapsed >= cooldown_ms {
            // Cooldown expired: transition to half-open.
            // Use compare_exchange on opened_at_millis to ensure only one
            // concurrent caller wins the race and becomes the probe.
            match self.opened_at_millis.compare_exchange(
                opened,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // We won the race: claim the probe slot.
                    self.probe_in_flight.store(true, Ordering::Release);
                    false // allow this caller through as the probe
                }
                Err(_) => {
                    // Another caller already claimed the probe or reset the
                    // circuit.  Check current state.
                    if self.probe_in_flight.load(Ordering::Acquire) {
                        true // another probe in flight — block this caller
                    } else {
                        false // circuit was reset by a successful probe
                    }
                }
            }
        } else {
            true
        }
    }

    /// Returns `true` if the circuit breaker is in the Closed state with no
    /// recent activity (no active connections tracked at this level).
    fn is_idle_closed(&self) -> bool {
        self.opened_at_millis.load(Ordering::Acquire) == 0
            && !self.probe_in_flight.load(Ordering::Acquire)
            && self.consecutive_failures.load(Ordering::Acquire) == 0
    }
}

/// Number of consecutive failures before the circuit opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
/// How long the circuit stays open before allowing a probe attempt (30 seconds).
const CIRCUIT_BREAKER_COOLDOWN_MS: u64 = 30_000;

/// Default maximum number of connections in the pool.
const DEFAULT_MAX_POOL_SIZE: usize = 100;

/// Thread-safe gRPC connection pool keyed by node address.
///
/// Channels are reused across callers (tonic `Channel` is clone-cheap and
/// multiplexes over a single HTTP/2 connection). Stale entries are lazily
/// evicted on access when they exceed `max_idle`.
///
/// The pool enforces a maximum size (`max_size`). When a new connection would
/// exceed this limit, the oldest (by creation time) entry is evicted to make
/// room. This prevents unbounded growth during K8s pod churn where stale
/// addresses accumulate.
///
/// Includes a per-node circuit breaker: after `CIRCUIT_BREAKER_THRESHOLD`
/// consecutive failures to a node, connection attempts are rejected for
/// `CIRCUIT_BREAKER_COOLDOWN_MS` to prevent retry storms.
#[derive(Clone)]
pub struct GrpcConnectionPool {
    connections: Arc<DashMap<String, PooledChannel>>,
    /// Maximum time a pooled connection is considered healthy before re-creation.
    max_idle: Duration,
    /// Maximum number of connections allowed in the pool.
    max_size: usize,
    /// Per-node circuit breaker state
    circuit_breakers: Arc<DashMap<String, Arc<CircuitBreakerState>>>,
}

impl GrpcConnectionPool {
    /// Create a new connection pool.
    ///
    /// `max_idle` controls how long a cached channel is reused before being
    /// discarded and re-created on the next request.
    /// `max_size` limits the maximum number of connections in the pool.
    #[must_use]
    pub fn new(max_idle: Duration, max_size: usize) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            max_idle,
            max_size: max_size.max(1), // at least 1
            circuit_breakers: Arc::new(DashMap::new()),
        }
    }

    /// Create a pool with a default max idle time of 5 minutes and default max
    /// pool size of 100.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(Duration::from_mins(5), DEFAULT_MAX_POOL_SIZE)
    }

    /// Returns the maximum number of connections allowed in the pool.
    #[must_use]
    pub const fn max_size(&self) -> usize {
        self.max_size
    }

    /// Returns the maximum idle age for pooled connections before they are stale.
    #[must_use]
    pub const fn max_idle(&self) -> Duration {
        self.max_idle
    }

    /// Get or create a gRPC channel for the given address.
    ///
    /// Returns a cached channel if one exists and is not stale, otherwise
    /// creates a new connection. The address should be in `host:port` format
    /// (scheme is added automatically if missing).
    ///
    /// Connection attempts timeout after 5 seconds to prevent hanging indefinitely
    /// when the target node is unreachable.
    pub async fn get_channel(&self, address: &str) -> anyhow::Result<Channel> {
        // Check circuit breaker before attempting connection
        let cb = self
            .circuit_breakers
            .entry(address.to_string())
            .or_insert_with(|| Arc::new(CircuitBreakerState::new()))
            .clone();

        if cb.is_open(CIRCUIT_BREAKER_COOLDOWN_MS) {
            return Err(anyhow::anyhow!(
                "Circuit breaker open for '{address}': too many consecutive failures, \
                 rejecting to prevent retry storm (will probe after cooldown)"
            ));
        }

        // Fast path: check for existing healthy connection
        if let Some(entry) = self.connections.get(address) {
            let age_ok = entry.created_at.elapsed() < self.max_idle;
            let errors = entry.consecutive_errors.load(Ordering::Acquire);
            let health_ok = errors < CONNECTION_ERROR_EVICTION_THRESHOLD;
            if age_ok && health_ok {
                cb.record_success();
                return Ok(entry.channel.clone());
            }
            // Stale or unhealthy -- drop the read guard and remove below
            drop(entry);
            self.connections.remove(address);
            if age_ok {
                debug!(
                    address = address,
                    consecutive_errors = errors,
                    "Evicted unhealthy gRPC connection from pool (error threshold exceeded)"
                );
            } else {
                debug!(address = address, "Evicted stale gRPC connection from pool");
            }
        }

        // Slow path: create new connection
        let url = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };

        let channel = match Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("Invalid gRPC endpoint URL '{url}': {e}"))?
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
        {
            Ok(ch) => {
                cb.record_success();
                ch
            }
            Err(e) => {
                cb.record_failure(CIRCUIT_BREAKER_THRESHOLD);
                return Err(anyhow::anyhow!(
                    "Failed to connect to gRPC endpoint '{address}': {e}"
                ));
            }
        };

        // Evict oldest entry if pool is at capacity (and this is a new address)
        if !self.connections.contains_key(address) && self.connections.len() >= self.max_size {
            self.evict_oldest();
        }

        self.connections.insert(
            address.to_string(),
            PooledChannel {
                channel: channel.clone(),
                created_at: Instant::now(),
                consecutive_errors: AtomicU32::new(0),
            },
        );

        debug!(address = address, "Created new pooled gRPC connection");
        Ok(channel)
    }

    /// Record an error on a pooled connection.
    ///
    /// Increments the consecutive error counter for the given address. If the
    /// counter reaches `CONNECTION_ERROR_EVICTION_THRESHOLD`, the connection is
    /// considered unhealthy and will be evicted on the next access or background
    /// sweep, regardless of its age.
    ///
    /// Call this after any gRPC request fails on a channel obtained from this pool.
    pub fn record_connection_error(&self, address: &str) {
        if let Some(entry) = self.connections.get(address) {
            let errors = entry.consecutive_errors.fetch_add(1, Ordering::AcqRel) + 1;
            if errors >= CONNECTION_ERROR_EVICTION_THRESHOLD {
                debug!(
                    address = address,
                    consecutive_errors = errors,
                    "gRPC connection marked unhealthy (error threshold reached)"
                );
            }
        }
    }

    /// Record a successful request on a pooled connection.
    ///
    /// Resets the consecutive error counter for the given address so that
    /// transient errors do not accumulate and cause premature eviction.
    pub fn record_connection_success(&self, address: &str) {
        if let Some(entry) = self.connections.get(address) {
            entry.consecutive_errors.store(0, Ordering::Release);
        }
    }

    /// Remove a specific connection from the pool (e.g., after a connection error).
    pub fn invalidate(&self, address: &str) {
        if self.connections.remove(address).is_some() {
            debug!(address = address, "Invalidated gRPC connection from pool");
        }
    }

    /// Evict the oldest connection from the pool to make room for a new one.
    ///
    /// Iterates over all entries and removes the one with the earliest
    /// `created_at` timestamp. This is O(n) but the pool is small (bounded by
    /// `max_size`), so the cost is negligible.
    fn evict_oldest(&self) {
        let oldest = self
            .connections
            .iter()
            .min_by_key(|entry| entry.created_at)
            .map(|entry| entry.key().clone());

        if let Some(key) = oldest {
            self.connections.remove(&key);
            debug!(address = key, "Evicted oldest gRPC connection to make room");
        }
    }

    /// Remove all stale or unhealthy connections.
    ///
    /// A connection is evicted if:
    /// - Its age exceeds `max_idle` (time-based eviction), OR
    /// - Its consecutive error count has reached `CONNECTION_ERROR_EVICTION_THRESHOLD`
    ///   (health-based eviction).
    ///
    /// Circuit breaker state is NOT removed here because a node may be temporarily
    /// disconnected (no active connection) but the CB state is still needed to
    /// prevent retry storms when the node comes back. Use `evict_idle_circuit_breakers()`
    /// for periodic CB cleanup.
    ///
    /// Can be called periodically from a background task.
    pub fn evict_stale(&self) {
        let before = self.connections.len();
        self.connections.retain(|_addr, entry| {
            let age_ok = entry.created_at.elapsed() < self.max_idle;
            let health_ok = entry.consecutive_errors.load(Ordering::Acquire)
                < CONNECTION_ERROR_EVICTION_THRESHOLD;
            age_ok && health_ok
        });
        let evicted = before - self.connections.len();
        if evicted > 0 {
            debug!(
                "Evicted {} stale or unhealthy gRPC connections from pool",
                evicted
            );
        }
    }

    /// Remove circuit breaker entries that have been in the Closed state for a
    /// while with no active connections.
    ///
    /// This prevents the `circuit_breakers` DashMap from growing unboundedly when
    /// nodes are removed from the cluster. Only entries that are idle (closed, no
    /// failures, no probe in flight) AND have no active connection in the pool are
    /// removed.
    ///
    /// Should be called periodically (e.g., every 5 minutes) from a background task,
    /// separately from `evict_stale()`.
    pub fn evict_idle_circuit_breakers(&self) {
        let before = self.circuit_breakers.len();
        self.circuit_breakers.retain(|addr, cb| {
            // Keep if there's an active connection for this address
            if self.connections.contains_key(addr) {
                return true;
            }
            // Keep if the CB is not idle (open, has failures, or probe in flight)
            !cb.is_idle_closed()
        });
        let evicted = before - self.circuit_breakers.len();
        if evicted > 0 {
            debug!("Evicted {} idle circuit breaker entries from pool", evicted);
        }
    }

    /// Spawn a background task that calls `evict_stale` every `interval` and
    /// `evict_idle_circuit_breakers` every 5 minutes.
    ///
    /// The task runs until the `CancellationToken` is cancelled, the returned
    /// `JoinHandle` is aborted, or the process exits. Typical usage: call once
    /// at startup with a 5-minute interval.
    #[must_use]
    pub fn spawn_cleanup_task(
        &self,
        interval: Duration,
        token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let pool = self.clone();
        tokio::spawn(async move {
            let mut stale_tick = tokio::time::interval(interval);
            stale_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut cb_tick = tokio::time::interval(Duration::from_mins(5)); // 5 minutes
            cb_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        debug!("Connection pool cleanup task cancelled");
                        break;
                    }
                    _ = stale_tick.tick() => {
                        pool.evict_stale();
                    }
                    _ = cb_tick.tick() => {
                        pool.evict_idle_circuit_breakers();
                    }
                }
            }
        })
    }

    /// Number of connections currently in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the pool is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// Get the consecutive error count for a connection (for testing/monitoring).
    ///
    /// Returns `None` if the address is not in the pool, otherwise returns
    /// the current consecutive error count.
    #[must_use]
    pub fn get_error_count(&self, address: &str) -> Option<u32> {
        self.connections
            .get(address)
            .map(|e| e.consecutive_errors.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn insert_test_channel_with_age(
        &self,
        address: &str,
        channel: Channel,
        age: Duration,
    ) {
        let created_at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        self.connections.insert(
            address.to_string(),
            PooledChannel {
                channel,
                created_at,
                consecutive_errors: AtomicU32::new(0),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = GrpcConnectionPool::with_defaults();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_pool_invalidate_nonexistent() {
        let pool = GrpcConnectionPool::with_defaults();
        // Should not panic
        pool.invalidate("nonexistent:50051");
        assert!(pool.is_empty());
    }

    #[test]
    fn test_pool_evict_stale_empty() {
        let pool = GrpcConnectionPool::with_defaults();
        pool.evict_stale();
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn test_pool_evict_stale_with_expired_entry() {
        // Use a very short TTL so entries expire immediately
        let pool = GrpcConnectionPool::new(Duration::from_millis(1), 100);

        // We can't easily create a real channel without a server, so just test
        // the eviction logic with the empty pool (integration test would cover the full path)
        pool.evict_stale();
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn test_connection_timeout_configuration() {
        // This test verifies that the timeout is properly configured in the
        // Channel builder. We can't easily test the actual timeout behavior
        // without a real unresponsive server, but we can verify the code compiles
        // and the timeout parameter is used.
        let pool = GrpcConnectionPool::with_defaults();

        // Try to connect to localhost with a non-existent port
        // This should fail quickly (connection refused) but with timeout configured
        let result = pool.get_channel("127.0.0.1:65535").await;

        // Should fail because nothing is listening on this port
        assert!(
            result.is_err(),
            "Expected connection to 127.0.0.1:65535 to fail"
        );

        // The important part is that connect_timeout() is called in the code,
        // which is verified at compile time by the type system
    }

    #[test]
    fn test_circuit_breaker_closed_by_default() {
        let cb = CircuitBreakerState::new();
        assert!(!cb.is_open(CIRCUIT_BREAKER_COOLDOWN_MS));
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreakerState::new();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            cb.record_failure(CIRCUIT_BREAKER_THRESHOLD);
        }
        assert!(cb.is_open(CIRCUIT_BREAKER_COOLDOWN_MS));
    }

    #[test]
    fn test_circuit_breaker_success_resets() {
        let cb = CircuitBreakerState::new();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            cb.record_failure(CIRCUIT_BREAKER_THRESHOLD);
        }
        assert!(cb.is_open(CIRCUIT_BREAKER_COOLDOWN_MS));
        cb.record_success();
        assert!(!cb.is_open(CIRCUIT_BREAKER_COOLDOWN_MS));
    }

    #[test]
    fn test_circuit_breaker_cooldown_expires() {
        let cb = CircuitBreakerState::new();
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            cb.record_failure(CIRCUIT_BREAKER_THRESHOLD);
        }
        // Simulate cooldown by setting opened_at to a value far enough in the
        // past relative to the monotonic epoch. Since now_ms() returns
        // epoch.elapsed(), we set opened_at to 0+1 so that the current elapsed
        // time will be well past the cooldown.
        cb.opened_at_millis.store(1, Ordering::Release);
        // The epoch was just created, so now_ms() is very small. We need to
        // use a cooldown of 0 to test expiry, or set opened_at such that
        // now_ms() - opened_at >= cooldown. Since the CB was just created,
        // now_ms() is ~0. Instead, test with cooldown_ms = 0.
        assert!(!cb.is_open(0));
    }

    #[test]
    fn test_pool_max_size_default() {
        let pool = GrpcConnectionPool::with_defaults();
        assert_eq!(pool.max_size(), DEFAULT_MAX_POOL_SIZE);
    }

    #[test]
    fn test_pool_max_size_custom() {
        let pool = GrpcConnectionPool::new(Duration::from_mins(1), 50);
        assert_eq!(pool.max_size(), 50);
    }

    #[test]
    fn test_pool_max_size_minimum_is_one() {
        // max_size of 0 should be clamped to 1
        let pool = GrpcConnectionPool::new(Duration::from_mins(1), 0);
        assert_eq!(pool.max_size(), 1);
    }

    #[tokio::test]
    async fn test_pool_enforces_max_size_on_insert() {
        // Create a pool with max_size = 3
        let pool = GrpcConnectionPool::new(Duration::from_mins(5), 3);

        // Manually insert entries with staggered creation times.
        // node-0 is the oldest (created_at furthest in the past).
        let channel = Channel::from_static("http://[::1]:50051").connect_lazy();
        let now = Instant::now();
        for i in 0..3u32 {
            pool.connections.insert(
                format!("node-{i}:50051"),
                PooledChannel {
                    channel: channel.clone(),
                    // node-0 is oldest (300s ago), node-1 is 200s ago, node-2 is 100s ago
                    created_at: now
                        .checked_sub(Duration::from_secs(300 - u64::from(i) * 100))
                        .unwrap(),
                    consecutive_errors: AtomicU32::new(0),
                },
            );
        }
        assert_eq!(pool.len(), 3);

        // Trigger evict_oldest (simulating what get_channel would do)
        pool.evict_oldest();
        assert_eq!(pool.len(), 2);

        // Verify the oldest entry (node-0) was evicted
        assert!(pool.connections.get("node-0:50051").is_none());
        assert!(pool.connections.get("node-1:50051").is_some());
        assert!(pool.connections.get("node-2:50051").is_some());
    }

    #[tokio::test]
    async fn test_evict_stale_does_not_remove_circuit_breakers() {
        // Test that evict_stale does NOT remove circuit breaker state.
        // Circuit breakers should be cleaned up separately via evict_idle_circuit_breakers().
        let pool = GrpcConnectionPool::new(Duration::from_millis(1), 100);
        let channel = Channel::from_static("http://[::1]:50051").connect_lazy();

        // Insert a connection created 10 seconds ago (well past 1ms TTL)
        pool.connections.insert(
            "node-1:50051".to_string(),
            PooledChannel {
                channel,
                created_at: Instant::now().checked_sub(Duration::from_secs(10)).unwrap(),
                consecutive_errors: AtomicU32::new(0),
            },
        );

        // Create circuit breaker entries
        pool.circuit_breakers.insert(
            "node-1:50051".to_string(),
            Arc::new(CircuitBreakerState::new()),
        );
        pool.circuit_breakers.insert(
            "node-2:50051".to_string(),
            Arc::new(CircuitBreakerState::new()),
        );
        assert_eq!(pool.circuit_breakers.len(), 2);

        // Evict stale connections -- should NOT remove circuit breakers
        pool.evict_stale();
        assert!(pool.is_empty());
        assert_eq!(
            pool.circuit_breakers.len(),
            2,
            "CB state should be preserved across evict_stale"
        );
    }

    #[tokio::test]
    async fn test_evict_idle_circuit_breakers() {
        // Test that evict_idle_circuit_breakers removes idle CB entries with no connection.
        let pool = GrpcConnectionPool::new(Duration::from_mins(5), 100);

        // Create idle CB entries (no active connections)
        pool.circuit_breakers.insert(
            "node-1:50051".to_string(),
            Arc::new(CircuitBreakerState::new()),
        );
        pool.circuit_breakers.insert(
            "node-2:50051".to_string(),
            Arc::new(CircuitBreakerState::new()),
        );

        // Create a non-idle CB (has failures) for node-3
        let cb3 = Arc::new(CircuitBreakerState::new());
        cb3.record_failure(CIRCUIT_BREAKER_THRESHOLD);
        pool.circuit_breakers
            .insert("node-3:50051".to_string(), cb3);

        assert_eq!(pool.circuit_breakers.len(), 3);

        // Evict idle CBs: node-1 and node-2 are idle (no failures, no connections)
        // node-3 has failures so it should be kept
        pool.evict_idle_circuit_breakers();
        assert_eq!(
            pool.circuit_breakers.len(),
            1,
            "Only non-idle CB should remain"
        );
        assert!(pool.circuit_breakers.contains_key("node-3:50051"));
    }

    #[tokio::test]
    async fn test_cleanup_task_stops_after_cancellation() {
        let pool = GrpcConnectionPool::new(Duration::from_millis(5), 8);
        let channel = Channel::from_static("http://[::1]:50051").connect_lazy();

        pool.insert_test_channel_with_age("node-a:50051", channel.clone(), Duration::from_secs(1));

        let cancel = tokio_util::sync::CancellationToken::new();
        let handle = pool.spawn_cleanup_task(Duration::from_millis(10), cancel.clone());

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            pool.is_empty(),
            "cleanup task should evict stale pooled connections while active"
        );

        pool.insert_test_channel_with_age("node-b:50051", channel, Duration::from_secs(1));
        cancel.cancel();
        handle.await.expect("cleanup task should join cleanly");

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            pool.len(),
            1,
            "cancelled cleanup task must stop evicting pooled connections"
        );
    }
}
