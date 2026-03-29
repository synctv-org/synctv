//! Shared helper functions for room service tests
//!
//! This module provides common utilities used across room-related tests
//! to reduce code duplication and improve maintainability.
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{User, UserId, UserRole, UserStatus},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};

/// Creates a `UserService` for testing
///
/// Uses test-specific configurations for JWT, caching, and brute force protection.
#[must_use]
pub fn make_user_service(pool: PgPool) -> UserService {
    // 32-byte secret for HS256
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

/// Creates a `RoomService` for testing
///
/// Convenience function that creates both `UserService` and `RoomService`.
#[must_use]
pub fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    let mut svc = RoomService::new(pool, user_service);
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

/// Creates a test User with default values
///
/// # Arguments
///
/// * `username` - The username for the test user
///
/// # Example
///
/// ```ignore
/// let user = make_test_user("alice");
/// let user = user_repo.create(&user).await.unwrap();
/// ```
#[must_use]
pub fn make_test_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Creates a test User with custom fields
///
/// # Arguments
///
/// * `username` - The username for the test user
/// * `role` - The user role (default: `UserRole::User`)
/// * `status` - The user status (default: `UserStatus::Active`)
///
/// # Example
///
/// ```ignore
/// let admin = make_test_user_with_role("bob", UserRole::Admin);
/// let admin = user_repo.create(&admin).await.unwrap();
/// ```
#[must_use]
pub fn make_test_user_with_role(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Creates a test User with inactive status
///
/// Useful for testing banned/suspended user scenarios.
#[must_use]
pub fn make_test_user_inactive(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Banned,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}
