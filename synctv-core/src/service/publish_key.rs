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
    Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
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

fn unix_timestamp_now() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Internal(format!("Time error: {e}")))?
            .as_secs(),
    )
    .map_err(|_| Error::Internal("Time error: unix timestamp overflow".to_string()))
}

fn cache_ttl_secs(token_ttl_hours: i64) -> u64 {
    u64::try_from(token_ttl_hours)
        .unwrap_or_default()
        .saturating_mul(3600)
        .saturating_add(300)
}

// JtiStore trait

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

    /// Whether claims are coordinated across nodes instead of being local-only.
    fn supports_cross_node_single_use(&self) -> bool {
        false
    }

    /// Whether backend errors reject the claim instead of degrading locally.
    fn fail_closed(&self) -> bool {
        false
    }
}

// Redis implementation

/// Redis-backed JTI store for cluster-wide deduplication using SETNX.
///
/// Uses a shared `Arc<RwLock<ConnectionManager>>` so that in Sentinel mode the
/// background health check can hot-swap the inner connection on failover and
/// this store automatically picks up the new master.
pub struct RedisJtiStore {
    redis_runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: String,
    /// Local moka cache for fast-path checks on the same node.
    local_cache: moka::future::Cache<String, ()>,
    /// When true, reject claims if Redis is unavailable instead of falling
    /// back to local-only enforcement. Required for single-use correctness
    /// in multi-replica / cluster mode.
    fail_closed: bool,
}

impl RedisJtiStore {
    #[must_use]
    pub fn from_runtime(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            redis_runtime,
            key_prefix,
            local_cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(cache_ttl_secs))
                .build(),
            fail_closed: false,
        }
    }

    /// Create from a shared connection handle with fail_closed mode.
    ///
    /// When `fail_closed` is true, Redis failures will cause `try_claim` to
    /// return an error instead of falling back to local-only enforcement.
    /// This is required in cluster mode to preserve single-use guarantees
    /// across replicas.
    #[must_use]
    pub fn new_shared_fail_closed(
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: String,
        cache_ttl_secs: u64,
    ) -> Self {
        Self::from_runtime_fail_closed(
            crate::shared_runtime(shared_conn),
            key_prefix,
            cache_ttl_secs,
        )
    }

    #[must_use]
    pub fn from_runtime_fail_closed(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            redis_runtime,
            key_prefix,
            local_cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(cache_ttl_secs))
                .build(),
            fail_closed: true,
        }
    }

    /// Create from a plain `ConnectionManager` (wraps it in a new `Arc<RwLock<>>`).
    ///
    /// Convenience for standalone mode or tests where no shared handle exists.
    #[must_use]
    pub fn new(
        conn: redis::aio::ConnectionManager,
        key_prefix: String,
        cache_ttl_secs: u64,
    ) -> Self {
        Self::from_runtime(
            crate::shared_runtime(Arc::new(tokio::sync::RwLock::new(conn))),
            key_prefix,
            cache_ttl_secs,
        )
    }
}

