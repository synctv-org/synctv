//! WebSocket ticket Redis tests
//!
//! Tests the RedisTicketStore backend via testcontainers:
//! roundtrip, one-time use, room mismatch, TTL expiry, concurrent consumption.
//!
//! Run with: cargo test --test ws_ticket_redis_tests

use synctv_core::models::{RoomId, UserId};
use synctv_core::service::WsTicketService;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const REDIS_VERSION: &str = "7-alpine";

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, redis::aio::ConnectionManager) {
    let container = Redis::default()
        .with_tag(REDIS_VERSION)
        .start()
        .await
        .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{}", port);
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create connection manager");
    (container, conn)
}

fn user_id(id: &str) -> UserId {
    UserId::from_string(id.to_string())
}

fn room_id(id: &str) -> RoomId {
    RoomId::from_string(id.to_string())
}

#[tokio::test]
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
    assert!(result2.is_err(), "Second consumption should fail (one-time use)");
}

#[tokio::test]
async fn test_redis_ticket_room_mismatch_rejected() {
    let (_container, conn) = start_redis().await;
    let service = WsTicketService::with_redis(conn, Some(30));

    let uid = user_id("user_rm_1");
    let room_a = room_id("room_a");
    let room_b = room_id("room_b");

    let ticket = service.create_ticket(&uid, &room_a, 0).await.unwrap();

    // Try to consume with wrong room
    let result = service.validate_and_consume(&ticket, &room_b).await;
    assert!(result.is_err(), "Ticket for room A should not work for room B");
}

#[tokio::test]
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
        handles.push(tokio::spawn(async move {
            s.validate_and_consume(&t, &r).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(successes, 1, "Exactly 1 of 10 concurrent consumers should succeed");
    assert_eq!(failures, 9, "9 consumers should fail");
}
