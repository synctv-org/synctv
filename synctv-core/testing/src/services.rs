//! Service factory helpers for tests

use crate::constants;
use std::sync::Arc;
use synctv_core::cache::{KeyBuilder, UsernameCache};
use synctv_core::config::PasswordComplexityConfig;
use synctv_core::repository::SettingsRepository;
use synctv_core::service::{
    auth::{
        brute_force::InMemoryAttemptTracker, jwt::JwtService,
        token_blacklist::InMemoryTokenBlacklistStore,
    },
    rate_limit::RequestRateLimiterService,
    BruteForceProtection, BruteForceProtectionService, RateLimiter, RoomService, SettingsRegistry,
    SettingsService, StreamingPublishKeyService, TokenBlacklistStore, UserService,
    WebSocketTicketService, WsTicketService,
};

/// Creates a JWT service for testing
///
/// Uses a fixed test secret key. Do not use in production!
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_jwt_service;
///
/// let jwt_service = create_test_jwt_service();
/// let token = jwt_service.sign_token(user_id, UserRole::User, TokenType::Access)?;
/// ```
#[must_use]
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
/// ```text
/// let jwt_service = create_test_jwt_service_with_secret("my-custom-secret-32-chars-long!!");
/// ```
#[must_use]
pub fn create_test_jwt_service_with_secret(secret: &str) -> JwtService {
    JwtService::new(secret).expect("Failed to create JWT service")
}

/// Creates a brute force protection service for testing
///
/// Uses in-memory tracking with test-specific thresholds.
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_brute_force_protection;
///
/// let protection = create_test_brute_force_protection();
/// protection.record_failure("user", ip).await?;
/// ```
#[must_use]
pub fn create_test_brute_force_protection() -> BruteForceProtection {
    BruteForceProtection::in_memory("test".to_string())
}

/// Creates a brute-force protection service trait object for testing.
#[must_use]
pub fn create_test_brute_force_protection_service() -> Arc<dyn BruteForceProtectionService> {
    Arc::new(create_test_brute_force_protection())
}

/// Creates an attempt tracker for testing
///
/// Uses test-specific capacity and TTL values.
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_attempt_tracker;
///
/// let tracker = create_test_attempt_tracker();
/// tracker.record_failure("user", now, ttl).await?;
/// ```
#[must_use]
pub fn create_test_attempt_tracker() -> InMemoryAttemptTracker {
    InMemoryAttemptTracker::new(1000, 900)
}

/// Creates a token blacklist store for testing
///
/// Uses in-memory storage with test-specific capacity and TTL values.
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_token_blacklist_store;
///
/// let store = create_test_token_blacklist_store();
/// store.blacklist("jti:123", ttl).await?;
/// ```
#[must_use]
pub fn create_test_token_blacklist_store() -> InMemoryTokenBlacklistStore {
    InMemoryTokenBlacklistStore::new(
        constants::token_blacklist::CAPACITY as u64,
        constants::token_blacklist::SHORT_TTL_SECS,
        constants::token_blacklist::LONG_TTL_SECS,
    )
}

/// Creates a token blacklist store trait object for testing.
#[must_use]
pub fn create_test_token_blacklist_store_service() -> Arc<dyn TokenBlacklistStore> {
    Arc::new(create_test_token_blacklist_store())
}

/// Creates a `UserService` with in-memory test dependencies.
#[must_use]
pub fn create_test_user_service(pool: sqlx::PgPool) -> UserService {
    let mut service = UserService::new(
        pool,
        create_test_jwt_service(),
        UsernameCache::local_only("test:username:".to_string(), 128, 60),
        PasswordComplexityConfig::default(),
        create_test_token_blacklist_store_service(),
        KeyBuilder::new("test"),
        create_test_brute_force_protection_service(),
    );
    service.enable_password_registration_for_tests();
    service
}

/// Creates a `RoomService` with in-memory test dependencies where possible.
#[must_use]
pub fn create_test_room_service(pool: sqlx::PgPool) -> RoomService {
    let mut service = RoomService::new(pool.clone(), create_test_user_service(pool.clone()));
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool,
    ));
    service.set_settings_registry(Arc::new(SettingsRegistry::new(settings_service)));
    service
}

/// Creates a request rate limiter trait object for testing.
#[must_use]
pub fn create_test_request_rate_limiter(prefix: &str) -> Arc<dyn RequestRateLimiterService> {
    Arc::new(RateLimiter::local_only(prefix.to_string()))
}

/// Creates a WebSocket ticket service trait object for testing.
#[must_use]
pub fn create_test_websocket_ticket_service(
    ticket_ttl_secs: Option<u64>,
) -> Arc<dyn WebSocketTicketService> {
    Arc::new(WsTicketService::local_only(ticket_ttl_secs))
}

/// Creates a publish-key service trait object for testing.
#[must_use]
pub fn create_test_streaming_publish_key_service(
    token_ttl_hours: i64,
) -> Arc<dyn StreamingPublishKeyService> {
    Arc::new(synctv_core::service::PublishKeyService::new(
        create_test_jwt_service(),
        token_ttl_hours,
    ))
}
