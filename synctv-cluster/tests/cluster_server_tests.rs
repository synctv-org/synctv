//! `ClusterServer` gRPC handlers.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use tonic::Request;

use synctv_cluster::grpc::server::ClusterServer;
use synctv_cluster::grpc::synctv::cluster::cluster_service_server::ClusterService;
use synctv_cluster::grpc::synctv::cluster::GetNodesRequest;
use synctv_cluster::{NodeInfo, NodeRegistry};

fn make_server() -> (Arc<NodeRegistry>, ClusterServer) {
    let registry = Arc::new(
        NodeRegistry::new_local_only("test-node".to_string(), 30, "cluster-server-test:")
            .expect("local registry should initialize"),
    );
    let server = ClusterServer::new(registry.clone());
    (registry, server)
}

fn with_cluster_secret<T>(mut request: Request<T>, secret: &str) -> Request<T> {
    request.metadata_mut().insert(
        synctv_cluster::grpc::CLUSTER_SECRET_METADATA_KEY,
        secret.parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn get_nodes_returns_registered_topology() {
    let (registry, server) = make_server();
    registry
        .test_insert_local(NodeInfo::new(
            "peer-node".to_string(),
            "127.0.0.1:9001".to_string(),
        ))
        .await;

    let response = server
        .with_cluster_secret("cluster-test-secret".to_string())
        .get_nodes(with_cluster_secret(
            Request::new(GetNodesRequest {}),
            "cluster-test-secret",
        ))
        .await
        .expect("authorized get_nodes should succeed")
        .into_inner();

    assert_eq!(response.nodes.len(), 1);
    assert_eq!(response.nodes[0].node_id, "peer-node");
    assert_eq!(response.nodes[0].address, "127.0.0.1:9001");
}

#[tokio::test]
async fn cluster_server_rejects_requests_without_configured_secret() {
    let (_registry, server) = make_server();

    let status = server
        .get_nodes(Request::new(GetNodesRequest {}))
        .await
        .expect_err("server without configured secret must fail closed");

    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(
        status.message().contains("not configured"),
        "misconfiguration should be explicit, got: {}",
        status.message()
    );
}

#[tokio::test]
async fn cluster_server_rejects_missing_secret_header() {
    let (_registry, server) = make_server();
    let server = server.with_cluster_secret("cluster-test-secret".to_string());

    let status = server
        .get_nodes(Request::new(GetNodesRequest {}))
        .await
        .expect_err("missing cluster secret header must be rejected");

    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(
        status.message().contains("Missing"),
        "error should mention missing cluster secret header, got: {}",
        status.message()
    );
}
