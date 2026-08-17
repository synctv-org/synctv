//! Publish key generation for RTMP live streaming
//!
//! Generates JWT tokens for RTMP push authentication.
//! Supports single-use, reusable expiring, and permanent publish keys.
//!
//! ## JTI Deduplication Backends
//!
//! - `RedisJtiStore`: Redis SETNX for cluster-wide deduplication
//! - `InMemoryJtiStore`: moka cache for per-node deduplication

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    models::{MediaId, RoomId, UserId},
    service::JwtService,
    Clock, Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
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
    /// Expiration timestamp, absent for permanent keys
    pub expires_at: Option<i64>,
    /// Key lifecycle type
    pub key_type: PublishKeyType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishKeyType {
    SingleUse,
    Expiring,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishKeyOptions {
    pub key_type: PublishKeyType,
    pub expires_at: Option<i64>,
}

impl PublishKeyOptions {
    fn validate(self, now: i64) -> Result<Self> {
        match (self.key_type, self.expires_at) {
            (PublishKeyType::SingleUse | PublishKeyType::Expiring, Some(expires_at))
                if expires_at > now =>
            {
                Ok(self)
            }
            (PublishKeyType::SingleUse | PublishKeyType::Expiring, Some(_)) => Err(
                Error::InvalidInput("publish key expiration must be in the future".to_string()),
            ),
            (PublishKeyType::SingleUse | PublishKeyType::Expiring, None) => {
                Err(Error::InvalidInput(format!(
                    "{} publish keys require an expiration time",
                    match self.key_type {
                        PublishKeyType::SingleUse => "single-use",
                        PublishKeyType::Expiring => "expiring",
                        PublishKeyType::Permanent => unreachable!(),
                    }
                )))
            }
            (PublishKeyType::Permanent, None) => Ok(self),
            (PublishKeyType::Permanent, Some(_)) => Err(Error::InvalidInput(
                "permanent publish keys must not have an expiration time".to_string(),
            )),
        }
    }
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
    pub perm_manage_live_streams: bool,
    /// Issued at timestamp
    pub iat: i64,
    /// Expiration timestamp, absent for permanent keys
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    /// JWT ID (unique token identifier)
    pub jti: String,
    /// Key lifecycle type
    pub key_type: PublishKeyType,
}

/// Publish key service for generating RTMP streaming tokens.
///
/// Single-use keys use a pluggable `JtiStore` backend for deduplication.
#[derive(Clone)]
pub struct PublishKeyService {
    jwt_service: JwtService,
    clock: Arc<dyn Clock>,
    token_ttl_hours: i64,
    jti_store: Arc<dyn JtiStore>,
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

fn default_publish_key_options(
    clock: &dyn Clock,
    token_ttl_hours: i64,
) -> Result<PublishKeyOptions> {
    let expires_at = clock
        .now()
        .timestamp()
        .checked_add(token_lifetime_secs(token_ttl_hours)?)
        .ok_or_else(|| Error::InvalidInput("publish key expiration overflow".to_string()))?;
    Ok(PublishKeyOptions {
        key_type: PublishKeyType::SingleUse,
        expires_at: Some(expires_at),
    })
}

#[async_trait]
pub trait StreamingPublishKeyService: Send + Sync {
    fn generate_publish_key(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<PublishKey>;

    fn generate_publish_key_with_options(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
        options: PublishKeyOptions,
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
        let claims: PublishClaims = self
            .jwt_service
            .verify_custom(token)
            .map_err(|e| Error::Authentication(format!("Invalid token format: {e}")))?;

        let now = self.clock.now().timestamp();

        match (claims.key_type, claims.exp) {
            (PublishKeyType::SingleUse | PublishKeyType::Expiring, Some(expires_at)) => {
                if now > expires_at {
                    return Err(Error::Authentication("Token has expired".to_string()));
                }
            }
            (PublishKeyType::Permanent, None) => {}
            _ => {
                return Err(Error::Authentication(
                    "Publish key lifecycle claims are invalid".to_string(),
                ));
            }
        }

        if !claims.perm_manage_live_streams {
            return Err(Error::Authorization(
                "Token does not have MANAGE_LIVE_STREAMS permission".to_string(),
            ));
        }

        Ok(claims)
    }

    async fn claim_publish_key(&self, claims: &PublishClaims) -> Result<()> {
        if claims.key_type != PublishKeyType::SingleUse {
            return Ok(());
        }
        let expires_at = claims.exp.ok_or_else(|| {
            Error::Authentication("Single-use publish key has no expiration".to_string())
        })?;
        let ttl_secs = (expires_at - self.clock.now().timestamp())
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
        clock: Arc<dyn Clock>,
        token_ttl_hours: i64,
        jti_store: Arc<dyn JtiStore>,
    ) -> Self {
        Self {
            jwt_service,
            clock,
            token_ttl_hours,
            jti_store,
        }
    }

    /// Create a new publish key service (local-only JTI deduplication)
    pub fn new(
        jwt_service: JwtService,
        clock: Arc<dyn Clock>,
        token_ttl_hours: i64,
    ) -> Result<Self> {
        let store = Arc::new(InMemoryJtiStore::new(0));
        Ok(Self::from_store(jwt_service, clock, token_ttl_hours, store))
    }

    pub fn with_default_ttl(jwt_service: JwtService, clock: Arc<dyn Clock>) -> Result<Self> {
        Self::new(jwt_service, clock, 24)
    }

    fn with_redis_runtime(
        jwt_service: JwtService,
        clock: Arc<dyn Clock>,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Result<Self> {
        let store = Arc::new(RedisJtiStore::from_runtime(redis_runtime, key_prefix, 0));
        Ok(Self::from_store(jwt_service, clock, token_ttl_hours, store))
    }

    fn with_redis_runtime_fail_closed(
        jwt_service: JwtService,
        clock: Arc<dyn Clock>,
        token_ttl_hours: i64,
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
    ) -> Result<Self> {
        let store = Arc::new(RedisJtiStore::from_runtime_fail_closed(
            redis_runtime,
            key_prefix,
            0,
        ));
        Ok(Self::from_store(jwt_service, clock, token_ttl_hours, store))
    }

    pub fn from_shared_state_profile(
        jwt_service: JwtService,
        clock: Arc<dyn Clock>,
        token_ttl_hours: i64,
        profile: &SharedStateProfile,
    ) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Self::with_redis_runtime_fail_closed(
                jwt_service,
                clock,
                token_ttl_hours,
                profile.require_shared_runtime("publish-key deduplication state")?,
                profile.key_prefix().to_string(),
            ),
            SharedStateMode::SharedBestEffort => Self::with_redis_runtime(
                jwt_service,
                clock,
                token_ttl_hours,
                profile.best_effort_shared_runtime("publish-key deduplication state")?,
                profile.key_prefix().to_string(),
            ),
            SharedStateMode::LocalOnly => Self::new(jwt_service, clock, token_ttl_hours),
        }
    }

    /// Generate a publish key for RTMP streaming
    pub fn generate_publish_key(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<PublishKey> {
        let options = default_publish_key_options(self.clock.as_ref(), self.token_ttl_hours)?;
        self.generate_publish_key_with_options(room_id, media_id, user_id, options)
    }

    pub fn generate_publish_key_with_options(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
        options: PublishKeyOptions,
    ) -> Result<PublishKey> {
        let now = self.clock.now().timestamp();
        let options = options.validate(now)?;

        let claims = PublishClaims {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            perm_manage_live_streams: true,
            iat: now,
            exp: options.expires_at,
            jti: synctv_common::snanoid!(32),
            key_type: options.key_type,
        };

        let token = self.jwt_service.sign_custom(&claims)?;

        Ok(PublishKey {
            token,
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            expires_at: options.expires_at,
            key_type: options.key_type,
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

    fn generate_publish_key_with_options(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
        options: PublishKeyOptions,
    ) -> Result<PublishKey> {
        PublishKeyService::generate_publish_key_with_options(
            self, room_id, media_id, user_id, options,
        )
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
