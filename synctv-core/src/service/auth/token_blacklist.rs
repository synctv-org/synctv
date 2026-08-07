//! Refresh token blacklist and refresh-session revocation storage.
//!
//! ## Backend Abstraction
//!
//! Storage is abstracted via the [`TokenBlacklistStore`] trait. Three
//! implementations are provided:
//! - [`TieredTokenBlacklistStore`]: Production store with L1 (moka) + optional
//!   L2 (Redis) + PG primary. Includes negative caching for cache penetration
//!   protection.
//! - [`PgTokenBlacklistStore`]: PostgreSQL-backed, used as the inner durable
//!   layer inside `TieredTokenBlacklistStore`.
//! - [`InMemoryTokenBlacklistStore`]: moka cache, for tests and standalone mode
//!   without a database. Data is lost on restart.

use async_trait::async_trait;

use crate::Result;

mod stores;
pub use stores::{InMemoryTokenBlacklistStore, PgTokenBlacklistStore, TieredTokenBlacklistStore};

// TokenBlacklistStore trait

/// Storage backend for refresh token blacklist and refresh-session revocation.
///
/// Used by `UserService` for Refresh Token Rotation:
/// 1. **JTI blacklist**: Each used refresh token's JTI is recorded so replays
/// are detected.
/// 2. **Family/session revocation**: When a blacklisted JTI is replayed
/// (indicating token theft), the refresh-token family for that login session is
/// revoked. The same primitive is also used by a few higher-level version keys.
#[async_trait]
pub trait TokenBlacklistStore: Send + Sync {
    /// Check if a JTI key is blacklisted, propagating storage errors.
    ///
    /// Implementations must provide this method; authentication code relies on
    /// it to preserve fail-closed semantics.
    async fn is_blacklisted_checked(&self, key: &str) -> Result<bool>;

    /// Blacklist a JTI key with the given TTL in seconds.
    ///
    /// Returns `Err` only on critical failures where the caller should
    /// refuse to issue new tokens (fail-closed).
    async fn blacklist(&self, key: &str, ttl_secs: u64) -> Result<()>;

    /// Atomically blacklist a JTI key if it doesn't already exist.
    ///
    /// This is the core method for preventing TOCTOU race conditions in
    /// refresh token rotation. It performs an atomic "check and set" operation:
    ///
    /// - Returns `Ok(true)` if the key **already existed** (replay detected)
    /// - Returns `Ok(false)` if the key was **newly inserted** (first use)
    /// - Returns `Err` only on critical failures (fail-closed)
    ///
    /// # Security Implications
    ///
    /// When refreshing a token:
    /// - `Ok(false)` means this is the first use → proceed to issue new token
    /// - `Ok(true)` means a replay was detected → trigger family revocation
    ///
    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> Result<bool>;

    /// Get the family revocation timestamp for a key, propagating storage errors.
    ///
    /// Authentication-sensitive code must use this variant so storage failures
    /// fail closed instead of silently treating the family as valid.
    async fn get_family_revoked_at_checked(&self, key: &str) -> Result<Option<i64>>;

    /// Set the family revocation timestamp for a key with TTL.
    ///
    /// This is a security-critical write. Callers must be able to fail closed
    /// if persistence does not succeed.
    async fn set_family_revoked(&self, key: &str, timestamp: i64, ttl_secs: u64) -> Result<()>;

    /// Get an integer version marker for a caller-defined key.
    ///
    /// This reuses the same TTL-backed integer storage as family revocation
    /// without leaking refresh-token terminology into non-refresh-token callers.
    async fn get_version_checked(&self, key: &str) -> Result<Option<i64>> {
        self.get_family_revoked_at_checked(key).await
    }

    /// Set an integer version marker for a caller-defined key with TTL.
    async fn set_version(&self, key: &str, version: i64, ttl_secs: u64) -> Result<()> {
        self.set_family_revoked(key, version, ttl_secs).await
    }
}

#[cfg(test)]
mod tests;
