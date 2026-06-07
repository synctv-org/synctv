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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    models::{MediaId, RoomId, UserId},
    service::auth::JwtService,
    Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

mod jti;
pub use jti::{InMemoryJtiStore, JtiStore, RedisJtiStore};

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
    /// Permission to control live streams
    pub perm_live_control: bool,
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

fn cache_ttl_secs(token_ttl_hours: i64) -> Result<u64> {
    if token_ttl_hours <= 0 {
        tracing::warn!(
            token_ttl_hours,
            "Publish key token TTL must be positive; using deduplication grace window only"
        );
        return Ok(300);
    }
    let hours = u64::try_from(token_ttl_hours)
        .map_err(|_| Error::InvalidInput("publish key token TTL is invalid".to_string()))?;
    hours
        .checked_mul(3600)
        .and_then(|seconds| seconds.checked_add(300))
        .ok_or_else(|| Error::InvalidInput("publish key cache TTL is too large".to_string()))
}

fn token_lifetime_secs(token_ttl_hours: i64) -> Result<i64> {
    if token_ttl_hours <= 0 {
        return Err(Error::InvalidInput(format!(
            "publish key token_ttl_hours must be positive, got {token_ttl_hours}"
        )));
    }
    token_ttl_hours
        .checked_mul(3600)
        .ok_or_else(|| Error::InvalidInput("publish key token TTL is too large".to_string()))
}

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

        if !claims.perm_live_control {
            return Err(Error::Authorization(
                "Token does not have LIVE_CONTROL permission".to_string(),
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
    pub fn new(jwt_service: JwtService, token_ttl_hours: i64) -> Result<Self> {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours)?;
        let store = Arc::new(InMemoryJtiStore::new(cache_ttl_secs));
        Ok(Self::from_store(jwt_service, token_ttl_hours, store))
    }

    /// Create a new publish key service with default TTL (24 hours)
    pub fn with_default_ttl(jwt_service: JwtService) -> Result<Self> {
        Self::new(jwt_service, 24)
    }

    fn with_redis_runtime(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Result<Self> {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours)?;
        let store = Arc::new(RedisJtiStore::from_runtime(
            redis_runtime,
            key_prefix,
            cache_ttl_secs,
        ));
        Ok(Self::from_store(jwt_service, token_ttl_hours, store))
    }

    fn with_redis_runtime_fail_closed(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Result<Self> {
        let cache_ttl_secs = cache_ttl_secs(token_ttl_hours)?;
        let store = Arc::new(RedisJtiStore::from_runtime_fail_closed(
            redis_runtime,
            key_prefix,
            cache_ttl_secs,
        ));
        Ok(Self::from_store(jwt_service, token_ttl_hours, store))
    }

    pub fn from_shared_state_profile(
        jwt_service: JwtService,
        token_ttl_hours: i64,
        profile: &SharedStateProfile,
    ) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Self::with_redis_runtime_fail_closed(
                jwt_service,
                token_ttl_hours,
                profile.require_shared_runtime("publish-key deduplication state")?,
                profile.key_prefix().to_string(),
            ),
            SharedStateMode::SharedBestEffort => Self::with_redis_runtime(
                jwt_service,
                token_ttl_hours,
                profile.best_effort_shared_runtime("publish-key deduplication state")?,
                profile.key_prefix().to_string(),
            ),
            SharedStateMode::LocalOnly => Self::new(jwt_service, token_ttl_hours),
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
        let exp = now
            .checked_add(token_lifetime_secs(self.token_ttl_hours)?)
            .ok_or_else(|| Error::InvalidInput("publish key expiration overflow".to_string()))?;

        let claims = PublishClaims {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            perm_live_control: true,
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
mod tests;