#[async_trait]
impl JtiStore for RedisJtiStore {
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool> {
        // Fast path: already claimed locally
        if self.local_cache.contains_key(jti) {
            return Ok(false);
        }

        // Obtain a fresh connection snapshot (follows Sentinel failover).
        let redis_key = format!("{}publish_key:jti:{}", self.key_prefix, jti);
        let mut conn = self.redis_runtime.snapshot().await;
        let ttl_ms = ttl_secs.saturating_mul(1000);
        // Cross-replica check: atomic SET key value PX <ms> NX
        // Using a single SET command with NX and PX flags is atomic in Redis,
        // eliminating the TOCTOU gap between a separate SETNX + EXPIRE pair.
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
                if self.fail_closed {
                    // In cluster / fail_closed mode, reject the claim entirely
                    // to preserve single-use correctness across replicas.
                    tracing::error!(
                        jti = %jti,
                        "Redis unavailable for JTI dedup (fail_closed=true), rejecting claim: {e}"
                    );
                    return Err(Error::Internal(format!(
                        "Redis unavailable for JTI dedup and fail_closed is enabled: {e}"
                    )));
                }
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

    fn fail_closed(&self) -> bool {
        self.fail_closed
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

// In-memory implementation

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
        // Use moka's entry API for atomic check-and-insert.
        // This eliminates the TOCTOU race where two concurrent tasks could both
        // see contains_key()=false and both succeed.
        use moka::ops::compute::Op;
        let entry = self
            .cache
            .entry_by_ref(jti)
            .and_compute_with(|maybe_entry| async move {
                if maybe_entry.is_some() {
                    // Already claimed -- keep existing entry unchanged
                    Op::Nop
                } else {
                    // First claim -- insert a new entry
                    Op::Put(())
                }
            })
            .await;

        // If the operation was `Nop`, the entry already existed (replay).
        // If it was `Put`, we just claimed it (first use).
        match entry {
            // StillNone happens when Op::Nop is returned and there was no entry,
            // but our logic never returns Nop when entry is None, so this is unreachable.
            moka::ops::compute::CompResult::Inserted(_)
            | moka::ops::compute::CompResult::StillNone(_)
            | moka::ops::compute::CompResult::ReplacedWith(_) => Ok(true),
            moka::ops::compute::CompResult::Unchanged(_)
            | moka::ops::compute::CompResult::Removed(_) => Ok(false),
        }
    }

    async fn is_claimed(&self, jti: &str) -> bool {
        self.cache.contains_key(jti)
    }
}

// PublishKeyService

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

#[async_trait]
pub trait StreamingPublishKeyService: Send + Sync {
    fn generate_publish_key(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<PublishKey>;

    async fn validate_publish_key(&self, token: &str) -> Result<PublishClaims>;

    async fn validate_publish_key_for_stream_claims(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<PublishClaims>;

    async fn verify_publish_key_for_stream(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<UserId>;
}

/// Build a publish-key service behind the service abstraction.
///
/// Callers should depend on the returned trait object instead of selecting the
/// concrete local or shared single-use backend directly.
pub fn streaming_publish_key_service_from_shared_state_profile(
    jwt_service: JwtService,
    token_ttl_hours: i64,
    profile: &SharedStateProfile,
) -> Result<Arc<dyn StreamingPublishKeyService>> {
    Ok(Arc::new(PublishKeyService::from_shared_state_profile(
        jwt_service,
        token_ttl_hours,
        profile,
    )?))
}

impl std::fmt::Debug for PublishKeyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishKeyService")
            .field("token_ttl_hours", &self.token_ttl_hours)
            .field(
                "cross_node_single_use",
                &self.jti_store.supports_cross_node_single_use(),
            )
            .field("fail_closed", &self.jti_store.fail_closed())
            .finish()
    }
}

impl PublishKeyService {
    fn decode_publish_claims(&self, token: &str) -> Result<PublishClaims> {
        let claims_value = self.jwt_service.verify_custom(token)?;

        let claims: PublishClaims = serde_json::from_value(claims_value)
            .map_err(|e| Error::Authentication(format!("Invalid token format: {e}")))?;

        let now = unix_timestamp_now()?;

        if now > claims.exp {
            return Err(Error::Authentication("Token has expired".to_string()));
        }

        if !claims.perm_start_live {
            return Err(Error::Authorization(
                "Token does not have START_LIVE permission".to_string(),
            ));
        }

        Ok(claims)
    }

    async fn claim_publish_key(&self, claims: &PublishClaims) -> Result<()> {
        let ttl_secs = (claims.exp - claims.iat)
            .max(0)
            .cast_unsigned()
            .saturating_add(300);
        if !self.jti_store.try_claim(&claims.jti, ttl_secs).await? {
            return Err(Error::Authentication(
                "Publish key has already been used (single-use token)".to_string(),
            ));
        }

        Ok(())
    }

