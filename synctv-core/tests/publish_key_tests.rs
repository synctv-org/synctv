//! Publish key service tests
//!
//! Tests token validation, expiration, and JTI store deduplication.
//!
//! Run with: cargo test --test `publish_key_tests`
//! Run Docker tests: cargo test --test `publish_key_tests` -- --ignored
#![allow(clippy::unwrap_used)]

use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::service::{
    auth::JwtService,
    publish_key::{InMemoryJtiStore, PublishKeyService},
    JtiStore,
};
use synctv_core_testing::{start_redis_with_client, test_redis_key_prefix};

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap()
}

fn create_service() -> PublishKeyService {
    PublishKeyService::new(create_jwt_service(), 24)
}

fn create_service_with_short_ttl() -> PublishKeyService {
    // 0-hour TTL to generate already-expired tokens for testing
    PublishKeyService::new(create_jwt_service(), 0)
}

// ============================================================================
// Token expiration tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_validate_expired_token_rejected() {
    // Create a service with 0-hour TTL -- tokens expire at creation time
    let service = create_service_with_short_ttl();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(room_id, media_id, user_id)
        .await
        .unwrap();

    // Wait a moment to ensure the token is past its expiration
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    let result = service.validate_publish_key(&key.token).await;
    assert!(result.is_err(), "Expired token should be rejected");
    if let Err(synctv_core::Error::Authentication(msg)) = result {
        assert!(
            msg.contains("expired") || msg.contains("Expired"),
            "Error should mention expiration, got: {msg}"
        );
    }
}

// ============================================================================
// InMemoryJtiStore tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_in_memory_jti_store_first_claim_succeeds() {
    let store = InMemoryJtiStore::new(300);
    let result = store.try_claim("jti_1", 300).await.unwrap();
    assert!(result, "First claim should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_in_memory_jti_store_duplicate_returns_false() {
    let store = InMemoryJtiStore::new(300);

    let first = store.try_claim("jti_dup", 300).await.unwrap();
    assert!(first, "First claim should succeed");

    let second = store.try_claim("jti_dup", 300).await.unwrap();
    assert!(!second, "Duplicate claim should return false");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_in_memory_jti_store_different_jti_independent() {
    let store = InMemoryJtiStore::new(300);

    let a = store.try_claim("jti_a", 300).await.unwrap();
    let b = store.try_claim("jti_b", 300).await.unwrap();

    assert!(a);
    assert!(b, "Different JTIs should be independent");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_in_memory_jti_store_is_claimed() {
    let store = InMemoryJtiStore::new(300);

    assert!(
        !store.is_claimed("jti_x").await,
        "Unclaimed JTI should return false"
    );

    store.try_claim("jti_x", 300).await.unwrap();
    assert!(
        store.is_claimed("jti_x").await,
        "Claimed JTI should return true"
    );
}

// ============================================================================
// PublishKeyService single-use enforcement
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_publish_key_single_use() {
    let service = create_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(room_id, media_id, user_id)
        .await
        .unwrap();

    // First use should succeed
    let result = service.validate_publish_key(&key.token).await;
    assert!(result.is_ok(), "First validation should succeed");

    // Second use should fail
    let result = service.validate_publish_key(&key.token).await;
    assert!(
        result.is_err(),
        "Second validation should fail (single-use)"
    );
    if let Err(synctv_core::Error::Authentication(msg)) = result {
        assert!(
            msg.contains("single-use"),
            "Expected single-use error, got: {msg}"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_publish_key_room_mismatch_does_not_consume_token() {
    let service = create_service();
    let room_id = RoomId::new();
    let wrong_room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .unwrap();

    let first_result = service
        .verify_publish_key_for_stream(&key.token, &wrong_room_id, &media_id)
        .await;
    assert!(
        matches!(first_result, Err(synctv_core::Error::Authorization(_))),
        "room mismatch should return authorization error"
    );

    let second_result = service
        .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
        .await;
    assert!(
        second_result.is_ok(),
        "room mismatch must not consume a valid single-use publish key"
    );
    assert_eq!(second_result.unwrap(), user_id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_publish_key_user_validator_failure_does_not_consume_token() {
    let service = create_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .unwrap();

    let first_result = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| {
            Err(synctv_core::Error::Authorization("user banned".to_string()))
        })
        .await;
    assert!(
        matches!(first_result, Err(synctv_core::Error::Authorization(_))),
        "user validator failure should propagate as authorization error"
    );

    let second_result = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| Ok(()))
        .await;
    assert!(
        second_result.is_ok(),
        "user validator rejection must not consume a valid single-use publish key"
    );
    assert_eq!(second_result.unwrap(), user_id);
}

// ============================================================================
// Redis JTI store tests (require Docker)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_jti_store_cross_service_dedup() {
    use synctv_core::service::publish_key::RedisJtiStore;
    let (_container, client) = start_redis_with_client().await;
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis ConnectionManager");

    // Create two stores simulating two replicas
    let prefix = test_redis_key_prefix("jti-store");
    let store1 = RedisJtiStore::new(conn.clone(), prefix.clone(), 300);
    let store2 = RedisJtiStore::new(conn.clone(), prefix, 300);

    // Claim on store1
    let result1 = store1.try_claim("cross_jti", 300).await.unwrap();
    assert!(result1, "First claim on store1 should succeed");

    // Same JTI on store2 should fail (cross-replica dedup via Redis)
    let result2 = store2.try_claim("cross_jti", 300).await.unwrap();
    assert!(
        !result2,
        "Same JTI on store2 should fail (cross-replica dedup)"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_publish_key_service_with_redis_full_lifecycle() {
    use synctv_core::service::auth::JwtService;
    use synctv_core::service::publish_key::PublishKeyService;
    let (_container, client) = start_redis_with_client().await;
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis ConnectionManager");

    let jwt =
        JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap();
    let prefix = test_redis_key_prefix("publish-key");
    let service = PublishKeyService::with_redis(jwt, 24, conn, prefix);

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(room_id, media_id, user_id)
        .await
        .unwrap();

    // First validation should succeed
    let result = service.validate_publish_key(&key.token).await;
    assert!(
        result.is_ok(),
        "First validation with Redis JTI store should succeed"
    );

    // Second validation should fail (single-use via Redis SETNX)
    let result = service.validate_publish_key(&key.token).await;
    assert!(
        result.is_err(),
        "Second validation should fail (single-use via Redis)"
    );
    if let Err(synctv_core::Error::Authentication(msg)) = result {
        assert!(
            msg.contains("single-use"),
            "Expected single-use error, got: {msg}"
        );
    }
}
