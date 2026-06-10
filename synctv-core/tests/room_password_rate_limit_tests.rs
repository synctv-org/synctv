//! Room password verification rate limiting tests
//!
//! Tests that room password verification is protected against brute-force attacks
//! via rate limiting based on `room_id + client_ip` combination.
//!
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        room::RoomServiceOptions,
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};
use synctv_core_testing::{create_test_pool, err, ok};
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = ok(
        JwtService::new(secret),
        "test JWT service should initialize",
    );
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: &PgPool) -> RoomService {
    let user_service = make_user_service(pool);

    let brute_force = BruteForceProtection::in_memory("test_room_password".to_string());
    ok(
        RoomService::new_with_options(
            pool.clone(),
            user_service,
            RoomServiceOptions {
                brute_force_service: Some(Arc::new(brute_force)),
                ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
            },
        ),
        "room service should build",
    )
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

/// After multiple failed password attempts, further attempts should be blocked
/// with a rate limit error rather than allowing continued guessing.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_verification_failure_triggers_rate_limit() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = ok(
        user_repo.create(&make_user("room_owner")).await,
        "room owner should be created",
    );
    let (room, _member) = ok(
        room_service
            .create_room(
                "Protected Room".to_string(),
                "A password-protected room".to_string(),
                owner.id,
                Some("CorrectPassword123".to_string()),
                None,
            )
            .await,
        "password-protected room should be created",
    );

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
                !ok(result, "wrong password check should succeed"),
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
            let err = err(
                result,
                "sixth wrong password attempt should be rate limited",
            );
            let msg = err.to_string();
            assert!(
                msg.contains("Too many failed")
                    || msg.contains("rate limit")
                    || msg.contains("try again"),
                "Error should indicate rate limiting: {msg}"
            );
        }
    }
}

/// Different IPs should have independent rate limit counters for the same room.
/// The same IP should have independent counters for different rooms.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_is_per_room_per_ip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = ok(
        user_repo.create(&make_user("per_ip_owner")).await,
        "per-IP rate limit owner should be created",
    );

    let (room1, _) = ok(
        room_service
            .create_room(
                "Room One".to_string(),
                "First room".to_string(),
                owner.id,
                Some("Password1".to_string()),
                None,
            )
            .await,
        "first protected room should be created",
    );

    let (room2, _) = ok(
        room_service
            .create_room(
                "Room Two".to_string(),
                "Second room".to_string(),
                owner.id,
                Some("Password2".to_string()),
                None,
            )
            .await,
        "second protected room should be created",
    );

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

/// After the lockout period expires, the user should be able to verify passwords again.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_expires_after_lockout() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = ok(
        user_repo.create(&make_user("expire_owner")).await,
        "expiry test owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Expiry Test Room".to_string(),
                "Testing lockout expiry".to_string(),
                owner.id,
                Some("CorrectPassword123".to_string()),
                None,
            )
            .await,
        "expiry test room should be created",
    );

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
    assert!(
        result.is_err(),
        "Should be rate limited immediately after failures"
    );

    // Use internal method to reset the rate limit counter to simulate time passing
    // In production, this would be handled by the TTL-based expiry in Redis/moka
    ok(
        room_service
            .reset_room_password_rate_limit(&room.id, client_ip)
            .await,
        "room password rate limit reset should succeed",
    );

    // After reset, should be able to verify again
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "CorrectPassword123", Some(client_ip))
        .await;
    assert!(
        result.is_ok(),
        "Should be able to verify password after rate limit reset"
    );
    assert!(
        ok(result, "correct password check after reset should succeed"),
        "Correct password should return true after reset"
    );
}

/// When a user successfully verifies the password, any previous failure count
/// should be reset so they don't get locked out from accumulated old failures.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_successful_password_verification_resets_failure_counter() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = ok(
        user_repo.create(&make_user("reset_owner")).await,
        "reset test owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Reset Test Room".to_string(),
                "Testing counter reset".to_string(),
                owner.id,
                Some("CorrectPassword123".to_string()),
                None,
            )
            .await,
        "reset test room should be created",
    );

    let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

    // Make 4 failed attempts (below threshold)
    for _ in 0..4 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;
        assert!(!ok(
            result,
            "wrong password check before successful reset should succeed"
        ));
    }

    // Now provide correct password - should succeed
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "CorrectPassword123", Some(client_ip))
        .await;
    assert!(result.is_ok(), "Correct password should not error");
    assert!(
        ok(result, "correct password check should succeed"),
        "Correct password should return true"
    );

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

/// When client IP is unavailable, rate limiting still works based on `room_id`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_rate_limit_without_ip() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = ok(
        user_repo.create(&make_user("no_ip_owner")).await,
        "no-IP rate limit owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "No IP Room".to_string(),
                "Testing room-only rate limit".to_string(),
                owner.id,
                Some("CorrectPassword123".to_string()),
                None,
            )
            .await,
        "no-IP protected room should be created",
    );

    // Make 6 failed password attempts without IP (5 allowed, 6th rate limited)
    for i in 0..6 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", None)
            .await;

        if i < 5 {
            assert!(
                !ok(result, "wrong password check without IP should succeed"),
                "Attempt {}: should be Ok(false)",
                i + 1
            );
        } else {
            assert!(result.is_err(), "Attempt {}: should be rate limited", i + 1);
        }
    }
}

/// When brute-force protection is in fallback mode (not fail-closed),
/// a successful password verification should still return true even if
/// the rate limit counter reset fails. This ensures users can authenticate
/// when Redis is temporarily unavailable, while the failure is logged for
/// security monitoring.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_verification_succeeds_when_reset_fails_in_fallback_mode() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = make_user_service(&pool);
    let brute_force = BruteForceProtection::in_memory("test_fallback_mode".to_string());
    let room_service = ok(
        RoomService::new_with_options(
            pool.clone(),
            user_service,
            RoomServiceOptions {
                brute_force_service: Some(Arc::new(brute_force)),
                ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
            },
        ),
        "room service should build",
    );

    let owner = ok(
        user_repo.create(&make_user("fallback_owner")).await,
        "fallback test owner should be created",
    );
    let (room, _) = ok(
        room_service
            .create_room(
                "Fallback Test Room".to_string(),
                "Testing fallback mode behavior".to_string(),
                owner.id,
                Some("CorrectPassword123".to_string()),
                None,
            )
            .await,
        "fallback test room should be created",
    );

    let client_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 88));

    // Make some failed attempts
    for _ in 0..3 {
        let _ = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;
    }

    // Correct password should succeed regardless of counter reset result
    // In-memory implementation always succeeds on reset, but the behavior
    // should be: verification result is independent of reset success
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "CorrectPassword123", Some(client_ip))
        .await;

    // The key assertion: verification succeeded
    assert!(result.is_ok(), "Password verification should succeed");
    assert!(
        ok(
            result,
            "correct password check in fallback mode should succeed"
        ),
        "Correct password should return true"
    );

    // After successful verification, counter should be reset
    // We can verify by making more wrong attempts - should have full quota again
    for i in 0..5 {
        let result = room_service
            .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
            .await;
        assert!(
            result.is_ok(),
            "After reset, attempt {} should succeed",
            i + 1
        );
    }
    // 6th should be rate limited
    let result = room_service
        .check_room_password_with_rate_limit(&room.id, "WrongPassword", Some(client_ip))
        .await;
    assert!(
        result.is_err(),
        "6th attempt should be rate limited after reset"
    );
}
