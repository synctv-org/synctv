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

use redis::AsyncCommands;
use std::sync::Arc;
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::{
    ws_ticket::RedisTicketStore, UserValidationResult, UserValidator, WsTicketService,
};
use synctv_core::{Error, Result};
use synctv_core_testing::{start_redis_handle as start_test_redis_handle, test_redis_key_prefix};
use tokio::sync::RwLock;

async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    Arc<RwLock<redis::aio::ConnectionManager>>,
    String,
) {
    let (container, conn) = start_test_redis_handle().await;
    let prefix = test_redis_key_prefix("ws-ticket");
    (container, conn, prefix)
}

fn redis_ticket_service(
    conn: Arc<RwLock<redis::aio::ConnectionManager>>,
    prefix: &str,
    ticket_ttl_secs: Option<u64>,
) -> WsTicketService {
    WsTicketService::from_store(
        Arc::new(RedisTicketStore::new(conn, prefix)),
        ticket_ttl_secs,
    )
}

fn user_id(id: &str) -> UserId {
    UserId::from_string(id.to_string())
}

fn room_id(id: &str) -> RoomId {
    RoomId::from_string(id.to_string())
}

struct StaticUserValidator {
    result: std::result::Result<UserValidationResult, &'static str>,
}

#[async_trait::async_trait]
impl UserValidator for StaticUserValidator {
    async fn validate_for_ticket(&self, _user_id: &UserId) -> Result<UserValidationResult> {
        self.result
            .clone()
            .map_err(|message| Error::Authorization((*message).to_string()))
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_create_and_validate_roundtrip() {
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

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
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

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
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

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

    let retry_result = service.validate_and_consume(&ticket, &room_a).await;
    assert!(
        retry_result.is_ok(),
        "room mismatch must not consume a valid Redis-backed ticket"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_ttl_expiry() {
    let (_container, conn, prefix) = start_redis().await;
    // 1-second TTL
    let service = redis_ticket_service(conn, &prefix, Some(1));

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
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_user_validation_failure_does_not_consume_ticket() {
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

    let uid = user_id("user_checked_1");
    let rid = room_id("room_checked_1");
    let ticket = service.create_ticket(&uid, &rid, 9).await.unwrap();

    let rejecting_validator = StaticUserValidator {
        result: Err("banned"),
    };
    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 9,
        }),
    };

    let first_result = service
        .validate_and_consume_checked(&ticket, &rid, &rejecting_validator)
        .await;
    assert!(
        matches!(first_result, Err(Error::Authorization(_))),
        "user validation failure should reject the ticket"
    );

    let second_result = service
        .validate_and_consume_checked(&ticket, &rid, &allow_validator)
        .await;
    assert!(
        second_result.is_ok(),
        "user validation rejection must not consume a valid Redis-backed ticket"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_checked_validation_is_still_one_time_use() {
    let (_container, conn, prefix) = start_redis().await;
    let service = redis_ticket_service(conn, &prefix, Some(30));

    let uid = user_id("user_checked_once");
    let rid = room_id("room_checked_once");
    let ticket = service.create_ticket(&uid, &rid, 3).await.unwrap();

    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 3,
        }),
    };

    let first_result = service
        .validate_and_consume_checked(&ticket, &rid, &allow_validator)
        .await;
    assert!(
        first_result.is_ok(),
        "first checked validation should succeed"
    );

    let second_result = service
        .validate_and_consume_checked(&ticket, &rid, &allow_validator)
        .await;
    assert!(
        matches!(second_result, Err(Error::Authorization(_))),
        "checked validation must still enforce one-time use"
    );
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
    let (_container, conn, prefix) = start_redis().await;

    let service = redis_ticket_service(conn, &prefix, Some(30));
    assert!(
        service.supports_cluster_runtime(),
        "Cluster mode with Redis should use cross-node capable ticket storage"
    );
}

/// Test: cluster mode with Redis allows ticket creation and validation.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_with_redis_roundtrip() {
    let (_container, conn, prefix) = start_redis().await;

    let service = redis_ticket_service(conn, &prefix, Some(30));

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
    let (_container, conn, prefix) = start_redis().await;

    let service = redis_ticket_service(conn, &prefix, Some(60));

    assert_eq!(service.ticket_ttl_secs(), 60);
}

/// Test: `WsTicketService::with_redis` always creates a Redis-backed service
/// regardless of cluster mode flag (which is only checked in `::new`).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_with_redis_creates_redis_backend() {
    let (_container, conn, prefix) = start_redis().await;

    let service = redis_ticket_service(conn, &prefix, Some(30));

    assert!(service.supports_cluster_runtime());
}

/// Test: simulate multi-replica scenario - ticket created on "replica A"
/// can be validated on "replica B" when using Redis.
/// This simulates the core problem that the fix addresses.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_simulated_multi_replica_roundtrip() {
    let (_container, conn, prefix) = start_redis().await;

    // Clone the connection for the second "replica"
    let conn_clone = conn.clone();

    // "Replica A" creates the ticket
    let service_a = redis_ticket_service(conn, &prefix, Some(30));
    let uid = user_id("multi_replica_user");
    let rid = room_id("multi_replica_room");

    let ticket = service_a.create_ticket(&uid, &rid, 0).await.unwrap();

    // "Replica B" (different service instance, same Redis) validates the ticket
    let service_b = redis_ticket_service(conn_clone, &prefix, Some(30));

    let validated = service_b.validate_and_consume(&ticket, &rid).await.unwrap();
    assert_eq!(validated.user_id.as_str(), "multi_replica_user");

    // Ticket should be consumed (one-time use)
    let result = service_a.validate_and_consume(&ticket, &rid).await;
    assert!(result.is_err(), "Ticket should already be consumed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_ticket_uses_configured_key_prefix() {
    let (_container, conn, _prefix) = start_redis().await;
    let service = redis_ticket_service(conn.clone(), "tenant-a:", Some(30));

    let uid = user_id("user_prefix_1");
    let rid = room_id("room_prefix_1");
    let ticket = service.create_ticket(&uid, &rid, 0).await.unwrap();

    let mut redis_conn = conn.read().await.clone();
    let payload: Option<String> = redis_conn
        .get(format!("tenant-a:ws_ticket:{}:{ticket}", rid.as_str()))
        .await
        .unwrap();
    assert!(
        payload.is_some(),
        "ticket must be stored under configured prefix"
    );

    let old_key_payload: Option<String> = redis_conn
        .get(format!("synctv:ws_ticket:{}:{ticket}", rid.as_str()))
        .await
        .unwrap();
    assert!(
        old_key_payload.is_none(),
        "ticket must not leak into hard-coded default prefix"
    );
}
