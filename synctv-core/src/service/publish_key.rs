//! Publish key generation for RTMP live streaming
//!
//! Generates JWT tokens for RTMP push authentication.
//! Includes single-use enforcement to prevent TOCTOU races.
//!
//! In multi-replica deployments, JTI deduplication is backed by Redis
//! (SETNX with TTL) so the same token cannot be replayed on different nodes.

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
///
/// Includes single-use enforcement: each publish key `jti` can only be
/// consumed once by `validate_publish_key`. This prevents TOCTOU races
/// where the same token is replayed by a second RTMP connection before
/// the first session registers in Redis.
///
/// In multi-replica deployments, pass a Redis connection via
/// [`with_redis`](Self::with_redis) so that JTI deduplication is
/// cluster-wide. Without Redis, enforcement is per-node only.
#[derive(Clone)]
pub struct PublishKeyService {
    jwt_service: JwtService,
    token_ttl_hours: i64,
    /// In-memory set of consumed `jti` values (moka cache with TTL matching
    /// token lifetime). Prevents publish key replay within the same node.
    consumed_jtis: Arc<moka::future::Cache<String, ()>>,
    /// Optional Redis connection for cross-replica JTI deduplication.
    redis_conn: Option<redis::aio::ConnectionManager>,
    /// Redis key prefix (e.g. "synctv:").
    redis_key_prefix: String,
}

impl std::fmt::Debug for PublishKeyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishKeyService")
            .field("token_ttl_hours", &self.token_ttl_hours)
            .field("consumed_jtis_count", &self.consumed_jtis.entry_count())
            .field("redis_enabled", &self.redis_conn.is_some())
            .finish()
    }
}

impl PublishKeyService {
    /// Create a new publish key service (local-only JTI deduplication)
    #[must_use]
    pub fn new(jwt_service: JwtService, token_ttl_hours: i64) -> Self {
        // Cache TTL slightly exceeds token TTL so consumed jtis remain tracked
        // until their tokens are definitely expired.
        let cache_ttl_secs = (token_ttl_hours as u64).saturating_mul(3600).saturating_add(300);
        Self {
            jwt_service,
            token_ttl_hours,
            consumed_jtis: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(100_000)
                    .time_to_live(Duration::from_secs(cache_ttl_secs))
                    .build(),
            ),
            redis_conn: None,
            redis_key_prefix: String::new(),
        }
    }

    /// Create a new publish key service with default TTL (24 hours)
    #[must_use]
    pub fn with_default_ttl(jwt_service: JwtService) -> Self {
        Self::new(jwt_service, 24)
    }

    /// Enable Redis-backed JTI deduplication for multi-replica deployments.
    ///
    /// When set, `validate_publish_key` uses Redis SETNX so a JTI consumed
    /// on one node is rejected on all other nodes.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: String) -> Self {
        self.redis_conn = Some(conn);
        self.redis_key_prefix = key_prefix;
        self
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

    /// Validate a publish key token (single-use).
    ///
    /// Each publish key can only be validated once. Subsequent calls with the
    /// same token (same `jti`) will fail with an authentication error. This
    /// prevents TOCTOU races where the same publish key is used by two
    /// concurrent RTMP connections.
    ///
    /// # Arguments
    /// * `token` - The JWT token to validate
    ///
    /// # Returns
    /// The validated claims if the token is valid, not expired, and not already consumed
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

        // Single-use enforcement: check if the jti has already been consumed.
        // If so, reject the token to prevent replay attacks.
        //
        // 1. Fast path: check local moka cache (covers same-node replay).
        if self.consumed_jtis.contains_key(&claims.jti) {
            return Err(Error::Authentication(
                "Publish key has already been used (single-use token)".to_string(),
            ));
        }

        // 2. Cross-replica check: use Redis SETNX so a JTI consumed on any
        //    node is rejected cluster-wide. If Redis is unavailable, fall
        //    back to local-only enforcement with a warning.
        if let Some(ref conn) = self.redis_conn {
            let redis_key = format!("{}publish_key:jti:{}", self.redis_key_prefix, claims.jti);
            // TTL matches token TTL + 5 min buffer (same as moka cache).
            let ttl_secs = (claims.exp - claims.iat).max(0) as u64 + 300;
            let mut conn = conn.clone();
            match conn.set_nx::<_, _, bool>(&redis_key, 1).await {
                Ok(true) => {
                    // We won the SETNX race -- set the TTL and proceed.
                    let _: std::result::Result<(), _> = conn.expire(&redis_key, ttl_secs as i64).await;
                }
                Ok(false) => {
                    // Another replica already consumed this JTI.
                    return Err(Error::Authentication(
                        "Publish key has already been used (single-use token)".to_string(),
                    ));
                }
                Err(e) => {
                    // Redis unavailable -- fall back to local-only enforcement.
                    tracing::warn!(
                        jti = %claims.jti,
                        "Redis unavailable for JTI dedup, using local-only enforcement: {e}"
                    );
                }
            }
        }

        // 3. Add to local moka cache for fast subsequent checks on this node.
        self.consumed_jtis.insert(claims.jti.clone(), ()).await;

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

    // ========== Single-use enforcement ==========

    #[tokio::test]
    async fn test_validate_publish_key_single_use() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(room_id, media_id, user_id)
            .await
            .unwrap();

        // First validation succeeds
        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_ok());

        // Second validation with same token fails (jti already consumed)
        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_err());
        if let Err(Error::Authentication(msg)) = result {
            assert!(msg.contains("single-use"), "Expected single-use error, got: {msg}");
        } else {
            panic!("Expected Authentication error for replay");
        }
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
