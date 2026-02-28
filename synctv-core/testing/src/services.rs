//! Service factory helpers for tests

use synctv_core::{
    service::{
        auth::jwt::JwtService,
        BruteForceProtection,
        InMemoryAttemptTracker,
        InMemoryTokenBlacklistStore,
    },
};
use crate::constants;

/// Creates a JWT service for testing
///
/// Uses a fixed test secret key. Do not use in production!
///
/// # Example
///
/// ```ignore
/// use synctv_core_testing::create_test_jwt_service;
///
/// let jwt_service = create_test_jwt_service();
/// let token = jwt_service.sign_token(user_id, UserRole::User, TokenType::Access)?;
/// ```
pub fn create_test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

/// Creates a JWT service with custom secret for testing
///
/// # Arguments
///
/// * `secret` - JWT secret key (must be at least 32 bytes)
///
/// # Example
///
/// ```ignore
/// let jwt_service = create_test_jwt_service_with_secret("my-custom-secret-32-chars-long!!");
/// ```
pub fn create_test_jwt_service_with_secret(secret: &str) -> JwtService {
    JwtService::new(secret).expect("Failed to create JWT service")
}

/// Creates a brute force protection service for testing
///
/// Uses in-memory tracking with test-specific thresholds.
///
/// # Example
///
/// ```ignore
/// use synctv_core_testing::create_test_brute_force_protection;
///
/// let protection = create_test_brute_force_protection();
/// protection.record_failure("user", ip).await?;
/// ```
pub fn create_test_brute_force_protection() -> BruteForceProtection {
    BruteForceProtection::in_memory("test".to_string())
}

/// Creates an attempt tracker for testing
///
/// Uses test-specific capacity and TTL values.
///
/// # Example
///
/// ```ignore
/// use synctv_core_testing::create_test_attempt_tracker;
///
/// let tracker = create_test_attempt_tracker();
/// tracker.record_failure("user", now, ttl).await?;
/// ```
pub fn create_test_attempt_tracker() -> InMemoryAttemptTracker {
    InMemoryAttemptTracker::new(1000, 900)
}

/// Creates a token blacklist store for testing
///
/// Uses in-memory storage with test-specific capacity and TTL values.
///
/// # Example
///
/// ```ignore
/// use synctv_core_testing::create_test_token_blacklist_store;
///
/// let store = create_test_token_blacklist_store();
/// store.blacklist("jti:123", ttl).await?;
/// ```
pub fn create_test_token_blacklist_store() -> InMemoryTokenBlacklistStore {
    InMemoryTokenBlacklistStore::new(
        constants::token_blacklist::CAPACITY as u64,
        constants::token_blacklist::SHORT_TTL_SECS,
        constants::token_blacklist::LONG_TTL_SECS,
    )
}
