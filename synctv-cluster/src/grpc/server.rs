//! Cluster gRPC server implementation
//!
//! Handles inter-node communication for cluster coordination.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use super::synctv::cluster::cluster_service_server::ClusterService;
use super::synctv::cluster::{
    GetNodesRequest, GetNodesResponse, GetRoomConnectionsRequest, GetRoomConnectionsResponse,
    GetSliceCacheStatsRequest, GetUserOnlineStatusRequest, GetUserOnlineStatusResponse, NodeInfo,
    PurgeSliceCacheRequest, PurgeSliceCacheResponse, RoomConnection, SliceCacheConfigInfo,
    SliceCacheStatsResponse, UserOnlineStatus,
};
use super::ClusterAuthInterceptor;
use crate::discovery::{ClusterNodeDirectory, NodeInfo as DiscoveryNodeInfo};
use crate::sync::ConnectionRuntime;
use synctv_core::models::{RoomId, UserId};

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Cluster gRPC service
///
/// Handles cluster discovery and state synchronization.
///
/// # Architecture Overview
///
/// Redis is the **sole discovery mechanism** for this cluster:
/// - Nodes self-register in Redis via `NodeRegistry::register()` on startup
/// - Periodic heartbeats are sent directly to Redis via `NodeRegistry::heartbeat()`
/// - Graceful deregistration uses `NodeRegistry::unregister()` with epoch validation
///
/// # Endpoint Usage Status
///
/// | Endpoint | Status | Notes |
/// |----------|--------|-------|
/// | `GetNodes` | ACTIVE | Returns all known nodes from Redis registry |
/// | `GetUserOnlineStatus` | ACTIVE | Fan-out query for user presence across nodes |
/// | `GetRoomConnections` | ACTIVE | Fan-out query for room participants across nodes |
#[derive(Clone)]
pub struct ClusterServer {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    connection_runtime: Option<Arc<dyn ConnectionRuntime>>,
    proxy_slice_cache: Option<Arc<synctv_proxy::slice_cache::SliceCache>>,
    node_id: String,
    auth: Option<ClusterAuthInterceptor>,
}

