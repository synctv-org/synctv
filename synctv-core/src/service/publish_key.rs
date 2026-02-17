//! Publish key generation for RTMP live streaming
//!
//! Generates JWT tokens for RTMP push authentication.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    models::{MediaId, RoomId, UserId},
    service::auth::JwtService,
    Error, Result,
};

/// Generated publish key for RTMP streaming
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishKey {
    /// JWT token for RTMP authentication
    pub token: String,
    /// Room ID
    pub room_id: String,
    /// Media ID (stream ID)
    pub media_id: String,
    /// User ID who requested the key
    pub user_id: String,
    /// Expiration timestamp
    pub expires_at: i64,
}

/// Claims for RTMP publish token
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishClaims {
    /// Room ID
    pub room_id: String,
    /// Media ID
    pub media_id: String,
    /// User ID
    pub user_id: String,
    /// Permission to start live stream
    pub perm_start_live: bool,
    /// Issued at timestamp
    pub iat: i64,
    /// Expiration timestamp
    pub exp: i64,
    /// JWT ID (unique token identifier)
    pub jti: String,
}

/// Publish key service for generating RTMP streaming tokens
#[derive(Clone)]
pub struct PublishKeyService {
    jwt_service: JwtService,
    token_ttl_hours: i64,
}

impl std::fmt::Debug for PublishKeyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishKeyService")
            .field("token_ttl_hours", &self.token_ttl_hours)
            .finish()
    }
}

impl PublishKeyService {
    /// Create a new publish key service
    #[must_use] 
    pub const fn new(jwt_service: JwtService, token_ttl_hours: i64) -> Self {
        Self {
            jwt_service,
            token_ttl_hours,
        }
    }

    /// Create a new publish key service with default TTL (24 hours)
    #[must_use] 
    pub const fn with_default_ttl(jwt_service: JwtService) -> Self {
        Self::new(jwt_service, 24)
    }

