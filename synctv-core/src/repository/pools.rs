//! Shared primary/read pool wrapper for repositories.
//!
//! Many repositories need a primary (write) pool plus an optional
//! eventually-consistent read-replica pool, with reads falling back to the
//! primary when no dedicated replica is configured. This newtype encapsulates
//! that pair so the field declarations and the `read()` fallback accessor are
//! not re-implemented in every repository.

use sqlx::PgPool;

/// Primary write pool plus an optional eventually-consistent read replica pool.
///
/// Repositories opt in to [`RepoPools::read`] deliberately for read-only,
/// eventually-consistent queries; writes, transactions, and consistency-coupled
/// reads must use [`RepoPools::primary`].
#[derive(Debug, Clone)]
pub struct RepoPools {
    primary: PgPool,
    read: Option<PgPool>,
}

impl RepoPools {
    /// Build a pool pair that routes every query to the primary pool.
    #[must_use]
    pub const fn new(primary: PgPool) -> Self {
        Self {
            primary,
            read: None,
        }
    }

    /// Build a pool pair with a dedicated eventually-consistent read pool.
    #[must_use]
    pub const fn with_read(primary: PgPool, read: PgPool) -> Self {
        Self {
            primary,
            read: Some(read),
        }
    }

    /// Primary write pool. Use for writes, transactions, auth/security checks,
    /// and any read whose result must be strongly consistent.
    #[must_use]
    pub const fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// Eventually-consistent read pool, falling back to the primary pool when no
    /// dedicated read replica is configured.
    #[must_use]
    pub fn read(&self) -> &PgPool {
        self.read.as_ref().unwrap_or(&self.primary)
    }
}
