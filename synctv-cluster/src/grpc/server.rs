//! Cluster gRPC server implementation.
//!
//! This service is intentionally limited to cluster topology/discovery. Business
//! inter-node calls live in their owning crates and are mounted on the main API
//! tonic server with internal shared-secret auth.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use super::synctv::cluster::cluster_service_server::ClusterService;
use super::synctv::cluster::{GetNodesRequest, GetNodesResponse, NodeInfo};
use super::ClusterAuthInterceptor;
use crate::discovery::{ClusterNodeDirectory, NodeInfo as DiscoveryNodeInfo};

#[derive(Clone)]
pub struct ClusterServer {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    auth: Option<ClusterAuthInterceptor>,
}

impl ClusterServer {
    #[must_use]
    pub fn new<N>(node_registry: Arc<N>) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry)
    }

    #[must_use]
    pub fn from_runtime(node_registry: Arc<dyn ClusterNodeDirectory>) -> Self {
        Self {
            node_registry,
            auth: None,
        }
    }

    #[must_use]
    pub fn with_cluster_secret(mut self, secret: String) -> Self {
        self.auth = Some(ClusterAuthInterceptor::new(secret));
        self
    }

    fn discovery_to_proto_node(discovery: &DiscoveryNodeInfo) -> NodeInfo {
        NodeInfo {
            node_id: discovery.node_id.clone(),
            address: discovery.api_address.clone(),
            last_heartbeat: discovery.last_heartbeat.timestamp(),
            epoch: discovery.epoch,
        }
    }

    #[allow(clippy::result_large_err)]
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
#[allow(clippy::result_large_err)]
impl ClusterService for ClusterServer {
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
                synctv_core::metrics::remote_transport::REMOTE_TRANSPORT_REQUEST_DURATION
                    .with_label_values(&["cluster", "get_nodes", "ok"])
                    .observe(elapsed);
                synctv_core::metrics::remote_transport::REMOTE_TRANSPORT_REQUESTS_TOTAL
                    .with_label_values(&["cluster", "get_nodes", "ok"])
                    .inc();
                let proto_nodes = nodes.iter().map(Self::discovery_to_proto_node).collect();

                Ok(Response::new(GetNodesResponse { nodes: proto_nodes }))
            }
            Err(error) => {
                let elapsed = start.elapsed().as_secs_f64();
                synctv_core::metrics::remote_transport::REMOTE_TRANSPORT_REQUEST_DURATION
                    .with_label_values(&["cluster", "get_nodes", "error"])
                    .observe(elapsed);
                synctv_core::metrics::remote_transport::REMOTE_TRANSPORT_REQUESTS_TOTAL
                    .with_label_values(&["cluster", "get_nodes", "error"])
                    .inc();
                tracing::error!("Failed to get nodes from cluster registry: {error}");
                Err(Status::unavailable(error.to_string()))
            }
        }
    }
}