    /// Generate a publish key for RTMP streaming
    ///
    /// # Arguments
    /// * `room_id` - Room ID where the stream will be published
    /// * `media_id` - Media ID (stream identifier)
    /// * `user_id` - User ID requesting the publish key
    ///
    /// # Returns
    /// A `PublishKey` containing the JWT token and metadata
    pub async fn generate_publish_key(
        &self,
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
    ) -> Result<PublishKey> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Internal(format!("Time error: {e}")))?
            .as_secs() as i64;

        let exp = now + (self.token_ttl_hours * 3600);

        // Create claims
        let claims = PublishClaims {
            room_id: room_id.as_str().to_string(),
            media_id: media_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
            perm_start_live: true,
            iat: now,
            exp,
            jti: nanoid::nanoid!(32),
        };

        // Serialize claims to JSON
        let claims_json = serde_json::to_value(&claims)
            .map_err(|e| Error::Internal(format!("Failed to serialize claims: {e}")))?;

        // Sign with JWT service (using RS256)
        let token = self
            .jwt_service
            .sign_custom(&claims_json)?;

        Ok(PublishKey {
            token,
            room_id: room_id.as_str().to_string(),
            media_id: media_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
            expires_at: exp,
        })
    }

    /// Validate a publish key token
    ///
    /// # Arguments
    /// * `token` - The JWT token to validate
    ///
    /// # Returns
    /// The validated claims if the token is valid and not expired
    pub async fn validate_publish_key(&self, token: &str) -> Result<PublishClaims> {
        // Verify JWT signature and expiration
        let claims_value = self
            .jwt_service
            .verify_custom(token)?;

        // Deserialize claims
        let claims: PublishClaims = serde_json::from_value(claims_value)
            .map_err(|e| Error::Authentication(format!("Invalid token format: {e}")))?;

        // Check expiration
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Internal(format!("Time error: {e}")))?
            .as_secs() as i64;

        if now > claims.exp {
            return Err(Error::Authentication("Token has expired".to_string()));
        }

        // Verify permission
        if !claims.perm_start_live {
            return Err(Error::Authorization(
                "Token does not have START_LIVE permission".to_string(),
            ));
        }

        Ok(claims)
    }

    /// Verify a publish key for a specific room/media
    ///
    /// # Arguments
    /// * `token` - The JWT token
    /// * `room_id` - Expected room ID
    /// * `media_id` - Expected media ID
    ///
    /// # Returns
    /// The user ID if the token is valid for this room/media
    pub async fn verify_publish_key_for_stream(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<UserId> {
        let claims = self.validate_publish_key(token).await?;

        // Verify room and media match
        if claims.room_id != room_id.as_str() {
            return Err(Error::Authorization(format!(
                "Token room mismatch: expected {}, got {}",
                room_id.as_str(),
                claims.room_id
            )));
        }

        if claims.media_id != media_id.as_str() {
            return Err(Error::Authorization(format!(
                "Token media mismatch: expected {}, got {}",
                media_id.as_str(),
                claims.media_id
            )));
        }

        Ok(UserId::from_string(claims.user_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::auth::JwtService;

    fn create_jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap()
    }

    fn create_publish_key_service() -> PublishKeyService {
        let jwt = create_jwt_service();
        PublishKeyService::new(jwt, 24)
    }

    fn create_publish_key_service_with_ttl(ttl_hours: i64) -> PublishKeyService {
        let jwt = create_jwt_service();
        PublishKeyService::new(jwt, ttl_hours)
    }

    // ========== Construction ==========

    #[test]
    fn test_publish_key_service_new() {
        let service = create_publish_key_service();
        let debug = format!("{:?}", service);
        assert!(debug.contains("token_ttl_hours"));
        assert!(debug.contains("24"));
    }

    #[test]
    fn test_publish_key_service_with_default_ttl() {
        let jwt = create_jwt_service();
        let service = PublishKeyService::with_default_ttl(jwt);
        let debug = format!("{:?}", service);
        assert!(debug.contains("24"));
    }

    // ========== Generate Publish Key ==========

    #[tokio::test]
    async fn test_generate_publish_key_returns_valid_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
            .await
            .unwrap();

        assert!(!key.token.is_empty());
        assert_eq!(key.room_id, room_id.as_str());
        assert_eq!(key.media_id, media_id.as_str());
        assert_eq!(key.user_id, user_id.as_str());
        assert!(key.expires_at > 0);
    }

    #[tokio::test]
    async fn test_generate_publish_key_expiration_matches_ttl() {
        let service = create_publish_key_service_with_ttl(2);
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(room_id, media_id, user_id)
            .await
            .unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Expiration should be approximately 2 hours from now
        let expected_exp = now + (2 * 3600);
        let diff = (key.expires_at - expected_exp).abs();
        assert!(diff < 5, "Expiration time is off by more than 5 seconds: diff={diff}");
    }

    // ========== Validate Publish Key ==========

    #[tokio::test]
    async fn test_validate_publish_key_valid_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
            .await
            .unwrap();

        let claims = service.validate_publish_key(&key.token).await.unwrap();

        assert_eq!(claims.room_id, room_id.as_str());
        assert_eq!(claims.media_id, media_id.as_str());
        assert_eq!(claims.user_id, user_id.as_str());
        assert!(claims.perm_start_live);
    }

    #[tokio::test]
    async fn test_validate_publish_key_invalid_token() {
        let service = create_publish_key_service();
        let result = service.validate_publish_key("invalid.token.here").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_publish_key_wrong_secret() {
        let service1 = create_publish_key_service();
        let service2 = PublishKeyService::new(
            JwtService::new("different-secret-key-for-tests-abcdef-long-enough-1234567890").unwrap(),
            24,
        );

        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service1
            .generate_publish_key(room_id, media_id, user_id)
            .await
            .unwrap();

        let result = service2.validate_publish_key(&key.token).await;
        assert!(result.is_err());
    }

    // ========== Verify Publish Key For Stream ==========

    #[tokio::test]
    async fn test_verify_publish_key_for_stream_matching() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
            .await
            .unwrap();

        let returned_user_id = service
            .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
            .await
            .unwrap();

        assert_eq!(returned_user_id, user_id);
    }

    #[tokio::test]
    async fn test_verify_publish_key_for_stream_wrong_room() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();
        let wrong_room_id = RoomId::new();

        let key = service
            .generate_publish_key(room_id, media_id.clone(), user_id)
            .await
            .unwrap();

        let result = service
            .verify_publish_key_for_stream(&key.token, &wrong_room_id, &media_id)
            .await;
        assert!(result.is_err());
        if let Err(Error::Authorization(msg)) = result {
            assert!(msg.contains("room mismatch"));
        } else {
            panic!("Expected Authorization error with room mismatch");
        }
    }

    #[tokio::test]
    async fn test_verify_publish_key_for_stream_wrong_media() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();
        let wrong_media_id = MediaId::new();

        let key = service
            .generate_publish_key(room_id.clone(), media_id, user_id)
            .await
            .unwrap();

        let result = service
            .verify_publish_key_for_stream(&key.token, &room_id, &wrong_media_id)
            .await;
        assert!(result.is_err());
        if let Err(Error::Authorization(msg)) = result {
            assert!(msg.contains("media mismatch"));
        } else {
            panic!("Expected Authorization error with media mismatch");
        }
    }

    // ========== PublishClaims and PublishKey structs ==========

    #[test]
    fn test_publish_claims_serialization() {
        let claims = PublishClaims {
            room_id: "room123".to_string(),
            media_id: "media456".to_string(),
            user_id: "user789".to_string(),
            perm_start_live: true,
            iat: 1000,
            exp: 2000,
            jti: "unique-id".to_string(),
        };

        let json = serde_json::to_string(&claims).unwrap();
        let back: PublishClaims = serde_json::from_str(&json).unwrap();

        assert_eq!(back.room_id, "room123");
        assert_eq!(back.media_id, "media456");
        assert_eq!(back.user_id, "user789");
        assert!(back.perm_start_live);
        assert_eq!(back.iat, 1000);
        assert_eq!(back.exp, 2000);
        assert_eq!(back.jti, "unique-id");
    }

    #[test]
    fn test_publish_key_serialization() {
        let key = PublishKey {
            token: "jwt.token.here".to_string(),
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            user_id: "user1".to_string(),
            expires_at: 9999,
        };

        let json = serde_json::to_string(&key).unwrap();
        let back: PublishKey = serde_json::from_str(&json).unwrap();

        assert_eq!(back.token, "jwt.token.here");
        assert_eq!(back.room_id, "room1");
        assert_eq!(back.expires_at, 9999);
    }

    // ========== Unique JTI per token ==========

    #[tokio::test]
    async fn test_generate_publish_key_unique_jti() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key1 = service
            .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
            .await
            .unwrap();
        let key2 = service
            .generate_publish_key(room_id, media_id, user_id)
            .await
            .unwrap();

        // Tokens should be different (different JTI)
        assert_ne!(key1.token, key2.token);
    }
}
