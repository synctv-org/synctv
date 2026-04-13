//! Cluster gRPC client for fan-out queries across nodes
//!
//! Provides parallel fan-out queries to all cluster nodes for:
//! - User online status (`GetUserOnlineStatus`)
//! - Room connections (`GetRoomConnections`)
//!
//! Features:
//! - Per-node connection caching (reuses `tonic::Channel`)
//! - Configurable per-node timeout
//! - Partial failure tolerance (returns results from successful nodes)
//! - Shared-secret authentication via `x-cluster-secret` header

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use moka::sync::Cache;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, warn};

/// Aggregate timeout for fan-out operations across all nodes.
/// Individual per-node timeouts protect against single slow nodes,
/// but this aggregate timeout prevents the entire fan-out from hanging
/// when multiple nodes are slow or unreachable simultaneously.
const FAN_OUT_AGGREGATE_TIMEOUT: Duration = Duration::from_secs(5);

use super::circuit_breaker::GrpcCircuitBreakerRegistry;
use super::synctv::cluster::cluster_service_client::ClusterServiceClient;
use super::synctv::cluster::{
    GetRoomConnectionsRequest, GetRoomConnectionsResponse, GetUserOnlineStatusRequest,
    GetUserOnlineStatusResponse, RoomConnection, UserOnlineStatus,
};
use crate::discovery::ClusterNodeDirectory;
use crate::error::{Error, Result};

/// Configuration for the cluster fan-out client
#[derive(Debug, Clone)]
pub struct ClusterClientConfig {
    /// Timeout for individual node RPCs
    pub per_node_timeout: Duration,
    /// Timeout for establishing a new connection to a node
    pub connect_timeout: Duration,
    /// Shared secret for cluster authentication
    pub cluster_secret: String,
    /// This node's ID (excluded from fan-out queries since we query locally)
    pub self_node_id: String,
}

impl Default for ClusterClientConfig {
    fn default() -> Self {
        Self {
            per_node_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(2),
            cluster_secret: String::new(),
            self_node_id: String::new(),
        }
    }
}

/// Result of a fan-out query, containing merged results and error information
#[derive(Debug)]
pub struct FanOutResult<T> {
    /// Merged results from all successful nodes
    pub data: T,
    /// Number of nodes that responded successfully
    pub nodes_succeeded: usize,
    /// Number of nodes that failed (timeout, network error, etc.)
    pub nodes_failed: usize,
    /// Node IDs that failed, with error descriptions
    pub failures: Vec<(String, String)>,
}

impl<T> FanOutResult<T> {
    /// Whether all queried nodes responded successfully
    pub const fn is_complete(&self) -> bool {
        self.nodes_failed == 0
    }

    /// Total number of nodes queried
    pub const fn total_nodes(&self) -> usize {
        self.nodes_succeeded + self.nodes_failed
    }
}

/// TTL for cached gRPC channels (5 minutes).
/// Channels to nodes that are no longer in the registry will be
/// automatically evicted after this duration of inactivity.
const CHANNEL_CACHE_TTL_SECS: u64 = 300;

/// Maximum number of cached gRPC channels.
const CHANNEL_CACHE_MAX_CAPACITY: u64 = 256;

/// Cluster gRPC client for fan-out queries
///
/// Queries all known cluster nodes in parallel and merges their responses.
/// Skips the local node (identified by `self_node_id`) since local data
/// should be queried directly via `ConnectionManager`.
pub struct ClusterClient {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    config: ClusterClientConfig,
    /// Cached gRPC channels keyed by `"{node_id}|{address}"`.
    ///
    /// Using `(node_id, address)` as the composite key ensures that when a node
    /// restarts and re-registers with a different gRPC address, the old channel
    /// (keyed under the old address) is not reused for the new address. Without
    /// this, a node restart could cause RPCs to be sent to the old (dead) address
    /// until the cache TTL expires.
    ///
    /// Entries are automatically evicted after `CHANNEL_CACHE_TTL_SECS` of
    /// inactivity (no get/insert), preventing unbounded growth from stale nodes.
    channels: Cache<String, Channel>,
    /// Circuit breaker registry for endpoint health tracking
    circuit_breakers: Arc<GrpcCircuitBreakerRegistry>,
}

