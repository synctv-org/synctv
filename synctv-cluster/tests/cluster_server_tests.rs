//! `ClusterServer` gRPC handlers
//!
//! - `validate_node_id`: empty/long/invalid -> error
//! - `get_user_online_status`: `MAX_USER_IDS+1` -> `invalid_argument`
//! - `connection_runtime=None` -> empty results

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use tonic::Request;

use synctv_cluster::discovery::node_registry::NodeRegistry;
use synctv_cluster::grpc::server::ClusterServer;

// Import the ClusterService trait to call the gRPC methods
use synctv_cluster::grpc::synctv::cluster::cluster_service_server::ClusterService;
use synctv_cluster::grpc::synctv::cluster::{
    GetRoomConnectionsRequest, GetUserOnlineStatusRequest,
};

/// Helper: create a `ClusterServer` with no connection query runtime.
fn make_server() -> ClusterServer {
    let registry = Arc::new(
        NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(
                redis::Client::open("redis://127.0.0.1:1").unwrap(),
            ),
            "test-node".to_string(),
            30,
            "cl5test:",
        )
        .unwrap(),
    );
    ClusterServer::new(registry, "test-node".to_string())
}

fn make_authenticated_server() -> ClusterServer {
    make_server().with_cluster_secret("cluster-test-secret".to_string())
}

fn with_cluster_secret<T>(mut request: Request<T>, secret: &str) -> Request<T> {
    request
        .metadata_mut()
        .insert("x-cluster-secret", secret.parse().unwrap());
    request
}

// get_user_online_status: MAX_USER_IDS+1 -> invalid_argument

/// Sending more than 1000 user IDs -> `invalid_argument`.
#[tokio::test]
async fn test_get_user_online_status_too_many_ids() {
    let server = make_authenticated_server();

    let user_ids: Vec<i64> = (1..=1001).collect();
    let request = with_cluster_secret(
        Request::new(GetUserOnlineStatusRequest { user_ids }),
        "cluster-test-secret",
    );

    let result = server.get_user_online_status(request).await;
    assert!(result.is_err(), "1001 user_ids should be rejected");

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::InvalidArgument,
        "Error code should be InvalidArgument"
    );
    assert!(
        status.message().contains("1000"),
        "Error message should mention the max limit, got: {}",
        status.message()
    );
}

/// Sending exactly 1000 user IDs should be accepted.
#[tokio::test]
async fn test_get_user_online_status_at_limit() {
    let server = make_authenticated_server();

    let user_ids: Vec<i64> = (1..=1000).collect();
    let request = with_cluster_secret(
        Request::new(GetUserOnlineStatusRequest { user_ids }),
        "cluster-test-secret",
    );

    // Without connection_manager, returns empty statuses
    let result = server.get_user_online_status(request).await;
    assert!(result.is_ok(), "Exactly 1000 user_ids should be accepted");

    let response = result.unwrap().into_inner();
    assert!(
        response.statuses.is_empty(),
        "Without connection_manager, should return empty statuses"
    );
}

// connection_manager=None -> empty results

/// `get_user_online_status` with no `ConnectionManager` returns empty.
#[tokio::test]
async fn test_get_user_online_status_no_connection_manager() {
    let server = make_authenticated_server(); // No with_connection_manager() call

    let request = with_cluster_secret(
        Request::new(GetUserOnlineStatusRequest {
            user_ids: vec![1, 2],
        }),
        "cluster-test-secret",
    );

    let result = server.get_user_online_status(request).await;
    assert!(result.is_ok());

    let response = result.unwrap().into_inner();
    assert!(
        response.statuses.is_empty(),
        "Should return empty statuses when ConnectionManager is None"
    );
}

/// `get_room_connections` with no `ConnectionManager` returns empty.
#[tokio::test]
async fn test_get_room_connections_no_connection_manager() {
    let server = make_authenticated_server(); // No with_connection_manager() call

    let request = with_cluster_secret(
        Request::new(GetRoomConnectionsRequest { room_id: 101 }),
        "cluster-test-secret",
    );

    let result = server.get_room_connections(request).await;
    assert!(result.is_ok());

    let response = result.unwrap().into_inner();
    assert!(
        response.connections.is_empty(),
        "Should return empty connections when ConnectionManager is None"
    );
}

// With connection runtime: actual user/room queries

#[tokio::test]
async fn test_get_user_online_status_with_connection_manager() {
    use synctv_cluster::sync::connection_manager::{ConnectionLimits, ConnectionManager};
    use synctv_core::models::id::UserId;

    let registry = Arc::new(
        NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(
                redis::Client::open("redis://127.0.0.1:1").unwrap(),
            ),
            "test-node".to_string(),
            30,
            "cl5cm:",
        )
        .unwrap(),
    );

    let cm = Arc::new(ConnectionManager::new(ConnectionLimits::default()));

    // Register a connection
    let user_id = UserId::expect_positive(10_000_028);
    cm.register("conn-1".to_string(), user_id).await.unwrap();

    let server = ClusterServer::new(registry, "test-node".to_string())
        .with_cluster_secret("cluster-test-secret".to_string())
        .with_connection_runtime(cm);

    let request = with_cluster_secret(
        Request::new(GetUserOnlineStatusRequest {
            user_ids: vec![user_id.as_i64(), 10_000_029],
        }),
        "cluster-test-secret",
    );

    let result = server.get_user_online_status(request).await;
    assert!(result.is_ok());

    let response = result.unwrap().into_inner();
    assert_eq!(response.statuses.len(), 2);

    let online_user = response
        .statuses
        .iter()
        .find(|s| s.user_id == user_id.as_i64())
        .unwrap();
    assert!(online_user.is_online, "test user should be online");
    assert_eq!(online_user.node_id, "test-node");

    let offline_user = response
        .statuses
        .iter()
        .find(|s| s.user_id == 10_000_029)
        .unwrap();
    assert!(!offline_user.is_online, "offline user should be offline");
}

#[tokio::test]
async fn test_cluster_server_rejects_requests_without_configured_secret() {
    let server = make_server();

    let result = server
        .get_user_online_status(Request::new(GetUserOnlineStatusRequest {
            user_ids: vec![1],
        }))
        .await;

    let status = result.expect_err("server without configured secret must fail closed");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(
        status.message().contains("not configured"),
        "misconfiguration should be explicit, got: {}",
        status.message()
    );
}

#[tokio::test]
async fn test_cluster_server_rejects_missing_secret_header() {
    let server = make_authenticated_server();

    let result = server
        .get_user_online_status(Request::new(GetUserOnlineStatusRequest {
            user_ids: vec![1],
        }))
        .await;

    let status = result.expect_err("missing cluster secret header must be rejected");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert!(
        status.message().contains("Missing"),
        "error should mention missing cluster secret header, got: {}",
        status.message()
    );
}
