//! WebSocket ticket Redis tests
//!
//! Tests the `RedisTicketStore` backend via testcontainers:
//! roundtrip, one-time use, room mismatch, TTL expiry, concurrent consumption.
//!
//! Also tests cluster mode Redis dependency:
//! - cluster mode with Redis should work correctly
//! - cluster mode without Redis should return an error (tested in unit tests)
//!
//! Run with: cargo test --test `ws_ticket_redis_tests`
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::WsTicketService;
use synctv_core_testing::start_redis_shared as start_test_redis_shared;
use tokio::sync::RwLock;

async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    Arc<RwLock<redis::aio::ConnectionManager>>,
) {
    start_test_redis_shared().await
}

fn user_id(id: &str) -> UserId {
    UserId::from_string(id.to_string())
}

fn room_id(id: &str) -> RoomId {
    RoomId::from_string(id.to_string())
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_create_and_validate_roundtrip() {
    let (_container, conn) = start_redis().await;
    let service = WsTicketService::with_redis(conn, Some(30));

    let uid = user_id("user_rt_1");
    let rid = room_id("room_rt_1");

    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();
    assert!(!ticket.is_empty());

    let validated = service.validate_and_consume(&ticket, &rid).await.unwrap();
    assert_eq!(validated.user_id.as_str(), "user_rt_1");
    assert_eq!(validated.password_version, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_one_time_use() {
    let (_container, conn) = start_redis().await;
    let service = WsTicketService::with_redis(conn, Some(30));

    let uid = user_id("user_otu_1");
    let rid = room_id("room_otu_1");

    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();

    // First consume succeeds
    let result1 = service.validate_and_consume(&ticket, &rid).await;
    assert!(result1.is_ok(), "First consumption should succeed");

    // Second consume fails
    let result2 = service.validate_and_consume(&ticket, &rid).await;
    assert!(
        result2.is_err(),
        "Second consumption should fail (one-time use)"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_room_mismatch_rejected() {
    let (_container, conn) = start_redis().await;
    let service = WsTicketService::with_redis(conn, Some(30));

    let uid = user_id("user_rm_1");
    let room_a = room_id("room_a");
    let room_b = room_id("room_b");

    let ticket = service.create_ticket(&uid, &room_a, 0).await.unwrap();

    // Try to consume with wrong room
    let result = service.validate_and_consume(&ticket, &room_b).await;
    assert!(
        result.is_err(),
        "Ticket for room A should not work for room B"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_ttl_expiry() {
    let (_container, conn) = start_redis().await;
    // 1-second TTL
    let service = WsTicketService::with_redis(conn, Some(1));

    let uid = user_id("user_ttl_1");
    let rid = room_id("room_ttl_1");

    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();

    // Wait for Redis TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let result = service.validate_and_consume(&ticket, &rid).await;
    assert!(result.is_err(), "Ticket should have expired after TTL");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_concurrent_consumption() {
    let (_container, conn) = start_redis().await;
    let service = WsTicketService::with_redis(conn, Some(30));

    let uid = user_id("user_conc_1");
    let rid = room_id("room_conc_1");

    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();

    // Spawn 10 tasks that all try to consume the same ticket concurrently
    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = service.clone();
        let t = ticket.clone();
        let r = rid.clone();
        handles.push(tokio::spawn(
            async move { s.validate_and_consume(&t, &r).await },
        ));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(
        successes, 1,
        "Exactly 1 of 10 concurrent consumers should succeed"
    );
    assert_eq!(failures, 9, "9 consumers should fail");
}

// ============================================================================
// Cluster mode Redis dependency tests (TDD)
// ============================================================================

/// Test: cluster mode with Redis works correctly.
/// This is the main fix - in cluster mode with Redis, tickets should work
/// because they are stored in shared Redis storage visible to all replicas.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_with_redis_succeeds() {
    let (_container, conn) = start_redis().await;

    let service = WsTicketService::new(Some(conn), Some(30));
    assert_eq!(
        service.backend_name(),
        "redis",
        "Cluster mode with Redis should use Redis backend"
    );
}

/// Test: cluster mode with Redis allows ticket creation and validation.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_with_redis_roundtrip() {
    let (_container, conn) = start_redis().await;

    let service = WsTicketService::new(Some(conn), Some(30));

    let uid = user_id("cluster_user_1");
    let rid = room_id("cluster_room_1");

    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();
    assert!(!ticket.is_empty());

    let validated = service.validate_and_consume(&ticket, &rid).await.unwrap();
    assert_eq!(validated.user_id.as_str(), "cluster_user_1");
    assert_eq!(validated.password_version, 0);
}

/// Test: cluster mode with Redis custom TTL works.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_with_redis_custom_ttl() {
    let (_container, conn) = start_redis().await;

    let service = WsTicketService::new(Some(conn), Some(60));

    assert_eq!(service.ticket_ttl_secs(), 60);
}

/// Test: `WsTicketService::with_redis` always creates a Redis-backed service
/// regardless of cluster mode flag (which is only checked in `::new`).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_with_redis_creates_redis_backend() {
    let (_container, conn) = start_redis().await;

    let service = WsTicketService::with_redis(conn, Some(30));

    assert_eq!(service.backend_name(), "redis");
}

/// Test: simulate multi-replica scenario - ticket created on "replica A"
/// can be validated on "replica B" when using Redis.
/// This simulates the core problem that the fix addresses.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_simulated_multi_replica_roundtrip() {
    let (_container, conn) = start_redis().await;

    // Clone the connection for the second "replica"
    let conn_clone = conn.clone();

    // "Replica A" creates the ticket
    let service_a = WsTicketService::new(Some(conn), Some(30));
    let uid = user_id("multi_replica_user");
    let rid = room_id("multi_replica_room");

    let ticket = service_a.create_ticket(&uid, &rid, 0).await.unwrap();

    // "Replica B" (different service instance, same Redis) validates the ticket
    let service_b = WsTicketService::new(Some(conn_clone), Some(30));

    let validated = service_b.validate_and_consume(&ticket, &rid).await.unwrap();
    assert_eq!(validated.user_id.as_str(), "multi_replica_user");

    // Ticket should be consumed (one-time use)
    let result = service_a.validate_and_consume(&ticket, &rid).await;
    assert!(result.is_err(), "Ticket should already be consumed");
}