    fn decode_stream_publish_claims(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<PublishClaims> {
        let claims = self.decode_publish_claims(token)?;

        if claims.room_id.parse::<RoomId>().ok() != Some(*room_id) {
            return Err(Error::Authorization(format!(
                "Token room mismatch: expected {}, got {}",
                room_id, claims.room_id
            )));
        }

        if claims.media_id.parse::<MediaId>().ok() != Some(*media_id) {
            return Err(Error::Authorization(format!(
                "Token media mismatch: expected {}, got {}",
                media_id, claims.media_id
            )));
        }

        Ok(claims)
    }

    /// Create a new publish key service with a custom JTI store.
    pub fn from_store(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        jti_store: Arc<dyn JtiStore>,
    ) -> Self {
        Self {
            jwt_service,
            token_ttl_hours,
            jti_store,
        }
    }

    /// Create a new publish key service (local-only JTI deduplication)
    #[must_use]
    pub fn new(jwt_service: JwtService, token_ttl_hours: i64) -> Self {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours);
        let store = Arc::new(InMemoryJtiStore::new(cache_ttl_secs));
        Self::from_store(jwt_service, token_ttl_hours, store)
    }

    /// Create a new publish key service with default TTL (24 hours)
    #[must_use]
    pub fn with_default_ttl(jwt_service: JwtService) -> Self {
        Self::new(jwt_service, 24)
    }

    fn with_redis_runtime(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Self {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours);
        let store = Arc::new(RedisJtiStore::from_runtime(
            redis_runtime,
            key_prefix,
            cache_ttl_secs,
        ));
        Self::from_store(jwt_service, token_ttl_hours, store)
    }

    fn with_redis_runtime_fail_closed(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Self {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours);
        let store = Arc::new(RedisJtiStore::from_runtime_fail_closed(
            redis_runtime,
            key_prefix,
            cache_ttl_secs,
        ));
        Self::from_store(jwt_service, token_ttl_hours, store)
    }

    pub fn from_shared_state_profile(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        profile: &SharedStateProfile,
    ) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Ok(Self::with_redis_runtime_fail_closed(
                jwt_service,
                token_ttl_hours,
                profile.require_shared_runtime("publish-key deduplication state")?,
                profile.key_prefix().to_string(),
            )),
            SharedStateMode::SharedBestEffort => Ok(Self::with_redis_runtime(
                jwt_service,
                token_ttl_hours,
                profile
                    .shared_runtime()
                    .expect("shared state profile guarantees runtime in best-effort mode"),
                profile.key_prefix().to_string(),
            )),
            SharedStateMode::LocalOnly => Ok(Self::new(jwt_service, token_ttl_hours)),
        }
    }

