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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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
use crate::discovery::NodeRegistry;
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
    pub fn is_complete(&self) -> bool {
        self.nodes_failed == 0
    }

    /// Total number of nodes queried
    pub fn total_nodes(&self) -> usize {
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
    node_registry: Arc<NodeRegistry>,
    config: ClusterClientConfig,
    /// Cached gRPC channels keyed by node gRPC address.
    /// Entries are automatically evicted after `CHANNEL_CACHE_TTL_SECS` of
    /// inactivity (no get/insert), preventing unbounded growth from stale nodes.
    channels: Cache<String, Channel>,
    /// Circuit breaker registry for endpoint health tracking
    circuit_breakers: Arc<GrpcCircuitBreakerRegistry>,
}

impl ClusterClient {
    /// Create a new cluster client
    pub fn new(node_registry: Arc<NodeRegistry>, config: ClusterClientConfig) -> Self {
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

    /// Get or create a cached gRPC channel for a node address.
    ///
    /// Channels are cached with a TTL; stale entries are automatically evicted
    /// by the moka cache after `CHANNEL_CACHE_TTL_SECS` of inactivity.
    async fn get_channel(&self, address: &str) -> Result<Channel> {
        // Return cached channel if available
        if let Some(channel) = self.channels.get(address) {
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

        self.channels.insert(address.to_string(), channel.clone());
        Ok(channel)
    }

    /// Create an authenticated client for a given channel
    fn make_client(
        &self,
        channel: Channel,
    ) -> ClusterServiceClient<Channel> {
        ClusterServiceClient::new(channel)
    }

    /// Attach the shared secret to a tonic request
    fn attach_secret<T>(&self, request: &mut tonic::Request<T>) {
        if !self.config.cluster_secret.is_empty() {
            if let Ok(val) = self.config.cluster_secret.parse::<MetadataValue<_>>() {
                request.metadata_mut().insert("x-cluster-secret", val);
            }
        }
    }

    /// Remove a cached channel (e.g., after connection failure)
    fn invalidate_channel(&self, address: &str) {
        self.channels.invalidate(address);
    }

    /// Fan-out `GetUserOnlineStatus` to all remote nodes in parallel.
    ///
    /// Returns merged `UserOnlineStatus` entries from all responding nodes.
    /// A user is considered online if ANY node reports them as online.
    pub async fn fan_out_user_online_status(
        &self,
        user_ids: Vec<String>,
    ) -> Result<FanOutResult<Vec<UserOnlineStatus>>> {
        let nodes = self.node_registry.get_all_nodes().await?;

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
        let mut skipped_nodes = 0usize;
        let query_nodes: Vec<_> = if self.circuit_breakers.is_cluster_degraded().await {
            let healthy = self.circuit_breakers.healthy_endpoints().await;
            let (queryable, skipped): (Vec<_>, Vec<_>) = remote_nodes
                .iter()
                .partition(|n| healthy.contains(&n.grpc_address));
            skipped_nodes = skipped.len();
            if skipped_nodes > 0 {
                warn!(
                    skipped = skipped_nodes,
                    healthy = queryable.len(),
                    "Cluster degraded: skipping unhealthy nodes for GetUserOnlineStatus fan-out"
                );
            }
            queryable.into_iter().cloned().collect()
        } else {
            remote_nodes.clone()
        };

        // Fan out to all remote nodes in parallel
        let futures: Vec<_> = query_nodes
            .iter()
            .map(|node| {
                let user_ids = user_ids.clone();
                let address = node.grpc_address.clone();
                let node_id = node.node_id.clone();
                async move {
                    let result = self.query_user_status_single(&address, user_ids).await;
                    (node_id, address, result)
                }
            })
            .collect();

        let results = match tokio::time::timeout(
            FAN_OUT_AGGREGATE_TIMEOUT,
            futures::future::join_all(futures),
        ).await {
            Ok(results) => results,
            Err(_) => {
                warn!(
                    node_count = query_nodes.len(),
                    "Fan-out GetUserOnlineStatus aggregate timeout ({:?}), returning empty partial results",
                    FAN_OUT_AGGREGATE_TIMEOUT,
                );
                return Ok(FanOutResult {
                    data: Vec::new(),
                    nodes_succeeded: 0,
                    nodes_failed: query_nodes.len() + skipped_nodes,
                    failures: query_nodes.iter().map(|n| (n.node_id.clone(), "aggregate timeout".to_string())).collect(),
                });
            }
        };

        // Merge results
        let mut all_statuses: Vec<UserOnlineStatus> = Vec::new();
        let mut nodes_succeeded = 0usize;
        let mut nodes_failed = skipped_nodes;
        let mut failures = Vec::new();

        for (node_id, address, result) in results {
            match result {
                Ok(response) => {
                    nodes_succeeded += 1;
                    all_statuses.extend(response.statuses);
                }
                Err(e) => {
                    nodes_failed += 1;
                    warn!(
                        node_id = %node_id,
                        address = %address,
                        error = %e,
                        "Fan-out GetUserOnlineStatus failed for node"
                    );
                    self.invalidate_channel(&address);
                    failures.push((node_id, e.to_string()));
                }
            }
        }

        debug!(
            succeeded = nodes_succeeded,
            failed = nodes_failed,
            total_statuses = all_statuses.len(),
            "Fan-out GetUserOnlineStatus complete"
        );

        Ok(FanOutResult {
            data: all_statuses,
            nodes_succeeded,
            nodes_failed,
            failures,
        })
    }

    /// Query a single node for user online status
    async fn query_user_status_single(
        &self,
        address: &str,
        user_ids: Vec<String>,
    ) -> Result<GetUserOnlineStatusResponse> {
        // Check circuit breaker before attempting call
        if !self.circuit_breakers.is_call_permitted(address).await {
            return Err(Error::Rpc(format!(
                "GetUserOnlineStatus rejected: circuit breaker open for {address}"
            )));
        }

        let channel = self.get_channel(address).await?;
        let mut client = self.make_client(channel);

        let mut request = tonic::Request::new(GetUserOnlineStatusRequest { user_ids });
        self.attach_secret(&mut request);

        // Timeout is already set at the Endpoint level (see get_channel),
        // so no additional tokio::time::timeout wrapper is needed.
        let result = client.get_user_online_status(request).await;

        match result {
            Ok(response) => {
                self.circuit_breakers.on_success(address).await;
                Ok(response.into_inner())
            }
            Err(e) => {
                self.circuit_breakers.on_error(address).await;
                Err(Error::Rpc(format!("GetUserOnlineStatus RPC failed for {address}: {e}")))
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
        let nodes = self.node_registry.get_all_nodes().await?;

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
        let mut skipped_nodes = 0usize;
        let query_nodes: Vec<_> = if self.circuit_breakers.is_cluster_degraded().await {
            let healthy = self.circuit_breakers.healthy_endpoints().await;
            let (queryable, skipped): (Vec<_>, Vec<_>) = remote_nodes
                .iter()
                .partition(|n| healthy.contains(&n.grpc_address));
            skipped_nodes = skipped.len();
            if skipped_nodes > 0 {
                warn!(
                    skipped = skipped_nodes,
                    healthy = queryable.len(),
                    "Cluster degraded: skipping unhealthy nodes for GetRoomConnections fan-out"
                );
            }
            queryable.into_iter().cloned().collect()
        } else {
            remote_nodes.clone()
        };

        // Fan out to all remote nodes in parallel
        let futures: Vec<_> = query_nodes
            .iter()
            .map(|node| {
                let room_id = room_id.clone();
                let address = node.grpc_address.clone();
                let node_id = node.node_id.clone();
                async move {
                    let result = self.query_room_connections_single(&address, room_id).await;
                    (node_id, address, result)
                }
            })
            .collect();

        let results = match tokio::time::timeout(
            FAN_OUT_AGGREGATE_TIMEOUT,
            futures::future::join_all(futures),
        ).await {
            Ok(results) => results,
            Err(_) => {
                warn!(
                    node_count = query_nodes.len(),
                    "Fan-out GetRoomConnections aggregate timeout ({:?}), returning empty partial results",
                    FAN_OUT_AGGREGATE_TIMEOUT,
                );
                return Ok(FanOutResult {
                    data: Vec::new(),
                    nodes_succeeded: 0,
                    nodes_failed: query_nodes.len() + skipped_nodes,
                    failures: query_nodes.iter().map(|n| (n.node_id.clone(), "aggregate timeout".to_string())).collect(),
                });
            }
        };

        // Merge results
        let mut all_connections: Vec<RoomConnection> = Vec::new();
        let mut nodes_succeeded = 0usize;
        let mut nodes_failed = skipped_nodes;
        let mut failures = Vec::new();

        for (node_id, address, result) in results {
            match result {
                Ok(response) => {
                    nodes_succeeded += 1;
                    all_connections.extend(response.connections);
                }
                Err(e) => {
                    nodes_failed += 1;
                    warn!(
                        node_id = %node_id,
                        address = %address,
                        error = %e,
                        "Fan-out GetRoomConnections failed for node"
                    );
                    self.invalidate_channel(&address);
                    failures.push((node_id, e.to_string()));
                }
            }
        }

        debug!(
            succeeded = nodes_succeeded,
            failed = nodes_failed,
            total_connections = all_connections.len(),
            "Fan-out GetRoomConnections complete"
        );

        Ok(FanOutResult {
            data: all_connections,
            nodes_succeeded,
            nodes_failed,
            failures,
        })
    }

    /// Query a single node for room connections
    async fn query_room_connections_single(
        &self,
        address: &str,
        room_id: String,
    ) -> Result<GetRoomConnectionsResponse> {
        // Check circuit breaker before attempting call
        if !self.circuit_breakers.is_call_permitted(address).await {
            return Err(Error::Rpc(format!(
                "GetRoomConnections rejected: circuit breaker open for {address}"
            )));
        }

        let channel = self.get_channel(address).await?;
        let mut client = self.make_client(channel);

        let mut request = tonic::Request::new(GetRoomConnectionsRequest { room_id });
        self.attach_secret(&mut request);

        // Timeout is already set at the Endpoint level (see get_channel),
        // so no additional tokio::time::timeout wrapper is needed.
        let result = client.get_room_connections(request).await;

        match result {
            Ok(response) => {
                self.circuit_breakers.on_success(address).await;
                Ok(response.into_inner())
            }
            Err(e) => {
                self.circuit_breakers.on_error(address).await;
                Err(Error::Rpc(format!("GetRoomConnections RPC failed for {address}: {e}")))
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn test_cluster_client_no_remote_nodes() {
        // Create a local-only node registry (no Redis)
        let registry = Arc::new(
            NodeRegistry::new(None, "self_node".to_string(), 30, "synctv:").unwrap(),
        );
        // Register only ourselves
        registry
            .register("localhost:50051".to_string(), "localhost:8080".to_string())
            .await
            .unwrap();

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
    }
}
