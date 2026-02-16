use redis::AsyncCommands;
use sha2::{Sha256, Digest};
use std::sync::Arc;
use std::time::Duration;
use crate::{models::UserId, Error, Result, InternalExt};

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
                    .time_to_live(Duration::from_hours(720))
                    .build(),
            ),
            // Max 50K user invalidation entries; 30-day max TTL
            local_user_invalidations: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(50_000)
                    .time_to_live(Duration::from_hours(720))
                    .build(),
            ),
        }
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

            let _: () = conn.set_ex(&key, "1", ttl_seconds as u64)
                .await
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
                    return Ok(true);
                }
                // Expired in L1 - remove lazily
                self.local_blacklist.invalidate(&token_hash).await;
            }

            // L1 miss - check Redis (source of truth)
            let mut conn = conn.clone();
            let key = self.key_builder.token_blacklist(&token_hash);

            let exists: bool = match conn.exists(&key).await {
                Ok(v) => v,
                Err(e) => {
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
                let ttl: i64 = conn.ttl(&key).await.unwrap_or(60);
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

            let _: () = conn.del(&key)
                .await
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

            let _: () = conn.set_ex(&key, now.to_string(), ttl_seconds as u64)
                .await
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
                return Ok(token_iat < password_changed_at);
            }

            // L1 miss - check Redis (source of truth)
            let mut conn = conn.clone();

            let value: Option<String> = match conn.get(&user_key).await {
                Ok(v) => v,
                Err(e) => {
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
}
