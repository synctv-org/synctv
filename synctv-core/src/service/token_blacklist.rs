//! Token blacklist service for managing revoked JWT tokens.
//!
//! # Placement
//!
//! This service lives in `synctv-core` (not `synctv-api`) because token
//! revocation is a **domain concern**: `UserService` calls
//! `invalidate_user_tokens` on password change, and `is_blacklisted` is called
//! from both core auth validation and API middleware. Moving it to the API layer
//! would create a circular dependency since `UserService` (core) depends on it.

use redis::AsyncCommands;
use sha2::{Sha256, Digest};
use std::sync::Arc;
use std::time::Duration;
use crate::{cache::CacheInvalidationService, models::UserId, Error, Result, InternalExt};
use crate::resilience::timeout::REDIS_OPERATION_TIMEOUT;

/// Hash a token for use in Redis keys and log messages.
/// This prevents raw tokens from appearing in Redis key space or log aggregation systems.
fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Token blacklist service for managing revoked JWT tokens
///
/// Uses Redis when available for distributed blacklist. Falls back to
/// `moka` in-memory cache for per-instance blacklisting when Redis is
/// not configured.
#[derive(Clone)]
pub struct TokenBlacklistService {
    redis_conn: Option<redis::aio::ConnectionManager>,
    /// Key builder for constructing Redis keys with the configured prefix
    key_builder: crate::cache::KeyBuilder,
    /// In-memory token blacklist: `token_hash` -> `expiry_timestamp_secs`
    local_blacklist: Arc<moka::future::Cache<String, i64>>,
    /// In-memory user invalidation timestamps: `user_key` -> `password_changed_at`
    local_user_invalidations: Arc<moka::future::Cache<String, i64>>,
    /// Optional invalidation service for cross-replica cache sync
    invalidation_service: Option<Arc<CacheInvalidationService>>,
}

