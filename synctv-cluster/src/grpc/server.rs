//! Cluster gRPC server implementation
//!
//! Handles inter-node communication for cluster coordination.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use super::synctv::cluster::cluster_service_server::ClusterService;
use super::synctv::cluster::{
    DeregisterNodeRequest, DeregisterNodeResponse, GetNodesRequest, GetNodesResponse,
    GetRoomConnectionsRequest, GetRoomConnectionsResponse, GetUserOnlineStatusRequest,
    GetUserOnlineStatusResponse, NodeInfo, NodeStatus, RoomConnection, UserOnlineStatus,
};
use super::ClusterAuthInterceptor;
use crate::discovery::{NodeInfo as DiscoveryNodeInfo, NodeRegistry};
use crate::sync::connection_manager::ConnectionManager;

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
/// | `DeregisterNode` | ACTIVE | Handles graceful shutdown with epoch validation |
/// | `GetUserOnlineStatus` | ACTIVE | Fan-out query for user presence across nodes |
/// | `GetRoomConnections` | ACTIVE | Fan-out query for room participants across nodes |
#[derive(Clone)]
pub struct ClusterServer {
    node_registry: Arc<NodeRegistry>,
    connection_manager: Option<Arc<ConnectionManager>>,
    node_id: String,
    auth: Option<ClusterAuthInterceptor>,
}

#[allow(clippy::result_large_err)] // tonic::Status is inherently large; required by gRPC API
impl ClusterServer {
    /// Create a new cluster server
    #[must_use]
    pub const fn new(node_registry: Arc<NodeRegistry>, node_id: String) -> Self {
        Self {
            node_registry,
            connection_manager: None,
            node_id,
            auth: None,
        }
    }

    /// Set the connection manager for user/room connection queries
    #[must_use]
    pub fn with_connection_manager(mut self, cm: Arc<ConnectionManager>) -> Self {
        self.connection_manager = Some(cm);
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

    /// Maximum length for `node_id`
    const MAX_NODE_ID_LEN: usize = 64;
    /// Maximum number of `user_ids` in a single request
    const MAX_USER_IDS: usize = 1000;

    /// Validate a `node_id`: non-empty, max 64 chars, alphanumeric + underscore/hyphen
    fn validate_node_id(node_id: &str) -> std::result::Result<(), Status> {
        if node_id.is_empty() {
            return Err(Status::invalid_argument("node_id must not be empty"));
        }
        if node_id.len() > Self::MAX_NODE_ID_LEN {
            return Err(Status::invalid_argument(format!(
                "node_id must be at most {} characters",
                Self::MAX_NODE_ID_LEN
            )));
        }
        if !node_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Status::invalid_argument(
                "node_id must contain only alphanumeric characters, underscores, or hyphens",
            ));
        }
        Ok(())
    }

    /// Convert discovery `NodeInfo` to proto `NodeInfo`.
    ///
    /// Proto enum `NodeStatus` mapping (see `synctv.cluster.proto`):
    ///   0 = Unknown, 1 = Active, 2 = Draining, 3 = Offline
    fn discovery_to_proto_node(&self, discovery: &DiscoveryNodeInfo) -> NodeInfo {
        NodeInfo {
            node_id: discovery.node_id.clone(),
            address: discovery.grpc_address.clone(),
            region: String::new(),
            status: NodeStatus::Active as i32,
            // Use last_heartbeat as proxy for registered_at since
            // DiscoveryNodeInfo doesn't track actual registration time.
            registered_at: discovery.last_heartbeat.timestamp(),
            last_heartbeat: discovery.last_heartbeat.timestamp(),
            metrics: None,
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
                let proto_nodes: Vec<NodeInfo> = nodes
                    .iter()
                    .map(|n| self.discovery_to_proto_node(n))
                    .collect();

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

    /// Deregister a node from the cluster
    async fn deregister_node(
        &self,
        request: Request<DeregisterNodeRequest>,
    ) -> std::result::Result<Response<DeregisterNodeResponse>, Status> {
        self.authorize(&request)?;
        let start = std::time::Instant::now();
        let req = request.into_inner();

        Self::validate_node_id(&req.node_id)?;

        // Epoch is required to prevent stale deregister requests from removing
        // re-registered nodes.
        if req.epoch == 0 {
            return Err(Status::invalid_argument(
                "epoch is required for deregister requests",
            ));
        }

        // Remove the node from Redis registry with epoch validation
        if let Err(e) = self
            .node_registry
            .unregister_remote(&req.node_id, Some(req.epoch))
            .await
        {
            let elapsed = start.elapsed().as_secs_f64();
            synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
                .with_label_values(&["cluster", "deregister_node", "error"])
                .observe(elapsed);
            synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
                .with_label_values(&["cluster", "deregister_node", "error"])
                .inc();
            tracing::warn!(
                node_id = %req.node_id,
                epoch = req.epoch,
                error = %e,
                "Failed to deregister node from cluster"
            );
            return Err(Status::unavailable(e.to_string()));
        }

        let elapsed = start.elapsed().as_secs_f64();
        synctv_core::metrics::grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["cluster", "deregister_node", "ok"])
            .observe(elapsed);
        synctv_core::metrics::grpc::GRPC_REQUESTS_TOTAL
            .with_label_values(&["cluster", "deregister_node", "ok"])
            .inc();

        tracing::info!(
            node_id = %req.node_id,
            epoch = req.epoch,
            reason = %req.reason,
            "Node deregistered from cluster"
        );

        Ok(Response::new(DeregisterNodeResponse { success: true }))
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

        let Some(ref cm) = self.connection_manager else {
            return Ok(Response::new(GetUserOnlineStatusResponse {
                statuses: Vec::new(),
            }));
        };

        let statuses: Vec<UserOnlineStatus> = req
            .user_ids
            .iter()
            .map(|uid| {
                let user_id = synctv_core::models::UserId::from_string(uid.clone());
                let connections = cm.get_user_connections(&user_id);
                let is_online = !connections.is_empty();
                let room_ids: Vec<String> = connections
                    .iter()
                    .filter_map(|c| c.room_id.as_ref().map(|r| r.as_str().to_string()))
                    .collect();

                UserOnlineStatus {
                    user_id: uid.clone(),
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

        let Some(ref cm) = self.connection_manager else {
            return Ok(Response::new(GetRoomConnectionsResponse {
                connections: Vec::new(),
            }));
        };

        let room_id = synctv_core::models::RoomId::from_string(req.room_id);
        let room_conns = cm.get_room_connections(&room_id);

        let connections: Vec<RoomConnection> = room_conns
            .iter()
            .map(|conn| {
                // Convert Instant durations to Unix timestamps (approximate)
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let connected_secs_ago = conn.connected_at.elapsed().as_secs() as i64;
                let last_activity_secs_ago = conn.last_activity.elapsed().as_secs() as i64;

                RoomConnection {
                    user_id: conn.user_id.as_str().to_string(),
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
}
