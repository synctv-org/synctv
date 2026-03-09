//! Guest token validation with blacklist support
//!
//! This module provides a dedicated validator for guest tokens that includes
//! blacklist checking for token revocation. Guest tokens can be revoked:
//!
//! 1. **Individually** - by blacklisting the token's JTI (e.g., when a guest is kicked)
//! 2. **Room-wide** - by incrementing the room's guest version (e.g., when settings change)
//!
//! ## Architecture
//!
//! The validator uses the existing `TokenBlacklistStore` trait for storage,
//! reusing the same tiered architecture (L1 moka + optional L2 Redis + PG primary).

use std::sync::Arc;

use crate::{cache::KeyBuilder, service::TokenBlacklistStore, Error, Result};

use super::jwt::{GuestClaims, JwtService};

/// Guest token validator with blacklist support
///
/// Provides validation of guest tokens including:
/// - JWT signature and expiration verification
/// - Individual token blacklist check (by JTI)
/// - Room-level version check (by room guest version)
#[derive(Clone)]
pub struct GuestTokenValidator {
    jwt_service: Arc<JwtService>,
    token_blacklist: Option<Arc<dyn TokenBlacklistStore>>,
    key_builder: Option<KeyBuilder>,
}

impl GuestTokenValidator {
    /// Create a new guest token validator
    #[must_use]
    pub const fn new(jwt_service: Arc<JwtService>) -> Self {
        Self {
            jwt_service,
            token_blacklist: None,
            key_builder: None,
        }
    }

    /// Attach a [`TokenBlacklistStore`] and [`KeyBuilder`] for blacklist checking
    #[must_use]
    pub fn with_blacklist(
        mut self,
        store: Arc<dyn TokenBlacklistStore>,
        key_builder: KeyBuilder,
    ) -> Self {
        self.token_blacklist = Some(store);
        self.key_builder = Some(key_builder);
        self
    }

    /// Validate a guest token (sync version - JWT only, no blacklist check)
    ///
    /// This method only verifies the JWT signature and expiration.
    /// Use [`validate_async`] for full validation including blacklist check.
    ///
    /// # Arguments
    /// * `token` - The guest JWT token string
    ///
    /// # Returns
    /// The validated [`GuestClaims`] on success, or an error on failure.
    pub fn validate(&self, token: &str) -> Result<GuestClaims> {
        self.jwt_service.verify_guest_token(token)
    }

    /// Validate a guest token with blacklist check
    ///
    /// Performs the following checks in order:
    /// 1. JWT signature verification and expiration check
    /// 2. Token type verification (must be "guest")
    /// 3. JTI blacklist check (if blacklist is configured)
    ///
    /// # Arguments
    /// * `token` - The guest JWT token string
    ///
    /// # Returns
    /// The validated [`GuestClaims`] on success, or an error on failure.
    pub async fn validate_async(&self, token: &str) -> Result<GuestClaims> {
        // Step 1: Verify JWT signature and expiration
        let claims = self.jwt_service.verify_guest_token(token)?;

        // Step 2: Check JTI blacklist (if configured)
        if let (Some(store), Some(kb)) = (&self.token_blacklist, &self.key_builder) {
            let key = kb.guest_token_blacklist(&claims.jti);
            match store.is_blacklisted_checked(&key).await {
                Ok(true) => {
                    return Err(Error::Authentication(
                        "Guest token has been revoked".to_string(),
                    ));
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(
                        jti = %claims.jti,
                        error = %e,
                        "Guest token blacklist check failed due to storage error (fail-closed)"
                    );
                    return Err(Error::Authentication(
                        "Authentication service temporarily unavailable".to_string(),
                    ));
                }
            }
        }

        Ok(claims)
    }

