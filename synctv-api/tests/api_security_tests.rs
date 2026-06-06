//! API Security Tests //!
//! Tests for API security behavior:
//! - Guest-token validation must use GuestTokenValidator (blacklist check)
//! - sqlx::Error must not leak DB details in gRPC responses

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::cache::KeyBuilder;
use synctv_core::models::RoomId;
use synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore;
use synctv_core::service::auth::{GuestTokenValidator, JwtService, TokenBlacklistStore};
use synctv_core::Error;

/// A blacklisted guest token MUST be rejected by validate_async.
/// This tests the core requirement that shared guest-token validation must use
/// GuestTokenValidator::validate_async() (which checks blacklist) instead of
/// just jwt_service.verify_guest_token() (which only checks JWT signature).
#[tokio::test]
async fn test_blacklisted_guest_token_rejected_by_validator() {
    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone(), blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // First validate succeeds
    let claims = validator.validate_async(&token).await.unwrap();
    assert!(claims.is_guest());

    // Blacklist the token
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();

    // Now validate_async must reject it
    let result = validator.validate_async(&token).await;
    assert!(result.is_err(), "Blacklisted guest token must be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("revoked"),
        "Error message should indicate revocation, got: {err_msg}"
    );
}

/// Shared guest-token validation must use validate_async (not
/// verify_guest_token). We verify this by checking that the
/// GuestTokenValidator path catches blacklisted tokens.
#[tokio::test]
async fn test_non_blacklisted_guest_token_passes() {
    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone(), blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // Not blacklisted - should pass
    let result = validator.validate_async(&token).await;
    assert!(result.is_ok(), "Non-blacklisted guest token should pass");
}

#[tokio::test]
async fn test_guest_blacklist_storage_error_surfaces_service_unavailable() {
    struct FailingBlacklistStore;

    #[async_trait::async_trait]
    impl TokenBlacklistStore for FailingBlacklistStore {
        async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool, Error> {
            Err(Error::Internal("blacklist backend unavailable".to_string()))
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<(), Error> {
            Ok(())
        }

        async fn blacklist_if_not_exists(&self, _key: &str, _ttl_secs: u64) -> Result<bool, Error> {
            Ok(false)
        }

        async fn get_family_revoked_at_checked(&self, _key: &str) -> Result<Option<i64>, Error> {
            Ok(None)
        }

        async fn set_family_revoked(
            &self,
            _key: &str,
            _timestamp: i64,
            _ttl_secs: u64,
        ) -> Result<(), Error> {
            Ok(())
        }
    }

    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FailingBlacklistStore);
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone(), blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    let err = validator
        .validate_async(&token)
        .await
        .expect_err("storage failures must fail closed");

    assert!(
        matches!(err, Error::ServiceUnavailable(ref msg) if msg.contains("temporarily unavailable")),
        "guest token validator must surface service unavailability, got: {err}"
    );
}

/// Internal errors (including sqlx::Error) must be sanitized before
/// returning to gRPC clients. The map_api_error function should return
/// a generic "Internal error" message, not the raw error string.
#[test]
fn test_api_error_internal_sanitized_for_grpc() {
    use synctv_api::impls::ApiError;

    // Simulate a sqlx::Error being converted to ApiError::Internal
    let api_err = ApiError::Internal(
        "error returned from database: connection refused (os error 111)".to_string(),
    );

    // The proto error message should be sanitized
    let proto_err = api_err.to_proto_error();
    assert_eq!(
        proto_err.message, "Internal error",
        "Internal errors must be sanitized, not expose DB details"
    );
    assert!(
        !proto_err.message.contains("connection"),
        "DB connection details must not leak"
    );
}

// Test helpers

fn create_test_jwt_service() -> Arc<JwtService> {
    Arc::new(
        JwtService::new("test-secret-for-api-security-tests-that-is-long-enough-1234567890")
            .unwrap(),
    )
}