impl ClusterClient {
    /// Create a new cluster client
    pub fn new<N>(node_registry: Arc<N>, config: ClusterClientConfig) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry, config)
    }

    pub fn from_runtime(
        node_registry: Arc<dyn ClusterNodeDirectory>,
        config: ClusterClientConfig,
    ) -> Self {
        let channels = Cache::builder()
            .max_capacity(CHANNEL_CACHE_MAX_CAPACITY)
            .time_to_idle(Duration::from_secs(CHANNEL_CACHE_TTL_SECS))
            .build();

        Self {
            node_registry,
            config,
            channels,
            circuit_breakers: Arc::new(GrpcCircuitBreakerRegistry::new()),
        }
    }

    /// Build the composite cache key for a `(node_id, address)` pair.
    fn channel_cache_key(node_id: &str, address: &str) -> String {
        format!("{node_id}|{address}")
    }

    /// Get or create a cached gRPC channel for a node.
    ///
    /// The cache key is `"{node_id}|{address}"` so that when a node restarts
    /// with a new address, the stale channel for the old address is not reused.
    ///
    /// Channels are cached with a TTL; stale entries are automatically evicted
    /// by the moka cache after `CHANNEL_CACHE_TTL_SECS` of inactivity.
    async fn get_channel(&self, node_id: &str, address: &str) -> Result<Channel> {
        let cache_key = Self::channel_cache_key(node_id, address);

        // Return cached channel if available
        if let Some(channel) = self.channels.get(&cache_key) {
            return Ok(channel);
        }

        // Create new channel
        let uri = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };

        let endpoint = Endpoint::from_shared(uri)
            .map_err(|e| Error::Rpc(format!("Invalid endpoint URI for {address}: {e}")))?
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.per_node_timeout);

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| Error::Rpc(format!("Failed to connect to {address}: {e}")))?;

        self.channels.insert(cache_key, channel.clone());
        Ok(channel)
    }

    /// Create an authenticated client for a given channel
    fn make_client(channel: Channel) -> ClusterServiceClient<Channel> {
        ClusterServiceClient::new(channel)
    }

    /// Attach the shared secret to a tonic request.
    ///
    /// Returns `Err` if the cluster secret contains invalid (non-ASCII) characters,
    /// which would cause the request to be sent without authentication.
    fn attach_secret<T>(&self, request: &mut tonic::Request<T>) -> Result<()> {
        if !self.config.cluster_secret.is_empty() {
            match self
                .config
                .cluster_secret
                .parse::<MetadataValue<tonic::metadata::Ascii>>()
            {
                Ok(val) => {
                    request.metadata_mut().insert("x-cluster-secret", val);
                }
                Err(e) => {
                    tracing::error!(
                        "cluster_secret contains invalid characters (non-ASCII?): {}",
                        e
                    );
                    return Err(Error::Rpc(
                        "invalid cluster secret configuration".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Remove a cached channel for the given node.
    ///
    /// Call this when a node re-registers with a new gRPC address or when
    /// `node_deregistered`/`node_updated` events are received from the cluster.
    /// Without explicit invalidation, the moka TTL (`CHANNEL_CACHE_TTL_SECS` = 5 minutes)
    /// means the old channel would continue to be used for up to 5 minutes after
    /// the node re-registers with a different address.
    ///
    /// Also called internally after any RPC failure so that the next call
    /// creates a fresh channel.
    pub fn invalidate_channel(&self, node_id: &str, address: &str) {
        let cache_key = Self::channel_cache_key(node_id, address);
        self.channels.invalidate(&cache_key);
        debug!(node_id = %node_id, address = %address, "Invalidated cached gRPC channel");
    }

    /// Invalidate all cached channels.
    ///
    /// Use this when the cluster topology changes significantly (e.g., after a
    /// leader election, or when multiple nodes re-register simultaneously).
    pub fn invalidate_all_channels(&self) {
        self.channels.invalidate_all();
        debug!("Invalidated all cached gRPC channels");
    }

    /// Generic fan-out to all remote nodes in parallel.
    ///
    /// Queries all remote nodes concurrently, collecting results from nodes that
    /// respond before the aggregate timeout. Skips unhealthy nodes when the
    /// cluster is in degraded mode (>50% circuit breakers open).
    ///
    /// Also opportunistically prunes circuit breakers for nodes no longer in
    /// the registry (ISSUE 4: prevents unbounded HashMap growth).
    ///
    /// # Type Parameters
    /// - `T`: Per-node RPC response type
    /// - `Item`: Individual result item extracted from each response
    /// - `QueryFn`: Async function that queries a single node: `(node_id, address) -> Result<T>`
    /// - `ExtractFn`: Function that extracts items from a successful response: `T -> Vec<Item>`
    async fn fan_out<T, Item, QueryFn, QueryFut, ExtractFn>(
        &self,
        rpc_name: &str,
        query_fn: QueryFn,
        extract_fn: ExtractFn,
    ) -> Result<FanOutResult<Vec<Item>>>
    where
        Item: Send + 'static,
        T: Send + 'static,
        QueryFn: Fn(String, String) -> QueryFut,
        QueryFut: std::future::Future<Output = Result<T>> + Send,
        ExtractFn: Fn(T) -> Vec<Item>,
    {
        let (nodes, _view_mode) = self.node_registry.get_routable_nodes().await?;

        // Opportunistically prune circuit breakers for nodes no longer in the registry
        let active_addresses: HashSet<String> =
            nodes.iter().map(|n| n.api_address.clone()).collect();
        self.circuit_breakers.retain_only(&active_addresses).await;

        // Filter out self
        let remote_nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.node_id != self.config.self_node_id)
            .collect();

        if remote_nodes.is_empty() {
            return Ok(FanOutResult {
                data: Vec::new(),
                nodes_succeeded: 0,
                nodes_failed: 0,
                failures: Vec::new(),
            });
        }

        // In degraded mode (>50% circuit breakers open), only query healthy nodes
        // to return partial results quickly instead of waiting for timeouts.
        let mut skipped_nodes = Vec::new();
        let query_nodes: Vec<_> = if self.circuit_breakers.is_cluster_degraded().await {
            let mut known_open_addresses = HashSet::new();
            for node in &remote_nodes {
                if self
                    .circuit_breakers
                    .is_endpoint_open_known(&node.api_address)
                    .await
                {
                    known_open_addresses.insert(node.api_address.clone());
                }
            }
            let (queryable, skipped) =
                partition_degraded_query_nodes(&remote_nodes, &known_open_addresses);
            skipped_nodes = skipped;
            if !skipped_nodes.is_empty() {
                warn!(
                    skipped = skipped_nodes.len(),
                    healthy = queryable.len(),
                    rpc = %rpc_name,
                    "Cluster degraded: skipping unhealthy nodes for fan-out"
                );
            }
            queryable
        } else {
            remote_nodes.clone()
        };

        // Fan out to all remote nodes in parallel using FuturesUnordered
        // so that on aggregate timeout we can collect already-completed results
        // instead of discarding everything.
        let mut futs: FuturesUnordered<_> = query_nodes
            .iter()
            .map(|node| {
                let address = node.api_address.clone();
                let node_id = node.node_id.clone();
                let fut = query_fn(node_id.clone(), address.clone());
                async move { (node_id, address, fut.await) }
            })
            .collect();

        let mut all_items: Vec<Item> = Vec::new();
        let mut nodes_succeeded = 0usize;
        let mut nodes_failed = skipped_nodes.len();
        let mut failures: Vec<(String, String)> = skipped_nodes
            .iter()
            .map(|node| {
                (
                    node.node_id.clone(),
                    format!(
                        "skipped in degraded mode: circuit breaker open for {}",
                        node.api_address
                    ),
                )
            })
            .collect();
        let mut pending_nodes: HashMap<String, String> = query_nodes
            .iter()
            .map(|node| (node.node_id.clone(), node.api_address.clone()))
            .collect();

        let deadline = tokio::time::Instant::now() + FAN_OUT_AGGREGATE_TIMEOUT;
        let mut timed_out = false;

        while !futs.is_empty() {
            tokio::select! {
                biased;
                maybe_result = futs.next() => {
                    if let Some((node_id, address, result)) = maybe_result {
                        pending_nodes.remove(&node_id);
                        match result {
                            Ok(response) => {
                                nodes_succeeded += 1;
                                all_items.extend(extract_fn(response));
                            }
                            Err(e) => {
                                nodes_failed += 1;
                                warn!(
                                    node_id = %node_id,
                                    address = %address,
                                    error = %e,
                                    rpc = %rpc_name,
                                    "Fan-out failed for node"
                                );
                                self.invalidate_channel(&node_id, &address);
                                failures.push((node_id, e.to_string()));
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    timed_out = true;
                    break;
                }
            }
        }

        if timed_out {
            let remaining = futs.len();
            nodes_failed += remaining;
            failures.extend(pending_nodes.into_iter().map(|(node_id, address)| {
                (
                    node_id,
                    format!(
                        "aggregate timeout after {FAN_OUT_AGGREGATE_TIMEOUT:?} while waiting for {address}"
                    ),
                )
            }));
            warn!(
                remaining_nodes = remaining,
                collected_succeeded = nodes_succeeded,
                rpc = %rpc_name,
                "Fan-out aggregate timeout ({:?}), returning partial results",
                FAN_OUT_AGGREGATE_TIMEOUT,
            );
        }

        debug!(
            succeeded = nodes_succeeded,
            failed = nodes_failed,
            total_items = all_items.len(),
            rpc = %rpc_name,
            "Fan-out complete"
        );

        Ok(FanOutResult {
            data: all_items,
            nodes_succeeded,
            nodes_failed,
            failures,
        })
    }

    /// Fan-out `GetUserOnlineStatus` to all remote nodes in parallel.
    ///
    /// Returns merged `UserOnlineStatus` entries from all responding nodes.
    /// A user is considered online if ANY node reports them as online.
    pub async fn fan_out_user_online_status(
        &self,
        user_ids: Vec<String>,
    ) -> Result<FanOutResult<Vec<UserOnlineStatus>>> {
        self.fan_out(
            "GetUserOnlineStatus",
            |node_id, address| {
                let user_ids = user_ids.clone();
                async move {
                    self.query_user_status_single(&node_id, &address, user_ids)
                        .await
                }
            },
            |response: GetUserOnlineStatusResponse| response.statuses,
        )
        .await
    }

    /// Query a single node for user online status
    async fn query_user_status_single(
        &self,
        node_id: &str,
        address: &str,
        user_ids: Vec<String>,
    ) -> Result<GetUserOnlineStatusResponse> {
        // Check circuit breaker before attempting call
        if !self.circuit_breakers.is_call_permitted(address).await {
            return Err(Error::Rpc(format!(
                "GetUserOnlineStatus rejected: circuit breaker open for {address}"
            )));
        }

        let channel = self.get_channel(node_id, address).await?;
        let mut client = Self::make_client(channel);

        let mut request = tonic::Request::new(GetUserOnlineStatusRequest { user_ids });
        self.attach_secret(&mut request)?;

        let result = client.get_user_online_status(request).await;

        match result {
            Ok(response) => {
                self.circuit_breakers.on_success(address).await;
                Ok(response.into_inner())
            }
            Err(e) => {
                self.circuit_breakers.on_error(address).await;
                Err(Error::Rpc(format!(
                    "GetUserOnlineStatus RPC failed for {address}: {e}"
                )))
            }
        }
    }

    /// Fan-out `GetRoomConnections` to all remote nodes in parallel.
    ///
    /// Returns merged `RoomConnection` entries from all responding nodes,
    /// giving a cluster-wide view of who is connected to a room.
    pub async fn fan_out_room_connections(
        &self,
        room_id: String,
    ) -> Result<FanOutResult<Vec<RoomConnection>>> {
        self.fan_out(
            "GetRoomConnections",
            |node_id, address| {
                let room_id = room_id.clone();
                async move {
                    self.query_room_connections_single(&node_id, &address, room_id)
                        .await
                }
            },
            |response: GetRoomConnectionsResponse| response.connections,
        )
        .await
    }

    /// Query a single node for room connections
    async fn query_room_connections_single(
        &self,
        node_id: &str,
        address: &str,
        room_id: String,
    ) -> Result<GetRoomConnectionsResponse> {
        // Check circuit breaker before attempting call
        if !self.circuit_breakers.is_call_permitted(address).await {
            return Err(Error::Rpc(format!(
                "GetRoomConnections rejected: circuit breaker open for {address}"
            )));
        }

        let channel = self.get_channel(node_id, address).await?;
        let mut client = Self::make_client(channel);

        let mut request = tonic::Request::new(GetRoomConnectionsRequest { room_id });
        self.attach_secret(&mut request)?;

        let result = client.get_room_connections(request).await;

        match result {
            Ok(response) => {
                self.circuit_breakers.on_success(address).await;
                Ok(response.into_inner())
            }
            Err(e) => {
                self.circuit_breakers.on_error(address).await;
                Err(Error::Rpc(format!(
                    "GetRoomConnections RPC failed for {address}: {e}"
                )))
            }
        }
    }

    /// Merge user online statuses from multiple nodes into a deduplicated view.
    ///
    /// If the same user appears on multiple nodes, their statuses are merged:
    /// - `is_online` is true if online on ANY node
    /// - `room_ids` are combined from all nodes
    /// - `node_id` becomes a comma-separated list of all nodes
    pub fn merge_user_statuses(statuses: Vec<UserOnlineStatus>) -> Vec<UserOnlineStatus> {
        let mut by_user: HashMap<String, UserOnlineStatus> = HashMap::new();

        for status in statuses {
            by_user
                .entry(status.user_id.clone())
                .and_modify(|existing| {
                    existing.is_online = existing.is_online || status.is_online;
                    // Merge room_ids, avoiding duplicates
                    for room_id in &status.room_ids {
                        if !existing.room_ids.contains(room_id) {
                            existing.room_ids.push(room_id.clone());
                        }
                    }
                    // Append node_id
                    if !existing.node_id.contains(&status.node_id) {
                        existing.node_id = format!("{},{}", existing.node_id, status.node_id);
                    }
                })
                .or_insert(status);
        }

        by_user.into_values().collect()
    }
}

fn partition_degraded_query_nodes(
    remote_nodes: &[crate::discovery::NodeInfo],
    known_open_addresses: &HashSet<String>,
) -> (
    Vec<crate::discovery::NodeInfo>,
    Vec<crate::discovery::NodeInfo>,
) {
    let mut queryable = Vec::new();
    let mut skipped_nodes = Vec::new();

    for node in remote_nodes {
        if known_open_addresses.contains(&node.api_address) {
            skipped_nodes.push(node.clone());
        } else {
            queryable.push(node.clone());
        }
    }

    (queryable, skipped_nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeRegistry;

    #[test]
    fn test_fan_out_result_is_complete() {
        let result: FanOutResult<Vec<()>> = FanOutResult {
            data: Vec::new(),
            nodes_succeeded: 3,
            nodes_failed: 0,
            failures: Vec::new(),
        };
        assert!(result.is_complete());
        assert_eq!(result.total_nodes(), 3);
    }

    #[test]
    fn test_fan_out_result_partial_failure() {
        let result: FanOutResult<Vec<()>> = FanOutResult {
            data: Vec::new(),
            nodes_succeeded: 2,
            nodes_failed: 1,
            failures: vec![("node3".to_string(), "timeout".to_string())],
        };
        assert!(!result.is_complete());
        assert_eq!(result.total_nodes(), 3);
    }

    #[test]
    fn test_fan_out_result_failure_details_cover_timeout_nodes() {
        let result: FanOutResult<Vec<()>> = FanOutResult {
            data: Vec::new(),
            nodes_succeeded: 1,
            nodes_failed: 2,
            failures: vec![
                ("node-b".to_string(), "aggregate timeout".to_string()),
                ("node-c".to_string(), "aggregate timeout".to_string()),
            ],
        };

        assert_eq!(result.failures.len(), result.nodes_failed);
    }

    #[test]
    fn test_merge_user_statuses_single_node() {
        let statuses = vec![UserOnlineStatus {
            user_id: "user1".to_string(),
            is_online: true,
            room_ids: vec!["room1".to_string()],
            node_id: "node1".to_string(),
        }];

        let merged = ClusterClient::merge_user_statuses(statuses);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].is_online);
        assert_eq!(merged[0].room_ids, vec!["room1".to_string()]);
        assert_eq!(merged[0].node_id, "node1");
    }

    #[test]
    fn test_merge_user_statuses_multi_node() {
        let statuses = vec![
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: true,
                room_ids: vec!["room1".to_string()],
                node_id: "node1".to_string(),
            },
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: true,
                room_ids: vec!["room2".to_string()],
                node_id: "node2".to_string(),
            },
            UserOnlineStatus {
                user_id: "user2".to_string(),
                is_online: false,
                room_ids: Vec::new(),
                node_id: "node1".to_string(),
            },
        ];

        let merged = ClusterClient::merge_user_statuses(statuses);
        assert_eq!(merged.len(), 2);

        let user1 = merged.iter().find(|s| s.user_id == "user1").unwrap();
        assert!(user1.is_online);
        assert_eq!(user1.room_ids.len(), 2);
        assert!(user1.room_ids.contains(&"room1".to_string()));
        assert!(user1.room_ids.contains(&"room2".to_string()));
        assert!(user1.node_id.contains("node1"));
        assert!(user1.node_id.contains("node2"));

        let user2 = merged.iter().find(|s| s.user_id == "user2").unwrap();
        assert!(!user2.is_online);
    }

    #[test]
    fn test_merge_user_statuses_dedup_rooms() {
        let statuses = vec![
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: true,
                room_ids: vec!["room1".to_string(), "room2".to_string()],
                node_id: "node1".to_string(),
            },
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: true,
                room_ids: vec!["room2".to_string(), "room3".to_string()],
                node_id: "node2".to_string(),
            },
        ];

        let merged = ClusterClient::merge_user_statuses(statuses);
        assert_eq!(merged.len(), 1);
        let user1 = &merged[0];
        assert_eq!(user1.room_ids.len(), 3);
        assert!(user1.room_ids.contains(&"room1".to_string()));
        assert!(user1.room_ids.contains(&"room2".to_string()));
        assert!(user1.room_ids.contains(&"room3".to_string()));
    }

    #[test]
    fn test_merge_user_statuses_any_online_wins() {
        let statuses = vec![
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: false,
                room_ids: Vec::new(),
                node_id: "node1".to_string(),
            },
            UserOnlineStatus {
                user_id: "user1".to_string(),
                is_online: true,
                room_ids: vec!["room1".to_string()],
                node_id: "node2".to_string(),
            },
        ];

        let merged = ClusterClient::merge_user_statuses(statuses);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].is_online);
    }

    #[test]
    fn test_merge_user_statuses_empty() {
        let merged = ClusterClient::merge_user_statuses(Vec::new());
        assert!(merged.is_empty());
    }

    #[test]
    fn test_partition_degraded_query_nodes_keeps_unknown_endpoints_queryable() {
        let remote_nodes = vec![
            crate::discovery::NodeInfo::new("node-open".to_string(), "10.0.0.1:8080".to_string()),
            crate::discovery::NodeInfo::new(
                "node-unknown".to_string(),
                "10.0.0.2:8080".to_string(),
            ),
        ];
        let known_open_addresses = HashSet::from(["10.0.0.1:8080".to_string()]);

        let (queryable, skipped) =
            partition_degraded_query_nodes(&remote_nodes, &known_open_addresses);

        assert_eq!(skipped.len(), 1);
        assert_eq!(queryable.len(), 1);
        assert_eq!(queryable[0].node_id, "node-unknown");
        assert_eq!(skipped[0].node_id, "node-open");
    }

    #[test]
    fn test_fan_out_result_failures_cover_skipped_nodes_in_degraded_mode() {
        let result: FanOutResult<Vec<()>> = FanOutResult {
            data: Vec::new(),
            nodes_succeeded: 0,
            nodes_failed: 1,
            failures: vec![(
                "node-open".to_string(),
                "skipped in degraded mode: circuit breaker open for 10.0.0.1:8080".to_string(),
            )],
        };

        assert_eq!(
            result.failures.len(),
            result.nodes_failed,
            "every degraded-mode skipped node must have an explicit failure reason"
        );
        assert!(
            result.failures[0].1.contains("skipped in degraded mode"),
            "operators need to distinguish deliberate degraded-mode skips from RPC failures"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn test_cluster_client_no_remote_nodes() {
        use testcontainers::core::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        /// Default Redis version for test containers
        const REDIS_VERSION: &str = "8";

        let redis_container = Redis::default()
            .with_tag(REDIS_VERSION)
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        let redis_url = format!("redis://{redis_host}:{redis_port}");
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        // Verify Redis is reachable with retry logic
        // The container may report ready but TCP might not be fully established yet
        let mut conn = {
            let mut retries = 0;
            loop {
                match redis_client.get_multiplexed_async_connection().await {
                    Ok(conn) => break conn,
                    Err(_) if retries < 10 => {
                        retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
                }
            }
        };
        let _: () = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .expect("Redis PING failed");
        drop(conn);

        let registry = Arc::new(
            NodeRegistry::new(
                synctv_core::coordination_runtime_from_client(redis_client),
                "self_node".to_string(),
                30,
                "synctv:",
            )
            .unwrap(),
        );
        // Populate local cache directly (no Redis connection needed)
        {
            let mut nodes = registry.local_nodes.write().await;
            nodes.insert(
                "self_node".to_string(),
                crate::discovery::NodeInfo::new(
                    "self_node".to_string(),
                    "localhost:8080".to_string(),
                ),
            );
        }

        let config = ClusterClientConfig {
            self_node_id: "self_node".to_string(),
            ..Default::default()
        };
        let client = ClusterClient::new(registry, config);

        // Fan-out should return empty results since there are no remote nodes
        let result = client
            .fan_out_user_online_status(vec!["user1".to_string()])
            .await
            .unwrap();

        assert!(result.data.is_empty());
        assert_eq!(result.nodes_succeeded, 0);
        assert_eq!(result.nodes_failed, 0);
        assert!(result.is_complete());

        let result = client
            .fan_out_room_connections("room1".to_string())
            .await
            .unwrap();

        assert!(result.data.is_empty());
        assert_eq!(result.nodes_succeeded, 0);
        assert_eq!(result.nodes_failed, 0);
        assert!(result.is_complete());

        // Explicitly drop the container at the end of the test
        drop(redis_container);
    }
}
