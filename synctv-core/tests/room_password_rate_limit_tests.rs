//! Room password verification rate limiting tests
//!
//! Tests that room password verification is protected against brute-force attacks
//! via rate limiting based on `room_id + client_ip` combination.
//!
//! ## Test Cases
//!
//! 1. Password verification failure triggers rate limiting
//! 2. Rate limiting is based on `room_id + client_ip` (not just room_id)
//! 3. After lockout expires, verification is allowed again
//!
//! Run with: cargo test -p synctv-core --test room_password_rate_limit_tests -- --nocapture

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{BruteForceProtection, JwtService},
    },
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    let mut room_service = RoomService::new(pool, user_service);

    // Set up brute-force protection for rate limiting tests
    let brute_force = BruteForceProtection::in_memory("test_room_password".to_string());
    room_service.set_brute_force_service(brute_force);

    room_service
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

// ========== Rate Limiting Tests ==========

/// Test 1: Password verification failure should trigger rate limiting
///
/// After multiple failed password attempts, further attempts should be blocked
/// with a rate limit error rather than allowing continued guessing.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_verification_failure_triggers_rate_limit() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner and a password-protected room
    let owner = user_repo.create(&make_user("room_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Protected Room".to_string(),
            "A password-protected room".to_string(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));

    // Make 6 password attempts (5 failed, 6th should be rate limited)
    for i in 0..6 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;

        // First 5 attempts should return false (password incorrect)
        // 6th attempt should trigger rate limiting and return an error
        if i < 5 {
            assert!(
                result.is_ok(),
                "Attempt {}: should be Ok(false) for wrong password",
                i + 1
            );
            assert!(
                !result.unwrap(),
                "Attempt {}: wrong password should return false",
                i + 1
            );
        } else {
            // 6th attempt should be rate limited
            assert!(
                result.is_err(),
                "Attempt {}: should be rate limited after 5 failures",
                i + 1
            );
            let err = result.unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("Too many failed") || msg.contains("rate limit") || msg.contains("try again"),
                "Error should indicate rate limiting: {msg}"
            );
        }
    }
}

/// Test 2: Rate limiting is based on `room_id + client_ip` combination
///
/// Different IPs should have independent rate limit counters for the same room.
/// The same IP should have independent counters for different rooms.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_is_per_room_per_ip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner
    let owner = user_repo.create(&make_user("per_ip_owner")).await.unwrap();

    // Create two password-protected rooms
    let (room1, _) = room_service
        .create_room(
            "Room One".to_string(),
            "First room".to_string(),
            owner.id.clone(),
            Some("Password1".to_string()),
            None,
        )
        .await
        .unwrap();

    let (room2, _) = room_service
        .create_room(
            "Room Two".to_string(),
            "Second room".to_string(),
            owner.id.clone(),
            Some("Password2".to_string()),
            None,
        )
        .await
        .unwrap();

    let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
    let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101));

    // Make 6 failed attempts from ip1 to room1 (5 allowed, 6th rate limited)
    for _ in 0..6 {
        let _ = room_service
            .check_room_password_with_rate_limit(&room1.id, "WrongPassword", Some(ip1))
            .await;
    }

    // ip1 should be rate limited for room1
    let result = room_service
        .check_room_password_with_rate_limit(&room1.id, "WrongPassword", Some(ip1))
        .await;
    assert!(result.is_err(), "ip1 should be rate limited for room1");

    // But ip2 should NOT be rate limited for room1 (different IP)
    let result = room_service
        .check_room_password_with_rate_limit(&room1.id, "WrongPassword", Some(ip2))
        .await;
    assert!(
        result.is_ok(),
        "ip2 should NOT be rate limited for room1 (different IP)"
    );

    // And ip1 should NOT be rate limited for room2 (different room)
    let result = room_service
        .check_room_password_with_rate_limit(&room2.id, "WrongPassword", Some(ip1))
        .await;
    assert!(
        result.is_ok(),
        "ip1 should NOT be rate limited for room2 (different room)"
    );
}

/// Test 3: Rate limit expires after lockout duration
///
/// After the lockout period expires, the user should be able to verify passwords again.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_expires_after_lockout() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner and a password-protected room
    let owner = user_repo.create(&make_user("expire_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "Expiry Test Room".to_string(),
            "Testing lockout expiry".to_string(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 200));

    // Make 5 failed password attempts to trigger rate limiting
    for _ in 0..5 {
        let _ = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;
    }

    // Verify rate limited
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
        .await;
    assert!(result.is_err(), "Should be rate limited immediately after failures");

    // Use internal method to reset the rate limit counter to simulate time passing
    // In production, this would be handled by the TTL-based expiry in Redis/moka
    room_service
        .reset_room_password_rate_limit(&room.id, client_ip)
        .await
        .expect("Reset should succeed");

    // After reset, should be able to verify again
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "CorrectPassword123", Some(client_ip))
        .await;
    assert!(
        result.is_ok(),
        "Should be able to verify password after rate limit reset"
    );
    assert!(
        result.unwrap(),
        "Correct password should return true after reset"
    );
}

/// Test 4: Successful password verification resets the failure counter
///
/// When a user successfully verifies the password, any previous failure count
/// should be reset so they don't get locked out from accumulated old failures.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_successful_password_verification_resets_failure_counter() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner and a password-protected room
    let owner = user_repo.create(&make_user("reset_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "Reset Test Room".to_string(),
            "Testing counter reset".to_string(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

    // Make 4 failed attempts (below threshold)
    for _ in 0..4 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;
        assert!(result.is_ok() && !result.unwrap());
    }

    // Now provide correct password - should succeed
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "CorrectPassword123", Some(client_ip))
        .await;
    assert!(result.is_ok(), "Correct password should not error");
    assert!(result.unwrap(), "Correct password should return true");

    // After successful verification, we should have more attempts available
    // (counter was reset, so we can fail 5 more times before lockout on 6th)
    for i in 0..6 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;

        if i < 5 {
            assert!(
                result.is_ok(),
                "Attempt {} after reset: should be Ok(false)",
                i + 1
            );
        } else {
            assert!(
                result.is_err(),
                "Attempt {} after reset: should be rate limited",
                i + 1
            );
        }
    }
}

/// Test 5: Rate limiting works without IP (room-only mode)
///
/// When client IP is not available, rate limiting should still work based on room_id only.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_without_ip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner and a password-protected room
    let owner = user_repo.create(&make_user("no_ip_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "No IP Room".to_string(),
            "Testing room-only rate limit".to_string(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Make 6 failed password attempts without IP (5 allowed, 6th rate limited)
    for i in 0..6 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", None)
            .await;

        if i < 5 {
            assert!(
                result.is_ok() && !result.unwrap(),
                "Attempt {}: should be Ok(false)",
                i + 1
            );
        } else {
            assert!(
                result.is_err(),
                "Attempt {}: should be rate limited",
                i + 1
            );
        }
    }
}
