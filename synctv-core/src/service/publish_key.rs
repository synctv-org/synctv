//! Publish key generation for RTMP live streaming
//!
//! Generates JWT tokens for RTMP push authentication.
//! Includes single-use enforcement to prevent TOCTOU races.
//!
//! ## JTI Deduplication Backends
//!
//! - `RedisJtiStore`: Redis SETNX for cluster-wide deduplication
//! - `InMemoryJtiStore`: moka cache for per-node deduplication

use async_trait::async_trait;
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

// ============================================================================
// JtiStore trait
// ============================================================================

/// Backend for JTI (JWT Token ID) deduplication.
///
/// Ensures each publish key can only be consumed once. Implementations must
/// provide atomic "try-claim" semantics.
#[async_trait]
pub trait JtiStore: Send + Sync {
    /// Try to claim a JTI. Returns `true` if this is the first claim (success),
    /// `false` if the JTI was already claimed.
    ///
    /// `ttl_secs` is the lifetime for the JTI record (should match token lifetime + buffer).
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool>;

    /// Check if a JTI has been claimed (fast path, may be local-only).
    async fn is_claimed(&self, jti: &str) -> bool;

    /// A label for logging/debug purposes.
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis-backed JTI store for cluster-wide deduplication using SETNX.
pub struct RedisJtiStore {
    conn: redis::aio::ConnectionManager,
    key_prefix: String,
    /// Local moka cache for fast-path checks on the same node.
    local_cache: moka::future::Cache<String, ()>,
}

impl RedisJtiStore {
    #[must_use] 
    pub fn new(conn: redis::aio::ConnectionManager, key_prefix: String, cache_ttl_secs: u64) -> Self {
        Self {
            conn,
            key_prefix,
            local_cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(cache_ttl_secs))
                .build(),
        }
    }
}

#[async_trait]
impl JtiStore for RedisJtiStore {
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool> {
        // Fast path: already claimed locally
        if self.local_cache.contains_key(jti) {
            return Ok(false);
        }

        // Cross-replica check: atomic SET key value PX <ms> NX
        // Using a single SET command with NX and PX flags is atomic in Redis,
        // eliminating the TOCTOU gap between a separate SETNX + EXPIRE pair.
        let redis_key = format!("{}publish_key:jti:{}", self.key_prefix, jti);
        let mut conn = self.conn.clone();
        let ttl_ms = ttl_secs.saturating_mul(1000);
        let set_result: std::result::Result<Option<String>, _> = redis::cmd("SET")
            .arg(&redis_key)
            .arg(1i64)
            .arg("PX")
            .arg(ttl_ms)
            .arg("NX")
            .query_async(&mut conn)
            .await;
        match set_result {
            Ok(Some(_)) => {
                // SET returned OK — we claimed the key atomically with its TTL
                self.local_cache.insert(jti.to_string(), ()).await;
                Ok(true)
            }
            Ok(None) => {
                // SET returned nil — key already existed; JTI already claimed
                self.local_cache.insert(jti.to_string(), ()).await;
                Ok(false)
            }
            Err(e) => {
                // Redis unavailable -- fall back to local-only enforcement
                tracing::warn!(
                    jti = %jti,
                    "Redis unavailable for JTI dedup, using local-only enforcement: {e}"
                );
                if self.local_cache.contains_key(jti) {
                    Ok(false)
                } else {
                    self.local_cache.insert(jti.to_string(), ()).await;
                    Ok(true)
                }
            }
        }
    }

    async fn is_claimed(&self, jti: &str) -> bool {
        self.local_cache.contains_key(jti)
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

// ============================================================================
// In-memory implementation
// ============================================================================

/// In-memory JTI store using moka cache with TTL.
///
/// Per-node only: cannot detect replays on other nodes.
pub struct InMemoryJtiStore {
    cache: moka::future::Cache<String, ()>,
}

impl InMemoryJtiStore {
    #[must_use] 
    pub fn new(cache_ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(cache_ttl_secs))
                .build(),
        }
    }
}

#[async_trait]
impl JtiStore for InMemoryJtiStore {
    async fn try_claim(&self, jti: &str, _ttl_secs: u64) -> Result<bool> {
        if self.cache.contains_key(jti) {
            Ok(false)
        } else {
            self.cache.insert(jti.to_string(), ()).await;
            Ok(true)
        }
    }