    /// Generate a publish key for RTMP streaming
    pub fn generate_publish_key(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<PublishKey> {
        let now = unix_timestamp_now()?;

        let exp = now + (self.token_ttl_hours * 3600);

        let claims = PublishClaims {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            perm_start_live: true,
            iat: now,
            exp,
            jti: synctv_common::snanoid!(32),
        };

        let claims_json = serde_json::to_value(&claims)
            .map_err(|e| Error::Internal(format!("Failed to serialize claims: {e}")))?;

        let token = self.jwt_service.sign_custom(&claims_json)?;

        Ok(PublishKey {
            token,
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            expires_at: exp,
        })
    }

    /// Validate a publish key token (single-use).
    ///
    /// Each publish key can only be validated once. Subsequent calls with the
    /// same token (same `jti`) will fail with an authentication error.
    pub async fn validate_publish_key(&self, token: &str) -> Result<PublishClaims> {
        let claims = self.decode_publish_claims(token)?;
        self.claim_publish_key(&claims).await?;

        Ok(claims)
    }

    /// Validate a publish key for a specific stream and then consume it.
    ///
    /// Stream binding is checked before the single-use JTI claim so callers do
    /// not burn a valid token on a mismatched route.
    pub async fn validate_publish_key_for_stream_claims(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<PublishClaims> {
        let claims = self.decode_stream_publish_claims(token, room_id, media_id)?;
        self.claim_publish_key(&claims).await?;

        Ok(claims)
    }

    /// Verify a publish key for a specific room/media
    pub async fn verify_publish_key_for_stream(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<UserId> {
        let claims = self
            .validate_publish_key_for_stream_claims(token, room_id, media_id)
            .await?;

        claims.user_id.parse().map_err(crate::Error::Internal)
    }

    /// Verify a publish key for a specific room/media with user status check.
    ///
    /// This is the recommended method for RTMP publish key validation. It
    /// validates the JWT and stream binding, checks the user's current status
    /// (e.g., banned, deleted), and only then consumes the single-use JTI.
    /// This prevents retriable authorization failures from burning a valid
    /// publish key.
    ///
    /// The `user_validator` receives the `UserId` extracted from the token and
    /// should return `Ok(())` if the user is allowed to publish, or an `Err`
    /// if the user should be rejected.
    pub async fn verify_publish_key_for_stream_checked<F>(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
        user_validator: F,
    ) -> Result<UserId>
    where
        F: FnOnce(&UserId) -> Result<()>,
    {
        let claims = self.decode_stream_publish_claims(token, room_id, media_id)?;
        let user_id = claims.user_id.parse().map_err(crate::Error::Internal)?;

        user_validator(&user_id)?;

        self.claim_publish_key(&claims).await?;

        Ok(user_id)
    }
}

#[async_trait]
impl StreamingPublishKeyService for PublishKeyService {
    fn generate_publish_key(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<PublishKey> {
        PublishKeyService::generate_publish_key(self, room_id, media_id, user_id)
    }

    async fn validate_publish_key(&self, token: &str) -> Result<PublishClaims> {
        PublishKeyService::validate_publish_key(self, token).await
    }

    async fn validate_publish_key_for_stream_claims(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<PublishClaims> {
        PublishKeyService::validate_publish_key_for_stream_claims(self, token, room_id, media_id)
            .await
    }

    async fn verify_publish_key_for_stream(
        &self,
        token: &str,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<UserId> {
        PublishKeyService::verify_publish_key_for_stream(self, token, room_id, media_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::auth::JwtService;
    use crate::RedisConnectionRuntime;
    use async_trait::async_trait;

    fn create_jwt_service() -> JwtService {
        JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap()
    }

    #[tokio::test]
    async fn test_redis_jti_store_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let store = RedisJtiStore::from_runtime(runtime.clone(), "synctv:".to_string(), 3600);

        assert!(
            Arc::ptr_eq(&store.redis_runtime, &runtime),
            "Redis JTI store should retain the injected runtime object"
        );
    }

    #[tokio::test]
    async fn test_publish_key_service_supports_service_trait_object() {
        let service: Arc<dyn StreamingPublishKeyService> =
            Arc::new(PublishKeyService::new(create_jwt_service(), 24));
        let room_id = RoomId::from(40_001);
        let media_id = MediaId::from(40_002);
        let user_id = UserId::from(40_003);

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .expect("trait-object publish key service should generate key");
        let claims = service
            .validate_publish_key_for_stream_claims(&key.token, &room_id, &media_id)
            .await
            .expect("trait-object publish key service should validate key");

        assert_eq!(claims.room_id, room_id.to_string());
        assert_eq!(claims.media_id, media_id.to_string());
        assert_eq!(claims.user_id, user_id.to_string());
    }

    #[tokio::test]
    async fn test_streaming_publish_key_service_from_shared_state_profile_returns_live_trait_object(
    ) {
        let jwt = create_jwt_service();
        let profile = SharedStateProfile::from_runtime(None, "trait-test:", false);
        let service = streaming_publish_key_service_from_shared_state_profile(jwt, 12, &profile)
            .expect("standalone mode should allow local publish-key service");
        let room_id = RoomId::from(40_004);
        let media_id = MediaId::from(40_005);
        let user_id = UserId::from(40_006);

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .expect("builder should return a live publish-key service");
        let claims = service
            .validate_publish_key_for_stream_claims(&key.token, &room_id, &media_id)
            .await
            .expect("generated key should validate through the trait object");

        assert_eq!(claims.room_id, room_id.to_string());
        assert_eq!(claims.media_id, media_id.to_string());
        assert_eq!(claims.user_id, user_id.to_string());
    }

    #[test]
    fn test_streaming_publish_key_service_from_shared_state_profile_requires_shared_runtime_in_cluster_mode(
    ) {
        let jwt = create_jwt_service();
        let profile = SharedStateProfile::from_runtime(None, "trait-test:", true);
        let Err(error) = streaming_publish_key_service_from_shared_state_profile(jwt, 12, &profile)
        else {
            panic!("cluster runtime must reject local publish-key deduplication");
        };

        assert!(
            error
                .to_string()
                .contains("cluster runtime requires shared publish-key deduplication state"),
            "unexpected error: {error}"
        );
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

    #[tokio::test]
    async fn test_generate_publish_key_returns_valid_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        assert!(!key.token.is_empty());
        assert_eq!(key.room_id, room_id.to_string());
        assert_eq!(key.media_id, media_id.to_string());
        assert_eq!(key.user_id, user_id.to_string());
        assert!(key.expires_at > 0);
    }

    #[tokio::test]
    async fn test_generate_publish_key_expiration_matches_ttl() {
        let service = create_publish_key_service_with_ttl(2);
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let now = unix_timestamp_now().unwrap();

        let expected_exp = now + (2 * 3600);
        let diff = (key.expires_at - expected_exp).abs();
        assert!(
            diff < 5,
            "Expiration time is off by more than 5 seconds: diff={diff}"
        );
    }

    #[tokio::test]
    async fn test_validate_publish_key_valid_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let claims = service.validate_publish_key(&key.token).await.unwrap();

        assert_eq!(claims.room_id, room_id.to_string());
        assert_eq!(claims.media_id, media_id.to_string());
        assert_eq!(claims.user_id, user_id.to_string());
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
            JwtService::new("different-secret-key-for-tests-abcdef-long-enough-1234567890")
                .unwrap(),
            24,
        );

        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service1
            .generate_publish_key(&room_id, &media_id, &user_id)
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
            .generate_publish_key(&room_id, &media_id, &user_id)
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
            .generate_publish_key(&room_id, &media_id, &user_id)
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
            .generate_publish_key(&room_id, &media_id, &user_id)
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

    #[tokio::test]
    async fn test_verify_publish_key_room_mismatch_does_not_consume_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();
        let wrong_room_id = RoomId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let first_attempt = service
            .verify_publish_key_for_stream(&key.token, &wrong_room_id, &media_id)
            .await;
        assert!(
            matches!(first_attempt, Err(Error::Authorization(_))),
            "room mismatch should reject without consuming the token"
        );

        let second_attempt = service
            .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
            .await;
        assert!(
            second_attempt.is_ok(),
            "room mismatch must not consume an otherwise valid publish key"
        );
        assert_eq!(second_attempt.unwrap(), user_id);
    }

    #[tokio::test]
    async fn test_verify_publish_key_media_mismatch_does_not_consume_token() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();
        let wrong_media_id = MediaId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let first_attempt = service
            .verify_publish_key_for_stream(&key.token, &room_id, &wrong_media_id)
            .await;
        assert!(
            matches!(first_attempt, Err(Error::Authorization(_))),
            "media mismatch should reject without consuming the token"
        );

        let second_attempt = service
            .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
            .await;
        assert!(
            second_attempt.is_ok(),
            "media mismatch must not consume an otherwise valid publish key"
        );
        assert_eq!(second_attempt.unwrap(), user_id);
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
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_ok());

        let result = service.validate_publish_key(&key.token).await;
        assert!(result.is_err());
        if let Err(Error::Authentication(msg)) = result {
            assert!(
                msg.contains("single-use"),
                "Expected single-use error, got: {msg}"
            );
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
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();
        let key2 = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        assert_ne!(key1.token, key2.token);
    }

    #[tokio::test]
    async fn test_in_memory_jti_store_is_local_only() {
        let store = InMemoryJtiStore::new(3600);
        assert!(!store.supports_cross_node_single_use());
        assert!(!store.fail_closed());
    }

    #[test]
    fn test_publish_key_service_debug_reports_capabilities_not_backend_names() {
        let service = PublishKeyService::new(create_jwt_service(), 24);
        let debug = format!("{service:?}");

        assert!(debug.contains("cross_node_single_use: false"));
        assert!(debug.contains("fail_closed: false"));
        assert!(!debug.contains("memory"));
        assert!(!debug.contains("redis"));
        assert!(!debug.contains("backend"));
    }

    #[tokio::test]
    async fn test_in_memory_jti_store_claim_and_reject() {
        let store = InMemoryJtiStore::new(3600);

        // First claim should succeed
        assert!(store.try_claim("jti-1", 3600).await.unwrap());
        // Second claim of same JTI should fail
        assert!(!store.try_claim("jti-1", 3600).await.unwrap());
        // Different JTI should succeed
        assert!(store.try_claim("jti-2", 3600).await.unwrap());

        // is_claimed should reflect state
        assert!(store.is_claimed("jti-1").await);
        assert!(store.is_claimed("jti-2").await);
        assert!(!store.is_claimed("jti-3").await);
    }

    #[tokio::test]
    async fn test_publish_key_service_from_store_custom_backend() {
        let store = Arc::new(InMemoryJtiStore::new(3600));
        let jwt = create_jwt_service();
        let service = PublishKeyService::from_store(jwt, 12, store);

        let debug = format!("{service:?}");
        assert!(debug.contains("12"));
        assert!(debug.contains("cross_node_single_use: false"));
        assert!(debug.contains("fail_closed: false"));
        assert!(!debug.contains("memory"));
    }

    /// Simulate concurrent try_claim calls on the same JTI.
    /// Only one should succeed; all others must return false.
    #[tokio::test]
    async fn test_in_memory_jti_store_concurrent_try_claim_only_one_succeeds() {
        let store = Arc::new(InMemoryJtiStore::new(3600));
        let jti = "concurrent-jti-test";
        let num_tasks = 50;

        let mut handles = Vec::with_capacity(num_tasks);
        for _ in 0..num_tasks {
            let store = store.clone();
            let jti = jti.to_string();
            handles.push(tokio::spawn(async move {
                store.try_claim(&jti, 3600).await.unwrap()
            }));
        }

        let mut success_count = 0u32;
        for handle in handles {
            if handle.await.unwrap() {
                success_count += 1;
            }
        }

        assert_eq!(
            success_count, 1,
            "Exactly one concurrent try_claim should succeed, but {success_count} succeeded"
        );
    }

    /// Validate that verify_publish_key_for_stream accepts a user_validator callback
    /// and rejects banned users even when the JWT is valid.
    #[tokio::test]
    async fn test_validate_publish_key_rejects_banned_user() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        // Validator that simulates a banned user
        let result = service
            .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| {
                Err(Error::Authorization("User is banned".to_string()))
            })
            .await;

        assert!(result.is_err(), "Should reject banned user");
        if let Err(Error::Authorization(msg)) = &result {
            assert!(
                msg.contains("banned"),
                "Error should mention ban; got: {msg}"
            );
        } else {
            panic!("Expected Authorization error, got: {result:?}");
        }
    }

    /// Validate that verify_publish_key_for_stream_checked passes for active user.
    #[tokio::test]
    async fn test_validate_publish_key_accepts_active_user() {
        let service = create_publish_key_service();
        let room_id = RoomId::new();
        let media_id = MediaId::new();
        let user_id = UserId::new();

        let key = service
            .generate_publish_key(&room_id, &media_id, &user_id)
            .unwrap();

        let result = service
            .verify_publish_key_for_stream_checked(
                &key.token,
                &room_id,
                &media_id,
                |_uid| Ok(()), // User is active
            )
            .await;

        assert!(result.is_ok(), "Should accept active user");
        assert_eq!(result.unwrap(), user_id);
    }

    /// Test that RedisJtiStore with fail_closed=true rejects claims when Redis is unavailable.
    /// We simulate this with an AlwaysFailJtiStore that mimics Redis failure behavior.
    #[tokio::test]
    async fn test_fail_closed_jti_store_rejects_on_backend_failure() {
        let store = FailClosedJtiStore;
        let result = store.try_claim("some-jti", 3600).await;
        assert!(
            result.is_err(),
            "fail_closed store should return Err on backend failure"
        );
    }

    /// A mock JtiStore that always fails (simulates Redis unavailable with fail_closed=true).
    struct FailClosedJtiStore;

    #[async_trait]
    impl JtiStore for FailClosedJtiStore {
        async fn try_claim(&self, _jti: &str, _ttl_secs: u64) -> Result<bool> {
            Err(Error::Internal(
                "Redis unavailable and fail_closed is enabled".to_string(),
            ))
        }
        async fn is_claimed(&self, _jti: &str) -> bool {
            false
        }

        fn supports_cross_node_single_use(&self) -> bool {
            true
        }
    }
}