    /// Validate a guest token with room version check
    ///
    /// In addition to the standard validation, this also checks that the
    /// token's guest version (`gv`) is >= the provided room guest version.
    /// This is a sync version that does not check the blacklist.
    ///
    /// # Arguments
    /// * `token` - The guest JWT token string
    /// * `current_room_guest_version` - The room's current guest version
    ///
    /// # Returns
    /// The validated [`GuestClaims`] on success, or an error on failure.
    pub fn validate_with_version(
        &self,
        token: &str,
        current_room_guest_version: i64,
    ) -> Result<GuestClaims> {
        let claims = self.validate(token)?;

        // Check room version - if the token's version is lower than the room's
        // current version, the token has been revoked room-wide
        if claims.gv < current_room_guest_version {
            return Err(Error::Authentication(
                "Guest token has been revoked for this room".to_string(),
            ));
        }

        Ok(claims)
    }

    /// Validate a guest token with both blacklist and room version check
    ///
    /// Performs all validation checks:
    /// 1. JWT signature verification and expiration check
    /// 2. JTI blacklist check (if blacklist is configured)
    /// 3. Room version check
    ///
    /// # Arguments
    /// * `token` - The guest JWT token string
    /// * `current_room_guest_version` - The room's current guest version
    ///
    /// # Returns
    /// The validated [`GuestClaims`] on success, or an error on failure.
    pub async fn validate_with_version_async(
        &self,
        token: &str,
        current_room_guest_version: i64,
    ) -> Result<GuestClaims> {
        let claims = self.validate_async(token).await?;

        // Check room version
        if claims.gv < current_room_guest_version {
            return Err(Error::Authentication(
                "Guest token has been revoked for this room".to_string(),
            ));
        }

        Ok(claims)
    }

    /// Blacklist a guest token by its JTI
    ///
    /// This is used to revoke individual guest tokens (e.g., when a guest is kicked).
    ///
    /// # Arguments
    /// * `jti` - The JWT ID of the token to blacklist
    /// * `ttl_secs` - Time-to-live in seconds (should match or exceed token's remaining lifetime)
    ///
    /// # Returns
    /// `Ok(())` on success, or an error if the blacklist store is not configured
    /// or the write fails.
    pub async fn blacklist_token(&self, jti: &str, ttl_secs: u64) -> Result<()> {
        match (&self.token_blacklist, &self.key_builder) {
            (Some(store), Some(kb)) => {
                let key = kb.guest_token_blacklist(jti);
                store.blacklist(&key, ttl_secs).await
            }
            _ => Err(Error::Internal(
                "Guest token blacklist store not configured".to_string(),
            )),
        }
    }

    /// Check if the blacklist store is configured
    #[must_use]
    pub fn has_blacklist(&self) -> bool {
        self.token_blacklist.is_some() && self.key_builder.is_some()
    }
}