    async fn is_claimed(&self, jti: &str) -> bool {
        self.cache.contains_key(jti)
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ============================================================================
// PublishKeyService
// ============================================================================

/// Publish key service for generating RTMP streaming tokens.
///
/// Includes single-use enforcement: each publish key `jti` can only be
/// consumed once by `validate_publish_key`. Uses a pluggable `JtiStore`
/// backend for deduplication.
#[derive(Clone)]
pub struct PublishKeyService {
    jwt_service: JwtService,
    token_ttl_hours: i64,
    jti_store: Arc<dyn JtiStore>,
}

impl std::fmt::Debug for PublishKeyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishKeyService")
            .field("token_ttl_hours", &self.token_ttl_hours)
            .field("backend", &self.jti_store.backend_name())
            .finish()
    }
}

impl PublishKeyService {
    /// Create a new publish key service with a custom JTI store.
    pub fn from_store(jwt_service: JwtService, token_ttl_hours: i64, jti_store: Arc<dyn JtiStore>) -> Self {
        Self {
            jwt_service,
            token_ttl_hours,
            jti_store,
        }
    }

    /// Create a new publish key service (local-only JTI deduplication)
    #[must_use]
    pub fn new(jwt_service: JwtService, token_ttl_hours: i64) -> Self {
        let cache_ttl_secs = (token_ttl_hours as u64).saturating_mul(3600).saturating_add(300);
        let store = Arc::new(InMemoryJtiStore::new(cache_ttl_secs));
        Self::from_store(jwt_service, token_ttl_hours, store)
    }

    /// Create a new publish key service with default TTL (24 hours)
    #[must_use]
    pub fn with_default_ttl(jwt_service: JwtService) -> Self {
        Self::new(jwt_service, 24)
    }

    /// Enable Redis-backed JTI deduplication for multi-replica deployments.
    #[must_use]
    pub fn with_redis(jwt_service: JwtService, token_ttl_hours: i64, conn: redis::aio::ConnectionManager, key_prefix: String) -> Self {
        let cache_ttl_secs = (token_ttl_hours as u64).saturating_mul(3600).saturating_add(300);
        let store = Arc::new(RedisJtiStore::new(conn, key_prefix, cache_ttl_secs));
        Self::from_store(jwt_service, token_ttl_hours, store)
    }

    /// Generate a publish key for RTMP streaming
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

        let claims = PublishClaims {
            room_id: room_id.as_str().to_string(),
            media_id: media_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
            perm_start_live: true,
            iat: now,
            exp,
            jti: nanoid::nanoid!(32),
        };

        let claims_json = serde_json::to_value(&claims)
            .map_err(|e| Error::Internal(format!("Failed to serialize claims: {e}")))?;

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
    /// same token (same `jti`) will fail with an authentication error.
    pub async fn validate_publish_key(&self, token: &str) -> Result<PublishClaims> {
        let claims_value = self
            .jwt_service
            .verify_custom(token)?;

        let claims: PublishClaims = serde_json::from_value(claims_value)
            .map_err(|e| Error::Authentication(format!("Invalid token format: {e}")))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Internal(format!("Time error: {e}")))?
            .as_secs() as i64;

        if now > claims.exp {
            return Err(Error::Authentication("Token has expired".to_string()));
        }

        if !claims.perm_start_live {
            return Err(Error::Authorization(
                "Token does not have START_LIVE permission".to_string(),
            ));
        }

        // Single-use enforcement via JTI store
        let ttl_secs = (claims.exp - claims.iat).max(0) as u64 + 300;
        if !self.jti_store.try_claim(&claims.jti, ttl_secs).await? {
            return Err(Error::Authentication(
                "Publish key has already been used (single-use token)".to_string(),
            ));
        }

        Ok(claims)
    }

    /// Verify a publish key for a specific room/media
    pub async fn verify_publish_key_for_stream(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<UserId> {
        let claims = self.validate_publish_key(token).await?;

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

    #[test]
    fn test_publish_key_service_new() {
        let service = create_publish_key_service();
        let debug = format!("{service:?}");
        assert!(debug.contains("token_ttl_hours"));
        assert!(debug.contains("24"));
    }

    #[test]
    fn test_publish_key_service_with_default_ttl() {
        let jwt = create_jwt_service();
        let service = PublishKeyService::with_default_ttl(jwt);
        let debug = format!("{service:?}");
        assert!(debug.contains("24"));
    }

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

        let expected_exp = now + (2 * 3600);
        let diff = (key.expires_at - expected_exp).abs();
        assert!(diff < 5, "Expiration time is off by more than 5 seconds: diff={diff}");
    }

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

        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_ok());

        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_err());
        if let Err(Error::Authentication(msg)) = result {
            assert!(msg.contains("single-use"), "Expected single-use error, got: {msg}");
        } else {
            panic!("Expected Authentication error for replay");
        }
    }

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

        assert_ne!(key1.token, key2.token);
    }
}
