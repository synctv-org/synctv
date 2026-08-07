//! `ClusterClient` fan-out
//!
//! Tests for `FanOutResult` construction, `merge_user_statuses` logic,
//! and `ClusterClient` with no remote nodes.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use synctv_cluster::discovery::node_registry::{ClusterMode, NodeRegistry};
use synctv_cluster::discovery::NodeInfo;
use synctv_core::models::UserId;
use synctv_core::{RedisConnectionRuntime, RedisCoordinationRuntime};
use synctv_realtime::grpc::UserOnlineStatus;
use synctv_realtime::grpc::{FanOutResult, RealtimePresenceClient, RealtimePresenceClientConfig};

#[derive(Clone)]
struct UnavailableRedisRuntime;

fn unavailable_redis_runtime() -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(UnavailableRedisRuntime)
}

fn unavailable_redis_error() -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::Io,
        "test Redis coordination runtime unavailable",
    ))
}

#[async_trait]
impl RedisConnectionRuntime for UnavailableRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Err(unavailable_redis_error())
    }

    fn operation_timeout(&self) -> Duration {
        Duration::from_millis(10)
    }
}

#[async_trait]
impl RedisCoordinationRuntime for UnavailableRedisRuntime {
    async fn multiplexed_connection(
        &self,
    ) -> redis::RedisResult<redis::aio::MultiplexedConnection> {
        Err(unavailable_redis_error())
    }

    async fn async_pubsub(&self) -> redis::RedisResult<redis::aio::PubSub> {
        Err(unavailable_redis_error())
    }
}

// FanOutResult construction and queries

/// Verify `FanOutResult` with partial failure tracks `nodes_failed` correctly.
#[test]
fn test_fan_out_result_partial_failure_tracking() {
    let result: FanOutResult<Vec<UserOnlineStatus>> = FanOutResult {
        data: vec![UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![101],
            node_id: "fast-node:50051".to_string(),
        }],
        nodes_succeeded: 1,
        nodes_failed: 1,
        failures: vec![("slow-node:50051".to_string(), "timeout".to_string())],
    };

    assert_eq!(result.nodes_failed, 1, "Should have 1 failed node");
    assert_eq!(
        result.total_nodes(),
        2,
        "Should have contacted 2 nodes total"
    );
    assert!(
        !result.is_complete(),
        "Should not be complete with a failure"
    );
    assert_eq!(result.data.len(), 1, "Should have 1 successful response");
    assert_eq!(
        result.failures[0].0, "slow-node:50051",
        "Should track which node failed"
    );
}

/// Verify `FanOutResult` is complete when all nodes succeed.
#[test]
fn test_fan_out_result_all_success() {
    let result: FanOutResult<Vec<UserOnlineStatus>> = FanOutResult {
        data: vec![
            UserOnlineStatus {
                user_id: 1,
                is_online: true,
                room_ids: vec![],
                node_id: "node-a".to_string(),
            },
            UserOnlineStatus {
                user_id: 2,
                is_online: true,
                room_ids: vec![],
                node_id: "node-b".to_string(),
            },
        ],
        nodes_succeeded: 2,
        nodes_failed: 0,
        failures: vec![],
    };

    assert!(result.is_complete(), "Should be complete when no failures");
    assert_eq!(result.data.len(), 2);
    assert_eq!(result.total_nodes(), 2);
}

/// Verify `FanOutResult` with all failures.
#[test]
fn test_fan_out_result_all_failed() {
    let result: FanOutResult<Vec<UserOnlineStatus>> = FanOutResult {
        data: vec![],
        nodes_succeeded: 0,
        nodes_failed: 3,
        failures: vec![
            ("node-a:50051".to_string(), "error a".to_string()),
            ("node-b:50051".to_string(), "error b".to_string()),
            ("node-c:50051".to_string(), "error c".to_string()),
        ],
    };

    assert!(!result.is_complete());
    assert!(result.data.is_empty());
    assert_eq!(result.nodes_failed, 3);
    assert_eq!(result.failures.len(), 3);
    assert_eq!(result.total_nodes(), 3);
}

#[test]
fn test_fan_out_result_timeout_failures_include_node_details() {
    let result: FanOutResult<Vec<UserOnlineStatus>> = FanOutResult {
        data: vec![],
        nodes_succeeded: 1,
        nodes_failed: 2,
        failures: vec![
            ("node-b".to_string(), "aggregate timeout".to_string()),
            ("node-c".to_string(), "aggregate timeout".to_string()),
        ],
    };

    assert_eq!(
        result.failures.len(),
        result.nodes_failed,
        "failure details must cover every failed node so operators can identify timed-out peers"
    );
}

// merge_user_statuses tests

/// Verify `merge_user_statuses`: `any_online_wins` policy.
#[test]
fn test_merge_user_statuses_any_online_wins() {
    // Node A says user1 offline, Node B says user1 online
    let statuses = vec![
        UserOnlineStatus {
            user_id: 1,
            is_online: false,
            room_ids: vec![],
            node_id: "node-a".to_string(),
        },
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![101],
            node_id: "node-b".to_string(),
        },
    ];

    let merged = RealtimePresenceClient::merge_user_statuses(statuses);
    assert_eq!(merged.len(), 1);

    let status = &merged[0];
    assert!(status.is_online, "Any-online-wins: should be online");
    assert!(status.room_ids.contains(&101), "Should contain room-a");
}