#[allow(clippy::result_large_err)] // tonic::Status is inherently large; required by gRPC API
impl ClusterServer {
    /// Create a new cluster server
    #[must_use]
    pub fn new<N>(node_registry: Arc<N>, node_id: String) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry, node_id)
    }

    #[must_use]
    pub fn from_runtime(node_registry: Arc<dyn ClusterNodeDirectory>, node_id: String) -> Self {
        Self {
            node_registry,
            connection_runtime: None,
            proxy_slice_cache: None,
            node_id,
            auth: None,
        }
    }

    /// Set the connection query runtime for user/room presence queries.
    #[must_use]
    pub fn with_connection_runtime(
        mut self,
        connection_runtime: Arc<dyn ConnectionRuntime>,
    ) -> Self {
        self.connection_runtime = Some(connection_runtime);
        self
    }

    /// Set the local slice cache runtime for cluster-level management queries.
    #[must_use]
    pub fn with_slice_cache_runtime(
        mut self,
        proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    ) -> Self {
        self.proxy_slice_cache = Some(proxy_slice_cache);
        self
    }

    /// Enable shared-secret authentication for cluster RPC handlers.
    ///
    /// Cluster RPCs are internal-only and must never be exposed without an
    /// application-layer shared secret. `ClusterServer::new()` defaults to
    /// fail-closed until a secret is provided here.
    #[must_use]
    pub fn with_cluster_secret(mut self, secret: String) -> Self {
        self.auth = Some(ClusterAuthInterceptor::new(secret));
        self
    }

    /// Maximum number of `user_ids` in a single request
    const MAX_USER_IDS: usize = 1000;

    /// Convert discovery `NodeInfo` to proto `NodeInfo`.
    fn discovery_to_proto_node(discovery: &DiscoveryNodeInfo) -> NodeInfo {
        NodeInfo {
            node_id: discovery.node_id.clone(),
            address: discovery.api_address.clone(),
            last_heartbeat: discovery.last_heartbeat.timestamp(),
            epoch: discovery.epoch,
        }
    }

    fn authorize<T>(&self, request: &Request<T>) -> std::result::Result<(), Status> {
        let Some(auth) = &self.auth else {
            tracing::error!(
                "ClusterServer called without configured shared-secret auth; refusing request"
            );
            return Err(Status::unauthenticated(
                "Cluster authentication secret is not configured",
            ));
        };

        auth.validate_metadata(request.metadata())
    }

    fn slice_cache_stats_response(&self) -> std::result::Result<SliceCacheStatsResponse, Status> {
        let cache = self.proxy_slice_cache.as_ref().ok_or_else(|| {
            Status::failed_precondition("Proxy slice cache runtime is unavailable")
        })?;
        let stats = cache.stats();
        Ok(SliceCacheStatsResponse {
            node_id: self.node_id.clone(),
            config: Some(SliceCacheConfigInfo {
                engine_enabled: stats.engine_enabled,
                backend: stats.backend,
                file_cache_dir: stats.file_cache_dir.unwrap_or_default(),
                slice_size: stats.slice_size,
                max_cache_size: stats.max_cache_size,
                max_cacheable_body: stats.max_cacheable_body,
                manifest_ttl_secs: stats.manifest_ttl_secs,
                segment_ttl_secs: stats.segment_ttl_secs,
                stale_max_age_secs: stats.stale_max_age_secs,
                stale_while_revalidate: stats.stale_while_revalidate,
                eviction_interval_secs: stats.eviction_interval_secs,
                watermark_ratio: stats.watermark_ratio,
            }),
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
        })
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; required by gRPC trait
impl ClusterService for ClusterServer {
    /// Get all nodes in the cluster
    async fn get_nodes(
        &self,
        request: Request<GetNodesRequest>,
    ) -> std::result::Result<Response<GetNodesResponse>, Status> {
        self.authorize(&request)?;
        let start = std::time::Instant::now();
        let result = self.node_registry.get_all_nodes().await;

        match result {
            Ok(nodes) => {
                let elapsed = start.elapsed().as_secs_f64();
                synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
                    .with_label_values(&["cluster", "get_nodes", "ok"])
                    .observe(elapsed);
                synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                    .with_label_values(&["cluster", "get_nodes", "ok"])
                    .inc();
                let proto_nodes: Vec<NodeInfo> =
                    nodes.iter().map(Self::discovery_to_proto_node).collect();

                Ok(Response::new(GetNodesResponse { nodes: proto_nodes }))
            }
            Err(e) => {
                let elapsed = start.elapsed().as_secs_f64();
                synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
                    .with_label_values(&["cluster", "get_nodes", "error"])
                    .observe(elapsed);
                synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                    .with_label_values(&["cluster", "get_nodes", "error"])
                    .inc();
                tracing::error!("Failed to get nodes: {}", e);
                Err(Status::unavailable(e.to_string()))
            }
        }
    }

    /// Get online status of users on this node
    ///
    /// Returns the online status for requested users based on this node's
    /// `ConnectionManager`. In a multi-replica setup, the caller should fan out
    /// this query to all nodes to get the global picture.
    async fn get_user_online_status(
        &self,
        request: Request<GetUserOnlineStatusRequest>,
    ) -> std::result::Result<Response<GetUserOnlineStatusResponse>, Status> {
        self.authorize(&request)?;
        let start = std::time::Instant::now();
        let req = request.into_inner();

        if req.user_ids.len() > Self::MAX_USER_IDS {
            return Err(Status::invalid_argument(format!(
                "user_ids array must contain at most {} entries",
                Self::MAX_USER_IDS
            )));
        }

        let Some(ref cm) = self.connection_runtime else {
            return Ok(Response::new(GetUserOnlineStatusResponse {
                statuses: Vec::new(),
            }));
        };

        let statuses: Vec<UserOnlineStatus> = req
            .user_ids
            .iter()
            .map(|uid| {
                let user_id = UserId::from(*uid);
                let connections = cm.get_user_connections(&user_id);
                let is_online = !connections.is_empty();
                let room_ids: Vec<i64> = connections
                    .iter()
                    .filter_map(|c| c.room_id.as_ref().map(RoomId::as_i64))
                    .collect();

                UserOnlineStatus {
                    user_id: *uid,
                    is_online,
                    room_ids,
                    node_id: self.node_id.clone(),
                }
            })
            .collect();

        let elapsed = start.elapsed().as_secs_f64();
        synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["cluster", "get_user_online_status", "ok"])
            .observe(elapsed);
        synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
            .with_label_values(&["cluster", "get_user_online_status", "ok"])
            .inc();

        Ok(Response::new(GetUserOnlineStatusResponse { statuses }))
    }

    /// Get connections for a room on this node
    ///
    /// Returns the active connections in a specific room based on this node's
    /// `ConnectionManager`. In a multi-replica setup, the caller should fan out
    /// this query to all nodes to get the global room connections.
    async fn get_room_connections(
        &self,
        request: Request<GetRoomConnectionsRequest>,
    ) -> std::result::Result<Response<GetRoomConnectionsResponse>, Status> {
        self.authorize(&request)?;
        let start = std::time::Instant::now();
        let req = request.into_inner();

        let Some(ref cm) = self.connection_runtime else {
            return Ok(Response::new(GetRoomConnectionsResponse {
                connections: Vec::new(),
            }));
        };

        let room_id = RoomId::from(req.room_id);
        let room_conns = cm.get_room_connections(&room_id);

        let connections: Vec<RoomConnection> = room_conns
            .iter()
            .map(|conn| {
                // Convert Instant durations to Unix timestamps (approximate)
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let now_unix = u64_to_i64(now_unix);
                let connected_secs_ago = u64_to_i64(conn.connected_at.elapsed().as_secs());
                let last_activity_secs_ago = u64_to_i64(conn.last_activity.elapsed().as_secs());

                RoomConnection {
                    user_id: conn.user_id.as_i64(),
                    node_id: self.node_id.clone(),
                    connected_at: now_unix - connected_secs_ago,
                    last_activity: now_unix - last_activity_secs_ago,
                }
            })
            .collect();

        let elapsed = start.elapsed().as_secs_f64();
        synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["cluster", "get_room_connections", "ok"])
            .observe(elapsed);
        synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
            .with_label_values(&["cluster", "get_room_connections", "ok"])
            .inc();

        Ok(Response::new(GetRoomConnectionsResponse { connections }))
    }

    async fn get_slice_cache_stats(
        &self,
        request: Request<GetSliceCacheStatsRequest>,
    ) -> std::result::Result<Response<SliceCacheStatsResponse>, Status> {
        self.authorize(&request)?;
        Ok(Response::new(self.slice_cache_stats_response()?))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> std::result::Result<Response<PurgeSliceCacheResponse>, Status> {
        self.authorize(&request)?;
        let cache = self.proxy_slice_cache.as_ref().ok_or_else(|| {
            Status::failed_precondition("Proxy slice cache runtime is unavailable")
        })?;
        let result = cache.purge_all().await;
        Ok(Response::new(PurgeSliceCacheResponse {
            node_id: self.node_id.clone(),
            success: true,
            removed_entries: result.removed_entries,
            freed_bytes: result.freed_bytes,
            stats: Some(self.slice_cache_stats_response()?),
        }))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<super::synctv::cluster::EvictExpiredSliceCacheRequest>,
    ) -> std::result::Result<Response<super::synctv::cluster::EvictExpiredSliceCacheResponse>, Status>
    {
        self.authorize(&request)?;
        let cache = self.proxy_slice_cache.as_ref().ok_or_else(|| {
            Status::failed_precondition("Proxy slice cache runtime is unavailable")
        })?;
        let removed_expired_entries = cache.evict_expired_entries().await;
        Ok(Response::new(
            super::synctv::cluster::EvictExpiredSliceCacheResponse {
                node_id: self.node_id.clone(),
                success: true,
                removed_expired_entries,
                stats: Some(self.slice_cache_stats_response()?),
            },
        ))
    }
}