impl std::fmt::Debug for GuestTokenValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestTokenValidator")
            .field("has_blacklist", &self.has_blacklist())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RoomId, UserId};
    use crate::service::auth::jwt::TokenType;
    use crate::service::auth::token_blacklist::InMemoryTokenBlacklistStore;

    struct FailingBlacklistStore;

    #[async_trait::async_trait]
    impl crate::service::TokenBlacklistStore for FailingBlacklistStore {
        async fn is_blacklisted(&self, _key: &str) -> bool {
            false
        }

        async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool> {
            Err(Error::Internal("blacklist backend unavailable".to_string()))
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn blacklist_if_not_exists(&self, _key: &str, _ttl_secs: u64) -> Result<bool> {
            Ok(false)
        }

        async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
            None
        }

        async fn set_family_revoked(
            &self,
            _key: &str,
            _timestamp: i64,
            _ttl_secs: u64,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn create_test_jwt_service() -> Arc<JwtService> {
        Arc::new(
            JwtService::new("test-secret-for-guest-validator-that-is-long-enough-1234567890")
                .unwrap(),
        )
    }

    fn create_test_validator_with_blacklist() -> GuestTokenValidator {
        let jwt = create_test_jwt_service();
        let blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
        let kb = KeyBuilder::new("test");
        GuestTokenValidator::new(jwt).with_blacklist(blacklist, kb)
    }

    fn create_test_validator_without_blacklist() -> GuestTokenValidator {
        GuestTokenValidator::new(create_test_jwt_service())
    }

    fn create_test_validator_with_failing_blacklist() -> GuestTokenValidator {
        let jwt = create_test_jwt_service();
        let blacklist = Arc::new(FailingBlacklistStore);
        let kb = KeyBuilder::new("test");
        GuestTokenValidator::new(jwt).with_blacklist(blacklist, kb)
    }

    #[tokio::test]
    async fn test_validate_valid_guest_token() {
        let validator = create_test_validator_without_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        let token = jwt.sign_guest_token(&room_id).unwrap();
        let claims = validator.validate_async(&token).await.unwrap();

        assert_eq!(claims.room_id(), room_id);
        assert!(claims.is_guest());
    }

    #[tokio::test]
    async fn test_validate_token_with_version_check() {
        let validator = create_test_validator_without_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        // Token with version 5
        let token = jwt.sign_guest_token_with_version(&room_id, 5).unwrap();
        let claims = validator
            .validate_with_version_async(&token, 5)
            .await
            .unwrap();
        assert_eq!(claims.gv, 5);

        // Version check passes when token version >= room version
        let claims = validator
            .validate_with_version_async(&token, 3)
            .await
            .unwrap();
        assert_eq!(claims.gv, 5);

        // Version check fails when token version < room version
        let result = validator.validate_with_version_async(&token, 10).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
    }

    #[tokio::test]
    async fn test_validate_non_guest_token_fails() {
        let validator = create_test_validator_without_blacklist();
        let jwt = create_test_jwt_service();
        let user_id = UserId::new();

        let access_token = jwt.sign_token(&user_id, TokenType::Access, 0).unwrap();
        let result = validator.validate_async(&access_token).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_blacklisted_token_fails() {
        let validator = create_test_validator_with_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        let token = jwt.sign_guest_token(&room_id).unwrap();
        let claims = jwt.verify_guest_token(&token).unwrap();

        // First, validation should succeed
        validator.validate_async(&token).await.unwrap();

        // Blacklist the token
        validator.blacklist_token(&claims.jti, 3600).await.unwrap();

        // Now validation should fail
        let result = validator.validate_async(&token).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::Authentication(msg) if msg.contains("revoked")));
    }

    #[tokio::test]
    async fn test_validate_blacklist_storage_error_fails_closed() {
        let validator = create_test_validator_with_failing_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        let token = jwt.sign_guest_token(&room_id).unwrap();
        let err = validator
            .validate_async(&token)
            .await
            .expect_err("storage errors must fail closed");

        assert!(
            matches!(err, Error::Authentication(msg) if msg.contains("temporarily unavailable"))
        );
    }

    #[tokio::test]
    async fn test_blacklist_without_store_fails() {
        let validator = create_test_validator_without_blacklist();

        let result = validator.blacklist_token("some_jti", 3600).await;

        assert!(result.is_err());
    }

    #[test]
    fn test_has_blacklist() {
        let with_blacklist = create_test_validator_with_blacklist();
        assert!(with_blacklist.has_blacklist());

        let without_blacklist = create_test_validator_without_blacklist();
        assert!(!without_blacklist.has_blacklist());
    }

    #[test]
    fn test_sync_validate_valid_guest_token() {
        let validator = create_test_validator_without_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        let token = jwt.sign_guest_token(&room_id).unwrap();
        let claims = validator.validate(&token).unwrap();

        assert_eq!(claims.room_id(), room_id);
        assert!(claims.is_guest());
    }

    #[test]
    fn test_sync_validate_with_version() {
        let validator = create_test_validator_without_blacklist();
        let jwt = create_test_jwt_service();
        let room_id = RoomId::new();

        let token = jwt.sign_guest_token_with_version(&room_id, 5).unwrap();

        // Version check passes
        let claims = validator.validate_with_version(&token, 3).unwrap();
        assert_eq!(claims.gv, 5);

        // Version check fails
        let result = validator.validate_with_version(&token, 10);
        assert!(result.is_err());
    }
}