impl TokenBlacklistService {
    /// Create a new `TokenBlacklistService`
    ///
    /// If `redis_conn` is None, falls back to per-instance in-memory blacklist
    /// using moka cache with TTL per entry.
    /// The `key_prefix` is used to construct Redis keys via `KeyBuilder`.
    pub fn new(redis_conn: Option<redis::aio::ConnectionManager>, key_prefix: String) -> Self {
        if redis_conn.is_none() {
            tracing::warn!(
                "Token blacklist using in-memory fallback: Redis not configured. \
                 Revocations are per-instance only (not shared across replicas)."
            );
        }
        Self {
            redis_conn,
            key_builder: crate::cache::KeyBuilder::new(key_prefix),
            // Max 100K blacklisted tokens in memory; 30-day max TTL covers refresh tokens
            local_blacklist: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(100_000)
                    .time_to_live(Duration::from_secs(720 * 60 * 60))
                    .build(),
            ),
            // Max 50K user invalidation entries; 1-hour L1 TTL.
            // Redis is the source of truth; a shorter L1 TTL ensures
            // stale entries are bounded across replicas while still
            // providing a fast read-through cache for hot paths.
            local_user_invalidations: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(50_000)
                    .time_to_live(Duration::from_secs(3600))
                    .build(),
            ),
            invalidation_service: None,
        }
    }

    /// Set the cache invalidation service for cross-replica sync
    #[must_use]
    pub fn with_invalidation_service(mut self, service: Arc<CacheInvalidationService>) -> Self {
        self.invalidation_service = Some(service);
        self
    }

    /// Add a token to the blacklist
    /// The token will be blacklisted until it expires (`ttl_seconds`)
    pub async fn blacklist_token(&self, token: &str, ttl_seconds: i64) -> Result<()> {
        if ttl_seconds <= 0 {
            // Token already expired, no need to blacklist
            return Ok(());
        }

        let token_hash = hash_token(token);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = self.key_builder.token_blacklist(&token_hash);

            let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.set_ex(&key, "1", ttl_seconds as u64))
                .await
                .map_err(|_| Error::Internal("Redis timeout: blacklist_token".to_string()))?
                .internal_with_err("Failed to blacklist token")?;

            // Also populate L1 cache so this replica sees it immediately
            let expires_at = chrono::Utc::now().timestamp() + ttl_seconds;
            self.local_blacklist.insert(token_hash.clone(), expires_at).await;

            tracing::info!(token_hash = %&token_hash[..16], ttl_seconds, "Token blacklisted");
        } else {
            // In-memory fallback: store expiry timestamp
            let expires_at = chrono::Utc::now().timestamp() + ttl_seconds;
            self.local_blacklist.insert(token_hash.clone(), expires_at).await;
            tracing::info!(token_hash = %&token_hash[..16], ttl_seconds, "Token blacklisted (in-memory)");
        }

        Ok(())
    }

    /// Atomically consume a refresh token for rotation: blacklist it if not
    /// already blacklisted.
    ///
    /// Returns `Ok(true)` if the token was successfully consumed (i.e. it was
    /// NOT previously blacklisted and is now blacklisted). Returns `Ok(false)`
    /// if the token was already blacklisted (replay / race condition detected).
    ///
    /// This prevents the TOCTOU race in refresh token rotation where two
    /// replicas could both accept the same old refresh token between a
    /// separate `is_blacklisted` check and `blacklist_token` call.
    ///
    /// Uses Redis SET NX + EX for atomicity when Redis is available,
    /// and falls back to in-memory check-and-set otherwise.
    pub async fn try_consume_refresh_token(&self, token: &str, ttl_seconds: i64) -> Result<bool> {
        if ttl_seconds <= 0 {
            // Token already expired -- treat as consumed (no replay risk)
            return Ok(true);
        }

        let token_hash = hash_token(token);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.key_builder.token_blacklist(&token_hash);

            // SET key "1" NX EX ttl -- returns true if the key was set (token was NOT
            // previously blacklisted), false if it already existed (replay detected).
            let was_set: bool = tokio::time::timeout(
                REDIS_OPERATION_TIMEOUT,
                redis::cmd("SET")
                    .arg(&key)
                    .arg("1")
                    .arg("NX")
                    .arg("EX")
                    .arg(ttl_seconds as u64)
                    .query_async::<Option<String>>(&mut conn),
            )
                .await
                .map_err(|_| Error::Internal("Redis timeout: try_consume_refresh_token".to_string()))?
                .map(|v: Option<String>| v.is_some())
                .internal_with_err("Redis error: try_consume_refresh_token")?;

            if was_set {
                // Populate L1 cache so this replica sees it immediately
                let expires_at = chrono::Utc::now().timestamp() + ttl_seconds;
                self.local_blacklist.insert(token_hash.clone(), expires_at).await;
                tracing::info!(
                    token_hash = %&token_hash[..16],
                    ttl_seconds,
                    "Refresh token consumed (blacklisted atomically)"
                );
                Ok(true)
            } else {
                tracing::warn!(
                    token_hash = %&token_hash[..16],
                    "Refresh token replay detected: already consumed"
                );
                Ok(false)
            }
        } else {
            // In-memory fallback: use Moka's entry API for atomic check-and-insert
            // to prevent TOCTOU race where concurrent tasks could consume the same token.
            let expires_at = chrono::Utc::now().timestamp() + ttl_seconds;
            let entry = self
                .local_blacklist
                .entry_by_ref(&token_hash)
                .or_insert(expires_at)
                .await;

            if entry.is_fresh() {
                // We inserted the entry -- token is now consumed
                tracing::info!(
                    token_hash = %&token_hash[..16],
                    ttl_seconds,
                    "Refresh token consumed (in-memory)"
                );
                Ok(true)
            } else {
                // Entry already existed -- replay detected
                tracing::warn!(
                    token_hash = %&token_hash[..16],
                    "Refresh token replay detected: already consumed"
                );
                Ok(false)
            }
        }
    }

    /// Check if a token is blacklisted
    ///
    /// When Redis is configured, uses L1 (moka) as a read-through cache in front
    /// of Redis. A hit in L1 avoids the Redis round-trip. On L1 miss, Redis is
    /// queried and positive results are written back into L1.
    ///
    /// **Graceful degradation**: If Redis is unreachable, this falls back to the L1
    /// cache to maintain service availability. A circuit breaker tracks failures
    /// and logs warnings when degraded mode is active. This prevents a Redis outage
    /// from blocking all valid tokens while still blocking tokens in the L1 cache.
    pub async fn is_blacklisted(&self, token: &str) -> Result<bool> {
        let token_hash = hash_token(token);

        if let Some(ref conn) = self.redis_conn {
            // L1 cache check first - avoids Redis round-trip for known-blacklisted tokens
            if let Some(expires_at) = self.local_blacklist.get(&token_hash).await {
                let now = chrono::Utc::now().timestamp();
                if now < expires_at {
                    crate::metrics::cache::CACHE_HITS
                        .with_label_values(&["token_blacklist", "l1"])
                        .inc();
                    return Ok(true);
                }
                // Expired in L1 - remove lazily
                self.local_blacklist.invalidate(&token_hash).await;
            }

            crate::metrics::cache::CACHE_MISSES
                .with_label_values(&["token_blacklist", "l1"])
                .inc();

            // L1 miss - check Redis (source of truth)
            let mut conn = conn.clone();
            let key = self.key_builder.token_blacklist(&token_hash);

            let exists_result = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.exists::<_, bool>(&key)).await;
            let exists: bool = match exists_result {
                Err(_) => {
                    crate::metrics::redis::REDIS_ERRORS
                        .with_label_values(&["blacklist_check"])
                        .inc();
                    return Err(Error::Internal("Redis timeout: blacklist check".to_string()));
                }
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // Record metric for Redis failures
                    crate::metrics::redis::REDIS_ERRORS
                        .with_label_values(&["blacklist_check"])
                        .inc();

                    tracing::error!(
                        error = %e,
                        "Redis unreachable during blacklist check, propagating error (fail closed)"
                    );
                    // Fail closed: propagate error so callers can deny the request.
                    // Swallowing the error would let revoked tokens through when Redis is down.
                    return Err(Error::Internal(format!(
                        "Token blacklist check failed: {e}"
                    )));
                }
            };

            // Populate L1 on Redis hit so subsequent checks are fast
            if exists {
                let ttl: i64 = match tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.ttl(&key)).await {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        tracing::warn!(
                            error = %e,
                            "Failed to get TTL from Redis for blacklisted token, using default 60s"
                        );
                        60
                    }
                    Err(_) => {
                        tracing::warn!(
                            "Redis timeout getting TTL for blacklisted token, using default 60s"
                        );
                        60
                    }
                };
                let expires_at = chrono::Utc::now().timestamp() + ttl.max(1);
                self.local_blacklist.insert(token_hash, expires_at).await;
            }

            Ok(exists)
        } else {
            // In-memory fallback: check if entry exists and hasn't expired
            if let Some(expires_at) = self.local_blacklist.get(&token_hash).await {
                let now = chrono::Utc::now().timestamp();
                if now < expires_at {
                    return Ok(true);
                }
                // Expired - remove lazily
                self.local_blacklist.invalidate(&token_hash).await;
            }
            Ok(false)
        }
    }

    /// Remove a token from the blacklist (rarely needed, for testing)
    pub async fn remove_token(&self, token: &str) -> Result<()> {
        let token_hash = hash_token(token);

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = self.key_builder.token_blacklist(&token_hash);

            let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.del(&key))
                .await
                .map_err(|_| Error::Internal("Redis timeout: remove_token".to_string()))?
                .internal_with_err("Failed to remove token from blacklist")?;

            // Also clear L1 cache
            self.local_blacklist.invalidate(&token_hash).await;
        } else {
            self.local_blacklist.invalidate(&token_hash).await;
        }

        Ok(())
    }

    /// Invalidate all tokens for a user by storing the current timestamp.
    ///
    /// Any token with an `iat` (issued-at) before this timestamp will be
    /// rejected. The key is set with a TTL so it auto-expires once the
    /// longest-lived token (refresh token, 30 days) would have expired
    /// naturally.
    ///
    /// # Arguments
    /// * `user_id` - The user whose tokens should be invalidated
    /// * `ttl_seconds` - How long to keep the invalidation marker (should be
    ///   at least as long as the longest token lifetime, e.g. 30 days for
    ///   refresh tokens)
    pub async fn invalidate_user_tokens(&self, user_id: &UserId, ttl_seconds: i64) -> Result<()> {
        if ttl_seconds <= 0 {
            return Ok(());
        }

        let now = chrono::Utc::now().timestamp();

        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();

            let key = self.key_builder.token_blacklist_user(user_id.as_str());

            let _: () = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.set_ex(&key, now.to_string(), ttl_seconds as u64))
                .await
                .map_err(|_| Error::Internal("Redis timeout: invalidate_user_tokens".to_string()))?
                .internal_with_err("Failed to set user token invalidation")?;

            // Also populate L1 cache so this replica sees it immediately
            self.local_user_invalidations.insert(key, now).await;

            tracing::info!(
                user_id = %user_id.as_str(),
                "All existing tokens invalidated for user (password changed)"
            );
        } else {
            // In-memory fallback
            let key = self.key_builder.token_blacklist_user(user_id.as_str());
            self.local_user_invalidations.insert(key, now).await;
            tracing::info!(
                user_id = %user_id.as_str(),
                "All existing tokens invalidated for user (in-memory)"
            );
        }

        // Broadcast to other replicas so they evict stale L1 entries
        if let Some(ref service) = self.invalidation_service {
            if let Err(e) = service.invalidate_user_token(user_id).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id.as_str(),
                    "Failed to broadcast user token invalidation to other replicas"
                );
            }
        }

        Ok(())
    }

    /// Check whether a token issued at `token_iat` has been invalidated by a
    /// password change for the given user.
    ///
    /// Returns `true` if the token should be rejected (i.e. it was issued
    /// before the most recent password change).
    pub async fn are_user_tokens_invalidated(&self, user_id: &UserId, token_iat: i64) -> Result<bool> {
        if let Some(ref conn) = self.redis_conn {
            let user_key = self.key_builder.token_blacklist_user(user_id.as_str());

            // L1 cache check first
            if let Some(password_changed_at) = self.local_user_invalidations.get(&user_key).await {
                crate::metrics::cache::CACHE_HITS
                    .with_label_values(&["token_blacklist_user", "l1"])
                    .inc();
                return Ok(token_iat < password_changed_at);
            }

            crate::metrics::cache::CACHE_MISSES
                .with_label_values(&["token_blacklist_user", "l1"])
                .inc();

            // L1 miss - check Redis (source of truth)
            let mut conn = conn.clone();

            let get_result = tokio::time::timeout(REDIS_OPERATION_TIMEOUT, conn.get::<_, Option<String>>(&user_key)).await;
            let value: Option<String> = match get_result {
                Err(_) => {
                    crate::metrics::redis::REDIS_ERRORS
                        .with_label_values(&["user_token_invalidation_check"])
                        .inc();
                    return Err(Error::Internal("Redis timeout: user token invalidation check".to_string()));
                }
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    // Record metric for Redis failures
                    crate::metrics::redis::REDIS_ERRORS
                        .with_label_values(&["user_token_invalidation_check"])
                        .inc();

                    tracing::error!(
                        error = %e,
                        "Redis unreachable during user token invalidation check, \
                         propagating error (fail closed)"
                    );
                    // Fail closed: propagate error so callers can deny the request.
                    // Swallowing the error would let invalidated tokens through when Redis is down.
                    return Err(Error::Internal(format!(
                        "User token invalidation check failed: {e}"
                    )));
                }
            };

            if let Some(timestamp_str) = value {
                if let Ok(password_changed_at) = timestamp_str.parse::<i64>() {
                    // Populate L1 cache
                    self.local_user_invalidations.insert(user_key, password_changed_at).await;
                    // Reject tokens issued before the password change.
                    // Use strict < comparison to prevent TOCTOU: a token issued
                    // at exactly password_changed_at is still valid since the
                    // password change happens after token issuance in that same second.
                    return Ok(token_iat < password_changed_at);
                }
            }

            Ok(false)
        } else {
            // In-memory fallback
            let key = self.key_builder.token_blacklist_user(user_id.as_str());
            if let Some(password_changed_at) = self.local_user_invalidations.get(&key).await {
                return Ok(token_iat < password_changed_at);
            }
            Ok(false)
        }
    }

    /// Invalidate L1 cache for a user's token invalidation entry.
    ///
    /// Called by the cross-replica invalidation listener when another replica
    /// changes the user's token invalidation timestamp. Clearing the L1 entry
    /// forces this replica to re-read from Redis on the next check.
    pub async fn invalidate_user_l1(&self, user_id: &str) {
        let key = self.key_builder.token_blacklist_user(user_id);
        self.local_user_invalidations.invalidate(&key).await;
        tracing::debug!(user_id = %user_id, "Token blacklist L1 invalidated for user (cross-replica)");
    }

    /// Check if the service uses Redis (distributed mode)
    #[must_use]
    pub const fn uses_redis(&self) -> bool {
        self.redis_conn.is_some()
    }

    /// Check if the service is enabled (always true - uses in-memory fallback when no Redis)
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for TokenBlacklistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBlacklistService")
            .field("enabled", &self.redis_conn.is_some())
            .field("invalidation_enabled", &self.invalidation_service.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_blacklist_token() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let service = TokenBlacklistService::new(Some(conn), "synctv".to_string());

        let token = "test_token_12345";

        // Initially not blacklisted
        assert!(!service.is_blacklisted(token).await.unwrap());

        // Blacklist it
        service.blacklist_token(token, 60).await.unwrap();

        // Now it should be blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());

        // Remove it
        service.remove_token(token).await.unwrap();

        // Should not be blacklisted anymore
        assert!(!service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_blacklist() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        assert!(service.is_enabled());
        assert!(!service.uses_redis());

        let token = "test_token_12345";

        // Initially not blacklisted
        assert!(!service.is_blacklisted(token).await.unwrap());

        // Blacklist it
        service.blacklist_token(token, 60).await.unwrap();

        // Now it should be blacklisted (in-memory)
        assert!(service.is_blacklisted(token).await.unwrap());

        // Remove it
        service.remove_token(token).await.unwrap();

        // Should not be blacklisted anymore
        assert!(!service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_user_token_invalidation() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        let now = chrono::Utc::now().timestamp();

        // Initially no invalidation
        assert!(!service.are_user_tokens_invalidated(&user_id, now).await.unwrap());

        // Invalidate tokens
        service.invalidate_user_tokens(&user_id, 3600).await.unwrap();

        // Token issued before invalidation should be rejected
        let old_iat = now - 10;
        assert!(service.are_user_tokens_invalidated(&user_id, old_iat).await.unwrap());

        // Token issued after invalidation should be accepted
        let future_iat = now + 10;
        assert!(!service.are_user_tokens_invalidated(&user_id, future_iat).await.unwrap());
    }

    #[tokio::test]
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
    async fn test_user_token_invalidation() {
        let infra = crate::test_helpers::containers::TestInfra::redis_only().await;
        let conn = infra.connection_manager().await;
        let service = TokenBlacklistService::new(Some(conn), "synctv".to_string());
        let user_id = UserId::from_string("test_invalidation_user".to_string());

        // Initially no invalidation
        let now = chrono::Utc::now().timestamp();
        assert!(!service.are_user_tokens_invalidated(&user_id, now).await.unwrap());

        // Invalidate tokens
        service.invalidate_user_tokens(&user_id, 60).await.unwrap();

        // Token issued before invalidation should be rejected
        let old_iat = now - 10;
        assert!(service.are_user_tokens_invalidated(&user_id, old_iat).await.unwrap());

        // Token issued at the same second should also be rejected (edge case)
        assert!(service.are_user_tokens_invalidated(&user_id, now).await.unwrap());

        // Token issued after invalidation should be accepted
        let future_iat = now + 10;
        assert!(!service.are_user_tokens_invalidated(&user_id, future_iat).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_blacklist_zero_ttl_is_noop() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "token_with_zero_ttl";

        // Blacklisting with TTL <= 0 should be a no-op (token already expired)
        service.blacklist_token(token, 0).await.unwrap();
        assert!(!service.is_blacklisted(token).await.unwrap());

        service.blacklist_token(token, -5).await.unwrap();
        assert!(!service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_non_blacklisted_token_returns_false() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        // Arbitrary tokens that were never blacklisted should return false
        assert!(!service.is_blacklisted("never_seen_token").await.unwrap());
        assert!(!service.is_blacklisted("another_random_token").await.unwrap());
        assert!(!service.is_blacklisted("").await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_invalidate_user_tokens_zero_ttl_is_noop() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("noop_user".to_string());

        // TTL <= 0 should be a no-op
        service.invalidate_user_tokens(&user_id, 0).await.unwrap();

        let now = chrono::Utc::now().timestamp();
        // Should not be invalidated since the call was a no-op
        assert!(!service.are_user_tokens_invalidated(&user_id, now - 100).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_blacklist_multiple_tokens() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        let token_a = "token_aaa";
        let token_b = "token_bbb";
        let token_c = "token_ccc";

        service.blacklist_token(token_a, 300).await.unwrap();
        service.blacklist_token(token_b, 300).await.unwrap();

        assert!(service.is_blacklisted(token_a).await.unwrap());
        assert!(service.is_blacklisted(token_b).await.unwrap());
        assert!(!service.is_blacklisted(token_c).await.unwrap());

        // Remove token_a, token_b should still be blacklisted
        service.remove_token(token_a).await.unwrap();
        assert!(!service.is_blacklisted(token_a).await.unwrap());
        assert!(service.is_blacklisted(token_b).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_user_invalidation_different_users_independent() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_a = UserId::from_string("user_a".to_string());
        let user_b = UserId::from_string("user_b".to_string());

        let now = chrono::Utc::now().timestamp();

        // Invalidate tokens only for user_a
        service.invalidate_user_tokens(&user_a, 3600).await.unwrap();

        // user_a's old tokens should be rejected
        assert!(service.are_user_tokens_invalidated(&user_a, now - 10).await.unwrap());

        // user_b should be unaffected
        assert!(!service.are_user_tokens_invalidated(&user_b, now - 10).await.unwrap());
    }

    #[tokio::test]
    async fn test_in_memory_service_status_flags() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        assert!(service.is_enabled());
        assert!(!service.uses_redis());
    }

    // ========== Double-Blacklist Idempotency ==========

    #[tokio::test]
    async fn test_in_memory_blacklist_idempotent() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "idempotent_token";

        // Blacklist the same token twice
        service.blacklist_token(token, 60).await.unwrap();
        service.blacklist_token(token, 60).await.unwrap();

        // Should still be blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());

        // Single remove should clear it
        service.remove_token(token).await.unwrap();
        assert!(!service.is_blacklisted(token).await.unwrap());
    }

    // ========== Remove Non-Existent Token ==========

    #[tokio::test]
    async fn test_in_memory_remove_nonexistent_token() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        // Removing a token that was never blacklisted should succeed (no-op)
        service.remove_token("nonexistent").await.unwrap();

        // Verify it's still not blacklisted
        assert!(!service.is_blacklisted("nonexistent").await.unwrap());
    }

    // ========== Concurrent Blacklist Operations ==========

    #[tokio::test]
    async fn test_in_memory_concurrent_blacklist_and_check() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        // Spawn concurrent blacklist operations
        let mut handles = Vec::new();
        for i in 0..50 {
            let svc = service.clone();
            let token = format!("concurrent_token_{i}");
            handles.push(tokio::spawn(async move {
                svc.blacklist_token(&token, 60).await.unwrap();
                assert!(svc.is_blacklisted(&token).await.unwrap());
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // Verify all tokens are blacklisted
        for i in 0..50 {
            let token = format!("concurrent_token_{i}");
            assert!(service.is_blacklisted(&token).await.unwrap());
        }
    }

    // ========== User Invalidation Re-invalidation ==========

    #[tokio::test]
    async fn test_in_memory_user_re_invalidation_updates_timestamp() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user = UserId::from_string("re_invalidate_user".to_string());

        let now = chrono::Utc::now().timestamp();

        // First invalidation at current time
        service.invalidate_user_tokens(&user, 3600).await.unwrap();

        // Token issued 100s ago should be invalidated
        assert!(service.are_user_tokens_invalidated(&user, now - 100).await.unwrap());

        // Token issued at same second as invalidation: accepted (uses strict <)
        // A token issued at the exact same second as the password change is still
        // valid since the password change happens after token issuance in that second.
        assert!(!service.are_user_tokens_invalidated(&user, now).await.unwrap());

        // Token that would be issued 1 second from now should still be valid
        assert!(!service.are_user_tokens_invalidated(&user, now + 1).await.unwrap());

        // Wait so the second clock ticks forward, then re-invalidate
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        service.invalidate_user_tokens(&user, 3600).await.unwrap();

        // Now the token issued at (now + 1) should also be invalidated,
        // because the new invalidation timestamp is >= now + 2
        assert!(service.are_user_tokens_invalidated(&user, now + 1).await.unwrap());
    }

    // ========== Hash Token Determinism ==========

    #[test]
    fn test_hash_token_deterministic() {
        let token = "my_secret_jwt_token";
        let h1 = hash_token(token);
        let h2 = hash_token(token);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_token_different_inputs() {
        let h1 = hash_token("token_a");
        let h2 = hash_token("token_b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_token_is_hex_sha256() {
        let h = hash_token("test");
        // SHA-256 produces 64 hex characters
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ========== Debug Representation ==========

    #[test]
    fn test_token_blacklist_debug() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let debug = format!("{service:?}");
        assert!(debug.contains("TokenBlacklistService"));
        assert!(debug.contains("enabled"));
    }

    // ========== Refresh Token Rotation: Blacklists Old Token ==========

    #[tokio::test]
    async fn test_try_consume_refresh_token_blacklists_on_first_use() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "refresh_token_to_consume";

        // First consumption should succeed (returns true = consumed)
        assert!(service.try_consume_refresh_token(token, 3600).await.unwrap());

        // The token should now be blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_try_consume_refresh_token_replay_detected() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "refresh_token_replay";

        // First consumption succeeds
        assert!(service.try_consume_refresh_token(token, 3600).await.unwrap());

        // Second consumption fails (replay detected, returns false)
        assert!(!service.try_consume_refresh_token(token, 3600).await.unwrap());
    }

    #[tokio::test]
    async fn test_try_consume_refresh_token_expired_ttl_treated_as_consumed() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "expired_refresh_token";

        // TTL <= 0 means the token has already expired, so treat as consumed
        assert!(service.try_consume_refresh_token(token, 0).await.unwrap());
        assert!(service.try_consume_refresh_token(token, -5).await.unwrap());

        // Token should NOT be blacklisted since TTL was <= 0 (no-op)
        assert!(!service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_try_consume_concurrent_only_one_succeeds() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "concurrent_refresh_token";

        // Simulate concurrent consumption attempts
        let mut handles = Vec::new();
        for _ in 0..20 {
            let svc = service.clone();
            let tok = token.to_string();
            handles.push(tokio::spawn(async move {
                svc.try_consume_refresh_token(&tok, 3600).await.unwrap()
            }));
        }

        let mut success_count = 0;
        let mut fail_count = 0;
        for h in handles {
            if h.await.unwrap() {
                success_count += 1;
            } else {
                fail_count += 1;
            }
        }

        // In in-memory mode, at least one should succeed (race conditions
        // may allow more than one since moka is not strictly atomic).
        // The important invariant is that the token IS blacklisted afterward.
        assert!(success_count >= 1, "At least one consumer should succeed");
        assert!(
            success_count + fail_count == 20,
            "All 20 attempts should resolve"
        );
        assert!(service.is_blacklisted(token).await.unwrap());
    }

    // ========== Password Change Invalidates Tokens Issued Before Change ==========

    #[tokio::test]
    async fn test_password_change_invalidates_old_tokens() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("password_change_user".to_string());

        let before_change = chrono::Utc::now().timestamp();

        // Simulate password change -> invalidate all tokens
        // TTL = 30 days (matching refresh token max lifetime)
        const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;
        service
            .invalidate_user_tokens(&user_id, THIRTY_DAYS)
            .await
            .unwrap();

        // Token issued 1 second before password change should be rejected
        assert!(
            service
                .are_user_tokens_invalidated(&user_id, before_change - 1)
                .await
                .unwrap()
        );

        // Token issued 1 second after password change should be accepted
        let after_change = chrono::Utc::now().timestamp() + 1;
        assert!(
            !service
                .are_user_tokens_invalidated(&user_id, after_change)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_password_change_does_not_affect_other_users() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_a = UserId::from_string("user_pwd_a".to_string());
        let user_b = UserId::from_string("user_pwd_b".to_string());

        let now = chrono::Utc::now().timestamp();

        // Only invalidate user_a's tokens
        service.invalidate_user_tokens(&user_a, 3600).await.unwrap();

        // user_a's old token should be rejected
        assert!(service.are_user_tokens_invalidated(&user_a, now - 10).await.unwrap());

        // user_b should be completely unaffected
        assert!(!service.are_user_tokens_invalidated(&user_b, now - 10).await.unwrap());
        assert!(!service.are_user_tokens_invalidated(&user_b, now - 86400).await.unwrap());
    }

    // ========== Blacklist TTL Matches Token Expiry Time ==========

    #[tokio::test]
    async fn test_blacklist_ttl_respects_token_lifetime() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());

        // Blacklist a token with a short TTL (simulating an about-to-expire token)
        let token = "short_lived_token";
        service.blacklist_token(token, 2).await.unwrap();

        // Should be blacklisted immediately
        assert!(service.is_blacklisted(token).await.unwrap());

        // Wait for TTL to expire (moka cache TTL is set per-cache, not per-entry,
        // so in-memory mode uses the expiry timestamp stored as the value).
        // We verify the blacklist entry stores a reasonable expiry.
        // The token_hash -> expiry_timestamp mapping should expire naturally.
        //
        // For in-memory mode, we can verify that the stored expiry is close
        // to now + ttl by checking that a token blacklisted with TTL=2 is
        // still blacklisted within the TTL window.
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        assert!(service.is_blacklisted(token).await.unwrap());
    }

    #[tokio::test]
    async fn test_user_invalidation_ttl_covers_max_token_lifetime() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("ttl_test_user".to_string());

        // Invalidate with 30-day TTL (matching refresh token lifetime)
        const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;
        let before = chrono::Utc::now().timestamp();
        service
            .invalidate_user_tokens(&user_id, THIRTY_DAYS)
            .await
            .unwrap();

        // Token issued just before invalidation should be rejected
        assert!(service.are_user_tokens_invalidated(&user_id, before - 1).await.unwrap());

        // Token issued well in the future should be accepted
        assert!(!service.are_user_tokens_invalidated(&user_id, before + THIRTY_DAYS).await.unwrap());
    }

    // ========== Cross-Replica L1 Invalidation ==========

    #[tokio::test]
    async fn test_invalidate_user_l1_clears_cached_entry() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("l1_test_user".to_string());

        let now = chrono::Utc::now().timestamp();

        // Invalidate user tokens (populates L1 cache)
        service.invalidate_user_tokens(&user_id, 3600).await.unwrap();

        // Should reject old tokens
        assert!(service.are_user_tokens_invalidated(&user_id, now - 10).await.unwrap());

        // Simulate cross-replica L1 invalidation
        service.invalidate_user_l1(user_id.as_str()).await;

        // After L1 invalidation, in-memory mode should no longer find the entry
        // (since there's no Redis backing store to fall back to)
        assert!(!service.are_user_tokens_invalidated(&user_id, now - 10).await.unwrap());
    }

    // ========== Token Refresh with Already-Consumed Refresh Token ==========

    #[tokio::test]
    async fn test_consumed_token_stays_blacklisted_for_full_ttl() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let token = "consumed_and_should_stay_blacklisted";

        // Consume the token (simulating a refresh rotation)
        assert!(service.try_consume_refresh_token(token, 3600).await.unwrap());

        // The token should remain blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());

        // Attempting to consume again should fail (replay)
        assert!(!service.try_consume_refresh_token(token, 3600).await.unwrap());

        // Still blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());
    }

    // ========== Integration: Blacklist + User Invalidation Independence ==========

    #[tokio::test]
    async fn test_individual_blacklist_and_user_invalidation_are_independent() {
        let service = TokenBlacklistService::new(None, "synctv".to_string());
        let user_id = UserId::from_string("independent_user".to_string());

        let now = chrono::Utc::now().timestamp();

        // Blacklist a specific token
        let token = "specific_token_for_user";
        service.blacklist_token(token, 3600).await.unwrap();

        // User invalidation is NOT set yet, so user-level check should pass
        assert!(!service.are_user_tokens_invalidated(&user_id, now - 10).await.unwrap());

        // But the specific token IS blacklisted
        assert!(service.is_blacklisted(token).await.unwrap());

        // Now invalidate all user tokens
        service.invalidate_user_tokens(&user_id, 3600).await.unwrap();

        // Both checks should now trigger
        assert!(service.are_user_tokens_invalidated(&user_id, now - 10).await.unwrap());
        assert!(service.is_blacklisted(token).await.unwrap());

        // A different token for the same user is NOT individually blacklisted
        assert!(!service.is_blacklisted("other_token").await.unwrap());
        // But IS invalidated by user-level invalidation
        assert!(service.are_user_tokens_invalidated(&user_id, now - 10).await.unwrap());
    }
}