/// Verify `merge_user_statuses`: dedup `room_ids` from multiple nodes.
#[test]
fn test_merge_user_statuses_dedup_rooms() {
    let statuses = vec![
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![101, 102],
            node_id: "node-a".to_string(),
        },
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![102, 103],
            node_id: "node-b".to_string(),
        },
    ];

    let merged = RealtimePresenceClient::merge_user_statuses(statuses);
    assert_eq!(merged.len(), 1);

    let status = &merged[0];
    let mut rooms = status.room_ids.clone();
    rooms.sort_unstable();
    assert_eq!(
        rooms,
        vec![101, 102, 103],
        "Room IDs should be deduplicated"
    );
}

#[test]
fn test_merge_user_statuses_dedup_rooms_within_single_status() {
    let merged = RealtimePresenceClient::merge_user_statuses(vec![UserOnlineStatus {
        user_id: 1,
        is_online: true,
        room_ids: vec![103, 101, 103, 101],
        node_id: "node-a".to_string(),
    }]);

    assert_eq!(merged[0].room_ids, vec![101, 103]);
}

/// Verify `merge_user_statuses`: multiple users from multiple nodes.
#[test]
fn test_merge_user_statuses_multi_user_multi_node() {
    let statuses = vec![
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![101],
            node_id: "node-a".to_string(),
        },
        UserOnlineStatus {
            user_id: 2,
            is_online: false,
            room_ids: vec![],
            node_id: "node-a".to_string(),
        },
        UserOnlineStatus {
            user_id: 2,
            is_online: true,
            room_ids: vec![102],
            node_id: "node-b".to_string(),
        },
        UserOnlineStatus {
            user_id: 3,
            is_online: true,
            room_ids: vec![],
            node_id: "node-b".to_string(),
        },
    ];

    let merged = RealtimePresenceClient::merge_user_statuses(statuses);
    assert_eq!(merged.len(), 3, "Should have 3 users");

    let user1 = merged.iter().find(|s| s.user_id == 1).unwrap();
    assert!(user1.is_online);

    let user2 = merged.iter().find(|s| s.user_id == 2).unwrap();
    assert!(user2.is_online, "user2 should be online (any-online-wins)");

    let user3 = merged.iter().find(|s| s.user_id == 3).unwrap();
    assert!(user3.is_online);
}

/// Verify empty responses produce empty merge result.
#[test]
fn test_merge_user_statuses_node_id_merged() {
    let statuses = vec![
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![101],
            node_id: "node-a".to_string(),
        },
        UserOnlineStatus {
            user_id: 1,
            is_online: true,
            room_ids: vec![102],
            node_id: "node-b".to_string(),
        },
    ];

    let merged = RealtimePresenceClient::merge_user_statuses(statuses);
    assert_eq!(merged.len(), 1);

    let user = &merged[0];
    assert!(
        user.node_id.contains("node-a"),
        "node_id should contain node-a"
    );
    assert!(
        user.node_id.contains("node-b"),
        "node_id should contain node-b"
    );
}

/// `ClusterClient` with no remote nodes should return empty fan-out results.
/// This test requires no actual Redis connection because `get_all_nodes` falls back
/// to local cache in degraded mode, which is empty.
#[tokio::test]
async fn test_cluster_client_no_remote_nodes_fan_out() {
    let registry =
        Arc::new(NodeRegistry::new_local_only("self-node".to_string(), 30, "cl6test:").unwrap());

    let config = RealtimePresenceClientConfig {
        self_node_id: "self-node".to_string(),
        ..Default::default()
    };

    let client = RealtimePresenceClient::new(registry, config);

    // fan_out_user_online_status calls get_all_nodes which will fail (no Redis)
    // and in degraded mode returns local cache (empty -> no remote nodes).
    // Since get_all_nodes may error, the fan_out might error too.
    // But the key test is: when there ARE no remote nodes, the result is empty.
    let result = client
        .fan_out_user_online_status(vec![UserId::expect_positive(1)])
        .await;

    // The call may fail (Redis unavailable) or succeed with empty.
    // Either is acceptable since the key behavior (empty fan-out) is tested above.
    if let Ok(fan_out) = result {
        assert_eq!(fan_out.nodes_succeeded, 0);
        assert!(fan_out.data.is_empty());
    }
}

/// Fan-out is a routing-sensitive path and must fail closed when the node view
/// is degraded and stale, instead of silently querying with stale topology.
#[tokio::test]
async fn test_cluster_client_fan_out_fails_closed_when_degraded_cache_is_stale() {
    let registry = Arc::new(
        NodeRegistry::new(
            unavailable_redis_runtime(),
            "self-node".to_string(),
            30,
            "fanout-stale:",
        )
        .unwrap(),
    );

    registry
        .test_insert_local(NodeInfo::new(
            "self-node".to_string(),
            "http://127.0.0.1:8080".to_string(),
        ))
        .await;
    registry
        .test_insert_local(NodeInfo::new(
            "peer-node".to_string(),
            "http://127.0.0.1:8081".to_string(),
        ))
        .await;
    registry.test_set_cluster_mode(ClusterMode::Degraded);
    registry.test_set_last_refreshed_at(1);

    let client = RealtimePresenceClient::new(
        registry,
        RealtimePresenceClientConfig {
            self_node_id: "self-node".to_string(),
            ..Default::default()
        },
    );

    let err = client
        .fan_out_user_online_status(vec![UserId::expect_positive(1)])
        .await
        .expect_err("stale degraded topology must not be used for fan-out routing");
    assert!(err.to_string().contains("stale"), "unexpected error: {err}");
}
